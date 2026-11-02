//! Locks that a panic does not turn into a dead server.
//!
//! When a thread panics while it holds a `std::sync::Mutex`, the mutex is "poisoned" and every later `lock().expect(..)` panics too: the
//! process stays up, answers nothing useful and the service manager never restarts it (a zombie), and a stop that has to take the lock of a
//! serial link would not reach the machine. `locked()` takes the lock anyway, counts the event (the health check reports it as `degraded`)
//! and tells the console once; the data behind the lock is plain state that every writer leaves whole between two statements.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

static RECOVERED: AtomicU64 = AtomicU64::new(0);

/// How many times a poisoned lock was taken over since the server started.
pub fn recovered() -> u64 {
    RECOVERED.load(Ordering::Relaxed)
}

pub trait Locked<T> {
    /// Takes the lock, also when a thread panicked while it held it.
    fn locked(&self) -> MutexGuard<'_, T>;
}

impl<T> Locked<T> for Mutex<T> {
    fn locked(&self) -> MutexGuard<'_, T> {
        match self.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                if RECOVERED.fetch_add(1, Ordering::Relaxed) == 0 {
                    eprintln!("eh3d-server: a thread panicked while it held a lock; going on with the data as it was left (see the health check)");
                }
                // later users find the lock clean; the counter keeps the memory of it
                self.clear_poison();
                poisoned.into_inner()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_lock_that_a_panic_poisoned_is_still_taken() {
        let m = Arc::new(Mutex::new(5));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("on purpose");
        })
        .join();
        assert!(m.lock().is_err(), "the panic poisoned the lock");
        let before = recovered();
        assert_eq!(*m.locked(), 5);
        assert!(recovered() > before);
        // clean again
        assert!(m.lock().is_ok());
    }

    #[test]
    fn a_clean_lock_is_not_counted() {
        let m = Mutex::new(1);
        let before = recovered();
        assert_eq!(*m.locked(), 1);
        assert_eq!(recovered(), before);
    }
}
