//! The way to stop a GRBL machine at once, from another thread than the one that is sending the lines.
//!
//! The thread of a G-code job writes a line and then waits for its `ok` for as long as the controller needs (seconds, or minutes for a homing
//! cycle), and during that time nothing else could reach the port. The port is therefore shared: the wait is made in short turns that let go of
//! the port between them, and a [`StopHandle`] takes it in one of those turns to write the real-time bytes of GRBL, the feed hold (`!`) and the
//! soft reset (`0x18`), which GRBL obeys at once, in the middle of a line or of a move. The thread that was waiting is told to stop waiting.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eh3d_controller::grbl::LineTransport;

/// One turn of waiting for a line: how long the port is held before it is let go.
const TURN: Duration = Duration::from_millis(25);

/// The feed hold and the soft reset of GRBL.
pub const REAL_TIME_STOP: [u8; 2] = [b'!', 0x18];

/// The link the thread of the job uses.
pub struct SharedLink<T: LineTransport> {
    inner: Arc<Mutex<T>>,
    stopped: Arc<AtomicBool>,
}

/// What another thread uses to stop the machine.
pub struct StopHandle<T: LineTransport> {
    inner: Arc<Mutex<T>>,
    stopped: Arc<AtomicBool>,
}

/// Shares a transport between the thread of a job and whoever may have to stop it.
pub fn share<T: LineTransport>(transport: T) -> (SharedLink<T>, StopHandle<T>) {
    let inner = Arc::new(Mutex::new(transport));
    let stopped = Arc::new(AtomicBool::new(false));
    (
        SharedLink {
            inner: inner.clone(),
            stopped: stopped.clone(),
        },
        StopHandle { inner, stopped },
    )
}

impl<T: LineTransport> StopHandle<T> {
    /// Writes the feed hold and the soft reset, and makes the thread that waits for an answer stop waiting.
    pub fn stop(&self) -> io::Result<()> {
        self.stopped.store(true, Ordering::SeqCst);
        let mut port = self
            .inner
            .lock()
            .map_err(|_| io::Error::other("the link is poisoned"))?;
        port.write_all(&REAL_TIME_STOP)
    }
}

impl<T: LineTransport> LineTransport for SharedLink<T> {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.stopped.load(Ordering::SeqCst) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
        }
        self.inner
            .lock()
            .map_err(|_| io::Error::other("the link is poisoned"))?
            .write_all(bytes)
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
            let line = self
                .inner
                .lock()
                .map_err(|_| io::Error::other("the link is poisoned"))?
                .read_line(left.min(TURN))?;
            if line.is_some() {
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

    #[test]
    fn a_stop_gets_in_while_the_job_waits_for_an_answer_and_ends_the_wait() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let (mut link, handle) = share(Silent {
            written: written.clone(),
        });
        link.write_all(b"$H\n").unwrap();
        let waiter = std::thread::spawn(move || {
            let started = Instant::now();
            let answer = link.read_line(Duration::from_secs(60));
            (answer.is_err(), started.elapsed())
        });
        std::thread::sleep(Duration::from_millis(150));
        handle.stop().unwrap();
        let (interrupted, took) = waiter.join().unwrap();
        assert!(
            interrupted,
            "the wait ends with an error, not after a minute"
        );
        assert!(took < Duration::from_secs(5), "{took:?}");
        let bytes = written.lock().unwrap().clone();
        assert!(bytes.ends_with(&REAL_TIME_STOP), "{bytes:?}");
    }

    #[test]
    fn nothing_more_is_written_after_a_stop() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let (mut link, handle) = share(Silent {
            written: written.clone(),
        });
        handle.stop().unwrap();
        assert!(link.write_all(b"G0 X1\n").is_err());
        assert_eq!(written.lock().unwrap().as_slice(), &REAL_TIME_STOP);
    }
}
