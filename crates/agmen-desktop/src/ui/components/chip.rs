//! A small rounded label: the title bar's listener chip, the composer's
//! target and model chips, a message's "Open in terminal" chip, the
//! reference chips in message text and the panels' worktree chip.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, FontWeight, IntoElement, ParentElement as _, Pixels, RenderOnce, SharedString,
    Styled as _, Window, div,
};

use super::{Color, Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::tokens::{
    BORDER_WIDTH, DOT_SIZE, RADIUS_MD, RADIUS_REF, RADIUS_SM, SPACE_1, SPACE_2, SPACE_3, SPACE_4,
    SPACE_5, SPACE_6, SPACE_7, SPACE_8, TINT_BORDER, TINT_FILL, tint,
};

/// The chip's padding, radius and default fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ChipShape {
    /// The title bar's chip: surface fill, 3 by 8 padding.
    #[default]
    Default,
    /// The composer's chips (nodes 376 and 380): selected fill, 2 by 7.
    Compact,
    /// A chip in a message's head row (node 300): no fill of its own, 1 by 7.
    Inline,
    /// A note in a banner (node 5709): selected fill, 1 by 7, a 4 px gap.
    Note,
    /// A reference in message text (`rich_body`): no fill of its own, 4 px
    /// at the sides only, a small radius.
    Reference,
    /// A chip that opens a menu, as the take-over terminal's agent chip
    /// (node 1134): selected fill, a border, 2 by 8.
    Picker,
}

#[derive(IntoElement)]
pub(crate) struct Chip {
    label: SharedString,
    color: Color,
    dot: Option<Color>,
    code: bool,
    shape: ChipShape,
    tint: Option<Color>,
    outline: Option<Color>,
    leading: Option<AnyElement>,
    trailing: Option<AnyElement>,
    flush_start: bool,
    flush_end: bool,
    underline: bool,
    shrink: bool,
    full_width: bool,
    weight: Option<FontWeight>,
    line_height: Option<Pixels>,
}

impl std::fmt::Debug for Chip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chip")
            .field("label", &self.label)
            .field("color", &self.color)
            .field("shape", &self.shape)
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
            shape: ChipShape::Default,
            tint: None,
            outline: None,
            leading: None,
            trailing: None,
            flush_start: false,
            flush_end: false,
            underline: false,
            shrink: false,
            full_width: false,
            weight: None,
            line_height: None,
        }
    }

    /// The label's weight, as the take-over terminal's bold agent chip
    /// (node 1134).
    #[must_use]
    pub(crate) const fn weight(mut self, weight: FontWeight) -> Self {
        self.weight = Some(weight);
        self
    }

    /// The composer's chips (nodes 376 and 380): a selected fill, tighter
    /// padding and a smaller radius.
    #[must_use]
    pub(crate) const fn compact(mut self) -> Self {
        self.shape = ChipShape::Compact;
        self
    }

    /// A note in a banner, as "♪ chime played" (node 5709): a selected
    /// fill, 1 by 7 padding and a 4 px gap.
    #[must_use]
    pub(crate) const fn note(mut self) -> Self {
        self.shape = ChipShape::Note;
        self
    }

    /// A chip that opens a menu, as the take-over terminal's agent chip
    /// (node 1134): a selected fill inside a border, 2 by 8 padding.
    #[must_use]
    pub(crate) const fn picker(mut self) -> Self {
        self.shape = ChipShape::Picker;
        self
    }

    /// A chip in a line of text, such as a message's head row (node 300):
    /// 1 by 7 padding and no fill unless tinted.
    #[must_use]
    pub(crate) const fn inline(mut self) -> Self {
        self.shape = ChipShape::Inline;
        self
    }

    /// A reference in message text (`rich_body` in console.py): 4 px at
    /// the sides, a 3 px radius, and no fill unless tinted.
    #[must_use]
    pub(crate) const fn reference(mut self) -> Self {
        self.shape = ChipShape::Reference;
        self
    }

    /// Fills the chip with `color` at the mockup's tint and sets the label
    /// in it, as a verb badge does.
    #[must_use]
    pub(crate) fn tinted(mut self, color: Color) -> Self {
        self.color = color.clone();
        self.tint = Some(color);
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

    /// No padding before the label, because punctuation touches it there
    /// (mockup fix 54).
    #[must_use]
    pub(crate) const fn flush_start(mut self) -> Self {
        self.flush_start = true;
        self
    }

    /// No padding after the label, because punctuation follows it (mockup
    /// fix 54).
    #[must_use]
    pub(crate) const fn flush_end(mut self) -> Self {
        self.flush_end = true;
        self
    }

    /// Underlines the label, as a reference chip with no fill.
    #[must_use]
    pub(crate) const fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    /// Lets the chip shrink to the space it has and wrap its label there,
    /// so a long reference stays inside a narrow column.
    #[must_use]
    pub(crate) const fn shrink(mut self) -> Self {
        self.shrink = true;
        self
    }

    /// The label's line height, so a chip in a line of prose is no taller
    /// than the line (node 2440).
    #[must_use]
    pub(crate) const fn line_height(mut self, height: Pixels) -> Self {
        self.line_height = Some(height);
        self
    }

    /// Fills its row, with the trailing element at the far end.
    #[must_use]
    pub(crate) const fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }
}

impl RenderOnce for Chip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let label = Label::new(self.label).color(self.color);
        let label = if self.code {
            label.code()
        } else {
            label.size(LabelSize::Small)
        };
        let label = if self.underline {
            label.underline()
        } else {
            label
        };
        let label = if self.shrink { label.wrap() } else { label };
        let label = match self.weight {
            Some(weight) => label.weight(weight),
            None => label,
        };
        let label = match self.line_height {
            Some(height) => label.line_height(height),
            None => label,
        };
        // A reference sits in a line of text, so it pads only its sides.
        let (py, px, radius, gap) = match self.shape {
            ChipShape::Default => (Some(SPACE_3), SPACE_8, RADIUS_MD, SPACE_6),
            ChipShape::Compact => (Some(SPACE_2), SPACE_7, RADIUS_SM, SPACE_5),
            ChipShape::Inline => (Some(SPACE_1), SPACE_7, RADIUS_SM, SPACE_5),
            ChipShape::Note => (Some(SPACE_1), SPACE_7, RADIUS_SM, SPACE_4),
            ChipShape::Reference => (None, SPACE_4, RADIUS_REF, SPACE_4),
            ChipShape::Picker => (Some(SPACE_2), SPACE_8, RADIUS_SM, SPACE_6),
        };
        let fill = match (&self.tint, self.shape) {
            (Some(color), _) => Some(tint(color.hsla(theme), TINT_FILL)),
            (None, ChipShape::Default) => Some(theme.elevated_surface),
            (None, ChipShape::Compact | ChipShape::Note | ChipShape::Picker) => {
                Some(theme.element_selected)
            }
            (None, ChipShape::Inline | ChipShape::Reference) => None,
        };
        div()
            .flex()
            .items_center()
            .gap(gap)
            .when_some(py, gpui::Styled::py)
            .when(!self.flush_start, |this| this.pl(px))
            .when(!self.flush_end, |this| this.pr(px))
            .rounded(radius)
            .map(|this| {
                if self.shrink {
                    this.min_w_0()
                } else {
                    this.flex_none()
                }
            })
            .when_some(fill, gpui::Styled::bg)
            .when(self.shape == ChipShape::Picker, |this| {
                this.border(BORDER_WIDTH).border_color(theme.border)
            })
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
            .map(|this| {
                if self.full_width {
                    // The label takes the room left and clips, so the
                    // trailing element stays at the far end.
                    this.w_full()
                        .child(div().flex_1().min_w_0().overflow_hidden().child(label))
                } else {
                    this.child(label)
                }
            })
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

    use gpui::InteractiveElement as _;

    /// A 300 px row holding the chip and a marker right after it.
    struct Row(bool);

    impl gpui::Render for Row {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let chip = Chip::new("fx1 · main").trailing(
                crate::ui::components::Icon::new(crate::icons::UiIcon::ChevronDown)
                    .size(crate::ui::components::IconSize::Small),
            );
            gpui::div()
                .debug_selector(|| "row".into())
                .w(px(300.))
                .flex()
                .child(if self.0 { chip.full_width() } else { chip })
                .child(
                    gpui::div()
                        .debug_selector(|| "after".into())
                        .w(px(1.))
                        .h(px(1.)),
                )
        }
    }

    fn marker_offset(cx: &mut TestAppContext, full: bool) -> gpui::Pixels {
        crate::ui::test_support::init(cx);
        let (_, cx) = cx.add_window_view(|_, _| Row(full));
        crate::ui::test_support::redraw(cx);
        let row = cx.debug_bounds("row").unwrap();
        cx.debug_bounds("after").unwrap().left() - row.left()
    }

    #[gpui::test]
    fn full_width_chip_fills_its_row(cx: &mut TestAppContext) {
        // Review C190.
        let plain = marker_offset(cx, false);
        let full = marker_offset(cx, true);
        assert!(plain < px(150.), "{plain:?}");
        assert!(full > px(250.), "{full:?}");
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

    #[gpui::test]
    fn inline_chip_pads_seven_by_one_inside_its_outline(cx: &mut TestAppContext) {
        // Node 300: padding [1, 7, 1, 7] and a 1 px tinted stroke.
        let chip = rendered_size(cx, || {
            Chip::new("Open in terminal")
                .inline()
                .tinted(Color::Accent)
                .outline(Color::Accent)
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let text = rendered_size(&mut cx2, || {
            Label::new("Open in terminal")
                .size(LabelSize::Small)
                .into_any_element()
        });
        assert_eq!(chip.width - text.width, px(16.));
        assert_eq!(chip.height - text.height, px(4.));
    }

    #[gpui::test]
    fn note_chip_pads_seven_by_one_with_a_four_px_gap(cx: &mut TestAppContext) {
        // Node 5709, "♪ chime played": 93 by 15, padding [1, 7], gap 4.
        let chip = rendered_size(cx, || {
            Chip::new("chime played")
                .note()
                .leading(Label::new("♪").size(LabelSize::Small))
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let text = rendered_size(&mut cx2, || {
            div()
                .flex()
                .child(Label::new("♪").size(LabelSize::Small))
                .child(Label::new("chime played").size(LabelSize::Small))
                .into_any_element()
        });
        assert_eq!(chip.width - text.width, px(14.) + SPACE_4);
        assert_eq!(chip.height - text.height, px(2.));
    }

    #[gpui::test]
    fn reference_chip_pads_its_sides_unless_flush(cx: &mut TestAppContext) {
        let make = |flush: bool| {
            move || {
                let chip = Chip::new("4d9fdc8").reference().code().underline();
                if flush { chip.flush_end() } else { chip }.into_any_element()
            }
        };
        let padded = rendered_size(cx, make(false));
        let mut cx2 = TestAppContext::single();
        let flush = rendered_size(&mut cx2, make(true));
        let mut cx3 = TestAppContext::single();
        let text = rendered_size(&mut cx3, || Label::new("4d9fdc8").code().into_any_element());
        assert_eq!(padded.width - flush.width, SPACE_4);
        assert_eq!(padded.width - text.width, SPACE_4 * 2.);
        assert_eq!(padded.height, text.height);
    }

    // Task 14 review: the take-over terminal's agent chip (node 1134) pads
    // 2 by 8 inside a 1 px border, with 6 between its parts.
    #[gpui::test]
    fn picker_chip_pads_two_by_eight_inside_a_border(cx: &mut TestAppContext) {
        let chip = rendered_size(cx, || {
            Chip::new("claude")
                .picker()
                .code()
                .trailing(Label::new("▾").size(LabelSize::XSmall))
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let parts = rendered_size(&mut cx2, || {
            div()
                .flex()
                .items_center()
                .child(Label::new("claude").code())
                .child(Label::new("▾").size(LabelSize::XSmall))
                .into_any_element()
        });
        assert_eq!(chip.width - parts.width, SPACE_8 * 2. + SPACE_6 + px(2.));
        assert_eq!(chip.height - parts.height, SPACE_2 * 2. + px(2.));
    }

    #[gpui::test]
    fn shrinking_chip_wraps_in_a_narrow_column(cx: &mut TestAppContext) {
        let make = |shrink: bool| {
            move || {
                let chip = Chip::new("crates/agmen-desktop/src/backend/fixture_data.rs:239")
                    .reference()
                    .code()
                    .tinted(Color::Accent);
                div()
                    .w(px(100.))
                    .flex()
                    .child(if shrink { chip.shrink() } else { chip })
                    .into_any_element()
            }
        };
        let one_line = rendered_size(cx, make(false));
        let mut cx2 = TestAppContext::single();
        let wrapped = rendered_size(&mut cx2, make(true));
        assert_eq!(wrapped.width, px(100.));
        assert!(
            wrapped.height > one_line.height,
            "{wrapped:?} vs {one_line:?}"
        );
    }
}
