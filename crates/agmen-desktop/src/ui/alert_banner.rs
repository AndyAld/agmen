//! The banner under the chat's breadcrumb: "Needs you" with the oldest
//! alert that needs me and "Jump to it" (frame "Desktop · needs you", node
//! 5704), or "Answered" once the last one is answered (frame "Desktop ·
//! needs you (answered)", node 6257). Each draws its rule inside, so it is
//! 37 or 35 px tall as drawn (V61).

use std::rc::Rc;

use agmen_proto::MessageId;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    Styled as _, Window, div,
};

use super::components::{
    Button, ButtonStyle, ChangeHandler, Chip, Color, GlyphDisc, Label, LabelSize,
};
use super::needs_you::Banner;
use super::tokens::{
    ANSWERED_BANNER_HEIGHT, BORDER_WIDTH, NEEDS_YOU_BANNER_HEIGHT, SPACE_10, SPACE_14,
    TINT_ALERT_EDGE, TINT_ANSWERED, TINT_ANSWERED_EDGE, TINT_BANNER, tint,
};
use crate::appearance::Appearance;
use crate::icons::UiIcon;

/// The banner. Tests find it as `"banner"`, its "chime played" chip as
/// `"banner:chime"` and its button as `"banner:jump"`.
#[derive(IntoElement)]
pub(crate) struct AlertBanner {
    banner: Banner,
    on_jump: Option<ChangeHandler<MessageId>>,
}

impl std::fmt::Debug for AlertBanner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlertBanner")
            .field("banner", &self.banner)
            .finish_non_exhaustive()
    }
}

impl AlertBanner {
    #[must_use]
    pub(crate) const fn new(banner: Banner) -> Self {
        Self {
            banner,
            on_jump: None,
        }
    }

    /// Called with the alert when "Jump to it" is clicked.
    #[must_use]
    pub(crate) fn on_jump(
        mut self,
        handler: impl Fn(MessageId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_jump = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for AlertBanner {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        let (needs_you, edge) = match &self.banner {
            Banner::NeedsYou { .. } => (
                tint(theme.error, TINT_BANNER),
                tint(theme.error, TINT_ALERT_EDGE),
            ),
            Banner::Answered { .. } => (
                tint(theme.success, TINT_ANSWERED),
                tint(theme.success, TINT_ANSWERED_EDGE),
            ),
        };
        let base = div()
            .debug_selector(|| "banner".into())
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_10)
            .px(SPACE_14)
            .bg(needs_you)
            .border_b(BORDER_WIDTH)
            .border_color(edge);
        let title = |text: &'static str| {
            Label::new(text)
                .weight(FontWeight::SEMIBOLD)
                .color(Color::TextMuted)
        };
        match self.banner {
            Banner::NeedsYou {
                alert,
                message,
                chimed,
            } => {
                let on_jump = self.on_jump;
                base.h(NEEDS_YOU_BANNER_HEIGHT)
                    .child(GlyphDisc::text("!", Color::Error))
                    .child(title("Needs you"))
                    .child(Label::new(message).truncate())
                    .when(chimed, |this| {
                        this.child(
                            div()
                                .debug_selector(|| "banner:chime".into())
                                .flex_none()
                                .child(Chip::new("chime played").compact().leading(
                                    Label::new("♪").size(LabelSize::Small).color(Color::Accent),
                                )),
                        )
                    })
                    .child(
                        Button::new("banner:jump", "Jump to it")
                            .style(ButtonStyle::Danger)
                            .when_some(on_jump, |button, handler| {
                                button.on_click(move |_, window, cx| {
                                    handler(alert.clone(), window, cx);
                                })
                            }),
                    )
            }
            Banner::Answered { message } => base
                .h(ANSWERED_BANNER_HEIGHT)
                .child(GlyphDisc::icon(UiIcon::Check, Color::Success))
                .child(title("Answered"))
                .child(Label::new(message).truncate()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::ui::test_support::{click, draw};
    use gpui::{TestAppContext, px};

    fn alert() -> MessageId {
        "01JD0000000000000000000010".parse().unwrap()
    }

    fn needs_you(chimed: bool) -> Banner {
        Banner::NeedsYou {
            alert: alert(),
            message: "adam/coord: CI on main is red after #10 merged; the release tag is blocked."
                .into(),
            chimed,
        }
    }

    fn answered() -> Banner {
        Banner::Answered {
            message:
                "adam (coordinator) answered adam/coord's alert at 15:59. Cleared for everyone."
                    .into(),
        }
    }

    /// The banner in an 828 px column, as the chat pane is in the frame.
    fn banner_in_column(banner: Banner) -> impl Fn() -> gpui::AnyElement {
        move || {
            div()
                .w(px(828.))
                .flex()
                .flex_col()
                .child(AlertBanner::new(banner.clone()).on_jump(|_, _, _| {}))
                .into_any_element()
        }
    }

    #[gpui::test]
    fn needs_you_banner_is_37_px_with_its_rule(cx: &mut TestAppContext) {
        let cx = draw(cx, banner_in_column(needs_you(true)));
        let banner = cx.debug_bounds("banner").unwrap();
        assert_eq!(banner.size.height, px(37.));
        assert_eq!(banner.size.width, px(828.));
        assert!(cx.debug_bounds("banner:jump").is_some());
    }

    #[gpui::test]
    fn answered_banner_is_35_px_without_a_button(cx: &mut TestAppContext) {
        let cx = draw(cx, banner_in_column(answered()));
        assert_eq!(cx.debug_bounds("banner").unwrap().size.height, px(35.));
        assert!(cx.debug_bounds("banner:jump").is_none());
        assert!(cx.debug_bounds("banner:chime").is_none());
    }

    #[gpui::test]
    fn chime_chip_shows_only_after_a_chime(cx: &mut TestAppContext) {
        let chimed = draw(cx, banner_in_column(needs_you(true)));
        let chip = chimed.debug_bounds("banner:chime").unwrap();
        let jump = chimed.debug_bounds("banner:jump").unwrap();
        assert!(
            chip.right() <= jump.left(),
            "the chip sits before the button"
        );
        let mut quiet = TestAppContext::single();
        let quiet = draw(&mut quiet, banner_in_column(needs_you(false)));
        assert!(quiet.debug_bounds("banner:chime").is_none());
    }

    #[gpui::test]
    fn jump_reports_the_alert(cx: &mut TestAppContext) {
        let heard: Rc<RefCell<Vec<MessageId>>> = Rc::default();
        let sink = heard.clone();
        let cx = draw(cx, move || {
            let sink = sink.clone();
            div()
                .w(px(828.))
                .child(AlertBanner::new(needs_you(false)).on_jump(move |id, _, _| {
                    sink.borrow_mut().push(id);
                }))
                .into_any_element()
        });
        click(cx, "banner:jump");
        assert_eq!(*heard.borrow(), [alert()]);
    }
}
