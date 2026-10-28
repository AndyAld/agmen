//! A clip at an element's sides that lets glyphs taller than their line
//! show above and below it (C264a, C287).
//!
//! gpui's `overflow_hidden`, and so `truncate`, clips paint to the
//! element's box on both axes. A font whose ascent and descent outgrow a
//! fixed line height paints past that box, and the clip cut its glyphs.

use gpui::{
    AnyElement, App, Bounds, ContentMask, Element, ElementId, Font, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Window, point, px,
};

/// How far painted glyphs reach past their line boxes.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Bleed {
    pub(crate) above: Pixels,
    pub(crate) below: Pixels,
}

impl Bleed {
    /// The overflow of a line shaped with `ascent` and `descent` in a
    /// `line_height` box: ascent plus |descent| minus the line height,
    /// split where gpui's paint puts the baseline (half the spare height
    /// plus the ascent, as kv.rs's painted_baseline reads it). Zero on a
    /// side the glyphs do not reach past.
    #[must_use]
    pub(crate) fn of(ascent: Pixels, descent: Pixels, line_height: Pixels) -> Self {
        let baseline = (line_height - ascent - descent) / 2. + ascent;
        Self {
            above: (ascent - baseline).max(px(0.)),
            below: (baseline + descent.abs() - line_height).max(px(0.)),
        }
    }

    /// The overflow of a line of `font` at `size` in a `line_height` box,
    /// from the font's own ascent and descent.
    // shortcut: the font's metrics, not the shaped line's, so a fallback
    // font taller than `font` is still cut; shape the text if a label
    // shows such glyphs.
    #[must_use]
    pub(crate) fn of_font(font: &Font, size: Pixels, line_height: Pixels, window: &Window) -> Self {
        let text = window.text_system();
        let id = text.resolve_font(font);
        // CoreText gives the descent as negative (C259).
        Self::of(
            text.ascent(id, size),
            text.descent(id, size).abs(),
            line_height,
        )
    }
}

/// An element whose paint is clipped at its left and right edges. Above
/// and below it clips at its top and bottom grown by a [`Bleed`], or
/// leaves the clip to the element that holds it.
pub(crate) struct InkClip {
    child: AnyElement,
    /// `None` keeps the parent's clip above and below.
    bleed: Option<Bleed>,
}

impl InkClip {
    /// Clips `child` at its sides, and at its top and bottom grown by
    /// `bleed`, so its glyphs show and paint no further.
    #[must_use]
    pub(crate) fn new(child: impl IntoElement, bleed: Bleed) -> Self {
        Self {
            child: child.into_any_element(),
            bleed: Some(bleed),
        }
    }

    /// Clips `child` at its sides only, for a part that holds a label
    /// clipped with [`InkClip::new`]: a clip at its own top and bottom
    /// would cut what the label lets show.
    #[must_use]
    pub(crate) fn sides(child: impl IntoElement) -> Self {
        Self {
            child: child.into_any_element(),
            bleed: None,
        }
    }

    fn mask(&self, bounds: Bounds<Pixels>, window: &Window) -> ContentMask<Pixels> {
        let (top, bottom) = match self.bleed {
            Some(bleed) => (bounds.top() - bleed.above, bounds.bottom() + bleed.below),
            None => {
                let parent = window.content_mask().bounds;
                (parent.top(), parent.bottom())
            }
        };
        // gpui intersects this with the parent's mask.
        ContentMask {
            bounds: Bounds::from_corners(point(bounds.left(), top), point(bounds.right(), bottom)),
        }
    }
}

impl IntoElement for InkClip {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for InkClip {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // The child's own node, so its flex and size styles lay it out.
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let mask = self.mask(bounds, window);
        window.with_content_mask(Some(mask), |window| {
            // The focus handle is the child's to track; this adds none.
            drop(self.child.prepaint(window, cx));
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut Self::RequestLayoutState,
        (): &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mask = self.mask(bounds, window);
        window.with_content_mask(Some(mask), |window| self.child.paint(window, cx));
    }
}
