//! The in-app toast for an urgent alert that needs me (frame "Desktop ·
//! needs you", node 6004): the app's mark, "agmen" and "now", the title,
//! two lines of the alert's text and the chime line. The workspace floats it
//! at the window's top right. Clicking it jumps to the alert.
//!
//! The mockup's footer also says "also sent to your phone"; push to a phone
//! is outside the desktop plan, so the footer names only the chime.

use std::rc::Rc;

use agmen_proto::MessageId;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, BoxShadow, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Pixels,
    RenderOnce, StatefulInteractiveElement as _, Styled as _, Window, div, point,
};

use super::components::{Card, ChangeHandler, Color, GlyphDisc, Label, LabelSize};
use super::needs_you::Toast;
use super::tokens::{
    BORDER_WIDTH, GLYPH_DISC, RADIUS_XL, SPACE_4, SPACE_8, SPACE_10, SPACE_12, TINT_ALERT_EDGE,
    TOAST_SHADOW_ALPHA, TOAST_SHADOW_BLUR, TOAST_SHADOW_OFFSET, TOAST_WIDTH, tint,
};
use crate::appearance::{Appearance, Theme};

/// How many lines of the alert's text the toast shows.
const BODY_LINES: usize = 2;

/// The toast. Tests find it as `"toast"`.
#[derive(IntoElement)]
pub(crate) struct AlertToast {
    toast: Toast,
    on_click: Option<ChangeHandler<MessageId>>,
}

impl std::fmt::Debug for AlertToast {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlertToast")
            .field("toast", &self.toast)
            .finish_non_exhaustive()
    }
}

impl AlertToast {
    #[must_use]
    pub(crate) const fn new(toast: Toast) -> Self {
        Self {
            toast,
            on_click: None,
        }
    }

    /// Called with the alert when the toast is clicked.
    #[must_use]
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(MessageId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for AlertToast {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let shadow = BoxShadow {
            color: tint(Theme::SHADOW.into(), TOAST_SHADOW_ALPHA),
            offset: point(Pixels::ZERO, TOAST_SHADOW_OFFSET),
            blur_radius: TOAST_SHADOW_BLUR,
            spread_radius: Pixels::ZERO,
            inset: false,
        };
        let part = |name: &'static str| div().debug_selector(move || name.into());
        let card = Card::new()
            .w(TOAST_WIDTH)
            .py(SPACE_10)
            .px(SPACE_12)
            .gap(SPACE_4)
            .rounded(RADIUS_XL)
            .border(BORDER_WIDTH)
            .border_color(tint(theme.error, TINT_ALERT_EDGE))
            .shadow(vec![shadow])
            .child(
                div()
                    .h(GLYPH_DISC)
                    .flex()
                    .items_center()
                    .gap(SPACE_8)
                    .child(GlyphDisc::text("!", Color::Error).square())
                    .child(
                        Label::new("agmen")
                            .size(LabelSize::Small)
                            .color(Color::Placeholder),
                    )
                    .child(div().flex_1())
                    .child(Label::new("now").size(LabelSize::Small).color(Color::Muted)),
            )
            .child(
                part("toast:title").child(
                    Label::new(self.toast.title)
                        .weight(FontWeight::SEMIBOLD)
                        .color(Color::TextMuted),
                ),
            )
            .child(
                part("toast:body")
                    .flex()
                    .line_clamp(BODY_LINES)
                    .child(Label::new(self.toast.body).size(LabelSize::Small).wrap()),
            )
            .child(
                part("toast:footer").child(
                    Label::new("♪ urgent chime")
                        .size(LabelSize::Small)
                        .color(Color::Placeholder),
                ),
            );
        let alert = self.toast.alert;
        div()
            .id("toast")
            .debug_selector(|| "toast".into())
            .flex_none()
            .when_some(self.on_click, |this, handler| {
                this.cursor_pointer().on_click(move |_, window, cx| {
                    handler(alert.clone(), window, cx);
                })
            })
            .child(card)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::ui::test_support::{click, draw};
    use gpui::{TestAppContext, px};

    fn toast() -> Toast {
        Toast {
            alert: "01JD0000000000000000000010".parse().unwrap(),
            title: "adam/coord needs you".into(),
            body: "CI on main is red after #10 merged: it_tabs, 3 failed. The release tag is \
                   blocked. Revert #10, or hotfix on a branch? And then some more text so the \
                   body runs past two lines."
                .into(),
        }
    }

    #[gpui::test]
    fn toast_is_360_wide_and_clamps_its_text(cx: &mut TestAppContext) {
        let cx = draw(cx, || AlertToast::new(toast()).into_any_element());
        let card = cx.debug_bounds("toast").unwrap();
        assert_eq!(card.size.width, TOAST_WIDTH);
        let body = cx.debug_bounds("toast:body").unwrap();
        let title = cx.debug_bounds("toast:title").unwrap();
        // Two lines of small text are under three title lines.
        assert!(body.size.height < title.size.height * 3., "{body:?}");
        assert!(body.size.height > title.size.height, "{body:?}");
        assert!(cx.debug_bounds("toast:footer").is_some());
        assert!(
            card.size.height > px(80.) && card.size.height < px(130.),
            "{card:?}"
        );
    }

    #[gpui::test]
    fn clicking_the_toast_reports_its_alert(cx: &mut TestAppContext) {
        let heard: Rc<RefCell<Vec<MessageId>>> = Rc::default();
        let sink = heard.clone();
        let cx = draw(cx, move || {
            let sink = sink.clone();
            AlertToast::new(toast())
                .on_click(move |id, _, _| sink.borrow_mut().push(id))
                .into_any_element()
        });
        click(cx, "toast");
        assert_eq!(*heard.borrow(), [toast().alert]);
    }
}
