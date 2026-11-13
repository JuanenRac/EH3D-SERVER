//! The logins can be kept for the restart that an update of the server makes: the switch of the security page, what it is by default, and what happens to the logins when
//! the server starts again.

mod common;

use axum::http::{Method, StatusCode};
use common::{admin, call, Server};
use eh3d_server::api::router;
use eh3d_server::config::ServerConfig;
use eh3d_server::state::AppState;
use serde_json::{json, Value};

const POLICY: &str = "/api/v1/auth/policy";
const KEPT: &str = "sessions-update.json";

fn with_updates(name: &str, seconds: u64) -> (Server, std::path::PathBuf, ServerConfig) {
    let dir = std::env::temp_dir().join(format!("eh3d-server-keep-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let folder = dir.join("meet");
    std::fs::create_dir_all(&folder).unwrap();
    let cfg = ServerConfig {
        data_dir: dir.join("data"),
        updates_dir: Some(folder.clone()),
        update_keep_logins_s: seconds,
        ..ServerConfig::default()
    };
    let state = AppState::open(cfg.clone()).unwrap();
    let server = Server {
        app: router(state.clone()),
        state,
        dir: dir.clone(),
    };
    (server, folder, cfg)
}

/// A server that starts again on the folder of the data of `cfg` (the folder is not removed when it is dropped: the first server owns it).
fn start_again(cfg: &ServerConfig, tag: &str) -> Server {
    let state = AppState::open(cfg.clone()).unwrap();
    Server {
        app: router(state.clone()),
        state,
        dir: std::env::temp_dir().join(format!(
            "eh3d-server-keep-none-{tag}-{}",
            std::process::id()
        )),
    }
}

async fn apply(s: &Server, token: &str, components: Value) -> StatusCode {
    let (status, _) = call(
        s,
        Method::POST,
        "/api/v1/updates/apply",
        Some(token),
        Some(json!({ "components": components })),
    )
    .await;
    status
}

async fn turn_on(s: &Server, token: &str) {
    let (status, body) = call(
        s,
        Method::PUT,
        POLICY,
        Some(token),
        Some(json!({"key": "update_keep_logins", "value": true, "confirm_weak": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

async fn logged_in(s: &Server, token: &str) -> bool {
    let (status, _) = call(s, Method::GET, "/api/v1/session", Some(token), None).await;
    status == StatusCode::OK
}

#[test]
fn the_default_is_that_the_logins_end_with_the_restart_and_the_example_file_says_so() {
    let c = ServerConfig::default();
    assert!(!c.update_keep_logins && c.update_keep_logins_s == 300 && !c.persist_sessions);
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("server.cfg.example"),
    )
    .unwrap();
    let from_the_example = eh3d_server::config::parse(&text).expect("server.cfg.example reads");
    assert_eq!(from_the_example.update_keep_logins, c.update_keep_logins);
    assert_eq!(
        from_the_example.update_keep_logins_s,
        c.update_keep_logins_s
    );
    for key in ["update_keep_logins", "update_keep_logins_s"] {
        assert!(text
            .lines()
            .any(|l| l.trim_start().starts_with(&format!("{key} ="))));
    }
    assert!(eh3d_server::config::parse("update_keep_logins_s = 29\n").is_err());
    assert!(eh3d_server::config::parse("update_keep_logins_s = 901\n").is_err());
    assert!(eh3d_server::config::parse("update_keep_logins = maybe\n").is_err());
}

#[tokio::test]
async fn the_switch_is_on_the_security_page_looser_than_recommended_and_needs_the_confirmation() {
    let (s, _, _) = with_updates("switch", 300);
    let boss = admin(&s).await;
    let (_, policy) = call(&s, Method::GET, POLICY, Some(&boss), None).await;
    let switch = policy["switches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["key"] == "update_keep_logins")
        .cloned()
        .expect("the switch is in the list");
    assert_eq!(
        (switch["value"].clone(), switch["safe"].clone()),
        (json!(false), json!(true))
    );
    assert!(policy["editable"]
        .as_array()
        .unwrap()
        .contains(&json!("update_keep_logins")));
    let (status, body) = call(
        &s,
        Method::PUT,
        POLICY,
        Some(&boss),
        Some(json!({"key": "update_keep_logins", "value": true})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("weak_confirm_required"))
    );
    turn_on(&s, &boss).await;
    let (_, policy) = call(&s, Method::GET, POLICY, Some(&boss), None).await;
    assert!(policy["attention"]
        .as_array()
        .unwrap()
        .contains(&json!("update_keep_logins")));
}

#[tokio::test]
async fn with_the_switch_off_an_update_of_the_server_keeps_nothing() {
    let (s, _, cfg) = with_updates("off", 300);
    let boss = admin(&s).await;
    assert_eq!(apply(&s, &boss, json!(["server"])).await, StatusCode::OK);
    assert!(!cfg.data_dir.join(KEPT).exists());
    let again = start_again(&cfg, "off");
    assert!(
        !logged_in(&again, &boss).await,
        "the restart ends the logins"
    );
}

#[tokio::test]
async fn with_the_switch_on_an_update_of_the_server_keeps_the_logins_for_the_restart_only() {
    let (s, folder, cfg) = with_updates("on", 300);
    let boss = admin(&s).await;
    turn_on(&s, &boss).await;
    // an update of the studio alone does not restart the server: nothing is kept
    assert_eq!(apply(&s, &boss, json!(["studio"])).await, StatusCode::OK);
    assert!(!cfg.data_dir.join(KEPT).exists());
    std::fs::remove_file(folder.join("request.json")).unwrap();
    // an update of the server does
    assert_eq!(apply(&s, &boss, json!(["server"])).await, StatusCode::OK);
    let file = cfg.data_dir.join(KEPT);
    let kept = std::fs::read_to_string(&file).unwrap();
    assert!(!kept.contains(&boss), "the token itself is not kept");
    // the server starts again on the same folder: the person is still logged in, once
    let again = start_again(&cfg, "on");
    assert!(logged_in(&again, &boss).await);
    assert!(!file.exists(), "the file is used once");
    // and the setting of the switch stayed (it is kept in policy.json)
    let (_, policy) = call(&again, Method::GET, POLICY, Some(&boss), None).await;
    assert!(policy["switches"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["key"] == "update_keep_logins" && x["value"] == json!(true)));
    // a login is not kept for ever: the next restart, with no update asked for, ends it
    let third = start_again(&cfg, "third");
    assert!(!logged_in(&third, &boss).await);
}

#[tokio::test]
async fn a_login_that_is_closed_after_the_request_does_not_come_back() {
    let (s, _, cfg) = with_updates("logout", 300);
    let boss = admin(&s).await;
    turn_on(&s, &boss).await;
    assert_eq!(apply(&s, &boss, json!(["server"])).await, StatusCode::OK);
    let (status, _) = call(&s, Method::POST, "/api/v1/logout", Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK);
    let again = start_again(&cfg, "logout");
    assert!(!logged_in(&again, &boss).await);
}
