use agmen_proto::StampedEnvelope;
use serde_json::{Value, json};

use super::*;
use crate::backend::{Backend as _, FixtureBackend, Screen, Snapshot};

fn screen(screen: Screen) -> Snapshot {
    FixtureBackend::for_screen(screen).unwrap().snapshot()
}

fn andy() -> Me {
    Me::from(&screen(Screen::Chat))
}

/// A stamped message from literal wire JSON: `fields` replace the defaults.
fn stamped(fields: &Value) -> StampedEnvelope {
    let mut value = json!({
        "v": 1,
        "id": "01JD0000000000000000000001",
        "body": "test",
        "sent_at": "2026-10-25T22:00:00Z",
        "verb": "FYI",
        "data": {},
        "stream": "feed",
        "seq": 1,
        "from": "seana100",
        "from_handle": "sean/a1",
        "from_kind": "agent",
        "hub_at": "2026-10-25T22:00:00Z",
    });
    for (key, field) in fields.as_object().unwrap() {
        value[key] = field.clone();
    }
    serde_json::from_value(value).unwrap()
}

fn alert(id: &str, from: &str, handle: &str, kind: &str, data: &Value) -> StampedEnvelope {
    stamped(&json!({
        "id": id, "from": from, "from_handle": handle, "from_kind": kind,
        "verb": "ALERT", "data": data,
    }))
}

fn urgent(needs: &str) -> Value {
    json!({"need": "decide", "urgency": "urgent", "needs": needs, "actions": []})
}

fn answer(id: &str, reply_to: &str, from: &str, handle: &str, kind: &str) -> StampedEnvelope {
    stamped(&json!({
        "id": id, "reply_to": reply_to, "from": from, "from_handle": handle,
        "from_kind": kind, "verb": "ANSWER",
    }))
}

fn replay(snapshot: &Snapshot, prefs: AlertPrefs) -> (AlertState, Vec<AlertEffect>) {
    let me = Me::from(snapshot);
    let mut state = AlertState {
        prefs,
        ..AlertState::default()
    };
    let effects = snapshot
        .messages
        .iter()
        .map(|m| state.on_message(m, &me))
        .collect();
    (state, effects)
}

const ALERT_1: &str = "01JD0000000000000000000010";
const ALERT_2: &str = "01JD0000000000000000000011";
const ALERT_3: &str = "01JD0000000000000000000012";
const ANSWER_1: &str = "01JD0000000000000000000020";
const ANSWER_2: &str = "01JD0000000000000000000021";

fn mentioning(mut env: StampedEnvelope, who: &IdentityId) -> StampedEnvelope {
    env.mentioned.push(who.clone());
    env
}

fn quiet() -> AlertState {
    AlertState {
        prefs: AlertPrefs {
            quiet_hours: QuietHours::On,
            ..AlertPrefs::default()
        },
        ..AlertState::default()
    }
}

#[test]
fn urgent_alert_needs_me() {
    let s = screen(Screen::NeedsYou);
    let (state, effects) = replay(&s, AlertPrefs::default());
    let alert = s.messages.last().unwrap();
    assert_eq!(alert.from_handle, "adam/coord");
    assert_eq!(state.needing_me(), std::slice::from_ref(&alert.client.id));
    assert_eq!(effects.last(), Some(&AlertEffect::HighlightChimeNotify));
    // Before it, only andy/a1's normal alert (my own agent) makes an effect.
    let earlier: Vec<AlertEffect> = effects[..effects.len() - 1]
        .iter()
        .copied()
        .filter(|e| *e != AlertEffect::None)
        .collect();
    assert_eq!(earlier, [AlertEffect::Notify]);
}

#[test]
fn coordinator_alert_for_someone_else_is_not_mine() {
    let me = andy();
    let mut state = AlertState::default();
    // adam/coord's urgent alert to sean: not mine, does not mention me.
    let to_sean = mentioning(
        alert(
            ALERT_1,
            "adamcoor",
            "adam/coord",
            "coordinator",
            &urgent("anyone"),
        ),
        &"sean0000".parse().unwrap(),
    );
    assert_eq!(state.on_message(&to_sean, &me), AlertEffect::None);
    assert!(state.needing_me().is_empty());
    assert_eq!(quiet().on_message(&to_sean, &me), AlertEffect::None);
}

#[test]
fn normal_alert_notifies_without_chime() {
    let me = andy();
    let mut state = AlertState::default();
    let normal = json!({"need": "look", "urgency": "normal", "needs": "anyone", "actions": []});
    // Not about me: nothing.
    let other = alert(ALERT_1, "seana100", "sean/a1", "agent", &normal);
    assert_eq!(state.on_message(&other, &me), AlertEffect::None);
    // From my own agent: a plain notification.
    let mine = alert(ALERT_2, "andyfx10", "andy/fx1", "agent", &normal);
    assert_eq!(state.on_message(&mine, &me), AlertEffect::Notify);
    // Mentioning me: a plain notification, no highlight and no chime.
    let mentions = mentioning(
        alert(ALERT_3, "seana100", "sean/a1", "agent", &normal),
        &me.id,
    );
    assert_eq!(state.on_message(&mentions, &me), AlertEffect::Notify);
    assert!(state.needing_me().is_empty());
    // My own alert never notifies me.
    let own = alert(
        "01JD0000000000000000000013",
        me.id.as_str(),
        "andy",
        "human",
        &normal,
    );
    assert_eq!(state.on_message(&own, &me), AlertEffect::None);
}

#[test]
fn anyone_alert_clears_on_first_answer() {
    let answered = screen(Screen::NeedsYouAnswered);
    let (state, effects) = replay(&answered, AlertPrefs::default());
    let last = answered.messages.last().unwrap();
    assert_eq!(last.from_handle, "adam");
    assert!(state.needing_me().is_empty(), "{:?}", state.needing_me());
    assert_eq!(effects.last(), Some(&AlertEffect::None));

    // Anyone's answer clears it, not only the sender's or a coordinator's.
    let me = andy();
    let mut state = AlertState::default();
    let alert = mentioning(
        alert(
            ALERT_1,
            "adamcoor",
            "adam/coord",
            "coordinator",
            &urgent("anyone"),
        ),
        &me.id,
    );
    state.on_message(&alert, &me);
    assert_eq!(state.needing_me().len(), 1);
    let reply = answer(ANSWER_1, ALERT_1, "seana100", "sean/a1", "agent");
    assert_eq!(state.on_message(&reply, &me), AlertEffect::None);
    assert!(state.needing_me().is_empty());
}

#[test]
fn owner_alert_stays_for_owner_after_coordinator_reply() {
    let me = andy();
    let mut state = AlertState::default();
    let budget = alert(ALERT_1, "andyfx10", "andy/fx1", "agent", &urgent("owner"));
    assert_eq!(
        state.on_message(&budget, &me),
        AlertEffect::HighlightChimeNotify
    );
    let coordinator = answer(ANSWER_1, ALERT_1, "adamcoor", "adam/coord", "coordinator");
    state.on_message(&coordinator, &me);
    assert_eq!(
        state.needing_me().len(),
        1,
        "a coordinator's reply keeps it"
    );
    let mine = answer(ANSWER_2, ALERT_1, me.id.as_str(), "andy", "human");
    state.on_message(&mine, &me);
    assert!(
        state.needing_me().is_empty(),
        "the owner's answer clears it"
    );

    // Someone else's agent's owner alert clears for me at the first answer.
    let mut state = AlertState::default();
    let theirs = mentioning(
        alert(ALERT_2, "seana100", "sean/a1", "agent", &urgent("owner")),
        &me.id,
    );
    state.on_message(&theirs, &me);
    assert_eq!(state.needing_me().len(), 1);
    state.on_message(
        &answer(ANSWER_1, ALERT_2, "adamcoor", "adam/coord", "coordinator"),
        &me,
    );
    assert!(state.needing_me().is_empty());
}

#[test]
fn quiet_hours_mute_others_but_not_my_agents() {
    let me = andy();
    let mut state = quiet();
    let others = mentioning(
        alert(ALERT_1, "seana100", "sean/a1", "agent", &urgent("anyone")),
        &me.id,
    );
    assert_eq!(state.on_message(&others, &me), AlertEffect::Highlight);
    let mine = alert(ALERT_2, "andyfx10", "andy/fx1", "agent", &urgent("anyone"));
    assert_eq!(
        state.on_message(&mine, &me),
        AlertEffect::HighlightChimeNotify
    );
    let coordinator = mentioning(
        alert(
            ALERT_3,
            "adamcoor",
            "adam/coord",
            "coordinator",
            &urgent("anyone"),
        ),
        &me.id,
    );
    assert_eq!(
        state.on_message(&coordinator, &me),
        AlertEffect::HighlightChimeNotify
    );
    // Muted to a highlight, but each one still needs me.
    assert_eq!(state.needing_me().len(), 3);
    // The fixture's coordinator alert sounds through quiet hours too.
    let (_, effects) = replay(
        &screen(Screen::NeedsYou),
        AlertPrefs {
            quiet_hours: QuietHours::On,
            ..AlertPrefs::default()
        },
    );
    assert_eq!(effects.last(), Some(&AlertEffect::HighlightChimeNotify));
}

#[test]
fn muted_agent_alert_arrives_normal() {
    let me = andy();
    let fx1: IdentityId = "andyfx10".parse().unwrap();
    let mut state = AlertState {
        prefs: AlertPrefs {
            muted: BTreeSet::from([fx1.clone()]),
            ..AlertPrefs::default()
        },
        ..AlertState::default()
    };
    // A muted urgent alert from my agent is a normal one: a notification only.
    let mine = alert(
        ALERT_1,
        fx1.as_str(),
        "andy/fx1",
        "agent",
        &urgent("anyone"),
    );
    assert_eq!(state.on_message(&mine, &me), AlertEffect::Notify);
    assert!(state.needing_me().is_empty());
    // Others still get through.
    let other = alert(ALERT_2, "andyt7k0", "andy/t7k", "agent", &urgent("anyone"));
    assert_eq!(
        state.on_message(&other, &me),
        AlertEffect::HighlightChimeNotify
    );
}

#[test]
fn chat_tab_badge_counts() {
    let me = andy();
    let (mut state, _) = replay(&screen(Screen::NeedsYou), AlertPrefs::default());
    assert_eq!(state.badge(), 1);
    let second = alert(ALERT_1, "andyfx10", "andy/fx1", "agent", &urgent("anyone"));
    assert_eq!(
        state.on_message(&second, &me),
        AlertEffect::HighlightChimeNotify
    );
    assert_eq!(state.badge(), 2);
    let answered = screen(Screen::NeedsYouAnswered);
    state.on_message(answered.messages.last().unwrap(), &me);
    assert_eq!(state.badge(), 1);
    state.on_message(
        &answer(ANSWER_1, ALERT_1, "seana100", "sean/a1", "agent"),
        &me,
    );
    assert_eq!(state.badge(), 0);
    // The badge agrees with the status bar's needs-you count on every screen.
    for s in Screen::ALL {
        let snapshot = screen(s);
        let (state, _) = replay(&snapshot, AlertPrefs::default());
        assert_eq!(state.badge(), snapshot.status.needs_you as usize, "{s:?}");
    }
}

#[test]
fn redelivered_alert_neither_chimes_nor_reopens() {
    let me = andy();
    let mut state = AlertState::default();
    let urgent_alert = alert(ALERT_1, "andyfx10", "andy/fx1", "agent", &urgent("anyone"));
    assert_eq!(
        state.on_message(&urgent_alert, &me),
        AlertEffect::HighlightChimeNotify
    );
    assert_eq!(state.on_message(&urgent_alert, &me), AlertEffect::None);
    assert_eq!(state.badge(), 1);
    state.on_message(
        &answer(ANSWER_1, ALERT_1, "seana100", "sean/a1", "agent"),
        &me,
    );
    assert_eq!(state.on_message(&urgent_alert, &me), AlertEffect::None);
    assert_eq!(state.badge(), 0, "a late copy does not reopen it");
    // A normal alert notifies once.
    let normal = json!({"need": "look", "urgency": "normal", "needs": "anyone", "actions": []});
    let note = alert(ALERT_2, "andyfx10", "andy/fx1", "agent", &normal);
    assert_eq!(state.on_message(&note, &me), AlertEffect::Notify);
    assert_eq!(state.on_message(&note, &me), AlertEffect::None);
}

#[test]
fn remembered_alerts_are_bounded() {
    let me = andy();
    let mut state = AlertState::default();
    let normal = json!({"need": "look", "urgency": "normal", "needs": "anyone", "actions": []});
    for i in 0..MAX_REMEMBERED_ALERTS + 5 {
        let id = format!("01JE{i:022}");
        state.on_message(&alert(&id, "seana100", "sean/a1", "agent", &normal), &me);
    }
    assert_eq!(state.remembered_len(), MAX_REMEMBERED_ALERTS);
}

#[test]
fn snooze_and_clear_end_the_need() {
    let me = andy();
    let mut state = AlertState::default();
    let budget = alert(ALERT_1, "andyfx10", "andy/fx1", "agent", &urgent("owner"));
    let other = alert(ALERT_2, "andyt7k0", "andy/t7k", "agent", &urgent("anyone"));
    state.on_message(&budget, &me);
    state.on_message(&other, &me);
    let id1: MessageId = ALERT_1.parse().unwrap();
    let id2: MessageId = ALERT_2.parse().unwrap();
    assert!(state.snooze(&id1));
    assert!(!state.snooze(&id1));
    assert!(state.clear(&id2));
    assert_eq!(state.badge(), 0);
    // Neither comes back on redelivery.
    assert_eq!(state.on_message(&budget, &me), AlertEffect::None);
    assert_eq!(state.badge(), 0);
}

#[test]
fn alert_count_chat_is_one() {
    let s = screen(Screen::Chat);
    assert_eq!(alert_count(&s.messages, &Me::from(&s)), 1);
}

#[test]
fn alert_count_needs_you_is_two() {
    let s = screen(Screen::NeedsYou);
    assert_eq!(alert_count(&s.messages, &Me::from(&s)), 2);
}

#[test]
fn alert_count_answered_is_one() {
    let s = screen(Screen::NeedsYouAnswered);
    assert_eq!(alert_count(&s.messages, &Me::from(&s)), 1);
}

#[test]
fn alert_count_matches_status_summary() {
    for s in Screen::ALL {
        let snapshot = screen(s);
        assert_eq!(
            alert_count(&snapshot.messages, &Me::from(&snapshot)),
            snapshot.status.alerts,
            "{s:?}"
        );
    }
}

#[test]
fn alert_count_keeps_owner_alerts_open_for_the_owner() {
    let me = andy();
    let budget = alert(ALERT_1, "andyfx10", "andy/fx1", "agent", &urgent("owner"));
    let coordinator = answer(ANSWER_1, ALERT_1, "adamcoor", "adam/coord", "coordinator");
    assert_eq!(alert_count(&[budget.clone(), coordinator.clone()], &me), 1);
    let mine = answer(ANSWER_2, ALERT_1, me.id.as_str(), "andy", "human");
    assert_eq!(
        alert_count(&[budget.clone(), coordinator, mine, budget], &me),
        0,
        "a copy after the answer does not reopen it"
    );
}
