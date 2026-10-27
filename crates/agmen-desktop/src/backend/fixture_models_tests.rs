use agmen_proto::Verb;
use futures::StreamExt as _;

use super::*;
use crate::backend::{Backend, Draft, FixtureBackend, Screen, Update};

fn backend(screen: Screen) -> FixtureBackend {
    FixtureBackend::for_screen(screen).unwrap()
}

#[test]
fn groups_follow_the_installed_agents() {
    let s = backend(Screen::ModelPicker).snapshot();
    let groups: Vec<&str> = s.models.iter().map(|g| g.agent.as_str()).collect();
    assert_eq!(groups, ["claude", "codex", "opencode", "gemini", "agy"]);
    for group in &s.models {
        let agent = s.installed_agents.iter().find(|a| a.name == group.agent);
        assert!(
            agent.is_some_and(|a| a.version().is_some()),
            "{}",
            group.agent
        );
    }
    let codex = s.models.iter().find(|g| g.agent == "codex").unwrap();
    assert!(codex.efforts.is_empty());
}

#[test]
fn both_ask_messages_carry_the_opus_tag() {
    let s = backend(Screen::Ask).snapshot();
    let tagged: Vec<&str> = s
        .messages
        .iter()
        .filter(|m| s.message_models.contains_key(&(m.stream.clone(), m.seq)))
        .map(|m| m.from_handle.as_str())
        .collect();
    assert_eq!(tagged, ["andy", "andy/listen"]);
    assert!(
        s.message_models
            .values()
            .all(|m| m.label() == "opus · high")
    );
    assert!(backend(Screen::Chat).snapshot().message_models.is_empty());
}

#[test]
fn a_reply_to_an_alert_goes_out_as_an_answer() {
    let backend = backend(Screen::Chat);
    let s = backend.snapshot();
    let alert = s
        .messages
        .iter()
        .find(|m| m.client.payload.verb() == Some(Verb::Alert))
        .unwrap();
    backend
        .send(Draft {
            to: vec![alert.from.clone()],
            body: "on it".into(),
            reply_to: Some(alert.client.id.clone()),
            model: None,
        })
        .unwrap();
    let sent = backend.snapshot().messages.pop().unwrap();
    assert_eq!(sent.client.payload.verb(), Some(Verb::Answer));
    assert_eq!(sent.client.reply_to.as_ref(), Some(&alert.client.id));
}

#[test]
fn a_draft_with_a_model_is_tagged_with_it() {
    let backend = backend(Screen::Chat);
    let mut updates = backend.subscribe();
    backend
        .send(Draft {
            to: vec![backend.snapshot().listener_id().cloned().unwrap()],
            body: "why?".into(),
            reply_to: None,
            model: Some(ask_model()),
        })
        .unwrap();
    let Some(Update::Snapshot(s)) = futures::executor::block_on(updates.next()) else {
        panic!("the tag comes with a snapshot");
    };
    let sent = s.messages.last().unwrap();
    assert_eq!(sent.client.payload.verb(), Some(Verb::Fyi));
    assert_eq!(
        s.message_models.get(&(sent.stream.clone(), sent.seq)),
        Some(&ask_model())
    );
}
