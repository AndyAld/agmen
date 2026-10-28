//! `agmend`: runs the per-user daemon until SIGINT or SIGTERM, prints help,
//! or manages its per-user OS service.

use std::ffi::OsString;
use std::process::ExitCode;

use agmend::{Agmend, Config};
use anyhow::{Context as _, Result};

mod service;

use service::ServiceAction;

const USAGE: &str = "usage: agmend [--help | -h]\n       agmend service <install | uninstall | status>\n\n\
                     Runs the agmen daemon in the foreground until Ctrl-C. \
                     Leave it running while you use agmen-desktop.\n\n\
                     service install [--program <absolute-path>] [--token-store-dir <absolute-path>]\n\
                     service status [--json]\n\
                     agmend store-schema --json";

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Foreground,
    Help,
    Service(ServiceAction),
    InstallProgram(std::path::PathBuf),
    InstallOptions {
        program: Option<std::path::PathBuf>,
        token: std::path::PathBuf,
    },
    StoreSchema,
}

fn parse(args: &[OsString]) -> Result<Command, String> {
    if args.len() >= 2
        && args[0] == "service"
        && args[1] == "install"
        && args.iter().any(|arg| arg == "--token-store-dir")
    {
        let mut program = None;
        let mut token = None;
        for pair in args[2..].chunks(2) {
            let [flag, value] = pair else {
                return Err("service install option requires a path".into());
            };
            if flag == "--program" && program.is_none() {
                program = Some(value.into());
            } else if flag == "--token-store-dir" && token.is_none() {
                token = Some(value.into());
            } else {
                return Err("unknown or repeated service install option".into());
            }
        }
        return Ok(Command::InstallOptions {
            program,
            token: token.ok_or("missing token store directory")?,
        });
    }
    match args {
        [command, flag] if command == "store-schema" && flag == "--json" => {
            Ok(Command::StoreSchema)
        }
        [] => Ok(Command::Foreground),
        [arg] if arg == "--help" || arg == "-h" => Ok(Command::Help),
        [command, action, flag]
            if command == "service" && action == "status" && flag == "--json" =>
        {
            Ok(Command::Service(ServiceAction::StatusJson))
        }
        [command, action, flag, program]
            if command == "service" && action == "install" && flag == "--program" =>
        {
            Ok(Command::InstallProgram(program.into()))
        }
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
    let args: Vec<_> = std::env::args_os().skip(1).take(7).collect();
    match parse(&args) {
        Ok(Command::Foreground) => {}
        Ok(Command::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Command::Service(action)) => {
            return service_exit(action, None);
        }
        Ok(Command::InstallProgram(program)) => {
            return service_exit(ServiceAction::Install, Some(program));
        }
        Ok(Command::InstallOptions { program, token }) => {
            return output_exit(service::execute_with_token(
                ServiceAction::Install,
                program,
                Some(token),
            ));
        }
        Ok(Command::StoreSchema) => {
            return output_exit((|| -> Result<String> {
                Ok(format!(
                    "{}\n",
                    agmend::store_snapshot(&agmen_client::state_dir()?)?
                ))
            })());
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

fn service_exit(action: ServiceAction, program: Option<std::path::PathBuf>) -> ExitCode {
    output_exit(service::execute(action, program))
}

fn output_exit(result: Result<String>) -> ExitCode {
    match result {
        Ok(message) => {
            print!("{message}");
            ExitCode::SUCCESS
        }
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
    // serve_until joins the collectors and sessions before it returns, so a
    // signal stops them within their 10 s bound instead of dropping them.
    let mut signal_result = Ok(());
    agmend
        .serve_until(async {
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    signal_result = result.context("listening for SIGINT");
                }
                _ = term.recv() => {}
            }
        })
        .await;
    tracing::info!("agmend stopped");
    signal_result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    #[test]
    fn token_store_option_parses_with_program() {
        let args: Vec<_> = [
            "service",
            "install",
            "--program",
            "/active/agmend",
            "--token-store-dir",
            "/private/tokens",
        ]
        .map(OsString::from)
        .into();
        assert!(parse(&args).is_ok());
        assert!(parse(&[OsString::from("store-schema"), OsString::from("--json")]).is_ok());
    }

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
    fn service_options_parse() {
        let args = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            parse(&args(&["service", "status", "--json"])),
            Ok(Command::Service(ServiceAction::StatusJson))
        );
        assert_eq!(
            parse(&args(&[
                "service",
                "install",
                "--program",
                "/active/agmend"
            ])),
            Ok(Command::InstallProgram("/active/agmend".into()))
        );
        for words in [
            vec!["service", "install", "--program"],
            vec!["service", "install", "--program", "/active/agmend", "extra"],
            vec!["service", "status", "--json", "extra"],
        ] {
            assert!(parse(&args(&words)).is_err());
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
