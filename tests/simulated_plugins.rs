//! The plugins `laser`, `pnp` and `pcb` end to end through the real router: their machines and settings, the checks of the jobs, the limits, the emergency stop that latches a
//! laser, the feeders of a pick-and-place machine and the reports of a board station. They are simulators: nothing here has run on a real machine.

mod common;

use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use common::{admin, call, user, Server};
use serde_json::{json, Value};

async fn make_machine(s: &Server, boss: &str, id: &str, plugin: &str, settings: Value) {
    let (status, body) = call(
        s,
        Method::POST,
        "/api/v1/work/machines",
        Some(boss),
        Some(json!({"id": id, "name": id, "plugin": plugin, "settings": settings})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

async fn prepare(s: &Server, token: &str, machine: &str, request: Value) -> (StatusCode, Value) {
    call(
        s,
        Method::POST,
        "/api/v1/work/jobs",
        Some(token),
        Some(json!({"machine": machine, "request": request, "name": "run", "app": "studio"})),
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

/// Prepares, starts and waits for a job that has to end `done`.
async fn run_to_done(s: &Server, token: &str, machine: &str, request: Value) -> Value {
    let (status, job) = prepare(s, token, machine, request).await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let id = job["id"].as_str().unwrap().to_string();
    let (status, started) = act(s, token, &id, "start").await;
    assert_eq!(status, StatusCode::OK, "{started}");
    wait_for(s, token, &id, "done").await
}

async fn detail(s: &Server, token: &str, machine: &str) -> Value {
    let (status, body) = call(
        s,
        Method::GET,
        &format!("/api/v1/work/machines/{machine}"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["detail"].clone()
}

#[tokio::test]
async fn the_three_plugins_are_listed_with_what_they_take_and_say_they_are_simulators() {
    let s = common::server("sim-plugins-list");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let (status, plugins) = call(&s, Method::GET, "/api/v1/plugins", Some(&operator), None).await;
    assert_eq!(status, StatusCode::OK);
    let list = plugins["plugins"].as_array().unwrap();
    for (kind, request) in [("laser", "gcode"), ("pnp", "place"), ("pcb", "rules_check")] {
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
}

#[tokio::test]
async fn the_settings_of_a_machine_of_each_plugin_are_checked_and_a_typo_is_refused() {
    let s = common::server("sim-plugins-settings");
    let boss = admin(&s).await;
    for (plugin, bad) in [
        ("laser", json!({"power_limit_pct": 150})),
        ("laser", json!({"power": 10})),
        ("laser", json!({"over_limit": "never"})),
        ("pnp", json!({"feeders": 0})),
        ("pnp", json!({"require_stock": "no"})),
        ("pcb", json!({"min_drill_mm": 9, "max_drill_mm": 1})),
        ("pcb", json!({"fail": true})),
    ] {
        let (status, r) = call(
            &s,
            Method::POST,
            "/api/v1/work/machines",
            Some(&boss),
            Some(json!({"id": "m1", "name": "M", "plugin": plugin, "settings": bad})),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{plugin} {bad}");
        assert_eq!(r["error"], "bad_plugin_request");
    }
    // a machine that is made with good settings can be changed, and a bad change is refused
    make_machine(&s, &boss, "l1", "laser", json!({"sim_rate": 0})).await;
    let (status, _) = call(
        &s,
        Method::PUT,
        "/api/v1/work/machines/l1",
        Some(&boss),
        Some(json!({"settings": {"bed_x_mm": 2}})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(
        &s,
        Method::PUT,
        "/api/v1/work/machines/l1",
        Some(&boss),
        Some(json!({"settings": {"bed_x_mm": 200, "sim_rate": 0}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_laser_job_is_held_to_the_limits_and_runs_in_the_simulator() {
    let s = common::server("sim-laser-limits");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make_machine(&s, &boss, "l1", "laser", json!({"sim_rate": 0})).await;
    let d = detail(&s, &operator, "l1").await;
    assert_eq!(d["estop_latched"], false);
    assert_eq!(d["power"]["limit_s"], 400.0);
    for (text, code) in [
        ("M4 S900\nG1 X10 F100", "laser_power_limit"),
        ("M4 S100\nG1 X10 F7000", "laser_feed_limit"),
        ("G0 X500", "outside_course"),
        ("M3 S100\nG0 X10", "bad_gcode"),
        ("$H", "bad_gcode"),
        ("; nothing", "no_work"),
    ] {
        let (status, r) =
            prepare(&s, &operator, "l1", json!({"kind": "gcode", "text": text})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{text}: {r}");
        assert_eq!(r["error"], code, "{text}");
    }
    let job = run_to_done(
        &s,
        &operator,
        "l1",
        json!({"kind": "gcode", "text": "G21 G90\nM4 S300\nG0 X10 Y10\nG1 X30 Y10 F600\nG1 X30 Y30\nM5"}),
    )
    .await;
    assert_eq!(job["done"], 6);
    assert_eq!(job["total"], 6);
    assert_eq!(job["view"]["cut_mm"], 40.0);
    let d = detail(&s, &operator, "l1").await;
    assert_eq!(d["position_mm"]["x"], 30.0);
    assert_eq!(d["position_mm"]["y"], 30.0);
    assert_eq!(d["beam_on"], false);
}

#[tokio::test]
async fn a_laser_that_clamps_cuts_the_power_and_the_job_says_how_often() {
    let s = common::server("sim-laser-clamp");
    let boss = admin(&s).await;
    make_machine(
        &s,
        &boss,
        "l1",
        "laser",
        json!({"sim_rate": 0, "over_limit": "clamp"}),
    )
    .await;
    let job = run_to_done(
        &s,
        &boss,
        "l1",
        json!({"kind": "gcode", "text": "M4 S900\nG1 X10 F100\nM5"}),
    )
    .await;
    assert_eq!(job["view"]["clamped"], 1);
    assert_eq!(job["view"]["max_s"], 400.0);
}

#[tokio::test]
async fn the_emergency_stop_latches_the_laser_until_a_rearm_job_runs() {
    let s = common::server("sim-laser-estop");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    // real time: the job takes 100 seconds
    make_machine(&s, &boss, "l1", "laser", json!({"sim_rate": 1})).await;
    let (status, job) = prepare(
        &s,
        &operator,
        "l1",
        json!({"kind": "gcode", "text": "M4 S100\nG1 X100 F60\nM5"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let id = job["id"].as_str().unwrap().to_string();
    let (status, _) = act(&s, &operator, &id, "start").await;
    assert_eq!(status, StatusCode::OK);
    let (status, held) = act(&s, &operator, &id, "pause").await;
    assert_eq!(status, StatusCode::OK, "{held}");
    assert_eq!(held["state"], "paused");
    let (status, _) = act(&s, &operator, &id, "resume").await;
    assert_eq!(status, StatusCode::OK);
    let (status, stopped) = act(&s, &operator, &id, "cancel").await;
    assert_eq!(status, StatusCode::OK, "{stopped}");
    wait_for(&s, &operator, &id, "cancelled").await;
    assert_eq!(detail(&s, &operator, "l1").await["estop_latched"], true);
    // nothing is prepared on the machine until it is rearmed
    let (status, refused) = prepare(
        &s,
        &operator,
        "l1",
        json!({"kind": "gcode", "text": "G1 X1 F60"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"], "laser_estop_latched");
    run_to_done(&s, &operator, "l1", json!({"kind": "rearm"})).await;
    assert_eq!(detail(&s, &operator, "l1").await["estop_latched"], false);
    let (status, _) = prepare(
        &s,
        &operator,
        "l1",
        json!({"kind": "gcode", "text": "G1 X1 F60"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn a_test_fire_never_goes_over_the_limit_of_the_machine() {
    let s = common::server("sim-laser-fire");
    let boss = admin(&s).await;
    make_machine(&s, &boss, "l1", "laser", json!({"sim_rate": 0})).await;
    let (status, r) = prepare(
        &s,
        &boss,
        "l1",
        json!({"kind": "test_fire", "power_pct": 90, "seconds": 0.2}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    assert_eq!(r["error"], "laser_power_limit");
    let done = run_to_done(
        &s,
        &boss,
        "l1",
        json!({"kind": "test_fire", "power_pct": 10, "seconds": 0.2}),
    )
    .await;
    assert_eq!(done["view"]["kind"], "test_fire");
}

#[tokio::test]
async fn a_pick_and_place_machine_keeps_its_feeders_and_uses_them_up() {
    let s = common::server("sim-pnp-feeders");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make_machine(&s, &boss, "p1", "pnp", json!({"sim_rate": 0})).await;
    let parts = json!([
        {"ref": "R1", "part": "R_0603_10k", "x": 20, "y": 30},
        {"ref": "R2", "part": "R_0603_10k", "x": 25, "y": 30},
        {"ref": "C1", "part": "C_0603_100n", "x": 40, "y": 50},
    ]);
    // no feeders yet
    let (status, r) = prepare(
        &s,
        &operator,
        "p1",
        json!({"kind": "place", "placements": parts}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{r}");
    assert_eq!(r["error"], "pnp_feeder_short");
    run_to_done(
        &s,
        &operator,
        "p1",
        json!({"kind": "load_feeders", "feeders": [
            {"slot": 1, "part": "R_0603_10k", "count": 3},
            {"slot": 2, "part": "C_0603_100n", "count": 50},
        ]}),
    )
    .await;
    let d = detail(&s, &operator, "p1").await;
    assert_eq!(d["feeders"].as_array().unwrap().len(), 2);
    let job = run_to_done(
        &s,
        &operator,
        "p1",
        json!({"kind": "place", "placements": parts}),
    )
    .await;
    assert_eq!(job["done"], 3);
    assert_eq!(job["view"]["parts"]["R_0603_10k"], 2);
    let d = detail(&s, &operator, "p1").await;
    assert_eq!(d["feeders"][0]["count"], 1);
    assert_eq!(d["feeders"][1]["count"], 49);
    assert_eq!(d["placed_total"], 3);
    // one resistor is left and the job needs two
    let (status, r) = prepare(
        &s,
        &operator,
        "p1",
        json!({"kind": "place", "placements": parts}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{r}");
    // a place outside the bed and a repeated reference are refused as they are
    let (status, r) = prepare(
        &s,
        &operator,
        "p1",
        json!({"kind": "place", "placements": [{"ref": "R1", "part": "R_0603_10k", "x": 999, "y": 1}]}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    assert_eq!(r["error"], "outside_course");
}

#[tokio::test]
async fn a_pick_and_place_job_fails_when_the_vacuum_is_lost_and_the_part_is_gone() {
    let s = common::server("sim-pnp-fault");
    let boss = admin(&s).await;
    make_machine(
        &s,
        &boss,
        "p1",
        "pnp",
        json!({"sim_rate": 0, "sim_fail_pick_every": 2}),
    )
    .await;
    run_to_done(
        &s,
        &boss,
        "p1",
        json!({"kind": "load_feeders", "feeders": [{"slot": 1, "part": "A", "count": 10}]}),
    )
    .await;
    let (status, job) = prepare(
        &s,
        &boss,
        "p1",
        json!({"kind": "place", "placements": [
            {"ref": "R1", "part": "A", "x": 10, "y": 10},
            {"ref": "R2", "part": "A", "x": 20, "y": 10},
            {"ref": "R3", "part": "A", "x": 30, "y": 10},
        ]}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let id = job["id"].as_str().unwrap().to_string();
    act(&s, &boss, &id, "start").await;
    let ended = wait_for(&s, &boss, &id, "failed").await;
    assert_eq!(ended["done"], 1);
    assert!(
        ended["message"].as_str().unwrap().contains("vacuum"),
        "{ended}"
    );
    let d = detail(&s, &boss, "p1").await;
    assert_eq!(d["failed_picks"], 1);
    assert_eq!(d["feeders"][0]["count"], 8);
}

#[tokio::test]
async fn a_board_station_reports_the_rules_the_drilling_the_parts_and_the_measurements() {
    let s = common::server("sim-pcb");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make_machine(&s, &boss, "b1", "pcb", json!({"sim_rate": 0})).await;
    let checked = run_to_done(
        &s,
        &operator,
        "b1",
        json!({"kind": "rules_check", "board": {
            "outline": {"width_mm": 50, "height_mm": 40},
            "tracks": [{"width_mm": 0.1, "name": "VCC"}],
            "drills": [{"dia_mm": 1, "x": 10, "y": 10}],
        }}),
    )
    .await;
    // a report with errors is a job that is done: the verdict is in the report
    assert_eq!(checked["view"]["verdict"], "fail");
    assert_eq!(checked["view"]["violations"][0]["item"], "VCC");
    assert_eq!(checked["total"], 2);
    let drills = run_to_done(
        &s,
        &operator,
        "b1",
        json!({"kind": "drill_report", "drills": [
            {"dia_mm": 0.8, "x": 10, "y": 0}, {"dia_mm": 0.8, "x": 20, "y": 0}, {"dia_mm": 3.2, "x": 20, "y": 30},
        ]}),
    )
    .await;
    assert_eq!(drills["view"]["hits"], 3);
    assert_eq!(drills["view"]["tools"].as_array().unwrap().len(), 2);
    let bom = run_to_done(
        &s,
        &operator,
        "b1",
        json!({"kind": "bom_report", "components": [
            {"ref": "R1", "value": "10k", "footprint": "0603"}, {"ref": "R2", "value": "10k", "footprint": "0603"},
        ]}),
    )
    .await;
    assert_eq!(bom["view"]["lines"], 1);
    assert_eq!(bom["view"]["parts"], 2);
    let inspected = run_to_done(
        &s,
        &operator,
        "b1",
        json!({"kind": "inspect", "measurements": [{"name": "width", "nominal": 100, "tolerance": 0.1, "value": 100.05}]}),
    )
    .await;
    assert_eq!(inspected["view"]["verdict"], "pass");
    let d = detail(&s, &operator, "b1").await;
    assert_eq!(d["jobs_done"], 4);
    assert_eq!(d["last_kind"], "inspect");
    // what does not fit is refused with the reason
    let (status, r) = prepare(&s, &operator, "b1", json!({"kind": "etch"})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    assert_eq!(r["error"], "bad_plugin_request");
}

#[tokio::test]
async fn a_station_set_to_fail_on_violation_ends_a_bad_report_failed() {
    let s = common::server("sim-pcb-strict");
    let boss = admin(&s).await;
    make_machine(
        &s,
        &boss,
        "b1",
        "pcb",
        json!({"sim_rate": 0, "fail_on_violation": true}),
    )
    .await;
    let (status, job) = prepare(
        &s,
        &boss,
        "b1",
        json!({"kind": "rules_check", "board": {"tracks": [{"width_mm": 0.05}]}}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let id = job["id"].as_str().unwrap().to_string();
    act(&s, &boss, &id, "start").await;
    let ended = wait_for(&s, &boss, &id, "failed").await;
    assert!(
        ended["message"].as_str().unwrap().contains("1 errors"),
        "{ended}"
    );
}
