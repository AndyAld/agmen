//! The mockup's model lists (`MODELS` in
//! `docs/mockups/scripts/console.py`; `INSTALLED` is
//! `fixture_take_over::installed_agents`), and the model tags on the ask
//! screen's messages (`ASK_CHAT`).

use super::Snapshot;
use super::models::{ChoiceScope, Effort, ModelChoice, ModelGroup};

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
/// high effort, and the listener answered on it. He picked it until he
/// changes it, so the ask frame's chip still shows it (fix 31, node 3982).
pub(super) fn ask_model() -> ModelChoice {
    ModelChoice {
        agent: "claude".into(),
        model: Some("opus".into()),
        effort: Some(Effort::High),
        scope: ChoiceScope::UntilChanged,
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
#[path = "fixture_models_tests.rs"]
mod tests;
