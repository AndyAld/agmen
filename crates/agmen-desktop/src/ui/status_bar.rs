//! The status bar: panel buttons, counts, the listener, spend, the Listener
//! toggle and the right-side buttons.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::chat::AlertCounts;
use super::components::{Badge, Button, ButtonStyle, Color, IconButton, KeyHint, Label, LabelSize};
use super::format::{counted, counted_capped, spend_today, status_listener_label};
use super::left_dock::PanelButtons;
use super::tokens::{BORDER_WIDTH, SPACE_5, SPACE_10, SPACE_14, STATUS_BAR_HEIGHT};
use super::workspace::{OpenAlerts, ToggleRightDock};
use crate::backend::{Link, Snapshot, StatusSummary};
use crate::icons::UiIcon;
use crate::keymap::{OpenSettings, ToggleCommandPalette, ToggleTerminal};

#[derive(Debug, IntoElement)]
pub(crate) struct StatusBar {
    status: StatusSummary,
    /// The listener's state, or what the active tab is about.
    context: SharedString,
    /// Whether to offer the Listener toggle, which reopens the right dock.
    listener_toggle: bool,
    panels: PanelButtons,
    /// Task 18: while the palette is open its hint does nothing.
    palette_open: bool,
    /// Task 13: the chat's counts, not stored numbers (mockup fix 34).
    alerts: AlertCounts,
    /// Task 7: the live link's state and its color; `None` draws nothing.
    link: Option<(SharedString, Color)>,
}

impl StatusBar {
    #[must_use]
    pub(crate) fn new(snapshot: &Snapshot, listener_toggle: bool) -> Self {
        Self {
            status: snapshot.status.clone(),
            context: status_listener_label(snapshot.listener.as_ref()).into(),
            listener_toggle,
            panels: PanelButtons::default(),
            palette_open: false,
            alerts: AlertCounts::default(),
            link: None,
        }
    }

    /// Task 7: the link to agmend and the hub, as [`link_label`] words it.
    #[must_use]
    pub(crate) fn link(mut self, link: Option<(SharedString, Color)>) -> Self {
        self.link = link;
        self
    }

    /// The red needs-you pill and the alert count, from the chat's alert
    /// state (Task 13).
    #[must_use]
    pub(crate) const fn alert_counts(mut self, counts: AlertCounts) -> Self {
        self.alerts = counts;
        self
    }

    /// The files, git and search buttons and what they show as active.
    #[must_use]
    pub(crate) const fn panels(mut self, panels: PanelButtons) -> Self {
        self.panels = panels;
        self
    }

    /// Task 18: whether the palette is open. While it is, a click on the
    /// ⌘⇧P hint is the click outside that closes it, so the hint takes no
    /// click of its own and does not reopen it (C113).
    #[must_use]
    pub(crate) const fn palette_open(mut self, open: bool) -> Self {
        self.palette_open = open;
        self
    }

    /// Shows `context`, what the active tab is about
    /// (`Workspace::status_context`), in the listener's place.
    #[must_use]
    pub(crate) fn context(mut self, context: impl Into<SharedString>) -> Self {
        self.context = context.into();
        self
    }
}

/// Task 7: the status bar's words for the live backend's link. agmend and
/// the hub are up: connected. agmend is up and retries the hub: connecting
/// before the first success, reconnecting after. The hub or agmend refused
/// for a reason a retry will not fix, or agmend is gone: offline, with why.
#[must_use]
pub(crate) fn link_label(link: &Link, was_live: bool) -> (SharedString, Color) {
    match link {
        Link::Live => ("connected".into(), Color::Success),
        Link::HubDown { why: None } if was_live => ("reconnecting".into(), Color::Warning),
        Link::HubDown { why: None } => ("connecting".into(), Color::Warning),
        Link::HubDown { why: Some(why) } => (format!("offline: {why}").into(), Color::Error),
        // Final review fix 2: agmend answered, so its reason is the cause.
        Link::Refused { why } => (format!("offline: {why}").into(), Color::Error),
        Link::AgmendDown { why } => (
            format!("offline: can't reach agmend: {why}").into(),
            Color::Error,
        ),
    }
}

/// A glyph and its count, as "● 9 online".
fn item(glyph: &'static str, color: Color, label: String) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(SPACE_5)
        .child(Label::new(glyph).size(LabelSize::Small).color(color))
        .child(
            Label::new(label)
                .size(LabelSize::Small)
                .color(Color::Placeholder),
        )
}

/// The right-side buttons; the gear opens Settings.
fn buttons(icons: &[(&'static str, UiIcon)]) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(SPACE_10)
        .children(icons.iter().map(|(id, icon)| {
            let button = IconButton::new(*id, *icon);
            match icon {
                UiIcon::Terminal => button.on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(ToggleTerminal), cx);
                }),
                UiIcon::Bell => button.on_click(|_, window, cx| {
                    window.dispatch_action(Box::new(OpenAlerts), cx);
                }),
                // shortcut: open the chat tab here once a chat action exists.
                _ => button,
            }
        }))
        .child(
            IconButton::new("status-settings", UiIcon::Settings).on_click(|_, window, cx| {
                window.dispatch_action(Box::new(OpenSettings), cx);
            }),
        )
}

impl RenderOnce for StatusBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let s = &self.status;
        div()
            .debug_selector(|| "status-bar".into())
            .h(STATUS_BAR_HEIGHT)
            .border_t(BORDER_WIDTH)
            .border_color(super::rule_color(cx))
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_14)
            .px(SPACE_10)
            .child(self.panels)
            .when(self.alerts.needs_you > 0, |this| {
                this.child(
                    div().debug_selector(|| "needs-you-pill".into()).child(
                        Badge::new(
                            counted_capped(self.alerts.needs_you, "needs you", "need you"),
                            Color::Error,
                        )
                        .solid()
                        .leading("!"),
                    ),
                )
            })
            // Task 7: connected, reconnecting or offline, with the hub's
            // reason; the long reason gives way before the counts.
            .when_some(self.link, |this, (text, color)| {
                this.child(
                    div()
                        .debug_selector(|| "status-link".into())
                        .flex()
                        .min_w_0()
                        .items_center()
                        .gap(SPACE_5)
                        .child(Label::new("●").size(LabelSize::Small).color(color.clone()))
                        .child(
                            Label::new(text)
                                .size(LabelSize::Small)
                                .color(color)
                                .truncate(),
                        ),
                )
            })
            .child(item(
                "●",
                Color::Success,
                counted(s.online, "online", "online"),
            ))
            // Task 13: the count opens the chat at the alert (node 456).
            .child(
                div()
                    .id("status-alerts")
                    .debug_selector(|| "status-alerts".into())
                    .when(self.alerts.alerts > 0, |this| {
                        this.cursor_pointer().on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(OpenAlerts), cx);
                        })
                    })
                    .child(item(
                        "⚠",
                        Color::Warning,
                        counted_capped(self.alerts.alerts, "alert", "alerts"),
                    )),
            )
            .child(item(
                "⎇",
                Color::Placeholder,
                counted(s.worktrees, "worktree", "worktrees"),
            ))
            .child(div().flex_1())
            .child(
                div().debug_selector(|| "status-context".into()).child(
                    Label::new(self.context)
                        .size(LabelSize::Small)
                        .color(Color::Placeholder),
                ),
            )
            .child(
                Label::new(spend_today(s.spend_today_usd))
                    .size(LabelSize::Small)
                    .color(Color::Placeholder),
            )
            .when(self.listener_toggle, |this| {
                this.child(
                    Button::new("listener-toggle", "Listener")
                        .style(ButtonStyle::Filled)
                        .leading(Label::new("◨").size(LabelSize::Small).color(Color::Accent))
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(ToggleRightDock), cx);
                        }),
                )
            })
            .child(buttons(&[
                ("status-terminal", UiIcon::Terminal),
                ("status-chat", UiIcon::Chat),
                ("status-bell", UiIcon::Bell),
            ]))
            .child(palette_trigger(self.palette_open))
    }
}

/// Task 18: the ⌘⇧P hint, which opens the palette while it is closed.
fn palette_trigger(palette_open: bool) -> impl IntoElement {
    div()
        .id("palette-trigger")
        .debug_selector(|| "palette-trigger".into())
        .when(!palette_open, |this| {
            this.cursor_pointer().on_click(|_, window, cx| {
                window.dispatch_action(Box::new(ToggleCommandPalette), cx);
            })
        })
        .child(KeyHint::new(crate::keymap::palette_hint()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn status_bar_buttons_either_act_or_do_not_highlight(cx: &mut gpui::TestAppContext) {
        use crate::keymap::ToggleTerminal;
        use crate::ui::test_support::{click, draw, fills_at, redraw};
        use std::cell::Cell;
        use std::rc::Rc;

        let terminal = Rc::new(Cell::new(0));
        let alerts = Rc::new(Cell::new(0));
        let (terminal_seen, alerts_seen) = (terminal.clone(), alerts.clone());
        let focus = cx.update(|cx| cx.focus_handle());
        let tracked_focus = focus.clone();
        let cx = draw(cx, move || {
            let (terminal, alerts) = (terminal_seen.clone(), alerts_seen.clone());
            div()
                .track_focus(&tracked_focus)
                .on_action(move |_: &ToggleTerminal, _, _| terminal.set(terminal.get() + 1))
                .on_action(move |_: &OpenAlerts, _, _| alerts.set(alerts.get() + 1))
                .child(buttons(&[
                    ("status-terminal", UiIcon::Terminal),
                    ("status-chat", UiIcon::Chat),
                    ("status-bell", UiIcon::Bell),
                ]))
                .into_any_element()
        });
        cx.update(|window, cx| window.focus(&focus, cx));
        for (selector, action) in [
            ("status-terminal", Some(&terminal)),
            ("status-chat", None),
            ("status-bell", Some(&alerts)),
        ] {
            let before = fills_at(cx, selector);
            let center = cx.debug_bounds(selector).unwrap().center();
            cx.simulate_mouse_move(center, None, gpui::Modifiers::none());
            redraw(cx);
            click(cx, selector);
            if let Some(action) = action {
                assert_eq!(action.get(), 1, "{selector} must dispatch its action");
            } else {
                assert_eq!(
                    fills_at(cx, selector),
                    before,
                    "{selector} must not highlight"
                );
            }
        }
    }

    #[test]
    fn link_label_words_each_state() {
        let hub = |why: Option<&str>| Link::HubDown {
            why: why.map(str::to_owned),
        };
        let text = |link: &Link, was_live| link_label(link, was_live).0.to_string();
        assert_eq!(text(&Link::Live, false), "connected");
        // The first connect is not a reconnect.
        assert_eq!(text(&hub(None), false), "connecting");
        assert_eq!(text(&hub(None), true), "reconnecting");
        assert_eq!(
            text(&hub(Some("signed out: run agmen login")), true),
            "offline: signed out: run agmen login"
        );
        // Final review fix 2: each cause shows its reason.
        let down = Link::AgmendDown {
            why: "nothing answers at its socket".into(),
        };
        assert_eq!(
            text(&down, true),
            "offline: can't reach agmend: nothing answers at its socket"
        );
        let refused = Link::Refused {
            why: "this console fell 3 events behind; take a new snapshot and watch again".into(),
        };
        assert_eq!(
            text(&refused, true),
            "offline: this console fell 3 events behind; take a new snapshot and watch again"
        );
        assert_eq!(link_label(&refused, true).1, Color::Error);
        assert_eq!(link_label(&down, true).1, Color::Error);
        assert_eq!(link_label(&hub(None), true).1, Color::Warning);
        assert_eq!(link_label(&Link::Live, true).1, Color::Success);
    }

    #[test]
    fn context_names_the_listener_without_a_colon() {
        // Task 20 shot diff (ask, palette, needs-you-answered): the status
        // bar reads "listener codex · idle" (console.py:815).
        let bar = StatusBar::new(&crate::backend::chat_snapshot(), false);
        assert_eq!(bar.context.as_ref(), "listener codex · idle");
    }
}
