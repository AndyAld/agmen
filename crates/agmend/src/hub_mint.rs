//! Best-effort rollback at the hub mint credential boundary.

use std::future::Future;
use std::sync::Arc;

use agmen_client::Login;
use agmen_local::console::Status;
use agmen_proto::{IdentityId, IdentityKind, ProjectId};

use crate::hub::Hub;
use crate::identities::Identities;

/// Revokes a hub mint the caller made but will not keep, and deletes its stored
/// credential. Both steps are best effort and both run, so one that fails
/// cannot skip the other. Their errors may contain hub text or credentials, so
/// warnings name only the validated identity id (`noun` is "agent" or
/// "listener"). Returns whether the hub revoked the identity.
pub(crate) async fn rollback_hub_mint(
    hub: &Hub,
    origin: &url::Url,
    id: &IdentityId,
    noun: &str,
) -> bool {
    tracing::warn!(identity = %id, "rolling back an unregistered hub {noun}");
    let revoked = hub.revoke_identity(id).await.is_ok();
    if !revoked {
        tracing::warn!(identity = %id, "revoking an unregistered hub {noun} failed");
    }
    if hub.credentials().delete_token(origin, id).await.is_err() {
        tracing::warn!(identity = %id,
            "deleting an unregistered hub {noun}'s credential failed");
    }
    revoked
}

/// Whether agmend's durable state holds `id`, so it cannot be a mint made just
/// now: an identity row in agmend.db (any kind or project of this person on
/// this hub) or a stored credential (a mint stores one, and `agmen join` does
/// for an identity joined by hand). `identities` is `None` once the identity
/// service has stopped. A lookup that fails counts as held: a revoke cannot be
/// undone.
pub(crate) async fn identity_on_record(
    hub: &Hub,
    identities: Option<Arc<Identities>>,
    login: &Login,
    id: &IdentityId,
) -> bool {
    let Some(identities) = identities else {
        return true;
    };
    let rows = identities
        .managed(
            agmen_client::origin_key(&login.hub_url),
            login.human_id.clone(),
        )
        .await;
    if rows.map_or(true, |rows| rows.iter().any(|row| row.identity_id == *id)) {
        return true;
    }
    hub.credentials()
        .token(&login.hub_url, id)
        .await
        .map_or(true, |token| token.is_some())
}

/// Rolls back a hub mint that was accepted but whose answer cannot be trusted
/// to name it: not JSON, the wrong shape, or another handle or project. Its id
/// is not safe to revoke, because DELETE takes any identity the person may
/// administer, and a group admin may administer a teammate's. The hub allows
/// one live identity per handle in a project (identities_live_handle), and
/// mint accepts only the owner's handle prefix. So after a success status, an
/// offline identity of the requested kind and handle in this project's roster
/// is this mint only if it is fresh: a stopped worker or listener of the
/// person's is just as live and offline under that handle. Fresh means the
/// caller does not hold it (`on_record`) and it has said no HELLO, which would
/// have set the roster's agent and model (docs/protocol.md, state/roster). Both
/// filters run before the count, so an entry they drop is not ambiguity.
/// Nothing is revoked when the roster cannot be read or does not leave exactly
/// one fresh match; one warning says so, with no id and no hub text.
pub(crate) async fn rollback_untrusted_hub_mint<F: Future<Output = bool>>(
    hub: &Hub,
    login: &Login,
    project: &ProjectId,
    handle: &str,
    kind: IdentityKind,
    noun: &str,
    mut on_record: impl FnMut(IdentityId) -> F,
) {
    let Ok(roster) = hub.roster(project).await else {
        tracing::warn!(
            "cannot revoke an unregistered hub {noun}: the project roster could not be read"
        );
        return;
    };
    // shortcut: a HELLO that names neither agent nor model (`agmen watch`
    // sends one) leaves both unset, so only the record check drops that
    // identity; upgrade when the roster shows whether a HELLO was seen.
    let mut fresh = Vec::new();
    for entry in roster.iter().filter(|entry| {
        entry.kind == kind
            && entry.handle == handle
            && entry.status == Status::Offline
            && entry.agent.is_none()
            && entry.model.is_none()
    }) {
        if !on_record(entry.identity_id.clone()).await {
            fresh.push(&entry.identity_id);
            // Two are already ambiguous: read no more records.
            if fresh.len() > 1 {
                break;
            }
        }
    }
    if let [id] = fresh.as_slice() {
        rollback_hub_mint(hub, &login.hub_url, id, noun).await;
    } else {
        tracing::warn!(
            "cannot revoke an unregistered hub {noun}: no single fresh offline match in the project roster"
        );
    }
}
