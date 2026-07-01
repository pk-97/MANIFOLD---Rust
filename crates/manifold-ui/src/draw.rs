//! Immediate-mode paint surface for the UI views that render *outside* the
//! `UITree` — the graph canvas and its mapping popover.
//!
//! Chrome panels describe a `UITree` that `manifold-renderer` walks and draws.
//! The graph canvas is immediate-mode by design (`docs/UI_ARCHITECTURE_OVERHAUL.md`
//! §5.4): it paints rects/lines/text directly each frame. Historically it called
//! `manifold_renderer::ui_renderer::UIRenderer` for that, which forced the canvas
//! to live app-side (a `manifold-ui` → `manifold-renderer` dependency is a cycle).
//!
//! [`Painter`] is the thin abstraction that breaks the cycle. The canvas paints
//! through `&mut dyn Painter`; `manifold-renderer` implements the trait for
//! `UIRenderer` (it already depends on `manifold-ui`). So the canvas is now a
//! pure UI component with no renderer dependency, and the renderer side is one
//! adapter `impl`. See `docs/CANVAS_API_DESIGN.md` §0 and Phase 8 of the
//! overhaul.

use crate::node::Color32;

/// Layering depth for immediate-mode draws. Mirror of
/// `manifold_renderer::ui_renderer::Depth` — the renderer's `Painter` impl maps
/// one to the other 1:1, so the same constants name the same layers on both
/// sides. Higher draws over lower; rects of a layer batch before its lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Depth(pub i32);

impl Depth {
    /// Wires, grid — the canvas backdrop.
    pub const BASE: Depth = Depth(0);
    /// Node bodies + on-face text, above the wires.
    pub const CONTENT: Depth = Depth(100);
    /// General overlay band.
    pub const OVERLAY: Depth = Depth(200);
    /// Floating popovers (the mapping editor) above the nodes.
    pub const POPOVER: Depth = Depth(300);
    /// Hover tooltips + the debug HUD, topmost.
    pub const TOOLTIP: Depth = Depth(400);
}

/// The immediate-mode draw primitives the graph canvas + mapping popover need.
///
/// Colours are passed as [`Color32`] — sRGB bytes, the app-wide colour currency.
/// The renderer adapter is the single place that converts sRGB → linear light
/// (`Color32::to_f32`) before the GPU write, so no call site ever hand-converts
/// or can pass an already-linear value by mistake. Object-safe — consumed as
/// `&mut dyn Painter`.
pub trait Painter {
    /// Solid rectangle.
    fn draw_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Color32);

    /// Rounded rectangle (no border).
    fn draw_rounded_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Color32, corner: f32);

    /// Rounded rectangle with a border.
    #[allow(clippy::too_many_arguments)]
    fn draw_bordered_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Color32,
        corner: f32,
        border_width: f32,
        border_color: Color32,
    );

    /// Oriented line segment of the given thickness.
    #[allow(clippy::too_many_arguments)]
    fn draw_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, thickness: f32, color: Color32);

    /// Text at a position. `color` is sRGB `[r, g, b, a]`.
    fn draw_text(&mut self, x: f32, y: f32, text: &str, font_size: f32, color: [u8; 4]);

    /// Push an immediate-mode scissor rect; nested draws are clipped to it
    /// (intersected with any outer clip) until [`Painter::pop_immediate_clip`].
    fn push_immediate_clip(&mut self, x: f32, y: f32, w: f32, h: f32);

    /// Pop the innermost immediate-mode clip.
    fn pop_immediate_clip(&mut self);

    /// Push a layering depth; subsequent draws sit at it until
    /// [`Painter::pop_depth`].
    fn push_depth(&mut self, depth: Depth);

    /// Pop the innermost depth.
    fn pop_depth(&mut self);
}

/// The bitmap font's advance width as a fraction of the em (font) size. One
/// estimate for every immediate-mode caller (the canvas, [`crate::slider::BitmapSlider::draw`])
/// that needs to measure or truncate text without a `UITree` on hand.
pub const CHAR_W_RATIO: f32 = 0.55;

/// Screen width (logical px) of `text` rendered at `font_size`, via the shared
/// [`CHAR_W_RATIO`] estimate.
pub fn text_width(text: &str, font_size: f32) -> f32 {
    text.chars().count() as f32 * font_size * CHAR_W_RATIO
}

/// Trim `text` to fit `budget_px` at `font_size`, appending an ellipsis when
/// it's clipped; returns the original (borrowed) when it already fits.
pub fn elide_to_width(text: &str, font_size: f32, budget_px: f32) -> std::borrow::Cow<'_, str> {
    let char_w = (font_size * CHAR_W_RATIO).max(0.01);
    let max_chars = (budget_px / char_w) as usize;
    if text.chars().count() > max_chars && max_chars > 1 {
        let take = max_chars.saturating_sub(1);
        std::borrow::Cow::Owned(format!(
            "{}…",
            text.chars().take(take).collect::<String>()
        ))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}
