use std::rc::Rc;

use gpui::{Entity, TestAppContext, VisualTestContext};

use super::*;
use crate::backend::{Backend as _, BackendHandle, FixtureBackend, Screen};
use crate::ui::dock::DockSide;
use crate::ui::pane::TabItem;
use crate::ui::test_support::{click, init, redraw};

/// The workspace over `screen`'s fixture, with the app's keys and a
/// backend, focused and drawn once.
fn open_screen(
    cx: &mut TestAppContext,
    screen: Screen,
) -> (
    Entity<Workspace>,
    Rc<FixtureBackend>,
    &mut VisualTestContext,
) {
    open_edited_screen(cx, screen, |_| {})
}

/// [`open_screen`] over the fixture after `edit` changes it.
fn open_edited_screen(
    cx: &mut TestAppContext,
    screen: Screen,
    edit: impl FnOnce(&mut crate::backend::Snapshot),
) -> (
    Entity<Workspace>,
    Rc<FixtureBackend>,
    &mut VisualTestContext,
) {
    init(cx);
    let backend = Rc::new(FixtureBackend::for_screen(screen).unwrap());
    backend.edit(edit);
    let snapshot = backend.snapshot();
    let handle: Rc<dyn crate::backend::Backend> = backend.clone();
    cx.update(|cx| {
        crate::keymap::bind_all(cx);
        cx.set_global(BackendHandle(handle));
    });
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let mut workspace = Workspace::new(snapshot, window, cx);
        workspace.show_screen(screen, cx);
        workspace
    });
    workspace.update_in(cx, |w, window, cx| w.focus_handle.focus(window, cx));
    redraw(cx);
    // Tests have no frame loop: deliver the frame the palette screen waits for.
    cx.update(|window, cx| window.simulate_next_frame(cx));
    redraw(cx);
    (workspace, backend, cx)
}

fn open(cx: &mut TestAppContext) -> (Entity<Workspace>, &mut VisualTestContext) {
    let (workspace, _, cx) = open_screen(cx, Screen::Chat);
    (workspace, cx)
}

fn shown(workspace: &Entity<Workspace>, cx: &VisualTestContext) -> Option<Vec<String>> {
    workspace.read_with(cx, |w, cx| {
        w.palette_entity()
            .map(|p| p.read(cx).shown().map(str::to_owned).collect())
    })
}

#[gpui::test]
fn cmd_shift_p_lists_what_the_workspace_can_run(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    assert!(cx.debug_bounds("command-palette").is_none());
    cx.simulate_keystrokes("cmd-shift-p");
    redraw(cx);
    assert!(cx.debug_bounds("command-palette").is_some());
    let rows = shown(&workspace, cx).unwrap();
    // One spawn row per installed agent (desktop.md:380, C261): the
    // listener's codex first, as the mockup's first row (node 11489), then
    // the others in installed order, outdated agy included. grok is not
    // installed (fix 28).
    assert_eq!(rows[0], "agmen: spawn worker\u{2026}");
    let spawn: Vec<&str> = rows[1..6].iter().map(String::as_str).collect();
    assert_eq!(
        spawn,
        ["codex", "claude", "opencode", "gemini", "agy"]
            .map(|a| format!("agmen: spawn worker with {a}"))
    );
    assert!(!rows.iter().any(|r| r.contains("grok")));
    for label in [
        "workspace: toggle left dock",
        "agents panel: toggle focus",
        "roles panel: toggle focus",
        "workspace: toggle vim mode",
        "agmen: quit",
    ] {
        assert!(rows.contains(&label.to_owned()), "{label}");
    }
    // Task 14: the workspace handles take-over's actions, so both are listed.
    for label in [
        "agmen: open agent in terminal here",
        "terminal panel: toggle",
    ] {
        assert!(rows.contains(&label.to_owned()), "{label} in {rows:?}");
    }
    // Zed's palette lists its own toggle with its key.
    assert!(
        rows.contains(&"command palette: toggle".to_owned()),
        "{rows:?}"
    );

    cx.simulate_keystrokes("cmd-shift-p");
    redraw(cx);
    assert!(
        cx.debug_bounds("command-palette").is_none(),
        "toggles closed"
    );
}

#[gpui::test]
fn palette_runs_the_action_and_closes(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("toggle left dock");
    cx.simulate_keystrokes("enter");
    redraw(cx);
    assert!(cx.debug_bounds("command-palette").is_none());
    workspace.read_with(cx, |w, _| assert!(!w.is_dock_open(DockSide::Left)));
    // Focus went back to the workspace, so its keys work again.
    cx.simulate_keystrokes("cmd-b");
    workspace.read_with(cx, |w, _| assert!(w.is_dock_open(DockSide::Left)));
}

#[gpui::test]
fn clicking_a_row_runs_it_and_puts_the_focus_back(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("toggle left dock");
    redraw(cx);
    click(cx, "palette-row:0");
    assert!(shown(&workspace, cx).is_none());
    workspace.read_with(cx, |w, _| assert!(!w.is_dock_open(DockSide::Left)));
    // The click landed in the palette, so the focus went back to the
    // workspace and its keys work again.
    cx.simulate_keystrokes("cmd-b");
    workspace.read_with(cx, |w, _| assert!(w.is_dock_open(DockSide::Left)));
}

#[gpui::test]
fn escape_closes_the_palette(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_keystrokes("escape");
    redraw(cx);
    assert!(shown(&workspace, cx).is_none());
    assert!(cx.debug_bounds("command-palette").is_none());
}

#[gpui::test]
fn agents_and_roles_open_from_the_palette(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("roles panel");
    cx.simulate_keystrokes("enter");
    workspace.read_with(cx, |w, _| {
        assert_eq!(w.shown_left_tab(), Some(LeftDockTab::Roles));
    });
    cx.simulate_keystrokes("cmd-shift-p");
    cx.simulate_input("agents panel");
    cx.simulate_keystrokes("enter");
    workspace.read_with(cx, |w, _| {
        assert_eq!(w.shown_left_tab(), Some(LeftDockTab::Agents));
    });
}

#[gpui::test]
fn spawn_row_opens_the_delegated_flow_in_the_chat(cx: &mut TestAppContext) {
    let (workspace, backend, cx) = open_screen(cx, Screen::Chat);
    click(cx, "pane-0:tab-1");
    let before = backend.snapshot().messages.len();
    cx.simulate_keystrokes("cmd-n");
    redraw(cx);
    let rows = shown(&workspace, cx).unwrap();
    assert!(
        rows.iter()
            .all(|r| r.starts_with("agmen: spawn worker with ")),
        "{rows:?}"
    );
    assert_eq!(rows.len(), 5, "{rows:?}");
    // The first row is the listener's codex (node 11489).
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    // The plan: here the row opens the fixture's DELEGATED flow, andy's
    // ask, the listener's ANSWER and its DELEGATED with the chosen agent.
    let messages = backend.snapshot().messages;
    assert_eq!(messages.len(), before + 3);
    let sent = &messages[before];
    assert_eq!(sent.client.body.as_str(), "Spawn a worker with codex");
    let listener = workspace.read_with(cx, |w, _| w.snapshot.listener_id().cloned().unwrap());
    assert_eq!(sent.client.to, [listener]);
    let delegated = messages.last().unwrap();
    assert!(
        matches!(
            &delegated.client.payload,
            agmen_proto::Payload::Delegated { agent, .. } if agent == "codex"
        ),
        "{:?}",
        delegated.client.payload
    );
    assert_eq!(delegated.client.reply_to, Some(sent.client.id.clone()));
    let active = workspace.read_with(cx, |w, cx| w.pane_group.read(cx).active_item(cx));
    assert_eq!(active, Some(TabItem::Chat));
    // The chat shows the exchange.
    let shown = workspace.read_with(cx, |w, cx| {
        w.pane_group
            .read(cx)
            .chat()
            .map(|chat| chat.read(cx).message_count())
    });
    assert_eq!(shown, Some(before + 3));
}

#[gpui::test]
fn clicking_another_input_keeps_its_focus(cx: &mut TestAppContext) {
    // Zed's modal layer puts the focus back only while it is still in the
    // modal: a click that closes the palette keeps what it focused.
    let (workspace, cx) = open(cx);
    cx.simulate_keystrokes("cmd-shift-p");
    redraw(cx);
    assert!(shown(&workspace, cx).is_some());
    click(cx, "composer:input");
    assert!(
        shown(&workspace, cx).is_none(),
        "the click closed the palette"
    );
    let composer_focused = workspace.update_in(cx, |w, window, cx| {
        w.pane_group
            .read(cx)
            .chat()
            .map(|chat| chat.focus_handle(cx).is_focused(window))
    });
    assert_eq!(composer_focused, Some(true));
}

#[gpui::test]
fn escape_puts_the_focus_back(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    cx.simulate_keystrokes("cmd-shift-p escape");
    redraw(cx);
    let focused = workspace.update_in(cx, |w, window, _| w.focus_handle.is_focused(window));
    assert!(focused);
}

#[gpui::test]
fn status_bar_hint_opens_the_palette(cx: &mut TestAppContext) {
    let (workspace, cx) = open(cx);
    click(cx, "palette-trigger");
    assert!(shown(&workspace, cx).is_some());
    // While open, the hint has no handler: a click there closes the
    // palette on mouse down and does not reopen it on mouse up (C113).
    click(cx, "palette-trigger");
    assert!(shown(&workspace, cx).is_none());
}

#[gpui::test]
fn palette_screen_opens_with_spawn_typed(cx: &mut TestAppContext) {
    let (workspace, _, cx) = open_screen(cx, Screen::Palette);
    let rows = shown(&workspace, cx).expect("the palette screen opens the palette");
    assert_eq!(rows[0], "agmen: spawn worker\u{2026}");
    assert!(rows.iter().all(|r| r.contains("spawn")), "fix 30: {rows:?}");
    workspace.read_with(cx, |w, cx| {
        let palette = w.palette_entity().unwrap().read(cx);
        assert_eq!(palette.query(), "spawn");
        assert_eq!(
            palette.selected_label(),
            Some("agmen: spawn worker\u{2026}")
        );
    });
    let bounds = cx.debug_bounds("command-palette").unwrap();
    assert_eq!(bounds.origin.y, crate::ui::tokens::PALETTE_TOP);
    assert_eq!(bounds.size.width, crate::ui::tokens::PALETTE_WIDTH);
    // Node 11473: centered; rows 590 by 30 inside the 1 px border and the
    // list's 4 px padding, the first under the 39 px query row.
    let window = cx.update(|window, _| window.viewport_size());
    assert_eq!(bounds.origin.x, (window.width - bounds.size.width) / 2.);
    let row = cx.debug_bounds("palette-row:0").unwrap();
    assert_eq!(row.size, gpui::size(gpui::px(590.), gpui::px(30.)));
    assert_eq!(row.origin.x - bounds.origin.x, gpui::px(5.));
    assert_eq!(row.origin.y - bounds.origin.y, gpui::px(1. + 39. + 4.));
    // "spawn worker…" and five agents: fix 28 drops grok, the outdated agy
    // keeps its row (C261), and fix 30 drops the rows that do not match.
    let last = cx.debug_bounds("palette-row:5").unwrap();
    assert!(cx.debug_bounds("palette-row:6").is_none());
    assert_eq!(
        bounds.size.height,
        last.origin.y + last.size.height + gpui::px(4. + 1.) - bounds.origin.y
    );
}

#[gpui::test]
fn an_outdated_agents_spawn_row_says_why_and_is_refused(cx: &mut TestAppContext) {
    // C261: typing "agy" finds its row, which carries the switch menu's
    // note, and the backend refuses the spawn with the same text above the
    // chat's composer.
    let (workspace, backend, cx) = open_screen(cx, Screen::Chat);
    click(cx, "pane-0:tab-1");
    let before = backend.snapshot().messages.len();
    cx.simulate_keystrokes("cmd-n");
    cx.simulate_input("agy");
    redraw(cx);
    let rows = shown(&workspace, cx).unwrap();
    assert_eq!(rows, ["agmen: spawn worker with agy"]);
    let agy = backend
        .snapshot()
        .installed_agents
        .into_iter()
        .find(|a| a.name == "agy")
        .unwrap();
    let (note, _) = crate::ui::switch_menu::note(&agy).unwrap();
    assert_eq!(note, "update to 1.1.15+ for JSON");
    assert!(cx.debug_bounds("palette-row:0:note").is_some());
    let entries = workspace.update_in(cx, |w, window, cx| w.palette_entries(window, cx));
    let entry = entries
        .iter()
        .find(|e| e.label.as_ref() == "agmen: spawn worker with agy")
        .unwrap();
    assert_eq!(
        entry.note.as_ref().map(|(t, _)| t.as_ref()),
        Some(note.as_str())
    );
    // The ready agents' rows carry no note.
    assert!(
        entries
            .iter()
            .filter(|e| e.label.starts_with("agmen: spawn worker with "))
            .filter(|e| e.note.is_some())
            .all(|e| e.label.ends_with(" agy"))
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    redraw(cx);
    assert_eq!(backend.snapshot().messages.len(), before);
    let error = workspace.read_with(cx, |w, cx| {
        w.pane_group
            .read(cx)
            .chat()
            .and_then(|chat| chat.read(cx).error().map(str::to_owned))
    });
    assert_eq!(
        error.as_deref(),
        Some("agy cannot run a worker: update to 1.1.15+ for JSON")
    );
    assert!(cx.debug_bounds("chat:error").is_some());
}

#[gpui::test]
fn a_spawn_of_an_agent_with_an_unread_version_is_refused(cx: &mut TestAppContext) {
    // C261: an agent whose --version agmen could not read keeps its row,
    // which carries the switch menu's note (C274), and the backend refuses
    // its spawn with not_ready's text above the chat's composer.
    let (workspace, backend, cx) = open_edited_screen(cx, Screen::Chat, |snapshot| {
        let gemini = snapshot
            .installed_agents
            .iter_mut()
            .find(|a| a.name == "gemini")
            .unwrap();
        gemini.found = crate::backend::AgentFound::VersionUnknown;
    });
    click(cx, "pane-0:tab-1");
    let before = backend.snapshot().messages.len();
    let gemini = backend
        .snapshot()
        .installed_agents
        .into_iter()
        .find(|a| a.name == "gemini")
        .unwrap();
    assert_eq!(
        gemini.not_ready().as_deref(),
        Some("could not read its version")
    );
    cx.simulate_keystrokes("cmd-n");
    cx.simulate_input("gemini");
    redraw(cx);
    assert_eq!(
        shown(&workspace, cx).unwrap(),
        ["agmen: spawn worker with gemini"]
    );
    let (note, _) = crate::ui::switch_menu::note(&gemini).unwrap();
    assert_eq!(note, "could not read its version");
    assert!(cx.debug_bounds("palette-row:0:note").is_some());
    let entries = workspace.update_in(cx, |w, window, cx| w.palette_entries(window, cx));
    let entry = entries
        .iter()
        .find(|e| e.label.as_ref() == "agmen: spawn worker with gemini")
        .unwrap();
    assert_eq!(
        entry.note.as_ref().map(|(t, _)| t.as_ref()),
        Some(note.as_str())
    );
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    redraw(cx);
    assert_eq!(backend.snapshot().messages.len(), before);
    let error = workspace.read_with(cx, |w, cx| {
        w.pane_group
            .read(cx)
            .chat()
            .and_then(|chat| chat.read(cx).error().map(str::to_owned))
    });
    assert_eq!(
        error.as_deref(),
        Some("gemini cannot run a worker: could not read its version")
    );
    let shown_error = cx.debug_bounds("chat:error").unwrap();
    let composer = cx.debug_bounds("composer:box").unwrap();
    assert!(
        shown_error.bottom() <= composer.top(),
        "{shown_error:?} is not above {composer:?}"
    );
}
