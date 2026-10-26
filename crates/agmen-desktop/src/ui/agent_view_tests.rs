use gpui::TestAppContext;

use super::*;
use crate::backend::chat_snapshot;
use crate::ui::test_support::{assert_breadcrumb_glyphs, init, redraw};

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
    for tone in [Tone::Plain, Tone::Success, Tone::Accent] {
        assert_eq!(out_colors(tone), (tone_color(tone), tone_color(tone)));
    }
}

#[gpui::test]
fn breadcrumb_splits_handle_item_and_branch_with_a_glyph(cx: &mut TestAppContext) {
    init(cx);
    let snapshot = chat_snapshot();
    let id = crate::backend::fixture_id("andy/fx1");
    let session = snapshot.sessions.get(&id).unwrap().clone();
    let (_, cx) = cx.add_window_view(|_, cx| AgentView::new("andy/fx1", session, cx));
    redraw(cx);
    assert_breadcrumb_glyphs(cx, 3);
}
