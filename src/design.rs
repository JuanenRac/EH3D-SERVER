//! Runs the design program on a board of a project and keeps its report as a file of the project: `POST /api/v1/apps/pcb/jobs`.
//!
//! The server does not make the design work itself: it runs the program that is installed on the computer (`phoenix`), and only when the settings say so. The run is
//! off by default (`design_run`, a switch of the security page), the program is named by its full path in `server.cfg` (`design_program`, never looked up in the search path),
//! it is started with no shell and with an empty environment, in a folder of its own that holds nothing but a copy of the board, with a time limit (`design_timeout_s`) and
//! a limit on what it may write (`design_max_output_kb`). A request that cannot be served says so with `501 design_run_unavailable`.
//!
//! The run is a job of the generic queue (plugin `phoenix`, `GET /api/v1/work/jobs/{id}`): it goes to `running` at once and ends `done` or `failed`. The report is written to the
//! project as `reports/<kind>.json` through the project store, so it has versions, quotas and the editing lock like any other file.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::{ApiError, ApiJson, Authed, Shared};
use crate::auth::{Identity, Role};
use crate::ids::{self, Kind};
use crate::jobs::now;
use crate::plugins::{PluginStatus, WorkState};
use crate::state::AppState;
use crate::sync::Locked;
use crate::work::{HistoryEntry, WorkJob};

/// The name of the plugin the jobs of this module belong to in the generic queue.
pub const PLUGIN: &str = "phoenix";

/// The application the design jobs and their reports belong to.
const APP: &str = "pcb";

/// The kinds of run, in the order the documents list them.
pub const KINDS: [&str; 4] = ["check", "bom", "fabpack", "panel"];

/// The longest stretch of the errors of the program a report keeps, in characters.
const STDERR_KEPT: usize = 4_000;

pub fn routes() -> Router<Shared> {
    Router::new().route("/api/v1/apps/pcb/jobs", post(create_job))
}

#[derive(Deserialize)]
struct NewDesignJob {
    kind: String,
    project: String,
    /// The board file of the project; it may also come as `options.file`.
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    options: Option<Value>,
}

fn bad(detail: impl Into<String>) -> ApiError {
    ApiError::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "bad_design_request",
        detail,
    )
}

fn plain(text: &str, longest: usize) -> bool {
    text.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && text.len() <= longest
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// The arguments the program gets for a kind and the options of the request, apart from the kind itself and the board. `check`, `bom` and `panel` are asked for their JSON;
/// `fabpack` is a dry run (it writes no package), and it is the only kind with options: `profile`, `lang`, `panel_boards` and `dnp`.
pub fn arguments(kind: &str, options: &Value) -> Result<Vec<String>, String> {
    if !KINDS.contains(&kind) {
        return Err(format!("kind must be one of {}", KINDS.join(", ")));
    }
    let empty = serde_json::Map::new();
    let map = match options {
        Value::Null => &empty,
        Value::Object(m) => m,
        _ => return Err("options must be an object".into()),
    };
    const OPTIONS: [&str; 4] = ["profile", "lang", "panel_boards", "dnp"];
    for key in map.keys() {
        if key == "file" {
            continue;
        }
        if kind != "fabpack" {
            return Err(format!("{kind} takes no options (not {key})"));
        }
        if !OPTIONS.contains(&key.as_str()) {
            return Err(format!("unknown option {key}"));
        }
    }
    let mut args: Vec<String> = Vec::new();
    for key in OPTIONS {
        let Some(value) = map.get(key) else {
            continue;
        };
        match key {
            "profile" | "lang" => {
                let text = value.as_str().filter(|t| plain(t, 40));
                let text = text.ok_or_else(|| format!("{key} must be a short plain name"))?;
                args.push(format!("--{key}"));
                args.push(text.to_string());
            }
            "panel_boards" => {
                let n = value.as_u64().filter(|n| (1..=1_000).contains(n));
                let n = n.ok_or("panel_boards must be a whole number from 1 to 1000")?;
                args.push("--panel-boards".into());
                args.push(n.to_string());
            }
            _ => {
                let list = value
                    .as_array()
                    .filter(|l| l.len() <= 1_000)
                    .ok_or("dnp must be a list of designators")?;
                let mut names: Vec<&str> = Vec::new();
                for item in list {
                    let name = item.as_str().filter(|t| plain(t, 24));
                    names.push(name.ok_or("a designator in dnp is not a plain name")?);
                }
                if !names.is_empty() {
                    args.push("--dnp".into());
                    args.push(names.join(","));
                }
            }
        }
    }
    let mut full: Vec<String> = Vec::new();
    if kind != "fabpack" {
        full.push("--json".into());
    }
    full.extend(args);
    Ok(full)
}

/// How a run of the program ended.
#[derive(Debug)]
pub struct Ran {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    pub truncated: bool,
}

/// Reads a pipe to its end, keeping the first `max` bytes and dropping the rest (the program is never blocked on a full pipe). Whether something was dropped.
fn drain<R: Read + Send + 'static>(
    pipe: Option<R>,
    max: usize,
) -> std::thread::JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let Some(mut pipe) = pipe else {
            return (kept, false);
        };
        let _ = pipe.by_ref().take(max as u64).read_to_end(&mut kept);
        let dropped = std::io::copy(&mut pipe, &mut std::io::sink()).unwrap_or(0);
        (kept, dropped > 0)
    })
}

/// Runs `program` with `args` in `dir`: no shell, an empty environment, no input, and stopped when it takes longer than `timeout` or writes more than `max` bytes to either
/// output (the rest is dropped). The error is the reason it did not start.
pub fn run_limited(
    program: &Path,
    args: &[String],
    dir: &Path,
    timeout: Duration,
    max: usize,
) -> Result<Ran, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(dir)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("the design program did not start ({:?})", e.kind()))?;
    let out = drain(child.stdout.take(), max);
    let err = drain(child.stderr.take(), max);
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if started.elapsed() >= timeout {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait().ok();
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => {
                let _ = child.kill();
                break child.wait().ok();
            }
        }
    };
    let (stdout, cut_out) = out.join().unwrap_or_default();
    let (stderr, cut_err) = err.join().unwrap_or_default();
    Ok(Ran {
        code: status.and_then(|s| s.code()),
        stdout,
        stderr,
        timed_out,
        truncated: cut_out || cut_err,
    })
}

/// The report as it is written to the project.
pub fn report(kind: &str, file: &str, ran: &Ran) -> Value {
    let parsed: Option<Value> = if kind == "fabpack" {
        None
    } else {
        serde_json::from_slice(&ran.stdout).ok()
    };
    let text = if parsed.is_none() {
        Some(String::from_utf8_lossy(&ran.stdout).to_string())
    } else {
        None
    };
    let stderr: String = String::from_utf8_lossy(&ran.stderr)
        .chars()
        .take(STDERR_KEPT)
        .collect();
    json!({
        "kind": kind,
        "file": file,
        "ok": ran.code == Some(0) && !ran.timed_out,
        "exit_code": ran.code,
        "timed_out": ran.timed_out,
        "truncated": ran.truncated,
        "report": parsed,
        "text": text,
        "stderr": stderr,
    })
}

fn available(s: &Shared) -> Result<(PathBuf, Duration, usize), ApiError> {
    let program = s.cfg.design_program.clone();
    match program {
        Some(path) if s.policy.get().design_run => Ok((
            path,
            Duration::from_secs(s.cfg.design_timeout_s),
            (s.cfg.design_max_output_kb as usize).saturating_mul(1024),
        )),
        _ => Err(ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "design_run_unavailable",
            "the server is not set up to run the design program (design_run and design_program)",
        )),
    }
}

/// Checks the request, copies the board out of the project and makes the job; the run goes on in a thread of its own.
fn prepare(s: &Shared, who: &Identity, b: NewDesignJob) -> Result<WorkJob, ApiError> {
    let (program, timeout, max) = available(s)?;
    if !KINDS.contains(&b.kind.as_str()) {
        return Err(bad(format!("kind must be one of {}", KINDS.join(", "))));
    }
    let options = b.options.clone().unwrap_or(Value::Null);
    let args = arguments(&b.kind, &options).map_err(bad)?;
    let file = b
        .file
        .clone()
        .or_else(|| {
            options
                .get("file")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .ok_or_else(|| bad("give the board file of the project in file"))?;
    crate::pstore::check_file_name(&file)?;
    let bytes = {
        let book = s.pstore.book.locked();
        let (version, path) = book.locate(APP, &b.project, who, &file, None)?;
        crate::pstore::read_verified(&path, &version)?
    };
    if s.work.jobs.locked().queued_count() >= s.cfg.work_queue_max {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "queue_full",
            "",
        ));
    }
    let at = now();
    let job = WorkJob {
        id: ids::new(Kind::Work),
        machine: String::new(),
        plugin: PLUGIN.to_string(),
        app: APP.to_string(),
        name: format!("{} {}", b.kind, b.project)
            .chars()
            .take(80)
            .collect(),
        owner: who.owner(),
        by: who.name.clone(),
        state: WorkState::Running,
        created: at,
        started: Some(at),
        finished: None,
        native: String::new(),
        done: 0,
        total: 1,
        message: String::new(),
        history: vec![HistoryEntry {
            at,
            state: WorkState::Running,
            by: who.name.clone(),
            note: "started".into(),
        }],
        view: json!({
            "kind": b.kind, "project": b.project, "file": file,
            "report": format!("reports/{}.json", b.kind),
        }),
        decile: 0,
    };
    s.work.jobs.locked().add(job.clone())?;
    s.announce_job("job.created", &job);
    let run = Run {
        state: s.clone(),
        job: job.id.clone(),
        who: who.clone(),
        kind: b.kind,
        project: b.project,
        file,
        bytes,
        args,
        program,
        timeout,
        max,
    };
    let spawned = std::thread::Builder::new()
        .name("eh3d-design".into())
        .spawn(move || run.go());
    if spawned.is_err() {
        finish(
            s,
            &job.id,
            WorkState::Failed,
            "the run could not be started",
        );
    }
    Ok(s.work.jobs.locked().get(&job.id).cloned().unwrap_or(job))
}

/// What a run needs; it owns all of it because it goes on in a thread of its own.
struct Run {
    state: Arc<AppState>,
    job: String,
    who: Identity,
    kind: String,
    project: String,
    file: String,
    bytes: Vec<u8>,
    args: Vec<String>,
    program: PathBuf,
    timeout: Duration,
    max: usize,
}

/// Ends the job and tells the stream.
fn finish(s: &AppState, job: &str, state: WorkState, message: &str) {
    let status = PluginStatus {
        state,
        done: 1,
        total: 1,
        message: message.chars().take(200).collect(),
    };
    let update = s.work.jobs.locked().apply_status(job, &status, now());
    if let Some(u) = update {
        if u.state_changed {
            s.announce_job("job.state", &u.job);
        }
    }
}

/// The extension of the board file for the copy the program reads (the program tells the format from it).
fn extension_of(file: &str) -> String {
    let name = file.rsplit('/').next().unwrap_or(file);
    match name.rsplit_once('.') {
        Some((_, ext)) if plain(ext, 12) => format!(".{}", ext.to_ascii_lowercase()),
        _ => String::new(),
    }
}

impl Run {
    fn go(self) {
        let started = Instant::now();
        let scratch = self
            .state
            .cfg
            .data_dir
            .join("design-scratch")
            .join(&self.job);
        let board = format!("board{}", extension_of(&self.file));
        let outcome = self.work_in(&scratch, &board);
        let _ = std::fs::remove_dir_all(&scratch);
        let (state, message) = match outcome {
            Ok((state, message)) => (state, message),
            Err(message) => (WorkState::Failed, message),
        };
        self.state.audit.event(
            "design_run",
            &[
                ("who", self.who.name.as_str()),
                ("project", self.project.as_str()),
                ("kind", self.kind.as_str()),
                ("result", state.name()),
                ("ms", &started.elapsed().as_millis().to_string()),
            ],
        );
        finish(&self.state, &self.job, state, &message);
    }

    /// Runs the program on a copy of the board and writes the report to the project. The error is the reason the report could not be made.
    fn work_in(&self, scratch: &Path, board: &str) -> Result<(WorkState, String), String> {
        std::fs::create_dir_all(scratch).map_err(|_| "the work folder could not be made")?;
        std::fs::write(scratch.join(board), &self.bytes)
            .map_err(|_| "the board could not be copied")?;
        let mut args = vec![self.kind.clone()];
        args.extend(self.args.iter().cloned());
        args.push(board.to_string());
        let ran = run_limited(&self.program, &args, scratch, self.timeout, self.max)?;
        let text = serde_json::to_vec_pretty(&report(&self.kind, &self.file, &ran))
            .map_err(|_| "the report could not be written")?;
        let name = format!("reports/{}.json", self.kind);
        let mut book = self.state.pstore.book.locked();
        let current = book
            .locate(APP, &self.project, &self.who, &name, None)
            .ok()
            .map(|(version, _)| version.sha256);
        let put = book.put_file(
            APP,
            &self.project,
            &self.who,
            &name,
            &text,
            current.as_deref(),
            &format!("phoenix {}", self.kind),
            now(),
        );
        if let Err(e) = put {
            return Err(format!("the report was not kept ({})", e.code()));
        }
        drop(book);
        if ran.timed_out {
            return Ok((
                WorkState::Failed,
                format!("stopped after {} s", self.timeout.as_secs()),
            ));
        }
        match ran.code {
            Some(0) => Ok((WorkState::Done, format!("{name} written"))),
            Some(code) => Ok((
                WorkState::Failed,
                format!("the program ended with code {code}; {name} written"),
            )),
            None => Ok((
                WorkState::Failed,
                format!("the program was stopped; {name} written"),
            )),
        }
    }
}

async fn create_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(b): ApiJson<NewDesignJob>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:prepare")?;
    crate::api::need_scope(&who, "projects:write")?;
    let state = s.clone();
    let by = who.clone();
    let (kind, project) = (b.kind.clone(), b.project.clone());
    let job = tokio::task::spawn_blocking(move || prepare(&state, &by, b))
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))??;
    crate::api::note(
        &s,
        "design_job_create",
        &who,
        &[("job", &job.id), ("project", &project), ("kind", &kind)],
    );
    Ok((StatusCode::CREATED, Json(job.full())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_fabpack_takes_options_and_the_other_kinds_ask_for_json() {
        assert_eq!(
            arguments("check", &Value::Null).unwrap(),
            vec!["--json".to_string()]
        );
        assert_eq!(
            arguments("bom", &json!({"file": "a.json"})).unwrap(),
            vec!["--json".to_string()]
        );
        assert!(arguments("panel", &json!({"profile": "x"})).is_err());
        let args = arguments(
            "fabpack",
            &json!({"profile": "jlc", "panel_boards": 4, "dnp": ["R1", "C2"]}),
        )
        .unwrap();
        assert_eq!(
            args,
            vec!["--profile", "jlc", "--panel-boards", "4", "--dnp", "R1,C2"]
        );
        assert!(arguments("fabpack", &Value::Null).unwrap().is_empty());
    }

    #[test]
    fn what_could_become_another_argument_or_a_path_is_refused() {
        assert!(arguments("fabpack", &json!({"profile": "--out"})).is_err());
        assert!(arguments("fabpack", &json!({"profile": "a b"})).is_err());
        assert!(arguments("fabpack", &json!({"profile": "../x"})).is_err());
        assert!(arguments("fabpack", &json!({"lang": ""})).is_err());
        assert!(arguments("fabpack", &json!({"panel_boards": 0})).is_err());
        assert!(arguments("fabpack", &json!({"panel_boards": 1001})).is_err());
        assert!(arguments("fabpack", &json!({"dnp": ["R1;rm"]})).is_err());
        assert!(arguments("fabpack", &json!({"out": "x.zip"})).is_err());
        assert!(arguments("fabpack", &json!(["a"])).is_err());
        assert!(arguments("route", &Value::Null).is_err());
    }

    #[test]
    fn the_copy_of_the_board_keeps_a_plain_extension_only() {
        assert_eq!(extension_of("boards/Main.KiCad_PCB"), ".kicad_pcb");
        assert_eq!(extension_of("a/b.json"), ".json");
        assert_eq!(extension_of("noext"), "");
        assert_eq!(extension_of("dir.x/noext"), "");
        assert_eq!(extension_of("odd.e xt"), "");
    }

    #[test]
    fn the_report_holds_the_json_of_the_program_or_its_text() {
        let ran = Ran {
            code: Some(0),
            stdout: br#"{"violations":[]}"#.to_vec(),
            stderr: Vec::new(),
            timed_out: false,
            truncated: false,
        };
        let r = report("check", "a.json", &ran);
        assert_eq!(r["ok"], true);
        assert_eq!(r["report"]["violations"], json!([]));
        assert!(r["text"].is_null());
        let dry = Ran {
            code: Some(2),
            stdout: b"dry run".to_vec(),
            stderr: b"oops".to_vec(),
            timed_out: false,
            truncated: false,
        };
        let r = report("fabpack", "a.json", &dry);
        assert_eq!(r["ok"], false);
        assert!(r["report"].is_null());
        assert_eq!(r["text"], "dry run");
        assert_eq!(r["stderr"], "oops");
        assert_eq!(r["exit_code"], 2);
    }

    #[test]
    fn a_program_that_is_not_there_does_not_start() {
        let missing = std::env::temp_dir().join("eh3d-no-such-program-here");
        let err = run_limited(
            &missing,
            &[],
            &std::env::temp_dir(),
            Duration::from_secs(5),
            1024,
        )
        .unwrap_err();
        assert!(err.contains("did not start"), "{err}");
    }
}
