//! Entry point.

use std::net::SocketAddr;
use std::sync::Arc;

use logger_server::config::Config;
use logger_server::middleware::ratelimit;
use logger_server::{build_state, routes};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

// musl's allocator serialises badly across threads; mimalloc recovers the
// throughput a static build would otherwise lose. Not used on glibc, where the
// system allocator is already competitive and adds less RSS.
#[cfg(all(feature = "mimalloc", target_env = "musl"))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> std::process::ExitCode {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_env("LOGGER_LOG").unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer().compact())
        .init();

    let cfg = Config::from_env();

    // Built by hand rather than via #[tokio::main] so worker count and stack
    // size are capped: the defaults would size themselves to the host's core
    // count and reserve 2 MiB of stack per worker.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(cfg.workers)
        .max_blocking_threads(cfg.reader_conns + 4)
        .thread_stack_size(512 * 1024)
        .thread_name("logger")
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "cannot build runtime");
            return std::process::ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run(cfg)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "fatal");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(cfg: Config) -> Result<(), Box<dyn std::error::Error>> {
    let port = cfg.port;
    let workers = cfg.workers;
    let (state, writer, shutdown_tx, alert_rx) = build_state(cfg)?;

    if let Some(limiter) = state.limiter.clone() {
        ratelimit::spawn_janitor(limiter);
    }
    spawn_session_janitor(state.clone());
    // One task owns webhook delivery, so a slow endpoint cannot affect ingest.
    tokio::spawn(logger_server::alerts::delivery::run(
        state.clone(),
        alert_rx,
    ));

    if state.devices.is_empty() {
        tracing::warn!(
            "no devices registered yet -- writes will be rejected until one is \
             created at /devices or via POST /api/v1/devices"
        );
    }

    let app = routes::build(state.clone());
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!(%addr, workers, "logger_server listening");

    // ConnectInfo is required by the rate limiter to see the peer address.
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(shutdown_tx));

    server.await?;

    // Every request has finished and every SSE stream has been told to stop, so
    // nothing else holds a reference to the store by now.
    drop(state);

    // Flush whatever is still queued. Without this, the last batch is lost on
    // every redeploy, since the platform SIGTERMs the process.
    tracing::info!("flushing writer");
    writer.shutdown();
    tracing::info!("shutdown complete");

    Ok(())
}

/// Drops expired sessions so the table cannot grow without bound.
fn spawn_session_janitor(state: Arc<logger_server::state::AppState>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(600));
        loop {
            tick.tick().await;
            state.sessions.purge_expired();
        }
    });
}

/// Resolves on SIGTERM or SIGINT, first signalling in-flight SSE streams to end.
async fn shutdown_signal(tx: tokio::sync::watch::Sender<bool>) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    tracing::info!("shutdown signal received");
    // Ends open SSE streams; otherwise graceful shutdown would wait forever on
    // connections that are, by design, never going to close on their own.
    let _ = tx.send(true);
}
