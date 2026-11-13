//! Launch a managed agent through the local backend: its provider, model,
//! role, color and budget.

use super::components::{Button, Color, DialogFooter, Dropdown, Field, InputFrame, Label, Modal};
use super::text_input::TextInput;
use super::tokens::{
    BORDER_WIDTH, DIALOG_SMALL_WIDTH, RADIUS_MD, SPACE_4, SPACE_6, SPACE_14, SPACE_20,
};
use crate::appearance::Appearance;
use crate::backend::{Action, BackendHandle, Snapshot};
use crate::roles::RoleKind;
use gpui::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div,
};

/// The provider agmend launches as a managed agent.
const ANTHROPIC: &str = "Anthropic";
/// Claude Code's model names, offered when agmend lists none for it.
const CLAUDE_MODELS: [&str; 3] = ["sonnet", "opus", "haiku"];
/// The Effort options and what each sends; the first leaves the model's own.
const EFFORTS: [(&str, Option<&str>); 6] = [
    ("default", None),
    ("low", Some("low")),
    ("medium", Some("medium")),
    ("high", Some("high")),
    ("extra high", Some("xhigh")),
    ("max", Some("max")),
];
/// The Role option that launches with no role.
const GENERAL_PURPOSE: &str = "general-purpose";
/// The most palette colors offered; Zed themes carry eight players.
const MAX_SWATCHES: u8 = 8;

/// A coding agent the Provider dropdown lists.
struct Provider {
    title: SharedString,
    /// agmend's name for it; `None` for Claude Code, which a launch need not name.
    name: Option<String>,
    /// Whether agmend can launch it as a managed agent.
    launchable: bool,
    /// Whether the first model is the provider's own default, sent as no model.
    own_default: bool,
    models: Vec<SharedString>,
}

/// Which dropdown a click belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Provider,
    Model,
    Effort,
    Role,
}

/// An Anthropic model id as its name: `claude-opus-5-5` reads "Opus 5.5" and
/// `claude-fable-5-2-fast` "Fable 5.2 fast". Any other name, an alias such
/// as `sonnet` included, stays as it is.
fn model_label(id: &str) -> SharedString {
    let Some((family, rest)) = id
        .strip_prefix("claude-")
        .and_then(|name| name.split_once('-'))
        .filter(|(family, _)| family.is_ascii() && !family.is_empty())
    else {
        return id.to_owned().into();
    };
    let (version, extra): (Vec<&str>, Vec<&str>) = rest
        .split('-')
        .partition(|part| part.len() < 3 && part.bytes().all(|byte| byte.is_ascii_digit()));
    let (first, others) = family.split_at(1);
    let mut label = format!(
        "{}{others} {}",
        first.to_ascii_uppercase(),
        version.join(".")
    );
    for part in extra {
        label.push(' ');
        label.push_str(part);
    }
    label.into()
}

pub(crate) struct Closed;
pub(crate) struct NewAgentDialog {
    name: Entity<TextInput>,
    budget: Entity<TextInput>,
    default_model: String,
    providers: Vec<Provider>,
    /// `GENERAL_PURPOSE` first, then the roles an agent can be launched in.
    roles: Vec<SharedString>,
    provider: usize,
    model: usize,
    /// An index into [`EFFORTS`].
    effort: usize,
    role: usize,
    /// The palette slot picked; `None` follows the role's color.
    color: Option<u8>,
    open: Option<Pick>,
    error: Option<String>,
}
impl EventEmitter<Closed> for NewAgentDialog {}

/// Claude Code with `listed` models, or its usual three, and always the
/// person's default model.
fn claude(default_model: &str, listed: &[String]) -> Provider {
    let mut models: Vec<SharedString> = if listed.is_empty() {
        CLAUDE_MODELS.iter().map(|&model| model.into()).collect()
    } else {
        listed.iter().map(|model| model.clone().into()).collect()
    };
    if !models.iter().any(|model| model.as_ref() == default_model) {
        models.insert(0, default_model.to_owned().into());
    }
    Provider {
        title: ANTHROPIC.into(),
        name: None,
        launchable: true,
        own_default: false,
        models,
    }
}

impl NewAgentDialog {
    pub(crate) fn new(
        default_model: String,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut dialog = Self {
            name: cx.new(|cx| TextInput::new(cx).placeholder("worker")),
            budget: cx.new(|cx| TextInput::new(cx).placeholder("leave blank for no budget")),
            providers: vec![claude(&default_model, &[])],
            default_model,
            roles: vec![GENERAL_PURPOSE.into()],
            provider: 0,
            model: 0,
            effort: 0,
            role: 0,
            color: None,
            open: None,
            error,
        };
        dialog.model = dialog.default_model_index();
        dialog
    }

    /// Lists the snapshot's installed agents, their models and the roles an
    /// agent can take. Without it the dialog offers Claude Code and no role.
    #[must_use]
    pub(crate) fn choices(mut self, snapshot: &Snapshot) -> Self {
        let listed = |agent: &str| {
            snapshot
                .models
                .iter()
                .find(|group| group.agent == agent)
                .map_or(&[][..], |group| group.models.as_slice())
        };
        // What agmend says it can launch here. Before it answers, or when it
        // is too old to say, Claude Code alone.
        let mut providers = Vec::new();
        if snapshot.launch_providers.is_empty() {
            providers.push(claude(&self.default_model, listed("claude")));
        }
        for provider in &snapshot.launch_providers {
            if provider.name == "claude" {
                let models = if provider.models.is_empty() {
                    listed("claude")
                } else {
                    &provider.models
                };
                providers.push(claude(&self.default_model, models));
            } else {
                providers.push(Provider {
                    title: provider.title.clone().into(),
                    name: Some(provider.name.clone()),
                    launchable: true,
                    own_default: true,
                    models: std::iter::once("default".into())
                        .chain(provider.models.iter().map(|model| model.clone().into()))
                        .collect(),
                });
            }
        }
        // Other agents found on this machine are listed, and refused at launch.
        let others = snapshot
            .installed_agents
            .iter()
            .filter(|agent| {
                agent.is_installed()
                    && agent.name != "claude"
                    && !snapshot
                        .launch_providers
                        .iter()
                        .any(|provider| provider.name == agent.name)
            })
            .map(|agent| Provider {
                title: agent.title.clone().into(),
                name: Some(agent.name.clone()),
                launchable: false,
                own_default: false,
                models: match listed(&agent.name) {
                    [] => vec!["default".into()],
                    models => models.iter().map(|model| model.clone().into()).collect(),
                },
            });
        providers.extend(others);
        self.providers = providers;
        self.model = self.default_model_index();
        for file in &snapshot.roles {
            let name = SharedString::from(file.name.clone());
            // agmend starts listeners itself and refuses them here; base has no kind.
            let launchable = file
                .info
                .as_ref()
                .is_some_and(|info| info.kind != RoleKind::Listener);
            // The first entry is the no-role option, whatever a role is named.
            if launchable && !self.roles.iter().skip(1).any(|role| *role == name) {
                self.roles.push(name);
            }
        }
        self
    }

    /// Where the person's default model sits in the chosen provider's list;
    /// the first model when it is not there.
    fn default_model_index(&self) -> usize {
        self.providers
            .get(self.provider)
            .and_then(|provider| {
                provider
                    .models
                    .iter()
                    .position(|model| model.as_ref() == self.default_model)
            })
            .unwrap_or(0)
    }

    fn choose(&mut self, pick: Pick, index: usize) {
        match pick {
            Pick::Provider => {
                self.provider = index;
                self.model = self.default_model_index();
                self.error = None;
            }
            Pick::Model => self.model = index,
            Pick::Effort => self.effort = index,
            Pick::Role => self.role = index,
        }
    }

    fn launch(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = self.providers.get(self.provider) else {
            return;
        };
        if !provider.launchable {
            self.error = Some(format!(
                "agmen cannot launch {} agents yet. Choose a provider it can launch, such as {ANTHROPIC}.",
                provider.title
            ));
            cx.notify();
            return;
        }
        let name = self.name.read(cx).text().trim().to_owned();
        let model = if provider.own_default && self.model == 0 {
            None
        } else {
            provider
                .models
                .get(self.model)
                .map(ToString::to_string)
                .or_else(|| provider.name.is_none().then(|| self.default_model.clone()))
        };
        let provider = provider.name.clone();
        let budget = self.budget.read(cx).text().trim();
        let budget_cents = if budget.is_empty() {
            None
        } else if let Some(cents) = budget_cents(budget) {
            Some(cents)
        } else {
            self.error = Some(
                "Enter a positive budget in dollars, with at most two decimal places, or leave it blank for no budget."
                    .into(),
            );
            cx.notify();
            return;
        };
        let effort = EFFORTS
            .get(self.effort)
            .and_then(|(_, sent)| sent.map(str::to_owned));
        // OpenCode takes an effort as a variant of a named model.
        if effort.is_some() && model.is_none() {
            self.error = Some("Pick a model to use an effort with this provider.".into());
            cx.notify();
            return;
        }
        // The first option is general-purpose: no role.
        let role = self
            .roles
            .get(self.role)
            .filter(|_| self.role > 0)
            .map(ToString::to_string);
        let result = cx
            .try_global::<BackendHandle>()
            .ok_or_else(|| "No project backend is connected.".to_owned())
            .and_then(|backend| {
                backend
                    .0
                    .act(Action::LaunchAgent {
                        name,
                        provider,
                        model,
                        effort,
                        budget_cents,
                        role,
                        color: self.color,
                    })
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok(()) => cx.emit(Closed),
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn dropdown(
        &self,
        pick: Pick,
        id: &'static str,
        options: Vec<SharedString>,
        selected: usize,
        cx: &mut Context<Self>,
    ) -> Dropdown {
        let toggle = cx.listener(move |this, open: &bool, _, cx| {
            if *open {
                this.open = Some(pick);
            } else if this.open == Some(pick) {
                this.open = None;
            }
            cx.notify();
        });
        let change = cx.listener(move |this, index: &usize, _, cx| {
            this.choose(pick, *index);
            cx.notify();
        });
        Dropdown::new(id, options, selected)
            // The card's inner width, so a dropdown lines up with the text fields.
            .width(DIALOG_SMALL_WIDTH - SPACE_20 * 2. - BORDER_WIDTH * 2.)
            .open(self.open == Some(pick))
            .on_toggle(move |open, window, cx| toggle(&open, window, cx))
            .on_change(move |index, window, cx| change(&index, window, cx))
    }

    /// The role's own color first, then the theme's palette.
    fn swatches(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = &cx.global::<Appearance>().theme;
        let ring = theme.text_accent;
        let role = self
            .roles
            .get(self.role)
            .filter(|_| self.role > 0)
            .cloned()
            .unwrap_or_default();
        let count = u8::try_from(theme.players.len())
            .unwrap_or(MAX_SWATCHES)
            .min(MAX_SWATCHES);
        let fills: Vec<_> = std::iter::once(None)
            .chain((0..count).map(Some))
            .map(|slot| (slot, Color::agent(slot, &role).hsla(theme)))
            .collect();
        let swatches = fills.into_iter().map(|(slot, fill)| {
            let id = slot.map_or_else(
                || "new-agent:color:role".to_owned(),
                |slot| format!("new-agent:color:{slot}"),
            );
            let selector = id.clone();
            let border = if self.color == slot {
                ring
            } else {
                gpui::transparent_black()
            };
            div()
                .id(id)
                .debug_selector(move || selector)
                .tab_index(0)
                .p(SPACE_4)
                .rounded(RADIUS_MD)
                .border(BORDER_WIDTH)
                .border_color(border)
                .focus_visible(move |style| style.border_color(ring))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.color = slot;
                    cx.notify();
                }))
                .child(div().size(SPACE_14).rounded_full().bg(fill))
        });
        div().flex().items_center().gap(SPACE_6).children(swatches)
    }
}

fn budget_cents(raw: &str) -> Option<u32> {
    let (whole, fraction) = raw.split_once('.').unwrap_or((raw, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 2
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let dollars = whole.parse::<u32>().ok()?;
    let fraction = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u32>().ok()?.checked_mul(10)?,
        _ => fraction.parse::<u32>().ok()?,
    };
    dollars
        .checked_mul(100)?
        .checked_add(fraction)
        .filter(|cents| *cents > 0)
}

impl Render for NewAgentDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let providers = self.providers.iter().map(|p| p.title.clone()).collect();
        // Anthropic's models read by name; OpenCode's keep their provider/model ids.
        let models = self
            .providers
            .get(self.provider)
            .map(|provider| match provider.name {
                None => provider.models.iter().map(|id| model_label(id)).collect(),
                Some(_) => provider.models.clone(),
            })
            .unwrap_or_default();
        Modal::small("new-agent")
            .on_dismiss(cx.listener(|_, _, _, cx| cx.emit(Closed)))
            .title(Label::title("New Agent"))
            .child(
                Field::new("new-agent:name-field", "Name").child(
                    div()
                        .debug_selector(|| "new-agent:name".into())
                        .child(InputFrame::new().child(self.name.clone())),
                ),
            )
            .child(
                Field::new("new-agent:provider-field", "Provider").child(self.dropdown(
                    Pick::Provider,
                    "new-agent:provider",
                    providers,
                    self.provider,
                    cx,
                )),
            )
            .child(
                Field::new("new-agent:model-field", "Model").child(self.dropdown(
                    Pick::Model,
                    "new-agent:model",
                    models,
                    self.model,
                    cx,
                )),
            )
            .child(
                Field::new("new-agent:effort-field", "Effort").child(self.dropdown(
                    Pick::Effort,
                    "new-agent:effort",
                    EFFORTS.iter().map(|(label, _)| (*label).into()).collect(),
                    self.effort,
                    cx,
                )),
            )
            .child(
                Field::new("new-agent:role-field", "Role").child(self.dropdown(
                    Pick::Role,
                    "new-agent:role",
                    self.roles.clone(),
                    self.role,
                    cx,
                )),
            )
            .child(
                Field::new("new-agent:color-field", "Color")
                    .child(
                        div()
                            .debug_selector(|| "new-agent:color".into())
                            .child(self.swatches(cx)),
                    )
                    .hint("The first one follows the role."),
            )
            .child(
                Field::new("new-agent:budget-field", "Budget (USD)").child(
                    div()
                        .debug_selector(|| "new-agent:budget".into())
                        .child(InputFrame::new().child(self.budget.clone())),
                ),
            )
            .error(self.error.clone())
            .footer(
                DialogFooter::new(
                    Button::new("new-agent:launch", "Launch")
                        .on_click(cx.listener(|this, _, _, cx| this.launch(cx))),
                )
                .cancel(
                    Button::new("new-agent:cancel", "Cancel")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(Closed))),
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_are_bounded_positive_dollar_amounts() {
        assert_eq!(budget_cents("5"), Some(500));
        assert_eq!(budget_cents("0.01"), Some(1));
        assert_eq!(budget_cents("12.5"), Some(1250));
        for value in ["0", "-1", "NaN", "inf", "1.001", "42949673", "1..2", ""] {
            assert_eq!(budget_cents(value), None, "{value}");
        }
    }
}

#[cfg(test)]
mod interaction_tests {
    use super::*;
    use crate::backend::{Backend as _, Screen};
    use crate::ui::plan_test_support::plan_globals;
    use crate::ui::test_support::click;
    use gpui::TestAppContext;

    #[gpui::test]
    fn launch_button_passes_name_configured_model_and_budget_to_backend(cx: &mut TestAppContext) {
        let (backend, _) = plan_globals(cx, Screen::Plan, None);
        let (dialog, cx) = cx.add_window_view(|_, cx| NewAgentDialog::new("opus".into(), None, cx));
        dialog.update_in(cx, |dialog, window, cx| {
            dialog
                .name
                .update(cx, |input, cx| input.set_text("worker", window, cx));
            dialog
                .budget
                .update(cx, |input, cx| input.set_text("12.50", window, cx));
        });
        click(cx, "new-agent:launch");
        assert_eq!(
            *backend.acts.borrow(),
            vec![Action::LaunchAgent {
                name: "worker".into(),
                provider: None,
                model: Some("opus".into()),
                effort: None,
                budget_cents: Some(1250),
                role: None,
                color: None,
            }]
        );
    }

    #[gpui::test]
    fn blank_budget_launches_with_no_budget_and_the_picked_model_role_and_color(
        cx: &mut TestAppContext,
    ) {
        let (backend, _) = plan_globals(cx, Screen::Plan, None);
        let snapshot = backend.snapshot();
        let (dialog, cx) = cx.add_window_view(|_, cx| {
            NewAgentDialog::new("sonnet".into(), None, cx).choices(&snapshot)
        });
        let (model, role) = dialog.read_with(cx, |dialog, cx| {
            assert_eq!(dialog.roles[0], GENERAL_PURPOSE);
            assert!(dialog.roles.len() > 1, "the fixture has roles to launch in");
            for refused in ["base", "listener"] {
                assert!(!dialog.roles.iter().any(|role| role == refused));
            }
            assert_eq!(
                dialog.budget.read(cx).placeholder_text(),
                "leave blank for no budget"
            );
            // The default model is selected, wherever the list puts it.
            assert_eq!(dialog.providers[0].models[dialog.model], "sonnet");
            let other = usize::from(dialog.model == 0);
            (
                dialog.providers[0].models[other].to_string(),
                dialog.roles[1].to_string(),
            )
        });
        dialog.update_in(cx, |dialog, window, cx| {
            dialog
                .name
                .update(cx, |input, cx| input.set_text("worker", window, cx));
            // Each dropdown's menu row reports through the same handler as a click.
            let other = usize::from(dialog.model == 0);
            dialog.choose(Pick::Model, other);
            cx.notify();
        });
        click(cx, "new-agent:role");
        click(cx, "new-agent:role:1");
        click(cx, "new-agent:color:2");
        click(cx, "new-agent:launch");
        assert_eq!(
            *backend.acts.borrow(),
            vec![Action::LaunchAgent {
                name: "worker".into(),
                provider: None,
                model: Some(model),
                effort: None,
                budget_cents: None,
                role: Some(role),
                color: Some(2),
            }]
        );
    }

    #[gpui::test]
    fn a_provider_agmend_cannot_launch_is_listed_and_refused(cx: &mut TestAppContext) {
        let (backend, _) = plan_globals(cx, Screen::Plan, None);
        let snapshot = backend.snapshot();
        let (dialog, cx) = cx.add_window_view(|_, cx| {
            NewAgentDialog::new("sonnet".into(), None, cx).choices(&snapshot)
        });
        let other = dialog.read_with(cx, |dialog, _| {
            assert_eq!(dialog.providers[0].title, ANTHROPIC);
            assert!(dialog.providers.len() > 1, "the fixture has other agents");
            dialog.providers[1].title.to_string()
        });
        click(cx, "new-agent:provider");
        click(cx, "new-agent:provider:1");
        click(cx, "new-agent:launch");
        assert!(backend.acts.borrow().is_empty());
        let error = dialog.read_with(cx, |dialog, _| dialog.error.clone().unwrap());
        assert!(error.contains("cannot launch"), "{error}");
        assert!(error.contains(&other), "{error}");
        // Choosing Claude Code again clears the refusal and launches.
        click(cx, "new-agent:provider");
        click(cx, "new-agent:provider:0");
        assert!(dialog.read_with(cx, |dialog, _| dialog.error.is_none()));
        click(cx, "new-agent:launch");
        assert_eq!(backend.acts.borrow().len(), 1);
    }

    #[gpui::test]
    fn opencode_launches_on_its_own_default_model_or_a_listed_one(cx: &mut TestAppContext) {
        use crate::backend::LaunchProvider;
        let (backend, _) = plan_globals(cx, Screen::Plan, None);
        let mut snapshot = backend.snapshot();
        snapshot.launch_providers = vec![
            LaunchProvider {
                name: "claude".into(),
                title: ANTHROPIC.into(),
                models: Vec::new(),
            },
            LaunchProvider {
                name: "opencode".into(),
                title: "OpenCode".into(),
                models: vec!["anthropic/claude-haiku-5-5".into(), "ollama/qwen3".into()],
            },
        ];
        let (dialog, cx) = cx.add_window_view(|_, cx| {
            NewAgentDialog::new("sonnet".into(), None, cx).choices(&snapshot)
        });
        dialog.read_with(cx, |dialog, _| {
            let opencode = &dialog.providers[1];
            assert_eq!(opencode.title, "OpenCode");
            assert!(opencode.launchable);
            assert_eq!(
                opencode.models,
                ["default", "anthropic/claude-haiku-5-5", "ollama/qwen3"]
            );
            // agmend's list names OpenCode once; the fixture's installed copy is not repeated.
            assert_eq!(
                dialog
                    .providers
                    .iter()
                    .filter(|provider| provider.title == "OpenCode")
                    .count(),
                1
            );
        });
        click(cx, "new-agent:provider");
        click(cx, "new-agent:provider:1");
        click(cx, "new-agent:launch");
        click(cx, "new-agent:model");
        click(cx, "new-agent:model:1");
        click(cx, "new-agent:launch");
        let launch = |model: Option<&str>| Action::LaunchAgent {
            name: String::new(),
            provider: Some("opencode".into()),
            model: model.map(Into::into),
            effort: None,
            budget_cents: None,
            role: None,
            color: None,
        };
        assert_eq!(
            *backend.acts.borrow(),
            vec![launch(None), launch(Some("anthropic/claude-haiku-5-5"))]
        );
    }

    #[test]
    fn anthropic_model_ids_read_by_name_and_other_names_stay() {
        for (id, label) in [
            ("claude-opus-5-5", "Opus 5.5"),
            ("claude-opus-4-6", "Opus 4.6"),
            ("claude-fable-5", "Fable 5"),
            ("claude-haiku-4-5", "Haiku 4.5"),
            ("claude-fable-5-2-fast", "Fable 5.2 fast"),
            ("claude-haiku-4-5-20251001", "Haiku 4.5 20251001"),
            ("sonnet", "sonnet"),
            ("anthropic/claude-haiku-5-5", "anthropic/claude-haiku-5-5"),
            ("claude-", "claude-"),
        ] {
            assert_eq!(model_label(id), label, "{id}");
        }
    }

    #[gpui::test]
    fn effort_and_a_named_anthropic_model_reach_the_launch(cx: &mut TestAppContext) {
        use crate::backend::LaunchProvider;
        let (backend, _) = plan_globals(cx, Screen::Plan, None);
        let mut snapshot = backend.snapshot();
        snapshot.launch_providers = vec![
            LaunchProvider {
                name: "claude".into(),
                title: ANTHROPIC.into(),
                models: vec!["claude-fable-5-2".into(), "claude-opus-5-5".into()],
            },
            LaunchProvider {
                name: "opencode".into(),
                title: "OpenCode".into(),
                models: vec!["anthropic/claude-haiku-5-5".into()],
            },
        ];
        let (dialog, cx) = cx.add_window_view(|_, cx| {
            NewAgentDialog::new("sonnet".into(), None, cx).choices(&snapshot)
        });
        dialog.read_with(cx, |dialog, _| {
            assert_eq!(dialog.providers[0].title, "Anthropic");
            // The person's default alias first, then agmend's list.
            assert_eq!(
                dialog.providers[0].models,
                ["sonnet", "claude-fable-5-2", "claude-opus-5-5"]
            );
        });
        click(cx, "new-agent:model");
        click(cx, "new-agent:model:2");
        click(cx, "new-agent:effort");
        // "extra high" sends Claude Code's word for it.
        click(cx, "new-agent:effort:4");
        click(cx, "new-agent:launch");
        // OpenCode's own default model has no variant to carry an effort.
        click(cx, "new-agent:provider");
        click(cx, "new-agent:provider:1");
        click(cx, "new-agent:launch");
        let error = dialog.read_with(cx, |dialog, _| dialog.error.clone().unwrap());
        assert!(error.contains("Pick a model"), "{error}");
        assert_eq!(
            *backend.acts.borrow(),
            vec![Action::LaunchAgent {
                name: String::new(),
                provider: None,
                model: Some("claude-opus-5-5".into()),
                effort: Some("xhigh".into()),
                budget_cents: None,
                role: None,
                color: None,
            }]
        );
    }
}
