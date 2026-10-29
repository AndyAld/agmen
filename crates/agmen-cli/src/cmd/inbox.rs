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
    let unread = unread(&agent, args.after).await?;
    for message in &unread.messages {
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
    if unread.messages.is_empty() && !args.json {
        println_flush("no new messages").context("writing to stdout")?;
    }
    if unread.mark_read().await? {
        eprintln!("more messages are waiting: run agmen inbox again");
    }
    Ok(())
}

/// The messages for this identity after its read cursors (or after
/// `after` in each stream), oldest first. The read cursors move only on
/// [`Unread::mark_read`], once they have been shown.
#[derive(Debug)]
pub(crate) struct Unread {
    pub(crate) messages: Vec<StampedEnvelope>,
    cursors: Cursors,
    /// Where each read cursor moves: past what was scanned unless capped
    /// by [`Unread::keep_first`].
    marks: Vec<(Stream, u64)>,
    /// More pages or capped messages were left unread.
    more: bool,
}

impl Unread {
    /// Keeps the oldest `max` messages in their existing `hub_at` order.
    /// Cursors stop before each stream's first dropped message so the next
    /// read still returns it. Returns how many messages were dropped.
    pub(crate) fn keep_first(mut self, max: usize) -> (Self, usize) {
        let dropped = self.messages.len().saturating_sub(max);
        self.marks = capped_marks(&self.messages, max, &self.marks);
        self.messages.truncate(max);
        self.more |= dropped > 0;
        (self, dropped)
    }

    /// Moves the read cursors to the scan marks, capped by [`Self::keep_first`]
    /// when called. Returns whether more messages are waiting.
    pub(crate) async fn mark_read(self) -> Result<bool> {
        for (stream, last) in self.marks {
            let cursors = self.cursors.clone();
            blocking(move || Ok(cursors.mark_read(&stream, last)?)).await?;
        }
        Ok(self.more)
    }
}

/// Caps each read mark before that stream's first message beyond `max`.
/// Streams with no dropped message keep their original marks, including
/// messages scanned but not addressed to this identity.
pub(crate) fn capped_marks(
    messages: &[StampedEnvelope],
    max: usize,
    marks: &[(Stream, u64)],
) -> Vec<(Stream, u64)> {
    marks
        .iter()
        .map(|(stream, mark)| {
            let capped = messages
                .iter()
                .skip(max)
                .find(|message| &message.stream == stream)
                .map_or(*mark, |message| {
                    (*mark).min(message.seq.0.saturating_sub(1))
                });
            (stream.clone(), capped)
        })
        .collect()
}

/// Reads the feed and this identity's inbox from their read cursors, or
/// from `after`, and keeps the messages for this identity (ruling R31).
pub(crate) async fn unread(agent: &Agent, after: Option<u64>) -> Result<Unread> {
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
        let start = match after {
            Some(after) => after,
            None => {
                let (cursors, stream) = (cursors.clone(), stream.clone());
                blocking(move || Ok(cursors.read_cursor(&stream)?)).await?
            }
        };
        scans.push(scan(agent, stream, start).await?);
    }

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
    Ok(Unread {
        messages: found,
        cursors,
        marks,
        more,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn messages() -> Vec<StampedEnvelope> {
        (1..=25)
            .map(|index| {
                let (stream, seq) = if index <= 15 {
                    (Stream::Feed, index)
                } else {
                    (Stream::Inbox("idn7k2q9".parse().unwrap()), index - 15)
                };
                serde_json::from_value(serde_json::json!({
                    "v": 1, "id": "01JBX6Q4T7W3C2M8Y9K5N0P1RS", "verb": "DM",
                    "data": {}, "body": format!("message {index}"),
                    "sent_at": "2026-10-25T16:04:00Z", "stream": stream, "seq": seq,
                    "from": "adamcoord", "from_handle": "adam/coord", "from_kind": "agent",
                    "hub_at": format!("2026-10-25T16:04:{index:02}Z"),
                }))
                .unwrap()
            })
            .collect()
    }

    #[test]
    fn keep_first_keeps_the_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let messages = messages();
        let marks = vec![(Stream::Feed, 15), (messages[15].stream.clone(), 10)];
        for (max, count, dropped) in [(20, 20, 5), (30, 25, 0), (0, 0, 25)] {
            let unread = Unread {
                messages: messages.clone(),
                cursors: Cursors::open(dir.path().join(STATE_DB)).unwrap(),
                marks: marks.clone(),
                more: false,
            };
            let (kept, actual_dropped) = unread.keep_first(max);
            assert_eq!(kept.messages, messages[..count]);
            assert_eq!(actual_dropped, dropped);
            assert_eq!(kept.more, dropped > 0);
            assert_eq!(kept.marks, capped_marks(&messages, max, &marks));
        }
    }

    #[test]
    fn capped_marks_stop_before_the_first_dropped() {
        let messages = messages();
        let inbox = messages[15].stream.clone();
        let marks = vec![(Stream::Feed, 15), (inbox.clone(), 10)];
        assert_eq!(
            capped_marks(&messages, 20, &marks),
            [(Stream::Feed, 15), (inbox.clone(), 5)]
        );
        assert_eq!(capped_marks(&messages, 30, &marks), marks);
        assert_eq!(
            capped_marks(&messages, 0, &marks),
            [(Stream::Feed, 0), (inbox.clone(), 0)]
        );
        let earlier_marks = vec![(Stream::Feed, 12), (inbox.clone(), 3)];
        assert_eq!(capped_marks(&messages, 20, &earlier_marks), earlier_marks);
        assert_eq!(capped_marks(&[], 0, &marks), marks);
        // A stream's first dropped message need not have seq 1.
        assert_eq!(
            capped_marks(&messages[18..], 0, &marks),
            [(Stream::Feed, 15), (inbox, 3)]
        );
    }
}
