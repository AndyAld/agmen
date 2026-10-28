//! `agmen claim`, `release`, `heartbeat` and `done`: the claims this
//! worktree's identity holds. A CLAIM records the commit checked out at
//! that moment in `claims.db` (and a DENY removes it again); DONE diffs
//! from it.

use agmen_proto::{ClientEnvelope, ItemKey, Payload, Timestamp};
use anyhow::{Context as _, Result, bail};
use clap::Args;
use jiff::SignedDuration;

use super::Wait;
use super::post::body;
use crate::claims::Claims;
use crate::client::blocking;
use crate::git;
use crate::joined::{Agent, Recorded, Via, agent, envelope};
use crate::output::{Code, Fail, body_text, clean, println_flush};

/// How long a CLAIM stays valid on its way to the hub (the hub allows 120 s
/// of clock skew past it).
const CLAIM_VALID: SignedDuration = SignedDuration::from_secs(60);

#[derive(Debug, Args)]
pub(crate) struct ClaimArgs {
    /// The item, such as B-412.
    item: ItemKey,
    #[command(flatten)]
    wait: Wait,
}

/// `agmen claim`: exits 0 on GRANT, 4 on DENY.
pub(crate) async fn claim(args: ClaimArgs) -> Result<()> {
    let agent = agent().await?;
    let (recorded, answer) = claim_item(&agent, Via::OneShot(args.wait.wait), args.item).await?;
    Agent::print_recorded(&recorded)?;
    match answer {
        ClaimAnswer::Denied { .. } => Err(Fail::new(Code::Rejected, answer.line()).into()),
        _ => println_flush(&answer.line()).context("writing to stdout"),
    }
}

/// The hub's answer to a CLAIM.
#[derive(Debug)]
pub(crate) enum ClaimAnswer {
    Granted {
        item: ItemKey,
        lease_until: Timestamp,
    },
    Denied {
        item: ItemKey,
        reason: String,
        body: String,
    },
    /// Recorded, but the GRANT or DENY was not found right after it.
    NotFound,
}

impl ClaimAnswer {
    /// The answer for people and the model, every hub-supplied string
    /// through the sanitizer.
    pub(crate) fn line(&self) -> String {
        match self {
            Self::Granted { item, lease_until } => format!("granted {item} until {lease_until}"),
            Self::Denied { item, reason, body } => {
                format!("denied {item}: {} ({})", clean(reason), body_text(body))
            }
            Self::NotFound => "no GRANT or DENY found yet: check agmen inbox".to_owned(),
        }
    }
}

/// Claims `item` through `via` and reads the hub's answer. The commit
/// checked out now is recorded in `claims.db` before the CLAIM goes out, so
/// DONE can diff from it and HEARTBEAT renews it even when the GRANT comes
/// after the wait. A row this call added is removed again on DENY or a
/// final rejection.
///
/// # Errors
///
/// [`Agent::send`]'s, a failure when `HEAD` names no commit, and an error
/// when the hub answers with something other than GRANT or DENY.
pub(crate) async fn claim_item(
    agent: &Agent,
    via: Via<'_>,
    item: ItemKey,
) -> Result<(Recorded, ClaimAnswer)> {
    let base = git::commit(&agent.root, "HEAD")
        .await
        .context("a claim records the commit it starts from, and HEAD has none")?;
    let mut env = envelope(Payload::Claim { item: item.clone() }, String::new())?;
    env.valid_until = Some(
        Timestamp::now()
            .checked_add(CLAIM_VALID)
            .context("the clock is out of range")?,
    );
    let key = item.clone();
    let added = with_claims(agent, move |claims| claims.record(&key, &base)).await?;
    let err = match claim_answer(agent, via, env).await {
        Ok((recorded, answer)) => {
            if matches!(answer, ClaimAnswer::Denied { .. }) {
                forget_added(agent, added, item).await?;
            }
            return Ok((recorded, answer));
        }
        Err(err) => err,
    };
    // Queued, or failed in a way that may still deliver it: the GRANT may
    // come later, so the row stays. A HEARTBEAT for an item the identity
    // does not hold changes nothing at the hub.
    let rejected = err
        .downcast_ref::<Fail>()
        .is_some_and(|fail| fail.code == Code::Rejected);
    if rejected {
        forget_added(agent, added, item).await?;
    }
    Err(err)
}

/// Sends the CLAIM `env` through `via` and reads the hub's answer.
async fn claim_answer(
    agent: &Agent,
    via: Via<'_>,
    env: ClientEnvelope,
) -> Result<(Recorded, ClaimAnswer)> {
    let recorded = agent.send(via, env).await?;
    let answer = match agent.hub_answer(&recorded).await?.map(|m| m.client) {
        Some(answer) => match answer.payload {
            Payload::Grant { item, lease_until } => ClaimAnswer::Granted { item, lease_until },
            Payload::Deny { item, reason } => ClaimAnswer::Denied {
                item,
                reason: serde_json::to_value(reason)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                body: answer.body.as_str().to_owned(),
            },
            _ => bail!("the hub answered the CLAIM with something other than GRANT or DENY"),
        },
        // Recorded, so the hub has decided; the answer will show in the
        // inbox.
        None => ClaimAnswer::NotFound,
    };
    Ok((recorded, answer))
}

/// Removes `item` from `claims.db` when this claim added it: a row from an
/// earlier GRANT stays.
async fn forget_added(agent: &Agent, added: bool, item: ItemKey) -> Result<()> {
    if added {
        with_claims(agent, move |claims| claims.remove(&item)).await?;
    }
    Ok(())
}

#[derive(Debug, Args)]
pub(crate) struct ReleaseArgs {
    /// The item.
    item: ItemKey,
    /// Why, for the room.
    #[arg(long, value_name = "TEXT")]
    reason: Option<String>,
    #[command(flatten)]
    wait: Wait,
}

/// `agmen release`.
pub(crate) async fn release(args: ReleaseArgs) -> Result<()> {
    let agent = agent().await?;
    let recorded =
        release_item(&agent, Via::OneShot(args.wait.wait), args.item, args.reason).await?;
    Agent::print_recorded(&recorded)
}

/// Gives up the claim of `item` through `via` and forgets it in
/// `claims.db`.
pub(crate) async fn release_item(
    agent: &Agent,
    via: Via<'_>,
    item: ItemKey,
    reason: Option<String>,
) -> Result<Recorded> {
    let payload = Payload::Release {
        item: item.clone(),
        reason,
    };
    let recorded = agent.send(via, envelope(payload, String::new())?).await?;
    with_claims(agent, move |claims| claims.remove(&item)).await?;
    Ok(recorded)
}

/// `agmen heartbeat`: one HEARTBEAT naming every claim `claims.db` holds.
pub(crate) async fn heartbeat(wait: Wait) -> Result<()> {
    let agent = agent().await?;
    let Some(env) = heartbeat_envelope(&agent).await? else {
        return println_flush("no claims to renew").context("writing to stdout");
    };
    let recorded = agent.send(Via::OneShot(wait.wait), env).await?;
    Agent::print_recorded(&recorded)
}

/// A HEARTBEAT naming every claim `claims.db` holds, or `None` when it
/// holds none.
pub(crate) async fn heartbeat_envelope(agent: &Agent) -> Result<Option<ClientEnvelope>> {
    let items = with_claims(agent, |claims| claims.items()).await?;
    if items.is_empty() {
        return Ok(None);
    }
    envelope(Payload::Heartbeat { items }, String::new()).map(Some)
}

#[derive(Debug, Args)]
pub(crate) struct DoneArgs {
    /// The item.
    item: ItemKey,
    /// The commit holding the work (a sha, or any name git resolves).
    #[arg(long, value_name = "SHA")]
    commit: String,
    /// A note for the room; - reads it from stdin.
    #[arg(long, value_name = "TEXT")]
    body: Option<String>,
    #[command(flatten)]
    wait: Wait,
}

/// `agmen done`.
pub(crate) async fn done(args: DoneArgs) -> Result<()> {
    let body = body(args.body.as_deref())?;
    let agent = agent().await?;
    let (recorded, line) = done_item(
        &agent,
        Via::OneShot(args.wait.wait),
        args.item,
        &args.commit,
        body,
    )
    .await?;
    Agent::print_recorded(&recorded)?;
    println_flush(&line).context("writing to stdout")
}

/// Reports `item` done at `commit` through `via`: DONE with `branch` from
/// `git rev-parse` and `changes` from `git diff --name-status --no-renames
/// <base_sha_at_claim>...<commit>`. Returns where it was recorded and a
/// line saying what it reported; the claim is forgotten in `claims.db`.
///
/// # Errors
///
/// A failure when `claims.db` holds no claim of `item` or `commit` names
/// no commit, and [`Agent::send`]'s.
pub(crate) async fn done_item(
    agent: &Agent,
    via: Via<'_>,
    item: ItemKey,
    commit: &str,
    body: String,
) -> Result<(Recorded, String)> {
    let key = item.clone();
    let base = with_claims(agent, move |claims| claims.base(&key))
        .await?
        .with_context(|| {
            format!(
                "this worktree has no recorded claim of {item}: claim it with agmen claim first"
            )
        })?;
    let commit = git::commit(&agent.root, commit).await?;
    let branch = git::branch(&agent.root).await?;
    let changes = git::changes(&agent.root, &base, &commit).await?;
    let count = changes.len();
    let line = format!(
        "done {item} at {commit} on {}, {count} changed {}",
        clean(&branch),
        if count == 1 { "path" } else { "paths" }
    );
    let payload = Payload::Done {
        item: item.clone(),
        branch,
        commit,
        base_sha: base,
        checks: Vec::new(),
        changes,
    };
    let recorded = agent.send(via, envelope(payload, body)?).await?;
    with_claims(agent, move |claims| claims.remove(&item)).await?;
    Ok((recorded, line))
}

/// Runs `work` on the identity's claims file, off the async runtime.
async fn with_claims<R: Send + 'static>(
    agent: &Agent,
    work: impl FnOnce(&Claims) -> Result<R> + Send + 'static,
) -> Result<R> {
    let dir = agent.state_dir.clone();
    blocking(move || work(&Claims::open(&dir)?)).await
}
