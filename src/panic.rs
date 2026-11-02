//! What the server does when a thread panics.
//!
//! The default is to stop the machines that are running and to exit, so that the service manager starts the server again with its data in order (`Restart=on-failure` of the
//! unit): a panic in the middle of a job leaves nobody knowing what state the rest of the program is in. With `exit_on_panic = false` the panic is only written to the console
//! and the server goes on, with the locks recovered (see `sync`).
//!
//! The hook runs on the thread that panicked, which may still hold a lock of the state: the work is done on a thread of its own, which waits its turn for the locks while the
//! panicking thread unwinds. A second thread ends the process after a grace time whatever happens, so a stop that hangs cannot keep a broken server alive.

use std::sync::Arc;
use std::time::Duration;

use crate::state::AppState;

/// The exit status of a server that stopped because of a panic.
pub const EXIT_CODE: i32 = 70;
/// How long the machines have to stop before the process ends anyway.
pub const GRACE: Duration = Duration::from_secs(10);

/// Installs the hook for the whole process. Call it once, after the state is open.
pub fn install(state: Arc<AppState>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        let place = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown place".into());
        handle(&state, &place, |code| std::process::exit(code));
    }));
}

/// Writes the panic and, when the settings say so, stops the machines and ends the process with `exit` (a parameter so that a test can watch it).
pub fn handle<F>(state: &Arc<AppState>, place: &str, exit: F)
where
    F: Fn(i32) + Send + Sync + 'static,
{
    state.audit.event("panic", &[("place", place)]);
    if !state.cfg.exit_on_panic {
        eprintln!("eh3d-server: a thread panicked at {place}; going on (exit_on_panic is off)");
        return;
    }
    eprintln!("eh3d-server: a thread panicked at {place}; stopping the machines and exiting so that the service starts again");
    let exit = Arc::new(exit);
    let late = Arc::clone(&exit);
    std::thread::spawn(move || {
        std::thread::sleep(GRACE);
        late(EXIT_CODE);
    });
    let state = Arc::clone(state);
    std::thread::spawn(move || {
        state.stop_all();
        exit(EXIT_CODE);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ServerConfig;
    use std::sync::atomic::{AtomicI32, Ordering};

    fn state(name: &str, exit_on_panic: bool) -> Arc<AppState> {
        let dir =
            std::env::temp_dir().join(format!("eh3d-server-panic-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        AppState::open(ServerConfig {
            data_dir: dir,
            exit_on_panic,
            ..ServerConfig::default()
        })
        .unwrap()
    }

    #[test]
    fn a_panic_stops_the_machines_and_exits_by_default() {
        let state = state("on", true);
        let code = Arc::new(AtomicI32::new(0));
        let seen = code.clone();
        handle(&state, "somewhere.rs:1", move |c| {
            seen.store(c, Ordering::SeqCst)
        });
        let limit = std::time::Instant::now() + Duration::from_secs(5);
        while code.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(code.load(Ordering::SeqCst), EXIT_CODE);
    }

    #[test]
    fn with_the_switch_off_the_server_goes_on() {
        let state = state("off", false);
        let code = Arc::new(AtomicI32::new(0));
        let seen = code.clone();
        handle(&state, "somewhere.rs:1", move |c| {
            seen.store(c, Ordering::SeqCst)
        });
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(code.load(Ordering::SeqCst), 0);
    }
}
