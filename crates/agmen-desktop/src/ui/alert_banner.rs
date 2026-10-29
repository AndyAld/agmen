//! The banner under the chat's breadcrumb: "Needs you" with the oldest
//! alert that needs me and "Jump to it" (frame "Desktop · needs you", node
//! 5704), or "Answered" once the last one is answered (frame "Desktop ·
//! needs you (answered)", node 6257). Each draws its rule inside, so it is
//! 37 or 35 px tall as drawn (V61). A click on the Answered banner
//! dismisses it; a close button also lets keyboard users dismiss it.

use std::rc::Rc;

use agmen_proto::MessageId;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, RenderOnce,
    StatefulInteractiveElement as _, Styled as _, Window, div,
};

use super::components::{
    Button, ButtonStyle, ChangeHandler, Chip, Color, GlyphDisc, IconButton, Label, LabelSize,
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
    on_dismiss: Option<Rc<DismissHandler>>,
}

type DismissHandler = dyn Fn(&mut Window, &mut App);

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
            on_dismiss: None,
        }
    }

    /// Called when the Answered banner is clicked.
    #[must_use]
    pub(crate) fn on_dismiss(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Some(Rc::new(handler));
        self
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
            // A pane too narrow even for the disc and the button cuts the
            // banner at its edge instead of drawing over the next panel.
            .overflow_hidden()
            .bg(needs_you)
            .border_b(BORDER_WIDTH)
            .border_color(edge);
        // In a narrow pane the summary goes first, then the title and the
        // chime chip are cut, so the button stays whole.
        let title = |text: &'static str| {
            div().min_w_0().overflow_hidden().child(
                Label::new(text)
                    .weight(FontWeight::SEMIBOLD)
                    .color(Color::TextMuted),
            )
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
                                .min_w_0()
                                .overflow_hidden()
                                .child(Chip::new("chime played").note().leading(
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
                    .into_any_element()
            }
            Banner::Answered { message } => {
                let on_dismiss = self.on_dismiss.clone();
                base.id("banner")
                    .h(ANSWERED_BANNER_HEIGHT)
                    .when_some(self.on_dismiss, |this, handler| {
                        this.cursor_pointer()
                            .on_click(move |_, window, cx| handler(window, cx))
                    })
                    .child(GlyphDisc::icon(UiIcon::Check, Color::Success))
                    .child(title("Answered"))
                    .child(Label::new(message).truncate())
                    .when_some(on_dismiss, |this, handler| {
                        this.child(IconButton::new("banner:dismiss", UiIcon::Close).on_click(
                            move |_, window, cx| {
                                cx.stop_propagation();
                                handler(window, cx);
                            },
                        ))
                    })
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::ui::test_support::{click, clip_right, draw};
    use crate::ui::tokens::GLYPH_DISC;
    use gpui::{Pixels, TestAppContext, px};

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
        // Jump to it is a small line padded 3 with no border, 20 px in the
        // mockup (node 5712).
        let jump = cx.debug_bounds("banner:jump").unwrap();
        let mut text = TestAppContext::single();
        let text = crate::ui::test_support::rendered_size(&mut text, || {
            Label::new("Jump to it")
                .size(LabelSize::Small)
                .into_any_element()
        });
        assert_eq!(jump.size.height - text.height, px(20. - 14.));
    }

    /// The needs-you banner with a chime in a pane `width` wide.
    fn narrow_banner(cx: &mut TestAppContext, width: Pixels) -> &mut gpui::VisualTestContext {
        draw(cx, move || {
            div()
                .w(width)
                .flex()
                .flex_col()
                .child(AlertBanner::new(needs_you(true)).on_jump(|_, _, _| {}))
                .into_any_element()
        })
    }

    #[gpui::test]
    fn a_narrow_pane_keeps_the_banner_inside_it(cx: &mut TestAppContext) {
        // needs-you.png at a 158 px pane: the summary shrank to nothing,
        // then the chime chip and Jump to it drew past the pane's edge, over
        // the Listener panel. Now the title and the chip give way and the
        // button stays whole while the pane has room for it.
        let wide = draw(cx, banner_in_column(needs_you(true)));
        let jump = wide.debug_bounds("banner:jump").unwrap();
        let chip = wide.debug_bounds("banner:chime").unwrap();
        // The padding, the disc, four gaps and the button.
        let width = SPACE_14 * 2. + GLYPH_DISC + SPACE_10 * 4. + jump.size.width;
        let mut narrow = TestAppContext::single();
        let narrow = narrow_banner(&mut narrow, width);
        let banner = narrow.debug_bounds("banner").unwrap();
        let chime = narrow.debug_bounds("banner:chime").unwrap();
        let jump_at = narrow.debug_bounds("banner:jump").unwrap();
        assert_eq!(banner.size.width, width);
        assert_eq!(
            jump_at.size.width, jump.size.width,
            "the button stays whole"
        );
        assert!(
            jump_at.right() <= banner.right(),
            "{jump_at:?} past {banner:?}"
        );
        assert!(
            chime.right() <= jump_at.left(),
            "{chime:?} over {jump_at:?}"
        );
        // With room for half the chip, its wrapper reports a shrunken box;
        // the chip is clipped there too, so its label stops at the button.
        let mut half = TestAppContext::single();
        let half = narrow_banner(&mut half, width + chip.size.width / 2.);
        let jump_at = half.debug_bounds("banner:jump").unwrap();
        let clip = clip_right(half, "banner:chime").expect("the chip's fill");
        assert!(
            clip <= jump_at.left(),
            "the chip shows to {clip:?}, over {jump_at:?}"
        );
    }

    #[gpui::test]
    fn a_pane_narrower_than_the_button_cuts_it_at_the_banner_edge(cx: &mut TestAppContext) {
        let jump = draw(cx, banner_in_column(needs_you(true)))
            .debug_bounds("banner:jump")
            .unwrap();
        // Half the button fits past the padding, the disc and the gaps.
        let width = SPACE_14 * 2. + GLYPH_DISC + SPACE_10 * 4. + jump.size.width / 2.;
        let mut narrow = TestAppContext::single();
        let narrow = narrow_banner(&mut narrow, width);
        let banner = narrow.debug_bounds("banner").unwrap();
        let jump_at = narrow.debug_bounds("banner:jump").unwrap();
        assert!(jump_at.right() > banner.right(), "the button runs past");
        let clip = clip_right(narrow, "banner:jump").expect("the button's fill");
        assert!(clip <= banner.right(), "{clip:?} past {banner:?}");
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
        // Node 5709: 1 px above and below the small label, so 15 px tall.
        let mut text = TestAppContext::single();
        let text = crate::ui::test_support::rendered_size(&mut text, || {
            Label::new("chime played")
                .size(LabelSize::Small)
                .into_any_element()
        });
        assert_eq!(chip.size.height, text.height + px(2.));
        assert!(
            chip.right() <= jump.left(),
            "the chip sits before the button"
        );
        let mut quiet = TestAppContext::single();
        let quiet = draw(&mut quiet, banner_in_column(needs_you(false)));
        assert!(quiet.debug_bounds("banner:chime").is_none());
    }

    #[gpui::test]
    fn clicking_the_answered_banner_reports_a_dismiss(cx: &mut TestAppContext) {
        let heard = Rc::new(std::cell::Cell::new(0));
        let sink = heard.clone();
        let cx = draw(cx, move || {
            let sink = sink.clone();
            div()
                .w(px(828.))
                .child(
                    AlertBanner::new(answered()).on_dismiss(move |_, _| sink.set(sink.get() + 1)),
                )
                .into_any_element()
        });
        click(cx, "banner");
        assert_eq!(heard.get(), 1);
    }

    #[gpui::test]
    fn answered_close_hovers_takes_focus_and_dismisses_once(cx: &mut TestAppContext) {
        use crate::ui::test_support::{assert_control_feedback, draw_with_tab, press};
        let dismissed = Rc::new(std::cell::Cell::new(0));
        let seen = dismissed.clone();
        let cx = draw_with_tab(cx, move || {
            let seen = seen.clone();
            div()
                .w(px(828.))
                .child(
                    AlertBanner::new(answered()).on_dismiss(move |_, _| seen.set(seen.get() + 1)),
                )
                .into_any_element()
        });
        assert_control_feedback(cx, "banner:dismiss", "banner:dismiss", 1);
        press(cx, "enter");
        assert_eq!(dismissed.get(), 1);
        press(cx, "space");
        assert_eq!(dismissed.get(), 2);
        click(cx, "banner:dismiss");
        assert_eq!(dismissed.get(), 3, "close must not bubble to the banner");
        click(cx, "banner");
        assert_eq!(
            dismissed.get(),
            4,
            "the existing banner click must still dismiss"
        );
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
