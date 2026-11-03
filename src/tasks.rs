//! Computing tasks: work the server does for a program that has no effect on any machine (checking a design, making the files for a factory, running a simulation).
//! They are a family of their own, apart from the jobs of the machines: a task may start by itself according to the queue and the limits, a job never does; a task has an
//! engine, a job has a connection to a machine.
//!
//! **What keeps the two apart.** This file and the one of the artifacts know nothing of machines, connections, jobs or nodes: they use none of the modules that plan, hold or
//! run a job, talk to a node or a controller, or reach the libraries of the machine programs, and the test `engines_cannot_reach_a_machine` reads the source and fails if
//! one of them ever does. That is the first layer; running the engines in a process of their own is the second and is not done yet (see `docs/API.md`, "Tasks").
//!
//! A task goes `queued` -> `running` -> `done`, `failed` or `cancelled`. The list is kept in `tasks.json`; after a restart the tasks that were queued stay queued and
//! the one that was running is marked failed ("interrupted by restart"): it may be sent again. A task that is cancelled while its engine works leaves no result.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eh3d_diagnostic::{Category, Diagnostic, Recoverability, Severity};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::auth::{Identity, Role};
use crate::config::ServerConfig;
use crate::jobs::now;
use crate::state::{AppState, Audience};
use crate::store;
use crate::sync::Locked;

/// The kinds of task of the design suite. A kind is known as soon as it is named here; it is *available* only when an engine for it is in the build
/// (`GET /api/v1/tasks/kinds`), and asking for one that is not answers `engine_missing`.
pub const KNOWN_KINDS: [&str; 11] = [
    "phoenix.import",
    "phoenix.check",
    "phoenix.export.fab",
    "phoenix.route.auto",
    "phoenix.library.sanitize",
    "phoenix.verify",
    "phoenix.passport.build",
    "phoenix.sim.spice",
    "phoenix.sim.thermal",
    "phoenix.sim.rf",
    "phoenix.ai.assist",
];

/// The kind of the engine that exercises the task system itself (switched on with `tasks_selftest`).
pub const SELFTEST_KIND: &str = "server.selftest";

/// The diagnostic codes the task system itself gives (the engines give their own).
pub const CODES: [(&str, &str); 6] = [
    (
        "EH3D-TASK-001",
        "The task was asked to fail (the self-test engine).",
    ),
    ("EH3D-TASK-002", "The task ran longer than task_timeout_s."),
    (
        "EH3D-TASK-004",
        "The server restarted while the task ran: send it again.",
    ),
    (
        "EH3D-TASK-005",
        "The engine of the task failed unexpectedly.",
    ),
    (
        "EH3D-TASK-006",
        "The artifact the task works on could not be read.",
    ),
    (
        "EH3D-TASK-007",
        "The files the task made could not be stored.",
    ),
];

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl TaskState {
    pub fn finished(self) -> bool {
        matches!(
            self,
            TaskState::Done | TaskState::Failed | TaskState::Cancelled
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            TaskState::Queued => "queued",
            TaskState::Running => "running",
            TaskState::Done => "done",
            TaskState::Failed => "failed",
            TaskState::Cancelled => "cancelled",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Progress {
    pub done: u64,
    pub total: Option<u64>,
    pub unit: Option<String>,
}

/// What a finished task gives.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct TaskResult {
    /// The artifacts the task made, as `{artifact_id, name, kind, result_type}`.
    pub artifacts: Vec<Value>,
    pub diagnostics: Vec<Diagnostic>,
    /// A small result that is not worth a file.
    #[serde(default)]
    pub data: Value,
}

/// The key a client gave to make a request safe to send twice, with the hash of the body it came with.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Idempotency {
    pub key: String,
    pub body_sha256: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TaskRecord {
    pub id: String,
    pub kind: String,
    pub name: String,
    /// The owner key (see `Identity::owner`).
    pub owner: String,
    pub state: TaskState,
    pub created: u64,
    pub started: Option<u64>,
    pub finished: Option<u64>,
    pub params: Value,
    /// The artifact the task works on.
    #[serde(default)]
    pub project: Option<String>,
    pub progress: Progress,
    pub message: Option<String>,
    pub result: Option<TaskResult>,
    /// The version of the engine the task was made for.
    pub engine_version: String,
    #[serde(default)]
    pub idempotency: Option<Idempotency>,
}

impl TaskRecord {
    /// Whether `who` may see (and so be told about) this task: its owner and the administrators.
    pub fn visible_to(&self, who: &Identity) -> bool {
        who.role == Role::Admin || self.owner == who.owner()
    }

    /// The task as the API shows it.
    pub fn view(&self) -> Value {
        json!({
            "id": self.id, "kind": self.kind, "name": self.name, "owner": self.owner, "state": self.state,
            "created": self.created, "started": self.started, "finished": self.finished,
            "progress": self.progress, "message": self.message, "result": self.result,
            "engine": { "version": self.engine_version },
        })
    }
}

/// The tasks the server holds, in the order they were made.
pub struct TaskBook {
    path: PathBuf,
    tasks: Vec<TaskRecord>,
}

impl TaskBook {
    /// Reads `tasks.json` of the data folder. A task that was running when the server stopped is marked failed.
    pub fn open(dir: &Path) -> io::Result<TaskBook> {
        let path = dir.join("tasks.json");
        let mut tasks: Vec<TaskRecord> = store::read_json(&path)?.unwrap_or_default();
        let mut changed = false;
        for t in tasks.iter_mut().filter(|t| t.state == TaskState::Running) {
            t.state = TaskState::Failed;
            t.finished = Some(now());
            t.message = Some("interrupted by restart".into());
            t.result = Some(TaskResult {
                diagnostics: vec![diagnostic(
                    "EH3D-TASK-004",
                    Severity::Error,
                    "interrupted by restart",
                )],
                ..TaskResult::default()
            });
            changed = true;
        }
        let book = TaskBook { path, tasks };
        if changed {
            book.save()?;
        }
        Ok(book)
    }

    pub fn save(&self) -> io::Result<()> {
        store::write_json(&self.path, &self.tasks)
    }

    pub fn list(&self) -> &[TaskRecord] {
        &self.tasks
    }

    pub fn get(&self, id: &str) -> Option<&TaskRecord> {
        self.tasks.iter().find(|t| t.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut TaskRecord> {
        self.tasks.iter_mut().find(|t| t.id == id)
    }

    pub fn push(&mut self, task: TaskRecord) {
        self.tasks.push(task);
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.tasks.len();
        self.tasks.retain(|t| t.id != id);
        self.tasks.len() != before
    }

    /// How many tasks have not finished, and how many of those belong to `owner`.
    pub fn unfinished(&self, owner: &str) -> (usize, usize) {
        let all = self.tasks.iter().filter(|t| !t.state.finished());
        (
            all.clone().count(),
            all.filter(|t| t.owner == owner).count(),
        )
    }

    /// Drops the oldest finished tasks beyond `keep`; returns their ids and owners.
    pub fn prune(&mut self, keep: usize) -> Vec<(String, String)> {
        let finished = self.tasks.iter().filter(|t| t.state.finished()).count();
        let mut excess = finished.saturating_sub(keep);
        let mut dropped = Vec::new();
        self.tasks.retain(|t| {
            if excess > 0 && t.state.finished() {
                excess -= 1;
                dropped.push((t.id.clone(), t.owner.clone()));
                false
            } else {
                true
            }
        });
        dropped
    }
}

/// A diagnostic of the task system itself.
pub fn diagnostic(code: &'static str, severity: Severity, message: &str) -> Diagnostic {
    Diagnostic::new(0, severity, message)
        .with_code(code)
        .with_category(Category::Internal)
        .with_recoverability(Recoverability::Recoverable)
}

// ---- the engines ----------------------------------------------------------------------------------------------------------------------

/// Why an engine was told to stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Cancelled,
    TimedOut,
}

/// What an engine gets to look at while it works: whether it must stop, and a way to say how far it is.
pub struct TaskControl {
    cancel: Arc<AtomicBool>,
    deadline: Instant,
    report: Box<dyn Fn(Progress) + Send + Sync>,
}

impl TaskControl {
    pub fn new(
        cancel: Arc<AtomicBool>,
        timeout: Duration,
        report: Box<dyn Fn(Progress) + Send + Sync>,
    ) -> TaskControl {
        TaskControl {
            cancel,
            deadline: Instant::now() + timeout,
            report,
        }
    }

    /// Whether the engine has to stop now. An engine asks often and returns [`EngineFailure::Stopped`] as soon as it can.
    pub fn stop(&self) -> Option<Stop> {
        if self.cancel.load(Ordering::SeqCst) {
            Some(Stop::Cancelled)
        } else if Instant::now() >= self.deadline {
            Some(Stop::TimedOut)
        } else {
            None
        }
    }

    pub fn progress(&self, done: u64, total: Option<u64>, unit: &str) {
        (self.report)(Progress {
            done,
            total,
            unit: Some(unit.to_string()),
        });
    }
}

/// What an engine gets to work on.
#[derive(Debug)]
pub struct TaskInput {
    pub params: Value,
    /// The bytes of the artifact the task works on, when it names one.
    pub project: Option<Vec<u8>>,
}

/// A file an engine made. The server stores it as an artifact of the owner of the task, with the provenance filled in.
#[derive(Debug, Clone)]
pub struct OutFile {
    pub name: String,
    pub format: String,
    /// What it is for the person: `report`, `fab`, `diff`...
    pub kind: String,
    /// The class of the result (T1 to T7), when it has one.
    pub result_type: Option<String>,
    pub bytes: Vec<u8>,
}

/// What an engine gives back when it finishes.
#[derive(Debug, Default)]
pub struct TaskOutput {
    pub data: Value,
    pub diagnostics: Vec<Diagnostic>,
    pub files: Vec<OutFile>,
}

#[derive(Debug)]
pub enum EngineFailure {
    Stopped(Stop),
    Failed(Diagnostic),
}

/// A piece of computing the server can do for a task.
pub trait Engine: Send + Sync {
    fn kind(&self) -> &'static str;
    fn version(&self) -> &'static str;
    /// Whether the parameters are good; the text says what is wrong. Called before the task exists.
    fn validate(&self, params: &Value) -> Result<(), String>;
    fn run(&self, input: &TaskInput, control: &TaskControl) -> Result<TaskOutput, EngineFailure>;
}

/// The engines of this build.
pub struct Engines {
    list: Vec<Box<dyn Engine>>,
}

impl Engines {
    /// The engines every build has: none of the design suite yet, and the self-test one when asked.
    pub fn builtin(selftest: bool) -> Engines {
        let mut list: Vec<Box<dyn Engine>> = Vec::new();
        if selftest {
            list.push(Box::new(SelfTest));
        }
        Engines { list }
    }

    pub fn get(&self, kind: &str) -> Option<&dyn Engine> {
        self.list
            .iter()
            .find(|e| e.kind() == kind)
            .map(|b| b.as_ref())
    }

    /// Every kind the server knows, with whether an engine is there for it and which version.
    pub fn kinds(&self) -> Vec<Value> {
        let mut out: Vec<Value> = KNOWN_KINDS
            .iter()
            .map(|k| match self.get(k) {
                Some(e) => json!({ "kind": k, "available": true, "version": e.version() }),
                None => json!({ "kind": k, "available": false, "version": null }),
            })
            .collect();
        for e in &self.list {
            if !KNOWN_KINDS.contains(&e.kind()) {
                out.push(json!({ "kind": e.kind(), "available": true, "version": e.version() }));
            }
        }
        out
    }

    pub fn knows(&self, kind: &str) -> bool {
        KNOWN_KINDS.contains(&kind) || self.get(kind).is_some()
    }
}

/// The engine that exercises the task system: it counts steps (with a pause between them, looking at the stop and reporting its progress), can be asked to fail and
/// gives the SHA-256 of a text. It computes nothing a person needs, so it is off unless `tasks_selftest` is on.
struct SelfTest;

const SELFTEST_MAX_STEPS: u64 = 1000;
const SELFTEST_MAX_STEP_MS: u64 = 10_000;

impl Engine for SelfTest {
    fn kind(&self) -> &'static str {
        SELFTEST_KIND
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn validate(&self, params: &Value) -> Result<(), String> {
        let object = params.as_object().ok_or("the parameters are an object")?;
        for (key, value) in object {
            match key.as_str() {
                "steps" => {
                    let n = value.as_u64().ok_or("steps is a whole number")?;
                    if !(1..=SELFTEST_MAX_STEPS).contains(&n) {
                        return Err(format!("steps is from 1 to {SELFTEST_MAX_STEPS}"));
                    }
                }
                "step_ms" => {
                    let n = value.as_u64().ok_or("step_ms is a whole number")?;
                    if n > SELFTEST_MAX_STEP_MS {
                        return Err(format!("step_ms is from 0 to {SELFTEST_MAX_STEP_MS}"));
                    }
                }
                "text" => {
                    if !value.is_string() {
                        return Err("text is a text".into());
                    }
                }
                "fail" => {
                    if !value.is_boolean() {
                        return Err("fail is true or false".into());
                    }
                }
                "emit" => {
                    let o = value.as_object().ok_or("emit is {name, text}")?;
                    if !o.get("name").is_some_and(Value::is_string)
                        || !o.get("text").is_some_and(Value::is_string)
                    {
                        return Err("emit is {name, text}".into());
                    }
                }
                other => return Err(format!("unknown parameter '{other}'")),
            }
        }
        Ok(())
    }

    fn run(&self, input: &TaskInput, control: &TaskControl) -> Result<TaskOutput, EngineFailure> {
        let params = &input.params;
        let steps = params.get("steps").and_then(Value::as_u64).unwrap_or(3);
        let step_ms = params.get("step_ms").and_then(Value::as_u64).unwrap_or(10);
        for step in 0..steps {
            let mut waited = 0;
            while waited < step_ms {
                if let Some(stop) = control.stop() {
                    return Err(EngineFailure::Stopped(stop));
                }
                let slice = (step_ms - waited).min(10);
                std::thread::sleep(Duration::from_millis(slice));
                waited += slice;
            }
            if let Some(stop) = control.stop() {
                return Err(EngineFailure::Stopped(stop));
            }
            control.progress(step + 1, Some(steps), "steps");
        }
        if params.get("fail").and_then(Value::as_bool) == Some(true) {
            return Err(EngineFailure::Failed(diagnostic(
                "EH3D-TASK-001",
                Severity::Error,
                "the task was asked to fail",
            )));
        }
        let mut data = json!({ "steps": steps });
        if let Some(text) = params.get("text").and_then(Value::as_str) {
            data["sha256"] = json!(hex::encode(Sha256::digest(text.as_bytes())));
        }
        if let Some(project) = &input.project {
            data["project_sha256"] = json!(hex::encode(Sha256::digest(project)));
            data["project_size"] = json!(project.len());
        }
        let mut files = Vec::new();
        if let Some(emit) = params.get("emit") {
            files.push(OutFile {
                name: emit["name"].as_str().unwrap_or("out.txt").to_string(),
                format: "text/plain".into(),
                kind: "report".into(),
                result_type: Some("T4".into()),
                bytes: emit["text"].as_str().unwrap_or("").as_bytes().to_vec(),
            });
        }
        Ok(TaskOutput {
            data,
            diagnostics: Vec::new(),
            files,
        })
    }
}

// ---- the limits and the system --------------------------------------------------------------------------------------------------------

/// The limits of the settings that apply to tasks.
#[derive(Clone, Debug)]
pub struct Limits {
    pub enabled: bool,
    /// Tasks that have not finished, all users together.
    pub max_tasks: usize,
    /// Finished tasks kept in the list.
    pub keep_tasks: usize,
    pub max_concurrent: usize,
    pub timeout: Duration,
    /// Tasks that have not finished, one user.
    pub per_user: usize,
    pub params_bytes: usize,
    pub idempotency_ttl_s: u64,
}

impl Limits {
    pub fn from_config(c: &ServerConfig) -> Limits {
        Limits {
            enabled: c.tasks_enabled,
            max_tasks: c.max_tasks,
            keep_tasks: c.keep_tasks,
            max_concurrent: c.max_concurrent_tasks,
            timeout: Duration::from_secs(c.task_timeout_s),
            per_user: c.tasks_per_user,
            params_bytes: c.task_params_kb * 1024,
            idempotency_ttl_s: c.idempotency_ttl_s,
        }
    }
}

/// Everything the server keeps about tasks.
pub struct Tasks {
    pub book: Mutex<TaskBook>,
    /// The cancel flag of each task that is running.
    flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
    pub engines: Engines,
    pub limits: Limits,
}

impl Tasks {
    pub fn open(cfg: &ServerConfig) -> io::Result<Tasks> {
        Ok(Tasks {
            book: Mutex::new(TaskBook::open(&cfg.data_dir)?),
            flags: Mutex::new(HashMap::new()),
            engines: Engines::builtin(cfg.tasks_selftest),
            limits: Limits::from_config(cfg),
        })
    }
}

/// What a request to make a task carries.
#[derive(Deserialize, Debug, Clone)]
pub struct NewTask {
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    /// The artifact the task works on.
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum TaskError {
    Off,
    UnknownKind,
    BadParams(String),
    BadName,
    NoProject,
    TooBig,
    QueueFull,
    EngineMissing,
    Quota,
    IdempotencyConflict,
    UnknownTask,
    Active,
    Storage,
}

impl TaskError {
    pub fn code(&self) -> &'static str {
        match self {
            TaskError::Off => "tasks_off",
            TaskError::UnknownKind => "unknown_kind",
            TaskError::BadParams(_) => "bad_params",
            TaskError::BadName => "bad_name",
            TaskError::NoProject => "no_project",
            TaskError::TooBig => "too_big",
            TaskError::QueueFull => "queue_full",
            TaskError::EngineMissing => "engine_missing",
            TaskError::Quota => "quota",
            TaskError::IdempotencyConflict => "idempotency_conflict",
            TaskError::UnknownTask => "unknown_task",
            TaskError::Active => "task_active",
            TaskError::Storage => "storage",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            TaskError::Off => 501,
            TaskError::UnknownKind
            | TaskError::BadParams(_)
            | TaskError::BadName
            | TaskError::NoProject
            | TaskError::TooBig
            | TaskError::EngineMissing => 422,
            TaskError::QueueFull | TaskError::Quota => 429,
            TaskError::IdempotencyConflict | TaskError::Active => 409,
            TaskError::UnknownTask => 404,
            TaskError::Storage => 500,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            TaskError::BadParams(why) => why.clone(),
            _ => String::new(),
        }
    }
}

fn valid_task_name(name: &str) -> bool {
    !name.trim().is_empty() && name.chars().count() <= 80 && !name.chars().any(char::is_control)
}

fn valid_key(key: &str) -> bool {
    (1..=128).contains(&key.len()) && key.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// The hash that tells whether two requests are the same request: the kind, the name, the project and the parameters, with the parameters in their canonical order.
fn body_hash(kind: &str, name: &str, project: &Option<String>, params: &Value) -> String {
    let canonical = json!({ "kind": kind, "name": name, "project": project, "params": params });
    hex::encode(Sha256::digest(canonical.to_string().as_bytes()))
}

/// How a task ends, as the thread of the engine tells it.
enum Outcome {
    Done(TaskOutput),
    /// Done, with the files stored: the output and the entries of the result that list them.
    Stored(TaskOutput, Vec<(String, Value)>),
    Cancelled,
    Failed(Diagnostic),
}

impl AppState {
    /// Makes a task for `owner` (or finds the one an earlier request with the same idempotency key made). The bool says whether it is the earlier one.
    pub fn submit_task(
        self: &Arc<Self>,
        who: &Identity,
        request: NewTask,
    ) -> Result<(TaskRecord, bool), TaskError> {
        let limits = &self.tasks.limits;
        if !limits.enabled {
            return Err(TaskError::Off);
        }
        let owner = who.owner();
        let params = match request.params.clone() {
            None | Some(Value::Null) => json!({}),
            Some(p) => p,
        };
        if !params.is_object() {
            return Err(TaskError::BadParams("the parameters are an object".into()));
        }
        if params.to_string().len() > limits.params_bytes {
            return Err(TaskError::TooBig);
        }
        let name = request.name.clone().unwrap_or_else(|| request.kind.clone());
        if !valid_task_name(&name) {
            return Err(TaskError::BadName);
        }
        if let Some(key) = &request.idempotency_key {
            if !valid_key(key) {
                return Err(TaskError::BadParams(
                    "idempotency_key is from 1 to 128 printable characters".into(),
                ));
            }
        }
        let hash = body_hash(&request.kind, &name, &request.project, &params);
        // a request sent again gets the answer of the first
        if let Some(key) = &request.idempotency_key {
            let book = self.tasks.book.locked();
            let oldest = now().saturating_sub(limits.idempotency_ttl_s);
            let earlier = book.list().iter().find(|t| {
                t.owner == owner
                    && t.created >= oldest
                    && t.idempotency.as_ref().is_some_and(|i| &i.key == key)
            });
            if let Some(t) = earlier {
                let same = t
                    .idempotency
                    .as_ref()
                    .is_some_and(|i| i.body_sha256 == hash);
                return if same {
                    Ok((t.clone(), true))
                } else {
                    Err(TaskError::IdempotencyConflict)
                };
            }
        }
        if !self.tasks.engines.knows(&request.kind) {
            return Err(TaskError::UnknownKind);
        }
        let engine = self
            .tasks
            .engines
            .get(&request.kind)
            .ok_or(TaskError::EngineMissing)?;
        engine.validate(&params).map_err(TaskError::BadParams)?;
        // the artifact the task works on has to be one the caller can see
        if let Some(project) = &request.project {
            let store = self.artifacts.store.locked();
            let seen = store
                .get(project)
                .is_some_and(|r| self.artifact_visible(r, who));
            if !seen {
                return Err(TaskError::NoProject);
            }
        }
        let record = TaskRecord {
            id: crate::ids::new(crate::ids::Kind::Task),
            kind: request.kind.clone(),
            name,
            owner: owner.clone(),
            state: TaskState::Queued,
            created: now(),
            started: None,
            finished: None,
            params,
            project: request.project.clone(),
            progress: Progress::default(),
            message: None,
            result: None,
            engine_version: engine.version().to_string(),
            idempotency: request.idempotency_key.map(|key| Idempotency {
                key,
                body_sha256: hash,
            }),
        };
        {
            let mut book = self.tasks.book.locked();
            let (all, mine) = book.unfinished(&owner);
            if all >= limits.max_tasks {
                return Err(TaskError::QueueFull);
            }
            if mine >= limits.per_user {
                return Err(TaskError::Quota);
            }
            book.push(record.clone());
            if book.save().is_err() {
                book.remove(&record.id);
                return Err(TaskError::Storage);
            }
        }
        self.publish_task(&record);
        self.tasks_pump();
        let current = self
            .tasks
            .book
            .locked()
            .get(&record.id)
            .cloned()
            .unwrap_or(record);
        Ok((current, false))
    }

    /// Starts queued tasks while there are places for them.
    pub fn tasks_pump(self: &Arc<Self>) {
        loop {
            let started = {
                let mut book = self.tasks.book.locked();
                let running = book
                    .list()
                    .iter()
                    .filter(|t| t.state == TaskState::Running)
                    .count();
                if running >= self.tasks.limits.max_concurrent {
                    return;
                }
                let Some(next) = book
                    .list()
                    .iter()
                    .find(|t| t.state == TaskState::Queued)
                    .map(|t| t.id.clone())
                else {
                    return;
                };
                let flag = Arc::new(AtomicBool::new(false));
                self.tasks.flags.locked().insert(next.clone(), flag.clone());
                let task = book.get_mut(&next).expect("the task was just found");
                task.state = TaskState::Running;
                task.started = Some(now());
                let started = task.clone();
                let _ = book.save();
                (started, flag)
            };
            let (task, flag) = started;
            self.publish_task(&task);
            let state = Arc::clone(self);
            std::thread::spawn(move || state.run_task(task, flag));
        }
    }

    /// The bytes of an artifact, for a task that works on it.
    fn read_artifact_bytes(&self, id: &str) -> Result<Vec<u8>, ()> {
        let store = self.artifacts.store.locked();
        let record = store.get(id).ok_or(())?;
        if record.size > self.artifacts.limits.max_bytes {
            return Err(());
        }
        store.read_blob(&record.blob_sha256).map_err(|_| ())
    }

    /// Stores the files an engine made as artifacts of the owner of the task, with their provenance; gives the entries of the result.
    fn store_task_files(
        &self,
        task: &TaskRecord,
        files: Vec<OutFile>,
    ) -> Result<Vec<(String, Value)>, ()> {
        let mut made: Vec<(String, Value)> = Vec::new();
        let input = task.project.as_ref().and_then(|id| {
            self.artifacts
                .store
                .locked()
                .get(id)
                .map(|r| crate::artifacts::InputRef {
                    artifact_id: r.artifact_id.clone(),
                    blob_sha256: r.blob_sha256.clone(),
                })
        });
        let config = hex::encode(Sha256::digest(task.params.to_string().as_bytes()));
        for f in files {
            let meta = crate::artifacts::NewArtifact {
                name: f.name.clone(),
                format: f.format,
                schema_version: None,
                origin: crate::artifacts::Origin {
                    project: task.project.clone(),
                    revision: None,
                },
                producer: Some(crate::artifacts::Producer {
                    tool: task.kind.clone(),
                    version: task.engine_version.clone(),
                    config_sha256: Some(config.clone()),
                }),
                inputs: input.iter().cloned().collect(),
                params: Some(task.params.clone()),
                result_type: f.result_type.clone(),
            };
            match self.store_artifact(&task.owner, meta, &f.bytes) {
                Ok(record) => made.push((
                    record.artifact_id.clone(),
                    json!({
                        "artifact_id": record.artifact_id, "name": f.name, "kind": f.kind,
                        "result_type": f.result_type,
                    }),
                )),
                Err(_) => {
                    // no partial result: what was stored for this task goes
                    let ids: Vec<String> = made.iter().map(|(id, _)| id.clone()).collect();
                    let _ = self.artifacts.store.locked().remove(&ids);
                    return Err(());
                }
            }
        }
        Ok(made)
    }

    fn run_task(self: Arc<Self>, task: TaskRecord, flag: Arc<AtomicBool>) {
        let report_state = Arc::clone(&self);
        let id = task.id.clone();
        let report = Box::new(move |p: Progress| {
            let snapshot = {
                let mut book = report_state.tasks.book.locked();
                match book.get_mut(&id) {
                    Some(t) if t.state == TaskState::Running => {
                        t.progress = p;
                        Some(t.clone())
                    }
                    _ => None,
                }
            };
            if let Some(t) = snapshot {
                report_state.publish_task(&t);
            }
        });
        let control = TaskControl::new(flag.clone(), self.tasks.limits.timeout, report);
        let project = match &task.project {
            None => Ok(None),
            Some(id) => self.read_artifact_bytes(id).map(Some),
        };
        let outcome = match (self.tasks.engines.get(&task.kind), project) {
            (_, Err(())) => Outcome::Failed(diagnostic(
                "EH3D-TASK-006",
                Severity::Error,
                "the artifact the task works on could not be read",
            )),
            (None, _) => Outcome::Failed(diagnostic(
                "EH3D-TASK-005",
                Severity::Error,
                "the engine is no longer there",
            )),
            (Some(engine), Ok(project)) => {
                let input = TaskInput {
                    params: task.params.clone(),
                    project,
                };
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    engine.run(&input, &control)
                }));
                match ran {
                    Ok(Ok(output)) => Outcome::Done(output),
                    Ok(Err(EngineFailure::Stopped(Stop::Cancelled))) => Outcome::Cancelled,
                    Ok(Err(EngineFailure::Stopped(Stop::TimedOut))) => Outcome::Failed(diagnostic(
                        "EH3D-TASK-002",
                        Severity::Error,
                        "the task ran longer than the limit",
                    )),
                    Ok(Err(EngineFailure::Failed(d))) => Outcome::Failed(d),
                    Err(_) => Outcome::Failed(diagnostic(
                        "EH3D-TASK-005",
                        Severity::Error,
                        "the engine failed unexpectedly",
                    )),
                }
            }
        };
        // the files of a task that was cancelled are not stored at all; a cancel that comes while they are being stored undoes it below
        let outcome = match outcome {
            Outcome::Done(mut output) if !flag.load(Ordering::SeqCst) => {
                let files = std::mem::take(&mut output.files);
                match self.store_task_files(&task, files) {
                    Ok(made) => Outcome::Stored(output, made),
                    Err(()) => Outcome::Failed(diagnostic(
                        "EH3D-TASK-007",
                        Severity::Error,
                        "the files the task made could not be stored",
                    )),
                }
            }
            other => other,
        };
        let finished = {
            let mut book = self.tasks.book.locked();
            // the flag is read under the lock of the book, where a cancel sets it: a task whose cancel was accepted ends cancelled, whatever the engine did
            let cancelled = flag.load(Ordering::SeqCst);
            self.tasks.flags.locked().remove(&task.id);
            if cancelled {
                if let Outcome::Stored(_, made) = &outcome {
                    let ids: Vec<String> = made.iter().map(|(id, _)| id.clone()).collect();
                    let _ = self.artifacts.store.locked().remove(&ids);
                }
            }
            let mut dropped = Vec::new();
            let finished = match book.get_mut(&task.id) {
                None => None,
                Some(t) => {
                    t.finished = Some(now());
                    match outcome {
                        _ if cancelled => {
                            t.state = TaskState::Cancelled;
                            t.message = Some("cancelled".into());
                            t.result = None;
                        }
                        Outcome::Stored(out, made) => {
                            t.state = TaskState::Done;
                            t.message = None;
                            t.result = Some(TaskResult {
                                artifacts: made.into_iter().map(|(_, v)| v).collect(),
                                diagnostics: out.diagnostics,
                                data: out.data,
                            });
                        }
                        Outcome::Done(_) => {
                            unreachable!("the files of a finished task are stored first")
                        }
                        Outcome::Cancelled => {
                            t.state = TaskState::Cancelled;
                            t.message = Some("cancelled".into());
                            t.result = None;
                        }
                        Outcome::Failed(d) => {
                            t.state = TaskState::Failed;
                            t.message = Some(d.message.clone());
                            t.result = Some(TaskResult {
                                diagnostics: vec![d],
                                ..TaskResult::default()
                            });
                        }
                    }
                    Some(t.clone())
                }
            };
            if finished.is_some() {
                dropped = book.prune(self.tasks.limits.keep_tasks);
                let _ = book.save();
            }
            (finished, dropped)
        };
        let (finished, dropped) = finished;
        if let Some(t) = finished {
            self.publish_task(&t);
            self.audit.event(
                "task_end",
                &[
                    ("task", &t.id),
                    ("kind", &t.kind),
                    ("state", t.state.name()),
                ],
            );
        }
        for (id, owner) in dropped {
            self.publish_task_removed(&owner, &id);
        }
        self.tasks_pump();
    }

    /// Asks a task to cancel. A task that is queued is cancelled at once; one that is running is told to stop and ends cancelled (it leaves no result). The bool says
    /// whether the cancel took effect: false when the task had already finished.
    pub fn cancel_task(
        self: &Arc<Self>,
        who: &Identity,
        id: &str,
    ) -> Result<(TaskRecord, bool), TaskError> {
        let (record, accepted) = {
            let mut book = self.tasks.book.locked();
            let task = book
                .get_mut(id)
                .filter(|t| t.visible_to(who))
                .ok_or(TaskError::UnknownTask)?;
            let accepted = match task.state {
                TaskState::Queued => {
                    task.state = TaskState::Cancelled;
                    task.finished = Some(now());
                    task.message = Some("cancelled".into());
                    true
                }
                TaskState::Running => {
                    if let Some(flag) = self.tasks.flags.locked().get(id) {
                        flag.store(true, Ordering::SeqCst);
                    }
                    true
                }
                _ => false,
            };
            let record = task.clone();
            if accepted && record.state == TaskState::Cancelled {
                let _ = book.save();
            }
            (record, accepted)
        };
        if accepted && record.state == TaskState::Cancelled {
            self.publish_task(&record);
        }
        Ok((record, accepted))
    }

    /// Removes a finished task.
    pub fn delete_task(&self, who: &Identity, id: &str) -> Result<(), TaskError> {
        let owner = {
            let mut book = self.tasks.book.locked();
            let task = book
                .get(id)
                .filter(|t| t.visible_to(who))
                .ok_or(TaskError::UnknownTask)?;
            if !task.state.finished() {
                return Err(TaskError::Active);
            }
            let owner = task.owner.clone();
            book.remove(id);
            book.save().map_err(|_| TaskError::Storage)?;
            owner
        };
        self.publish_task_removed(&owner, id);
        Ok(())
    }

    /// Tells the clients who may see the task how it stands.
    pub fn publish_task(&self, task: &TaskRecord) {
        self.publish_scoped(
            Audience::of(&task.owner),
            json!({ "type": "task", "task": task.view() }),
        );
    }

    fn publish_task_removed(&self, owner: &str, id: &str) {
        self.publish_scoped(
            Audience::of(owner),
            json!({ "type": "task_removed", "id": id }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("eh3d-server-tasks-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn sample(id: &str, state: TaskState, owner: &str) -> TaskRecord {
        TaskRecord {
            id: id.into(),
            kind: SELFTEST_KIND.into(),
            name: "n".into(),
            owner: owner.into(),
            state,
            created: 1,
            started: None,
            finished: None,
            params: json!({}),
            project: None,
            progress: Progress::default(),
            message: None,
            result: None,
            engine_version: "1".into(),
            idempotency: None,
        }
    }

    #[test]
    fn a_task_that_was_running_is_failed_after_a_restart_and_a_queued_one_waits() {
        let d = dir("restart");
        let mut book = TaskBook::open(&d).unwrap();
        book.push(sample("t-1", TaskState::Running, "ana"));
        book.push(sample("t-2", TaskState::Queued, "ana"));
        book.push(sample("t-3", TaskState::Done, "ana"));
        book.save().unwrap();
        let again = TaskBook::open(&d).unwrap();
        let one = again.get("t-1").unwrap();
        assert_eq!(one.state, TaskState::Failed);
        assert_eq!(one.message.as_deref(), Some("interrupted by restart"));
        let codes: Vec<_> = one
            .result
            .as_ref()
            .unwrap()
            .diagnostics
            .iter()
            .filter_map(|d| d.code.as_deref())
            .collect();
        assert_eq!(codes, ["EH3D-TASK-004"]);
        assert_eq!(again.get("t-2").unwrap().state, TaskState::Queued);
        assert_eq!(again.get("t-3").unwrap().state, TaskState::Done);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_oldest_finished_tasks_go_first_and_the_unfinished_never() {
        let d = dir("prune");
        let mut book = TaskBook::open(&d).unwrap();
        book.push(sample("t-1", TaskState::Done, "a"));
        book.push(sample("t-2", TaskState::Queued, "a"));
        book.push(sample("t-3", TaskState::Failed, "a"));
        book.push(sample("t-4", TaskState::Cancelled, "a"));
        let dropped: Vec<_> = book.prune(1).into_iter().map(|(id, _)| id).collect();
        assert_eq!(dropped, ["t-1", "t-3"]);
        let left: Vec<_> = book.list().iter().map(|t| t.id.as_str()).collect();
        assert_eq!(left, ["t-2", "t-4"]);
        assert_eq!(book.unfinished("a"), (1, 1));
        assert_eq!(book.unfinished("b"), (1, 0));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn visibility_is_the_owner_and_the_administrators() {
        let t = sample("t-1", TaskState::Done, "ana");
        let person = |name: &str, role| Identity {
            name: name.into(),
            role,
            is_token: false,
            token_id: None,
            scopes: None,
            template: None,
        };
        assert!(t.visible_to(&person("ana", Role::Viewer)));
        assert!(!t.visible_to(&person("bob", Role::Operator)));
        assert!(t.visible_to(&person("bob", Role::Admin)));
        // a program token that carries the name of a user is not that user
        let token = Identity {
            name: "ana".into(),
            role: Role::Operator,
            is_token: true,
            token_id: Some("ab12".into()),
            scopes: None,
            template: None,
        };
        assert!(!t.visible_to(&token));
        assert_eq!(token.owner(), "token:ab12");
    }

    #[test]
    fn the_selftest_engine_checks_its_parameters() {
        let e = SelfTest;
        assert!(e.validate(&json!({})).is_ok());
        assert!(e.validate(&json!({"steps": 0})).is_err());
        assert!(e.validate(&json!({"steps": 1001})).is_err());
        assert!(e.validate(&json!({"step_ms": 10_001})).is_err());
        assert!(e.validate(&json!({"text": 5})).is_err());
        assert!(e.validate(&json!({"other": 1})).is_err());
        assert!(e.validate(&json!([1])).is_err());
    }

    #[test]
    fn the_hash_of_a_request_ignores_the_order_of_the_parameters() {
        let a = body_hash("k", "n", &None, &json!({"a": 1, "b": 2}));
        let b = body_hash("k", "n", &None, &json!({"b": 2, "a": 1}));
        let c = body_hash("k", "n", &None, &json!({"a": 1, "b": 3}));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn the_kinds_list_says_which_are_available() {
        let none = Engines::builtin(false).kinds();
        assert_eq!(none.len(), KNOWN_KINDS.len());
        assert!(none.iter().all(|k| k["available"] == json!(false)));
        let with = Engines::builtin(true).kinds();
        assert_eq!(with.len(), KNOWN_KINDS.len() + 1);
        assert!(with
            .iter()
            .any(|k| k["kind"] == json!(SELFTEST_KIND) && k["available"] == json!(true)));
        assert!(Engines::builtin(false).knows("phoenix.check"));
        assert!(!Engines::builtin(false).knows(SELFTEST_KIND));
    }

    /// The architecture test: the code of the tasks cannot reach a machine, a connection or a job, because it does not even name them.
    #[test]
    fn engines_cannot_reach_a_machine() {
        let forbidden = [
            "crate::machines",
            "crate::head",
            "crate::gcode",
            "crate::gcode_run",
            "crate::plan",
            "crate::spindle",
            "crate::emergency",
            "crate::inspect",
            "eh3d_controller",
            "eh3d_drillpro",
            "eh3d_routepro",
            "eh3d_inspect",
            "eh3d_fleet",
        ];
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        // `jobs::now` is the clock; the check looks at the use of anything else of the module
        for file in ["tasks.rs", "artifacts.rs"] {
            let path = root.join(file);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let code = text.split("#[cfg(test)]").next().unwrap();
            for word in forbidden {
                assert!(!code.contains(word), "{file} names {word}");
            }
            let uses: Vec<_> = code
                .lines()
                .filter(|l| l.trim_start().starts_with("use crate::jobs"))
                .collect();
            assert!(
                uses.iter().all(|l| l.trim() == "use crate::jobs::now;"),
                "{file} uses more of crate::jobs than the clock: {uses:?}"
            );
        }
        let cargo = std::fs::read_to_string(root.parent().unwrap().join("Cargo.toml")).unwrap();
        assert!(cargo.contains("eh3d-diagnostic"));
    }
}
