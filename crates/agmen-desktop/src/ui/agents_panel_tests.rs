use super::*;
use crate::backend::chat_snapshot;
use crate::ui::test_support::{click, init, redraw};
use crate::ui::tokens::{TREE_INDENT_BASE, TREE_INDENT_STEP};
use gpui::TestAppContext;

#[gpui::test]
fn agents_panel_groups_and_folds(cx: &mut TestAppContext) {
    init(cx);
    let (panel, cx) = cx.add_window_view(|_, _| AgentsPanel::new(chat_snapshot().roster));
    redraw(cx);
    panel.read_with(cx, |p, _| assert_eq!(p.visible_agents().count(), 9));
    assert!(cx.debug_bounds("agent:sean/a1").is_some());

    click(cx, "person:sean");
    panel.read_with(cx, |p, _| assert_eq!(p.visible_agents().count(), 7));
    assert!(cx.debug_bounds("agent:sean/a1").is_none());
    assert!(cx.debug_bounds("agent:sean/plan").is_none());
    assert!(cx.debug_bounds("agent:adam/coord").is_some());

    click(cx, "person:sean");
    panel.read_with(cx, |p, _| assert_eq!(p.visible_agents().count(), 9));
}

#[gpui::test]
fn agents_rows_indent_by_depth(cx: &mut TestAppContext) {
    init(cx);
    let (_, cx) = cx.add_window_view(|_, _| AgentsPanel::new(chat_snapshot().roster));
    redraw(cx);
    let inset = |cx: &mut gpui::VisualTestContext, row: &'static str, lead: &'static str| {
        let row = cx.debug_bounds(row).unwrap();
        let lead = cx.debug_bounds(lead).unwrap();
        lead.origin.x - row.origin.x
    };
    assert_eq!(
        inset(cx, "agent:andy/rv2", "agent:andy/rv2:leading"),
        TREE_INDENT_BASE + TREE_INDENT_STEP * 2.
    );
    assert_eq!(
        inset(cx, "agent:andy/fx1", "agent:andy/fx1:leading"),
        TREE_INDENT_BASE + TREE_INDENT_STEP
    );
    assert_eq!(
        inset(cx, "agent:andy/listen", "agent:andy/listen:leading"),
        TREE_INDENT_BASE
    );
}

#[gpui::test]
fn selecting_a_row_emits_its_handle(cx: &mut TestAppContext) {
    init(cx);
    let (panel, cx) = cx.add_window_view(|_, _| AgentsPanel::new(chat_snapshot().roster));
    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&panel, move |_, event: &AgentsPanelEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    click(cx, "agent:sean/plan");
    assert_eq!(
        *events.borrow(),
        vec![AgentsPanelEvent::Selected("seanplan".parse().unwrap())]
    );
}

#[gpui::test]
fn status_glyphs_sit_in_a_fixed_slot(cx: &mut TestAppContext) {
    // Mockup check V62: handles line up at the mockup's x whatever the glyph.
    init(cx);
    let (_, cx) = cx.add_window_view(|_, _| AgentsPanel::new(chat_snapshot().roster));
    redraw(cx);
    for row in [
        "agent:andy/fx1:leading",
        "agent:andy/rv2:leading",
        "agent:sean/plan:leading",
    ] {
        let lead = cx.debug_bounds(row).unwrap();
        assert_eq!(lead.size.width, crate::ui::tokens::LEADING_SLOT, "{row}");
    }
}
