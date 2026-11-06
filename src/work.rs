//! The generic work of the machines: the machines of the plugins, the queue of jobs with its history, and the notifications.
//!
//! A **job** of the generic queue is made from a request that the plugin of the machine checked (`POST /api/v1/work/jobs`): it is `queued` until a person starts it, then
//! `running`, maybe `paused`, and ends `done`, `failed` or `cancelled`. Every change of state is kept in the **history** of the job, told on the stream of events and, when
//! it needs a person (the job ended, failed or waits at a dialog of the machine), turned into a **notification** for its owner. The state of a job that runs is read from the
//! plugin every half second by a thread of the server, and again whenever somebody asks for it.
//!
//! The jobs and the machines of the plugins are kept in the data folder (`work-jobs.json`, `work-machines.json`); the notifications are not (they are for the people who
//! are there). A job of the `demo` plugin that was not finished when the server stopped is marked as failed: the demo machine lives in memory.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::auth::{Identity, Role};
use crate::config::ServerConfig;
use crate::feed::{FeedEvent, Visibility};
use crate::ids::{self, Kind};
use crate::jobs::now;
use crate::plugins::{PluginError, PluginStatus, WorkState};
use crate::state::AppState;
use crate::store;
use crate::sync::Locked;

/// The longest history a job keeps.
const MAX_HISTORY: usize = 200;

#[derive(Debug, PartialEq)]
pub enum WError {
    BadId,
    BadName,
    BadSettings,
    UnknownPlugin,
    MachineExists,
    UnknownMachine,
    UnknownJob,
    NotReady,
    NotCancellable,
    JobActive,
    QueueFull,
    MachineBusy,
    NotInState,
    Storage,
}

impl WError {
    pub fn code(&self) -> &'static str {
        match self {
            WError::BadId => "bad_id",
            WError::BadName => "bad_name",
            WError::BadSettings => "bad_plugin_request",
            WError::UnknownPlugin => "unknown_plugin",
            WError::MachineExists => "machine_exists",
            WError::UnknownMachine => "unknown_machine",
            WError::UnknownJob => "unknown_job",
            WError::NotReady => "not_ready",
            WError::NotCancellable => "not_cancellable",
            WError::JobActive => "job_active",
            WError::QueueFull => "queue_full",
            WError::MachineBusy => "machine_busy",
            WError::NotInState => "not_in_state",
            WError::Storage => "storage",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            WError::BadId | WError::BadName | WError::BadSettings | WError::UnknownPlugin => 422,
            WError::UnknownMachine | WError::UnknownJob => 404,
            WError::QueueFull => 429,
            WError::Storage => 500,
            _ => 409,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            WError::BadSettings => "settings is an object".into(),
            _ => String::new(),
        }
    }
}

/// Why an operation of the generic work was refused: by the server or by the plugin.
#[derive(Debug, PartialEq)]
pub enum Fail {
    Work(WError),
    Plugin(PluginError),
}

impl From<WError> for Fail {
    fn from(e: WError) -> Self {
        Fail::Work(e)
    }
}

impl From<PluginError> for Fail {
    fn from(e: PluginError) -> Self {
        Fail::Plugin(e)
    }
}

impl Fail {
    pub fn code(&self) -> &'static str {
        match self {
            Fail::Work(e) => e.code(),
            Fail::Plugin(e) => e.code(),
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            Fail::Work(e) => e.status(),
            Fail::Plugin(e) => e.status(),
        }
    }

    pub fn detail(&self) -> String {
        match self {
            Fail::Work(e) => e.detail(),
            Fail::Plugin(e) => e.detail(),
        }
    }
}

// ---- the machines of the plugins ------------------------------------------------------------------------------------------------------

/// A machine that was made through the API for a plugin that keeps its machines here (`demo`). The machines of `bungard` are those of `/api/v1/machines`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WorkMachine {
    pub id: String,
    pub name: String,
    pub plugin: String,
    #[serde(default)]
    pub settings: Value,
    pub created: u64,
}

impl WorkMachine {
    pub fn view(&self) -> Value {
        json!({
            "id": self.id, "name": self.name, "plugin": self.plugin,
            "settings": self.settings, "created": self.created,
        })
    }
}

pub struct MachineBook {
    path: PathBuf,
    list: Vec<WorkMachine>,
}

impl MachineBook {
    pub fn open(dir: &Path) -> io::Result<MachineBook> {
        let path = dir.join("work-machines.json");
        let list = store::read_json(&path)?.unwrap_or_default();
        Ok(MachineBook { path, list })
    }

    fn save(&self) -> Result<(), WError> {
        store::write_json(&self.path, &self.list).map_err(|_| WError::Storage)
    }

    pub fn list(&self) -> &[WorkMachine] {
        &self.list
    }

    pub fn get(&self, id: &str) -> Option<&WorkMachine> {
        self.list.iter().find(|m| m.id == id)
    }

    pub fn add(&mut self, machine: WorkMachine) -> Result<(), WError> {
        if self.get(&machine.id).is_some() {
            return Err(WError::MachineExists);
        }
        self.list.push(machine);
        self.save()
    }

    pub fn edit(
        &mut self,
        id: &str,
        name: Option<String>,
        settings: Option<Value>,
    ) -> Result<WorkMachine, WError> {
        let m = self
            .list
            .iter_mut()
            .find(|m| m.id == id)
            .ok_or(WError::UnknownMachine)?;
        if let Some(name) = name {
            m.name = name;
        }
        if let Some(settings) = settings {
            m.settings = settings;
        }
        let out = m.clone();
        self.save()?;
        Ok(out)
    }

    pub fn remove(&mut self, id: &str) -> Result<(), WError> {
        let before = self.list.len();
        self.list.retain(|m| m.id != id);
        if self.list.len() == before {
            return Err(WError::UnknownMachine);
        }
        self.save()
    }
}

// ---- the queue --------------------------------------------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HistoryEntry {
    pub at: u64,
    pub state: WorkState,
    /// Who made the change: a person, or `machine` when the plugin said so.
    pub by: String,
    pub note: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WorkJob {
    pub id: String,
    pub machine: String,
    pub plugin: String,
    /// The application that asked for it.
    pub app: String,
    pub name: String,
    /// The owner key of the person who asked (`Identity::owner`) and the name to show.
    pub owner: String,
    pub by: String,
    pub state: WorkState,
    pub created: u64,
    pub started: Option<u64>,
    pub finished: Option<u64>,
    /// The id the plugin knows the job by.
    pub native: String,
    pub done: u64,
    pub total: u64,
    pub message: String,
    pub history: Vec<HistoryEntry>,
    /// What the plugin drew of the job when it prepared it; kept in memory only.
    #[serde(skip)]
    pub view: Value,
    /// The tenth of the work that was last told on the stream.
    #[serde(skip)]
    pub decile: u8,
}

impl WorkJob {
    pub fn percent(&self) -> u64 {
        (self.done * 100)
            .checked_div(self.total)
            .map_or(0, |p| p.min(100))
    }

    pub fn summary(&self) -> Value {
        json!({
            "id": self.id, "machine": self.machine, "plugin": self.plugin, "app": self.app,
            "name": self.name, "owner": self.owner, "by": self.by, "state": self.state,
            "created": self.created, "started": self.started, "finished": self.finished,
            "done": self.done, "total": self.total, "percent": self.percent(), "message": self.message,
        })
    }

    pub fn full(&self) -> Value {
        let mut v = self.summary();
        v["native"] = json!(self.native);
        v["view"] = self.view.clone();
        v["history"] = json!(self.history);
        v
    }

    fn push(&mut self, at: u64, state: WorkState, by: &str, note: &str) {
        self.history.push(HistoryEntry {
            at,
            state,
            by: by.to_string(),
            note: note.chars().take(200).collect(),
        });
        if self.history.len() > MAX_HISTORY {
            let extra = self.history.len() - MAX_HISTORY;
            self.history.drain(..extra);
        }
    }
}

/// What a reading of the plugin changed in a job.
pub struct Update {
    pub job: WorkJob,
    pub state_changed: bool,
    pub progress_changed: bool,
    pub message_changed: bool,
}

pub struct WorkBook {
    path: PathBuf,
    keep: usize,
    list: Vec<WorkJob>,
}

impl WorkBook {
    pub fn open(dir: &Path, keep: usize) -> io::Result<WorkBook> {
        let path = dir.join("work-jobs.json");
        let mut list: Vec<WorkJob> = store::read_json(&path)?.unwrap_or_default();
        let at = now();
        let mut changed = false;
        for job in &mut list {
            if job.plugin == "demo" && !job.state.finished() {
                job.state = WorkState::Failed;
                job.finished = Some(at);
                job.message = "the server restarted while the demo job was open".into();
                let note = job.message.clone();
                job.push(at, WorkState::Failed, "server", &note);
                changed = true;
            }
        }
        let book = WorkBook { path, keep, list };
        if changed {
            book.save()?;
        }
        Ok(book)
    }

    fn save(&self) -> io::Result<()> {
        store::write_json(&self.path, &self.list)
    }

    pub fn list(&self) -> &[WorkJob] {
        &self.list
    }

    pub fn get(&self, id: &str) -> Option<&WorkJob> {
        self.list.iter().find(|j| j.id == id)
    }

    pub fn queued_count(&self) -> usize {
        self.list
            .iter()
            .filter(|j| j.state == WorkState::Queued)
            .count()
    }

    /// The job that has the machine (running or held), if any.
    pub fn active_on(&self, machine: &str) -> Option<&WorkJob> {
        self.list
            .iter()
            .find(|j| j.machine == machine && j.state.active())
    }

    /// The place of a queued job among the queued jobs of its machine (1 is the next); 0 for a job that is not queued.
    pub fn position(&self, job: &WorkJob) -> usize {
        if job.state != WorkState::Queued {
            return 0;
        }
        let mut place = 0;
        for j in &self.list {
            if j.machine == job.machine && j.state == WorkState::Queued {
                place += 1;
                if j.id == job.id {
                    return place;
                }
            }
        }
        0
    }

    pub fn add(&mut self, job: WorkJob) -> Result<(), WError> {
        self.list.push(job);
        while self.list.len() > self.keep {
            match self.list.iter().position(|j| j.state.finished()) {
                Some(i) => {
                    self.list.remove(i);
                }
                None => break,
            }
        }
        self.save().map_err(|_| WError::Storage)
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.list.len();
        self.list.retain(|j| j.id != id);
        let removed = self.list.len() != before;
        if removed {
            let _ = self.save();
        }
        removed
    }

    /// Changes the state of a job when it is in `from`; the job as it is now, and whether it was changed.
    pub fn set_state(
        &mut self,
        id: &str,
        from: WorkState,
        to: WorkState,
        by: &str,
        note: &str,
        at: u64,
    ) -> Option<(WorkJob, bool)> {
        let job = self.list.iter_mut().find(|j| j.id == id)?;
        if job.state != from {
            return Some((job.clone(), false));
        }
        job.state = to;
        if to != WorkState::Queued && job.started.is_none() {
            job.started = Some(at);
        }
        if to.finished() {
            job.finished = Some(at);
        }
        job.push(at, to, by, note);
        let out = job.clone();
        let _ = self.save();
        Some((out, true))
    }

    /// Takes a reading of the plugin into the job.
    pub fn apply_status(&mut self, id: &str, status: &PluginStatus, at: u64) -> Option<Update> {
        let job = self.list.iter_mut().find(|j| j.id == id)?;
        if job.state.finished() {
            return None;
        }
        job.done = status.done;
        job.total = status.total;
        let message_changed = job.message != status.message;
        job.message = status.message.clone();
        let mut state_changed = false;
        // a job the server started is not taken back to `queued` by a reading that came a moment too early
        if status.state != job.state && status.state != WorkState::Queued {
            job.state = status.state;
            state_changed = true;
            if job.started.is_none() {
                job.started = Some(at);
            }
            if status.state.finished() {
                job.finished = Some(at);
            }
            let note = status.message.clone();
            job.push(at, status.state, "machine", &note);
        }
        let decile = (job.done * 10)
            .checked_div(job.total)
            .map_or(0, |d| d.min(10) as u8);
        let progress_changed = decile != job.decile;
        job.decile = decile;
        let out = job.clone();
        if state_changed {
            let _ = self.save();
        }
        Some(Update {
            job: out,
            state_changed,
            progress_changed,
            message_changed,
        })
    }
}

// ---- the notifications ------------------------------------------------------------------------------------------------------------------

struct Note {
    id: u64,
    at: u64,
    kind: String,
    title: String,
    /// The owner key of the person it is for; `None`: everybody.
    owner: Option<String>,
    data: Value,
    read_by: Vec<String>,
}

pub struct Notes {
    next: u64,
    keep: usize,
    list: VecDeque<Note>,
}

impl Notes {
    pub fn new(keep: usize) -> Notes {
        Notes {
            next: 1,
            keep,
            list: VecDeque::new(),
        }
    }

    pub fn add(&mut self, kind: &str, title: &str, owner: Option<String>, data: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        self.list.push_back(Note {
            id,
            at: now(),
            kind: kind.to_string(),
            title: title.to_string(),
            owner,
            data,
            read_by: Vec::new(),
        });
        while self.list.len() > self.keep {
            self.list.pop_front();
        }
        id
    }

    fn visible(note: &Note, who: &Identity) -> bool {
        match &note.owner {
            None => true,
            Some(o) => who.role == Role::Admin || *o == who.owner(),
        }
    }

    /// The notifications for the caller, the newest first.
    pub fn for_user(&self, who: &Identity, unread_only: bool, limit: usize) -> Vec<Value> {
        let me = who.owner();
        self.list
            .iter()
            .rev()
            .filter(|n| Self::visible(n, who))
            .filter(|n| !unread_only || !n.read_by.contains(&me))
            .take(limit)
            .map(|n| {
                json!({
                    "id": n.id, "at": n.at, "kind": n.kind, "title": n.title, "data": n.data,
                    "read": n.read_by.contains(&me),
                })
            })
            .collect()
    }

    /// Marks notifications as read by the caller: the ones named, or all of those it can see. How many were marked.
    pub fn mark_read(&mut self, who: &Identity, ids: &[u64], all: bool) -> usize {
        let me = who.owner();
        let mut marked = 0;
        for note in &mut self.list {
            if !Self::visible(note, who) || note.read_by.contains(&me) {
                continue;
            }
            if all || ids.contains(&note.id) {
                note.read_by.push(me.clone());
                marked += 1;
            }
        }
        marked
    }
}

// ---- all together -----------------------------------------------------------------------------------------------------------------------

pub struct Work {
    pub machines: Mutex<MachineBook>,
    pub jobs: Mutex<WorkBook>,
    pub notes: Mutex<Notes>,
}

impl Work {
    pub fn open(cfg: &ServerConfig) -> io::Result<Work> {
        Ok(Work {
            machines: Mutex::new(MachineBook::open(&cfg.data_dir)?),
            jobs: Mutex::new(WorkBook::open(&cfg.data_dir, cfg.work_keep.max(1))?),
            notes: Mutex::new(Notes::new(cfg.notifications_keep)),
        })
    }
}

/// How often the thread of the server reads the state of the jobs that are open.
const REFRESH_EVERY: Duration = Duration::from_millis(500);

impl AppState {
    /// Starts the thread that reads the jobs of the plugins every half second. It holds the state only while it works, so it ends when the server is dropped.
    pub fn start_work_thread(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("eh3d-work".into())
            .spawn(move || loop {
                std::thread::sleep(REFRESH_EVERY);
                match weak.upgrade() {
                    Some(state) => state.work_refresh_all(),
                    None => break,
                }
            });
        if let Err(e) = spawned {
            eprintln!("eh3d-server: the thread that reads the jobs of the plugins did not start ({e}); they are read when somebody asks");
        }
    }

    /// The plugin that has a machine with this id.
    pub fn work_find(
        self: &Arc<Self>,
        machine: &str,
    ) -> Option<Arc<dyn crate::plugins::MachinePlugin>> {
        self.plugins
            .all()
            .iter()
            .find(|p| p.machines(self).iter().any(|m| m.id == machine))
            .cloned()
    }

    /// Every machine of every plugin, with its state, the job it has and what it can do.
    pub fn work_machines_view(self: &Arc<Self>) -> Vec<Value> {
        let mut out = Vec::new();
        for plugin in self.plugins.all() {
            let capabilities = plugin.capabilities();
            for m in plugin.machines(self) {
                let job = self
                    .work
                    .jobs
                    .locked()
                    .active_on(&m.id)
                    .map(|j| j.id.clone());
                out.push(json!({
                    "id": m.id, "name": m.name, "plugin": plugin.kind(), "state": m.state,
                    "job": job, "capabilities": capabilities,
                }));
            }
        }
        out
    }

    pub fn announce_job(&self, kind: &str, job: &WorkJob) {
        self.feed.publish(
            FeedEvent::new(kind, Visibility::Everyone, job.summary())
                .machine(&job.machine)
                .job(&job.id)
                .app(&job.app),
        );
    }

    pub fn announce_machine(&self, kind: &str, id: &str, name: &str) {
        self.feed.publish(
            FeedEvent::new(
                kind,
                Visibility::Everyone,
                json!({ "id": id, "name": name }),
            )
            .machine(id),
        );
    }

    /// Makes a job ready on a machine: the plugin checks the request and prepares it; nothing moves until a person starts it.
    pub fn work_create(
        self: &Arc<Self>,
        who: &Identity,
        machine: &str,
        name: Option<&str>,
        app: &str,
        request: &Value,
    ) -> Result<WorkJob, Fail> {
        let plugin = self.work_find(machine).ok_or(WError::UnknownMachine)?;
        if self.work.jobs.locked().queued_count() >= self.cfg.work_queue_max {
            return Err(WError::QueueFull.into());
        }
        let prepared = plugin.prepare(self, machine, request, &who.name)?;
        let at = now();
        let name = name
            .map(str::to_string)
            .or_else(|| {
                request
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .filter(|n| crate::apps::valid_label(n, 80))
            .unwrap_or_else(|| "job".into());
        let job = WorkJob {
            id: ids::new(Kind::Work),
            machine: machine.to_string(),
            plugin: plugin.kind().to_string(),
            app: app.to_string(),
            name,
            owner: who.owner(),
            by: who.name.clone(),
            state: WorkState::Queued,
            created: at,
            started: None,
            finished: None,
            native: prepared.native_id,
            done: 0,
            total: 0,
            message: String::new(),
            history: vec![HistoryEntry {
                at,
                state: WorkState::Queued,
                by: who.name.clone(),
                note: "prepared".into(),
            }],
            view: prepared.view,
            decile: 0,
        };
        self.work.jobs.locked().add(job.clone())?;
        self.announce_job("job.created", &job);
        Ok(job)
    }

    fn work_job(&self, id: &str) -> Result<WorkJob, WError> {
        self.work
            .jobs
            .locked()
            .get(id)
            .cloned()
            .ok_or(WError::UnknownJob)
    }

    fn work_plugin(&self, job: &WorkJob) -> Result<Arc<dyn crate::plugins::MachinePlugin>, Fail> {
        self.plugins
            .get(&job.plugin)
            .ok_or(Fail::Plugin(PluginError::UnknownMachine))
    }

    /// The change of state that a person made, as the book keeps it and the stream tells it.
    fn work_moved(
        &self,
        id: &str,
        from: WorkState,
        to: WorkState,
        by: &str,
        note: &str,
    ) -> Result<WorkJob, Fail> {
        let moved = self
            .work
            .jobs
            .locked()
            .set_state(id, from, to, by, note, now());
        let (job, changed) = moved.ok_or(WError::UnknownJob)?;
        if changed {
            self.announce_job("job.state", &job);
        }
        Ok(job)
    }

    pub fn work_start(self: &Arc<Self>, id: &str, who: &Identity) -> Result<WorkJob, Fail> {
        let job = self.work_job(id)?;
        if job.state != WorkState::Queued {
            return Err(WError::NotReady.into());
        }
        if self.work.jobs.locked().active_on(&job.machine).is_some() {
            return Err(WError::MachineBusy.into());
        }
        let plugin = self.work_plugin(&job)?;
        plugin.start(self, &job.machine, &job.native, &who.name)?;
        self.work_moved(
            id,
            WorkState::Queued,
            WorkState::Running,
            &who.name,
            "started",
        )
    }

    pub fn work_pause(self: &Arc<Self>, id: &str, who: &Identity) -> Result<WorkJob, Fail> {
        let job = self.work_job(id)?;
        if job.state != WorkState::Running {
            return Err(WError::NotInState.into());
        }
        let plugin = self.work_plugin(&job)?;
        plugin.pause(self, &job.machine, &job.native)?;
        self.work_moved(id, WorkState::Running, WorkState::Paused, &who.name, "held")
    }

    pub fn work_resume(self: &Arc<Self>, id: &str, who: &Identity) -> Result<WorkJob, Fail> {
        let job = self.work_job(id)?;
        if job.state != WorkState::Paused {
            return Err(WError::NotInState.into());
        }
        let plugin = self.work_plugin(&job)?;
        plugin.resume(self, &job.machine, &job.native)?;
        self.work_moved(
            id,
            WorkState::Paused,
            WorkState::Running,
            &who.name,
            "went on",
        )
    }

    /// Cancels a queued job, or stops a running or held one.
    pub fn work_cancel(self: &Arc<Self>, id: &str, who: &Identity) -> Result<WorkJob, Fail> {
        let job = self.work_job(id)?;
        let plugin = self.work_plugin(&job)?;
        match job.state {
            WorkState::Queued => {
                plugin.discard(self, &job.machine, &job.native, &who.name);
                self.work_moved(
                    id,
                    WorkState::Queued,
                    WorkState::Cancelled,
                    &who.name,
                    "cancelled before it started",
                )
            }
            WorkState::Running | WorkState::Paused => {
                plugin.stop(self, &job.machine, &job.native, &who.name)?;
                self.work_moved(id, job.state, WorkState::Cancelled, &who.name, "stopped")
            }
            _ => Err(WError::NotCancellable.into()),
        }
    }

    /// Removes a finished job from the list.
    pub fn work_remove(&self, id: &str) -> Result<(), WError> {
        let mut jobs = self.work.jobs.locked();
        let state = jobs.get(id).map(|j| j.state);
        match state {
            None => Err(WError::UnknownJob),
            Some(s) if !s.finished() => Err(WError::JobActive),
            Some(_) => {
                jobs.remove(id);
                Ok(())
            }
        }
    }

    /// Reads the state of one job from its plugin, keeps it, tells the stream and the owner of what changed. The job as it is now.
    pub fn work_refresh_one(self: &Arc<Self>, id: &str) -> Option<WorkJob> {
        let job = self.work.jobs.locked().get(id).cloned()?;
        if job.state.finished() {
            return Some(job);
        }
        let Some(plugin) = self.plugins.get(&job.plugin) else {
            return Some(job);
        };
        let status = match plugin.status(self, &job.machine, &job.native) {
            Ok(status) => status,
            Err(e) => {
                let why = e.detail();
                PluginStatus {
                    state: WorkState::Failed,
                    done: job.done,
                    total: job.total,
                    message: if why.is_empty() {
                        e.code().to_string()
                    } else {
                        why
                    },
                }
            }
        };
        let update = self.work.jobs.locked().apply_status(id, &status, now());
        match update {
            Some(u) => {
                if u.state_changed {
                    self.announce_job("job.state", &u.job);
                    self.note_transition(&u.job);
                } else if u.progress_changed {
                    self.announce_job("job.progress", &u.job);
                }
                if u.message_changed && u.job.message.starts_with("waiting for the operator") {
                    self.notify(&u.job, "job.waiting", "A job waits for the operator");
                }
                Some(u.job)
            }
            None => self.work.jobs.locked().get(id).cloned(),
        }
    }

    pub fn work_refresh_all(self: &Arc<Self>) {
        let ids: Vec<String> = self
            .work
            .jobs
            .locked()
            .list()
            .iter()
            .filter(|j| !j.state.finished())
            .map(|j| j.id.clone())
            .collect();
        for id in ids {
            self.work_refresh_one(&id);
        }
    }

    fn note_transition(&self, job: &WorkJob) {
        let (kind, title) = match job.state {
            WorkState::Done => ("job.done", "A job finished"),
            WorkState::Failed => ("job.failed", "A job failed"),
            WorkState::Cancelled => ("job.cancelled", "A job was stopped by the machine"),
            WorkState::Paused => ("job.paused", "A job is held"),
            _ => return,
        };
        self.notify(job, kind, title);
    }

    /// A notification for the owner of the job, and a nudge on the stream (heard by the owner and the administrators only).
    fn notify(&self, job: &WorkJob, kind: &str, title: &str) {
        let data = json!({
            "job": job.id, "machine": job.machine, "name": job.name,
            "state": job.state, "message": job.message,
        });
        let id = self
            .work
            .notes
            .locked()
            .add(kind, title, Some(job.owner.clone()), data.clone());
        self.feed.publish(
            FeedEvent::new(
                "notification",
                Visibility::Owner(job.owner.clone()),
                json!({ "id": id, "kind": kind, "title": title, "data": data }),
            )
            .job(&job.id)
            .machine(&job.machine)
            .app(&job.app),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("eh3d-server-work-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn person(name: &str, role: Role) -> Identity {
        Identity {
            name: name.into(),
            role,
            is_token: false,
            token_id: None,
            scopes: None,
            template: None,
        }
    }

    fn job(id: &str, machine: &str, state: WorkState) -> WorkJob {
        WorkJob {
            id: id.into(),
            machine: machine.into(),
            plugin: "demo".into(),
            app: "studio".into(),
            name: "job".into(),
            owner: "ana".into(),
            by: "ana".into(),
            state,
            created: 1,
            started: None,
            finished: None,
            native: format!("n-{id}"),
            done: 0,
            total: 0,
            message: String::new(),
            history: Vec::new(),
            view: Value::Null,
            decile: 0,
        }
    }

    fn status(state: WorkState, done: u64, total: u64) -> PluginStatus {
        PluginStatus {
            state,
            done,
            total,
            message: String::new(),
        }
    }

    #[test]
    fn the_places_of_the_queue_are_counted_per_machine() {
        let d = dir("positions");
        let mut book = WorkBook::open(&d, 100).unwrap();
        for (id, machine, state) in [
            ("a", "m1", WorkState::Queued),
            ("b", "m2", WorkState::Queued),
            ("c", "m1", WorkState::Queued),
            ("d", "m1", WorkState::Running),
        ] {
            book.add(job(id, machine, state)).unwrap();
        }
        assert_eq!(book.queued_count(), 3);
        let place = |id: &str| book.position(book.get(id).unwrap());
        assert_eq!(
            (place("a"), place("b"), place("c"), place("d")),
            (1, 1, 2, 0)
        );
        assert_eq!(book.active_on("m1").unwrap().id, "d");
        assert!(book.active_on("m2").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_state_changes_only_from_the_state_it_was_asked_from() {
        let d = dir("moves");
        let mut book = WorkBook::open(&d, 100).unwrap();
        book.add(job("a", "m1", WorkState::Queued)).unwrap();
        let (j, changed) = book
            .set_state(
                "a",
                WorkState::Queued,
                WorkState::Running,
                "ana",
                "started",
                5,
            )
            .unwrap();
        assert!(changed);
        assert_eq!((j.state, j.started), (WorkState::Running, Some(5)));
        // a second try from the old state changes nothing
        let (_, again) = book
            .set_state(
                "a",
                WorkState::Queued,
                WorkState::Running,
                "ana",
                "started",
                6,
            )
            .unwrap();
        assert!(!again);
        let (j, _) = book
            .set_state("a", WorkState::Running, WorkState::Done, "ana", "", 9)
            .unwrap();
        assert_eq!(
            (j.state, j.finished, j.history.len()),
            (WorkState::Done, Some(9), 2)
        );
        assert!(book
            .set_state("zzz", WorkState::Queued, WorkState::Done, "x", "", 1)
            .is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_reading_of_the_plugin_moves_the_job_and_a_finished_job_is_left_alone() {
        let d = dir("readings");
        let mut book = WorkBook::open(&d, 100).unwrap();
        book.add(job("a", "m1", WorkState::Running)).unwrap();
        let u = book
            .apply_status("a", &status(WorkState::Running, 3, 10), 5)
            .unwrap();
        assert!(!u.state_changed);
        assert!(u.progress_changed);
        assert_eq!((u.job.done, u.job.total, u.job.percent()), (3, 10, 30));
        // the same tenth is not told again
        let u = book
            .apply_status("a", &status(WorkState::Running, 3, 10), 5)
            .unwrap();
        assert!(!u.progress_changed);
        // a reading that says queued does not take a started job back
        let u = book
            .apply_status("a", &status(WorkState::Queued, 0, 10), 6)
            .unwrap();
        assert!(!u.state_changed);
        let u = book
            .apply_status("a", &status(WorkState::Done, 10, 10), 7)
            .unwrap();
        assert!(u.state_changed);
        assert_eq!((u.job.state, u.job.finished), (WorkState::Done, Some(7)));
        assert_eq!(u.job.history.last().unwrap().by, "machine");
        assert!(book
            .apply_status("a", &status(WorkState::Running, 1, 10), 8)
            .is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_oldest_finished_jobs_go_when_the_list_is_longer_than_it_should_be() {
        let d = dir("keep");
        let mut book = WorkBook::open(&d, 3).unwrap();
        book.add(job("a", "m", WorkState::Done)).unwrap();
        book.add(job("b", "m", WorkState::Running)).unwrap();
        book.add(job("c", "m", WorkState::Done)).unwrap();
        book.add(job("d", "m", WorkState::Queued)).unwrap();
        let ids: Vec<&str> = book.list().iter().map(|j| j.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c", "d"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_demo_job_open_at_a_restart_is_failed_and_the_rest_is_read_again() {
        let d = dir("restart");
        {
            let mut book = WorkBook::open(&d, 100).unwrap();
            book.add(job("a", "m1", WorkState::Running)).unwrap();
            book.add(job("b", "m1", WorkState::Done)).unwrap();
        }
        let book = WorkBook::open(&d, 100).unwrap();
        let a = book.get("a").unwrap();
        assert_eq!(a.state, WorkState::Failed);
        assert!(a.message.contains("restarted"));
        assert_eq!(book.get("b").unwrap().state, WorkState::Done);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_machines_of_a_plugin_are_kept_and_not_made_twice() {
        let d = dir("machines");
        let mut book = MachineBook::open(&d).unwrap();
        let m = WorkMachine {
            id: "demo-1".into(),
            name: "Demo".into(),
            plugin: "demo".into(),
            settings: json!({"step_ms": 5}),
            created: 1,
        };
        book.add(m.clone()).unwrap();
        assert_eq!(book.add(m), Err(WError::MachineExists));
        let edited = book.edit("demo-1", Some("Other".into()), None).unwrap();
        assert_eq!(edited.name, "Other");
        assert_eq!(edited.settings["step_ms"], 5);
        assert_eq!(book.edit("nope", None, None), Err(WError::UnknownMachine));
        assert_eq!(MachineBook::open(&d).unwrap().list().len(), 1);
        book.remove("demo-1").unwrap();
        assert_eq!(book.remove("demo-1"), Err(WError::UnknownMachine));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_notification_is_for_its_owner_and_the_administrators_until_it_is_read() {
        let mut notes = Notes::new(3);
        let (ana, bob, root) = (
            person("ana", Role::Operator),
            person("bob", Role::Operator),
            person("root", Role::Admin),
        );
        let mine = notes.add("job.done", "done", Some("ana".into()), json!({}));
        notes.add("job.failed", "failed", Some("bob".into()), json!({}));
        notes.add("server.notice", "for all", None, json!({}));
        assert_eq!(notes.for_user(&ana, false, 10).len(), 2);
        assert_eq!(notes.for_user(&bob, false, 10).len(), 2);
        assert_eq!(notes.for_user(&root, false, 10).len(), 3);
        // newest first
        assert_eq!(notes.for_user(&ana, false, 10)[0]["kind"], "server.notice");
        assert_eq!(notes.mark_read(&ana, &[mine], false), 1);
        assert_eq!(notes.mark_read(&ana, &[mine], false), 0);
        assert_eq!(notes.for_user(&ana, true, 10).len(), 1);
        // the others are not marked by somebody else's reading
        assert_eq!(notes.for_user(&root, true, 10).len(), 3);
        assert_eq!(notes.mark_read(&root, &[], true), 3);
        // only the last ones are kept
        notes.add("x", "x", None, json!({}));
        assert_eq!(notes.for_user(&root, false, 10).len(), 3);
    }
}
