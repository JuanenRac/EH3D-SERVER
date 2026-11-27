//! The running of a G-code job: its own thread opens the controller of the machine (the simulator, or GRBL on a serial port), sends the lines one by
//! one, and can be held, resumed and stopped from outside. The server follows it the way it follows the run of a drilling job.
//!
//! What it does when something goes wrong: a line the controller refuses, an alarm or a link that fails ends the job as failed and, as far as the
//! controller lets, stops it (the queue is cleared and the spindle goes off), because nobody is at the screen to decide.
//!
//! A program that ends with the spindle or the coolant on and does not say `M5`, `M9`, `M2` or `M30` is given them after its last line.
//!
//! A stop holds the movement, waits until the controller says that the machine stands still and only then resets it; when the controller does not
//! say so in time the reset goes all the same and the job is left with a position that is not known. A job that goes on after a stop first looks at
//! the controller (idle, and where the stopped job left the machine), then waits for a person, looks again, and only then runs the sequence that
//! takes the tool back (see [`crate::gcode::with_resume_sequence`]), with the time of the spindle in it. Where the state or the position is not
//! certain the job is refused with the reason and nothing is sent.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use eh3d_controller::grbl::{LineTransport, Timeouts};
use eh3d_controller::sim::SimController;
use eh3d_controller::{Command, Controller, State};

use crate::sync::Locked;

use crate::emergency::StopHandle;
use crate::gcode::Entry;
use crate::jobs::StopRecord;
use crate::machines::Connection;

/// Why a job stands still waiting for a person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// The operator asked for a pause.
    Operator,
    /// The job goes on after a stop: the operator looks at the machine (the tool, the spindle) before the first move.
    Resume,
    /// The sequence of a job that goes on is about to switch the spindle or the laser on again: a person confirms that apart.
    PowerOn,
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

/// What writes the real-time hold to the port.
type HoldFn = Box<dyn Fn() -> std::io::Result<()> + Send + Sync>;

/// What stops a machine from any thread: first the hold, then, once the machine stands still or the limit has passed, the reset.
trait Stopper: Send + Sync {
    fn begin(&self) -> std::io::Result<()>;
    fn finish(&self, limit: Duration) -> StopRecord;
}

impl<T: LineTransport + Send> Stopper for StopHandle<T> {
    fn begin(&self) -> std::io::Result<()> {
        StopHandle::begin(self)
    }
    fn finish(&self, limit: Duration) -> StopRecord {
        StopHandle::finish(self, limit)
    }
}

/// What a run is told besides its lines.
#[derive(Debug, Clone, PartialEq)]
pub struct RunOptions {
    /// The entry after which the spindle is given `spindle_wait` to reach its speed.
    pub spindle_wait_after: Option<usize>,
    pub spindle_wait: Duration,
    /// The entry that switches the spindle or the laser on again, when a person has to confirm it before it goes out.
    pub power_on_hold: Option<usize>,
    /// How long a machine that is stopped may take to say that it stands still.
    pub stop_wait: Duration,
    /// A pause also stops the spindle and the coolant of a GRBL machine (the safety door); off by default.
    pub stop_spindle_on_pause: bool,
    /// A job that goes on by itself: where the machine has to be, in mm, and how far from it each axis may be.
    pub expected_mm: Option<[f64; 3]>,
    pub tolerance_mm: f64,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            spindle_wait_after: None,
            spindle_wait: Duration::from_secs(3),
            power_on_hold: None,
            stop_wait: Duration::from_secs(3),
            stop_spindle_on_pause: false,
            expected_mm: None,
            tolerance_mm: 0.05,
        }
    }
}

struct Shared {
    state: Mutex<GState>,
    /// The index of the entry that goes next (the entries before it were taken by the controller).
    next: AtomicUsize,
    cancel: AtomicBool,
    hold: AtomicBool,
    confirm: AtomicBool,
    /// The job stopped following: the server ends.
    detach: AtomicBool,
    /// Stops the machine from any thread (a GRBL machine only; set by the thread of the job once the port is open).
    stopper: Mutex<Option<Arc<dyn Stopper>>>,
    /// Writes the real-time feed hold to the port from any thread (a GRBL machine only).
    hold_now: Mutex<Option<HoldFn>>,
    /// The real-time feed hold went out: the thread of the job has to take over the pause (and the resume) even if it was already lifted.
    hold_sent: AtomicBool,
    /// Writes the real-time resume to the port from any thread (a GRBL machine only).
    resume_now: Mutex<Option<HoldFn>>,
    /// The machine is held by the real-time byte and the thread of the job has not taken over: it may be waiting for the answer to a line,
    /// which a held controller does not give. The job counts as held meanwhile and a resume goes to the port at once.
    held_outside: AtomicBool,
    options: RunOptions,
    /// A line of the job went out to the controller.
    sent: AtomicBool,
    /// What is known of the machine once the job has ended.
    record: Mutex<Option<StopRecord>>,
}

impl Shared {
    #[cfg(test)]
    fn new() -> Shared {
        Shared::with(RunOptions::default())
    }

    fn with(options: RunOptions) -> Shared {
        Shared {
            options,
            sent: AtomicBool::new(false),
            record: Mutex::new(None),
            state: Mutex::new(GState::Connecting),
            next: AtomicUsize::new(0),
            cancel: AtomicBool::new(false),
            hold: AtomicBool::new(false),
            confirm: AtomicBool::new(false),
            detach: AtomicBool::new(false),
            stopper: Mutex::new(None),
            hold_now: Mutex::new(None),
            hold_sent: AtomicBool::new(false),
            resume_now: Mutex::new(None),
            held_outside: AtomicBool::new(false),
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
    let handle = Arc::new(handle);
    let for_hold = handle.clone();
    let door = shared.options.stop_spindle_on_pause;
    *shared.hold_now.locked() = Some(Box::new(move || {
        if door {
            for_hold.hold_with_door()
        } else {
            for_hold.hold()
        }
    }));
    let for_resume = handle.clone();
    *shared.resume_now.locked() = Some(Box::new(move || for_resume.resume()));
    *shared.stopper.locked() = Some(handle);
    Ok(Box::new(
        GrblController::new(link)
            .with_timeouts(timeouts)
            .with_spindle_stop_on_pause(door),
    ))
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
        GcodeRun::start_with(
            connection,
            entries,
            preamble,
            raw_total,
            resume,
            finish_wait,
            timeouts,
            RunOptions::default(),
        )
    }

    /// Like [`GcodeRun::start`], with what a job that goes on after a stop and the stop itself need.
    #[allow(clippy::too_many_arguments)]
    pub fn start_with(
        connection: Connection,
        entries: Vec<Entry>,
        preamble: usize,
        raw_total: usize,
        resume: bool,
        finish_wait: Duration,
        timeouts: Timeouts,
        options: RunOptions,
    ) -> Arc<GcodeRun> {
        let shared = Arc::new(Shared::with(options));
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
            settle(&worker);
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
        let state = self.shared.state.locked().clone();
        if state == GState::Running && self.shared.held_outside.load(Ordering::SeqCst) {
            return GState::Held(Hold::Operator);
        }
        state
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

    /// Holds the job where it is; it stays held until [`GcodeRun::resume`]. On a GRBL machine the feed hold is written to the port at once, from
    /// this thread, so that the machine stops moving even while the thread of the job waits for the end of a line or for the last moves (with
    /// `stop_spindle_on_pause`, the safety door byte follows it, so the spindle and the coolant stop too).
    pub fn pause(&self) {
        self.shared.hold.store(true, Ordering::SeqCst);
        let sent = self
            .shared
            .hold_now
            .locked()
            .as_ref()
            .map(|hold| hold().is_ok());
        if sent == Some(true) {
            self.shared.hold_sent.store(true, Ordering::SeqCst);
            self.shared.held_outside.store(true, Ordering::SeqCst);
        }
    }

    /// Lets a held job go on.
    pub fn resume(&self) {
        self.shared.hold.store(false, Ordering::SeqCst);
        // a hold the thread of the job has not taken over: it cannot end it (it may be waiting for a line the held machine does not
        // answer), so the resume goes to the port from here
        if self.shared.held_outside.swap(false, Ordering::SeqCst) {
            let sent = self
                .shared
                .resume_now
                .locked()
                .as_ref()
                .map(|resume| resume().is_ok());
            if sent == Some(true) {
                self.shared.hold_sent.store(false, Ordering::SeqCst);
            }
        }
        // the confirmation counts only for a job that waits for it: one given early must not skip the check of a job that goes on after a stop
        if matches!(self.state(), GState::Held(Hold::Resume | Hold::PowerOn)) {
            self.shared.confirm.store(true, Ordering::SeqCst);
        }
    }

    /// Stops the machine and ends the job. On a GRBL machine the feed hold is written to the port at once, from this thread, without waiting for
    /// the line that is being sent to end; the soft reset follows, from a thread of its own, once the controller says that the machine stands
    /// still or the time for that has passed. The job is marked as cancelled when its thread has seen the end of the stop.
    pub fn cancel(&self) {
        if self.shared.cancel.swap(true, Ordering::SeqCst) {
            return;
        }
        let stopper = self.shared.stopper.locked().clone();
        if let Some(stopper) = stopper {
            // the reset goes also when the hold could not be written, and the record of the stop says what failed
            let _ = stopper.begin();
            let limit = self.shared.options.stop_wait;
            thread::spawn(move || stopper.finish(limit));
        }
    }

    /// What is known of the machine of a job that has ended: where it stands, or why that is not known.
    pub fn stop_record(&self) -> Option<StopRecord> {
        self.shared.record.locked().clone()
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

/// Stops the machine and writes down what is known of it afterwards. A controller that is in alarm is already stopped.
fn stop(c: &mut dyn Controller, shared: &Shared) -> Result<(), String> {
    let limit = shared.options.stop_wait;
    let stopper = shared.stopper.locked().clone();
    let (record, outcome) = match stopper {
        // the hold, the wait for the machine to stand still and the reset; a stop that is already going on is waited for
        Some(stopper) => {
            let _ = stopper.begin();
            (stopper.finish(limit), Ok(()))
        }
        None => match c.command(Command::Stop) {
            Ok(()) | Err(eh3d_controller::Error::Alarm(_)) => (standing(c, limit), Ok(())),
            Err(e) => (
                StopRecord::unknown(format!("it could not be stopped ({e})")),
                Err(format!("it could not be stopped: {e}")),
            ),
        },
    };
    *shared.record.locked() = Some(record);
    outcome
}

/// Asks the controller until it says that the machine stands still, at most `limit`.
fn standing(c: &mut dyn Controller, limit: Duration) -> StopRecord {
    let end = Instant::now() + limit;
    loop {
        match c.status() {
            Ok(s)
                if matches!(
                    s.state,
                    State::Idle | State::Hold | State::Check | State::Sleep
                ) =>
            {
                return StopRecord::at([s.machine_mm[0], s.machine_mm[1], s.machine_mm[2]])
            }
            Ok(s) if s.state == State::Alarm => {
                return StopRecord::unknown("the controller is in alarm")
            }
            Ok(_) => {}
            Err(e) => return StopRecord::unknown(format!("the state could not be read ({e})")),
        }
        if Instant::now() >= end {
            return StopRecord::unknown(format!(
                "the controller did not say that the machine stood still within {} s",
                limit.as_secs_f64()
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// What the job leaves written of its machine. A job that sent nothing did not move it: one that goes on keeps the place it was waiting for,
/// unless the controller said that its position is lost.
fn settle(shared: &Shared) {
    if shared.sent.load(Ordering::SeqCst) {
        return;
    }
    let mut record = shared.record.locked();
    if record.as_ref().is_none_or(|r| r.known) {
        if let Some(expected) = shared.options.expected_mm {
            *record = Some(StopRecord::at(expected));
        }
    }
}

/// The end of a job that was cancelled.
fn cancelled(c: &mut dyn Controller, shared: &Shared) -> Result<GState, String> {
    stop(c, shared)?;
    Ok(GState::Cancelled)
}

/// Stops the machine as far as the controller allows; the word of what went wrong goes first.
fn stop_after(c: &mut dyn Controller, shared: &Shared, why: String) -> String {
    match stop(c, shared) {
        Ok(()) => why,
        Err(e) => format!("{why}; {e}"),
    }
}

/// Whether a job that goes on may move this machine: the controller is idle and, for a job that goes on by itself, the machine is where the
/// stopped job left it. Nothing is sent to find out but the request of the state.
fn resume_ready(
    c: &mut dyn Controller,
    shared: &Shared,
    expected: Option<[f64; 3]>,
) -> Result<(), String> {
    const REFUSED: &str = "the job is not resumed";
    let status = c
        .status()
        .map_err(|e| format!("{REFUSED}: the state of the controller could not be read ({e})"))?;
    match status.state {
        State::Idle => {}
        State::Alarm => {
            *shared.record.locked() = Some(StopRecord::unknown("the controller is in alarm"));
            return Err(format!(
                "{REFUSED}: the controller is in alarm, so the position of the machine is not certain: home it and check the work zero first"
            ));
        }
        other => {
            return Err(format!(
                "{REFUSED}: the controller is not idle (it says {other:?})"
            ))
        }
    }
    if let Some(expected) = expected {
        let at = status.machine_mm;
        let moved = (0..3).any(|i| {
            let apart = (at[i] - expected[i]).abs();
            apart.is_nan() || apart > shared.options.tolerance_mm
        });
        if moved {
            return Err(format!(
                "{REFUSED}: the machine is not where the job left it (it says X{:.3} Y{:.3} Z{:.3} and was at X{:.3} Y{:.3} Z{:.3}): it was moved or the controller lost its position; check it, then name the line to go on from",
                at[0], at[1], at[2], expected[0], expected[1], expected[2]
            ));
        }
    }
    Ok(())
}

/// Before the sequence of a job that goes on switches the spindle or the laser on again: waits for the machine to be still, then for the person,
/// and looks once more that it is idle where it stood. `Ok(false)` when the job was cancelled meanwhile.
fn power_on_confirmed(
    c: &mut dyn Controller,
    shared: &Shared,
    still_wait: Duration,
) -> Result<bool, String> {
    let mut limit = Instant::now() + still_wait;
    let at = loop {
        if shared.cancel.load(Ordering::SeqCst) {
            return Ok(false);
        }
        if hold_pending(shared) {
            if !operator_pause(c, shared)? {
                return Ok(false);
            }
            limit = Instant::now() + still_wait;
        }
        let status = c.status().map_err(|e| e.to_string())?;
        match status.state {
            State::Idle => break status.machine_mm,
            State::Alarm => return Err("the controller is in alarm".into()),
            _ => {}
        }
        if Instant::now() >= limit {
            return Err(format!(
                "the machine was not still {} s after the moves of the sequence",
                still_wait.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(50));
    };
    if !wait_held(shared, Hold::PowerOn) {
        return Ok(false);
    }
    resume_ready(c, shared, Some([at[0], at[1], at[2]]))?;
    Ok(true)
}

/// Gives the spindle its time to reach its speed. A pause holds the machine and the time starts again afterwards; `Ok(false)` when the job was
/// cancelled meanwhile.
fn spin_up(c: &mut dyn Controller, shared: &Shared) -> Result<bool, String> {
    let mut end = Instant::now() + shared.options.spindle_wait;
    while Instant::now() < end {
        if shared.cancel.load(Ordering::SeqCst) {
            return Ok(false);
        }
        if hold_pending(shared) {
            if !operator_pause(c, shared)? {
                return Ok(false);
            }
            end = Instant::now() + shared.options.spindle_wait;
        }
        thread::sleep(Duration::from_millis(20));
    }
    alarm_of(c)?;
    Ok(true)
}

/// The operator asked for a pause that the thread of the job has not taken over yet.
fn hold_pending(shared: &Shared) -> bool {
    shared.hold.load(Ordering::SeqCst) || shared.hold_sent.load(Ordering::SeqCst)
}

/// The pause of the operator: the controller is told to hold, the job waits for the resume and then lets the controller go on.
/// `Ok(true)` when the job goes on, `Ok(false)` when it was cancelled while held.
fn operator_pause(c: &mut dyn Controller, shared: &Shared) -> Result<bool, String> {
    shared.hold_sent.store(false, Ordering::SeqCst);
    c.command(Command::Pause)
        .map_err(|e| format!("pause: {e}"))?;
    if !wait_held(shared, Hold::Operator) {
        return Ok(false);
    }
    c.command(Command::Resume)
        .map_err(|e| format!("resume: {e}"))?;
    shared.set(GState::Running);
    Ok(true)
}

/// Waits while the job is held. `Ok(true)` when it can go on, `Ok(false)` when it was cancelled.
fn wait_held(shared: &Shared, hold: Hold) -> bool {
    // a confirmation given before this question was asked does not answer it
    shared.confirm.store(false, Ordering::SeqCst);
    shared.set(GState::Held(hold));
    // from here on the thread of the job holds the pause and ends it
    shared.held_outside.store(false, Ordering::SeqCst);
    loop {
        if shared.cancel.load(Ordering::SeqCst) {
            return false;
        }
        let released = match hold {
            // a pause ends when the operator takes the hold off; a resume check ends with the confirmation
            Hold::Operator => !shared.hold.load(Ordering::SeqCst),
            Hold::Resume | Hold::PowerOn => shared.confirm.swap(false, Ordering::SeqCst),
        };
        if released {
            shared.confirm.store(false, Ordering::SeqCst);
            // the wait is over from now on: what is done before the next line goes out must not show as the same question still being asked
            shared.set(GState::Running);
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
    // the simulator of a demo machine is made anew for each job: it is put where the machine was left, as a real one would still be
    if let (Connection::Demo, Some([x, y, z])) = (connection, shared.options.expected_mm) {
        for line in ["G21 G90".to_string(), format!("G0 X{x} Y{y} Z{z}")] {
            c.send_line(&line).map_err(|e| e.to_string())?;
        }
    }
    drive(c, lines, preamble, resume, finish_wait, shared)
}

/// Sends the lines to the controller and waits for the machine to be still.
fn drive(
    mut c: Box<dyn Controller>,
    lines: &[Entry],
    preamble: usize,
    resume: bool,
    finish_wait: Duration,
    shared: &Shared,
) -> Result<GState, String> {
    if resume {
        // looked at before a person is asked, and again after the answer: the machine may have been touched meanwhile
        resume_ready(c.as_mut(), shared, shared.options.expected_mm)?;
        if !wait_held(shared, Hold::Resume) {
            return cancelled(c.as_mut(), shared);
        }
        resume_ready(c.as_mut(), shared, shared.options.expected_mm)?;
    } else {
        alarm_of(c.as_mut())?;
    }
    shared.set(GState::Running);
    for (i, entry) in lines.iter().enumerate() {
        if shared.cancel.load(Ordering::SeqCst) {
            return cancelled(c.as_mut(), shared);
        }
        if shared.options.power_on_hold == Some(i) {
            match power_on_confirmed(c.as_mut(), shared, finish_wait) {
                Ok(true) => shared.set(GState::Running),
                Ok(false) => return cancelled(c.as_mut(), shared),
                Err(_) if shared.cancel.load(Ordering::SeqCst) => {
                    return cancelled(c.as_mut(), shared)
                }
                Err(why) => return Err(stop_after(c.as_mut(), shared, why)),
            }
        }
        if hold_pending(shared) {
            match operator_pause(c.as_mut(), shared) {
                Ok(true) => {}
                Ok(false) => return cancelled(c.as_mut(), shared),
                Err(why) => return Err(stop_after(c.as_mut(), shared, why)),
            }
        }
        shared.sent.store(true, Ordering::SeqCst);
        if let Err(e) = c.send_line(&entry.text) {
            // a line that was waiting when the operator stopped the machine is not a failure
            if shared.cancel.load(Ordering::SeqCst) {
                return cancelled(c.as_mut(), shared);
            }
            // the lines of the sequence of a job that goes on are not lines of the program
            let what = if i < preamble {
                format!("resume ({}): {e}", entry.text)
            } else {
                format!("line {} ({}): {e}", entry.raw + 1, entry.text)
            };
            return Err(stop_after(c.as_mut(), shared, what));
        }
        shared.next.store(i + 1, Ordering::SeqCst);
        if shared.options.spindle_wait_after == Some(i) {
            match spin_up(c.as_mut(), shared) {
                Ok(true) => {}
                Ok(false) => return cancelled(c.as_mut(), shared),
                Err(_) if shared.cancel.load(Ordering::SeqCst) => {
                    return cancelled(c.as_mut(), shared)
                }
                Err(why) => return Err(stop_after(c.as_mut(), shared, why)),
            }
        }
        if (i + 1) % STATUS_EVERY == 0 {
            if let Err(why) = alarm_of(c.as_mut()) {
                return Err(stop_after(c.as_mut(), shared, why));
            }
        }
    }
    // a program that ends with the spindle or the coolant on, without M5, M9, M2 or M30: nobody else would switch them off
    let (spindle, coolant) = crate::gcode::left_on(lines);
    for (on, off) in [(spindle, "M5"), (coolant, "M9")] {
        if !on {
            continue;
        }
        if shared.cancel.load(Ordering::SeqCst) {
            return cancelled(c.as_mut(), shared);
        }
        if let Err(e) = c.send_line(off) {
            if shared.cancel.load(Ordering::SeqCst) {
                return cancelled(c.as_mut(), shared);
            }
            let why = format!("{off} after the program: {e}");
            return Err(stop_after(c.as_mut(), shared, why));
        }
    }
    // the controller takes lines ahead of the movement: the job is done when the machine is still
    let mut limit = Instant::now() + finish_wait;
    loop {
        if shared.cancel.load(Ordering::SeqCst) {
            return cancelled(c.as_mut(), shared);
        }
        // a pause during the last moves holds the machine too, and the time it stands held does not count as time spent finishing
        if hold_pending(shared) {
            match operator_pause(c.as_mut(), shared) {
                Ok(true) => limit = Instant::now() + finish_wait,
                Ok(false) => return cancelled(c.as_mut(), shared),
                Err(why) => return Err(stop_after(c.as_mut(), shared, why)),
            }
        }
        match c.status() {
            Ok(s) if s.state == State::Alarm => {
                return Err(stop_after(
                    c.as_mut(),
                    shared,
                    "the controller went into alarm".into(),
                ))
            }
            Ok(s) if matches!(s.state, State::Idle | State::Check | State::Sleep) => {
                return Ok(GState::Done)
            }
            Ok(_) => {}
            // the status that could not be read because the operator stopped the machine is not a failure
            Err(_) if shared.cancel.load(Ordering::SeqCst) => return cancelled(c.as_mut(), shared),
            // the machine may still be moving with its spindle on: it is stopped as far as the link allows
            Err(e) => return Err(stop_after(c.as_mut(), shared, e.to_string())),
        }
        if Instant::now() >= limit {
            return Err(stop_after(
                c.as_mut(),
                shared,
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

    /// A controller that reports itself still moving for a number of polls and remembers what it was told.
    struct Busy {
        inner: SimController,
        moving_polls: Arc<AtomicUsize>,
        told: Arc<Mutex<Vec<String>>>,
        held: bool,
    }

    impl Controller for Busy {
        fn family(&self) -> eh3d_controller::Family {
            self.inner.family()
        }
        fn capabilities(&self) -> eh3d_controller::Capabilities {
            self.inner.capabilities()
        }
        fn status(&mut self) -> Result<eh3d_controller::Status, eh3d_controller::Error> {
            let mut s = self.inner.status()?;
            if self.held {
                s.state = State::Hold;
            } else if self.moving_polls.load(Ordering::SeqCst) > 0 {
                self.moving_polls.fetch_sub(1, Ordering::SeqCst);
                s.state = State::Run;
            }
            Ok(s)
        }
        fn command(&mut self, command: Command) -> Result<(), eh3d_controller::Error> {
            let name = match &command {
                Command::Pause => {
                    self.held = true;
                    Some("pause")
                }
                Command::Resume => {
                    self.held = false;
                    Some("resume")
                }
                Command::Stop => Some("stop"),
                _ => None,
            };
            if let Some(n) = name {
                self.told.lock().unwrap().push(n.to_string());
            }
            self.inner.command(command)
        }
        fn send_line(&mut self, line: &str) -> Result<(), eh3d_controller::Error> {
            self.inner.send_line(line)
        }
    }

    type Busy3 = (
        Arc<Shared>,
        Arc<AtomicUsize>,
        Arc<Mutex<Vec<String>>>,
        JoinHandle<Result<GState, String>>,
    );

    /// A job of three lines on a machine that goes on moving for `moving` polls after the last one went out.
    fn drive_busy(moving: usize, finish_wait: Duration) -> Busy3 {
        let shared = Arc::new(Shared::new());
        let moving_polls = Arc::new(AtomicUsize::new(moving));
        let told = Arc::new(Mutex::new(Vec::new()));
        let busy = Busy {
            inner: SimController::new(),
            moving_polls: moving_polls.clone(),
            told: told.clone(),
            held: false,
        };
        let p = gcode::parse("G21\nG0 X1\nG0 X2\n", 0, false).unwrap();
        let worker = shared.clone();
        let handle = thread::spawn(move || {
            drive(
                Box::new(busy),
                &p.entries,
                p.preamble,
                p.resume,
                finish_wait,
                &worker,
            )
        });
        (shared, moving_polls, told, handle)
    }

    fn until(what: &str, ok: impl Fn() -> bool) {
        let limit = Instant::now() + Duration::from_secs(20);
        while !ok() {
            assert!(Instant::now() < limit, "{what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Starts the job and asks for the pause once every line is in and the machine still moves.
    fn paused_in_the_last_moves(moving: usize, finish_wait: Duration) -> Busy3 {
        let (shared, moving_polls, told, handle) = drive_busy(moving, finish_wait);
        until("the lines did not go out", || {
            shared.next.load(Ordering::SeqCst) >= 3
        });
        shared.hold.store(true, Ordering::SeqCst);
        until("the pause was not attended", || {
            *shared.state.locked() == GState::Held(Hold::Operator)
        });
        (shared, moving_polls, told, handle)
    }

    #[test]
    fn a_pause_during_the_last_moves_holds_the_machine_and_the_resume_lets_it_finish() {
        let (shared, moving, told, handle) = paused_in_the_last_moves(200, Duration::from_secs(30));
        assert_eq!(told.lock().unwrap().as_slice(), ["pause"]);
        // held: the job does not end while the operator has not resumed
        thread::sleep(Duration::from_millis(300));
        assert!(!handle.is_finished());
        moving.store(0, Ordering::SeqCst);
        shared.hold.store(false, Ordering::SeqCst);
        assert_eq!(handle.join().unwrap(), Ok(GState::Done));
        assert_eq!(told.lock().unwrap().as_slice(), ["pause", "resume"]);
    }

    #[test]
    fn a_hold_that_went_out_in_real_time_and_was_lifted_at_once_is_still_resumed() {
        let (shared, moving, told, handle) = drive_busy(200, Duration::from_secs(30));
        until("the lines did not go out", || {
            shared.next.load(Ordering::SeqCst) >= 3
        });
        // what pause() followed by resume() leaves when the thread of the job did not see the flag in between
        shared.hold_sent.store(true, Ordering::SeqCst);
        until("the hold was not taken over", || {
            told.lock().unwrap().len() >= 2
        });
        moving.store(0, Ordering::SeqCst);
        assert_eq!(handle.join().unwrap(), Ok(GState::Done));
        assert_eq!(told.lock().unwrap().as_slice(), ["pause", "resume"]);
    }

    /// A controller whose state cannot be read once the lines are in, and which remembers what it was told.
    struct Deaf {
        inner: SimController,
        lines: usize,
        told: Arc<Mutex<Vec<String>>>,
    }

    impl Controller for Deaf {
        fn family(&self) -> eh3d_controller::Family {
            self.inner.family()
        }
        fn capabilities(&self) -> eh3d_controller::Capabilities {
            self.inner.capabilities()
        }
        fn status(&mut self) -> Result<eh3d_controller::Status, eh3d_controller::Error> {
            if self.lines >= 2 {
                return Err(eh3d_controller::Error::Timeout);
            }
            self.inner.status()
        }
        fn command(&mut self, command: Command) -> Result<(), eh3d_controller::Error> {
            if matches!(command, Command::Stop) {
                self.told.lock().unwrap().push("stop".to_string());
            }
            self.inner.command(command)
        }
        fn send_line(&mut self, line: &str) -> Result<(), eh3d_controller::Error> {
            self.lines += 1;
            self.told.lock().unwrap().push(line.to_string());
            self.inner.send_line(line)
        }
    }

    #[test]
    fn a_link_that_fails_during_the_last_moves_stops_the_machine() {
        let told = Arc::new(Mutex::new(Vec::new()));
        let deaf = Deaf {
            inner: SimController::new(),
            lines: 0,
            told: told.clone(),
        };
        let p = gcode::parse("M3 S9000\nG0 X1\n", 0, false).unwrap();
        let end = drive(
            Box::new(deaf),
            &p.entries,
            p.preamble,
            p.resume,
            Duration::from_secs(5),
            &Shared::new(),
        );
        assert!(end.is_err(), "{end:?}");
        assert_eq!(
            told.lock().unwrap().as_slice(),
            ["M3 S9000", "G0 X1", "M5", "stop"]
        );
    }

    #[test]
    fn a_cancel_while_held_in_the_last_moves_stops_the_machine() {
        let (shared, _moving, told, handle) =
            paused_in_the_last_moves(200, Duration::from_secs(30));
        shared.cancel.store(true, Ordering::SeqCst);
        assert_eq!(handle.join().unwrap(), Ok(GState::Cancelled));
        assert_eq!(told.lock().unwrap().as_slice(), ["pause", "stop"]);
    }

    #[test]
    fn the_time_held_does_not_count_against_the_wait_for_the_machine_to_be_still() {
        // a wait of 1 s for the end and a hold of 1.5 s: the job still ends as done
        let (shared, moving, _told, handle) = paused_in_the_last_moves(30, Duration::from_secs(1));
        thread::sleep(Duration::from_millis(1500));
        moving.store(5, Ordering::SeqCst);
        shared.hold.store(false, Ordering::SeqCst);
        assert_eq!(handle.join().unwrap(), Ok(GState::Done));
    }

    #[test]
    fn a_pause_while_a_line_waits_counts_as_held_and_the_resume_goes_to_the_port_at_once() {
        let shared = Arc::new(Shared::new());
        shared.set(GState::Running);
        let written = Arc::new(Mutex::new(Vec::new()));
        let holds = written.clone();
        *shared.hold_now.locked() = Some(Box::new(move || {
            holds.lock().unwrap().push("hold");
            Ok(())
        }));
        let resumes = written.clone();
        *shared.resume_now.locked() = Some(Box::new(move || {
            resumes.lock().unwrap().push("resume");
            Ok(())
        }));
        // no thread of a job: it stands for one that waits for the answer to a line
        let run = GcodeRun {
            shared: shared.clone(),
            handle: Mutex::new(None),
            preamble: 0,
            raws: vec![0],
            raw_total: 1,
        };
        run.pause();
        assert_eq!(run.state(), GState::Held(Hold::Operator));
        run.resume();
        assert_eq!(run.state(), GState::Running);
        assert!(
            !hold_pending(&shared),
            "nothing is left for the thread to do"
        );
        assert_eq!(written.lock().unwrap().as_slice(), ["hold", "resume"]);
        // a resume with nothing held writes nothing
        run.resume();
        assert_eq!(written.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_pause_on_the_simulator_holds_the_job_and_needs_no_port() {
        let text: String = (0..400).map(|i| format!("G0 X{}\n", i % 100)).collect();
        let run = start(&text, 0);
        run.pause();
        assert!(run.shared.hold.load(Ordering::SeqCst));
        // no port, no real-time byte
        assert!(!run.shared.hold_sent.load(Ordering::SeqCst));
        run.resume();
        assert_eq!(wait(&run, GState::finished), GState::Done);
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

    #[test]
    fn a_program_that_ends_with_the_spindle_or_the_coolant_on_has_them_switched_off() {
        let cases: [(&str, &[&str]); 3] = [
            (
                "M3 S9000\nM8\nG0 X1\n",
                &["M3 S9000", "M8", "G0 X1", "M5", "M9"],
            ),
            ("M3 S9000\nG0 X1\nM5\n", &["M3 S9000", "G0 X1", "M5"]),
            ("M4 S1\nG0 X1\nM30\n", &["M4 S1", "G0 X1", "M30"]),
        ];
        for (text, expected) in cases {
            let told = Arc::new(Told::default());
            let machine = Scripted {
                inner: SimController::new(),
                told: told.clone(),
            };
            let p = gcode::parse(text, 0, false).unwrap();
            let end = drive(
                Box::new(machine),
                &p.entries,
                p.preamble,
                p.resume,
                Duration::from_secs(10),
                &Shared::new(),
            );
            assert_eq!(end, Ok(GState::Done), "{text:?}");
            assert_eq!(said(&told), expected, "{text:?}");
        }
    }

    /// What a scripted machine was told, and the state and place it answers with.
    #[derive(Default)]
    struct Told {
        lines: Mutex<Vec<(String, Instant)>>,
        state: Mutex<Option<State>>,
        at: Mutex<[f64; 3]>,
        silent: AtomicBool,
    }

    struct Scripted {
        inner: SimController,
        told: Arc<Told>,
    }

    impl Controller for Scripted {
        fn family(&self) -> eh3d_controller::Family {
            self.inner.family()
        }
        fn capabilities(&self) -> eh3d_controller::Capabilities {
            self.inner.capabilities()
        }
        fn status(&mut self) -> Result<eh3d_controller::Status, eh3d_controller::Error> {
            if self.told.silent.load(Ordering::SeqCst) {
                return Err(eh3d_controller::Error::Timeout);
            }
            let mut s = self.inner.status()?;
            if let Some(state) = *self.told.state.lock().unwrap() {
                s.state = state;
            }
            let at = *self.told.at.lock().unwrap();
            s.machine_mm[..3].copy_from_slice(&at);
            Ok(s)
        }
        fn command(&mut self, command: Command) -> Result<(), eh3d_controller::Error> {
            if matches!(command, Command::Stop) {
                let mut lines = self.told.lines.lock().unwrap();
                lines.push(("stop".into(), Instant::now()));
            }
            self.inner.command(command)
        }
        fn send_line(&mut self, line: &str) -> Result<(), eh3d_controller::Error> {
            self.told
                .lines
                .lock()
                .unwrap()
                .push((line.to_string(), Instant::now()));
            self.inner.send_line(line)
        }
    }

    const WAS_AT: [f64; 3] = [30.0, 20.0, -1.5];
    const PROGRAM: &str = "G21 G90\nM3 S9000\nG0 X10 Y20\nG1 Z-1.5 F300\nX30\nX40\n";

    type Resumed = (Arc<Shared>, Arc<Told>, JoinHandle<Result<GState, String>>);

    /// Like [`resumed`], on a machine that asks apart before it switches the spindle or the laser on again.
    fn resumed_asking(laser: bool) -> Resumed {
        let mut program = gcode::parse(PROGRAM, 5, false).unwrap();
        let point = gcode::resume_point(PROGRAM, 5).unwrap();
        let moves = gcode::ResumeMoves {
            laser,
            ..Default::default()
        };
        gcode::with_resume_sequence(&mut program, &point, moves, false);
        let shared = Arc::new(Shared::with(RunOptions {
            spindle_wait_after: program.spindle_wait_after,
            spindle_wait: Duration::from_millis(50),
            power_on_hold: program.power_on_at,
            stop_wait: Duration::from_millis(300),
            stop_spindle_on_pause: false,
            expected_mm: Some(WAS_AT),
            tolerance_mm: 0.05,
        }));
        let told = Arc::new(Told::default());
        *told.at.lock().unwrap() = WAS_AT;
        let machine = Scripted {
            inner: SimController::new(),
            told: told.clone(),
        };
        let worker = shared.clone();
        let handle = thread::spawn(move || {
            let end = drive(
                Box::new(machine),
                &program.entries,
                program.preamble,
                program.resume,
                Duration::from_secs(10),
                &worker,
            );
            settle(&worker);
            end
        });
        (shared, told, handle)
    }

    fn confirm_the_first_question(shared: &Shared) {
        waiting(shared);
        shared.confirm.store(true, Ordering::SeqCst);
        until("the job did not ask before the power", || {
            *shared.state.locked() == GState::Held(Hold::PowerOn)
        });
    }

    #[test]
    fn the_spindle_or_the_laser_goes_on_only_after_a_confirmation_of_its_own() {
        for (laser, before) in [
            (false, vec!["G21 G90", "G0 Z5"]),
            (
                true,
                vec!["G21 G90", "G0 Z5", "G0 X30 Y20", "G1 Z-1.5 F100"],
            ),
        ] {
            let (shared, told, handle) = resumed_asking(laser);
            confirm_the_first_question(&shared);
            thread::sleep(Duration::from_millis(200));
            assert_eq!(said(&told), before, "laser {laser}");
            shared.confirm.store(true, Ordering::SeqCst);
            assert_eq!(handle.join().unwrap(), Ok(GState::Done));
            let all = said(&told);
            assert_eq!(all[before.len()], "M3 S9000", "{all:?}");
            assert_eq!(all.iter().filter(|l| l.starts_with("M3")).count(), 1);
        }
    }

    #[test]
    fn a_job_cancelled_at_the_question_of_the_power_never_switches_it_on() {
        let (shared, told, handle) = resumed_asking(true);
        confirm_the_first_question(&shared);
        shared.cancel.store(true, Ordering::SeqCst);
        assert_eq!(handle.join().unwrap(), Ok(GState::Cancelled));
        let all = said(&told);
        assert!(!all.iter().any(|l| l.starts_with("M3")), "{all:?}");
    }

    /// A job that goes on from the fifth line, on a machine that stands where the stopped job left it unless `set` says otherwise.
    fn resumed(spindle_wait: Duration, set: impl Fn(&Told)) -> Resumed {
        let mut program = gcode::parse(PROGRAM, 5, false).unwrap();
        let point = gcode::resume_point(PROGRAM, 5).unwrap();
        gcode::with_resume_sequence(&mut program, &point, Default::default(), false);
        let shared = Arc::new(Shared::with(RunOptions {
            spindle_wait_after: program.spindle_wait_after,
            spindle_wait,
            power_on_hold: None,
            stop_wait: Duration::from_millis(300),
            stop_spindle_on_pause: false,
            expected_mm: Some(WAS_AT),
            tolerance_mm: 0.05,
        }));
        let told = Arc::new(Told::default());
        *told.at.lock().unwrap() = WAS_AT;
        set(&told);
        let machine = Scripted {
            inner: SimController::new(),
            told: told.clone(),
        };
        let worker = shared.clone();
        let handle = thread::spawn(move || {
            let end = drive(
                Box::new(machine),
                &program.entries,
                program.preamble,
                program.resume,
                Duration::from_secs(10),
                &worker,
            );
            settle(&worker);
            end
        });
        (shared, told, handle)
    }

    fn waiting(shared: &Shared) {
        until("the job did not wait for the person", || {
            *shared.state.locked() == GState::Held(Hold::Resume)
        });
    }

    fn said(told: &Told) -> Vec<String> {
        told.lines
            .lock()
            .unwrap()
            .iter()
            .map(|l| l.0.clone())
            .collect()
    }

    #[test]
    fn nothing_goes_out_before_the_person_confirms_and_then_the_whole_sequence_does_in_order() {
        let (shared, told, handle) = resumed(Duration::from_millis(400), |_| {});
        waiting(&shared);
        thread::sleep(Duration::from_millis(200));
        assert!(said(&told).is_empty(), "{:?}", said(&told));
        shared.confirm.store(true, Ordering::SeqCst);
        until("the job did not go on", || {
            *shared.state.locked() != GState::Held(Hold::Resume)
        });
        // a second confirmation of a job that already goes on changes nothing
        shared.confirm.store(true, Ordering::SeqCst);
        assert_eq!(handle.join().unwrap(), Ok(GState::Done));
        assert_eq!(
            said(&told),
            [
                "G21 G90",
                "G0 Z5",
                "M3 S9000",
                "G0 X30 Y20",
                "G1 Z-1.5 F100",
                "G0",
                "F300",
                "G1 X40",
                "M5"
            ]
        );
        let lines = told.lines.lock().unwrap();
        let waited = lines[3].1.duration_since(lines[2].1);
        assert!(waited >= Duration::from_millis(400), "{waited:?}");
    }

    #[test]
    fn a_job_is_refused_with_its_reason_where_the_state_or_the_place_is_not_certain() {
        type Setting = fn(&Told);
        let cases: [(Setting, &str); 4] = [
            (|t| *t.at.lock().unwrap() = [30.0, 20.0, 0.0], "not where"),
            (|t| *t.state.lock().unwrap() = Some(State::Alarm), "alarm"),
            (|t| *t.state.lock().unwrap() = Some(State::Hold), "not idle"),
            (
                |t| t.silent.store(true, Ordering::SeqCst),
                "could not be read",
            ),
        ];
        for (set, why) in cases {
            let (shared, told, handle) = resumed(Duration::ZERO, set);
            let refused = handle.join().unwrap().unwrap_err();
            assert!(
                refused.starts_with("the job is not resumed") && refused.contains(why),
                "{refused}"
            );
            assert!(said(&told).is_empty(), "{why}: {:?}", said(&told));
            let record = shared.record.locked().clone().unwrap();
            // a machine in alarm lost its place; otherwise the job may be tried again from the same one
            assert_eq!(record.known, why != "alarm", "{why}: {record:?}");
        }
    }

    #[test]
    fn a_machine_moved_while_the_job_waited_for_the_person_is_not_moved() {
        let (shared, told, handle) = resumed(Duration::ZERO, |_| {});
        waiting(&shared);
        *told.at.lock().unwrap() = [0.0, 0.0, 0.0];
        shared.confirm.store(true, Ordering::SeqCst);
        assert!(handle.join().unwrap().unwrap_err().contains("not where"));
        assert!(said(&told).is_empty());
        assert_eq!(
            shared.record.locked().as_ref().unwrap().machine_mm,
            Some(WAS_AT)
        );
    }

    #[test]
    fn a_cancel_while_the_spindle_starts_stops_before_the_tool_crosses() {
        let (shared, told, handle) = resumed(Duration::from_secs(30), |_| {});
        waiting(&shared);
        shared.confirm.store(true, Ordering::SeqCst);
        until("the spindle was not started", || {
            said(&told).iter().any(|l| l == "M3 S9000")
        });
        shared.cancel.store(true, Ordering::SeqCst);
        assert_eq!(handle.join().unwrap(), Ok(GState::Cancelled));
        assert_eq!(said(&told), ["G21 G90", "G0 Z5", "M3 S9000", "stop"]);
    }

    #[test]
    fn a_stop_that_is_not_confirmed_leaves_the_position_as_not_known() {
        let (shared, told, handle) = resumed(Duration::from_secs(30), |_| {});
        waiting(&shared);
        shared.confirm.store(true, Ordering::SeqCst);
        until("the spindle was not started", || {
            said(&told).iter().any(|l| l == "M3 S9000")
        });
        *told.state.lock().unwrap() = Some(State::Run);
        shared.cancel.store(true, Ordering::SeqCst);
        assert_eq!(handle.join().unwrap(), Ok(GState::Cancelled));
        let record = shared.record.locked().clone().unwrap();
        assert!(
            !record.known && record.why.contains("stood still"),
            "{record:?}"
        );
    }

    /// A stop of two steps that only writes down what it was asked.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<String>>);

    impl Stopper for Recorder {
        fn begin(&self) -> std::io::Result<()> {
            self.0.lock().unwrap().push("begin".into());
            Ok(())
        }
        fn finish(&self, limit: Duration) -> StopRecord {
            self.0
                .lock()
                .unwrap()
                .push(format!("finish {}", limit.as_secs()));
            StopRecord::at([1.0, 2.0, 3.0])
        }
    }

    #[test]
    fn a_cancel_holds_at_once_and_ends_with_the_stop_of_two_steps() {
        let run = start("G21\nG90\nG0 X1\nG0 X2\n", 2);
        wait(&run, |s| matches!(s, GState::Held(Hold::Resume)));
        let recorder = Arc::new(Recorder::default());
        *run.shared.stopper.locked() = Some(recorder.clone());
        run.cancel();
        assert_eq!(recorder.0.lock().unwrap().first().unwrap(), "begin");
        run.cancel();
        assert_eq!(wait(&run, GState::finished), GState::Cancelled);
        assert!(recorder.0.lock().unwrap().iter().any(|c| c == "finish 3"));
        assert_eq!(run.stop_record(), Some(StopRecord::at([1.0, 2.0, 3.0])));
    }
}
