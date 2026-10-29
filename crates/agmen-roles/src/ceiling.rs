use std::collections::BTreeSet;

use agmen_proto::{IdentityKind, Verb};

use crate::{EditScope, Kind, ToolClass};

// The kind table lives in agmen-proto.
pub(crate) fn apply(
    kind: Kind,
    tools: &mut BTreeSet<ToolClass>,
    verbs: &mut BTreeSet<String>,
    edits: &mut EditScope,
    stripped: &mut Vec<String>,
) {
    tools.retain(|tool| {
        let allowed = match kind {
            Kind::Listener => !matches!(
                tool,
                ToolClass::Edit | ToolClass::Shell | ToolClass::Web | ToolClass::Subagents
            ),
            Kind::Agent => *tool != ToolClass::Settings,
            Kind::Coordinator => true,
        };
        if !allowed {
            stripped.push(format!("tool {tool}"));
        }
        allowed
    });
    let identity_kind = match kind {
        Kind::Agent => IdentityKind::Agent,
        Kind::Coordinator => IdentityKind::Coordinator,
        Kind::Listener => IdentityKind::Listener,
    };
    verbs.retain(|verb| {
        // A role file cannot know whether its person will be a coordinator;
        // the hub decides listener ASSIGN at mint.
        let allowed = !(kind == Kind::Listener && verb == "ASSIGN")
            && Verb::ALL.iter().any(|candidate| {
                candidate.as_str() == verb && agmen_proto::kind_ceiling(identity_kind, *candidate)
            });
        if !allowed {
            stripped.push(format!("verb {verb}"));
        }
        allowed
    });
    if kind == Kind::Listener && *edits != EditScope::None {
        stripped.push(format!("edits {edits}"));
        *edits = EditScope::None;
    }
}
