//! An agent's live session in a center tab (`desktop-agent.webp`): the
//! breadcrumb with Open in terminal and Stop, the turn's tool calls and
//! output, and a DM composer.

use gpui::{
    App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div,
};

use super::breadcrumb::Breadcrumb;
use super::components::{Button, ButtonStyle, Color, Icon, IconSize, Label, LabelSize};
use super::text_input::TextInput;
use super::tokens::{BORDER_WIDTH, RADIUS_MD, SPACE_5, SPACE_6, SPACE_8, SPACE_10, SPACE_14};
use crate::appearance::Appearance;
use crate::backend::{AgentSession, SessionRow, Tone};
use crate::icons::UiIcon;

/// Tests find row `ix` as `"session:{ix}:{kind}"`, where kind is `tool`,
/// `out`, `say` or `cost`, and the buttons as `agent-open-terminal` and
/// `agent-stop`.
pub(crate) struct AgentView {
    handle: SharedString,
    session: AgentSession,
    composer: Entity<TextInput>,
}

impl AgentView {
    pub(crate) fn new(
        handle: impl Into<SharedString>,
        session: AgentSession,
        cx: &mut Context<Self>,
    ) -> Self {
        let handle = handle.into();
        let placeholder = format!("Message {handle} · sent as a DM, recorded by the hub");
        let composer = cx.new(|cx| TextInput::new(cx).placeholder(placeholder));
        Self {
            handle,
            session,
            composer,
        }
    }
}

const fn kind(row: &SessionRow) -> &'static str {
    match row {
        SessionRow::Tool { .. } => "tool",
        SessionRow::Out { .. } => "out",
        SessionRow::Say(_) => "say",
        SessionRow::Cost(_) => "cost",
    }
}

const fn tone_color(tone: Tone) -> Color {
    match tone {
        Tone::Plain => Color::Default,
        Tone::Success => Color::Success,
        Tone::Accent => Color::Accent,
    }
}

fn session_row(row: &SessionRow, cx: &App) -> gpui::AnyElement {
    let theme = &cx.global::<Appearance>().theme;
    match row {
        // A folded tool call: chevron, tool name, argument.
        SessionRow::Tool { name, arg } => div()
            .flex()
            .items_center()
            .gap(SPACE_8)
            .py(SPACE_5)
            .px(SPACE_10)
            .rounded(RADIUS_MD)
            .border(BORDER_WIDTH)
            .border_color(theme.border)
            .bg(theme.elevated_surface)
            .child(
                Icon::new(UiIcon::ChevronRight)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .child(
                Label::new(name.clone())
                    .code()
                    .weight(FontWeight::SEMIBOLD)
                    .color(Color::Accent),
            )
            .child(Label::new(arg.clone()).code().color(Color::Placeholder))
            .into_any_element(),
        SessionRow::Out { text, tone } => div()
            .flex()
            .gap(SPACE_8)
            .pl(SPACE_14)
            .child(Label::new("⎿").code().color(Color::Muted))
            .child(
                Label::new(text.clone())
                    .code()
                    .color(tone_color(*tone))
                    .wrap(),
            )
            .into_any_element(),
        SessionRow::Say(text) => div()
            .child(Label::new(text.clone()).wrap())
            .into_any_element(),
        SessionRow::Cost(text) => div()
            .child(
                Label::new(text.clone())
                    .size(LabelSize::Small)
                    .color(Color::Placeholder),
            )
            .into_any_element(),
    }
}

impl Render for AgentView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let rows = self.session.rows.iter().enumerate().map(|(ix, row)| {
            let selector = format!("session:{ix}:{}", kind(row));
            div()
                .debug_selector(|| selector)
                .child(session_row(row, cx))
        });
        let buttons = div()
            .flex()
            .items_center()
            .gap(SPACE_6)
            // shortcut: the buttons do nothing until the hub backend can open
            // a terminal and stop an agent (Task 14).
            .child(
                Button::new("agent-open-terminal", "Open in terminal  ⌥O")
                    .style(ButtonStyle::Filled),
            )
            .child(Button::new("agent-stop", "Stop").style(ButtonStyle::Filled));
        div()
            .debug_selector(|| "agent-view".into())
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.editor_background)
            .child(
                Breadcrumb::new([
                    self.handle.clone(),
                    self.session.item.clone().into(),
                    self.session.branch.clone().into(),
                ])
                .end(buttons),
            )
            .child(
                div()
                    .id("agent-session")
                    .flex_1()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(SPACE_8)
                    .p(SPACE_14)
                    .children(rows),
            )
            .child(
                div().p(SPACE_14).child(
                    div()
                        .py(SPACE_8)
                        .px(SPACE_10)
                        .rounded(RADIUS_MD)
                        .border(BORDER_WIDTH)
                        .border_color(theme.border)
                        .bg(theme.elevated_surface)
                        .child(self.composer.clone()),
                ),
            )
    }
}

#[cfg(test)]
#[path = "agent_view_tests.rs"]
mod tests;
