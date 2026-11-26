//! The interface of the generic work of the machines: the plugins and their machines (`/api/v1/plugins`, `/api/v1/work/machines`), the queue of jobs with its history
//! (`/api/v1/work/jobs`), the stream of events (`/api/v1/events/stream`, server-sent events) and the notifications (`/api/v1/notifications`). The old routes of the machines
//! and of the jobs (`/api/v1/machines`, `/api/v1/jobs`) are not touched; the `bungard` plugin works on the same machines and the same jobs. The rules are in `src/work.rs`
//! and `src/plugins.rs`, and `docs/MACHINE_PLUGINS.md` explains them.

use std::collections::{HashSet, VecDeque};
use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;

use crate::api::{identify_open, ApiError, ApiJson, ApiResult, Authed, Shared, TokenQuery};
use crate::auth::{Identity, Role};
use crate::feed::{ClientGuard, FeedEvent, Filter};
use crate::jobs::now;
use crate::machines::valid_id;
use crate::plugins::PluginError;
use crate::state::AppState;
use crate::sync::Locked;
use crate::work::{Fail, WError, WorkMachine};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/v1/plugins", get(list_plugins))
        .route(
            "/api/v1/work/machines",
            get(list_machines).post(add_machine),
        )
        .route(
            "/api/v1/work/machines/:id",
            get(get_machine).put(change_machine).delete(remove_machine),
        )
        .route("/api/v1/work/machines/:id/position", get(op_position))
        .route("/api/v1/work/machines/:id/jog", post(op_jog))
        .route(
            "/api/v1/work/machines/:id/emergency-stop",
            post(op_emergency_stop),
        )
        .route("/api/v1/work/machines/:id/release", post(op_release))
        .route(
            "/api/v1/work/machines/:id/laser/test-fire",
            post(op_test_fire),
        )
        .route(
            "/api/v1/work/machines/:id/laser/release",
            post(op_laser_release),
        )
        .route("/api/v1/work/machines/:id/tools", get(op_tools))
        .route(
            "/api/v1/work/machines/:id/feeders",
            get(op_feeders).put(op_set_feeder),
        )
        .route(
            "/api/v1/work/machines/:id/temperatures",
            get(op_temperatures).put(op_set_temperature),
        )
        .route("/api/v1/work/machines/:id/layer", get(op_layer))
        .route(
            "/api/v1/work/machines/:id/compat/moonraker",
            get(op_compat_moonraker),
        )
        .route(
            "/api/v1/work/machines/:id/compat/octoprint",
            get(op_compat_octoprint),
        )
        .route(
            "/api/v1/work/machines/:id/compat/key",
            get(compat_key_state)
                .post(compat_key_issue)
                .delete(compat_key_revoke),
        )
        .route("/api/v1/work/jobs", get(list_jobs).post(create_job))
        .route("/api/v1/work/farm/jobs", post(create_farm_jobs))
        .route("/api/v1/work/jobs/:id", get(get_job).delete(remove_job))
        .route("/api/v1/work/jobs/:id/layers", get(job_layers))
        .route("/api/v1/work/jobs/:id/findings", get(job_findings))
        .route("/api/v1/work/jobs/:id/authorize", post(authorize_job))
        .route("/api/v1/work/jobs/:id/start", post(start_job))
        .route("/api/v1/work/jobs/:id/pause", post(pause_job))
        .route("/api/v1/work/jobs/:id/resume", post(resume_job))
        .route("/api/v1/work/jobs/:id/cancel", post(cancel_job))
        .route("/api/v1/events/stream", get(stream_events))
        .route("/api/v1/notifications", get(list_notifications))
        .route("/api/v1/notifications/read", post(read_notifications))
}

impl From<Fail> for ApiError {
    fn from(e: Fail) -> Self {
        let status = StatusCode::from_u16(e.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        ApiError::new(status, e.code(), e.detail())
    }
}

impl From<WError> for ApiError {
    fn from(e: WError) -> Self {
        ApiError::from(Fail::Work(e))
    }
}

impl From<PluginError> for ApiError {
    fn from(e: PluginError) -> Self {
        ApiError::from(Fail::Plugin(e))
    }
}

/// Runs the work of the plugins off the threads that answer requests (a plan can take a while).
async fn run<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Fail> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))?
        .map_err(Into::into)
}

fn bad_app() -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_app", "")
}

// ---- plugins and machines ---------------------------------------------------------------------------------------------------------------

async fn list_plugins(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let list: Vec<Value> = s
        .plugins
        .all()
        .iter()
        .map(|p| {
            json!({
                "kind": p.kind(), "title": p.title(), "capabilities": p.capabilities(),
                "manages_machines": p.manages_machines(),
            })
        })
        .collect();
    Ok(Json(json!({ "plugins": list })))
}

async fn list_machines(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let state = s.clone();
    let machines = run(move || Ok(state.work_machines_view())).await?;
    Ok(Json(json!({ "machines": machines })))
}

async fn get_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let state = s.clone();
    let found = run(move || Ok(state.work_machines_view())).await?;
    found
        .into_iter()
        .find(|m| m["id"] == id.as_str())
        .map(Json)
        .ok_or_else(|| WError::UnknownMachine.into())
}

#[derive(Deserialize)]
struct NewMachine {
    id: String,
    name: String,
    plugin: String,
    #[serde(default)]
    settings: Option<Value>,
}

/// The settings of a machine: nothing, or an object that the plugin accepts.
fn settings_of(s: &Shared, plugin: &str, settings: Option<Value>) -> Result<Value, Fail> {
    let plugin = s.plugins.get(plugin).ok_or(WError::UnknownPlugin)?;
    let settings = match settings {
        None | Some(Value::Null) => json!({}),
        Some(v) if v.is_object() => v,
        Some(_) => return Err(WError::BadSettings.into()),
    };
    plugin.check_settings(&settings)?;
    Ok(settings)
}

async fn add_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(b): ApiJson<NewMachine>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "machines:manage")?;
    if !valid_id(&b.id) {
        return Err(WError::BadId.into());
    }
    if !crate::apps::valid_label(&b.name, 80) {
        return Err(WError::BadName.into());
    }
    let plugin = s.plugins.get(&b.plugin).ok_or(WError::UnknownPlugin)?;
    if !plugin.manages_machines() {
        return Err(PluginError::NotSupported(
            "the machines of this plugin are made elsewhere (bungard: POST /api/v1/machines)",
        )
        .into());
    }
    let settings = settings_of(&s, &b.plugin, b.settings)?;
    let state = s.clone();
    let id = b.id.clone();
    let taken = run(move || Ok(state.work_find(&id).is_some())).await?;
    if taken {
        return Err(WError::MachineExists.into());
    }
    let machine = WorkMachine {
        id: b.id,
        name: b.name.trim().to_string(),
        plugin: b.plugin,
        settings,
        created: now(),
    };
    s.work.machines.locked().add(machine.clone())?;
    crate::api::note(
        &s,
        "work_machine_create",
        &who,
        &[("machine", &machine.id), ("plugin", &machine.plugin)],
    );
    s.announce_machine("machine.created", &machine.id, &machine.name);
    Ok((StatusCode::CREATED, Json(machine.view())))
}

#[derive(Deserialize)]
struct MachineChange {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    settings: Option<Value>,
}

async fn change_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    ApiJson(b): ApiJson<MachineChange>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "machines:manage")?;
    let current = s
        .work
        .machines
        .locked()
        .get(&id)
        .cloned()
        .ok_or(WError::UnknownMachine)?;
    let name = match b.name {
        Some(n) if crate::apps::valid_label(&n, 80) => Some(n.trim().to_string()),
        Some(_) => return Err(WError::BadName.into()),
        None => None,
    };
    let settings = match b.settings {
        Some(mut v) => {
            crate::secrets::keep_masked(&current.settings, &mut v);
            Some(settings_of(&s, &current.plugin, Some(v))?)
        }
        None => None,
    };
    let machine = s.work.machines.locked().edit(&id, name, settings)?;
    crate::api::note(&s, "work_machine_change", &who, &[("machine", &id)]);
    s.announce_machine("machine.changed", &machine.id, &machine.name);
    Ok(Json(machine.view()))
}

async fn remove_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "machines:manage")?;
    let name = s
        .work
        .machines
        .locked()
        .get(&id)
        .map(|m| m.name.clone())
        .ok_or(WError::UnknownMachine)?;
    let open = s
        .work
        .jobs
        .locked()
        .list()
        .iter()
        .any(|j| j.machine == id && !j.state.finished());
    if open {
        return Err(WError::MachineBusy.into());
    }
    s.work.machines.locked().remove(&id)?;
    // the key of the compatibility routes goes with the machine
    let _ = s.compat_keys.revoke(&id);
    crate::api::note(&s, "work_machine_remove", &who, &[("machine", &id)]);
    s.announce_machine("machine.removed", &id, &name);
    Ok(Json(json!({ "ok": true })))
}

// ---- what a plugin adds to its machines -------------------------------------------------------------------------------------------------

fn role_of(name: &str) -> Role {
    match name {
        "admin" => Role::Admin,
        "operator" => Role::Operator,
        _ => Role::Viewer,
    }
}

/// Does one of the routes a plugin adds under `/work/machines/{id}/`: the machine has to be of a plugin that has the route, and the caller has the role and the scope that
/// the plugin names for it. The body is JSON, or nothing (an emergency stop must not need a body).
async fn machine_op(
    s: &Shared,
    who: &Identity,
    id: &str,
    method: &'static str,
    route: &'static str,
    raw: &[u8],
) -> ApiResult {
    let state = s.clone();
    let wanted = id.to_string();
    let plugin = run(move || {
        state
            .work_find(&wanted)
            .ok_or(Fail::Work(WError::UnknownMachine))
    })
    .await?;
    let spec = plugin
        .routes()
        .iter()
        .find(|r| r.method == method && r.path == route)
        .ok_or(PluginError::NotSupported(
            "this kind of machine has no such operation",
        ))?;
    crate::api::need(who, role_of(spec.role))?;
    crate::api::need_scope(who, spec.scope)?;
    let body: Value = if raw.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        serde_json::from_slice(raw).map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "the body is not JSON",
            )
        })?
    };
    // a write with no body is an empty object (an emergency stop does not need a body); a read has none
    let body = if method != "GET" && body.is_null() {
        json!({})
    } else {
        body
    };
    let state = s.clone();
    let (machine, by) = (id.to_string(), who.name.clone());
    let answer = run(move || {
        plugin
            .operation(&state, &machine, route, &body, &by)
            .map_err(Fail::Plugin)
    })
    .await?;
    if method != "GET" {
        crate::api::note(s, "work_machine_op", who, &[("machine", id), ("op", route)]);
    }
    Ok(Json(answer))
}

macro_rules! machine_op {
    ($name:ident, $method:literal, $route:literal) => {
        async fn $name(
            State(s): State<Shared>,
            Authed(who, _): Authed,
            Path(id): Path<String>,
            body: axum::body::Bytes,
        ) -> ApiResult {
            machine_op(&s, &who, &id, $method, $route, &body).await
        }
    };
}

machine_op!(op_position, "GET", "position");
machine_op!(op_jog, "POST", "jog");
machine_op!(op_emergency_stop, "POST", "emergency-stop");
machine_op!(op_release, "POST", "release");
machine_op!(op_test_fire, "POST", "laser/test-fire");
machine_op!(op_laser_release, "POST", "laser/release");
machine_op!(op_tools, "GET", "tools");
machine_op!(op_feeders, "GET", "feeders");
machine_op!(op_set_feeder, "PUT", "feeders");
machine_op!(op_temperatures, "GET", "temperatures");
machine_op!(op_set_temperature, "PUT", "temperatures");
machine_op!(op_layer, "GET", "layer");
machine_op!(op_compat_moonraker, "GET", "compat/moonraker");
machine_op!(op_compat_octoprint, "GET", "compat/octoprint");

/// The machine of the plugin `printer3d` whose compatibility key a route reads or changes.
fn printer_for_key(s: &Shared, id: &str) -> Result<(), ApiError> {
    let plugin = s
        .work
        .machines
        .locked()
        .get(id)
        .map(|m| m.plugin.clone())
        .ok_or(WError::UnknownMachine)?;
    if plugin != "printer3d" {
        return Err(PluginError::NotSupported(
            "only a machine of the plugin printer3d has a key for the compatibility routes",
        )
        .into());
    }
    Ok(())
}

/// Whether the machine has a key for the routes of Moonraker and OctoPrint at the root of the address (the key itself is never shown again).
async fn compat_key_state(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "machines:manage")?;
    printer_for_key(&s, &id)?;
    Ok(Json(
        json!({ "machine": id, "set": s.compat_keys.has(&id), "write": s.compat_keys.can_write(&id) }),
    ))
}

/// Makes a key for the machine and shows it this once; a key it had stops working. The body is optional: `{"write": true}` makes a key that may also write through the
/// routes of Moonraker and OctoPrint (start, pause and cancel a print, upload a file), which still need the switch `compat_write`.
async fn compat_key_issue(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "machines:manage")?;
    printer_for_key(&s, &id)?;
    let write = if body.iter().all(u8::is_ascii_whitespace) {
        false
    } else {
        let asked: Value = serde_json::from_slice(&body).map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "the body is a JSON object, or empty",
            )
        })?;
        if !asked.is_object() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "the body is a JSON object, or empty",
            ));
        }
        match asked.get("write") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            Some(_) => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "write is true or false",
                ))
            }
        }
    };
    // a key of a machine never expires and, made to write, starts, stops and feeds prints: it is not made with more than the caller has
    let needed: &[&str] = if write {
        &["machines:view", "jobs:prepare", "jobs:start", "jobs:cancel"]
    } else {
        &["machines:view"]
    };
    if who.expires.is_some() || !needed.iter().all(|scope| who.allows(scope)) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden_scope",
            format!(
                "a key of a machine needs a caller that does not expire and carries {}",
                needed.join(", ")
            ),
        ));
    }
    let key = s
        .compat_keys
        .issue(&id, write)
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "storage", ""))?;
    crate::api::note(
        &s,
        "compat_key_issue",
        &who,
        &[
            ("machine", &id),
            ("write", if write { "true" } else { "false" }),
        ],
    );
    Ok(Json(
        json!({ "machine": id, "key": key, "header": "X-Api-Key", "write": write }),
    ))
}

/// Takes the key of the machine away.
async fn compat_key_revoke(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "machines:manage")?;
    printer_for_key(&s, &id)?;
    let revoked = s
        .compat_keys
        .revoke(&id)
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "storage", ""))?;
    crate::api::note(&s, "compat_key_revoke", &who, &[("machine", &id)]);
    Ok(Json(json!({ "machine": id, "revoked": revoked })))
}

// ---- the queue --------------------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct NewJob {
    machine: String,
    request: Value,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    app: Option<String>,
}

fn job_view(s: &AppState, job: &crate::work::WorkJob, full: bool) -> Value {
    let position = s.work.jobs.locked().position(job);
    let mut v = if full { job.full() } else { job.summary() };
    v["position"] = json!(position);
    v
}

async fn create_job(
    State(s): State<Shared>,
    Authed(who, token): Authed,
    ApiJson(b): ApiJson<NewJob>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:prepare")?;
    // the application that asks: the one it says, else the one of its login
    let app = match b.app {
        Some(a) if crate::apps::allowed(&s.cfg, &a) => a,
        Some(_) => return Err(bad_app()),
        None => s.auth.locked().app_of(&token).unwrap_or_default(),
    };
    let state = s.clone();
    let (machine, name, request, by) = (b.machine, b.name, b.request, who.clone());
    let created = {
        let app = app.clone();
        run(move || state.work_create(&by, &machine, name.as_deref(), &app, &request)).await?
    };
    crate::api::note(
        &s,
        "work_job_create",
        &who,
        &[("job", &created.id), ("machine", &created.machine)],
    );
    Ok((StatusCode::CREATED, Json(job_view(&s, &created, true))))
}

#[derive(Deserialize)]
struct FarmJob {
    request: Value,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    app: Option<String>,
    /// The 3D printers to choose among, in the order to try them; none: all of them, in the order of the list.
    #[serde(default)]
    machines: Option<Vec<String>>,
    /// How many copies, each on a printer of its own (1 to 16, 1 by default).
    #[serde(default)]
    copies: Option<u32>,
}

/// The 3D printers that are free, in the order to try them: not running or held, not latched and with no job waiting for a person. The list narrows the choice and sets the order.
fn free_printers(s: &Shared, only: Option<&[String]>) -> Result<Vec<String>, Fail> {
    let plugin = s
        .plugins
        .get("printer3d")
        .ok_or(PluginError::NotSupported("this server has no 3D printers"))?;
    let waiting: HashSet<String> = s
        .work
        .jobs
        .locked()
        .list()
        .iter()
        .filter(|j| j.plugin == "printer3d" && j.state == crate::plugins::WorkState::Queued)
        .map(|j| j.machine.clone())
        .collect();
    let listed = plugin.machines(s);
    let order: Vec<String> = match only {
        Some(ids) => {
            for id in ids {
                if !listed.iter().any(|m| &m.id == id) {
                    return Err(PluginError::UnknownMachine.into());
                }
            }
            ids.to_vec()
        }
        None => listed.iter().map(|m| m.id.clone()).collect(),
    };
    Ok(order
        .into_iter()
        .filter(|id| {
            listed.iter().any(|m| &m.id == id && m.state == "idle") && !waiting.contains(id)
        })
        .collect())
}

/// Shares a print among the 3D printers that are free: each copy goes to the first free printer that accepts it (the limits of its model hold) and WAITS there in the queue
/// for a person to start it; nothing starts by itself. The answer says where each copy went and why a printer refused.
async fn create_farm_jobs(
    State(s): State<Shared>,
    Authed(who, token): Authed,
    ApiJson(b): ApiJson<FarmJob>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:prepare")?;
    let app = match b.app {
        Some(a) if crate::apps::allowed(&s.cfg, &a) => a,
        Some(_) => return Err(bad_app()),
        None => s.auth.locked().app_of(&token).unwrap_or_default(),
    };
    let copies = b.copies.unwrap_or(1);
    if !(1..=16).contains(&copies) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_plugin_request",
            "copies is from 1 to 16",
        ));
    }
    let (state, by) = (s.clone(), who.clone());
    let (request, name, only) = (b.request, b.name, b.machines);
    let (placed, tried, free_count) = run(move || {
        let mut free = free_printers(&state, only.as_deref())?.into_iter();
        let free_count = free.len();
        let (mut placed, mut tried): (Vec<Value>, Vec<Value>) = (Vec::new(), Vec::new());
        'copies: for _ in 0..copies {
            loop {
                let Some(machine) = free.next() else {
                    break 'copies;
                };
                match state.work_create(&by, &machine, name.as_deref(), &app, &request) {
                    Ok(job) => {
                        placed.push(job_view(&state, &job, true));
                        break;
                    }
                    Err(e) => tried
                        .push(json!({"machine": machine, "error": e.code(), "detail": e.detail()})),
                }
            }
        }
        Ok((placed, tried, free_count))
    })
    .await?;
    if placed.is_empty() {
        let why = if free_count == 0 {
            "no 3D printer is free: each one is busy, latched or has a job waiting for a person"
                .to_string()
        } else {
            format!("no free printer took the job: {}", Value::Array(tried))
        };
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "farm_no_free_printer",
            why,
        ));
    }
    for job in &placed {
        let (id, machine) = (
            job["id"].as_str().unwrap_or(""),
            job["machine"].as_str().unwrap_or(""),
        );
        crate::api::note(
            &s,
            "work_farm_job",
            &who,
            &[("job", id), ("machine", machine)],
        );
    }
    let unplaced = copies as usize - placed.len();
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "jobs": placed,
            "unplaced": unplaced,
            "tried": tried,
            "confirm": "each job waits in the queue of its printer: a person starts it with POST /api/v1/work/jobs/{id}/start",
        })),
    ))
}

#[derive(Deserialize)]
struct JobFilter {
    machine: Option<String>,
    state: Option<String>,
    app: Option<String>,
    mine: Option<bool>,
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn list_jobs(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Query(f): Query<JobFilter>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let state = s.clone();
    run(move || {
        state.work_refresh_all();
        Ok(())
    })
    .await?;
    let limit = f.limit.unwrap_or(50).clamp(1, 500);
    let offset = f.offset.unwrap_or(0);
    let me = who.owner();
    let book = s.work.jobs.locked();
    let found: Vec<&crate::work::WorkJob> = book
        .list()
        .iter()
        .rev()
        .filter(|j| f.machine.as_deref().is_none_or(|m| j.machine == m))
        .filter(|j| f.state.as_deref().is_none_or(|st| j.state.name() == st))
        .filter(|j| f.app.as_deref().is_none_or(|a| j.app == a))
        .filter(|j| !f.mine.unwrap_or(false) || j.owner == me)
        .collect();
    let total = found.len();
    let items: Vec<Value> = found
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|j| {
            let mut v = j.summary();
            v["position"] = json!(book.position(j));
            v
        })
        .collect();
    Ok(Json(
        json!({ "total": total, "offset": offset, "limit": limit, "items": items }),
    ))
}

async fn get_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let state = s.clone();
    let wanted = id.clone();
    let job = run(move || {
        state
            .work_refresh_one(&wanted)
            .ok_or(Fail::Work(WError::UnknownJob))
    })
    .await?;
    Ok(Json(job_view(&s, &job, true)))
}

/// The job of a gerber machine whose result the route reads: the job must exist and belong to a machine of the plugin `gerber`.
fn gerber_job(state: &std::sync::Arc<AppState>, id: &str) -> Result<crate::work::WorkJob, Fail> {
    let job = state
        .work_refresh_one(id)
        .ok_or(Fail::Work(WError::UnknownJob))?;
    if job.plugin != "gerber" {
        return Err(Fail::Plugin(PluginError::NotSupported(
            "only the jobs of a gerber machine have layers and findings",
        )));
    }
    Ok(job)
}

/// The layers of a job of a gerber machine (`load`, `drc` and `report` list them): the function, the units, the box and the counts of each file read.
async fn job_layers(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let state = s.clone();
    let job = run(move || gerber_job(&state, &id)).await?;
    Ok(Json(json!({
        "job": job.id,
        "kind": job.view["kind"],
        "layers": job.view["layers"],
        "board": job.view["board"],
    })))
}

#[derive(Deserialize)]
struct FindingsFilter {
    severity: Option<String>,
    rule: Option<String>,
    layer: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

/// The findings of a job of a gerber machine that checked the board (`drc` and `report`), a page at a time and filtered by `severity`, `rule` and `layer`. A `load` job has
/// no checks: `409 not_in_state`.
async fn job_findings(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    Query(f): Query<FindingsFilter>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let state = s.clone();
    let job = run(move || gerber_job(&state, &id)).await?;
    if job.view.get("findings").is_none() {
        return Err(Fail::Plugin(PluginError::NotInState).into());
    }
    let mut page = crate::gerber::findings_page(
        &job.view["findings"],
        f.severity.as_deref(),
        f.rule.as_deref(),
        f.layer.as_deref(),
        f.offset.unwrap_or(0),
        f.limit.unwrap_or(200),
    );
    page["job"] = json!(job.id);
    page["verdict"] = job.view["verdict"].clone();
    page["findings_total"] = job.view["findings_total"].clone();
    page["truncated"] = job.view["truncated"].clone();
    Ok(Json(page))
}

/// The body of a start or an authorization: the hash of what the person was shown. A start may have no body at all.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HashBody {
    toolpath_sha256: Option<String>,
}

/// The hash named by the body of a request, if it has a body; a body that does not read is refused, never ignored.
fn named_hash(body: &axum::body::Bytes) -> Result<Option<String>, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let parsed: HashBody = serde_json::from_slice(body).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("the body is {{\"toolpath_sha256\": \"...\"}}: {e}"),
        )
    })?;
    match parsed.toolpath_sha256 {
        Some(h) if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) => {
            Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "toolpath_sha256 is 64 hexadecimal digits",
            ))
        }
        other => Ok(other.map(|h| h.to_ascii_lowercase())),
    }
}

async fn authorize_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:authorize")?;
    let hash = named_hash(&body)?.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the body is {\"toolpath_sha256\": \"...\"}: the hash of the job as it was shown",
        )
    })?;
    let state = s.clone();
    let (wanted, by, h) = (id.clone(), who.clone(), hash.clone());
    let job = run(move || state.work_authorize(&wanted, &by, &h)).await?;
    crate::api::note(
        &s,
        "work_job_authorize",
        &who,
        &[("job", &id), ("toolpath_sha256", &hash)],
    );
    Ok(Json(job_view(&s, &job, true)))
}

async fn start_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:start")?;
    let hash = named_hash(&body)?;
    let state = s.clone();
    let (wanted, by) = (id.clone(), who.clone());
    let job = run(move || state.work_start_checked(&wanted, &by, hash.as_deref())).await?;
    crate::api::note(&s, "work_job_start", &who, &[("job", &id)]);
    Ok(Json(job_view(&s, &job, true)))
}

async fn pause_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:start")?;
    let state = s.clone();
    let (wanted, by) = (id.clone(), who.clone());
    let job = run(move || state.work_pause(&wanted, &by)).await?;
    crate::api::note(&s, "work_job_pause", &who, &[("job", &id)]);
    Ok(Json(job_view(&s, &job, true)))
}

async fn resume_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:start")?;
    let state = s.clone();
    let (wanted, by) = (id.clone(), who.clone());
    let job = run(move || state.work_resume(&wanted, &by)).await?;
    crate::api::note(&s, "work_job_resume", &who, &[("job", &id)]);
    Ok(Json(job_view(&s, &job, true)))
}

async fn cancel_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:cancel")?;
    let state = s.clone();
    let (wanted, by) = (id.clone(), who.clone());
    let job = run(move || state.work_cancel(&wanted, &by)).await?;
    crate::api::note(&s, "work_job_cancel", &who, &[("job", &id)]);
    Ok(Json(job_view(&s, &job, true)))
}

async fn remove_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:cancel")?;
    s.work_remove(&id)?;
    crate::api::note(&s, "work_job_remove", &who, &[("job", &id)]);
    Ok(Json(json!({ "ok": true })))
}

// ---- the stream of events ---------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct StreamQuery {
    token: Option<String>,
    ticket: Option<String>,
    machine: Option<String>,
    project: Option<String>,
    job: Option<String>,
    app: Option<String>,
    /// Kinds separated by commas; `job` also matches `job.state`.
    kinds: Option<String>,
    last_event_id: Option<u64>,
}

/// How long the stream waits in silence before it asks whether the login is still good.
const RECHECK: Duration = Duration::from_secs(30);

/// How long is left before the login has to be asked about again: the time of the stream is counted from the last check, not from the last event, so that a server that is
/// never quiet is asked as often as a quiet one.
fn recheck_wait(since_check: Duration) -> Duration {
    RECHECK.saturating_sub(since_check)
}

struct Live {
    /// When the login was last asked about.
    checked: tokio::time::Instant,
    rx: broadcast::Receiver<FeedEvent>,
    backlog: VecDeque<FeedEvent>,
    /// The number of the last event sent: the ones at or below it are not sent twice.
    high: u64,
    hello: Option<String>,
    who: Identity,
    token: String,
    filter: Filter,
    state: Shared,
    _place: ClientGuard,
}

impl Live {
    /// Asks again whether the login is good and takes the identity as it is now (a role that was lowered, a token that was narrowed). False when it is not good any more.
    fn still_logged_in(&mut self) -> bool {
        self.checked = tokio::time::Instant::now();
        let current = self.state.auth.locked().identify(&self.token, now());
        match current {
            Some(who) => {
                self.who = who;
                true
            }
            None => false,
        }
    }

    fn allowed(&self, e: &FeedEvent) -> bool {
        self.filter.passes(e)
            && e.visible_to(&self.who)
            && e.scope().is_none_or(|scope| self.who.allows(scope))
    }
}

fn sse_event(e: &FeedEvent) -> Event {
    Event::default()
        .id(e.id.to_string())
        .event(e.kind.clone())
        .data(e.text())
}

async fn stream_events(
    State(s): State<Shared>,
    parts: Parts,
    Query(q): Query<StreamQuery>,
) -> Result<Response, ApiError> {
    let (who, token) = identify_open(
        &s,
        &parts,
        &TokenQuery {
            token: q.token.clone(),
            ticket: q.ticket.clone(),
        },
    )?;
    let place = s.feed.join().ok_or_else(|| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_clients",
            "as many clients listen as events_clients_max allows",
        )
    })?;
    let last = parts
        .headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .or(q.last_event_id);
    let filter = Filter {
        machine: q.machine,
        project: q.project,
        job: q.job,
        app: q.app,
        kinds: q
            .kinds
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_string)
            .collect(),
    };
    // listen first, then ask for what was missed: nothing falls between the two, and the number of the last event sent keeps a repeat out
    let rx = s.feed.subscribe();
    let newest = s.feed.last_id();
    let mut live = Live {
        checked: tokio::time::Instant::now(),
        rx,
        backlog: VecDeque::new(),
        high: last.unwrap_or(newest),
        hello: None,
        who,
        token,
        filter,
        state: s.clone(),
        _place: place,
    };
    let mut gap = false;
    if let Some(last) = last {
        let kept = s.feed.since(last);
        gap = last < newest && kept.first().is_none_or(|e| e.id > last + 1);
        for e in kept {
            if live.allowed(&e) {
                live.backlog.push_back(e);
            }
        }
    }
    live.hello = Some(
        json!({
            "last_id": newest, "resumed": last.is_some(), "resync": gap,
            "clients": s.feed.clients(),
        })
        .to_string(),
    );
    let stream = futures_util::stream::unfold(live, |mut live| async move {
        loop {
            if let Some(text) = live.hello.take() {
                let event = Event::default().event("hello").data(text);
                return Some((Ok::<Event, Infallible>(event), live));
            }
            if recheck_wait(live.checked.elapsed()).is_zero() && !live.still_logged_in() {
                return None;
            }
            if let Some(e) = live.backlog.pop_front() {
                live.high = live.high.max(e.id);
                return Some((Ok(sse_event(&e)), live));
            }
            match tokio::time::timeout(recheck_wait(live.checked.elapsed()), live.rx.recv()).await {
                // the time to ask whether the login is still good has come (the next turn of the loop does it)
                Err(_) => {}
                Ok(Ok(e)) => {
                    if e.id > live.high && live.allowed(&e) {
                        live.high = e.id;
                        return Some((Ok(sse_event(&e)), live));
                    }
                }
                Ok(Err(RecvError::Lagged(missed))) => {
                    let event = Event::default()
                        .event("resync")
                        .data(json!({ "missed": missed }).to_string());
                    return Some((Ok(event), live));
                }
                Ok(Err(RecvError::Closed)) => return None,
            }
        }
    });
    let keep_alive = KeepAlive::new()
        .interval(Duration::from_secs(s.cfg.events_ping_s))
        .text("ping");
    Ok(Sse::new(stream).keep_alive(keep_alive).into_response())
}

// ---- the notifications ------------------------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct NoteQuery {
    unread: Option<bool>,
    limit: Option<usize>,
}

async fn list_notifications(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Query(q): Query<NoteQuery>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let list = s
        .work
        .notes
        .locked()
        .for_user(&who, q.unread.unwrap_or(false), limit);
    Ok(Json(json!({ "notifications": list })))
}

#[derive(Deserialize)]
struct ReadBody {
    #[serde(default)]
    ids: Vec<u64>,
    #[serde(default)]
    all: bool,
}

async fn read_notifications(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(b): ApiJson<ReadBody>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    let marked = s.work.notes.locked().mark_read(&who, &b.ids, b.all);
    Ok(Json(json!({ "marked": marked })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_login_is_asked_about_by_the_clock_and_not_by_the_silence() {
        assert_eq!(recheck_wait(Duration::ZERO), RECHECK);
        assert_eq!(
            recheck_wait(Duration::from_secs(10)),
            Duration::from_secs(20)
        );
        assert!(recheck_wait(RECHECK).is_zero());
        assert!(recheck_wait(RECHECK + Duration::from_secs(5)).is_zero());
    }
}
