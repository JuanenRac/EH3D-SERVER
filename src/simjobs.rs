//! The jobs of the simulated machines: what the plugins that have no real machine behind (`cnc`, `gerber`, `cam`, `printer3d`) share. A job here is a list of items
//! (lines of a program, checks of a report, layers of a print) with the second each one ends at; a clock counts the seconds it ran, stops while the job is held and can be
//! made to run faster than real time, so the state of the job is worked out from the clock whenever somebody asks for it and nothing runs in a thread of its own.
//!
//! The plugin keeps what is specific to its machine (the position of the head, the latch of an emergency stop, the heaters) and asks this book for the rest.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::Value;

use crate::plugins::{PluginError, PluginResult, PluginStatus, WorkState};
use crate::simclock::{finished_items, SimClock};
use crate::sync::Locked;

struct SimJob {
    machine: String,
    kind: &'static str,
    cumulative: Vec<f64>,
    state: WorkState,
    clock: SimClock,
    rate: f64,
    message: String,
}

impl SimJob {
    fn done(&self) -> usize {
        match self.state {
            WorkState::Queued => 0,
            WorkState::Done => self.cumulative.len(),
            _ => finished_items(&self.cumulative, self.clock.work_s(self.rate)),
        }
    }
}

/// How far a job is, as the plugin that owns it reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Peek {
    pub machine: String,
    pub kind: &'static str,
    pub state: WorkState,
    pub done: usize,
    pub total: usize,
}

/// The jobs of the machines of one plugin.
#[derive(Default)]
pub struct SimBook {
    jobs: Mutex<HashMap<String, SimJob>>,
}

fn missing() -> PluginError {
    PluginError::Bad("the job is not there".into())
}

fn lost() -> PluginError {
    PluginError::Failed("the machine lost the job (the server restarted)".into())
}

impl SimBook {
    /// Makes a job that waits for a person to start it: `cumulative` is the second each item ends at (non-decreasing), `rate` how many times faster than real time the
    /// work goes (0 means all at once as soon as it runs). Gives the id the plugin knows the job by.
    pub fn add(
        &self,
        machine: &str,
        kind: &'static str,
        cumulative: Vec<f64>,
        rate: f64,
    ) -> String {
        let id = crate::ids::new(crate::ids::Kind::Work);
        self.jobs.locked().insert(
            id.clone(),
            SimJob {
                machine: machine.to_string(),
                kind,
                cumulative,
                state: WorkState::Queued,
                clock: SimClock::new(),
                rate,
                message: String::new(),
            },
        );
        id
    }

    /// Whether a job is running or held on the machine.
    pub fn busy(&self, machine: &str) -> bool {
        self.jobs
            .locked()
            .values()
            .any(|j| j.machine == machine && j.state.active())
    }

    pub fn peek(&self, id: &str) -> Option<Peek> {
        self.jobs.locked().get(id).map(|j| Peek {
            machine: j.machine.clone(),
            kind: j.kind,
            state: j.state,
            done: j.done(),
            total: j.cumulative.len(),
        })
    }

    /// The job that is running or held on the machine, if there is one.
    pub fn active_on(&self, machine: &str) -> Option<String> {
        self.jobs
            .locked()
            .iter()
            .find(|(_, j)| j.machine == machine && j.state.active())
            .map(|(id, _)| id.clone())
    }

    pub fn start(&self, machine: &str, id: &str) -> PluginResult<()> {
        let mut jobs = self.jobs.locked();
        if jobs
            .values()
            .any(|j| j.machine == machine && j.state.active())
        {
            return Err(PluginError::Busy);
        }
        let job = jobs.get_mut(id).ok_or_else(missing)?;
        if job.state != WorkState::Queued {
            return Err(PluginError::NotInState);
        }
        job.state = WorkState::Running;
        job.clock.run();
        Ok(())
    }

    pub fn pause(&self, id: &str) -> PluginResult<()> {
        let mut jobs = self.jobs.locked();
        let job = jobs.get_mut(id).ok_or_else(missing)?;
        if job.state != WorkState::Running {
            return Err(PluginError::NotInState);
        }
        job.clock.hold();
        job.state = WorkState::Paused;
        Ok(())
    }

    pub fn resume(&self, id: &str) -> PluginResult<()> {
        let mut jobs = self.jobs.locked();
        let job = jobs.get_mut(id).ok_or_else(missing)?;
        if job.state != WorkState::Paused {
            return Err(PluginError::NotInState);
        }
        job.clock.run();
        job.state = WorkState::Running;
        Ok(())
    }

    /// Ends a running or held job at once as `cancelled` with `message`; gives how many items were done.
    pub fn stop(&self, id: &str, message: &str) -> PluginResult<usize> {
        let mut jobs = self.jobs.locked();
        let job = jobs.get_mut(id).ok_or_else(missing)?;
        if !job.state.active() {
            return Err(PluginError::NotInState);
        }
        let done = job.done();
        job.clock.hold();
        job.state = WorkState::Cancelled;
        job.message = message.to_string();
        Ok(done)
    }

    /// Ends a running or held job as `failed` (the machine said so) with `message`.
    pub fn fail(&self, id: &str, message: &str) {
        if let Some(job) = self.jobs.locked().get_mut(id) {
            if job.state.active() {
                job.clock.hold();
                job.state = WorkState::Failed;
                job.message = message.to_string();
            }
        }
    }

    /// The state of the job; a running job whose items are all done becomes `done` here.
    pub fn status(&self, id: &str) -> PluginResult<PluginStatus> {
        let mut jobs = self.jobs.locked();
        let job = jobs.get_mut(id).ok_or_else(lost)?;
        if job.state == WorkState::Running && job.done() >= job.cumulative.len() {
            job.clock.hold();
            job.state = WorkState::Done;
        }
        Ok(PluginStatus {
            state: job.state,
            done: job.done() as u64,
            total: job.cumulative.len() as u64,
            message: job.message.clone(),
        })
    }

    pub fn discard(&self, id: &str) {
        self.jobs.locked().remove(id);
    }

    /// For the tests, which cannot wait: sets the seconds a job ran.
    #[cfg(test)]
    pub fn set_ran_s(&self, id: &str, seconds: f64) {
        if let Some(job) = self.jobs.locked().get_mut(id) {
            job.clock.set_ran_s(seconds);
            if job.state == WorkState::Running {
                job.clock.run();
            }
        }
    }
}

/// The seconds each of `n` equal items ends at, when each takes `item_s` seconds.
pub fn evenly(n: usize, item_s: f64) -> Vec<f64> {
    (1..=n).map(|i| i as f64 * item_s).collect()
}

/// A number setting of a machine inside its range, or `None` when the key is not there.
pub fn number_in(settings: &Value, key: &str, low: f64, high: f64) -> Result<Option<f64>, String> {
    match settings.get(key) {
        None => Ok(None),
        Some(v) => match v.as_f64() {
            Some(n) if n.is_finite() && (low..=high).contains(&n) => Ok(Some(n)),
            _ => Err(format!("{key} is a number from {low} to {high}")),
        },
    }
}

/// A true-or-false setting of a machine, or `None` when the key is not there.
pub fn flag_in(settings: &Value, key: &str) -> Result<Option<bool>, String> {
    match settings.get(key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("{key} is true or false")),
    }
}

/// Refuses a key of the settings that is not one of `known`.
pub fn known_keys(settings: &Value, known: &[&str], what: &str) -> Result<(), String> {
    if let Some(map) = settings.as_object() {
        for key in map.keys() {
            if !known.contains(&key.as_str()) {
                return Err(format!("{key} is not a setting of {what}"));
            }
        }
    }
    Ok(())
}

/// A number as short as it can be written (four decimals at the most).
pub fn num(v: f64) -> String {
    let text = format!("{v:.4}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() || text == "-0" {
        "0".to_string()
    } else {
        text.to_string()
    }
}

pub fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn job(book: &SimBook, machine: &str, n: usize, step: f64) -> String {
        book.add(machine, "demo", evenly(n, step), 1.0)
    }

    #[test]
    fn a_job_waits_to_be_started_and_one_job_at_a_time_runs_on_a_machine() {
        let book = SimBook::default();
        let a = job(&book, "m1", 4, 1.0);
        let b = job(&book, "m1", 4, 1.0);
        assert_eq!(book.peek(&a).unwrap().state, WorkState::Queued);
        assert_eq!(book.peek(&a).unwrap().done, 0);
        book.start("m1", &a).unwrap();
        assert!(book.busy("m1"));
        assert_eq!(book.start("m1", &b), Err(PluginError::Busy));
        assert_eq!(book.start("m1", &a), Err(PluginError::Busy));
        assert_eq!(book.active_on("m1"), Some(a.clone()));
        let other = job(&book, "m2", 1, 1.0);
        book.start("m2", &other).unwrap();
        assert_eq!(book.active_on("m3"), None);
    }

    #[test]
    fn a_held_job_goes_on_and_a_stopped_job_keeps_what_it_did() {
        let book = SimBook::default();
        let a = job(&book, "m1", 10, 1.0);
        assert_eq!(book.pause(&a), Err(PluginError::NotInState));
        assert_eq!(book.stop(&a, "x"), Err(PluginError::NotInState));
        book.start("m1", &a).unwrap();
        book.set_ran_s(&a, 3.5);
        book.pause(&a).unwrap();
        assert_eq!(book.status(&a).unwrap().state, WorkState::Paused);
        assert_eq!(book.pause(&a), Err(PluginError::NotInState));
        book.resume(&a).unwrap();
        assert_eq!(book.resume(&a), Err(PluginError::NotInState));
        book.set_ran_s(&a, 3.5);
        assert_eq!(book.stop(&a, "stopped").unwrap(), 3);
        let st = book.status(&a).unwrap();
        assert_eq!(st.state, WorkState::Cancelled);
        assert_eq!((st.done, st.total, st.message.as_str()), (3, 10, "stopped"));
        assert!(!book.busy("m1"));
    }

    #[test]
    fn a_job_whose_items_are_all_done_ends_as_done_and_a_failed_one_says_why() {
        let book = SimBook::default();
        let a = job(&book, "m1", 2, 1.0);
        book.start("m1", &a).unwrap();
        book.set_ran_s(&a, 2.0);
        let st = book.status(&a).unwrap();
        assert_eq!((st.state, st.done), (WorkState::Done, 2));
        let b = job(&book, "m1", 5, 1.0);
        book.start("m1", &b).unwrap();
        book.fail(&b, "the sensor is lost");
        let st = book.status(&b).unwrap();
        assert_eq!(st.state, WorkState::Failed);
        assert_eq!(st.message, "the sensor is lost");
        book.fail(&b, "again");
        assert_eq!(book.status(&b).unwrap().message, "the sensor is lost");
    }

    #[test]
    fn a_job_with_no_waiting_ends_as_soon_as_it_runs_and_a_discarded_one_is_lost() {
        let book = SimBook::default();
        let a = book.add("m1", "demo", evenly(3, 0.0), 1.0);
        book.start("m1", &a).unwrap();
        assert_eq!(book.status(&a).unwrap().state, WorkState::Done);
        book.discard(&a);
        assert!(matches!(book.status(&a), Err(PluginError::Failed(_))));
        assert_eq!(
            book.pause("nope"),
            Err(PluginError::Bad("the job is not there".into()))
        );
        assert!(book.peek("nope").is_none());
    }

    #[test]
    fn the_helpers_read_settings_and_write_numbers() {
        let s = json!({"a": 5, "b": true, "c": "x"});
        assert_eq!(number_in(&s, "a", 1.0, 10.0), Ok(Some(5.0)));
        assert_eq!(number_in(&s, "z", 1.0, 10.0), Ok(None));
        assert!(number_in(&s, "a", 6.0, 10.0).is_err());
        assert!(number_in(&s, "c", 1.0, 10.0).is_err());
        assert_eq!(flag_in(&s, "b"), Ok(Some(true)));
        assert!(flag_in(&s, "a").is_err());
        assert_eq!(flag_in(&s, "z"), Ok(None));
        assert!(known_keys(&s, &["a", "b", "c"], "x").is_ok());
        assert!(known_keys(&s, &["a", "b"], "x").is_err());
        assert_eq!(num(1.5), "1.5");
        assert_eq!(num(-0.00001), "0");
        assert_eq!(num(10.0), "10");
        assert_eq!(round2(1.006), 1.01);
        assert_eq!(evenly(3, 0.5), vec![0.5, 1.0, 1.5]);
    }
}
