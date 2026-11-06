//! The interface of the project store, under `/api/v1/apps/:app/projects`: the projects and the files of every application of EH3D, with versions, `If-Match`, editing
//! locks and the import and export of the `.eh3dproj` container. A project is seen by its owner and the administrators (and by everybody with access to the store when
//! `project_shared` is on); for anybody else it does not exist (`404 unknown_project`). The rules are in `src/pstore.rs` and `docs/PROJECT_STORE.md`.

use axum::body::{Body, Bytes};
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::{ApiError, ApiJson, ApiResult, Authed, Shared};
use crate::auth::Role;
use crate::config::ServerConfig;
use crate::jobs::now;
use crate::pstore::{read_container, read_verified, write_container, PError, ProjectMeta};
use crate::sync::Locked;

pub fn routes(cfg: &ServerConfig) -> Router<Shared> {
    // a file is at most project_max_file_mb, a container at most project_max_mb (a little more for the framing of the request)
    let file_limit = (cfg.project_max_file_mb as usize + 1) * 1024 * 1024;
    let container_limit = (cfg.project_max_mb as usize + 1) * 1024 * 1024;
    Router::new()
        .route(
            "/api/v1/apps/:app/projects",
            get(list_projects).post(create_project),
        )
        .route(
            "/api/v1/apps/:app/projects/:id",
            get(get_project).put(rename_project).delete(hide_project),
        )
        .route("/api/v1/apps/:app/quota", get(app_quota))
        .route("/api/v1/apps/:app/projects/:id/purge", post(purge_project))
        .route("/api/v1/apps/:app/projects/:id/files", get(list_files))
        .route(
            "/api/v1/apps/:app/projects/:id/files/content",
            get(read_file)
                .put(write_file)
                .delete(remove_file)
                .layer(DefaultBodyLimit::max(file_limit)),
        )
        .route(
            "/api/v1/apps/:app/projects/:id/files/versions",
            get(file_versions),
        )
        .route(
            "/api/v1/apps/:app/projects/:id/lock",
            get(read_lock).put(take_lock).delete(release_lock),
        )
        .route(
            "/api/v1/apps/:app/projects/:id/import",
            post(import_container).layer(DefaultBodyLimit::max(container_limit)),
        )
        .route(
            "/api/v1/apps/:app/projects/:id/export",
            get(export_container),
        )
}

impl From<PError> for ApiError {
    fn from(e: PError) -> Self {
        let status = StatusCode::from_u16(e.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        ApiError::new(status, e.code(), e.detail())
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, PError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))?
        .map_err(Into::into)
}

/// The application of the path has to be one the server accepts.
fn app_of(s: &Shared, app: &str) -> Result<(), ApiError> {
    if crate::apps::allowed(&s.cfg, app) {
        Ok(())
    } else {
        Err(PError::BadApp.into())
    }
}

/// The bytes of the body, or the refusal in the shape of every other.
fn bytes_of(body: Result<Bytes, BytesRejection>) -> Result<Bytes, ApiError> {
    body.map_err(|r| {
        let status = r.status();
        let code = if status == StatusCode::PAYLOAD_TOO_LARGE {
            "too_big"
        } else {
            "bad_request"
        };
        ApiError::new(status, code, r.body_text())
    })
}

fn if_match(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_string())
}

fn needed(name: Option<String>) -> Result<String, ApiError> {
    name.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "give the name of the file in ?name=",
        )
    })
}

/// A project as the API shows it, with its ETag in the header.
fn with_etag(status: StatusCode, m: &ProjectMeta, body: Value) -> Response {
    (status, [(header::ETAG, m.etag())], Json(body)).into_response()
}

#[derive(Deserialize)]
struct ListQuery {
    q: Option<String>,
    limit: Option<usize>,
    offset: Option<usize>,
}

async fn list_projects(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(app): Path<String>,
    Query(q): Query<ListQuery>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "projects:read")?;
    app_of(&s, &app)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let offset = q.offset.unwrap_or(0);
    let at = now();
    let book = s.pstore.book.locked();
    let (total, page) = book.list(&app, &who, q.q.as_deref().unwrap_or(""), offset, limit);
    let items: Vec<Value> = page.iter().map(|m| m.summary(at)).collect();
    Ok(Json(
        json!({ "total": total, "offset": offset, "limit": limit, "items": items }),
    ))
}

/// The limits of the store (`project_max_mb`, `project_max_file_mb`, `project_max_files`, `project_max_projects`) and what the application uses of them, so that a client can
/// show the limit and not only the use. The projects listed are the ones the caller sees.
async fn app_quota(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(app): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "projects:read")?;
    app_of(&s, &app)?;
    let book = s.pstore.book.locked();
    Ok(Json(book.usage(&app, &who, 200)))
}

#[derive(Deserialize)]
struct NameBody {
    name: String,
}

async fn create_project(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(app): Path<String>,
    ApiJson(b): ApiJson<NameBody>,
) -> Result<Response, ApiError> {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "projects:write")?;
    app_of(&s, &app)?;
    let at = now();
    let meta = s.pstore.book.locked().create(&app, &who, &b.name, at)?;
    crate::api::note(
        &s,
        "pstore_project_create",
        &who,
        &[("app", &app), ("project", &meta.id)],
    );
    s.announce_project("project.created", &meta);
    Ok(with_etag(StatusCode::CREATED, &meta, meta.view(at)))
}

async fn get_project(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((app, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "projects:read")?;
    app_of(&s, &app)?;
    let book = s.pstore.book.locked();
    let m = book.visible(&app, &id, &who)?;
    Ok(with_etag(StatusCode::OK, m, m.view(now())))
}

async fn rename_project(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((app, id)): Path<(String, String)>,
    headers: HeaderMap,
    ApiJson(b): ApiJson<NameBody>,
) -> Result<Response, ApiError> {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "projects:write")?;
    app_of(&s, &app)?;
    let at = now();
    let meta = s.pstore.book.locked().rename(
        &app,
        &id,
        &who,
        &b.name,
        if_match(&headers).as_deref(),
        at,
    )?;
    crate::api::note(
        &s,
        "pstore_project_rename",
        &who,
        &[("app", &app), ("project", &id)],
    );
    s.announce_project("project.changed", &meta);
    Ok(with_etag(StatusCode::OK, &meta, meta.view(at)))
}

async fn hide_project(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((app, id)): Path<(String, String)>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "projects:write")?;
    app_of(&s, &app)?;
    let meta = {
        let mut book = s.pstore.book.locked();
        let meta = book.visible(&app, &id, &who)?.clone();
        book.soft_delete(&app, &id, &who, now())?;
        meta
    };
    crate::api::note(
        &s,
        "pstore_project_delete",
        &who,
        &[("app", &app), ("project", &id)],
    );
    s.announce_project("project.removed", &meta);
    Ok(Json(json!({ "ok": true })))
}

async fn purge_project(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((app, id)): Path<(String, String)>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "projects:write")?;
    app_of(&s, &app)?;
    s.pstore.book.locked().purge(&app, &id, &who)?;
    crate::api::note(
        &s,
        "pstore_project_purge",
        &who,
        &[("app", &app), ("project", &id)],
    );
    Ok(Json(json!({ "ok": true })))
}

async fn list_files(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((app, id)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "projects:read")?;
    app_of(&s, &app)?;
    let book = s.pstore.book.locked();
    let m = book.visible(&app, &id, &who)?;
    Ok(with_etag(
        StatusCode::OK,
        m,
        json!({ "files": m.file_list() }),
    ))
}

#[derive(Deserialize)]
struct FileQuery {
    name: Option<String>,
    version: Option<u64>,
    comment: Option<String>,
}

fn file_response(status: StatusCode, version: &crate::pstore::Version, body: Value) -> Response {
    (
        status,
        [(header::ETAG, format!("\"{}\"", version.sha256))],
        Json(body),
    )
        .into_response()
}

async fn read_file(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((app, id)): Path<(String, String)>,
    Query(q): Query<FileQuery>,
) -> Result<Response, ApiError> {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "projects:read")?;
    app_of(&s, &app)?;
    let name = needed(q.name)?;
    let (version, path) = s
        .pstore
        .book
        .locked()
        .locate(&app, &id, &who, &name, q.version)?;
    let checked = version.clone();
    // the bytes are read and checked against their hash every time
    let bytes = blocking(move || read_verified(&path, &checked)).await?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::ETAG, format!("\"{}\"", version.sha256))
        .header("x-eh3d-version", version.n.to_string())
        .body(Body::from(bytes))
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))
}

async fn write_file(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((app, id)): Path<(String, String)>,
    Query(q): Query<FileQuery>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Response, ApiError> {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "projects:write")?;
    app_of(&s, &app)?;
    let name = needed(q.name)?;
    let bytes = bytes_of(body)?;
    let tag = if_match(&headers);
    let comment = q.comment.unwrap_or_default();
    let outcome = {
        let state = s.clone();
        let who = who.clone();
        let (app, id, name) = (app.clone(), id.clone(), name.clone());
        blocking(move || {
            let mut book = state.pstore.book.locked();
            book.put_file(
                &app,
                &id,
                &who,
                &name,
                &bytes,
                tag.as_deref(),
                &comment,
                now(),
            )
        })
        .await?
    };
    let version = outcome.version.n.to_string();
    if outcome.created {
        crate::api::note(
            &s,
            "pstore_file_write",
            &who,
            &[
                ("app", &app),
                ("project", &id),
                ("file", &name),
                ("version", &version),
            ],
        );
        s.announce_file("project.file", &app, &id, &name, &outcome.version, &who);
    }
    let status = if outcome.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    let mut view = outcome.version.view();
    view["name"] = json!(name);
    view["unchanged"] = json!(!outcome.created);
    Ok(file_response(status, &outcome.version, view))
}

async fn remove_file(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((app, id)): Path<(String, String)>,
    Query(q): Query<FileQuery>,
    headers: HeaderMap,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "projects:write")?;
    app_of(&s, &app)?;
    let name = needed(q.name)?;
    s.pstore.book.locked().delete_file(
        &app,
        &id,
        &who,
        &name,
        if_match(&headers).as_deref(),
        now(),
    )?;
    crate::api::note(
        &s,
        "pstore_file_delete",
        &who,
        &[("app", &app), ("project", &id), ("file", &name)],
    );
    s.announce_removed_file(&app, &id, &name, &who);
    Ok(Json(json!({ "ok": true })))
}

async fn file_versions(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((app, id)): Path<(String, String)>,
    Query(q): Query<FileQuery>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "projects:read")?;
    app_of(&s, &app)?;
    let name = needed(q.name)?;
    let list = s.pstore.book.locked().versions(&app, &id, &who, &name)?;
    let versions: Vec<Value> = list.iter().map(|v| v.view()).collect();
    Ok(Json(json!({ "name": name, "versions": versions })))
}

async fn read_lock(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((app, id)): Path<(String, String)>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "projects:read")?;
    app_of(&s, &app)?;
    let book = s.pstore.book.locked();
    let m = book.visible(&app, &id, &who)?;
    Ok(Json(
        json!({ "lock": m.live_lock(now()).map(crate::pstore::Lock::view) }),
    ))
}

#[derive(Deserialize)]
struct LockQuery {
    ttl_s: Option<u64>,
    force: Option<bool>,
}

async fn take_lock(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((app, id)): Path<(String, String)>,
    Query(q): Query<LockQuery>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "projects:write")?;
    app_of(&s, &app)?;
    let lock = s
        .pstore
        .book
        .locked()
        .take_lock(&app, &id, &who, q.ttl_s, now())?;
    crate::api::note(&s, "pstore_lock", &who, &[("app", &app), ("project", &id)]);
    s.announce_lock(&app, &id, true, &who);
    Ok(Json(json!({ "lock": lock.view() })))
}

async fn release_lock(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((app, id)): Path<(String, String)>,
    Query(q): Query<LockQuery>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "projects:write")?;
    app_of(&s, &app)?;
    s.pstore
        .book
        .locked()
        .release_lock(&app, &id, &who, q.force.unwrap_or(false), now())?;
    crate::api::note(
        &s,
        "pstore_unlock",
        &who,
        &[("app", &app), ("project", &id)],
    );
    s.announce_lock(&app, &id, false, &who);
    Ok(Json(json!({ "ok": true })))
}

async fn import_container(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((app, id)): Path<(String, String)>,
    body: Result<Bytes, BytesRejection>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "projects:write")?;
    app_of(&s, &app)?;
    let bytes = bytes_of(body)?;
    let (max_file, max_project) = {
        let book = s.pstore.book.locked();
        // the project has to be there before anybody spends time on the container
        book.visible(&app, &id, &who)?;
        (book.limits.max_file, book.limits.max_project)
    };
    let entries = blocking(move || read_container(&bytes, max_file, max_project)).await?;
    let state = s.clone();
    let (a, i, w) = (app.clone(), id.clone(), who.clone());
    let done = blocking(move || {
        let mut book = state.pstore.book.locked();
        book.import(&a, &i, &w, entries, now())
    })
    .await?;
    let changed = done.iter().filter(|(_, _, created)| *created).count();
    crate::api::note(
        &s,
        "pstore_import",
        &who,
        &[
            ("app", &app),
            ("project", &id),
            ("files", &changed.to_string()),
        ],
    );
    let files: Vec<Value> = done
        .iter()
        .map(|(name, v, created)| json!({ "name": name, "version": v.n, "unchanged": !created }))
        .collect();
    Ok(Json(
        json!({ "imported": changed, "unchanged": done.len() - changed, "files": files }),
    ))
}

#[derive(Deserialize)]
struct ExportQuery {
    spec: Option<String>,
}

/// A name that is safe in the header of a download.
fn download_name(name: &str) -> String {
    let plain: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    if plain.is_empty() {
        "project".into()
    } else {
        plain
    }
}

async fn export_container(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((app, id)): Path<(String, String)>,
    Query(q): Query<ExportQuery>,
) -> Result<Response, ApiError> {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "projects:read")?;
    app_of(&s, &app)?;
    let (name, files, scratch) = {
        let book = s.pstore.book.locked();
        let name = book.visible(&app, &id, &who)?.name.clone();
        (name, book.snapshot(&app, &id, &who)?, book.scratch())
    };
    let spec = q.spec;
    let bytes = blocking(move || {
        let mut entries = Vec::new();
        for (file, version, path) in files {
            entries.push((file, read_verified(&path, &version)?));
        }
        write_container(&entries, spec.as_deref(), &scratch)
    })
    .await?;
    crate::api::note(
        &s,
        "pstore_export",
        &who,
        &[("app", &app), ("project", &id)],
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}.eh3dproj\"", download_name(&name)),
        )
        .body(Body::from(bytes))
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))
}
