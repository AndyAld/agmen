//! Shared dialog contracts and keyboard focus regressions.

use gpui::{
    AppContext as _, Context, Entity, FocusHandle, Focusable as _, InputEvent as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _, TestAppContext,
    Window, div, px,
};

use super::components::{Button, ButtonStyle, DialogFooter, Modal};
use super::test_support::{draw, fills_at, init, redraw};
use super::text_input::TextInput;

/// GPUI's simulate_keystrokes sends key-down only. Buttons activate on
/// key-up, so send the full pair when checking an action.
pub(super) fn press(cx: &mut gpui::VisualTestContext, key: &'static str) {
    let key = gpui::Keystroke::parse(key).unwrap();
    cx.update(|window, cx| {
        window.dispatch_event(
            gpui::KeyDownEvent {
                keystroke: key.clone(),
                is_held: false,
                prefer_character_input: false,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(gpui::KeyUpEvent { keystroke: key }.to_platform_input(), cx);
    });
    redraw(cx);
}

const DIALOGS: [(&str, &str, &str); 7] = [
    (
        "new agent",
        include_str!("new_agent_dialog.rs"),
        "Modal::small(",
    ),
    (
        "remove agent",
        include_str!("remove_agent_dialog.rs"),
        "Modal::small(",
    ),
    (
        "add agent",
        include_str!("add_agent_dialog.rs"),
        "Modal::large(",
    ),
    (
        "kick off",
        include_str!("kickoff_dialog.rs"),
        "Modal::large(",
    ),
    (
        "open project",
        include_str!("live_root_project.rs"),
        "Modal::small(",
    ),
    ("quit", include_str!("live_root.rs"), "Modal::medium("),
    (
        "invite",
        include_str!("live_root_membership.rs"),
        "Modal::small(",
    ),
];

#[test]
fn every_dialog_uses_the_footer_with_the_primary_rightmost() {
    for (name, source, _) in DIALOGS {
        assert!(
            source.contains(".footer("),
            "{name} must use Modal's footer slot"
        );
        assert!(
            source.contains("DialogFooter::"),
            "{name} must use DialogFooter"
        );
    }
}

#[test]
fn remove_agent_footer_is_destructive() {
    assert!(include_str!("remove_agent_dialog.rs").contains(".destructive(true)"));
}

#[test]
fn dialog_titles_share_one_style() {
    for (name, source, _) in DIALOGS {
        if name != "invite" {
            assert!(
                source.contains(".title("),
                "{name} must use Modal's title slot"
            );
        }
        assert!(
            !source.contains(".size(LabelSize::Title)"),
            "{name} has a hand-styled title"
        );
    }
    assert!(include_str!("settings_view.rs").contains("Label::title(title)"));
}

#[test]
fn dialogs_use_the_size_presets() {
    for (name, source, preset) in DIALOGS {
        assert!(source.contains(preset), "{name} must use {preset}");
        assert!(!source.contains("Modal::new("), "{name} has raw geometry");
    }
}

/// Check the actual actions drawn by a dialog, including its main action's
/// position and style. The reference heights follow ButtonSize::Large.
pub(super) fn assert_footer(
    cx: &mut gpui::VisualTestContext,
    card: &'static str,
    actions: &[(&'static str, ButtonStyle)],
) {
    use super::tokens::{BORDER_WIDTH, LINE_HEIGHT, SPACE_6, SPACE_8, SPACE_20, TEXT_DEFAULT};
    redraw(cx);
    let theme = cx.update(|_, cx| cx.global::<crate::appearance::Appearance>().theme.clone());
    let line = cx.update(|window, _| TEXT_DEFAULT.to_pixels(window.rem_size()) * LINE_HEIGHT);
    let mut previous: Option<gpui::Bounds<gpui::Pixels>> = None;
    for &(id, style) in actions {
        let bounds = cx
            .debug_bounds(id)
            .unwrap_or_else(|| panic!("missing {id}"));
        if let Some(previous) = previous {
            assert_eq!(bounds.left() - previous.right(), SPACE_8, "{id} gap");
            assert_eq!(bounds.center().y, previous.center().y, "{id} alignment");
        }
        let border = if style == ButtonStyle::Inset {
            BORDER_WIDTH * 2.
        } else {
            px(0.)
        };
        assert!(
            (bounds.size.height - (line + SPACE_6 * 2. + border)).abs() < px(1.),
            "{id} must be Large"
        );
        let fill = match style {
            ButtonStyle::Primary => theme.text_accent,
            ButtonStyle::Danger => theme.error,
            ButtonStyle::Inset => theme.background,
            _ => panic!("unexpected footer style {style:?}"),
        };
        let viewport = cx.update(|window, _| window.viewport_size());
        assert_eq!(
            fills_at(cx, id),
            [fill],
            "{id} style at {bounds:?} in {viewport:?}"
        );
        previous = Some(bounds);
    }
    assert_eq!(
        previous.expect("a footer action").right(),
        cx.debug_bounds(card).unwrap().right() - SPACE_20 - BORDER_WIDTH,
        "the main action is rightmost and right-aligned",
    );
}

#[gpui::test]
fn rendered_agent_and_kickoff_footers_keep_the_main_action_rightmost(_cx: &mut TestAppContext) {
    use super::add_agent_dialog::{AddAgentDialog, AgentDialog};
    use super::kickoff_dialog::KickoffDialog;
    use super::new_agent_dialog::NewAgentDialog;
    use super::plan_test_support::plan_globals;
    use super::remove_agent_dialog::{Kept, RemoveAgentDialog};
    use crate::backend::Screen;
    for dialog in 0..4 {
        let mut app = TestAppContext::single();
        init(&mut app);
        match dialog {
            0 => {
                let (_, cx) =
                    app.add_window_view(|_, cx| NewAgentDialog::new("opus".into(), None, cx));
                assert_footer(
                    cx,
                    "new-agent:card",
                    &[
                        ("new-agent:cancel", ButtonStyle::Inset),
                        ("new-agent:launch", ButtonStyle::Primary),
                    ],
                );
            }
            1 => {
                let (_, cx) = app.add_window_view(|_, _| {
                    RemoveAgentDialog::new(
                        crate::backend::fixture_id("andy/x"),
                        "andy/x".into(),
                        None,
                        Kept::Joined {
                            folder: "/r".into(),
                        },
                    )
                });
                assert_footer(
                    cx,
                    "remove-agent:card",
                    &[
                        ("remove-agent:cancel", ButtonStyle::Inset),
                        ("remove-agent:confirm", ButtonStyle::Danger),
                    ],
                );
            }
            2 => {
                let cx = draw(&mut app, || {
                    div()
                        .relative()
                        .w(px(1000.))
                        .h(px(900.))
                        .child(AddAgentDialog::new(AgentDialog::Other, None).into_modal())
                        .into_any_element()
                });
                assert_footer(
                    cx,
                    "add-agent-dialog:card",
                    &[
                        ("add-agent:cancel", ButtonStyle::Inset),
                        ("add-agent:check", ButtonStyle::Primary),
                    ],
                );
            }
            _ => {
                let (_, model) = plan_globals(&mut app, Screen::KickOff, None);
                model.update(&mut app, |model, cx| model.open_kickoff(cx));
                let cx = draw(&mut app, move || {
                    div()
                        .relative()
                        .w(px(1440.))
                        .h(px(900.))
                        .child(KickoffDialog::new(model.clone()))
                        .into_any_element()
                });
                assert_footer(
                    cx,
                    "kickoff-dialog:card",
                    &[
                        ("kickoff:cancel", ButtonStyle::Inset),
                        ("kickoff:go", ButtonStyle::Primary),
                    ],
                );
            }
        }
    }
}

struct FocusFixture {
    open: bool,
    footer: bool,
    last_action: Option<&'static str>,
    second_dialog: bool,
    behind: FocusHandle,
    first: Entity<TextInput>,
    last: Entity<TextInput>,
}

impl Render for FocusFixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = div()
            .size_full()
            .relative()
            .on_key_down(super::focus_tab)
            .child(div().track_focus(&self.behind).tab_index(0));
        if self.open {
            let mut modal = Modal::small("focus-dialog")
                .child(self.first.clone())
                .child(self.last.clone());
            if self.footer {
                modal =
                    modal
                        .child(Button::new("disabled", "Disabled").disabled(true).on_click(
                            cx.listener(|this, _, _, _| this.last_action = Some("disabled")),
                        ))
                        .footer(
                            DialogFooter::new(Button::new("go", "Go").on_click(
                                cx.listener(|this, _, _, _| this.last_action = Some("go")),
                            ))
                            .cancel(
                                Button::new("cancel", "Cancel").on_click(
                                    cx.listener(|this, _, _, _| this.last_action = Some("cancel")),
                                ),
                            ),
                        );
            }
            root = root.child(modal);
        }
        if self.second_dialog {
            root = root.child(Modal::small("second-dialog").child(
                Button::new("second-dialog:cancel", "Cancel").on_click(cx.listener(
                    |this, _, window, cx| {
                        this.second_dialog = false;
                        window.focus(&this.behind, cx);
                        cx.notify();
                    },
                )),
            ));
        }
        root
    }
}

fn open(cx: &mut TestAppContext) -> (Entity<FocusFixture>, &mut gpui::VisualTestContext) {
    init(cx);
    cx.update(crate::keymap::bind_all);
    let (view, cx) = cx.add_window_view(|window, cx| {
        let behind = cx.focus_handle().tab_index(0).tab_stop(true);
        window.focus(&behind, cx);
        FocusFixture {
            open: false,
            footer: false,
            last_action: None,
            second_dialog: false,
            behind,
            first: cx.new(TextInput::new),
            last: cx.new(TextInput::new),
        }
    });
    redraw(cx);
    view.update(cx, |view, cx| {
        view.open = true;
        cx.notify();
    });
    redraw(cx);
    (view, cx)
}

#[gpui::test]
fn opening_a_dialog_moves_focus_into_it(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.update(|window, cx| {
        let first = view.read(cx).first.read(cx).focus_handle(cx);
        assert!(
            first.is_focused(window),
            "opening focuses the first dialog control: current {:?}, first {:?}",
            window.focused(cx),
            first
        );
    });
}

#[gpui::test]
fn closing_the_top_dialog_keeps_focus_inside_the_dialog_under_it(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    view.update(cx, |view, cx| {
        view.second_dialog = true;
        cx.notify();
    });
    redraw(cx);
    super::test_support::click(cx, "second-dialog:cancel");
    cx.update(|window, cx| {
        assert!(
            view.read(cx)
                .first
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        );
    });
    press(cx, "tab");
    cx.update(|window, cx| {
        assert!(
            view.read(cx)
                .last
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        );
    });
    press(cx, "shift-tab");
    cx.update(|window, cx| {
        assert!(
            view.read(cx)
                .first
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        );
    });
}

#[gpui::test]
fn tab_and_shift_tab_stay_inside_an_open_dialog(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    let (first, last) = view.read_with(cx, |view, cx| {
        (
            view.first.read(cx).focus_handle(cx),
            view.last.read(cx).focus_handle(cx),
        )
    });
    cx.update(|window, cx| window.focus(&first, cx));
    redraw(cx);
    for (key, expected) in [
        ("tab", &last),
        ("tab", &first),
        ("shift-tab", &last),
        ("shift-tab", &first),
    ] {
        press(cx, key);
        redraw(cx);
        cx.update(|window, cx| {
            assert!(
                expected.is_focused(window),
                "{key} must cycle within the dialog: current {:?}, expected {:?}",
                window.focused(cx),
                expected
            )
        });
    }
    view.read_with(cx, |view, cx| {
        assert_eq!(view.first.read(cx).text(), "");
        assert_eq!(view.last.read(cx).text(), "");
    });
}

#[gpui::test]
fn tab_reaches_footer_actions_skips_disabled_controls_and_wraps(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    view.update_in(cx, |view, window, cx| {
        view.footer = true;
        window.focus(&view.last.read(cx).focus_handle(cx), cx);
        cx.notify();
    });
    redraw(cx);
    for (key, action) in [("space", "cancel"), ("enter", "go")] {
        cx.simulate_keystrokes("tab");
        redraw(cx);
        press(cx, key);
        redraw(cx);
        assert_eq!(view.read_with(cx, |view, _| view.last_action), Some(action));
    }
    cx.simulate_keystrokes("tab");
    redraw(cx);
    cx.update(|window, cx| {
        assert!(
            view.read(cx)
                .first
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
        )
    });
    cx.simulate_keystrokes("shift-tab");
    redraw(cx);
    view.update(cx, |view, _| view.last_action = None);
    press(cx, "space");
    assert_eq!(view.read_with(cx, |view, _| view.last_action), Some("go"));
}

struct KickoffFocusFixture {
    model: Entity<super::plan_model::PlanModel>,
    behind: FocusHandle,
    show_top: bool,
}

impl Render for KickoffFocusFixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = div().size_full().relative().track_focus(&self.behind);
        if self.model.read(cx).kickoff_open() {
            root = root.child(super::kickoff_dialog::KickoffDialog::new(
                self.model.clone(),
            ));
        }
        if self.show_top {
            root = root.child(
                Modal::small("above-kickoff")
                    .child(Button::new("top-action", "Close").on_click(|_, _, _| {})),
            );
        }
        root
    }
}

#[gpui::test]
fn kickoff_keeps_its_original_focus_restore_target_when_another_dialog_opens(
    cx: &mut TestAppContext,
) {
    let (_, model) =
        super::plan_test_support::plan_globals(cx, crate::backend::Screen::KickOff, None);
    model.update(cx, |model, cx| model.open_kickoff(cx));
    let (view, cx) = cx.add_window_view(|window, cx| {
        let behind = cx.focus_handle();
        window.focus(&behind, cx);
        KickoffFocusFixture {
            model,
            behind,
            show_top: false,
        }
    });
    redraw(cx);
    view.update(cx, |view, cx| {
        view.show_top = true;
        cx.notify();
    });
    redraw(cx);
    redraw(cx);
    view.update(cx, |view, cx| {
        view.show_top = false;
        cx.notify();
    });
    redraw(cx);
    super::test_support::click(cx, "kickoff:cancel");
    cx.update(|window, cx| assert!(view.read(cx).behind.is_focused(window)));
}

#[gpui::test]
fn closing_new_agent_returns_focus_to_the_workspace(cx: &mut TestAppContext) {
    use super::Workspace;
    use crate::backend::chat_snapshot;
    init(cx);
    let (workspace, cx) = cx.add_window_view(|window, cx| {
        let workspace = Workspace::new(chat_snapshot(), window, cx);
        let handle = gpui::Focusable::focus_handle(&workspace, cx);
        window.focus(&handle, cx);
        workspace
    });
    redraw(cx);
    let workspace_focus = workspace.read_with(cx, gpui::Focusable::focus_handle);
    workspace.update(cx, |w, cx| w.open_new_agent(cx));
    redraw(cx);
    redraw(cx);
    cx.update(|window, cx| {
        assert!(
            window.focused(cx).is_some_and(|f| f != workspace_focus),
            "opening moves focus into the dialog"
        );
    });
    super::test_support::click(cx, "new-agent:cancel");
    redraw(cx);
    assert!(cx.debug_bounds("new-agent").is_none(), "the dialog closed");
    cx.update(|window, cx| {
        let focused = window.focused(cx);
        assert!(
            focused
                .as_ref()
                .is_some_and(|f| workspace_focus.contains(f, window)),
            "focus is back in the workspace, not stranded: {focused:?}"
        );
    });
}
