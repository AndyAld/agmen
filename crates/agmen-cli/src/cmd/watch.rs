//! `agmen watch`: follows the feed and this identity's inbox through a
//! persistent `<id>-watch` session until stopped. With `--json` it prints
//! one `ClientEvent` per line (ruling R31), the first being
//! `{"kind":"self","identity_id","handle","owner_id"}`, which the Claude
//! Code plugin reads.
//!
//! On connect it posts a HELLO, which joins the room: the hub then lists
//! the identity in the retained roster that handle lookups read (rulings
//! R51 and R60).

use agmen_client::{ClientEvent, ClientKind, Session, SessionConfig};
use agmen_proto::Payload;
use anyhow::{Context as _, Result};
use clap::Args;

use crate::joined::{agent, client_error, envelope};
use crate::output::{message_line, println_flush};

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
        format!(
            "watching as {} in project {}",
            me.handle, agent.identity.project
        )
    };
    if println_flush(&first).is_err() {
        return Ok(());
    }
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
