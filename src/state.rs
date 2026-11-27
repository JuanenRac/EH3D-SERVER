//! What the server is made of while it runs: the users, the machines, the jobs, the jobs that are running and the channel that tells every
//! client what changes. The running of a job is the library of EH3D-DRILLPRO (`RealRun`: its own thread, the handshake, the arming, the items,
//! the stop); the server only starts it, follows it and relays what it says.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eh3d_drillpro::protocol::job::Pause;
use eh3d_drillpro::runner::{RealRun, RunState};
use tokio::sync::broadcast;

use crate::auth::Auth;
use crate::config::ServerConfig;
use crate::gcode_run::{GState, GcodeRun, Hold, RunOptions};
use crate::jobs::{new_question_id, now, JobBook, JobRecord, JobState, PauseView};
use crate::machines::Machines;
use crate::plan::{self, JobRequest};
use crate::sync::Locked;

/// Who an event is for besides the administrators: the owners it names.
#[derive(Clone, Debug, PartialEq)]
pub struct Audience {
    owners: Vec<String>,
}

impl Audience {
    /// The administrators and the owner with this key.
    pub fn of(owner: &str) -> Audience {
        Audience {
            owners: vec![owner.to_string()],
        }
    }

    /// The administrators and each of these owners.
    pub fn of_all<I: IntoIterator<Item = String>>(owners: I) -> Audience {
        Audience {
            owners: owners.into_iter().collect(),
        }
    }

    pub fn includes(&self, who: &crate::auth::Identity) -> bool {
        who.role == crate::auth::Role::Admin || self.owners.iter().any(|o| *o == who.owner())
    }
}

/// An event that only some clients may receive.
#[derive(Clone, Debug)]
pub struct ScopedEvent {
    pub audience: Audience,
    pub text: String,
}

pub struct AppState {
    pub cfg: ServerConfig,
    /// When the server started (for the uptime the health check tells to those who are logged in).
    pub started: std::time::Instant,
    pub auth: Mutex<Auth>,
    pub machines: Mutex<Machines>,
    pub jobs: Mutex<JobBook>,
    /// The jobs that are running, by the id of the job.
    pub runs: Mutex<HashMap<String, Arc<RealRun>>>,
    /// The G-code jobs that are running, by the id of the job.
    pub gcode_runs: Mutex<HashMap<String, Arc<GcodeRun>>>,
    /// The identifier of the question each waiting job asks, by the id of the job; one that was answered is no longer here.
    questions: Mutex<HashMap<String, String>>,
    /// Every change, as a JSON text, for the clients that listen (the websocket).
    pub events: broadcast::Sender<String>,
    /// The events about things that have an owner (tasks, artifacts): each client gets only those of the things it may see.
    pub scoped: broadcast::Sender<ScopedEvent>,
    /// The computing tasks (they have no effect on any machine).
    pub tasks: crate::tasks::Tasks,
    /// The artifacts, the blobs and the uploads.
    pub artifacts: crate::artifacts::Artifacts,
    /// The projects, their revisions and who they are shared with.
    pub projects: crate::projects::Projects,
    /// The projects and the files of every application (`/api/v1/apps/:app/projects`).
    pub pstore: crate::pstore::ProjectStore,
    /// The shared catalogs (`/api/v1/catalogs`).
    pub catalogs: crate::catalogs::Catalogs,
    /// The machine plugins (`bungard`, `demo`, `laser`, `pnp`, `pcb`) the generic queue talks to.
    pub plugins: crate::plugins::Registry,
    /// The machines of the plugins, the generic queue of jobs and the notifications.
    pub work: crate::work::Work,
    /// The events of the common services, with their numbers, for the stream and for the clients that reconnect.
    pub feed: crate::feed::Feed,
    /// The updates of the installed services, when the settings give the folder for them.
    pub updates: Option<crate::updates::Updates>,
    /// The last check of the work through the camera of the drill head, by the id of the machine.
    pub inspections: Mutex<HashMap<String, serde_json::Value>>,
    /// The tickets that open the camera and the websocket without the token in the address.
    pub tickets: Mutex<crate::tickets::Tickets>,
    /// What happens, as lines of the audit (who did what).
    pub audit: crate::audit::Audit,
    /// The line at the start and at the end of every job, for the diagnosis of a failure afterwards.
    pub job_log: crate::audit::LogFile,
    /// The switches of security that can change while the server runs (`PUT /api/v1/auth/policy`).
    pub policy: crate::policy::Policy,
    /// The keys of the compatibility routes of Moonraker and OctoPrint, by machine.
    pub compat_keys: crate::compat::CompatKeys,
    /// The nodes of the pick-and-place machines (feeders and heads) and what they said last.
    pub pnp_nodes: crate::pnp_nodes::PnpNodes,
    /// The 3D printers the server watches (read only) and what they said last.
    pub printers: crate::printers::Printers,
}

#[derive(Debug, PartialEq)]
pub enum StartError {
    UnknownJob,
    UnknownMachine,
    /// The job is not ready (it already ran, or it is running).
    NotReady,
    /// Another job is running on this machine.
    MachineBusy,
    /// The job no longer plans with the machine's present constants.
    Plan(&'static str, String),
}

#[derive(Debug, PartialEq)]
pub enum PauseError {
    UnknownJob,
    /// Only a G-code job can be held and resumed.
    NotGcode,
    /// The job is not in the state this needs (running to hold it, held to resume it).
    NotInState,
}

/// Why the answer of a person to the question of a job was not taken.
#[derive(Debug, PartialEq)]
pub enum AnswerError {
    /// The setting `answer_needs_question` is on and the answer names no question.
    QuestionRequired,
    /// The question named is not the one the job asks now.
    QuestionChanged,
    Pause(PauseError),
}

/// The longest an answer waits for the run to leave the question it answered.
const ANSWER_WAIT: Duration = Duration::from_secs(1);

/// Gives the question a job stops at its identifier, and forgets it once the job goes on. The caller holds `open` from the reading of the run.
fn name_question(open: &mut HashMap<String, String>, id: &str, pause: &mut Option<PauseView>) {
    match pause {
        Some(p) => {
            p.question = open
                .entry(id.to_string())
                .or_insert_with(new_question_id)
                .clone();
        }
        None => {
            open.remove(id);
        }
    }
}

fn wait_until_left(left: impl Fn() -> bool) {
    let end = Instant::now() + ANSWER_WAIT;
    while !left() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Lets a held G-code run go on and waits until it has left the wait.
fn release(run: &GcodeRun) {
    let asked = run.state();
    run.resume();
    wait_until_left(|| run.state() != asked);
}

/// Gives the logins the limits and the rules of the passwords in force.
pub fn apply_policy(auth: &mut Auth, hot: &crate::policy::Hot) {
    auth.set_ip_limit(hot.login_ip_limit);
    auth.set_lockout(hot.auth_max_failures, hot.auth_lockout_s);
    auth.set_refresh_max_hours(hot.auth_refresh_max_hours);
    auth.set_password_policy(crate::auth::PasswordPolicy {
        min_len: hot.password_min_len,
        classes: hot.password_classes,
        deny_name: hot.password_deny_name,
    });
    // the keeping of the logins on the disk is a switch that takes effect at once: on, the logins that are open are written now; off, the file goes
    if hot.persist_sessions != auth.persists_sessions() {
        auth.set_persist_sessions(hot.persist_sessions);
    }
}

impl AppState {
    pub fn open(cfg: ServerConfig) -> io::Result<Arc<AppState>> {
        std::fs::create_dir_all(&cfg.data_dir)?;
        let mut auth = Auth::open(&cfg.data_dir, cfg.session_hours)?;
        let policy = crate::policy::Policy::open(&cfg, &cfg.data_dir)?;
        apply_policy(&mut auth, &policy.get());
        auth.set_persist_sessions(policy.get().persist_sessions);
        let kept = auth.restore_after_restart(policy.get().update_keep_logins, crate::jobs::now());
        if kept > 0 {
            eprintln!("eh3d-server: {kept} login(s) kept across the update");
        }
        crate::plan::set_rack_max_slots(cfg.rack_max_slots);
        let machines = Machines::open(&cfg.data_dir)?;
        let jobs = JobBook::open(&cfg.data_dir, cfg.keep_jobs)?;
        let (events, _) = broadcast::channel(256);
        let (scoped, _) = broadcast::channel(256);
        let tasks = crate::tasks::Tasks::open(&cfg)?;
        let artifacts = crate::artifacts::Artifacts::open(&cfg)?;
        let projects = crate::projects::Projects {
            book: Mutex::new(crate::projects::ProjectBook::open(&cfg.data_dir)?),
            shares_enabled: cfg.shares_enabled,
        };
        let pstore = crate::pstore::ProjectStore::open(&cfg)?;
        let catalogs = crate::catalogs::Catalogs::open(&cfg)?;
        let work = crate::work::Work::open(&cfg)?;
        let feed = crate::feed::Feed::new(cfg.events_queue, cfg.events_clients_max);
        let audit = crate::audit::Audit::new(&cfg.data_dir, cfg.audit_file, cfg.log_max_kb);
        let job_log = crate::audit::LogFile::new(
            cfg.data_dir.join("eh3d-jobs.log"),
            cfg.job_log,
            cfg.log_max_kb,
        );
        let compat_keys = crate::compat::CompatKeys::open(&cfg.data_dir)?;
        let pnp_nodes = crate::pnp_nodes::PnpNodes::open(&cfg)?;
        let printers = crate::printers::Printers::open(&cfg)?;
        let updates = cfg.updates_dir.clone().map(|d| {
            crate::updates::Updates::new(d, env!("CARGO_PKG_VERSION"), cfg.studio_dir.clone())
        });
        let state = Arc::new(AppState {
            cfg,
            started: std::time::Instant::now(),
            auth: Mutex::new(auth),
            machines: Mutex::new(machines),
            jobs: Mutex::new(jobs),
            runs: Mutex::new(HashMap::new()),
            gcode_runs: Mutex::new(HashMap::new()),
            questions: Mutex::new(HashMap::new()),
            events,
            scoped,
            tasks,
            artifacts,
            projects,
            pstore,
            catalogs,
            plugins: crate::plugins::Registry::standard(),
            work,
            feed,
            updates,
            inspections: Mutex::new(HashMap::new()),
            tickets: Mutex::new(crate::tickets::Tickets::default()),
            audit,
            job_log,
            policy,
            compat_keys,
            pnp_nodes,
            printers,
        });
        // the tasks that were queued when the server stopped go on, and what waited long enough is cleaned
        state.tasks_pump();
        state.start_work_thread();
        state.artifact_housekeeping();
        Ok(state)
    }

    /// Tells the clients that listen. Nobody listening is not a problem.
    pub fn publish(&self, value: serde_json::Value) {
        let _ = self.events.send(value.to_string());
    }

    /// Tells the clients that may see the thing it is about.
    pub fn publish_scoped(&self, audience: Audience, value: serde_json::Value) {
        let _ = self.scoped.send(ScopedEvent {
            audience,
            text: value.to_string(),
        });
    }

    pub fn publish_job(&self, job: &JobRecord) {
        self.publish(serde_json::json!({ "type": "job", "job": job.summary() }));
    }

    /// Starts a ready job on its machine: plans it again with the machine's constants of this moment, opens the link and runs.
    pub fn start_job(self: &Arc<Self>, id: &str, by: &str) -> Result<(), StartError> {
        let (machine_id, request) = {
            let jobs = self.jobs.locked();
            let job = jobs.get(id).ok_or(StartError::UnknownJob)?;
            if job.state != JobState::Ready {
                return Err(StartError::NotReady);
            }
            if jobs.active_on(&job.machine).is_some() {
                return Err(StartError::MachineBusy);
            }
            (job.machine.clone(), job.request.clone())
        };
        let machine = self
            .machines
            .locked()
            .get(&machine_id)
            .cloned()
            .ok_or(StartError::UnknownMachine)?;
        let request: JobRequest = serde_json::from_value(request)
            .map_err(|e| StartError::Plan("bad_request", e.to_string()))?;
        let planned =
            plan::plan(&machine, &request).map_err(|e| StartError::Plan(e.code, e.detail))?;
        if let Some(program) = planned.gcode {
            // where the program was, for the sequence of a job that goes on (`plan` has already refused one that is not certain)
            let back = match &request {
                JobRequest::Gcode(g) if program.resume => (
                    crate::gcode::resume_point(&g.gcode, g.skip)
                        .map_err(|why| StartError::Plan("bad_request", why))?,
                    g.restore_outputs,
                ),
                _ => Default::default(),
            };
            return self.start_gcode(id, &machine, planned.view, program, back, by);
        }
        let config = machine.connection_config().map_err(|e| {
            StartError::Plan(e.code(), "the machine's tuning does not parse".into())
        })?;
        let hole_ends = planned.view.hole_ends.clone();
        let started_line;
        let items_total = planned.items.len();

        // the job is marked running before the thread starts, under the same lock that checked the machine was free
        {
            let mut jobs = self.jobs.locked();
            if jobs.active_on(&machine_id).is_some() {
                return Err(StartError::MachineBusy);
            }
            let job = jobs.get_mut(id).ok_or(StartError::UnknownJob)?;
            // the plan took time: a job cancelled or removed meanwhile must not start
            if job.state != JobState::Ready {
                return Err(StartError::NotReady);
            }
            job.state = JobState::Connecting;
            job.started = Some(now());
            job.items_total = items_total;
            job.items_done = 0;
            job.holes_done = 0;
            job.view = planned.view.clone();
            job.message.clear();
            job.pause = None;
            job.started_by = by.to_string();
            job.stopped_by.clear();
            started_line = start_line(job, &machine);
            self.publish_job(job);
            jobs.changed();
        }
        self.job_log.append(&started_line);
        let run = Arc::new(RealRun::start(config, planned.items));
        self.runs.locked().insert(id.to_string(), run.clone());
        if self.stop_was_asked(id) {
            run.cancel();
        }
        let state = Arc::clone(self);
        let job_id = id.to_string();
        std::thread::spawn(move || state.follow(&job_id, run, &hole_ends));
        Ok(())
    }

    /// Starts a G-code job: the thread of its run sends the lines and the server follows it.
    fn start_gcode(
        self: &Arc<Self>,
        id: &str,
        machine: &crate::machines::MachineRecord,
        view: crate::plan::JobView,
        program: crate::gcode::Program,
        (point, restore_outputs): (crate::gcode::ResumePoint, bool),
        by: &str,
    ) -> Result<(), StartError> {
        let started_line;
        {
            let mut jobs = self.jobs.locked();
            if jobs.active_on(&machine.id).is_some() {
                return Err(StartError::MachineBusy);
            }
            let job = jobs.get_mut(id).ok_or(StartError::UnknownJob)?;
            // the plan took time: a job cancelled or removed meanwhile must not start
            if job.state != JobState::Ready {
                return Err(StartError::NotReady);
            }
            job.state = JobState::Connecting;
            job.started = Some(now());
            job.items_total = view.items;
            job.items_done = 0;
            job.holes_done = 0;
            job.line = 0;
            job.stop = None;
            job.view = view;
            job.message.clear();
            job.pause = None;
            job.started_by = by.to_string();
            job.stopped_by.clear();
            started_line = start_line(job, machine);
            self.publish_job(job);
            jobs.changed();
        }
        self.job_log.append(&started_line);
        let mut program = program;
        let resume_at_mm = self.jobs.locked().get(id).and_then(|j| j.resume_at_mm);
        if program.resume {
            let moves = crate::gcode::ResumeMoves {
                safe_z_mm: self.cfg.resume_safe_z_mm as f64,
                plunge_feed_mm_min: self.cfg.resume_plunge_feed_mm_min as f64,
                laser: machine.is_laser(),
            };
            crate::gcode::with_resume_sequence(&mut program, &point, moves, restore_outputs);
        }
        let options = RunOptions {
            spindle_wait_after: program.spindle_wait_after,
            spindle_wait: Duration::from_secs(self.cfg.resume_spindle_wait_s),
            power_on_hold: program.power_on_at.filter(|_| machine.confirms_power_on()),
            plunge_hold: program.plunge_at,
            stop_wait: Duration::from_secs(self.cfg.grbl_stop_wait_s),
            stop_spindle_on_pause: self.cfg.grbl_spindle_stop_on_pause,
            expected_mm: resume_at_mm.filter(|_| program.resume),
            tolerance_mm: self.cfg.resume_position_tolerance_um as f64 / 1000.0,
        };
        let run = GcodeRun::start_with(
            machine.connection.clone(),
            program.entries,
            program.preamble,
            program.raw_total,
            program.resume,
            Duration::from_secs(self.cfg.gcode_finish_wait_s),
            self.cfg.grbl_timeouts(),
            options,
        );
        self.gcode_runs.locked().insert(id.to_string(), run.clone());
        if self.stop_was_asked(id) {
            run.cancel();
        }
        let state = Arc::clone(self);
        let job_id = id.to_string();
        std::thread::spawn(move || state.follow_gcode(&job_id, run));
        Ok(())
    }

    /// Watches a running G-code job and keeps the book and the clients up to date until it finishes.
    fn follow_gcode(&self, id: &str, run: Arc<GcodeRun>) {
        let mut last: Option<(JobState, usize, usize, Option<PauseView>, String)> = None;
        loop {
            // the run is read with the questions held, so that an answer cannot fall between what is read and the name it is given
            let mut open = self.questions.locked();
            let (state, message, mut pause) = match run.state() {
                GState::Connecting => (JobState::Connecting, String::new(), None),
                GState::Running => (JobState::Running, String::new(), None),
                GState::Held(hold) => (
                    JobState::WaitingOperator,
                    String::new(),
                    Some(PauseView {
                        kind: match hold {
                            Hold::Operator => "hold",
                            Hold::Resume | Hold::PowerOn | Hold::Plunge => "resume",
                        }
                        .into(),
                        tool: None,
                        question: String::new(),
                        power_on: hold == Hold::PowerOn,
                        plunge: hold == Hold::Plunge,
                    }),
                ),
                GState::Done => (JobState::Done, String::new(), None),
                GState::Cancelled => (JobState::Cancelled, String::new(), None),
                GState::Failed(why) => (JobState::Failed, why, None),
            };
            name_question(&mut open, id, &mut pause);
            drop(open);
            let (done, total) = run.progress();
            let items_done = if state == JobState::Done { total } else { done };
            let line = run.line();
            // the end of the job says what is known of its machine, and why when the stop was not confirmed
            let stop = state.finished().then(|| run.stop_record()).flatten();
            let message = match &stop {
                Some(s) if !s.known && !s.why.is_empty() => {
                    let note = format!("the position of the machine is not known: {}", s.why);
                    if message.is_empty() {
                        note
                    } else {
                        format!("{message}; {note}")
                    }
                }
                _ => message,
            };
            let now_view = (state, items_done, line, pause.clone(), message.clone());
            if last.as_ref() != Some(&now_view) {
                let mut ended = None;
                let mut jobs = self.jobs.locked();
                if let Some(job) = jobs.get_mut(id) {
                    job.state = state;
                    job.items_done = items_done;
                    job.line = line;
                    job.pause = pause;
                    job.message = message;
                    if state.finished() {
                        job.stop = stop;
                        job.finished = Some(now());
                        ended = Some((end_line(job), job.stopped_by.clone()));
                    }
                    self.publish_job(job);
                    if state != JobState::Running
                        || last.as_ref().map(|l| l.0) != Some(JobState::Running)
                    {
                        jobs.changed();
                    }
                }
                drop(jobs);
                if let Some((text, stopped_by)) = ended {
                    self.job_log.append(&text);
                    self.audit.event(
                        "job_end",
                        &[
                            ("job", id),
                            ("result", &state_name(state)),
                            ("stopped_by", &stopped_by),
                        ],
                    );
                }
                last = Some(now_view);
            }
            if state.finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        self.gcode_runs.locked().remove(id);
        self.jobs.locked().changed();
    }

    /// Holds a running G-code job where it is.
    pub fn pause_job(&self, id: &str) -> Result<(), PauseError> {
        self.gcode_in(id, JobState::Running)?.pause();
        Ok(())
    }

    /// Lets a job that the operator held go on: for the routes of the work queue and of the programs, which carry no question. It never answers
    /// the question of a job that goes on after a stop.
    pub fn resume_job(&self, id: &str) -> Result<(), PauseError> {
        let run = self.gcode_in(id, JobState::WaitingOperator)?;
        let mut open = self.questions.locked();
        if run.state() != GState::Held(Hold::Operator) {
            return Err(PauseError::NotInState);
        }
        open.remove(id);
        release(&run);
        Ok(())
    }

    /// A person lets a waiting G-code job go on, naming (or not) the question they answer.
    pub fn answer_resume(&self, id: &str, question: Option<&str>) -> Result<(), AnswerError> {
        let run = self
            .gcode_in(id, JobState::WaitingOperator)
            .map_err(AnswerError::Pause)?;
        let mut open = self.questions.locked();
        // a confirmation sent twice, or late, by a program that names no question must not switch the spindle or the laser on, nor take the tool down
        if question.is_none() && matches!(run.state(), GState::Held(Hold::PowerOn | Hold::Plunge)) {
            return Err(AnswerError::QuestionRequired);
        }
        if !self.take_question(&mut open, id, question)? {
            return Err(AnswerError::Pause(PauseError::NotInState));
        }
        release(&run);
        Ok(())
    }

    /// The run of a G-code job that the book has in the state asked for.
    fn gcode_in(&self, id: &str, wanted: JobState) -> Result<Arc<GcodeRun>, PauseError> {
        let state = {
            let jobs = self.jobs.locked();
            let job = jobs.get(id).ok_or(PauseError::UnknownJob)?;
            if job.kind != "gcode" {
                return Err(PauseError::NotGcode);
            }
            job.state
        };
        if state != wanted {
            return Err(PauseError::NotInState);
        }
        self.gcode_runs
            .locked()
            .get(id)
            .cloned()
            .ok_or(PauseError::NotInState)
    }

    /// Checks an answer against the question the job asks and takes the question, so that it is answered once. `Ok(false)` when no question is
    /// named yet or the one asked was already answered.
    fn take_question(
        &self,
        open: &mut HashMap<String, String>,
        id: &str,
        question: Option<&str>,
    ) -> Result<bool, AnswerError> {
        match question {
            Some(q) if open.get(id).map(String::as_str) != Some(q) => {
                Err(AnswerError::QuestionChanged)
            }
            None if self.cfg.answer_needs_question => Err(AnswerError::QuestionRequired),
            _ => Ok(open.remove(id).is_some()),
        }
    }

    /// Watches a running job and keeps the book and the clients up to date until it finishes.
    fn follow(&self, id: &str, run: Arc<RealRun>, hole_ends: &[usize]) {
        let mut last: Option<(JobState, usize, usize, Option<PauseView>, String)> = None;
        loop {
            let mut open = self.questions.locked();
            let (state, message, mut pause) = match run.state() {
                RunState::Connecting => (JobState::Connecting, String::new(), None),
                RunState::Running => (JobState::Running, String::new(), None),
                RunState::WaitPause(p) => (
                    JobState::WaitingOperator,
                    String::new(),
                    Some(match p {
                        Pause::ToolChange { tool } => PauseView {
                            kind: "tool_change".into(),
                            tool: Some(tool),
                            question: String::new(),
                            power_on: false,
                            plunge: false,
                        },
                        Pause::SpindleStart => PauseView {
                            kind: "spindle_start".into(),
                            tool: None,
                            question: String::new(),
                            power_on: false,
                            plunge: false,
                        },
                    }),
                ),
                RunState::Done => (JobState::Done, String::new(), None),
                RunState::Cancelled => (JobState::Cancelled, String::new(), None),
                RunState::Failed(why) => (JobState::Failed, why, None),
            };
            name_question(&mut open, id, &mut pause);
            drop(open);
            let (next, _) = run.progress();
            let holes_done = hole_ends.iter().filter(|&&e| e < next).count();
            let items_done = if state == JobState::Done {
                run.progress().1
            } else {
                next
            };
            let now_view = (
                state,
                items_done,
                holes_done,
                pause.clone(),
                message.clone(),
            );
            if last.as_ref() != Some(&now_view) {
                let mut ended = None;
                let mut jobs = self.jobs.locked();
                if let Some(job) = jobs.get_mut(id) {
                    job.state = state;
                    job.items_done = items_done;
                    job.holes_done = if state == JobState::Done {
                        job.holes_total.max(holes_done)
                    } else {
                        holes_done
                    };
                    job.pause = pause;
                    job.message = message;
                    if state.finished() {
                        job.finished = Some(now());
                        ended = Some((end_line(job), job.stopped_by.clone()));
                    }
                    self.publish_job(job);
                    // the book is written when something that matters changes, not at every hole
                    if state != JobState::Running
                        || last.as_ref().map(|l| l.0) != Some(JobState::Running)
                    {
                        jobs.changed();
                    }
                }
                drop(jobs);
                if let Some((text, stopped_by)) = ended {
                    self.job_log.append(&text);
                    self.audit.event(
                        "job_end",
                        &[
                            ("job", id),
                            ("result", &state_name(state)),
                            ("stopped_by", &stopped_by),
                        ],
                    );
                }
                last = Some(now_view);
            }
            if state.finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        self.runs.locked().remove(id);
        self.jobs.locked().changed();
    }

    /// The operator answers the dialog the job is waiting at, naming (or not) the question they answer. `NotInState` when the job is not running.
    pub fn confirm_job(&self, id: &str, question: Option<&str>) -> Result<(), AnswerError> {
        // the dialog of a G-code job is the hold: confirming it lets the job go on
        if self.gcode_runs.locked().contains_key(id) {
            return self.answer_resume(id, question);
        }
        let run = self
            .runs
            .locked()
            .get(id)
            .cloned()
            .ok_or(AnswerError::Pause(PauseError::NotInState))?;
        let mut open = self.questions.locked();
        // an answer to a question that nobody has been shown does nothing: it must not answer the one that comes next
        if self.take_question(&mut open, id, question)? {
            let asked = run.state();
            run.confirm();
            wait_until_left(|| run.state() != asked);
        }
        Ok(())
    }

    /// Stops a running job (the machine stops with the drivers off); a job that is only ready is cancelled where it stands.
    pub fn cancel_job(&self, id: &str, by: &str) -> bool {
        let running =
            self.gcode_runs.locked().contains_key(id) || self.runs.locked().contains_key(id);
        if running {
            if let Some(job) = self.jobs.locked().get_mut(id) {
                job.stopped_by = by.to_string();
            }
        }
        if let Some(run) = self.gcode_runs.locked().get(id) {
            run.cancel();
            return true;
        }
        if let Some(run) = self.runs.locked().get(id) {
            run.cancel();
            return true;
        }
        let mut jobs = self.jobs.locked();
        match jobs.get_mut(id) {
            Some(job) if job.state == JobState::Ready => {
                job.state = JobState::Cancelled;
                job.stopped_by = by.to_string();
                job.finished = Some(now());
                self.publish_job(job);
                jobs.changed();
                true
            }
            // the job was marked as connecting but its run is not registered yet: the stop is written down and the start looks for it once the run exists; if the run
            // appeared while this was being decided, it is stopped here (one of the two always sees the other)
            Some(job) if job.state == JobState::Connecting => {
                job.stopped_by = by.to_string();
                drop(jobs);
                if let Some(run) = self.gcode_runs.locked().get(id) {
                    run.cancel();
                }
                if let Some(run) = self.runs.locked().get(id) {
                    run.cancel();
                }
                true
            }
            _ => false,
        }
    }

    /// Whether a stop arrived for the job while its run was being started (see `cancel_job`).
    fn stop_was_asked(&self, id: &str) -> bool {
        self.jobs
            .locked()
            .get(id)
            .is_some_and(|job| !job.stopped_by.is_empty())
    }

    /// The stop of a machine: the job that is active on it, if any, is stopped at once (a GRBL machine gets the real-time hold and reset on the port; a
    /// node gets its drivers switched off). The id of the job, or `None` when the machine had no job of this server.
    pub fn stop_machine(&self, machine: &str, by: &str) -> Option<String> {
        let id = self
            .jobs
            .locked()
            .active_on(machine)
            .map(|j| j.id.clone())?;
        self.cancel_job(&id, by);
        Some(id)
    }

    /// Stops every running job: for the end of the server, which never leaves a machine moving on its own.
    pub fn stop_all(&self) {
        {
            let mut jobs = self.jobs.locked();
            let active: Vec<String> = jobs
                .list()
                .iter()
                .filter(|j| j.state.active())
                .map(|j| j.id.clone())
                .collect();
            for id in active {
                if let Some(job) = jobs.get_mut(&id) {
                    job.stopped_by = "server".into();
                }
            }
        }
        let runs: Vec<Arc<RealRun>> = self.runs.locked().values().cloned().collect();
        let gcode: Vec<Arc<GcodeRun>> = self.gcode_runs.locked().values().cloned().collect();
        for run in &runs {
            run.cancel();
        }
        for run in &gcode {
            run.cancel();
        }
        // a stop of a G-code job waits for the machine to stand still before its reset: the server does not leave before it is out
        for _ in 0..50.max((self.cfg.grbl_stop_wait_s + 2) * 10) {
            if runs.iter().all(|r| r.finished()) && gcode.iter().all(|r| r.finished()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn state_name(state: JobState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The line of the log of the jobs at the start of a job: what was asked, of which file, for which machine and by whom.
fn start_line(job: &JobRecord, machine: &crate::machines::MachineRecord) -> String {
    use crate::machines::Connection;
    use sha2::{Digest, Sha256};
    let request = job.request.to_string();
    let hash = hex::encode(&Sha256::digest(request.as_bytes())[..4]);
    let connection = match &machine.connection {
        Connection::Demo => "demo".to_string(),
        Connection::Node { address, .. } => format!("node {address}"),
        Connection::Grbl { port, baud } => format!("grbl {port}@{baud}"),
    };
    let work = match job.kind.as_str() {
        "drill" => "holes",
        "route" => "paths",
        _ => "lines",
    };
    let count = if job.holes_total > 0 {
        job.holes_total
    } else {
        job.items_total
    };
    let fields: [(&str, &str); 14] = [
        ("job", job.id.as_str()),
        ("name", job.name.as_str()),
        ("kind", job.kind.as_str()),
        ("machine", job.machine.as_str()),
        ("connection", connection.as_str()),
        ("by", job.by.as_str()),
        ("started_by", job.started_by.as_str()),
        ("request_bytes", &request.len().to_string()),
        ("request_hash", hash.as_str()),
        (work, &count.to_string()),
        ("items", &job.items_total.to_string()),
        ("skipped", &job.view.skipped.to_string()),
        ("estimate_s", &format!("{:.0}", job.view.estimate_s)),
        ("server", env!("CARGO_PKG_VERSION")),
    ];
    format!(
        "{} start {}",
        crate::audit::iso(job.started.unwrap_or_else(now)),
        crate::audit::pairs(&fields)
    )
}

/// The line of the log of the jobs at the end of a job: how it ended, where it was, and the real time against the estimate.
fn end_line(job: &JobRecord) -> String {
    let finished = job.finished.unwrap_or_else(now);
    let duration = finished.saturating_sub(job.started.unwrap_or(finished));
    let fields: [(&str, &str); 11] = [
        ("job", job.id.as_str()),
        ("result", &state_name(job.state)),
        ("message", job.message.as_str()),
        ("items_done", &job.items_done.to_string()),
        ("items", &job.items_total.to_string()),
        ("holes_done", &job.holes_done.to_string()),
        ("line", &job.line.to_string()),
        ("started_by", job.started_by.as_str()),
        ("stopped_by", job.stopped_by.as_str()),
        ("duration_s", &duration.to_string()),
        ("estimate_s", &format!("{:.0}", job.view.estimate_s)),
    ];
    format!(
        "{} end {}",
        crate::audit::iso(finished),
        crate::audit::pairs(&fields)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_lines_of_a_job_tell_what_a_failure_needs() {
        let mut job = crate::jobs::JobRecord::sample("ab12", JobState::Running);
        job.name = "my board".into();
        job.started = Some(951_782_400);
        job.started_by = "juan".into();
        let machine: crate::machines::MachineRecord = serde_json::from_value(serde_json::json!({
            "id": "m1", "name": "M1", "connection": {"kind": "node", "address": "10.0.0.5:80", "token": "secret"}
        }))
        .unwrap();
        let start = start_line(&job, &machine);
        assert!(start.starts_with("2000-02-29T00:00:00Z start job=ab12 name=\"my board\""));
        assert!(start.contains("connection=\"node 10.0.0.5:80\""));
        assert!(start.contains("request_hash="));
        assert!(!start.contains("secret"), "no token reaches the log");
        job.state = JobState::Failed;
        job.message = "the node stopped answering".into();
        job.items_done = 7;
        job.finished = Some(951_782_460);
        let end = end_line(&job);
        assert!(end.starts_with("2000-02-29T00:01:00Z end job=ab12 result=failed"));
        assert!(end.contains("items_done=7") && end.contains("duration_s=60"));
        assert!(end.contains("message=\"the node stopped answering\""));
    }
}
