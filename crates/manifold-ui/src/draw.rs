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
/// Colours are passed as concrete arrays (the canvas already works in
/// `[f32; 4]` linear RGBA for fills and `[u8; 4]` sRGB for text); the renderer
/// adapter converts at the boundary. Object-safe — consumed as `&mut dyn
/// Painter`.
pub trait Painter {
    /// Solid rectangle.
    fn draw_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]);

    /// Rounded rectangle (no border).
    fn draw_rounded_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4], corner: f32);

    /// Rounded rectangle with a border.
    #[allow(clippy::too_many_arguments)]
    fn draw_bordered_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: [f32; 4],
        corner: f32,
        border_width: f32,
        border_color: [f32; 4],
    );

    /// Oriented line segment of the given thickness.
    #[allow(clippy::too_many_arguments)]
    fn draw_line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, thickness: f32, color: [f32; 4]);

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
