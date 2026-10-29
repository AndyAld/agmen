//! The identity this worktree acts as: its trusted identity file (rulings
//! R42 to R42b, through agmen-client's `find_identity`), its token, and
//! what every command that speaks for it shares: retained state, handle
//! lookup over HTTP (rulings R60 and R68), envelopes, and publishing that
//! waits for the hub's answer, through a one-shot `<id>-cli` session (the
//! CLI) or a session the caller holds open (`agmen mcp`).

use std::path::{Path, PathBuf};
use std::time::Duration;

use agmen_client::{
    ClientEvent, ClientKind, HubClient, IdentityError, IdentityFile, Joined, OutboxState, Session,
    SessionConfig, TokenKey, TokenStore, find_identity, read_state, resolve_handle,
};
use agmen_proto::{
    Body, ClientEnvelope, IdentityId, Mention, MessageId, Payload, StampedEnvelope, Stream,
    Timestamp, Topics,
};
use anyhow::{Context as _, Result};
use serde_json::Value;
use tokio::sync::Notify;
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
    Agent::load(joined, TokenStore::from_env(), &paths::state_base()?).await
}

impl Agent {
    /// The identity of `joined`, which passed the trust test, with its token
    /// from `tokens` and its state folder in `state_base`.
    ///
    /// # Errors
    ///
    /// A [`Code::NotLoggedIn`] failure when `tokens` holds no token for it.
    pub(crate) async fn load(
        joined: Joined,
        tokens: TokenStore,
        state_base: &Path,
    ) -> Result<Self> {
        let identity = joined.identity;
        let key = TokenKey::new(&identity.hub_url, &identity.identity_id)
            .context("the identity file's hub URL")?;
        let token = get_token(tokens, key).await?.ok_or_else(|| {
            Fail::new(
                Code::NotLoggedIn,
                format!(
                    "this machine holds no agmen token for {} at {}: run agmen leave, then agmen \
                     join again in this worktree",
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
        let state_dir = state_base.join(identity.identity_id.as_str());
        Ok(Self {
            root: joined.root,
            identity,
            hub,
            token,
            state_dir,
        })
    }
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

    /// The project's retained `state/{key}`, read the way `via` says: a
    /// one-shot `<id>-cli` connection for the CLI, the held session for
    /// `agmen mcp` (ruling R68), which must not take `<id>-cli` from a
    /// concurrent command or hook of the same identity.
    ///
    /// # Errors
    ///
    /// The client's error for a bad key, a payload that is not JSON, or a
    /// failed read.
    pub(crate) async fn read_state(
        &self,
        via: Via<'_>,
        key: &str,
    ) -> Result<Option<Value>, agmen_client::ClientError> {
        match via {
            Via::OneShot(_) => read_state(&self.identity, &self.hub, key).await,
            Via::Held { session, .. } => session.read_state(key).await,
        }
    }

    /// The ids of `names`, each a handle (`adam/coord`) or an id, looked up
    /// in the roster [`HubClient::roster`] returns (ruling R60). A name no
    /// roster entry has is used as an id when it is one.
    ///
    /// # Errors
    ///
    /// A usage failure naming the first name that is neither, or the
    /// client's error when the roster cannot be read.
    pub(crate) async fn resolve(&self, names: &[String]) -> Result<Vec<IdentityId>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let roster = self
            .hub
            .roster()
            .await
            .map_err(|err| client_error(err, "reading the project's roster failed"))?;
        names
            .iter()
            .map(|name| {
                if let Some(id) =
                    resolve_handle(&roster, name).context("the project's roster is not valid")?
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
        let (mut out, names) = mention_tokens(mentions.iter().map(String::as_str));
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
        match state.and_then(|state| settled(&id, state)) {
            Some(outcome) => outcome,
            None => {
                println_flush(&queued(&id)).context("writing to stdout")?;
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

    /// Sends `env` the way `via` says and waits for the hub's answer.
    ///
    /// # Errors
    ///
    /// As [`Agent::publish`]; through a held session a message still
    /// queued after the wait is a [`Code::Queued`] failure whose message is
    /// `queued id=<id> (not yet recorded)`, and nothing is printed.
    pub(crate) async fn send(&self, via: Via<'_>, env: ClientEnvelope) -> Result<Recorded> {
        match via {
            Via::OneShot(wait) => self.publish(env, wait).await,
            Via::Held {
                session,
                answered,
                wait,
            } => publish_held(session, answered, env, wait).await,
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

/// Splits mention tokens into protocol groups and names to resolve. Both
/// explicit post mentions and chat's whitespace-delimited @ tokens use this
/// parser, preserving the existing group-first order.
pub(crate) fn mention_tokens<'a>(
    tokens: impl IntoIterator<Item = &'a str>,
) -> (Vec<Mention>, Vec<String>) {
    let mut groups = Vec::new();
    let mut names = Vec::new();
    for token in tokens {
        let name = token.strip_prefix('@').unwrap_or(token);
        match name.parse::<Mention>() {
            Ok(Mention::Identity(_)) | Err(_) => names.push(name.to_owned()),
            Ok(group) => groups.push(group),
        }
    }
    (groups, names)
}

/// How a command's message reaches the hub, and how long the command waits
/// for the hub's answer.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Via<'a> {
    /// A one-shot `<id>-cli` session per message (the CLI).
    OneShot(Duration),
    /// A session the caller keeps open (`agmen mcp`).
    Held {
        session: &'a Session,
        /// Woken whenever the session reports a delivery.
        answered: &'a Notify,
        wait: Duration,
    },
}

/// `queued id=<id> (not yet recorded)`: the hub has not answered yet.
fn queued(id: &MessageId) -> String {
    format!("queued id={id} (not yet recorded)")
}

/// What a final outbox state means for the command that sent `id`:
/// recorded, or rejected for good. `None` while it is still pending.
fn settled(id: &MessageId, state: OutboxState) -> Option<Result<Recorded>> {
    match state {
        OutboxState::Recorded { stream, seq } => Some(Ok(Recorded {
            id: id.clone(),
            stream,
            seq: seq.0,
        })),
        OutboxState::Rejected { reason } => Some(Err(Fail::rejected(reason).into())),
        OutboxState::Pending { .. } => None,
    }
}

/// Queues `env` on `session` and waits up to `wait` for the hub's answer,
/// looking at the outbox whenever `answered` wakes and every
/// [`OUTBOX_POLL`].
async fn publish_held(
    session: &Session,
    answered: &Notify,
    env: ClientEnvelope,
    wait: Duration,
) -> Result<Recorded> {
    let id = env.id.clone();
    match session.publish(env).await {
        Ok(()) => {}
        Err(agmen_client::ClientError::EnvelopeTooLarge { .. }) => {
            return Err(
                Fail::rejected("schema (the message is over the 64 KiB envelope limit)").into(),
            );
        }
        Err(err) => return Err(client_error(err, "queueing the message failed")),
    }
    let deadline = Instant::now() + wait;
    loop {
        // Registered before the outbox read, so a delivery reported while
        // it runs still wakes this wait.
        let woken = answered.notified();
        tokio::pin!(woken);
        woken.as_mut().enable();
        let (outbox, read_id) = (session.outbox().clone(), id.clone());
        let state = blocking(move || outbox.state(&read_id).context("reading the outbox")).await?;
        if let Some(outcome) = state.and_then(|state| settled(&id, state)) {
            return outcome;
        }
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => {
                return Err(Fail::new(Code::Queued, queued(&id)).into());
            }
            () = &mut woken => {}
            () = tokio::time::sleep(OUTBOX_POLL) => {}
        }
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
    fn mention_tokens_parse_once() {
        let post = [
            "@adam/coord",
            "all",
            "@agents",
            "humans",
            "@L-UI",
            "role:reviewer",
            "idn7k2q9",
        ];
        let chat = "stand by @adam/coord @all @agents @humans @L-UI @role:reviewer @idn7k2q9";
        assert_eq!(
            mention_tokens(post),
            mention_tokens(chat.split_whitespace().filter(|word| word.starts_with('@')))
        );
        let (groups, names) = mention_tokens(post);
        assert_eq!(groups.len(), 5);
        assert_eq!(names, ["adam/coord", "idn7k2q9"]);
    }

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
