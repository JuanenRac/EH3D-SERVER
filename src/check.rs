//! `eh3d-server --check [--config FILE]`: verifies what the server would start from, without starting it and without writing anything: the file of the settings, the file of
//! the changes of the security page (`policy.json`) in the data folder and the contract of the settings the server embeds.
//!
//! The exit code tells what happened: [`EXIT_OK`] when every check passed, [`EXIT_NOT_VERIFIED`] when a check did not pass (the server would not start as it should, or would
//! start with something it does not understand), and [`EXIT_USAGE`] for a command line that makes no sense.

use std::path::Path;

use crate::{config, policy, settings};

/// Every check passed.
pub const EXIT_OK: i32 = 0;
/// The command could not be run (the server could not open its data).
pub const EXIT_FAILED: i32 = 1;
/// The verification was not passed: at least one check failed.
pub const EXIT_NOT_VERIFIED: i32 = 2;

/// What the checks found, one line each: `ok:` or `FAILED:` and what.
pub struct Report {
    pub lines: Vec<String>,
    pub passed: bool,
}

impl Report {
    /// The exit code of the command for this report.
    pub fn exit_code(&self) -> i32 {
        if self.passed {
            EXIT_OK
        } else {
            EXIT_NOT_VERIFIED
        }
    }
}

/// Runs the checks. A check that cannot be made counts as not passed: an answer of "I could not tell" is never "ok".
pub fn verify(path: Option<&Path>) -> Report {
    let mut lines = Vec::new();
    let mut passed = true;
    let cfg = match config::load(path) {
        Ok(cfg) => {
            lines.push(
                "ok: the file of the settings is read and every value is in range".to_string(),
            );
            Some(cfg)
        }
        Err(e) => {
            lines.push(format!("FAILED: the file of the settings: {e}"));
            passed = false;
            None
        }
    };
    if let Some(cfg) = cfg {
        match policy::Policy::open(&cfg, &cfg.data_dir) {
            Ok(p) => {
                lines.push(format!(
                    "ok: the changes of the security page in {} are read",
                    cfg.data_dir.display()
                ));
                let doc = settings::document(&cfg, &p.get());
                if doc["values"].as_object().is_some_and(|v| !v.is_empty()) {
                    lines.push(
                        "ok: the contract of the settings gives a value for each key".to_string(),
                    );
                } else {
                    lines.push("FAILED: the contract of the settings has no values".to_string());
                    passed = false;
                }
                // the light view for the screens must fit its limit with the settings this server would start with
                let light = settings::values_view(&cfg, &p.get(), &settings::Selection::default())
                    .to_string()
                    .len();
                if light <= settings::VALUES_MAX_BYTES {
                    lines.push(format!(
                        "ok: the light view of the settings is {light} bytes (the limit is {})",
                        settings::VALUES_MAX_BYTES
                    ));
                } else {
                    lines.push(format!(
                        "FAILED: the light view of the settings is {light} bytes and the limit is {}",
                        settings::VALUES_MAX_BYTES
                    ));
                    passed = false;
                }
            }
            Err(e) => {
                lines.push(format!("FAILED: the changes of the security page: {e}"));
                passed = false;
            }
        }
        // the list of the nodes of the pick-and-place machines must read (a damaged one is never taken for an empty one)
        match crate::pnp_nodes::PnpNodes::file_reads(&cfg.data_dir) {
            Ok(n) => lines.push(format!(
                "ok: the list of the pick-and-place nodes is read ({n} node(s))"
            )),
            Err(e) => {
                lines.push(format!("FAILED: the list of the pick-and-place nodes: {e}"));
                passed = false;
            }
        }
    }
    Report { lines, passed }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("eh3d-server-check-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_good_file_passes_and_the_exit_code_is_zero() {
        let dir = folder("good");
        let file = dir.join("server.cfg");
        std::fs::write(
            &file,
            format!("data_dir = {}\n", dir.join("data").display()).replace('\\', "/"),
        )
        .unwrap();
        let r = verify(Some(&file));
        assert!(r.passed, "{:?}", r.lines);
        assert_eq!(r.exit_code(), EXIT_OK);
        assert!(r.lines.iter().all(|l| l.starts_with("ok:")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_value_out_of_range_or_a_missing_file_is_not_verified_and_the_exit_code_is_two() {
        let dir = folder("bad");
        let file = dir.join("server.cfg");
        std::fs::write(&file, "session_hours = 100000000\nno_such_key = 1\n").unwrap();
        let r = verify(Some(&file));
        assert!(!r.passed);
        assert_eq!(r.exit_code(), EXIT_NOT_VERIFIED);
        assert!(r.lines[0].starts_with("FAILED:"));
        let missing = verify(Some(&dir.join("absent.cfg")));
        assert_eq!(missing.exit_code(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_policy_file_that_cannot_be_read_is_not_verified() {
        let dir = folder("policy");
        let file = dir.join("server.cfg");
        let data = dir.join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            &file,
            format!("data_dir = {}\n", data.display()).replace('\\', "/"),
        )
        .unwrap();
        std::fs::write(data.join("policy.json"), "{ broken").unwrap();
        let r = verify(Some(&file));
        assert_eq!(r.exit_code(), EXIT_NOT_VERIFIED, "{:?}", r.lines);
        assert!(r.lines.iter().any(|l| l.starts_with("FAILED: the changes")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
