use gpui::{
    Context, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext, Window, div, px,
    relative, size,
};

use super::*;
use crate::ui::test_support::{init, redraw};
use crate::ui::tokens::{LINE_HEIGHT, MOCKUP_REM, SPACE_3, SPACE_4, TEXT_DEFAULT};

const ADVANCE: Pixels = px(10.);
const LINE: Pixels = px(16.);

fn run(text: &str) -> Seg {
    Seg::Run(Glyphs::monospace(text, ADVANCE))
}

fn atom(width: f32, height: f32) -> Seg {
    Seg::Atom(size(px(width), px(height)))
}

/// Lays out `segs` at `width`; an atom wider than the line is 40 px tall.
fn lay_out(segs: &[Seg], width: Option<f32>) -> Flow {
    flow(segs, width.map(px), LINE, |_, _| px(40.))
}

/// The text of each placed piece of a run, with its line.
fn texts<'a>(segs: &'a [Seg], laid: &Flow) -> Vec<(usize, &'a str)> {
    laid.placed
        .iter()
        .filter_map(|placed| match &segs[placed.seg] {
            Seg::Run(glyphs) => Some((placed.line, &glyphs.text[placed.bytes.clone()])),
            Seg::Atom(_) => None,
        })
        .collect()
}

#[test]
fn a_run_after_an_atom_fills_the_rest_of_the_line() {
    let segs = [atom(30., 18.), run("aa bb cc dd")];
    let laid = lay_out(&segs, Some(100.));
    // "aa bb" ends at 30 + 4 + 50 = 84; "aa bb cc" would end at 114.
    assert_eq!(texts(&segs, &laid), [(0, "aa bb"), (1, "cc dd")]);
    let first = &laid.placed[1];
    assert_eq!(first.bounds.origin.x, px(30.) + SPACE_4);
}

#[test]
fn a_first_word_that_does_not_fit_moves_down_whole() {
    let segs = [atom(90., 18.), run("aaaa b")];
    let laid = lay_out(&segs, Some(100.));
    assert_eq!(texts(&segs, &laid), [(1, "aaaa b")]);
    assert_eq!(laid.placed[1].bounds.origin.x, px(0.));
}

#[test]
fn a_wrapped_line_never_starts_or_ends_with_a_space() {
    let segs = [run("aaa bbb ccc ddd eee")];
    for width in [35., 40., 45., 60., 70., 75., 110.] {
        let laid = lay_out(&segs, Some(width));
        for (_, text) in texts(&segs, &laid) {
            assert!(
                !text.starts_with(' ') && !text.ends_with(' '),
                "{text:?} at {width}"
            );
        }
    }
}

#[test]
fn a_break_on_the_separator_space_moves_to_the_next_word() {
    // "ab" fits in 25 px, its space would end at 30, "c" at 40.
    let segs = [run("ab cd")];
    let laid = lay_out(&segs, Some(25.));
    assert_eq!(texts(&segs, &laid), [(0, "ab"), (1, "cd")]);
    assert_eq!(laid.placed[0].bytes, 0..2);
    assert_eq!(laid.placed[1].bytes, 3..5);
    assert_eq!(laid.placed[0].bounds.size.width, px(20.));
}

#[test]
fn a_word_wider_than_the_line_breaks_where_it_overflows() {
    let segs = [run(&"x".repeat(25))];
    let laid = lay_out(&segs, Some(100.));
    let lines: Vec<usize> = texts(&segs, &laid).iter().map(|(_, t)| t.len()).collect();
    assert_eq!(lines, [10, 10, 5]);
}

#[test]
fn an_atom_wider_than_the_line_sits_alone_at_the_line_width() {
    let segs = [run("aa"), atom(300., 18.), run("bb")];
    let laid = lay_out(&segs, Some(100.));
    let wide = &laid.placed[1];
    assert_eq!(wide.line, 1);
    assert_eq!(wide.bounds.origin.x, px(0.));
    assert_eq!(wide.bounds.size, size(px(100.), px(40.)));
    assert_eq!(texts(&segs, &laid), [(0, "aa"), (2, "bb")]);
}

#[test]
fn atoms_start_and_end_lines() {
    // An atom that does not fit after a run starts the next line; the run
    // before it ends its line with no trailing space.
    let segs = [run("aaaa bbbb"), atom(40., 18.), run("cc")];
    let laid = lay_out(&segs, Some(100.));
    assert_eq!(laid.placed[1].line, 1);
    assert_eq!(laid.placed[1].bounds.origin.x, px(0.));
    assert_eq!(texts(&segs, &laid), [(0, "aaaa bbbb"), (1, "cc")]);
    assert_eq!(laid.placed[2].bounds.origin.x, px(40.) + SPACE_4);
}

#[test]
fn multibyte_text_beside_an_atom_breaks_on_character_boundaries() {
    let text = "é ü 中文字 ö";
    let segs = [atom(20., 18.), run(text), atom(20., 18.)];
    for width in [25., 30., 45., 60., 80., 200.] {
        let laid = lay_out(&segs, Some(width));
        let mut at = 0;
        for placed in &laid.placed {
            if placed.seg != 1 {
                continue;
            }
            assert!(text.is_char_boundary(placed.bytes.start), "{placed:?}");
            assert!(text.is_char_boundary(placed.bytes.end), "{placed:?}");
            assert!(placed.bytes.start >= at, "{placed:?} before {at}");
            at = placed.bytes.end;
        }
        assert_eq!(at, text.len(), "the run is placed to its end at {width}");
    }
}

#[test]
fn ideographs_break_between_characters() {
    let segs = [run("中文字体")];
    let laid = lay_out(&segs, Some(25.));
    assert_eq!(texts(&segs, &laid), [(0, "中文"), (1, "字体")]);
}

#[test]
fn line_boxes_fit_their_tallest_item_and_center_the_rest() {
    let segs = [run("aa"), atom(20., 24.), run("bbbbbbbb")];
    let laid = lay_out(&segs, Some(100.));
    assert_eq!(laid.lines.len(), 2);
    assert_eq!(laid.lines[0].height, px(24.));
    assert_eq!(laid.lines[1].top, px(24.) + SPACE_3);
    assert_eq!(laid.lines[1].height, LINE);
    assert_eq!(laid.placed[0].bounds.origin.y, px(4.));
    assert_eq!(laid.size.height, px(24.) + SPACE_3 + LINE);
}

#[test]
fn without_a_width_everything_is_one_line() {
    let segs = [run("aa bb"), atom(300., 18.), run("cc")];
    let laid = lay_out(&segs, None);
    assert_eq!(laid.lines.len(), 1);
    assert_eq!(
        laid.size.width,
        px(50.) + SPACE_4 + px(300.) + SPACE_4 + px(20.)
    );
}

#[test]
fn nothing_passes_the_line_width() {
    let segs = [
        run("main does not compile:"),
        atom(57., 18.),
        run("committed conflict markers at"),
        atom(301., 18.),
        run(&format!("{} tail", "y".repeat(40))),
    ];
    for width in 5..60 {
        let width = 10. * width as f32;
        let laid = lay_out(&segs, Some(width));
        for placed in &laid.placed {
            assert!(
                placed.bounds.right() <= px(width) || placed.bytes.len() == 1,
                "{placed:?} at {width}"
            );
        }
    }
}

/// Draws an [`InlineFlow`] of `items()` in the body text style.
struct Host {
    width: Pixels,
    items: Box<dyn Fn() -> Vec<FlowItem>>,
}

impl Render for Host {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        div()
            .w(self.width)
            .text_size(TEXT_DEFAULT)
            .line_height(relative(LINE_HEIGHT))
            .child(InlineFlow::new((self.items)()))
    }
}

fn box_atom(label: &str) -> FlowItem {
    FlowItem::Chip {
        element: div().w(px(30.)).h(px(18.)).into_any_element(),
        label: label.to_owned().into(),
    }
}

#[gpui::test]
fn shaped_runs_split_at_every_boundary_kind(cx: &mut TestAppContext) {
    // Multibyte characters next to atoms, breaks on separator spaces, and
    // atoms at the start and end of lines: take_until panics on a byte
    // that is not a character boundary or that moves backwards.
    let runs = ["é", "ü ö é ä ß ç ñ å ø", "中文字体 abc", "日本"];
    init(cx);
    for width in [40., 60., 90., 130., 400.] {
        let (_, cx) = cx.add_window_view(move |_, _| Host {
            width: px(width),
            items: Box::new(move || {
                let mut items: Vec<FlowItem> = Vec::new();
                for text in runs {
                    items.push(box_atom("x"));
                    items.push(FlowItem::Run(text.into()));
                }
                items.push(box_atom("y"));
                items
            }),
        });
        take_drawn();
        redraw(cx);
        let drawn = take_drawn().pop().unwrap_or_default();
        let joined: String = drawn
            .iter()
            .filter(|d| !d.chip)
            .map(|d| d.text.as_ref())
            .collect::<Vec<_>>()
            .join(" ");
        let expected = runs.join(" ");
        // Ideographs break with no space, so compare without spaces.
        assert_eq!(
            joined.replace(' ', ""),
            expected.replace(' ', ""),
            "at {width}"
        );
        assert_eq!(drawn.iter().filter(|d| d.chip).count(), runs.len() + 1);
    }
}
