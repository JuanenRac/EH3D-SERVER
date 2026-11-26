//! The nodes of the pick-and-place machines (a feeder node and a head node) through the real router, against nodes in memory: each is a small server on `127.0.0.1` with a
//! port the system gives, that answers the words of the contract of its node (`GET /api/status` open, the reads and the orders with the token) from a model of the node.
//! No test opens a real network, and nothing here has run on a real feeder or head.

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::{admin, call, server_with, user, Server};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The model of a node: what it holds, how it is, and how it answers.
struct Model {
    kind: &'static str,
    token: String,
    /// The node does not answer at all (the connection is closed).
    mute: bool,
    /// The node answers with something that is not its contract.
    junk: bool,
    safe: bool,
    selfcheck: &'static str,
    /// The tape left in each feeder slot, or the vacuum of each nozzle fault.
    remaining: Vec<u32>,
    nozzle_faults: Vec<bool>,
    /// How many requests came with the right token, and how many for the status.
    heard: u32,
    status_hits: u32,
    /// The answer to the first question of the status is held this long, as on a node that takes long to give one.
    hold_first_ms: u64,
    /// The time the node says in its status, when it says one.
    link_lost_ms: Option<u64>,
}

type Node = Arc<Mutex<Model>>;

fn model(kind: &'static str, token: &str) -> Node {
    Arc::new(Mutex::new(Model {
        kind,
        token: token.to_string(),
        mute: false,
        junk: false,
        safe: false,
        selfcheck: "ok",
        remaining: vec![40, 40, 40],
        nozzle_faults: vec![false, false],
        heard: 0,
        status_hits: 0,
        hold_first_ms: 0,
        link_lost_ms: None,
    }))
}

fn status_json(m: &Model) -> Value {
    if m.kind == "feeder" {
        let feeders: Vec<Value> = m
            .remaining
            .iter()
            .enumerate()
            .map(|(i, r)| {
                json!({
                    "slot": i + 1, "online": true, "state": if *r == 0 { "empty" } else { "idle" },
                    "fault": "none", "remaining": r, "low": *r <= 5,
                })
            })
            .collect();
        json!({
            "node": "feeder-sim", "version": "0.0.1", "slots": m.remaining.len(), "online": m.remaining.len(),
            "safe": m.safe, "safe_reason": if m.safe { "server_lost" } else { "none" }, "bus": "rs485",
            "uptime_s": 7, "selfcheck": m.selfcheck, "selfcheck_codes": "", "feeders": feeders,
        })
    } else {
        let nozzles: Vec<Value> = m
            .nozzle_faults
            .iter()
            .enumerate()
            .map(|(i, f)| {
                json!({
                    "n": i + 1, "phase": if *f { "fault" } else { "idle" }, "fault": if *f { "HEAD-S01" } else { "none" },
                    "alarm": "none", "part": false, "vacuum_ok": false, "raised": true, "kpa": null,
                })
            })
            .collect();
        let mut status = json!({
            "node": "head-sim", "version": "0.0.1", "nozzle_count": m.nozzle_faults.len(), "safe": m.safe,
            "safe_reason": if m.safe { "server_lost" } else { "none" }, "safe_code": if m.safe { "HEAD-L01" } else { "none" },
            "pump": false, "xy_clear": true, "xy_moving": false, "z_safe_mm": 10.0, "calibrated": false,
            "uptime_s": 7, "selfcheck": m.selfcheck, "selfcheck_codes": "", "nozzles": nozzles,
        });
        if let Some(ms) = m.link_lost_ms {
            status["link_lost_ms"] = json!(ms);
        }
        status
    }
}

/// Starts the node on a port the system gives; the address to register is returned. The task ends with the test (the runtime is dropped).
async fn start_node(node: Node) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let node = node.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let path = text.split_whitespace().nth(1).unwrap_or("").to_string();
                let bearer = text
                    .lines()
                    .find_map(|l| l.strip_prefix("Authorization: Bearer "))
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let hold = {
                    let m = node.lock().unwrap();
                    if m.status_hits == 0 && path == "/api/status" {
                        m.hold_first_ms
                    } else {
                        0
                    }
                };
                let (code, body) = {
                    let mut m = node.lock().unwrap();
                    if m.mute {
                        return;
                    }
                    let authorised = !m.token.is_empty() && bearer == m.token;
                    if authorised {
                        m.heard += 1;
                    }
                    if path == "/api/status" {
                        m.status_hits += 1;
                        if m.junk {
                            (200, "not json at all".to_string())
                        } else {
                            (200, status_json(&m).to_string())
                        }
                    } else if path.starts_with("/api/feeder") || path.starts_with("/api/nozzle") {
                        if authorised {
                            (200, json!({"ok": true}).to_string())
                        } else {
                            (401, json!({"error": "unauthorized"}).to_string())
                        }
                    } else {
                        (404, json!({"error": "not_found"}).to_string())
                    }
                };
                tokio::time::sleep(Duration::from_millis(hold)).await;
                let reply = format!(
                    "HTTP/1.0 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    address
}

fn nodes_on(name: &str) -> Server {
    server_with(name, |c| {
        c.pnp_nodes_enabled = true;
        c.pnp_node_poll_s = 1;
        c.pnp_node_silence_s = 2;
        c.pnp_node_timeout_s = 2;
    })
}

async fn register(s: &Server, boss: &str, body: Value) -> Value {
    let (status, v) = call(s, Method::POST, "/api/v1/pnp/nodes", Some(boss), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    v
}

async fn poll(s: &Server, token: &str, id: &str) -> Value {
    let (status, v) = call(
        s,
        Method::POST,
        &format!("/api/v1/pnp/nodes/{id}/poll"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v
}

async fn make_machine(s: &Server, boss: &str, id: &str) {
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/work/machines",
        Some(boss),
        Some(json!({"id": id, "name": id, "plugin": "pnp", "settings": {"sim_rate": 0}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn run_job(s: &Server, token: &str, machine: &str, request: Value) -> (StatusCode, Value) {
    let (status, job) = call(
        s,
        Method::POST,
        "/api/v1/work/jobs",
        Some(token),
        Some(json!({"machine": machine, "request": request, "name": "run", "app": "studio"})),
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
    if status != StatusCode::OK {
        return (status, started);
    }
    let limit = Instant::now() + Duration::from_secs(30);
    loop {
        let (_, body) = call(
            s,
            Method::GET,
            &format!("/api/v1/work/jobs/{id}"),
            Some(token),
            None,
        )
        .await;
        if body["state"] == "done" {
            return (StatusCode::OK, body);
        }
        assert!(Instant::now() < limit, "waiting for done: {body}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

#[tokio::test]
async fn off_by_default_the_routes_answer_501_and_the_fleet_is_as_it_was() {
    let s = common::server("pnpn-off");
    let boss = admin(&s).await;
    for (method, path, body) in [
        (Method::GET, "/api/v1/pnp/nodes", None),
        (
            Method::POST,
            "/api/v1/pnp/nodes",
            Some(json!({"kind": "head", "name": "h", "address": "127.0.0.1:1"})),
        ),
        (Method::GET, "/api/v1/pnp/nodes/head-1", None),
        (
            Method::PUT,
            "/api/v1/pnp/nodes/head-1",
            Some(json!({"enabled": false})),
        ),
        (Method::DELETE, "/api/v1/pnp/nodes/head-1", None),
        (Method::POST, "/api/v1/pnp/nodes/head-1/poll", None),
    ] {
        let (status, v) = call(&s, method.clone(), path, Some(&boss), body).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{method} {path}: {v}");
        assert_eq!(v["error"], "pnp_nodes_off");
    }
    let (_, fleet) = call(&s, Method::GET, "/api/v1/fleet", Some(&boss), None).await;
    assert!(fleet.get("pnp_nodes").is_none(), "{fleet}");
}

#[tokio::test]
async fn the_roles_and_the_scopes_of_the_routes_and_the_audit_without_the_token() {
    let s = nodes_on("pnpn-roles");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let viewer = user(&s, &boss, "view1", "viewer").await;
    let body = json!({"kind": "feeder", "name": "Feeders left", "address": "127.0.0.1:9", "token": "tok-secret-1"});
    for who in [&operator, &viewer] {
        let (status, v) = call(
            &s,
            Method::POST,
            "/api/v1/pnp/nodes",
            Some(who),
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{v}");
    }
    let (status, _) = call(&s, Method::GET, "/api/v1/pnp/nodes", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let made = register(&s, &boss, body).await;
    assert_eq!(made["id"], "feeder-1");
    assert_eq!(made["token"], "********", "{made}");
    assert_eq!(
        (made["health"].clone(), made["ready"].clone()),
        (json!("unknown"), json!(false))
    );
    assert_eq!(made["evidence"], "simulator");
    // a viewer reads; a viewer and an operator do not change or remove
    let (status, list) = call(&s, Method::GET, "/api/v1/pnp/nodes", Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(list["summary"]["registered"], 1);
    assert_eq!(list["limits"]["kinds"], json!(["feeder", "head"]));
    for who in [&operator, &viewer] {
        let (status, _) = call(
            &s,
            Method::PUT,
            "/api/v1/pnp/nodes/feeder-1",
            Some(who),
            Some(json!({"name": "x"})),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = call(
            &s,
            Method::DELETE,
            "/api/v1/pnp/nodes/feeder-1",
            Some(who),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    // a viewer does not ask a node; a token limited to other scopes does not see them
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/pnp/nodes/feeder-1/poll",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let narrow = common::scoped_token(&s, &boss, "viewer", &["tasks:read"]).await;
    let (status, v) = call(&s, Method::GET, "/api/v1/pnp/nodes", Some(&narrow), None).await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::FORBIDDEN, json!("forbidden_scope"))
    );
    // unknown things and bad bodies
    let (status, v) = call(
        &s,
        Method::GET,
        "/api/v1/pnp/nodes/nope",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::NOT_FOUND, json!("unknown_pnp_node"))
    );
    let (status, v) = call(
        &s,
        Method::POST,
        "/api/v1/pnp/nodes",
        Some(&boss),
        Some(json!({"kind": "robot", "name": "r", "address": "127.0.0.1:2"})),
    )
    .await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::UNPROCESSABLE_ENTITY, json!("bad_pnp_node"))
    );
    let (status, v) = call(
        &s,
        Method::POST,
        "/api/v1/pnp/nodes",
        Some(&boss),
        Some(json!({"kind": "head", "name": "h", "address": "127.0.0.1:3", "machine": "ghost"})),
    )
    .await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::UNPROCESSABLE_ENTITY, json!("bad_pnp_node")),
        "{v}"
    );
    let (status, v) = call(
        &s,
        Method::POST,
        "/api/v1/pnp/nodes",
        Some(&boss),
        Some(json!({"kind": "head", "name": "h", "address": "127.0.0.1:9"})),
    )
    .await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::CONFLICT, json!("pnp_node_exists"))
    );
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/pnp/nodes",
        Some(&boss),
        Some(json!({"kind": "head", "name": "h", "address": "127.0.0.1:4", "surplus": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // change, keep the token with the stars, switch off and remove
    let (status, v) = call(
        &s,
        Method::PUT,
        "/api/v1/pnp/nodes/feeder-1",
        Some(&boss),
        Some(json!({"name": "Renamed", "token": "********", "enabled": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        (v["name"].clone(), v["health"].clone(), v["token"].clone()),
        (json!("Renamed"), json!("disabled"), json!("********"))
    );
    let (status, v) = call(
        &s,
        Method::POST,
        "/api/v1/pnp/nodes/feeder-1/poll",
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::CONFLICT, json!("pnp_node_disabled"))
    );
    let (status, _) = call(
        &s,
        Method::DELETE,
        "/api/v1/pnp/nodes/feeder-1",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // the audit knows who did what and has no token in it
    let (_, audit) = call(&s, Method::GET, "/api/v1/audit", Some(&boss), None).await;
    let text = audit.to_string();
    for event in ["pnp_node_add", "pnp_node_change", "pnp_node_remove"] {
        assert!(text.contains(event), "{event} is not in the audit: {text}");
    }
    assert!(!text.contains("tok-secret-1"));
}

#[tokio::test]
async fn a_feeder_node_and_a_head_node_are_polled_with_their_token_and_summed_up() {
    let s = nodes_on("pnpn-poll");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let feeder = model("feeder", "feeder-token");
    feeder.lock().unwrap().remaining = vec![40, 3, 0];
    let head = model("head", "head-token");
    {
        let mut h = head.lock().unwrap();
        h.selfcheck = "warn";
        h.nozzle_faults = vec![false, true];
    }
    let fa = start_node(feeder.clone()).await;
    let ha = start_node(head.clone()).await;
    register(
        &s,
        &boss,
        json!({"kind": "feeder", "name": "Feeders", "address": fa, "token": "feeder-token"}),
    )
    .await;
    register(
        &s,
        &boss,
        json!({"kind": "head", "name": "Head", "address": ha, "token": "head-token"}),
    )
    .await;
    let f = poll(&s, &operator, "feeder-1").await;
    assert_eq!(
        (f["health"].clone(), f["ready"].clone()),
        (json!("ok"), json!(true)),
        "{f}"
    );
    let (low, empty) = (f["status"]["low"].clone(), f["status"]["empty"].clone());
    assert_eq!((low, empty), (json!(2), json!(1)));
    assert_eq!(f["polls"], json!({"ok": 1, "failed": 0}));
    let h = poll(&s, &operator, "head-1").await;
    assert_eq!(
        (h["health"].clone(), h["ready"].clone()),
        (json!("warn"), json!(true)),
        "{h}"
    );
    assert_eq!(h["status"]["faulted"], 1);
    assert_eq!(h["status"]["nozzles"].as_array().unwrap().len(), 2);
    // the server asked with the token, once for the status and once for a read that needs it
    for (node, hits) in [(&feeder, 1), (&head, 1)] {
        let m = node.lock().unwrap();
        assert_eq!(
            (m.heard, m.status_hits),
            (hits * 2, hits),
            "the status is open and the read needs the token"
        );
    }
    // the token is nowhere in what the server shows
    let (_, list) = call(&s, Method::GET, "/api/v1/pnp/nodes", Some(&operator), None).await;
    let text = list.to_string();
    assert!(!text.contains("feeder-token") && !text.contains("head-token"));
    assert_eq!(list["summary"]["ready"], 2);
    // the fleet counts them
    let (_, fleet) = call(&s, Method::GET, "/api/v1/fleet", Some(&operator), None).await;
    assert_eq!(
        (
            fleet["pnp_nodes"]["registered"].clone(),
            fleet["pnp_nodes"]["not_ready"].clone()
        ),
        (json!(2), json!(0))
    );
}

#[tokio::test]
async fn a_node_that_refuses_the_token_says_something_that_is_not_its_contract_or_goes_quiet_is_not_ready(
) {
    let s = nodes_on("pnpn-bad");
    let boss = admin(&s).await;
    let node = model("head", "right-token");
    let a = start_node(node.clone()).await;
    register(
        &s,
        &boss,
        json!({"kind": "head", "name": "Head", "address": a, "token": "wrong-token"}),
    )
    .await;
    let v = poll(&s, &boss, "head-1").await;
    assert_eq!(
        (
            v["health"].clone(),
            v["ready"].clone(),
            v["last_error"].clone()
        ),
        (json!("refused"), json!(false), json!("token_rejected")),
        "{v}"
    );
    // the right token
    call(
        &s,
        Method::PUT,
        "/api/v1/pnp/nodes/head-1",
        Some(&boss),
        Some(json!({"token": "right-token"})),
    )
    .await;
    assert_eq!(poll(&s, &boss, "head-1").await["health"], "ok");
    // an answer that is not the contract
    node.lock().unwrap().junk = true;
    let v = poll(&s, &boss, "head-1").await;
    assert_eq!(
        (
            v["health"].clone(),
            v["last_error"].clone(),
            v["status"].clone()
        ),
        (json!("unreachable"), json!("bad_answer"), Value::Null),
        "{v}"
    );
    node.lock().unwrap().junk = false;
    assert_eq!(poll(&s, &boss, "head-1").await["health"], "ok");
    // the node says it is safe, or fails its own check
    node.lock().unwrap().safe = true;
    assert_eq!(poll(&s, &boss, "head-1").await["health"], "safe");
    {
        let mut m = node.lock().unwrap();
        m.safe = false;
        m.selfcheck = "fail";
    }
    assert_eq!(poll(&s, &boss, "head-1").await["health"], "fail");
    node.lock().unwrap().selfcheck = "ok";
    assert_eq!(poll(&s, &boss, "head-1").await["health"], "ok");
    // it goes mute: unreachable at first, silent after the time without an answer
    node.lock().unwrap().mute = true;
    let v = poll(&s, &boss, "head-1").await;
    assert_eq!(
        (v["health"].clone(), v["ready"].clone()),
        (json!("unreachable"), json!(false)),
        "{v}"
    );
    tokio::time::sleep(Duration::from_millis(2300)).await;
    let (_, v) = call(
        &s,
        Method::GET,
        "/api/v1/pnp/nodes/head-1",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(
        (v["health"].clone(), v["ready"].clone()),
        (json!("silent"), json!(false)),
        "{v}"
    );
    // a node nobody listens to at all
    let dead = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().to_string()
    };
    register(
        &s,
        &boss,
        json!({"kind": "feeder", "name": "Gone", "address": dead, "token": "t"}),
    )
    .await;
    let v = poll(&s, &boss, "feeder-1").await;
    assert_eq!(
        (v["health"].clone(), v["last_error"].clone()),
        (json!("unreachable"), json!("unreachable")),
        "{v}"
    );
}

#[tokio::test]
async fn a_pick_and_place_machine_does_not_start_while_a_node_of_it_is_not_ready() {
    let s = nodes_on("pnpn-hold");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make_machine(&s, &boss, "p1").await;
    let head = model("head", "tok");
    let feeder = model("feeder", "tok");
    let ha = start_node(head.clone()).await;
    let fa = start_node(feeder.clone()).await;
    register(
        &s,
        &boss,
        json!({"kind": "head", "name": "Head", "address": ha, "token": "tok", "machine": "p1"}),
    )
    .await;
    register(&s, &boss, json!({"kind": "feeder", "name": "Feeders", "address": fa, "token": "tok", "machine": "p1"})).await;
    // nobody asked them yet: unknown holds the machine
    let load = json!({"kind": "load_feeders", "feeders": [{"slot": 1, "part": "R_0603_10k", "count": 10}]});
    let (status, v) = run_job(&s, &operator, "p1", load.clone()).await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::CONFLICT, json!("pnp_node_not_ready")),
        "{v}"
    );
    assert!(v["detail"].as_str().unwrap().contains("unknown"));
    // the detail of the machine lists its nodes
    let (_, m) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/p1",
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(m["detail"]["nodes"].as_array().unwrap().len(), 2, "{m}");
    poll(&s, &operator, "head-1").await;
    poll(&s, &operator, "feeder-1").await;
    // both ready: the job runs
    let (status, v) = run_job(&s, &operator, "p1", load.clone()).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    // the head says it is safe: the machine is held with the reason
    head.lock().unwrap().safe = true;
    poll(&s, &operator, "head-1").await;
    let (status, v) = run_job(&s, &operator, "p1", load.clone()).await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::CONFLICT, json!("pnp_node_not_ready")),
        "{v}"
    );
    assert!(
        v["detail"].as_str().unwrap().contains("head-1 is safe"),
        "{v}"
    );
    // a person acknowledged it on the node, the server asks again and the machine goes on
    head.lock().unwrap().safe = false;
    poll(&s, &operator, "head-1").await;
    let (status, _) = run_job(&s, &operator, "p1", load.clone()).await;
    assert_eq!(status, StatusCode::OK);
    // a node switched off does not hold the machine
    head.lock().unwrap().safe = true;
    poll(&s, &operator, "head-1").await;
    call(
        &s,
        Method::PUT,
        "/api/v1/pnp/nodes/head-1",
        Some(&boss),
        Some(json!({"enabled": false})),
    )
    .await;
    let (status, _) = run_job(&s, &operator, "p1", load).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_clients_are_told_when_a_node_changes_its_health() {
    let s = nodes_on("pnpn-event");
    let boss = admin(&s).await;
    let node = model("feeder", "tok");
    let a = start_node(node.clone()).await;
    register(
        &s,
        &boss,
        json!({"kind": "feeder", "name": "Feeders", "address": a, "token": "tok"}),
    )
    .await;
    let mut events = s.state.events.subscribe();
    poll(&s, &boss, "feeder-1").await;
    let text = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    let event: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        event,
        json!({"type": "pnp_node", "id": "feeder-1", "health": "ok", "ready": true})
    );
    // the same health again is not told again
    poll(&s, &boss, "feeder-1").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), events.recv())
            .await
            .is_err()
    );
    node.lock().unwrap().safe = true;
    poll(&s, &boss, "feeder-1").await;
    let text = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    let event: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        (event["health"].clone(), event["ready"].clone()),
        (json!("safe"), json!(false))
    );
}

#[tokio::test]
async fn the_poller_asks_the_nodes_by_itself_while_the_switch_is_on_and_not_at_all_when_it_is_off()
{
    let node = model("feeder", "tok");
    let a = start_node(node.clone()).await;
    let off = server_with("pnpn-poller-off", |c| c.pnp_node_poll_s = 1);
    assert!(eh3d_server::pnp_nodes::spawn(off.state.clone()).is_none());
    let s = nodes_on("pnpn-poller");
    let boss = admin(&s).await;
    register(
        &s,
        &boss,
        json!({"kind": "feeder", "name": "Feeders", "address": a, "token": "tok"}),
    )
    .await;
    let handle = eh3d_server::pnp_nodes::spawn(s.state.clone()).expect("the poller starts");
    let limit = Instant::now() + Duration::from_secs(8);
    loop {
        let (_, v) = call(
            &s,
            Method::GET,
            "/api/v1/pnp/nodes/feeder-1",
            Some(&boss),
            None,
        )
        .await;
        if v["health"] == "ok" {
            break;
        }
        assert!(
            Instant::now() < limit,
            "the poller did not ask the node: {v}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    handle.abort();
    assert!(node.lock().unwrap().heard >= 1);
}

#[tokio::test]
async fn the_nodes_go_on_being_asked_while_an_answer_is_still_awaited() {
    let slow = model("head", "tok");
    slow.lock().unwrap().hold_first_ms = 6_000;
    let other = model("head", "tok");
    let a = start_node(slow.clone()).await;
    let b = start_node(other.clone()).await;
    let s = server_with("pnpn-held", |c| {
        c.pnp_nodes_enabled = true;
        c.pnp_node_timeout_s = 10;
        assert_eq!(c.pnp_node_poll_s, 1, "the interval a head can live with");
    });
    let boss = admin(&s).await;
    for address in [a, b] {
        register(
            &s,
            &boss,
            json!({"kind": "head", "name": "Head", "address": address, "token": "tok"}),
        )
        .await;
    }
    let handle = eh3d_server::pnp_nodes::spawn(s.state.clone()).expect("the poller starts");
    // well before the held answer comes: the same node has been asked again on other connections, and so has the other one
    tokio::time::sleep(Duration::from_millis(4_500)).await;
    let (held, free) = (
        slow.lock().unwrap().status_hits,
        other.lock().unwrap().status_hits,
    );
    handle.abort();
    assert!(held >= 3, "the node with the held answer was asked {held} times");
    assert!(free >= 3, "the other node was asked {free} times");
}

#[tokio::test]
async fn an_interval_that_is_too_slow_for_a_node_is_said() {
    let node = model("head", "tok");
    let a = start_node(node.clone()).await;
    let s = nodes_on("pnpn-slow");
    let boss = admin(&s).await;
    register(
        &s,
        &boss,
        json!({"kind": "head", "name": "Head", "address": a, "token": "tok"}),
    )
    .await;
    assert!(s.state.pnp_nodes.startup_warnings().is_empty());
    let v = poll(&s, &boss, "head-1").await;
    assert_eq!(
        (&v["link_lost_ms"], &v["link_lost_declared"], &v["poll_too_slow"]),
        (&json!(2000), &json!(false), &json!(false))
    );
    // the node says a time shorter than the interval
    node.lock().unwrap().link_lost_ms = Some(1000);
    let v = poll(&s, &boss, "head-1").await;
    assert_eq!(
        (&v["link_lost_ms"], &v["link_lost_declared"], &v["poll_too_slow"]),
        (&json!(1000), &json!(true), &json!(true))
    );
    let (status, list) = call(&s, Method::GET, "/api/v1/pnp/nodes", Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["warnings"][0]["code"], "poll_too_slow");
    assert_eq!(list["warnings"][0]["node"], "head-1");
    assert_eq!(s.state.pnp_nodes.startup_warnings().len(), 1);

    // two seconds between questions is already too slow for a head with the usual time
    let slow = server_with("pnpn-slow-start", |c| {
        c.pnp_nodes_enabled = true;
        c.pnp_node_poll_s = 2;
    });
    assert!(slow.state.pnp_nodes.startup_warnings()[0].contains("pnp_node_poll_s is 2"));
    let off = server_with("pnpn-slow-off", |c| c.pnp_node_poll_s = 2);
    assert!(off.state.pnp_nodes.startup_warnings().is_empty());
}

#[tokio::test]
async fn with_a_fixed_seed_the_health_follows_the_node_through_a_made_up_life() {
    let s = nodes_on("pnpn-seed");
    let boss = admin(&s).await;
    let node = model("feeder", "tok");
    let a = start_node(node.clone()).await;
    register(
        &s,
        &boss,
        json!({"kind": "feeder", "name": "Feeders", "address": a, "token": "tok"}),
    )
    .await;
    // the same 120 steps on every run
    let mut seed: u64 = 0xD03_5EED;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for step in 0..120 {
        let (expect_ready, low, empty) = {
            let mut m = node.lock().unwrap();
            match next() % 5 {
                0 => m.safe = !m.safe,
                1 => m.selfcheck = ["ok", "warn", "fail"][(next() % 3) as usize],
                2 => {
                    let slot = (next() % 3) as usize;
                    m.remaining[slot] = (next() % 9) as u32;
                }
                3 => m.junk = next() % 4 == 0,
                _ => {}
            }
            let low = m.remaining.iter().filter(|r| **r <= 5).count();
            let empty = m.remaining.iter().filter(|r| **r == 0).count();
            (!m.junk && !m.safe && m.selfcheck != "fail", low, empty)
        };
        eh3d_server::pnp_nodes::poll_node(&s.state, "feeder-1").await;
        let (_, v) = call(
            &s,
            Method::GET,
            "/api/v1/pnp/nodes/feeder-1",
            Some(&boss),
            None,
        )
        .await;
        assert_eq!(v["ready"], json!(expect_ready), "step {step}: {v}");
        if !node.lock().unwrap().junk {
            assert_eq!(
                (v["status"]["low"].clone(), v["status"]["empty"].clone()),
                (json!(low), json!(empty)),
                "step {step}"
            );
        }
    }
}
