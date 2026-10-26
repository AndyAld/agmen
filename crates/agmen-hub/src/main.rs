//! The agmen hub: auth, identities, ingest, numbering, replay, and the ledger.
//!
//! `agmen-hub` (or `agmen-hub serve`) serves the HTTP API. Configuration
//! comes from `AGMEN_*` environment variables (see `agmen_hub::Config`);
//! `RUST_LOG` sets the log filter (default `info`). Logs go to stderr.
//!
//! `agmen-hub bootstrap --tenant <name> --project <name> --admin-github
//! <login>` makes a tenant, a project and its first admin, reusing what an
//! earlier run with the same names made, and prints their ids as JSON. It
//! needs only `AGMEN_DATABASE_URL`.

use std::ffi::OsString;

use anyhow::{Context as _, bail};
use tracing_subscriber::EnvFilter;

const USAGE: &str = "usage: agmen-hub [serve]
       agmen-hub bootstrap --tenant <name> --project <name> --admin-github <login>";

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Serve,
    Bootstrap {
        tenant: String,
        project: String,
        admin_github: String,
    },
    Help,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    match parse_args(std::env::args_os().skip(1))? {
        Command::Serve => serve().await,
        Command::Bootstrap {
            tenant,
            project,
            admin_github,
        } => bootstrap(tenant, project, &admin_github).await,
        Command::Help => {
            println!("{USAGE}");
            Ok(())
        }
    }
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> anyhow::Result<Command> {
    let mut args = args
        .into_iter()
        .map(|arg| {
            arg.into_string()
                .map_err(|arg| anyhow::anyhow!("argument {arg:?} is not UTF-8"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter();
    let command = match args.next().as_deref() {
        None | Some("serve") => Command::Serve,
        Some("-h" | "--help" | "help") => return Ok(Command::Help),
        Some("bootstrap") => {
            let (mut tenant, mut project, mut admin_github) = (None, None, None);
            while let Some(flag) = args.next() {
                let slot = match flag.as_str() {
                    "--tenant" => &mut tenant,
                    "--project" => &mut project,
                    "--admin-github" => &mut admin_github,
                    _ => bail!("unknown option {flag:?}\n{USAGE}"),
                };
                let value = args
                    .next()
                    .with_context(|| format!("{flag} needs a value\n{USAGE}"))?;
                if slot.replace(value).is_some() {
                    bail!("{flag} is given twice");
                }
            }
            let missing = |flag: &str| format!("bootstrap needs {flag}\n{USAGE}");
            return Ok(Command::Bootstrap {
                tenant: tenant.with_context(|| missing("--tenant"))?,
                project: project.with_context(|| missing("--project"))?,
                admin_github: admin_github.with_context(|| missing("--admin-github"))?,
            });
        }
        Some(other) => bail!("unknown command {other:?}\n{USAGE}"),
    };
    if let Some(extra) = args.next() {
        bail!("unexpected argument {extra:?}\n{USAGE}");
    }
    Ok(command)
}

async fn serve() -> anyhow::Result<()> {
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

async fn bootstrap(tenant: String, project: String, admin_github: &str) -> anyhow::Result<()> {
    let request = agmen_hub::Bootstrap::new(tenant, project, admin_github)?;
    let database_url = std::env::var("AGMEN_DATABASE_URL")
        .ok()
        .filter(|url| !url.is_empty())
        .context("AGMEN_DATABASE_URL is not set")?;
    let made = request
        .run(&database_url)
        .await
        .context("bootstrapping the project")?;
    tracing::info!(
        tenant = %made.tenant,
        project = %made.project,
        admin_github = %made.admin_github,
        "bootstrapped"
    );
    println!(
        "{}",
        serde_json::to_string(&made).context("encoding the ids")?
    );
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Command> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn commands() {
        assert_eq!(parse(&[]).unwrap(), Command::Serve);
        assert_eq!(parse(&["serve"]).unwrap(), Command::Serve);
        assert_eq!(parse(&["--help"]).unwrap(), Command::Help);
        assert_eq!(
            parse(&[
                "bootstrap",
                "--project",
                "agmen",
                "--admin-github",
                "Andy",
                "--tenant",
                "Acme Co",
            ])
            .unwrap(),
            Command::Bootstrap {
                tenant: "Acme Co".to_owned(),
                project: "agmen".to_owned(),
                admin_github: "Andy".to_owned(),
            }
        );
    }

    #[test]
    fn bad_command_lines() {
        for args in [
            &["serve", "now"][..],
            &["start"],
            &["bootstrap", "--tenant", "t", "--project", "p"],
            &[
                "bootstrap",
                "--tenant",
                "t",
                "--project",
                "p",
                "--admin-github",
            ],
            &[
                "bootstrap",
                "--tenant",
                "t",
                "--tenant",
                "u",
                "--project",
                "p",
                "--admin-github",
                "a",
            ],
            &[
                "bootstrap",
                "--tenant",
                "t",
                "--project",
                "p",
                "--admin-github",
                "a",
                "--x",
            ],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }
}
