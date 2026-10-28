use gpui::Keystroke;

use super::*;

/// Whether the palette leaves `action` out: its menu keys and the text
/// input's editing keys. Zed lists the palette's own toggle, so it does too.
#[must_use]
pub(crate) fn is_internal(action: &dyn Action) -> bool {
    let name = action.name();
    name.starts_with("menu::") || name.starts_with("text_input::")
}

/// The bindings whose keystrokes match `keys` in `context`.
fn bound(keys: &'static str, context: Option<&str>) -> Vec<Binding> {
    let typed: Vec<Keystroke> = keys
        .split(' ')
        .map(|k| Keystroke::parse(k).unwrap())
        .collect();
    table()
        .into_iter()
        .filter(|b| b.context == context && b.key_binding.match_keystrokes(&typed) == Some(false))
        .collect()
}

#[test]
fn bindings_do_not_collide() {
    // A collision is one keystroke bound to two different actions in the
    // same context. The palette's up, down and enter override the text
    // input's in a deeper context, so they are not collisions.
    let table = table();
    for (i, a) in table.iter().enumerate() {
        for b in &table[i + 1..] {
            let same_keys = a.key_binding.keystrokes() == b.key_binding.keystrokes();
            assert!(
                !(same_keys && a.context == b.context),
                "{} is bound twice in {:?}",
                a.keys,
                a.context
            );
        }
    }
}

#[test]
fn every_action_has_palette_entry() {
    let commands = commands();
    for binding in table() {
        let action = binding.key_binding.action();
        if is_internal(action) {
            continue;
        }
        assert!(
            commands.iter().any(|c| c.action.partial_eq(action)),
            "{} ({}) has no palette entry",
            binding.keys,
            action.name()
        );
    }
    // Agents and Roles have no key: the palette and a click reach them.
    for action in [
        Box::new(ToggleAgentsPanel) as Box<dyn Action>,
        Box::new(ToggleRolesPanel),
        Box::new(ToggleVimMode),
    ] {
        assert!(
            commands.iter().any(|c| c.action.partial_eq(&*action)),
            "{} has no palette entry",
            action.name()
        );
    }
}

#[test]
fn palette_entries_are_unique_and_skip_internal_actions() {
    let commands = commands();
    for (i, a) in commands.iter().enumerate() {
        assert!(!is_internal(&*a.action), "{} is internal", a.label);
        for b in &commands[i + 1..] {
            assert_ne!(a.label, b.label);
            assert!(!a.action.partial_eq(&*b.action), "{} twice", a.label);
        }
    }
}

#[test]
fn v86_set_is_bound_in_the_workspace() {
    let ws = Some(WORKSPACE_CONTEXT);
    let cases: [(&str, Box<dyn Action>); 14] = [
        ("cmd-b", Box::new(ToggleLeftDock)),
        ("cmd-r", Box::new(ToggleRightDock)),
        ("cmd-alt-b", Box::new(ToggleRightDock)),
        ("cmd-shift-e", Box::new(ToggleProjectPanel)),
        ("ctrl-shift-g", Box::new(ToggleGitPanel)),
        ("cmd-1", Box::new(ActivatePane(0))),
        ("cmd-9", Box::new(ActivatePane(8))),
        ("cmd-w", Box::new(CloseActiveTab)),
        ("cmd-\\", Box::new(SplitRight)),
        ("cmd-shift-p", Box::new(ToggleCommandPalette)),
        ("cmd-,", Box::new(OpenSettings)),
        ("cmd-shift-f", Box::new(OpenSearch)),
        ("ctrl-`", Box::new(ToggleTerminal)),
        ("cmd-n", Box::new(SpawnWorker)),
    ];
    for (keys, action) in cases {
        let found = bound(keys, ws);
        assert_eq!(found.len(), 1, "{keys}");
        assert!(found[0].key_binding.action().partial_eq(&*action), "{keys}");
    }
    // Quit works in every window, so it has no context.
    let quit = bound("cmd-q", None);
    assert_eq!(quit.len(), 1);
    assert!(quit[0].key_binding.action().partial_eq(&Quit));
}

#[test]
fn palette_keys_override_the_text_input_after_it() {
    // Equal depth goes to the binding added later, so the palette's
    // up, down and enter must come after the text input's.
    let table = table();
    let position = |keys: &str, ctx: &str| {
        table
            .iter()
            .position(|b| b.keys == keys && b.context == Some(ctx))
            .unwrap()
    };
    for keys in ["up", "down", "enter"] {
        assert!(position(keys, PALETTE_INPUT_CONTEXT) > position(keys, TEXT_INPUT_CONTEXT));
    }
    assert_eq!(bound("escape", Some(PALETTE_CONTEXT)).len(), 1);
}

#[test]
fn glyphs_follow_zed_mac_order() {
    assert_eq!(glyphs("cmd-shift-p"), "⌘⇧P");
    assert_eq!(glyphs("cmd-alt-b"), "⌥⌘B");
    assert_eq!(glyphs("ctrl-shift-g"), "⌃⇧G");
    assert_eq!(glyphs("ctrl-`"), "⌃`");
    assert_eq!(glyphs("alt-o"), "⌥O");
    assert_eq!(glyphs("cmd-,"), "⌘,");
    assert_eq!(glyphs("cmd-\\"), "⌘\\");
    assert_eq!(glyphs("enter"), "⏎");
    assert_eq!(glyphs("escape"), "⎋");
    assert_eq!(glyphs("up"), "↑");
    assert_eq!(glyphs("home"), "Home");
    assert_eq!(glyphs("cmd-k cmd-s"), "⌘K ⌘S");
}

#[test]
fn hint_shows_the_first_binding() {
    let t = table();
    assert_eq!(hint(&t, &ToggleRightDock).as_deref(), Some("⌘R"));
    assert_eq!(hint(&t, &ToggleCommandPalette).as_deref(), Some("⌘⇧P"));
    assert_eq!(hint(&t, &ActivatePane(2)).as_deref(), Some("⌘3"));
    assert_eq!(hint(&t, &ToggleAgentsPanel), None);
}

#[test]
fn cmd_comma_opens_settings() {
    // V86: Zed binds cmd-comma to zed::OpenSettings.
    let found = bound("cmd-,", Some(WORKSPACE_CONTEXT));
    assert_eq!(found.len(), 1);
    assert!(found[0].key_binding.action().partial_eq(&OpenSettings));
}

#[test]
fn quit_runs_without_a_window() {
    let quit = commands()
        .into_iter()
        .find(|c| c.action.partial_eq(&Quit))
        .unwrap();
    assert_eq!(quit.scope, Scope::App);
    assert_eq!(quit.label, "agmen: quit");
}

/// Whether `binding` applies with `stack` focused, outermost first, as
/// gpui's keymap decides it: the predicate matches at some depth.
fn applies(binding: &Binding, stack: &[&str]) -> bool {
    let contexts: Vec<gpui::KeyContext> = stack
        .iter()
        .map(|c| gpui::KeyContext::parse(c).unwrap())
        .collect();
    binding
        .key_binding
        .predicate()
        .is_none_or(|predicate| predicate.depth_of(&contexts).is_some())
}

#[test]
fn alt_o_leaves_text_inputs_their_character() {
    // gpui on macOS runs a matched binding before text input
    // (prefer_character_input is always false), so a binding inside a text
    // input would make "ø" impossible to type.
    let table = table();
    let alt_o: Vec<_> = table
        .iter()
        .filter(|b| b.keys == OPEN_IN_TERMINAL_KEYS)
        .collect();
    assert_eq!(alt_o.len(), 1);
    assert!(alt_o[0].key_binding.action().partial_eq(&OpenInTerminal));
    assert!(applies(alt_o[0], &[WORKSPACE_CONTEXT]));
    assert!(!applies(alt_o[0], &[WORKSPACE_CONTEXT, TEXT_INPUT_CONTEXT]));
    assert!(!applies(
        alt_o[0],
        &[WORKSPACE_CONTEXT, PALETTE_CONTEXT, TEXT_INPUT_CONTEXT]
    ));
}

#[test]
fn palette_lists_its_own_toggle_with_its_key() {
    let commands = commands();
    let toggle = commands
        .iter()
        .find(|c| c.action.partial_eq(&ToggleCommandPalette))
        .expect("Zed's palette lists command palette: toggle");
    assert_eq!(toggle.label, "command palette: toggle");
    assert_eq!(toggle.scope, Scope::Window);
    assert_eq!(hint(&table(), &ToggleCommandPalette), Some(palette_hint()));
}

#[test]
fn hint_keys_have_one_home() {
    let table = table();
    assert_eq!(
        hint(&table, &ToggleCommandPalette),
        Some(glyphs(TOGGLE_PALETTE_KEYS))
    );
    assert_eq!(
        hint(&table, &OpenInTerminal),
        Some(glyphs(OPEN_IN_TERMINAL_KEYS))
    );
    assert_eq!(palette_hint(), "⌘⇧P");
    assert_eq!(open_in_terminal_hint(), "⌥O");
}
