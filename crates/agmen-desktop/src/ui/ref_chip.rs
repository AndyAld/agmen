//! Reference chips in message text and their hover cards (spec 9.3; mockup
//! `rich_body` and `ref_color` in `docs/mockups/scripts/console.py`).
//!
//! [`RefChip`] draws one reference; a click reports [`RefEvent::OpenRef`], a
//! ⌘-click [`RefEvent::OpenRefInSplit`], and hovering shows the reference's
//! card from the [`ChatContext`].

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::chat_context::{ChatContext, RefCard};
use super::components::{Card, ChangeHandler, Chip, Color, KvRow, Label, LabelSize};
use super::tokens::{REF_CARD_WIDTH, SPACE_6};
use crate::refs::RefTarget;

/// The debug selector of a drawn hover card, for tests.
#[cfg(test)]
pub(crate) const REF_CARD_SELECTOR: &str = "ref-card";

/// What a reference chip asks its caller to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RefEvent {
    /// Open the reference in a tab (a click).
    OpenRef(RefTarget),
    /// Open the reference in a split beside the current pane (a ⌘-click).
    OpenRefInSplit(RefTarget),
}

/// Whether the chip has a tinted fill (agents, people, roles, branches,
/// items, lanes) or is plain underlined text (files, commits, PRs), as
/// `rich_body` draws them.
#[must_use]
pub(crate) const fn filled(target: &RefTarget) -> bool {
    !matches!(
        target,
        RefTarget::Commit(_) | RefTarget::File { .. } | RefTarget::Pr(_)
    )
}

/// The hover card's view: GPUI tooltips are views.
struct RefCardView {
    card: RefCard,
}

impl Render for RefCardView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let card = &self.card;
        div().debug_selector(|| "ref-card".into()).child(
            Card::new()
                .w(REF_CARD_WIDTH)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(SPACE_6)
                        .child(
                            Label::new(card.kind)
                                .size(LabelSize::Small)
                                .color(Color::Placeholder),
                        )
                        .child(
                            Label::new(card.title.clone())
                                .code()
                                .color(card.color.clone()),
                        ),
                )
                .children(
                    card.rows
                        .iter()
                        .map(|(label, value)| KvRow::new(*label, value.clone())),
                ),
        )
    }
}

/// One reference in message text: a chip in the reference's color.
#[derive(IntoElement)]
pub(crate) struct RefChip {
    id: ElementId,
    text: SharedString,
    target: RefTarget,
    color: Color,
    flush_start: bool,
    flush_end: bool,
    context: Option<Rc<ChatContext>>,
    on_ref: Option<ChangeHandler<RefEvent>>,
}

impl std::fmt::Debug for RefChip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefChip")
            .field("id", &self.id)
            .field("text", &self.text)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl RefChip {
    /// `text` as the message writes it; `color` from
    /// [`ChatContext::chip_color`]. Tests find the chip by `id`.
    #[must_use]
    pub(crate) fn new(
        id: impl Into<ElementId>,
        text: impl Into<SharedString>,
        target: RefTarget,
        color: Color,
    ) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            target,
            color,
            flush_start: false,
            flush_end: false,
            context: None,
            on_ref: None,
        }
    }

    /// No padding before the text, because punctuation touches it there
    /// (mockup fix 54).
    #[must_use]
    pub(crate) const fn flush_start(mut self) -> Self {
        self.flush_start = true;
        self
    }

    /// No padding after the text, because punctuation follows it (mockup
    /// fix 54).
    #[must_use]
    pub(crate) const fn flush_end(mut self) -> Self {
        self.flush_end = true;
        self
    }

    /// Shows the reference's card on hover, built from `context` when the
    /// pointer arrives.
    #[must_use]
    pub(crate) fn card(mut self, context: Rc<ChatContext>) -> Self {
        self.context = Some(context);
        self
    }

    /// Called with [`RefEvent::OpenRef`] on a click and
    /// [`RefEvent::OpenRefInSplit`] on a ⌘-click (Ctrl elsewhere).
    #[must_use]
    pub(crate) fn on_ref(
        mut self,
        handler: impl Fn(RefEvent, &mut Window, &mut gpui::App) + 'static,
    ) -> Self {
        self.on_ref = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for RefChip {
    fn render(self, _window: &mut Window, _cx: &mut gpui::App) -> impl IntoElement {
        let chip = Chip::new(self.text).reference().code().shrink();
        let chip = if filled(&self.target) {
            chip.tinted(self.color)
        } else {
            chip.color(self.color).underline()
        };
        let chip = chip
            .when(self.flush_start, Chip::flush_start)
            .when(self.flush_end, Chip::flush_end);
        let selector = self.id.to_string();
        let target = self.target;
        let hover_target = target.clone();
        // min_w_0 lets a reference wider than the column shrink to it.
        div()
            .id(self.id)
            .debug_selector(|| selector)
            .flex()
            .min_w_0()
            .when_some(self.on_ref, |this, handler| {
                this.cursor_pointer().on_click(move |event, window, cx| {
                    let target = target.clone();
                    let event = if event.modifiers().secondary(cx) {
                        RefEvent::OpenRefInSplit(target)
                    } else {
                        RefEvent::OpenRef(target)
                    };
                    handler(event, window, cx);
                })
            })
            .when_some(self.context, |this, context| {
                this.hoverable_tooltip(move |_window, cx| {
                    let card = context.card(&hover_target);
                    cx.new(|_| RefCardView { card }).into()
                })
            })
            .child(chip)
    }
}

#[cfg(test)]
#[path = "ref_chip_tests.rs"]
mod tests;
