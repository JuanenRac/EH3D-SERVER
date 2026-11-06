//! The single login of every application of EH3D, under `/api/v1/auth`: a login that says which application it is for (`app`), with an expiry that can be renewed up to a
//! limit, a list of the open logins of a person that they can close one by one, the policy of the passwords and of the lockout from `server.cfg`, and the audit of the
//! logins, the logouts and the refusals. The first version of the login (`POST /login`, `POST /logout`, `GET /session`) is not touched: it makes the same kind of login
//! with no application. The rules are in `src/auth.rs` and `docs/AUTH.md`.

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::{ApiError, ApiJson, ApiResult, Authed, Shared};
use crate::auth::{Role, SessionInfo};
use crate::jobs::now;
use crate::sync::Locked;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/logout-all", post(logout_all))
        .route("/api/v1/auth/refresh", post(refresh))
        .route("/api/v1/auth/session", get(session))
        .route("/api/v1/auth/sessions", get(list_sessions))
        .route("/api/v1/auth/sessions/:id", delete(close_session))
        .route("/api/v1/auth/policy", get(policy))
        .route("/api/v1/auth/apps", get(apps))
}

fn internal() -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "")
}

fn person_only() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "forbidden",
        "a token of a program has no login",
    )
}

#[derive(Deserialize)]
struct AppLogin {
    name: String,
    password: String,
    app: String,
}

async fn login(
    State(s): State<Shared>,
    peer: Option<ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    ApiJson(b): ApiJson<AppLogin>,
) -> ApiResult {
    // the address of the socket: a header could be written by the one who is guessing (see `client_ip`)
    let ip = crate::api::client_ip(&s, peer, &headers);
    let shown_ip = ip.map(|a| a.to_string()).unwrap_or_default();
    if !crate::apps::allowed(&s.cfg, &b.app) {
        s.audit.event(
            "login_denied",
            &[
                ("who", &b.name),
                ("app", &b.app),
                ("ip", &shown_ip),
                ("code", "bad_app"),
            ],
        );
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_app",
            "",
        ));
    }
    let (name, app) = (b.name.clone(), b.app.clone());
    let max = s.cfg.auth_max_sessions;
    let state = s.clone();
    let result = tokio::task::spawn_blocking(move || {
        let mut auth = state.auth.locked();
        auth.login_with(&b.name, &b.password, now(), ip, &b.app, max)
    })
    .await
    .map_err(|_| internal())?;
    match &result {
        Ok((_, role, _, id)) => s.audit.event(
            "login",
            &[
                ("who", &name),
                ("role", role.name()),
                ("app", &app),
                ("session", id),
                ("ip", &shown_ip),
            ],
        ),
        Err(e) => s.audit.event(
            "login_failed",
            &[
                ("who", &name),
                ("app", &app),
                ("ip", &shown_ip),
                ("code", e.code()),
            ],
        ),
    }
    let (token, role, expires, id) = result?;
    Ok(Json(json!({
        "token": token, "name": name, "role": role, "app": app,
        "session_id": id, "expires_s": expires,
    })))
}

async fn logout(State(s): State<Shared>, Authed(who, token): Authed) -> ApiResult {
    let info = s.auth.locked().session_of(&token);
    s.auth.locked().logout(&token);
    let (app, id) = info.map(|i| (i.app, i.id)).unwrap_or_default();
    crate::api::note(&s, "logout", &who, &[("app", &app), ("session", &id)]);
    Ok(Json(json!({ "ok": true })))
}

async fn logout_all(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    if who.is_token {
        return Err(person_only());
    }
    let closed = s.auth.locked().close_sessions_of(&who.name);
    crate::api::note(&s, "logout_all", &who, &[("closed", &closed.to_string())]);
    Ok(Json(json!({ "closed": closed })))
}

async fn refresh(State(s): State<Shared>, Authed(who, token): Authed) -> ApiResult {
    if who.is_token {
        return Err(person_only());
    }
    let left = s.auth.locked().refresh(&token, now())?;
    crate::api::note(&s, "session_refresh", &who, &[]);
    Ok(Json(json!({ "expires_s": left })))
}

fn info_json(info: &SessionInfo, current: bool) -> Value {
    json!({
        "id": info.id, "name": info.name, "role": info.role, "app": info.app,
        "created": info.created, "last_seen": info.last_seen, "expires": info.expires,
        "ip": info.ip, "current": current,
    })
}

/// Who the caller is, as the claims of the login: the role, the application, the scopes.
async fn session(State(s): State<Shared>, Authed(who, token): Authed) -> ApiResult {
    let info = s.auth.locked().session_of(&token);
    let app = info
        .as_ref()
        .map(|i| i.app.clone())
        .filter(|a| !a.is_empty());
    Ok(Json(json!({
        "name": who.name, "role": who.role, "is_token": who.is_token,
        "app": app,
        "session_id": info.as_ref().map(|i| i.id.clone()),
        "created": info.as_ref().map(|i| i.created),
        "last_seen": info.as_ref().map(|i| i.last_seen),
        "expires": info.as_ref().map(|i| i.expires),
        "token_id": who.token_id, "template": who.template, "scopes": who.scopes,
    })))
}

#[derive(Deserialize)]
struct SessionsQuery {
    all: Option<bool>,
}

async fn list_sessions(
    State(s): State<Shared>,
    Authed(who, token): Authed,
    Query(q): Query<SessionsQuery>,
) -> ApiResult {
    let all = q.all.unwrap_or(false);
    if all {
        crate::api::need(&who, Role::Admin)?;
    }
    let auth = s.auth.locked();
    let mine = auth.session_of(&token).map(|i| i.id);
    let list = if who.is_token {
        Vec::new()
    } else if all {
        auth.sessions(None, now())
    } else {
        auth.sessions(Some(&who.name), now())
    };
    let items: Vec<Value> = list
        .iter()
        .map(|i| info_json(i, mine.as_deref() == Some(i.id.as_str())))
        .collect();
    Ok(Json(json!({ "sessions": items })))
}

async fn close_session(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    if who.is_token {
        return Err(person_only());
    }
    // a person closes their own; an administrator closes anybody's
    let owner = if who.role == Role::Admin {
        None
    } else {
        Some(who.name.as_str())
    };
    let closed = s.auth.locked().close_session(&id, owner)?;
    crate::api::note(
        &s,
        "session_close",
        &who,
        &[
            ("session", &closed.id),
            ("of", &closed.name),
            ("app", &closed.app),
        ],
    );
    Ok(Json(json!({ "ok": true })))
}

fn switch(key: &str, value: Value, safe: bool) -> Value {
    json!({ "key": key, "value": value, "safe": safe })
}

/// The switches of security of `server.cfg` with their values (never a secret), and which of them are looser than the recommended value.
async fn policy(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    let c = &s.cfg;
    let needs_setup = s.auth.locked().needs_setup();
    let switches = vec![
        switch("url_token", json!(c.url_token), !c.url_token),
        switch(
            "trust_forwarded_for",
            json!(c.trust_forwarded_for),
            !c.trust_forwarded_for,
        ),
        switch(
            "local_until_setup",
            json!(c.local_until_setup),
            c.local_until_setup || !needs_setup,
        ),
        switch(
            "persist_sessions",
            json!(c.persist_sessions),
            !c.persist_sessions,
        ),
        switch("exit_on_panic", json!(c.exit_on_panic), c.exit_on_panic),
        switch(
            "login_ip_limit",
            json!(c.login_ip_limit),
            c.login_ip_limit > 0,
        ),
        switch(
            "session_hours",
            json!(c.session_hours),
            c.session_hours <= 168,
        ),
        switch(
            "token_default_days",
            json!(c.token_default_days),
            c.token_default_days > 0,
        ),
        switch(
            "auth_known_apps_only",
            json!(c.auth_known_apps_only),
            c.auth_known_apps_only,
        ),
        switch(
            "auth_max_sessions",
            json!(c.auth_max_sessions),
            c.auth_max_sessions > 0,
        ),
        switch(
            "auth_max_failures",
            json!(c.auth_max_failures),
            c.auth_max_failures <= 10,
        ),
        switch(
            "auth_lockout_s",
            json!(c.auth_lockout_s),
            c.auth_lockout_s >= 10,
        ),
        switch(
            "auth_refresh_max_hours",
            json!(c.auth_refresh_max_hours),
            c.auth_refresh_max_hours <= 720,
        ),
        switch(
            "password_min_len",
            json!(c.password_min_len),
            c.password_min_len >= 8,
        ),
        switch(
            "password_classes",
            json!(c.password_classes),
            c.password_classes >= 2,
        ),
        switch(
            "password_deny_name",
            json!(c.password_deny_name),
            c.password_deny_name,
        ),
        switch("audit_file", json!(c.audit_file), c.audit_file),
        switch("audit_denials", json!(c.audit_denials), c.audit_denials),
        switch(
            "project_require_if_match",
            json!(c.project_require_if_match),
            c.project_require_if_match,
        ),
        switch("project_shared", json!(c.project_shared), !c.project_shared),
    ];
    let attention: Vec<String> = switches
        .iter()
        .filter(|v| v["safe"] == false)
        .filter_map(|v| v["key"].as_str().map(str::to_string))
        .collect();
    Ok(Json(json!({
        "switches": switches,
        "attention": attention,
        "extra_apps": c.auth_extra_apps,
    })))
}

async fn apps(State(s): State<Shared>, Authed(_who, _): Authed) -> ApiResult {
    Ok(Json(json!({
        "apps": crate::apps::list(&s.cfg),
        "known_only": s.cfg.auth_known_apps_only,
    })))
}

/// Writes a line in the audit for every request refused for lack of a login or of a right (`401` and `403`): the method, the path (never the query, where a ticket
/// may be), the address and whether a token came. The refusals of the logins have their own lines.
pub async fn audit_denials(
    State(s): State<Shared>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let ip = req
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip().to_string())
        .unwrap_or_default();
    let had_token = req.headers().contains_key(header::AUTHORIZATION);
    let response = next.run(req).await;
    let status = response.status();
    let own_line = path == "/api/v1/login" || path == "/api/v1/auth/login";
    if s.cfg.audit_denials
        && !own_line
        && (status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN)
    {
        s.audit.event(
            "denied",
            &[
                ("method", &method),
                ("path", &path),
                ("status", &status.as_u16().to_string()),
                ("ip", &ip),
                ("token", if had_token { "yes" } else { "no" }),
            ],
        );
    }
    response
}
