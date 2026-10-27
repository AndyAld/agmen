//! `agmend`: runs the per-user daemon until SIGINT or SIGTERM.

use std::process::ExitCode;

use agmend::{Agmend, Config};
use anyhow::{Context as _, Result};

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("AGMEN_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    // aws-lc-rs is the process-level rustls provider (ADR-0003); the
    // workspace builds rustls with two providers, so it cannot pick one.
    if rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .is_err()
    {
        eprintln!("agmend: a rustls crypto provider is already installed");
        return ExitCode::FAILURE;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("agmend: starting the async runtime failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("agmend: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let agmend = Agmend::bind(Config::from_env()?)?;
    tracing::info!(socket = %agmend.socket().display(), "agmend is listening");
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("listening for SIGTERM")?;
    tokio::select! {
        () = agmend.serve() => {}
        result = tokio::signal::ctrl_c() => result.context("listening for SIGINT")?,
        _ = term.recv() => {}
    }
    tracing::info!("agmend stopped");
    Ok(())
}
