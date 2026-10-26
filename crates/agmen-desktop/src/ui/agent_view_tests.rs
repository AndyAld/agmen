use gpui::TestAppContext;

use super::*;
use crate::backend::chat_snapshot;
use crate::ui::test_support::{init, redraw};

#[gpui::test]
fn agent_view_lists_tool_calls(cx: &mut TestAppContext) {
    init(cx);
    let snapshot = chat_snapshot();
    let id = crate::backend::fixture_id("andy/fx1");
    let session = snapshot.sessions.get(&id).unwrap().clone();
    let (_, cx) = cx.add_window_view(|_, cx| AgentView::new("andy/fx1", session, cx));
    redraw(cx);
    // Review C166: every row, in console.py's order.
    let kinds = [
        "tool", "tool", "out", "say", "tool", "tool", "out", "tool", "out", "cost", "tool", "out",
    ];
    for (ix, kind) in kinds.into_iter().enumerate() {
        let sel: &'static str = Box::leak(format!("session:{ix}:{kind}").into_boxed_str());
        assert!(cx.debug_bounds(sel).is_some(), "{sel}");
    }
    // Review C177: 12 rows, numbered from 0, whatever the kind.
    for kind in ["tool", "out", "say", "cost"] {
        let sel: &'static str = Box::leak(format!("session:12:{kind}").into_boxed_str());
        assert!(cx.debug_bounds(sel).is_none(), "{sel}");
    }
    assert!(cx.debug_bounds("agent-open-terminal").is_some());
    assert!(cx.debug_bounds("agent-stop").is_some());
}

#[test]
fn plain_output_is_dim() {
    // Mockup check: console.py draws plain tool output in FAINT.
    assert_eq!(tone_color(Tone::Plain), Color::Placeholder);
    assert_eq!(tone_color(Tone::Success), Color::Success);
}

#[test]
fn output_glyph_takes_the_line_color() {
    // V79: "⎿ ok" is all success, "⎿ recorded" all accent, "⎿ 2 files" all
    // text.placeholder.
    assert_eq!(
        out_colors(Tone::Plain),
        (Color::Placeholder, Color::Placeholder)
    );
    assert_eq!(out_colors(Tone::Success), (Color::Success, Color::Success));
    assert_eq!(out_colors(Tone::Accent), (Color::Accent, Color::Accent));
}

#[gpui::test]
fn agent_header_buttons_are_8_px_apart(cx: &mut TestAppContext) {
    // V105: node 607 spaces "Open in terminal" and "Stop" 8 px apart.
    init(cx);
    let snapshot = chat_snapshot();
    let id = crate::backend::fixture_id("andy/fx1");
    let session = snapshot.sessions.get(&id).unwrap().clone();
    let (_, cx) = cx.add_window_view(|_, cx| AgentView::new("andy/fx1", session, cx));
    redraw(cx);
    let open = cx.debug_bounds("agent-open-terminal").unwrap();
    let stop = cx.debug_bounds("agent-stop").unwrap();
    assert_eq!(
        stop.left() - open.right(),
        gpui::px(8.),
        "{open:?} {stop:?}"
    );
}

#[gpui::test]
fn rows_between_cards_take_the_mockups_line_heights(cx: &mut TestAppContext) {
    // V108, V113 (ruling C243): console.py's rows between tool cards are
    // 14 px (the ⎿ rows), 16 px (Say) and 13 px (cost) tall at any font size.
    init(cx);
    cx.update(|cx| {
        let mut appearance = cx.global::<crate::appearance::Appearance>().clone();
        // Andy's code size, where a 14/12 ratio gave 15.17 px.
        appearance.fonts.code_size = gpui::px(13.);
        cx.set_global(appearance);
    });
    let snapshot = chat_snapshot();
    let id = crate::backend::fixture_id("andy/fx1");
    let session = snapshot.sessions.get(&id).unwrap().clone();
    let (_, cx) = cx.add_window_view(|_, cx| AgentView::new("andy/fx1", session, cx));
    redraw(cx);
    let mut height = |sel: &'static str| cx.debug_bounds(sel).unwrap().size.height;
    assert_eq!(height("session:2:out"), gpui::px(14.));
    assert_eq!(height("session:3:say"), gpui::px(16.));
    assert_eq!(height("session:9:cost"), gpui::px(13.));
}
