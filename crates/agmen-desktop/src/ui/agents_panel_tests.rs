use super::*;
use crate::backend::chat_snapshot;
use crate::ui::test_support::{click, init, redraw};
use crate::ui::tokens::{TREE_INDENT_BASE, TREE_INDENT_STEP};
use gpui::{TestAppContext, px};

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

#[gpui::test]
fn person_chevrons_keep_their_width(cx: &mut TestAppContext) {
    // V74: node 49 puts the 12 px chevron at x 10, so node 51's name starts
    // at x 28 (10 + 12 + gap 6).
    init(cx);
    let (_, cx) = cx.add_window_view(|_, _| AgentsPanel::new(chat_snapshot().roster));
    redraw(cx);
    for person in ["person:andy", "person:sean", "person:adam"] {
        let row = cx.debug_bounds(person).unwrap();
        let lead = cx
            .debug_bounds(Box::leak(format!("{person}:leading").into_boxed_str()))
            .unwrap();
        assert_eq!(lead.origin.x - row.origin.x, px(10.), "{person}");
        assert_eq!(lead.size.width, crate::ui::tokens::ICON_SM, "{person}");
    }
}

#[gpui::test]
fn new_agent_and_stop_emit_actions(cx: &mut TestAppContext) {
    init(cx);
    let (panel, cx) = cx.add_window_view(|_, _| AgentsPanel::new(chat_snapshot().roster));
    panel.update(cx, |panel, cx| {
        panel.set_managed(
            std::collections::BTreeMap::from([(
                crate::backend::fixture_id("andy/fx1"),
                "running".into(),
            )]),
            None,
            cx,
        )
    });
    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&panel, move |_, event: &AgentsPanelEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    click(cx, "agents:new");
    click(cx, "agent:andy/fx1:stop");
    assert_eq!(
        *events.borrow(),
        vec![
            AgentsPanelEvent::NewAgent,
            AgentsPanelEvent::Stop(crate::backend::fixture_id("andy/fx1"))
        ]
    );
}

/// The panel over the chat fixture with andy's listener set, and the
/// events it emits.
fn menu_panel<'a>(
    cx: &'a mut TestAppContext,
    managed: &[(&str, &str)],
) -> (
    gpui::Entity<AgentsPanel>,
    &'a mut gpui::VisualTestContext,
    std::rc::Rc<std::cell::RefCell<Vec<AgentsPanelEvent>>>,
) {
    init(cx);
    let snapshot = chat_snapshot();
    let listener = snapshot.listener_id().cloned();
    let (panel, cx) = cx.add_window_view(|_, _| AgentsPanel::new(snapshot.roster));
    let managed = managed
        .iter()
        .map(|(handle, state)| (crate::backend::fixture_id(handle), (*state).to_owned()))
        .collect();
    panel.update(cx, |panel, cx| {
        panel.set_listener(listener, cx);
        panel.set_managed(managed, None, cx);
    });
    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = events.clone();
    cx.update(|_, cx| {
        cx.subscribe(&panel, move |_, event: &AgentsPanelEvent, _| {
            seen.borrow_mut().push(event.clone());
        })
        .detach();
    });
    (panel, cx, events)
}

fn menu_items(cx: &mut gpui::VisualTestContext, handle: &str) -> Vec<&'static str> {
    redraw(cx);
    ["start", "stop", "attach", "detach", "remove"]
        .into_iter()
        .filter(|item| {
            let selector: &'static str =
                Box::leak(format!("agent:{handle}:menu:{item}").into_boxed_str());
            cx.debug_bounds(selector).is_some()
        })
        .collect()
}

#[gpui::test]
fn own_agents_open_a_menu_of_actions_for_their_state(cx: &mut TestAppContext) {
    let (_, cx, events) = menu_panel(cx, &[("andy/fx1", "running"), ("andy/rv2", "attached")]);
    click(cx, "agent:andy/fx1");
    assert!(cx.debug_bounds("agent:andy/fx1:menu").is_some());
    assert_eq!(menu_items(cx, "andy/fx1"), ["stop", "attach", "remove"]);
    // A left click still selects the row.
    assert_eq!(
        *events.borrow(),
        vec![AgentsPanelEvent::Selected(crate::backend::fixture_id(
            "andy/fx1"
        ))]
    );

    crate::ui::test_support::right_click(cx, "agent:andy/rv2");
    assert_eq!(menu_items(cx, "andy/rv2"), ["detach", "remove"]);
    crate::ui::test_support::right_click(cx, "agent:andy/t7k");
    assert!(
        cx.debug_bounds("agent:andy/rv2:menu").is_none(),
        "one menu at a time"
    );
    // Not managed here: offline, as after agmend restarts or for a folder
    // joined by hand.
    assert_eq!(menu_items(cx, "andy/t7k"), ["start", "attach", "remove"]);
}

#[gpui::test]
fn others_agents_and_the_listener_get_no_menu(cx: &mut TestAppContext) {
    let (_, cx, _) = menu_panel(cx, &[]);
    click(cx, "agent:sean/plan");
    assert!(cx.debug_bounds("agent:sean/plan:menu").is_none());
    crate::ui::test_support::right_click(cx, "agent:sean/plan");
    assert!(cx.debug_bounds("agent:sean/plan:menu").is_none());
    click(cx, "agent:andy/listen");
    assert!(cx.debug_bounds("agent:andy/listen:menu").is_none());
}

#[gpui::test]
fn menu_items_emit_their_actions_and_close_the_menu(cx: &mut TestAppContext) {
    let (_, cx, events) = menu_panel(cx, &[("andy/fx1", "idle"), ("andy/rv2", "attached")]);
    let id = crate::backend::fixture_id;
    for (row, item, event) in [
        (
            "agent:andy/t7k",
            "agent:andy/t7k:menu:start",
            AgentsPanelEvent::Start(id("andy/t7k")),
        ),
        (
            "agent:andy/fx1",
            "agent:andy/fx1:menu:stop",
            AgentsPanelEvent::Stop(id("andy/fx1")),
        ),
        (
            "agent:andy/fx1",
            "agent:andy/fx1:menu:attach",
            AgentsPanelEvent::Attach(id("andy/fx1")),
        ),
        (
            "agent:andy/rv2",
            "agent:andy/rv2:menu:detach",
            AgentsPanelEvent::Detach(id("andy/rv2")),
        ),
        (
            "agent:andy/rv2",
            "agent:andy/rv2:menu:remove",
            AgentsPanelEvent::Remove(id("andy/rv2")),
        ),
    ] {
        crate::ui::test_support::right_click(cx, row);
        events.borrow_mut().clear();
        click(cx, item);
        assert_eq!(*events.borrow(), vec![event], "{item}");
        assert!(
            cx.debug_bounds(Box::leak(format!("{row}:menu").into_boxed_str()))
                .is_none(),
            "{item} closes the menu"
        );
    }
}

#[gpui::test]
fn a_notice_shows_under_new_agent(cx: &mut TestAppContext) {
    let (panel, cx, _) = menu_panel(cx, &[]);
    assert!(cx.debug_bounds("agents:notice").is_none());
    panel.update(cx, |panel, cx| {
        panel.set_notice(
            Some("Started fresh: its last session included messages from other people.".into()),
            cx,
        )
    });
    redraw(cx);
    assert!(cx.debug_bounds("agents:notice").is_some());
}
