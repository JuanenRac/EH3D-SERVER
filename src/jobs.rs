//! The jobs the server holds: what was asked, where it stands and how far it got. A job is **ready** when it has been planned and
//! accepted, and it starts only when a person with the right to says so (the board must be on the table); it then runs until it is done,
//! cancelled or failed. The list is kept on disk, so a restart of the server does not lose it, but a job that was running then is marked as
//! failed: nobody knows where the machine was.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use std::sync::{Arc, Condvar, Mutex};

use crate::plan::JobView;
use crate::store;
use crate::sync::Locked;

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
    /// `tool_change` or `spindle_start` (a drilling or routing job); `hold` (a G-code job the operator paused) or `resume` (a G-code job that goes on
    /// after a stop and waits for the operator before the first move).
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
    /// A G-code job: the line of the program text it has got to, which is the one to start from again if the job stops (`skip` of a new job); 0 for the others.
    #[serde(default)]
    pub line: usize,
    /// Why it failed, in English.
    pub message: String,
    pub pause: Option<PauseView>,
    /// What was asked, as it came: the job is planned again when it starts, with the machine's constants of that moment. It is kept in a file of its own
    /// (`requests/<id>.json`), not in `jobs.json`; a `jobs.json` of an older version that still holds it is read all the same.
    #[serde(default, skip_serializing)]
    pub request: serde_json::Value,
    /// The job this one goes on from (made by `resume-from`); empty for the others.
    #[serde(default)]
    pub resumed_from: String,
    pub view: JobView,
}

impl JobRecord {
    /// The job without its (large) view and request: for the lists.
    pub fn summary(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id, "machine": self.machine, "name": self.name, "kind": self.kind, "state": self.state,
            "created": self.created, "started": self.started, "finished": self.finished, "by": self.by,
            "items_total": self.items_total, "items_done": self.items_done, "holes_total": self.holes_total, "holes_done": self.holes_done,
            "line": self.line, "resumed_from": self.resumed_from, "message": self.message, "pause": self.pause, "estimate_s": self.view.estimate_s,
        })
    }

    /// The whole job, with the view a client draws.
    pub fn full(&self) -> serde_json::Value {
        let mut value = self.summary();
        value["view"] = serde_json::to_value(&self.view).unwrap_or(serde_json::Value::Null);
        value
    }
}

/// What is waiting to be written: the files are written by a thread of their own, so that the lock of the book is held only for the time it takes to
/// turn the book into bytes and never for the disk (a slow SD card of the CM5 must not stop the API or the jobs that are running).
#[derive(Default)]
struct Pending {
    /// The new `jobs.json` (without the requests).
    list: Option<Vec<u8>>,
    /// The request of a job to keep, by id.
    writes: Vec<(String, Vec<u8>)>,
    /// The ids of the jobs whose request is no longer needed.
    deletes: Vec<String>,
    busy: bool,
    stop: bool,
}

impl Pending {
    fn is_empty(&self) -> bool {
        self.list.is_none() && self.writes.is_empty() && self.deletes.is_empty()
    }
}

type Shared = Arc<(Mutex<Pending>, Condvar)>;

/// The list of the jobs is kept once as `jobs.json.bak` (it is what the server cannot start without); the request of each job is not.
fn write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let is_list = path.file_name().is_some_and(|n| n == "jobs.json");
    crate::store::write_durable(path, bytes, is_list)
}

fn writer_loop(shared: Shared, list_path: PathBuf, requests_dir: PathBuf) {
    let (lock, cv) = &*shared;
    loop {
        let work = {
            let mut p = lock.locked();
            while p.is_empty() && !p.stop {
                p = cv
                    .wait(p)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            if p.is_empty() && p.stop {
                return;
            }
            // taking the work leaves a fresh `Pending` behind: the marks of this one (the writer is busy, the stop was asked) are set again after it,
            // or `flush` would return while the work is still being written
            let stop = p.stop;
            let work = std::mem::take(&mut *p);
            p.busy = true;
            p.stop = stop;
            work
        };
        // a request is written before the list that names its job, and removed after it
        for (id, bytes) in &work.writes {
            if let Err(e) = write_bytes(&requests_dir.join(format!("{id}.json")), bytes) {
                eprintln!("eh3d-server: cannot write the request of {id}: {e}");
            }
        }
        if let Some(bytes) = &work.list {
            if let Err(e) = write_bytes(&list_path, bytes) {
                eprintln!("eh3d-server: cannot write {}: {e}", list_path.display());
            }
        }
        for id in &work.deletes {
            let _ = std::fs::remove_file(requests_dir.join(format!("{id}.json")));
        }
        let mut p = lock.locked();
        p.busy = false;
        cv.notify_all();
    }
}

pub struct JobBook {
    requests_dir: PathBuf,
    keep: usize,
    jobs: Vec<JobRecord>,
    shared: Shared,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl JobBook {
    pub fn open(dir: &Path, keep: usize) -> io::Result<JobBook> {
        let path = dir.join("jobs.json");
        let requests_dir = dir.join("requests");
        let mut jobs: Vec<JobRecord> = store::read_json(&path)?.unwrap_or_default();
        let mut migrate: Vec<(String, Vec<u8>)> = Vec::new();
        for job in &mut jobs {
            if job.state.active() {
                job.state = JobState::Failed;
                job.message = "the server restarted while the job was running".into();
                job.pause = None;
            }
            if job.request.is_null() {
                // the request is in its own file
                if let Ok(bytes) = std::fs::read(requests_dir.join(format!("{}.json", job.id))) {
                    job.request = serde_json::from_slice(&bytes).unwrap_or_default();
                }
            } else if !requests_dir.join(format!("{}.json", job.id)).exists() {
                // a book of an older version: the request moves to a file of its own
                migrate.push((
                    job.id.clone(),
                    serde_json::to_vec(&job.request).unwrap_or_default(),
                ));
            }
        }
        let shared: Shared = Arc::new((Mutex::new(Pending::default()), Condvar::new()));
        let handle = {
            let (shared, requests_dir) = (shared.clone(), requests_dir.clone());
            std::thread::Builder::new()
                .name("eh3d-jobs-writer".into())
                .spawn(move || writer_loop(shared, path, requests_dir))?
        };
        let book = JobBook {
            requests_dir,
            keep,
            jobs,
            shared,
            handle: Some(handle),
        };
        if !migrate.is_empty() {
            book.shared.0.locked().writes.extend(migrate);
            book.save();
        }
        Ok(book)
    }

    /// Turns the book into bytes (with the lock of the book, which is quick) and hands them to the writer thread.
    fn save(&self) {
        let bytes = serde_json::to_vec(&self.jobs).unwrap_or_default();
        let (lock, cv) = &*self.shared;
        lock.locked().list = Some(bytes);
        cv.notify_all();
    }

    /// Waits until everything handed to the writer is on the disk (the end of the server, and the tests).
    pub fn flush(&self) {
        let (lock, cv) = &*self.shared;
        let mut p = lock.locked();
        while !p.is_empty() || p.busy {
            p = cv
                .wait(p)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn queue_delete(&self, id: &str) {
        self.shared.0.locked().deletes.push(id.to_string());
    }

    /// Where the request of a job is kept.
    pub fn request_path(&self, id: &str) -> PathBuf {
        self.requests_dir.join(format!("{id}.json"))
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
        let bytes = serde_json::to_vec(&job.request).unwrap_or_default();
        self.shared.0.locked().writes.push((job.id.clone(), bytes));
        self.jobs.push(job);
        // the oldest finished jobs go when the list is longer than it should be
        while self.jobs.len() > self.keep {
            match self.jobs.iter().position(|j| j.state.finished()) {
                Some(i) => {
                    let gone = self.jobs.remove(i);
                    self.queue_delete(&gone.id);
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
            self.queue_delete(id);
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

impl Drop for JobBook {
    fn drop(&mut self) {
        self.flush();
        {
            let (lock, cv) = &*self.shared;
            lock.locked().stop = true;
            cv.notify_all();
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
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
            line: 0,
            message: String::new(),
            pause: None,
            request: serde_json::json!({}),
            resumed_from: String::new(),
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
                skipped_tools: vec![],
                skipped: 0,
                spindle_warnings: vec![],
                dry_run: None,
                warnings: vec![],
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
        book.flush();
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
    fn the_request_of_a_job_is_a_file_of_its_own_and_an_old_book_is_migrated() {
        let d = dir("jobs-req");
        let mut book = JobBook::open(&d, 10).unwrap();
        let mut a = record("a", JobState::Ready);
        a.request = serde_json::json!({"kind": "gcode", "gcode": "G0 X1"});
        book.add(a);
        book.flush();
        let list = std::fs::read_to_string(d.join("jobs.json")).unwrap();
        assert!(
            !list.contains("G0 X1"),
            "the list does not carry the request"
        );
        assert!(std::fs::read_to_string(book.request_path("a"))
            .unwrap()
            .contains("G0 X1"));
        drop(book);
        let again = JobBook::open(&d, 10).unwrap();
        assert_eq!(again.get("a").unwrap().request["gcode"], "G0 X1");
        // removing the job removes its file
        let mut again = again;
        assert!(again.remove("a"));
        again.flush();
        assert!(!again.request_path("a").exists());
        drop(again);

        // a jobs.json of an older version carries the request: it is read, and moved to its own file
        let d2 = dir("jobs-old");
        std::fs::create_dir_all(&d2).unwrap();
        let mut old = serde_json::to_value(record("o", JobState::Done)).unwrap();
        old["request"] = serde_json::json!({"kind": "gcode", "gcode": "G1 X2"});
        std::fs::write(
            d2.join("jobs.json"),
            serde_json::to_vec(&vec![old]).unwrap(),
        )
        .unwrap();
        let migrated = JobBook::open(&d2, 10).unwrap();
        assert_eq!(migrated.get("o").unwrap().request["gcode"], "G1 X2");
        migrated.flush();
        assert!(std::fs::read_to_string(migrated.request_path("o"))
            .unwrap()
            .contains("G1 X2"));
        assert!(!std::fs::read_to_string(d2.join("jobs.json"))
            .unwrap()
            .contains("G1 X2"));
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&d2);
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
