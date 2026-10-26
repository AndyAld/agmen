//! A text button, such as "Change listener…" or the status bar's Listener
//! toggle.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div,
};

use super::{ClickHandler, Color, Label, LabelSize};
use crate::appearance::Appearance;
use crate::ui::tokens::{
    BORDER_WIDTH, BUTTON_HOVER_OPACITY, BUTTON_TINT, RADIUS_INPUT, RADIUS_MD, RADIUS_SM, SPACE_1,
    SPACE_3, SPACE_5, SPACE_7, SPACE_8, SPACE_9, SPACE_10, SPACE_12, SPACE_14, TINT_SELECTED, tint,
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
    /// Task 19: the window background with a border, so it reads on an
    /// elevated card, as the kick-off dialog's Cancel (node 10318).
    Recessed,
}

#[derive(IntoElement)]
pub(crate) struct Button {
    id: ElementId,
    label: SharedString,
    style: ButtonStyle,
    selected: bool,
    // Task 19: the 32 px buttons of the plan panel and kick-off dialog.
    large: bool,
    disabled: bool,
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
            large: false,
            disabled: false,
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

    /// Task 19: 13 px text and padding that makes a 32 px button with or
    /// without a border (nodes 9777 and 10953): Primary pads 8 by 14,
    /// Outlined 7 by 12 inside its border.
    #[must_use]
    pub(crate) const fn large(mut self) -> Self {
        self.large = true;
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
        // A disabled button does not light up under the pointer: its hover
        // fill is the outlined fill it is drawn with.
        let hover = if self.disabled {
            theme.elevated_surface
        } else {
            theme.element_hover
        };
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
        let styled = match self.style {
            ButtonStyle::Outlined | ButtonStyle::Recessed => base
                .bg(if self.style == ButtonStyle::Recessed {
                    theme.background
                } else {
                    theme.elevated_surface
                })
                .border(BORDER_WIDTH)
                .border_color(theme.border)
                .rounded(RADIUS_MD)
                .py(SPACE_5)
                .px(SPACE_10)
                .hover(move |style| style.bg(hover)),
            ButtonStyle::Filled => base
                .bg(theme.element_selected)
                .rounded(RADIUS_SM)
                .py(SPACE_1)
                .px(SPACE_7)
                .hover(move |style| style.bg(hover)),
            ButtonStyle::Tinted | ButtonStyle::Plain => base
                .bg(if self.style == ButtonStyle::Tinted {
                    theme.text_accent.opacity(BUTTON_TINT)
                } else {
                    theme.elevated_surface
                })
                .rounded(RADIUS_MD)
                .py(SPACE_3)
                .px(SPACE_9)
                .hover(move |style| style.bg(hover)),
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
                .px(SPACE_9)
                .hover(|style| style.opacity(BUTTON_HOVER_OPACITY)),
            ButtonStyle::Subtle => base
                .bg(theme.elevated_surface)
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
        let styled = if self.large {
            let (py, px) = if primary {
                (SPACE_8, SPACE_14)
            } else {
                (SPACE_7, SPACE_12)
            };
            styled.py(py).px(px).rounded(RADIUS_INPUT)
        } else {
            styled
        };
        let label = Label::new(self.label).size(if self.large {
            LabelSize::Default
        } else {
            LabelSize::Small
        });
        let on_click = if self.disabled { None } else { self.on_click };
        styled
            .when_some(on_click, |this, handler| {
                this.cursor_pointer().on_click(handler)
            })
            .children(self.leading)
            .child(match self.style {
                _ if self.disabled => label.color(Color::Placeholder),
                ButtonStyle::Primary => label.weight(FontWeight::SEMIBOLD).color(Color::Background),
                ButtonStyle::Tinted => label.color(Color::Accent),
                _ if selected => label.color(Color::Accent),
                ButtonStyle::Outlined
                | ButtonStyle::Recessed
                | ButtonStyle::Filled
                | ButtonStyle::Plain
                | ButtonStyle::Secondary
                | ButtonStyle::Subtle => label.color(Color::Default),
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

    // Task 19: the plan panel's Kick off and Pause all (nodes 9046, 9777,
    // 10953) and the dialog's buttons are 13 px text, 32 tall.
    #[gpui::test]
    fn large_buttons_are_one_height_with_or_without_a_border(cx: &mut TestAppContext) {
        let primary = rendered_size(cx, || {
            Button::new("go", "Kick off · 7 agents")
                .style(ButtonStyle::Primary)
                .large()
                .into_any_element()
        });
        let mut cx2 = TestAppContext::single();
        let outlined = rendered_size(&mut cx2, || {
            Button::new("pause", "Pause all").large().into_any_element()
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
                .large()
                .disabled(true)
                .on_click(move |_, _, _| flag.set(true))
                .into_any_element()
        });
        crate::ui::test_support::click(cx, "wait");
        assert!(!clicked.get());
    }
}
