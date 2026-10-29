//! Identity naming and verb ceilings shared by every backend.

use std::collections::BTreeSet;

use agmen_proto::{IdentityKind, Verb};

/// Intersects requested verbs with the identity kind's protocol ceiling.
/// Listener ASSIGN additionally requires the coordinator role at mint.
///
/// ```
/// use std::collections::BTreeSet;
/// use agmen_core::cap_verbs;
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

/// Whether `name` may follow `<owner>/` in a child handle.
///
/// ```
/// assert!(agmen_core::is_child_name("coord-2"));
/// assert!(!agmen_core::is_child_name("Coord-2"));
/// ```
#[must_use]
pub fn is_child_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use agmen_proto::IdentityKind;

    #[test]
    fn child_names() {
        for good in ["sol", "a", "w3f", "coord-2", &"x".repeat(32)] {
            assert!(is_child_name(good), "{good}");
        }
        for bad in ["", "-x", "Sol", "a/b", "a b", &"x".repeat(33)] {
            assert!(!is_child_name(bad), "{bad}");
        }
    }

    #[test]
    fn cap_verbs_caps_at_the_kind() {
        let verbs = BTreeSet::from(["FYI".into(), "GRANT".into()]);
        assert_eq!(
            cap_verbs(IdentityKind::Agent, &verbs),
            BTreeSet::from(["FYI".into()])
        );
    }
}
