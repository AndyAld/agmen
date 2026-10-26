//! An agent's live session in a center tab (`desktop-agent.webp`): the
//! breadcrumb with Open in terminal and Stop, the turn's tool calls and
//! output, and a DM composer.

use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::breadcrumb::Breadcrumb;
use super::components::{Button, ButtonStyle, Color, Icon, IconSize, Label, LabelSize, ToolCard};
use super::text_input::TextInput;
use super::tokens::{
    BORDER_WIDTH, RADIUS_CARD, SESSION_COST_LINE, SESSION_OUT_LINE, SESSION_SAY_LINE, SPACE_8,
    SPACE_10, SPACE_12, SPACE_14, SPACE_18,
};
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
        Tone::Plain => Color::Placeholder,
        Tone::Success => Color::Success,
        Tone::Accent => Color::Accent,
    }
}

/// An output line's "⎿" and text colors: both the line's tone, as
/// console.py draws `"   ⎿ " + text` as one text (rows 637 and 648).
const fn out_colors(tone: Tone) -> (Color, Color) {
    (tone_color(tone), tone_color(tone))
}

fn session_row(row: &SessionRow) -> gpui::AnyElement {
    match row {
        SessionRow::Tool { name, arg } => {
            ToolCard::new(name.clone(), arg.clone()).into_any_element()
        }
        SessionRow::Out { text, tone } => {
            let (glyph, text_color) = out_colors(*tone);
            div()
                .flex()
                .gap(SPACE_8)
                .pl(SPACE_14)
                .child(
                    Label::new("⎿")
                        .code()
                        .color(glyph)
                        .line_height(SESSION_OUT_LINE),
                )
                .child(
                    Label::new(text.clone())
                        .code()
                        .color(text_color)
                        .line_height(SESSION_OUT_LINE)
                        .wrap(),
                )
                .into_any_element()
        }
        SessionRow::Say(text) => div()
            .child(
                Label::new(text.clone())
                    .line_height(SESSION_SAY_LINE)
                    .wrap(),
            )
            .into_any_element(),
        SessionRow::Cost(text) => div()
            .child(
                Label::new(text.clone())
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .line_height(SESSION_COST_LINE),
            )
            .into_any_element(),
    }
}

impl Render for AgentView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let rows = self.session.rows.iter().enumerate().map(|(ix, row)| {
            let selector = format!("session:{ix}:{}", kind(row));
            div().debug_selector(|| selector).child(session_row(row))
        });
        let buttons = div()
            .flex()
            .items_center()
            .gap(SPACE_8)
            // shortcut: the buttons do nothing until the hub backend can open
            // a terminal and stop an agent (Task 14).
            .child(
                Button::new("agent-open-terminal", "Open in terminal  ⌥O")
                    .style(ButtonStyle::Tinted),
            )
            .child(Button::new("agent-stop", "Stop").style(ButtonStyle::Plain));
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
                    .pt(SPACE_14)
                    .px(SPACE_18)
                    .pb(SPACE_10)
                    .children(rows),
            )
            .child(
                // shortcut: a box of its own until Task 9's Composer component
                // can host a DM composer.
                div().pt(SPACE_8).px(SPACE_14).pb(SPACE_12).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(SPACE_8)
                        .py(SPACE_8)
                        .px(SPACE_10)
                        .rounded(RADIUS_CARD)
                        .border(BORDER_WIDTH)
                        .border_color(theme.border)
                        .bg(theme.elevated_surface)
                        .child(div().flex_1().min_w_0().child(self.composer.clone()))
                        .child(
                            Icon::new(UiIcon::Enter)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        ),
                ),
            )
    }
}

#[cfg(test)]
#[path = "agent_view_tests.rs"]
mod tests;
