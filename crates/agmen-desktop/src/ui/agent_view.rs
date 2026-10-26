//! An agent's live session in a center tab (`desktop-agent.webp`): the
//! breadcrumb with Open in terminal and Stop, the turn's tool calls and
//! output, and the chat's composer as a DM to the agent.

use agmen_proto::IdentityId;
use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::components::{Breadcrumb, Button, ButtonStyle, Color, Label, LabelSize, ToolCard};
use super::composer::{Composer, Target};
use super::tokens::{SPACE_6, SPACE_8, SPACE_10, SPACE_14, SPACE_18};
use crate::appearance::Appearance;
use crate::backend::{AgentSession, BackendError, BackendHandle, SessionRow, Snapshot, Tone};
use crate::keymap::{OpenInTerminal, TakeOverAgent};

/// Tests find row `ix` as `"session:{ix}:{kind}"`, where kind is `tool`,
/// `out`, `say` or `cost`, and the buttons as `agent-open-terminal` and
/// `agent-stop`.
pub(crate) struct AgentView {
    /// Task 14: the agent Open in terminal takes over.
    id: IdentityId,
    handle: SharedString,
    session: AgentSession,
    composer: Entity<Composer>,
}

impl AgentView {
    /// Agent `id`'s session, with a composer that DMs it through the
    /// [`BackendHandle`] global. An agent the roster does not have shows
    /// its id as its handle.
    pub(crate) fn new(
        snapshot: &Snapshot,
        id: &IdentityId,
        session: AgentSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let handle = snapshot
            .roster
            .iter()
            .flat_map(|group| &group.agents)
            .find(|agent| agent.id == *id)
            .map_or_else(|| id.to_string(), |agent| agent.handle.clone());
        let agent = Target {
            id: id.clone(),
            handle: handle.clone(),
        };
        // A refused DM comes back into the input.
        let composer = cx.new(|cx| {
            Composer::direct(
                snapshot,
                agent,
                |draft, _, cx| match cx.try_global::<BackendHandle>() {
                    Some(backend) => backend.0.send(draft),
                    None => Err(BackendError::Unsupported("sending without a backend")),
                },
                window,
                cx,
            )
        });
        Self {
            id: id.clone(),
            handle: handle.into(),
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
                .child(Label::new("⎿").code().color(glyph))
                .child(Label::new(text.clone()).code().color(text_color).wrap())
                .into_any_element()
        }
        SessionRow::Say(text) => div()
            .child(Label::new(text.clone()).wrap())
            .into_any_element(),
        SessionRow::Cost(text) => div()
            .child(
                Label::new(text.clone())
                    .size(LabelSize::Small)
                    .color(Color::Muted),
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
            .gap(SPACE_6)
            // Task 14: the terminal opens in the chat (spec 5.4).
            .child({
                let id = self.id.clone();
                Button::new("agent-open-terminal", "Open in terminal  ⌥O")
                    .style(ButtonStyle::Tinted)
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(Box::new(TakeOverAgent(id.clone())), cx);
                    })
            })
            // shortcut: Stop does nothing until the hub backend can stop an
            // agent.
            .child(Button::new("agent-stop", "Stop").style(ButtonStyle::Plain));
        let id = self.id.clone();
        div()
            .debug_selector(|| "agent-view".into())
            // Task 14: ⌥O here is this view's button, so it takes over this
            // view's agent, not the agent of the chat's newest DONE; the
            // action stops here.
            .on_action(move |_: &OpenInTerminal, window, cx| {
                window.dispatch_action(Box::new(TakeOverAgent(id.clone())), cx);
            })
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
            .child(self.composer.clone())
    }
}

#[cfg(test)]
#[path = "agent_view_tests.rs"]
mod tests;
