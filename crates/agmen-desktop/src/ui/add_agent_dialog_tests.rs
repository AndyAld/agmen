use std::cell::Cell;
use std::rc::Rc;

use gpui::TestAppContext;

use super::*;
use crate::backend::Backend as _;
use crate::backend::{FixtureBackend, Screen};
use crate::ui::components::split_code;
use crate::ui::test_support::{click, draw};

fn agent(name: &str) -> InstalledAgent {
    FixtureBackend::for_screen(Screen::AddAgent)
        .unwrap()
        .snapshot()
        .installed_agents
        .into_iter()
        .find(|a| a.name == name)
        .unwrap()
}

#[test]
fn add_agent_prompt_names_official_source_and_warns() {
    // Spec 5.2: the official source, the look-alike package, and the two
    // commands at the end, in order.
    let asked = prompt(&AgentDialog::Add("grok".into()), Some(&agent("grok"))).unwrap();
    assert!(asked.starts_with("Install xAI's Grok Build CLI (the `grok` command)"));
    assert!(asked.contains("from xAI's official instructions at github.com/xai-org/grok-build."));
    assert!(asked.contains(
        "Do not use the community npm package @vibe-kit/grok-cli; it also installs a `grok` command."
    ));
    let (text, spans) = split_code(&asked);
    let commands: Vec<&str> = spans.iter().map(|s| &text[s.clone()]).collect();
    assert_eq!(
        commands[commands.len() - 2..],
        ["grok --version", "agmen agents scan"]
    );
    assert!(asked.ends_with("and `agmen agents scan`, and tell me both outputs."));
}

#[test]
fn update_prompt_names_the_minimum_and_no_source_it_lacks() {
    let asked = prompt(&AgentDialog::Update("agy".into()), Some(&agent("agy"))).unwrap();
    assert!(asked.contains("to 1.1.15 or later"), "{asked}");
    assert!(!asked.contains("official instructions at"), "{asked}");
    assert!(
        asked.ends_with("run `agy --version` and `agmen agents scan`, and tell me both outputs.")
    );
    assert_eq!(prompt(&AgentDialog::Other, None), None);
}

#[test]
fn titles_follow_the_dialog() {
    assert_eq!(
        title(&AgentDialog::Add("grok".into()), Some(&agent("grok"))),
        "Add Grok Build"
    );
    assert_eq!(
        title(&AgentDialog::Update("agy".into()), Some(&agent("agy"))),
        "Update Antigravity"
    );
    assert_eq!(title(&AgentDialog::Other, None), "Add a terminal agent");
}

#[gpui::test]
fn copy_prompt_writes_clipboard(cx: &mut TestAppContext) {
    // GPUI's test clipboard, never the system one.
    let copied = Rc::new(Cell::new(false));
    let seen = copied.clone();
    let cx = draw(cx, move || {
        let seen = seen.clone();
        AddAgentDialog::new(AgentDialog::Add("grok".into()), Some(agent("grok")))
            .on_copied(move |_, _| seen.set(true))
            .into_any_element()
    });
    assert!(cx.read_from_clipboard().is_none());
    click(cx, "add-agent:copy");
    let expected = prompt(&AgentDialog::Add("grok".into()), Some(&agent("grok"))).unwrap();
    let item = cx.read_from_clipboard().unwrap();
    assert_eq!(item.text().as_deref(), Some(expected.as_str()));
    assert!(copied.get());
}

#[gpui::test]
fn dialog_draws_source_prompt_and_buttons(cx: &mut TestAppContext) {
    let cx = draw(cx, || {
        AddAgentDialog::new(AgentDialog::Add("grok".into()), Some(agent("grok"))).into_any_element()
    });
    for part in [
        "add-agent",
        "add-agent:link",
        "add-agent:prompt",
        "add-agent:copy",
        "add-agent:cancel",
        "add-agent:check",
    ] {
        assert!(cx.debug_bounds(part).is_some(), "{part}");
    }
    let dialog = cx.debug_bounds("add-agent").unwrap();
    assert_eq!(dialog.size.width, DIALOG_WIDTH);
}

#[gpui::test]
fn the_generic_dialog_has_no_prompt(cx: &mut TestAppContext) {
    let cx = draw(cx, || {
        AddAgentDialog::new(AgentDialog::Other, None).into_any_element()
    });
    assert!(cx.debug_bounds("add-agent:prompt").is_none());
    assert!(cx.debug_bounds("add-agent:check").is_some());
}
