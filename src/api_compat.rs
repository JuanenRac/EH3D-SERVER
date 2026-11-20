//! The compatibility routes: a few read-only routes at the root of the address that answer the way Moonraker and OctoPrint do, so that a program written for them can read the
//! state of a 3D printer of the plugin `printer3d`. They are **off** unless the switch `compat_api` of the security page is on (`404 compat_off` otherwise), and each needs
//! the API key of one machine in the header `X-Api-Key` (made by an administrator with `POST /api/v1/work/machines/{id}/compat/key`); the key opens that machine and no other.
//! The machine must also be set to give the shape (its setting `compat`: `moonraker`, `octoprint` or `both`). Nothing is written through these routes: starting, pausing or
//! cancelling a print, sending G-code and uploading files are not served here (use `/api/v1/work/`). The shapes are the ones of `GET /api/v1/work/machines/{id}/compat/...`.
//! They are not part of the OpenAPI document of the interface, because they are not under `/api/v1/`; `docs/MACHINE_PLUGINS.md` describes them.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::api::{ApiError, ApiResult, Shared};
use crate::plugins::PluginError;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/server/info", get(moonraker_server_info))
        .route("/printer/info", get(moonraker_printer_info))
        .route("/printer/objects/query", get(moonraker_query))
        .route("/api/version", get(octoprint_version))
        .route("/api/printer", get(octoprint_printer))
        .route("/api/job", get(octoprint_job))
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
