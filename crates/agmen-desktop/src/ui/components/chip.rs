//! A small rounded label on a surface fill: the title bar's listener chip,
//! the composer's target chip and the panels' worktree chip.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _, Window, div,
};

use super::{Color, Icon, IconSize, Label, LabelSize};
use crate::appearance::Appearance;
use crate::icons::UiIcon;
use crate::ui::tokens::{DOT_SIZE, RADIUS_MD, SPACE_3, SPACE_6, SPACE_8};

#[derive(Debug, IntoElement)]
pub(crate) struct Chip {
    label: SharedString,
    color: Color,
    dot: Option<Color>,
    code: bool,
    trailing: Option<UiIcon>,
    full_width: bool,
}

impl Chip {
    #[must_use]
    pub(crate) fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            color: Color::Placeholder,
            dot: None,
            code: false,
            trailing: None,
            full_width: false,
        }
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the composer's target chip uses it in Task 9")
    )]
    #[must_use]
    pub(crate) fn color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    /// A round dot before the label.
    #[must_use]
    pub(crate) fn dot(mut self, color: Color) -> Self {
        self.dot = Some(color);
        self
    }

    #[must_use]
    pub(crate) const fn code(mut self) -> Self {
        self.code = true;
        self
    }

    /// A small icon at the right edge, as the worktree chip's chevron.
    #[must_use]
    pub(crate) const fn trailing(mut self, icon: UiIcon) -> Self {
        self.trailing = Some(icon);
        self
    }

    /// Fills its row, with the trailing icon at the far end.
    #[must_use]
    pub(crate) const fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }
}

impl RenderOnce for Chip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let label = Label::new(self.label)
            .size(LabelSize::Small)
            .color(self.color);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_6)
            .py(SPACE_3)
            .px(SPACE_8)
            .rounded(RADIUS_MD)
            .bg(theme.elevated_surface)
            .when_some(self.dot, |this, dot| {
                this.child(
                    div()
                        .size(DOT_SIZE)
                        .flex_none()
                        .rounded_full()
                        .bg(dot.hsla(theme)),
                )
            })
            .map(|this| {
                let label = if self.code { label.code() } else { label };
                if self.full_width {
                    // The label takes the room left and clips, so the
                    // trailing icon stays at the far end.
                    this.w_full()
                        .child(div().flex_1().min_w_0().overflow_hidden().child(label))
                } else {
                    this.child(label)
                }
            })
            .when_some(self.trailing, |this, icon| {
                this.child(
                    Icon::new(icon)
                        .size(IconSize::Small)
                        .color(Color::Placeholder),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::rendered_size;
    use gpui::{TestAppContext, px};

    #[gpui::test]
    fn chip_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            Chip::new("listener: codex · idle")
                .dot(Color::Placeholder)
                .into_any_element()
        });
        assert!(size.width > px(16.));
    }

    #[gpui::test]
    fn code_chip_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || {
            Chip::new("to andy/listen")
                .code()
                .color(Color::Accent)
                .into_any_element()
        });
        assert!(size.width > px(16.));
    }
}
