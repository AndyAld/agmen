use std::cell::RefCell;
use std::rc::Rc;

use gpui::{TestAppContext, VisualTestContext};

use super::*;
use crate::backend::chat_snapshot;
use crate::ui::test_support::{click, init, redraw};
use crate::ui::tokens::{MOCKUP_REM, TAB_CLOSE_SLOT};

/// The window's root in these tests: the center at the mockup's rem size.
struct Host(Entity<PaneGroup>);

impl Render for Host {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(MOCKUP_REM);
        div().size_full().flex().child(self.0.clone())
    }
}

fn open_center(cx: &mut TestAppContext) -> (Entity<PaneGroup>, &mut VisualTestContext) {
    init(cx);
    let snapshot = chat_snapshot();
    let group = cx.new(|cx| PaneGroup::new(&snapshot, cx));
    let host = group.clone();
    let (_, cx) = cx.add_window_view(|_, _| Host(host));
    redraw(cx);
    (group, cx)
}

fn fx1() -> TabItem {
    let snapshot = chat_snapshot();
    let id = snapshot
        .roster
        .iter()
        .flat_map(|g| &g.agents)
        .find(|a| a.handle == "andy/fx1")
        .map(|a| a.id.clone())
        .expect("andy/fx1 is in the fixture roster");
    TabItem::Agent(id)
}

fn first_pane(group: &Entity<PaneGroup>, cx: &VisualTestContext) -> Entity<Pane> {
    group.read_with(cx, |group, _| group.panes()[0].clone())
}

#[gpui::test]
fn pane_starts_on_chat(cx: &mut TestAppContext) {
    let (group, cx) = open_center(cx);
    let pane = first_pane(&group, cx);
    pane.read_with(cx, |pane, _| {
        assert_eq!(pane.tabs(), &[TabItem::Chat]);
        assert_eq!(pane.active_item(), Some(&TabItem::Chat));
    });
}

#[gpui::test]
fn open_adds_a_tab_and_activates_it(cx: &mut TestAppContext) {
    let (group, cx) = open_center(cx);
    let pane = first_pane(&group, cx);
    pane.update(cx, |pane, cx| pane.open(fx1(), cx));
    pane.read_with(cx, |pane, _| {
        assert_eq!(pane.tabs(), &[TabItem::Chat, fx1()]);
        assert_eq!(pane.active_item(), Some(&fx1()));
    });
    // Opening an item that has a tab activates that tab instead of adding one.
    pane.update(cx, |pane, cx| pane.open(TabItem::Chat, cx));
    pane.read_with(cx, |pane, _| {
        assert_eq!(pane.tabs().len(), 2);
        assert_eq!(pane.active_item(), Some(&TabItem::Chat));
    });
}

#[gpui::test]
fn clicking_a_tab_activates_it(cx: &mut TestAppContext) {
    let (group, cx) = open_center(cx);
    let pane = first_pane(&group, cx);
    pane.update(cx, |pane, cx| {
        pane.open(fx1(), cx);
        pane.activate(0, cx);
    });
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.active_item().cloned()),
        Some(TabItem::Chat)
    );
    click(cx, "pane-0:tab-1");
    assert_eq!(
        pane.read_with(cx, |pane, _| pane.active_item().cloned()),
        Some(fx1())
    );
}

#[gpui::test]
fn close_removes_the_tab_and_activates_a_neighbor(cx: &mut TestAppContext) {
    let (group, cx) = open_center(cx);
    let pane = first_pane(&group, cx);
    pane.update(cx, |pane, cx| {
        pane.open(fx1(), cx);
        pane.open(TabItem::Plan, cx);
        pane.activate(1, cx);
    });
    click(cx, "pane-0:tab-1:close-button");
    pane.read_with(cx, |pane, _| {
        assert_eq!(pane.tabs(), &[TabItem::Chat, TabItem::Plan]);
        assert_eq!(pane.active_item(), Some(&TabItem::Plan));
    });
    // Closing a tab left of the active one keeps the same tab active.
    pane.update(cx, |pane, cx| pane.close(0, cx));
    pane.read_with(cx, |pane, _| {
        assert_eq!(pane.tabs(), &[TabItem::Plan]);
        assert_eq!(pane.active_item(), Some(&TabItem::Plan));
    });
}

#[gpui::test]
fn close_slot_is_reserved_on_every_tab(cx: &mut TestAppContext) {
    let (group, cx) = open_center(cx);
    let pane = first_pane(&group, cx);
    pane.update(cx, |pane, cx| {
        pane.open(fx1(), cx);
        pane.activate(0, cx);
    });
    redraw(cx);
    let width = |cx: &mut VisualTestContext, selector: &'static str| {
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("nothing drawn as {selector}"))
            .size
            .width
    };
    assert_eq!(width(cx, "pane-0:tab-0:close"), TAB_CLOSE_SLOT);
    assert_eq!(width(cx, "pane-0:tab-1:close"), TAB_CLOSE_SLOT);
    let chat_active = width(cx, "pane-0:tab-0");
    let fx1_inactive = width(cx, "pane-0:tab-1");

    pane.update(cx, |pane, cx| pane.activate(1, cx));
    redraw(cx);
    assert_eq!(width(cx, "pane-0:tab-0"), chat_active);
    assert_eq!(width(cx, "pane-0:tab-1"), fx1_inactive);
}

#[gpui::test]
fn show_ledger_toggle_reports_each_change(cx: &mut TestAppContext) {
    let (group, cx) = open_center(cx);
    let reported = Rc::new(RefCell::new(Vec::new()));
    let sink = reported.clone();
    cx.update(|_, cx| {
        cx.subscribe(&group, move |_, event: &PaneGroupEvent, _| {
            let PaneGroupEvent::ShowLedger(on) = event;
            sink.borrow_mut().push(*on);
        })
        .detach();
    });
    let pane = first_pane(&group, cx);
    assert!(!pane.read_with(cx, |pane, _| pane.show_ledger()));

    click(cx, "pane-0:show-ledger");
    assert!(pane.read_with(cx, |pane, _| pane.show_ledger()));
    click(cx, "pane-0:show-ledger");
    assert!(!pane.read_with(cx, |pane, _| pane.show_ledger()));
    assert_eq!(*reported.borrow(), vec![true, false]);
}

#[gpui::test]
fn split_right_opens_a_second_pane_beside_the_first(cx: &mut TestAppContext) {
    let (group, cx) = open_center(cx);
    let second = group
        .update(cx, |group, cx| {
            group.split(SplitDirection::Right, fx1(), cx)
        })
        .expect("one pane has room to split");
    second.read_with(cx, |pane, _| {
        assert_eq!(pane.tabs(), &[fx1()]);
    });
    group.read_with(cx, |group, _| {
        assert_eq!(group.panes().len(), 2);
        assert_eq!(group.direction(), Some(SplitDirection::Right));
    });
    redraw(cx);
    let left = cx.debug_bounds("pane-0").expect("first pane drawn");
    let right = cx.debug_bounds("pane-1").expect("second pane drawn");
    assert!(right.left() >= left.right());
    assert_eq!(right.top(), left.top());
}

#[gpui::test]
fn split_count_is_bounded(cx: &mut TestAppContext) {
    let (group, cx) = open_center(cx);
    for _ in 1..MAX_PANES {
        group
            .update(cx, |group, cx| {
                group.split(SplitDirection::Right, fx1(), cx)
            })
            .expect("room for another pane");
    }
    let refused = group.update(cx, |group, cx| {
        group.split(SplitDirection::Right, fx1(), cx).map(|_| ())
    });
    assert_eq!(refused, Err(SplitError::Full));
    assert_eq!(
        group.read_with(cx, |group, _| group.panes().len()),
        MAX_PANES
    );
}

#[gpui::test]
fn split_down_after_right_is_refused(cx: &mut TestAppContext) {
    let (group, cx) = open_center(cx);
    group
        .update(cx, |group, cx| {
            group.split(SplitDirection::Right, fx1(), cx)
        })
        .expect("room to split");
    let refused = group.update(cx, |group, cx| {
        group.split(SplitDirection::Down, fx1(), cx).map(|_| ())
    });
    assert_eq!(refused, Err(SplitError::OtherDirection));
}

#[gpui::test]
fn closing_the_last_tab_of_a_split_removes_its_pane(cx: &mut TestAppContext) {
    let (group, cx) = open_center(cx);
    group
        .update(cx, |group, cx| {
            group.split(SplitDirection::Right, fx1(), cx)
        })
        .expect("room to split");
    click(cx, "pane-1:tab-0:close-button");
    group.read_with(cx, |group, _| {
        assert_eq!(group.panes().len(), 1);
        assert_eq!(group.direction(), None);
    });
}

#[gpui::test]
fn tabs_are_bounded(cx: &mut TestAppContext) {
    let (group, cx) = open_center(cx);
    let pane = first_pane(&group, cx);
    pane.update(cx, |pane, cx| {
        for i in 0..MAX_TABS + 5 {
            pane.open(TabItem::Spec(format!("docs/specs/{i}.md")), cx);
        }
    });
    pane.read_with(cx, |pane, _| {
        assert_eq!(pane.tabs().len(), MAX_TABS);
        let last = format!("docs/specs/{}.md", MAX_TABS + 4);
        assert_eq!(pane.active_item(), Some(&TabItem::Spec(last)));
    });
}

#[test]
fn tab_labels_follow_the_mockup() {
    let names = PaneNames::from_snapshot(&chat_snapshot());
    assert_eq!(names.tab_label(&TabItem::Chat), "chat");
    assert_eq!(names.tab_label(&fx1()), "andy/fx1");
    assert_eq!(
        names.tab_label(&TabItem::RoleFile("reviewer".into())),
        "reviewer.xml"
    );
    assert_eq!(
        names.tab_label(&TabItem::Diff {
            worktree: "fx1".into(),
            path: "src/main.rs".into()
        }),
        "main.rs (diff)"
    );
    let main_rs = TabItem::File {
        worktree: "fx1".into(),
        path: "src/main.rs".into(),
    };
    assert_eq!(names.tab_label(&main_rs), "main.rs");
    assert_eq!(
        names.breadcrumb(&main_rs),
        vec!["fx1".to_owned(), "src/main.rs".to_owned()]
    );
    let settings = TabItem::Settings(SettingsPage::Appearance);
    assert_eq!(names.tab_label(&settings), "settings");
    assert_eq!(
        names.breadcrumb(&settings),
        vec!["settings".to_owned(), "Appearance".to_owned()]
    );
    assert_eq!(
        names.breadcrumb(&TabItem::Chat),
        vec!["pr-3331".to_owned(), "chat".to_owned()]
    );
}
