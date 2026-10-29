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

pub use crate::RoleError;

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
    /// Classes removed after inheriting or replacing the tool allowlist.
    pub tools_deny: Vec<String>,
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

/// Whether the hub's NULL-verb fallback lets an identity of `kind` send `verb`.
// The kind table lives in agmen-proto.
#[must_use]
pub const fn kind_may_send(kind: RoleKind, verb: agmen_proto::Verb) -> bool {
    let kind = match kind {
        RoleKind::Agent => agmen_proto::IdentityKind::Agent,
        RoleKind::Coordinator => agmen_proto::IdentityKind::Coordinator,
        RoleKind::Listener => agmen_proto::IdentityKind::Listener,
    };
    agmen_proto::kind_may_send(kind, verb)
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
    crate::schema::parse_inner(text, false)?;
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
        tools_deny: child("tools")
            .and_then(|n| n.attribute("deny"))
            .map(words)
            .unwrap_or_default(),
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
pub fn resolve(name: &str, specs: &[RoleSpec]) -> Result<ResolvedRole, RoleError> {
    let find = |n: &str| {
        specs
            .iter()
            .find(|s| s.name == n)
            .ok_or_else(|| RoleError::Missing(n.to_owned()))
    };
    let mut chain = vec![find(name)?];
    while let Some(parent) = chain.last().and_then(|s| s.extends.as_deref()) {
        if parent == "none" {
            break;
        }
        if chain.len() >= MAX_DEPTH || chain.iter().any(|s| s.name == parent) {
            return Err(RoleError::TooDeep(name.to_owned()));
        }
        chain.push(find(parent)?);
    }
    let base = chain.last().filter(|s| s.name == "base");
    let nearest = |pick: &dyn Fn(&RoleSpec) -> bool| chain.iter().find(|s| pick(s)).copied();
    let no_kind = || RoleError::NoKind(name.to_owned());
    let kind = chain.iter().find_map(|s| s.kind).ok_or_else(no_kind)?;
    let tier = chain.iter().find_map(|s| s.tier).ok_or_else(no_kind)?;
    let effective = crate::layer::resolve(name, &sources(specs)?)?;
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
    verbs.retain(|verb| effective.verbs.contains(verb));
    let mut tools = Vec::new();
    for spec in chain.iter().rev() {
        if let Some(allow) = &spec.tools {
            tools = allow.clone();
        }
        tools.retain(|tool| !spec.tools_deny.contains(tool));
    }
    tools.retain(|tool| {
        tool.parse()
            .is_ok_and(|class| effective.tools.contains(&class))
    });
    Ok(ResolvedRole {
        kind,
        tier,
        tools,
        verbs,
        // Preserve the legacy display API; launch permissions use the resolved EditScope.
        edits: match effective.edits {
            crate::EditScope::None => Edits::None,
            crate::EditScope::Lanes => Edits::Lanes,
            crate::EditScope::Paths(globs) | crate::EditScope::LanesWithin(globs) => {
                Edits::Paths(globs)
            }
        },
    })
}

#[cfg(test)]
#[path = "roles_tests.rs"]
mod tests;

fn sources(specs: &[RoleSpec]) -> Result<crate::layer::Sources, RoleError> {
    use crate::{Prompt, Role, ToolSet};
    let mut sources = crate::layer::Sources::new();
    for spec in specs {
        let classes = |list: &[String]| {
            list.iter()
                .map(|s| s.parse())
                .collect::<Result<_, RoleError>>()
        };
        let role = Role {
            name: spec.name.clone(),
            extends: Some(spec.extends.clone().unwrap_or_else(|| "none".into())),
            kind: spec.kind,
            tier: spec.tier,
            version: 1,
            summary: String::new(),
            tools: ToolSet {
                allow: spec.tools.as_deref().map(classes).transpose()?,
                deny: classes(&spec.tools_deny)?,
            },
            verbs: spec.verbs.clone().unwrap_or_default().into_iter().collect(),
            verbs_declared: spec.verbs.is_some(),
            edits: spec.edits.as_ref().map(|edits| match edits {
                Edits::None => crate::EditScope::None,
                Edits::Lanes => crate::EditScope::Lanes,
                Edits::Paths(globs) => crate::EditScope::Paths(globs.clone()),
            }),
            worktree: None,
            prompt: Prompt::default(),
        };
        // Preserve the legacy first-definition-wins lookup order.
        sources
            .entry(spec.name.clone())
            .or_insert((role, String::new()));
    }
    Ok(sources)
}
