//! The interface of the shared catalogs, under `/api/v1/catalogs`: everybody who is logged in reads (search, filters, sorting, pages, `ETag` and `If-None-Match`), an
//! administrator writes. The rules are in `src/catalogs.rs` and `docs/CATALOGS.md`.

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::api::{ApiError, ApiResult, Authed, Shared};
use crate::auth::Role;
use crate::catalogs::{search, CError, Query as CatalogQuery};
use crate::config::ServerConfig;
use crate::feed::{FeedEvent, Visibility};
use crate::jobs::now;
use crate::sync::Locked;

pub fn routes(cfg: &ServerConfig) -> Router<Shared> {
    let limit = (cfg.catalog_max_mb as usize + 1) * 1024 * 1024;
    Router::new()
        .route("/api/v1/catalogs", get(list_catalogs))
        .route(
            "/api/v1/catalogs/:catalog",
            get(read_catalog)
                .put(replace_catalog)
                .delete(clear_catalog)
                .layer(DefaultBodyLimit::max(limit)),
        )
        .route("/api/v1/catalogs/:catalog/items/:item", get(read_item))
}

impl From<CError> for ApiError {
    fn from(e: CError) -> Self {
        let status = StatusCode::from_u16(e.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        ApiError::new(status, e.code(), e.detail())
    }
}

/// Whether the `If-None-Match` of a request names the tag (`*`, a list, weak tags).
fn not_modified(headers: &HeaderMap, etag: &str) -> bool {
    let Some(value) = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    value.split(',').any(|part| {
        let part = part.trim();
        part == "*" || part.strip_prefix("W/").unwrap_or(part) == etag
    })
}

fn with_tag(status: StatusCode, etag: String, body: Value) -> Response {
    (status, [(header::ETAG, etag)], Json(body)).into_response()
}

async fn list_catalogs(State(s): State<Shared>, Authed(who, _): Authed) -> ApiResult {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "catalog:read")?;
    let book = s.catalogs.book.locked();
    Ok(Json(json!({ "catalogs": book.list() })))
}

#[derive(Deserialize)]
struct PageQuery {
    q: Option<String>,
    /// `field:value` conditions separated by commas.
    filter: Option<String>,
    sort: Option<String>,
    desc: Option<bool>,
    limit: Option<usize>,
    offset: Option<usize>,
}

/// The conditions of `?filter=kind:drill,dia:0.8` (a value may start with `~`).
fn filters_of(text: &str) -> Result<Vec<(String, String)>, ApiError> {
    let mut out = Vec::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let Some((field, value)) = part.split_once(':') else {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "filter is field:value, several separated by commas",
            ));
        };
        out.push((field.trim().to_string(), value.trim().to_string()));
    }
    Ok(out)
}

async fn read_catalog(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(catalog): Path<String>,
    headers: HeaderMap,
    Query(q): Query<PageQuery>,
) -> Result<Response, ApiError> {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "catalog:read")?;
    let filters = match &q.filter {
        Some(text) => filters_of(text)?,
        None => Vec::new(),
    };
    let book = s.catalogs.book.locked();
    let (def, ds) = book.dataset(&catalog)?;
    let etag = ds.etag(def.name);
    if not_modified(&headers, &etag) {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }
    let query = CatalogQuery {
        q: q.q.unwrap_or_default(),
        filters,
        sort: q.sort.filter(|f| !f.is_empty()),
        descending: q.desc.unwrap_or(false),
        offset: q.offset.unwrap_or(0),
        limit: q.limit.unwrap_or(50).clamp(1, book.page_max),
    };
    let (total, page) = search(ds, &query);
    Ok(with_tag(
        StatusCode::OK,
        etag.clone(),
        json!({
            "catalog": def.name, "version": ds.version, "etag": etag, "total": total,
            "offset": query.offset, "limit": query.limit, "items": page,
        }),
    ))
}

async fn read_item(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path((catalog, item)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    crate::api::need(&who, Role::Viewer)?;
    crate::api::need_scope(&who, "catalog:read")?;
    let book = s.catalogs.book.locked();
    let (def, ds) = book.dataset(&catalog)?;
    let found = ds.get(&item).ok_or(CError::UnknownItem)?;
    Ok(with_tag(StatusCode::OK, ds.etag(def.name), found.clone()))
}

fn is_csv(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.trim().to_lowercase().starts_with("text/csv"))
}

async fn replace_catalog(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(catalog): Path<String>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "catalog:write")?;
    let bytes = body.map_err(|r| {
        let status = r.status();
        let code = if status == StatusCode::PAYLOAD_TOO_LARGE {
            "too_big"
        } else {
            "bad_request"
        };
        ApiError::new(status, code, r.body_text())
    })?;
    let csv = is_csv(&headers);
    let summary = s
        .catalogs
        .book
        .locked()
        .replace(&catalog, &bytes, csv, &who.name, now())?;
    crate::api::note(
        &s,
        "catalog_replace",
        &who,
        &[
            ("catalog", &catalog),
            ("version", &summary["version"].to_string()),
        ],
    );
    announce(&s, &catalog, &summary);
    Ok(Json(summary))
}

async fn clear_catalog(
    State(s): State<Shared>,
    Authed(who, _): Authed,
    Path(catalog): Path<String>,
) -> ApiResult {
    crate::api::need(&who, Role::Admin)?;
    crate::api::need_scope(&who, "catalog:write")?;
    let summary = s.catalogs.book.locked().clear(&catalog, &who.name, now())?;
    crate::api::note(&s, "catalog_clear", &who, &[("catalog", &catalog)]);
    announce(&s, &catalog, &summary);
    Ok(Json(summary))
}

fn announce(s: &Shared, catalog: &str, summary: &Value) {
    s.feed.publish(FeedEvent::new(
        "catalog.changed",
        Visibility::Everyone,
        json!({ "catalog": catalog, "version": summary["version"], "items": summary["items"] }),
    ));
}
