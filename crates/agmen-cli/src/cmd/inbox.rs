//! `agmen inbox` (ruling R45): the messages addressed to this identity
//! (the ruling R31 rule) after its read cursors, oldest first, read through
//! the hub's replay API from the feed and its inbox. It moves each read
//! cursor past what it scanned; `--after` starts from that seq instead.
//! The delivery cursors belong to `agmen watch` and are not touched.

use std::collections::HashSet;

use agmen_client::{ClientEvent, Cursors, Outbox, STATE_DB};
use agmen_proto::{Payload, StampedEnvelope, Stream};
use anyhow::{Context as _, Result};
use clap::Args;

use crate::client::blocking;
use crate::joined::{Agent, agent, client_error};
use crate::output::{message_line, println_flush};

/// The most replay pages one stream is read for in one call (500
/// messages each).
///
/// shortcut: a reader more than 10,000 messages behind sees the rest on
/// the next call; page on if that ever bites.
const MAX_PAGES: usize = 20;

#[derive(Debug, Args)]
pub(crate) struct InboxArgs {
    /// List from after this seq in each stream instead of after the read
    /// cursors.
    #[arg(long, value_name = "SEQ")]
    after: Option<u64>,
    /// Print each message as one JSON line of kind message, as agmen watch
    /// --json does.
    #[arg(long)]
    json: bool,
}

/// What one stream gave.
struct Scanned {
    stream: Stream,
    /// The last seq read; the read cursor moves here.
    last: u64,
    messages: Vec<StampedEnvelope>,
    /// More pages were left unread.
    more: bool,
}

/// `agmen inbox`.
pub(crate) async fn inbox(args: InboxArgs) -> Result<()> {
    let agent = agent().await?;
    let db = agent.state_dir.join(STATE_DB);
    let dir = agent.state_dir.clone();
    let (cursors, outbox) = blocking(move || {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok((Cursors::open(&db)?, Outbox::open(&db)?))
    })
    .await?;
    let streams = [
        Stream::Feed,
        Stream::Inbox(agent.identity.identity_id.clone()),
    ];
    let mut scans = Vec::with_capacity(streams.len());
    for stream in streams {
        let start = match args.after {
            Some(after) => after,
            None => {
                let (cursors, stream) = (cursors.clone(), stream.clone());
                blocking(move || Ok(cursors.read_cursor(&stream)?)).await?
            }
        };
        scans.push(scan(&agent, stream, start).await?);
    }

    // The read cursors move past everything scanned, shown or not.
    let marks: Vec<(Stream, u64)> = scans
        .iter()
        .map(|scan| (scan.stream.clone(), scan.last))
        .collect();
    let more = scans.iter().any(|scan| scan.more);
    let mut seen = HashSet::new();
    let messages: Vec<StampedEnvelope> = scans
        .into_iter()
        .flat_map(|scan| scan.messages)
        // The feed and the inbox can both carry one message (a GRANT).
        .filter(|message| seen.insert(message.client.id.clone()))
        .collect();
    let me = agent.identity.me();
    let mut found = blocking(move || {
        // An ANSWER is for this identity when it answers one of its own
        // messages, which its outbox holds.
        let mut own = HashSet::new();
        for message in &messages {
            if let (Payload::Answer {}, Some(id)) =
                (&message.client.payload, &message.client.reply_to)
                && outbox.state(id)?.is_some()
            {
                own.insert(id.clone());
            }
        }
        Ok(messages
            .into_iter()
            .filter(|message| me.is_for_me(message, |id| own.contains(id)))
            .collect::<Vec<_>>())
    })
    .await?;
    found.sort_by_key(|message| message.hub_at);

    for message in &found {
        let line = if args.json {
            serde_json::to_string(&ClientEvent::Message {
                message: Box::new(message.clone()),
            })
            .context("encoding a message")?
        } else {
            message_line(message)
        };
        println_flush(&line).context("writing to stdout")?;
    }
    if found.is_empty() && !args.json {
        println_flush("no new messages").context("writing to stdout")?;
    }
    for (stream, last) in marks {
        let cursors = cursors.clone();
        blocking(move || Ok(cursors.mark_read(&stream, last)?)).await?;
    }
    if more {
        eprintln!("more messages are waiting: run agmen inbox again");
    }
    Ok(())
}

/// Reads `stream` after seq `after`, up to [`MAX_PAGES`] pages.
async fn scan(agent: &Agent, stream: Stream, after: u64) -> Result<Scanned> {
    let mut last = after;
    let mut messages = Vec::new();
    for _ in 0..MAX_PAGES {
        let page =
            agent.hub.page(&stream, last).await.map_err(|err| {
                client_error(err, &format!("reading {stream} from the hub failed"))
            })?;
        if let Some(message) = page.messages.last() {
            last = last.max(message.seq.0);
        }
        messages.extend(page.messages);
        match page.next_after {
            Some(next) => last = last.max(next),
            None => {
                return Ok(Scanned {
                    stream,
                    last,
                    messages,
                    more: false,
                });
            }
        }
    }
    Ok(Scanned {
        stream,
        last,
        messages,
        more: true,
    })
}
