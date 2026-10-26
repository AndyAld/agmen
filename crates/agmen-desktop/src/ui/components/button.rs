//! A text button, such as "Change listener…" or the status bar's Listener
//! toggle.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, RenderOnce, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::{ClickHandler, Color, Label, LabelSize};
use crate::appearance::{Appearance, Theme};
use crate::ui::tokens::{
    BORDER_WIDTH, BUTTON_HOVER_OPACITY, BUTTON_TINT, RADIUS_CARD, RADIUS_INPUT, RADIUS_MD,
    RADIUS_SM, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_5, SPACE_6, SPACE_7, SPACE_8, SPACE_9,
    SPACE_10, SPACE_12, SPACE_14, TINT_SELECTED, tint,
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
    /// Error fill with semibold text, as an alert's Reply and the banner's
    /// Jump to it (nodes 5996 and 5712).
    Danger,
    /// Plain with a 1 px border, as an alert's own actions (nodes 5998 to
    /// 6002).
    Bordered,
    /// Background fill with a border, as a dialog's Cancel (node 2443).
    Inset,
}

/// What changes under the pointer.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Hover {
    /// The fill turns to this color.
    Fill(Hsla),
    /// The fill stays and the border turns to this color.
    Border(Hsla),
    /// The whole button fades.
    Fade,
}

/// How `style` hovers in `theme`.
fn hover(style: ButtonStyle, theme: &Theme) -> Hover {
    match style {
        // An accent or error fill, or a card whose fill is the hover color:
        // fading shows on any of them.
        ButtonStyle::Primary | ButtonStyle::Secondary | ButtonStyle::Danger => Hover::Fade,
        ButtonStyle::Outlined
        | ButtonStyle::Filled
        | ButtonStyle::Tinted
        | ButtonStyle::Plain
        | ButtonStyle::Subtle
        | ButtonStyle::Inset => Hover::Fill(theme.element_hover),
        // An alert's own actions: Andy's theme hovers in the surface's own
        // color, so the border lightens instead (Zed's outlined buttons
        // show their state on the border too).
        ButtonStyle::Bordered => Hover::Border(theme.text_placeholder),
    }
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
    /// Radius 6 and body text, as a dialog's Cancel and Check again:
    /// Primary pads 6 by 14 (node 2445), the other styles 6 by 12 inside
    /// their border (node 2443).
    Large,
    /// Task 19: body text and padding that makes a 32 px button with or
    /// without a border (nodes 9777 and 10953): Primary pads 8 by 14, the
    /// other styles 7 by 12 inside their border; the input's radius. The
    /// plan panel's Kick off and Pause all, and the kick-off dialog's
    /// buttons.
    Tall,
}

/// The fill `style` draws in `theme`, before hover and selection.
fn fill(style: ButtonStyle, theme: &Theme) -> Hsla {
    match style {
        ButtonStyle::Outlined
        | ButtonStyle::Plain
        | ButtonStyle::Subtle
        | ButtonStyle::Bordered => theme.elevated_surface,
        ButtonStyle::Filled | ButtonStyle::Secondary => theme.element_selected,
        ButtonStyle::Primary => theme.text_accent,
        ButtonStyle::Tinted => theme.text_accent.opacity(BUTTON_TINT),
        ButtonStyle::Inset => theme.background,
        ButtonStyle::Danger => theme.error,
    }
}

/// `styled` padded and rounded for `size`, with its label's size. Later
/// padding and radius replace the style's.
fn sized(
    styled: Stateful<Div>,
    size: ButtonSize,
    style: ButtonStyle,
) -> (Stateful<Div>, LabelSize) {
    match size {
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
        ButtonSize::Large => {
            let px = if style == ButtonStyle::Primary {
                SPACE_14
            } else {
                SPACE_12
            };
            (
                styled.py(SPACE_6).px(px).rounded(RADIUS_CARD),
                LabelSize::Default,
            )
        }
        ButtonSize::Tall => {
            let (py, px) = if style == ButtonStyle::Primary {
                (SPACE_8, SPACE_14)
            } else {
                (SPACE_7, SPACE_12)
            };
            (
                styled.py(py).px(px).rounded(RADIUS_INPUT),
                LabelSize::Default,
            )
        }
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
    // Task 19: "Kick off · waiting on sean".
    disabled: bool,
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
            disabled: false,
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

    /// Task 19: a button that cannot be clicked now, its label dimmed, as
    /// "Kick off · waiting on sean" (node 9046).
    #[must_use]
    pub(crate) const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
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
        // A disabled button does not light up under the pointer (Task 19).
        let hover = (!self.disabled).then(|| hover(self.style, theme));
        let selector = self.id.to_string();
        let base = div()
            .id(self.id)
            .debug_selector(|| selector)
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_5);
        let primary = matches!(self.style, ButtonStyle::Primary | ButtonStyle::Danger);
        let selected = self.selected && !primary;
        let base = base.bg(fill(self.style, theme));
        let styled = match self.style {
            ButtonStyle::Outlined | ButtonStyle::Inset => base
                .border(BORDER_WIDTH)
                .border_color(theme.border)
                .rounded(RADIUS_MD)
                .py(SPACE_5)
                .px(SPACE_10),
            ButtonStyle::Filled => base.rounded(RADIUS_SM).py(SPACE_1).px(SPACE_7),
            ButtonStyle::Tinted | ButtonStyle::Plain | ButtonStyle::Bordered => base
                .rounded(RADIUS_MD)
                .py(SPACE_3)
                .px(SPACE_9)
                .when(self.style == ButtonStyle::Bordered, |this| {
                    this.border(BORDER_WIDTH).border_color(theme.border)
                }),
            // An alert's Reply sits beside bordered actions, so its border
            // in its own fill keeps the row one height; the error fill is the
            // alert's tint, so hover fades it as Primary does.
            ButtonStyle::Danger => base
                .border(BORDER_WIDTH)
                .border_color(theme.error)
                .rounded(RADIUS_MD)
                .py(SPACE_3)
                .px(SPACE_9),
            // These sit on a card whose fill is the hover color, so hover
            // fades the button instead of filling it.
            ButtonStyle::Primary | ButtonStyle::Secondary => {
                base.rounded(RADIUS_MD).py(SPACE_3).px(SPACE_9)
            }
            ButtonStyle::Subtle => base.rounded(RADIUS_MD).py(SPACE_3).px(SPACE_9),
        };
        let styled = styled.when_some(hover, |styled, hover| {
            styled.hover(move |style| match hover {
                Hover::Fill(fill) => style.bg(fill),
                Hover::Border(border) => style.border_color(border),
                Hover::Fade => style.opacity(BUTTON_HOVER_OPACITY),
            })
        });
        let styled = if selected {
            styled.bg(tint(theme.text_accent, TINT_SELECTED))
        } else {
            styled
        };
        let (styled, text) = sized(styled, self.size, self.style);
        let label = Label::new(self.label).size(text);
        let on_click = if self.disabled { None } else { self.on_click };
        styled
            .when_some(on_click, |this, handler| {
                this.cursor_pointer().on_click(handler)
            })
            .children(self.leading)
            .child({
                let label = match self.style {
                    _ if self.disabled => label.color(Color::Placeholder),
                    ButtonStyle::Primary | ButtonStyle::Danger => {
                        label.weight(FontWeight::SEMIBOLD).color(Color::Background)
                    }
                    ButtonStyle::Tinted => label.color(Color::Accent),
                    _ if selected => label.color(Color::Accent),
                    ButtonStyle::Outlined
                    | ButtonStyle::Inset
                    | ButtonStyle::Filled
                    | ButtonStyle::Plain
                    | ButtonStyle::Secondary
                    | ButtonStyle::Subtle
                    | ButtonStyle::Bordered => label.color(Color::Default),
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
        // Node 2445: Primary pads 6 by 14.
        assert_eq!(button.width - text.width, px(28.));
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
    fn alert_buttons_share_one_height(cx: &mut TestAppContext) {
        // Nodes 5996 to 6002: Reply and the alert's actions sit in one 22 px
        // row; the actions are Plain plus a 1 px border.
        let make =
            |style: ButtonStyle| move || Button::new("b", "Reply").style(style).into_any_element();
        let danger = rendered_size(cx, make(ButtonStyle::Danger));
        let mut cx2 = TestAppContext::single();
        let bordered = rendered_size(&mut cx2, make(ButtonStyle::Bordered));
        let mut cx3 = TestAppContext::single();
        let plain = rendered_size(&mut cx3, make(ButtonStyle::Plain));
        assert_eq!(bordered.height - plain.height, px(2.));
        assert_eq!(bordered.width - plain.width, px(2.));
        assert_eq!(danger, bordered);
    }

    #[test]
    fn bordered_hover_shows_where_the_hover_fill_matches_the_surface() {
        // Gruvbox Material Dark Hard: element.hover and the elevated surface
        // are both #282828 (reference sheet A4), so a fill change is unseen.
        let mut theme = Appearance::builtin().theme;
        theme.element_hover = theme.elevated_surface;
        let shown = match hover(ButtonStyle::Bordered, &theme) {
            Hover::Fill(fill) => fill != theme.elevated_surface,
            Hover::Border(border) => border != theme.border,
            Hover::Fade => true,
        };
        assert!(shown, "{:?}", hover(ButtonStyle::Bordered, &theme));
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

    // Task 19: the plan panel's Kick off and Pause all (nodes 9046, 9777,
    // 10953) and the dialog's buttons are 13 px text, 32 tall.
    #[gpui::test]
    fn large_buttons_are_one_height_with_or_without_a_border(cx: &mut TestAppContext) {
        let primary = rendered_size(cx, || {
            Button::new("go", "Kick off · 7 agents")
                .style(ButtonStyle::Primary)
                .size(ButtonSize::Tall)
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let outlined = rendered_size(&mut cx2, || {
            Button::new("pause", "Pause all")
                .size(ButtonSize::Tall)
                .into_any_element()
        });
        let mut cx3 = TestAppContext::single();
        let text = rendered_size(&mut cx3, || Label::new("Pause all").into_any_element());
        assert_eq!(primary.height, outlined.height);
        assert_eq!(primary.height - text.height, px(16.));
    }

    #[gpui::test]
    fn disabled_button_ignores_clicks(cx: &mut TestAppContext) {
        let clicked = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = clicked.clone();
        let cx = crate::ui::test_support::draw(cx, move || {
            let flag = flag.clone();
            Button::new("wait", "Kick off · waiting on sean")
                .size(ButtonSize::Tall)
                .disabled(true)
                .on_click(move |_, _, _| flag.set(true))
                .into_any_element()
        });
        crate::ui::test_support::click(cx, "wait");
        assert!(!clicked.get());
    }
}
