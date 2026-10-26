//! The right dock's Agent panel, for the agent selected in the Agents dock.

use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window,
    div,
};

use super::components::{Color, KvRow, KvSection, ProgressBar};
use super::dock::RightDockHeader;
use super::format::{agent_tokens, money_of, wall_clock};
use super::tokens::{SPACE_7, SPACE_14, SPACE_16};
use crate::backend::AgentInfo;

#[derive(Debug, IntoElement)]
pub(crate) struct AgentPanel {
    info: AgentInfo,
}

impl AgentPanel {
    #[must_use]
    pub(crate) const fn new(info: AgentInfo) -> Self {
        Self { info }
    }
}

/// The role key in a label such as "Engineer (built-in)".
fn role_key(role_label: &str) -> String {
    role_label
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase()
}

impl RenderOnce for AgentPanel {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let i = self.info;
        let state_color = if i.state == "working" {
            Color::Success
        } else {
            Color::Accent
        };
        let role = Color::Role(role_key(&i.role_label).into());
        let mut runs = KvSection::new("Runs")
            .row(KvRow::new("Role", i.role_label).color(role))
            .row(KvRow::new("Harness", i.harness_label))
            .row(KvRow::new("Model", i.model));
        if let Some(by) = i.delegated_by {
            runs = runs.row(KvRow::new("Delegated by", by));
        }
        let runs = runs.row(KvRow::new("State", i.state).color(state_color));
        let enforced = KvSection::new("Enforced")
            .row(KvRow::new("Tools", i.tools.join(" ")))
            .row(KvRow::new("Edits", i.edits))
            .row(KvRow::new("Verbs", i.verbs.join(" ")).color(Color::Success))
            .row(KvRow::new("Refused", i.refused.join(" ")).color(Color::Error));
        let worktree = KvSection::new("Worktree")
            .row(KvRow::new("Branch", i.branch))
            .row(KvRow::new("Base", i.base))
            .row(KvRow::new("State", i.worktree_state).color(Color::Success))
            .row(KvRow::new("Cleanup", i.cleanup).color(Color::Placeholder));
        let budget = KvSection::new("Budget")
            .row(KvRow::new("Spent", money_of(i.spent_usd, i.budget_usd)))
            .row(KvRow::new(
                "Wall clock",
                wall_clock(i.since_launch_minutes, i.working_minutes),
            ))
            .row(KvRow::new(
                "Tokens",
                agent_tokens(i.tokens_in, i.tokens_out, i.cache_read),
            ));
        let used = (i.spent_usd / i.budget_usd) as f32;
        div()
            .debug_selector(|| "agent-panel".into())
            .size_full()
            .flex()
            .flex_col()
            .child(RightDockHeader::new("Agent", i.handle))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(SPACE_16)
                    .p(SPACE_14)
                    .child(runs)
                    .child(enforced)
                    .child(worktree)
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .gap(SPACE_7)
                            .child(budget)
                            .child(ProgressBar::new(used)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::chat_snapshot;
    use crate::ui::test_support::rendered_size;
    use gpui::{TestAppContext, px};

    #[test]
    fn role_key_from_label() {
        assert_eq!(role_key("Engineer (built-in)"), "engineer");
        assert_eq!(role_key(""), "");
    }

    #[gpui::test]
    fn agent_panel_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            let info = chat_snapshot().agents.remove("andy/fx1").unwrap();
            div()
                .w(crate::ui::tokens::RIGHT_DOCK_WIDTH)
                .child(AgentPanel::new(info))
                .into_any_element()
        });
        assert!(size.height > px(300.));
    }
}
