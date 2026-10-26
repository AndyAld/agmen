//! The mockup's installed agents and model lists (`INSTALLED` and `MODELS`
//! in `docs/mockups/scripts/console.py`), and the model tags on the ask
//! screen's messages (`ASK_CHAT`).

use super::Snapshot;
use super::models::{ChoiceScope, Effort, InstalledAgent, ModelChoice, ModelGroup};

/// `INSTALLED`: what `agmen agents scan` found on andy's machine. grok is
/// not on PATH, so no model group offers it (mockup fix 28).
pub(super) fn installed_agents() -> Vec<InstalledAgent> {
    let agent = |name: &str, version: Option<&str>, note: Option<&str>| InstalledAgent {
        name: name.into(),
        version: version.map(Into::into),
        note: note.map(Into::into),
    };
    vec![
        agent("claude", Some("2.1.310"), None),
        agent("codex", Some("0.164.0"), None),
        agent("opencode", Some("2.1.4"), None),
        agent("gemini", Some("0.46.0"), None),
        agent("agy", Some("1.1.7"), Some("update to 1.1.15+ for JSON")),
        agent("grok", None, Some("not installed")),
    ]
}

/// `MODELS`: claude's three models with its five effort levels; codex,
/// opencode and gemini take a model only; agy takes low to high (spec 4).
/// opencode and agy list their models with `opencode models` and
/// `agy models`, which the fixture does not run, so they offer only their
/// configured model, as codex and gemini do.
pub(super) fn model_groups() -> Vec<ModelGroup> {
    let group = |agent: &str, models: &[&str], efforts: &[Effort]| ModelGroup {
        agent: agent.into(),
        models: models.iter().map(|&m| m.into()).collect(),
        efforts: efforts.to_vec(),
    };
    let claude = [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];
    vec![
        group("claude", &["haiku", "sonnet", "opus"], &claude),
        group("codex", &[], &[]),
        group("opencode", &[], &[]),
        group("gemini", &[], &[]),
        group("agy", &[], &[Effort::Low, Effort::Medium, Effort::High]),
    ]
}

/// The model both `ASK_CHAT` messages carry (V67): andy asked on opus at
/// high effort for this message, and the listener answered on it.
pub(super) fn ask_model() -> ModelChoice {
    ModelChoice {
        agent: "claude".into(),
        model: Some("opus".into()),
        effort: Some(Effort::High),
        scope: ChoiceScope::ThisMessage,
    }
}

/// Tags every message from index `first` on with `model`.
pub(super) fn tag_from(snapshot: &mut Snapshot, first: usize, model: &ModelChoice) {
    let tags: Vec<_> = snapshot
        .messages
        .iter()
        .skip(first)
        .map(|m| ((m.stream.clone(), m.seq), model.clone()))
        .collect();
    snapshot.message_models.extend(tags);
}

#[cfg(test)]
mod tests {
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
                agent.is_some_and(|a| a.version.is_some()),
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
                to: vec![backend.snapshot().listener_id.clone()],
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
}
