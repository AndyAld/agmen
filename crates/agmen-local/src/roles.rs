//! Role files (spec 3.2): the settings agmen reads from a role's XML, and
//! what a role resolves to once it inherits from its parents. agmend reads
//! role files with it and the desktop resolves them for its Roles panel.

use std::fmt;
use std::str::FromStr;

/// A role file's `extends` chain stops here, base included.
const MAX_DEPTH: usize = 8;

/// The built-in roles (spec 3.3) by name, base first: the repo's `roles/`
/// folder, compiled into every binary that shows or launches roles.
pub const BUILT_IN: [(&str, &str); 8] = [
    ("base", include_str!("../../../roles/base.xml")),
    ("listener", include_str!("../../../roles/listener.xml")),
    (
        "coordinator",
        include_str!("../../../roles/coordinator.xml"),
    ),
    ("planner", include_str!("../../../roles/planner.xml")),
    ("researcher", include_str!("../../../roles/researcher.xml")),
    ("engineer", include_str!("../../../roles/engineer.xml")),
    ("tester", include_str!("../../../roles/tester.xml")),
    ("reviewer", include_str!("../../../roles/reviewer.xml")),
];

/// What can go wrong reading or resolving a role.
#[derive(Debug, thiserror::Error)]
pub enum RoleError {
    /// The file is not well-formed XML.
    #[error("not XML: {0}")]
    Xml(#[from] roxmltree::Error),
    /// The root element is some other tag.
    #[error("the root element is not <role>")]
    NotARole,
    /// `<role>` has no `name` attribute.
    #[error("<role> has no name")]
    NoName,
    /// An attribute holds a word it does not allow.
    #[error("{attribute}=\"{value}\" is not one of {allowed}")]
    Attribute {
        /// The attribute, such as `kind`.
        attribute: &'static str,
        /// What the file says.
        value: String,
        /// The words it allows.
        allowed: &'static str,
    },
    /// An `extends` chain names a role no file defines.
    #[error("no role is named \"{0}\"")]
    Missing(String),
    /// The `extends` chain is too long, or loops.
    #[error("\"{0}\" extends more than {MAX_DEPTH} roles deep, or in a loop")]
    TooDeep(String),
    /// No role in the chain declares a kind or a tier.
    #[error("\"{0}\" and its parents name no kind or tier")]
    NoKind(String),
}

/// The identity kind a role needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoleKind {
    /// A worker agent.
    Agent,
    /// The board's coordinator.
    Coordinator,
    /// A person's listener.
    Listener,
}

/// A role's default model tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// The cheapest models.
    Small,
    /// The default.
    Standard,
    /// The strongest models.
    Deep,
}

/// What a role may edit.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Edits {
    /// Nothing.
    None,
    /// Claimed or lent lanes.
    Lanes,
    /// Files matching these globs.
    Paths(Vec<String>),
}

macro_rules! words_enum {
    ($ty:ident, $attr:literal, $allowed:literal, $($word:literal => $variant:ident),+) => {
        impl FromStr for $ty {
            type Err = RoleError;
            fn from_str(value: &str) -> Result<Self, RoleError> {
                match value {
                    $($word => Ok(Self::$variant),)+
                    _ => Err(RoleError::Attribute {
                        attribute: $attr,
                        value: value.to_owned(),
                        allowed: $allowed,
                    }),
                }
            }
        }
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(match self { $(Self::$variant => $word,)+ })
            }
        }
    };
}

words_enum!(RoleKind, "kind", "agent, coordinator, listener",
    "agent" => Agent, "coordinator" => Coordinator, "listener" => Listener);
words_enum!(Tier, "tier", "small, standard, deep",
    "small" => Small, "standard" => Standard, "deep" => Deep);

impl fmt::Display for Edits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::Lanes => f.write_str("lanes"),
            Self::Paths(globs) => write!(f, "paths {}", globs.join(" ")),
        }
    }
}

/// The settings one role file declares; `None` means inherited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSpec {
    /// The `name` attribute.
    pub name: String,
    /// The parent role, when the file names one.
    pub extends: Option<String>,
    /// The identity kind the role needs.
    pub kind: Option<RoleKind>,
    /// The default model tier.
    pub tier: Option<Tier>,
    /// Tool classes, such as `read search shell`.
    pub tools: Option<Vec<String>>,
    /// Verbs added to the base set.
    pub verbs: Option<Vec<String>>,
    /// What the role may edit.
    pub edits: Option<Edits>,
}

/// A role with everything it inherits filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRole {
    /// The identity kind the role needs.
    pub kind: RoleKind,
    /// The default model tier.
    pub tier: Tier,
    /// Tool classes.
    pub tools: Vec<String>,
    /// The role's own verbs first, then base's.
    pub verbs: Vec<String>,
    /// What the role may edit.
    pub edits: Edits,
}

/// Whether the hub lets an identity of `kind` send `verb` at all, whatever
/// its role: the kind cap of protocol section 1.
// shortcut: a copy of agmen-hub's private `may_send` (ingest.rs) for the
// three role kinds; read one shared table once agmen-proto exports it.
#[must_use]
pub const fn kind_may_send(kind: RoleKind, verb: agmen_proto::Verb) -> bool {
    use RoleKind::{Agent, Coordinator, Listener};
    use agmen_proto::Verb;
    match verb {
        Verb::Hello
        | Verb::Bye
        | Verb::Fyi
        | Verb::Ask
        | Verb::Answer
        | Verb::Ack
        | Verb::Status
        | Verb::Alert
        | Verb::Dm
        | Verb::Found
        | Verb::Proposal => true,
        Verb::Rollcall | Verb::Decision | Verb::Ruling | Verb::Assign => {
            matches!(kind, Coordinator)
        }
        Verb::Claim | Verb::Heartbeat | Verb::Release | Verb::Done | Verb::Lend => {
            matches!(kind, Agent | Coordinator)
        }
        Verb::Delegated => matches!(kind, Listener),
        Verb::PushRequest => matches!(kind, Agent),
        Verb::PushOk
        | Verb::PushNo
        | Verb::Review
        | Verb::Plan
        | Verb::Grant
        | Verb::Deny
        | Verb::Item
        | Verb::State => false,
    }
}

/// Splits a space-separated attribute into its words.
#[must_use]
pub fn words(list: &str) -> Vec<String> {
    list.split_whitespace().map(str::to_owned).collect()
}

/// Reads the settings of one role file.
///
/// # Errors
///
/// A [`RoleError`] for text that is not XML, not a `<role>`, has no name,
/// or holds an attribute word agmen does not know.
pub fn parse(text: &str) -> Result<RoleSpec, RoleError> {
    let doc = roxmltree::Document::parse(text)?;
    let role = doc.root_element();
    if role.tag_name().name() != "role" {
        return Err(RoleError::NotARole);
    }
    let child = |tag: &str| role.children().find(|n| n.has_tag_name(tag));
    let allow = |tag: &str| child(tag).and_then(|n| n.attribute("allow")).map(words);
    let edits = match child("edits") {
        None => None,
        Some(node) => Some(match node.attribute("scope") {
            Some("none") => Edits::None,
            Some("lanes") => Edits::Lanes,
            Some("paths") => Edits::Paths(node.attribute("globs").map(words).unwrap_or_default()),
            other => {
                return Err(RoleError::Attribute {
                    attribute: "scope",
                    value: other.unwrap_or_default().to_owned(),
                    allowed: "none, lanes, paths",
                });
            }
        }),
    };
    Ok(RoleSpec {
        name: role.attribute("name").ok_or(RoleError::NoName)?.to_owned(),
        extends: role.attribute("extends").map(str::to_owned),
        kind: role.attribute("kind").map(str::parse).transpose()?,
        tier: role.attribute("tier").map(str::parse).transpose()?,
        tools: allow("tools"),
        verbs: allow("verbs"),
        edits,
    })
}

/// Resolves `name` against `specs`: each setting comes from the nearest role
/// in its `extends` chain that declares it, and verbs add to base's.
///
/// # Errors
///
/// [`RoleError::Missing`], [`RoleError::TooDeep`] or [`RoleError::NoKind`]
/// when the chain cannot be followed or settles nothing.
// shortcut: a child's verbs replace its parent's additions instead of being
// checked as a narrowing; the hub caps verbs at the identity kind anyway.
pub fn resolve(name: &str, specs: &[RoleSpec]) -> Result<ResolvedRole, RoleError> {
    let find = |n: &str| {
        specs
            .iter()
            .find(|s| s.name == n)
            .ok_or_else(|| RoleError::Missing(n.to_owned()))
    };
    let mut chain = vec![find(name)?];
    while let Some(parent) = chain.last().and_then(|s| s.extends.as_deref()) {
        if chain.len() >= MAX_DEPTH {
            return Err(RoleError::TooDeep(name.to_owned()));
        }
        chain.push(find(parent)?);
    }
    let base = chain.last().filter(|s| s.name == "base");
    let nearest = |pick: &dyn Fn(&RoleSpec) -> bool| chain.iter().find(|s| pick(s)).copied();
    let no_kind = || RoleError::NoKind(name.to_owned());
    let kind = chain.iter().find_map(|s| s.kind).ok_or_else(no_kind)?;
    let tier = chain.iter().find_map(|s| s.tier).ok_or_else(no_kind)?;
    let own_verbs = nearest(&|s| s.verbs.is_some() && s.name != "base")
        .and_then(|s| s.verbs.clone())
        .unwrap_or_default();
    let base_verbs = base.and_then(|s| s.verbs.clone()).unwrap_or_default();
    let mut verbs = own_verbs;
    for verb in base_verbs {
        if !verbs.contains(&verb) {
            verbs.push(verb);
        }
    }
    Ok(ResolvedRole {
        kind,
        tier,
        tools: chain
            .iter()
            .find_map(|s| s.tools.clone())
            .unwrap_or_default(),
        verbs,
        edits: chain
            .iter()
            .find_map(|s| s.edits.clone())
            .unwrap_or(Edits::None),
    })
}

#[cfg(test)]
#[path = "roles_tests.rs"]
mod tests;
