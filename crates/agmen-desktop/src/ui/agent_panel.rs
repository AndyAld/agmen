//! The right dock's Agent panel, for the agent selected in the Agents dock.

use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div,
};

use super::components::{Color, EmptyState, KvRow, KvSection, ProgressBar};
use super::dock::RightDockHeader;
use super::format::{agent_tokens, money_of, wall_clock};
use super::tokens::{SPACE_7, SPACE_14, SPACE_16};
use crate::backend::{AgentInfo, AgentStatus};

#[derive(Debug, IntoElement)]
pub(crate) struct AgentPanel {
    info: AgentInfo,
    taken_over: bool,
}

impl AgentPanel {
    #[must_use]
    pub(crate) const fn new(info: AgentInfo) -> Self {
        Self {
            info,
            taken_over: false,
        }
    }

    /// Task 14: whether the person took the agent's session over in the
    /// chat, which pauses its headless run (spec 5.4).
    #[must_use]
    pub(crate) const fn taken_over(mut self, taken_over: bool) -> Self {
        self.taken_over = taken_over;
        self
    }
}

/// What the Agent panel says for an agent while this machine has no
/// local agents to show details for (Task 7).
pub(crate) const NO_LOCAL_AGENTS: &str = "No local agents";

/// The Agent panel for `handle` without details: its empty state. Tests
/// find it as `agent-panel:none`.
pub(crate) fn no_local_agents(handle: impl Into<SharedString>) -> impl IntoElement {
    div()
        .debug_selector(|| "agent-panel".into())
        .size_full()
        .flex()
        .flex_col()
        .child(RightDockHeader::new("Agent", handle))
        .child(EmptyState::new("agent-panel:none", NO_LOCAL_AGENTS))
}

/// The State row's text and color: the take-over in the accent color while
/// the agent's status stays working (frame 760, node 1198).
fn state(info: &AgentInfo, taken_over: bool) -> (String, Color) {
    if taken_over {
        (
            "taken over in chat; headless run paused".into(),
            Color::Accent,
        )
    } else if info.status == AgentStatus::Working {
        (info.state.clone(), Color::Success)
    } else {
        (info.state.clone(), Color::Accent)
    }
}

impl RenderOnce for AgentPanel {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let (state_text, state_color) = state(&self.info, self.taken_over);
        let i = self.info;
        let role = Color::Role(i.role.clone().into());
        let mut runs = KvSection::new("Runs")
            .row(KvRow::new("Role", i.role_label).color(role))
            .row(KvRow::new("Harness", i.harness_label))
            .row(KvRow::new("Model", i.model));
        if let Some(by) = i.delegated_by {
            runs = runs.row(KvRow::new("Delegated by", by));
        }
        let runs = runs.row(KvRow::new("State", state_text).color(state_color));
        // Spec 5.5: a harness that cannot enforce the role's tool list says
        // what it does enforce, in the warning color.
        let tools = KvRow::new("Tools", i.tools.join(" "));
        let tools = if i.tools_lossy {
            tools.color(Color::Warning)
        } else {
            tools
        };
        let enforced = KvSection::new("Enforced")
            .row(tools)
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
    fn taken_over_state_is_accent_while_the_status_stays_working() {
        // V66: frame 760's State, node 1198, beside a green status dot.
        let info = chat_snapshot()
            .agents
            .remove(&crate::backend::fixture_id("andy/fx1"))
            .unwrap();
        assert_eq!(info.status, AgentStatus::Working);
        assert_eq!(state(&info, false), ("working".into(), Color::Success));
        assert_eq!(
            state(&info, true),
            (
                "taken over in chat; headless run paused".into(),
                Color::Accent
            )
        );
    }

    #[gpui::test]
    fn agent_panel_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            let info = chat_snapshot()
                .agents
                .remove(&"andyfx10".parse::<agmen_proto::IdentityId>().unwrap())
                .unwrap();
            div()
                .w(crate::ui::tokens::RIGHT_DOCK_WIDTH)
                .child(AgentPanel::new(info))
                .into_any_element()
        });
        assert!(size.height > px(300.));
    }
}
