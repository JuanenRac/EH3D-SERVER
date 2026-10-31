//! The server's HTTP and websocket interface, under `/api/v1/`. Every answer is JSON; a mistake is `{"error": "<code>", "detail": "..."}` with
//! the status that fits. Everything but the health check, the set-up and the login needs `Authorization: Bearer <token>`: the token of a
//! login or the token of a program. A viewer may read, an operator may also prepare, start, stop and confirm jobs, an administrator may also
//! manage users, tokens and machines.

use std::sync::Arc;

use axum::async_trait;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, Query, State};
use axum::http::request::Parts;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::{AuthError, Identity, Role};
use crate::head::{self, HeadError};
use crate::inspect::{self, InspectError};
use crate::jobs::{new_id, now, JobRecord, JobState};
use crate::machines::{MachineError, MachineRecord};
use crate::plan::{self, JobRequest};
use crate::state::{AppState, StartError};

pub type Shared = Arc<AppState>;

pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    detail: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, detail: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            detail: detail.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({ "error": self.code, "detail": self.detail })),
        )
            .into_response()
    }
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        let status = match e {
            AuthError::BadSetup => StatusCode::FORBIDDEN,
            AuthError::BadName | AuthError::WeakPassword => StatusCode::UNPROCESSABLE_ENTITY,
            AuthError::BadCredentials => StatusCode::UNAUTHORIZED,
            AuthError::Throttled => StatusCode::TOO_MANY_REQUESTS,
            AuthError::UnknownUser | AuthError::UnknownToken => StatusCode::NOT_FOUND,
            AuthError::UserExists | AuthError::LastAdmin => StatusCode::CONFLICT,
            AuthError::Storage => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError::new(status, e.code(), "")
    }
}

impl From<MachineError> for ApiError {
    fn from(e: MachineError) -> Self {
        let status = match e {
            MachineError::Unknown => StatusCode::NOT_FOUND,
            MachineError::Exists => StatusCode::CONFLICT,
            MachineError::Storage => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::UNPROCESSABLE_ENTITY,
        };
        ApiError::new(status, e.code(), "")
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

fn forbidden() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "forbidden",
        "this needs a higher role",
    )
}

fn need(who: &Identity, role: Role) -> Result<(), ApiError> {
    if who.role >= role {
        Ok(())
    } else {
        Err(forbidden())
    }
}

/// The bearer token of a request, if it brought one.
fn bearer(parts: &Parts) -> Option<String> {
    let value = parts.headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(|t| t.trim().to_string())
}

/// Who is asking: the extractor of every endpoint that needs a login.
pub struct Authed(pub Identity, pub String);

#[async_trait]
impl FromRequestParts<Shared> for Authed {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &Shared) -> Result<Self, ApiError> {
        let token = bearer(parts).ok_or_else(|| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "send Authorization: Bearer <token>",
            )
        })?;
        let who = state
            .auth
            .lock()
            .expect("auth lock")
            .identify(&token, now());
        match who {
            Some(who) => Ok(Authed(who, token)),
            None => Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "the token is not valid or has expired",
            )),
        }
    }
}

pub fn router(state: Shared) -> Router {
    let limit = state.cfg.max_body_mb * 1024 * 1024;
    let studio = state.cfg.studio_dir.clone();
    let app = Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/setup", get(setup_status).post(setup))
        .route("/api/v1/login", post(login))
        .route("/api/v1/logout", post(logout))
        .route("/api/v1/session", get(session))
        .route("/api/v1/account", put(change_password))
        .route("/api/v1/users", get(list_users).post(add_user))
        .route("/api/v1/users/:name", put(update_user).delete(delete_user))
        .route("/api/v1/tokens", get(list_tokens).post(add_token))
        .route("/api/v1/tokens/:id", delete(delete_token))
        .route("/api/v1/machines", get(list_machines).post(add_machine))
        .route(
            "/api/v1/machines/:id",
            get(get_machine).put(put_machine).delete(delete_machine),
        )
        .route("/api/v1/machines/:id/preview", post(preview))
        .route("/api/v1/machines/:id/jobs", post(create_job))
        .route("/api/v1/machines/:id/head/status", get(head_status))
        .route("/api/v1/machines/:id/head/measure", post(head_measure))
        .route("/api/v1/machines/:id/head/stream", get(head_stream))
        .route(
            "/api/v1/machines/:id/head/light",
            get(head_light).post(head_light_set),
        )
        .route("/api/v1/machines/:id/head/snapshot", get(head_snapshot))
        .route(
            "/api/v1/machines/:id/head/inspect",
            get(inspect_last).post(inspect_run),
        )
        .route(
            "/api/v1/machines/:id/head/inspect/profile",
            get(inspect_profile).put(inspect_profile_put),
        )
        .route(
            "/api/v1/machines/:id/head/inspect/calibrate",
            post(inspect_calibrate),
        )
        .route("/api/v1/fleet", get(fleet))
        .route("/api/v1/jobs", get(list_jobs))
        .route("/api/v1/jobs/:id", get(get_job).delete(delete_job))
        .route("/api/v1/jobs/:id/start", post(start_job))
        .route("/api/v1/jobs/:id/confirm", post(confirm_job))
        .route("/api/v1/jobs/:id/cancel", post(cancel_job))
        .route("/api/v1/updates", get(updates_view))
        .route("/api/v1/updates/check", post(updates_check))
        .route("/api/v1/updates/apply", post(updates_apply))
        .route("/api/v1/events", get(events))
        .layer(DefaultBodyLimit::max(limit))
        .with_state(state);
    match studio {
        // the pages of Studio: a file that is not there is the start page, so a reload on any page of the application works
        Some(dir) => {
            let index = dir.join("index.html");
            app.fallback_service(
                tower_http::services::ServeDir::new(dir)
                    .fallback(tower_http::services::ServeFile::new(index)),
            )
        }
        None => app,
    }
}

// ---- who is who -----------------------------------------------------------------------------------------------------------------------

async fn health() -> Json<Value> {
    Json(json!({ "name": "eh3d-server", "version": env!("CARGO_PKG_VERSION") }))
}

async fn setup_status(State(s): State<Shared>) -> Json<Value> {
    Json(json!({ "needed": s.auth.lock().expect("auth lock").needs_setup() }))
}

#[derive(Deserialize)]
struct SetupBody {
    code: String,
    name: String,
    password: String,
}

async fn setup(State(s): State<Shared>, Json(b): Json<SetupBody>) -> ApiResult {
    // hashing a password takes a moment of work: not on the thread that serves the others
    tokio::task::spawn_blocking(move || {
        s.auth
            .lock()
            .expect("auth lock")
            .setup(&b.code, &b.name, &b.password)
    })
    .await
    .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))??;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct LoginBody {
    name: String,
    password: String,
}

async fn login(State(s): State<Shared>, Json(b): Json<LoginBody>) -> ApiResult {
    let name = b.name.clone();
    let (token, role, expires) = tokio::task::spawn_blocking(move || {
        s.auth
            .lock()
            .expect("auth lock")
            .login(&b.name, &b.password, now())
    })
    .await
    .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))??;
    Ok(Json(
        json!({ "token": token, "name": name, "role": role, "expires_s": expires }),
    ))
}

async fn logout(State(s): State<Shared>, Authed(_, token): Authed) -> ApiResult {
    s.auth.lock().expect("auth lock").logout(&token);
    Ok(Json(json!({ "ok": true })))
}

async fn session(Authed(who, _): Authed) -> ApiResult {
    Ok(Json(
        json!({ "name": who.name, "role": who.role, "is_token": who.is_token }),
    ))
}

#[derive(Deserialize)]
struct PasswordBody {
    old: String,
    new: String,
}

async fn change_password(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Json(b): Json<PasswordBody>,
) -> ApiResult {
    if who.is_token {
        return Err(forbidden());
    }
    tokio::task::spawn_blocking(move || {
        s.auth
            .lock()
            .expect("auth lock")
            .change_password(&who.name, &b.old, &b.new)
    })
    .await
    .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))??;
    Ok(Json(json!({ "ok": true })))
}

/// The updates of the installed services: what is installed, what the last check found and what the helper is doing.
async fn updates_view(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    need(&who, Role::Admin)?;
    Ok(Json(match &s.updates {
        Some(u) => u.view(),
        None => {
            json!({ "enabled": false, "components": [], "state": "idle", "message": "", "log": [], "pending": false })
        }
    }))
}

fn updates_of(s: &Shared) -> Result<&crate::updates::Updates, ApiError> {
    s.updates.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_IMPLEMENTED,
            "updates_off",
            "the updates are not set up on this server (updates_dir in server.cfg)",
        )
    })
}

fn updates_fail(e: String) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, "update_refused", e)
}

async fn updates_check(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    need(&who, Role::Admin)?;
    updates_of(&s)?
        .request("check", &[])
        .map_err(updates_fail)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ApplyBody {
    components: Vec<String>,
}

async fn updates_apply(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Json(b): Json<ApplyBody>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    updates_of(&s)?
        .request("apply", &b.components)
        .map_err(updates_fail)?;
    Ok(Json(json!({ "ok": true })))
}

async fn list_users(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    need(&who, Role::Admin)?;
    let users: Vec<Value> = s
        .auth
        .lock()
        .expect("auth lock")
        .users()
        .into_iter()
        .map(|(name, role, disabled)| json!({ "name": name, "role": role, "disabled": disabled }))
        .collect();
    Ok(Json(Value::Array(users)))
}

#[derive(Deserialize)]
struct NewUser {
    name: String,
    role: Role,
    password: String,
}

async fn add_user(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Json(b): Json<NewUser>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    tokio::task::spawn_blocking(move || {
        s.auth
            .lock()
            .expect("auth lock")
            .add_user(&b.name, b.role, &b.password)
    })
    .await
    .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))??;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct UserChange {
    role: Option<Role>,
    password: Option<String>,
    disabled: Option<bool>,
}

async fn update_user(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(name): Path<String>,
    Json(b): Json<UserChange>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    tokio::task::spawn_blocking(move || {
        s.auth.lock().expect("auth lock").update_user(
            &name,
            b.role,
            b.password.as_deref(),
            b.disabled,
        )
    })
    .await
    .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))??;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_user(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(name): Path<String>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    s.auth.lock().expect("auth lock").delete_user(&name)?;
    Ok(Json(json!({ "ok": true })))
}

async fn list_tokens(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    need(&who, Role::Admin)?;
    let tokens: Vec<Value> = s
        .auth
        .lock()
        .expect("auth lock")
        .tokens()
        .iter()
        .map(|t| json!({ "id": t.id, "name": t.name, "role": t.role, "created": t.created }))
        .collect();
    Ok(Json(Value::Array(tokens)))
}

#[derive(Deserialize)]
struct NewToken {
    name: String,
    role: Role,
}

async fn add_token(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Json(b): Json<NewToken>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    let (id, secret) = s
        .auth
        .lock()
        .expect("auth lock")
        .create_token(&b.name, b.role, now())?;
    // the secret is shown this once
    Ok(Json(json!({ "id": id, "token": secret })))
}

async fn delete_token(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    s.auth.lock().expect("auth lock").delete_token(&id)?;
    Ok(Json(json!({ "ok": true })))
}

// ---- the machines ---------------------------------------------------------------------------------------------------------------------

async fn list_machines(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    need(&who, Role::Viewer)?;
    let list: Vec<Value> = s
        .machines
        .lock()
        .expect("machines lock")
        .list()
        .iter()
        .map(MachineRecord::public)
        .collect();
    Ok(Json(Value::Array(list)))
}

/// Every machine with what it is doing, grouped by station: the one view a central screen needs to follow all of them at once.
async fn fleet(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    need(&who, Role::Viewer)?;
    let machines = s.machines.lock().expect("machines lock");
    let jobs = s.jobs.lock().expect("jobs lock");
    let mut list = Vec::new();
    let mut stations: Vec<(String, Vec<String>)> = Vec::new();
    let (mut idle, mut busy, mut waiting) = (0usize, 0usize, 0usize);
    for m in machines.list() {
        let public = m.public();
        let mine = jobs.list().iter().filter(|j| j.machine == m.id);
        let active = mine.clone().find(|j| j.state.active());
        let ready = mine
            .clone()
            .filter(|j| j.state == crate::jobs::JobState::Ready)
            .count();
        let state = match active.map(|j| j.state) {
            Some(crate::jobs::JobState::WaitingOperator) => {
                waiting += 1;
                "waiting"
            }
            Some(_) => {
                busy += 1;
                "busy"
            }
            None => {
                idle += 1;
                "idle"
            }
        };
        let job = active.map(|j| {
            let percent = (j.items_done * 100)
                .checked_div(j.items_total)
                .map_or(0, |p| p.min(100));
            json!({
                "id": j.id, "name": j.name, "kind": j.kind, "state": j.state, "by": j.by,
                "items_done": j.items_done, "items_total": j.items_total,
                "holes_done": j.holes_done, "holes_total": j.holes_total,
                "percent": percent, "pause": j.pause, "estimate_s": j.view.estimate_s,
            })
        });
        match stations.iter_mut().find(|(id, _)| *id == m.station) {
            Some((_, ids)) => ids.push(m.id.clone()),
            None => stations.push((m.station.clone(), vec![m.id.clone()])),
        }
        list.push(json!({
            "id": m.id, "name": m.name, "model": m.model, "station": m.station,
            "connection": public["connection"]["kind"], "state": state, "job": job, "ready": ready,
        }));
    }
    // the machines of a named station first, those without one at the end
    stations.sort_by(|a, b| a.0.is_empty().cmp(&b.0.is_empty()).then(a.0.cmp(&b.0)));
    let station_count = stations.iter().filter(|(id, _)| !id.is_empty()).count();
    Ok(Json(json!({
        "stations": stations.into_iter().map(|(id, ids)| json!({"id": id, "machines": ids})).collect::<Vec<_>>(),
        "machines": list,
        "summary": {"machines": idle + busy + waiting, "idle": idle, "busy": busy, "waiting": waiting, "stations": station_count},
    })))
}

async fn add_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Json(record): Json<MachineRecord>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    let shown = record.public();
    s.machines.lock().expect("machines lock").add(record)?;
    s.publish(json!({ "type": "machine", "id": shown["id"] }));
    Ok(Json(shown))
}

async fn get_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Viewer)?;
    let machines = s.machines.lock().expect("machines lock");
    machines
        .get(&id)
        .map(|m| Json(m.public()))
        .ok_or_else(|| MachineError::Unknown.into())
}

async fn put_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    Json(mut record): Json<MachineRecord>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    record.id = id;
    let shown = record.public();
    s.machines.lock().expect("machines lock").replace(record)?;
    s.publish(json!({ "type": "machine", "id": shown["id"] }));
    Ok(Json(shown))
}

async fn delete_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    if s.jobs.lock().expect("jobs lock").active_on(&id).is_some() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "machine_busy",
            "a job is running on this machine",
        ));
    }
    s.machines.lock().expect("machines lock").remove(&id)?;
    s.publish(json!({ "type": "machine", "id": id }));
    Ok(Json(json!({ "ok": true })))
}

// ---- the jobs -------------------------------------------------------------------------------------------------------------------------

fn plan_error(e: plan::PlanError) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, e.code, e.detail)
}

fn machine_of(s: &Shared, id: &str) -> Result<MachineRecord, ApiError> {
    s.machines
        .lock()
        .expect("machines lock")
        .get(id)
        .cloned()
        .ok_or_else(|| MachineError::Unknown.into())
}

fn request_of(body: &Value) -> Result<JobRequest, ApiError> {
    serde_json::from_value(body.clone()).map_err(|e| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_request",
            e.to_string(),
        )
    })
}

/// What a client would see for a job, without making it: the work in the machine's order, the time it takes and where each hole ends.
async fn preview(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    let machine = machine_of(&s, &id)?;
    let request = request_of(&body)?;
    let planned = tokio::task::spawn_blocking(move || plan::plan(&machine, &request))
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))?
        .map_err(plan_error)?;
    Ok(Json(
        serde_json::to_value(&planned.view).unwrap_or(Value::Null),
    ))
}

async fn create_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    let machine = machine_of(&s, &id)?;
    let request = request_of(&body)?;
    let name = request.name().map(str::to_string);
    let planned = tokio::task::spawn_blocking(move || plan::plan(&machine, &request))
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))?
        .map_err(plan_error)?;
    let view = planned.view;
    let job = JobRecord {
        id: new_id(),
        machine: id,
        name: name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| "job".into()),
        kind: view.kind.clone(),
        state: JobState::Ready,
        created: now(),
        started: None,
        finished: None,
        by: who.name,
        items_total: view.items,
        items_done: 0,
        holes_total: if view.kind == "drill" {
            view.holes.len()
        } else {
            view.paths.len()
        },
        holes_done: 0,
        message: String::new(),
        pause: None,
        request: body,
        view,
    };
    let shown = job.full();
    s.publish_job(&job);
    s.jobs.lock().expect("jobs lock").add(job);
    Ok(Json(shown))
}

#[derive(Deserialize)]
struct JobFilter {
    machine: Option<String>,
    state: Option<String>,
}

async fn list_jobs(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Query(f): Query<JobFilter>,
) -> ApiResult {
    need(&who, Role::Viewer)?;
    let jobs = s.jobs.lock().expect("jobs lock");
    let list: Vec<Value> = jobs
        .list()
        .iter()
        .rev()
        .filter(|j| f.machine.as_deref().is_none_or(|m| j.machine == m))
        .filter(|j| {
            f.state.as_deref().is_none_or(|st| {
                serde_json::to_value(j.state)
                    .ok()
                    .and_then(|v| v.as_str().map(|x| x == st))
                    .unwrap_or(false)
            })
        })
        .map(JobRecord::summary)
        .collect();
    Ok(Json(Value::Array(list)))
}

async fn get_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Viewer)?;
    let jobs = s.jobs.lock().expect("jobs lock");
    jobs.get(&id)
        .map(|j| Json(j.full()))
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown_job", ""))
}

async fn start_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    let state = s.clone();
    let result = tokio::task::spawn_blocking(move || state.start_job(&id).map(|_| id))
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))?;
    match result {
        Ok(id) => {
            let jobs = s.jobs.lock().expect("jobs lock");
            Ok(Json(
                jobs.get(&id).map(JobRecord::summary).unwrap_or(Value::Null),
            ))
        }
        Err(StartError::UnknownJob) => Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_job", "")),
        Err(StartError::UnknownMachine) => Err(MachineError::Unknown.into()),
        Err(StartError::NotReady) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_ready",
            "the job already ran or is running",
        )),
        Err(StartError::MachineBusy) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "machine_busy",
            "another job is running on this machine",
        )),
        Err(StartError::Plan(code, detail)) => Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            code,
            detail,
        )),
    }
}

async fn confirm_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    if s.confirm_job(&id) {
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_running",
            "the job is not running",
        ))
    }
}

async fn cancel_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    if s.cancel_job(&id) {
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_cancellable",
            "the job is not ready or running",
        ))
    }
}

async fn delete_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    {
        let jobs = s.jobs.lock().expect("jobs lock");
        match jobs.get(&id) {
            None => return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_job", "")),
            Some(j) if j.state.active() => {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "job_active",
                    "stop it first",
                ))
            }
            Some(_) => {}
        }
    }
    s.jobs.lock().expect("jobs lock").remove(&id);
    s.publish(json!({ "type": "job_removed", "id": id }));
    Ok(Json(json!({ "ok": true })))
}

// ---- what happens, as it happens ------------------------------------------------------------------------------------------------------

// ---- the node of the drill head, through the server

/// The address and the token of the node of this machine's drill head, from the machine's configuration.
fn head_of(s: &Shared, id: &str) -> Result<(String, String), ApiError> {
    let machine = machine_of(s, id)?;
    let c = machine.connection_config()?;
    if c.head_address.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "no_head",
            "this machine has no node of the drill head: set head_address in its configuration",
        ));
    }
    Ok((c.head_address, c.head_token))
}

fn head_error(e: HeadError) -> ApiError {
    match e {
        HeadError::NoAddress => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_address",
            "head_address is not a host and a port",
        ),
        HeadError::Unreachable => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "head_unreachable",
            "the node of the drill head does not answer",
        ),
    }
}

/// What the node says as it says it: its status code and its content type are kept, so the picture of a client and the one of the node are the same.
fn pass_on(status: u16, content_type: String, body: Vec<u8>) -> Response {
    let mut r = Response::new(axum::body::Body::from(body));
    *r.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    if let Ok(v) = header::HeaderValue::from_str(&content_type) {
        r.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    r
}

/// The status of the node: the camera, the laser, the speed and the temperature of the spindle and the protect output, as it answers them.
async fn head_status(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    need(&who, Role::Viewer)?;
    let (address, token) = head_of(&s, &id)?;
    let (status, ct, body) = head::fetch(&address, &token, "GET", "/api/status")
        .await
        .map_err(head_error)?;
    if status != 200 {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "head_refused",
            format!("the node answered {status}"),
        ));
    }
    Ok(pass_on(200, ct, body))
}

#[derive(Deserialize)]
struct MeasureQuery {
    samples: Option<u32>,
}

/// One measure of the table under the head (the laser): the node's own answer, with its status code, so that a refused measure says why.
async fn head_measure(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    Query(q): Query<MeasureQuery>,
) -> Result<Response, ApiError> {
    need(&who, Role::Operator)?;
    let (address, token) = head_of(&s, &id)?;
    let path = match q.samples {
        Some(n) if (1..=31).contains(&n) => format!("/api/measure?samples={n}"),
        Some(_) => {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "bad_samples",
                "samples is from 1 to 31",
            ))
        }
        None => "/api/measure".to_string(),
    };
    let (status, ct, body) = head::fetch(&address, &token, "POST", &path)
        .await
        .map_err(head_error)?;
    Ok(pass_on(status, ct, body))
}

/// The ring of LEDs around the camera of the node.
async fn head_light(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    need(&who, Role::Viewer)?;
    let (address, token) = head_of(&s, &id)?;
    let (status, ct, body) = head::fetch(&address, &token, "GET", "/api/light")
        .await
        .map_err(head_error)?;
    Ok(pass_on(status, ct, body))
}

/// Asks the ring for something: the words are the node's (`on`, `level`, `tone`, `color`, `sectors`, `cue`, `cue_ms`) and go through as they come.
async fn head_light_set(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    parts: Parts,
) -> Result<Response, ApiError> {
    need(&who, Role::Operator)?;
    let query = parts.uri.query().unwrap_or("");
    if !inspect::light_query_ok(query) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_light",
            "the request for the light has signs that the node does not use",
        ));
    }
    let (address, token) = head_of(&s, &id)?;
    let path = if query.is_empty() {
        "/api/light".to_string()
    } else {
        format!("/api/light?{query}")
    };
    let (status, ct, body) = head::fetch(&address, &token, "POST", &path)
        .await
        .map_err(head_error)?;
    Ok(pass_on(status, ct, body))
}

/// One picture of the camera as JPEG.
async fn head_snapshot(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    need(&who, Role::Viewer)?;
    let (address, token) = head_of(&s, &id)?;
    let (status, ct, body) =
        head::fetch_limit(&address, &token, "GET", "/api/snapshot", 6 * 1024 * 1024)
            .await
            .map_err(head_error)?;
    Ok(pass_on(status, ct, body))
}

/// The profile of the analyser of this machine: the tolerances and which checks are on.
async fn inspect_profile(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Viewer)?;
    machine_of(&s, &id)?;
    let p = inspect::load_profile(&s.cfg.data_dir, &id);
    Ok(Json(p.to_json()))
}

/// Changes the profile: `{"preset": "fine_pitch"}` starts from one of the presets, and any member of the profile changes what it names. What cannot be used is listed in
/// `rejected` and changes nothing.
async fn inspect_profile_put(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    machine_of(&s, &id)?;
    let mut base = inspect::load_profile(&s.cfg.data_dir, &id);
    let mut body = body;
    let mut rejected: Vec<String> = Vec::new();
    if let Some(name) = body.get("preset").and_then(Value::as_str) {
        match eh3d_inspect::Profile::preset(name) {
            Some(mut preset) => {
                // a preset keeps the scale of the picture: it is a property of the camera, not of the strictness
                preset.px_per_mm = base.px_per_mm;
                base = preset;
            }
            None => rejected.push("preset".to_string()),
        }
    }
    if let Some(obj) = body.as_object_mut() {
        obj.remove("preset");
    }
    let (p, bad) = eh3d_inspect::Profile::from_json_over(&base, &body.to_string())
        .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_profile", e))?;
    rejected.extend(bad);
    inspect::save_profile(&s.cfg.data_dir, &id, &p).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store",
            format!("the profile could not be kept: {e}"),
        )
    })?;
    let mut shown = p.to_json();
    shown["rejected"] = json!(rejected);
    Ok(Json(shown))
}

#[derive(Deserialize, Default)]
struct InspectBody {
    /// The holes of the job that the camera should see, from the centre of the picture.
    #[serde(default)]
    holes: Vec<InspectHole>,
    /// Turn the ring on at this level (white) before the picture. Absent: the light stays as it is.
    light: Option<u32>,
    #[serde(default = "yes")]
    auto_light: bool,
    /// Make the ring say how it went.
    #[serde(default = "yes")]
    cue: bool,
    /// Put the ring back as it was when done.
    #[serde(default)]
    restore_light: bool,
    /// Members of the profile for this check only.
    profile: Option<Value>,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct InspectHole {
    x_mm: f64,
    y_mm: f64,
    dia_mm: f64,
}

/// The check of the work: light, picture, analysis, the cue on the ring. The answer is the report, which is also kept as the last one of the machine.
async fn inspect_run(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    body: Option<Json<InspectBody>>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    let (address, token) = head_of(&s, &id)?;
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let mut profile = inspect::load_profile(&s.cfg.data_dir, &id);
    if let Some(over) = &b.profile {
        let (p, _) = eh3d_inspect::Profile::from_json_over(&profile, &over.to_string())
            .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_profile", e))?;
        profile = p;
    }
    if b.holes.len() > 500 {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "too_many_holes",
            "a picture is checked against at most 500 holes",
        ));
    }
    let params = inspect::Params {
        holes: b
            .holes
            .iter()
            .map(|h| eh3d_inspect::ExpectedHole {
                x_mm: h.x_mm,
                y_mm: h.y_mm,
                dia_mm: h.dia_mm,
            })
            .collect(),
        light_level: b.light.map(|l| l.min(100)),
        auto_light: b.auto_light,
        cue: b.cue,
        restore_light: b.restore_light,
    };
    let report = inspect::run(&address, &token, profile, params)
        .await
        .map_err(|e| match e {
            InspectError::Head(e) => head_error(e),
            InspectError::Refused(status, what) => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "head_refused",
                format!("the node answered {status} to the {what}"),
            ),
            InspectError::Picture(why) => ApiError::new(
                StatusCode::BAD_GATEWAY,
                "bad_picture",
                format!("the picture of the node could not be read: {why}"),
            ),
        })?;
    let mut shown = report.to_json();
    shown["at"] = json!(now());
    s.inspections
        .lock()
        .expect("inspections lock")
        .insert(id.clone(), shown.clone());
    s.publish(json!({ "type": "inspection", "id": id, "verdict": shown["verdict"] }));
    Ok(Json(shown))
}

/// Sets the scale of the picture from the last check: the first hole that was found tells how many pixels it covered, and the job says how wide it is. The profile keeps the
/// new scale. `409 no_hole` when the last check found no hole to measure.
async fn inspect_calibrate(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    machine_of(&s, &id)?;
    let last = s
        .inspections
        .lock()
        .expect("inspections lock")
        .get(&id)
        .cloned()
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "no_inspection",
                "check a hole first: the scale comes from its width",
            )
        })?;
    let mut profile = inspect::load_profile(&s.cfg.data_dir, &id);
    let hole = last["holes"].as_array().and_then(|holes| {
        holes.iter().find(|h| {
            h["found"] == json!(true) && h["expected_dia_mm"].as_f64().unwrap_or(0.0) > 0.0
        })
    });
    let scale = hole.and_then(|h| {
        // the width was worked out with the scale of that moment: in pixels it did not depend on it
        let d_px = h["dia_mm"].as_f64()? * profile.px_per_mm;
        eh3d_inspect::Profile::calibrate_from_hole(d_px, h["expected_dia_mm"].as_f64()?)
    });
    let Some(scale) = scale else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "no_hole",
            "the last check found no hole to measure the scale with",
        ));
    };
    profile.px_per_mm = (scale * 10.0).round() / 10.0;
    inspect::save_profile(&s.cfg.data_dir, &id, &profile).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store",
            format!("the profile could not be kept: {e}"),
        )
    })?;
    // what was checked with the old scale is no longer the last check
    s.inspections.lock().expect("inspections lock").remove(&id);
    Ok(Json(profile.to_json()))
}

/// The last check of this machine.
async fn inspect_last(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Viewer)?;
    machine_of(&s, &id)?;
    match s.inspections.lock().expect("inspections lock").get(&id) {
        Some(v) => Ok(Json(v.clone())),
        None => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "no_inspection",
            "this machine has not been checked since the server started",
        )),
    }
}

/// The camera of the node as the node sends it (MJPEG), handed on while it comes. The token may be in the address because a browser cannot set a header on a picture.
async fn head_stream(
    State(s): State<Shared>,
    parts: Parts,
    Path(id): Path<String>,
    Query(q): Query<TokenQuery>,
) -> Result<Response, ApiError> {
    let token = bearer(&parts).or(q.token).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "send the token in the address or in Authorization",
        )
    })?;
    let who = s
        .auth
        .lock()
        .expect("auth lock")
        .identify(&token, now())
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", ""))?;
    need(&who, Role::Viewer)?;
    let (address, head_token) = head_of(&s, &id)?;
    let answer = head::open(&address, &head_token, "GET", "/stream")
        .await
        .map_err(head_error)?;
    if answer.status != 200 {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "head_refused",
            format!("the node answered {}", answer.status),
        ));
    }
    use tokio::io::AsyncReadExt;
    let reader = std::io::Cursor::new(answer.rest).chain(answer.stream);
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(reader));
    let mut r = Response::new(body);
    if let Ok(v) = header::HeaderValue::from_str(&answer.content_type) {
        r.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    Ok(r)
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// A websocket that sends every change as a JSON text: `{"type":"job","job":{...}}` for a job, `{"type":"job_removed","id":...}` and
/// `{"type":"machine","id":...}`. The token goes in the address (`?token=`), because a browser cannot set headers on a websocket.
async fn events(
    State(s): State<Shared>,
    parts: Parts,
    Query(q): Query<TokenQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let token = bearer(&parts).or(q.token).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "send the token in the address or in Authorization",
        )
    })?;
    let who = s
        .auth
        .lock()
        .expect("auth lock")
        .identify(&token, now())
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", ""))?;
    need(&who, Role::Viewer)?;
    Ok(ws.on_upgrade(move |socket| relay(socket, s)))
}

async fn relay(mut socket: WebSocket, s: Shared) {
    let mut rx = s.events.subscribe();
    let hello = {
        let jobs = s.jobs.lock().expect("jobs lock");
        json!({ "type": "hello", "jobs": jobs.list().iter().map(JobRecord::summary).collect::<Vec<_>>() }).to_string()
    };
    if socket.send(Message::Text(hello)).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            event = rx.recv() => match event {
                Ok(text) => { if socket.send(Message::Text(text)).await.is_err() { return; } }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => {}
            },
        }
    }
}
