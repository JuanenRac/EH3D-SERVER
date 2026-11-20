//! The announcement of the server on the local network is a switch of the security page that is off by default, takes effect at once and needs the confirmation to be turned
//! on; the same goes for the routes of Moonraker and OctoPrint at the root of the address.

mod common;

use axum::http::{Method, StatusCode};
use common::{admin, call, server, user};
use serde_json::json;

const POLICY: &str = "/api/v1/auth/policy";

#[tokio::test]
async fn both_switches_are_listed_as_safe_and_editable_and_need_no_restart() {
    let s = server("mdns-list");
    let boss = admin(&s).await;
    let (status, policy) = call(&s, Method::GET, POLICY, Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    for key in [
        "mdns_announce",
        "compat_api",
        "compat_write",
        "slice_enabled",
    ] {
        let switch = policy["switches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["key"] == key)
            .cloned()
            .unwrap_or_else(|| panic!("no switch {key}"));
        assert_eq!(
            (switch["value"].clone(), switch["safe"].clone()),
            (json!(false), json!(true)),
            "{key}"
        );
        assert!(policy["editable"].as_array().unwrap().contains(&json!(key)));
        assert!(!policy["restart_required"]
            .as_array()
            .unwrap()
            .contains(&json!(key)));
    }
}

#[tokio::test]
async fn turning_them_on_needs_the_confirmation_and_an_administrator_and_off_does_not() {
    let s = server("mdns-change");
    let boss = admin(&s).await;
    let ana = user(&s, &boss, "ana", "operator").await;
    for key in [
        "mdns_announce",
        "compat_api",
        "compat_write",
        "slice_enabled",
    ] {
        let (status, body) = call(
            &s,
            Method::PUT,
            POLICY,
            Some(&boss),
            Some(json!({"key": key, "value": true})),
        )
        .await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::CONFLICT, Some("weak_confirm_required")),
            "{key}"
        );
        let (status, _) = call(
            &s,
            Method::PUT,
            POLICY,
            Some(&ana),
            Some(json!({"key": key, "value": true, "confirm_weak": true})),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{key}");
        let (status, policy) = call(
            &s,
            Method::PUT,
            POLICY,
            Some(&boss),
            Some(json!({"key": key, "value": true, "confirm_weak": true})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{policy}");
        assert!(policy["attention"]
            .as_array()
            .unwrap()
            .contains(&json!(key)));
        assert!(s.state.policy.get().value(key) == Some(json!(true)));
        let (status, policy) = call(
            &s,
            Method::PUT,
            POLICY,
            Some(&boss),
            Some(json!({"key": key, "value": false})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{policy}");
        assert!(!policy["attention"]
            .as_array()
            .unwrap()
            .contains(&json!(key)));
    }
}
