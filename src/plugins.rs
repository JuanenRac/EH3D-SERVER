//! The machine plugins: how the server talks to a kind of machine without knowing which.
//!
//! The generic queue of jobs (`/api/v1/work/jobs`, `src/work.rs`) and the stream of events do not know what a drill or a laser is: they ask the **plugin** of the machine to
//! prepare a job from a request, start it, hold it, let it go on, stop it, say how far it is and say what it can do. Two plugins come with the server:
//!
//! * `bungard`: the machines the server already runs with DRILLPRO and ROUTEPRO (drilling, routing and free G-code on a node, a GRBL controller or the simulator). It
//!   is an adapter over what the server did before: the old routes (`/api/v1/machines`, `/api/v1/jobs`...) keep working as they were and a job made through the plugin is
//!   the same job in the same book.
//! * `demo`: a machine with no hardware, for the tests of the applications and of the server: a job of `steps` steps, one every `step_ms` milliseconds.
//!
//! A plugin is a value of a type that implements [`MachinePlugin`] and is added to [`Registry::standard`]; see `docs/MACHINE_PLUGINS.md`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::jobs::{new_id, now, JobRecord, JobState};
use crate::plan::{self, JobRequest};
use crate::state::{AppState, PauseError, StartError};
use crate::sync::Locked;

/// Where a job of the generic queue stands.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum WorkState {
    /// Prepared and waiting for a person to start it.
    Queued,
    Running,
    /// Held by a person, or waiting at a dialog of the machine.
    Paused,
    Done,
    Failed,
    Cancelled,
}

impl WorkState {
    pub fn name(self) -> &'static str {
        match self {
            WorkState::Queued => "queued",
            WorkState::Running => "running",
            WorkState::Paused => "paused",
            WorkState::Done => "done",
            WorkState::Failed => "failed",
            WorkState::Cancelled => "cancelled",
        }
    }

    pub fn finished(self) -> bool {
        matches!(
            self,
            WorkState::Done | WorkState::Failed | WorkState::Cancelled
        )
    }

    /// The machine is taken: running or held.
    pub fn active(self) -> bool {
        matches!(self, WorkState::Running | WorkState::Paused)
    }
}

#[derive(Debug, PartialEq)]
pub enum PluginError {
    UnknownMachine,
    /// The request does not fit what the machine takes.
    Bad(String),
    Busy,
    NotInState,
    NotSupported(&'static str),
    Failed(String),
}

impl PluginError {
    pub fn code(&self) -> &'static str {
        match self {
            PluginError::UnknownMachine => "unknown_machine",
            PluginError::Bad(_) => "bad_plugin_request",
            PluginError::Busy => "machine_busy",
            PluginError::NotInState => "not_in_state",
            PluginError::NotSupported(_) => "not_supported",
            PluginError::Failed(_) => "plugin_failed",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            PluginError::UnknownMachine => 404,
            PluginError::Bad(_) => 422,
            PluginError::Busy | PluginError::NotInState => 409,
            PluginError::NotSupported(_) => 501,
            PluginError::Failed(_) => 502,
        }
    }

    pub fn detail(&self) -> String {
        match self {
            PluginError::Bad(why) | PluginError::Failed(why) => why.clone(),
            PluginError::NotSupported(what) => (*what).to_string(),
            _ => String::new(),
        }
    }
}

pub type PluginResult<T> = Result<T, PluginError>;

/// What a plugin made of a request: the id the job has for the plugin and what a client draws of it.
pub struct Prepared {
    pub native_id: String,
    pub view: Value,
}

/// How a job stands, as the plugin sees it.
pub struct PluginStatus {
    pub state: WorkState,
    pub done: u64,
    pub total: u64,
    pub message: String,
}

/// One machine of a plugin.
pub struct MachineInfo {
    pub id: String,
    pub name: String,
    /// `idle` or `busy`.
    pub state: &'static str,
}

pub trait MachinePlugin: Send + Sync {
    /// The short name of the kind (`bungard`, `demo`): what `plugin` says in the machines and the jobs.
    fn kind(&self) -> &'static str;

    fn title(&self) -> &'static str;

    /// What the machines of this kind can do: the kinds of request they take and which operations they support.
    fn capabilities(&self) -> Value;

    /// Whether the machines of this kind are made through `/api/v1/work/machines`. `false` when the server has them from somewhere else (the machines of `bungard` are those of `/api/v1/machines`).
    fn manages_machines(&self) -> bool {
        false
    }

    /// Checks the settings of a machine of this kind that is made or changed.
    fn check_settings(&self, _settings: &Value) -> PluginResult<()> {
        Ok(())
    }

    /// The machines of this kind.
    fn machines(&self, st: &Arc<AppState>) -> Vec<MachineInfo>;

    /// Checks a request and makes a job ready for `machine`; nothing moves.
    fn prepare(
        &self,
        st: &Arc<AppState>,
        machine: &str,
        request: &Value,
        by: &str,
    ) -> PluginResult<Prepared>;

    fn start(
        &self,
        st: &Arc<AppState>,
        machine: &str,
        native_id: &str,
        by: &str,
    ) -> PluginResult<()>;

    fn pause(&self, st: &Arc<AppState>, machine: &str, native_id: &str) -> PluginResult<()>;

    fn resume(&self, st: &Arc<AppState>, machine: &str, native_id: &str) -> PluginResult<()>;

    /// Stops a running or held job at once.
    fn stop(
        &self,
        st: &Arc<AppState>,
        machine: &str,
        native_id: &str,
        by: &str,
    ) -> PluginResult<()>;

    fn status(
        &self,
        st: &Arc<AppState>,
        machine: &str,
        native_id: &str,
    ) -> PluginResult<PluginStatus>;

    /// A job that was prepared and will not start: the plugin lets go of what it kept for it.
    fn discard(&self, st: &Arc<AppState>, machine: &str, native_id: &str, by: &str);
}

/// The plugins of this server, by the name of their kind.
pub struct Registry {
    plugins: Vec<Arc<dyn MachinePlugin>>,
}

impl Registry {
    pub fn standard() -> Registry {
        Registry {
            plugins: vec![Arc::new(BungardPlugin), Arc::new(DemoPlugin::default())],
        }
    }

    pub fn get(&self, kind: &str) -> Option<Arc<dyn MachinePlugin>> {
        self.plugins.iter().find(|p| p.kind() == kind).cloned()
    }

    pub fn all(&self) -> &[Arc<dyn MachinePlugin>] {
        &self.plugins
    }
}

// ---- bungard ------------------------------------------------------------------------------------------------------------------------

/// The machines of DRILLPRO and ROUTEPRO: an adapter over the jobs and the machines the server already has.
pub struct BungardPlugin;

fn start_error(e: StartError) -> PluginError {
    match e {
        StartError::UnknownJob => PluginError::Bad("the job is not there".into()),
        StartError::UnknownMachine => PluginError::UnknownMachine,
        StartError::NotReady => PluginError::NotInState,
        StartError::MachineBusy => PluginError::Busy,
        StartError::Plan(code, detail) => PluginError::Bad(format!("{code}: {detail}")),
    }
}

fn pause_error(e: PauseError) -> PluginError {
    match e {
        PauseError::UnknownJob => PluginError::Bad("the job is not there".into()),
        PauseError::NotGcode => PluginError::NotSupported(
            "only a G-code job can be held; a drilling or routing job is stopped",
        ),
        PauseError::NotInState => PluginError::NotInState,
    }
}

impl MachinePlugin for BungardPlugin {
    fn kind(&self) -> &'static str {
        "bungard"
    }

    fn title(&self) -> &'static str {
        "Bungard drilling and routing machines (DRILLPRO, ROUTEPRO and free G-code)"
    }

    fn capabilities(&self) -> Value {
        json!({
            "requests": ["drill", "route", "gcode"],
            "pause": "gcode",
            "confirm": true,
            "stop": true,
            "progress": "items",
            "old_routes": "/api/v1/machines and /api/v1/jobs",
        })
    }

    fn machines(&self, st: &Arc<AppState>) -> Vec<MachineInfo> {
        let ids: Vec<(String, String)> = st
            .machines
            .locked()
            .list()
            .iter()
            .map(|m| (m.id.clone(), m.name.clone()))
            .collect();
        let jobs = st.jobs.locked();
        ids.into_iter()
            .map(|(id, name)| {
                let busy = jobs.active_on(&id).is_some();
                MachineInfo {
                    id,
                    name,
                    state: if busy { "busy" } else { "idle" },
                }
            })
            .collect()
    }

    fn prepare(
        &self,
        st: &Arc<AppState>,
        machine: &str,
        request: &Value,
        by: &str,
    ) -> PluginResult<Prepared> {
        let record = st
            .machines
            .locked()
            .get(machine)
            .cloned()
            .ok_or(PluginError::UnknownMachine)?;
        let parsed: JobRequest =
            serde_json::from_value(request.clone()).map_err(|e| PluginError::Bad(e.to_string()))?;
        let name = parsed.name().map(str::to_string);
        let planned = plan::plan(&record, &parsed)
            .map_err(|e| PluginError::Bad(format!("{}: {}", e.code, e.detail)))?;
        let view = planned.view;
        let job = JobRecord {
            id: new_id(),
            machine: record.id.clone(),
            name: name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| "job".into()),
            kind: view.kind.clone(),
            state: JobState::Ready,
            created: now(),
            started: None,
            finished: None,
            by: by.to_string(),
            started_by: String::new(),
            stopped_by: String::new(),
            items_total: view.items,
            items_done: 0,
            holes_total: match view.kind.as_str() {
                "drill" => view.holes.len(),
                "route" => view.paths.len(),
                _ => 0,
            },
            holes_done: 0,
            line: 0,
            message: String::new(),
            pause: None,
            request: request.clone(),
            resumed_from: String::new(),
            view,
        };
        let shown = job.summary();
        let native_id = job.id.clone();
        st.publish_job(&job);
        st.jobs.locked().add(job);
        Ok(Prepared {
            native_id,
            view: shown,
        })
    }

    fn start(
        &self,
        st: &Arc<AppState>,
        _machine: &str,
        native_id: &str,
        by: &str,
    ) -> PluginResult<()> {
        st.start_job(native_id, by).map_err(start_error)
    }

    fn pause(&self, st: &Arc<AppState>, _machine: &str, native_id: &str) -> PluginResult<()> {
        st.pause_job(native_id).map_err(pause_error)
    }

    fn resume(&self, st: &Arc<AppState>, _machine: &str, native_id: &str) -> PluginResult<()> {
        st.resume_job(native_id).map_err(pause_error)
    }

    fn stop(
        &self,
        st: &Arc<AppState>,
        _machine: &str,
        native_id: &str,
        by: &str,
    ) -> PluginResult<()> {
        if st.cancel_job(native_id, by) {
            Ok(())
        } else {
            Err(PluginError::NotInState)
        }
    }

    fn status(
        &self,
        st: &Arc<AppState>,
        _machine: &str,
        native_id: &str,
    ) -> PluginResult<PluginStatus> {
        let jobs = st.jobs.locked();
        let job = jobs.get(native_id).ok_or_else(|| {
            PluginError::Failed("the job is no longer in the book of jobs".into())
        })?;
        let held = job.pause.as_ref().is_some_and(|p| p.kind == "hold");
        let (state, message) = match job.state {
            JobState::Ready => (WorkState::Queued, String::new()),
            JobState::Connecting | JobState::Running => (WorkState::Running, String::new()),
            JobState::WaitingOperator if held => (WorkState::Paused, "held".to_string()),
            JobState::WaitingOperator => (
                WorkState::Running,
                job.pause
                    .as_ref()
                    .map(|p| format!("waiting for the operator ({})", p.kind))
                    .unwrap_or_default(),
            ),
            JobState::Done => (WorkState::Done, String::new()),
            JobState::Cancelled => (WorkState::Cancelled, job.message.clone()),
            JobState::Failed => (WorkState::Failed, job.message.clone()),
        };
        Ok(PluginStatus {
            state,
            done: job.items_done as u64,
            total: job.items_total as u64,
            message,
        })
    }

    fn discard(&self, st: &Arc<AppState>, _machine: &str, native_id: &str, by: &str) {
        // a job that is only ready is cancelled where it stands
        let _ = st.cancel_job(native_id, by);
    }
}

// ---- demo ---------------------------------------------------------------------------------------------------------------------------

/// The longest job of the demo machine, in steps.
const DEMO_MAX_STEPS: u64 = 100_000;
/// The longest step of the demo machine, in milliseconds (an hour: a test that wants a job that does not finish).
const DEMO_MAX_STEP_MS: u64 = 3_600_000;

struct DemoJob {
    machine: String,
    steps: u64,
    step_ms: u64,
    state: WorkState,
    /// The milliseconds it ran before the last hold.
    ran_ms: u64,
    since: Option<Instant>,
}

impl DemoJob {
    fn elapsed_ms(&self) -> u64 {
        self.ran_ms
            + self.since.map_or(0, |t| {
                t.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
            })
    }

    fn done(&self) -> u64 {
        if self.state == WorkState::Queued {
            0
        } else {
            self.elapsed_ms()
                .checked_div(self.step_ms)
                .map_or(self.steps, |q| q.min(self.steps))
        }
    }
}

/// A machine with no hardware. Its jobs are `{"kind": "demo", "steps": 10}`; the machine has `step_ms` in its settings (20 by default).
#[derive(Default)]
pub struct DemoPlugin {
    jobs: Mutex<HashMap<String, DemoJob>>,
}

impl DemoPlugin {
    fn step_ms_of(st: &Arc<AppState>, machine: &str) -> Option<u64> {
        let book = st.work.machines.locked();
        let found = book.get(machine).filter(|m| m.plugin == "demo")?;
        Some(
            found
                .settings
                .get("step_ms")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .min(DEMO_MAX_STEP_MS),
        )
    }
}

impl MachinePlugin for DemoPlugin {
    fn kind(&self) -> &'static str {
        "demo"
    }

    fn title(&self) -> &'static str {
        "A machine with no hardware, for tests and demonstrations"
    }

    fn capabilities(&self) -> Value {
        json!({
            "requests": ["demo"],
            "pause": "all",
            "confirm": false,
            "stop": true,
            "progress": "steps",
            "settings": { "step_ms": "milliseconds per step, 0 to 3600000 (20 by default)" },
            "limits": { "steps": DEMO_MAX_STEPS },
        })
    }

    fn manages_machines(&self) -> bool {
        true
    }

    fn check_settings(&self, settings: &Value) -> PluginResult<()> {
        match settings.get("step_ms") {
            None => Ok(()),
            Some(v) if v.as_u64().is_some_and(|n| n <= DEMO_MAX_STEP_MS) => Ok(()),
            Some(_) => Err(PluginError::Bad(format!(
                "step_ms is a whole number from 0 to {DEMO_MAX_STEP_MS}"
            ))),
        }
    }

    fn machines(&self, st: &Arc<AppState>) -> Vec<MachineInfo> {
        let list: Vec<(String, String)> = st
            .work
            .machines
            .locked()
            .list()
            .iter()
            .filter(|m| m.plugin == "demo")
            .map(|m| (m.id.clone(), m.name.clone()))
            .collect();
        let jobs = self.jobs.locked();
        list.into_iter()
            .map(|(id, name)| {
                let busy = jobs.values().any(|j| j.machine == id && j.state.active());
                MachineInfo {
                    id,
                    name,
                    state: if busy { "busy" } else { "idle" },
                }
            })
            .collect()
    }

    fn prepare(
        &self,
        st: &Arc<AppState>,
        machine: &str,
        request: &Value,
        _by: &str,
    ) -> PluginResult<Prepared> {
        let step_ms = Self::step_ms_of(st, machine).ok_or(PluginError::UnknownMachine)?;
        if request.get("kind").and_then(Value::as_str) != Some("demo") {
            return Err(PluginError::Bad(
                "the request has to be {\"kind\": \"demo\", \"steps\": N}".into(),
            ));
        }
        let steps = match request.get("steps") {
            None => 10,
            Some(v) => v
                .as_u64()
                .filter(|n| (1..=DEMO_MAX_STEPS).contains(n))
                .ok_or_else(|| {
                    PluginError::Bad(format!(
                        "steps is a whole number from 1 to {DEMO_MAX_STEPS}"
                    ))
                })?,
        };
        let native_id = crate::ids::new(crate::ids::Kind::Work);
        self.jobs.locked().insert(
            native_id.clone(),
            DemoJob {
                machine: machine.to_string(),
                steps,
                step_ms,
                state: WorkState::Queued,
                ran_ms: 0,
                since: None,
            },
        );
        Ok(Prepared {
            native_id,
            view: json!({
                "kind": "demo", "steps": steps, "step_ms": step_ms,
                "estimate_s": (steps * step_ms) as f64 / 1000.0,
            }),
        })
    }

    fn start(
        &self,
        _st: &Arc<AppState>,
        machine: &str,
        native_id: &str,
        _by: &str,
    ) -> PluginResult<()> {
        let mut jobs = self.jobs.locked();
        if jobs
            .values()
            .any(|j| j.machine == machine && j.state.active())
        {
            return Err(PluginError::Busy);
        }
        let job = jobs
            .get_mut(native_id)
            .ok_or_else(|| PluginError::Bad("the job is not there".into()))?;
        if job.state != WorkState::Queued {
            return Err(PluginError::NotInState);
        }
        job.state = WorkState::Running;
        job.since = Some(Instant::now());
        Ok(())
    }

    fn pause(&self, _st: &Arc<AppState>, _machine: &str, native_id: &str) -> PluginResult<()> {
        let mut jobs = self.jobs.locked();
        let job = jobs
            .get_mut(native_id)
            .ok_or_else(|| PluginError::Bad("the job is not there".into()))?;
        if job.state != WorkState::Running {
            return Err(PluginError::NotInState);
        }
        job.ran_ms = job.elapsed_ms();
        job.since = None;
        job.state = WorkState::Paused;
        Ok(())
    }

    fn resume(&self, _st: &Arc<AppState>, _machine: &str, native_id: &str) -> PluginResult<()> {
        let mut jobs = self.jobs.locked();
        let job = jobs
            .get_mut(native_id)
            .ok_or_else(|| PluginError::Bad("the job is not there".into()))?;
        if job.state != WorkState::Paused {
            return Err(PluginError::NotInState);
        }
        job.since = Some(Instant::now());
        job.state = WorkState::Running;
        Ok(())
    }

    fn stop(
        &self,
        _st: &Arc<AppState>,
        _machine: &str,
        native_id: &str,
        _by: &str,
    ) -> PluginResult<()> {
        let mut jobs = self.jobs.locked();
        let job = jobs
            .get_mut(native_id)
            .ok_or_else(|| PluginError::Bad("the job is not there".into()))?;
        if !job.state.active() {
            return Err(PluginError::NotInState);
        }
        job.ran_ms = job.elapsed_ms();
        job.since = None;
        job.state = WorkState::Cancelled;
        Ok(())
    }

    fn status(
        &self,
        _st: &Arc<AppState>,
        _machine: &str,
        native_id: &str,
    ) -> PluginResult<PluginStatus> {
        let mut jobs = self.jobs.locked();
        let job = jobs.get_mut(native_id).ok_or_else(|| {
            PluginError::Failed("the demo machine lost the job (the server restarted)".into())
        })?;
        if job.state == WorkState::Running && job.done() >= job.steps {
            job.ran_ms = job.elapsed_ms();
            job.since = None;
            job.state = WorkState::Done;
        }
        Ok(PluginStatus {
            state: job.state,
            done: job.done(),
            total: job.steps,
            message: String::new(),
        })
    }

    fn discard(&self, _st: &Arc<AppState>, _machine: &str, native_id: &str, _by: &str) {
        self.jobs.locked().remove(native_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_demo_job_counts_its_steps_from_the_clock_and_stops_counting_while_held() {
        let mut job = DemoJob {
            machine: "m".into(),
            steps: 10,
            step_ms: 1000,
            state: WorkState::Queued,
            ran_ms: 0,
            since: None,
        };
        assert_eq!(job.done(), 0);
        job.state = WorkState::Running;
        job.ran_ms = 3500;
        assert_eq!(job.done(), 3);
        // held: the time that ran is kept, the clock is not running
        job.state = WorkState::Paused;
        assert_eq!(job.done(), 3);
        job.ran_ms = 99_000;
        assert_eq!(job.done(), 10);
        // no wait per step: it is done as soon as it runs
        job.step_ms = 0;
        job.ran_ms = 0;
        job.state = WorkState::Running;
        assert_eq!(job.done(), 10);
    }

    #[test]
    fn the_states_say_which_are_finished_and_which_hold_the_machine() {
        for (s, finished, active) in [
            (WorkState::Queued, false, false),
            (WorkState::Running, false, true),
            (WorkState::Paused, false, true),
            (WorkState::Done, true, false),
            (WorkState::Failed, true, false),
            (WorkState::Cancelled, true, false),
        ] {
            assert_eq!(
                (s.finished(), s.active()),
                (finished, active),
                "{}",
                s.name()
            );
        }
    }

    #[test]
    fn the_registry_has_the_two_plugins() {
        let r = Registry::standard();
        assert!(r.get("bungard").is_some());
        assert!(r.get("demo").is_some());
        assert!(r.get("laser").is_none());
        assert_eq!(r.all().len(), 2);
    }
}
