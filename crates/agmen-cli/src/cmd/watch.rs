//! `agmen watch`: follows the feed and this identity's inbox through a
//! persistent `<id>-watch` session until stopped. With `--json` it prints
//! one `ClientEvent` per line (ruling R31), the first being
//! `{"kind":"self","identity_id","handle","owner_id"}`, which the Claude
//! Code plugin reads.
//!
//! The session publishes the identity's retained presence on every
//! connect, with a last will that sets it offline (agmen-client), so the
//! retained roster shows the identity online while watch runs. The hub
//! lists the identity in that roster from the moment it is minted, so
//! handle lookups (rulings R51 and R60) never wait for a HELLO. Watch still
//! posts one HELLO per process, after the first connect, to name its client
//! to the room; the session reconnects on its own and posts none then.

use agmen_client::{ClientEvent, ClientKind, Session, SessionConfig};
use agmen_proto::Payload;
use anyhow::{Context as _, Result};
use clap::Args;

use crate::joined::{agent, client_error, envelope};
use crate::output::{clean, message_line, println_flush};
use crate::watch_lock::WatchLock;

#[derive(Debug, Args)]
pub(crate) struct WatchArgs {
    /// Print one JSON object per line instead of text.
    #[arg(long)]
    json: bool,
}

/// `agmen watch`.
pub(crate) async fn watch(args: WatchArgs) -> Result<()> {
    let agent = agent().await?;
    let me = agent.identity.me();
    let first = if args.json {
        serde_json::to_string(&ClientEvent::Me(me)).context("encoding the self line")?
    } else {
        banner(&me.handle, agent.identity.project.as_str())
    };
    if println_flush(&first).is_err() {
        return Ok(());
    }
    let _watch = WatchLock::try_hold(&agent.state_dir).await;
    let config = SessionConfig::new(
        ClientKind::Watch,
        agent.topics(),
        agent.hub.clone(),
        agent.identity.broker_url.clone(),
        agent.state_dir.clone(),
    );
    let (session, mut events) = Session::connect(config)
        .await
        .map_err(|err| client_error(err, "connecting to the broker failed"))?;
    let hello = Payload::Hello {
        client: concat!("agmen-cli/", env!("CARGO_PKG_VERSION")).to_owned(),
        agent: None,
        model: None,
        looking_at: None,
    };
    session
        .publish(envelope(hello, String::new())?)
        .await
        .context("queueing the HELLO failed")?;
    let stop = stop_signal();
    tokio::pin!(stop);
    loop {
        let event = tokio::select! {
            event = events.recv() => event,
            () = &mut stop => break,
        };
        let Some(event) = event else {
            // The session stopped; close says why.
            break;
        };
        let line = if args.json {
            Some(serde_json::to_string(&event).context("encoding an event")?)
        } else {
            text_line(&event)
        };
        // A closed stdout means the reader is gone: stop quietly.
        if let Some(line) = line
            && println_flush(&line).is_err()
        {
            break;
        }
    }
    session.close().await.context("the watch session failed")
}

/// Text mode's first line. The handle comes from the identity file, and
/// R63 cleans every printed handle wherever it was read.
fn banner(handle: &str, project: &str) -> String {
    format!(
        "watching as {} in project {}",
        clean(handle),
        clean(project)
    )
}

/// What text mode prints for `event`, if anything.
fn text_line(event: &ClientEvent) -> Option<String> {
    match event {
        ClientEvent::Message { message } => Some(message_line(message)),
        ClientEvent::Connection { connected: true } => Some("connected".to_owned()),
        ClientEvent::Connection { connected: false } => Some("disconnected".to_owned()),
        _ => None,
    }
}

/// Resolves on Ctrl-C, or on SIGTERM where there is one.
async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "SIGTERM cannot be caught; Ctrl-C still stops");
                if tokio::signal::ctrl_c().await.is_err() {
                    std::future::pending::<()>().await;
                }
            }
        }
    }
    #[cfg(not(unix))]
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_drops_escape_sequences() {
        let line = banner("andy/\u{1b}[2Jsol\r\n#9 adam/coord: fake", "project1");
        assert_eq!(
            line,
            "watching as andy/[2Jsol#9 adam/coord: fake in project project1"
        );
    }
}
