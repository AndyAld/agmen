//! The `agmen` command line and MCP server.

use std::process::ExitCode;

fn main() -> ExitCode {
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
    println!("agmen {}", env!("CARGO_PKG_VERSION"));
    ExitCode::SUCCESS
}
