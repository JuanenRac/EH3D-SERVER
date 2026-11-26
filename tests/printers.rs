//! The printers the server watches, through the real router: the switch, the roles, the secret that is never shown or audited, several printers read at once from printers
//! in memory (a Moonraker on `127.0.0.1` with a port the system gives, that writes down every request), and the proof that no route and no reading can write to a
//! printer. No test opens a real network and nothing here has run on a real printer.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::{admin, call, scoped_token, send, server, server_with, user, Server};
use eh3d_server::printers::poll_all;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const KEY: &str = "key-that-must-not-show";

fn on(name: &str) -> Server {
    server_with(name, |c| {
        c.printers_enabled = true;
        c.printer_timeout_s = 2;
    })
}

/// A Moonraker in memory that answers `body` to the query and keeps the first line and the key of every request.
async fn moonraker(body: Arc<Mutex<String>>, seen: Arc<Mutex<Vec<(String, String)>>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let (body, seen) = (body.clone(), seen.clone());
            tokio::spawn(async move {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 1024];
                while !buffer.windows(4).any(|w| w == b"\r\n\r\n") {
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buffer.extend_from_slice(&chunk[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&buffer).to_string();
                let first = text.lines().next().unwrap_or("").to_string();
                let key = text
                    .lines()
                    .find_map(|l| l.strip_prefix("X-Api-Key: "))
                    .unwrap_or("")
                    .to_string();
                seen.lock().unwrap().push((first.clone(), key));
                let answer = if first.contains("/server/webcams/list") {
                    json!({"result": {"webcams": []}}).to_string()
                } else {
                    body.lock().unwrap().clone()
                };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{answer}",
                    answer.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    address
}

fn klipper(progress: f64) -> String {
    json!({"result": {"status": {
        "extruder": {"temperature": 205.0, "target": 210.0}, "heater_bed": {"temperature": 60.0, "target": 60.0},
        "print_stats": {"state": "printing", "filename": "part.gcode", "print_duration": 100.0},
        "virtual_sdcard": {"progress": progress}, "webhooks": {"state": "ready"},
    }}})
    .to_string()
}

async fn register(s: &Server, boss: &str, body: Value) -> Value {
    let (status, v) = call(s, Method::POST, "/api/v1/printers", Some(boss), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{v}");
    v
}

async fn read_all(s: &Server) {
    for handle in poll_all(&s.state) {
        tokio::time::timeout(Duration::from_secs(30), handle)
            .await
            .expect("a reading ends")
            .unwrap();
    }
}

async fn list(s: &Server, token: &str) -> Value {
    let (status, v) = call(s, Method::GET, "/api/v1/printers", Some(token), None).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    v
}

#[tokio::test]
async fn with_the_switch_off_every_route_says_so_and_nothing_is_read() {
    let s = server("printers-off");
    let boss = admin(&s).await;
    for (method, path, body) in [
        (Method::GET, "/api/v1/printers", None),
        (Method::GET, "/api/v1/printers/printer-1", None),
        (
            Method::POST,
            "/api/v1/printers",
            Some(json!({"name": "a", "family": "moonraker", "link": "sim"})),
        ),
        (Method::PUT, "/api/v1/printers/printer-1", Some(json!({}))),
        (Method::DELETE, "/api/v1/printers/printer-1", None),
    ] {
        let (status, v) = call(&s, method.clone(), path, Some(&boss), body).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{method} {path}: {v}");
        assert_eq!(v["error"], "printers_off");
    }
    assert!(poll_all(&s.state).is_empty());
    assert!(eh3d_server::printers::spawn(s.state.clone()).is_none());
}

#[tokio::test]
async fn several_printers_are_read_at_once_and_only_looked_at() {
    let s = on("printers-read");
    let boss = admin(&s).await;
    let body = Arc::new(Mutex::new(klipper(0.5)));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let address = moonraker(body.clone(), seen.clone()).await;
    // a port that nobody listens on
    let gone = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().to_string()
    };
    register(&s, &boss, json!({"name": "Klipper", "family": "moonraker", "link": "lan", "address": address, "secret": KEY})).await;
    register(
        &s,
        &boss,
        json!({"name": "Off the shelf", "family": "moonraker", "link": "lan", "address": gone}),
    )
    .await;
    register(
        &s,
        &boss,
        json!({"name": "Pretend", "family": "bambu_lan", "link": "sim"}),
    )
    .await;
    register(&s, &boss, json!({"name": "Real Bambu", "family": "bambu_lan", "link": "lan", "address": "127.0.0.1:9", "serial": "01P00A111111111", "secret": KEY})).await;
    let before = list(&s, &boss).await;
    assert_eq!(before["schema"], "eh3d.printers.status/1");
    assert_eq!(before["read_only"], true);
    assert_eq!(before["printers"][0]["health"], "unknown");
    assert!(
        seen.lock().unwrap().is_empty(),
        "reading the list opens no connection"
    );

    let mut events = s.state.events.subscribe();
    for _ in 0..5 {
        read_all(&s).await;
    }
    let now = list(&s, &boss).await;
    let health: Vec<&str> = now["printers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["health"].as_str().unwrap())
        .collect();
    // the real Bambu printer is not reached: bambu_lan_enabled is off
    assert_eq!(health, vec!["online", "offline", "online", "off"]);
    let first = &now["printers"][0];
    assert_eq!(first["status"]["state"], "printing");
    assert_eq!(first["status"]["progress_pct"], 50.0);
    assert_eq!(first["status"]["remaining_estimated"], true);
    assert_eq!(first["evidence"], "unvalidated");
    assert_eq!(now["printers"][1]["status"], Value::Null);
    assert_eq!(now["printers"][2]["evidence"], "simulator");
    assert_eq!(now["printers"][3]["last_error"], "bambu_lan_off");
    assert!(!now.to_string().contains(KEY));

    let told = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        told.contains("\"type\":\"printer\"") && !told.contains(KEY),
        "{told}"
    );

    // a damaged and then a cut answer: the printer is not shown as what it was
    for damaged in ["{\"result\":{\"status\":", "[1,2,3]", ""] {
        *body.lock().unwrap() = damaged.to_string();
        read_all(&s).await;
        let p = list(&s, &boss).await["printers"][0].clone();
        assert_eq!(p["health"], "bad_report", "{damaged:?}");
        assert_eq!(p["status"], Value::Null);
    }

    // every request the printer got was a look, with its key and nothing else
    let seen = seen.lock().unwrap();
    assert!(seen.len() >= 8, "{}", seen.len());
    for (line, key) in seen.iter() {
        assert!(
            line.starts_with("GET /printer/objects/query?")
                || line.starts_with("GET /server/webcams/list "),
            "{line}"
        );
        assert_eq!(key, KEY);
    }
}

#[tokio::test]
async fn there_is_no_route_to_ask_a_printer_to_do_anything() {
    let s = on("printers-no-write");
    let boss = admin(&s).await;
    register(
        &s,
        &boss,
        json!({"id": "p1", "name": "a", "family": "moonraker", "link": "sim"}),
    )
    .await;
    for path in ["/api/v1/printers/p1", "/api/v1/printers"] {
        for method in [Method::PATCH, Method::OPTIONS] {
            let r = send(&s, method.clone(), path, Some(&boss), &[], None, None).await;
            assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED, "{method} {path}");
        }
    }
    let r = send(
        &s,
        Method::POST,
        "/api/v1/printers/p1",
        Some(&boss),
        &[],
        None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    for tail in [
        "pause",
        "resume",
        "stop",
        "cancel",
        "print",
        "gcode",
        "files",
        "temperature",
        "emergency-stop",
        "command",
        "poll",
    ] {
        for method in [Method::GET, Method::POST, Method::PUT] {
            let path = format!("/api/v1/printers/p1/{tail}");
            let r = send(&s, method.clone(), &path, Some(&boss), &[], None, None).await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{method} {path}");
        }
    }
    // a change cannot carry an order
    for body in [
        json!({"state": "paused"}),
        json!({"gcode": "M112"}),
        json!({"target_c": 200}),
    ] {
        let (status, v) = call(
            &s,
            Method::PUT,
            "/api/v1/printers/p1",
            Some(&boss),
            Some(body),
        )
        .await;
        assert_eq!(
            (status, v["error"].clone()),
            (StatusCode::BAD_REQUEST, json!("bad_request"))
        );
    }
    // and the sources of the readers have no word that moves a printer
    for source in [
        include_str!("../src/printer_readers.rs"),
        include_str!("../src/printers.rs"),
        include_str!("../src/api_printers.rs"),
    ] {
        let code = source.replace("\r\n", "\n");
        let code = code.split("\n#[cfg(test)]").next().unwrap();
        for word in [
            "request_document",
            "BambuCommand",
            "BambuTlsLink",
            "PrinterDriver",
            "MoonrakerDriver",
            "\"POST\"",
            "\"PUT\"",
            "\"DELETE\"",
            "gcode_line",
            "/printer/print",
        ] {
            assert!(!code.contains(word), "{word}");
        }
    }
}

#[tokio::test]
async fn the_roles_the_scopes_the_audit_and_the_secret_that_is_never_shown() {
    let s = on("printers-roles");
    let boss = admin(&s).await;
    let viewer = user(&s, &boss, "ana", "viewer").await;
    let operator = user(&s, &boss, "luis", "operator").await;
    let new = json!({"name": "Bambu", "family": "bambu_lan", "link": "lan", "address": "10.255.255.1", "serial": "ABC123456", "secret": KEY, "enabled": false});
    for token in [&viewer, &operator] {
        let (status, _) = call(
            &s,
            Method::POST,
            "/api/v1/printers",
            Some(token),
            Some(new.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let made = register(&s, &boss, new.clone()).await;
    assert_eq!(
        (made["secret"].as_str(), made["serial"].as_str()),
        (Some("***"), Some("***3456"))
    );
    assert_eq!(made["health"], "disabled");
    let (status, v) = call(&s, Method::POST, "/api/v1/printers", Some(&boss), Some(new)).await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::CONFLICT, json!("printer_exists"))
    );
    let one = "/api/v1/printers/printer-1";
    for token in [&viewer, &operator] {
        let (status, v) = call(&s, Method::GET, one, Some(token), None).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!v.to_string().contains(KEY));
        let (status, _) = call(
            &s,
            Method::PUT,
            one,
            Some(token),
            Some(json!({"name": "x"})),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = call(&s, Method::DELETE, one, Some(token), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let program = scoped_token(&s, &boss, "viewer", &["tasks:read"]).await;
    let (status, v) = call(&s, Method::GET, one, Some(&program), None).await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::FORBIDDEN, json!("forbidden_scope"))
    );
    let (status, _) = call(&s, Method::GET, one, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // a body that does not read is refused without quoting what it held
    let r = send(
        &s,
        Method::POST,
        "/api/v1/printers",
        Some(&boss),
        &[],
        Some(format!("{{\"secret\": [\"{KEY}\"]}}").into_bytes()),
        Some("application/json"),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(!String::from_utf8_lossy(&r.bytes).contains(KEY));

    let (status, v) = call(
        &s,
        Method::PUT,
        one,
        Some(&boss),
        Some(json!({"name": "Left", "secret": "***"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        (v["name"].as_str(), v["secret"].as_str()),
        (Some("Left"), Some("***"))
    );
    let (status, v) = call(
        &s,
        Method::PUT,
        one,
        Some(&boss),
        Some(json!({"address": "bad host"})),
    )
    .await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::UNPROCESSABLE_ENTITY, json!("bad_printer"))
    );
    let (status, _) = call(&s, Method::DELETE, one, Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, v) = call(&s, Method::GET, one, Some(&boss), None).await;
    assert_eq!(
        (status, v["error"].clone()),
        (StatusCode::NOT_FOUND, json!("unknown_printer"))
    );

    let (_, audit) = call(&s, Method::GET, "/api/v1/audit", Some(&boss), None).await;
    let text = audit.to_string();
    for event in ["printer_add", "printer_change", "printer_remove"] {
        assert!(text.contains(event), "{event} is not in the audit: {text}");
    }
    assert!(!text.contains(KEY) && !text.contains("ABC123456"), "{text}");
    for entry in std::fs::read_dir(&s.dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let held = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).to_string();
            assert!(!held.contains(KEY), "{} holds the key", path.display());
        }
    }
}
