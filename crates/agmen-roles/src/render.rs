use crate::{Resolved, ToolClass};

/// Render prompt fragments ancestor first, preserving each fragment's XML.
#[must_use]
pub fn render_prompt(role: &Resolved) -> String {
    role.role.prompt.elements.join("\n")
}

/// Map file/web/worker classes to Claude tool names; MCP classes add no tools.
#[must_use]
pub fn claude_tools(role: &Resolved) -> Vec<&'static str> {
    role.tools
        .iter()
        .flat_map(|class| match class {
            ToolClass::Read => &["Read"][..],
            ToolClass::Search => &["Grep", "Glob"][..],
            ToolClass::Edit => &["Edit", "Write", "NotebookEdit"][..],
            ToolClass::Shell => &["Bash"][..],
            ToolClass::Web => &["WebFetch", "WebSearch"][..],
            ToolClass::Subagents => &["Agent"][..],
            ToolClass::GitRead | ToolClass::Settings => &[],
        })
        .copied()
        .collect()
}
