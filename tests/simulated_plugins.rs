//! The plugins `laser`, `pnp`, `pcb`, `cnc`, `gerber`, `cam` and `printer3d` end to end through the real router: their machines and settings, the checks of the jobs, the
//! limits, the emergency stop that latches a laser, a CNC machine or a printer until an administrator releases it, the feeders of a pick-and-place machine, the reports of a
//! board station and the checks of the heaters. They are simulators: nothing here has run on a real machine.

mod common;

use std::time::{Duration, Instant};

use axum::http::{Method, StatusCode};
use base64::Engine as _;
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
    for (kind, request) in [
        ("laser", "gcode"),
        ("pnp", "place"),
        ("pcb", "rules_check"),
        ("cnc", "gcode"),
        ("gerber", "drc"),
        ("cam", "drill"),
        ("printer3d", "print"),
        ("vision", "detect"),
        ("cad", "check"),
    ] {
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
        ("cnc", json!({"travel_x_mm": 9999})),
        ("cnc", json!({"connection": {"kind": "grbl"}})),
        ("gerber", json!({"min_track_mm": 0})),
        ("cam", json!({"max_depth_mm": 0})),
        ("printer3d", json!({"model": "mystery"})),
        ("printer3d", json!({"hotend_max_c": 500})),
        (
            "printer3d",
            json!({"connection": {"kind": "bambu_lan", "url": "http://x"}}),
        ),
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
async fn the_emergency_stop_latches_the_laser_until_an_administrator_releases_it() {
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
    // nothing is prepared on the machine until an administrator releases it
    let (status, refused) = prepare(
        &s,
        &operator,
        "l1",
        json!({"kind": "gcode", "text": "G1 X1 F60"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"], "laser_estop_latched");
    // the old way, a job of the kind rearm, is not there; the operator cannot release it either
    let (status, old) = prepare(&s, &operator, "l1", json!({"kind": "rearm"})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{old}");
    let (status, no) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines/l1/laser/release",
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{no}");
    assert_eq!(detail(&s, &operator, "l1").await["estop_latched"], true);
    let (status, freed) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines/l1/laser/release",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{freed}");
    assert_eq!(freed["released"], true);
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
        json!({"kind": "test_fire", "power_pct": 1, "seconds": 0.2}),
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

const COPPER: &str = "%FSLAX24Y24*%\n%MOMM*%\n%TF.FileFunction,Copper,L1,Top*%\n%ADD10C,0.100*%\n%ADD11C,1.600*%\nD10*\nX100000Y100000D02*\nX400000Y100000D01*\nD11*\nX100000Y200000D03*\nM02*\n";
const OUTLINE: &str = "%FSLAX24Y24*%\n%MOMM*%\n%ADD10C,0.100*%\nD10*\nX0Y0D02*\nX500000Y0D01*\nX500000Y400000D01*\nX0Y400000D01*\nX0Y0D01*\nM02*\n";
const DRILL: &str =
    "M48\nMETRIC\nT1C0.800\nT2C1.000\n%\nT1\nX10.0Y10.0\nX20.0Y10.0\nT2\nX15.0Y30.0\nM30\n";
const PART: &str = "G28\nM104 S200\nM140 S60\n;LAYER_CHANGE\nG1 X10 Y10 Z0.2 F3000\nG1 X20 Y10\n;LAYER_CHANGE\nG1 X20 Y20 Z0.4\n;LAYER_CHANGE\nG1 X10 Y20 Z0.6\nM104 S0\nM140 S0\n";

async fn put_settings(s: &Server, boss: &str, id: &str, settings: Value) {
    let (status, body) = call(
        s,
        Method::PUT,
        &format!("/api/v1/work/machines/{id}"),
        Some(boss),
        Some(json!({"settings": settings})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn post_to(s: &Server, token: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    call(s, Method::POST, path, Some(token), body).await
}

#[tokio::test]
async fn a_cnc_machine_holds_a_program_to_its_travel_jogs_with_limits_and_latches_on_the_emergency_stop(
) {
    let s = common::server("sim-cnc");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let viewer = user(&s, &boss, "view1", "viewer").await;
    make_machine(
        &s,
        &boss,
        "c1",
        "cnc",
        json!({"travel_x_mm": 100, "travel_y_mm": 100, "travel_z_mm": 20, "step_ms": 0, "jog_max_mm": 10}),
    )
    .await;
    for (text, code) in [
        ("G1 X150 F100", "out_of_travel"),
        ("M3 S20000", "spindle_limit"),
        ("$H", "bad_gcode"),
    ] {
        let (status, r) =
            prepare(&s, &operator, "c1", json!({"kind": "gcode", "text": text})).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{text}: {r}");
        assert_eq!(r["error"], code, "{text}");
    }
    let job = run_to_done(
        &s,
        &operator,
        "c1",
        json!({"kind": "gcode", "text": "G21 G90\nG0 X10 Y10\nG1 X20 Y10 F300"}),
    )
    .await;
    assert_eq!(
        (job["done"].clone(), job["total"].clone()),
        (json!(3), json!(3))
    );
    assert_eq!(job["view"]["travel_check"], "on");
    let (status, here) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/c1/position",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{here}");
    assert_eq!(
        (here["state"].clone(), here["machine_mm"]["x"].clone()),
        (json!("Idle"), json!(20.0))
    );
    // the jog: a viewer cannot, an operator can inside the limits
    let jog = |axis: &str, d: f64| json!({"axis": axis, "distance_mm": d});
    let (status, _) = post_to(
        &s,
        &viewer,
        "/api/v1/work/machines/c1/jog",
        Some(jog("y", 5.0)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, moved) = post_to(
        &s,
        &operator,
        "/api/v1/work/machines/c1/jog",
        Some(jog("y", 5.0)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{moved}");
    assert_eq!(moved["position_mm"]["y"], 15.0);
    for (body, code) in [
        (jog("w", 1.0), "bad_axis"),
        (jog("x", 11.0), "bad_plugin_request"),
    ] {
        let (status, r) = post_to(&s, &operator, "/api/v1/work/machines/c1/jog", Some(body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
        assert_eq!(r["error"], code);
    }
    for _ in 0..2 {
        let (status, r) = post_to(
            &s,
            &operator,
            "/api/v1/work/machines/c1/jog",
            Some(jog("x", -10.0)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{r}");
    }
    let (status, r) = post_to(
        &s,
        &operator,
        "/api/v1/work/machines/c1/jog",
        Some(jog("x", -10.0)),
    )
    .await;
    assert_eq!(
        (status, r["error"].clone()),
        (StatusCode::UNPROCESSABLE_ENTITY, json!("out_of_travel"))
    );
    // the emergency stop needs no body, latches the machine, and only an administrator frees it
    let (status, stopped) = post_to(
        &s,
        &operator,
        "/api/v1/work/machines/c1/emergency-stop",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stopped}");
    assert_eq!(stopped["latched"], true);
    let (status, r) = prepare(
        &s,
        &operator,
        "c1",
        json!({"kind": "gcode", "text": "G0 X5"}),
    )
    .await;
    assert_eq!(
        (status, r["error"].clone()),
        (StatusCode::CONFLICT, json!("machine_alarm"))
    );
    let (status, r) = post_to(
        &s,
        &operator,
        "/api/v1/work/machines/c1/jog",
        Some(jog("y", 1.0)),
    )
    .await;
    assert_eq!(
        (status, r["error"].clone()),
        (StatusCode::CONFLICT, json!("machine_alarm"))
    );
    let (status, _) = post_to(&s, &operator, "/api/v1/work/machines/c1/release", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, freed) = post_to(&s, &boss, "/api/v1/work/machines/c1/release", None).await;
    assert_eq!(status, StatusCode::OK, "{freed}");
    assert_eq!(freed["released"], true);
    let (status, _) = prepare(
        &s,
        &operator,
        "c1",
        json!({"kind": "gcode", "text": "G0 X5"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    // a route of another kind of machine is not served by this one
    let (status, r) = post_to(&s, &boss, "/api/v1/work/machines/c1/laser/release", None).await;
    assert_eq!(
        (status, r["error"].clone()),
        (StatusCode::NOT_IMPLEMENTED, json!("not_supported"))
    );
}

#[tokio::test]
async fn the_laser_test_fire_route_and_the_switches_of_the_enclosure_refuse_what_is_not_safe() {
    let s = common::server("sim-laser-switches");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make_machine(&s, &boss, "l1", "laser", json!({"sim_rate": 0})).await;
    let fire = |pct: f64| json!({"power_pct": pct, "seconds": 0.1});
    let (status, ok) = post_to(
        &s,
        &operator,
        "/api/v1/work/machines/l1/laser/test-fire",
        Some(fire(1.0)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ok}");
    assert_eq!(ok["fired"], true);
    for (pct, code) in [(5.0, "test_fire_refused"), (90.0, "laser_power_limit")] {
        let (status, r) = post_to(
            &s,
            &operator,
            "/api/v1/work/machines/l1/laser/test-fire",
            Some(fire(pct)),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
        assert_eq!(r["error"], code);
    }
    let gcode = json!({"kind": "gcode", "text": "G1 X1 F60"});
    put_settings(
        &s,
        &boss,
        "l1",
        json!({"sim_rate": 0, "enclosure_closed": false}),
    )
    .await;
    let (status, r) = prepare(&s, &operator, "l1", gcode.clone()).await;
    assert_eq!(
        (status, r["error"].clone()),
        (StatusCode::CONFLICT, json!("enclosure_open"))
    );
    let (status, r) = post_to(
        &s,
        &operator,
        "/api/v1/work/machines/l1/laser/test-fire",
        Some(fire(1.0)),
    )
    .await;
    assert_eq!(
        (status, r["error"].clone()),
        (StatusCode::CONFLICT, json!("enclosure_open"))
    );
    put_settings(
        &s,
        &boss,
        "l1",
        json!({"sim_rate": 0, "air_assist_on": false}),
    )
    .await;
    let (status, r) = prepare(&s, &operator, "l1", gcode.clone()).await;
    assert_eq!(
        (status, r["error"].clone()),
        (StatusCode::CONFLICT, json!("air_assist_off"))
    );
    put_settings(&s, &boss, "l1", json!({"sim_rate": 0, "focused": false})).await;
    let (status, r) = prepare(&s, &operator, "l1", gcode.clone()).await;
    assert_eq!(
        (status, r["error"].clone()),
        (StatusCode::CONFLICT, json!("focus_not_checked"))
    );
    put_settings(&s, &boss, "l1", json!({"sim_rate": 0})).await;
    // a vector job becomes a program held to the same limits
    let vector = |pct: f64| {
        json!({
            "kind": "vector",
            "paths": [{"layer": "cut", "points": [[10, 10], [30, 10], [30, 30]], "closed": true}],
            "operations": [{"layer": "cut", "mode": "cut", "power_pct": pct, "speed": 600}],
        })
    };
    let done = run_to_done(&s, &operator, "l1", vector(40.0)).await;
    assert_eq!(done["view"]["kind"], "vector");
    assert_eq!(done["view"]["max_s"], 400.0);
    let (status, r) = prepare(&s, &operator, "l1", vector(41.0)).await;
    assert_eq!(
        (status, r["error"].clone()),
        (StatusCode::UNPROCESSABLE_ENTITY, json!("laser_power_limit"))
    );
}

#[tokio::test]
async fn a_placement_list_can_be_an_artifact_of_the_store() {
    let s = common::server("sim-pnp-artifact");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make_machine(&s, &boss, "p1", "pnp", json!({"sim_rate": 0})).await;
    run_to_done(
        &s,
        &operator,
        "p1",
        json!({"kind": "load_feeders", "feeders": [{"slot": 1, "part": "R_0603_10k", "count": 50}]}),
    )
    .await;
    let csv = "Ref,Val,Package,PosX,PosY,Rot,Side\nR1,R_0603_10k,R_0603,20,30,90,top\nR2,R_0603_10k,R_0603,25,30,0,top\n";
    let (status, art) = post_to(
        &s,
        &operator,
        "/api/v1/artifacts",
        Some(json!({"name": "board-pos.csv", "format": "text/csv", "data_base64": base64::engine::general_purpose::STANDARD.encode(csv)})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{art}");
    let id = art["artifact_id"].as_str().unwrap().to_string();
    let done = run_to_done(&s, &operator, "p1", json!({"kind": "place", "job": id})).await;
    assert_eq!(done["view"]["placements"], 2);
    // a list nobody can see, or one that is not a list, is refused
    let other = user(&s, &boss, "oper2", "operator").await;
    let (status, r) = prepare(&s, &other, "p1", json!({"kind": "place", "job": id})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    let (status, junk) = post_to(
        &s,
        &operator,
        "/api/v1/artifacts",
        Some(json!({"name": "notes.txt", "format": "text/plain", "data_base64": base64::engine::general_purpose::STANDARD.encode("just words")})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{junk}");
    let (status, r) = prepare(
        &s,
        &operator,
        "p1",
        json!({"kind": "place", "job": junk["artifact_id"]}),
    )
    .await;
    assert_eq!(
        (status, r["error"].clone()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            json!("bad_placement_list")
        )
    );
}

#[tokio::test]
async fn a_gerber_worker_checks_a_board_from_the_files_of_the_request_or_from_an_artifact() {
    let s = common::server("sim-gerber");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make_machine(&s, &boss, "g1", "gerber", json!({})).await;
    let files = json!([
        {"name": "board.gtl", "text": COPPER},
        {"name": "board.gko", "text": OUTLINE},
        {"name": "board.drl", "text": DRILL},
    ]);
    let done = run_to_done(&s, &operator, "g1", json!({"kind": "drc", "files": files})).await;
    assert_eq!(done["view"]["verdict"], "fail");
    let rules: Vec<&str> = done["view"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["rule"].as_str().unwrap())
        .collect();
    assert!(rules.contains(&"track_width"), "{rules:?}");
    assert_eq!(done["view"]["layers"].as_array().unwrap().len(), 3);
    assert_eq!(done["total"], 6);
    // the rules of the request lie over the ones of the worker
    let lax = run_to_done(
        &s,
        &operator,
        "g1",
        json!({"kind": "drc", "files": [{"name": "board.gtl", "text": COPPER}], "rules": {"min_track_mm": 0.05, "min_clearance_mm": 0.05}}),
    )
    .await;
    assert_eq!(lax["view"]["rules"]["min_track_mm"], 0.05);
    for (request, code) in [
        (
            json!({"kind": "drc", "files": [{"name": "board.gko", "text": OUTLINE}]}),
            "missing_layer",
        ),
        (
            json!({"kind": "load", "files": [{"name": "a.gbr", "text": "junk"}]}),
            "bad_gerber",
        ),
        (
            json!({"kind": "drc", "files": files, "rules": {"min_hole_mm": 99}}),
            "bad_rules",
        ),
        (json!({"kind": "paint"}), "bad_plugin_request"),
    ] {
        let (status, r) = prepare(&s, &operator, "g1", request.clone()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{request}: {r}");
        assert_eq!(r["error"], code, "{request}");
    }
    // the same file as an artifact of the store: its text reaches the worker, and nobody else can use it
    let (status, art) = post_to(
        &s,
        &operator,
        "/api/v1/artifacts",
        Some(json!({"name": "top.gtl", "format": "text/plain", "data_base64": base64::engine::general_purpose::STANDARD.encode(COPPER)})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{art}");
    let id = art["artifact_id"].as_str().unwrap().to_string();
    let loaded = run_to_done(&s, &operator, "g1", json!({"kind": "load", "artifact": id})).await;
    assert_eq!(loaded["view"]["layers"][0]["function"], "copper_top");
    let other = user(&s, &boss, "oper2", "operator").await;
    let (status, r) = prepare(&s, &other, "g1", json!({"kind": "load", "artifact": id})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    // a client cannot slip its own text in as if it came from an artifact
    let (status, r) = prepare(
        &s,
        &operator,
        "g1",
        json!({"kind": "load", "artifact_text": COPPER}),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{r}");
    assert_eq!(r["error"], "missing_layer");
}

#[tokio::test]
async fn a_cam_worker_makes_gcode_and_checks_a_program_without_moving_anything() {
    let s = common::server("sim-cam");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make_machine(&s, &boss, "w1", "cam", json!({"max_depth_mm": 2})).await;
    let drill = run_to_done(
        &s,
        &operator,
        "w1",
        json!({"kind": "drill", "files": [{"name": "a.drl", "text": DRILL}], "params": {"depth_mm": 1.6}}),
    )
    .await;
    let text = drill["view"]["gcode"].as_str().unwrap();
    assert!(
        text.contains("M0 (change to a 0.8 mm drill)") && text.contains("G1 Z-1.6 F100"),
        "{text}"
    );
    assert_eq!(drill["view"]["result"]["holes"], 3);
    let outline = run_to_done(
        &s,
        &operator,
        "w1",
        json!({"kind": "outline", "files": [{"name": "a.gko", "text": OUTLINE}]}),
    )
    .await;
    assert_eq!(outline["view"]["result"]["board_mm"]["width"], 50.0);
    let isolate = run_to_done(&s, &operator, "w1", json!({"kind": "isolate", "files": [{"name": "a.gtl", "text": COPPER}], "params": {"depth_mm": 0.05}})).await;
    assert!(isolate["view"]["result"]["loops"].as_u64().unwrap() >= 1);
    let verify = run_to_done(
        &s,
        &operator,
        "w1",
        json!({"kind": "verify", "text": "G21 G90\nM3 S10000\nG1 X5 F100\nM5\nM2\n"}),
    )
    .await;
    assert_eq!(verify["view"]["result"]["verdict"], "pass");
    // a request over a limit of the worker is refused, never cut down
    let (status, r) = prepare(
        &s,
        &operator,
        "w1",
        json!({"kind": "drill", "files": [{"name": "a.drl", "text": DRILL}], "params": {"depth_mm": 3}}),
    )
    .await;
    assert_eq!(
        (status, r["error"].clone()),
        (StatusCode::UNPROCESSABLE_ENTITY, json!("bad_params"))
    );
    let (status, tools) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/w1/tools",
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tools}");
    assert_eq!(tools["tools"].as_array().unwrap().len(), 7);
    // a CAM job cannot be held
    let (status, job) = prepare(
        &s,
        &operator,
        "w1",
        json!({"kind": "verify", "text": "G1 X1 F10\nM2\n"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let id = job["id"].as_str().unwrap().to_string();
    let (status, _) = act(&s, &operator, &id, "start").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = act(&s, &operator, &id, "pause").await;
    assert!(
        status == StatusCode::NOT_IMPLEMENTED || status == StatusCode::CONFLICT,
        "{status}"
    );
}

#[tokio::test]
async fn a_printer_has_the_limits_of_its_model_a_queue_a_person_starts_and_an_emergency_stop_that_latches(
) {
    let s = common::server("sim-printer");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let viewer = user(&s, &boss, "view1", "viewer").await;
    make_machine(
        &s,
        &boss,
        "r1",
        "printer3d",
        json!({"model": "bambu_a1", "sim_rate": 0, "step_ms": 20}),
    )
    .await;
    let d = detail(&s, &viewer, "r1").await;
    assert_eq!(
        (
            d["model"].clone(),
            d["limits_c"]["hotend"].clone(),
            d["limits_c"]["chamber"].clone()
        ),
        (json!("bambu_a1"), json!(300.0), json!(0.0))
    );
    assert_eq!(
        (d["latched"].clone(), d["auto_start_queue"].clone()),
        (json!(false), json!(false))
    );
    // the temperatures: a viewer reads, an operator sets inside the limit, nobody over it
    let (status, t) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/r1/temperatures",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{t}");
    assert_eq!(t["heaters"][0]["limit_c"], 300.0);
    let set = |heater: &str, c: f64| json!({"heater": heater, "target_c": c});
    let put = |token: String, body: Value| {
        let s = &s;
        async move {
            call(
                s,
                Method::PUT,
                "/api/v1/work/machines/r1/temperatures",
                Some(&token),
                Some(body),
            )
            .await
        }
    };
    let (status, _) = put(viewer.clone(), set("bed", 60.0)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, ok) = put(operator.clone(), set("bed", 60.0)).await;
    assert_eq!(status, StatusCode::OK, "{ok}");
    for (body, code) in [
        (set("hotend", 301.0), "temperature_over_limit"),
        (set("chamber", 30.0), "temperature_over_limit"),
        (set("nozzle", 30.0), "bad_heater"),
    ] {
        let (status, r) = put(operator.clone(), body.clone()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}: {r}");
        assert_eq!(r["error"], code, "{body}");
    }
    // a program over a limit is refused whole
    let (status, r) = prepare(
        &s,
        &operator,
        "r1",
        json!({"kind": "print", "text": "M104 S310\nG1 X1 F600\n"}),
    )
    .await;
    assert_eq!(
        (status, r["error"].clone()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            json!("program_over_limit")
        )
    );
    let (status, r) = prepare(
        &s,
        &operator,
        "r1",
        json!({"kind": "print", "text": "M112\n"}),
    )
    .await;
    assert_eq!(
        (status, r["error"].clone()),
        (StatusCode::UNPROCESSABLE_ENTITY, json!("bad_gcode"))
    );
    // a print waits for a person and runs to the end on the simulator
    let (status, queued) = prepare(
        &s,
        &operator,
        "r1",
        json!({"kind": "print", "text": PART, "name": "part"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{queued}");
    assert_eq!(queued["state"], "queued");
    assert_eq!(queued["view"]["layers"], 3);
    tokio::time::sleep(Duration::from_millis(700)).await;
    let (_, still) = call(
        &s,
        Method::GET,
        &format!("/api/v1/work/jobs/{}", queued["id"].as_str().unwrap()),
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(still["state"], "queued", "nothing starts by itself");
    let id = queued["id"].as_str().unwrap().to_string();
    let (status, started) = act(&s, &operator, &id, "start").await;
    assert_eq!(status, StatusCode::OK, "{started}");
    wait_for(&s, &operator, &id, "done").await;
    let (status, layer) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/r1/layer",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{layer}");
    assert_eq!(layer["layers"], 3);
    // the answers shaped like another program's are off until the machine asks for them
    let (status, r) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/r1/compat/moonraker",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(
        (status, r["error"].clone()),
        (StatusCode::NOT_IMPLEMENTED, json!("not_supported"))
    );
    put_settings(
        &s,
        &boss,
        "r1",
        json!({"model": "bambu_a1", "sim_rate": 0, "step_ms": 20, "compat": "both"}),
    )
    .await;
    let (status, m) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/r1/compat/moonraker",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{m}");
    assert_eq!(m["result"]["status"]["print_stats"]["state"], "complete");
    let (status, o) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/r1/compat/octoprint",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{o}");
    assert_eq!(o["printer"]["state"]["flags"]["operational"], true);
    // the emergency stop: no body, heaters off, the machine latched until an administrator releases it
    let (status, stopped) = post_to(
        &s,
        &operator,
        "/api/v1/work/machines/r1/emergency-stop",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stopped}");
    assert_eq!(
        (
            stopped["latched"].clone(),
            stopped["printer_reached"].clone()
        ),
        (json!(true), json!(true))
    );
    for refused in [
        prepare(&s, &operator, "r1", json!({"kind": "print", "text": PART})).await,
        put(operator.clone(), set("bed", 30.0)).await,
    ] {
        assert_eq!(
            (refused.0, refused.1["error"].clone()),
            (StatusCode::CONFLICT, json!("printer_latched"))
        );
    }
    assert_eq!(detail(&s, &operator, "r1").await["latched"], true);
    let (status, _) = post_to(&s, &operator, "/api/v1/work/machines/r1/release", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, freed) = post_to(&s, &boss, "/api/v1/work/machines/r1/release", None).await;
    assert_eq!(status, StatusCode::OK, "{freed}");
    assert_eq!(
        (freed["released"].clone(), freed["printer_reset"].clone()),
        (json!(true), json!(true))
    );
    let (status, _) = prepare(&s, &operator, "r1", json!({"kind": "print", "text": PART})).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn a_simulated_fault_of_a_heater_latches_the_printer_and_the_key_of_a_connection_is_never_shown(
) {
    let s = common::server("sim-printer-fault");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make_machine(
        &s,
        &boss,
        "r1",
        "printer3d",
        json!({"sim_rate": 1, "step_ms": 3600000, "sim_fault": "sensor_lost"}),
    )
    .await;
    let (status, queued) =
        prepare(&s, &operator, "r1", json!({"kind": "print", "text": PART})).await;
    assert_eq!(status, StatusCode::CREATED, "{queued}");
    let id = queued["id"].as_str().unwrap().to_string();
    let (status, started) = act(&s, &operator, &id, "start").await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let failed = wait_for(&s, &operator, &id, "failed").await;
    assert!(
        failed["message"]
            .as_str()
            .unwrap()
            .starts_with("sensor_fault:"),
        "{failed}"
    );
    assert_eq!(detail(&s, &operator, "r1").await["latched"], true);
    let (status, r) = prepare(&s, &operator, "r1", json!({"kind": "print", "text": PART})).await;
    assert_eq!(
        (status, r["error"].clone()),
        (StatusCode::CONFLICT, json!("printer_latched"))
    );
    // the key of the connection of a printer is kept and not shown again
    let (status, made) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines",
        Some(&boss),
        Some(json!({"id": "r2", "name": "Klipper", "plugin": "printer3d",
            "settings": {"connection": {"kind": "moonraker", "url": "http://127.0.0.1:9", "api_key": "very-long-secret-value"}}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(made["settings"]["connection"]["api_key"], "********");
    assert!(!made.to_string().contains("very-long-secret-value"));
    let (status, changed) = call(
        &s,
        Method::PUT,
        "/api/v1/work/machines/r2",
        Some(&boss),
        Some(json!({"settings": {"connection": {"kind": "moonraker", "url": "http://127.0.0.1:9", "api_key": "********"}}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert!(!changed.to_string().contains("very-long-secret-value"));
    let stored = std::fs::read_to_string(s.dir.join("work-machines.json")).unwrap();
    assert!(
        stored.contains("very-long-secret-value"),
        "the key itself is kept for the server to use"
    );
    // a printer that cannot be reached is a machine that is listed, with its detail saying so
    let (status, list) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/r2",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["detail"]["online"], false);
}

#[tokio::test]
async fn every_simulated_machine_says_in_its_detail_that_it_is_only_a_simulator() {
    let s = common::server("sim-plugins-evidence");
    let boss = admin(&s).await;
    for (id, plugin) in [
        ("e-laser", "laser"),
        ("e-pnp", "pnp"),
        ("e-pcb", "pcb"),
        ("e-cnc", "cnc"),
        ("e-gerber", "gerber"),
        ("e-cam", "cam"),
        ("e-printer", "printer3d"),
        ("e-vision", "vision"),
        ("e-cad", "cad"),
    ] {
        make_machine(&s, &boss, id, plugin, json!({})).await;
        let d = detail(&s, &boss, id).await;
        assert_eq!(d["evidence"], "simulator", "{plugin}: {d}");
        assert_eq!(d["simulator_only"], true, "{plugin}: {d}");
    }
    // the demo machine has no hardware either, but it is not a model of a machine: it does not claim to be one
    make_machine(&s, &boss, "e-demo", "demo", json!({})).await;
    let d = detail(&s, &boss, "e-demo").await;
    assert!(d.is_null() || d.get("simulator_only").is_none(), "{d}");
}

#[tokio::test]
async fn the_layers_and_the_findings_of_a_gerber_job_have_routes_of_their_own() {
    let s = common::server("sim-gerber-routes");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let viewer = user(&s, &boss, "view1", "viewer").await;
    make_machine(&s, &boss, "g1", "gerber", json!({})).await;
    make_machine(&s, &boss, "d1", "demo", json!({})).await;
    // two tracks 0.1 mm apart (0.3 mm between the center lines, 0.2 mm wide) and one far away
    let close = "%FSLAX24Y24*%
%MOMM*%
%TF.FileFunction,Copper,L1,Top*%
%ADD10C,0.200*%
D10*
X0Y0D02*
X100000Y0D01*
X0Y3000D02*
X100000Y3000D01*
X0Y100000D02*
X100000Y100000D01*
M02*
";
    let files =
        json!([{"name": "close.gtl", "text": close}, {"name": "board.gko", "text": OUTLINE}]);
    let drc = run_to_done(&s, &operator, "g1", json!({"kind": "drc", "files": files})).await;
    let id = drc["id"].as_str().unwrap().to_string();
    let rules: Vec<&str> = drc["view"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["rule"].as_str().unwrap())
        .collect();
    assert!(rules.contains(&"track_clearance"), "{rules:?}");
    let (status, layers) = call(
        &s,
        Method::GET,
        &format!("/api/v1/work/jobs/{id}/layers"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{layers}");
    assert_eq!(layers["kind"], "drc");
    assert_eq!(layers["layers"].as_array().unwrap().len(), 2);
    assert_eq!(layers["layers"][0]["function"], "copper_top");
    let (status, all) = call(
        &s,
        Method::GET,
        &format!("/api/v1/work/jobs/{id}/findings"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(all["verdict"], "fail");
    assert_eq!(all["matched"], all["findings_total"]);
    let (_, one) = call(
        &s,
        Method::GET,
        &format!("/api/v1/work/jobs/{id}/findings?rule=track_clearance"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(one["matched"], 1);
    assert_eq!(one["findings"][0]["layer"], "close.gtl");
    let (_, none) = call(
        &s,
        Method::GET,
        &format!("/api/v1/work/jobs/{id}/findings?layer=other.gtl"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(none["matched"], 0);
    let (_, page) = call(
        &s,
        Method::GET,
        &format!("/api/v1/work/jobs/{id}/findings?severity=error&limit=1"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(page["findings"].as_array().unwrap().len(), 1);
    assert_eq!(page["limit"], 1);
    // a job that only loaded the files has layers and no findings
    let loaded = run_to_done(&s, &operator, "g1", json!({"kind": "load", "files": files})).await;
    let lid = loaded["id"].as_str().unwrap().to_string();
    let (status, _) = call(
        &s,
        Method::GET,
        &format!("/api/v1/work/jobs/{lid}/layers"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &s,
        Method::GET,
        &format!("/api/v1/work/jobs/{lid}/findings"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("not_in_state"))
    );
    // a job of another plugin, a job that is not there and no login
    let (status, demo) = prepare(&s, &operator, "d1", json!({"kind": "demo", "steps": 1})).await;
    assert_eq!(status, StatusCode::CREATED, "{demo}");
    let did = demo["id"].as_str().unwrap().to_string();
    let (status, body) = call(
        &s,
        Method::GET,
        &format!("/api/v1/work/jobs/{did}/layers"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_IMPLEMENTED, Some("not_supported"))
    );
    let (status, body) = call(
        &s,
        Method::GET,
        "/api/v1/work/jobs/w-nothing/findings",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_job"))
    );
    let (status, _) = call(
        &s,
        Method::GET,
        &format!("/api/v1/work/jobs/{id}/layers"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_cam_worker_can_keep_its_program_as_an_artifact_of_the_store() {
    let s = common::server("sim-cam-artifact");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let other = user(&s, &boss, "oper2", "operator").await;
    make_machine(&s, &boss, "w1", "cam", json!({})).await;
    let files = json!([{"name": "a.drl", "text": DRILL}]);
    // the default leaves the program in the view
    let in_view = run_to_done(
        &s,
        &operator,
        "w1",
        json!({"kind": "drill", "files": files}),
    )
    .await;
    assert!(in_view["view"]["gcode"]
        .as_str()
        .unwrap()
        .starts_with("(drill)"));
    assert!(in_view["view"].get("artifact").is_none());
    let explicit = run_to_done(
        &s,
        &operator,
        "w1",
        json!({"kind": "drill", "files": files, "output": "view"}),
    )
    .await;
    assert!(explicit["view"]["gcode"].is_string());
    // with `output: artifact` the program is an artifact of the caller
    let kept = run_to_done(
        &s,
        &operator,
        "w1",
        json!({"kind": "drill", "files": files, "output": "artifact", "artifact_name": "drill-top.nc"}),
    )
    .await;
    assert!(kept["view"].get("gcode").is_none(), "{kept}");
    let artifact = &kept["view"]["artifact"];
    assert_eq!(artifact["name"], "drill-top.nc");
    assert_eq!(artifact["format"], "text/x-gcode");
    let id = artifact["artifact_id"].as_str().unwrap().to_string();
    let bytes = common::send(
        &s,
        Method::GET,
        &format!("/api/v1/artifacts/{id}"),
        Some(&operator),
        &[],
        None,
        None,
    )
    .await;
    assert_eq!(bytes.status, StatusCode::OK);
    let text = String::from_utf8(bytes.bytes).unwrap();
    assert!(
        text.starts_with(
            "(drill)
G21
"
        ) && text.trim_end().ends_with("M2"),
        "{text}"
    );
    assert_eq!(artifact["size"], text.len());
    let (status, meta) = call(
        &s,
        Method::GET,
        &format!("/api/v1/artifacts/{id}/meta"),
        Some(&operator),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{meta}");
    assert_eq!(meta["producer"]["tool"], "eh3d-server cam");
    assert_eq!(meta["owner"], "oper1");
    // nobody else sees it
    let (status, _) = call(
        &s,
        Method::GET,
        &format!("/api/v1/artifacts/{id}/meta"),
        Some(&other),
        None,
    )
    .await;
    assert_ne!(status, StatusCode::OK);
    // the default name is the kind of the job
    let named = run_to_done(&s, &operator, "w1", json!({"kind": "outline", "files": [{"name": "o.gko", "text": OUTLINE}], "output": "artifact"})).await;
    assert_eq!(named["view"]["artifact"]["name"], "outline.nc");
    // what makes no program, a wrong output and a bad name are refused
    for (request, why) in [
        (
            json!({"kind": "verify", "text": "G21
G0 X1
M2
", "output": "artifact"}),
            "verify",
        ),
        (
            json!({"kind": "drill", "files": files, "output": "disk"}),
            "disk",
        ),
        (
            json!({"kind": "drill", "files": files, "output": 3}),
            "number",
        ),
        (
            json!({"kind": "drill", "files": files, "output": "artifact", "artifact_name": "a/b.nc"}),
            "slash",
        ),
    ] {
        let (status, r) = prepare(&s, &operator, "w1", request).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{why}: {r}");
        assert_eq!(r["error"], "bad_plugin_request", "{why}");
    }
}

#[tokio::test]
async fn a_cam_worker_makes_a_stencil_a_probe_program_a_probe_report_and_an_inspection() {
    let s = common::server("sim-cam-more");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    make_machine(&s, &boss, "w1", "cam", json!({})).await;
    let paste = "%FSLAX24Y24*%
%MOMM*%
%TF.FileFunction,Paste,Top*%
%ADD10R,1.200X0.600*%
%ADD11C,1.000*%
D10*
X100000Y100000D03*
D11*
X150000Y200000D03*
M02*
";
    let stencil = run_to_done(
        &s,
        &operator,
        "w1",
        json!({"kind": "stencil", "files": [{"name": "board.gtp", "text": paste}], "params": {"tool_dia_mm": 0.4, "shrink_pct": 5}}),
    )
    .await;
    assert_eq!(stencil["view"]["result"]["apertures"], 2);
    assert!(stencil["view"]["gcode"].as_str().unwrap().contains("G2 "));
    let (status, r) = prepare(
        &s,
        &operator,
        "w1",
        json!({"kind": "stencil", "files": [{"name": "board.gtp", "text": paste}], "params": {"tool_dia_mm": 0.9}}),
    )
    .await;
    assert_eq!(
        (status, r["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_params")),
        "{r}"
    );
    let probe = run_to_done(
        &s,
        &operator,
        "w1",
        json!({"kind": "autolevel", "params": {"columns": 3, "rows": 3}}),
    )
    .await;
    assert_eq!(probe["view"]["result"]["points"], 9);
    assert_eq!(
        probe["view"]["gcode"]
            .as_str()
            .unwrap()
            .matches("G38.2")
            .count(),
        9
    );
    let map = run_to_done(
        &s,
        &operator,
        "w1",
        json!({"kind": "autolevel", "mode": "map", "heights": [[0.0, 0.0], [0.3, 0.3]], "params": {"columns": 2, "rows": 2, "tolerance_mm": 0.1}}),
    )
    .await;
    assert_eq!(map["view"]["result"]["verdict"], "uneven");
    assert!(map["view"].get("gcode").is_none());
    let inspected = run_to_done(
        &s,
        &operator,
        "w1",
        json!({"kind": "inspect", "text": "G21
G0 X0 Y0 Z2
G1 Z-1 F100
G1 X10
G0 Z2
M2
"}),
    )
    .await;
    assert_eq!(inspected["view"]["result"]["plunges"], 1);
    assert_eq!(inspected["view"]["result"]["levels"][0]["z_mm"], -1.0);
    // a program kept in the store is inspected by naming it
    let (status, art) = post_to(
        &s,
        &operator,
        "/api/v1/artifacts",
        Some(json!({"name": "p.nc", "format": "text/plain", "data_base64": base64::engine::general_purpose::STANDARD.encode("G21
G1 X5 Y0 Z-1 F100
M2
")})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{art}");
    let id = art["artifact_id"].as_str().unwrap().to_string();
    let again = run_to_done(
        &s,
        &operator,
        "w1",
        json!({"kind": "inspect", "artifact": id}),
    )
    .await;
    assert_eq!(again["view"]["result"]["cut_moves"], 1);
}

#[tokio::test]
async fn a_pick_and_place_machine_has_routes_for_its_feeders_and_takes_a_dry_run_and_a_calibration()
{
    let s = common::server("sim-pnp-feeders");
    let boss = admin(&s).await;
    let operator = user(&s, &boss, "oper1", "operator").await;
    let viewer = user(&s, &boss, "view1", "viewer").await;
    make_machine(&s, &boss, "p1", "pnp", json!({"sim_rate": 0, "feeders": 4})).await;
    let feeders = "/api/v1/work/machines/p1/feeders";
    let (status, empty) = call(&s, Method::GET, feeders, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{empty}");
    assert_eq!(empty["free"], json!([1, 2, 3, 4]));
    // a viewer reads, an operator writes
    let part = json!({"slot": 2, "part": "R_0603_10k", "count": 20});
    let (status, _) = call(&s, Method::PUT, feeders, Some(&viewer), Some(part.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, set) = call(&s, Method::PUT, feeders, Some(&operator), Some(part)).await;
    assert_eq!(status, StatusCode::OK, "{set}");
    assert_eq!(set["feeders"][0]["part"], "R_0603_10k");
    assert_eq!(set["free"], json!([1, 3, 4]));
    let (status, bad) = call(
        &s,
        Method::PUT,
        feeders,
        Some(&operator),
        Some(json!({"slot": 9, "part": "A", "count": 1})),
    )
    .await;
    assert_eq!(
        (status, bad["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_plugin_request"))
    );
    // a dry run takes nothing off the feeder
    let placements = json!([{"ref": "R1", "part": "R_0603_10k", "x": 20, "y": 30}, {"ref": "R2", "part": "R_0603_10k", "x": 25, "y": 30}]);
    let dry = run_to_done(
        &s,
        &operator,
        "p1",
        json!({"kind": "place", "placements": placements, "dry_run": true}),
    )
    .await;
    assert_eq!(dry["view"]["dry_run"], true);
    let (_, after) = call(&s, Method::GET, feeders, Some(&viewer), None).await;
    assert_eq!(after["feeders"][0]["count"], 20);
    let real = run_to_done(
        &s,
        &operator,
        "p1",
        json!({"kind": "place", "placements": placements}),
    )
    .await;
    assert_eq!(real["view"]["dry_run"], false);
    let (_, after) = call(&s, Method::GET, feeders, Some(&viewer), None).await;
    assert_eq!(after["feeders"][0]["count"], 18);
    // a calibration moves where the slot is picked from
    run_to_done(&s, &operator, "p1", json!({"kind": "calibrate", "what": "feeder", "target": "2", "offset_mm": {"x": 1.0, "y": 0.5}})).await;
    let (_, moved) = call(&s, Method::GET, feeders, Some(&viewer), None).await;
    assert_eq!(moved["feeders"][0]["offset_mm"]["x"], 1.0);
    assert_eq!(moved["feeders"][0]["x_mm"], 19.0);
    let d = detail(&s, &viewer, "p1").await;
    assert_eq!(d["calibrations"]["feeder:2"]["evidence"], "simulator");
    let (status, r) = prepare(
        &s,
        &operator,
        "p1",
        json!({"kind": "calibrate", "what": "feeder", "target": "7"}),
    )
    .await;
    assert_eq!(
        (status, r["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_plugin_request"))
    );
    // the route belongs to the pnp plugin only
    make_machine(&s, &boss, "l1", "laser", json!({})).await;
    let (status, r) = call(
        &s,
        Method::GET,
        "/api/v1/work/machines/l1/feeders",
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(
        (status, r["error"].as_str()),
        (StatusCode::NOT_IMPLEMENTED, Some("not_supported"))
    );
    // while a job runs the feeders cannot be changed
    make_machine(&s, &boss, "p2", "pnp", json!({"sim_rate": 1})).await;
    let (status, _) = call(
        &s,
        Method::PUT,
        "/api/v1/work/machines/p2/feeders",
        Some(&operator),
        Some(json!({"slot": 1, "part": "R_0603_10k", "count": 5})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, job) = prepare(&s, &operator, "p2", json!({"kind": "place", "placements": [{"ref": "R1", "part": "R_0603_10k", "x": 20, "y": 30}]})).await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    let id = job["id"].as_str().unwrap().to_string();
    let (status, started) = act(&s, &operator, &id, "start").await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let (status, busy) = call(
        &s,
        Method::PUT,
        "/api/v1/work/machines/p2/feeders",
        Some(&operator),
        Some(json!({"slot": 1, "clear": true})),
    )
    .await;
    assert_eq!(
        (status, busy["error"].as_str()),
        (StatusCode::CONFLICT, Some("machine_busy"))
    );
    let (status, _) = act(&s, &operator, &id, "cancel").await;
    assert_eq!(status, StatusCode::OK);
}
