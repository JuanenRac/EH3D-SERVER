//! The shared catalogs end to end through the real router: who reads and who writes, the search, the filters, the pages, the tag, and what an upload has to be.

mod common;

use axum::http::{Method, StatusCode};
use common::{admin, call, enc, scoped_token, send, server, server_with, user};
use serde_json::json;

async fn upload(
    s: &common::Server,
    token: &str,
    catalog: &str,
    body: &str,
    content_type: &str,
) -> common::Reply {
    send(
        s,
        Method::PUT,
        &format!("/api/v1/catalogs/{catalog}"),
        Some(token),
        &[],
        Some(body.as_bytes().to_vec()),
        Some(content_type),
    )
    .await
}

const TOOLS: &str = r#"{"tools": [
    {"id": "d08", "kind": "drill", "dia_mm": 0.8, "tags": ["fr4", "hss"]},
    {"id": "d10", "kind": "drill", "dia_mm": 1.0, "tags": ["alu"]},
    {"id": "m20", "kind": "mill", "dia_mm": 2.0, "tags": ["fr4"]}
]}"#;

#[tokio::test]
async fn every_catalog_is_listed_and_the_materials_are_built_in() {
    let s = server("cat-list");
    let boss = admin(&s).await;
    let viewer = user(&s, &boss, "view1", "viewer").await;
    let (status, list) = call(&s, Method::GET, "/api/v1/catalogs", Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = list["catalogs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "materials",
            "tools",
            "machine-profiles",
            "footprints",
            "symbols",
            "components"
        ]
    );
    assert_eq!(list["catalogs"][0]["builtin"], true);
    assert!(list["catalogs"][0]["items"].as_u64().unwrap() > 0);
    assert_eq!(list["catalogs"][1]["items"], 0);
    assert_eq!(list["catalogs"][1]["version"], 0);
    let (status, _) = call(&s, Method::GET, "/api/v1/catalogs", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_materials_are_searched_read_and_never_written() {
    let s = server("cat-materials");
    let boss = admin(&s).await;
    let (_, page) = call(
        &s,
        Method::GET,
        "/api/v1/catalogs/materials?limit=3&sort=id",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(page["limit"], 3);
    assert_eq!(page["items"].as_array().unwrap().len(), 3);
    let first = page["items"][0]["id"].as_str().unwrap().to_string();
    // the id of a material has a slash (`manufacturer/name`): it goes in the address as %2F
    let item = send(
        &s,
        Method::GET,
        &format!("/api/v1/catalogs/materials/items/{}", enc(&first)),
        Some(&boss),
        &[],
        None,
        None,
    )
    .await;
    assert_eq!(item.status, StatusCode::OK, "{}", item.json());
    assert_eq!(item.json()["id"], first.as_str());
    // the text finds it in any letter case
    let (_, found) = call(
        &s,
        Method::GET,
        &format!(
            "/api/v1/catalogs/materials?q={}",
            enc(&first.to_uppercase())
        ),
        Some(&boss),
        None,
    )
    .await;
    assert!(found["total"].as_u64().unwrap() >= 1);
    let (status, body) = call(
        &s,
        Method::GET,
        "/api/v1/catalogs/materials/items/nothing-like-this",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "unknown_item");
    // not written, not even by an administrator
    let r = upload(&s, &boss, "materials", "[]", "application/json").await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.error(), "catalog_readonly");
}

#[tokio::test]
async fn only_an_administrator_writes_and_the_scope_narrows_a_token() {
    let s = server("cat-roles");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let viewer = user(&s, &boss, "view1", "viewer").await;
    for token in [&operator, &viewer] {
        let r = upload(&s, token, "tools", TOOLS, "application/json").await;
        assert_eq!(r.status, StatusCode::FORBIDDEN);
        assert_eq!(r.error(), "forbidden");
    }
    let (status, _) = call(
        &s,
        Method::DELETE,
        "/api/v1/catalogs/tools",
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // an administrator's token that carries only the scope to read
    let reader = scoped_token(&s, &boss, "admin", &["catalog:read"]).await;
    let r = upload(&s, &reader, "tools", TOOLS, "application/json").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.error(), "forbidden_scope");
    let (status, _) = call(
        &s,
        Method::GET,
        "/api/v1/catalogs/tools",
        Some(&reader),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let nothing = scoped_token(&s, &boss, "admin", &["projects:read"]).await;
    let (status, body) = call(
        &s,
        Method::GET,
        "/api/v1/catalogs/tools",
        Some(&nothing),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "forbidden_scope");
}

#[tokio::test]
async fn a_dataset_is_uploaded_as_json_or_csv_and_searched() {
    let s = server("cat-upload");
    let boss = admin(&s).await;
    let viewer = user(&s, &boss, "view1", "viewer").await;
    let r = upload(&s, &boss, "tools", TOOLS, "application/json").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json());
    assert_eq!(r.json()["version"], 1);
    assert_eq!(r.json()["items"], 3);
    let get = |query: &'static str| {
        let (s, viewer) = (&s, viewer.clone());
        async move {
            call(
                s,
                Method::GET,
                &format!("/api/v1/catalogs/tools{query}"),
                Some(&viewer),
                None,
            )
            .await
            .1
        }
    };
    assert_eq!(get("").await["total"], 3);
    assert_eq!(get("?q=MILL").await["total"], 1);
    let drills = get("?filter=kind:Drill&sort=dia_mm&desc=true").await;
    assert_eq!(drills["total"], 2);
    assert_eq!(drills["items"][0]["id"], "d10");
    assert_eq!(get("?filter=tags:fr4").await["total"], 2);
    assert_eq!(get("?filter=id:~d").await["total"], 2);
    let paged = get("?sort=id&offset=1&limit=1").await;
    assert_eq!(paged["items"][0]["id"], "d10");
    assert_eq!(paged["total"], 3);
    let (status, body) = call(
        &s,
        Method::GET,
        "/api/v1/catalogs/tools?filter=nonsense",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "bad_request");
    // the CSV replaces the dataset as a new version
    let csv = upload(
        &s,
        &boss,
        "tools",
        "id,kind,dia_mm\nx1,drill,0.5\nx2,mill,3\n",
        "text/csv",
    )
    .await;
    assert_eq!(csv.status, StatusCode::OK, "{}", csv.json());
    assert_eq!(csv.json()["version"], 2);
    assert_eq!(csv.json()["source"], "csv");
    assert_eq!(get("").await["total"], 2);
    // empty, and a version more
    let (status, emptied) = call(
        &s,
        Method::DELETE,
        "/api/v1/catalogs/tools",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (emptied["version"].as_u64(), emptied["items"].as_u64()),
        (Some(3), Some(0))
    );
}

#[tokio::test]
async fn a_dataset_that_does_not_fit_is_refused_whole() {
    let s = server("cat-bad");
    let boss = admin(&s).await;
    for (catalog, body, content_type) in [
        (
            "tools",
            r#"{"tools": [{"id": "a"}, {"id": "a"}]}"#,
            "application/json",
        ),
        ("tools", r#"{"tools": [{"name": "a"}]}"#, "application/json"),
        ("tools", r#"{"nothing": []}"#, "application/json"),
        ("tools", "not json at all", "application/json"),
        ("tools", "id,kind\nonly-one\n", "text/csv"),
        (
            "components",
            r#"{"parts": [{"id": "r-10k"}]}"#,
            "application/json",
        ),
        ("components", "id\nr-10k\n", "text/csv"),
        ("footprints", "id,pads\nR0603,2\n", "text/csv"),
    ] {
        let r = upload(&s, &boss, catalog, body, content_type).await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{catalog}: {body}"
        );
        assert_eq!(r.error(), "bad_catalog", "{catalog}: {body}");
    }
    // nothing was written
    let (_, list) = call(&s, Method::GET, "/api/v1/catalogs", Some(&boss), None).await;
    assert_eq!(list["catalogs"][1]["version"], 0);
    // the components base with its format, and a catalog that does not exist
    let ok = upload(
        &s,
        &boss,
        "components",
        r#"{"format": "phoenix-parts/1", "parts": [{"id": "r-10k", "value": "10k"}]}"#,
        "application/json",
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.json());
    let r = upload(&s, &boss, "nothing", "[]", "application/json").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.error(), "unknown_catalog");
}

#[tokio::test]
async fn the_tag_is_the_content_and_a_client_with_a_copy_gets_a_304() {
    let s = server("cat-etag");
    let boss = admin(&s).await;
    upload(&s, &boss, "tools", TOOLS, "application/json").await;
    let first = send(
        &s,
        Method::GET,
        "/api/v1/catalogs/tools",
        Some(&boss),
        &[],
        None,
        None,
    )
    .await;
    let tag = first.header("etag");
    assert!(tag.starts_with("\"tools-"), "{tag}");
    let same = send(
        &s,
        Method::GET,
        "/api/v1/catalogs/tools",
        Some(&boss),
        &[("if-none-match", tag.as_str())],
        None,
        None,
    )
    .await;
    assert_eq!(same.status, StatusCode::NOT_MODIFIED);
    assert!(same.bytes.is_empty());
    // the same content again is the same tag, other content is another
    upload(&s, &boss, "tools", TOOLS, "application/json").await;
    let again = send(
        &s,
        Method::GET,
        "/api/v1/catalogs/tools",
        Some(&boss),
        &[],
        None,
        None,
    )
    .await;
    assert_eq!(again.header("etag"), tag);
    assert_eq!(again.json()["version"], 2);
    upload(&s, &boss, "tools", r#"[{"id": "z"}]"#, "application/json").await;
    let other = send(
        &s,
        Method::GET,
        "/api/v1/catalogs/tools",
        Some(&boss),
        &[("if-none-match", tag.as_str())],
        None,
        None,
    )
    .await;
    assert_eq!(other.status, StatusCode::OK);
    assert_ne!(other.header("etag"), tag);
}

#[tokio::test]
async fn the_size_of_a_page_and_of_a_dataset_come_from_the_settings() {
    let s = server_with("cat-limits", |c| {
        c.catalog_page_max = 2;
        c.catalog_max_items = 3;
    });
    let boss = admin(&s).await;
    let (_, page) = call(
        &s,
        Method::GET,
        "/api/v1/catalogs/materials?limit=500",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(page["limit"], 2);
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let many = json!([{"id": "1"}, {"id": "2"}, {"id": "3"}, {"id": "4"}]).to_string();
    let r = upload(&s, &boss, "tools", &many, "application/json").await;
    assert_eq!(r.error(), "bad_catalog");
}

#[tokio::test]
async fn the_datasets_are_kept_when_the_server_starts_again() {
    let s = server("cat-restart");
    let boss = admin(&s).await;
    upload(&s, &boss, "tools", TOOLS, "application/json").await;
    let again = eh3d_server::state::AppState::open(eh3d_server::config::ServerConfig {
        data_dir: s.dir.clone(),
        ..eh3d_server::config::ServerConfig::default()
    })
    .unwrap();
    let book = again.catalogs.book.lock().unwrap();
    let (_, ds) = book.dataset("tools").unwrap();
    assert_eq!((ds.version, ds.items.len()), (1, 3));
}
