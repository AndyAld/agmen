//! References in message text, with UTF-8 byte offsets and known targets.

use std::collections::{BTreeMap, BTreeSet};

use agmen_proto::{ItemKey, Ref, RefKind};

use crate::ingest::Roster;

/// The most references stamped on one message, in body order.
pub(crate) const MAX_REFS_PER_MESSAGE: usize = 128;

fn token_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '/' | '-' | '@' | '#')
}

/// Only whole item-key words can need a database lookup. Deduplicate them
/// before applying the cap so repeated words use one query parameter.
pub(crate) fn candidate_item_keys(body: &str) -> Vec<String> {
    let mut keys = BTreeSet::new();
    for word in body.split(|c| !token_char(c)) {
        if let Ok(key) = word.parse::<ItemKey>() {
            keys.insert(key.to_string());
            if keys.len() == MAX_REFS_PER_MESSAGE {
                break;
            }
        }
    }
    keys.into_iter().collect()
}

/// Stamps known names in body order. Tokens consume whole words so a known
/// name inside a longer handle, lane, item or role never becomes a link.
pub(crate) fn stamp_refs(
    body: &str,
    roster: &Roster,
    lanes: &[String],
    items: &[ItemKey],
) -> Vec<Ref> {
    let handles: BTreeMap<&str, &str> = roster
        .0
        .iter()
        .map(|entry| (entry.handle.as_str(), entry.id.as_str()))
        .collect();
    let roles: BTreeSet<&str> = roster
        .0
        .iter()
        .filter_map(|entry| entry.identity_role.as_deref())
        .collect();
    let lanes: BTreeSet<&str> = lanes.iter().map(String::as_str).collect();
    let items: BTreeSet<&str> = items.iter().map(ItemKey::as_str).collect();
    let mut offset = 0;
    let mut refs = Vec::new();
    // At most body.len() chunks, bounded by the protocol body limit.
    for chunk in body.split_inclusive(|c| !token_char(c)) {
        let token = chunk.trim_end_matches(|c| !token_char(c));
        let handle = token.strip_prefix('@').unwrap_or(token);
        let target = if let Some(id) = handles.get(handle) {
            Some((RefKind::Identity, *id))
        } else if lanes.contains(token) {
            Some((RefKind::Lane, token))
        } else if items.contains(token) {
            Some((RefKind::Item, token))
        } else if let Some(role) = token
            .strip_prefix('@')
            .and_then(|word| word.strip_suffix('s'))
        {
            roles.contains(role).then_some((RefKind::Role, role))
        } else if let Some(number) = token.strip_prefix('#') {
            (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
                .then_some((RefKind::Pr, number))
        } else {
            None
        };
        if let Some((kind, target)) = target
            && let (Ok(start), Ok(end)) =
                (u32::try_from(offset), u32::try_from(offset + token.len()))
        {
            refs.push(Ref {
                kind,
                start,
                end,
                target: target.to_owned(),
            });
            if refs.len() == MAX_REFS_PER_MESSAGE {
                break;
            }
        }
        offset += chunk.len();
    }
    refs
}
