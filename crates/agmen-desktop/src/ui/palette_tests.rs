use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Entity, Rems, TestAppContext, VisualTestContext, px};

use super::*;
use crate::keymap::{OpenSettings, Quit, SpawnWorkerWith, ToggleLeftDock};
use crate::ui::test_support::{click, init, redraw};
use crate::ui::tokens::{MOCKUP_REM, PALETTE_INPUT_LINE, PALETTE_QUERY_TEXT, TEXT_DEFAULT};

type Events = Rc<RefCell<Vec<String>>>;

fn entries() -> Vec<PaletteEntry> {
    let spawn = |agent: &str| {
        PaletteEntry::new(
            format!("agmen: spawn worker with {agent}"),
            Box::new(SpawnWorkerWith {
                agent: agent.to_owned().into(),
            }),
        )
    };
    vec![
        spawn("claude"),
        spawn("codex"),
        PaletteEntry::new("workspace: toggle left dock", Box::new(ToggleLeftDock))
            .keys(Some("⌘B".into())),
        PaletteEntry::new("agmen: open settings", Box::new(OpenSettings)),
        PaletteEntry::new("agmen: quit", Box::new(Quit)),
    ]
}

/// A palette over `entries()`, focused, with its events as text.
fn open(cx: &mut TestAppContext) -> (Entity<CommandPalette>, Events, &mut VisualTestContext) {
    init(cx);
    cx.update(crate::keymap::bind_all);
    let (palette, cx) = cx.add_window_view(|window, cx| CommandPalette::new(entries(), window, cx));
    let events: Events = Rc::default();
    let sink = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&palette, move |_, event: &PaletteEvent, _| {
            sink.borrow_mut().push(match event {
                PaletteEvent::Dismissed => "dismissed".to_owned(),
                PaletteEvent::Confirmed(action) => action.name().to_owned(),
            });
        })
        .detach();
    });
    redraw(cx);
    (palette, events, cx)
}

fn shown(palette: &Entity<CommandPalette>, cx: &VisualTestContext) -> Vec<String> {
    palette.read_with(cx, |p, _| p.shown().map(str::to_owned).collect())
}

fn selected(palette: &Entity<CommandPalette>, cx: &VisualTestContext) -> Option<String> {
    palette.read_with(cx, |p, _| p.selected_label().map(str::to_owned))
}

#[gpui::test]
fn palette_filters_fuzzy(cx: &mut TestAppContext) {
    let (palette, _, cx) = open(cx);
    cx.simulate_input("swc");
    redraw(cx);
    let rows = shown(&palette, cx);
    assert!(rows.contains(&"agmen: spawn worker with codex".to_owned()));
    assert!(!rows.iter().any(|r| r.contains("left dock")), "{rows:?}");
    // The drawn rows are the matches, and codex's match is s, w, c.
    assert!(cx.debug_bounds("palette-row:1").is_some());
    assert!(cx.debug_bounds("palette-row:2").is_none());
    let label = "agmen: spawn worker with codex";
    let hits: Vec<String> = palette.read_with(cx, |p, _| {
        p.ranges_of(label)
            .unwrap()
            .iter()
            .map(|r| label[r.clone()].to_owned())
            .collect()
    });
    assert_eq!(hits, ["s", "w", "c"]);
}

#[gpui::test]
fn empty_query_lists_every_entry_in_order(cx: &mut TestAppContext) {
    let (palette, _, cx) = open(cx);
    let labels: Vec<String> = entries().iter().map(|e| e.label.to_string()).collect();
    assert_eq!(shown(&palette, cx), labels);
    assert_eq!(selected(&palette, cx).as_deref(), Some(labels[0].as_str()));
    assert!(cx.debug_bounds("palette-row:4").is_some());
    assert!(cx.debug_bounds("palette-empty").is_none());
}

#[gpui::test]
fn no_match_shows_one_row_and_enter_dismisses(cx: &mut TestAppContext) {
    let (palette, events, cx) = open(cx);
    cx.simulate_input("zzzz");
    redraw(cx);
    assert!(shown(&palette, cx).is_empty());
    assert!(cx.debug_bounds("palette-empty").is_some());
    assert!(cx.debug_bounds("palette-row:0").is_none());
    cx.simulate_keystrokes("enter");
    assert_eq!(*events.borrow(), ["dismissed"]);
}

#[gpui::test]
fn arrows_wrap_and_enter_runs_the_selected_row(cx: &mut TestAppContext) {
    let (palette, events, cx) = open(cx);
    cx.simulate_keystrokes("up");
    assert_eq!(selected(&palette, cx).as_deref(), Some("agmen: quit"));
    cx.simulate_keystrokes("down");
    cx.simulate_keystrokes("down");
    assert_eq!(
        selected(&palette, cx).as_deref(),
        Some("agmen: spawn worker with codex")
    );
    cx.simulate_keystrokes("enter");
    assert_eq!(*events.borrow(), ["agmen::SpawnWorkerWith"]);
}

#[gpui::test]
fn selection_clamps_when_matches_shrink(cx: &mut TestAppContext) {
    // Zed keeps the index and clamps it to the last match.
    let (palette, events, cx) = open(cx);
    for _ in 0..3 {
        cx.simulate_keystrokes("down");
    }
    assert_eq!(
        selected(&palette, cx).as_deref(),
        Some("agmen: open settings")
    );
    cx.simulate_input("spawn");
    assert_eq!(shown(&palette, cx).len(), 2);
    assert_eq!(
        selected(&palette, cx).as_deref(),
        Some("agmen: spawn worker with codex")
    );
    cx.simulate_keystrokes("enter");
    assert_eq!(*events.borrow(), ["agmen::SpawnWorkerWith"]);
}

#[gpui::test]
fn escape_dismisses_and_click_runs_a_row(cx: &mut TestAppContext) {
    let (_, events, cx) = open(cx);
    cx.simulate_keystrokes("escape");
    assert_eq!(*events.borrow(), ["dismissed"]);
    click(cx, "palette-row:2");
    assert_eq!(*events.borrow(), ["dismissed", "workspace::ToggleLeftDock"]);
}

#[gpui::test]
fn set_query_refilters(cx: &mut TestAppContext) {
    let (palette, _, cx) = open(cx);
    palette.update_in(cx, |p, window, cx| {
        p.set_query("spawn worker with ", window, cx)
    });
    redraw(cx);
    assert_eq!(
        shown(&palette, cx),
        [
            "agmen: spawn worker with claude",
            "agmen: spawn worker with codex"
        ]
    );
    palette.read_with(cx, |p, _| assert_eq!(p.query(), "spawn worker with "));
}

#[gpui::test]
fn the_query_caret_is_the_mockups_18_px(cx: &mut TestAppContext) {
    // Node 11481: a 2 by 18 caret in the 39 px query row. The input took
    // the row's 1.2 line, 16 px at a 15 px rem.
    let (palette, _, cx) = open(cx);
    let caret = palette.read_with(cx, |p, cx| p.input.read(cx).last_cursor.unwrap());
    assert_eq!(caret.size.height, PALETTE_INPUT_LINE);
}

#[gpui::test]
fn the_query_is_14_px_one_step_above_the_rows(cx: &mut TestAppContext) {
    // Node 11480: the query "spawn" is SF Pro Text 14, the rows 13. The
    // caret ends the text, so its x from the input's left is the text's
    // width at the query's size.
    let (palette, _, cx) = open(cx);
    cx.update(|window, _| window.set_rem_size(MOCKUP_REM));
    palette.update_in(cx, |p, window, cx| p.set_query("spawn", window, cx));
    redraw(cx);
    let (input, caret) = palette.read_with(cx, |p, cx| {
        let input = p.input.read(cx);
        (input.last_bounds.unwrap(), input.last_cursor.unwrap())
    });
    let drawn = caret.left() - input.left();
    let (query, row) = cx.update(|window, _| {
        let run = window.text_style().to_run("spawn".len());
        let width = |size: Rems| {
            let size = size.to_pixels(MOCKUP_REM);
            window
                .text_system()
                .shape_line("spawn".into(), size, std::slice::from_ref(&run), None)
                .width()
        };
        (width(PALETTE_QUERY_TEXT), width(TEXT_DEFAULT))
    });
    assert!(row < query, "{row:?} {query:?}");
    assert!(
        (drawn - query).abs() < px(0.5),
        "{drawn:?} against {query:?}"
    );
}

#[gpui::test]
fn a_row_with_a_note_draws_it_at_the_end(cx: &mut TestAppContext) {
    // C261: an outdated agent's spawn row says what to update.
    init(cx);
    let (_, cx) = cx.add_window_view(|window, cx| {
        let entries = vec![
            PaletteEntry::new("agmen: quit", Box::new(Quit)),
            PaletteEntry::new(
                "agmen: spawn worker with agy",
                Box::new(SpawnWorkerWith {
                    agent: "agy".into(),
                }),
            )
            .note(Some(("update to 1.1.15+ for JSON", Color::Warning))),
        ];
        CommandPalette::new(entries, window, cx)
    });
    redraw(cx);
    assert!(cx.debug_bounds("palette-row:0:note").is_none());
    let row = cx.debug_bounds("palette-row:1").unwrap();
    let note = cx
        .debug_bounds("palette-row:1:note")
        .expect("the agy row's note");
    assert!(row.contains(&note.center()), "{note:?} in {row:?}");
    assert!(note.center().x > row.center().x, "at the row's end");
}
