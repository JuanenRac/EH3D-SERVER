//! The single login of every application end to end through the real router: the claims, the list and the closing of the sessions, the limits, the policy of the passwords,
//! the switches of security that Studio shows, and what the audit says (and does not say).

mod common;

use axum::http::{Method, StatusCode};
use common::{add_user, admin, call, scoped_token, server, server_with, user, Server, PASSWORD};
use eh3d_server::api::router;
use serde_json::{json, Value};

async fn app_login(s: &Server, name: &str, password: &str, app: &str) -> (StatusCode, Value) {
    call(
        s,
        Method::POST,
        "/api/v1/auth/login",
        None,
        Some(json!({"name": name, "password": password, "app": app})),
    )
    .await
}

async fn token_of(s: &Server, name: &str, app: &str) -> String {
    let (status, body) = app_login(s, name, PASSWORD, app).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["token"].as_str().unwrap().to_string()
}

async fn claims_status(s: &Server, token: &str) -> StatusCode {
    call(s, Method::GET, "/api/v1/auth/session", Some(token), None)
        .await
        .0
}

#[tokio::test]
async fn a_login_says_which_application_it_is_for_and_the_claims_tell_it() {
    let s = server("auth-login");
    admin(&s).await;
    let (status, body) = app_login(&s, "juan", "una-clave-larga", "studio").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["app"], "studio");
    assert_eq!(body["role"], "admin");
    assert_eq!(body["expires_s"], 12 * 3600);
    let token = body["token"].as_str().unwrap().to_string();
    let id = body["session_id"].as_str().unwrap().to_string();
    let (status, claims) = call(&s, Method::GET, "/api/v1/auth/session", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(claims["app"], "studio");
    assert_eq!(claims["session_id"], id.as_str());
    assert_eq!(claims["is_token"], false);
    assert!(claims["expires"].as_u64().unwrap() > claims["created"].as_u64().unwrap());
    // the token is a token of the server like any other
    let (status, old) = call(&s, Method::GET, "/api/v1/session", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(old["name"], "juan");
    // a login made the first way has no application
    let (_, first) = call(
        &s,
        Method::POST,
        "/api/v1/login",
        None,
        Some(json!({"name": "juan", "password": "una-clave-larga"})),
    )
    .await;
    let plain = first["token"].as_str().unwrap();
    let (_, claims) = call(&s, Method::GET, "/api/v1/auth/session", Some(plain), None).await;
    assert_eq!(claims["app"], Value::Null);
    // the applications the server accepts, and the ones it does not
    let (_, apps) = call(&s, Method::GET, "/api/v1/auth/apps", Some(&token), None).await;
    let list: Vec<&str> = apps["apps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    for known in ["studio", "android", "ios", "watch", "hmi", "bungard-gui"] {
        assert!(list.contains(&known), "{known}");
    }
    assert_eq!(apps["known_only"], true);
    for bad in ["my-tool", "Studio", "", "a b", "../x"] {
        let (status, body) = app_login(&s, "juan", "una-clave-larga", bad).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{bad:?}");
        assert_eq!(body["error"], "bad_app");
    }
    let (status, body) = app_login(&s, "juan", "mala-clave-larga", "studio").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "bad_credentials");
    // no login, no claims
    let (status, _) = call(&s, Method::GET, "/api/v1/auth/session", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_settings_open_the_door_to_more_applications() {
    let extra = server_with("auth-extra", |c| c.auth_extra_apps = vec!["my-tool".into()]);
    admin(&extra).await;
    assert_eq!(
        app_login(&extra, "juan", "una-clave-larga", "my-tool")
            .await
            .0,
        StatusCode::OK
    );
    let open = server_with("auth-open", |c| c.auth_known_apps_only = false);
    admin(&open).await;
    assert_eq!(
        app_login(&open, "juan", "una-clave-larga", "anything-else")
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        app_login(&open, "juan", "una-clave-larga", "Not Valid")
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn the_open_logins_are_listed_closed_and_renewed() {
    let s = server("auth-sessions");
    let boss = admin(&s).await;
    user(&s, &boss, "ana1", "operator").await;
    user(&s, &boss, "bob1", "operator").await;
    let phone = token_of(&s, "ana1", "android").await;
    let studio = token_of(&s, "ana1", "studio").await;
    let other = token_of(&s, "bob1", "ios").await;
    let (status, list) = call(
        &s,
        Method::GET,
        "/api/v1/auth/sessions",
        Some(&studio),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sessions = list["sessions"].as_array().unwrap();
    // the two of ana (the login made by `user` for the setup of the test has no application), the one asking marked
    let apps: Vec<&str> = sessions
        .iter()
        .map(|x| x["app"].as_str().unwrap())
        .collect();
    assert!(
        apps.contains(&"android") && apps.contains(&"studio"),
        "{apps:?}"
    );
    assert!(sessions.iter().all(|x| x["name"] == "ana1"));
    let current: Vec<&Value> = sessions.iter().filter(|x| x["current"] == true).collect();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0]["app"], "studio");
    let phone_id = sessions.iter().find(|x| x["app"] == "android").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    // only an administrator lists everybody's
    let (status, body) = call(
        &s,
        Method::GET,
        "/api/v1/auth/sessions?all=true",
        Some(&studio),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    let (_, all) = call(
        &s,
        Method::GET,
        "/api/v1/auth/sessions?all=true",
        Some(&boss),
        None,
    )
    .await;
    assert!(all["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["name"] == "bob1"));
    // bob does not see, close or know ana's sessions
    let (status, body) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/auth/sessions/{phone_id}"),
        Some(&other),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("unknown_session"))
    );
    // ana closes her phone from the studio: its token stops working at once
    let (status, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/auth/sessions/{phone_id}"),
        Some(&studio),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&s, Method::GET, "/api/v1/auth/session", Some(&phone), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/auth/sessions/{phone_id}"),
        Some(&studio),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // an administrator closes anybody's
    let (_, bobs) = call(&s, Method::GET, "/api/v1/auth/sessions", Some(&other), None).await;
    let bob_id = bobs["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["current"] == true)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = call(
        &s,
        Method::DELETE,
        &format!("/api/v1/auth/sessions/{bob_id}"),
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        call(&s, Method::GET, "/api/v1/auth/session", Some(&other), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    // renewed
    let (status, renewed) = call(
        &s,
        Method::POST,
        "/api/v1/auth/refresh",
        Some(&studio),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renewed}");
    assert!(renewed["expires_s"].as_u64().unwrap() >= 12 * 3600 - 5);
    // a token of a program has no login to renew or to close
    let program = scoped_token(&s, &boss, "operator", &["projects:read"]).await;
    let (status, body) = call(
        &s,
        Method::POST,
        "/api/v1/auth/refresh",
        Some(&program),
        None,
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    let (_, claims) = call(
        &s,
        Method::GET,
        "/api/v1/auth/session",
        Some(&program),
        None,
    )
    .await;
    assert_eq!(
        (claims["is_token"].clone(), claims["app"].clone()),
        (json!(true), Value::Null)
    );
    assert_eq!(claims["scopes"][0], "projects:read");
    let (_, none) = call(
        &s,
        Method::GET,
        "/api/v1/auth/sessions",
        Some(&program),
        None,
    )
    .await;
    assert_eq!(none["sessions"].as_array().unwrap().len(), 0);
    // every login of ana at once
    let again = token_of(&s, "ana1", "watch").await;
    let (status, out) = call(
        &s,
        Method::POST,
        "/api/v1/auth/logout-all",
        Some(&again),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(out["closed"].as_u64().unwrap() >= 2);
    for token in [&studio, &again] {
        assert_eq!(
            call(&s, Method::GET, "/api/v1/auth/session", Some(token), None)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    // the logout of one
    let one = token_of(&s, "bob1", "hmi").await;
    assert_eq!(
        call(&s, Method::POST, "/api/v1/auth/logout", Some(&one), None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&s, Method::GET, "/api/v1/auth/session", Some(&one), None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn the_most_logins_and_the_lockout_come_from_the_settings() {
    let s = server_with("auth-limits", |c| {
        c.auth_max_sessions = 2;
        c.auth_max_failures = 3;
        c.auth_lockout_s = 60;
    });
    let boss = admin(&s).await;
    add_user(&s, &boss, "ana1", "operator").await;
    let first = token_of(&s, "ana1", "android").await;
    let second = token_of(&s, "ana1", "ios").await;
    let third = token_of(&s, "ana1", "studio").await;
    assert_eq!(
        claims_status(&s, &first).await,
        StatusCode::UNAUTHORIZED,
        "the oldest was closed"
    );
    assert_eq!(claims_status(&s, &second).await, StatusCode::OK);
    assert_eq!(claims_status(&s, &third).await, StatusCode::OK);
    // three wrong passwords block the name, even for the right one
    for _ in 0..3 {
        let (status, _) = app_login(&s, "bob9", "mala-clave-larga", "studio").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, body) = app_login(&s, "bob9", "mala-clave-larga", "studio").await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("throttled"))
    );
    // the other names are not blocked
    assert_eq!(
        app_login(&s, "ana1", PASSWORD, "studio").await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn the_passwords_follow_the_policy_of_the_settings() {
    let s = server_with("auth-policy-passwords", |c| {
        c.password_min_len = 12;
        c.password_classes = 2;
        c.password_deny_name = true;
    });
    let boss = admin(&s).await;
    for (name, password) in [
        ("ana1", "short-1"),
        ("ana1", "abcdefghijklmnop"),
        ("ana1", "xx-ANA1-clave-larga"),
    ] {
        let (status, body) = call(
            &s,
            Method::POST,
            "/api/v1/users",
            Some(&boss),
            Some(json!({"name": name, "role": "viewer", "password": password})),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{password}");
        assert_eq!(body["error"], "weak_password");
    }
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/users",
        Some(&boss),
        Some(json!({"name": "ana1", "role": "viewer", "password": "Clave-larga-77"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // the same for a person who changes their own
    let (status, body) = call(
        &s,
        Method::PUT,
        "/api/v1/account",
        Some(&boss),
        Some(json!({"old": "una-clave-larga", "new": "corta-1"})),
    )
    .await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::UNPROCESSABLE_ENTITY, Some("weak_password"))
    );
}

#[tokio::test]
async fn the_switches_of_security_are_shown_to_the_administrator_without_secrets() {
    let s = server_with("auth-policy-view", |c| {
        c.url_token = true;
        c.auth_max_failures = 50;
    });
    let boss = admin(&s).await;
    let viewer = user(&s, &boss, "view1", "viewer").await;
    let (status, body) = call(&s, Method::GET, "/api/v1/auth/policy", Some(&viewer), None).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("forbidden"))
    );
    let (status, policy) = call(&s, Method::GET, "/api/v1/auth/policy", Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK);
    let switch = |key: &str| {
        policy["switches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["key"] == key)
            .cloned()
            .unwrap_or_else(|| panic!("no switch {key}"))
    };
    assert_eq!(
        (
            switch("url_token")["value"].clone(),
            switch("url_token")["safe"].clone()
        ),
        (json!(true), json!(false))
    );
    assert_eq!(switch("auth_known_apps_only")["safe"], true);
    assert_eq!(switch("project_require_if_match")["value"], true);
    assert_eq!(switch("audit_denials")["value"], true);
    let attention: Vec<&str> = policy["attention"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert!(
        attention.contains(&"url_token") && attention.contains(&"auth_max_failures"),
        "{attention:?}"
    );
    assert!(!attention.contains(&"exit_on_panic"));
    let text = policy.to_string();
    for secret in [
        boss.as_str(),
        viewer.as_str(),
        "una-clave-larga",
        PASSWORD,
        "hash",
    ] {
        assert!(!text.contains(secret), "the policy shows {secret}");
    }
}

#[tokio::test]
async fn the_audit_has_the_logins_the_refusals_and_the_closings_and_no_secret() {
    let s = server("auth-audit");
    let boss = admin(&s).await;
    user(&s, &boss, "ana1", "operator").await;
    let studio = token_of(&s, "ana1", "studio").await;
    app_login(&s, "ana1", "mala-clave-larga", "android").await;
    app_login(&s, "ana1", PASSWORD, "my-tool").await;
    // a request without a login, with a secret in the query that must not reach the audit
    let (status, _) = call(
        &s,
        Method::GET,
        "/api/v1/users?ticket=TICKET-SECRET-1",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&s, Method::GET, "/api/v1/users", Some(&studio), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, list) = call(
        &s,
        Method::GET,
        "/api/v1/auth/sessions",
        Some(&studio),
        None,
    )
    .await;
    let id = list["sessions"][0]["id"].as_str().unwrap().to_string();
    call(
        &s,
        Method::DELETE,
        &format!("/api/v1/auth/sessions/{id}"),
        Some(&studio),
        None,
    )
    .await;
    let text = std::fs::read_to_string(s.dir.join("audit.jsonl")).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let has = |event: &str, key: &str, value: &str| {
        lines.iter().any(|l| l["event"] == event && l[key] == value)
    };
    assert!(has("login", "app", "studio"));
    assert!(has("login_failed", "code", "bad_credentials"));
    assert!(has("login_denied", "code", "bad_app"));
    assert!(has("denied", "path", "/api/v1/users"));
    assert!(lines
        .iter()
        .any(|l| l["event"] == "denied" && l["status"] == "401" && l["token"] == "no"));
    assert!(lines
        .iter()
        .any(|l| l["event"] == "denied" && l["status"] == "403" && l["token"] == "yes"));
    assert!(has("session_close", "of", "ana1"));
    // no password, no token, no secret of the query
    for secret in [
        studio.as_str(),
        boss.as_str(),
        PASSWORD,
        "una-clave-larga",
        "mala-clave-larga",
        "TICKET-SECRET-1",
    ] {
        assert!(!text.contains(secret), "the audit has {secret}");
    }
}

#[tokio::test]
async fn the_denials_are_left_out_of_the_audit_when_the_setting_says_so() {
    let s = server_with("auth-audit-off", |c| c.audit_denials = false);
    admin(&s).await;
    let (status, _) = call(&s, Method::GET, "/api/v1/users", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let text = std::fs::read_to_string(s.dir.join("audit.jsonl")).unwrap_or_default();
    assert!(!text.contains("\"denied\""));
}

#[tokio::test]
async fn a_job_asked_without_an_application_has_the_one_of_the_login() {
    let s = server("auth-app-of-job");
    let boss = admin(&s).await;
    user(&s, &boss, "ana1", "operator").await;
    let (status, _) = call(
        &s,
        Method::POST,
        "/api/v1/work/machines",
        Some(&boss),
        Some(json!({"id": "demo-1", "name": "Demo", "plugin": "demo"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let android = token_of(&s, "ana1", "android").await;
    let (status, job) = call(
        &s,
        Method::POST,
        "/api/v1/work/jobs",
        Some(&android),
        Some(json!({"machine": "demo-1", "request": {"kind": "demo"}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{job}");
    assert_eq!(job["app"], "android");
}

#[tokio::test]
async fn the_logins_kept_on_the_disk_keep_their_application_and_id() {
    let s = server_with("auth-persist", |c| c.persist_sessions = true);
    admin(&s).await;
    let (_, body) = app_login(&s, "juan", "una-clave-larga", "hmi").await;
    let token = body["token"].as_str().unwrap().to_string();
    let id = body["session_id"].as_str().unwrap().to_string();
    let again = eh3d_server::state::AppState::open(eh3d_server::config::ServerConfig {
        data_dir: s.dir.clone(),
        persist_sessions: true,
        ..eh3d_server::config::ServerConfig::default()
    })
    .unwrap();
    let restarted = Server {
        app: router(again.clone()),
        state: again,
        dir: std::env::temp_dir().join("eh3d-server-never-made-by-the-test"),
    };
    let (status, claims) = call(
        &restarted,
        Method::GET,
        "/api/v1/auth/session",
        Some(&token),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{claims}");
    assert_eq!(claims["app"], "hmi");
    assert_eq!(claims["session_id"], id.as_str());
}
