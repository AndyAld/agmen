use std::cell::Cell;
use std::rc::Rc;

use gpui::{TestAppContext, px};

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
fn check_again_keeps_its_height_beside_the_bordered_cancel(cx: &mut TestAppContext) {
    // Shot diff (nodes 2442 to 2445): Check again is 28 tall, centered on
    // the 30 px bordered Cancel, not stretched to it.
    let cx = draw(cx, || {
        AddAgentDialog::new(AgentDialog::Add("grok".into()), Some(agent("grok"))).into_any_element()
    });
    let cancel = cx.debug_bounds("add-agent:cancel").unwrap();
    let check = cx.debug_bounds("add-agent:check").unwrap();
    assert_eq!(
        cancel.size.height - check.size.height,
        px(2.),
        "{cancel:?} {check:?}"
    );
    assert_eq!(cancel.center().y, check.center().y);
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
}

#[gpui::test]
fn the_generic_dialog_has_no_prompt(cx: &mut TestAppContext) {
    let cx = draw(cx, || {
        AddAgentDialog::new(AgentDialog::Other, None).into_any_element()
    });
    assert!(cx.debug_bounds("add-agent:prompt").is_none());
    assert!(cx.debug_bounds("add-agent:check").is_some());
}

#[test]
fn prompt_lines_set_the_mockups_lines() {
    // Node 2428 at its 69 character measure. Plain wrapping at spaces gives
    // these lines too; prompt_lines_keep_each_command_whole tests the code
    // spans.
    let asked = prompt(&AgentDialog::Add("grok".into()), Some(&agent("grok"))).unwrap();
    let lines = prompt_lines(&asked, crate::ui::tokens::PROMPT_COLUMNS);
    assert_eq!(
        lines,
        [
            "Install xAI's Grok Build CLI (the `grok` command) on this machine",
            "from xAI's official instructions at github.com/xai-org/grok-build. Do",
            "not use the community npm package @vibe-kit/grok-cli; it also",
            "installs a `grok` command. When it is installed, run `grok --version`",
            "and `agmen agents scan`, and tell me both outputs.",
        ]
    );
    // The clipboard text is the prompt itself; the lines only set it.
    assert_eq!(lines.join(" "), asked);
}

#[test]
fn prompt_lines_keep_each_command_whole() {
    // C276: at 68 columns plain wrapping at spaces ends line 4 with
    // "run `grok" and starts line 5 with "--version`". The prompt breaks
    // only at spaces outside its code spans, so the command moves to line
    // 5 whole.
    let asked = prompt(&AgentDialog::Add("grok".into()), Some(&agent("grok"))).unwrap();
    assert_eq!(
        prompt_lines(&asked, 68),
        [
            "Install xAI's Grok Build CLI (the `grok` command) on this machine",
            "from xAI's official instructions at github.com/xai-org/grok-build.",
            "Do not use the community npm package @vibe-kit/grok-cli; it also",
            "installs a `grok` command. When it is installed, run",
            "`grok --version` and `agmen agents scan`, and tell me both outputs.",
        ]
    );
}

#[test]
fn prompt_lines_move_a_code_span_to_the_next_line_whole() {
    assert_eq!(
        prompt_lines("run `agmen agents scan` now", 12),
        ["run", "`agmen agents scan`", "now"]
    );
    assert_eq!(prompt_lines("a `b c` d", 9), ["a `b c` d"]);
    assert_eq!(prompt_lines("a `b c` d", 6), ["a", "`b c`", "d"]);
    assert!(prompt_lines("", 10).is_empty());
}

#[gpui::test]
fn prompt_draws_its_lines_at_the_mockups_measure(cx: &mut TestAppContext) {
    // Node 2428: five set lines, the widest 69 characters (node 2432),
    // well inside the 680 px dialog.
    let cx = draw(cx, || {
        div()
            .w(crate::ui::tokens::DIALOG_WIDTH)
            .child(AddAgentDialog::new(
                AgentDialog::Add("grok".into()),
                Some(agent("grok")),
            ))
            .into_any_element()
    });
    let (cell, line) = cx.update(|window, cx| {
        let fonts = &cx.global::<Appearance>().fonts;
        (
            crate::ui::code_view::cell_width(window, fonts),
            fonts.code_size * crate::ui::tokens::LINE_HEIGHT,
        )
    });
    let text = cx.debug_bounds("add-agent:prompt-text").unwrap();
    let columns = text.size.width / cell;
    assert!((68.5..=69.5).contains(&columns), "{columns} cells");
    let lines = text.size.height / line;
    assert!((lines - 5.).abs() < 0.2, "{lines} lines");
}
