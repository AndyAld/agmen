//! A dialog's actions: Cancel, secondary actions, then the main action.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    Styled as _, Window, div,
};

use super::{Button, ButtonSize, ButtonStyle};
use crate::ui::tokens::SPACE_8;

#[derive(Debug, Default, IntoElement)]
pub(crate) struct DialogFooter {
    cancel: Option<Button>,
    secondary: Vec<Button>,
    primary: Option<Button>,
    destructive: bool,
    selector: Option<SharedString>,
}

impl DialogFooter {
    #[must_use]
    pub(crate) fn selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.selector = Some(selector.into());
        self
    }

    #[must_use]
    pub(crate) fn new(primary: Button) -> Self {
        Self {
            primary: Some(primary),
            ..Self::default()
        }
    }

    /// An informational dialog can have just a Close action.
    #[must_use]
    pub(crate) fn close(cancel: Button) -> Self {
        Self::default().cancel(cancel)
    }

    #[must_use]
    pub(crate) fn cancel(mut self, button: Button) -> Self {
        self.cancel = Some(button);
        self
    }

    /// Secondary actions keep the style their caller chose.
    #[must_use]
    pub(crate) fn secondary(mut self, button: Button) -> Self {
        self.secondary.push(button);
        self
    }

    #[must_use]
    pub(crate) const fn destructive(mut self, destructive: bool) -> Self {
        self.destructive = destructive;
        self
    }
}

impl RenderOnce for DialogFooter {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .when_some(self.selector, |this, selector| {
                this.debug_selector(move || selector.to_string())
            })
            .w_full()
            .flex()
            .justify_end()
            .items_center()
            .gap(SPACE_8)
            .children(
                self.cancel
                    .map(|button| button.style(ButtonStyle::Inset).size(ButtonSize::Large)),
            )
            .children(
                self.secondary
                    .into_iter()
                    .map(|button| button.size(ButtonSize::Large)),
            )
            .children(self.primary.map(|button| {
                button
                    .style(if self.destructive {
                        ButtonStyle::Danger
                    } else {
                        ButtonStyle::Primary
                    })
                    .size(ButtonSize::Large)
            }))
    }
}
