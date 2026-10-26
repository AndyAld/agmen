//! A test window that renders one control, so tests can click its parts.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    TestAppContext, VisualTestContext, Window, div,
};

use super::ChangeHandler;

use crate::ui::test_support::{init, redraw};
use crate::ui::tokens::MOCKUP_REM;

/// The last value a control's `on_change` reported.
pub(super) type Reported<T> = Rc<Cell<Option<T>>>;

struct Controls(Box<dyn Fn() -> AnyElement>);

impl Render for Controls {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        div().size_full().flex().items_start().child((self.0)())
    }
}

/// Opens a window that draws `make()` every frame.
pub(super) fn open(
    cx: &mut TestAppContext,
    make: impl Fn() -> AnyElement + 'static,
) -> &mut VisualTestContext {
    init(cx);
    let (_, cx) = cx.add_window_view(|_, _| Controls(Box::new(make)));
    redraw(cx);
    cx
}

/// A caller that owns one flag, as a view owns a dropdown's `open`: its
/// handler stores the reported value and re-renders, so a test sees what a
/// person would between mouse down and mouse up.
pub(super) struct Caller {
    pub(super) flag: bool,
    make: Box<dyn Fn(bool, ChangeHandler<bool>) -> AnyElement>,
}

impl Render for Caller {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        let this = cx.entity();
        let set: ChangeHandler<bool> = Rc::new(move |flag, _, cx| {
            this.update(cx, |caller, cx| {
                caller.flag = flag;
                cx.notify();
            });
        });
        div()
            .size_full()
            .flex()
            .items_start()
            .child((self.make)(self.flag, set))
    }
}

/// Opens a window whose `Caller` starts with `flag` and draws
/// `make(flag, set_flag)` every frame.
pub(super) fn open_with_caller(
    cx: &mut TestAppContext,
    flag: bool,
    make: impl Fn(bool, ChangeHandler<bool>) -> AnyElement + 'static,
) -> (Entity<Caller>, &mut VisualTestContext) {
    init(cx);
    let (caller, cx) = cx.add_window_view(|_, _| Caller {
        flag,
        make: Box::new(make),
    });
    redraw(cx);
    (caller, cx)
}
