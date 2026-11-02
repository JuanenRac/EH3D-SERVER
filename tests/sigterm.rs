//! The real binary, stopped the way `systemctl stop` stops it: SIGTERM must end it in order (the machines that are running are stopped first) with a success
//! status, as Ctrl-C does. Only on systems that have signals.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[test]
fn sigterm_stops_the_server_in_order_with_a_success_status() {
    let dir = std::env::temp_dir().join(format!("eh3d-server-sigterm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("server.cfg");
    std::fs::write(
        &cfg,
        format!(
            "listen = 127.0.0.1:0\ndata_dir = {}\n",
            dir.join("data").display()
        ),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_eh3d-server"))
        .arg("--config")
        .arg(&cfg)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let (tx, rx) = mpsc::channel();
    let out = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });

    // it is up when it says it listens (the set-up code line comes first)
    let limit = Instant::now() + Duration::from_secs(30);
    loop {
        let line = rx
            .recv_timeout(limit.saturating_duration_since(Instant::now()))
            .expect("the server did not say it was listening");
        if line.contains("listening on") {
            break;
        }
    }

    let sent = Command::new("kill")
        .arg("-TERM")
        .arg(child.id().to_string())
        .status()
        .unwrap();
    assert!(sent.success());

    let end = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > end {
            let _ = child.kill();
            panic!("the server did not stop on SIGTERM");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status:?}");
    // the reader thread ends with the output of the process
    let rest: Vec<String> =
        std::iter::from_fn(|| rx.recv_timeout(Duration::from_secs(5)).ok()).collect();
    assert!(
        rest.iter().any(|l| l.contains("stopping")),
        "it says it is stopping: {rest:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
