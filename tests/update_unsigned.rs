//! An update is installed only with a signature unless an administrator allows the hash alone: the switch of the security page, its confirmation, and what the request to the
//! helper of the updates carries.

mod common;

use axum::http::{Method, StatusCode};
use common::{admin, call, Server};
use eh3d_server::api::router;
use eh3d_server::config::ServerConfig;
use eh3d_server::state::AppState;
use serde_json::{json, Value};

const POLICY: &str = "/api/v1/auth/policy";

fn with_updates(name: &str) -> (Server, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "eh3d-server-unsigned-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let folder = dir.join("meet");
    std::fs::create_dir_all(&folder).unwrap();
    let cfg = ServerConfig {
        data_dir: dir.join("data"),
        updates_dir: Some(folder.clone()),
        ..ServerConfig::default()
    };
    let state = AppState::open(cfg).unwrap();
    let server = Server {
        app: router(state.clone()),
        state,
        dir: dir.clone(),
    };
    (server, folder)
}

fn request_of(folder: &std::path::Path) -> Value {
    let file = folder.join("request.json");
    let value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    std::fs::remove_file(&file).unwrap();
    value
}

#[test]
fn the_default_is_that_a_signature_is_required() {
    let c = ServerConfig::default();
    assert!(!c.update_allow_unsigned);
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("server.cfg.example"),
    )
    .unwrap();
    let from_the_example = eh3d_server::config::parse(&text).expect("server.cfg.example reads");
    assert_eq!(
        from_the_example.update_allow_unsigned,
        c.update_allow_unsigned
    );
    assert!(text
        .lines()
        .any(|l| l.trim_start().starts_with("update_allow_unsigned =")));
}

#[tokio::test]
async fn the_apply_asks_the_helper_for_a_signature_until_the_switch_allows_the_hash_alone() {
    let (s, folder) = with_updates("apply");
    let boss = admin(&s).await;
    let (_, policy) = call(&s, Method::GET, POLICY, Some(&boss), None).await;
    let switch = policy["switches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["key"] == "update_allow_unsigned")
        .cloned()
        .expect("the switch is in the list");
    assert_eq!(
        (switch["value"].clone(), switch["safe"].clone()),
        (json!(false), json!(true))
    );
    assert!(policy["editable"]
        .as_array()
        .unwrap()
        .contains(&json!("update_allow_unsigned")));
    // the default: the request says the helper must ask for the signature
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/updates/apply",
        Some(&boss),
        Some(json!({"components": ["studio"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let request = request_of(&folder);
    assert_eq!(request["action"], "apply");
    assert_eq!(request["allow_unsigned"], json!(false));
    // looser than recommended: it needs the confirmation
    let (status, body) = call(
        &s,
        Method::PUT,
        POLICY,
        Some(&boss),
        Some(json!({"key": "update_allow_unsigned", "value": true})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::CONFLICT, Some("weak_confirm_required"))
    );
    let (status, policy) = call(
        &s,
        Method::PUT,
        POLICY,
        Some(&boss),
        Some(json!({"key": "update_allow_unsigned", "value": true, "confirm_weak": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{policy}");
    assert!(policy["attention"]
        .as_array()
        .unwrap()
        .contains(&json!("update_allow_unsigned")));
    // it takes effect at once: the next apply carries it, a check and an undo do not
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/updates/apply",
        Some(&boss),
        Some(json!({"components": ["studio"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(request_of(&folder)["allow_unsigned"], json!(true));
    let (status, _) = call(&s, Method::POST, "/api/v1/updates/check", Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(request_of(&folder).get("allow_unsigned").is_none());
    // and back to the strict value without a confirmation
    let (status, _) = call(
        &s,
        Method::PUT,
        POLICY,
        Some(&boss),
        Some(json!({"key": "update_allow_unsigned", "value": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/updates/apply",
        Some(&boss),
        Some(json!({"components": ["studio"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(request_of(&folder)["allow_unsigned"], json!(false));
    let _ = std::fs::remove_dir_all(&s.dir);
}
