//! The right dock's Listener panel.

use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window,
    div,
};

use super::components::{Button, Color, EmptyState, KvRow, KvSection};
use super::dock::RightDockHeader;
use super::format::{duration, listener_tokens, money, money_of, tokens, workers_today};
use super::tokens::{SPACE_14, SPACE_16};
use crate::backend::{ListenerDetails, ListenerField, ListenerInfo, RecentKind};

/// What the panel says when the person has no listener in the project.
pub(crate) const NO_LISTENER: &str = "No listener for this project";

/// What it says while agmend reports the listener but not its policy or
/// spend (Task 7 review).
pub(crate) const NO_DETAILS: &str = "No policy or spend from agmend yet";

/// Tests find it as `listener-panel`, and its empty state as
/// `listener-panel:none`.
#[derive(Debug, IntoElement)]
pub(crate) struct ListenerPanel {
    info: Option<ListenerInfo>,
}

impl ListenerPanel {
    #[must_use]
    pub(crate) const fn new(info: Option<ListenerInfo>) -> Self {
        Self { info }
    }
}

impl RenderOnce for ListenerPanel {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let Some(i) = self.info else {
            return div()
                .debug_selector(|| "listener-panel".into())
                .size_full()
                .flex()
                .flex_col()
                .child(RightDockHeader::new("Listener", ""))
                .child(EmptyState::new("listener-panel:none", NO_LISTENER))
                .into_any_element();
        };
        let Some(d) = i.details else {
            // Review fix 3: the handle, agent and state agmend reports,
            // and no figures it has no source for.
            return div()
                .debug_selector(|| "listener-panel".into())
                .size_full()
                .flex()
                .flex_col()
                .child(RightDockHeader::new("Listener", i.handle))
                .child(
                    div().w_full().p(SPACE_14).child(
                        KvSection::new("Runs")
                            .row(KvRow::new("Agent", i.agent))
                            .row(KvRow::new("State", i.state)),
                    ),
                )
                .child(EmptyState::new("listener-panel:no-details", NO_DETAILS))
                .into_any_element();
        };
        div()
            .debug_selector(|| "listener-panel".into())
            .size_full()
            .flex()
            .flex_col()
            .child(RightDockHeader::new("Listener", i.handle))
            .child(details(d))
            .into_any_element()
    }
}

/// The Runs, Policy and Recent sections, and the Change listener button.
fn details(d: ListenerDetails) -> impl IntoElement {
    let [
        can_use,
        never,
        takes_work_from,
        per_worker,
        workers,
        listener,
        tokens,
    ] = panel_values(&d);
    let runs = KvSection::new("Runs")
        .row(KvRow::new("Harness", d.harness_label))
        .row(KvRow::new("Role", d.role_label))
        .row(value_row("Can use", can_use))
        .row(value_row("Never", never));
    let policy = KvSection::new("Policy")
        .row(value_row("Takes work from", takes_work_from))
        .row(value_row("Per worker", per_worker))
        .row(value_row("Workers today", workers))
        .row(value_row("Listener today", listener))
        .row(value_row("Listener tokens", tokens));
    let recent = d
        .recent
        .into_iter()
        .fold(KvSection::new("Recent"), |section, action| {
            let color = recent_color(action.kind);
            section.row(KvRow::new(action.time, action.text).color(color))
        });
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
        .child(Button::new("change-listener", "Change listener…"))
}

fn value_row(label: &'static str, (text, color): (String, Color)) -> KvRow {
    KvRow::new(label, text).color(color)
}

/// Keep each reported part of a composite row, including measured zeroes.
fn panel_values(d: &ListenerDetails) -> [(String, Color); 7] {
    use ListenerField::{
        CanUse, Never, PerWorker, TakesWorkFrom, TokensIn, TokensOut, Uptime, WorkersCap,
        WorkersMax, WorkersRunning, WorkersToday,
    };
    let value = |field, text: String| {
        if d.unreported.contains(&field) {
            "not reported".to_owned()
        } else {
            text
        }
    };
    let missing =
        |fields: &[ListenerField]| fields.iter().any(|field| d.unreported.contains(field));
    let row = |fields: &[ListenerField], text, color| {
        let color = if missing(fields) {
            Color::Placeholder
        } else {
            color
        };
        (text, color)
    };
    [
        row(
            &[CanUse],
            value(CanUse, d.can_use.join(" ")),
            Color::Success,
        ),
        row(&[Never], value(Never, d.never.join(" ")), Color::Error),
        row(
            &[TakesWorkFrom],
            value(TakesWorkFrom, d.takes_work_from.clone()),
            Color::Default,
        ),
        row(
            &[PerWorker],
            value(PerWorker, money(d.per_worker_usd)),
            Color::Default,
        ),
        row(
            &[WorkersToday, WorkersCap, WorkersRunning, WorkersMax],
            if missing(&[WorkersToday, WorkersCap, WorkersRunning, WorkersMax]) {
                format!(
                    "{} of {} · {} of {} running",
                    value(WorkersToday, money(d.workers_today_usd)),
                    value(WorkersCap, money(d.workers_cap_usd)),
                    value(WorkersRunning, d.workers_running.to_string()),
                    value(WorkersMax, d.workers_max.to_string()),
                )
            } else {
                workers_today(
                    d.workers_today_usd,
                    d.workers_cap_usd,
                    d.workers_running,
                    d.workers_max,
                )
            },
            Color::Default,
        ),
        (
            money_of(d.listener_today_usd, d.listener_cap_usd),
            Color::Default,
        ),
        row(
            &[TokensIn, TokensOut, Uptime],
            if missing(&[TokensIn, TokensOut, Uptime]) {
                format!(
                    "{} in · {} out · up {}",
                    value(TokensIn, tokens(d.tokens_in)),
                    value(TokensOut, tokens(d.tokens_out)),
                    value(Uptime, duration(d.uptime_minutes)),
                )
            } else {
                listener_tokens(d.tokens_in, d.tokens_out, d.uptime_minutes)
            },
            Color::Default,
        ),
    ]
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
    use crate::ui::test_support::{draw, rendered_size};
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
    fn empty_listener_panel_says_none(cx: &mut TestAppContext) {
        // Task 7: a project with no listener for the person says so, with
        // none of the fixture's numbers.
        let cx = draw(cx, || {
            div()
                .w(crate::ui::tokens::RIGHT_DOCK_WIDTH)
                .child(ListenerPanel::new(None))
                .into_any_element()
        });
        assert!(cx.debug_bounds("listener-panel").is_some());
        assert!(cx.debug_bounds("listener-panel:none").is_some());
        assert!(cx.debug_bounds("kv-label:Harness:0").is_none());
        assert_eq!(NO_LISTENER, "No listener for this project");
    }

    #[gpui::test]
    fn listener_panel_shows_live_caps(cx: &mut TestAppContext) {
        // Task 10: a live snapshot carries the listener's caps, and the
        // panel draws them instead of the no-details empty state.
        let listener = crate::backend::live_snapshot().listener;
        let details = listener
            .as_ref()
            .and_then(|listener| listener.details.as_ref())
            .expect("live listener caps");
        assert_eq!(details.harness_label, "claude-haiku-5-5");
        assert_eq!(details.listener_today_usd, 0.40);
        assert_eq!(details.listener_cap_usd, 2.0);
        assert_eq!(details.per_worker_usd, 5.0);
        assert_eq!(details.workers_cap_usd, 20.0);
        assert_eq!(details.workers_max, 3);
        let cx = draw(cx, move || {
            div()
                .w(crate::ui::tokens::RIGHT_DOCK_WIDTH)
                .child(ListenerPanel::new(listener.clone()))
                .into_any_element()
        });
        assert!(cx.debug_bounds("kv-label:Listener today:3").is_some());
        assert!(cx.debug_bounds("kv-label:Harness:0").is_some());
        assert!(cx.debug_bounds("listener-panel:no-details").is_none());
    }

    #[gpui::test]
    fn a_listener_without_details_shows_no_dollar_figures(cx: &mut TestAppContext) {
        // A listener agmend has not given policy or spend for still shows
        // its handle, agent and state, and no $0.00 that reads as a cap.
        let listener = Some(crate::backend::ListenerInfo {
            id: "idlisten".parse().unwrap(),
            handle: "andy/listen".into(),
            agent: "codex".into(),
            state: "away".into(),
            details: None,
        });
        assert!(listener.as_ref().is_some_and(|l| l.details.is_none()));
        let cx = draw(cx, move || {
            div()
                .w(crate::ui::tokens::RIGHT_DOCK_WIDTH)
                .child(ListenerPanel::new(listener.clone()))
                .into_any_element()
        });
        assert!(cx.debug_bounds("listener-panel:no-details").is_some());
        assert!(cx.debug_bounds("kv-label:Agent:0").is_some());
        assert!(cx.debug_bounds("kv-label:State:1").is_some());
        for label in [
            "Per worker:1",
            "Workers today:2",
            "Listener today:3",
            "Harness:0",
        ] {
            let selector: &'static str = Box::leak(format!("kv-label:{label}").into_boxed_str());
            assert!(cx.debug_bounds(selector).is_none(), "{label}");
        }
        // Not wired yet, so not offered.
        assert!(cx.debug_bounds("change-listener").is_none());
    }

    #[gpui::test]
    fn a_listener_shows_its_rows_and_no_empty_state(cx: &mut TestAppContext) {
        // The other side of `empty_listener_panel_says_none`'s selectors.
        let cx = draw(cx, || {
            div()
                .w(crate::ui::tokens::RIGHT_DOCK_WIDTH)
                .child(ListenerPanel::new(chat_snapshot().listener))
                .into_any_element()
        });
        assert!(cx.debug_bounds("kv-label:Harness:0").is_some());
        assert!(cx.debug_bounds("listener-panel:none").is_none());
        assert!(cx.debug_bounds("kv-label:Per worker:1").is_some());
        assert!(cx.debug_bounds("change-listener").is_some());
        assert!(cx.debug_bounds("listener-panel:no-details").is_none());
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

    #[gpui::test]
    fn live_status_marks_each_missing_value_as_not_reported(cx: &mut TestAppContext) {
        let mut console = crate::backend::fake_agmend::console(Vec::new());
        console.listener = Some(
            serde_json::from_value(serde_json::json!({
                "identity_id": "idlisten", "handle": "andy/listen",
                "state": "running", "model": "claude-haiku-5-5",
                "today_usd": "0.40", "daily_usd": "2.00"
            }))
            .unwrap(),
        );
        use crate::backend::Backend as _;
        let fake = crate::backend::fake_agmend::FakeAgmend::start(console);
        let backend = crate::backend::AgmendBackend::connect(
            &fake.path(),
            crate::backend::fake_agmend::PROJECT,
            None,
        )
        .unwrap();
        assert_live_values(
            cx,
            backend.snapshot().listener,
            [
                "not reported",
                "not reported",
                "not reported",
                "not reported",
                "not reported of not reported · not reported of not reported running",
                "$0.40 of $2.00",
                "not reported in · not reported out · up not reported",
            ],
        );
    }

    #[gpui::test]
    fn live_caps_and_reported_zeroes_keep_their_values(cx: &mut TestAppContext) {
        use crate::backend::Backend as _;
        let mut console = crate::backend::fake_agmend::console(Vec::new());
        let entry = console.listener.as_mut().unwrap();
        entry.can_use.clear();
        entry.never.clear();
        entry.workers_today_usd = None;
        entry.workers_running = None;
        entry.tokens_in = Some(0);
        entry.tokens_out = Some(0);
        entry.uptime_minutes = Some(0);
        let fake = crate::backend::fake_agmend::FakeAgmend::start(console);
        let backend = crate::backend::AgmendBackend::connect(
            &fake.path(),
            crate::backend::fake_agmend::PROJECT,
            None,
        )
        .unwrap();
        assert_live_values(
            cx,
            backend.snapshot().listener,
            [
                "not reported",
                "not reported",
                "andy",
                "$5.00",
                "not reported of $20.00 · not reported of 3 running",
                "$0.40 of $2.00",
                "0 in · 0 out · up 0 min",
            ],
        );
    }

    fn assert_live_values(
        cx: &mut TestAppContext,
        listener: Option<ListenerInfo>,
        expected: [&str; 7],
    ) {
        let values = panel_values(listener.as_ref().unwrap().details.as_ref().unwrap());
        for ((text, color), expected) in values.iter().zip(expected) {
            assert_eq!(text, expected);
            if expected.contains("not reported") {
                assert_eq!(*color, Color::Placeholder);
                assert!(!text.contains("$0.00"));
            }
        }
        let cx = draw(cx, move || {
            div()
                .w(crate::ui::tokens::RIGHT_DOCK_WIDTH)
                .child(ListenerPanel::new(listener.clone()))
                .into_any_element()
        });
        for ((label, row), expected) in [
            ("Can use", 2),
            ("Never", 3),
            ("Takes work from", 0),
            ("Per worker", 1),
            ("Workers today", 2),
            ("Listener today", 3),
            ("Listener tokens", 4),
        ]
        .into_iter()
        .zip(expected)
        {
            let words = expected.split_whitespace().take(32).count();
            for index in 0..words {
                let selector = Box::leak(format!("kv-word:{label}:{row}:{index}").into_boxed_str());
                assert!(cx.debug_bounds(selector).is_some(), "{label}: {index}");
            }
            let selector = Box::leak(format!("kv-word:{label}:{row}:{words}").into_boxed_str());
            assert!(cx.debug_bounds(selector).is_none(), "extra word in {label}");
        }
    }
}
