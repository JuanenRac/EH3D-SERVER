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
    Router::new()
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
        .route("/api/v1/fleet", get(fleet))
        .route("/api/v1/jobs", get(list_jobs))
        .route("/api/v1/jobs/:id", get(get_job).delete(delete_job))
        .route("/api/v1/jobs/:id/start", post(start_job))
        .route("/api/v1/jobs/:id/confirm", post(confirm_job))
        .route("/api/v1/jobs/:id/cancel", post(cancel_job))
        .route("/api/v1/events", get(events))
        .layer(DefaultBodyLimit::max(limit))
        .with_state(state)
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
