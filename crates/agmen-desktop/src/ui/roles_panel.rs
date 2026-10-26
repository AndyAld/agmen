//! The left dock's Roles tab and the right dock's Role panel
//! (`desktop-role-file.webp`).

use std::path::Path;

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, RenderOnce, SharedString, Styled as _, Window, div,
};

use super::breadcrumb::Breadcrumb;
use super::code_view::CodeView;
use super::components::{
    Button, ButtonStyle, Color, Icon, IconSize, Indent, KvRow, KvSection, Label, ListItem,
};
use super::dock::RightDockHeader;
use super::syntax::Language;
use super::tokens::{DOT_SIZE, ICON_MD, SPACE_6, SPACE_14, SPACE_16};
use crate::appearance::Appearance;
use crate::backend::{RoleFile, RoleInfo, RoleLayer};
use crate::icons::{FileIcons, UiIcon};

/// What the Roles panel tells the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RolesPanelEvent {
    /// A role file was clicked; holds the role's name.
    Open(String),
}

/// Tests find each layer's header as `"role-layer:{slug}"` and each file as
/// `"role:{name}"`.
pub(crate) struct RolesPanel {
    roles: Vec<RoleFile>,
    selected: Option<String>,
}

impl EventEmitter<RolesPanelEvent> for RolesPanel {}

/// Each layer's header text and selector slug, in the order the panel
/// lists them (spec 3.2: built-in, then project, then personal).
const LAYERS: [(RoleLayer, &str, &str); 3] = [
    (RoleLayer::BuiltIn, "Built-in", "built-in"),
    (RoleLayer::Project, ".agmen/roles", "project"),
    (RoleLayer::Personal, "~/.config/agmen/roles", "personal"),
];

impl RolesPanel {
    pub(crate) const fn new(roles: Vec<RoleFile>) -> Self {
        Self {
            roles,
            selected: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Highlights `name`'s row without telling the workspace.
    pub(crate) fn set_selected(&mut self, name: &str) {
        self.selected = Some(name.to_owned());
    }

    fn clicked(&mut self, name: &str, cx: &mut Context<Self>) {
        self.set_selected(name);
        cx.emit(RolesPanelEvent::Open(name.to_owned()));
        cx.notify();
    }

    fn role_row(&self, role: &RoleFile, cx: &mut Context<Self>) -> ListItem {
        let file = format!("{}.xml", role.name);
        let theme = &cx.global::<Appearance>().theme;
        let icon_color = theme.icon_muted;
        let icon = cx.try_global::<FileIcons>().map_or_else(
            || {
                Icon::new(UiIcon::File)
                    .size(IconSize::Small)
                    .into_any_element()
            },
            |icons| {
                icons
                    .icon(Path::new(&file), false, false)
                    .render(icon_color, ICON_MD)
            },
        );
        let name = role.name.clone();
        ListItem::new(format!("role:{}", role.name), Indent::Depth(1))
            .selected(self.selected.as_deref() == Some(role.name.as_str()))
            .leading(icon)
            .child(Label::new(file).code())
            .end(role_dot(&role.name, theme.role_color(&role.name)))
            .on_click(cx.listener(move |this, _, _, cx| this.clicked(&name, cx)))
    }
}

/// A dot in the role's color (mockup fix 55). Tests find it as
/// `"role-dot:{name}"`.
fn role_dot(name: &str, color: gpui::Hsla) -> impl IntoElement {
    let selector = format!("role-dot:{name}");
    div()
        .debug_selector(|| selector)
        .size(DOT_SIZE)
        .flex_none()
        .rounded_full()
        .bg(color)
}

impl Render for RolesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows = Vec::new();
        for (layer, title, slug) in LAYERS {
            rows.push(
                ListItem::new(format!("role-layer:{slug}"), Indent::Group)
                    .leading(
                        Icon::new(UiIcon::ChevronDown)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
                    .child(Label::new(title).weight(FontWeight::MEDIUM)),
            );
            for role in self.roles.iter().filter(|r| r.layer == layer) {
                rows.push(self.role_row(role, cx));
            }
        }
        div().w_full().flex().flex_col().py(SPACE_6).children(rows)
    }
}

/// The right dock's Role panel: what agmen enforces for a role, who uses
/// it, and how to mention it. Tests find it as `role-panel`.
#[derive(Debug, IntoElement)]
pub(crate) struct RolePanel {
    name: SharedString,
    info: RoleInfo,
}

impl RolePanel {
    #[must_use]
    pub(crate) fn new(name: impl Into<SharedString>, info: RoleInfo) -> Self {
        Self {
            name: name.into(),
            info,
        }
    }
}

impl RenderOnce for RolePanel {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let i = self.info;
        let mut enforced = KvSection::new("Enforced by agmen")
            .row(KvRow::new("Kind", i.kind.to_string()))
            .row(KvRow::new("Tools", i.tools.join(" ")));
        if let Some(note) = i.bash_note {
            enforced = enforced.row(KvRow::new("Bash", note).color(Color::Warning));
        }
        let enforced = enforced
            .row(KvRow::new("Edits", i.edits.to_string()))
            .row(KvRow::new("Verbs", i.verbs.join(" ")).color(Color::Success))
            .row(KvRow::new("Refused", i.refused.join(" ")).color(Color::Error))
            .row(KvRow::new("Tier", i.tier.to_string()));
        let used_by = (!i.used_by.is_empty()).then(|| {
            i.used_by
                .into_iter()
                .fold(KvSection::new("Used by"), |section, (who, what)| {
                    section.row(KvRow::new(who, what))
                })
        });
        let (handle, what) = i.mention;
        let mention = KvSection::new("Mention").row(KvRow::new(handle, what));
        div()
            .debug_selector(|| "role-panel".into())
            .size_full()
            .flex()
            .flex_col()
            .child(RightDockHeader::new("Role", self.name))
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(SPACE_16)
                    .p(SPACE_14)
                    .child(enforced)
                    .children(used_by)
                    .child(mention),
            )
    }
}

/// A role file in a center tab: its layer in the breadcrumb and the XML in
/// a code view. A built-in role offers Duplicate to .agmen/roles, since
/// built-ins are read-only (spec 3.2). Tests find the button as
/// `role-duplicate`.
pub(crate) struct RoleFileView {
    parts: [SharedString; 3],
    built_in: bool,
    code: Entity<CodeView>,
}

impl RoleFileView {
    pub(crate) fn new(role: &RoleFile, cx: &mut Context<Self>) -> Self {
        let file = format!("{}.xml", role.name);
        let slug = LAYERS
            .iter()
            .find(|(layer, ..)| *layer == role.layer)
            .map_or("", |(_, _, slug)| *slug);
        let language = Language::for_path(Path::new(&file));
        let text = role.text.clone();
        let code = cx.new(|_| CodeView::new(&text, language, Vec::new()));
        Self {
            parts: ["roles".into(), slug.into(), file.into()],
            built_in: role.layer == RoleLayer::BuiltIn,
            code,
        }
    }
}

impl RoleFileView {
    /// The file name the breadcrumb ends with, such as `reviewer.xml`.
    #[cfg(test)]
    pub(crate) fn file_name(&self) -> &str {
        &self.parts[2]
    }
}

impl Render for RoleFileView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &cx.global::<Appearance>().theme;
        // shortcut: Duplicate does nothing until the backend writes role
        // files (Task 14).
        let duplicate = self.built_in.then(|| {
            Button::new("role-duplicate", "Duplicate to .agmen/roles").style(ButtonStyle::Filled)
        });
        let mut crumb = Breadcrumb::new(self.parts.clone());
        if let Some(button) = duplicate {
            crumb = crumb.end(button);
        }
        div()
            .debug_selector(|| "role-file".into())
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.editor_background)
            .child(crumb)
            .child(div().flex_1().min_h_0().child(self.code.clone()))
    }
}

#[cfg(test)]
#[path = "roles_panel_tests.rs"]
mod tests;
