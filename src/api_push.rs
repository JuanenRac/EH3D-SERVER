//! The phones that get notices (`src/push.rs`): a person who may run jobs registers the phone they are logged in with, and takes it away. A token of a program has no
//! phone. Registering works while `push_enabled` is off, and the answer says that nothing is sent.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::api::{need, ApiError, ApiJson, ApiResult, Authed, Shared};
use crate::auth::{Identity, Role};
use crate::jobs::now;
use crate::push;

pub fn routes() -> Router<Shared> {
    Router::new().route("/api/v1/push/devices", post(register).delete(forget))
}

fn bad(detail: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", detail)
}

fn internal() -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", "")
}

/// The token of the request, from a person who may run jobs.
fn token_of(who: &Identity, body: &Value) -> Result<String, ApiError> {
    need(who, Role::Operator)?;
    if who.is_token {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "a token of a program has no phone",
        ));
    }
    body.get("token")
        .and_then(Value::as_str)
        .filter(|t| push::token_ok(t))
        .map(str::to_string)
        .ok_or_else(|| bad("token must be the registration token of the phone"))
}

async fn register(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(body): ApiJson<Value>,
) -> ApiResult {
    let token = token_of(&who, &body)?;
    if !s
        .push_devices
        .add(&token, &who.name, now())
        .map_err(|_| internal())?
    {
        return Err(bad("the server keeps no more phones"));
    }
    Ok(Json(
        json!({"registered": true, "enabled": s.policy.get().push_enabled}),
    ))
}

async fn forget(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(body): ApiJson<Value>,
) -> ApiResult {
    let token = token_of(&who, &body)?;
    let removed = s.push_devices.remove(&token).map_err(|_| internal())?;
    Ok(Json(json!({ "removed": removed })))
}
