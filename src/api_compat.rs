//! The compatibility routes: a few read-only routes at the root of the address that answer the way Moonraker and OctoPrint do, so that a program written for them can read the
//! state of a 3D printer of the plugin `printer3d`. They are **off** unless the switch `compat_api` of the security page is on (`404 compat_off` otherwise), and each needs
//! the API key of one machine in the header `X-Api-Key` (made by an administrator with `POST /api/v1/work/machines/{id}/compat/key`); the key opens that machine and no other.
//! The machine must also be set to give the shape (its setting `compat`: `moonraker`, `octoprint` or `both`). The reads are the first group of routes. The routes that WRITE
//! (the second group: pause, resume and cancel a print, start one that waits in the queue, upload a file to the queue) are off behind a switch of their own, `compat_write`
//! (`404 compat_write_off`), need a key that was made to write (`POST .../compat/key` with `write: true`) and act as an operator that exists only for that machine. An uploaded file is
//! checked, prepared and WAITS in the queue for a person to start it; nothing ever starts by itself, a program may start a waiting print only if the machine's setting
//! `compat_start` says so, and sending G-code and the websocket are not served. The shapes are the ones of `GET /api/v1/work/machines/{id}/compat/...`.
//! They are not part of the OpenAPI document of the interface, because they are not under `/api/v1/`; `docs/MACHINE_PLUGINS.md` describes them.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::{ApiError, ApiJson, ApiResult, Shared};
use crate::auth::{Identity, Role};
use crate::plugins::{PluginError, WorkState};
use crate::printer3d::PrinterConfig;
use crate::sync::Locked;
use crate::work::{Fail, WError};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/server/info", get(moonraker_server_info))
        .route("/printer/info", get(moonraker_printer_info))
        .route("/printer/objects/query", get(moonraker_query))
        .route("/api/version", get(octoprint_version))
        .route("/api/printer", get(octoprint_printer))
        .route("/api/job", get(octoprint_job).post(octoprint_job_command))
        .route("/printer/print/start", post(moonraker_start))
        .route("/printer/print/pause", post(moonraker_pause))
        .route("/printer/print/resume", post(moonraker_resume))
        .route("/printer/print/cancel", post(moonraker_cancel))
        .route("/server/files/upload", post(moonraker_upload))
        .route("/api/files/local", post(octoprint_upload))
}

/// The machine the request is for: the switch is on and the key in `X-Api-Key` belongs to a machine of the plugin `printer3d` that still exists.
fn gate(s: &Shared, headers: &HeaderMap) -> Result<String, ApiError> {
    if !s.policy.get().compat_api {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "compat_off",
            "the compatibility routes are off (the switch compat_api)",
        ));
    }
    let denied = || {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "the header X-Api-Key is missing or is not the key of a machine",
        )
    };
    let key = headers
        .get(crate::compat::HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .ok_or_else(denied)?;
    let machine = s.compat_keys.find(key).ok_or_else(denied)?;
    let of_a_printer = s
        .work_find(&machine)
        .is_some_and(|p| p.kind() == "printer3d");
    if of_a_printer {
        Ok(machine)
    } else {
        Err(denied())
    }
}

/// The state of the printer in the shape of `flavor` (`moonraker` or `octoprint`), asked of the plugin off the thread that answers.
async fn printer_shape(
    s: &Shared,
    headers: &HeaderMap,
    flavor: &'static str,
) -> Result<(String, Value), ApiError> {
    let machine = gate(s, headers)?;
    let plugin = s.work_find(&machine).ok_or(PluginError::UnknownMachine)?;
    let route = if flavor == "moonraker" {
        "compat/moonraker"
    } else {
        "compat/octoprint"
    };
    let state: Arc<crate::state::AppState> = s.clone();
    let id = machine.clone();
    let value = tokio::task::spawn_blocking(move || {
        plugin.operation(&state, &id, route, &Value::Null, "compat")
    })
    .await
    .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))??;
    Ok((machine, value))
}

fn version() -> String {
    format!("eh3d-server {}", env!("CARGO_PKG_VERSION"))
}

/// The word Moonraker calls the state of the printer by, from the shape.
fn webhooks_state(shape: &Value) -> &str {
    shape["result"]["status"]["webhooks"]["state"]
        .as_str()
        .unwrap_or("shutdown")
}

async fn moonraker_server_info(State(s): State<Shared>, headers: HeaderMap) -> ApiResult {
    let (_, shape) = printer_shape(&s, &headers, "moonraker").await?;
    let ready = webhooks_state(&shape) == "ready";
    Ok(Json(json!({
        "result": {
            "klippy_connected": ready,
            "klippy_state": webhooks_state(&shape),
            "components": [],
            "failed_components": [],
            "registered_directories": [],
            "warnings": [],
            "websocket_count": 0,
            "moonraker_version": version(),
            "api_version": [1, 0, 0],
            "api_version_string": "1.0.0",
        }
    })))
}

async fn moonraker_printer_info(State(s): State<Shared>, headers: HeaderMap) -> ApiResult {
    let (machine, shape) = printer_shape(&s, &headers, "moonraker").await?;
    let state = webhooks_state(&shape);
    Ok(Json(json!({
        "result": {
            "state": state,
            "state_message": format!("Printer is {state}"),
            "hostname": machine,
            "software_version": version(),
        }
    })))
}

async fn moonraker_query(State(s): State<Shared>, headers: HeaderMap) -> ApiResult {
    let (_, shape) = printer_shape(&s, &headers, "moonraker").await?;
    Ok(Json(shape))
}

async fn octoprint_version(State(s): State<Shared>, headers: HeaderMap) -> ApiResult {
    // the key is checked; the state of the printer is not needed to say which server this is
    gate(&s, &headers)?;
    Ok(Json(json!({
        "api": "0.1",
        "server": env!("CARGO_PKG_VERSION"),
        "text": version(),
    })))
}

async fn octoprint_printer(State(s): State<Shared>, headers: HeaderMap) -> ApiResult {
    let (_, shape) = printer_shape(&s, &headers, "octoprint").await?;
    Ok(Json(shape["printer"].clone()))
}

async fn octoprint_job(State(s): State<Shared>, headers: HeaderMap) -> ApiResult {
    let (_, shape) = printer_shape(&s, &headers, "octoprint").await?;
    Ok(Json(shape["job"].clone()))
}

// ---- the routes that write ---------------------------------------------------------------------------------------------------------------

/// The most parts a body of an upload may have (the file, and a few fields of the program that sends it).
const MAX_PARTS: usize = 12;

/// The identity the work of a compatibility key is done as: an operator that exists only for this machine. A job it makes is owned by it, and nothing it makes is anyone else's.
fn compat_identity(machine: &str) -> Identity {
    Identity {
        name: format!("compat:{machine}"),
        role: Role::Operator,
        is_token: true,
        token_id: Some(format!("compat-{machine}")),
        scopes: None,
        template: None,
        uid: None,
        expires: None,
    }
}

/// The machine a request that writes is for: the routes are on (`compat_api`), the key is the machine's, the switch `compat_write` is on and the key was made to write.
/// Also the settings of the machine, which must give the shape the route is of (`moonraker` or `octoprint`).
fn write_gate(
    s: &Shared,
    headers: &HeaderMap,
    flavor: &str,
) -> Result<(String, PrinterConfig), ApiError> {
    let machine = gate(s, headers)?;
    if !s.policy.get().compat_write {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "compat_write_off",
            "the write routes are off (the switch compat_write)",
        ));
    }
    if !s.compat_keys.can_write(&machine) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "the key of this machine was not made to write (POST .../compat/key with write: true)",
        ));
    }
    let settings = crate::plugins::work_machine_settings(s, &machine, "printer3d")
        .ok_or(PluginError::UnknownMachine)?;
    let cfg = PrinterConfig::from_settings(&settings).map_err(|why| {
        ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_plugin_request", why)
    })?;
    if cfg.compat != "both" && cfg.compat != flavor {
        return Err(PluginError::NotSupported(
            "the machine does not give this shape (its setting compat)",
        )
        .into());
    }
    Ok((machine, cfg))
}

/// Runs the work of the queue off the thread that answers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Fail> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))?
        .map_err(ApiError::from)
}

/// The job that waits in the queue of the machine under the name of a file the program uploaded.
fn waiting_job(s: &Shared, machine: &str, filename: &str) -> Result<String, ApiError> {
    let name = base_name(filename);
    s.work
        .jobs
        .locked()
        .list()
        .iter()
        .find(|j| j.machine == machine && j.state == WorkState::Queued && j.name == name)
        .map(|j| j.id.clone())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_job",
                "no file of that name waits in the queue of the machine",
            )
        })
}

/// The job that runs or is held on the machine.
fn running_job(s: &Shared, machine: &str) -> Result<(String, WorkState), ApiError> {
    s.work
        .jobs
        .locked()
        .active_on(machine)
        .map(|j| (j.id.clone(), j.state))
        .ok_or_else(|| ApiError::from(WError::NotInState))
}

/// The file name of a path a program sent, without folders.
fn base_name(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Starts the print that waits under `filename`, if the machine lets a program do it.
async fn start_waiting(
    s: &Shared,
    machine: &str,
    cfg: &PrinterConfig,
    filename: &str,
) -> Result<(), ApiError> {
    if !cfg.compat_start {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "a print is started by a person with the routes of the server; the setting compat_start of the machine lets a program do it",
        ));
    }
    let id = waiting_job(s, machine, filename)?;
    let state = s.clone();
    let by = compat_identity(machine);
    blocking(move || state.work_start(&id, &by)).await?;
    audit_write(s, machine, "start", filename);
    Ok(())
}

async fn hold_running(s: &Shared, machine: &str, what: &str) -> Result<(), ApiError> {
    let (id, state) = running_job(s, machine)?;
    let (shared, by) = (s.clone(), compat_identity(machine));
    match (what, state) {
        ("pause", WorkState::Running) => {
            blocking(move || shared.work_pause(&id, &by)).await?;
        }
        ("resume", WorkState::Paused) => {
            blocking(move || shared.work_resume(&id, &by)).await?;
        }
        ("cancel", _) => {
            blocking(move || shared.work_cancel(&id, &by)).await?;
        }
        _ => return Err(WError::NotInState.into()),
    }
    audit_write(s, machine, what, "");
    Ok(())
}

fn audit_write(s: &Shared, machine: &str, what: &str, file: &str) {
    s.audit.event(
        "compat_write",
        &[("machine", machine), ("what", what), ("file", file)],
    );
}

/// Takes an uploaded file into the queue of the machine: the program is checked and prepared like any job and WAITS there for a person to start it. Nothing starts.
async fn queue_upload(
    s: &Shared,
    machine: &str,
    content_type: &str,
    body: &[u8],
) -> Result<(String, String), ApiError> {
    let parts = crate::multipart::parse(content_type, body, MAX_PARTS)
        .map_err(|why| ApiError::new(StatusCode::BAD_REQUEST, "bad_request", why))?;
    let file = parts
        .iter()
        .find(|p| p.name == "file" && p.filename.is_some())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "the body has no part `file` with a file name",
            )
        })?;
    let name = base_name(file.filename.as_deref().unwrap_or(""));
    if !crate::apps::valid_label(&name, 80) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_filename",
            "the name of the file is 1 to 80 characters with no control characters",
        ));
    }
    let text = String::from_utf8(file.data.clone()).map_err(|_| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_gcode",
            "the file is not text",
        )
    })?;
    // a file with the name of one that waits replaces it: the old one is let go
    if let Ok(old) = waiting_job(s, machine, &name) {
        let (shared, by) = (s.clone(), compat_identity(machine));
        blocking(move || shared.work_cancel(&old, &by)).await?;
    }
    let request = json!({"kind": "print", "text": text, "name": name});
    let (shared, by, m, n) = (
        s.clone(),
        compat_identity(machine),
        machine.to_string(),
        name.clone(),
    );
    let job = blocking(move || shared.work_create(&by, &m, Some(&n), "compat", &request)).await?;
    audit_write(s, machine, "upload", &name);
    Ok((job.id, name))
}

fn content_type(headers: &HeaderMap) -> String {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

#[derive(Deserialize)]
struct StartQuery {
    #[serde(default)]
    filename: String,
}

async fn moonraker_start(
    State(s): State<Shared>,
    headers: HeaderMap,
    Query(q): Query<StartQuery>,
) -> ApiResult {
    let (machine, cfg) = write_gate(&s, &headers, "moonraker")?;
    start_waiting(&s, &machine, &cfg, &q.filename).await?;
    Ok(Json(json!({"result": "ok"})))
}

async fn moonraker_pause(State(s): State<Shared>, headers: HeaderMap) -> ApiResult {
    let (machine, _) = write_gate(&s, &headers, "moonraker")?;
    hold_running(&s, &machine, "pause").await?;
    Ok(Json(json!({"result": "ok"})))
}

async fn moonraker_resume(State(s): State<Shared>, headers: HeaderMap) -> ApiResult {
    let (machine, _) = write_gate(&s, &headers, "moonraker")?;
    hold_running(&s, &machine, "resume").await?;
    Ok(Json(json!({"result": "ok"})))
}

async fn moonraker_cancel(State(s): State<Shared>, headers: HeaderMap) -> ApiResult {
    let (machine, _) = write_gate(&s, &headers, "moonraker")?;
    hold_running(&s, &machine, "cancel").await?;
    Ok(Json(json!({"result": "ok"})))
}

async fn moonraker_upload(
    State(s): State<Shared>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult {
    let (machine, _) = write_gate(&s, &headers, "moonraker")?;
    let (job, name) = queue_upload(&s, &machine, &content_type(&headers), &body).await?;
    Ok(Json(json!({
        "item": {"path": name, "root": "gcodes", "size": body.len(), "permissions": "rw"},
        "print_queued": true,
        "print_started": false,
        "eh3d": {"job": job, "state": "queued", "note": "the print waits for a person to start it"},
    })))
}

async fn octoprint_upload(
    State(s): State<Shared>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (machine, _) = write_gate(&s, &headers, "octoprint")?;
    let (job, name) = queue_upload(&s, &machine, &content_type(&headers), &body).await?;
    // `print` and `select` in the body are read as a wish and not obeyed: nothing starts by itself
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "files": {"local": {"name": name, "origin": "local", "path": name, "refs": {}}},
            "done": true,
            "eh3d": {"job": job, "state": "queued", "note": "the print waits for a person to start it"},
        })),
    ))
}

#[derive(Deserialize)]
struct OctoCommand {
    command: String,
    #[serde(default)]
    action: Option<String>,
    /// The file to start; OctoPrint starts the file that is selected, here the one that was uploaded last unless the program names one.
    #[serde(default)]
    filename: Option<String>,
}

async fn octoprint_job_command(
    State(s): State<Shared>,
    headers: HeaderMap,
    ApiJson(c): ApiJson<OctoCommand>,
) -> Result<StatusCode, ApiError> {
    let (machine, cfg) = write_gate(&s, &headers, "octoprint")?;
    match (c.command.as_str(), c.action.as_deref()) {
        ("start", _) => {
            let name = match c.filename {
                Some(n) => n,
                None => last_waiting(&s, &machine)?,
            };
            start_waiting(&s, &machine, &cfg, &name).await?;
        }
        ("cancel", _) => hold_running(&s, &machine, "cancel").await?,
        ("pause", Some("pause")) => hold_running(&s, &machine, "pause").await?,
        ("pause", Some("resume")) => hold_running(&s, &machine, "resume").await?,
        ("pause", Some("toggle")) => {
            let (_, state) = running_job(&s, &machine)?;
            let what = if state == WorkState::Paused {
                "resume"
            } else {
                "pause"
            };
            hold_running(&s, &machine, what).await?;
        }
        _ => {
            return Err(PluginError::NotSupported(
                "the command is start, cancel or pause (action pause, resume or toggle)",
            )
            .into())
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The name of the file that was uploaded last and still waits.
fn last_waiting(s: &Shared, machine: &str) -> Result<String, ApiError> {
    s.work
        .jobs
        .locked()
        .list()
        .iter()
        .filter(|j| j.machine == machine && j.state == WorkState::Queued)
        .max_by_key(|j| j.created)
        .map(|j| j.name.clone())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "unknown_job",
                "no file waits in the queue of the machine",
            )
        })
}
