//! The running of a G-code job: its own thread opens the controller of the machine (the simulator, or GRBL on a serial port), sends the lines one by
//! one, and can be held, resumed and stopped from outside. The server follows it the way it follows the run of a drilling job.
//!
//! What it does when something goes wrong: a line the controller refuses, an alarm or a link that fails ends the job as failed and, as far as the
//! controller lets, stops it (the queue is cleared and the spindle goes off), because nobody is at the screen to decide.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eh3d_controller::grbl::Timeouts;
use eh3d_controller::sim::SimController;
use eh3d_controller::{Command, Controller, State};

use crate::sync::Locked;

use crate::gcode::Entry;
use crate::machines::Connection;

/// Why a job stands still waiting for a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// The operator asked for a pause.
    Operator,
    /// The job goes on after a stop: the operator looks at the machine (the tool, the spindle) before the first move.
    Resume,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GState {
    Connecting,
    Running,
    Held(Hold),
    Done,
    Cancelled,
    Failed(String),
}

impl GState {
    pub fn finished(&self) -> bool {
        matches!(self, GState::Done | GState::Cancelled | GState::Failed(_))
    }
}

/// What writes the real-time stop to the port.
type StopFn = Box<dyn Fn() -> std::io::Result<()> + Send + Sync>;

struct Shared {
    state: Mutex<GState>,
    /// The index of the entry that goes next (the entries before it were taken by the controller).
    next: AtomicUsize,
    cancel: AtomicBool,
    hold: AtomicBool,
    confirm: AtomicBool,
    /// The job stopped following: the server ends.
    detach: AtomicBool,
    /// Writes the real-time stop to the port from any thread (a GRBL machine only; set by the thread of the job once the port is open).
    emergency: Mutex<Option<StopFn>>,
    /// The real-time stop went out: the controller was reset, so it will not answer the lines that were waiting.
    hard: AtomicBool,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            state: Mutex::new(GState::Connecting),
            next: AtomicUsize::new(0),
            cancel: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            confirm: AtomicBool::new(false),
            detach: AtomicBool::new(false),
            emergency: Mutex::new(None),
            hard: AtomicBool::new(false),
        }
    }

    fn set(&self, s: GState) {
        *self.state.locked() = s;
    }
}

pub struct GcodeRun {
    shared: Arc<Shared>,
    handle: Mutex<Option<JoinHandle<()>>>,
    preamble: usize,
    /// The line of the text each entry comes from.
    raws: Vec<usize>,
    raw_total: usize,
}

/// How often the controller is asked how it is while lines go out.
const STATUS_EVERY: usize = 25;

/// The controller of a machine for a G-code job.
fn open(
    connection: &Connection,
    timeouts: Timeouts,
    shared: &Shared,
) -> Result<Box<dyn Controller>, String> {
    match connection {
        Connection::Demo => Ok(Box::new(SimController::new())),
        Connection::Grbl { port, baud } => open_grbl(port, *baud, timeouts, shared),
        Connection::Node { .. } => {
            Err("a node of a Bungard machine takes drilling and routing jobs, not G-code".into())
        }
    }
}

#[cfg(feature = "serial")]
fn open_grbl(
    port: &str,
    baud: u32,
    timeouts: Timeouts,
    shared: &Shared,
) -> Result<Box<dyn Controller>, String> {
    use eh3d_controller::grbl::GrblController;
    use eh3d_controller::serial::SerialTransport;
    let transport = SerialTransport::open(port, baud).map_err(|e| format!("{port}: {e}"))?;
    // the port is shared so that a stop can be written while the thread of the job waits for an answer
    let (link, handle) = crate::emergency::share(transport);
    *shared.emergency.locked() = Some(Box::new(move || handle.stop()));
    Ok(Box::new(GrblController::new(link).with_timeouts(timeouts)))
}

#[cfg(not(feature = "serial"))]
fn open_grbl(
    port: &str,
    _baud: u32,
    _timeouts: Timeouts,
    _shared: &Shared,
) -> Result<Box<dyn Controller>, String> {
    Err(format!(
        "{port}: this server was built without serial port support (build it with --features serial)"
    ))
}

impl GcodeRun {
    /// Starts the job on the machine. `finish_wait` is how long it waits, after the last line, for the machine to be still; `timeouts` are
    /// the times a GRBL controller may take to answer each kind of line (a line that does not end in time stops the machine and fails the job).
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        connection: Connection,
        entries: Vec<Entry>,
        preamble: usize,
        raw_total: usize,
        resume: bool,
        finish_wait: Duration,
        timeouts: Timeouts,
    ) -> Arc<GcodeRun> {
        let shared = Arc::new(Shared::new());
        let lines_raw: Vec<usize> = entries.iter().map(|e| e.raw).collect();
        let (worker, lines) = (shared.clone(), entries);
        let handle = thread::spawn(move || {
            let end = match run(
                &connection,
                &lines,
                preamble,
                resume,
                finish_wait,
                timeouts,
                &worker,
            ) {
                Ok(s) => s,
                Err(why) => GState::Failed(why),
            };
            worker.set(end);
        });
        Arc::new(GcodeRun {
            shared,
            handle: Mutex::new(Some(handle)),
            preamble,
            raws: lines_raw,
            raw_total,
        })
    }

    pub fn state(&self) -> GState {
        self.shared.state.locked().clone()
    }

    /// How many lines of the program itself the controller has taken, and how many the program has.
    pub fn progress(&self) -> (usize, usize) {
        let next = self.shared.next.load(Ordering::SeqCst);
        (
            next.saturating_sub(self.preamble),
            self.raws.len() - self.preamble,
        )
    }

    /// The line of the text the job has got to: the next one to send, which is where a job that goes on after a stop starts again.
    pub fn line(&self) -> usize {
        let next = self.shared.next.load(Ordering::SeqCst);
        match self.raws.get(next.max(self.preamble)) {
            Some(&raw) if self.state() != GState::Done => raw,
            _ => self.raw_total,
        }
    }

    /// Holds the job where it is (the controller is told to hold its movement); it stays held until [`GcodeRun::resume`].
    pub fn pause(&self) {
        self.shared.hold.store(true, Ordering::SeqCst);
    }

    /// Lets a held job go on.
    pub fn resume(&self) {
        self.shared.hold.store(false, Ordering::SeqCst);
        // the confirmation counts only for a job that waits for it: one given early must not skip the check of a job that goes on after a stop
        if self.state() == GState::Held(Hold::Resume) {
            self.shared.confirm.store(true, Ordering::SeqCst);
        }
    }

    /// Stops the machine and ends the job. On a GRBL machine the feed hold and the soft reset are written to the port at once, from this thread, without
    /// waiting for the line that is being sent to end; the job is marked as cancelled when its thread notices.
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::SeqCst);
        let sent = self
            .shared
            .emergency
            .locked()
            .as_ref()
            .map(|stop| stop().is_ok());
        if sent == Some(true) {
            self.shared.hard.store(true, Ordering::SeqCst);
        }
    }

    pub fn finished(&self) -> bool {
        self.state().finished()
    }

    /// Waits for the thread of the job to end (it has to have been cancelled or finished).
    pub fn join(&self) {
        let handle = self.handle.locked().take();
        if let Some(h) = handle {
            let _ = h.join();
        }
    }
}

impl Drop for GcodeRun {
    fn drop(&mut self) {
        self.shared.detach.store(true, Ordering::SeqCst);
        self.shared.cancel.store(true, Ordering::SeqCst);
    }
}

/// What the controller says it is doing, as an alarm the job cannot go on with.
fn alarm_of(c: &mut dyn Controller) -> Result<(), String> {
    match c.status() {
        Ok(s) if s.state == State::Alarm => Err("the controller is in alarm".into()),
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Stops the machine. A controller that is in alarm is already stopped.
fn stop(c: &mut dyn Controller) -> Result<(), String> {
    match c.command(Command::Stop) {
        Ok(()) | Err(eh3d_controller::Error::Alarm(_)) => Ok(()),
        Err(e) => Err(format!("it could not be stopped: {e}")),
    }
}

/// The end of a job that was cancelled. When the real-time stop went out the controller has been reset and is already stopped.
fn cancelled(c: &mut dyn Controller, shared: &Shared) -> Result<GState, String> {
    if !shared.hard.load(Ordering::SeqCst) {
        stop(c)?;
    }
    Ok(GState::Cancelled)
}

/// Stops the machine as far as the controller allows; the word of what went wrong goes first.
fn stop_after(c: &mut dyn Controller, why: String) -> String {
    match stop(c) {
        Ok(()) => why,
        Err(e) => format!("{why}; {e}"),
    }
}

/// Waits while the job is held. `Ok(true)` when it can go on, `Ok(false)` when it was cancelled.
fn wait_held(shared: &Shared, hold: Hold) -> bool {
    shared.set(GState::Held(hold));
    loop {
        if shared.cancel.load(Ordering::SeqCst) {
            return false;
        }
        let released = match hold {
            // a pause ends when the operator takes the hold off; a resume check ends with the confirmation
            Hold::Operator => !shared.hold.load(Ordering::SeqCst),
            Hold::Resume => shared.confirm.swap(false, Ordering::SeqCst),
        };
        if released {
            shared.confirm.store(false, Ordering::SeqCst);
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn run(
    connection: &Connection,
    lines: &[Entry],
    preamble: usize,
    resume: bool,
    finish_wait: Duration,
    timeouts: Timeouts,
    shared: &Shared,
) -> Result<GState, String> {
    let mut c = open(connection, timeouts, shared)?;
    alarm_of(c.as_mut())?;
    shared.set(GState::Running);
    for (i, entry) in lines.iter().enumerate() {
        // the check before the first move after a stop
        if i == preamble && resume && !wait_held(shared, Hold::Resume) {
            return cancelled(c.as_mut(), shared);
        }
        if shared.cancel.load(Ordering::SeqCst) {
            return cancelled(c.as_mut(), shared);
        }
        if shared.hold.load(Ordering::SeqCst) {
            if let Err(e) = c.command(Command::Pause) {
                return Err(stop_after(c.as_mut(), format!("pause: {e}")));
            }
            if !wait_held(shared, Hold::Operator) {
                return cancelled(c.as_mut(), shared);
            }
            if let Err(e) = c.command(Command::Resume) {
                return Err(stop_after(c.as_mut(), format!("resume: {e}")));
            }
            shared.set(GState::Running);
        }
        if let Err(e) = c.send_line(&entry.text) {
            // a line that was waiting when the operator stopped the machine is not a failure
            if shared.cancel.load(Ordering::SeqCst) {
                return cancelled(c.as_mut(), shared);
            }
            return Err(stop_after(
                c.as_mut(),
                format!("line {} ({}): {e}", entry.raw + 1, entry.text),
            ));
        }
        shared.next.store(i + 1, Ordering::SeqCst);
        if (i + 1) % STATUS_EVERY == 0 {
            if let Err(why) = alarm_of(c.as_mut()) {
                return Err(stop_after(c.as_mut(), why));
            }
        }
    }
    // the controller takes lines ahead of the movement: the job is done when the machine is still
    let limit = Instant::now() + finish_wait;
    loop {
        if shared.cancel.load(Ordering::SeqCst) {
            return cancelled(c.as_mut(), shared);
        }
        match c.status() {
            Ok(s) if s.state == State::Alarm => {
                return Err(stop_after(
                    c.as_mut(),
                    "the controller went into alarm".into(),
                ))
            }
            Ok(s) if matches!(s.state, State::Idle | State::Check | State::Sleep) => {
                return Ok(GState::Done)
            }
            Ok(_) => {}
            Err(e) => return Err(e.to_string()),
        }
        if Instant::now() >= limit {
            return Err(stop_after(
                c.as_mut(),
                format!(
                    "the machine was not still {} s after the last line",
                    finish_wait.as_secs()
                ),
            ));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gcode;

    fn start(text: &str, skip: usize) -> Arc<GcodeRun> {
        let p = gcode::parse(text, skip, false).unwrap();
        GcodeRun::start(
            Connection::Demo,
            p.entries,
            p.preamble,
            p.raw_total,
            p.resume,
            Duration::from_secs(5),
            Timeouts::default(),
        )
    }

    fn wait(run: &GcodeRun, ok: impl Fn(&GState) -> bool) -> GState {
        let limit = Instant::now() + Duration::from_secs(20);
        loop {
            let s = run.state();
            if ok(&s) {
                return s;
            }
            assert!(Instant::now() < limit, "still {s:?}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_program_runs_to_the_end_on_the_simulator() {
        let run = start("G21 G90\nG0 X10 Y10\nG1 X20 F300\nM30\n", 0);
        assert_eq!(wait(&run, GState::finished), GState::Done);
        assert_eq!(run.progress(), (4, 4));
        // done: the line reached is the end of the text
        assert_eq!(run.line(), 4);
    }

    #[test]
    fn a_program_with_a_homing_and_a_jog_runs_on_the_simulator() {
        let text = "$H\nG0 X10 Y10\n$J=G91 X5 F200\n$G\nM30\n";
        let p = gcode::parse_with(text, 0, false, true).unwrap();
        let run = GcodeRun::start(
            Connection::Demo,
            p.entries,
            p.preamble,
            p.raw_total,
            p.resume,
            Duration::from_secs(5),
            Timeouts::default(),
        );
        assert_eq!(wait(&run, GState::finished), GState::Done);
    }

    #[test]
    fn a_line_the_controller_refuses_ends_the_job_there_and_says_which() {
        let run = start("G0 X1\nG2 X5 I1\nG0 X9\n", 0);
        match wait(&run, GState::finished) {
            GState::Failed(why) => assert!(why.contains("line 2"), "{why}"),
            other => panic!("{other:?}"),
        }
        // the line that failed is the one a job that goes on starts from
        assert_eq!(run.line(), 1);
        assert_eq!(run.progress().0, 1);
    }

    #[test]
    fn a_move_beyond_the_travel_is_an_alarm_and_a_failure() {
        let run = start("G0 X99999\n", 0);
        assert!(matches!(wait(&run, GState::finished), GState::Failed(_)));
    }

    #[test]
    fn a_pause_holds_the_lines_and_the_resume_lets_them_go() {
        let text: String = (0..400).map(|i| format!("G0 X{}\n", i % 100)).collect();
        let run = start(&text, 0);
        run.pause();
        wait(&run, |s| {
            matches!(s, GState::Held(Hold::Operator)) || s.finished()
        });
        if matches!(run.state(), GState::Held(_)) {
            let at = run.progress().0;
            thread::sleep(Duration::from_millis(200));
            assert_eq!(run.progress().0, at, "lines went out while it was held");
            run.resume();
        }
        assert_eq!(wait(&run, GState::finished), GState::Done);
        assert_eq!(run.progress().0, 400);
    }

    #[test]
    fn a_job_that_goes_on_waits_for_the_operator_before_it_moves() {
        let run = start("G21\nG90\nG0 X1\nG0 X2\nG0 X3\n", 2);
        wait(&run, |s| matches!(s, GState::Held(Hold::Resume)));
        // the state came first, the work has not started
        assert_eq!(run.progress().0, 0);
        run.resume();
        assert_eq!(wait(&run, GState::finished), GState::Done);
        assert_eq!(run.progress(), (3, 3));
    }

    #[test]
    fn a_cancel_while_held_stops_the_job() {
        let run = start("G21\nG90\nG0 X1\n", 2);
        wait(&run, |s| matches!(s, GState::Held(Hold::Resume)));
        run.cancel();
        assert_eq!(wait(&run, GState::finished), GState::Cancelled);
    }

    #[test]
    fn a_node_cannot_take_gcode_and_a_grbl_port_without_the_feature_says_so() {
        let node = Connection::Node {
            address: "10.0.0.2:5800".into(),
            token: String::new(),
        };
        assert!(open(&node, Timeouts::default(), &Shared::new()).is_err());
        #[cfg(not(feature = "serial"))]
        {
            let grbl = Connection::Grbl {
                port: "COM9".into(),
                baud: 115_200,
            };
            let e = open(&grbl, Timeouts::default(), &Shared::new())
                .err()
                .unwrap();
            assert!(e.contains("serial"), "{e}");
        }
    }
}
