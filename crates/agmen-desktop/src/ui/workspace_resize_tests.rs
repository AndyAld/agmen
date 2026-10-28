use gpui::{
    Context, Entity, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point,
    TestAppContext, VisualTestContext, Window, point, px, size,
};

use super::super::Workspace;
use crate::appearance::DesktopSettings;
use crate::backend::{Backend as _, FixtureBackend, Screen, chat_snapshot};
use crate::ui::test_support::{click, init, redraw};
use crate::ui::tokens::{BORDER_WIDTH, LEFT_DOCK_WIDTH, PLAN_PANEL_WIDTH, RIGHT_DOCK_WIDTH};

const LEFT_HANDLE: &str = "left-dock:resize";
const RIGHT_HANDLE: &str = "right-dock:resize";
/// Wide enough for both docks at their widest beside the center's minimum.
const WIDE: f32 = 1600.;

/// A workspace over `snapshot` in a window `width` wide, drawn once.
fn open_with(
    cx: &mut TestAppContext,
    width: f32,
    make: impl FnOnce(&mut Window, &mut Context<Workspace>) -> Workspace + 'static,
) -> (Entity<Workspace>, &mut VisualTestContext) {
    init(cx);
    let (workspace, cx) = cx.add_window_view(make);
    cx.simulate_resize(size(px(width), px(900.)));
    redraw(cx);
    (workspace, cx)
}

fn open(cx: &mut TestAppContext, width: f32) -> (Entity<Workspace>, &mut VisualTestContext) {
    open_with(cx, width, |window, cx| {
        Workspace::new(chat_snapshot(), window, cx)
    })
}

fn open_screen(
    cx: &mut TestAppContext,
    screen: Screen,
) -> (Entity<Workspace>, &mut VisualTestContext) {
    let snapshot = FixtureBackend::for_screen(screen).unwrap().snapshot();
    open_with(cx, WIDE, move |window, cx| {
        let mut workspace = Workspace::new(snapshot, window, cx);
        workspace.show_screen(screen, cx);
        workspace
    })
}

fn width(cx: &mut VisualTestContext, selector: &'static str) -> Pixels {
    redraw(cx);
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("nothing is drawn with selector {selector}"))
        .size
        .width
}

fn handle_center(cx: &mut VisualTestContext, handle: &'static str) -> Point<Pixels> {
    redraw(cx);
    cx.debug_bounds(handle)
        .unwrap_or_else(|| panic!("no drag handle drawn as {handle}"))
        .center()
}

/// Presses on `handle` and moves the mouse to `x`, without letting go.
fn drag_to(cx: &mut VisualTestContext, handle: &'static str, x: f32) -> Point<Pixels> {
    let start = handle_center(cx, handle);
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    // The first move past gpui's threshold starts the drag.
    cx.simulate_mouse_move(
        start + point(px(4.), px(0.)),
        MouseButton::Left,
        Modifiers::none(),
    );
    let end = point(px(x), start.y);
    cx.simulate_mouse_move(end, MouseButton::Left, Modifiers::none());
    redraw(cx);
    end
}

/// Drags `handle` to `x` and lets go there.
fn drag(cx: &mut VisualTestContext, handle: &'static str, x: f32) {
    let end = drag_to(cx, handle, x);
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    redraw(cx);
}

fn double_click(cx: &mut VisualTestContext, handle: &'static str) {
    let at = handle_center(cx, handle);
    for click_count in [1, 2] {
        cx.simulate_event(MouseDownEvent {
            position: at,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            position: at,
            modifiers: Modifiers::none(),
            button: MouseButton::Left,
            click_count,
        });
    }
    redraw(cx);
}

fn viewport_width(cx: &mut VisualTestContext) -> f32 {
    f32::from(cx.update(|window, _| window.viewport_size().width))
}

/// The center column's width: the body between the docks.
fn center_width(cx: &mut VisualTestContext) -> Pixels {
    let left = cx
        .debug_bounds("left-dock")
        .map_or(px(0.), |b| b.size.width);
    let right = cx
        .debug_bounds("right-dock")
        .map_or(px(0.), |b| b.size.width);
    px(viewport_width(cx)) - left - right
}

#[gpui::test]
fn dragging_the_left_handle_resizes_the_left_dock(cx: &mut TestAppContext) {
    let (_, cx) = open(cx, WIDE);
    assert_eq!(width(cx, "left-dock"), LEFT_DOCK_WIDTH);
    drag_to(cx, LEFT_HANDLE, 350.);
    assert_eq!(width(cx, "left-dock"), px(350.), "live, before letting go");
    drag(cx, LEFT_HANDLE, 300.);
    assert_eq!(width(cx, "left-dock"), px(300.));
}

#[gpui::test]
fn dragging_the_right_handle_resizes_the_right_dock(cx: &mut TestAppContext) {
    let (_, cx) = open(cx, WIDE);
    assert_eq!(width(cx, "right-dock"), RIGHT_DOCK_WIDTH);
    drag(cx, RIGHT_HANDLE, WIDE - 500.);
    assert_eq!(width(cx, "right-dock"), px(500.));
}

#[gpui::test]
fn the_handles_sit_on_the_docks_inner_borders(cx: &mut TestAppContext) {
    let (_, cx) = open(cx, WIDE);
    let left = cx.debug_bounds("left-dock").unwrap();
    let handle = cx.debug_bounds(LEFT_HANDLE).unwrap();
    assert_eq!(handle.size.width, crate::ui::tokens::RESIZE_HANDLE_WIDTH);
    assert_eq!(handle.size.height, left.size.height);
    let border = left.right() - BORDER_WIDTH / 2.;
    assert_eq!(handle.center().x, border, "centered on the left border");
    let right = cx.debug_bounds("right-dock").unwrap();
    let handle = cx.debug_bounds(RIGHT_HANDLE).unwrap();
    assert_eq!(handle.size.width, crate::ui::tokens::RESIZE_HANDLE_WIDTH);
    let border = right.left() + BORDER_WIDTH / 2.;
    assert_eq!(handle.center().x, border, "centered on the right border");
}

#[gpui::test]
fn docks_stop_at_their_bounds(cx: &mut TestAppContext) {
    let (_, cx) = open(cx, WIDE);
    drag(cx, LEFT_HANDLE, 50.);
    assert_eq!(width(cx, "left-dock"), px(200.), "left minimum");
    drag(cx, LEFT_HANDLE, 900.);
    assert_eq!(width(cx, "left-dock"), px(480.), "left maximum");
    drag(cx, RIGHT_HANDLE, WIDE - 50.);
    assert_eq!(width(cx, "right-dock"), px(260.), "right minimum");
    drag(cx, RIGHT_HANDLE, WIDE - 1000.);
    assert_eq!(width(cx, "right-dock"), px(640.), "right maximum");
    // Past the window's edge too.
    drag(cx, RIGHT_HANDLE, WIDE + 100.);
    assert_eq!(width(cx, "right-dock"), px(260.));
}

#[gpui::test]
fn the_center_keeps_its_minimum_while_dragging(cx: &mut TestAppContext) {
    let (_, cx) = open(cx, 1000.);
    // 1000 - 360 - 272 leaves the right dock 368.
    drag(cx, RIGHT_HANDLE, 0.);
    assert_eq!(width(cx, "right-dock"), px(368.));
    assert_eq!(center_width(cx), px(360.));
    // The left dock has no room left to grow.
    drag(cx, LEFT_HANDLE, 900.);
    assert_eq!(width(cx, "left-dock"), px(272.));
    assert_eq!(center_width(cx), px(360.));
}

#[gpui::test]
fn a_narrower_window_shrinks_the_docks_and_a_wider_one_restores_them(cx: &mut TestAppContext) {
    let (_, cx) = open(cx, WIDE);
    drag(cx, LEFT_HANDLE, 900.);
    drag(cx, RIGHT_HANDLE, 0.);
    assert_eq!(width(cx, "left-dock"), px(480.));
    assert_eq!(width(cx, "right-dock"), px(640.));

    // 480 + 640 + 360 is 480 too many: the right dock gives way first.
    cx.simulate_resize(size(px(1000.), px(900.)));
    assert_eq!(width(cx, "right-dock"), px(260.));
    assert_eq!(width(cx, "left-dock"), px(380.));
    assert_eq!(center_width(cx), px(360.));

    // Narrower than both minimums and the center's: the docks stay at
    // their minimums.
    cx.simulate_resize(size(px(700.), px(900.)));
    assert_eq!(width(cx, "left-dock"), px(200.));
    assert_eq!(width(cx, "right-dock"), px(260.));

    cx.simulate_resize(size(px(WIDE), px(900.)));
    assert_eq!(width(cx, "left-dock"), px(480.));
    assert_eq!(width(cx, "right-dock"), px(640.));
}

#[gpui::test]
fn double_clicking_a_handle_resets_its_dock(cx: &mut TestAppContext) {
    let (_, cx) = open(cx, WIDE);
    drag(cx, LEFT_HANDLE, 400.);
    drag(cx, RIGHT_HANDLE, WIDE - 500.);
    double_click(cx, LEFT_HANDLE);
    assert_eq!(width(cx, "left-dock"), LEFT_DOCK_WIDTH);
    assert_eq!(width(cx, "right-dock"), px(500.), "the other dock stays");
    double_click(cx, RIGHT_HANDLE);
    assert_eq!(width(cx, "right-dock"), RIGHT_DOCK_WIDTH);
}

/// Points the app at a throwaway settings file holding `text`.
fn settings_home(cx: &mut TestAppContext, text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".config");
    let file = config.join("agmen/settings.toml");
    std::fs::create_dir_all(config.join("agmen")).unwrap();
    std::fs::write(&file, text).unwrap();
    let sources = crate::ui::SettingsSources {
        config,
        extensions: home.path().join("extensions"),
        market: "http://127.0.0.1:9".into(),
        github: "http://127.0.0.1:9".into(),
    };
    cx.update(|cx| cx.set_global(sources));
    (home, file)
}

/// The `[desktop]` key `key` in `file`, as a number.
fn saved(file: &std::path::Path, key: &str) -> Option<i64> {
    let table: toml::Table = std::fs::read_to_string(file).unwrap().parse().unwrap();
    table
        .get("desktop")
        .and_then(|desktop| desktop.get(key))
        .and_then(toml::Value::as_integer)
}

#[gpui::test]
fn widths_are_saved_when_a_drag_ends_and_restored_at_launch(cx: &mut TestAppContext) {
    let (_home, file) = settings_home(cx, "# agmen settings\n");
    let mut app = cx.clone();
    let (_, window) = open(&mut app, WIDE);
    let end = drag_to(window, LEFT_HANDLE, 300.);
    assert_eq!(saved(&file, "left_dock_width"), None, "not while dragging");
    window.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    assert_eq!(saved(&file, "left_dock_width"), Some(300));
    drag(window, RIGHT_HANDLE, WIDE - 420.);
    assert_eq!(saved(&file, "right_dock_width"), Some(420));
    assert_eq!(saved(&file, "left_dock_width"), Some(300));
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("# agmen settings\n"),
        "the file's comment stays"
    );

    let (_, next) = open(cx, WIDE);
    assert_eq!(width(next, "left-dock"), px(300.));
    assert_eq!(width(next, "right-dock"), px(420.));

    // A reset forgets the saved width.
    double_click(next, LEFT_HANDLE);
    assert_eq!(saved(&file, "left_dock_width"), None);
    assert_eq!(saved(&file, "right_dock_width"), Some(420));
}

#[gpui::test]
fn saved_widths_out_of_range_are_clamped_and_bad_ones_ignored(cx: &mut TestAppContext) {
    let (_home, file) = settings_home(
        cx,
        "[desktop]\nleft_dock_width = 5000\nright_dock_width = \"wide\"\nstart_at_login = true\n",
    );
    // A bad width does not cost the rest of the table.
    assert!(DesktopSettings::load(&file).unwrap().start_at_login);
    let (_, cx) = open(cx, WIDE);
    assert_eq!(width(cx, "left-dock"), px(480.));
    assert_eq!(width(cx, "right-dock"), RIGHT_DOCK_WIDTH);
}

#[gpui::test]
fn saved_widths_below_the_minimum_are_clamped(cx: &mut TestAppContext) {
    let (_home, _file) = settings_home(
        cx,
        "[desktop]\nleft_dock_width = -40\nright_dock_width = 12.5\n",
    );
    let (_, cx) = open(cx, WIDE);
    assert_eq!(width(cx, "left-dock"), px(200.));
    assert_eq!(width(cx, "right-dock"), px(260.));
}

#[gpui::test]
fn every_right_dock_panel_follows_the_docks_width(cx: &mut TestAppContext) {
    let inner = px(520.) - BORDER_WIDTH;
    let (_, window) = open(cx, WIDE);
    drag(window, RIGHT_HANDLE, WIDE - 520.);
    assert_eq!(width(window, "listener-panel"), inner, "listener");
    click(window, "agent:andy/fx1");
    assert_eq!(width(window, "agent-panel"), inner, "agent");
    click(window, "tab-roles");
    click(window, "role:engineer");
    assert_eq!(width(window, "role-panel"), inner, "role");

    for (screen, panel) in [(Screen::Plan, "plan-panel"), (Screen::Spec, "review-panel")] {
        let mut app = TestAppContext::single();
        let (_, window) = open_screen(&mut app, screen);
        drag(window, RIGHT_HANDLE, WIDE - 520.);
        assert_eq!(width(window, panel), inner, "{panel}");
    }
}

#[gpui::test]
fn the_plan_panel_keeps_its_wider_default_until_resized(cx: &mut TestAppContext) {
    let (_, cx) = open_screen(cx, Screen::Plan);
    assert_eq!(width(cx, "right-dock"), PLAN_PANEL_WIDTH);
    drag(cx, RIGHT_HANDLE, WIDE - 400.);
    assert_eq!(width(cx, "right-dock"), px(400.));
    double_click(cx, RIGHT_HANDLE);
    assert_eq!(width(cx, "right-dock"), PLAN_PANEL_WIDTH);
}
