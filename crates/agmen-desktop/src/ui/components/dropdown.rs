//! A box showing the selected option that opens a menu of all options, as
//! Settings' theme and font pickers.

use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, anchored, deferred, div,
};

use super::{ChangeHandler, Color, Icon, IconSize, Label};
use crate::appearance::Appearance;
use crate::icons::UiIcon;
use crate::ui::tokens::{
    BORDER_WIDTH, DROPDOWN_MENU_MAX_HEIGHT, DROPDOWN_WIDTH, ICON_SM, RADIUS_LG, RADIUS_MD, SPACE_2,
    SPACE_3, SPACE_5, SPACE_8, SPACE_10,
};

/// The caller owns whether the menu is open: it passes `open` and flips it in
/// `on_toggle`, so the dropdown holds no state.
#[derive(IntoElement)]
pub(crate) struct Dropdown {
    id: SharedString,
    options: Vec<SharedString>,
    selected: usize,
    open: bool,
    code: bool,
    on_toggle: Option<ChangeHandler<bool>>,
    on_change: Option<ChangeHandler<usize>>,
}

impl std::fmt::Debug for Dropdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dropdown")
            .field("id", &self.id)
            .field("options", &self.options)
            .field("selected", &self.selected)
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

impl Dropdown {
    /// `id` names the control; tests find the box as `id` and menu row `i` as
    /// `"{id}:{i}"` with `debug_bounds`.
    #[must_use]
    pub(crate) fn new(
        id: impl Into<SharedString>,
        options: impl IntoIterator<Item = impl Into<SharedString>>,
        selected: usize,
    ) -> Self {
        Self {
            id: id.into(),
            options: options.into_iter().map(Into::into).collect(),
            selected,
            open: false,
            code: false,
            on_toggle: None,
            on_change: None,
        }
    }

    /// Shows the menu under the box.
    #[must_use]
    pub(crate) const fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Sets the options in the code font, as the code font picker.
    #[must_use]
    pub(crate) const fn code(mut self) -> Self {
        self.code = true;
        self
    }

    /// Called with the menu's new open state: `true` on a click on the closed
    /// box, `false` on a click on the open box, a row, or outside the menu,
    /// or on Escape while the box has focus.
    #[must_use]
    pub(crate) fn on_toggle(
        mut self,
        handler: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }

    /// Called with the index of the option clicked in the menu.
    #[must_use]
    pub(crate) fn on_change(
        mut self,
        handler: impl Fn(usize, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }

    /// The text in the box: the selected option, or nothing when `selected`
    /// is out of range.
    fn value(&self) -> SharedString {
        self.options.get(self.selected).cloned().unwrap_or_default()
    }

    /// Menu row `index`: a check on the selected row, then the option.
    fn menu_row(
        &self,
        index: usize,
        option: &SharedString,
        on_toggle: Option<ChangeHandler<bool>>,
        on_change: Option<ChangeHandler<usize>>,
        hover: Hsla,
    ) -> impl IntoElement + use<> {
        let selector = format!("{}:{index}", self.id);
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(|| selector)
            .flex()
            .items_center()
            .gap(SPACE_8)
            .py(SPACE_3)
            .px(SPACE_8)
            .rounded(RADIUS_MD)
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_click(move |_, window, cx| {
                if let Some(handler) = &on_change {
                    handler(index, window, cx);
                }
                if let Some(handler) = &on_toggle {
                    handler(false, window, cx);
                }
            })
            // An empty slot on other rows keeps every label in line.
            .child(
                div()
                    .size(ICON_SM)
                    .flex_none()
                    .when(index == self.selected, |this| {
                        this.child(
                            Icon::new(UiIcon::Check)
                                .size(IconSize::Small)
                                .color(Color::Accent),
                        )
                    }),
            )
            .child(self.label(option.clone(), Color::Default))
    }

    fn label(&self, text: SharedString, color: Color) -> Label {
        let label = Label::new(text).color(color);
        if self.code { label.code() } else { label }
    }
}

impl RenderOnce for Dropdown {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let (hover, focus) = (theme.element_hover, theme.text_accent);
        let open = self.open;
        let on_toggle = self.on_toggle.clone();
        let on_change = self.on_change.clone();
        let selector = self.id.to_string();
        let field = div()
            .id(self.id.clone())
            .debug_selector(|| selector)
            .tab_index(0)
            .flex()
            .items_center()
            .justify_between()
            .gap(SPACE_8)
            .w(DROPDOWN_WIDTH)
            .py(SPACE_5)
            .px(SPACE_10)
            .rounded(RADIUS_MD)
            .border(BORDER_WIDTH)
            .border_color(theme.border)
            .bg(theme.background)
            .hover(move |style| style.bg(hover))
            .focus_visible(move |style| style.border_color(focus))
            .when_some(on_toggle.clone(), |this, handler| {
                // While open, the menu's mouse down out closes it. A click
                // handler here too would reopen it on mouse up, after the
                // caller has re-rendered closed.
                this.cursor_pointer().map(|this| {
                    if open {
                        this.on_key_down(move |event, window, cx| {
                            if event.keystroke.key == "escape" {
                                // Settings and modals close on Escape too;
                                // the open menu takes it first.
                                cx.stop_propagation();
                                handler(false, window, cx);
                            }
                        })
                    } else {
                        this.on_click(move |_, window, cx| handler(true, window, cx))
                    }
                })
            })
            .child(self.label(self.value(), Color::Default))
            .child(
                Icon::new(UiIcon::ChevronDown)
                    .size(IconSize::Small)
                    .color(Color::Placeholder),
            );
        let menu = open.then(|| {
            let rows = self.options.iter().enumerate().map(|(index, option)| {
                self.menu_row(index, option, on_toggle.clone(), on_change.clone(), hover)
            });
            // Deferred so the menu paints over the rows below the dropdown.
            deferred(
                anchored().snap_to_window().child(
                    div()
                        .id(SharedString::from(format!("{}:menu", self.id)))
                        .occlude()
                        .mt(SPACE_2)
                        .w(DROPDOWN_WIDTH)
                        .max_h(DROPDOWN_MENU_MAX_HEIGHT)
                        .overflow_y_scroll()
                        .p(SPACE_2)
                        .rounded(RADIUS_LG)
                        .border(BORDER_WIDTH)
                        .border_color(theme.border_variant)
                        .bg(theme.elevated_surface)
                        .when_some(on_toggle.clone(), |this, handler| {
                            this.on_mouse_down_out(move |_, window, cx| handler(false, window, cx))
                        })
                        .children(rows),
                ),
            )
        });
        div()
            .relative()
            .flex_none()
            .child(field)
            .when_some(menu, |this, menu| {
                this.child(div().absolute().top_full().left_0().child(menu))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::components::control_harness::{Caller, Reported, open, open_with_caller};
    use crate::ui::test_support::{click, rendered_size};
    use crate::ui::tokens::DROPDOWN_WIDTH;
    use gpui::{Entity, TestAppContext, VisualTestContext};
    use std::cell::Cell;

    const THEMES: [&str; 3] = ["Gruvbox Material Dark Hard", "One Dark", "Ayu Light"];

    #[test]
    fn dropdown_shows_the_selection_label() {
        assert_eq!(Dropdown::new("theme", THEMES, 1).value(), "One Dark");
        assert_eq!(Dropdown::new("theme", THEMES, 9).value(), "");
    }

    #[gpui::test]
    fn dropdown_renders(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || Dropdown::new("theme", THEMES, 0).into_any_element());
        assert_eq!(size.width, DROPDOWN_WIDTH);
    }

    #[gpui::test]
    fn code_dropdown_renders_open(cx: &mut TestAppContext) {
        let cx = open(cx, || {
            Dropdown::new("code-font", ["Lilex", "CaskaydiaCove Nerd Font Mono"], 1)
                .code()
                .open(true)
                .into_any_element()
        });
        let row = cx.debug_bounds("code-font:0").expect("menu row");
        let field = cx.debug_bounds("code-font").expect("box");
        assert!(row.top() >= field.bottom());
    }

    #[gpui::test]
    fn dropdown_opens_on_click(cx: &mut TestAppContext) {
        let toggled = Reported::default();
        let sink = toggled.clone();
        let cx = open(cx, move || {
            let sink = sink.clone();
            Dropdown::new("theme", THEMES, 0)
                .on_toggle(move |open, _, _| sink.set(Some(open)))
                .into_any_element()
        });
        assert!(cx.debug_bounds("theme:1").is_none());
        click(cx, "theme");
        assert_eq!(toggled.get(), Some(true));
    }

    #[gpui::test]
    fn dropdown_reports_the_chosen_option(cx: &mut TestAppContext) {
        let chosen = Reported::default();
        let toggled = Reported::default();
        let (chosen_sink, toggled_sink) = (chosen.clone(), toggled.clone());
        let cx = open(cx, move || {
            let (chosen_sink, toggled_sink) = (chosen_sink.clone(), toggled_sink.clone());
            Dropdown::new("theme", THEMES, 0)
                .open(true)
                .on_change(move |index, _, _| chosen_sink.set(Some(index)))
                .on_toggle(move |open, _, _| toggled_sink.set(Some(open)))
                .into_any_element()
        });
        click(cx, "theme:2");
        assert_eq!(chosen.get(), Some(2));
        assert_eq!(toggled.get(), Some(false));
    }

    /// Opens a dropdown whose caller owns `open`, starting closed.
    fn with_caller(cx: &mut TestAppContext) -> (Entity<Caller>, &mut VisualTestContext) {
        open_with_caller(cx, false, |open, set| {
            Dropdown::new("theme", THEMES, 0)
                .open(open)
                .on_toggle(move |open, window, cx| set(open, window, cx))
                .into_any_element()
        })
    }

    #[gpui::test]
    fn clicking_the_open_box_closes_it(cx: &mut TestAppContext) {
        let (caller, cx) = with_caller(cx);
        click(cx, "theme");
        assert!(caller.read_with(cx, |caller, _| caller.flag));
        assert!(cx.debug_bounds("theme:1").is_some());

        click(cx, "theme");
        assert!(!caller.read_with(cx, |caller, _| caller.flag));
        assert!(cx.debug_bounds("theme:1").is_none());
    }

    #[gpui::test]
    fn escape_closes_the_open_menu(cx: &mut TestAppContext) {
        let (caller, cx) = with_caller(cx);
        click(cx, "theme");
        assert!(caller.read_with(cx, |caller, _| caller.flag));

        cx.simulate_keystrokes("escape");
        assert!(!caller.read_with(cx, |caller, _| caller.flag));
    }

    #[gpui::test]
    fn escape_on_the_open_menu_stays_in_the_dropdown(cx: &mut TestAppContext) {
        let outer = Rc::new(Cell::new(false));
        let seen = outer.clone();
        let (_, cx) = open_with_caller(cx, false, move |open, set| {
            let seen = seen.clone();
            div()
                .on_key_down(move |_, _, _| seen.set(true))
                .child(
                    Dropdown::new("theme", THEMES, 0)
                        .open(open)
                        .on_toggle(move |open, window, cx| set(open, window, cx)),
                )
                .into_any_element()
        });
        click(cx, "theme");
        cx.simulate_keystrokes("escape");
        assert!(!outer.get());
    }
}
