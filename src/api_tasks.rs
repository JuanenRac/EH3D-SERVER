//! The interface of the computing tasks, under `/api/v1/tasks`. A task is work for the computer (checking a design, making the files for a factory): it never
//! moves a machine, and the jobs of the machines (`/api/v1/jobs`) are a family of their own. Who sees what: the owner of a task and the administrators; for anybody
//! else a task is as if it did not exist (`404 unknown_task`).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::{ApiError, ApiJson, ApiResult, Authed, Shared};
use crate::auth::Role;
use crate::sync::Locked;
use crate::tasks::{NewTask, TaskError};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/v1/tasks", get(list_tasks).post(create_task))
        .route("/api/v1/tasks/kinds", get(task_kinds))
        .route("/api/v1/tasks/:id", get(get_task).delete(delete_task))
        .route("/api/v1/tasks/:id/cancel", post(cancel_task))
}

impl From<TaskError> for ApiError {
    fn from(e: TaskError) -> Self {
        let status = StatusCode::from_u16(e.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        ApiError::new(status, e.code(), e.detail())
    }
}

fn on(s: &Shared) -> Result<(), ApiError> {
    if s.tasks.limits.enabled {
        Ok(())
    } else {
        Err(TaskError::Off.into())
    }
}

#[derive(Deserialize)]
struct TaskFilter {
    state: Option<String>,
    kind: Option<String>,
}

async fn list_tasks(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Query(f): Query<TaskFilter>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    on(&s)?;
    let book = s.tasks.book.locked();
    let list: Vec<Value> = book
        .list()
        .iter()
        .rev()
        .filter(|t| t.visible_to(&who))
        .filter(|t| f.state.as_deref().is_none_or(|st| t.state.name() == st))
        .filter(|t| f.kind.as_deref().is_none_or(|k| t.kind == k))
        .map(|t| t.view())
        .collect();
    Ok(axum::Json(Value::Array(list)))
}

async fn task_kinds(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    on(&s)?;
    Ok(axum::Json(json!({ "kinds": s.tasks.engines.kinds() })))
}

async fn get_task(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    on(&s)?;
    let book = s.tasks.book.locked();
    book.get(&id)
        .filter(|t| t.visible_to(&who))
        .map(|t| axum::Json(t.view()))
        .ok_or_else(|| TaskError::UnknownTask.into())
}

async fn create_task(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(body): ApiJson<NewTask>,
) -> Result<(StatusCode, axum::Json<Value>), ApiError> {
    crate::api::need(&who, Role::Operator)?;
    on(&s)?;
    let state = s.clone();
    let by = who.clone();
    let made = tokio::task::spawn_blocking(move || state.submit_task(&by, body))
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))??;
    let (task, replayed) = made;
    if !replayed {
        crate::api::note(
            &s,
            "task_create",
            &who,
            &[("task", &task.id), ("kind", &task.kind)],
        );
    }
    Ok((StatusCode::CREATED, axum::Json(task.view())))
}

async fn cancel_task(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    on(&s)?;
    let (task, accepted) = s.cancel_task(&who, &id)?;
    if accepted {
        crate::api::note(&s, "task_cancel", &who, &[("task", &id)]);
    }
    Ok(axum::Json(
        json!({ "accepted": accepted, "task": task.view() }),
    ))
}

async fn delete_task(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    on(&s)?;
    s.delete_task(&who, &id)?;
    crate::api::note(&s, "task_delete", &who, &[("task", &id)]);
    Ok(axum::Json(json!({ "ok": true })))
}
