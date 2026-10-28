//! The `agmen` command line and `agmen mcp`, its stdio MCP server: sign in,
//! join a worktree to a project, post and read messages, claim items and
//! check lanes, all against an agmen hub over HTTP and its broker through
//! agmen-client.
//!
//! The `agmen` binary calls [`run`]. Tests start the MCP server in process
//! with [`serve`] and an [`McpOptions`] that names the working directory,
//! the token store and the state folder, so they never read the user's own.
//!
//! Exit codes: 0 ok, 1 error, 2 usage, 3 not signed in or not joined, 4
//! rejected, 5 queued but not recorded within `--wait` (see `output`).

#![warn(missing_docs)]

mod claims;
mod client;
mod cmd;
mod git;
mod joined;
mod mcp;
mod output;
mod paths;
mod setup;

use std::process::ExitCode;

use clap::Parser as _;

pub use crate::mcp::{McpError, McpOptions, serve};

/// Runs the `agmen` command line: parses the arguments, runs the command,
/// prints what failed to stderr and returns the exit code.
#[must_use]
pub fn run() -> ExitCode {
    // clap prints help, version and usage errors itself, the last with
    // exit code 2.
    let cli = cmd::Cli::parse();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("AGMEN_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    // aws-lc-rs is the process-level rustls provider (ADR-0003). The workspace
    // builds rustls with both the `ring` and `aws-lc-rs` features, so rustls
    // cannot pick a default on its own and panics at the first TLS use.
    if rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .is_err()
    {
        eprintln!("agmen: a rustls crypto provider is already installed");
        return ExitCode::FAILURE;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("agmen: starting the async runtime failed: {err}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(cli.run()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let (text, code) = output::explain(&err);
            eprintln!("{text}");
            ExitCode::from(code)
        }
    }
}
