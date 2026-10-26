//! [`FixtureBackend`]: the mockup's data behind the [`Backend`] trait, one
//! state per mockup screen.

use std::cell::RefCell;
use std::collections::BTreeMap;

use agmen_proto::MAX_BODY_BYTES;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use serde_json::json;

use super::fixture_data::{
    Msg, alert_answer, ask_messages, chat_messages, fx1, fx1_session, git_facts, id, listener,
    plan, plan_messages, plan_roster, role_files, roster, settings_card, settings_messages,
    spawn_exchange, stamp, urgent_alert, worktrees,
};
use super::fixture_models::{ask_model, model_groups, tag_from};
use super::{
    Action, Backend, BackendError, ChangeState, Draft, RecentAction, RecentKind, Screen, Snapshot,
    StatusSummary, Update,
};

/// Serves one mockup screen's state, and keeps what is sent and done in
/// memory so the views can be driven without agmend.
#[derive(Debug)]
pub(crate) struct FixtureBackend {
    state: RefCell<Snapshot>,
    subscribers: RefCell<Vec<UnboundedSender<Update>>>,
}

/// The state the `desktop-chat` render shows.
#[cfg(test)]
#[must_use]
pub(crate) fn chat_snapshot() -> Snapshot {
    FixtureBackend::for_screen(Screen::Chat)
        .map(|backend| backend.snapshot())
        .unwrap_or_else(|err| panic!("{err}"))
}

fn recent(time: &str, text: &str, kind: RecentKind) -> RecentAction {
    RecentAction {
        time: time.into(),
        text: text.into(),
        kind,
    }
}

fn base(project: &str) -> Result<Snapshot, BackendError> {
    let fx1 = fx1()?;
    let roster = roster()?;
    let roles = role_files(&roster);
    let listener = listener();
    let spend = listener.workers_today_usd + listener.listener_today_usd;
    Ok(Snapshot {
        project: project.into(),
        me: "andy".into(),
        me_id: id("andy")?,
        listener_id: id("andy/listen")?,
        roster,
        listener,
        agents: BTreeMap::from([(fx1.id.clone(), fx1)]),
        status: StatusSummary {
            online: 9,
            // Mockup fix 29: five worktrees, as the Git panel lists.
            worktrees: 5,
            spend_today_usd: spend,
        },
        messages: Vec::new(),
        changes: BTreeMap::new(),
        plan: None,
        git: git_facts(),
        sessions: BTreeMap::from([(id("andy/fx1")?, fx1_session())]),
        roles,
        worktrees: worktrees()?,
        take_overs: BTreeMap::new(),
        installed_agents: super::fixture_take_over::installed_agents(),
        agent_scans: 0,
        // Task 15: the model picker's models and message tags.
        models: model_groups(),
        message_models: BTreeMap::new(),
    })
}

fn with_messages(project: &str, msgs: Vec<Msg>) -> Result<Snapshot, BackendError> {
    let mut s = base(project)?;
    s.messages = stamp(msgs, 1)?;
    Ok(s)
}

impl FixtureBackend {
    /// The state `screen`'s render shows.
    ///
    /// # Errors
    ///
    /// When the built-in fixture data is not valid protocol data, which
    /// `fixture_messages_survive_the_wire_format` guards against.
    pub(crate) fn for_screen(screen: Screen) -> Result<Self, BackendError> {
        let snapshot = match screen {
            Screen::NeedsYou | Screen::NeedsYouAnswered => {
                let mut msgs = chat_messages();
                let alert = msgs.len();
                msgs.push(urgent_alert());
                // Task 13: the status bar counts these from the messages.
                if screen == Screen::NeedsYouAnswered {
                    msgs.push(alert_answer(alert));
                }
                with_messages("pr-3331", msgs)?
            }
            Screen::SettingsFromChat | Screen::SettingsApplied => {
                let mut msgs = chat_messages();
                let (more, card_ix) =
                    settings_messages(msgs.len(), screen == Screen::SettingsApplied);
                msgs.extend(more);
                let mut s = with_messages("pr-3331", msgs)?;
                let state = if screen == Screen::SettingsApplied {
                    ChangeState::Applied
                } else {
                    ChangeState::Pending
                };
                if let Some(m) = s.messages.get(card_ix) {
                    s.changes.insert(m.client.id.clone(), settings_card(state));
                }
                // console.py 1504 to 1508: the row follows the card's state.
                let row = if state == ChangeState::Applied {
                    recent(
                        "15:50",
                        "changed andy/fx1 lanes and budget (Undo)",
                        RecentKind::Applied,
                    )
                } else {
                    recent(
                        "15:50",
                        "waiting for Apply: andy/fx1 lanes and budget",
                        RecentKind::Waiting,
                    )
                };
                s.listener.recent.insert(0, row);
                s
            }
            Screen::Ask | Screen::ModelPicker => ask_snapshot(screen)?,
            Screen::Plan | Screen::Spec | Screen::PlanReady | Screen::KickOff | Screen::Running => {
                let ready = matches!(screen, Screen::PlanReady | Screen::KickOff);
                let running = screen == Screen::Running;
                let mut s = with_messages("ridgeline", plan_messages(ready, running))?;
                s.roster = plan_roster()?;
                s.plan = Some(plan(ready, running)?);
                // Mockup fix 35: the status bar counts this project, not pr-3331.
                s.status = StatusSummary {
                    online: if running { 11 } else { 4 },
                    worktrees: if running { 7 } else { 0 },
                    spend_today_usd: s.status.spend_today_usd,
                };
                s
            }
            // Task 14 (V66): fx1 taken over; on chat-codex, switched to codex.
            Screen::ChatTerminal | Screen::SwitchAgent | Screen::AddAgent | Screen::ChatCodex => {
                let s = with_messages("pr-3331", chat_messages())?;
                super::fixture_take_over::fx1_taken_over(s, screen == Screen::ChatCodex)?
            }
            Screen::Chat
            | Screen::Agent
            | Screen::ListenerCollapsed
            | Screen::Palette
            | Screen::RoleFile
            | Screen::Project
            | Screen::ProjectTestsOpen
            | Screen::Git
            | Screen::GitCollapsed
            | Screen::Settings => with_messages("pr-3331", chat_messages())?,
        };
        Ok(Self {
            state: RefCell::new(snapshot),
            subscribers: RefCell::new(Vec::new()),
        })
    }

    fn publish(&self, update: &Update) {
        // A dropped receiver means that view is gone; forget its sender.
        self.subscribers
            .borrow_mut()
            .retain(|tx| tx.unbounded_send(update.clone()).is_ok());
    }

    fn set_change(
        &self,
        id: &super::ChangeId,
        next: impl FnOnce(&super::ChangeCard) -> ChangeState,
    ) -> Result<(), BackendError> {
        {
            let mut snapshot = self.state.borrow_mut();
            let card = snapshot
                .changes
                .values_mut()
                .find(|card| &card.id == id)
                .ok_or_else(|| BackendError::NoSuchChange(id.to_string()))?;
            card.state = next(card);
        }
        let snapshot = self.snapshot();
        self.publish(&Update::Snapshot(Box::new(snapshot)));
        Ok(())
    }
}

impl FixtureBackend {
    /// Task 18: plays the 15:31 DELEGATED exchange for a worker on `agent`,
    /// numbering the worker after the ones already delegated.
    // shortcut: the fixture stands in for agmend's Spawn; a later plan sends
    // it to agmend, and the listener's answer comes from the hub.
    fn spawn_worker(&self, agent: &str) -> Result<(), BackendError> {
        let added = {
            let mut snapshot = self.state.borrow_mut();
            let worker = free_worker_handle(&snapshot)?;
            let added = stamp(spawn_exchange(agent, &worker), snapshot.messages.len() + 1)?;
            snapshot.messages.extend(added.iter().cloned());
            added
        };
        for message in added {
            self.publish(&Update::Message(Box::new(message)));
        }
        Ok(())
    }
}

impl Backend for FixtureBackend {
    fn snapshot(&self) -> Snapshot {
        self.state.borrow().clone()
    }

    fn send(&self, draft: Draft) -> Result<(), BackendError> {
        if draft.body.len() > MAX_BODY_BYTES {
            return Err(BackendError::BodyTooLarge {
                len: draft.body.len(),
                max: MAX_BODY_BYTES,
            });
        }
        let message = {
            let mut snapshot = self.state.borrow_mut();
            let seq = snapshot.messages.len() + 1;
            // Task 15: answering an ASK or an ALERT is an ANSWER (protocol
            // section 4); anything else the person writes is an FYI.
            let verb = if answers_a_question(&snapshot, draft.reply_to.as_ref()) {
                "ANSWER"
            } else {
                "FYI"
            };
            let value = json!({
                "v": 1,
                "id": super::fixture_data::message_id(seq),
                "reply_to": draft.reply_to,
                "to": draft.to,
                "body": draft.body,
                "sent_at": "2026-10-25T22:00:00Z",
                // Tasks 13 and 15: a reply to an ASK or an ALERT (an
                // alert's Reply) answers it.
                "verb": verb,
                "data": {},
                "stream": "feed",
                "seq": seq,
                "from": snapshot.me_id,
                "from_handle": snapshot.me,
                "from_kind": "human",
                "mentioned": draft.to,
                "hub_at": "2026-10-25T22:00:00Z",
            });
            let message: agmen_proto::StampedEnvelope = serde_json::from_value(value)
                .map_err(|_| BackendError::Unsupported("a message the protocol rejects"))?;
            snapshot.messages.push(message.clone());
            message
        };
        // Task 15: the model rides beside the message, so a tagged message
        // arrives with the snapshot that carries its tag.
        if let Some(model) = draft.model {
            let position = (message.stream.clone(), message.seq);
            self.state
                .borrow_mut()
                .message_models
                .insert(position, model);
            let snapshot = self.snapshot();
            self.publish(&Update::Snapshot(Box::new(snapshot)));
        } else {
            self.publish(&Update::Message(Box::new(message)));
        }
        Ok(())
    }

    fn act(&self, action: Action) -> Result<(), BackendError> {
        match action {
            Action::Apply(id) => self.set_change(&id, |_| ChangeState::Applied),
            Action::Cancel(id) => self.set_change(&id, |_| ChangeState::Cancelled),
            // Andy's answer to review C93 (V72): undoing a narrowing change
            // leaves it undone; undoing a widening one waits for Apply again
            // (mockup fix 44).
            Action::Undo(id) => self.set_change(&id, |card| {
                if card.widens {
                    ChangeState::Pending
                } else {
                    ChangeState::Undone
                }
            }),
            // Task 13: an alert's action is my answer to it, sent to its sender.
            Action::AnswerAlert { alert, choice } => {
                let sender = self
                    .state
                    .borrow()
                    .messages
                    .iter()
                    .find(|m| m.client.id == alert)
                    .map(|m| m.from.clone())
                    .ok_or(BackendError::Unsupported(
                        "answering an alert the chat lacks",
                    ))?;
                self.send(Draft {
                    to: vec![sender],
                    body: choice,
                    reply_to: Some(alert),
                    model: None,
                })
            }
            // Task 14: take-over, hand back, switching and the agent scan.
            Action::TakeOver(_)
            | Action::HandBack(_)
            | Action::SwitchAgent { .. }
            | Action::ScanAgents => {
                super::fixture_take_over::act(&mut self.state.borrow_mut(), action)?;
                self.publish(&Update::Snapshot(Box::new(self.snapshot())));
                Ok(())
            }
            // shortcut: the fixture records nothing for this; snoozing stays
            // in the person's own view (Task 13).
            Action::SnoozeAlert(_) => Ok(()),
            // Task 19: the planning room's actions change the plan.
            Action::ApproveSpec { .. } | Action::ApproveShare | Action::KickOff => {
                super::fixture_plan::act(&mut self.state.borrow_mut(), action)?;
                self.publish(&Update::Snapshot(Box::new(self.snapshot())));
                Ok(())
            }
            // Task 18: the palette's spawn rows.
            Action::SpawnWorker { agent } => self.spawn_worker(&agent),
        }
    }

    fn subscribe(&self) -> UnboundedReceiver<Update> {
        let (tx, rx) = unbounded();
        self.subscribers.borrow_mut().push(tx);
        rx
    }
}

/// The most workers one fixture session can spawn.
const MAX_SPAWNED: usize = 99;

/// The first `andy/fxN` no worker in the roster, the agents, the sessions
/// or a DELEGATED already has.
fn free_worker_handle(snapshot: &Snapshot) -> Result<String, BackendError> {
    let roster = snapshot
        .roster
        .iter()
        .flat_map(|group| group.agents.iter().map(|agent| &agent.id));
    let delegated = snapshot
        .messages
        .iter()
        .filter_map(|m| match &m.client.payload {
            agmen_proto::Payload::Delegated { worker, .. } => Some(worker),
            _ => None,
        });
    let taken: std::collections::BTreeSet<_> = roster
        .chain(snapshot.agents.keys())
        .chain(snapshot.sessions.keys())
        .chain(delegated)
        .collect();
    for n in 1..=MAX_SPAWNED {
        let handle = format!("andy/fx{n}");
        if !taken.contains(&id(&handle)?) {
            return Ok(handle);
        }
    }
    Err(BackendError::Unsupported(
        "more spawned workers than the fixture holds",
    ))
}

// Task 15: the ask and model picker screens, and answers to questions.
/// The ask screen's chat with its ASK and the opus answer, or the model
/// picker's chat before it.
fn ask_snapshot(screen: Screen) -> Result<Snapshot, BackendError> {
    let mut msgs = chat_messages();
    let first = msgs.len();
    if screen == Screen::Ask {
        msgs.extend(ask_messages(first));
    }
    let mut s = with_messages("pr-3331", msgs)?;
    if screen == Screen::Ask {
        // Mockup fix 36: the answer's $0.31 shows in the listener's spend.
        s.listener.listener_today_usd = 0.73;
        s.listener.recent.insert(
            0,
            recent(
                "15:52",
                "answered: musl in C014 (git_show, opus)",
                RecentKind::Answered,
            ),
        );
        s.status.spend_today_usd = s.listener.workers_today_usd + s.listener.listener_today_usd;
        // V67: the question and its answer ran on opus.
        tag_from(&mut s, first, &ask_model());
    }
    Ok(s)
}

/// Whether `reply_to` names an ASK or an ALERT in the chat (Task 15).
fn answers_a_question(snapshot: &Snapshot, reply_to: Option<&agmen_proto::MessageId>) -> bool {
    reply_to.is_some_and(|parent| {
        snapshot.messages.iter().any(|m| {
            &m.client.id == parent
                && matches!(
                    m.client.payload.verb(),
                    Some(agmen_proto::Verb::Ask | agmen_proto::Verb::Alert)
                )
        })
    })
}

#[cfg(test)]
#[path = "fixture_tests.rs"]
mod tests;
