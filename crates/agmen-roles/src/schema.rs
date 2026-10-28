use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use crate::{Kind, RoleError, Tier};

/// The effective edit permissions after inheritance.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EditScope {
    /// Nothing.
    None,
    /// Paths in claimed or lent lanes.
    Lanes,
    /// Paths in claimed or lent lanes that also match these globs.
    LanesWithin(Vec<String>),
    /// Paths matching these globs.
    Paths(Vec<String>),
}

impl fmt::Display for EditScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::Lanes => f.write_str("lanes"),
            Self::LanesWithin(globs) => write!(f, "lanes within {}", globs.join(" ")),
            Self::Paths(globs) => write!(f, "paths {}", globs.join(" ")),
        }
    }
}

/// Tool classes shared by agent adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ToolClass {
    /// File reading.
    Read,
    /// File and content search.
    Search,
    /// File writes.
    Edit,
    /// Shell commands.
    Shell,
    /// Web access.
    Web,
    /// Worker delegation.
    Subagents,
    /// Fixed read-only git commands served by agmen MCP.
    GitRead,
    /// Personal settings served by agmen MCP.
    Settings,
}

impl fmt::Display for ToolClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Read => "read",
            Self::Search => "search",
            Self::Edit => "edit",
            Self::Shell => "shell",
            Self::Web => "web",
            Self::Subagents => "subagents",
            Self::GitRead => "git-read",
            Self::Settings => "settings",
        })
    }
}

impl FromStr for ToolClass {
    type Err = RoleError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read" => Ok(Self::Read),
            "search" => Ok(Self::Search),
            "edit" => Ok(Self::Edit),
            "shell" => Ok(Self::Shell),
            "web" => Ok(Self::Web),
            "subagents" => Ok(Self::Subagents),
            "git-read" => Ok(Self::GitRead),
            "settings" => Ok(Self::Settings),
            _ => Err(RoleError::Schema(format!("unknown tool class {s}"))),
        }
    }
}

/// A tool declaration. Missing `allow` inherits the parent's tools.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolSet {
    /// Explicit allowlist, including an explicitly empty list.
    pub allow: Option<BTreeSet<ToolClass>>,
    /// Classes removed from the allowlist.
    pub deny: BTreeSet<ToolClass>,
}

/// Worktree creation mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum WorktreeMode {
    /// No worktree.
    #[default]
    None,
    /// A branch worktree.
    Branch,
    /// A detached worktree.
    Detached,
}

/// Worktree cleanup policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Remove {
    /// Preserve the worktree.
    #[default]
    Never,
    /// Remove after merge.
    AfterMerge,
    /// Remove after review.
    AfterReview,
}

/// Worktree settings passed to the launcher.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Worktree {
    /// Creation mode.
    pub mode: WorktreeMode,
    /// Branch name template.
    pub name: Option<String>,
    /// Commit selector for detached worktrees.
    pub at: Option<String>,
    /// Cleanup policy.
    pub remove: Remove,
}

/// Prompt XML fragments in source order, without setting elements.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Prompt {
    /// Purpose, rules, done-when and report XML.
    pub elements: Vec<String>,
}

/// Settings declared by a single role file. Optional fields inherit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    /// Role name.
    pub name: String,
    /// Parent name; `none` explicitly skips base.
    pub extends: Option<String>,
    /// Identity kind.
    pub kind: Option<Kind>,
    /// Default model tier.
    pub tier: Option<Tier>,
    /// Schema version, currently one.
    pub version: u32,
    /// Human-readable summary.
    pub summary: String,
    /// Tool declaration.
    pub tools: ToolSet,
    /// Declared verbs, added to base or narrowed from an ancestor.
    pub verbs: BTreeSet<String>,
    /// Whether the role explicitly declares verbs.
    pub verbs_declared: bool,
    /// Edit declaration.
    pub edits: Option<EditScope>,
    /// Worktree declaration.
    pub worktree: Option<Worktree>,
    /// Prompt fragments.
    pub prompt: Prompt,
}

impl From<EditScope> for agmen_proto::EditScope {
    fn from(scope: EditScope) -> Self {
        match scope {
            EditScope::None => Self::None,
            EditScope::Lanes => Self::Lanes,
            EditScope::LanesWithin(globs) => Self::LanesWithin { globs },
            EditScope::Paths(globs) => Self::Paths { globs },
        }
    }
}

pub(crate) fn valid_name(name: &str) -> bool {
    (2..=32).contains(&name.len())
        && name.bytes().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn attributes(node: roxmltree::Node<'_, '_>, allowed: &[&str]) -> Result<(), RoleError> {
    if let Some(a) = node
        .attributes()
        .find(|a| !allowed.contains(&a.name()) || a.namespace().is_some())
    {
        return Err(RoleError::Schema(format!(
            "unknown attribute {} on {}",
            a.name(),
            node.tag_name().name()
        )));
    }
    Ok(())
}

fn validate(root: roxmltree::Node<'_, '_>) -> Result<(), RoleError> {
    attributes(root, &["name", "extends", "kind", "tier", "version"])?;
    let mut seen = BTreeSet::new();
    for node in root.children().filter(roxmltree::Node::is_element) {
        let tag = node.tag_name().name();
        let allowed: &[&str] = match tag {
            "summary" | "purpose" | "done-when" => &[],
            "rules" => &["topic"],
            "report" => &["verb"],
            "tools" => &["allow", "deny"],
            "verbs" => &["allow"],
            "edits" => &["scope", "globs"],
            "worktree" => &["mode", "name", "at", "remove"],
            _ => return Err(RoleError::Schema(format!("unknown element {tag}"))),
        };
        if node.tag_name().namespace().is_some() || (tag != "rules" && !seen.insert(tag)) {
            return Err(RoleError::Schema(format!(
                "duplicate or namespaced element {tag}"
            )));
        }
        attributes(node, allowed)?;
        for child in node.children().filter(roxmltree::Node::is_element) {
            if tag != "rules"
                || !child.has_tag_name("rule")
                || child.tag_name().namespace().is_some()
            {
                return Err(RoleError::Schema(format!(
                    "unknown element {} in {tag}",
                    child.tag_name().name()
                )));
            }
            attributes(child, &[])?;
            if child.children().any(|n| n.is_element()) {
                return Err(RoleError::Schema("nested markup in rule".into()));
            }
        }
    }
    Ok(())
}

fn worktree(node: roxmltree::Node<'_, '_>) -> Result<Worktree, RoleError> {
    let mode = match node.attribute("mode") {
        Some("none") => WorktreeMode::None,
        Some("branch") => WorktreeMode::Branch,
        Some("detached") => WorktreeMode::Detached,
        _ => return Err(RoleError::Schema("invalid worktree mode".into())),
    };
    let remove = match node.attribute("remove") {
        None | Some("never") => Remove::Never,
        Some("after-merge") => Remove::AfterMerge,
        Some("after-review") => Remove::AfterReview,
        _ => return Err(RoleError::Schema("invalid worktree remove".into())),
    };
    Ok(Worktree {
        mode,
        remove,
        name: node.attribute("name").map(str::to_owned),
        at: node.attribute("at").map(str::to_owned),
    })
}

/// Parse one version-one role, rejecting unknown settings and prompt elements.
///
/// # Errors
/// Returns XML, schema, or attribute errors for unsupported input.
pub fn parse(xml: &str) -> Result<Role, RoleError> {
    parse_inner(xml, true)
}

pub(crate) fn parse_inner(xml: &str, check_name: bool) -> Result<Role, RoleError> {
    if xml.len() > 64 * 1024 {
        return Err(RoleError::Schema("role exceeds 64 KiB".into()));
    }
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    if !root.has_tag_name("role") {
        return Err(RoleError::NotARole);
    }
    if root.tag_name().namespace().is_some() {
        return Err(RoleError::Schema("namespaced role".into()));
    }
    validate(root)?;
    let name = root.attribute("name").ok_or(RoleError::NoName)?;
    if check_name && !valid_name(name) {
        return Err(RoleError::Schema(format!("invalid role name {name}")));
    }
    if root.attribute("version").is_some_and(|v| v != "1") {
        return Err(RoleError::Schema("unsupported role version".into()));
    }
    let child = |tag| root.children().find(|n| n.has_tag_name(tag));
    let classes = |value: &str| {
        value
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<BTreeSet<_>, _>>()
    };
    let tools = child("tools");
    let edits = child("edits")
        .map(|n| match n.attribute("scope") {
            Some("none") => Ok(EditScope::None),
            Some("lanes") => Ok(EditScope::Lanes),
            Some("paths") if n.attribute("globs").is_some_and(|g| !g.trim().is_empty()) => {
                Ok(EditScope::Paths(crate::compat::words(
                    n.attribute("globs").unwrap_or_default(),
                )))
            }
            _ => Err(RoleError::Schema(
                "invalid edits scope or missing globs".into(),
            )),
        })
        .transpose()?;
    let verbs = child("verbs");
    if let Some(EditScope::Paths(globs)) = &edits
        && globs.iter().any(|g| {
            g.starts_with('/') || g.contains('\\') || g.split('/').any(|part| part == "..")
        })
    {
        return Err(RoleError::Schema(
            "edit globs must stay relative to the project".into(),
        ));
    }
    Ok(Role {
        name: name.into(),
        extends: root.attribute("extends").map(str::to_owned),
        kind: root.attribute("kind").map(str::parse).transpose()?,
        tier: root.attribute("tier").map(str::parse).transpose()?,
        version: 1,
        summary: child("summary")
            .and_then(|n| n.text())
            .unwrap_or_default()
            .trim()
            .into(),
        tools: ToolSet {
            allow: tools
                .and_then(|n| n.attribute("allow"))
                .map(classes)
                .transpose()?,
            deny: tools
                .and_then(|n| n.attribute("deny"))
                .map(classes)
                .transpose()?
                .unwrap_or_default(),
        },
        verbs: verbs
            .and_then(|n| n.attribute("allow"))
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        verbs_declared: verbs.is_some(),
        edits,
        worktree: child("worktree").map(worktree).transpose()?,
        prompt: Prompt {
            elements: root
                .children()
                .filter(|n| {
                    n.is_element()
                        && matches!(
                            n.tag_name().name(),
                            "purpose" | "rules" | "done-when" | "report"
                        )
                })
                .map(|n| xml[n.range()].to_owned())
                .collect(),
        },
    })
}
