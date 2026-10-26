//! Named colors that resolve to the current theme's tokens.

use gpui::{Hsla, SharedString};

use crate::appearance::Theme;

/// A color by meaning; [`Color::hsla`] picks the theme token.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Color {
    /// Body text (`text`).
    #[default]
    Default,
    /// Headings, chevrons and separators (`icon.muted`).
    Muted,
    /// Secondary text, inactive icons, harness names (`text.placeholder`).
    Placeholder,
    /// `text.accent`.
    Accent,
    /// `success`.
    Success,
    /// `warning`.
    Warning,
    /// `error`.
    Error,
    /// The window background, for text on a solid colored fill.
    Background,
    /// A role's color, by lowercase role name (`Theme::role_color`).
    Role(SharedString),
}

impl Color {
    pub(crate) fn hsla(&self, theme: &Theme) -> Hsla {
        match self {
            Self::Default => theme.text,
            Self::Muted => theme.icon_muted,
            Self::Placeholder => theme.text_placeholder,
            Self::Accent => theme.text_accent,
            Self::Success => theme.success,
            Self::Warning => theme.warning,
            Self::Error => theme.error,
            Self::Background => theme.background,
            Self::Role(role) => theme.role_color(role),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_map_to_theme_tokens() {
        let theme = crate::appearance::Appearance::builtin().theme;
        assert_eq!(Color::Default.hsla(&theme), theme.text);
        assert_eq!(Color::Muted.hsla(&theme), theme.icon_muted);
        assert_eq!(Color::Placeholder.hsla(&theme), theme.text_placeholder);
        assert_eq!(
            Color::Role("engineer".into()).hsla(&theme),
            theme.role_color("engineer")
        );
    }
}
