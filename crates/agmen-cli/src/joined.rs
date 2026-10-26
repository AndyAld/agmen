//! The identity this worktree acts as: its trusted identity file (rulings
//! R42 to R42b, through agmen-client's `find_identity`), its token, and
//! what every command that speaks for it shares: handle lookup (ruling
//! R60), envelopes, and publishing through a one-shot `<id>-cli` session
//! that waits for the hub's answer.

use std::path::PathBuf;
use std::time::Duration;

use agmen_client::{
    ClientEvent, ClientKind, HubClient, IdentityError, IdentityFile, Joined, OutboxState, Session,
    SessionConfig, TokenKey, find_identity, read_state, resolve_handle,
};
use agmen_proto::{
    Body, ClientEnvelope, IdentityId, Mention, MessageId, Payload, StampedEnvelope, Stream,
    Timestamp, Topics,
};
use anyhow::{Context as _, Result};
use tokio::time::Instant;

use crate::client::{Api, blocking, get_token};
use crate::output::{Code, Fail, clean, println_flush};
use crate::paths;

/// How often a waiting publish also looks at the outbox, in case another
/// process of this identity got the hub's answer.
const OUTBOX_POLL: Duration = Duration::from_millis(250);

/// A joined worktree's identity, ready to act.
#[derive(Debug)]
pub(crate) struct Agent {
    /// The worktree root.
    pub(crate) root: PathBuf,
    pub(crate) identity: IdentityFile,
    /// The identity's hub client (replay pages and broker tokens).
    pub(crate) hub: HubClient,
    /// The identity's own hub token.
    token: String,
    /// Its state folder.
    pub(crate) state_dir: PathBuf,
}

/// The trusted identity file of the worktree the process runs in.
///
/// # Errors
///
/// [`Fail::not_joined`] outside a joined worktree, and the trust test's
/// refusal for an identity file agmen may not use.
pub(crate) async fn joined() -> Result<Joined> {
    let cwd = std::env::current_dir().context("reading the working directory")?;
    blocking(move || match find_identity(&cwd) {
        Ok(joined) => Ok(joined),
        Err(err @ IdentityError::Untrusted { .. }) => Err(anyhow::Error::new(err)),
        Err(err) => Err(anyhow::Error::new(err).context("loading .agmen/identity.json failed")),
    })
    .await?
    .ok_or_else(|| Fail::not_joined().into())
}

/// Loads the identity of the worktree the process runs in, with its token.
///
/// # Errors
///
/// [`Fail::not_joined`] outside a joined worktree, a [`Code::NotLoggedIn`]
/// failure when this machine holds no token for it, and the trust test's
/// refusal for an identity file agmen may not use.
pub(crate) async fn agent() -> Result<Agent> {
    let joined = joined().await?;
    let identity = joined.identity;
    let key = TokenKey::new(&identity.hub_url, &identity.identity_id)
        .context("the identity file's hub URL")?;
    let token = get_token(key).await?.ok_or_else(|| {
        Fail::new(
            Code::NotLoggedIn,
            format!(
                "this machine holds no agmen token for {} at {}: run agmen leave, then agmen join \
                 again in this worktree",
                identity.handle,
                identity.hub_url.origin().ascii_serialization()
            ),
        )
    })?;
    let hub = HubClient::new(
        identity.hub_url.clone(),
        identity.project.clone(),
        identity.identity_id.clone(),
        token.clone(),
    )
    .context("building the hub client")?;
    let state_dir = paths::state_dir(&identity.identity_id)?;
    Ok(Agent {
        root: joined.root,
        identity,
        hub,
        token,
        state_dir,
    })
}

/// Where a published message ended up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Recorded {
    pub(crate) id: MessageId,
    pub(crate) stream: Stream,
    pub(crate) seq: u64,
}

impl Agent {
    /// The project's topics.
    pub(crate) fn topics(&self) -> Topics {
        self.identity.topics()
    }

    /// An API client for the identity's hub with its token.
    pub(crate) fn api(&self) -> Result<Api> {
        Api::new(self.identity.hub_url.clone(), self.token.clone())
    }

    /// The ids of `names`, each a handle (`adam/coord`) or an id, looked up
    /// in the retained roster (ruling R60). A name no roster entry has is
    /// used as an id when it is one.
    ///
    /// # Errors
    ///
    /// A usage failure naming the first name that is neither.
    pub(crate) async fn resolve(&self, names: &[String]) -> Result<Vec<IdentityId>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let roster = read_state(&self.identity, &self.hub, "roster")
            .await
            .map_err(|err| client_error(err, "reading the project's roster failed"))?;
        names
            .iter()
            .map(|name| {
                if let Some(roster) = &roster
                    && let Some(id) =
                        resolve_handle(roster, name).context("the project's roster is not valid")?
                {
                    return Ok(id);
                }
                name.parse().map_err(|_| {
                    Fail::usage(format!("no identity in this project has the handle {name}")).into()
                })
            })
            .collect()
    }

    /// `mentions` as the wire takes them: `all`, `agents`, `humans`, lanes
    /// and `role:<name>` as they are, handles and ids through
    /// [`Agent::resolve`].
    pub(crate) async fn mentions(&self, mentions: &[String]) -> Result<Vec<Mention>> {
        let mut out = Vec::with_capacity(mentions.len());
        let mut names = Vec::new();
        for mention in mentions {
            let name = mention.strip_prefix('@').unwrap_or(mention);
            match name.parse::<Mention>() {
                Ok(Mention::Identity(_)) | Err(_) => names.push(name.to_owned()),
                Ok(group) => out.push(group),
            }
        }
        out.extend(
            self.resolve(&names)
                .await?
                .into_iter()
                .map(Mention::Identity),
        );
        Ok(out)
    }

    /// Publishes `env` through a one-shot `<id>-cli` session and waits up to
    /// `wait` for the hub's answer.
    ///
    /// # Errors
    ///
    /// [`Code::Rejected`] for a final NACK, [`Code::Queued`] (after printing
    /// `queued id=<id> (not yet recorded)`) when no answer came in time,
    /// [`Code::NotLoggedIn`] when the hub refuses the identity, and other
    /// errors when the session fails.
    pub(crate) async fn publish(&self, env: ClientEnvelope, wait: Duration) -> Result<Recorded> {
        let id = env.id.clone();
        let config = SessionConfig::new(
            ClientKind::Cli,
            self.topics(),
            self.hub.clone(),
            self.identity.broker_url.clone(),
            self.state_dir.clone(),
        );
        let (session, mut events) = Session::connect(config)
            .await
            .map_err(|err| client_error(err, "connecting to the broker failed"))?;
        let outcome = match session.publish(env).await {
            Ok(()) => await_answer(&session, &mut events, &id, wait).await,
            Err(agmen_client::ClientError::EnvelopeTooLarge { .. }) => {
                Err(Fail::rejected("schema (the message is over the 64 KiB envelope limit)").into())
            }
            Err(err) => Err(client_error(err, "queueing the message failed")),
        };
        let closed = session.close().await;
        let state = match outcome? {
            Waited::Answer(state) => {
                if let Err(err) = closed {
                    tracing::debug!(error = %err, "the session ended with an error");
                }
                state
            }
            Waited::Stopped => {
                return Err(match closed {
                    Err(err) => client_error(err, "the broker session failed"),
                    Ok(()) => anyhow::anyhow!("the broker session stopped before the hub answered"),
                });
            }
        };
        match state {
            Some(OutboxState::Recorded { stream, seq }) => Ok(Recorded {
                id,
                stream,
                seq: seq.0,
            }),
            Some(OutboxState::Rejected { reason }) => Err(Fail::rejected(reason).into()),
            Some(OutboxState::Pending { .. }) | None => {
                println_flush(&format!("queued id={id} (not yet recorded)"))
                    .context("writing to stdout")?;
                Err(Fail::new(
                    Code::Queued,
                    format!(
                        "the hub did not answer within {} s; the message stays in this \
                         identity's outbox, and the next agmen command or agmen watch sends it \
                         again",
                        wait.as_secs_f32()
                    ),
                )
                .into())
            }
        }
    }

    /// Prints `recorded #<seq> (<stream>)` for `recorded`.
    pub(crate) fn print_recorded(recorded: &Recorded) -> Result<()> {
        println_flush(&crate::output::recorded(&recorded.stream, recorded.seq))
            .context("writing to stdout")
    }

    /// The hub's answer to `recorded` (GRANT or DENY to a CLAIM, ITEM to a
    /// FOUND): the next message after it in its stream that replies to it.
    /// The hub posts it in the same transaction, so it is already there.
    pub(crate) async fn hub_answer(&self, recorded: &Recorded) -> Result<Option<StampedEnvelope>> {
        let page = self
            .hub
            .page(&recorded.stream, recorded.seq)
            .await
            .map_err(|err| client_error(err, "reading the hub's answer failed"))?;
        Ok(page
            .messages
            .into_iter()
            .find(|message| message.client.reply_to.as_ref() == Some(&recorded.id)))
    }
}

/// How waiting for the hub's answer ended.
#[derive(Debug)]
enum Waited {
    /// A recorded or rejected state, or `None` once the wait has passed.
    Answer(Option<OutboxState>),
    /// The session stopped first; closing it says why.
    Stopped,
}

/// Waits up to `wait` for the hub's answer to `id`.
async fn await_answer(
    session: &Session,
    events: &mut agmen_client::Events,
    id: &MessageId,
    wait: Duration,
) -> Result<Waited> {
    let deadline = Instant::now() + wait;
    let mut poll = tokio::time::interval(OUTBOX_POLL);
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => return Ok(Waited::Answer(None)),
            event = events.recv() => match event {
                Some(ClientEvent::Delivery { id: got, state }) if got == *id => {
                    if !matches!(state, OutboxState::Pending { .. }) {
                        return Ok(Waited::Answer(Some(state)));
                    }
                }
                Some(_) => {}
                None => return Ok(Waited::Stopped),
            },
            _ = poll.tick() => {
                let (outbox, id) = (session.outbox().clone(), id.clone());
                let state = blocking(move || outbox.state(&id).context("reading the outbox")).await?;
                if let Some(state @ (OutboxState::Recorded { .. } | OutboxState::Rejected { .. })) = state {
                    return Ok(Waited::Answer(Some(state)));
                }
            }
        }
    }
}

/// What to do about an identity the hub refuses for good.
pub(crate) const REVOKED_HINT: &str = "run agmen leave, then agmen join";

/// `err` with `what` as context, or a not-signed-in failure when the hub
/// refused the identity for good (revoked, or its token is unknown).
pub(crate) fn client_error(err: agmen_client::ClientError, what: &str) -> anyhow::Error {
    if err.is_refusal() {
        return Fail::new(
            Code::NotLoggedIn,
            format!(
                "the hub refused this worktree's identity ({}); it may have been revoked: \
                 {REVOKED_HINT}",
                clean(&err.to_string())
            ),
        )
        .into();
    }
    anyhow::Error::new(err).context(what.to_owned())
}

/// An envelope with a fresh id, sent now, with `payload` and `body`.
///
/// # Errors
///
/// [`Code::Rejected`] (`body_too_large`) for a body over 32,768 bytes.
pub(crate) fn envelope(payload: Payload, body: String) -> Result<ClientEnvelope> {
    let body = Body::try_from(body).map_err(|_| Fail::rejected("body_too_large"))?;
    Ok(ClientEnvelope {
        v: 1,
        id: new_message_id()?,
        reply_to: None,
        to: Vec::new(),
        mentions: Vec::new(),
        items: Vec::new(),
        lane: None,
        body,
        valid_until: None,
        sent_at: Timestamp::now(),
        payload,
    })
}

/// A fresh ULID: 48 bits of Unix milliseconds, then 80 random bits, in
/// Crockford base32.
fn new_message_id() -> Result<MessageId> {
    const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let ms = u128::try_from(Timestamp::now().as_millisecond())
        .context("the clock is before 1970")?
        & ((1 << 48) - 1);
    let mut random = [0_u8; 10];
    getrandom::fill(&mut random).context("the operating system's random source failed")?;
    let mut value = random
        .iter()
        .fold(ms, |value, byte| (value << 8) | u128::from(*byte));
    let mut out = [b'0'; 26];
    for slot in out.iter_mut().rev() {
        *slot = CROCKFORD[usize::from(u8::try_from(value & 31).unwrap_or(0))];
        value >>= 5;
    }
    let id = String::from_utf8(out.to_vec()).context("a ULID is ASCII")?;
    id.parse()
        .context("a generated ULID is not a valid message id")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_ids_are_ulids_in_time_order() {
        let first = new_message_id().unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let second = new_message_id().unwrap();
        assert_ne!(first, second);
        assert!(
            first.as_str()[..10] < second.as_str()[..10],
            "{first} {second}"
        );
    }

    #[test]
    fn oversize_bodies_are_rejected_locally() {
        let err = envelope(Payload::Fyi {}, "x".repeat(40_000)).unwrap_err();
        let fail = err.downcast_ref::<Fail>().unwrap();
        assert_eq!(fail.code, Code::Rejected);
        assert_eq!(fail.message, "rejected: body_too_large");
        assert!(envelope(Payload::Fyi {}, "x".repeat(32_768)).is_ok());
    }
}
