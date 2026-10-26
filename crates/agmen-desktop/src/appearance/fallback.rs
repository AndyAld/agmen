//! The built-in theme: the Zed theme "Gruvbox Material Dark Hard" that the
//! mockups use (`docs/mockups/scripts/console.py`).

use gpui::{Hsla, rgb, rgba};

use super::theme::Theme;

fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

/// The theme used when no Zed theme can be loaded, and for keys a theme lacks.
#[must_use]
pub(crate) fn gruvbox_material_dark_hard() -> Theme {
    let (bg, surf, sel, text, bright) = (0x1d2021, 0x282828, 0x3c3836, 0xd8bf94, 0xe0c89e);
    let (faint, dim, line, accent) = (0x928374, 0x7c6f64, 0x504945, 0x72b9a9);
    let (err, warn, ok) = (0xed675f, 0xdda752, 0xafbf5c);
    let (mag, grn, org) = (0xda7f98, 0x82be78, 0xea894b);
    let ansi = [
        bg, err, ok, warn, accent, mag, grn, text, line, err, ok, warn, accent, mag, grn, text,
    ];
    Theme {
        background: hex(bg),
        surface: hex(bg),
        elevated_surface: hex(surf),
        element_selected: hex(sel),
        element_hover: hex(surf),
        border: hex(sel),
        border_variant: hex(surf),
        text: hex(text),
        text_muted: hex(bright),
        text_placeholder: hex(faint),
        text_accent: hex(accent),
        icon_muted: hex(dim),
        editor_background: hex(bg),
        line_number: hex(line),
        error: hex(err),
        warning: hex(warn),
        success: hex(ok),
        info: hex(accent),
        created: hex(ok),
        modified: hex(warn),
        deleted: hex(err),
        ansi: ansi.map(hex),
        players: [accent, mag, err, org, grn, warn, ok, text]
            .into_iter()
            .map(hex)
            .collect(),
        element: hex(bg),
        selection: rgba(0x72b9_a930).into(),
        accents: [accent, mag, err, org, grn, warn, ok]
            .into_iter()
            .map(hex)
            .collect(),
        // The syntax colors of Andy's "Gruvbox Material Dark Hard".
        syntax: [
            ("attribute", accent),
            ("boolean", mag),
            ("comment", dim),
            ("comment.doc", faint),
            ("constant", mag),
            ("constructor", grn),
            ("function", grn),
            ("keyword", err),
            ("number", org),
            ("operator", org),
            ("property", accent),
            ("punctuation", text),
            ("string", ok),
            ("string.escape", org),
            ("tag", accent),
            ("title", err),
            ("type", warn),
            ("variable", text),
        ]
        .into_iter()
        .map(|(key, color)| (key.to_owned(), hex(color)))
        .collect(),
    }
}
