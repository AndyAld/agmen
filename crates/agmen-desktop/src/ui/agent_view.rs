//! An agent's live session in a center tab (`desktop-agent.webp`): the
//! breadcrumb with Open in terminal and Stop, the turn's tool calls and
//! output, and the chat's composer as a DM to the agent.

use agmen_proto::IdentityId;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::chat::refusal;
use super::code_view::cell_width;
use super::components::{Breadcrumb, Button, ButtonStyle, Color, Label, LabelSize, ToolCard};
use super::composer::{Composer, Target};
use super::tokens::{
    AGENT_COST_LINE, AGENT_OUT_LINE, AGENT_SAY_LINE, SPACE_8, SPACE_10, SPACE_14, SPACE_18,
};
use crate::appearance::Appearance;
use crate::backend::{
    AgentSession, BackendError, BackendHandle, Draft, SessionRow, Snapshot, Tone,
};
use crate::keymap::{OpenInTerminal, TakeOverAgent};

/// Tests find row `ix` as `"session:{ix}:{kind}"`, where kind is `tool`,
/// `out`, `say` or `cost`, the buttons as `agent-open-terminal` and
/// `agent-stop`, and a refused DM as `"agent:error"`.
pub(crate) struct AgentView {
    /// Task 14: the agent Open in terminal takes over.
    id: IdentityId,
    handle: SharedString,
    session: AgentSession,
    composer: Entity<Composer>,
    /// Why the last DM was refused, until the next one is sent (C234).
    error: Option<SharedString>,
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
        // A refused DM comes back into the input and says why above it.
        let view = cx.entity().downgrade();
        let composer = cx.new(|cx| {
            Composer::direct(
                snapshot,
                agent,
                move |draft, _, cx| {
                    view.update(cx, |view, cx| view.send(draft, cx))
                        // The view closes with its window before its
                        // composer; a draft sent in between is refused.
                        .unwrap_or(Err(BackendError::Unsupported(
                            "sending after the agent view closed",
                        )))
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
            error: None,
        }
    }

    /// Sends a DM through the backend; a refusal shows above the composer
    /// until the next send succeeds.
    fn send(&mut self, draft: Draft, cx: &mut Context<Self>) -> Result<(), BackendError> {
        let result = BackendHandle::send(draft, cx);
        self.error = result.as_ref().err().map(|err| err.to_string().into());
        cx.notify();
        result
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

/// Row `ix`; `cell` is the code font's cell, which output lines indent by.
fn session_row(ix: usize, row: &SessionRow, cell: Pixels) -> gpui::AnyElement {
    match row {
        SessionRow::Tool { name, arg } => {
            ToolCard::new(name.clone(), arg.clone()).into_any_element()
        }
        SessionRow::Out { text, tone } => {
            let (glyph, text_color) = out_colors(*tone);
            // console.py draws "   ⎿ text": three cells before the ⎿.
            div()
                .flex()
                .gap(SPACE_8)
                .pl(cell * 3.)
                .child(
                    div()
                        .debug_selector(move || format!("session:{ix}:glyph"))
                        .child(
                            Label::new("⎿")
                                .code()
                                .color(glyph)
                                .line_height(AGENT_OUT_LINE),
                        ),
                )
                .child(
                    Label::new(text.clone())
                        .code()
                        .color(text_color)
                        .line_height(AGENT_OUT_LINE)
                        .wrap(),
                )
                .into_any_element()
        }
        SessionRow::Say(text) => div()
            .child(Label::new(text.clone()).line_height(AGENT_SAY_LINE).wrap())
            .into_any_element(),
        SessionRow::Cost(text) => div()
            .child(
                Label::new(text.clone())
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .line_height(AGENT_COST_LINE),
            )
            .into_any_element(),
    }
}

impl Render for AgentView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = cx.global::<Appearance>();
        let theme = &appearance.theme;
        let cell = cell_width(window, &appearance.fonts);
        let rows = self.session.rows.iter().enumerate().map(|(ix, row)| {
            let selector = format!("session:{ix}:{}", kind(row));
            div()
                .debug_selector(|| selector)
                .child(session_row(ix, row, cell))
        });
        let buttons = div()
            .flex()
            .items_center()
            .gap(SPACE_8)
            // Task 14: the terminal opens in the chat (spec 5.4).
            .child({
                let id = self.id.clone();
                // The key's glyphs come from src/keymap.rs, its one home.
                let label = format!(
                    "Open in terminal  {}",
                    crate::keymap::open_in_terminal_hint()
                );
                Button::new("agent-open-terminal", label)
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
            .when_some(self.error.clone(), |this, error| {
                this.child(refusal("agent:error", error))
            })
            .child(self.composer.clone())
    }
}

#[cfg(test)]
#[path = "agent_view_tests.rs"]
mod tests;
