//! Slicing on the server (`POST /api/v1/slice`): off until the switch `slice_enabled` is on, a mesh and the profile of a printer in, G-code out that stays inside the limits of the
//! profile (a refusal, never a program cut down), kept in the store when asked, and a program that a simulated printer then prints. The small slicer is tried here end to end;
//! the program of the suite (`slice_phoenix_bin`) only on the paths that do not need it to exist.

mod common;

use axum::http::{Method, StatusCode};
use base64::Engine as _;
use common::{admin, call, server, server_with, user, Server};
use serde_json::{json, Value};

const POLICY: &str = "/api/v1/auth/policy";

/// A closed cube, as a text STL.
fn cube_stl(size: f64) -> String {
    let v = |x: usize, y: usize, z: usize| [x as f64 * size, y as f64 * size, z as f64 * size];
    let quads = [
        [v(0, 0, 0), v(0, 1, 0), v(1, 1, 0), v(1, 0, 0)],
        [v(0, 0, 1), v(1, 0, 1), v(1, 1, 1), v(0, 1, 1)],
        [v(0, 0, 0), v(1, 0, 0), v(1, 0, 1), v(0, 0, 1)],
        [v(1, 0, 0), v(1, 1, 0), v(1, 1, 1), v(1, 0, 1)],
        [v(1, 1, 0), v(0, 1, 0), v(0, 1, 1), v(1, 1, 1)],
        [v(0, 1, 0), v(0, 0, 0), v(0, 0, 1), v(0, 1, 1)],
    ];
    let mut text = String::from("solid cube\n");
    for q in quads {
        for tri in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
            text.push_str("facet normal 0 0 0\nouter loop\n");
            for p in tri {
                text.push_str(&format!("vertex {} {} {}\n", p[0], p[1], p[2]));
            }
            text.push_str("endloop\nendfacet\n");
        }
    }
    text.push_str("endsolid cube\n");
    text
}

fn mesh(size: f64) -> Value {
    json!({"base64": base64::engine::general_purpose::STANDARD.encode(cube_stl(size))})
}

async fn enable(s: &Server, boss: &str) {
    let (status, body) = call(
        s,
        Method::PUT,
        POLICY,
        Some(boss),
        Some(json!({"key": "slice_enabled", "value": true, "confirm_weak": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn slice(s: &Server, token: &str, body: Value) -> (StatusCode, Value) {
    call(s, Method::POST, "/api/v1/slice", Some(token), Some(body)).await
}

/// A request that has to be refused with `error` and `status`; gives the answer.
async fn refused(s: &Server, token: &str, body: Value, error: &str, status: StatusCode) -> Value {
    let (got, answer) = slice(s, token, body.clone()).await;
    assert_eq!(
        (got, answer["error"].as_str()),
        (status, Some(error)),
        "{body}: {answer}"
    );
    answer
}

#[tokio::test]
async fn slicing_is_off_by_default_and_the_switch_is_listed() {
    let s = server("slice-off");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let request = json!({"mesh": mesh(20.0), "profile": {"model": "bambu_a1"}});
    let (status, body) = slice(&s, &operator, request.clone()).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("slice_off")),
        "{body}"
    );
    let (_, policy) = call(&s, Method::GET, POLICY, Some(&boss), None).await;
    let listed = policy["switches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["key"] == "slice_enabled")
        .cloned()
        .unwrap();
    assert_eq!(
        (listed["value"].clone(), listed["safe"].clone()),
        (json!(false), json!(true))
    );
    assert!(policy["editable"]
        .as_array()
        .unwrap()
        .contains(&json!("slice_enabled")));
    // turning it on needs the confirmation; a viewer cannot slice
    let (status, body) = call(
        &s,
        Method::PUT,
        POLICY,
        Some(&boss),
        Some(json!({"key": "slice_enabled", "value": true})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("weak_confirm_required"))
    );
    enable(&s, &boss).await;
    let viewer = user(&s, &boss, "view1", "viewer").await;
    let (status, _) = slice(&s, &viewer, request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_cube_is_sliced_inside_the_limits_of_the_profile_and_the_program_prints_on_a_simulator() {
    let s = server("slice-cube");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    enable(&s, &boss).await;
    let (status, out) = slice(
        &s,
        &operator,
        json!({"mesh": mesh(20.0), "profile": {"model": "bambu_a1", "layer_mm": 0.2, "infill_pct": 15, "hotend_c": 215, "bed_c": 60}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert_eq!(
        (
            out["engine"].clone(),
            out["layers"].clone(),
            out["open_layers"].clone()
        ),
        (json!("builtin"), json!(100), json!(0))
    );
    assert_eq!(out["bounds_mm"]["max"][2], 20.0);
    assert_eq!(out["limits"]["hotend_max_c"], 300.0);
    assert_eq!(out["limits"]["bed_mm"], json!([256.0, 256.0, 256.0]));
    // the program was read again against the limits before it was given back
    assert_eq!(out["read"]["max_hotend_c"], 215.0);
    assert_eq!(out["read"]["max_bed_c"], 60.0);
    assert!(out["read"]["max_feed_mm_s"].as_f64().unwrap() <= 200.0);
    let gcode = out["gcode"].as_str().unwrap().to_string();
    assert_eq!(gcode.matches(";LAYER_CHANGE").count(), 100);
    // a simulated A1 prints it
    let (status, made) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines",
        Some(&boss),
        Some(json!({"id": "a1", "name": "A1", "plugin": "printer3d", "settings": {"model": "bambu_a1", "bed_x_mm": 256, "bed_y_mm": 256, "bed_z_mm": 256, "sim_rate": 0}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let (status, job) = call(
        &s,
        Method::POST,
        "/api/v1/work/jobs",
        Some(&operator),
        Some(json!({"machine": "a1", "request": {"kind": "print", "text": gcode, "name": "cube"}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    assert_eq!(job["view"]["layers"], 100);
    // the profile of a printer that exists gives the same limits
    let (status, by_machine) = slice(
        &s,
        &operator,
        json!({"mesh": mesh(20.0), "profile": {"machine": "a1"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{by_machine}");
    assert_eq!(by_machine["limits"]["bed_mm"], json!([256.0, 256.0, 256.0]));
    let (status, body) = slice(
        &s,
        &operator,
        json!({"mesh": mesh(20.0), "profile": {"machine": "nothing"}}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_machine")),
        "{body}"
    );
}

#[tokio::test]
async fn what_does_not_fit_or_goes_past_a_limit_is_refused_and_never_cut_down() {
    let s = server("slice-limits");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    enable(&s, &boss).await;
    let unprocessable = StatusCode::UNPROCESSABLE_ENTITY;
    // bigger than the bed of the A1 (256 mm)
    let answer = refused(
        &s,
        &operator,
        json!({"mesh": mesh(300.0), "profile": {"model": "bambu_a1"}}),
        "slice_limit",
        unprocessable,
    )
    .await;
    assert!(
        answer["detail"].as_str().unwrap().contains("along X"),
        "{answer}"
    );
    // a temperature over the limit of the model, a feed over the limit
    refused(
        &s,
        &operator,
        json!({"mesh": mesh(20.0), "profile": {"model": "bambu_a1", "hotend_c": 320}}),
        "slice_limit",
        unprocessable,
    )
    .await;
    refused(
        &s,
        &operator,
        json!({"mesh": mesh(20.0), "profile": {"model": "bambu_a1", "bed_c": 120}}),
        "slice_limit",
        unprocessable,
    )
    .await;
    refused(
        &s,
        &operator,
        json!({"mesh": mesh(20.0), "profile": {"model": "bambu_a1", "print_feed_mm_s": 300}}),
        "slice_limit",
        unprocessable,
    )
    .await;
    // the generic model has no known bed: it has to be told
    refused(
        &s,
        &operator,
        json!({"mesh": mesh(20.0), "profile": {"model": "generic"}}),
        "bad_plugin_request",
        unprocessable,
    )
    .await;
    let (status, ok) = slice(
        &s,
        &operator,
        json!({"mesh": mesh(20.0), "profile": {"bed_x_mm": 200, "bed_y_mm": 200, "bed_z_mm": 100}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ok}");
    assert_eq!(ok["limits"]["hotend_max_c"], 260.0);
    // a typo, a connection, a profile that is not an object
    refused(
        &s,
        &operator,
        json!({"mesh": mesh(20.0), "profile": {"model": "bambu_a1", "zzz": 1}}),
        "bad_plugin_request",
        unprocessable,
    )
    .await;
    refused(&s, &operator, json!({"mesh": mesh(20.0), "profile": {"model": "bambu_a1", "connection": {"kind": "demo"}}}), "bad_plugin_request", unprocessable).await;
    refused(
        &s,
        &operator,
        json!({"mesh": mesh(20.0), "profile": "bambu_a1"}),
        "bad_plugin_request",
        unprocessable,
    )
    .await;
    refused(
        &s,
        &operator,
        json!({"mesh": mesh(20.0), "profile": {"model": "bambu_a1"}, "engine": "other"}),
        "bad_plugin_request",
        unprocessable,
    )
    .await;
    refused(
        &s,
        &operator,
        json!({"mesh": mesh(20.0), "profile": {"model": "bambu_a1"}, "output": "file"}),
        "bad_plugin_request",
        unprocessable,
    )
    .await;
    // a mesh that is not one
    refused(&s, &operator, json!({"mesh": {"base64": base64::engine::general_purpose::STANDARD.encode("hello")}, "profile": {"model": "bambu_a1"}}), "bad_mesh", unprocessable).await;
    refused(
        &s,
        &operator,
        json!({"mesh": {"base64": "***"}, "profile": {"model": "bambu_a1"}}),
        "bad_plugin_request",
        unprocessable,
    )
    .await;
    refused(
        &s,
        &operator,
        json!({"mesh": {}, "profile": {"model": "bambu_a1"}}),
        "bad_plugin_request",
        unprocessable,
    )
    .await;
    refused(
        &s,
        &operator,
        json!({"mesh": {"artifact": "a-0000"}, "profile": {"model": "bambu_a1"}}),
        "unknown_artifact",
        StatusCode::NOT_FOUND,
    )
    .await;
    // a 3MF whose model is compressed is not read; a zip that is not a 3MF either
    let not_a_3mf = base64::engine::general_purpose::STANDARD
        .encode(b"PK\x03\x04 and nothing else that makes a zip");
    refused(
        &s,
        &operator,
        json!({"mesh": {"base64": not_a_3mf}, "profile": {"model": "bambu_a1"}}),
        "bad_mesh",
        unprocessable,
    )
    .await;
    // an open surface has no closed outline
    let open = "solid x\nfacet normal 0 0 0\nouter loop\nvertex 0 0 0\nvertex 10 0 0\nvertex 0 0 10\nendloop\nendfacet\nendsolid x\n";
    let open = base64::engine::general_purpose::STANDARD.encode(open);
    refused(
        &s,
        &operator,
        json!({"mesh": {"base64": open}, "profile": {"model": "bambu_a1"}}),
        "bad_mesh",
        unprocessable,
    )
    .await;
}

#[tokio::test]
async fn the_program_can_be_kept_in_the_store_and_printed_from_there() {
    let s = server("slice-artifact");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    enable(&s, &boss).await;
    // the mesh as an artifact
    let (status, made) = call(
        &s,
        Method::POST,
        "/api/v1/artifacts",
        Some(&operator),
        Some(json!({"name": "cube.stl", "format": "model/stl", "data_base64": base64::engine::general_purpose::STANDARD.encode(cube_stl(10.0))})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    let mesh_id = made["artifact_id"].as_str().unwrap().to_string();
    let (status, out) = slice(
        &s,
        &operator,
        json!({"mesh": {"artifact": mesh_id}, "profile": {"model": "bambu_a1"}, "output": "artifact", "artifact_name": "cube.gcode"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{out}");
    assert!(out.get("gcode").is_none());
    assert_eq!(out["artifact"]["name"], "cube.gcode");
    assert_eq!(out["artifact"]["format"], "text/x-gcode");
    let program = out["artifact"]["artifact_id"].as_str().unwrap().to_string();
    // its provenance names the mesh
    let (status, meta) = call(
        &s,
        Method::GET,
        &format!("/api/v1/artifacts/{program}/meta"),
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{meta}");
    assert_eq!(meta["inputs"][0]["artifact_id"], json!(mesh_id));
    // somebody else does not see the mesh
    let other = user(&s, &boss, "oper2", "operator").await;
    let (status, body) = slice(
        &s,
        &other,
        json!({"mesh": {"artifact": mesh_id}, "profile": {"model": "bambu_a1"}}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_artifact")),
        "{body}"
    );
    // a printer prints the artifact
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines",
        Some(&boss),
        Some(json!({"id": "a1", "name": "A1", "plugin": "printer3d", "settings": {"model": "bambu_a1", "sim_rate": 0}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, job) = call(
        &s,
        Method::POST,
        "/api/v1/work/jobs",
        Some(&operator),
        Some(json!({"machine": "a1", "request": {"kind": "print", "artifact": program}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    assert_eq!(job["view"]["layers"], 50);
}

#[tokio::test]
async fn a_mesh_over_the_size_limit_is_refused_and_the_suite_slicer_needs_to_be_told_where_it_is() {
    let s = server_with("slice-size", |c| c.slice_max_mb = 1);
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    enable(&s, &boss).await;
    let big = base64::engine::general_purpose::STANDARD.encode(vec![b'x'; 2 * 1024 * 1024]);
    let (status, body) = slice(
        &s,
        &operator,
        json!({"mesh": {"base64": big}, "profile": {"model": "bambu_a1"}}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("too_big")),
        "{body}"
    );
    // the engine phoenix is not set up
    let (status, body) = slice(&s, &operator, json!({"mesh": mesh(10.0), "profile": {"model": "bambu_a1", "phoenix_printer": "bambu-lab-a1"}, "engine": "phoenix"})).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_IMPLEMENTED, Some("not_supported")),
        "{body}"
    );
    // the layers have a limit of their own
    let t = server_with("slice-layers", |c| c.slice_max_layers = 10);
    let boss = admin(&t).await;
    let operator = user(&t, &boss, "oper1", "operator").await;
    enable(&t, &boss).await;
    let (status, body) = slice(
        &t,
        &operator,
        json!({"mesh": mesh(20.0), "profile": {"model": "bambu_a1"}}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("slice_limit")),
        "{body}"
    );
    assert!(
        body["detail"].as_str().unwrap().contains("layers"),
        "{body}"
    );
}

#[tokio::test]
async fn the_suite_slicer_that_fails_is_a_bad_gateway_and_leaves_nothing_behind() {
    // the program is a file that is there and is not a slicer: the test program itself, which refuses the arguments of the slicer
    let exe = std::env::current_exe().unwrap();
    let s = server_with("slice-phoenix", |c| c.slice_phoenix_bin = Some(exe.clone()));
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    enable(&s, &boss).await;
    let (status, body) = slice(&s, &operator, json!({"mesh": mesh(10.0), "profile": {"model": "bambu_a1", "phoenix_printer": "bambu-lab-a1"}, "engine": "phoenix"})).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("slice_failed")),
        "{body}"
    );
    // a printer id that could carry something else is refused before anything is run
    let (status, body) = slice(&s, &operator, json!({"mesh": mesh(10.0), "profile": {"model": "bambu_a1", "phoenix_printer": "a b; rm"}, "engine": "phoenix"})).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_plugin_request")),
        "{body}"
    );
    let (status, body) = slice(
        &s,
        &operator,
        json!({"mesh": mesh(10.0), "profile": {"model": "bambu_a1"}, "engine": "phoenix"}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_plugin_request")),
        "{body}"
    );
    // the folder made for the slice is gone
    let tmp = s.dir.join("slice-tmp");
    let left = std::fs::read_dir(&tmp).map(|d| d.count()).unwrap_or(0);
    assert_eq!(left, 0, "nothing is left in {}", tmp.display());
}
