//! What the server tells about what happens: the audit (who did what) and the log of the jobs.
//!
//! Both are lines of `key=value` text. The audit line goes to the console (the journal of the service keeps it with its date) and, when the settings say so, to
//! `audit.jsonl` in the data folder, one JSON object per line. The log of the jobs, `eh3d-jobs.log`, has one line at the start of every job and one at its end, with
//! what is needed to understand a failure afterwards. No secret is ever written: no password, no token and no part of one.
//!
//! The files are set aside at a size (`log_max_kb`): the full one becomes `<name>.1` and a new one starts, so they never grow without bound on the SD card of the CM5.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A text file that grows by lines and is set aside when it gets big. A line that cannot be written is lost without a word: the log is not worth stopping a job for.
pub struct LogFile {
    path: PathBuf,
    max_bytes: u64,
    enabled: bool,
    lock: Mutex<()>,
}

impl LogFile {
    pub fn new(path: PathBuf, enabled: bool, max_kb: u64) -> LogFile {
        LogFile {
            path,
            max_bytes: max_kb.saturating_mul(1024),
            enabled,
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append(&self, line: &str) {
        if !self.enabled {
            return;
        }
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let size = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        if size > 0 && size + line.len() as u64 + 1 > self.max_bytes {
            let mut older = self.path.as_os_str().to_owned();
            older.push(".1");
            let _ = std::fs::rename(&self.path, PathBuf::from(older));
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// A value as it goes in a line: as it is when it is made of plain characters, else between quotes (JSON text), so that a line splits on its spaces.
pub fn value_text(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '@' | '-'));
    if plain {
        value.to_string()
    } else {
        serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into())
    }
}

/// `k=v k=v ...`.
pub fn pairs(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{k}={}", value_text(v)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `event=name k=v ...`.
pub fn line(event: &str, fields: &[(&str, &str)]) -> String {
    let mut out = format!("event={}", value_text(event));
    if !fields.is_empty() {
        out.push(' ');
        out.push_str(&pairs(fields));
    }
    out
}

/// The audit of the server.
pub struct Audit {
    file: LogFile,
}

impl Audit {
    pub fn new(data_dir: &Path, file: bool, max_kb: u64) -> Audit {
        Audit {
            file: LogFile::new(data_dir.join("audit.jsonl"), file, max_kb),
        }
    }

    pub fn path(&self) -> &Path {
        self.file.path()
    }

    /// Says that something happened, and who did it (`who`, `role` and, when it is known, `ip` are the fields to give).
    pub fn event(&self, event: &str, fields: &[(&str, &str)]) {
        println!("{}", line(event, fields));
        if self.file.enabled {
            let mut object = serde_json::Map::new();
            object.insert("t".into(), serde_json::json!(crate::jobs::now()));
            object.insert("event".into(), serde_json::json!(event));
            for (k, v) in fields {
                if *k != "t" && *k != "event" {
                    object.insert((*k).into(), serde_json::json!(v));
                }
            }
            self.file
                .append(&serde_json::Value::Object(object).to_string());
        }
    }
}

/// What `GET /api/v1/audit` asks for: every field that is `None` lets all the lines through.
pub struct Filter {
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub who: Option<String>,
    pub event: Option<String>,
    pub limit: usize,
}

/// The lines of the audit file that match, the newest first and at most `filter.limit`. The older copy the file was set aside as is read first. A line that does not read is
/// skipped. Each line comes as an object with `at` (seconds since 1970), `event` and the fields the event was written with.
pub fn read_lines(path: &Path, filter: &Filter) -> Vec<serde_json::Value> {
    let mut older = path.as_os_str().to_owned();
    older.push(".1");
    let mut found: Vec<serde_json::Value> = Vec::new();
    for file in [PathBuf::from(older), path.to_path_buf()] {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for raw in text.lines() {
            let Ok(serde_json::Value::Object(mut line)) = serde_json::from_str(raw) else {
                continue;
            };
            let at = line.remove("t").and_then(|v| v.as_u64()).unwrap_or(0);
            if filter.since.is_some_and(|s| at < s) || filter.until.is_some_and(|u| at > u) {
                continue;
            }
            let text_of = |key: &str| line.get(key).and_then(|v| v.as_str()).map(str::to_string);
            if filter.who.is_some() && text_of("who") != filter.who {
                continue;
            }
            if filter.event.is_some() && text_of("event") != filter.event {
                continue;
            }
            line.insert("at".into(), serde_json::json!(at));
            found.push(serde_json::Value::Object(line));
        }
    }
    found.reverse();
    found.truncate(filter.limit);
    found
}

/// The moment of a count of seconds since 1970 in UTC, as `2000-01-31T12:00:00Z`.
pub fn iso(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // days since 1970-01-01 to a civil date (the algorithm of H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("eh3d-audit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn values_with_spaces_or_quotes_are_quoted_and_plain_ones_are_not() {
        assert_eq!(value_text("juan"), "juan");
        assert_eq!(value_text("10.0.0.7"), "10.0.0.7");
        assert_eq!(value_text("two words"), "\"two words\"");
        assert_eq!(value_text("a\"b\nc"), "\"a\\\"b\\nc\"");
        assert_eq!(value_text(""), "\"\"");
        assert_eq!(
            line("job_start", &[("job", "ab12"), ("name", "my board")]),
            "event=job_start job=ab12 name=\"my board\""
        );
    }

    #[test]
    fn the_dates_are_those_of_the_calendar() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso(1_709_210_096), "2024-02-29T12:34:56Z");
        assert_eq!(iso(4_102_444_799), "2099-12-31T23:59:59Z");
    }

    #[test]
    fn the_audit_file_gets_one_json_object_per_event_and_can_be_off() {
        let d = dir("audit");
        let on = Audit::new(&d, true, 1024);
        on.event(
            "login",
            &[("who", "juan"), ("ip", "10.0.0.7"), ("event", "forged")],
        );
        on.event("logout", &[("who", "juan")]);
        let text = std::fs::read_to_string(on.path()).unwrap();
        let rows: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["event"], "login");
        assert_eq!(rows[0]["who"], "juan");
        assert!(rows[0]["t"].as_u64().unwrap() > 1_000_000_000);
        let d2 = dir("audit-off");
        Audit::new(&d2, false, 1024).event("login", &[("who", "juan")]);
        assert!(!d2.join("audit.jsonl").exists());
    }

    #[test]
    fn the_audit_is_read_back_newest_first_with_its_filters() {
        let d = dir("read");
        let path = d.join("audit.jsonl");
        std::fs::create_dir_all(&d).unwrap();
        let rows = [
            r#"{"t":100,"event":"login","who":"juan"}"#,
            r#"{"t":200,"event":"logout","who":"juan"}"#,
            "not json at all",
            r#"{"t":300,"event":"login","who":"ana"}"#,
        ];
        std::fs::write(
            d.join("audit.jsonl.1"),
            format!(
                "{}
",
                rows[0]
            ),
        )
        .unwrap();
        std::fs::write(
            &path,
            format!(
                "{}
{}
{}
",
                rows[1], rows[2], rows[3]
            ),
        )
        .unwrap();
        let all = Filter {
            since: None,
            until: None,
            who: None,
            event: None,
            limit: 10,
        };
        let list = read_lines(&path, &all);
        let times: Vec<u64> = list.iter().map(|l| l["at"].as_u64().unwrap()).collect();
        assert_eq!(times, vec![300, 200, 100]);
        assert!(list[0].get("t").is_none());
        let juan = Filter {
            who: Some("juan".into()),
            ..all
        };
        assert_eq!(read_lines(&path, &juan).len(), 2);
        let logins_since = Filter {
            since: Some(150),
            event: Some("login".into()),
            limit: 10,
            until: None,
            who: None,
        };
        let found = read_lines(&path, &logins_since);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["who"], "ana");
        let one = Filter {
            since: None,
            until: Some(250),
            who: None,
            event: None,
            limit: 1,
        };
        assert_eq!(read_lines(&path, &one)[0]["at"], 200);
        assert!(read_lines(&d.join("missing.jsonl"), &one).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_full_file_is_set_aside_and_a_new_one_starts() {
        let d = dir("rotate");
        let log = LogFile::new(d.join("x.log"), true, 16);
        let long = "x".repeat(600);
        for _ in 0..40 {
            log.append(&long);
        }
        let now = std::fs::metadata(d.join("x.log")).unwrap().len();
        assert!(now <= 16 * 1024, "the file stays under its limit: {now}");
        assert!(d.join("x.log.1").exists());
    }
}
