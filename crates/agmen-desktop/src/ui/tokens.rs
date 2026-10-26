//! Sizes, spacing, radii and font sizes, from `docs/mockups/scripts/console.py`.
//! Views and components take every size from here, never a raw value.
//!
//! Layout sizes (bars, docks, rows, gaps, padding, radii, icons) are pixels.
//! Text sizes are rems, so they scale with the person's `ui_font_size`: the
//! window's rem size is that font size, as in Zed.

use gpui::{Pixels, Rems, px};

/// `px` pixels as rems at Zed's 16 px base (Zed's `rems_from_px`).
#[must_use]
pub(crate) const fn rems_from_px(px: f32) -> Rems {
    Rems(px / 16.)
}

/// Andy's `ui_font_size`, the rem size the mockup was drawn at.
#[cfg(test)]
pub(crate) const MOCKUP_REM: Pixels = px(15.);

// Zed's UI text scale (`TextSize` in Zed's ui crate), the only sizes UI text
// uses. At a 15 px rem: Large 15, Default 13.125, Small 11.25, XSmall 9.375.
// The mockup's 13 px text is Default, its 11 and 12 px text is Small, and
// its 10 px glyphs are XSmall. Code text uses the code font's own size.
pub(crate) const TEXT_LARGE: Rems = rems_from_px(16.);
pub(crate) const TEXT_DEFAULT: Rems = rems_from_px(14.);
pub(crate) const TEXT_SMALL: Rems = rems_from_px(12.);
pub(crate) const TEXT_XSMALL: Rems = rems_from_px(10.);

/// Line height for chrome text, as a multiple of its size (the mockup's
/// wrapped values advance 14 px at 12 px).
pub(crate) const LINE_HEIGHT: f32 = 1.2;

// Bars and docks.
pub(crate) const TITLE_BAR_HEIGHT: Pixels = px(38.);
pub(crate) const STATUS_BAR_HEIGHT: Pixels = px(28.);
pub(crate) const LEFT_DOCK_WIDTH: Pixels = px(272.);
pub(crate) const RIGHT_DOCK_WIDTH: Pixels = px(340.);
pub(crate) const TAB_BAR_HEIGHT: Pixels = px(34.);
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the chat pane uses it in Task 9 of the desktop plan"
    )
)]
pub(crate) const BREADCRUMB_HEIGHT: Pixels = px(32.);
/// Where the title bar's text starts, past the traffic lights at x 12 to 68
/// and the mockup's spacer.
pub(crate) const TRAFFIC_LIGHTS_INSET: Pixels = px(96.);

// Rows.
pub(crate) const AGENT_ROW_HEIGHT: Pixels = px(26.);
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the Project panel uses it in Task 11 of the desktop plan"
    )
)]
pub(crate) const PROJECT_ROW_HEIGHT: Pixels = px(24.);
/// A tree row's left padding at depth 0, past the group row's chevron.
pub(crate) const TREE_INDENT_BASE: Pixels = px(26.);
/// Extra left padding per level of depth.
pub(crate) const TREE_INDENT_STEP: Pixels = px(14.);
/// The label column of a label and value row.
pub(crate) const KV_LABEL_WIDTH: Pixels = px(96.);

// The code font's cell at 13 px.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the terminal view uses it in Task 14 of the desktop plan"
    )
)]
pub(crate) const MONO_CELL_WIDTH: Pixels = px(7.62);
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the terminal view uses it in Task 14 of the desktop plan"
    )
)]
pub(crate) const MONO_CELL_HEIGHT: Pixels = px(18.);

// Spacing: the mockup's gaps and padding, named by their pixels.
pub(crate) const SPACE_1: Pixels = px(1.);
pub(crate) const SPACE_2: Pixels = px(2.);
pub(crate) const SPACE_3: Pixels = px(3.);
pub(crate) const SPACE_5: Pixels = px(5.);
pub(crate) const SPACE_6: Pixels = px(6.);
pub(crate) const SPACE_7: Pixels = px(7.);
pub(crate) const SPACE_8: Pixels = px(8.);
/// The settings stepper's button and value padding (node 8159).
pub(crate) const SPACE_9: Pixels = px(9.);
pub(crate) const SPACE_10: Pixels = px(10.);
pub(crate) const SPACE_12: Pixels = px(12.);
pub(crate) const SPACE_14: Pixels = px(14.);
pub(crate) const SPACE_16: Pixels = px(16.);

// Radii.
pub(crate) const RADIUS_XS: Pixels = px(2.);
pub(crate) const RADIUS_SM: Pixels = px(4.);
pub(crate) const RADIUS_MD: Pixels = px(5.);
/// Menus and cards: the switch menu (node 1802) and the model picker (node 3355).
pub(crate) const RADIUS_LG: Pixels = px(8.);

// Controls in "Desktop · settings" (node 7908).
/// The Follow Zed toggle's track (node 8098).
pub(crate) const TOGGLE_WIDTH: Pixels = px(30.);
pub(crate) const TOGGLE_HEIGHT: Pixels = px(18.);
/// The toggle's knob (node 8099).
pub(crate) const TOGGLE_KNOB: Pixels = px(14.);
/// The theme dropdown (node 8107), console.py's `dropdown` default.
pub(crate) const DROPDOWN_WIDTH: Pixels = px(300.);

// Icons and marks.
pub(crate) const ICON_SM: Pixels = px(12.);
/// The dock collapse glyph (mockup node 391, V63).
pub(crate) const ICON_GLYPH: Pixels = px(13.);
/// The filled dock button around it: 17 by 20.
pub(crate) const DOCK_BUTTON: gpui::Size<Pixels> = gpui::Size {
    width: px(17.),
    height: px(20.),
};
/// The leading slot of an Agents row: the status glyph's box (V62).
pub(crate) const LEADING_SLOT: Pixels = px(10.);
pub(crate) const ICON_MD: Pixels = px(14.);
pub(crate) const AVATAR_SIZE: Pixels = px(22.);
pub(crate) const DOT_SIZE: Pixels = px(7.);
pub(crate) const BORDER_WIDTH: Pixels = px(1.);
pub(crate) const PROGRESS_HEIGHT: Pixels = px(4.);

/// How strongly a tinted badge's fill shows its color.
pub(crate) const BADGE_TINT: f32 = 0.18;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_sizes_follow_zed_at_15px_rem() {
        let at_15 = |r: Rems| r.to_pixels(MOCKUP_REM).as_f32();
        assert!((at_15(TEXT_LARGE) - 15.).abs() < 0.001);
        assert!((at_15(TEXT_DEFAULT) - 13.125).abs() < 0.001);
        assert!((at_15(TEXT_SMALL) - 11.25).abs() < 0.001);
        assert!((at_15(TEXT_XSMALL) - 9.375).abs() < 0.001);
    }

    #[test]
    fn mockup_sizes() {
        assert_eq!(BREADCRUMB_HEIGHT, px(32.));
        assert_eq!(PROJECT_ROW_HEIGHT, px(24.));
        assert_eq!(MONO_CELL_WIDTH, px(7.62));
        assert_eq!(MONO_CELL_HEIGHT, px(18.));
    }
}
