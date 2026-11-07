//! The server listens on this computer only until an administrator opens it to the network: the default address, the switch of the security page, its confirmation and the
//! restart it needs.

mod common;

use axum::http::{Method, StatusCode};
use common::{admin, call, server, user};
use eh3d_server::config::ServerConfig;
use serde_json::json;

const POLICY: &str = "/api/v1/auth/policy";

#[test]
fn the_default_address_is_this_computer_and_the_port_of_the_server() {
    let c = ServerConfig::default();
    assert_eq!(c.listen, "127.0.0.1:18200".parse().unwrap());
    assert!(c.listen.ip().is_loopback() && !c.network_open);
    assert_eq!(c.bind_address(false), "127.0.0.1:18200".parse().unwrap());
    assert_eq!(c.bind_address(true), "0.0.0.0:18200".parse().unwrap());
}

#[test]
fn the_example_file_listens_like_the_default_and_names_the_switch() {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("server.cfg.example"),
    )
    .unwrap();
    let from_the_example = eh3d_server::config::parse(&text).expect("server.cfg.example reads");
    let default = ServerConfig::default();
    assert_eq!(from_the_example.listen, default.listen);
    assert_eq!(from_the_example.network_open, default.network_open);
    assert!(text
        .lines()
        .any(|l| l.trim_start().starts_with("network_open =")));
}

#[tokio::test]
async fn the_switch_is_listed_as_safe_and_editable_and_says_it_needs_a_restart() {
    let s = server("network-open-list");
    let boss = admin(&s).await;
    let (status, policy) = call(&s, Method::GET, POLICY, Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    let switch = policy["switches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["key"] == "network_open")
        .cloned()
        .expect("the switch is in the list");
    assert_eq!(
        (switch["value"].clone(), switch["safe"].clone()),
        (json!(false), json!(true))
    );
    assert!(!policy["attention"]
        .as_array()
        .unwrap()
        .contains(&json!("network_open")));
    assert!(policy["editable"]
        .as_array()
        .unwrap()
        .contains(&json!("network_open")));
    assert_eq!(policy["restart_required"], json!(["network_open"]));
}

#[tokio::test]
async fn opening_the_network_needs_the_confirmation_and_an_administrator() {
    let s = server("network-open-change");
    let boss = admin(&s).await;
    let (status, body) = call(
        &s,
        Method::PUT,
        POLICY,
        Some(&boss),
        Some(json!({"key": "network_open", "value": true})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("weak_confirm_required"))
    );
    let ana = user(&s, &boss, "ana", "operator").await;
    let (status, _) = call(
        &s,
        Method::PUT,
        POLICY,
        Some(&ana),
        Some(json!({"key": "network_open", "value": true, "confirm_weak": true})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = call(
        &s,
        Method::PUT,
        POLICY,
        Some(&boss),
        Some(json!({"key": "network_open", "value": "yes", "confirm_weak": true})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("bad_policy"))
    );
    let (status, policy) = call(
        &s,
        Method::PUT,
        POLICY,
        Some(&boss),
        Some(json!({"key": "network_open", "value": true, "confirm_weak": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    assert!(policy["attention"]
        .as_array()
        .unwrap()
        .contains(&json!("network_open")));
    // closing it again is the strict value: no confirmation
    let (status, policy) = call(
        &s,
        Method::PUT,
        POLICY,
        Some(&boss),
        Some(json!({"key": "network_open", "value": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    assert!(!policy["attention"]
        .as_array()
        .unwrap()
        .contains(&json!("network_open")));
}
