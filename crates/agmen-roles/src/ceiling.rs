use std::collections::BTreeSet;

use crate::{EditScope, Kind, ToolClass};

/// Baseline protocol verbs for every role inheriting base.
pub(crate) const BASE_VERBS: [&str; 10] = [
    "HELLO", "BYE", "FYI", "ASK", "ANSWER", "ACK", "STATUS", "DM", "ALERT", "FOUND",
];

// shortcut: local spec ceilings while agmen-proto has no shared table;
// replace these checks when L6 exports that table.
pub(crate) fn apply(
    kind: Kind,
    tools: &mut BTreeSet<ToolClass>,
    verbs: &mut BTreeSet<String>,
    edits: &mut EditScope,
    stripped: &mut Vec<String>,
) {
    tools.retain(|tool| {
        let allowed = match kind {
            Kind::Listener => !matches!(tool, ToolClass::Edit | ToolClass::Shell | ToolClass::Web),
            Kind::Agent => *tool != ToolClass::Settings,
            Kind::Coordinator => true,
        };
        if !allowed {
            stripped.push(format!("tool {tool}"));
        }
        allowed
    });
    verbs.retain(|verb| {
        let allowed = BASE_VERBS.contains(&verb.as_str())
            || match kind {
                Kind::Agent => matches!(
                    verb.as_str(),
                    "CLAIM"
                        | "HEARTBEAT"
                        | "RELEASE"
                        | "DONE"
                        | "PUSH_REQUEST"
                        | "LEND"
                        | "REVIEW"
                        | "PLAN"
                        | "PROPOSAL"
                ),
                Kind::Coordinator => matches!(
                    verb.as_str(),
                    "CLAIM"
                        | "HEARTBEAT"
                        | "RELEASE"
                        | "DONE"
                        | "LEND"
                        | "ASSIGN"
                        | "RULING"
                        | "ROLLCALL"
                        | "PROPOSAL"
                        | "DECISION"
                        | "REVIEW"
                        | "PLAN"
                ),
                Kind::Listener => matches!(verb.as_str(), "DELEGATED" | "PROPOSAL"),
            };
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
