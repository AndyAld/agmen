use std::rc::Rc;

use gpui::{
    AnyElement, Context, Modifiers, Render, TestAppContext, VisualTestContext, Window, div, px,
};

use super::*;
use crate::backend::chat_snapshot;
use crate::refs::RefTarget;
use crate::ui::chat_context::ChatContext;
use crate::ui::inline_flow::{Drawn, take_drawn, take_estimates};
use crate::ui::ref_chip::REF_CARD_SELECTOR;
use crate::ui::test_support::{click, init, redraw, rendered_size};
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
fn a_chip_lines_indent_reaches_the_flow() {
    // C228 with the inline flow: an indented line with a chip keeps its
    // indent, a tab as four spaces, in the run the flow draws.
    let body = "\tsee 4d9fdc8 now";
    let start = body.find("4d9fdc8").unwrap();
    let drawn = blocks(lines(pieces(body, &[span(start..start + 7, commit())])));
    let [Block::Row(items)] = drawn.as_slice() else {
        panic!("{drawn:?}");
    };
    assert_eq!(items[0], Item::Run("    see".into()));
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

/// The height of `body` drawn in a column `width` wide, and the height of
/// its first chip.
fn heights(cx: &mut TestAppContext, width: f32, body: &str) -> (gpui::Pixels, gpui::Pixels) {
    let context = Rc::new(ChatContext::new(Rc::new(chat_snapshot())));
    let text = Rc::new(context.body_text(body, &[]));
    let cx = host(cx, px(width), move || {
        RichText::new("m", text.clone(), context.clone()).into_any_element()
    });
    let body = cx.debug_bounds("body").unwrap().size.height;
    let chip = cx.debug_bounds("m:ref:0").map_or(px(0.), |b| b.size.height);
    (body, chip)
}

#[gpui::test]
fn a_run_after_a_chip_fills_the_rest_of_the_line(cx: &mut TestAppContext) {
    // About one and a half lines of words after a chip, at 300 px: the
    // words fill the chip's line and wrap once, two lines in all. The bug
    // drew the chip alone, then the words below it in two more lines.
    let words = "the quick brown fox jumps over the lazy dog and runs on far away";
    let (plain, _) = heights(cx, 300., words);
    let (flowed, chip) = heights(
        &mut TestAppContext::single(),
        300.,
        &format!("4d9fdc8 {words}"),
    );
    let line = (plain - SPACE_3) / 2.;
    assert!(plain > line * 1.5, "the words alone wrap once: {plain:?}");
    let two_lines = chip.max(line) * 2. + SPACE_3 + px(0.5);
    assert!(flowed <= two_lines, "{flowed:?} over {two_lines:?}");
}

/// The fixture message whose body contains `needle`, drawn as `"m"` in a
/// column `width` wide, with what its chip paragraph laid out.
fn fixture_flow<'a>(
    cx: &'a mut TestAppContext,
    width: f32,
    needle: &str,
) -> (Vec<Drawn>, &'a mut VisualTestContext) {
    let snapshot = chat_snapshot();
    let m = snapshot
        .messages
        .iter()
        .find(|m| m.client.body.as_str().contains(needle))
        .unwrap()
        .clone();
    let context = Rc::new(ChatContext::new(Rc::new(snapshot)));
    let text = Rc::new(context.body_text(m.client.body.as_str(), &m.refs));
    let cx = host(cx, px(width), move || {
        RichText::new("m", text.clone(), context.clone()).into_any_element()
    });
    let drawn = take_drawn().pop().unwrap_or_default();
    (drawn, cx)
}

/// The line of the first piece of text that starts with `start`.
fn text_line(drawn: &[Drawn], start: &str) -> usize {
    drawn
        .iter()
        .find(|d| !d.chip && d.text.starts_with(start))
        .unwrap_or_else(|| panic!("no text starts with {start:?} in {drawn:#?}"))
        .line
}

/// The line of the chip labelled `label`.
fn chip_line(drawn: &[Drawn], label: &str) -> usize {
    drawn
        .iter()
        .find(|d| d.chip && d.text == label)
        .unwrap_or_else(|| panic!("no chip {label:?} in {drawn:#?}"))
        .line
}

#[gpui::test]
fn alert_text_flows_beside_its_chips(cx: &mut TestAppContext) {
    // The 15:28 ALERT in the chat shot: "please hold pushes" started a line
    // of its own under @sean/a1; the mockup flows it on.
    let (drawn, _) = fixture_flow(cx, 700., "please hold pushes");
    assert_eq!(chip_line(&drawn, "4d9fdc8"), 0);
    assert_eq!(text_line(&drawn, "committed conflict markers at"), 0);
    assert_eq!(
        text_line(&drawn, "please"),
        chip_line(&drawn, "@sean/a1"),
        "{drawn:#?}"
    );
}

#[gpui::test]
fn done_text_flows_beside_its_chips(cx: &mut TestAppContext) {
    // The 15:41 DONE: "· cargo test 71 passed" started a new line.
    let (drawn, _) = fixture_flow(cx, 700., "cargo test 71 passed");
    assert_eq!(
        text_line(&drawn, "· cargo test 71 passed"),
        chip_line(&drawn, "fix/c015-main-conflict@9e2a71c"),
        "{drawn:#?}"
    );
}

#[gpui::test]
fn nothing_drawn_passes_the_body_edge(_cx: &mut TestAppContext) {
    let snapshot = chat_snapshot();
    let mut checked = 0;
    for width in [300., 450., 700.] {
        for m in &snapshot.messages {
            let context = Rc::new(ChatContext::new(Rc::new(snapshot.clone())));
            let text = Rc::new(context.body_text(m.client.body.as_str(), &m.refs));
            let mut app = TestAppContext::single();
            let cx = host(&mut app, px(width), move || {
                RichText::new("m", text.clone(), context.clone()).into_any_element()
            });
            let body = cx.debug_bounds("body").unwrap();
            for drawn in take_drawn().into_iter().flatten() {
                checked += 1;
                assert!(
                    drawn.bounds.right() <= body.right() + px(0.5)
                        && drawn.bounds.left() >= body.left() - px(0.5),
                    "{drawn:?} outside {body:?} at {width}"
                );
            }
        }
    }
    assert!(checked > 20, "only {checked} pieces drawn");
}

#[gpui::test]
fn chips_draw_where_the_flow_put_them(_cx: &mut TestAppContext) {
    for width in [450., 700.] {
        let mut app = TestAppContext::single();
        let (drawn, cx) = fixture_flow(&mut app, width, "please hold pushes");
        let chips: Vec<&Drawn> = drawn.iter().filter(|d| d.chip).collect();
        assert_eq!(chips.len(), 3);
        for (i, placed) in chips.iter().enumerate() {
            let selector: &'static str = Box::leak(format!("m:ref:{i}").into_boxed_str());
            let chip = cx.debug_bounds(selector).unwrap();
            // The chip sits inside its holder, after any leading glue.
            let slack = px(1.);
            assert!(
                chip.left() >= placed.bounds.left() - slack
                    && chip.right() <= placed.bounds.right() + slack
                    && (chip.top() - placed.bounds.top()).abs() <= slack
                    && (chip.bottom() - placed.bounds.bottom()).abs() <= slack,
                "{selector} drawn at {chip:?}, placed at {:?}",
                placed.bounds
            );
        }
    }
}

#[gpui::test]
fn chip_height_estimate_matches_its_layout(_cx: &mut TestAppContext) {
    let path = "crates/agmen-desktop/src/ui/components/key_hint.rs:10-20";
    let mut snapshot = chat_snapshot();
    snapshot
        .git
        .files
        .insert("crates/agmen-desktop/src/ui/components/key_hint.rs".into());
    let bodies = [
        format!("see {path}."),
        format!("({path}) and more words after it"),
        "C015 fix/c015-main-conflict@9e2a71c · cargo test".to_owned(),
    ];
    let mut estimates = Vec::new();
    take_estimates();
    for width in [90., 120., 150., 200., 260.] {
        for body in &bodies {
            let context = Rc::new(ChatContext::new(Rc::new(snapshot.clone())));
            let text = Rc::new(context.body_text(body, &[]));
            let mut app = TestAppContext::single();
            host(&mut app, px(width), move || {
                RichText::new("m", text.clone(), context.clone()).into_any_element()
            });
            estimates.extend(take_estimates());
        }
    }
    assert!(estimates.len() >= 5, "{estimates:?}");
    for (estimated, laid_out) in estimates {
        assert!(
            (estimated - laid_out).abs() <= px(0.5),
            "estimated {estimated:?}, laid out {laid_out:?}"
        );
    }
}

#[gpui::test]
fn chips_in_a_flow_keep_their_size(cx: &mut TestAppContext) {
    // Padding and font come from the chip itself, so a chip drawn in the
    // flow is the size of the same chip drawn alone.
    let (_, cx) = fixture_flow(cx, 700., "please hold pushes");
    let in_flow = cx.debug_bounds("m:ref:2").unwrap().size;
    let snapshot = chat_snapshot();
    let m = &snapshot.messages[1];
    let context = ChatContext::new(Rc::new(snapshot.clone()));
    let target = context.body_text(m.client.body.as_str(), &m.refs).spans[2]
        .target
        .clone();
    let color = context.chip_color(&target);
    let alone = rendered_size(&mut TestAppContext::single(), move || {
        RefChip::new("chip", "@sean/a1", target.clone(), color.clone()).into_any_element()
    });
    assert_eq!(in_flow, alone);
}

type Heard = Rc<std::cell::RefCell<Vec<RefEvent>>>;

/// The fixture ALERT body with a handler that records chip events.
fn listening_body(cx: &mut TestAppContext) -> (Heard, RefTarget, &mut VisualTestContext) {
    let heard: Heard = Rc::default();
    let sink = heard.clone();
    let snapshot = chat_snapshot();
    let m = snapshot.messages[1].clone();
    let context = Rc::new(ChatContext::new(Rc::new(snapshot)));
    let text = Rc::new(context.body_text(m.client.body.as_str(), &m.refs));
    let target = text.spans[2].target.clone();
    let cx = host(cx, px(700.), move || {
        let sink = sink.clone();
        let handler: ChangeHandler<RefEvent> =
            Rc::new(move |event, _, _| sink.borrow_mut().push(event));
        RichText::new("m", text.clone(), context.clone())
            .on_ref(handler)
            .into_any_element()
    });
    (heard, target, cx)
}

#[gpui::test]
fn click_through_flow_opens_ref(cx: &mut TestAppContext) {
    let (heard, target, cx) = listening_body(cx);
    click(cx, "m:ref:2");
    assert_eq!(*heard.borrow(), [RefEvent::OpenRef(target)]);
}

#[gpui::test]
fn cmd_click_through_flow_opens_ref_in_split(cx: &mut TestAppContext) {
    let (heard, target, cx) = listening_body(cx);
    let center = cx.debug_bounds("m:ref:2").unwrap().center();
    let secondary = cx.update(|_, cx| Modifiers::secondary_key(cx));
    cx.simulate_click(center, secondary);
    assert_eq!(*heard.borrow(), [RefEvent::OpenRefInSplit(target)]);
}

#[gpui::test]
fn hover_through_flow_shows_the_card(cx: &mut TestAppContext) {
    let (_, _, cx) = listening_body(cx);
    assert!(cx.debug_bounds(REF_CARD_SELECTOR).is_none());
    let center = cx.debug_bounds("m:ref:2").unwrap().center();
    cx.simulate_mouse_move(center, None, Modifiers::none());
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    cx.run_until_parked();
    redraw(cx);
    assert!(cx.debug_bounds(REF_CARD_SELECTOR).is_some());
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

/// The time `per_frame` may take: `debug` in a debug build, else `release`.
fn bound(debug: u64, release: std::time::Duration) -> std::time::Duration {
    if cfg!(debug_assertions) {
        std::time::Duration::from_millis(debug)
    } else {
        release
    }
}

// a_32_kib_body_in_short_lines_draws_as_one_text is the gate; wall-clock
// time swings with machine load, so this timing runs only on request.
#[gpui::test]
#[ignore = "wall-clock timing; run with cargo nextest run --release --run-ignored only a_32_kib_body_draws_quickly"]
fn a_32_kib_body_draws_quickly(cx: &mut TestAppContext) {
    // Measured on an M2 Max, three runs each, release then debug: one line
    // 0.4 and 8 ms, 1,424 short lines 0.8 and 11 ms, one paragraph with a
    // chip in the middle 0.8 and 12 ms, a chip every 200 bytes 2.7 to 3.1
    // and 30 ms. In release a body gets a quarter of the frame budget.
    // Debug runs about 13 times slower, so a debug bound is 4 times the
    // debug frame for a loaded test run (a chip every 200 bytes took 87 ms
    // once in a full parallel run); the plain bodies' 50 ms is also under
    // the 148 ms of one element per line and the 552 ms of one element per
    // word that this replaced.
    let quarter = FRAME_BUDGET / 4;
    let chips_every = |bytes: usize| {
        let words = "a ".repeat((bytes - 8) / 2);
        format!("{words}4d9fdc8 ").repeat(32_000 / bytes)
    };
    let one_chip = format!("{}4d9fdc8 {}", "a ".repeat(8_000), "b ".repeat(8_000));
    let shapes = [
        (
            "one line",
            frame_time(cx, &"a ".repeat(16_384)),
            bound(50, quarter),
        ),
        (
            "short lines",
            frame_time(&mut TestAppContext::single(), &short_lines_body()),
            bound(50, quarter),
        ),
        (
            "one chip",
            frame_time(&mut TestAppContext::single(), &one_chip),
            bound(50, quarter),
        ),
        (
            "a chip per 200 bytes",
            frame_time(&mut TestAppContext::single(), &chips_every(200)),
            bound(120, quarter),
        ),
        // A chip every 32 bytes, 1,000 in all, is over the frame budget:
        // each chip is its own element tree, laid out at about 7 us in
        // release. Measured 13 to 15 ms in release and 130 to 137 ms in
        // debug; the flex row this replaced took 21 to 31 ms in release.
        // Rows are cached, so it costs only when the stream redraws. The
        // bound guards against it getting worse.
        (
            "a chip per 32 bytes",
            frame_time(&mut TestAppContext::single(), &chips_every(32)),
            bound(550, FRAME_BUDGET * 2),
        ),
    ];
    for (shape, per_frame, bound) in shapes {
        assert!(per_frame < bound, "{shape}: {per_frame:?} over {bound:?}");
    }
}
