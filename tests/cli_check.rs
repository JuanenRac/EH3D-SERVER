//! `eh3d-server --check`: the exit code is 0 when the verification passes and 2 when it does not, and nothing is written or started.

use std::path::PathBuf;
use std::process::Command;

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("eh3d-server-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(file: &std::path::Path) -> (i32, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_eh3d-server"))
        .arg("--check")
        .arg("--config")
        .arg(file)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
    )
}

#[test]
fn the_check_exits_zero_when_it_passes_and_two_when_it_does_not() {
    let dir = folder("exit");
    let data = dir.join("data").to_string_lossy().replace('\\', "/");
    let good = dir.join("good.cfg");
    std::fs::write(&good, format!("data_dir = {data}\n")).unwrap();
    let (code, text) = run(&good);
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("ok:"), "{text}");
    assert!(
        !dir.join("data").exists(),
        "a check writes nothing and does not make the data folder"
    );

    let bad = dir.join("bad.cfg");
    std::fs::write(&bad, "session_hours = 0\n").unwrap();
    let (code, text) = run(&bad);
    assert_eq!(code, 2, "{text}");
    assert!(text.contains("FAILED:"), "{text}");

    let (code, _) = run(&dir.join("absent.cfg"));
    assert_eq!(code, 2);

    // a command line that makes no sense is still a usage error, with the same code the server has always given
    let out = Command::new(env!("CARGO_BIN_EXE_eh3d-server"))
        .arg("--no-such-option")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&dir);
}
