//! Stopped managed agents, kept as trees until resumed or removed.

use std::collections::BTreeMap;
use std::rc::Rc;

use futures::StreamExt as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, Div, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div,
};
use jiff::tz::TimeZone;

use super::agents_panel::{AgentRole, agent_item};
use super::components::{Button, ButtonStyle, Color, EmptyState, Indent, Label, LabelSize};
use super::format::clock;
use super::tokens::{SPACE_4, SPACE_6};
use crate::backend::{
    Action, AgentStatus, BackendError, BackendHandle, MAX_DEPTH, RecentAgent, Snapshot, Update,
};
use crate::keymap::AskRemoveAgent;
use crate::local_time::LocalZone;

/// Indices in tree order, newest roots and siblings first. Missing parents
/// become roots; a cycle's newest remaining entry becomes a root too.
#[must_use]
pub(crate) fn recent_rows(recents: &[RecentAgent]) -> Vec<(usize, u8)> {
    let indices: BTreeMap<_, _> = recents
        .iter()
        .enumerate()
        .map(|(ix, a)| (&a.id, ix))
        .collect();
    let mut newest: Vec<_> = (0..recents.len()).collect();
    newest.sort_by(|&a, &b| recents[b].stopped_at.cmp(&recents[a].stopped_at));
    let mut children = vec![Vec::new(); recents.len()];
    let mut roots = Vec::new();
    for &ix in &newest {
        if let Some(parent) = recents[ix].parent.as_ref().and_then(|id| indices.get(id)) {
            children[*parent].push(ix);
        } else {
            roots.push(ix);
        }
    }
    let mut seen = vec![false; recents.len()];
    let mut rows = Vec::with_capacity(recents.len());
    let mut stack = Vec::with_capacity(recents.len());
    // The fallback seeds include cycles with no natural root. Each index
    // enters the stack once, so both storage and visits are bounded by input.
    for root in roots.into_iter().chain(newest) {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        stack.push((root, 0_u8));
        for _ in 0..recents.len() {
            let Some((ix, depth)) = stack.pop() else {
                break;
            };
            rows.push((ix, depth));
            for &child in children[ix].iter().rev() {
                if !seen[child] {
                    seen[child] = true;
                    stack.push((child, depth.saturating_add(1).min(MAX_DEPTH)));
                }
            }
        }
    }
    rows
}

/// Count below each row using parent links, because display indents stop
/// growing at MAX_DEPTH. A cycle's back edge never contributes to its count.
fn descendants(recents: &[RecentAgent], rows: &[(usize, u8)]) -> Vec<usize> {
    let positions: BTreeMap<_, _> = rows
        .iter()
        .enumerate()
        .map(|(position, &(ix, _))| (&recents[ix].id, (ix, position)))
        .collect();
    let mut below = vec![0; recents.len()];
    for (position, &(ix, _)) in rows.iter().enumerate().rev() {
        if let Some((parent, parent_position)) =
            recents[ix].parent.as_ref().and_then(|id| positions.get(id))
            && *parent_position < position
        {
            below[*parent] += below[ix] + 1;
        }
    }
    below
}

pub(crate) struct RecentsView {
    snapshot: Rc<Snapshot>,
    error: Option<String>,
    _updates: Option<Task<()>>,
}

impl RecentsView {
    pub(crate) fn new(
        snapshot: &Rc<Snapshot>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let backend = cx.try_global::<BackendHandle>();
        let updates = backend.map(|backend| backend.0.subscribe());
        // TabSources keeps the startup snapshot. A tab opened after a stop
        // must begin with the backend's current state, then follow updates.
        let snapshot = backend.map_or_else(
            || Rc::clone(snapshot),
            |backend| Rc::new(backend.0.snapshot()),
        );
        let updates = updates.map(|mut updates| {
            cx.spawn_in(window, async move |view, cx| {
                // Like ChatView, this subscription ends with the backend or
                // the view. It uses the backend's existing update channel.
                while let Some(update) = updates.next().await {
                    if let Update::Snapshot(snapshot) = update
                        && view
                            .update(cx, |view, cx| {
                                view.snapshot = Rc::new(*snapshot);
                                cx.notify();
                            })
                            .is_err()
                    {
                        break;
                    }
                }
            })
        });
        Self {
            snapshot,
            error: None,
            _updates: updates,
        }
    }

    fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        let result = match cx.try_global::<BackendHandle>() {
            Some(backend) => backend.0.act(action),
            None => Err(BackendError::Unsupported("acting without a backend")),
        };
        self.error = result.err().map(|error| error.to_string());
        cx.notify();
    }

    fn row_end(&self, agent: &RecentAgent, below: usize, cx: &mut Context<Self>) -> Div {
        let zone = cx
            .try_global::<LocalZone>()
            .map_or(TimeZone::UTC, |local| local.0.clone());
        let stopped = agent.stopped_at.map_or_else(
            || "stopped at an unknown time".into(),
            |at| format!("stopped {}", clock(at, &zone)),
        );
        let resume_id = agent.id.clone();
        let tree_id = agent.id.clone();
        let remove_id = agent.id.clone();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_4)
            .child(
                Label::new(stopped)
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .when(agent.resumable, |end| {
                end.child(
                    Button::new(format!("recents:resume:{}", agent.id), "Resume")
                        .style(ButtonStyle::Plain)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.act(
                                Action::StartAgent {
                                    identity: resume_id.clone(),
                                    root: None,
                                },
                                cx,
                            );
                        })),
                )
                .when(below > 0, |end| {
                    end.child(
                        Button::new(
                            format!("recents:resume-tree:{}", agent.id),
                            format!("Resume with {below} below"),
                        )
                        .style(ButtonStyle::Plain)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.act(Action::ResumeTree(tree_id.clone()), cx);
                        })),
                    )
                })
            })
            .child(
                Button::new(format!("recents:remove:{}", agent.id), "Remove")
                    .style(ButtonStyle::Plain)
                    // The workspace asks first, as the Agents panel's Remove does.
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        window.dispatch_action(Box::new(AskRemoveAgent(remove_id.clone())), cx);
                    }),
            )
    }

    fn row(&self, agent: &RecentAgent, indent: u8, below: usize, cx: &mut Context<Self>) -> Div {
        let id = agent.id.clone();
        let hint_id = agent.id.clone();
        div()
            .w_full()
            .child(
                agent_item(
                    format!("recents:row:{}", agent.id),
                    indent,
                    AgentStatus::Offline,
                    &agent.handle,
                    &AgentRole::of(agent.role.as_deref().unwrap_or("agent")),
                    "",
                )
                .end(self.row_end(agent, below, cx)),
            )
            .when(!agent.resumable, |row| {
                row.child(
                    div()
                        .debug_selector(move || format!("recents:revoked:{id}"))
                        .pl(Indent::Depth(indent).padding())
                        .child(
                            Label::new("can't be resumed; its identity was revoked")
                                .color(Color::Muted)
                                .wrap(),
                        ),
                )
            })
            .child(
                div()
                    .debug_selector(move || format!("recents:remove-hint:{hint_id}"))
                    .pl(Indent::Depth(indent).padding())
                    .child(
                        Label::new("Removing frees this agent's name for a new agent.")
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .wrap(),
                    ),
            )
    }
}

impl Render for RecentsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = recent_rows(&self.snapshot.recents);
        let below = descendants(&self.snapshot.recents, &rows);
        let errors: Vec<_> = self
            .error
            .iter()
            .chain(self.snapshot.agent_error.iter())
            .cloned()
            .collect();
        div()
            .id("recents")
            .size_full()
            .overflow_y_scroll()
            .py(SPACE_6)
            .when(!errors.is_empty(), |view| {
                view.child(
                    div().debug_selector(|| "recents:error".into()).children(
                        errors
                            .into_iter()
                            .map(|error| Label::new(error).color(Color::Error).wrap()),
                    ),
                )
            })
            .when(rows.is_empty(), |view| {
                view.child(EmptyState::new(
                    "recents:empty",
                    "No stopped agents. Agents you stop, or that stop when agmen quits, wait here.",
                ))
            })
            .children(
                rows.into_iter().map(|(ix, indent)| {
                    self.row(&self.snapshot.recents[ix], indent, below[ix], cx)
                }),
            )
    }
}

#[cfg(test)]
#[path = "recents_view_tests.rs"]
mod tests;
