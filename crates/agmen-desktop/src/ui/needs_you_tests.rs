use agmen_proto::StampedEnvelope;
use jiff::tz::{TimeZone, offset};

use super::*;
use crate::alerts::{AlertPrefs, QuietHours};
use crate::backend::{Backend as _, FixtureBackend, Screen, Snapshot};
use crate::ui::format::clock;

/// Mountain daylight time, as the mockup's clock shows.
const MDT: TimeZone = TimeZone::fixed(offset(-6));

fn mdt(at: Timestamp) -> String {
    clock(at, &MDT)
}

fn screen(screen: Screen) -> Snapshot {
    FixtureBackend::for_screen(screen).unwrap().snapshot()
}

/// Feeds every message of `snapshot` as `arrival`, returning each one's
/// signals.
fn feed(
    snapshot: &Snapshot,
    alerts: &mut AlertState,
    needs: &mut NeedsYou,
    arrival: Arrival,
) -> Vec<Signals> {
    let me = Me::from(snapshot);
    snapshot
        .messages
        .iter()
        .map(|env| needs.on_message(alerts, env, &me, &snapshot.messages, arrival))
        .collect()
}

/// The needs-you screen with its last message, the urgent alert, held back.
fn before_the_alert() -> (Snapshot, StampedEnvelope) {
    let mut snapshot = screen(Screen::NeedsYou);
    let alert = snapshot.messages.pop().unwrap();
    (snapshot, alert)
}

const CI_TEXT: &str = "CI on main is red after #10 merged: it_tabs, 3 failed. The release tag is blocked. Revert #10, or hotfix on a branch?";

#[test]
fn history_never_chimes_notifies_or_toasts() {
    for s in [Screen::Chat, Screen::NeedsYou, Screen::NeedsYouAnswered] {
        let snapshot = screen(s);
        let (mut alerts, mut needs) = (AlertState::default(), NeedsYou::default());
        let signals = feed(&snapshot, &mut alerts, &mut needs, Arrival::History);
        assert!(signals.iter().all(|s| *s == Signals::default()), "{s:?}");
        assert!(needs.toast().is_none(), "{s:?}");
    }
}

#[test]
fn history_still_shows_the_needs_you_banner_without_chime_played() {
    let snapshot = screen(Screen::NeedsYou);
    let (mut alerts, mut needs) = (AlertState::default(), NeedsYou::default());
    feed(&snapshot, &mut alerts, &mut needs, Arrival::History);
    let alert = snapshot.messages.last().unwrap();
    assert_eq!(
        needs.banner(&alerts, &snapshot.messages, mdt),
        Some(Banner::NeedsYou {
            alert: alert.client.id.clone(),
            message: format!("adam/coord: {CI_TEXT}"),
            chimed: false,
        })
    );
}

#[test]
fn a_live_urgent_alert_chimes_notifies_and_toasts() {
    let (mut snapshot, alert) = before_the_alert();
    let (mut alerts, mut needs) = (AlertState::default(), NeedsYou::default());
    feed(&snapshot, &mut alerts, &mut needs, Arrival::History);
    snapshot.messages.push(alert.clone());
    let me = Me::from(&snapshot);
    let signals = needs.on_message(&mut alerts, &alert, &me, &snapshot.messages, Arrival::Live);
    let title = "adam/coord needs you".to_owned();
    assert_eq!(
        signals,
        Signals {
            chime: true,
            notice: Some(Notice {
                tag: format!("alert-{}", alert.client.id),
                title: title.clone(),
                body: CI_TEXT.to_owned(),
            }),
            toast: true,
        }
    );
    assert_eq!(
        needs.toast(),
        Some(&Toast {
            alert: alert.client.id.clone(),
            title,
            body: CI_TEXT.to_owned(),
        })
    );
    assert!(matches!(
        needs.banner(&alerts, &snapshot.messages, mdt),
        Some(Banner::NeedsYou { chimed: true, .. })
    ));
}

#[test]
fn a_live_normal_alert_only_notifies() {
    // C101: andy/a1's normal ALERT reaches andy, its owner.
    let snapshot = screen(Screen::Chat);
    let alert = snapshot.messages[1].clone();
    let (mut alerts, mut needs) = (AlertState::default(), NeedsYou::default());
    let me = Me::from(&snapshot);
    let signals = needs.on_message(&mut alerts, &alert, &me, &snapshot.messages, Arrival::Live);
    assert!(!signals.chime && !signals.toast);
    let notice = signals.notice.unwrap();
    assert_eq!(notice.title, "Alert from andy/a1");
    assert!(notice.body.starts_with("main does not compile: 4d9fdc8"));
    assert!(needs.toast().is_none());
    assert_eq!(needs.banner(&alerts, &snapshot.messages, mdt), None);
}

#[test]
fn quiet_hours_highlight_without_chime_or_toast() {
    let (mut snapshot, mut alert) = before_the_alert();
    // Someone else's agent, not a coordinator: quiet hours hush it.
    alert.from_kind = IdentityKind::Agent;
    let mut alerts = AlertState::default();
    alerts.prefs = AlertPrefs {
        quiet_hours: QuietHours::On,
        ..AlertPrefs::default()
    };
    let mut needs = NeedsYou::default();
    feed(&snapshot, &mut alerts, &mut needs, Arrival::History);
    snapshot.messages.push(alert.clone());
    let me = Me::from(&snapshot);
    let signals = needs.on_message(&mut alerts, &alert, &me, &snapshot.messages, Arrival::Live);
    assert_eq!(signals, Signals::default());
    assert!(needs.toast().is_none());
    assert!(matches!(
        needs.banner(&alerts, &snapshot.messages, mdt),
        Some(Banner::NeedsYou { chimed: false, .. })
    ));
}

#[test]
fn an_answer_from_someone_else_shows_the_answered_banner() {
    // Frame 6122 (node 6261).
    let snapshot = screen(Screen::NeedsYouAnswered);
    let (mut alerts, mut needs) = (AlertState::default(), NeedsYou::default());
    feed(&snapshot, &mut alerts, &mut needs, Arrival::History);
    assert_eq!(
        needs.banner(&alerts, &snapshot.messages, mdt),
        Some(Banner::Answered {
            message:
                "adam (coordinator) answered adam/coord's alert at 15:59. Cleared for everyone."
                    .to_owned()
        })
    );
}

#[test]
fn my_answer_reads_you() {
    let mut snapshot = screen(Screen::NeedsYouAnswered);
    let me = Me::from(&snapshot);
    let answer = snapshot.messages.last_mut().unwrap();
    answer.from = me.id.clone();
    answer.from_handle = me.handle.clone();
    answer.from_kind = IdentityKind::Human;
    let (mut alerts, mut needs) = (AlertState::default(), NeedsYou::default());
    feed(&snapshot, &mut alerts, &mut needs, Arrival::History);
    assert_eq!(
        needs.banner(&alerts, &snapshot.messages, mdt),
        Some(Banner::Answered {
            message: "You answered adam/coord's alert at 15:59. Cleared for everyone.".to_owned()
        })
    );
}

#[test]
fn a_new_alert_replaces_the_answered_banner() {
    let mut snapshot = screen(Screen::NeedsYouAnswered);
    let (mut alerts, mut needs) = (AlertState::default(), NeedsYou::default());
    feed(&snapshot, &mut alerts, &mut needs, Arrival::History);
    let mut again = snapshot.messages[snapshot.messages.len() - 2].clone();
    again.client.id = "01JE0000000000000000000NEW".parse().unwrap();
    snapshot.messages.push(again.clone());
    let me = Me::from(&snapshot);
    needs.on_message(&mut alerts, &again, &me, &snapshot.messages, Arrival::Live);
    assert!(matches!(
        needs.banner(&alerts, &snapshot.messages, mdt),
        Some(Banner::NeedsYou { alert, .. }) if alert == again.client.id
    ));
    // Snoozing the new one leaves no banner, not the stale answered one.
    alerts.snooze(&again.client.id);
    needs.forget_closed(&alerts);
    assert_eq!(needs.banner(&alerts, &snapshot.messages, mdt), None);
}

#[test]
fn closing_the_toasts_alert_closes_the_toast() {
    let (mut snapshot, alert) = before_the_alert();
    let (mut alerts, mut needs) = (AlertState::default(), NeedsYou::default());
    feed(&snapshot, &mut alerts, &mut needs, Arrival::History);
    snapshot.messages.push(alert.clone());
    let me = Me::from(&snapshot);
    needs.on_message(&mut alerts, &alert, &me, &snapshot.messages, Arrival::Live);
    assert!(needs.toast().is_some());
    alerts.snooze(&alert.client.id);
    needs.forget_closed(&alerts);
    assert!(needs.toast().is_none());
}

#[test]
fn show_arrival_draws_the_screens_toast_and_chime_chip() {
    let snapshot = screen(Screen::NeedsYou);
    let (mut alerts, mut needs) = (AlertState::default(), NeedsYou::default());
    feed(&snapshot, &mut alerts, &mut needs, Arrival::History);
    needs.show_arrival(&alerts, &snapshot.messages);
    let toast = needs.toast().unwrap();
    assert_eq!(toast.title, "adam/coord needs you");
    assert!(matches!(
        needs.banner(&alerts, &snapshot.messages, mdt),
        Some(Banner::NeedsYou { chimed: true, .. })
    ));
    // Nothing needs me on the answered screen, so nothing shows.
    let answered = screen(Screen::NeedsYouAnswered);
    let (mut alerts, mut needs) = (AlertState::default(), NeedsYou::default());
    feed(&answered, &mut alerts, &mut needs, Arrival::History);
    needs.show_arrival(&alerts, &answered.messages);
    assert!(needs.toast().is_none());
}

#[test]
fn plain_text_drops_leading_mentions() {
    assert_eq!(plain_text("@andy CI on main is red"), "CI on main is red");
    assert_eq!(
        plain_text("@andy @sean/a1  hold\n pushes, @adam"),
        "hold pushes, @adam"
    );
    assert_eq!(plain_text("no mention"), "no mention");
    assert_eq!(plain_text("@andy"), "");
}

#[test]
fn long_bodies_are_clipped_in_the_notice() {
    let (mut snapshot, mut alert) = before_the_alert();
    alert.client.body = "x".repeat(10_000).try_into().unwrap();
    let (mut alerts, mut needs) = (AlertState::default(), NeedsYou::default());
    feed(&snapshot, &mut alerts, &mut needs, Arrival::History);
    snapshot.messages.push(alert.clone());
    let me = Me::from(&snapshot);
    let signals = needs.on_message(&mut alerts, &alert, &me, &snapshot.messages, Arrival::Live);
    let body = signals.notice.unwrap().body;
    assert_eq!(body.chars().count(), MAX_NOTICE_CHARS);
    assert!(body.ends_with('…'));
}

#[test]
fn looks_mark_the_alert_needing_me_then_snoozed() {
    let snapshot = screen(Screen::NeedsYou);
    let (mut alerts, mut needs) = (AlertState::default(), NeedsYou::default());
    feed(&snapshot, &mut alerts, &mut needs, Arrival::History);
    let last = snapshot.messages.len() - 1;
    assert_eq!(
        alert_looks(&alerts, &snapshot.messages),
        BTreeMap::from([(last, AlertLook::NeedsMe)])
    );
    alerts.snooze(&snapshot.messages[last].client.id);
    assert_eq!(
        alert_looks(&alerts, &snapshot.messages),
        BTreeMap::from([(last, AlertLook::Snoozed)])
    );
    // The answered screen has no alert that needs me.
    let answered = screen(Screen::NeedsYouAnswered);
    let (mut alerts, mut needs) = (AlertState::default(), NeedsYou::default());
    feed(&answered, &mut alerts, &mut needs, Arrival::History);
    assert!(alert_looks(&alerts, &answered.messages).is_empty());
}

#[test]
fn another_senders_message_with_the_alerts_id_is_not_marked() {
    let mut snapshot = screen(Screen::NeedsYou);
    let last = snapshot.messages.len() - 1;
    let mut spoof = snapshot.messages[last].clone();
    spoof.from_handle = "sean/a1".into();
    spoof.seq = agmen_proto::Seq(spoof.seq.0 + 1);
    snapshot.messages.push(spoof);
    let (mut alerts, mut needs) = (AlertState::default(), NeedsYou::default());
    feed(&snapshot, &mut alerts, &mut needs, Arrival::History);
    assert_eq!(
        alert_looks(&alerts, &snapshot.messages),
        BTreeMap::from([(last, AlertLook::NeedsMe)])
    );
}

#[test]
fn snooze_shows_once_after_the_senders_actions() {
    // The fixture ALERT offers Snooze itself; the app's Snooze is the one
    // drawn, last.
    let actions: Vec<String> = ["Hotfix: spawn an engineer", "Revert #10", "Snooze 15 min"]
        .map(str::to_owned)
        .into();
    assert_eq!(
        alert_buttons(&actions),
        [
            AlertButton::Reply,
            AlertButton::Answer("Hotfix: spawn an engineer".into()),
            AlertButton::Answer("Revert #10".into()),
            AlertButton::Snooze,
        ]
    );
    let shouty = vec![" snooze 15 MIN ".to_owned()];
    assert_eq!(
        alert_buttons(&shouty),
        [AlertButton::Reply, AlertButton::Snooze]
    );
    assert_eq!(
        alert_buttons(&[]),
        [AlertButton::Reply, AlertButton::Snooze]
    );
}

#[test]
fn sender_actions_are_bounded() {
    let many: Vec<String> = (0..50).map(|i| format!("Option {i}")).collect();
    let buttons = alert_buttons(&many);
    assert_eq!(buttons.len(), MAX_ALERT_ACTIONS + 2);
    assert_eq!(buttons.last(), Some(&AlertButton::Snooze));
}
