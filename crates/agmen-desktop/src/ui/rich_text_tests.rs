use std::rc::Rc;

use gpui::{AnyElement, Context, Render, TestAppContext, VisualTestContext, Window, div, px};

use super::*;
use crate::backend::chat_snapshot;
use crate::refs::RefTarget;
use crate::ui::test_support::{init, redraw};
use crate::ui::tokens::MOCKUP_REM;

fn span(range: std::ops::Range<usize>, target: RefTarget) -> Span {
    Span { range, target }
}

fn commit() -> RefTarget {
    RefTarget::Commit("4d9fdc8".into())
}

#[test]
fn punctuation_after_a_chip_stays_with_it() {
    let body = "see 4d9fdc8. now";
    assert_eq!(
        pieces(body, &[span(4..11, commit())]),
        [
            Piece::Word("see"),
            Piece::Ref {
                lead: "",
                text: "4d9fdc8",
                tail: ".",
                span: 0
            },
            Piece::Word("now"),
        ]
    );
}

#[test]
fn punctuation_before_a_chip_stays_with_it() {
    let body = "(4d9fdc8)";
    assert_eq!(
        pieces(body, &[span(1..8, commit())]),
        [Piece::Ref {
            lead: "(",
            text: "4d9fdc8",
            tail: ")",
            span: 0
        }]
    );
}

#[test]
fn a_tail_stops_at_the_next_chip() {
    let body = "4d9fdc8,016f235";
    let spans = [
        span(0..7, commit()),
        span(8..15, RefTarget::Commit("016f235".into())),
    ];
    assert_eq!(
        pieces(body, &spans),
        [
            Piece::Ref {
                lead: "",
                text: "4d9fdc8",
                tail: ",",
                span: 0
            },
            Piece::Ref {
                lead: "",
                text: "016f235",
                tail: "",
                span: 1
            },
        ]
    );
}

#[test]
fn line_breaks_are_kept() {
    assert_eq!(
        pieces("a\n\nb c", &[]),
        [
            Piece::Word("a"),
            Piece::LineBreak,
            Piece::LineBreak,
            Piece::Word("b"),
            Piece::Word("c"),
        ]
    );
}

#[test]
fn multibyte_text_around_a_chip_is_never_split() {
    // "é" is two bytes; the span starts after it and the tail is one.
    let body = "é 4d9fdc8é ü";
    let start = body.find('4').unwrap();
    let pieces = pieces(body, &[span(start..start + 7, commit())]);
    assert_eq!(
        pieces,
        [
            Piece::Word("é"),
            Piece::Ref {
                lead: "",
                text: "4d9fdc8",
                tail: "é",
                span: 0
            },
            Piece::Word("ü"),
        ]
    );
}

#[test]
fn spans_out_of_order_or_off_a_boundary_are_skipped() {
    // find_refs never returns these; pieces stays safe if it ever did.
    let body = "é abc";
    assert_eq!(
        pieces(body, &[span(1..3, commit()), span(10..12, commit())]),
        [Piece::Word("é"), Piece::Word("abc")]
    );
}

#[test]
fn fixture_alert_body_pieces() {
    let snapshot = chat_snapshot();
    let context = ChatContext::new(Rc::new(snapshot.clone()));
    let m = &snapshot.messages[1];
    let body = m.client.body.as_str();
    let spans = context.spans(body, &m.refs);
    let refs: Vec<(&str, &str, &str)> = pieces(body, &spans)
        .into_iter()
        .filter_map(|p| match p {
            Piece::Ref {
                lead, text, tail, ..
            } => Some((lead, text, tail)),
            _ => None,
        })
        .collect();
    assert_eq!(
        refs,
        [
            ("", "4d9fdc8", ""),
            ("", "src/main.rs:2-8", "."),
            ("", "@sean/a1", "")
        ]
    );
}

/// Draws one body in a column `width` wide.
struct Host {
    width: gpui::Pixels,
    make: Box<dyn Fn() -> AnyElement>,
}

impl Render for Host {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        div()
            .w(self.width)
            .flex()
            .flex_col()
            .child(div().debug_selector(|| "body".into()).child((self.make)()))
    }
}

fn host(
    cx: &mut TestAppContext,
    width: gpui::Pixels,
    make: impl Fn() -> AnyElement + 'static,
) -> &mut VisualTestContext {
    init(cx);
    let (_, cx) = cx.add_window_view(move |_, _| Host {
        width,
        make: Box::new(make),
    });
    redraw(cx);
    cx
}

#[gpui::test]
fn chips_draw_for_each_span(cx: &mut TestAppContext) {
    let snapshot = chat_snapshot();
    let m = snapshot.messages[1].clone();
    let context = Rc::new(ChatContext::new(Rc::new(snapshot)));
    let cx = host(cx, px(700.), move || {
        RichText::new("m2", m.client.body.as_str(), &m.refs, context.clone()).into_any_element()
    });
    for i in 0..3 {
        let selector: &'static str = Box::leak(format!("m2:ref:{i}").into_boxed_str());
        assert!(cx.debug_bounds(selector).is_some(), "{selector} not drawn");
    }
    assert!(cx.debug_bounds("m2:ref:3").is_none());
}

#[gpui::test]
fn long_token_wraps_within_the_width(cx: &mut TestAppContext) {
    let token = "x".repeat(500);
    let context = Rc::new(ChatContext::new(Rc::new(chat_snapshot())));
    let cx = host(cx, px(300.), move || {
        RichText::new("m", token.as_str(), &[], context.clone()).into_any_element()
    });
    let body = cx.debug_bounds("body").unwrap();
    assert!(body.size.width <= px(300.), "{:?}", body.size);
    // 500 characters at about 7 px is several lines at 300 px.
    assert!(body.size.height > px(60.), "{:?}", body.size);
}
