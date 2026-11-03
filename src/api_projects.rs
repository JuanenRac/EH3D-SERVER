//! The interface of the projects and of the sharing, under `/api/v1/projects` and `/api/v1/artifacts/:id/shares`. A project is seen by its owner, by the administrators and by
//! the people it was shared with; for anybody else it does not exist (`404 unknown_project`). Sharing is explicit, one person at a time, and is the owner's.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::Router;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::{ApiError, ApiJson, ApiResult, Authed, Shared};
use crate::artifacts::ArtifactError;
use crate::auth::{Identity, Role};
use crate::projects::{Access, ProjectError, ProjectRecord, Share};
use crate::state::Audience;
use crate::sync::Locked;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/v1/projects", get(list_projects).post(create_project))
        .route(
            "/api/v1/projects/:id",
            get(get_project).delete(delete_project),
        )
        .route("/api/v1/projects/:id/purge", post(purge_project))
        .route("/api/v1/projects/:id/revisions", post(add_revision))
        .route("/api/v1/projects/:id/shares", post(share_project))
        .route("/api/v1/projects/:id/shares/:who", delete(unshare_project))
        .route(
            "/api/v1/artifacts/:id/shares",
            get(list_artifact_shares).post(share_artifact),
        )
        .route(
            "/api/v1/artifacts/:id/shares/:who",
            delete(unshare_artifact),
        )
}

impl From<ProjectError> for ApiError {
    fn from(e: ProjectError) -> Self {
        let status = StatusCode::from_u16(e.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        ApiError::new(status, e.code(), e.detail())
    }
}

fn manager(p: &ProjectRecord, who: &Identity) -> bool {
    who.role == Role::Admin || p.owner == who.owner()
}

/// Everybody who should hear about the project: its owner and those it is shared with.
fn audience(p: &ProjectRecord) -> Audience {
    Audience::of_all(std::iter::once(p.owner.clone()).chain(p.shares.iter().map(|s| s.who.clone())))
}

async fn list_projects(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "projects:read")?;
    let book = s.projects.book.locked();
    let list: Vec<Value> = book
        .list()
        .iter()
        .rev()
        .filter(|p| p.access_of(&who).is_some())
        .map(|p| p.view(manager(p, &who)))
        .collect();
    Ok(axum::Json(Value::Array(list)))
}

async fn get_project(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "projects:read")?;
    let book = s.projects.book.locked();
    let p = book.visible(&id, &who)?;
    Ok(axum::Json(p.view(manager(p, &who))))
}

#[derive(Deserialize)]
struct NewProject {
    name: String,
}

async fn create_project(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(b): ApiJson<NewProject>,
) -> Result<(StatusCode, axum::Json<Value>), ApiError> {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "projects:write")?;
    let p = s.projects.book.locked().create(&who, &b.name)?;
    crate::api::note(&s, "project_create", &who, &[("project", &p.id)]);
    s.publish_scoped(
        audience(&p),
        json!({ "type": "project", "project": p.view(false) }),
    );
    Ok((StatusCode::CREATED, axum::Json(p.view(true))))
}

async fn delete_project(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "projects:write")?;
    let people = {
        let mut book = s.projects.book.locked();
        let people = book.visible(&id, &who).map(audience)?;
        book.soft_delete(&who, &id)?;
        people
    };
    crate::api::note(&s, "project_delete", &who, &[("project", &id)]);
    s.publish_scoped(people, json!({ "type": "project_removed", "id": id }));
    Ok(axum::Json(json!({ "ok": true })))
}

async fn purge_project(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "projects:write")?;
    s.projects.book.locked().purge(&id)?;
    crate::api::note(&s, "project_purge", &who, &[("project", &id)]);
    Ok(axum::Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct NewRevision {
    artifact_id: String,
    /// The revision this one follows: 0 for the first.
    #[serde(default)]
    parent: u64,
}

async fn add_revision(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    ApiJson(b): ApiJson<NewRevision>,
) -> Result<(StatusCode, axum::Json<Value>), ApiError> {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "projects:write")?;
    // the artifact has to be one the caller sees (the store is looked at before the projects: the order of the locks)
    let seen = s
        .artifacts
        .store
        .locked()
        .get(&b.artifact_id)
        .is_some_and(|r| s.artifact_visible(r, &who));
    if !seen {
        return Err(ProjectError::UnknownArtifact.into());
    }
    let (revision, people) = {
        let mut book = s.projects.book.locked();
        let revision = book.add_revision(&who, &id, &b.artifact_id, b.parent)?;
        let people = book.get(&id).map(audience).ok_or(ProjectError::Unknown)?;
        (revision, people)
    };
    crate::api::note(
        &s,
        "project_revision",
        &who,
        &[("project", &id), ("artifact", &b.artifact_id)],
    );
    s.publish_scoped(
        people,
        json!({ "type": "revision", "project": id, "revision": revision }),
    );
    Ok((StatusCode::CREATED, axum::Json(json!(revision))))
}

/// Who a share is for: a user by name, or a program's token by id.
#[derive(Deserialize)]
struct ShareBody {
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    token: Option<String>,
    access: Access,
}

/// The owner key of the person named, which has to exist.
fn target_of(s: &Shared, b: &ShareBody) -> Result<String, ApiError> {
    match (&b.user, &b.token) {
        (Some(name), None) => {
            let known = s.auth.locked().users().iter().any(|(n, _, _)| n == name);
            if known {
                Ok(name.clone())
            } else {
                Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_user", ""))
            }
        }
        (None, Some(id)) => {
            let known = s.auth.locked().tokens().iter().any(|t| &t.id == id);
            if known {
                Ok(format!("token:{id}"))
            } else {
                Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_token", ""))
            }
        }
        _ => Err(ProjectError::BadShare("give either user or token").into()),
    }
}

fn sharing_on(s: &Shared) -> Result<(), ApiError> {
    if s.projects.shares_enabled {
        Ok(())
    } else {
        Err(ProjectError::SharesOff.into())
    }
}

async fn share_project(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    ApiJson(b): ApiJson<ShareBody>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "projects:write")?;
    sharing_on(&s)?;
    // who cannot see the project is told it is not there, before anything is said about the people
    s.projects.book.locked().visible(&id, &who)?;
    let target = target_of(&s, &b)?;
    let (project, people) = {
        let mut book = s.projects.book.locked();
        // someone who cannot see the project hears nothing about its sharing
        book.visible(&id, &who)?;
        book.share(&who, &id, &target, b.access)?;
        let p = book.get(&id).cloned().ok_or(ProjectError::Unknown)?;
        let people = audience(&p);
        (p, people)
    };
    crate::api::note(
        &s,
        "project_share",
        &who,
        &[("project", &id), ("with", &target)],
    );
    s.publish_scoped(
        people,
        json!({ "type": "project", "project": project.view(false) }),
    );
    Ok(axum::Json(project.view(true)))
}

async fn unshare_project(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((id, target)): Path<(String, String)>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "projects:write")?;
    let project = {
        let mut book = s.projects.book.locked();
        book.visible(&id, &who)?;
        book.unshare(&who, &id, &target)?;
        book.get(&id).cloned().ok_or(ProjectError::Unknown)?
    };
    crate::api::note(
        &s,
        "project_unshare",
        &who,
        &[("project", &id), ("with", &target)],
    );
    // the one who lost it is told, and so is whoever still sees it
    s.publish_scoped(
        Audience::of(&target),
        json!({ "type": "project_removed", "id": id }),
    );
    s.publish_scoped(
        audience(&project),
        json!({ "type": "project", "project": project.view(false) }),
    );
    Ok(axum::Json(project.view(true)))
}

// ---- the sharing of an artifact

fn artifact_manager(r: &crate::artifacts::ArtifactRecord, who: &Identity) -> bool {
    who.role == Role::Admin || r.owner == who.owner()
}

async fn list_artifact_shares(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "artifacts:read")?;
    let store = s.artifacts.store.locked();
    let r = store
        .get(&id)
        .filter(|r| s.artifact_visible(r, &who))
        .ok_or(ArtifactError::Unknown)?;
    if !artifact_manager(r, &who) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden", ""));
    }
    Ok(axum::Json(json!({ "shares": r.shares })))
}

async fn share_artifact(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    ApiJson(b): ApiJson<ShareBody>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "artifacts:write")?;
    sharing_on(&s)?;
    if b.access != Access::Read {
        return Err(ProjectError::BadShare("an artifact is shared for reading only").into());
    }
    let target = target_of(&s, &b)?;
    let mut store = s.artifacts.store.locked();
    let seen = store.get(&id).is_some_and(|r| s.artifact_visible(r, &who));
    if !seen {
        return Err(ArtifactError::Unknown.into());
    }
    let owner = store.get(&id).map(|r| artifact_manager(r, &who));
    if owner != Some(true) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden", ""));
    }
    let record = store.get_mut(&id).ok_or(ArtifactError::Unknown)?;
    if target == record.owner {
        return Err(ProjectError::BadShare("the owner has it already").into());
    }
    if !record.shares.iter().any(|x| x.who == target) {
        record.shares.push(Share {
            who: target.clone(),
            access: Access::Read,
        });
    }
    store.save()?;
    drop(store);
    crate::api::note(
        &s,
        "artifact_share",
        &who,
        &[("artifact", &id), ("with", &target)],
    );
    Ok(axum::Json(json!({ "ok": true })))
}

async fn unshare_artifact(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((id, target)): Path<(String, String)>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "artifacts:write")?;
    let mut store = s.artifacts.store.locked();
    let seen = store.get(&id).is_some_and(|r| s.artifact_visible(r, &who));
    if !seen {
        return Err(ArtifactError::Unknown.into());
    }
    if store.get(&id).map(|r| artifact_manager(r, &who)) != Some(true) {
        return Err(ApiError::new(StatusCode::FORBIDDEN, "forbidden", ""));
    }
    let record = store.get_mut(&id).ok_or(ArtifactError::Unknown)?;
    let before = record.shares.len();
    record.shares.retain(|x| x.who != target);
    if record.shares.len() == before {
        return Err(ArtifactError::Unknown.into());
    }
    store.save()?;
    drop(store);
    crate::api::note(
        &s,
        "artifact_unshare",
        &who,
        &[("artifact", &id), ("with", &target)],
    );
    Ok(axum::Json(json!({ "ok": true })))
}
