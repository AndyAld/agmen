//! A small rounded label on a surface fill: the title bar's listener chip and
//! the composer's target and model chips.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, IntoElement, ParentElement as _, RenderOnce, SharedString, Styled as _,
    Window, div,
};

use super::{Color, Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::tokens::{
    BORDER_WIDTH, DOT_SIZE, RADIUS_MD, RADIUS_SM, SPACE_2, SPACE_3, SPACE_5, SPACE_6, SPACE_7,
    SPACE_8, TINT_BORDER, tint,
};

#[derive(IntoElement)]
pub(crate) struct Chip {
    label: SharedString,
    color: Color,
    dot: Option<Color>,
    code: bool,
    compact: bool,
    outline: Option<Color>,
    leading: Option<AnyElement>,
    trailing: Option<AnyElement>,
}

impl std::fmt::Debug for Chip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chip")
            .field("label", &self.label)
            .field("color", &self.color)
            .finish_non_exhaustive()
    }
}

impl Chip {
    #[must_use]
    pub(crate) fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            color: Color::Placeholder,
            dot: None,
            code: false,
            compact: false,
            outline: None,
            leading: None,
            trailing: None,
        }
    }

    /// The composer's chips (nodes 376 and 380): a selected fill, tighter
    /// padding and a smaller radius.
    #[must_use]
    pub(crate) const fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    /// A border in `color`, tinted, as the model chip's accent outline.
    #[must_use]
    pub(crate) fn outline(mut self, color: Color) -> Self {
        self.outline = Some(color);
        self
    }

    /// Something drawn before the label, such as "to" or a glyph.
    #[must_use]
    pub(crate) fn leading(mut self, leading: impl IntoElement) -> Self {
        self.leading = Some(leading.into_any_element());
        self
    }

    /// Something drawn after the label, such as a ▾.
    #[must_use]
    pub(crate) fn trailing(mut self, trailing: impl IntoElement) -> Self {
        self.trailing = Some(trailing.into_any_element());
        self
    }

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
}

impl RenderOnce for Chip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let label = Label::new(self.label)
            .size(LabelSize::Small)
            .color(self.color);
        let base = div().flex().flex_none().items_center();
        let styled = if self.compact {
            base.gap(SPACE_5)
                .py(SPACE_2)
                .px(SPACE_7)
                .rounded(RADIUS_SM)
                .bg(theme.element_selected)
        } else {
            base.gap(SPACE_6)
                .py(SPACE_3)
                .px(SPACE_8)
                .rounded(RADIUS_MD)
                .bg(theme.elevated_surface)
        };
        styled
            .when_some(self.outline, |this, outline| {
                this.border(BORDER_WIDTH)
                    .border_color(tint(outline.hsla(theme), TINT_BORDER))
            })
            .when_some(self.dot, |this, dot| {
                this.child(
                    div()
                        .size(DOT_SIZE)
                        .flex_none()
                        .rounded_full()
                        .bg(dot.hsla(theme)),
                )
            })
            .children(self.leading)
            .child(if self.code { label.code() } else { label })
            .children(self.trailing)
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

    #[gpui::test]
    fn compact_chip_is_shorter(cx: &mut TestAppContext) {
        let plain = rendered_size(cx, || Chip::new("default").into_any_element());
        let mut cx2 = TestAppContext::single();
        let compact = rendered_size(&mut cx2, || {
            Chip::new("default")
                .compact()
                .outline(Color::Accent)
                .leading(Label::new("◆").size(LabelSize::XSmall))
                .trailing(Label::new("▾").size(LabelSize::XSmall))
                .into_any_element()
        });
        // Compact pads 2 px where the plain chip pads 3, and the outline
        // adds 1 px on each side.
        assert_eq!(compact.height, plain.height);
        assert!(compact.width > plain.width);
    }
}
