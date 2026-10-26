//! A text button, such as "Change listener…" or the status bar's Listener
//! toggle.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div,
};

use super::{ClickHandler, Color, Label, LabelSize};
use crate::appearance::{Appearance, Theme};
use crate::ui::tokens::{
    BORDER_WIDTH, BUTTON_HOVER_OPACITY, BUTTON_TINT, RADIUS_CARD, RADIUS_MD, RADIUS_SM, SPACE_1,
    SPACE_2, SPACE_3, SPACE_4, SPACE_5, SPACE_6, SPACE_7, SPACE_8, SPACE_9, SPACE_10, SPACE_12,
    TINT_SELECTED, tint,
};

/// How the button is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ButtonStyle {
    /// Surface fill with a border, as "Change listener…".
    #[default]
    Outlined,
    /// Selected fill, compact, as the status bar's Listener toggle.
    Filled,
    /// Accent fill with semibold text, as a change card's Apply (node 7118).
    Primary,
    /// Accent text on a faint accent fill, as Open in terminal and
    /// Duplicate to .agmen/roles (console.py "primary" breadcrumb buttons).
    Tinted,
    /// Surface fill, as the agent view's Stop (console.py "plain").
    Plain,
    /// Selected fill, as a change card's Cancel and Undo (nodes 7120, 7736).
    Secondary,
    /// Surface fill, as the breadcrumb's "Show ledger" (node 135).
    Subtle,
    /// Background fill with a border, as a dialog's Cancel (node 2443).
    Inset,
}

/// How big the button is drawn; its style keeps its fill and border.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ButtonSize {
    /// The style's own padding and small text.
    #[default]
    Default,
    /// Padding 2 by 8 and radius 4, as the take-over terminal's Open as tab
    /// and the switch menu's Update… (nodes 1143, 1833).
    Compact,
    /// Padding 4 by 10 and 6 between its parts, as the add agent dialog's
    /// Copy prompt (node 2435).
    Medium,
    /// Padding 6 by 12, radius 6 and body text, as a dialog's Cancel and
    /// Check again (nodes 2443, 2445).
    Large,
}

/// The fill `style` draws in `theme`, before hover and selection.
fn fill(style: ButtonStyle, theme: &Theme) -> Hsla {
    match style {
        ButtonStyle::Outlined | ButtonStyle::Plain | ButtonStyle::Subtle => theme.elevated_surface,
        ButtonStyle::Filled | ButtonStyle::Secondary => theme.element_selected,
        ButtonStyle::Primary => theme.text_accent,
        ButtonStyle::Tinted => theme.text_accent.opacity(BUTTON_TINT),
        ButtonStyle::Inset => theme.background,
    }
}

#[derive(IntoElement)]
pub(crate) struct Button {
    id: ElementId,
    label: SharedString,
    style: ButtonStyle,
    size: ButtonSize,
    weight: Option<FontWeight>,
    selected: bool,
    leading: Option<AnyElement>,
    trailing: Option<AnyElement>,
    on_click: Option<ClickHandler>,
}

impl std::fmt::Debug for Button {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Button")
            .field("id", &self.id)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl Button {
    #[must_use]
    pub(crate) fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            style: ButtonStyle::Outlined,
            size: ButtonSize::Default,
            weight: None,
            selected: false,
            leading: None,
            trailing: None,
            on_click: None,
        }
    }

    #[must_use]
    pub(crate) const fn style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }

    /// Draws a toggle button's on state: an accent tint and accent text.
    /// The mockup draws only the off state (reference sheet mismatch 12).
    /// It takes a `bool`, as `Tab`, `ListItem` and `IconButton` do (Zed's
    /// `Toggleable::toggle_state`), so callers pass a comparison such as
    /// `index == active`.
    #[must_use]
    pub(crate) const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    #[must_use]
    pub(crate) const fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// The label's weight, over the style's own.
    #[must_use]
    pub(crate) const fn weight(mut self, weight: FontWeight) -> Self {
        self.weight = Some(weight);
        self
    }

    /// Something drawn after the label, such as Hand back's ×.
    #[must_use]
    pub(crate) fn trailing(mut self, trailing: impl IntoElement) -> Self {
        self.trailing = Some(trailing.into_any_element());
        self
    }

    /// Something drawn before the label, such as a glyph.
    #[must_use]
    pub(crate) fn leading(mut self, leading: impl IntoElement) -> Self {
        self.leading = Some(leading.into_any_element());
        self
    }

    #[must_use]
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let hover = theme.element_hover;
        let selector = self.id.to_string();
        let base = div()
            .id(self.id)
            .debug_selector(|| selector)
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_5);
        let primary = self.style == ButtonStyle::Primary;
        let selected = self.selected && !primary;
        let base = base.bg(fill(self.style, theme));
        let styled = match self.style {
            ButtonStyle::Outlined | ButtonStyle::Inset => base
                .border(BORDER_WIDTH)
                .border_color(theme.border)
                .rounded(RADIUS_MD)
                .py(SPACE_5)
                .px(SPACE_10)
                .hover(move |style| style.bg(hover)),
            ButtonStyle::Filled => base
                .rounded(RADIUS_SM)
                .py(SPACE_1)
                .px(SPACE_7)
                .hover(move |style| style.bg(hover)),
            ButtonStyle::Tinted | ButtonStyle::Plain => base
                .rounded(RADIUS_MD)
                .py(SPACE_3)
                .px(SPACE_9)
                .hover(move |style| style.bg(hover)),
            // These sit on a card whose fill is the hover color, so hover
            // fades the button instead of filling it.
            ButtonStyle::Primary | ButtonStyle::Secondary => base
                .rounded(RADIUS_MD)
                .py(SPACE_3)
                .px(SPACE_9)
                .hover(|style| style.opacity(BUTTON_HOVER_OPACITY)),
            ButtonStyle::Subtle => base
                .rounded(RADIUS_MD)
                .py(SPACE_3)
                .px(SPACE_9)
                .hover(move |style| style.bg(hover)),
        };
        let styled = if selected {
            styled.bg(tint(theme.text_accent, TINT_SELECTED))
        } else {
            styled
        };
        // Later padding and radius replace the style's.
        let (styled, text) = match self.size {
            ButtonSize::Default => (styled, LabelSize::Small),
            ButtonSize::Compact => (
                styled
                    .py(SPACE_2)
                    .px(SPACE_8)
                    .rounded(RADIUS_SM)
                    .gap(SPACE_5),
                LabelSize::Small,
            ),
            ButtonSize::Medium => (
                styled.py(SPACE_4).px(SPACE_10).gap(SPACE_6),
                LabelSize::Small,
            ),
            ButtonSize::Large => (
                styled.py(SPACE_6).px(SPACE_12).rounded(RADIUS_CARD),
                LabelSize::Default,
            ),
        };
        let label = Label::new(self.label).size(text);
        styled
            .when_some(self.on_click, |this, handler| {
                this.cursor_pointer().on_click(handler)
            })
            .children(self.leading)
            .child({
                let label = match self.style {
                    ButtonStyle::Primary => {
                        label.weight(FontWeight::SEMIBOLD).color(Color::Background)
                    }
                    ButtonStyle::Tinted => label.color(Color::Accent),
                    _ if selected => label.color(Color::Accent),
                    ButtonStyle::Outlined
                    | ButtonStyle::Inset
                    | ButtonStyle::Filled
                    | ButtonStyle::Plain
                    | ButtonStyle::Secondary
                    | ButtonStyle::Subtle => label.color(Color::Default),
                };
                match self.weight {
                    Some(weight) => label.weight(weight),
                    None => label,
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
    fn buttons_render(cx: &mut TestAppContext) {
        let outlined = rendered_size(cx, || {
            Button::new("change", "Change listener…").into_any_element()
        });
        assert!(outlined.width > px(20.));
    }

    #[gpui::test]
    fn filled_button_renders(cx: &mut TestAppContext) {
        let filled = rendered_size(cx, || {
            Button::new("toggle", "Listener")
                .style(ButtonStyle::Filled)
                .leading(Label::new("◨").color(Color::Accent))
                .on_click(|_, _, _| {})
                .into_any_element()
        });
        assert!(filled.width > px(14.));
    }

    #[gpui::test]
    fn secondary_button_is_compact(cx: &mut TestAppContext) {
        let outlined = rendered_size(cx, || Button::new("a", "Undo").into_any_element());
        let mut cx2 = TestAppContext::single();
        let secondary = rendered_size(&mut cx2, || {
            Button::new("b", "Undo")
                .style(ButtonStyle::Secondary)
                .into_any_element()
        });
        // Outlined pads 5 px and draws a 1 px border; secondary pads 3 px.
        assert_eq!(outlined.height - secondary.height, px(6.));
    }

    #[gpui::test]
    fn subtle_button_keeps_its_size_when_selected(cx: &mut TestAppContext) {
        let off = rendered_size(cx, || {
            Button::new("a", "Show ledger")
                .style(ButtonStyle::Subtle)
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let on = rendered_size(&mut cx2, || {
            Button::new("b", "Show ledger")
                .style(ButtonStyle::Subtle)
                .selected(true)
                .into_any_element()
        });
        assert_eq!(off, on);
    }

    // Task 14: the terminal head's and switch menu's compact buttons (nodes
    // 1143, 1833: padding 2 by 8, radius 4) and the dialog's large ones
    // (nodes 2443, 2445: padding 6 by 12, body text).
    #[gpui::test]
    fn compact_button_pads_two_by_eight(cx: &mut TestAppContext) {
        let button = rendered_size(cx, || {
            Button::new("tab", "Open as tab")
                .style(ButtonStyle::Secondary)
                .size(ButtonSize::Compact)
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let text = rendered_size(&mut cx2, || {
            Label::new("Open as tab")
                .size(LabelSize::Small)
                .into_any_element()
        });
        assert_eq!(button.width - text.width, px(16.));
        assert_eq!(button.height - text.height, px(4.));
    }

    #[gpui::test]
    fn large_button_sets_body_text(cx: &mut TestAppContext) {
        let button = rendered_size(cx, || {
            Button::new("check", "Check again")
                .style(ButtonStyle::Primary)
                .size(ButtonSize::Large)
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let text = rendered_size(&mut cx2, || {
            Label::new("Check again")
                .weight(FontWeight::SEMIBOLD)
                .into_any_element()
        });
        assert_eq!(button.width - text.width, px(24.));
        assert_eq!(button.height - text.height, px(12.));
    }

    #[gpui::test]
    fn trailing_follows_the_label(cx: &mut TestAppContext) {
        let plain = rendered_size(cx, || {
            Button::new("back", "Hand back")
                .size(ButtonSize::Compact)
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let with = rendered_size(&mut cx2, || {
            Button::new("back", "Hand back")
                .size(ButtonSize::Compact)
                .trailing(Label::new("×").size(LabelSize::Small))
                .into_any_element()
        });
        assert!(with.width > plain.width + SPACE_5);
    }

    #[gpui::test]
    fn primary_button_matches_secondary_height(cx: &mut TestAppContext) {
        let primary = rendered_size(cx, || {
            Button::new("apply", "Apply")
                .style(ButtonStyle::Primary)
                .on_click(|_, _, _| {})
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let secondary = rendered_size(&mut cx2, || {
            Button::new("cancel", "Cancel")
                .style(ButtonStyle::Secondary)
                .into_any_element()
        });
        assert_eq!(primary.height, secondary.height);
        assert!(primary.width > px(0.));
    }

    // Task 14 review: Copy prompt pads 4 by 10 (node 2435).
    #[gpui::test]
    fn medium_button_pads_four_by_ten(cx: &mut TestAppContext) {
        let button = rendered_size(cx, || {
            Button::new("copy", "Copy prompt")
                .style(ButtonStyle::Secondary)
                .size(ButtonSize::Medium)
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let text = rendered_size(&mut cx2, || {
            Label::new("Copy prompt")
                .size(LabelSize::Small)
                .into_any_element()
        });
        assert_eq!(button.width - text.width, px(20.));
        assert_eq!(button.height - text.height, px(8.));
    }

    // Task 14 review: a dialog's Cancel (node 2443) is outlined on the
    // background fill, the same size as Outlined.
    #[gpui::test]
    fn inset_button_keeps_the_outlined_size(cx: &mut TestAppContext) {
        let outlined = rendered_size(cx, || {
            Button::new("a", "Cancel")
                .size(ButtonSize::Large)
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let inset = rendered_size(&mut cx2, || {
            Button::new("b", "Cancel")
                .style(ButtonStyle::Inset)
                .size(ButtonSize::Large)
                .into_any_element()
        });
        assert_eq!(outlined, inset);
    }

    #[test]
    fn inset_fills_with_the_background() {
        let theme = Appearance::builtin().theme;
        assert_eq!(fill(ButtonStyle::Inset, &theme), theme.background);
        assert_eq!(fill(ButtonStyle::Outlined, &theme), theme.elevated_surface);
    }
}
