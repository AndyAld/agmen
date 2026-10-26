//! The `agmen` command line: sign in, join a worktree to a project, post
//! and read messages, claim items and check lanes, all against an agmen hub
//! over HTTP and its broker through agmen-client.
//!
//! Exit codes: 0 ok, 1 error, 2 usage, 3 not signed in or not joined, 4
//! rejected, 5 queued but not recorded within `--wait` (see `output`).

mod claims;
mod client;
mod cmd;
mod git;
mod joined;
mod output;
mod paths;

use std::process::ExitCode;

use clap::Parser as _;

use crate::client::refusal;
use crate::joined::REVOKED_HINT;
use crate::output::{Code, Fail, clean, clean_lines};

fn main() -> ExitCode {
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
        Err(err) => report(&err),
    }
}

/// Prints `err` and picks the exit code: a [`Fail`]'s own, 3 when the hub
/// refused the token (401) or the identity it speaks for is revoked (403
/// `identity_revoked`), else 1. What it prints goes through the output
/// sanitizer, since hub refusals carry the hub's own text (ruling R63).
fn report(err: &anyhow::Error) -> ExitCode {
    if let Some(fail) = err.downcast_ref::<Fail>() {
        eprintln!("{}", clean_lines(&fail.message));
        return ExitCode::from(fail.code.exit_code());
    }
    eprintln!("{}", clean(&format!("agmen: {err:#}")));
    match refusal(err) {
        Some(r) if r.status == 401 => {
            eprintln!(
                "the hub does not take the stored token: run agmen login, or agmen join again"
            );
            ExitCode::from(Code::NotLoggedIn.exit_code())
        }
        Some(r) if r.status == 403 && r.code == "identity_revoked" => {
            eprintln!("this worktree's identity is revoked: {REVOKED_HINT}");
            ExitCode::from(Code::NotLoggedIn.exit_code())
        }
        _ => ExitCode::FAILURE,
    }
}
