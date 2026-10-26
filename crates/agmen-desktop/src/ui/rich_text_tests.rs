use std::rc::Rc;

use gpui::{AnyElement, Context, Render, TestAppContext, VisualTestContext, Window, div, px};

use super::*;
use crate::backend::chat_snapshot;
use crate::refs::RefTarget;
use crate::ui::chat_context::ChatContext;
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
fn long_glue_becomes_a_word_of_its_own() {
    // Only short punctuation sticks to a chip; a long token beside one
    // wraps as a word (review finding on 500 characters glued to a ref).
    let tail = format!(",{}", "x".repeat(500));
    let body = format!("4d9fdc8{tail}");
    assert_eq!(
        pieces(&body, &[span(0..7, commit())]),
        [
            Piece::Ref {
                lead: "",
                text: "4d9fdc8",
                tail: "",
                span: 0
            },
            Piece::Word(&tail),
        ]
    );
    let lead = format!("build-{}", "y".repeat(500));
    let body = format!("{lead}4d9fdc8");
    let start = lead.len();
    assert_eq!(
        pieces(&body, &[span(start..start + 7, commit())]),
        [
            Piece::Word(&lead),
            Piece::Ref {
                lead: "",
                text: "4d9fdc8",
                tail: "",
                span: 0
            },
        ]
    );
}

#[test]
fn chip_free_lines_keep_their_indent() {
    // C228: indented logs and code keep each line's leading whitespace; a
    // tab draws as four spaces.
    let body = "  panicked:\n    at main.rs:3\n\tat lib.rs:9\n4d9fdc8 is the cause";
    let start = body.find("4d9fdc8").unwrap();
    let drawn = blocks(lines(pieces(body, &[span(start..start + 7, commit())])));
    assert_eq!(
        drawn[0],
        Block::Text("  panicked:\n    at main.rs:3\n    at lib.rs:9".into())
    );
    assert!(matches!(&drawn[1], Block::Row(items) if items.len() == 2));
}

#[test]
fn words_between_chips_draw_as_one_run() {
    let body = "see the 4d9fdc8 commit now";
    let flowed = lines(pieces(body, &[span(8..15, commit())]));
    assert_eq!(flowed.len(), 1);
    assert!(matches!(&flowed[0][0], Item::Run(run) if run == "see the"));
    assert!(matches!(flowed[0][1], Item::Ref { span: 0, .. }));
    assert!(matches!(&flowed[0][2], Item::Run(run) if run == "commit now"));
    assert_eq!(flowed[0].len(), 3);
}

/// A 32 KiB body of short lines of words, with no references.
fn short_lines_body() -> String {
    let body = "a few short words here\n".repeat(32 * 1024 / 23);
    assert!(body.len() <= agmen_proto::MAX_BODY_BYTES, "{}", body.len());
    body
}

#[test]
fn a_32_kib_body_in_short_lines_draws_as_one_text() {
    let body = short_lines_body();
    let drawn = lines(pieces(&body, &[]));
    let items: usize = drawn.iter().map(Vec::len).sum();
    // One run per line rather than one element per word, and the lines,
    // having no chips, join into a single text.
    assert_eq!(items, body.lines().count());
    assert!(items * 5 <= body.split_whitespace().count());
    assert!(matches!(blocks(drawn).as_slice(), [Block::Text(_)]));
}

#[test]
fn only_lines_with_chips_draw_as_rows() {
    let body = "a\n\nsee 4d9fdc8\nc d\ne";
    let start = body.find('4').unwrap();
    let drawn = blocks(lines(pieces(body, &[span(start..start + 7, commit())])));
    assert_eq!(drawn.len(), 3);
    assert_eq!(drawn[0], Block::Text("a\n".into()));
    assert!(matches!(&drawn[1], Block::Row(items) if items.len() == 2));
    assert_eq!(drawn[2], Block::Text("c d\ne".into()));
}

#[test]
fn a_long_line_draws_as_few_runs() {
    // A 32 KiB body of 16,384 words: one run, not 16,384 elements.
    let body = "a ".repeat(16_384);
    let one = lines(pieces(&body, &[]));
    assert_eq!(one.len(), 1);
    assert_eq!(one[0].len(), 1);
    assert!(matches!(&one[0][0], Item::Run(run) if run.len() == 16_384 * 2 - 1));

    // With a chip in the middle: the words before it, the chip, the rest.
    let mut body = "a ".repeat(8_000);
    let start = body.len();
    body.push_str("4d9fdc8 ");
    body.push_str(&"b ".repeat(8_000));
    let three = lines(pieces(&body, &[span(start..start + 7, commit())]));
    assert_eq!(three[0].len(), 3);
    assert!(matches!(three[0][1], Item::Ref { span: 0, .. }));
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
    let spans = context.body_text(body, &m.refs).spans;
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
    let text = Rc::new(context.body_text(m.client.body.as_str(), &m.refs));
    let cx = host(cx, px(700.), move || {
        RichText::new("m2", text.clone(), context.clone()).into_any_element()
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
    let text = Rc::new(context.body_text(&token, &[]));
    let cx = host(cx, px(300.), move || {
        RichText::new("m", text.clone(), context.clone()).into_any_element()
    });
    let body = cx.debug_bounds("body").unwrap();
    assert!(body.size.width <= px(300.), "{:?}", body.size);
    // 500 characters at about 7 px is several lines at 300 px.
    assert!(body.size.height > px(60.), "{:?}", body.size);
}

#[gpui::test]
fn chip_with_a_long_tail_wraps_within_the_width(cx: &mut TestAppContext) {
    let body = format!("4d9fdc8,{}", "x".repeat(500));
    let context = Rc::new(ChatContext::new(Rc::new(chat_snapshot())));
    let text = Rc::new(context.body_text(&body, &[]));
    assert_eq!(text.spans.len(), 1, "4d9fdc8 is a fixture commit");
    let cx = host(cx, px(300.), move || {
        RichText::new("m", text.clone(), context.clone()).into_any_element()
    });
    let body = cx.debug_bounds("body").unwrap();
    assert!(body.size.width <= px(300.), "{:?}", body.size);
    assert!(body.size.height > px(60.), "{:?}", body.size);
    let chip = cx.debug_bounds("m:ref:0").unwrap();
    assert!(chip.right() <= body.right(), "{chip:?} in {body:?}");
}

#[gpui::test]
fn chip_wider_than_the_column_wraps_inside_it(cx: &mut TestAppContext) {
    let path = "crates/agmen-desktop/src/ui/components/key_hint.rs:10-20";
    let mut snapshot = chat_snapshot();
    snapshot
        .git
        .files
        .insert("crates/agmen-desktop/src/ui/components/key_hint.rs".into());
    let context = Rc::new(ChatContext::new(Rc::new(snapshot)));
    let text = Rc::new(context.body_text(&format!("see {path}."), &[]));
    assert_eq!(text.spans.len(), 1, "the path is a known file");
    let cx = host(cx, px(150.), move || {
        RichText::new("m", text.clone(), context.clone()).into_any_element()
    });
    let body = cx.debug_bounds("body").unwrap();
    let chip = cx.debug_bounds("m:ref:0").unwrap();
    assert!(chip.right() <= body.right(), "{chip:?} in {body:?}");
    assert!(chip.size.height > px(20.), "the chip wraps: {chip:?}");
}

/// The mean time to draw a frame of `body` in a 600 px column.
fn frame_time(cx: &mut TestAppContext, body: &str) -> std::time::Duration {
    let context = Rc::new(ChatContext::new(Rc::new(chat_snapshot())));
    let text = Rc::new(context.body_text(body, &[]));
    let cx = host(cx, px(600.), move || {
        RichText::new("m", text.clone(), context.clone()).into_any_element()
    });
    let started = std::time::Instant::now();
    for _ in 0..5 {
        redraw(cx);
    }
    started.elapsed() / 5
}

/// Plan Focus 4: no frame over 16.7 ms.
const FRAME_BUDGET: std::time::Duration = std::time::Duration::from_micros(16_700);

// a_32_kib_body_in_short_lines_draws_as_one_text is the gate; wall-clock
// time swings with machine load, so this timing runs only on request.
#[gpui::test]
#[ignore = "wall-clock timing; run with cargo nextest run --release --run-ignored only a_32_kib_body_draws_quickly"]
fn a_32_kib_body_draws_quickly(cx: &mut TestAppContext) {
    // Measured on an M2 Max, three runs each: release 0.4 ms for one line
    // and 0.9 ms for 1,424 short lines; debug 8 ms and 12 ms. In release a
    // body gets a quarter of the frame budget. Debug runs about 13 times
    // slower, so its bound is 50 ms: 4 times the debug frame for a loaded
    // test run, and under the 148 ms of one element per line and the 552 ms
    // of one element per word that this replaced.
    let bound = if cfg!(debug_assertions) {
        std::time::Duration::from_millis(50)
    } else {
        FRAME_BUDGET / 4
    };
    let one_line = frame_time(cx, &"a ".repeat(16_384));
    let short_lines = frame_time(&mut TestAppContext::single(), &short_lines_body());
    for (shape, per_frame) in [("one line", one_line), ("short lines", short_lines)] {
        assert!(per_frame < bound, "{shape}: {per_frame:?} over {bound:?}");
    }
}
