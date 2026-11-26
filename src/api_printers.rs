//! The interface of the 3D printers the server watches, under `/api/v1/printers`. The state is **read** with `GET` and nothing else: there is no route to hold, stop or
//! start anything, to set a temperature, to send G-code or to give a file. The other methods register, change and remove a printer of the list (an administrator), as for
//! the nodes of the pick-and-place machines, and they touch the list of the server only, never the printer. The reading never opens a connection (it shows what the
//! poller found). The access code or key of a printer is never shown (`***`) and never audited.

use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::{ApiError, ApiJson, ApiResult, Authed, Shared};
use crate::auth::Role;
use crate::printers::{Change, PrinterError, PrinterRecord};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/v1/printers", get(list_printers).post(add_printer))
        .route(
            "/api/v1/printers/:id",
            get(get_printer).put(change_printer).delete(remove_printer),
        )
}

impl From<PrinterError> for ApiError {
    fn from(e: PrinterError) -> Self {
        let status = StatusCode::from_u16(e.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        ApiError::new(status, e.code(), e.detail())
    }
}

fn on(s: &Shared) -> Result<(), ApiError> {
    if s.printers.limits.enabled {
        Ok(())
    } else {
        Err(PrinterError::Off.into())
    }
}

fn view(s: &Shared, id: &str) -> Option<Value> {
    s.printers
        .view(id, s.policy.get().bambu_lan_enabled, Instant::now())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewPrinter {
    #[serde(default)]
    id: String,
    name: String,
    family: String,
    link: String,
    #[serde(default)]
    address: String,
    #[serde(default)]
    serial: String,
    #[serde(default)]
    fingerprint: String,
    #[serde(default)]
    secret: String,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrinterChange {
    name: Option<String>,
    address: Option<String>,
    serial: Option<String>,
    fingerprint: Option<String>,
    secret: Option<String>,
    enabled: Option<bool>,
}

/// A body that does not read is said without quoting it: what could not be read may be the access code of a printer.
fn body<T>(read: Result<ApiJson<T>, ApiError>) -> Result<T, ApiError> {
    match read {
        Ok(ApiJson(v)) => Ok(v),
        Err(_) => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "the body does not read as a printer",
        )),
    }
}

async fn list_printers(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    on(&s)?;
    Ok(axum::Json(s.printers.document(
        s.policy.get().bambu_lan_enabled,
        Instant::now(),
    )))
}

async fn get_printer(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    on(&s)?;
    view(&s, &id)
        .map(axum::Json)
        .ok_or_else(|| PrinterError::Unknown.into())
}

async fn add_printer(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    read: Result<ApiJson<NewPrinter>, ApiError>,
) -> Result<(StatusCode, axum::Json<Value>), ApiError> {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "machines:manage")?;
    on(&s)?;
    let new = body(read)?;
    let record = s.printers.add(
        PrinterRecord {
            id: new.id,
            name: new.name,
            family: new.family,
            link: new.link,
            address: new.address,
            serial: new.serial,
            fingerprint: new.fingerprint,
            enabled: new.enabled,
        },
        &new.secret,
    )?;
    crate::api::note(
        &s,
        "printer_add",
        &who,
        &[
            ("printer", &record.id),
            ("family", &record.family),
            ("link", &record.link),
        ],
    );
    Ok((
        StatusCode::CREATED,
        axum::Json(view(&s, &record.id).unwrap_or(Value::Null)),
    ))
}

async fn change_printer(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    read: Result<ApiJson<PrinterChange>, ApiError>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "machines:manage")?;
    on(&s)?;
    let change = body(read)?;
    let record = s.printers.update(
        &id,
        Change {
            name: change.name,
            address: change.address,
            serial: change.serial,
            fingerprint: change.fingerprint,
            secret: change.secret,
            enabled: change.enabled,
        },
    )?;
    crate::api::note(&s, "printer_change", &who, &[("printer", &record.id)]);
    Ok(axum::Json(view(&s, &record.id).unwrap_or(Value::Null)))
}

async fn remove_printer(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "machines:manage")?;
    on(&s)?;
    s.printers.remove(&id)?;
    crate::api::note(&s, "printer_remove", &who, &[("printer", &id)]);
    Ok(axum::Json(json!({ "ok": true })))
}
