//! The jobs the server holds: what was asked, where it stands and how far it got. A job is **ready** when it has been planned and
//! accepted, and it starts only when a person with the right to says so (the board must be on the table); it then runs until it is done,
//! cancelled or failed. The list is kept on disk, so a restart of the server does not lose it, but a job that was running then is marked as
//! failed: nobody knows where the machine was.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::plan::JobView;
use crate::store;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Ready,
    Connecting,
    Running,
    /// A dialog for the operator (change the tool, confirm the spindle): [`JobRecord::pause`] says which.
    WaitingOperator,
    Done,
    Cancelled,
    Failed,
}

impl JobState {
    pub fn finished(self) -> bool {
        matches!(
            self,
            JobState::Done | JobState::Cancelled | JobState::Failed
        )
    }

    pub fn active(self) -> bool {
        matches!(
            self,
            JobState::Connecting | JobState::Running | JobState::WaitingOperator
        )
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PauseView {
    /// `tool_change` or `spindle_start`.
    pub kind: String,
    pub tool: Option<u8>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct JobRecord {
    pub id: String,
    pub machine: String,
    pub name: String,
    pub kind: String,
    pub state: JobState,
    pub created: u64,
    pub started: Option<u64>,
    pub finished: Option<u64>,
    /// Who asked for it.
    pub by: String,
    pub items_total: usize,
    pub items_done: usize,
    pub holes_total: usize,
    pub holes_done: usize,
    /// Why it failed, in English.
    pub message: String,
    pub pause: Option<PauseView>,
    /// What was asked, as it came: the job is planned again when it starts, with the machine's constants of that moment.
    pub request: serde_json::Value,
    pub view: JobView,
}

impl JobRecord {
    /// The job without its (large) view and request: for the lists.
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id, "machine": self.machine, "name": self.name, "kind": self.kind, "state": self.state,
            "created": self.created, "started": self.started, "finished": self.finished, "by": self.by,
            "items_total": self.items_total, "items_done": self.items_done, "holes_total": self.holes_total, "holes_done": self.holes_done,
            "message": self.message, "pause": self.pause, "estimate_s": self.view.estimate_s,
        })
    }

    /// The whole job, with the view a client draws.
    pub fn full(&self) -> serde_json::Value {
        let mut value = self.summary();
        value["view"] = serde_json::to_value(&self.view).unwrap_or(serde_json::Value::Null);
        value
    }
}

pub struct JobBook {
    path: PathBuf,
    keep: usize,
    jobs: Vec<JobRecord>,
}

impl JobBook {
    pub fn open(dir: &Path, keep: usize) -> io::Result<JobBook> {
        let path = dir.join("jobs.json");
        let mut jobs: Vec<JobRecord> = store::read_json(&path)?.unwrap_or_default();
        for job in &mut jobs {
            if job.state.active() {
                job.state = JobState::Failed;
                job.message = "the server restarted while the job was running".into();
                job.pause = None;
            }
        }
        Ok(JobBook { path, keep, jobs })
    }

    fn save(&self) {
        // a job book that cannot be written is a problem for the log, not a reason to stop a machine in the middle of a board
        if let Err(e) = store::write_json(&self.path, &self.jobs) {
            eprintln!("eh3d-server: cannot write {}: {e}", self.path.display());
        }
    }

    pub fn list(&self) -> &[JobRecord] {
        &self.jobs
    }

    pub fn get(&self, id: &str) -> Option<&JobRecord> {
        self.jobs.iter().find(|j| j.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut JobRecord> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    pub fn add(&mut self, job: JobRecord) {
        self.jobs.push(job);
        // the oldest finished jobs go when the list is longer than it should be
        while self.jobs.len() > self.keep {
            match self.jobs.iter().position(|j| j.state.finished()) {
                Some(i) => {
                    self.jobs.remove(i);
                }
                None => break,
            }
        }
        self.save();
    }

    /// Writes the list after a change made through `get_mut`.
    pub fn changed(&self) {
        self.save();
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.jobs.len();
        self.jobs.retain(|j| j.id != id);
        let removed = self.jobs.len() != before;
        if removed {
            self.save();
        }
        removed
    }

    /// The job running (or waiting for the operator) on a machine, if any.
    pub fn active_on(&self, machine: &str) -> Option<&JobRecord> {
        self.jobs
            .iter()
            .find(|j| j.machine == machine && j.state.active())
    }
}

pub fn new_id() -> String {
    use rand::RngCore;
    let mut b = [0u8; 4];
    rand::thread_rng().fill_bytes(&mut b);
    format!("j{}", hex::encode(b))
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, state: JobState) -> JobRecord {
        JobRecord {
            id: id.into(),
            machine: "m".into(),
            name: "n".into(),
            kind: "drill".into(),
            state,
            created: 1,
            started: None,
            finished: None,
            by: "juan".into(),
            items_total: 10,
            items_done: 0,
            holes_total: 3,
            holes_done: 0,
            message: String::new(),
            pause: None,
            request: serde_json::json!({}),
            view: JobView {
                kind: "drill".into(),
                name: "n".into(),
                tools: vec![],
                holes: vec![],
                paths: vec![],
                bounds_mm: None,
                items: 10,
                hole_ends: vec![],
                estimate_s: 1.0,
                travel_mm: 0.0,
            },
        }
    }

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("eh3d-server-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn a_job_that_was_running_when_the_server_stopped_is_marked_failed() {
        let d = dir("jobs-a");
        let mut book = JobBook::open(&d, 10).unwrap();
        book.add(record("a", JobState::Ready));
        book.add(record("b", JobState::Running));
        book.add(record("c", JobState::WaitingOperator));
        book.add(record("d", JobState::Done));
        let again = JobBook::open(&d, 10).unwrap();
        let states: Vec<JobState> = again.list().iter().map(|j| j.state).collect();
        assert_eq!(
            states,
            vec![
                JobState::Ready,
                JobState::Failed,
                JobState::Failed,
                JobState::Done
            ]
        );
        assert!(again.get("b").unwrap().message.contains("restarted"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_finished_jobs_are_dropped_when_the_list_is_too_long() {
        let d = dir("jobs-b");
        let mut book = JobBook::open(&d, 3).unwrap();
        book.add(record("a", JobState::Done));
        book.add(record("b", JobState::Ready));
        book.add(record("c", JobState::Cancelled));
        book.add(record("d", JobState::Ready));
        let ids: Vec<&str> = book.list().iter().map(|j| j.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c", "d"]);
        book.add(record("e", JobState::Ready));
        let ids: Vec<&str> = book.list().iter().map(|j| j.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "d", "e"]);
        assert!(book.remove("d") && !book.remove("d"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn one_job_at_a_time_is_active_on_a_machine() {
        let d = dir("jobs-c");
        let mut book = JobBook::open(&d, 10).unwrap();
        book.add(record("a", JobState::Ready));
        assert!(book.active_on("m").is_none());
        book.get_mut("a").unwrap().state = JobState::Running;
        assert_eq!(book.active_on("m").unwrap().id, "a");
        assert!(book.active_on("other").is_none());
        assert!(
            JobState::Done.finished()
                && !JobState::Ready.finished()
                && JobState::WaitingOperator.active()
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_summary_has_no_view_and_the_full_job_has_it() {
        let job = record("a", JobState::Ready);
        assert!(job.summary().get("view").is_none() && job.summary().get("request").is_none());
        assert!(job.full().get("view").is_some());
        assert_eq!(job.summary()["state"], "ready");
        assert!(new_id().starts_with('j') && new_id().len() == 9);
    }
}
