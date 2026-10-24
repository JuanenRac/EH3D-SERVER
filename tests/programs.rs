//! The programs and the server together: the run of EH3D-DRILLPRO and EH3D-ROUTEPRO that sends its job to the server (`connection = server`)
//! against the real server on a real port, with the machine on the simulator.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eh3d_drillpro::protocol::connection::{ConnectionConfig, Kind};
use eh3d_drillpro::protocol::job::Pause;
use eh3d_drillpro::runner::{RealRun, RunState};
use eh3d_drillpro::server_link::Client;
use eh3d_server::api::router;
use eh3d_server::config::ServerConfig;
use eh3d_server::state::AppState;
use serde_json::{json, Value};

struct Running {
    state: Arc<AppState>,
    address: String,
    dir: std::path::PathBuf,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.state.stop_all();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn start_server(name: &str) -> Running {
    let dir = std::env::temp_dir().join(format!(
        "eh3d-server-programs-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let state = AppState::open(ServerConfig {
        data_dir: dir.clone(),
        ..ServerConfig::default()
    })
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let app = router(state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Running {
        state,
        address,
        dir,
    }
}

/// Sets the server up like a person would: the administrator, a demo machine and a token for the program. Returns the token.
async fn prepare(server: &Running) -> String {
    let address = server.address.clone();
    let code = server
        .state
        .auth
        .lock()
        .unwrap()
        .setup_code()
        .unwrap()
        .to_string();
    tokio::task::spawn_blocking(move || {
        let anonymous = Client {
            address: address.clone(),
            token: String::new(),
            machine: String::new(),
        };
        let (status, body) = anonymous
            .call(
                "POST",
                "/api/v1/setup",
                Some(&json!({"code": code, "name": "juan", "password": "una-clave-larga"})),
            )
            .unwrap();
        assert_eq!(status, 200, "{body}");
        let (_, login) = anonymous
            .call(
                "POST",
                "/api/v1/login",
                Some(&json!({"name": "juan", "password": "una-clave-larga"})),
            )
            .unwrap();
        let admin = Client {
            address: address.clone(),
            token: login["token"].as_str().unwrap().to_string(),
            machine: String::new(),
        };
        let (status, body) = admin
            .call(
                "POST",
                "/api/v1/machines",
                Some(&json!({"id": "bungard", "name": "Bungard", "connection": {"kind": "demo"}})),
            )
            .unwrap();
        assert_eq!(status, 200, "{body}");
        let (_, made) = admin
            .call(
                "POST",
                "/api/v1/tokens",
                Some(&json!({"name": "drillpro-pc", "role": "operator"})),
            )
            .unwrap();
        made["token"].as_str().unwrap().to_string()
    })
    .await
    .unwrap()
}

fn config(server: &Running, token: &str, machine: &str) -> ConnectionConfig {
    ConnectionConfig {
        kind: Kind::Server,
        server_address: server.address.clone(),
        server_token: token.to_string(),
        server_machine: machine.to_string(),
        ..ConnectionConfig::default()
    }
}

/// Follows a run the way the program's screen does: answers the dialogs, waits for the end.
async fn finish(run: &RealRun) -> (RunState, Vec<Pause>) {
    let limit = Instant::now() + Duration::from_secs(60);
    let mut dialogs = Vec::new();
    loop {
        match run.state() {
            RunState::WaitPause(p) => {
                if dialogs.last() != Some(&p) {
                    dialogs.push(p);
                }
                run.confirm();
            }
            s if s.finished() => return (s, dialogs),
            _ => {}
        }
        assert!(Instant::now() < limit, "timeout in {:?}", run.state());
        tokio::time::sleep(Duration::from_millis(60)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drilling_job_of_the_program_runs_on_the_server_to_the_end() {
    let server = start_server("drill").await;
    let token = prepare(&server).await;
    let request = json!({
        "kind": "drill", "name": "board.ncd",
        "tools": [{"number": 1, "feed_mm_min": 1000, "spindle_rpm": 24000}],
        "holes": [{"tool": 1, "x_mm": 10, "y_mm": 10}, {"tool": 1, "x_mm": 20, "y_mm": 10}, {"tool": 1, "x_mm": 30, "y_mm": 20}]
    });
    // the program's own plan of the job, as it makes it for its screen: three holes ending at these items of 20
    let run = RealRun::start_on_server(
        config(&server, &token, "bungard"),
        request,
        vec![5, 9, 13],
        20,
    );
    let (state, dialogs) = finish(&run).await;
    assert_eq!(state, RunState::Done, "{:?}", run.state());
    assert_eq!(dialogs.first(), Some(&Pause::ToolChange { tool: 1 }));
    assert_eq!(run.progress(), (20, 20));
    // the server kept the job as done, started by the program's token
    let jobs = server.state.jobs.lock().unwrap();
    let job = &jobs.list()[0];
    assert_eq!(
        (job.state, job.by.as_str(), job.name.as_str()),
        (
            eh3d_server::jobs::JobState::Done,
            "drillpro-pc",
            "board.ncd"
        )
    );
    assert_eq!(job.holes_done, 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_routing_job_of_the_program_runs_on_the_server_too() {
    let server = start_server("route").await;
    let token = prepare(&server).await;
    let request = json!({
        "kind": "route", "name": "outline.plt",
        "tools": [{"number": 1, "depth_mm": 1.7, "iterations": 2, "feed_xy_mm_min": 800, "feed_z_mm_min": 300, "spindle_rpm": 24000}],
        "paths": [{"pen": 1, "points_mm": [[10, 10], [40, 10], [40, 30], [10, 10]]}]
    });
    let run = RealRun::start_on_server(config(&server, &token, "bungard"), request, Vec::new(), 40);
    let (state, _) = finish(&run).await;
    assert_eq!(state, RunState::Done, "{:?}", run.state());
    assert_eq!(run.progress(), (40, 40));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_the_server_refuses_comes_back_to_the_program_as_a_failure_with_its_reason() {
    let server = start_server("refused").await;
    let token = prepare(&server).await;
    // a hole of a tool that is not in the table
    let request = json!({"kind": "drill", "holes": [{"tool": 7, "x_mm": 1, "y_mm": 1}]});
    let run = RealRun::start_on_server(
        config(&server, &token, "bungard"),
        request.clone(),
        vec![3],
        10,
    );
    let (state, _) = finish(&run).await;
    assert!(
        matches!(&state, RunState::Failed(why) if why.starts_with("unknown_tool")),
        "{state:?}"
    );
    // a machine the server does not know
    let run = RealRun::start_on_server(config(&server, &token, "nowhere"), request, vec![3], 10);
    let (state, _) = finish(&run).await;
    assert!(
        matches!(&state, RunState::Failed(why) if why.starts_with("unknown_machine")),
        "{state:?}"
    );
    // a token that is not valid
    let run = RealRun::start_on_server(
        config(&server, "eh3d_nonsense", "bungard"),
        json!({"kind": "drill"}),
        Vec::new(),
        1,
    );
    let (state, _) = finish(&run).await;
    assert!(
        matches!(&state, RunState::Failed(why) if why.starts_with("unauthorized")),
        "{state:?}"
    );
    // a connection file with no server address says so
    let mut c = config(&server, &token, "bungard");
    c.server_address.clear();
    let run = RealRun::start_on_server(c, json!({"kind": "drill"}), Vec::new(), 1);
    let (state, _) = finish(&run).await;
    assert!(
        matches!(&state, RunState::Failed(why) if why.contains("server_address")),
        "{state:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_program_can_stop_a_job_on_the_server_and_closing_the_program_does_not() {
    let server = start_server("stop").await;
    let token = prepare(&server).await;
    let request: Value = json!({
        "kind": "drill", "tools": [{"number": 1, "feed_mm_min": 1000, "spindle_rpm": 24000}],
        "holes": [{"tool": 1, "x_mm": 10, "y_mm": 10}, {"tool": 1, "x_mm": 20, "y_mm": 10}]
    });
    // stopped from the program: it waits at the first dialog and Escape ends it on the server
    let run = RealRun::start_on_server(
        config(&server, &token, "bungard"),
        request.clone(),
        vec![5, 9],
        12,
    );
    let limit = Instant::now() + Duration::from_secs(60);
    while !matches!(run.state(), RunState::WaitPause(_)) {
        assert!(Instant::now() < limit, "{:?}", run.state());
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    run.cancel();
    let (state, _) = finish(&run).await;
    assert_eq!(state, RunState::Cancelled);
    // closed instead of stopped: the job goes on without the program
    let run = RealRun::start_on_server(config(&server, &token, "bungard"), request, vec![5, 9], 12);
    let limit = Instant::now() + Duration::from_secs(60);
    while !matches!(run.state(), RunState::WaitPause(_)) {
        assert!(Instant::now() < limit, "{:?}", run.state());
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    tokio::task::spawn_blocking(move || drop(run))
        .await
        .unwrap();
    let jobs = server.state.jobs.lock().unwrap();
    let last = jobs.list().last().unwrap();
    assert!(
        last.state.active(),
        "the job stopped with the program: {:?}",
        last.state
    );
}
