//! The websocket of the events, on a real socket: it pings, it closes a connection that goes quiet, and it tells a client that fell behind to ask again.
//! The client is written by hand (the handshake and the frames) so the test needs nothing but the standard protocol.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use eh3d_server::api::router;
use eh3d_server::config::ServerConfig;
use eh3d_server::state::AppState;
use eh3d_server::sync::Locked;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct Running {
    addr: SocketAddr,
    state: Arc<AppState>,
    token: String,
    dir: std::path::PathBuf,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn start(name: &str, ping_s: u64, idle_s: u64) -> Running {
    let dir = std::env::temp_dir().join(format!("eh3d-server-ws-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let state = AppState::open(ServerConfig {
        data_dir: dir.clone(),
        events_ping_s: ping_s,
        events_idle_s: idle_s,
        ..ServerConfig::default()
    })
    .unwrap();
    let code = state.auth.locked().setup_code().unwrap().to_string();
    state
        .auth
        .locked()
        .setup(&code, "juan", "una-clave-larga")
        .unwrap();
    let (token, _, _) = state
        .auth
        .locked()
        .login("juan", "una-clave-larga", eh3d_server::jobs::now())
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
    Running {
        addr,
        state,
        token,
        dir,
    }
}

async fn connect(r: &Running) -> TcpStream {
    let mut stream = TcpStream::connect(r.addr).await.unwrap();
    let request = format!(
        "GET /api/v1/events HTTP/1.1\r\nHost: test\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nAuthorization: Bearer {}\r\n\r\n",
        r.token
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.unwrap();
        head.push(byte[0]);
    }
    assert!(
        String::from_utf8_lossy(&head).starts_with("HTTP/1.1 101"),
        "the upgrade was accepted"
    );
    stream
}

/// One frame from the server (they are never masked): its opcode and its payload. `None` when the connection ended.
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

/// A masked pong with the same payload, as a client sends it.
async fn pong(stream: &mut TcpStream, payload: &[u8]) {
    let mask = [1u8, 2, 3, 4];
    let mut out = vec![0x8A, 0x80 | payload.len() as u8];
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    stream.write_all(&out).await.unwrap();
}

#[tokio::test]
async fn a_client_that_answers_the_pings_stays_and_one_that_does_not_is_closed() {
    let r = start("ping", 1, 3).await;
    let mut ws = connect(&r).await;
    let (op, text) = frame(&mut ws).await.unwrap();
    assert_eq!(op, 1, "the first frame is the hello");
    assert!(String::from_utf8_lossy(&text).contains("\"hello\""));

    // it answers every ping for longer than the idle time: the connection stays
    let until = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut pings = 0;
    while tokio::time::Instant::now() < until {
        let got = tokio::time::timeout(Duration::from_secs(3), frame(&mut ws))
            .await
            .expect("a ping arrives within the time of two")
            .expect("the connection is still open");
        if got.0 == 9 {
            pings += 1;
            pong(&mut ws, &got.1).await;
        }
    }
    assert!(pings >= 3, "the server pings: {pings}");

    // it goes quiet: no pong, nothing: the server lets go within the idle time and a ping
    let quiet_since = tokio::time::Instant::now();
    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match frame(&mut ws).await {
                None => break,
                Some((8, _)) => break,
                Some(_) => {}
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "the connection that says nothing is closed");
    assert!(quiet_since.elapsed() >= Duration::from_secs(2));
}

#[tokio::test]
async fn a_client_that_fell_behind_is_told_to_ask_again() {
    let r = start("resync", 30, 90).await;
    let mut ws = connect(&r).await;
    let (op, _) = frame(&mut ws).await.unwrap();
    assert_eq!(op, 1);
    // more events than the channel holds, sent while the relay has no turn to read them (the runtime has one thread and this does not wait)
    for n in 0..1000 {
        r.state
            .publish(serde_json::json!({ "type": "machine", "id": format!("m{n}") }));
    }
    let (op, text) = tokio::time::timeout(Duration::from_secs(5), frame(&mut ws))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(op, 1);
    let v: serde_json::Value = serde_json::from_slice(&text).unwrap();
    assert_eq!(v["type"], "resync");
    assert!(v["missed"].as_u64().unwrap() >= 700);
    // and the events that are still there follow
    let (_, text) = tokio::time::timeout(Duration::from_secs(5), frame(&mut ws))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&text).contains("\"machine\""));
}

#[test]
fn the_resync_message_says_how_many_events_were_missed() {
    let v: serde_json::Value = serde_json::from_str(&eh3d_server::api::resync_message(12)).unwrap();
    assert_eq!(
        (v["type"].as_str(), v["missed"].as_u64()),
        (Some("resync"), Some(12))
    );
}
