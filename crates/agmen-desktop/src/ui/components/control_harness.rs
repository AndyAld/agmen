//! A test window that renders one control, so tests can click its parts.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, Context, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext,
    VisualTestContext, Window, div,
};

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
