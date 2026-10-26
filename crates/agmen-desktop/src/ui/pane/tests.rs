use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    TestAppContext, VisualTestContext, Window, div,
};

use super::group::{MAX_PANES, SplitError};
use super::item::SettingsPage;
use super::names::PaneNames;
use super::tabs::{MAX_TABS, Pane};
use super::*;
use crate::backend::chat_snapshot;
use crate::ui::chat::ChatView;
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
    let group = cx.new(|cx| PaneGroup::new(&snapshot, None, Vec::new(), cx));
    let host = group.clone();
    let (_, cx) = cx.add_window_view(|_, _| Host(host));
    redraw(cx);
    (group, cx)
}

/// The center over the chat fixture with its chat, and `tabs` behind it.
fn open_center_with_chat(
    cx: &mut TestAppContext,
    tabs: Vec<TabItem>,
) -> (Entity<PaneGroup>, Entity<ChatView>, &mut VisualTestContext) {
    init(cx);
    let (host, cx) = cx.add_window_view(|window, cx| {
        let snapshot = chat_snapshot();
        let chat = cx.new(|cx| ChatView::new(snapshot.clone(), window, cx));
        Host(cx.new(|cx| PaneGroup::new(&snapshot, Some(chat), tabs, cx)))
    });
    let group = host.read_with(cx, |host, _| host.0.clone());
    let chat = group.read_with(cx, |group, cx| {
        group.panes()[0].read(cx).chat().cloned().unwrap()
    });
    redraw(cx);
    (group, chat, cx)
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
        pane.open(TabItem::Spec("docs/spec.md".into()), cx);
        pane.activate(2, cx);
    });
    let spec = TabItem::Spec("docs/spec.md".into());
    click(cx, "pane-0:tab-2:close-button");
    pane.read_with(cx, |pane, _| {
        assert_eq!(pane.tabs(), &[TabItem::Chat, fx1(), spec.clone()]);
        assert_eq!(pane.active_item(), Some(&spec));
    });
    // Closing a tab left of the active one keeps the same tab active.
    pane.update(cx, |pane, cx| pane.close(1, cx));
    pane.read_with(cx, |pane, _| {
        assert_eq!(pane.tabs(), &[TabItem::Chat, spec.clone()]);
        assert_eq!(pane.active_item(), Some(&spec));
    });
}

#[gpui::test]
fn chat_tab_has_no_close_button_and_keeps_its_slot(cx: &mut TestAppContext) {
    let (group, _chat, cx) = open_center_with_chat(cx, vec![fx1()]);
    let pane = first_pane(&group, cx);
    redraw(cx);
    assert!(cx.debug_bounds("pane-0:tab-0:close-button").is_none());
    let slot = cx
        .debug_bounds("pane-0:tab-0:close")
        .expect("the chat tab's slot");
    assert_eq!(slot.size.width, TAB_CLOSE_SLOT);
    pane.update(cx, |pane, cx| pane.activate(1, cx));
    redraw(cx);
    assert!(cx.debug_bounds("pane-0:tab-0:close-button").is_none());
    assert!(cx.debug_bounds("pane-0:tab-1:close-button").is_some());
    // Closing the chat's tab in code is refused too.
    pane.update(cx, |pane, cx| pane.close(0, cx));
    pane.read_with(cx, |pane, _| {
        assert_eq!(pane.tabs(), &[TabItem::Chat, fx1()])
    });
}

/// Whether some pane in `group` has the chat's tab.
fn chat_is_open(group: &Entity<PaneGroup>, cx: &VisualTestContext) -> bool {
    group.read_with(cx, |group, cx| {
        group
            .panes()
            .iter()
            .any(|pane| pane.read(cx).tabs().contains(&TabItem::Chat))
    })
}

#[gpui::test]
fn chat_stays_reachable_through_any_closes(cx: &mut TestAppContext) {
    let (group, chat, cx) = open_center_with_chat(cx, Vec::new());
    let weak = chat.downgrade();
    drop(chat);
    // A fixed linear congruential sequence, so a failure replays.
    let mut seed: u64 = 0x5eed;
    let mut next = |bound: usize| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(seed >> 33).unwrap() % bound
    };
    let (mut splits, mut pane_closes) = (0, 0);
    for step in 0..120 {
        let panes = group.read_with(cx, |group, _| group.panes());
        let pane = panes[next(panes.len())].clone();
        match next(8) {
            0 => {
                let item = TabItem::Spec(format!("docs/{step}.md"));
                pane.update(cx, |pane, cx| pane.open(item, cx));
            }
            1 => {
                let item = TabItem::Spec(format!("docs/{step}.md"));
                group.update(cx, |group, cx| {
                    // A full center refuses; the sequence goes on.
                    if let Err(refused) = group.split(SplitDirection::Right, item, cx) {
                        assert_eq!(refused, SplitError::Full);
                    }
                });
            }
            2..=4 => {
                let len = pane.read_with(cx, |pane, _| pane.tabs().len());
                let index = next(len + 1);
                pane.update(cx, |pane, cx| pane.close(index, cx));
            }
            5 => {
                // The close button a person sees on the active tab, if any.
                let (number, len) =
                    pane.read_with(cx, |pane, _| (pane.number(), pane.tabs().len()));
                let index = next(len.max(1));
                pane.update(cx, |pane, cx| pane.activate(index, cx));
                let selector: &'static str =
                    Box::leak(format!("pane-{number}:tab-{index}:close-button").into_boxed_str());
                redraw(cx);
                if cx.debug_bounds(selector).is_some() {
                    click(cx, selector);
                }
            }
            6 => {
                let len = pane.read_with(cx, |pane, _| pane.tabs().len());
                let index = next(len.max(1));
                pane.update(cx, |pane, cx| pane.activate(index, cx));
            }
            _ => group.update(cx, |group, cx| group.open(TabItem::Plan, cx)),
        }
        let after = group.read_with(cx, |group, _| group.panes().len());
        match after.cmp(&panes.len()) {
            std::cmp::Ordering::Greater => splits += 1,
            std::cmp::Ordering::Less => pane_closes += 1,
            std::cmp::Ordering::Equal => {}
        }
        assert!(
            chat_is_open(&group, cx),
            "no pane has the chat after step {step}"
        );
        assert!(
            weak.upgrade().is_some(),
            "the chat was dropped at step {step}"
        );
    }
    // The sequence did split and close panes, not just tabs.
    assert!(
        splits > 3 && pane_closes > 3,
        "{splits} splits, {pane_closes} closes"
    );
    group.update(cx, |group, cx| group.open(TabItem::Chat, cx));
    redraw(cx);
    assert!(cx.debug_bounds("chat").is_some());
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
fn show_ledger_toggles_the_chats_flag(cx: &mut TestAppContext) {
    let (_group, chat, cx) = open_center_with_chat(cx, Vec::new());
    assert!(!chat.read_with(cx, |chat, _| chat.show_ledger()));
    click(cx, "pane-0:show-ledger");
    assert!(chat.read_with(cx, |chat, _| chat.show_ledger()));
    click(cx, "pane-0:show-ledger");
    assert!(!chat.read_with(cx, |chat, _| chat.show_ledger()));
}

#[gpui::test]
fn starting_tabs_open_behind_the_chat(cx: &mut TestAppContext) {
    let reviewer = TabItem::RoleFile("reviewer".into());
    let (group, _chat, cx) = open_center_with_chat(cx, vec![fx1(), reviewer.clone()]);
    let pane = first_pane(&group, cx);
    pane.read_with(cx, |pane, _| {
        assert_eq!(pane.tabs(), &[TabItem::Chat, fx1(), reviewer]);
        assert_eq!(pane.active_item(), Some(&TabItem::Chat));
    });
    assert!(cx.debug_bounds("pane-0:tab-2").is_some());
}

#[gpui::test]
fn closing_a_split_keeps_the_chat_in_a_remaining_pane(cx: &mut TestAppContext) {
    let (group, chat, cx) = open_center_with_chat(cx, Vec::new());
    let weak = chat.downgrade();
    drop(chat);
    group
        .update(cx, |group, cx| {
            group.split(SplitDirection::Right, fx1(), cx)
        })
        .expect("room to split");
    click(cx, "pane-1:tab-0:close-button");
    group.read_with(cx, |group, _| assert_eq!(group.panes().len(), 1));
    let pane = first_pane(&group, cx);
    pane.read_with(cx, |pane, _| {
        assert_eq!(pane.tabs(), &[TabItem::Chat]);
        assert_eq!(pane.chat().map(Entity::downgrade), Some(weak.clone()));
    });
    assert!(weak.upgrade().is_some(), "the group keeps the chat");
    assert!(cx.debug_bounds("chat").is_some());
}

#[gpui::test]
fn opening_the_chat_activates_its_one_tab(cx: &mut TestAppContext) {
    let (group, _chat, cx) = open_center_with_chat(cx, Vec::new());
    let second = group
        .update(cx, |group, cx| {
            group.split(SplitDirection::Right, fx1(), cx)
        })
        .expect("room to split");
    group.update(cx, |group, cx| group.open(TabItem::Chat, cx));
    second.read_with(cx, |pane, _| assert_eq!(pane.tabs(), &[fx1()]));
    let again = group
        .update(cx, |group, cx| {
            group.split(SplitDirection::Right, TabItem::Chat, cx)
        })
        .expect("the chat's pane");
    assert_eq!(again, first_pane(&group, cx));
    group.read_with(cx, |group, _| assert_eq!(group.panes().len(), 2));
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
        // The oldest tabs go first, but never the chat's.
        assert_eq!(pane.tabs().first(), Some(&TabItem::Chat));
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
