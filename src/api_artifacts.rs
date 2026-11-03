//! The interface of the artifacts and the uploads, under `/api/v1/artifacts`, `/api/v1/blobs` and `/api/v1/uploads`. An artifact is seen by its owner and the administrators;
//! for anybody else it does not exist (`404 unknown_artifact`), and the question "is this blob here?" is answered only about bytes the caller can see.

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, head, post, put};
use axum::Router;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::{ApiError, ApiJson, ApiResult, Authed, Shared};
use crate::artifacts::{ArtifactError, NewArtifact, Retention};
use crate::auth::Role;
use crate::sync::Locked;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route(
            "/api/v1/artifacts",
            get(list_artifacts).post(create_artifact),
        )
        .route("/api/v1/artifacts/verify", post(verify_store))
        .route(
            "/api/v1/artifacts/:id",
            get(get_artifact).delete(delete_artifact),
        )
        .route("/api/v1/artifacts/:id/meta", get(get_meta))
        .route("/api/v1/artifacts/:id/retention", put(set_retention))
        .route("/api/v1/blobs/:sha", head(blob_exists))
        .route("/api/v1/uploads", post(begin_upload))
        .route("/api/v1/uploads/:id", put(put_part).delete(abandon_upload))
        .route("/api/v1/uploads/:id/complete", post(complete_upload))
}

impl From<ArtifactError> for ApiError {
    fn from(e: ArtifactError) -> Self {
        let status = StatusCode::from_u16(e.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        ApiError::new(status, e.code(), e.detail())
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ArtifactError> + Send + 'static,
) -> Result<T, ApiError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", ""))?
        .map_err(Into::into)
}

#[derive(Deserialize)]
struct CreateArtifact {
    #[serde(flatten)]
    meta: NewArtifact,
    /// The bytes, in base64 (for small files; large ones go by `/uploads`).
    data_base64: String,
}

async fn create_artifact(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(body): ApiJson<CreateArtifact>,
) -> Result<(StatusCode, axum::Json<Value>), ApiError> {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "artifacts:write")?;
    let state = s.clone();
    let by = who.clone();
    let record = blocking(move || {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(body.data_base64.as_bytes())
            .map_err(|_| ArtifactError::Bad("data_base64 is not base64".into()))?;
        state.add_artifact(&by, body.meta, &bytes)
    })
    .await?;
    crate::api::note(
        &s,
        "artifact_create",
        &who,
        &[
            ("artifact", &record.artifact_id),
            ("blob", &record.blob_sha256),
        ],
    );
    Ok((
        StatusCode::CREATED,
        axum::Json(json!({
            "artifact_id": record.artifact_id, "blob_sha256": record.blob_sha256, "size": record.size,
        })),
    ))
}

#[derive(Deserialize)]
struct ArtifactFilter {
    format: Option<String>,
    result_type: Option<String>,
}

async fn list_artifacts(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Query(f): Query<ArtifactFilter>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "artifacts:read")?;
    let store = s.artifacts.store.locked();
    let list: Vec<Value> = store
        .records()
        .iter()
        .rev()
        .filter(|r| s.artifact_visible(r, &who))
        .filter(|r| f.format.as_deref().is_none_or(|x| r.format == x))
        .filter(|r| {
            f.result_type
                .as_deref()
                .is_none_or(|x| r.result_type.as_deref() == Some(x))
        })
        .map(|r| r.view())
        .collect();
    Ok(axum::Json(Value::Array(list)))
}

async fn get_meta(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "artifacts:read")?;
    let store = s.artifacts.store.locked();
    store
        .get(&id)
        .filter(|r| s.artifact_visible(r, &who))
        .map(|r| axum::Json(r.view()))
        .ok_or_else(|| ArtifactError::Unknown.into())
}

/// The bytes of an artifact, streamed from the blob.
async fn get_artifact(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "artifacts:read")?;
    let (path, record) = {
        let store = s.artifacts.store.locked();
        let record = store
            .get(&id)
            .filter(|r| s.artifact_visible(r, &who))
            .cloned()
            .ok_or(ArtifactError::Unknown)?;
        (store.blob_path(&record.blob_sha256), record)
    };
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::from(ArtifactError::Damaged))?;
    let stream = tokio_util::io::ReaderStream::new(file);
    let mut response = Response::new(Body::from_stream(stream));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(record.size));
    if let Ok(v) = HeaderValue::from_str(&record.blob_sha256) {
        headers.insert("x-eh3d-blob-sha256", v);
    }
    if let Ok(v) = HeaderValue::from_str(&record.format) {
        headers.insert("x-eh3d-format", v);
    }
    Ok(response)
}

async fn delete_artifact(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "artifacts:write")?;
    let state = s.clone();
    let target = id.clone();
    blocking(move || state.delete_artifact(&target)).await?;
    crate::api::note(&s, "artifact_delete", &who, &[("artifact", &id)]);
    Ok(axum::Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct SetRetention {
    retention: Retention,
    /// For `keep_until`: seconds since 1970.
    #[serde(default)]
    until: Option<u64>,
}

async fn set_retention(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    ApiJson(b): ApiJson<SetRetention>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "artifacts:write")?;
    if (b.retention == Retention::KeepUntil) != b.until.is_some() {
        return Err(
            ArtifactError::Bad("until goes with keep_until and with nothing else".into()).into(),
        );
    }
    let mut store = s.artifacts.store.locked();
    let record = store.get_mut(&id).ok_or(ArtifactError::Unknown)?;
    record.retention = b.retention;
    record.keep_until = b.until;
    let view = record.view();
    drop(store);
    crate::api::note(&s, "artifact_retention", &who, &[("artifact", &id)]);
    Ok(axum::Json(view))
}

/// Whether bytes with this hash are held by an artifact the caller can see. The answer is the same for a hash nobody holds and for one only others hold.
async fn blob_exists(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(sha): Path<String>,
) -> StatusCode {
    if !who.allows("artifacts:read") {
        return StatusCode::FORBIDDEN;
    }
    let store = s.artifacts.store.locked();
    let seen = crate::artifacts::is_sha256(&sha)
        && store.blob_exists(&sha)
        && store
            .records()
            .iter()
            .any(|r| r.blob_sha256 == sha && s.artifact_visible(r, &who));
    if seen {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

/// Reads every blob again against its hash (an administrator, after restoring a backup).
async fn verify_store(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "artifacts:read")?;
    let state = s.clone();
    let bad = blocking(move || Ok(state.artifacts.store.locked().verify())).await?;
    let list: Vec<Value> = bad
        .into_iter()
        .map(|(sha, why)| json!({ "blob_sha256": sha, "problem": why }))
        .collect();
    Ok(axum::Json(
        json!({ "ok": list.is_empty(), "problems": list }),
    ))
}

#[derive(Deserialize)]
struct BeginUpload {
    size: u64,
    sha256: String,
}

async fn begin_upload(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    ApiJson(b): ApiJson<BeginUpload>,
) -> Result<(StatusCode, axum::Json<Value>), ApiError> {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "artifacts:write")?;
    let state = s.clone();
    let by = who.clone();
    let id = blocking(move || state.begin_upload(&by, b.size, &b.sha256)).await?;
    crate::api::note(&s, "upload_begin", &who, &[("upload", &id)]);
    Ok((StatusCode::CREATED, axum::Json(json!({ "id": id }))))
}

#[derive(Deserialize)]
struct Offset {
    offset: u64,
}

async fn put_part(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    Query(q): Query<Offset>,
    body: Bytes,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "artifacts:write")?;
    let state = s.clone();
    let by = who.clone();
    let upload = blocking(move || state.put_part(&by, &id, q.offset, &body)).await?;
    Ok(axum::Json(json!({
        "id": upload.id, "size": upload.size, "received": upload.ranges.iter().map(|(a, b)| b - a).sum::<u64>(),
        "ranges": upload.ranges,
    })))
}

async fn abandon_upload(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "artifacts:write")?;
    let state = s.clone();
    let by = who.clone();
    blocking(move || state.abandon_upload_of(&by, &id)).await?;
    Ok(axum::Json(json!({ "ok": true })))
}

async fn complete_upload(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(id): Path<String>,
    ApiJson(meta): ApiJson<NewArtifact>,
) -> Result<(StatusCode, axum::Json<Value>), ApiError> {
    crate::api::need(&who, Role::Operator)?;
    crate::api::need_scope(&who, "artifacts:write")?;
    let state = s.clone();
    let by = who.clone();
    let target = id.clone();
    let record = blocking(move || state.complete_upload(&by, &target, meta)).await?;
    crate::api::note(
        &s,
        "artifact_create",
        &who,
        &[
            ("artifact", &record.artifact_id),
            ("blob", &record.blob_sha256),
            ("upload", &id),
        ],
    );
    Ok((
        StatusCode::CREATED,
        axum::Json(json!({
            "artifact_id": record.artifact_id, "blob_sha256": record.blob_sha256, "size": record.size,
        })),
    ))
}
