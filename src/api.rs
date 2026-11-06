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
use crate::state::{AppState, PauseError, StartError};
use crate::sync::Locked;

pub type Shared = Arc<AppState>;

pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    detail: String,
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, code: &'static str, detail: impl Into<String>) -> Self {
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

/// The body of a request as JSON; a body that does not read comes back as `{error, detail}` like every other refusal.
pub struct ApiJson<T>(pub T);

#[async_trait]
impl<S, T> axum::extract::FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    Json<T>: axum::extract::FromRequest<S, Rejection = axum::extract::rejection::JsonRejection>,
{
    type Rejection = ApiError;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(ApiJson(v)),
            Err(rejection) => {
                let status = rejection.status();
                let code = if status == StatusCode::PAYLOAD_TOO_LARGE {
                    "too_big"
                } else {
                    "bad_request"
                };
                Err(ApiError::new(status, code, rejection.body_text()))
            }
        }
    }
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        let status = match e {
            AuthError::BadSetup | AuthError::ForbiddenScope => StatusCode::FORBIDDEN,
            AuthError::BadName
            | AuthError::WeakPassword
            | AuthError::BadDays
            | AuthError::BadTemplate
            | AuthError::BadScope => StatusCode::UNPROCESSABLE_ENTITY,
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

pub(crate) type ApiResult = Result<Json<Value>, ApiError>;

fn forbidden() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "forbidden",
        "this needs a higher role",
    )
}

/// The address a request counts as coming from, for the limit of wrong passwords and for the audit: the address of the connection, and nothing else. Only when the
/// settings say `trust_forwarded_for`, and only on a connection that comes from 127.0.0.1 (the reverse proxy on this very computer), the address the proxy put at the end of
/// `X-Forwarded-For` is the one that counts: a client that reaches the server directly cannot choose its address by writing the header.
pub fn client_ip(
    s: &Shared,
    peer: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    headers: &axum::http::HeaderMap,
) -> Option<std::net::IpAddr> {
    let forwarded = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok());
    forwarded_ip(
        s.cfg.trust_forwarded_for,
        peer.map(|axum::extract::ConnectInfo(a)| a.ip()),
        forwarded,
    )
}

/// The decision of [`client_ip`], on plain values.
pub fn forwarded_ip(
    trust: bool,
    peer: Option<std::net::IpAddr>,
    forwarded: Option<&str>,
) -> Option<std::net::IpAddr> {
    let peer = peer?;
    let from_the_proxy = match peer {
        std::net::IpAddr::V4(a) => a == std::net::Ipv4Addr::LOCALHOST,
        std::net::IpAddr::V6(a) => a.to_ipv4_mapped() == Some(std::net::Ipv4Addr::LOCALHOST),
    };
    if !(trust && from_the_proxy) {
        return Some(peer);
    }
    // the proxy adds the address it saw at the end of the list; what is before it was written by the client
    let last = forwarded.and_then(|h| h.rsplit(',').next()).map(str::trim);
    let parsed = last.and_then(|v| {
        v.parse::<std::net::IpAddr>()
            .ok()
            .or_else(|| v.parse::<std::net::SocketAddr>().ok().map(|a| a.ip()))
    });
    Some(parsed.unwrap_or(peer))
}

/// Tells the audit that `who` did something (and, in `more`, to what).
pub(crate) fn note(s: &Shared, event: &str, who: &Identity, more: &[(&str, &str)]) {
    let mut fields: Vec<(&str, &str)> = vec![("who", who.name.as_str()), ("role", who.role.name())];
    fields.extend_from_slice(more);
    s.audit.event(event, &fields);
}

/// Refuses a caller whose token is limited to scopes that do not include `scope`. A person and a token without scopes pass; the role is checked apart.
pub(crate) fn need_scope(who: &Identity, scope: &str) -> Result<(), ApiError> {
    if who.allows(scope) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden_scope",
            format!("the token does not carry the scope {scope}"),
        ))
    }
}

pub(crate) fn need(who: &Identity, role: Role) -> Result<(), ApiError> {
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
        let who = state.auth.locked().identify(&token, now());
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

/// With `local_until_setup` on, a server that has no user yet answers only the connections from its own computer: nobody else on the network can make the first administrator.
async fn local_until_setup(
    State(s): State<Shared>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if s.cfg.local_until_setup {
        let peer = req
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|c| c.0.ip());
        if let Some(ip) = peer {
            if !ip.is_loopback() && s.auth.locked().needs_setup() {
                return ApiError::new(
                    StatusCode::FORBIDDEN,
                    "local_only",
                    "this server has no user yet: make the administrator from the computer it runs on",
                )
                .into_response();
            }
        }
    }
    next.run(req).await
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
        .route("/api/v1/tokens/:id/rotate", post(rotate_token))
        .route("/api/v1/machines", get(list_machines).post(add_machine))
        .route(
            "/api/v1/machines/:id",
            get(get_machine).put(put_machine).delete(delete_machine),
        )
        .route(
            "/api/v1/machines/:id/spindle",
            get(spindle_get).put(spindle_put),
        )
        .route("/api/v1/machines/:id/preview", post(preview))
        .route("/api/v1/machines/:id/stop", post(stop_machine))
        .route("/api/v1/tickets", post(make_ticket))
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
        .route("/api/v1/jobs/:id/pause", post(pause_job))
        .route("/api/v1/jobs/:id/resume", post(resume_job))
        .route("/api/v1/jobs/:id/request", get(job_request))
        .route("/api/v1/jobs/:id/resume-from", post(resume_from))
        .route("/api/v1/updates", get(updates_view))
        .route("/api/v1/updates/check", post(updates_check))
        .route("/api/v1/updates/apply", post(updates_apply))
        .route("/api/v1/updates/undo", post(updates_undo))
        .route("/api/v1/updates/clear-history", post(updates_clear_history))
        .route("/api/v1/events", get(events))
        .merge(crate::api_tasks::routes())
        .merge(crate::api_artifacts::routes())
        .merge(crate::api_projects::routes())
        .merge(crate::api_pstore::routes(&state.cfg))
        .merge(crate::api_catalogs::routes(&state.cfg))
        .layer(DefaultBodyLimit::max(limit))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            local_until_setup,
        ))
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

/// The revision of the interface of this server (`docs/API.md`): it goes up when a request or an answer changes in a way a client has to know. A client that needs a
/// newer revision than the one it finds can say so instead of failing on a field.
pub const API_REVISION: u32 = 1;

/// Whether the data folder takes a file: the thing that fails first on a full or read-only disk, which is when the jobs and the users can no longer be kept.
fn storage_writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(".health-probe");
    let ok = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Open to anyone, so it says little: the name, the version, the revision of the interface and whether the server is well (`ok`) or not (`degraded`: its data folder does
/// not take a file, or a thread panicked while it held a lock). Who is logged in also gets the uptime, the state of the storage and the features this build and these
/// settings have.
async fn health(State(s): State<Shared>, headers: axum::http::HeaderMap) -> Json<Value> {
    let logged_in = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|token| s.auth.locked().identify(token.trim(), now()).is_some());
    let dir = s.cfg.data_dir.clone();
    let writable = tokio::task::spawn_blocking(move || storage_writable(&dir))
        .await
        .unwrap_or(false);
    let recovered = crate::sync::recovered();
    let mut answer = json!({
        "name": "eh3d-server",
        "version": env!("CARGO_PKG_VERSION"),
        "api_revision": API_REVISION,
        "status": if writable && recovered == 0 { "ok" } else { "degraded" },
    });
    if logged_in {
        answer["uptime_s"] = json!(s.started.elapsed().as_secs());
        answer["storage"] = json!({ "writable": writable });
        answer["recovered_locks"] = json!(recovered);
        answer["features"] = json!({
            "serial": cfg!(feature = "serial"),
            "studio": s.cfg.studio_dir.is_some(),
            "updates": s.updates.is_some(),
            "url_token": s.cfg.url_token,
            "local_until_setup": s.cfg.local_until_setup,
            "trust_forwarded_for": s.cfg.trust_forwarded_for,
            "exit_on_panic": s.cfg.exit_on_panic,
            "persist_sessions": s.cfg.persist_sessions,
        });
    }
    Json(answer)
}

async fn setup_status(State(s): State<Shared>) -> Json<Value> {
    Json(json!({ "needed": s.auth.locked().needs_setup() }))
}

#[derive(Deserialize)]
struct SetupBody {
    code: String,
    name: String,
    password: String,
}

async fn setup(
    State(s): State<Shared>,
    peer: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    headers: axum::http::HeaderMap,
    ApiJson(b): ApiJson<SetupBody>,
) -> ApiResult {
    let ip = client_ip(&s, peer, &headers)
        .map(|a| a.to_string())
        .unwrap_or_default();
    let name = b.name.clone();
    let audit = s.clone();
    // hashing a password takes a moment of work: not on the thread that serves the others
    let result =
        tokio::task::spawn_blocking(move || s.auth.locked().setup(&b.code, &b.name, &b.password))
            .await
            .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))?;
    match result {
        Ok(()) => audit
            .audit
            .event("setup", &[("who", &name), ("role", "admin"), ("ip", &ip)]),
        Err(e) => audit.audit.event(
            "setup_failed",
            &[("who", &name), ("ip", &ip), ("code", e.code())],
        ),
    }
    result?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct LoginBody {
    name: String,
    password: String,
}

async fn login(
    State(s): State<Shared>,
    peer: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    headers: axum::http::HeaderMap,
    ApiJson(b): ApiJson<LoginBody>,
) -> ApiResult {
    // the address of the socket: a header could be written by the one who is guessing (see `client_ip`)
    let ip = client_ip(&s, peer, &headers);
    let shown_ip = ip.map(|a| a.to_string()).unwrap_or_default();
    let name = b.name.clone();
    let audit = s.clone();
    let result = tokio::task::spawn_blocking(move || {
        s.auth.locked().login_from(&b.name, &b.password, now(), ip)
    })
    .await
    .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))?;
    match &result {
        Ok((_, role, _)) => audit.audit.event(
            "login",
            &[("who", &name), ("role", role.name()), ("ip", &shown_ip)],
        ),
        Err(e) => audit.audit.event(
            "login_failed",
            &[("who", &name), ("ip", &shown_ip), ("code", e.code())],
        ),
    }
    let (token, role, expires) = result?;
    Ok(Json(
        json!({ "token": token, "name": name, "role": role, "expires_s": expires }),
    ))
}

async fn logout(State(s): State<Shared>, Authed(who, token): Authed) -> ApiResult {
    s.auth.locked().logout(&token);
    note(&s, "logout", &who, &[]);
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
    ApiJson(b): ApiJson<PasswordBody>,
) -> ApiResult {
    if who.is_token {
        return Err(forbidden());
    }
    let audit = s.clone();
    let name = who.clone();
    let result = tokio::task::spawn_blocking(move || {
        s.auth.locked().change_password(&who.name, &b.old, &b.new)
    })
    .await
    .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))?;
    match &result {
        Ok(()) => note(&audit, "password_changed", &name, &[]),
        Err(e) => note(
            &audit,
            "password_change_failed",
            &name,
            &[("code", e.code())],
        ),
    }
    result?;
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
    note(&s, "update_check", &who, &[]);
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ApplyBody {
    components: Vec<String>,
}

/// The update and its undoing restart the server, which stops every running job: neither is done while one runs.
fn no_job_running(s: &Shared) -> Result<(), ApiError> {
    if s.jobs.locked().list().iter().any(|j| j.state.active()) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "machines_busy",
            "a job is running: the update restarts the server and would stop it",
        ));
    }
    Ok(())
}

async fn updates_apply(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(b): ApiJson<ApplyBody>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    no_job_running(&s)?;
    updates_of(&s)?
        .request("apply", &b.components)
        .map_err(updates_fail)?;
    note(
        &s,
        "update_apply",
        &who,
        &[("components", &b.components.join(","))],
    );
    Ok(Json(json!({ "ok": true })))
}

/// Puts back the version that the last update of those components left behind.
async fn updates_undo(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(b): ApiJson<ApplyBody>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    no_job_running(&s)?;
    updates_of(&s)?
        .request("undo", &b.components)
        .map_err(updates_fail)?;
    note(
        &s,
        "update_undo",
        &who,
        &[("components", &b.components.join(","))],
    );
    Ok(Json(json!({ "ok": true })))
}

/// Empties the list of what the helper did before.
async fn updates_clear_history(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    need(&who, Role::Admin)?;
    updates_of(&s)?
        .request("clear_history", &[])
        .map_err(updates_fail)?;
    note(&s, "update_clear_history", &who, &[]);
    Ok(Json(json!({ "ok": true })))
}

async fn list_users(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    need(&who, Role::Admin)?;
    let users: Vec<Value> = s
        .auth
        .locked()
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
    ApiJson(b): ApiJson<NewUser>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    let audit = s.clone();
    let (name, role) = (b.name.clone(), b.role);
    tokio::task::spawn_blocking(move || s.auth.locked().add_user(&b.name, b.role, &b.password))
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))??;
    note(
        &audit,
        "user_add",
        &who,
        &[("user", &name), ("user_role", role.name())],
    );
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
    ApiJson(b): ApiJson<UserChange>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    let audit = s.clone();
    let shown = name.clone();
    let what = format!(
        "{}{}{}",
        if b.role.is_some() { "role " } else { "" },
        if b.password.is_some() {
            "password "
        } else {
            ""
        },
        if b.disabled.is_some() { "disabled" } else { "" }
    );
    tokio::task::spawn_blocking(move || {
        s.auth
            .locked()
            .update_user(&name, b.role, b.password.as_deref(), b.disabled)
    })
    .await
    .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))??;
    note(
        &audit,
        "user_update",
        &who,
        &[("user", &shown), ("changed", what.trim())],
    );
    Ok(Json(json!({ "ok": true })))
}

async fn delete_user(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(name): Path<String>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    s.auth.locked().delete_user(&name)?;
    note(&s, "user_delete", &who, &[("user", &name)]);
    Ok(Json(json!({ "ok": true })))
}

async fn list_tokens(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    need(&who, Role::Admin)?;
    let tokens: Vec<Value> = s
        .auth
        .locked()
        .tokens()
        .iter()
        .map(|t| {
            json!({
                "id": t.id, "name": t.name, "role": t.role, "created": t.created,
                "last_used": t.last_used, "expires": t.expires,
                "template": t.template, "scopes": t.scopes,
            })
        })
        .collect();
    Ok(Json(Value::Array(tokens)))
}

#[derive(Deserialize)]
struct NewToken {
    name: String,
    role: Role,
    /// How many days the token lasts (1 to 3650); left out, the server's `token_default_days` (0: it does not expire).
    days: Option<u32>,
    /// The kind of program: `phoenix`, `cam`, `client` or `custom`. The `phoenix` and `cam` templates can never carry the scopes of the machines.
    #[serde(default)]
    template: Option<String>,
    /// What the token is limited to (see `docs/API.md`); left out, the whole of its role (for `phoenix` and `cam`, the whole of what they may hold).
    #[serde(default)]
    scopes: Option<Vec<String>>,
}

async fn add_token(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(b): ApiJson<NewToken>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    let days = b.days.or(Some(s.cfg.token_default_days).filter(|d| *d > 0));
    let (id, secret) = s.auth.locked().create_token_with(
        &b.name,
        b.role,
        now(),
        days,
        b.template.as_deref(),
        b.scopes.as_deref(),
    )?;
    note(
        &s,
        "token_add",
        &who,
        &[
            ("token", &id),
            ("name", &b.name),
            ("token_role", b.role.name()),
            ("days", &days.map(|d| d.to_string()).unwrap_or_default()),
            ("template", b.template.as_deref().unwrap_or("")),
            (
                "scopes",
                &b.scopes.as_ref().map(|l| l.join(",")).unwrap_or_default(),
            ),
        ],
    );
    // the secret is shown this once
    Ok(Json(json!({ "id": id, "token": secret })))
}

/// Gives a token a new secret; the old one stops working at once.
async fn rotate_token(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    let secret = s.auth.locked().rotate_token(&id)?;
    note(&s, "token_rotate", &who, &[("token", &id)]);
    Ok(Json(json!({ "id": id, "token": secret })))
}

async fn delete_token(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    s.auth.locked().delete_token(&id)?;
    note(&s, "token_delete", &who, &[("token", &id)]);
    Ok(Json(json!({ "ok": true })))
}

// ---- the machines ---------------------------------------------------------------------------------------------------------------------

async fn list_machines(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    need(&who, Role::Viewer)?;
    need_scope(&who, "machines:view")?;
    let list: Vec<Value> = s
        .machines
        .locked()
        .list()
        .iter()
        .map(MachineRecord::public)
        .collect();
    Ok(Json(Value::Array(list)))
}

/// Every machine with what it is doing, grouped by station: the one view a central screen needs to follow all of them at once.
async fn fleet(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    need(&who, Role::Viewer)?;
    need_scope(&who, "machines:view")?;
    let machines = s.machines.locked();
    let jobs = s.jobs.locked();
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
                "percent": percent, "pause": j.pause, "estimate_s": j.view.estimate_s, "line": j.line,
                "tool": j.current_tool(),
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
    ApiJson(record): ApiJson<MachineRecord>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    let shown = record.public();
    s.machines.locked().add(record)?;
    note(
        &s,
        "machine_add",
        &who,
        &[("machine", shown["id"].as_str().unwrap_or(""))],
    );
    s.publish(json!({ "type": "machine", "id": shown["id"] }));
    Ok(Json(shown))
}

async fn get_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Viewer)?;
    need_scope(&who, "machines:view")?;
    let machines = s.machines.locked();
    machines
        .get(&id)
        .map(|m| Json(m.public()))
        .ok_or_else(|| MachineError::Unknown.into())
}

async fn put_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<Value>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    let mut record: MachineRecord = serde_json::from_value(body.clone()).map_err(|e| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_request",
            e.to_string(),
        )
    })?;
    record.id = id;
    if s.jobs.locked().active_on(&record.id).is_some() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "machine_busy",
            "a job is running on this machine",
        ));
    }
    // a client that does not know about spindles must not undo what was set: what the request leaves out stays as it was
    if let Some(old) = s.machines.locked().get(&record.id) {
        let sent = |k: &str| body.get(k).is_some();
        if !sent("spindle") {
            record.spindle = old.spindle.clone();
        }
        if !sent("spindle_guard") {
            record.spindle_guard = old.spindle_guard.clone();
        }
        if !sent("spindle_original") {
            record.spindle_original = old.spindle_original;
        }
        if !sent("spindle_vfd") {
            record.spindle_vfd = old.spindle_vfd;
        }
        if !sent("allow_dollar") {
            record.allow_dollar = old.allow_dollar;
        }
    }
    let shown = record.public();
    s.machines.locked().replace(record)?;
    note(
        &s,
        "machine_update",
        &who,
        &[("machine", shown["id"].as_str().unwrap_or(""))],
    );
    s.publish(json!({ "type": "machine", "id": shown["id"] }));
    Ok(Json(shown))
}

/// The spindle of the machine: which one it carries, what each can hold, the switch that says what happens to a job that asks for more, and the top speed the node should be set to.
async fn spindle_get(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Viewer)?;
    Ok(Json(machine_of(&s, &id)?.spindle_view()))
}

#[derive(Deserialize)]
struct SpindleBody {
    spindle: Option<String>,
    guard: Option<String>,
    original: Option<crate::spindle::Override>,
    vfd: Option<crate::spindle::Override>,
}

/// Sets the spindle, the switch (`reject` or `warn`) or the limits of a spindle (`original` and `vfd`: the members that are sent replace the starting ones, `{}` goes back to them).
async fn spindle_put(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    ApiJson(b): ApiJson<SpindleBody>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    let record = s
        .machines
        .locked()
        .edit(&id, |m| {
            if let Some(v) = b.spindle {
                m.spindle = v;
            }
            if let Some(v) = b.guard {
                m.spindle_guard = v;
            }
            if let Some(v) = b.original {
                m.spindle_original = Some(v).filter(|o| *o != Default::default());
            }
            if let Some(v) = b.vfd {
                m.spindle_vfd = Some(v).filter(|o| *o != Default::default());
            }
        })
        .map_err(|e| {
            ApiError::new(
                if e == MachineError::BadSpindle {
                    StatusCode::UNPROCESSABLE_ENTITY
                } else {
                    return ApiError::from(e);
                },
                e.code(),
                "the spindle must be original or vfd, the switch reject or warn, and the limits numbers that can be used (the slowest not above the fastest)",
            )
        })?;
    note(&s, "spindle_update", &who, &[("machine", &id)]);
    s.publish(json!({ "type": "machine", "id": id }));
    Ok(Json(record.spindle_view()))
}

async fn delete_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Admin)?;
    if s.jobs.locked().active_on(&id).is_some() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "machine_busy",
            "a job is running on this machine",
        ));
    }
    s.machines.locked().remove(&id)?;
    note(&s, "machine_delete", &who, &[("machine", &id)]);
    s.publish(json!({ "type": "machine", "id": id }));
    Ok(Json(json!({ "ok": true })))
}

// ---- the jobs -------------------------------------------------------------------------------------------------------------------------

fn plan_error(e: plan::PlanError) -> ApiError {
    ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, e.code, e.detail)
}

fn machine_of(s: &Shared, id: &str) -> Result<MachineRecord, ApiError> {
    s.machines
        .locked()
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
    ApiJson(body): ApiJson<Value>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    need_scope(&who, "jobs:prepare")?;
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
    ApiJson(body): ApiJson<Value>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    need_scope(&who, "jobs:prepare")?;
    let machine = machine_of(&s, &id)?;
    let shown = make_job(&s, &who.name, machine, body, String::new()).await?;
    // a job asked with the chaining of the tool changer says in the audit whether the plan used it
    let rack = match shown["rack_applied"].as_bool() {
        Some(true) => "applied",
        Some(false) => "left_out",
        None => "",
    };
    let mut fields = vec![
        ("job", shown["id"].as_str().unwrap_or("")),
        ("machine", id.as_str()),
    ];
    if !rack.is_empty() {
        fields.push(("rack", rack));
    }
    note(&s, "job_create", &who, &fields);
    Ok(Json(shown))
}

/// Plans a job request for a machine and puts it in the book as ready (it never starts by itself); `resumed_from` is the job it goes on from, if any.
async fn make_job(
    s: &Shared,
    by: &str,
    machine: MachineRecord,
    body: Value,
    resumed_from: String,
) -> Result<Value, ApiError> {
    let request = request_of(&body)?;
    let name = request.name().map(str::to_string);
    let id = machine.id.clone();
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
        by: by.to_string(),
        started_by: String::new(),
        stopped_by: String::new(),
        items_total: view.items,
        items_done: 0,
        holes_total: match view.kind.as_str() {
            "drill" => view.holes.len(),
            "route" => view.paths.len(),
            _ => 0,
        },
        holes_done: 0,
        line: 0,
        message: String::new(),
        pause: None,
        request: body,
        resumed_from,
        view,
    };
    let shown = job.full();
    s.publish_job(&job);
    s.jobs.locked().add(job);
    Ok(shown)
}

/// The job and its machine, for a job that stopped (cancelled or failed): the only ones that can be gone on from.
fn stopped_job(s: &Shared, id: &str) -> Result<(JobRecord, MachineRecord), ApiError> {
    let job = s
        .jobs
        .locked()
        .get(id)
        .cloned()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown_job", ""))?;
    if !matches!(job.state, JobState::Cancelled | JobState::Failed) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_stopped",
            "only a job that was cancelled or failed can be gone on from",
        ));
    }
    let machine = machine_of(s, &job.machine)?;
    Ok((job, machine))
}

fn bad_saved_request(e: serde_json::Error) -> plan::PlanError {
    plan::PlanError {
        code: "bad_request",
        detail: e.to_string(),
    }
}

/// How many holes (drilling), paths (routing) or lines of the text (G-code) a stopped job had done, counted from the start of the work: the `skip` of the job
/// that goes on. A routing job is replanned for it; the count is the paths whose whole plan, with the tail of the job, fits in the items that ran, so a path
/// that was cut short is done again and never left out.
async fn skip_of(
    machine: &MachineRecord,
    job: &JobRecord,
    margin: usize,
) -> Result<usize, ApiError> {
    let before = job.request.get("skip").and_then(Value::as_u64).unwrap_or(0) as usize;
    match job.kind.as_str() {
        "drill" => Ok(before + job.holes_done),
        // the lines the controller had taken but not yet done are sent again: a margin before the line reached
        "gcode" => Ok(job.line.saturating_sub(margin).max(before)),
        _ => {
            if job.items_done == 0 {
                return Ok(before);
            }
            let (machine, body, items_done) =
                (machine.clone(), job.request.clone(), job.items_done);
            let done = tokio::task::spawn_blocking(move || -> Result<usize, plan::PlanError> {
                let with = |n: Option<usize>| -> Result<Option<usize>, plan::PlanError> {
                    let mut request: JobRequest =
                        serde_json::from_value(body.clone()).map_err(bad_saved_request)?;
                    if let JobRequest::Route(r) = &mut request {
                        r.only_first = n;
                    }
                    match plan::plan(&machine, &request) {
                        Ok(p) => Ok(Some(if n.is_some() {
                            p.items.len()
                        } else {
                            p.view.paths.len()
                        })),
                        Err(e) if e.code == "no_work" => Ok(None),
                        Err(e) => Err(e),
                    }
                };
                let total = with(None)?.unwrap_or(0);
                let (mut low, mut high) = (0usize, total);
                while low < high {
                    let mid = (low + high).div_ceil(2);
                    match with(Some(mid))? {
                        Some(n) if n <= items_done => low = mid,
                        _ => high = mid - 1,
                    }
                }
                Ok(low)
            })
            .await
            .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))?
            .map_err(plan_error)?;
            Ok(before + done)
        }
    }
}

/// What was asked of a job, as it came (the file, the tool table, the options), so that a client can go on from a stopped job without asking for the file
/// again. It is kept as long as the job is and goes with it.
async fn job_request(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    need_scope(&who, "machines:view")?;
    let job = s
        .jobs
        .locked()
        .get(&id)
        .cloned()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "unknown_job", ""))?;
    let next_skip = if matches!(job.state, JobState::Cancelled | JobState::Failed) {
        match machine_of(&s, &job.machine) {
            Ok(m) => skip_of(&m, &job, s.cfg.resume_margin_lines).await.ok(),
            Err(_) => None,
        }
    } else {
        None
    };
    Ok(Json(json!({
        "id": job.id, "machine": job.machine, "kind": job.kind, "name": job.name, "state": job.state,
        "resumed_from": job.resumed_from, "next_skip": next_skip, "request": job.request,
    })))
}

#[derive(Deserialize, Default)]
struct ResumeFromBody {
    /// Where to go on from, in the unit of the job (holes, paths or lines), instead of what the job had done.
    skip: Option<usize>,
    /// Replaces the tools that are off.
    skip_tools: Option<Vec<u8>>,
    /// A G-code job: start the spindle and the coolant again as they were.
    restore_outputs: Option<bool>,
    name: Option<String>,
}

/// Makes a new job, ready and not started, from the request of a cancelled or failed one with its `skip` brought forward to what it had done (so the holes,
/// paths or lines done are left out, and the sums add up over several stops). The file and the options are not asked again; the new job waits for a person
/// to start it, and a G-code one waits again for the operator before its first move.
async fn resume_from(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    body: Option<Json<ResumeFromBody>>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    need_scope(&who, "jobs:prepare")?;
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let (job, machine) = stopped_job(&s, &id)?;
    let skip = match b.skip {
        Some(n) => n,
        None => skip_of(&machine, &job, s.cfg.resume_margin_lines).await?,
    };
    let mut request = job.request.clone();
    let object = request.as_object_mut().ok_or_else(|| {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_request",
            "the saved request is not an object",
        )
    })?;
    object.insert("skip".into(), json!(skip));
    if let Some(t) = b.skip_tools {
        object.insert("skip_tools".into(), json!(t));
    }
    if let Some(r) = b.restore_outputs {
        object.insert("restore_outputs".into(), json!(r));
    }
    if let Some(n) = b.name {
        object.insert("name".into(), json!(n));
    }
    let shown = make_job(&s, &who.name, machine, request, job.id.clone()).await?;
    note(
        &s,
        "job_create",
        &who,
        &[
            ("job", shown["id"].as_str().unwrap_or("")),
            ("resumed_from", &job.id),
        ],
    );
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
    need_scope(&who, "machines:view")?;
    let jobs = s.jobs.locked();
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
    need_scope(&who, "machines:view")?;
    let jobs = s.jobs.locked();
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
    need_scope(&who, "jobs:start")?;
    let state = s.clone();
    let by = who.name.clone();
    let result = tokio::task::spawn_blocking(move || state.start_job(&id, &by).map(|_| id))
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))?;
    match result {
        Ok(id) => {
            note(&s, "job_start", &who, &[("job", &id)]);
            let jobs = s.jobs.locked();
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
    need_scope(&who, "jobs:confirm")?;
    if s.confirm_job(&id) {
        note(&s, "job_confirm", &who, &[("job", &id)]);
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_running",
            "the job is not running",
        ))
    }
}

/// The stop of a machine, whether or not the caller knows which job runs on it: the button of the clients. It does not wait for the line that is being sent.
async fn stop_machine(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    need_scope(&who, "jobs:cancel")?;
    machine_of(&s, &id)?;
    let job = s.stop_machine(&id, &who.name);
    note(
        &s,
        "machine_stop",
        &who,
        &[("machine", &id), ("job", job.as_deref().unwrap_or(""))],
    );
    s.publish(json!({ "type": "machine", "id": id }));
    Ok(Json(
        json!({ "ok": true, "stopped": job.is_some(), "job": job }),
    ))
}

async fn cancel_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    need_scope(&who, "jobs:cancel")?;
    if s.cancel_job(&id, &who.name) {
        note(&s, "job_cancel", &who, &[("job", &id)]);
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(ApiError::new(
            StatusCode::CONFLICT,
            "not_cancellable",
            "the job is not ready or running",
        ))
    }
}

fn pause_error(e: PauseError) -> ApiError {
    match e {
        PauseError::UnknownJob => ApiError::new(StatusCode::NOT_FOUND, "unknown_job", ""),
        PauseError::NotGcode => ApiError::new(
            StatusCode::CONFLICT,
            "not_pausable",
            "only a G-code job can be held; a drilling or routing job is stopped with cancel",
        ),
        PauseError::NotInState => ApiError::new(
            StatusCode::CONFLICT,
            "not_in_state",
            "a job is held while it runs and resumed while it is held",
        ),
    }
}

/// Holds a running G-code job where it is: the controller is told to hold its movement and no line goes out until `resume`.
async fn pause_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    need_scope(&who, "jobs:start")?;
    s.pause_job(&id).map_err(pause_error)?;
    note(&s, "job_pause", &who, &[("job", &id)]);
    Ok(Json(json!({ "ok": true })))
}

/// Lets a held G-code job go on (also the wait of a job that goes on after a stop, before its first move).
async fn resume_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    need_scope(&who, "jobs:start")?;
    s.resume_job(&id).map_err(pause_error)?;
    note(&s, "job_resume", &who, &[("job", &id)]);
    Ok(Json(json!({ "ok": true })))
}

async fn delete_job(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    need(&who, Role::Operator)?;
    {
        let mut jobs = s.jobs.locked();
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
        // checked and removed under the same guard: a start cannot slip in between
        jobs.remove(&id);
    }
    note(&s, "job_delete", &who, &[("job", &id)]);
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
    ApiJson(body): ApiJson<Value>,
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
    s.inspections.locked().insert(id.clone(), shown.clone());
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
    let last = s.inspections.locked().get(&id).cloned().ok_or_else(|| {
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
    s.inspections.locked().remove(&id);
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
    match s.inspections.locked().get(&id) {
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
    let (_who, _token) = identify_open(&s, &parts, &q)?;
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
    ticket: Option<String>,
}

/// A ticket for the camera or the websocket: good for one use within half a minute, so that the address does not carry the token of the session.
async fn make_ticket(State(s): State<Shared>, Authed(_who, token): Authed) -> ApiResult {
    let ticket = s.tickets.locked().issue(&token, now()).ok_or_else(|| {
        ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "throttled",
            "too many tickets are waiting",
        )
    })?;
    Ok(Json(
        json!({ "ticket": ticket, "expires_s": crate::tickets::TICKET_SECONDS }),
    ))
}

/// Who asks, for the endpoints whose caller cannot always send a header: `Authorization`, else a ticket in the address, else (only if the settings allow it)
/// the token in the address. Returns the identity and the token of the session.
fn identify_open(
    s: &Shared,
    parts: &Parts,
    q: &TokenQuery,
) -> Result<(Identity, String), ApiError> {
    let unauthorized = |why: &str| ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized", why);
    let token = if let Some(t) = bearer(parts) {
        t
    } else if let Some(ticket) = &q.ticket {
        s.tickets
            .locked()
            .redeem(ticket, now())
            .ok_or_else(|| unauthorized("the ticket is unknown, used or expired"))?
    } else if let (true, Some(t)) = (s.cfg.url_token, q.token.clone()) {
        t
    } else {
        return Err(unauthorized(
            "send the token in Authorization, or a ticket (POST /api/v1/tickets) in the address",
        ));
    };
    let who = s
        .auth
        .locked()
        .identify(&token, now())
        .ok_or_else(|| unauthorized(""))?;
    need(&who, Role::Viewer)?;
    Ok((who, token))
}

/// A websocket that sends every change as a JSON text: `{"type":"job","job":{...}}` for a job, `{"type":"job_removed","id":...}` and
/// `{"type":"machine","id":...}`. The token goes in the address (`?token=`), because a browser cannot set headers on a websocket.
/// A client that has fallen behind (it did not read as fast as the events came) is told with `{"type":"resync","missed":N}`: it asks for the fleet and the jobs again.
async fn events(
    State(s): State<Shared>,
    parts: Parts,
    Query(q): Query<TokenQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let (who, token) = identify_open(&s, &parts, &q)?;
    Ok(ws.on_upgrade(move |socket| relay(socket, s, token, who)))
}

/// How often the websocket asks again whether its session is still good.
const RECHECK_SECS: u64 = 30;

/// What the websocket sends to a client that missed `missed` events.
pub fn resync_message(missed: u64) -> String {
    json!({ "type": "resync", "missed": missed }).to_string()
}

async fn relay(mut socket: WebSocket, s: Shared, token: String, mut who: Identity) {
    let mut rx = s.events.subscribe();
    let mut scoped = s.scoped.subscribe();
    // a session that ends (logout, expiry, a user taken off) ends the flow too
    let mut recheck = tokio::time::interval(std::time::Duration::from_secs(RECHECK_SECS));
    recheck.tick().await;
    // a ping now and then, and a connection that says nothing at all (not even the pong) for too long is closed: a phone that left the Wi-Fi does not hold a place for ever
    let idle = std::time::Duration::from_secs(s.cfg.events_idle_s);
    let mut ping = tokio::time::interval(std::time::Duration::from_secs(s.cfg.events_ping_s));
    ping.tick().await;
    let mut heard = std::time::Instant::now();
    let hello = {
        let jobs = s.jobs.locked();
        json!({ "type": "hello", "jobs": jobs.list().iter().map(JobRecord::summary).collect::<Vec<_>>() }).to_string()
    };
    if socket.send(Message::Text(hello)).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            _ = recheck.tick() => {
                match s.auth.locked().identify(&token, now()) {
                    Some(now_who) if now_who.role >= Role::Viewer => who = now_who,
                    _ => return,
                }
            },
            _ = ping.tick() => {
                if heard.elapsed() >= idle {
                    return;
                }
                if socket.send(Message::Ping(Vec::new())).await.is_err() {
                    return;
                }
            },
            event = rx.recv() => match event {
                Ok(text) => { if socket.send(Message::Text(text)).await.is_err() { return; } }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    if socket.send(Message::Text(resync_message(missed))).await.is_err() { return; }
                }
                Err(_) => return,
            },
            event = scoped.recv() => match event {
                // only the events of the things this client may see are sent; the rest are not even counted as missed
                Ok(event) => {
                    if event.audience.includes(&who) && socket.send(Message::Text(event.text)).await.is_err() { return; }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    if socket.send(Message::Text(resync_message(missed))).await.is_err() { return; }
                }
                Err(_) => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => heard = std::time::Instant::now(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> std::net::IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn the_forwarded_header_counts_only_from_the_local_proxy_and_only_when_asked() {
        let h = Some("6.6.6.6, 203.0.113.9");
        // off: the socket, always
        assert_eq!(
            forwarded_ip(false, Some(ip("127.0.0.1")), h),
            Some(ip("127.0.0.1"))
        );
        // on, from the proxy: the address it added at the end, not what the client wrote before
        assert_eq!(
            forwarded_ip(true, Some(ip("127.0.0.1")), h),
            Some(ip("203.0.113.9"))
        );
        assert_eq!(
            forwarded_ip(true, Some(ip("::ffff:127.0.0.1")), h),
            Some(ip("203.0.113.9"))
        );
        assert_eq!(
            forwarded_ip(true, Some(ip("127.0.0.1")), Some("203.0.113.9:5555")),
            Some(ip("203.0.113.9"))
        );
        // on, from anybody else (even another address of this computer): the socket, whatever the header says
        assert_eq!(
            forwarded_ip(true, Some(ip("10.0.0.7")), h),
            Some(ip("10.0.0.7"))
        );
        assert_eq!(
            forwarded_ip(true, Some(ip("127.0.0.2")), h),
            Some(ip("127.0.0.2"))
        );
        // no header, or one that does not read: the socket
        assert_eq!(
            forwarded_ip(true, Some(ip("127.0.0.1")), None),
            Some(ip("127.0.0.1"))
        );
        assert_eq!(
            forwarded_ip(true, Some(ip("127.0.0.1")), Some("unknown")),
            Some(ip("127.0.0.1"))
        );
        assert_eq!(forwarded_ip(true, None, h), None);
    }
}
