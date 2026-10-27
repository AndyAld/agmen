//! `agmend`: runs the per-user daemon until SIGINT or SIGTERM, prints help,
//! or parses a service command whose execution waits for A T3.

use std::ffi::OsString;
use std::process::ExitCode;

use agmend::{Agmend, Config};
use anyhow::{Context as _, Result};

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "A T2 renderers are consumed by service execution in A T3"
    )
)]
mod service;

use service::ServiceAction;

const USAGE: &str = "usage: agmend [--help | -h]\n       agmend service <install | uninstall | status>\n\n\
                     Runs the agmen daemon in the foreground until Ctrl-C. \
                     Leave it running while you use agmen-desktop.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Foreground,
    Help,
    Service(ServiceAction),
}

fn parse(args: &[OsString]) -> Result<Command, String> {
    match args {
        [] => Ok(Command::Foreground),
        [arg] if arg == "--help" || arg == "-h" => Ok(Command::Help),
        [command, action] if command == "service" => match action.to_str() {
            Some("install") => Ok(Command::Service(ServiceAction::Install)),
            Some("uninstall") => Ok(Command::Service(ServiceAction::Uninstall)),
            Some("status") => Ok(Command::Service(ServiceAction::Status)),
            _ => Err("unknown service action".to_owned()),
        },
        [arg, ..] => Err(format!("unknown argument: {}", arg.to_string_lossy())),
    }
}

fn main() -> ExitCode {
    // One argument beyond a valid form is sufficient to reject excess input.
    let args: Vec<_> = std::env::args_os().skip(1).take(4).collect();
    match parse(&args) {
        Ok(Command::Foreground) => {}
        Ok(Command::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Command::Service(_)) => {
            // shortcut: service execution waits for A T3 in this lane; remove before handoff.
            eprintln!("agmend: service execution is unavailable until A T3");
            return ExitCode::FAILURE;
        }
        Err(err) => {
            eprintln!("agmend: {err}\n{USAGE}");
            return ExitCode::from(2);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    #[test]
    fn service_commands_parse() {
        for (action, expected) in [
            ("install", ServiceAction::Install),
            ("uninstall", ServiceAction::Uninstall),
            ("status", ServiceAction::Status),
        ] {
            assert_eq!(
                parse(&[OsString::from("service"), OsString::from(action)]),
                Ok(Command::Service(expected))
            );
        }
    }

    #[test]
    fn foreground_and_help_parse() {
        assert_eq!(parse(&[]), Ok(Command::Foreground));
        for flag in ["--help", "-h"] {
            assert_eq!(parse(&[OsString::from(flag)]), Ok(Command::Help));
        }
    }

    #[test]
    fn extra_and_non_utf8_arguments_are_refused() {
        for args in [
            vec!["--help", "extra"],
            vec!["-h", "extra"],
            vec!["service"],
            vec!["service", "restart"],
            vec!["service", "install", "extra"],
            vec!["unknown"],
            vec!["service", "status", "extra", "extra"],
        ] {
            let args: Vec<_> = args.into_iter().map(OsString::from).collect();
            assert!(parse(&args).is_err(), "{args:?}");
        }
        let invalid = OsString::from_vec(vec![0xff]);
        assert!(parse(std::slice::from_ref(&invalid)).is_err());
        assert!(parse(&[OsString::from("service"), invalid.clone()]).is_err());
        assert!(parse(&[OsString::from("--help"), invalid]).is_err());
    }
}
