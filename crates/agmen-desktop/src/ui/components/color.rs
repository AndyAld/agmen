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
    /// The theme's `text.muted`, as a change card's "Settings changed". In
    /// Gruvbox Material Dark Hard it is brighter than `text` (#e0c89e over
    /// #d8bf94); in most Zed themes it is dimmer.
    TextMuted,
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
    /// A slot of the theme's player palette, for the mockup colors no named
    /// token holds: slot 1 is its magenta (items, ASSIGN), slot 3 its orange
    /// (CLAIM). A theme without that slot gives `text.muted`.
    Player(u8),
}

impl Color {
    pub(crate) fn hsla(&self, theme: &Theme) -> Hsla {
        match self {
            Self::Default => theme.text,
            Self::Muted => theme.icon_muted,
            Self::TextMuted => theme.text_muted,
            Self::Placeholder => theme.text_placeholder,
            Self::Accent => theme.text_accent,
            Self::Success => theme.success,
            Self::Warning => theme.warning,
            Self::Error => theme.error,
            Self::Background => theme.background,
            Self::Role(role) => theme.role_color(role),
            Self::Player(slot) => theme
                .players
                .get(usize::from(*slot))
                .copied()
                .unwrap_or(theme.text_muted),
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

    #[test]
    fn player_slots_map_to_the_palette() {
        let theme = crate::appearance::Appearance::builtin().theme;
        assert_eq!(Color::Player(1).hsla(&theme), theme.players[1]);
        assert_eq!(Color::Player(3).hsla(&theme), theme.players[3]);
        assert_eq!(Color::Player(200).hsla(&theme), theme.text_muted);
    }

    #[test]
    fn text_muted_is_the_text_muted_token() {
        let theme = crate::appearance::Appearance::builtin().theme;
        assert_eq!(Color::TextMuted.hsla(&theme), theme.text_muted);
    }
}
