//! The plugins `vision` and `cad` end to end through the real router: simulated stations that make the results of the camera programs and the checks of the project file of
//! PHOENIX-CAD, kept as artifacts with the names the convention says, and the project store of the application `phoenix-cad` with `If-Match`, where a CAD project lives.

mod common;

use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use base64::Engine as _;
use common::{admin, call, enc, send, server, user, Server};
use serde_json::{json, Value};

async fn make_machine(
    s: &Server,
    boss: &str,
    id: &str,
    plugin: &str,
    settings: Value,
) -> (StatusCode, Value) {
    call(
        s,
        Method::POST,
        "/api/v1/work/machines",
        Some(boss),
        Some(json!({"id": id, "name": id, "plugin": plugin, "settings": settings})),
    )
    .await
}

async fn run_job(s: &Server, token: &str, machine: &str, request: Value) -> Value {
    let (status, job) = call(
        s,
        Method::POST,
        "/api/v1/work/jobs",
        Some(token),
        Some(json!({"machine": machine, "request": request, "app": "studio"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let id = job["id"].as_str().unwrap().to_string();
    let (status, started) = call(
        s,
        Method::POST,
        &format!("/api/v1/work/jobs/{id}/start"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let limit = Instant::now() + Duration::from_secs(60);
    loop {
        let (_, done) = call(
            s,
            Method::GET,
            &format!("/api/v1/work/jobs/{id}"),
            Some(token),
            None,
        )
        .await;
        if done["state"] == "done" {
            return done;
        }
        assert!(Instant::now() < limit, "waiting for done: {done}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

/// Writes a file of a project of the store, with the tag of the version it was made from when there is one.
async fn put_file(
    s: &Server,
    token: &str,
    url: &str,
    text: String,
    tag: Option<&str>,
) -> common::Reply {
    let mut headers = Vec::new();
    if let Some(t) = tag {
        headers.push(("if-match", t));
    }
    send(
        s,
        Method::PUT,
        url,
        Some(token),
        &headers,
        Some(text.into_bytes()),
        Some("application/octet-stream"),
    )
    .await
}

async fn prepare(s: &Server, token: &str, machine: &str, request: Value) -> (StatusCode, Value) {
    call(
        s,
        Method::POST,
        "/api/v1/work/jobs",
        Some(token),
        Some(json!({"machine": machine, "request": request})),
    )
    .await
}

/// The JSON an artifact holds.
async fn artifact_json(s: &Server, token: &str, id: &str) -> Value {
    let reply = send(
        s,
        Method::GET,
        &format!("/api/v1/artifacts/{id}"),
        Some(token),
        &[],
        None,
        None,
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    reply.json()
}

fn detect(what: &str, seed: u64) -> Value {
    json!({"kind": "detect", "what": what, "image": {"width": 1280, "height": 960, "sha256": "ab".repeat(32)}, "seed": seed})
}

#[tokio::test]
async fn both_plugins_are_listed_as_simulators_and_say_what_they_take() {
    let s = server("vc-list");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let (_, plugins) = call(&s, Method::GET, "/api/v1/plugins", Some(&operator), None).await;
    let list = plugins["plugins"].as_array().unwrap();
    for (kind, request) in [("vision", "detect"), ("cad", "check"), ("cad", "report")] {
        let p = list.iter().find(|p| p["kind"] == kind).unwrap();
        assert_eq!(p["manages_machines"], true, "{kind}");
        assert_eq!(p["capabilities"]["evidence"], "simulator", "{kind}");
        assert!(
            p["capabilities"]["requests"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r == request),
            "{kind}"
        );
    }
    let vision = list.iter().find(|p| p["kind"] == "vision").unwrap();
    assert_eq!(
        vision["capabilities"]["result"]["schema"],
        "eh3d.vision.result/1"
    );
    assert_eq!(
        vision["capabilities"]["result"]["artifact_name"],
        "vision-<kind>.json"
    );
    let cad = list.iter().find(|p| p["kind"] == "cad").unwrap();
    assert!(cad["capabilities"]["project"]["store"]
        .as_str()
        .unwrap()
        .contains("/apps/phoenix-cad/"));
    for (id, plugin) in [("v1", "vision"), ("c1", "cad")] {
        assert_eq!(
            make_machine(&s, &boss, id, plugin, json!({})).await.0,
            StatusCode::CREATED
        );
        let (_, machine) = call(
            &s,
            Method::GET,
            &format!("/api/v1/work/machines/{id}"),
            Some(&boss),
            None,
        )
        .await;
        assert_eq!(machine["detail"]["evidence"], "simulator", "{plugin}");
        assert_eq!(machine["detail"]["simulator_only"], true, "{plugin}");
    }
    // a typo in the settings is refused
    for (plugin, bad) in [
        ("vision", json!({"zoom": 2})),
        ("vision", json!({"max_findings": 0})),
        ("cad", json!({"kernel": "occ"})),
        ("cad", json!({"max_items": 0})),
    ] {
        let (status, body) = make_machine(&s, &boss, "x1", plugin, bad.clone()).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_plugin_request")),
            "{plugin} {bad}: {body}"
        );
    }
}

#[tokio::test]
async fn a_vision_result_follows_the_convention_and_is_the_same_for_the_same_input() {
    let s = server("vc-vision");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make_machine(&s, &boss, "v1", "vision", json!({"sim_rate": 0})).await;
    let first = run_job(&s, &operator, "v1", detect("fiducials", 3)).await;
    let result = &first["view"]["result"];
    assert_eq!(
        (
            result["schema"].clone(),
            result["kind"].clone(),
            result["evidence"].clone()
        ),
        (
            json!("eh3d.vision.result/1"),
            json!("fiducials"),
            json!("simulator")
        )
    );
    assert_eq!(result["source"]["width"], 1280);
    assert!(!result["findings"].as_array().unwrap().is_empty());
    assert_eq!(first["view"]["artifact_name"], "vision-fiducials.json");
    let again = run_job(&s, &operator, "v1", detect("fiducials", 3)).await;
    assert_eq!(again["view"]["result"], first["view"]["result"]);
    let other = run_job(&s, &operator, "v1", detect("fiducials", 4)).await;
    assert_ne!(
        other["view"]["result"]["findings"],
        first["view"]["result"]["findings"]
    );
    for what in ["barcode", "focus", "blobs"] {
        let done = run_job(&s, &operator, "v1", detect(what, 0)).await;
        assert_eq!(done["view"]["result"]["kind"], what);
    }
    // a request that does not fit is refused with what is wrong
    for bad in [
        json!({"kind": "track", "what": "focus", "image": {"width": 1, "height": 1}}),
        json!({"kind": "detect", "what": "faces", "image": {"width": 1, "height": 1}}),
        json!({"kind": "detect", "what": "focus"}),
        json!({"kind": "detect", "what": "focus", "image": {"width": 0, "height": 1}}),
        json!({"kind": "detect", "what": "focus", "image": {"width": 1, "height": 1, "sha256": "nothex"}}),
    ] {
        let (status, body) = prepare(&s, &operator, "v1", bad.clone()).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_plugin_request")),
            "{bad}: {body}"
        );
    }
}

#[tokio::test]
async fn a_result_can_be_kept_as_an_artifact_with_the_official_name() {
    let s = server("vc-artifact");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make_machine(&s, &boss, "v1", "vision", json!({"sim_rate": 0})).await;
    let mut request = detect("barcode", 1);
    request["output"] = json!("artifact");
    let done = run_job(&s, &operator, "v1", request.clone()).await;
    assert!(
        done["view"].get("result").is_none(),
        "the result is in the artifact: {done}"
    );
    let artifact = &done["view"]["artifact"];
    assert_eq!(
        (artifact["name"].clone(), artifact["format"].clone()),
        (json!("vision-barcode.json"), json!("application/json"))
    );
    let id = artifact["artifact_id"].as_str().unwrap();
    let kept = artifact_json(&s, &operator, id).await;
    assert_eq!(
        (kept["schema"].clone(), kept["kind"].clone()),
        (json!("eh3d.vision.result/1"), json!("barcode"))
    );
    assert!(kept["findings"][0]["text"]
        .as_str()
        .unwrap()
        .starts_with("SIM-"));
    let (_, meta) = call(
        &s,
        Method::GET,
        &format!("/api/v1/artifacts/{id}/meta"),
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(meta["schema_version"], "eh3d.vision.result/1");
    assert_eq!(meta["producer"]["tool"], "eh3d-server vision");
    // a name of the caller's choosing, and a wrong `output`
    request["artifact_name"] = json!("vision-barcode-line2.json");
    let named = run_job(&s, &operator, "v1", request).await;
    assert_eq!(
        named["view"]["artifact"]["name"],
        "vision-barcode-line2.json"
    );
    let mut bad = detect("focus", 0);
    bad["output"] = json!("file");
    let (status, body) = prepare(&s, &operator, "v1", bad).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_plugin_request")),
        "{body}"
    );
}

fn project() -> Value {
    json!({
        "schema": "eh3d.cad.project/1", "name": "bracket", "units": "mm",
        "sketches": [{"id": "s1"}, {"id": "s2"}],
        "features": [
            {"id": "f1", "kind": "extrude", "sketch": "s1"},
            {"id": "f2", "kind": "hole", "sketch": "s2"},
            {"id": "f3", "kind": "fillet", "target": "f1"},
        ],
        "parts": [{"id": "p1", "name": "Bracket", "features": ["f1", "f2", "f3"]}],
    })
}

#[tokio::test]
async fn a_cad_project_is_checked_and_reported_from_the_request_or_from_an_artifact() {
    let s = server("vc-cad");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make_machine(&s, &boss, "c1", "cad", json!({"sim_rate": 0})).await;
    let report = run_job(
        &s,
        &operator,
        "c1",
        json!({"kind": "report", "project": project()}),
    )
    .await;
    assert_eq!(report["view"]["verdict"], "pass");
    let result = &report["view"]["result"];
    assert_eq!(result["schema"], "eh3d.cad.report/1");
    assert_eq!(result["counts"]["features"], 3);
    assert_eq!(result["counts"]["features_by_kind"]["fillet"], 1);
    assert_eq!(result["parts"][0]["name"], "Bracket");
    assert_eq!(result["evidence"], "simulator");
    // a project with a broken reference fails the check, and the job still ends done: the verdict is the answer
    let mut broken = project();
    broken["features"][1]["sketch"] = json!("s9");
    let check = run_job(
        &s,
        &operator,
        "c1",
        json!({"kind": "check", "project": broken}),
    )
    .await;
    assert_eq!(check["view"]["verdict"], "fail");
    assert_eq!(check["view"]["result"]["check"]["errors"], 1);
    assert_eq!(
        check["view"]["result"]["check"]["issues"][0]["code"],
        "missing_ref"
    );
    // the project as an artifact, and the report kept as one
    let (status, made) = call(
        &s,
        Method::POST,
        "/api/v1/artifacts",
        Some(&operator),
        Some(json!({"name": "project.json", "format": "application/json", "data_base64": base64::engine::general_purpose::STANDARD.encode(project().to_string())})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let id = made["artifact_id"].as_str().unwrap().to_string();
    let kept = run_job(
        &s,
        &operator,
        "c1",
        json!({"kind": "report", "artifact": id, "output": "artifact"}),
    )
    .await;
    assert_eq!(kept["view"]["artifact"]["name"], "cad-report.json");
    let report_id = kept["view"]["artifact"]["artifact_id"].as_str().unwrap();
    let body = artifact_json(&s, &operator, report_id).await;
    assert_eq!(body["check"]["verdict"], "pass");
    let (_, meta) = call(
        &s,
        Method::GET,
        &format!("/api/v1/artifacts/{report_id}/meta"),
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(meta["inputs"][0]["artifact_id"], json!(id));
    // what is not a project is refused
    for bad in [
        json!({"kind": "check"}),
        json!({"kind": "weigh", "project": project()}),
        json!({"kind": "check", "project": "text"}),
        json!({"kind": "check", "project": project(), "artifact": id}),
    ] {
        let (status, body) = prepare(&s, &operator, "c1", bad.clone()).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_plugin_request")),
            "{bad}: {body}"
        );
    }
    // a project over the limit of the station is not read
    make_machine(&s, &boss, "c2", "cad", json!({"max_items": 3})).await;
    let (status, body) = prepare(
        &s,
        &operator,
        "c2",
        json!({"kind": "check", "project": project()}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_plugin_request")),
        "{body}"
    );
}

#[tokio::test]
async fn a_cad_project_lives_in_the_store_of_the_application_with_if_match() {
    let s = server("vc-cad-store");
    let boss = admin(&s).await;
    let made = send(
        &s,
        Method::POST,
        "/api/v1/apps/phoenix-cad/projects",
        Some(&boss),
        &[],
        Some(json!({"name": "bracket"}).to_string().into_bytes()),
        Some("application/json"),
    )
    .await;
    assert_eq!(made.status, StatusCode::CREATED, "{}", made.json());
    let id = made.json()["id"].as_str().unwrap().to_string();
    let url = format!(
        "/api/v1/apps/phoenix-cad/projects/{id}/files/content?name={}",
        enc("project.json")
    );
    let first = put_file(&s, &boss, &url, project().to_string(), None).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.json());
    // a file that exists is written with the version that was read: without it 428, with another one 412
    let mut changed = project();
    changed["name"] = json!("bracket v2");
    let blind = put_file(&s, &boss, &url, changed.to_string(), None).await;
    assert_eq!(
        (blind.status, blind.error()),
        (
            StatusCode::PRECONDITION_REQUIRED,
            "precondition_required".to_string()
        )
    );
    let stale = put_file(&s, &boss, &url, changed.to_string(), Some("\"deadbeef\"")).await;
    assert_eq!(
        (stale.status, stale.error()),
        (
            StatusCode::PRECONDITION_FAILED,
            "precondition_failed".to_string()
        )
    );
    let tag = first.header("etag");
    let second = put_file(&s, &boss, &url, changed.to_string(), Some(tag.as_str())).await;
    assert_eq!(second.status, StatusCode::CREATED, "{}", second.json());
    assert_eq!(second.json()["version"], 2);
    // what was read from the store is what the station checks
    let read = send(&s, Method::GET, &url, Some(&boss), &[], None, None).await;
    assert_eq!(read.status, StatusCode::OK);
    let stored: Value = read.json();
    make_machine(&s, &boss, "c1", "cad", json!({"sim_rate": 0})).await;
    let report = run_job(
        &s,
        &boss,
        "c1",
        json!({"kind": "report", "project": stored}),
    )
    .await;
    assert_eq!(report["view"]["result"]["name"], "bracket v2");
    assert_eq!(report["view"]["verdict"], "pass");
}
