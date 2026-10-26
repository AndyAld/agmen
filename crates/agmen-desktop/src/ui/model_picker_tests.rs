use std::cell::RefCell;
use std::rc::Rc;

use gpui::{AppContext as _, Entity, Focusable as _, TestAppContext, VisualTestContext, px};

use super::*;
use crate::backend::{Draft, Snapshot, chat_snapshot};
use crate::ui::composer::Composer;
use crate::ui::test_support::{click, init, redraw};
use crate::ui::text_input::bind_keys;
use crate::ui::tokens::MOCKUP_REM;

type Sent = Rc<RefCell<Vec<Draft>>>;

/// A focused composer over the chat fixture (listener on codex) whose
/// sends land in `Sent`, in a window tall enough for the picker above it.
fn open_composer(cx: &mut TestAppContext) -> (Entity<Composer>, Sent, &mut VisualTestContext) {
    open_composer_with(cx, chat_snapshot())
}

/// As `open_composer`, over `snapshot`.
fn open_composer_with(
    cx: &mut TestAppContext,
    snapshot: Snapshot,
) -> (Entity<Composer>, Sent, &mut VisualTestContext) {
    init(cx);
    cx.update(bind_keys);
    // Escape closes the picker through the composer's binding.
    cx.update(|cx| cx.bind_keys(crate::keymap::bindings_in(crate::keymap::COMPOSER_CONTEXT)));
    let sent = Sent::default();
    let sink = sent.clone();
    let (host, cx) = cx.add_window_view(|window, cx| {
        window.set_rem_size(MOCKUP_REM);
        let composer = cx.new(|cx| {
            Composer::new(
                &snapshot,
                move |draft, _, _| {
                    sink.borrow_mut().push(draft);
                    Ok(())
                },
                window,
                cx,
            )
        });
        Bottom(composer)
    });
    let composer = host.read_with(cx, |host, _| host.0.clone());
    composer.update_in(cx, |composer, window, cx| {
        composer.focus_handle(cx).focus(window, cx);
    });
    cx.run_until_parked();
    (composer, sent, cx)
}

/// Puts the composer at the window's bottom, as the chat does.
struct Bottom(Entity<Composer>);

impl gpui::Render for Bottom {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        use gpui::{ParentElement as _, Styled as _};
        gpui::div()
            .size_full()
            .flex()
            .flex_col()
            .justify_end()
            .child(self.0.clone())
    }
}

fn chip(composer: &Entity<Composer>, cx: &mut VisualTestContext) -> String {
    composer.read_with(cx, |composer, _| {
        composer
            .model_choice()
            .map(ModelChoice::label)
            .unwrap_or_default()
    })
}

fn send(cx: &mut VisualTestContext, text: &str) {
    cx.simulate_input(text);
    cx.simulate_keystrokes("enter");
}

fn opus(scope: ChoiceScope) -> ModelChoice {
    ModelChoice {
        agent: "claude".into(),
        model: Some("opus".into()),
        effort: Some(Effort::High),
        scope,
    }
}

#[gpui::test]
fn effort_row_hidden_for_codex(cx: &mut TestAppContext) {
    let (composer, _, cx) = open_composer(cx);
    click(cx, "composer:model");
    // Fix 31: the picker opens on the listener's own codex, checked.
    assert!(cx.debug_bounds("model-picker").is_some());
    assert!(
        cx.debug_bounds("model-picker:codex:default:check")
            .is_some()
    );
    assert!(cx.debug_bounds("model-picker:effort").is_none());
    assert!(cx.debug_bounds("model-picker:scope:this").is_some());
    // An agent with listed models offers only those unless it is the
    // listener's.
    assert!(cx.debug_bounds("model-picker:claude:default").is_none());

    click(cx, "model-picker:claude:opus");
    assert_eq!(chip(&composer, cx), "opus · high");
    click(cx, "composer:model");
    assert!(cx.debug_bounds("model-picker:effort").is_some());

    click(cx, "model-picker:codex:default");
    let codex = composer.read_with(cx, |c, _| c.model_choice().cloned());
    assert_eq!(codex.and_then(|c| c.effort), None);
}

#[gpui::test]
fn choice_scope_this_message_resets_after_send(cx: &mut TestAppContext) {
    let (composer, sent, cx) = open_composer(cx);
    assert_eq!(chip(&composer, cx), "codex · default");
    click(cx, "composer:model");
    click(cx, "model-picker:claude:opus");
    send(cx, "why?");
    assert_eq!(chip(&composer, cx), "codex · default");
    send(cx, "and then?");
    let models: Vec<_> = sent.borrow().iter().map(|d| d.model.clone()).collect();
    assert_eq!(models, [Some(opus(ChoiceScope::ThisMessage)), None]);
}

#[gpui::test]
fn until_i_change_it_keeps_the_choice(cx: &mut TestAppContext) {
    let (composer, sent, cx) = open_composer(cx);
    click(cx, "composer:model");
    click(cx, "model-picker:claude:opus");
    click(cx, "composer:model");
    click(cx, "model-picker:scope:sticky");
    assert!(
        cx.debug_bounds("model-picker").is_some(),
        "scope keeps it open"
    );
    click(cx, "model-picker:effort-level:4");
    send(cx, "one");
    send(cx, "two");
    assert_eq!(chip(&composer, cx), "opus · max");
    let sticky = ModelChoice {
        effort: Some(Effort::Max),
        ..opus(ChoiceScope::UntilChanged)
    };
    let models: Vec<_> = sent.borrow().iter().map(|d| d.model.clone()).collect();
    assert_eq!(models, [Some(sticky.clone()), Some(sticky)]);

    // Back to "this message": the listener's own model returns after a send.
    click(cx, "composer:model");
    click(cx, "model-picker:scope:this");
    send(cx, "three");
    assert_eq!(chip(&composer, cx), "codex · default");
}

#[gpui::test]
fn clicking_the_open_chip_closes_the_picker(cx: &mut TestAppContext) {
    // C81: the chip's click must not reopen what mouse down closed.
    let (_, _, cx) = open_composer(cx);
    click(cx, "composer:model");
    assert!(cx.debug_bounds("model-picker").is_some());
    click(cx, "composer:model");
    assert!(cx.debug_bounds("model-picker").is_none());
}

#[gpui::test]
fn escape_closes_the_picker_and_keeps_the_draft(cx: &mut TestAppContext) {
    // C87 and C113: Escape closes the picker and goes no further.
    let (composer, sent, cx) = open_composer(cx);
    cx.simulate_input("half typed");
    click(cx, "composer:model");
    cx.simulate_keystrokes("escape");
    redraw(cx);
    assert!(cx.debug_bounds("model-picker").is_none());
    assert!(sent.borrow().is_empty());
    let text = composer.read_with(cx, |c, cx| c.text(cx).to_owned());
    assert_eq!(text, "half typed");
}

#[gpui::test]
fn another_agents_model_names_the_one_off_run(cx: &mut TestAppContext) {
    // Fix 31: a claude model runs one turn outside the codex listener.
    let (_, _, cx) = open_composer(cx);
    click(cx, "composer:model");
    assert!(cx.debug_bounds("model-picker:one-off").is_none());
    click(cx, "model-picker:claude:sonnet");
    click(cx, "composer:model");
    assert!(cx.debug_bounds("model-picker:one-off").is_some());
}

#[gpui::test]
fn picker_opens_above_the_chip(cx: &mut TestAppContext) {
    let (_, _, cx) = open_composer(cx);
    click(cx, "composer:model");
    let picker = cx.debug_bounds("model-picker").expect("the picker");
    let chip = cx.debug_bounds("composer:model").expect("the chip");
    // Node 3355 sits 43 px left of the chip (node 3349) and ends 31 px
    // above its top, 22 px clear of the composer box's top border.
    assert_eq!(
        picker.left(),
        chip.left() - px(43.),
        "{picker:?} by {chip:?}"
    );
    // The picker's height is a fractional 442.5 px at the mockup's rem, and
    // GPUI rounds its origin to a whole pixel.
    let rise = chip.top() - picker.bottom();
    assert!(
        (rise - px(31.)).abs() <= px(0.5),
        "{picker:?} over {chip:?}"
    );
}

#[gpui::test]
fn effort_levels_hug_their_options(cx: &mut TestAppContext) {
    // Node 3392: the levels box ends at "max", not at the section's edge.
    let (_, _, cx) = open_composer(cx);
    click(cx, "composer:model");
    click(cx, "model-picker:claude:opus");
    click(cx, "composer:model");
    let levels = cx
        .debug_bounds("model-picker:effort-level")
        .expect("levels");
    let max = cx.debug_bounds("model-picker:effort-level:4").expect("max");
    // The box's 1 px border sits outside the last option.
    assert_eq!(levels.right(), max.right() + BORDER_WIDTH);
}

#[gpui::test]
fn a_mention_carries_no_model_and_keeps_the_choice(cx: &mut TestAppContext) {
    // The choice is for the listener; a message @mentioning another agent
    // runs on that agent's own model and does not use the choice up.
    let (composer, sent, cx) = open_composer(cx);
    click(cx, "composer:model");
    click(cx, "model-picker:claude:opus");
    send(cx, "@andy/fx1 rebase");
    assert_eq!(chip(&composer, cx), "opus · high");
    send(cx, "why?");
    let models: Vec<_> = sent.borrow().iter().map(|d| d.model.clone()).collect();
    assert_eq!(models, [None, Some(opus(ChoiceScope::ThisMessage))]);
}

/// The chat fixture with the listener on claude.
fn claude_listener() -> Snapshot {
    let mut snapshot = chat_snapshot();
    snapshot.listener.agent = "claude".into();
    snapshot
}

#[gpui::test]
fn picker_checks_the_listeners_own_model(cx: &mut TestAppContext) {
    // An agent with listed models still offers the listener's configured
    // model, checked when the chip shows it.
    let (composer, sent, cx) = open_composer_with(cx, claude_listener());
    assert_eq!(chip(&composer, cx), "claude · default");
    click(cx, "composer:model");
    assert!(
        cx.debug_bounds("model-picker:claude:default:check")
            .is_some()
    );
    click(cx, "model-picker:claude:sonnet");
    click(cx, "composer:model");
    click(cx, "model-picker:scope:sticky");
    // Back to the listener's own model, which a message then runs on.
    click(cx, "model-picker:claude:default");
    assert_eq!(chip(&composer, cx), "claude · default");
    send(cx, "and now?");
    let models: Vec<_> = sent.borrow().iter().map(|d| d.model.clone()).collect();
    assert_eq!(models, [None]);
}

/// The ask screen's fixture: andy's 15:52 question and its answer.
fn ask_screen() -> Snapshot {
    use crate::backend::{Backend as _, FixtureBackend, Screen};
    FixtureBackend::for_screen(Screen::Ask).unwrap().snapshot()
}

#[gpui::test]
fn the_ask_screen_opens_on_the_standing_opus(cx: &mut TestAppContext) {
    // Fix 31 keeps "opus · high" on the ask frame's chip (node 3982): andy
    // picked it until he changes it, so a send keeps it.
    let (composer, sent, cx) = open_composer_with(cx, ask_screen());
    assert_eq!(chip(&composer, cx), "opus · high");
    send(cx, "and C016?");
    assert_eq!(chip(&composer, cx), "opus · high");
    let models: Vec<_> = sent.borrow().iter().map(|d| d.model.clone()).collect();
    assert_eq!(models, [Some(opus(ChoiceScope::UntilChanged))]);
}

#[test]
fn a_standing_model_the_picker_does_not_offer_opens_on_the_listener() {
    let mut snapshot = chat_snapshot();
    snapshot.standing_model = Some(ModelChoice {
        model: Some("gpt-9".into()),
        ..opus(ChoiceScope::UntilChanged)
    });
    assert_eq!(
        ModelMenu::new(&snapshot).current().label(),
        "codex · default"
    );
}

#[test]
fn a_new_snapshot_moves_the_menu_to_its_listener() {
    let mut menu = ModelMenu::new(&chat_snapshot());
    // The person's choice survives a snapshot that still offers it.
    menu.apply(&PickerEvent::Model {
        agent: "claude".into(),
        model: Some("opus".into()),
    });
    menu.apply(&PickerEvent::Scope(ChoiceScope::UntilChanged));
    menu.refresh(&chat_snapshot());
    assert_eq!(menu.current().label(), "opus · high");

    // A new listener agent brings its own model; the sticky opus stays.
    menu.refresh(&claude_listener());
    assert_eq!(menu.current().label(), "opus · high");
    menu.apply(&PickerEvent::Scope(ChoiceScope::ThisMessage));
    menu.sent();
    assert_eq!(menu.current().label(), "claude · default");
    assert_eq!(menu.outgoing(), None);

    // A choice the snapshot no longer offers goes back to the listener's.
    menu.apply(&PickerEvent::Model {
        agent: "claude".into(),
        model: Some("haiku".into()),
    });
    let mut fewer = claude_listener();
    for group in &mut fewer.models {
        group.models.retain(|m| m != "haiku");
    }
    menu.refresh(&fewer);
    assert_eq!(menu.current().label(), "claude · default");
}

#[test]
fn effort_hint_names_each_agent() {
    let groups = chat_snapshot().models;
    assert_eq!(
        effort_hint(&groups),
        "claude takes low to max and agy low to high; codex, opencode and gemini take a model only."
    );
}

#[gpui::test]
fn group_headings_are_mono_11_in_21_px_rows(cx: &mut TestAppContext) {
    // Nodes 3359 and 3360: a heading row is 21 px (padding 6 and 2 around
    // a 13 px line), its text the code font at Small (mono 11, as V93's
    // Git panel metadata), not the code font's own 13 px.
    let (_, _, cx) = open_composer(cx);
    click(cx, "composer:model");
    let heading = cx
        .debug_bounds("model-picker:heading:claude")
        .expect("the heading");
    let first = cx
        .debug_bounds("model-picker:claude:default")
        .or_else(|| cx.debug_bounds("model-picker:claude:haiku"))
        .expect("the first claude row");
    assert_eq!(heading.size.height, px(21.), "{heading:?}");
    assert_eq!(first.top(), heading.bottom(), "{first:?} under {heading:?}");
    let text = cx
        .debug_bounds("model-picker:heading-text:claude")
        .expect("the heading text");
    let snapshot = chat_snapshot();
    let version = snapshot
        .installed_agents
        .iter()
        .find(|agent| agent.name == "claude")
        .and_then(|agent| agent.version())
        .expect("claude's version");
    let label = format!("claude {version}");
    let (small, code) = cx.update(|window, cx| {
        let fonts = &cx.global::<crate::appearance::Appearance>().fonts;
        let mut style = window.text_style();
        style.font_family = fonts.code_family.clone();
        let run = style.to_run(label.len());
        let width = |size| {
            window
                .text_system()
                .shape_line(label.clone().into(), size, std::slice::from_ref(&run), None)
                .width()
        };
        (
            width(crate::ui::tokens::TEXT_SMALL.to_pixels(MOCKUP_REM)),
            width(fonts.code_size),
        )
    });
    assert!(small < code, "{small:?} {code:?}");
    assert!(
        (text.size.width - small).abs() < px(1.),
        "{text:?} against {small:?}"
    );
}
