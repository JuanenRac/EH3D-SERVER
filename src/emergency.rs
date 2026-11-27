//! The way to stop a GRBL machine, from another thread than the one that is sending the lines.
//!
//! The thread of a G-code job writes a line and then waits for its `ok` for as long as the controller needs (seconds, or minutes for a homing
//! cycle), and during that time nothing else could reach the port. The port is therefore shared: the wait is made in short turns that let go of
//! the port between them, and a [`StopHandle`] takes it in one of those turns to write the real-time bytes of GRBL, which it obeys at once, in the
//! middle of a line or of a move. The thread that was waiting is told to stop waiting.
//!
//! A stop has two steps. [`StopHandle::begin`] writes the feed hold (`!`) at once and no line can follow it. [`StopHandle::finish`] then asks for the
//! state until the controller says that the machine stands still, and only then writes the soft reset (`0x18`), which stops the spindle: a reset
//! while the machine still moves loses its position. When the controller does not say so within the time given, the reset goes all the same and the
//! position is written down as not known. A homing cycle does not obey the feed hold and answers nothing, so it is reset at once.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eh3d_controller::grbl::{parse_status, LineTransport};
use eh3d_controller::{State, AXES};

use crate::jobs::StopRecord;
use crate::sync::Locked;

/// One turn of waiting for a line: how long the port is held before it is let go.
const TURN: Duration = Duration::from_millis(25);

/// The soft reset of GRBL, which also stops the spindle and the coolant.
pub const REAL_TIME_RESET: [u8; 1] = [0x18];

/// The request of the state of GRBL.
const ASK_STATE: [u8; 1] = *b"?";

/// How long an answer to one request of the state is waited for before it is asked again.
const ASK_AGAIN: Duration = Duration::from_millis(250);

/// How long between two requests while the machine is still braking.
const BETWEEN_ASKS: Duration = Duration::from_millis(50);

/// The most lines that were already waiting in the port which are thrown away before the first request: what they say is older than the hold.
const MOST_OLD_LINES: usize = 64;

/// The feed hold alone: the machine stops moving but keeps its state, and the job can go on after it.
pub const REAL_TIME_HOLD: [u8; 1] = *b"!";

/// The link the thread of the job uses.
pub struct SharedLink<T: LineTransport> {
    inner: Arc<Mutex<T>>,
    stopped: Arc<AtomicBool>,
    homing: Arc<AtomicBool>,
}

/// What another thread uses to stop the machine.
pub struct StopHandle<T: LineTransport> {
    inner: Arc<Mutex<T>>,
    stopped: Arc<AtomicBool>,
    /// A homing cycle went out and has not been answered.
    homing: Arc<AtomicBool>,
    /// The feed hold of the stop is on the wire.
    held: AtomicBool,
    /// How the stop ended: the reset goes out once, whoever asks for it.
    ended: Mutex<Option<StopRecord>>,
}

/// Shares a transport between the thread of a job and whoever may have to stop it.
pub fn share<T: LineTransport>(transport: T) -> (SharedLink<T>, StopHandle<T>) {
    let inner = Arc::new(Mutex::new(transport));
    let stopped = Arc::new(AtomicBool::new(false));
    let homing = Arc::new(AtomicBool::new(false));
    (
        SharedLink {
            inner: inner.clone(),
            stopped: stopped.clone(),
            homing: homing.clone(),
        },
        StopHandle {
            inner,
            stopped,
            homing,
            held: AtomicBool::new(false),
            ended: Mutex::new(None),
        },
    )
}

impl<T: LineTransport> StopHandle<T> {
    /// Writes the feed hold only, at once, without ending anything: the thread that waits for an answer goes on waiting.
    pub fn hold(&self) -> io::Result<()> {
        // a stop has already held the machine and has the port until it ends
        if self.stopped.load(Ordering::SeqCst) {
            return Ok(());
        }
        let mut port = self.inner.locked();
        port.write_all(&REAL_TIME_HOLD)
    }

    /// The first step of a stop: the feed hold goes out at once, no line can follow it and the thread that waits for an answer stops waiting.
    pub fn begin(&self) -> io::Result<()> {
        self.stopped.store(true, Ordering::SeqCst);
        if self.held.load(Ordering::SeqCst) {
            return Ok(());
        }
        // a stop must reach the machine even if a thread panicked while it held the link
        let mut port = self.inner.locked();
        self.hold_once(&mut *port)
    }

    /// Writes the feed hold of the stop unless it is already out. Only with the port in hand: whoever asks for the state afterwards knows that the
    /// hold went first, also when another thread was about to write it.
    fn hold_once(&self, port: &mut T) -> io::Result<()> {
        if self.held.load(Ordering::SeqCst) {
            return Ok(());
        }
        port.write_all(&REAL_TIME_HOLD)?;
        self.held.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// The second step: waits at most `limit` for the controller to say that the machine stands still and then writes the soft reset. Whoever asks
    /// again gets the same answer and nothing more is written.
    pub fn finish(&self, limit: Duration) -> StopRecord {
        self.stopped.store(true, Ordering::SeqCst);
        let mut ended = self.ended.locked();
        if let Some(record) = ended.as_ref() {
            return record.clone();
        }
        // the port stays in the hands of the stop until the reset is out: nobody else may take the answers
        let mut port = self.inner.locked();
        // the feed hold goes first also when `begin` was not called, or has not got to the port yet
        let held = self.hold_once(&mut *port);
        let mut record = if self.homing.load(Ordering::SeqCst) {
            StopRecord::unknown(
                "a homing cycle was running, which a feed hold does not stop: it was reset at once and the position is not known",
            )
        } else if let Err(e) = held {
            StopRecord::unknown(format!("the feed hold could not be written ({e})"))
        } else {
            standstill(&mut *port, limit)
        };
        if let Err(e) = port.write_all(&REAL_TIME_RESET) {
            record = StopRecord::unknown(format!(
                "the reset could not be written ({e}): the spindle may still turn"
            ));
        }
        *ended = Some(record.clone());
        record
    }
}

/// Asks the controller for its state until it says that the machine stands still, at most `limit`. Only a state read from the controller counts:
/// silence, a state that goes on moving and an alarm all give a position that is not known.
fn standstill<T: LineTransport>(port: &mut T, limit: Duration) -> StopRecord {
    for _ in 0..MOST_OLD_LINES {
        match port.read_line(Duration::from_millis(1)) {
            Ok(Some(_)) => {}
            _ => break,
        }
    }
    let end = Instant::now() + limit;
    let mut last = String::new();
    while Instant::now() < end {
        if let Err(e) = port.write_all(&ASK_STATE) {
            return StopRecord::unknown(format!("the state could not be asked ({e})"));
        }
        let until = end.min(Instant::now() + ASK_AGAIN);
        while Instant::now() < until {
            let line = match port.read_line(until.saturating_duration_since(Instant::now())) {
                Ok(Some(line)) => line,
                Ok(None) => continue,
                Err(e) => return StopRecord::unknown(format!("the state could not be read ({e})")),
            };
            let line = line.trim();
            if line.starts_with("ALARM") {
                return StopRecord::unknown("the controller went into alarm while it stopped");
            }
            let Some(status) = parse_status(line, &mut [0.0; AXES]) else {
                continue;
            };
            let still = match status.state {
                State::Idle | State::Check | State::Sleep => true,
                // `Hold:1` is still braking; a hold that does not say which is not taken as complete
                State::Hold => line.starts_with("<Hold:0"),
                // an open door: `Door:0` and `Door:1` stand still, the others are still parking or going back
                State::Door => line.starts_with("<Door:0") || line.starts_with("<Door:1"),
                State::Alarm => {
                    return StopRecord::unknown("the controller is in alarm");
                }
                State::Home => {
                    return StopRecord::unknown("the controller was homing");
                }
                _ => false,
            };
            if still {
                return if line.contains("MPos:") || line.contains("WPos:") {
                    StopRecord::at([
                        status.machine_mm[0],
                        status.machine_mm[1],
                        status.machine_mm[2],
                    ])
                } else {
                    StopRecord::unknown("the controller did not say where the machine stands")
                };
            }
            last = line.to_string();
            std::thread::sleep(BETWEEN_ASKS);
            break;
        }
    }
    StopRecord::unknown(format!(
        "the controller did not say that the machine stood still within {} s{}",
        limit.as_secs_f64(),
        if last.is_empty() {
            String::new()
        } else {
            format!(" (it said {last})")
        }
    ))
}

impl<T: LineTransport> LineTransport for SharedLink<T> {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        // asked with the port in hand: a stop that comes between the question and the write must not be followed by a line
        let mut port = self.inner.locked();
        if self.stopped.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
        }
        if bytes.starts_with(b"$H") {
            self.homing.store(true, Ordering::SeqCst);
        }
        port.write_all(bytes)
    }

    fn read_line(&mut self, timeout: Duration) -> io::Result<Option<String>> {
        let end = Instant::now() + timeout;
        loop {
            if self.stopped.load(Ordering::SeqCst) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let line = self.inner.locked().read_line(left.min(TURN))?;
            if let Some(said) = &line {
                // the answer to the homing cycle: it is over
                let said = said.trim();
                if said == "ok" || said.starts_with("error:") {
                    self.homing.store(false, Ordering::SeqCst);
                }
                return Ok(line);
            }
            // the port is free between two turns: a stop gets in here
            std::thread::yield_now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A port that never answers and remembers what was written.
    struct Silent {
        written: Arc<Mutex<Vec<u8>>>,
    }

    impl LineTransport for Silent {
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.written.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }
        fn read_line(&mut self, timeout: Duration) -> io::Result<Option<String>> {
            std::thread::sleep(timeout);
            Ok(None)
        }
    }

    /// A port that answers each request of the state with the next of its reports (the last one for ever).
    struct Reports {
        written: Arc<Mutex<Vec<u8>>>,
        reports: VecDeque<&'static str>,
        waiting: VecDeque<String>,
        fail_reset: bool,
    }

    impl LineTransport for Reports {
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            if self.fail_reset && bytes == REAL_TIME_RESET {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "unplugged"));
            }
            self.written.lock().unwrap().extend_from_slice(bytes);
            if bytes == ASK_STATE {
                let report = if self.reports.len() > 1 {
                    self.reports.pop_front()
                } else {
                    self.reports.front().copied()
                };
                self.waiting.extend(report.map(str::to_string));
            }
            Ok(())
        }
        fn read_line(&mut self, timeout: Duration) -> io::Result<Option<String>> {
            match self.waiting.pop_front() {
                Some(line) => Ok(Some(line)),
                None => {
                    std::thread::sleep(timeout.min(Duration::from_millis(5)));
                    Ok(None)
                }
            }
        }
    }

    fn reports(list: &[&'static str], old: &[&str]) -> (Reports, Arc<Mutex<Vec<u8>>>) {
        let written = Arc::new(Mutex::new(Vec::new()));
        let port = Reports {
            written: written.clone(),
            reports: list.iter().copied().collect(),
            waiting: old.iter().map(|l| l.to_string()).collect(),
            fail_reset: false,
        };
        (port, written)
    }

    const LIMIT: Duration = Duration::from_secs(5);
    const SHORT: Duration = Duration::from_millis(400);

    #[test]
    fn the_reset_goes_out_only_after_the_controller_says_the_machine_stands_still() {
        let (port, written) = reports(
            &[
                "<Run|MPos:1.000,2.000,-1.000>",
                "<Hold:1|MPos:1.500,2.000,-1.000>",
                "<Hold:0|MPos:2.000,2.500,-1.000>",
            ],
            // what was already waiting in the port is older than the hold and says nothing
            &["<Idle|MPos:9.000,9.000,9.000>", "ok"],
        );
        let (_link, handle) = share(port);
        handle.begin().unwrap();
        assert_eq!(written.lock().unwrap().as_slice(), b"!");
        let record = handle.finish(LIMIT);
        assert!(record.known, "{record:?}");
        assert_eq!(record.machine_mm, Some([2.0, 2.5, -1.0]));
        assert_eq!(written.lock().unwrap().as_slice(), b"!???\x18");
        // whoever asks again gets the same answer and nothing more goes out
        assert_eq!(handle.finish(LIMIT), record);
        assert_eq!(written.lock().unwrap().as_slice(), b"!???\x18");
    }

    #[test]
    fn a_state_that_never_arrives_ends_in_a_reset_and_a_position_that_is_not_known() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let (_link, handle) = share(Silent {
            written: written.clone(),
        });
        let started = Instant::now();
        let record = handle.finish(SHORT);
        assert!(started.elapsed() >= SHORT);
        assert!(!record.known && record.machine_mm.is_none(), "{record:?}");
        assert!(record.why.contains("did not say"), "{}", record.why);
        let bytes = written.lock().unwrap().clone();
        assert!(
            bytes.starts_with(b"!?") && bytes.ends_with(&REAL_TIME_RESET),
            "{bytes:?}"
        );
    }

    #[test]
    fn a_machine_that_goes_on_braking_or_a_hold_that_does_not_say_which_is_not_taken_as_still() {
        for state in [
            "<Hold:1|MPos:0,0,0>",
            "<Hold|MPos:0,0,0>",
            "<Run|MPos:0,0,0>",
        ] {
            let (port, written) = reports(&[state], &[]);
            let (_link, handle) = share(port);
            let record = handle.finish(SHORT);
            assert!(!record.known, "{state}: {record:?}");
            assert!(record.why.contains(state), "{}", record.why);
            assert_eq!(written.lock().unwrap().last(), Some(&0x18));
        }
    }

    #[test]
    fn an_alarm_a_missing_position_and_a_reset_that_fails_are_said() {
        for (state, why) in [
            ("<Alarm|MPos:0,0,0>", "alarm"),
            ("ALARM:3", "alarm"),
            ("<Idle>", "where the machine stands"),
        ] {
            let (port, _) = reports(&[state], &[]);
            let (_link, handle) = share(port);
            let record = handle.finish(LIMIT);
            assert!(
                !record.known && record.why.contains(why),
                "{state}: {record:?}"
            );
        }
        let (mut port, _) = reports(&["<Idle|MPos:0,0,0>"], &[]);
        port.fail_reset = true;
        let (_link, handle) = share(port);
        let record = handle.finish(LIMIT);
        assert!(
            !record.known && record.why.contains("spindle"),
            "{record:?}"
        );
    }

    #[test]
    fn a_homing_cycle_is_reset_at_once_and_one_that_was_answered_is_not_taken_for_running() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let (mut link, handle) = share(Silent {
            written: written.clone(),
        });
        link.write_all(b"$H\n").unwrap();
        let started = Instant::now();
        let record = handle.finish(Duration::from_secs(60));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!record.known && record.why.contains("homing"), "{record:?}");
        assert_eq!(written.lock().unwrap().as_slice(), b"$H\n!\x18");

        let (port, _) = reports(&["<Idle|MPos:0,0,0>"], &["ok"]);
        let (mut link, handle) = share(port);
        link.write_all(b"$H\n").unwrap();
        assert_eq!(link.read_line(LIMIT).unwrap().as_deref(), Some("ok"));
        assert!(handle.finish(LIMIT).known);
    }

    #[test]
    fn a_stop_gets_in_while_the_job_waits_for_an_answer_and_ends_the_wait() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let (mut link, handle) = share(Silent {
            written: written.clone(),
        });
        link.write_all(b"G4 P60\n").unwrap();
        let waiter = std::thread::spawn(move || {
            let started = Instant::now();
            let answer = link.read_line(Duration::from_secs(60));
            (answer.is_err(), started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(150));
        handle.begin().unwrap();
        let (interrupted, took) = waiter.join().unwrap();
        assert!(
            interrupted,
            "the wait ends with an error, not after a minute"
        );
        assert!(took < Duration::from_secs(5), "{took:?}");
        assert!(written.lock().unwrap().ends_with(b"!"));
    }

    #[test]
    fn a_hold_gets_in_while_the_job_waits_and_does_not_end_the_wait() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let (mut link, handle) = share(Silent {
            written: written.clone(),
        });
        link.write_all(b"G4 P5\n").unwrap();
        handle.hold().unwrap();
        assert_eq!(written.lock().unwrap().as_slice(), b"G4 P5\n!");
        // the link still works: a hold is not a stop
        link.write_all(b"~").unwrap();
        assert!(link.read_line(Duration::from_millis(30)).unwrap().is_none());
    }

    #[test]
    fn a_line_that_was_waiting_for_the_port_when_the_stop_came_is_not_written_after_it() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let (mut link, handle) = share(Silent {
            written: written.clone(),
        });
        let (stopped, port) = (handle.stopped.clone(), handle.inner.clone());
        // the port is in the hands of the stop while the thread of the job asks to write
        let mut held = port.lock().unwrap();
        let writer = std::thread::spawn(move || link.write_all(b"G1 Z-5\n").is_err());
        std::thread::sleep(Duration::from_millis(200));
        stopped.store(true, Ordering::SeqCst);
        held.write_all(&REAL_TIME_HOLD).unwrap();
        drop(held);
        assert!(writer.join().unwrap(), "the line is refused");
        assert_eq!(written.lock().unwrap().as_slice(), &REAL_TIME_HOLD);
    }

    #[test]
    fn the_hold_goes_out_before_the_state_is_asked_even_when_the_first_step_has_not_got_to_the_port(
    ) {
        let (port, written) = reports(&["<Idle|MPos:0,0,0>"], &[]);
        let (_link, handle) = share(port);
        // what another thread leaves between saying that it stops and writing the hold
        handle.stopped.store(true, Ordering::SeqCst);
        assert!(handle.finish(LIMIT).known);
        assert_eq!(written.lock().unwrap().as_slice(), b"!?\x18");
        handle.begin().unwrap();
        assert_eq!(written.lock().unwrap().as_slice(), b"!?\x18");
    }

    #[test]
    fn nothing_more_is_written_after_a_stop() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let (mut link, handle) = share(Silent {
            written: written.clone(),
        });
        handle.begin().unwrap();
        // a second hold or begin writes nothing more
        handle.begin().unwrap();
        handle.hold().unwrap();
        assert!(link.write_all(b"G0 X1\n").is_err());
        assert_eq!(written.lock().unwrap().as_slice(), &REAL_TIME_HOLD);
    }
}
