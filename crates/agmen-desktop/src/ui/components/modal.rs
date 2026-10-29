//! A dialog over the window: a dim over everything and a card near the top
//! (frame "Desktop · kick off": dim node 10289, card node 10290; frame
//! "Desktop · add agent": dim node 2417, card node 2418). A click on the dim
//! dismisses it, as a click outside a Zed modal does.

use std::collections::HashMap;

use gpui::{
    AnyElement, AnyWindowHandle, App, ClickEvent, FocusHandle, Global, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement, Pixels, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled as _, WeakFocusHandle, Window, div,
};

use super::{ClickHandler, Color, DialogFooter, Label, LabelSize};
use crate::appearance::{Appearance, Theme};
use crate::ui::tokens::{
    BORDER_WIDTH, DIALOG_MEDIUM_WIDTH, DIALOG_SMALL_WIDTH, MODAL_TOP, MODAL_WIDTH, RADIUS_XL,
    SHADOW_BLUR, SHADOW_OFFSET_Y, SPACE_12, SPACE_20, TINT_DIM, drop_shadow, tint,
};

/// The last modal prepainted in each window is the one above its siblings.
/// Weak handles let closed dialogs and windows release their focus state.
#[derive(Debug, Default)]
struct ModalFocus(HashMap<AnyWindowHandle, WeakFocusHandle>);

impl Global for ModalFocus {}

/// Tests find the dim as `id` and the card as `"{id}:card"`.
#[derive(IntoElement)]
pub(crate) struct Modal {
    id: SharedString,
    width: Pixels,
    top: Pixels,
    children: Vec<AnyElement>,
    title: Option<AnyElement>,
    error: Option<SharedString>,
    error_selector: Option<SharedString>,
    footer: Option<DialogFooter>,
    on_dismiss: Option<ClickHandler>,
}

impl std::fmt::Debug for Modal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Modal")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Modal {
    /// Whether the current modal belongs to this outer key context. Views
    /// that restore focus must not capture focus from a dialog above them.
    #[must_use]
    pub(crate) fn is_topmost(focus: &FocusHandle, window: &Window, cx: &App) -> bool {
        cx.try_global::<ModalFocus>()
            .and_then(|active| active.0.get(&window.window_handle()))
            .and_then(WeakFocusHandle::upgrade)
            .is_none_or(|top| focus.contains(&top, window))
    }

    /// A card `width` wide whose top sits `top` below the window's.
    #[must_use]
    pub(crate) fn new(id: impl Into<SharedString>, width: Pixels, top: Pixels) -> Self {
        Self {
            id: id.into(),
            width,
            top,
            children: Vec::new(),
            title: None,
            error: None,
            error_selector: None,
            footer: None,
            on_dismiss: None,
        }
    }

    #[must_use]
    pub(crate) fn small(id: impl Into<SharedString>) -> Self {
        Self::new(id, DIALOG_SMALL_WIDTH, MODAL_TOP)
    }

    #[must_use]
    pub(crate) fn medium(id: impl Into<SharedString>) -> Self {
        Self::new(id, DIALOG_MEDIUM_WIDTH, MODAL_TOP)
    }

    #[must_use]
    pub(crate) fn large(id: impl Into<SharedString>) -> Self {
        Self::new(id, MODAL_WIDTH, MODAL_TOP)
    }

    /// Keeps an existing mockup's width when it differs from a preset.
    #[must_use]
    pub(crate) const fn width(mut self, width: Pixels) -> Self {
        self.width = width;
        self
    }

    #[must_use]
    pub(crate) const fn top(mut self, top: Pixels) -> Self {
        self.top = top;
        self
    }

    /// A title atom, or a title row with an adjacent subtitle.
    #[must_use]
    pub(crate) fn title(mut self, title: impl IntoElement) -> Self {
        self.title = Some(title.into_any_element());
        self
    }

    #[must_use]
    pub(crate) fn error(mut self, error: Option<impl Into<SharedString>>) -> Self {
        self.error = error.map(Into::into);
        self
    }

    /// Preserves selectors used by the dialog's interaction tests.
    #[must_use]
    pub(crate) fn error_selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.error_selector = Some(selector.into());
        self
    }

    #[must_use]
    pub(crate) fn footer(mut self, footer: DialogFooter) -> Self {
        self.footer = Some(footer);
        self
    }

    /// Called on a click on the dim, outside the card.
    #[must_use]
    pub(crate) fn on_dismiss(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_dismiss = Some(Box::new(handler));
        self
    }
}

impl ParentElement for Modal {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

/// The dim's fill: the mockup's black, the same in every theme, so the
/// background and every surface under it darken.
fn dim_fill() -> Hsla {
    tint(Theme::SHADOW.into(), TINT_DIM)
}

impl RenderOnce for Modal {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = window.use_keyed_state(format!("{}:focus", self.id), cx, |window, cx| {
            restore_focus_on_release(window, cx);
            (cx.focus_handle(), cx.focus_handle())
        });
        let (start, end) = focus.read(cx).clone();
        let top = start.clone();
        let bottom = end.clone();
        let theme = &cx.global::<Appearance>().theme;
        let card_selector = format!("{}:card", self.id);
        let dim_selector = self.id.to_string();
        let error_selector = self
            .error_selector
            .unwrap_or_else(|| format!("{}:error", self.id).into());
        let card = div()
            .on_children_prepainted(move |_, window, cx| {
                keep_top_modal_focused(&top, &bottom, window, cx);
            })
            .id(format!("{}:card", self.id))
            .debug_selector(|| card_selector)
            .occlude()
            .track_focus(&start)
            .tab_group()
            .tab_index(0)
            .tab_stop(false)
            .capture_key_down(move |event, window, cx| {
                let key = &event.keystroke;
                let modifiers = gpui::Modifiers {
                    shift: false,
                    ..key.modifiers
                };
                if key.key == "tab" && !modifiers.modified() {
                    cycle_focus(&start, &end, key.modifiers.shift, window, cx);
                    window.prevent_default();
                    cx.stop_propagation();
                }
            })
            // A click on the card stays on it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_, _, cx| cx.stop_propagation())
            .w(self.width)
            .flex()
            .flex_col()
            .gap(SPACE_12)
            .p(SPACE_20)
            .bg(theme.elevated_surface)
            .border(BORDER_WIDTH)
            .border_color(theme.border)
            .rounded(RADIUS_XL)
            // The palette's shadow, as node 2418 casts it.
            .shadow(vec![drop_shadow(SHADOW_OFFSET_Y, SHADOW_BLUR)])
            .children(self.title)
            .children(self.children)
            .children(self.error.map(|error| {
                div()
                    .debug_selector(move || error_selector.to_string())
                    .child(
                        Label::new(error)
                            .size(LabelSize::Small)
                            .color(Color::Error)
                            .wrap(),
                    )
            }))
            .children(self.footer)
            // This boundary is not a tab stop and takes no layout space.
            .child(
                div()
                    .absolute()
                    .track_focus(&focus.read(cx).1)
                    .tab_index(0)
                    .tab_stop(false),
            );
        let dim = div()
            .id(self.id)
            .debug_selector(|| dim_selector)
            .occlude()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt(self.top)
            .bg(dim_fill())
            .child(card);
        match self.on_dismiss {
            Some(handler) => dim.on_click(handler),
            None => dim,
        }
    }
}

/// A closed dialog takes its controls, and their focus, with it. When the
/// modal's state is released, give focus back to what had it before the
/// dialog opened, unless the closing code already moved it.
fn restore_focus_on_release<T: 'static>(window: &Window, cx: &mut gpui::Context<T>) {
    let previous = window.focused(cx).map(|focus| focus.downgrade());
    let handle = window.window_handle();
    cx.on_release(move |_, cx| {
        cx.defer(move |cx| {
            // An Err means the window closed with the dialog: nothing to restore.
            handle
                .update(cx, |_, window, cx| {
                    if window.focused(cx).is_none()
                        && let Some(previous) = previous.and_then(|previous| previous.upgrade())
                    {
                        window.focus(&previous, cx);
                    }
                })
                .unwrap_or_default();
        });
    })
    .detach();
}

fn keep_top_modal_focused(top: &FocusHandle, end: &FocusHandle, window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    let active = cx.default_global::<ModalFocus>();
    active.0.retain(|_, focus| focus.upgrade().is_some());
    active.0.insert(handle, top.downgrade());
    let top = top.clone();
    let end = end.clone();
    let focused = window.focused(cx);
    // Traverse after paint registers the controls. Only the last modal may
    // take focus, including after a dialog above closes.
    window.defer(cx, move |window, cx| {
        if cx
            .global::<ModalFocus>()
            .0
            .get(&handle)
            .is_some_and(|active| active == &top)
            // A close handler may have removed this modal or restored focus
            // since prepaint. Its queued callback must leave that focus alone.
            && top.contains(&end, window)
            && window.focused(cx) == focused
            && !top.contains_focused(window, cx)
        {
            window.focus(&top, cx);
            window.focus_next(cx);
            if !top.contains_focused(window, cx) {
                window.focus(&top, cx);
            }
        }
    });
}

fn cycle_focus(
    start: &FocusHandle,
    end: &FocusHandle,
    reverse: bool,
    window: &mut Window,
    cx: &mut App,
) {
    if reverse {
        window.focus_prev(cx);
    } else {
        window.focus_next(cx);
    }
    if !start.contains_focused(window, cx) {
        window.focus(if reverse { end } else { start }, cx);
        if reverse {
            window.focus_prev(cx);
        } else {
            window.focus_next(cx);
        }
        if !start.contains_focused(window, cx) {
            window.focus(start, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::ui::components::Label;
    use crate::ui::test_support::{click, draw};
    use crate::ui::tokens::{MODAL_TOP, MODAL_WIDTH};
    use gpui::{TestAppContext, px};

    #[test]
    fn the_dim_darkens_the_window_with_black() {
        // Shot diff (nodes 2417 and 10289): #000000 at 0xa0, so the
        // background and every surface under it darken.
        let theme = Appearance::builtin().theme;
        let dim = dim_fill();
        assert_eq!(dim, tint(Theme::SHADOW.into(), TINT_DIM));
        assert!(dim.l < theme.background.l, "{dim:?} does not darken");
    }

    #[gpui::test]
    fn card_sits_below_the_top_and_dismisses_only_outside(cx: &mut TestAppContext) {
        let dismissed = Rc::new(Cell::new(0));
        let count = dismissed.clone();
        let cx = draw(cx, move || {
            let count = count.clone();
            div()
                .relative()
                .w(px(1000.))
                .h(px(700.))
                .child(
                    Modal::new("dialog", MODAL_WIDTH, MODAL_TOP)
                        .on_dismiss(move |_, _, _| count.set(count.get() + 1))
                        .child(Label::new("Kick off")),
                )
                .into_any_element()
        });
        let card = cx.debug_bounds("dialog:card").unwrap();
        assert_eq!(card.size.width, MODAL_WIDTH);
        assert_eq!(card.origin.y, MODAL_TOP);
        click(cx, "dialog:card");
        assert_eq!(dismissed.get(), 0);
        let dim = cx.debug_bounds("dialog").unwrap();
        cx.simulate_click(
            gpui::point(dim.origin.x + px(5.), dim.origin.y + px(5.)),
            gpui::Modifiers::none(),
        );
        assert_eq!(dismissed.get(), 1);
    }
}
