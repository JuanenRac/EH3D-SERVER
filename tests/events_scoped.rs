//! The events of things that have an owner (tasks), on a real websocket: the owner and the administrators receive them, anybody else never does.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use eh3d_server::api::router;
use eh3d_server::auth::{Identity, Role};
use eh3d_server::config::ServerConfig;
use eh3d_server::state::AppState;
use eh3d_server::sync::Locked;
use eh3d_server::tasks::NewTask;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct Running {
    addr: SocketAddr,
    state: Arc<AppState>,
    dir: std::path::PathBuf,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.state.stop_all();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn start(name: &str) -> Running {
    let dir = std::env::temp_dir().join(format!("eh3d-server-wss-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let state = AppState::open(ServerConfig {
        data_dir: dir.clone(),
        tasks_selftest: true,
        ..ServerConfig::default()
    })
    .unwrap();
    let code = state.auth.locked().setup_code().unwrap().to_string();
    state
        .auth
        .locked()
        .setup(&code, "juan", "una-clave-larga")
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });
    Running { addr, state, dir }
}

/// A program token with the role; gives the id and the secret.
fn token(r: &Running, name: &str, role: Role) -> (String, String) {
    r.state
        .auth
        .locked()
        .create_token(name, role, eh3d_server::jobs::now(), None)
        .unwrap()
}

async fn connect(r: &Running, secret: &str) -> TcpStream {
    let mut stream = TcpStream::connect(r.addr).await.unwrap();
    let request = format!(
        "GET /api/v1/events HTTP/1.1\r\nHost: test\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nAuthorization: Bearer {secret}\r\n\r\n",
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.unwrap();
        head.push(byte[0]);
    }
    assert!(String::from_utf8_lossy(&head).starts_with("HTTP/1.1 101"));
    stream
}

async fn frame(stream: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut two = [0u8; 2];
    stream.read_exact(&mut two).await.ok()?;
    let mut len = u64::from(two[1] & 0x7f);
    if len == 126 {
        let mut b = [0u8; 2];
        stream.read_exact(&mut b).await.ok()?;
        len = u64::from(u16::from_be_bytes(b));
    } else if len == 127 {
        let mut b = [0u8; 8];
        stream.read_exact(&mut b).await.ok()?;
        len = u64::from_be_bytes(b);
    }
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).await.ok()?;
    Some((two[0] & 0x0f, payload))
}

/// Every text message that arrives within `wait`.
async fn texts(stream: &mut TcpStream, wait: Duration) -> Vec<Value> {
    let mut out = Vec::new();
    let end = tokio::time::Instant::now() + wait;
    while let Ok(Some((op, payload))) = tokio::time::timeout_at(end, frame(stream)).await {
        if op == 1 {
            out.push(serde_json::from_slice(&payload).unwrap());
        }
    }
    out
}

fn who(secret_owner: &str, role: Role) -> Identity {
    Identity {
        name: "x".into(),
        role,
        is_token: true,
        token_id: Some(secret_owner.trim_start_matches("token:").to_string()),
    }
}

#[tokio::test]
async fn the_events_of_a_task_reach_its_owner_and_the_administrators_and_nobody_else() {
    let r = start("tasks").await;
    let (ana_id, ana) = token(&r, "ana", Role::Operator);
    let (_, bob) = token(&r, "bob", Role::Viewer);
    let (_, boss) = token(&r, "boss", Role::Admin);
    let (mut sa, mut sb, mut sr) = (
        connect(&r, &ana).await,
        connect(&r, &bob).await,
        connect(&r, &boss).await,
    );
    // each one is told how things stand first
    for s in [&mut sa, &mut sb, &mut sr] {
        assert_eq!(
            texts(s, Duration::from_millis(300)).await[0]["type"],
            "hello"
        );
    }
    let state = r.state.clone();
    let made = tokio::task::spawn_blocking(move || {
        state.submit_task(
            &who(&format!("token:{ana_id}"), Role::Operator),
            NewTask {
                kind: "server.selftest".into(),
                name: Some("watched".into()),
                project: None,
                params: Some(json!({"steps": 2, "step_ms": 20})),
                idempotency_key: None,
            },
        )
    })
    .await
    .unwrap()
    .unwrap();
    let id = made.0.id;
    let (ea, eb, er) = (
        texts(&mut sa, Duration::from_millis(1200)).await,
        texts(&mut sb, Duration::from_millis(1200)).await,
        texts(&mut sr, Duration::from_millis(1200)).await,
    );
    let about = |events: &[Value]| {
        events
            .iter()
            .filter(|e| e["type"] == "task" && e["task"]["id"] == json!(id))
            .count()
    };
    assert!(about(&ea) >= 2, "the owner follows the task: {ea:?}");
    assert!(about(&er) >= 2, "the administrator too: {er:?}");
    assert!(ea.last().unwrap()["task"]["state"] == "done" || about(&ea) >= 3);
    assert!(
        eb.iter()
            .all(|e| e["type"] != "task" && e["type"] != "task_removed"),
        "the other never hears of it: {eb:?}"
    );
    // and the removal is told to the same people only
    let state = r.state.clone();
    let id2 = id.clone();
    tokio::task::spawn_blocking(move || {
        state
            .delete_task(&who("token:", Role::Admin), &id2)
            .unwrap();
    })
    .await
    .unwrap();
    let (ea, eb) = (
        texts(&mut sa, Duration::from_millis(500)).await,
        texts(&mut sb, Duration::from_millis(500)).await,
    );
    assert!(
        ea.iter()
            .any(|e| e["type"] == "task_removed" && e["id"] == json!(id)),
        "{ea:?}"
    );
    assert!(eb.is_empty(), "{eb:?}");
}
