//! `eh3d-server [--config FILE]`: starts the EH3D server. It listens where `server.cfg` says (port 18200 by default), keeps its data in the
//! data folder and, while there is no user, shows a set-up code on this console to make the first administrator.

use std::path::PathBuf;

use eh3d_server::sync::Locked;
use eh3d_server::{api, config, state::AppState};

#[tokio::main]
async fn main() {
    let mut path: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => path = args.next().map(PathBuf::from),
            "--version" | "-V" => {
                println!("eh3d-server {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--help" | "-h" => {
                println!("eh3d-server [--config FILE] [--version]\n\nReads server.cfg (or FILE): listen, data_dir, session_hours, max_body_mb, keep_jobs, studio_dir.\nStops the machines that are running and exits on Ctrl-C or SIGTERM.");
                return;
            }
            other => {
                eprintln!("eh3d-server: unknown argument '{other}' (try --help)");
                std::process::exit(2);
            }
        }
    }
    let cfg = match config::load(path.as_deref()) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("eh3d-server: {e}");
            std::process::exit(2);
        }
    };
    let state = match AppState::open(cfg.clone()) {
        Ok(state) => state,
        Err(e) => {
            eprintln!(
                "eh3d-server: cannot open the data in {}: {e}",
                cfg.data_dir.display()
            );
            std::process::exit(1);
        }
    };
    if let Some(code) = state.auth.locked().setup_code() {
        println!("SETUP: this server has no user yet. Make the administrator with POST /api/v1/setup and the set-up code {code}");
    }
    let listener = match tokio::net::TcpListener::bind(cfg.listen).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("eh3d-server: cannot listen on {}: {e}", cfg.listen);
            std::process::exit(1);
        }
    };
    // the signals are taken before the server says it listens: a SIGTERM that comes right after must not find the default action
    let stop = shutdown_signal();
    println!(
        "eh3d-server {} listening on {}",
        env!("CARGO_PKG_VERSION"),
        cfg.listen
    );
    let app = api::router(state.clone());
    let stopper = state.clone();
    let served = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        stop.await;
        println!("eh3d-server: stopping; the machines that are running are stopped first");
        let stopper = stopper.clone();
        let _ = tokio::task::spawn_blocking(move || stopper.stop_all()).await;
    });
    if let Err(e) = served.await {
        eprintln!("eh3d-server: {e}");
        std::process::exit(1);
    }
}

/// Waits for the request to stop: Ctrl-C, or SIGTERM, which is what `systemctl stop` and a container send. Either way the machines that are running are stopped first.
/// The handlers are installed when this is called, not when the result is first polled.
#[allow(clippy::manual_async_fn)] // not an `async fn` on purpose: the handlers must be installed when it is called
fn shutdown_signal() -> impl std::future::Future<Output = ()> {
    #[cfg(unix)]
    let term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    async move {
        #[cfg(unix)]
        if let Some(mut term) = term {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
        let _ = tokio::signal::ctrl_c().await;
    }
}
