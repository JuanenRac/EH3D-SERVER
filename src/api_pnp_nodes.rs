//! The interface of the nodes of the pick-and-place machines, under `/api/v1/pnp/nodes`: register a feeder node or a head node (its address, its token and the machine it
//! belongs to), read what it said last, ask it now, change it and remove it. The reading never opens a connection (it shows what the poller found); only `poll` does, and
//! only to the node it names. The token of a node is never shown.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::{ApiError, ApiJson, ApiResult, Authed, Shared};
use crate::auth::Role;
use crate::pnp_nodes::{NodeError, NodeRecord};
use std::time::Instant;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/v1/pnp/nodes", get(list_nodes).post(add_node))
        .route(
            "/api/v1/pnp/nodes/:id",
            get(get_node).put(change_node).delete(remove_node),
        )
        .route("/api/v1/pnp/nodes/:id/poll", post(poll_node))
}

impl From<NodeError> for ApiError {
    fn from(e: NodeError) -> Self {
        let status = StatusCode::from_u16(e.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        ApiError::new(status, e.code(), e.detail())
    }
}

fn on(s: &Shared) -> Result<(), ApiError> {
    if s.pnp_nodes.limits.enabled {
        Ok(())
    } else {
        Err(NodeError::Off.into())
    }
}

/// A node can belong only to a machine of the plugin `pnp`.
fn machine_exists(s: &Shared, machine: &str) -> Result<(), ApiError> {
    if machine.is_empty() || crate::plugins::work_machine_settings(s, machine, "pnp").is_some() {
        Ok(())
    } else {
        Err(NodeError::Bad("the machine of a node is a machine of the plugin pnp".into()).into())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewNode {
    #[serde(default)]
    id: String,
    kind: String,
    name: String,
    address: String,
    #[serde(default)]
    token: String,
    #[serde(default)]
    machine: String,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeChange {
    name: Option<String>,
    address: Option<String>,
    token: Option<String>,
    machine: Option<String>,
    enabled: Option<bool>,
}

async fn list_nodes(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    on(&s)?;
    Ok(axum::Json(json!({
        "nodes": s.pnp_nodes.list_views(Instant::now()),
        "summary": s.pnp_nodes.counts(Instant::now()),
        "limits": limits(&s),
    })))
}

fn limits(s: &Shared) -> Value {
    let l = &s.pnp_nodes.limits;
    json!({
        "max": l.max, "poll_s": l.poll_s, "silence_s": l.silence().as_secs(), "timeout_s": l.timeout_s,
        "kinds": crate::pnp_nodes::KINDS,
    })
}

async fn get_node(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "machines:view")?;
    on(&s)?;
    s.pnp_nodes
        .view(&id, Instant::now())
        .map(axum::Json)
        .ok_or_else(|| NodeError::Unknown.into())
}

async fn add_node(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(body): ApiJson<NewNode>,
) -> Result<(StatusCode, axum::Json<Value>), ApiError> {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "machines:manage")?;
    on(&s)?;
    machine_exists(&s, &body.machine)?;
    let record = s.pnp_nodes.add(NodeRecord {
        id: body.id,
        kind: body.kind,
        name: body.name,
        address: body.address,
        token: body.token,
        machine: body.machine,
        enabled: body.enabled,
    })?;
    crate::api::note(
        &s,
        "pnp_node_add",
        &who,
        &[
            ("node", &record.id),
            ("kind", &record.kind),
            ("machine", &record.machine),
        ],
    );
    let view = s
        .pnp_nodes
        .view(&record.id, Instant::now())
        .unwrap_or(Value::Null);
    Ok((StatusCode::CREATED, axum::Json(view)))
}

async fn change_node(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<NodeChange>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "machines:manage")?;
    on(&s)?;
    if let Some(machine) = &body.machine {
        machine_exists(&s, machine)?;
    }
    let record = s.pnp_nodes.update(
        &id,
        body.name,
        body.address,
        body.token,
        body.machine,
        body.enabled,
    )?;
    crate::api::note(
        &s,
        "pnp_node_change",
        &who,
        &[("node", &record.id), ("machine", &record.machine)],
    );
    Ok(axum::Json(
        s.pnp_nodes
            .view(&record.id, Instant::now())
            .unwrap_or(Value::Null),
    ))
}

async fn remove_node(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "machines:manage")?;
    on(&s)?;
    s.pnp_nodes.remove(&id)?;
    crate::api::note(&s, "pnp_node_remove", &who, &[("node", &id)]);
    Ok(axum::Json(json!({ "ok": true })))
}

async fn poll_node(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "jobs:prepare")?;
    on(&s)?;
    let target = s.pnp_nodes.target(&id).ok_or(NodeError::Unknown)?;
    if !target.enabled {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "pnp_node_disabled",
            "the node is switched off; switch it on to ask it",
        ));
    }
    if let Some((before, after)) = crate::pnp_nodes::poll_node(&s, &id).await {
        if before != after {
            crate::pnp_nodes::publish_change(&s, &id, after);
        }
    }
    crate::api::note(&s, "pnp_node_poll", &who, &[("node", &id)]);
    s.pnp_nodes
        .view(&id, Instant::now())
        .map(axum::Json)
        .ok_or_else(|| NodeError::Unknown.into())
}
