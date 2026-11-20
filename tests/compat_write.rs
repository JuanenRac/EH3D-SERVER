//! The routes of Moonraker and OctoPrint that WRITE: off behind a switch of their own (`compat_write`), opened only by a key that was made to write, acting as an operator that
//! exists for one machine, and never starting a print by themselves: an uploaded file waits in the queue, a program may start it only when the machine says so
//! (`compat_start`), and pausing and cancelling work on what runs.

mod common;

use axum::http::{Method, StatusCode};
use common::{admin, call, send, server, Server};
use serde_json::{json, Value};

const POLICY: &str = "/api/v1/auth/policy";
const PROGRAM: &str = "G28\nM104 S200\n;LAYER_CHANGE\nG1 X10 Y10 F3000\nG1 X20 Y10\n;LAYER_CHANGE\nG1 X20 Y20\nM104 S0\n";

async fn set(s: &Server, boss: &str, key: &str, on: bool) {
    let (status, body) = call(
        s,
        Method::PUT,
        POLICY,
        Some(boss),
        Some(json!({"key": key, "value": on, "confirm_weak": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn make_printer(s: &Server, boss: &str, id: &str, settings: Value) {
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/work/machines",
        Some(boss),
        Some(json!({"id": id, "name": id, "plugin": "printer3d", "settings": settings})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn issue_key(s: &Server, boss: &str, id: &str, body: Option<Value>) -> (String, Value) {
    let (status, answer) = call(
        s,
        Method::POST,
        &format!("/api/v1/work/machines/{id}/compat/key"),
        Some(boss),
        body,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    (answer["key"].as_str().unwrap().to_string(), answer)
}

/// A multipart body with a file and a field, the way a slicer program sends it.
fn multipart(filename: &str, text: &[u8]) -> (Vec<u8>, String) {
    let mut body = Vec::new();
    body.extend_from_slice(
        b"--XyZ\r\nContent-Disposition: form-data; name=\"print\"\r\n\r\ntrue\r\n",
    );
    body.extend_from_slice(
        format!("--XyZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: text/plain\r\n\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(text);
    body.extend_from_slice(b"\r\n--XyZ--\r\n");
    (body, "multipart/form-data; boundary=XyZ".to_string())
}

async fn post(s: &Server, path: &str, key: Option<&str>) -> (StatusCode, Value) {
    let headers: Vec<(&str, &str)> = key.map(|k| vec![("X-Api-Key", k)]).unwrap_or_default();
    let reply = send(s, Method::POST, path, None, &headers, None, None).await;
    (reply.status, reply.json())
}

async fn upload(
    s: &Server,
    path: &str,
    key: &str,
    filename: &str,
    text: &[u8],
) -> (StatusCode, Value) {
    let (body, kind) = multipart(filename, text);
    let reply = send(
        s,
        Method::POST,
        path,
        None,
        &[("X-Api-Key", key)],
        Some(body),
        Some(&kind),
    )
    .await;
    (reply.status, reply.json())
}

async fn octo_command(s: &Server, key: &str, body: Value) -> (StatusCode, Value) {
    let reply = send(
        s,
        Method::POST,
        "/api/job",
        None,
        &[("X-Api-Key", key)],
        Some(body.to_string().into_bytes()),
        Some("application/json"),
    )
    .await;
    (reply.status, reply.json())
}

async fn jobs(s: &Server, boss: &str, machine: &str) -> Vec<Value> {
    let (status, body) = call(
        s,
        Method::GET,
        &format!("/api/v1/work/jobs?machine={machine}"),
        Some(boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["items"].as_array().unwrap().clone()
}

async fn state_of_first(s: &Server, boss: &str, machine: &str) -> String {
    jobs(s, boss, machine).await[0]["state"]
        .as_str()
        .unwrap()
        .to_string()
}

/// A server with both switches on and a printer `p1` that gives both shapes, with a key that writes.
async fn ready(name: &str, settings: Value) -> (Server, String, String) {
    let s = server(name);
    let boss = admin(&s).await;
    set(&s, &boss, "compat_api", true).await;
    set(&s, &boss, "compat_write", true).await;
    let mut all = json!({"compat": "both", "sim_rate": 0});
    for (k, v) in settings.as_object().unwrap() {
        all[k] = v.clone();
    }
    make_printer(&s, &boss, "p1", all).await;
    let (key, answer) = issue_key(&s, &boss, "p1", Some(json!({"write": true}))).await;
    assert_eq!(answer["write"], true);
    (s, boss, key)
}

#[tokio::test]
async fn the_write_routes_are_off_by_default_whatever_the_key() {
    let s = server("cw-off");
    let boss = admin(&s).await;
    set(&s, &boss, "compat_api", true).await;
    make_printer(&s, &boss, "p1", json!({"compat": "both"})).await;
    let (key, answer) = issue_key(&s, &boss, "p1", Some(json!({"write": true}))).await;
    assert_eq!(answer["write"], true);
    // the switch is listed, off, as strict as recommended and editable
    let (_, policy) = call(&s, Method::GET, POLICY, Some(&boss), None).await;
    let listed = policy["switches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["key"] == "compat_write")
        .cloned()
        .unwrap();
    assert_eq!(
        (listed["value"].clone(), listed["safe"].clone()),
        (json!(false), json!(true))
    );
    assert!(policy["editable"]
        .as_array()
        .unwrap()
        .contains(&json!("compat_write")));
    for path in [
        "/printer/print/start",
        "/printer/print/pause",
        "/printer/print/resume",
        "/printer/print/cancel",
    ] {
        let (status, body) = post(&s, path, Some(&key)).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::NOT_FOUND, Some("compat_write_off")),
            "{path}"
        );
    }
    let (status, body) = upload(
        &s,
        "/server/files/upload",
        &key,
        "part.gcode",
        PROGRAM.as_bytes(),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("compat_write_off"))
    );
    let (status, body) = upload(
        &s,
        "/api/files/local",
        &key,
        "part.gcode",
        PROGRAM.as_bytes(),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("compat_write_off"))
    );
    let (status, body) = octo_command(&s, &key, json!({"command": "cancel"})).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("compat_write_off"))
    );
    assert!(jobs(&s, &boss, "p1").await.is_empty(), "nothing was queued");
    // the reads still answer; the whole family is off when compat_api is off
    set(&s, &boss, "compat_write", true).await;
    set(&s, &boss, "compat_api", false).await;
    let (status, body) = post(&s, "/printer/print/cancel", Some(&key)).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("compat_off"))
    );
}

#[tokio::test]
async fn a_key_writes_only_when_it_was_made_to_and_only_for_its_machine() {
    let (s, boss, key) = ready("cw-keys", json!({})).await;
    make_printer(&s, &boss, "p2", json!({"compat": "both", "sim_rate": 0})).await;
    let (read_only, answer) = issue_key(&s, &boss, "p2", None).await;
    assert_eq!(answer["write"], false);
    let (status, state) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/p2/compat/key",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(
        (status, state["set"].clone(), state["write"].clone()),
        (StatusCode::OK, json!(true), json!(false))
    );
    // a key that only reads is refused by every route that writes
    let (status, body) = upload(
        &s,
        "/server/files/upload",
        &read_only,
        "part.gcode",
        PROGRAM.as_bytes(),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    let (status, body) = post(&s, "/printer/print/cancel", Some(&read_only)).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    // no key, a wrong key
    for bad in [None, Some("eh3d_nothing")] {
        let (status, body) = post(&s, "/printer/print/cancel", bad).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNAUTHORIZED, Some("unauthorized"))
        );
    }
    // the key of p1 uploads to p1 and nowhere else
    let (status, _) = upload(
        &s,
        "/server/files/upload",
        &key,
        "part.gcode",
        PROGRAM.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(jobs(&s, &boss, "p1").await.len(), 1);
    assert!(jobs(&s, &boss, "p2").await.is_empty());
    // a new key made without `write` forgets it, and the old key stops working
    let (again, _) = issue_key(&s, &boss, "p1", None).await;
    let (status, body) = post(&s, "/printer/print/cancel", Some(&key)).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("unauthorized"))
    );
    let (status, body) = post(&s, "/printer/print/cancel", Some(&again)).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    // a body that is not JSON, or says something else about `write`, is refused
    for body in [json!("yes"), json!({"write": "yes"})] {
        let (status, answer) = call(
            &s,
            Method::POST,
            "/api/v1/work/machines/p1/compat/key",
            Some(&boss),
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
    }
}

#[tokio::test]
async fn an_uploaded_file_waits_in_the_queue_and_nothing_starts_by_itself() {
    let (s, boss, key) = ready("cw-upload", json!({})).await;
    // the field print=true of the program is read as a wish and not obeyed
    let (status, body) = upload(
        &s,
        "/server/files/upload",
        &key,
        "folder/part.gcode",
        PROGRAM.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (body["print_queued"].clone(), body["print_started"].clone()),
        (json!(true), json!(false))
    );
    assert_eq!(body["item"]["path"], "part.gcode");
    assert_eq!(body["eh3d"]["state"], "queued");
    let list = jobs(&s, &boss, "p1").await;
    assert_eq!(list.len(), 1);
    assert_eq!(
        (list[0]["state"].clone(), list[0]["name"].clone()),
        (json!("queued"), json!("part.gcode"))
    );
    assert_eq!(list[0]["by"], "compat:p1");
    let (_, machine) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/p1",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(machine["state"], "idle");
    // the same name replaces the one that waits
    let (status, _) = upload(
        &s,
        "/server/files/upload",
        &key,
        "part.gcode",
        PROGRAM.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let list = jobs(&s, &boss, "p1").await;
    let waiting = list.iter().filter(|j| j["state"] == "queued").count();
    assert_eq!(
        (list.len(), waiting),
        (2, 1),
        "the old one was cancelled: {list:?}"
    );
    // OctoPrint: 201, the same queue
    let (status, body) = upload(
        &s,
        "/api/files/local",
        &key,
        "other.gcode",
        PROGRAM.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["done"], true);
    assert_eq!(body["files"]["local"]["name"], "other.gcode");
    assert_eq!(body["eh3d"]["state"], "queued");
    assert_eq!(
        jobs(&s, &boss, "p1")
            .await
            .iter()
            .filter(|j| j["state"] == "queued")
            .count(),
        2
    );
}

#[tokio::test]
async fn what_is_uploaded_is_checked_against_the_limits_of_the_model_like_any_job() {
    let (s, boss, key) = ready("cw-limits", json!({})).await;
    let (status, body) = upload(
        &s,
        "/server/files/upload",
        &key,
        "hot.gcode",
        b"M104 S400\nG1 X1\n",
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("program_over_limit")),
        "{body}"
    );
    let (status, body) = upload(
        &s,
        "/server/files/upload",
        &key,
        "bin.gcode",
        &[0xff, 0xfe, 0x00, 0x01],
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_gcode")),
        "{body}"
    );
    let reply = send(
        &s,
        Method::POST,
        "/server/files/upload",
        None,
        &[("X-Api-Key", key.as_str())],
        Some(b"not a multipart body".to_vec()),
        Some("text/plain"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    let (body, kind) = multipart("a.gcode", b"G1 X1");
    let body = String::from_utf8(body)
        .unwrap()
        .replace("name=\"file\"; ", "");
    let reply = send(
        &s,
        Method::POST,
        "/server/files/upload",
        None,
        &[("X-Api-Key", key.as_str())],
        Some(body.into_bytes()),
        Some(&kind),
    )
    .await;
    assert_eq!(
        reply.status,
        StatusCode::BAD_REQUEST,
        "a part with no name: {}",
        reply.json()
    );
    assert!(jobs(&s, &boss, "p1").await.is_empty());
}

#[tokio::test]
async fn a_program_starts_a_waiting_print_only_when_the_machine_lets_it() {
    let (s, boss, key) = ready("cw-start", json!({})).await;
    upload(
        &s,
        "/server/files/upload",
        &key,
        "part.gcode",
        PROGRAM.as_bytes(),
    )
    .await;
    // compat_start is off by default: a person starts the print with the routes of the server
    let (status, body) = post(&s, "/printer/print/start?filename=part.gcode", Some(&key)).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden")),
        "{body}"
    );
    let (status, body) = octo_command(&s, &key, json!({"command": "start"})).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    assert_eq!(state_of_first(&s, &boss, "p1").await, "queued");
    // the machine allows it
    let (status, body) = call(
        &s,
        Method::PUT,
        "/api/v1/work/machines/p1",
        Some(&boss),
        Some(json!({"settings": {"compat": "both", "sim_rate": 0, "compat_start": true}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // a file that was never uploaded
    let (status, body) = post(
        &s,
        "/printer/print/start?filename=nothing.gcode",
        Some(&key),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_job"))
    );
    let (status, body) = post(&s, "/printer/print/start?filename=part.gcode", Some(&key)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], "ok");
    // the simulator prints at once
    let (_, job) = call(
        &s,
        Method::GET,
        &format!(
            "/api/v1/work/jobs/{}",
            jobs(&s, &boss, "p1").await[0]["id"].as_str().unwrap()
        ),
        Some(&boss),
        None,
    )
    .await;
    assert!(
        matches!(job["state"].as_str(), Some("running" | "done")),
        "{job}"
    );
    // OctoPrint starts the file that waited last, or the one it names
    upload(
        &s,
        "/api/files/local",
        &key,
        "second.gcode",
        PROGRAM.as_bytes(),
    )
    .await;
    let (status, _) = octo_command(&s, &key, json!({"command": "start"})).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = octo_command(&s, &key, json!({"command": "restart"})).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_IMPLEMENTED, Some("not_supported"))
    );
}

#[tokio::test]
async fn a_print_that_runs_is_held_gone_on_and_cancelled_through_both_shapes() {
    // a slow printer: the lines take an hour each, so the print is in the middle when it is asked
    let (s, boss, key) = ready(
        "cw-hold",
        json!({"sim_rate": 1, "step_ms": 3_600_000, "compat_start": true}),
    )
    .await;
    // nothing runs yet
    let (status, body) = post(&s, "/printer/print/pause", Some(&key)).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("not_in_state")),
        "{body}"
    );
    upload(
        &s,
        "/server/files/upload",
        &key,
        "part.gcode",
        PROGRAM.as_bytes(),
    )
    .await;
    let (status, _) = post(&s, "/printer/print/start?filename=part.gcode", Some(&key)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state_of_first(&s, &boss, "p1").await, "running");
    let (status, _) = post(&s, "/printer/print/pause", Some(&key)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state_of_first(&s, &boss, "p1").await, "paused");
    let (status, _) = post(&s, "/printer/print/pause", Some(&key)).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a print that is held cannot be held again"
    );
    let (status, _) = post(&s, "/printer/print/resume", Some(&key)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state_of_first(&s, &boss, "p1").await, "running");
    // OctoPrint: pause with an action, toggle, cancel
    let (status, _) = octo_command(&s, &key, json!({"command": "pause", "action": "pause"})).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(state_of_first(&s, &boss, "p1").await, "paused");
    let (status, _) = octo_command(&s, &key, json!({"command": "pause", "action": "toggle"})).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(state_of_first(&s, &boss, "p1").await, "running");
    let (status, _) = post(&s, "/printer/print/cancel", Some(&key)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state_of_first(&s, &boss, "p1").await, "cancelled");
    // the audit knows who did it: the person of the keys is an operator of the machine
    let (status, _) = call(
        &s,
        Method::DELETE,
        "/api/v1/work/machines/p1/compat/key",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post(&s, "/printer/print/cancel", Some(&key)).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNAUTHORIZED, Some("unauthorized"))
    );
}

#[tokio::test]
async fn the_shape_the_machine_gives_is_the_shape_its_write_routes_answer() {
    let (s, boss, key) = ready("cw-shape", json!({"compat": "moonraker"})).await;
    let (status, _) = upload(
        &s,
        "/server/files/upload",
        &key,
        "part.gcode",
        PROGRAM.as_bytes(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = upload(
        &s,
        "/api/files/local",
        &key,
        "part.gcode",
        PROGRAM.as_bytes(),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_IMPLEMENTED, Some("not_supported")),
        "{body}"
    );
    assert_eq!(jobs(&s, &boss, "p1").await.len(), 1);
}
