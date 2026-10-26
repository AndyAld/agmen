//! The agmen hub: auth, identities, ingest, numbering, replay, and the ledger.
//!
//! Configuration comes from `AGMEN_*` environment variables (see
//! `agmen_hub::Config`); `RUST_LOG` sets the log filter (default `info`).

use anyhow::Context as _;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let config = agmen_hub::Config::from_env().context("reading the configuration")?;
    let hub = agmen_hub::Hub::connect(&config)
        .await
        .context("starting the hub")?;
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("listening on {}", config.listen))?;
    tracing::info!(
        listen = %config.listen,
        version = env!("CARGO_PKG_VERSION"),
        "agmen-hub serving"
    );
    axum::serve(listener, hub.router())
        .with_graceful_shutdown(shutdown())
        .await
        .context("serving HTTP")
}

/// Resolves on Ctrl-C or SIGTERM, so in-flight requests finish first.
async fn shutdown() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %err, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                tracing::error!(error = %err, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutting down");
}
