//! `polyvec-server` binary: binds, serves, and shuts down gracefully.
//!
//! Startup: parse env config → refuse unsafe binds → create the data dir →
//! serve. Shutdown: on SIGTERM/SIGINT, stop accepting new connections,
//! finish in-flight requests (`HttpServer::stop(true)`), then flush every
//! open collection so the ANN index is checkpointed. Vector data itself is
//! durable at redb-commit time regardless (see ADR-004).

use actix_web::{App, HttpServer, middleware::Logger, web};
use polyvec_server::{AppState, ServerConfig, configure_routes};

/// Waits for SIGTERM or Ctrl-C (SIGINT), whichever comes first.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for ctrl-c");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to listen for SIGTERM")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => log::info!("received SIGINT, shutting down"),
        () = terminate => log::info!("received SIGTERM, shutting down"),
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();

    let config = ServerConfig::from_env().unwrap_or_else(|err| {
        eprintln!("polyvec-server: configuration error: {err}");
        std::process::exit(2);
    });

    std::fs::create_dir_all(&config.data_dir).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("cannot create data dir {}: {e}", config.data_dir.display()),
        )
    })?;

    let state = AppState::new(&config).unwrap_or_else(|err| {
        eprintln!("polyvec-server: cannot build state: {err}");
        std::process::exit(2);
    });
    let shutdown_state = state.clone();
    let serve_state = state.clone();
    let bind = config.bind;

    if config.api_key.is_some() {
        log::info!("API authentication enabled (bearer token on /v1/*)");
    } else if config.allow_nonlocal_bind {
        // The bind was explicitly opened beyond localhost with no
        // credential: anyone on the network has full read/write/delete.
        log::warn!(
            "binding {bind} with POLYVEC_ALLOW_NONLOCAL_BIND=1 but no POLYVEC_API_KEY: \
             the API is exposed to the network WITHOUT authentication"
        );
    }
    if config.rate_limit_rps == 0 {
        log::info!("rate limiting disabled (POLYVEC_RATE_LIMIT_RPS=0)");
    }

    let route_config = config.clone();
    let mut server = HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(serve_state.clone()))
            .wrap(Logger::default())
            .configure(|cfg| configure_routes(cfg, &route_config))
    });
    if let Some(workers) = config.workers {
        server = server.workers(workers);
    }
    let running = server.bind(bind)?.run();
    let handle = running.handle();
    log::info!("polyvec-server listening on {bind}");

    tokio::spawn(async move {
        shutdown_signal().await;
        // `true` = graceful: stop accepting, finish in-flight requests.
        handle.stop(true).await;
    });

    running.await?;

    // Graceful shutdown: drain ingest jobs first (in-flight jobs finish;
    // no new submissions), then checkpoint every open collection. Writes
    // are already durable via redb; this persists the ANN index dumps.
    log::info!("draining ingest jobs...");
    state.runner.shutdown().await;
    let mut failed = 0;
    for (id, result) in shutdown_state.registry.flush_all() {
        match result {
            Ok(()) => log::info!("flushed collection '{id}' on shutdown"),
            Err(e) => {
                failed += 1;
                log::error!("shutdown flush failed for '{id}': {e}");
            }
        }
    }
    if failed > 0 {
        log::error!("{failed} collection(s) failed to flush on shutdown");
    }
    log::info!("polyvec-server stopped");
    Ok(())
}
