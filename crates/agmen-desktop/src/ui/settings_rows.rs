//! The Settings form's rows and the browse list's rows: free helpers that
//! build elements and hand clicks to the view's private methods (review
//! C211). A child of `settings_view`, so those methods stay private.

use std::path::Path;

use gpui::{
    AnyElement, App, Context, Div, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, div,
};

use super::{ICON_PREVIEW_NAMES, SettingsView};
use crate::appearance::{
    AgmenSettings, Appearance, Density, InstalledTheme, PickerEntry, ThemeEntry, ThemeExtension,
    ThemeOrigin,
};
use crate::icons::FileIcons;
use crate::ui::components::{Button, DetailRow, Segmented, SettingText, Stepper};
use crate::ui::tokens::{
    BORDER_WIDTH, ICON_PREVIEW, RADIUS_MD, SETTING_LABEL_WIDTH, SPACE_6, SPACE_8, SPACE_10,
    SPACE_12, SPACE_16,
};

/// A font family as the list shows it: Zed's dotted names by their fonts.
pub(super) fn font_label(family: &str) -> String {
    match family {
        ".ZedSans" => "Zed Sans (IBM Plex Sans)".to_owned(),
        ".ZedMono" => "Zed Mono (Lilex)".to_owned(),
        other => other.to_owned(),
    }
}

/// `path` with the home folder written as `~`.
pub(super) fn tilde(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The credit line of an installed theme: who made it and where it is from.
pub(super) fn installed_detail(theme: &ThemeEntry) -> String {
    let manifest = match &theme.origin {
        ThemeOrigin::ZedExtension(t) | ThemeOrigin::Agmen(t) => Some(t),
        ThemeOrigin::BuiltIn | ThemeOrigin::ZedUser => None,
    };
    // The theme file's own author first, as Zed shows it; else the
    // extension's authors.
    let author = theme.author.clone().or_else(|| {
        manifest
            .filter(|t| !t.authors.is_empty())
            .map(|t| t.authors.join(", "))
    });
    let by = author
        .map(|a| format!("by {a} \u{b7} "))
        .unwrap_or_default();
    // Zed never copies a LICENSE into the folders it installs, so a Zed
    // install without one is no fault of the theme (V107). agmen's own
    // install has none when neither the archive nor the repository had
    // one, or when asking GitHub failed (Andy, 2026-10-26, C305(c)).
    let credit = |t: &InstalledTheme, missing: &str| {
        // A license file without a readable line is still kept (C303).
        let unlabeled = if t.licenses.is_empty() {
            missing
        } else {
            "license file kept"
        };
        let license = t.license.as_deref().unwrap_or(unlabeled);
        let repo = t
            .repository
            .as_ref()
            .map(|r| format!(" \u{b7} {r}"))
            .unwrap_or_default();
        // C238, C239: an unreadable manifest adds its error to the credit.
        let manifest = t
            .manifest_error
            .as_ref()
            .map(|err| format!(" \u{b7} extension.toml unreadable ({err})"))
            .unwrap_or_default();
        format!("{license}{repo}{manifest}")
    };
    let from = match &theme.origin {
        ThemeOrigin::BuiltIn => "built into agmen".to_owned(),
        ThemeOrigin::ZedUser => "your Zed themes folder".to_owned(),
        ThemeOrigin::ZedExtension(t) => {
            format!(
                "installed in Zed ({}) \u{b7} {}",
                t.id,
                credit(t, "license kept by Zed")
            )
        }
        ThemeOrigin::Agmen(t) => {
            // Only a 404, or no repository on GitHub, is "not found"; a
            // rate limit or any other failure is a check to repeat, and so
            // is an install with no recorded check, whose outcome is unknown
            // (C324).
            let missing = match t.license_check {
                Some(crate::appearance::LicenseCheck::NotFound) => "license not found",
                Some(
                    crate::appearance::LicenseCheck::Failed
                    | crate::appearance::LicenseCheck::Found,
                )
                | None => "license not checked; install again to retry",
            };
            format!("installed by agmen \u{b7} {}", credit(t, missing))
        }
    };
    format!("{by}{from}")
}

/// The credit line of an offered theme; its license is fetched on install,
/// and a missing one never blocks it (Andy, 2026-10-26).
pub(super) fn offered_detail(ext: &ThemeExtension) -> String {
    let by = if ext.authors.is_empty() {
        String::new()
    } else {
        format!("by {} \u{b7} ", ext.authors.join(", "))
    };
    let repo = ext.repository.as_deref().unwrap_or("no repository");
    format!("{by}{repo} \u{b7} license fetched on install")
}

pub(super) fn density_row(density: Density, cx: &mut Context<SettingsView>) -> AnyElement {
    let density_ix = usize::from(density == Density::Compact);
    setting("Chat density", "Spacing between messages.", cx)
        .child(controls().child(
            Segmented::new("density", ["Comfortable", "Compact"], density_ix).on_change(
                SettingsView::handler(cx, |this, ix: usize, cx| {
                    let density = if ix == 0 {
                        Density::Comfortable
                    } else {
                        Density::Compact
                    };
                    this.change_own(cx, |s| s.density = density);
                }),
            ),
        ))
        .into_any_element()
}

/// One row of the browse list: name, credit line, and Use or Install.
pub(super) fn browse_row(
    ix: usize,
    entry: PickerEntry,
    cx: &mut Context<SettingsView>,
) -> impl IntoElement + use<> {
    let (name, detail, action): (String, String, AnyElement) = match entry {
        PickerEntry::Installed(t) => {
            let detail = installed_detail(&t);
            let name = t.name.clone();
            let pick = Button::new(SharedString::from(format!("browse:{ix}:use")), "Use").on_click(
                cx.listener(move |this, _, _, cx| {
                    let name = name.clone();
                    this.change(cx, |s| s.theme = Some(name));
                }),
            );
            (t.name, detail, pick.into_any_element())
        }
        PickerEntry::Marketplace(ext) => {
            let detail = offered_detail(&ext);
            let name = ext.name.clone();
            let install = Button::new(
                SharedString::from(format!("browse:{ix}:install")),
                "Install",
            )
            .on_click(cx.listener(move |this, _, _, cx| this.install(ext.clone(), cx)));
            (name, detail, install.into_any_element())
        }
    };
    DetailRow::new(format!("browse:{ix}"), name, detail).end(action)
}

/// One setting: its label and hint on the left; the caller adds controls.
pub(super) fn setting(label: &'static str, hint: &'static str, cx: &App) -> Div {
    div()
        .debug_selector(|| format!("setting:{label}"))
        .w_full()
        .flex()
        .items_start()
        .gap(SPACE_16)
        .py(SPACE_12)
        .border_b(BORDER_WIDTH)
        .border_color(super::super::rule_color(cx))
        .child(
            div()
                .w(SETTING_LABEL_WIDTH)
                .flex_none()
                .child(SettingText::new(label, hint)),
        )
}

/// The control column of a setting.
pub(super) fn controls() -> Div {
    // Start-aligned, so a segmented control keeps its own width.
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .items_start()
        .gap(SPACE_8)
}

/// Controls side by side.
pub(super) fn row() -> Div {
    div().flex().items_center().gap(SPACE_10)
}

/// A font size stepper whose value `set` stores.
pub(super) fn size_stepper(
    id: &'static str,
    size: gpui::Pixels,
    cx: &Context<SettingsView>,
    set: fn(&mut AgmenSettings, f32),
) -> Stepper {
    // Sizes are whole pixels in the stepper; a fractional size from Zed's
    // settings shows rounded and steps from there.
    let value = size.as_f32().round().clamp(0., 1000.);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded and clamped to 0 to 1000 just above"
    )]
    let value = value as u32;
    Stepper::new(id, value).on_change(SettingsView::handler(cx, move |this, v: u32, cx| {
        #[expect(clippy::cast_precision_loss, reason = "6 to 72 is exact in f32")]
        let v = v as f32;
        this.change(cx, |s| set(s, v));
    }))
}

/// The icon theme preview: [`ICON_PREVIEW_NAMES`].
pub(super) fn icon_preview(cx: &mut App) -> impl IntoElement + use<> {
    let theme = &cx.global::<Appearance>().theme;
    let (bg, tint) = (theme.background, theme.icon_muted);
    let icons: Vec<AnyElement> = cx.try_global::<FileIcons>().map_or_else(Vec::new, |icons| {
        ICON_PREVIEW_NAMES
            .iter()
            .map(|(name, is_dir)| {
                icons
                    .icon(Path::new(name), *is_dir, false)
                    .render(tint, ICON_PREVIEW)
            })
            .collect()
    });
    cx.defer(FileIcons::fetch_wanted);
    div()
        .debug_selector(|| "icon-preview".into())
        .flex()
        .items_center()
        .gap(SPACE_10)
        .py(SPACE_6)
        .px(SPACE_10)
        .rounded(RADIUS_MD)
        .bg(bg)
        .children(icons)
}
