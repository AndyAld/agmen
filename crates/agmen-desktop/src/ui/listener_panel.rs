//! The right dock's Listener panel.

use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window,
    div,
};

use super::components::{Button, Color, KvRow, KvSection};
use super::dock::RightDockHeader;
use super::format::{listener_tokens, money, money_of, workers_today};
use super::tokens::{SPACE_14, SPACE_16};
use crate::backend::{ListenerInfo, RecentKind};

#[derive(Debug, IntoElement)]
pub(crate) struct ListenerPanel {
    info: ListenerInfo,
}

impl ListenerPanel {
    #[must_use]
    pub(crate) const fn new(info: ListenerInfo) -> Self {
        Self { info }
    }
}

impl RenderOnce for ListenerPanel {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let i = self.info;
        let runs = KvSection::new("Runs")
            .row(KvRow::new("Harness", i.harness_label))
            .row(KvRow::new("Role", i.role_label))
            .row(KvRow::new("Can use", i.can_use.join(" ")).color(Color::Success))
            .row(KvRow::new("Never", i.never.join(" ")).color(Color::Error));
        let policy = KvSection::new("Policy")
            .row(KvRow::new("Takes work from", i.takes_work_from))
            .row(KvRow::new("Per worker", money(i.per_worker_usd)))
            .row(KvRow::new(
                "Workers today",
                workers_today(
                    i.workers_today_usd,
                    i.workers_cap_usd,
                    i.workers_running,
                    i.workers_max,
                ),
            ))
            .row(KvRow::new(
                "Listener today",
                money_of(i.listener_today_usd, i.listener_cap_usd),
            ))
            .row(KvRow::new(
                "Listener tokens",
                listener_tokens(i.tokens_in, i.tokens_out, i.uptime_minutes),
            ));
        let recent = i
            .recent
            .into_iter()
            .fold(KvSection::new("Recent"), |section, action| {
                let color = recent_color(action.kind);
                section.row(KvRow::new(action.time, action.text).color(color))
            });
        div()
            .debug_selector(|| "listener-panel".into())
            .size_full()
            .flex()
            .flex_col()
            .child(RightDockHeader::new("Listener", i.handle))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(SPACE_16)
                    .p(SPACE_14)
                    .child(runs)
                    .child(policy)
                    .child(recent)
                    .child(Button::new("change-listener", "Change listener…")),
            )
    }
}

/// A Recent row's text color (console.py 1504 to 1511).
#[must_use]
const fn recent_color(kind: RecentKind) -> Color {
    // Work the listener started reads bright; its answers read dim. A
    // settings change reads as a warning while it waits, then as an accent.
    match kind {
        RecentKind::Delegated => Color::Default,
        RecentKind::Answered => Color::Placeholder,
        RecentKind::Waiting => Color::Warning,
        RecentKind::Applied => Color::Accent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::chat_snapshot;
    use crate::ui::test_support::rendered_size;
    use gpui::{TestAppContext, px};

    #[test]
    fn recent_rows_take_their_kind_color() {
        // V73 and V65: console.py 1504 to 1511 (WARN, ACCENT, TEXT, FAINT).
        assert_eq!(recent_color(RecentKind::Waiting), Color::Warning);
        assert_eq!(recent_color(RecentKind::Applied), Color::Accent);
        assert_eq!(recent_color(RecentKind::Delegated), Color::Default);
        assert_eq!(recent_color(RecentKind::Answered), Color::Placeholder);
    }

    #[gpui::test]
    fn listener_panel_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            div()
                .w(crate::ui::tokens::RIGHT_DOCK_WIDTH)
                .child(ListenerPanel::new(chat_snapshot().listener))
                .into_any_element()
        });
        assert!(size.height > px(300.));
    }
}
