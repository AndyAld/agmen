use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Entity, TestAppContext, VisualTestContext};

use super::*;
use crate::backend::chat_snapshot;
use crate::ui::scope_chip::ScopeOption;
use crate::ui::test_support::{click, init, redraw};

fn panel(cx: &mut TestAppContext) -> (Entity<GitPanel>, &mut VisualTestContext) {
    init(cx);
    let snapshot = chat_snapshot();
    cx.add_window_view(move |_, cx| {
        let options = ScopeOption::for_worktrees(&snapshot.worktrees, &snapshot.agents);
        GitPanel::new(
            snapshot.worktrees.clone(),
            snapshot.git.branches.clone(),
            options,
            cx,
        )
    })
}

#[gpui::test]
fn git_section_headers_share_the_section_header_line_height(cx: &mut TestAppContext) {
    use crate::ui::tokens::AUTO_LINE_11;
    let (_, cx) = panel(cx);
    redraw(cx);
    for selector in ["git-section:changes:title", "git-section:commits:title"] {
        assert_eq!(
            cx.debug_bounds(selector)
                .expect("SectionHeader title")
                .size
                .height,
            AUTO_LINE_11
        );
    }
    assert!(include_str!("git_panel.rs").contains("SectionHeader::new("));
}

#[gpui::test]
fn scope_chip_scopes_the_git_panel(cx: &mut TestAppContext) {
    // The merge of Tasks 11 and 12: the chip switches which worktree's
    // changes and commits the panel shows.
    let (panel, cx) = panel(cx);
    redraw(cx);
    assert!(cx.debug_bounds("scope-chip").is_some());
    let t7k = crate::backend::fixture_id("andy/t7k");
    panel.update(cx, |p, cx| p.switch(&t7k, cx));
    panel.read_with(cx, |p, _| {
        assert_eq!(p.badge(GitSection::Changes), "0");
        assert_eq!(p.badge(GitSection::Commits), "test/b-415");
        assert_eq!(
            p.badge(GitSection::Worktrees),
            "5",
            "the list is not scoped"
        );
    });
}

#[gpui::test]
fn git_sections_counts(cx: &mut TestAppContext) {
    let (panel, cx) = panel(cx);
    redraw(cx);
    panel.read_with(cx, |p, _| {
        assert_eq!(p.badge(GitSection::Changes), "2");
        assert_eq!(p.badge(GitSection::Worktrees), "5");
        assert_eq!(p.badge(GitSection::Branches), "14");
        // The mockup's Commits header names the branch instead of a count.
        assert_eq!(p.badge(GitSection::Commits), "fix/c015");
    });
    for sel in [
        "git-count:changes",
        "git-count:commits",
        "git-count:worktrees",
        "git-count:branches",
    ] {
        assert!(cx.debug_bounds(sel).is_some(), "{sel}");
    }
    for sel in [
        "git-change:src/main.rs",
        "git-change:Cargo.lock",
        "git-commit:9e2a71c",
        "git-commit:016f235",
        "git-person:andy",
        "git-worktree:andy/fx1",
        "git-worktree:andy/t7k",
        "git-worktree:andy/rv2",
        "git-person:sean",
    ] {
        assert!(cx.debug_bounds(sel).is_some(), "{sel}");
    }
}

#[gpui::test]
fn collapsed_state_hides_rows(cx: &mut TestAppContext) {
    let (_, cx) = panel(cx);
    redraw(cx);
    // Branches and other people's worktrees start folded, as in the mockup.
    assert!(cx.debug_bounds("git-branch:main").is_none());
    assert!(cx.debug_bounds("git-worktree:sean/a1").is_none());
    click(cx, "git-section:changes");
    assert!(cx.debug_bounds("git-change:src/main.rs").is_none());
    assert!(cx.debug_bounds("git-commit:9e2a71c").is_some());
    click(cx, "git-section:changes");
    assert!(cx.debug_bounds("git-change:src/main.rs").is_some());
    click(cx, "git-section:branches");
    assert!(cx.debug_bounds("git-branch:main").is_some());
    click(cx, "git-person:sean");
    assert!(cx.debug_bounds("git-worktree:sean/a1").is_some());
    assert!(cx.debug_bounds("git-worktree:sean/plan").is_some());
    click(cx, "git-section:worktrees");
    assert!(cx.debug_bounds("git-person:andy").is_none());
    assert!(cx.debug_bounds("git-worktree:sean/a1").is_none());
}

#[gpui::test]
fn selecting_change_opens_diff_tab(cx: &mut TestAppContext) {
    let (panel, cx) = panel(cx);
    let opened = Rc::new(RefCell::new(Vec::new()));
    let sink = opened.clone();
    cx.update(|_, cx| {
        cx.subscribe(&panel, move |_, event: &GitPanelEvent, _| {
            sink.borrow_mut().push(event.clone());
        })
        .detach();
    });
    click(cx, "git-change:Cargo.lock");
    assert_eq!(
        *opened.borrow(),
        vec![GitPanelEvent::OpenDiff {
            worktree: "andy/fx1".into(),
            path: "Cargo.lock".into(),
        }]
    );
    panel.read_with(cx, |p, _| assert_eq!(p.selected(), Some("Cargo.lock")));
}

#[gpui::test]
fn select_commit_ignores_a_commit_the_scoped_worktree_lacks(cx: &mut TestAppContext) {
    // C313: 9e2a71c is andy/fx1's, so with andy/t7k scoped no row lights.
    let (panel, cx) = panel(cx);
    let t7k = crate::backend::fixture_id("andy/t7k");
    let sha: GitSha = "9e2a71c".parse().unwrap();
    panel.update(cx, |p, cx| {
        p.switch(&t7k, cx);
        p.select_commit(&sha, cx);
    });
    panel.read_with(cx, |p, _| assert_eq!(p.selected_commit(), None));
}

#[gpui::test]
fn git_panel_keeps_one_selection(cx: &mut TestAppContext) {
    // C320, V137: frame 5057 has one selected row, so a change click
    // clears the selected commit and a selected commit clears the change.
    let (panel, cx) = panel(cx);
    let sha: GitSha = "9e2a71c".parse().unwrap();
    panel.update(cx, |p, cx| p.select_commit(&sha, cx));
    redraw(cx);
    click(cx, "git-change:Cargo.lock");
    panel.read_with(cx, |p, _| {
        assert_eq!(p.selected(), Some("Cargo.lock"));
        assert_eq!(p.selected_commit(), None, "the change click clears it");
    });
    panel.update(cx, |p, cx| p.select_commit(&sha, cx));
    panel.read_with(cx, |p, _| {
        assert_eq!(p.selected_commit(), Some(&sha));
        assert_eq!(p.selected(), None, "the commit clears the change");
    });
}

#[test]
fn worktree_rows_name_head_and_agent() {
    let worktrees = chat_snapshot().worktrees;
    let rv2 = worktrees.iter().find(|w| w.handle == "andy/rv2").unwrap();
    // Mockup fix 51: rv2 reads C015 detached at fx1's commit.
    assert_eq!(rv2.head.label(), "detached @9e2a71c");
    assert_eq!(rv2.state, WorktreeState::Reading);
    assert_eq!(agent_name(&rv2.handle), "rv2");
    assert_eq!(person(&rv2.handle), "andy");
}

#[test]
fn commit_tooltip_shows_author_and_local_time() {
    let fx1 = chat_snapshot().worktrees.into_iter().next().unwrap();
    let denver = jiff::tz::TimeZone::get("America/Denver").unwrap();
    let merge = fx1.commits.get(1).unwrap();
    assert_eq!(commit_tooltip(merge, &denver), "sean/a1 · 14:58");
    // The person's own agents drop the person, as the row does.
    assert_eq!(commit_author("andy/fx1", "andy/fx1"), "fx1");
    assert_eq!(commit_author("sean/a1", "andy/fx1"), "sean/a1");
}

#[test]
fn commits_header_cuts_branch_to_its_item() {
    assert_eq!(branch_item("fix/c015-main-conflict"), "fix/c015");
    assert_eq!(branch_item("test/b-415-replay-paging"), "test/b-415");
    assert_eq!(branch_item("plan/ridgeline"), "plan/ridgeline");
    assert_eq!(branch_item("main"), "main");
    assert_eq!(branch_item("fix/-12"), "fix/-12");
}

fn at(cx: &mut VisualTestContext, selector: &str) -> gpui::Bounds<gpui::Pixels> {
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
        .unwrap_or_else(|| panic!("nothing is drawn as {selector}"))
}

#[gpui::test]
fn change_rows_put_the_letter_at_24_and_a_16_px_icon_after_it(cx: &mut TestAppContext) {
    // V91: node 5115 is console.py's tree_row at depth 1 (padding 24, gap
    // 6): M at x 24, the 16 px icon at 38 and the path at 60.
    let (_, cx) = panel(cx);
    redraw(cx);
    let status = at(cx, "git-change:src/main.rs:status");
    let icon = at(cx, "git-change:src/main.rs:icon");
    let path = at(cx, "git-change:src/main.rs:path");
    assert_eq!(status.left(), gpui::px(24.), "{status:?}");
    assert_eq!(icon.size.width, gpui::px(16.), "{icon:?}");
    assert_eq!(icon.left(), status.right() + gpui::px(6.), "{icon:?}");
    assert_eq!(path.left(), icon.right() + gpui::px(6.), "{path:?}");
}

#[gpui::test]
fn git_rows_sit_on_the_tree_indent(cx: &mut TestAppContext) {
    // V91 and V101: commit row 5135 has its sha at x 24; person row 5156
    // its chevron at 24 and name at 42; worktree row 5160 its dot at 38
    // and branch 6 px after it.
    let (_, cx) = panel(cx);
    redraw(cx);
    let sha = at(cx, "git-commit:9e2a71c:sha");
    assert_eq!(sha.left(), gpui::px(24.), "{sha:?}");
    let chevron = at(cx, "git-person:andy:leading");
    let name = at(cx, "git-person:andy:name");
    assert_eq!(chevron.left(), gpui::px(24.), "{chevron:?}");
    assert_eq!(name.left(), gpui::px(42.), "{name:?}");
    let dot = at(cx, "git-worktree:andy/fx1:dot");
    let head = at(cx, "git-worktree:andy/fx1:head");
    assert_eq!(dot.left(), gpui::px(38.), "{dot:?}");
    assert_eq!(dot.size.width, crate::ui::tokens::DOT_SIZE, "{dot:?}");
    assert_eq!(head.left(), dot.right() + gpui::px(6.), "{head:?}");
}

#[test]
fn git_metadata_is_mono_placeholder() {
    // V93: the M letter is mono 12 Bold (node 5116); +2 -5, authors, the
    // folded person's count and worktree agents are mono 11 in
    // text.placeholder (nodes 5121, 5139, 5164, 5179).
    assert_eq!(
        meta("fx1"),
        Label::new("fx1")
            .code()
            .size(LabelSize::Small)
            .color(Color::Placeholder)
    );
    assert_eq!(
        status_letter(ChangeKind::Modified),
        Label::new("M")
            .code()
            .weight(FontWeight::BOLD)
            .color(Color::Warning)
    );
    let theme = crate::appearance::Appearance::builtin().theme;
    assert_eq!(Color::Placeholder.hsla(&theme), gpui::rgb(0x928374).into());
    assert_eq!(Color::Warning.hsla(&theme), gpui::rgb(0xdda752).into());
}

#[test]
fn section_counts_are_mono_placeholder() {
    // V94: node 5113 draws the count in text.placeholder, mono 11; the
    // accepted difference covers only the pill's fill.
    assert_eq!(
        count_badge("2"),
        Badge::new("2", Color::Placeholder).count()
    );
}

#[gpui::test]
fn section_counts_draw_as_count_chips(cx: &mut TestAppContext) {
    // V114: node 5113 is 13 px tall, mono 11 padded 0/6; at the 15 px rem
    // the chip is Small's 11.25 px times 1.2, not a 16 px pill.
    let (_, cx) = panel(cx);
    cx.update(|window, _| window.set_rem_size(crate::ui::tokens::MOCKUP_REM));
    redraw(cx);
    let count = at(cx, "git-count:changes");
    assert_eq!(count.size.height, gpui::px(13.5), "{count:?}");
}

#[gpui::test]
fn count_chips_are_drawn_on_the_elevated_surface(cx: &mut TestAppContext) {
    // V114, drawn per C254: node 5113's chip fills with the elevated
    // surface (`Badge::count`).
    let (_, cx) = panel(cx);
    redraw(cx);
    let theme = cx.update(|_, cx| cx.global::<crate::appearance::Appearance>().theme.clone());
    let fills = crate::ui::test_support::fills_at(cx, "git-count:changes");
    assert_eq!(fills, [theme.elevated_surface]);
}
