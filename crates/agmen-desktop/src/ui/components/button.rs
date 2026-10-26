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
    BORDER_WIDTH, BUTTON_HOVER_OPACITY, BUTTON_TINT, RADIUS_MD, RADIUS_SM, SPACE_1, SPACE_3,
    SPACE_5, SPACE_7, SPACE_9, SPACE_10, TINT_SELECTED, tint,
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
        | ButtonStyle::Subtle => Hover::Fill(theme.element_hover),
        // An alert's own actions: Andy's theme hovers in the surface's own
        // color, so the border lightens instead (Zed's outlined buttons
        // show their state on the border too).
        ButtonStyle::Bordered => Hover::Border(theme.text_placeholder),
    }
}

#[derive(IntoElement)]
pub(crate) struct Button {
    id: ElementId,
    label: SharedString,
    style: ButtonStyle,
    selected: bool,
    leading: Option<AnyElement>,
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
            selected: false,
            leading: None,
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
        let hover = hover(self.style, theme);
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
        let styled = match self.style {
            ButtonStyle::Outlined => base
                .bg(theme.elevated_surface)
                .border(BORDER_WIDTH)
                .border_color(theme.border)
                .rounded(RADIUS_MD)
                .py(SPACE_5)
                .px(SPACE_10),
            ButtonStyle::Filled => base
                .bg(theme.element_selected)
                .rounded(RADIUS_SM)
                .py(SPACE_1)
                .px(SPACE_7),
            ButtonStyle::Tinted | ButtonStyle::Plain | ButtonStyle::Bordered => base
                .bg(if self.style == ButtonStyle::Tinted {
                    theme.text_accent.opacity(BUTTON_TINT)
                } else {
                    theme.elevated_surface
                })
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
                .bg(theme.error)
                .border(BORDER_WIDTH)
                .border_color(theme.error)
                .rounded(RADIUS_MD)
                .py(SPACE_3)
                .px(SPACE_9),
            // These sit on a card whose fill is the hover color, so hover
            // fades the button instead of filling it.
            ButtonStyle::Primary | ButtonStyle::Secondary => base
                .bg(if primary {
                    theme.text_accent
                } else {
                    theme.element_selected
                })
                .rounded(RADIUS_MD)
                .py(SPACE_3)
                .px(SPACE_9),
            ButtonStyle::Subtle => base
                .bg(theme.elevated_surface)
                .rounded(RADIUS_MD)
                .py(SPACE_3)
                .px(SPACE_9),
        };
        let styled = styled.hover(move |style| match hover {
            Hover::Fill(fill) => style.bg(fill),
            Hover::Border(border) => style.border_color(border),
            Hover::Fade => style.opacity(BUTTON_HOVER_OPACITY),
        });
        let styled = if selected {
            styled.bg(tint(theme.text_accent, TINT_SELECTED))
        } else {
            styled
        };
        let label = Label::new(self.label).size(LabelSize::Small);
        styled
            .when_some(self.on_click, |this, handler| {
                this.cursor_pointer().on_click(handler)
            })
            .children(self.leading)
            .child(match self.style {
                ButtonStyle::Primary | ButtonStyle::Danger => {
                    label.weight(FontWeight::SEMIBOLD).color(Color::Background)
                }
                ButtonStyle::Tinted => label.color(Color::Accent),
                _ if selected => label.color(Color::Accent),
                ButtonStyle::Outlined
                | ButtonStyle::Filled
                | ButtonStyle::Plain
                | ButtonStyle::Secondary
                | ButtonStyle::Subtle
                | ButtonStyle::Bordered => label.color(Color::Default),
            })
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
}
