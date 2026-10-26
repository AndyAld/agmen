use super::*;
use crate::appearance::Appearance;

fn cell(ch: char, fg: TermColor, bold: bool) -> TermCell {
    TermCell {
        ch,
        fg,
        bg: TermColor::Default,
        bold,
    }
}

#[test]
fn cells_of_one_look_make_one_run() {
    let red = TermColor::Ansi(1);
    let cells = [
        cell('a', red, false),
        cell('b', red, false),
        cell('c', red, true),
        cell(' ', TermColor::Default, false),
    ];
    let runs = row_runs(&cells, None);
    let texts: Vec<&str> = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, ["ab", "c", " "]);
    assert!(runs[1].bold);
}

#[test]
fn the_cursor_is_a_run_of_its_own() {
    let cells = [
        cell('>', TermColor::Default, true),
        cell(' ', TermColor::Default, false),
        cell(' ', TermColor::Default, false),
    ];
    let runs = row_runs(&cells, Some(1));
    let texts: Vec<(&str, bool)> = runs.iter().map(|r| (r.text.as_str(), r.cursor)).collect();
    assert_eq!(texts, [(">", false), (" ", true), (" ", false)]);
}

#[test]
fn cell_colors_come_from_the_theme_or_the_program() {
    let theme = Appearance::builtin().theme;
    let fg = theme.terminal_foreground;
    assert_eq!(term_color(TermColor::Default, fg, &theme), fg);
    assert_eq!(term_color(TermColor::Ansi(8), fg, &theme), theme.ansi[8]);
    assert_eq!(term_color(TermColor::Ansi(99), fg, &theme), fg);
    let orange = term_color(TermColor::Rgb([0xea, 0x89, 0x4b]), fg, &theme).to_rgb();
    assert!((orange.r - 0xea as f32 / 255.).abs() < 0.002);
}

#[test]
fn the_head_says_how_the_session_started() {
    let resumed = SessionStart::Resumed("3f9c2e1a-7b40-4c1e-9d6a-2f0e8c5b1a77".into());
    assert_eq!(start_label(&resumed), "--resume 3f9c2e1a");
    assert_eq!(
        start_label(&SessionStart::Handoff),
        "new session · handoff brief"
    );
}
