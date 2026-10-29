//! Kind ceilings for role permissions supplied by an identity's owner.

use std::collections::BTreeSet;

use agmen_proto::{IdentityKind, Verb};

/// Intersects requested verbs with the identity kind's protocol ceiling.
/// Listener ASSIGN additionally requires the coordinator role at mint.
///
/// ```
/// use std::collections::BTreeSet;
/// use agmen_hub::cap_verbs;
/// use agmen_proto::IdentityKind;
/// let verbs = BTreeSet::from(["FYI".into(), "RULING".into()]);
/// assert_eq!(cap_verbs(IdentityKind::Agent, &verbs), BTreeSet::from(["FYI".into()]));
/// ```
#[must_use]
pub fn cap_verbs(kind: IdentityKind, verbs: &BTreeSet<String>) -> BTreeSet<String> {
    verbs
        .iter()
        .filter(|name| {
            Verb::ALL.iter().any(|verb| {
                verb.as_str() == name.as_str() && agmen_proto::kind_ceiling(kind, *verb)
            })
        })
        .cloned()
        .collect()
}
