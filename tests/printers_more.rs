//! More 3D printers through the real router, all of them simulators: a Marlin printer on a simulated serial line, a Bambu Lab printer in LAN mode (an experiment whose
//! certificate is never accepted unless it is the pinned one), and the farm that shares a print among the free printers and leaves each copy waiting for a person.

mod common;

use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::{admin, call, user, Server};
use eh3d_server::printer_lan::sim_fingerprint;
use serde_json::{json, Value};

const PART: &str = "G28\nM104 S200\nM140 S60\n;LAYER_CHANGE\nG1 X10 Y10 Z0.2 F3000\nG1 X20 Y10\n;LAYER_CHANGE\nG1 X20 Y20 Z0.4\nM104 S0\nM140 S0\n";
const SERIAL: &str = "01P00A111111111";

async fn make_machine(s: &Server, boss: &str, id: &str, settings: Value) -> (StatusCode, Value) {
    call(
        s,
        Method::POST,
        "/api/v1/work/machines",
        Some(boss),
        Some(json!({"id": id, "name": id, "plugin": "printer3d", "settings": settings})),
    )
    .await
}

async fn prepare(s: &Server, token: &str, machine: &str, text: &str) -> (StatusCode, Value) {
    call(
        s,
        Method::POST,
        "/api/v1/work/jobs",
        Some(token),
        Some(json!({"machine": machine, "request": {"kind": "print", "text": text, "name": "part"}, "name": "run", "app": "studio"})),
    )
    .await
}

async fn act(s: &Server, token: &str, id: &str, what: &str) -> (StatusCode, Value) {
    call(
        s,
        Method::POST,
        &format!("/api/v1/work/jobs/{id}/{what}"),
        Some(token),
        None,
    )
    .await
}

async fn wait_for(s: &Server, token: &str, id: &str, wanted: &str) -> Value {
    let limit = Instant::now() + Duration::from_secs(60);
    loop {
        let (_, body) = call(
            s,
            Method::GET,
            &format!("/api/v1/work/jobs/{id}"),
            Some(token),
            None,
        )
        .await;
        if body["state"] == wanted {
            return body;
        }
        assert!(Instant::now() < limit, "waiting for {wanted}: {body}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn run_to_done(s: &Server, token: &str, machine: &str, text: &str) -> Value {
    let (status, job) = prepare(s, token, machine, text).await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let id = job["id"].as_str().unwrap().to_string();
    let (status, started) = act(s, token, &id, "start").await;
    assert_eq!(status, StatusCode::OK, "{started}");
    wait_for(s, token, &id, "done").await
}

#[tokio::test]
async fn a_marlin_printer_prints_through_the_queue_inside_the_limits_of_its_model() {
    let s = common::server("pm-marlin");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let (status, body) = make_machine(
        &s,
        &boss,
        "mar",
        json!({"connection": {"kind": "marlin", "port": "sim", "baud": 250000}, "sim_rate": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let done = run_to_done(&s, &operator, "mar", PART).await;
    assert_eq!(done["percent"], 100);
    let (_, machine) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/mar",
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(machine["detail"]["connection"], "marlin");
    assert_eq!(machine["detail"]["evidence"], "simulator");
    // the limits of the model hold whoever asks
    let (status, body) = prepare(&s, &operator, "mar", "M104 S300\nG1 X1 F3000\n").await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("program_over_limit")),
        "{body}"
    );
    let (status, body) = call(
        &s,
        Method::PUT,
        "/api/v1/work/machines/mar/temperatures",
        Some(&operator),
        Some(json!({"heater": "hotend", "target_c": 270})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Some("temperature_over_limit")
        ),
        "{body}"
    );
    let (status, body) = call(
        &s,
        Method::PUT,
        "/api/v1/work/machines/mar/temperatures",
        Some(&operator),
        Some(json!({"heater": "hotend", "target_c": 200})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // only the simulated serial line is served
    for bad in [
        json!({"connection": {"kind": "marlin", "port": "COM3"}}),
        json!({"connection": {"kind": "marlin"}}),
        json!({"connection": {"kind": "marlin", "port": "sim", "baud": 3}}),
    ] {
        let (status, body) = make_machine(&s, &boss, "mar2", bad).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_plugin_request")),
            "{body}"
        );
    }
}

#[tokio::test]
async fn a_bambu_printer_is_an_experiment_and_its_certificate_is_never_accepted_unchecked() {
    let s = common::server("pm-bambu");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let connection = |fingerprint: Option<String>| {
        let mut c =
            json!({"kind": "bambu_lan", "host": "sim", "serial": SERIAL, "api_key": "12345678"});
        if let Some(f) = fingerprint {
            c["fingerprint"] = json!(f);
        }
        c
    };
    // no fingerprint pinned: the machine is made, nothing is sent to it, and the refusal says what the printer showed
    let (status, made) = make_machine(
        &s,
        &boss,
        "b1",
        json!({"model": "bambu_a1", "connection": connection(None), "sim_rate": 0}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(
        made["settings"]["connection"]["api_key"], "********",
        "the access code is never shown"
    );
    let (_, machine) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/b1",
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(machine["detail"]["bambu_lan"]["experimental"], true);
    assert_eq!(machine["detail"]["bambu_lan"]["fingerprint_pinned"], false);
    assert_eq!(machine["detail"]["online"], false);
    let (status, job) = prepare(&s, &operator, "b1", PART).await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let id = job["id"].as_str().unwrap().to_string();
    let (status, refused) = act(&s, &operator, &id, "start").await;
    assert_eq!(
        (status, refused["error"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("printer_untrusted")),
        "{refused}"
    );
    let shown = sim_fingerprint(SERIAL);
    assert!(
        refused["detail"].as_str().unwrap().contains(&shown),
        "{refused}"
    );
    // another certificate than the pinned one
    let wrong = sim_fingerprint("OTHER000000000");
    let (status, _) = call(
        &s,
        Method::PUT,
        "/api/v1/work/machines/b1",
        Some(&boss),
        Some(json!({"settings": {"model": "bambu_a1", "connection": connection(Some(wrong)), "sim_rate": 0}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, refused) = act(&s, &operator, &id, "start").await;
    assert_eq!(
        (status, refused["error"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("printer_untrusted")),
        "{refused}"
    );
    assert!(
        refused["detail"]
            .as_str()
            .unwrap()
            .contains("not the pinned one"),
        "{refused}"
    );
    // a fingerprint that is not 64 hexadecimal digits is refused when the machine is set
    let (status, body) = make_machine(
        &s,
        &boss,
        "b2",
        json!({"connection": connection(Some("abc".into()))}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_plugin_request")),
        "{body}"
    );
    // the pinned one, with the colons a screen shows
    let pinned: Vec<String> = shown
        .as_bytes()
        .chunks(2)
        .map(|c| String::from_utf8_lossy(c).to_uppercase())
        .collect();
    let (status, _) = call(
        &s,
        Method::PUT,
        "/api/v1/work/machines/b1",
        Some(&boss),
        Some(json!({"settings": {"model": "bambu_a1", "connection": connection(Some(pinned.join(":"))), "sim_rate": 0}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, started) = act(&s, &operator, &id, "start").await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let done = wait_for(&s, &operator, &id, "done").await;
    assert_eq!(done["percent"], 100);
    // the limits of the model (nozzle 300, bed 100) hold
    let (status, body) = prepare(&s, &operator, "b1", "M104 S310\nG1 X1 F3000\n").await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("program_over_limit")),
        "{body}"
    );
    let (status, _) = prepare(&s, &operator, "b1", "M104 S300\nM140 S100\nG1 X1 F3000\n").await;
    assert_eq!(status, StatusCode::CREATED);
}

async fn farm(s: &Server, token: &str, body: Value) -> (StatusCode, Value) {
    call(
        s,
        Method::POST,
        "/api/v1/work/farm/jobs",
        Some(token),
        Some(body),
    )
    .await
}

#[tokio::test]
async fn the_farm_shares_a_print_among_the_free_printers_and_leaves_each_copy_waiting() {
    let s = common::server("pm-farm");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let viewer = user(&s, &boss, "view1", "viewer").await;
    // slow printers: a print that is started stays running
    let slow = json!({"sim_rate": 1, "step_ms": 3_600_000});
    for id in ["g1", "g2"] {
        assert_eq!(
            make_machine(&s, &boss, id, slow.clone()).await.0,
            StatusCode::CREATED
        );
    }
    let mut bambu = slow.clone();
    bambu["model"] = json!("bambu_a1");
    assert_eq!(
        make_machine(&s, &boss, "b1", bambu).await.0,
        StatusCode::CREATED
    );
    let request = json!({"kind": "print", "text": PART, "name": "part"});
    // a viewer cannot
    let (status, _) = farm(&s, &viewer, json!({"request": request})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // two copies go to the first two free printers, and wait
    let (status, shared) = farm(&s, &operator, json!({"request": request, "copies": 2})).await;
    assert_eq!(status, StatusCode::CREATED, "{shared}");
    let machines: Vec<&str> = shared["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["machine"].as_str().unwrap())
        .collect();
    assert_eq!(machines, vec!["g1", "g2"]);
    assert_eq!(shared["unplaced"], 0);
    assert!(shared["confirm"]
        .as_str()
        .unwrap()
        .contains("a person starts it"));
    for job in shared["jobs"].as_array().unwrap() {
        assert_eq!(job["state"], "queued", "nothing starts by itself");
    }
    // a printer with a job waiting is not free: the next copy goes to the third
    let (status, shared) = farm(&s, &operator, json!({"request": request, "copies": 3})).await;
    assert_eq!(status, StatusCode::CREATED, "{shared}");
    assert_eq!(shared["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(shared["jobs"][0]["machine"], "b1");
    assert_eq!(shared["unplaced"], 2);
    // none is free now
    let (status, body) = farm(&s, &operator, json!({"request": request})).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("farm_no_free_printer")),
        "{body}"
    );
    // a person starts the one on g1: it runs, and the farm still leaves the others alone
    let (_, list) = call(
        &s,
        Method::GET,
        "/api/v1/work/jobs?machine=g1",
        Some(&operator),
        None,
    )
    .await;
    let id = list["items"][0]["id"].as_str().unwrap().to_string();
    let (status, _) = act(&s, &operator, &id, "start").await;
    assert_eq!(status, StatusCode::OK);
    wait_for(&s, &operator, &id, "running").await;
    let (_, machine) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/g1",
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(machine["state"], "busy");
    // a copy that no printer takes is not placed, and the answer says why
    let (status, body) = farm(
        &s,
        &operator,
        json!({"request": {"kind": "print", "text": "M104 S400\nG1 X1 F3000\n"}}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("farm_no_free_printer")),
        "{body}"
    );
    // the list narrows the choice and sets the order; a printer that is not there is refused
    let (status, body) = farm(
        &s,
        &operator,
        json!({"request": request, "machines": ["nothing"]}),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_machine")),
        "{body}"
    );
    for copies in [0, 17] {
        let (status, body) =
            farm(&s, &operator, json!({"request": request, "copies": copies})).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_plugin_request")),
            "{body}"
        );
    }
}

#[tokio::test]
async fn the_farm_skips_a_printer_whose_model_refuses_the_program_and_says_so() {
    let s = common::server("pm-farm-limits");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    assert_eq!(
        make_machine(&s, &boss, "g1", json!({"sim_rate": 0}))
            .await
            .0,
        StatusCode::CREATED
    );
    assert_eq!(
        make_machine(&s, &boss, "b1", json!({"model": "bambu_a1", "sim_rate": 0}))
            .await
            .0,
        StatusCode::CREATED
    );
    // 280 C is over the 260 C of the generic printer and inside the 300 C of the A1
    let hot = "G28\nM104 S280\nG1 X10 Y10 F3000\nM104 S0\n";
    let (status, shared) = farm(
        &s,
        &operator,
        json!({"request": {"kind": "print", "text": hot, "name": "hot"}, "machines": ["g1", "b1"]}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{shared}");
    assert_eq!(shared["jobs"][0]["machine"], "b1");
    let tried = shared["tried"].as_array().unwrap();
    assert_eq!(tried.len(), 1);
    assert_eq!(
        (tried[0]["machine"].clone(), tried[0]["error"].clone()),
        (json!("g1"), json!("program_over_limit"))
    );
}
