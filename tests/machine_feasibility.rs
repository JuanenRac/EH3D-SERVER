//! A feasibility pass over every machine configuration the server can simulate (`laser`, `pnp`, `cnc`, `pcb`, `printer3d`, `cam`, `gerber`, `vision`, `cad`, the machines of
//! `bungard` and the `demo` machine): each one is made, shows up in the list of the machines, is at rest, keeps limits that make sense and says in its detail that it has only
//! run against a simulator, so that no client shows it as proven on hardware. It is the check that runs in the CI on every one of them at once.

mod common;

use axum::http::{Method, StatusCode};
use common::{admin, call, Server};
use serde_json::{json, Value};

/// The kinds of plugin that make their machines through `/api/v1/work/machines`, and whether each one is a model of a machine (so it must say it is only a simulator).
const KINDS: [(&str, bool); 10] = [
    ("laser", true),
    ("pnp", true),
    ("cnc", true),
    ("pcb", true),
    ("printer3d", true),
    ("cam", true),
    ("gerber", true),
    ("vision", true),
    ("cad", true),
    ("demo", false),
];

async fn machines(s: &Server, boss: &str) -> Vec<Value> {
    let (status, body) = call(s, Method::GET, "/api/v1/work/machines", Some(boss), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["machines"].as_array().unwrap().clone()
}

#[tokio::test]
async fn every_simulated_machine_configuration_starts_answers_and_says_it_is_a_simulator() {
    let s = common::server("feasibility-all");
    let boss = admin(&s).await;
    // the plugins the server has: every kind of the table is there and no plugin that makes machines is left out of it
    let (status, plugins) = call(&s, Method::GET, "/api/v1/plugins", Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK);
    for p in plugins["plugins"].as_array().unwrap() {
        let kind = p["kind"].as_str().unwrap();
        if p["manages_machines"] == true {
            assert!(
                KINDS.iter().any(|(k, _)| *k == kind),
                "the plugin {kind} makes machines and the feasibility pass does not cover it"
            );
        }
    }
    for (kind, _) in KINDS {
        let (status, body) = call(
            &s,
            Method::POST,
            "/api/v1/work/machines",
            Some(&boss),
            Some(json!({"id": format!("f-{kind}"), "name": kind, "plugin": kind, "settings": {}})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{kind}: {body}");
    }
    // the machines of bungard are the ones of the older routes, on the simulator
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/machines",
        Some(&boss),
        Some(json!({"id": "f-bungard", "name": "bungard", "connection": {"kind": "demo"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let list = machines(&s, &boss).await;
    for (kind, model) in KINDS {
        let id = format!("f-{kind}");
        let m = list
            .iter()
            .find(|m| m["id"] == id.as_str())
            .unwrap_or_else(|| panic!("{kind}: the machine is not in the list"));
        assert_eq!(m["plugin"], kind);
        let state = m["state"].as_str().unwrap_or("");
        assert!(
            !state.is_empty() && !["emergency", "error", "running"].contains(&state),
            "{kind}: a new machine is at rest, not '{state}'"
        );
        // it answers on its own route too
        let (status, one) = call(
            &s,
            Method::GET,
            &format!("/api/v1/work/machines/{id}"),
            Some(&boss),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{kind}: {one}");
        // the limits it declares are whole numbers above zero
        let capabilities = &m["capabilities"];
        if let Some(limits) = capabilities["limits"].as_object() {
            assert!(!limits.is_empty(), "{kind}: empty limits");
            for (name, value) in limits {
                assert!(
                    value.as_u64().is_some_and(|n| n > 0),
                    "{kind}: the limit {name} is {value}"
                );
            }
        }
        // every request it takes is told, and every setting it takes has a help text
        assert!(
            capabilities["requests"]
                .as_array()
                .is_some_and(|r| !r.is_empty()),
            "{kind}: no requests"
        );
        for (key, help) in capabilities["settings"].as_object().into_iter().flatten() {
            assert!(
                help.as_str().is_some_and(|h| !h.is_empty()),
                "{kind}: the setting {key} has no help"
            );
        }
        if model {
            assert_eq!(capabilities["evidence"], "simulator", "{kind}");
            assert_eq!(m["detail"]["evidence"], "simulator", "{kind}: {m}");
            assert_eq!(m["detail"]["simulator_only"], true, "{kind}: {m}");
        }
        // a setting the plugin does not have is refused, never kept
        if model {
            let (status, body) = call(
                &s,
                Method::PUT,
                &format!("/api/v1/work/machines/{id}"),
                Some(&boss),
                Some(json!({"settings": {"no_such_setting_x": 1}})),
            )
            .await;
            assert!(
                status.is_client_error(),
                "{kind}: a setting that does not exist was accepted: {body}"
            );
        }
    }
    let bungard = list
        .iter()
        .find(|m| m["id"] == "f-bungard")
        .expect("the machine of bungard is in the list");
    assert_eq!(bungard["plugin"], "bungard");
    assert!(bungard["state"].is_string());
}
