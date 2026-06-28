use bitflags::bitflags;

// ── Geometry ──────────────────────────────────────────────────────────

/// 2D point/vector — top-left origin, Y grows downward.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const ZERO: Vec2 = Vec2 { x: 0.0, y: 0.0 };

    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    pub fn distance(self, other: Vec2) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }
}

impl std::ops::Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x - rhs.x, self.y - rhs.y)
    }
}

impl std::ops::Add for Vec2 {
    type Output = Vec2;
    fn add(self, rhs: Vec2) -> Vec2 {
        Vec2::new(self.x + rhs.x, self.y + rhs.y)
    }
}

/// Axis-aligned rectangle — top-left origin.
/// Matches Unity's Rect(x, y, width, height) semantics.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub const ZERO: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 0.0,
        height: 0.0,
    };

    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub fn x_max(&self) -> f32 {
        self.x + self.width
    }

    pub fn y_max(&self) -> f32 {
        self.y + self.height
    }

    pub fn contains(&self, pos: Vec2) -> bool {
        // Two half-open spans — the shared hit-test primitive. A point on the
        // right/bottom edge belongs to the abutting rect, not this one.
        use crate::hit::Span;
        Span::new(self.x, self.x_max()).contains(pos.x)
            && Span::new(self.y, self.y_max()).contains(pos.y)
    }

    /// Offset the rect's Y position.
    pub fn offset_y(self, dy: f32) -> Self {
        Self {
            y: self.y + dy,
            ..self
        }
    }
}

// ── Color32 ──────────────────────────────────────────────────────────

/// 8-bit RGBA color — matches Unity's Color32.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color32 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color32 {
    pub const TRANSPARENT: Color32 = Color32 {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
    pub const WHITE: Color32 = Color32 {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    pub const BLACK: Color32 = Color32 {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Convert a Unity-style float Color(r,g,b,a) in [0,1] to Color32.
    pub fn from_f32(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self {
            r: (r * 255.0 + 0.5) as u8,
            g: (g * 255.0 + 0.5) as u8,
            b: (b * 255.0 + 0.5) as u8,
            a: (a * 255.0 + 0.5) as u8,
        }
    }

    /// Convert sRGB byte values to linear float RGBA for GPU rendering.
    ///
    /// The UI colors are specified in sRGB space (matching Unity's UGUI).
    /// Since the surface uses an sRGB format (Bgra8UnormSrgb), the GPU
    /// automatically applies gamma encoding when writing. We must convert
    /// to linear space here so the final displayed color matches the
    /// original sRGB byte values.
    ///
    /// Without this conversion, colors appear ~3x brighter because the
    /// sRGB values get gamma-encoded a second time.
    pub fn to_f32(self) -> [f32; 4] {
        [
            srgb_to_linear(self.r as f32 / 255.0),
            srgb_to_linear(self.g as f32 / 255.0),
            srgb_to_linear(self.b as f32 / 255.0),
            self.a as f32 / 255.0, // Alpha is always linear
        ]
    }
}

/// Color parameter for the UI geometry pipeline (rects, lines): linear-space
/// RGBA floats. Accepts raw `[f32; 4]` (already linear, passed through
/// unchanged) or a [`Color32`] (sRGB bytes, converted via
/// [`Color32::to_f32`]) — so palette constants work directly at draw calls.
///
/// Deliberately distinct from [`TextColor`]: the text pipeline consumes sRGB
/// bytes end-to-end, so a single color type would have to re-encode one of
/// the two paths and shift rendered output.
#[derive(Clone, Copy, Debug)]
pub struct LinearColor(pub [f32; 4]);

impl From<[f32; 4]> for LinearColor {
    fn from(rgba: [f32; 4]) -> Self {
        Self(rgba)
    }
}

impl From<Color32> for LinearColor {
    fn from(c: Color32) -> Self {
        Self(c.to_f32())
    }
}

/// Color parameter for the text/icon pipeline: sRGB RGBA bytes. Accepts raw
/// `[u8; 4]` or a [`Color32`] unchanged.
#[derive(Clone, Copy, Debug)]
pub struct TextColor(pub [u8; 4]);

impl From<[u8; 4]> for TextColor {
    fn from(rgba: [u8; 4]) -> Self {
        Self(rgba)
    }
}

impl From<Color32> for TextColor {
    fn from(c: Color32) -> Self {
        Self([c.r, c.g, c.b, c.a])
    }
}

/// Convert an sRGB component (0.0-1.0) to linear light.
/// Uses the standard sRGB transfer function (IEC 61966-2-1).
fn srgb_to_linear(s: f32) -> f32 {
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

impl Default for Color32 {
    fn default() -> Self {
        Self::TRANSPARENT
    }
}

// ── Node identity ────────────────────────────────────────────────────

/// Stable identity of a node in [`crate::tree::UITree`] — an array index plus a
/// generation.
///
/// The `index` locates the node in the tree's SoA storage; the `generation`
/// stamps *which* node has lived at that slot. The tree bumps a counter on every
/// node it mints, so a slot reused after a `truncate_from`/`clear`+rebuild gets a
/// fresh generation. An id minted against the old occupant therefore no longer
/// matches the slot's current generation, and the tree's accessors treat it as
/// "no such node" (read → zero/false/None, write → no-op) instead of silently
/// touching whatever node now sits at that index. This is the storage-layer
/// backstop for the stale-id bug class: a panel that keeps an id across a rebuild
/// and forgets to re-capture it can no longer mutate the wrong node.
///
/// Generation `0` is reserved: the tree never mints it (its counter starts at 1),
/// so any id with generation 0 — including [`NodeId::PLACEHOLDER`] — never matches
/// a live node. That makes a stray placeholder safe by construction.
///
/// `Option<NodeId>` remains the one "no node" type for fields; `PLACEHOLDER` is
/// only for the rare non-`Option` slot that needs an inert default.
///
/// Layout: `index` in the low 32 bits, `generation` in the high 32 bits of one
/// `u64`, so the id stays a cheap `Copy` scalar with derived `Eq`/`Hash`/`Ord`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u64);

impl NodeId {
    /// An id that matches no live node (generation 0 is never minted). For the
    /// rare non-`Option` field that needs an inert default; prefer `Option<NodeId>`.
    pub const PLACEHOLDER: NodeId = NodeId(0);

    /// Mint an id from its parts. Crate-private: only [`crate::tree::UITree`]
    /// assigns generations, so ids can't be forged elsewhere (external code that
    /// needs an id for an index goes through [`crate::tree::UITree::id_at`]).
    #[inline]
    pub(crate) const fn from_parts(index: u32, generation: u32) -> Self {
        NodeId(((generation as u64) << 32) | index as u64)
    }

    /// Array index of this node in the tree's SoA storage.
    #[inline]
    pub fn index(self) -> usize {
        (self.0 & 0xFFFF_FFFF) as usize
    }

    /// Generation stamp — which occupant of the slot this id refers to. `0` means
    /// "no live node" (placeholder / never minted).
    #[inline]
    pub fn generation(self) -> u32 {
        (self.0 >> 32) as u32
    }
}

/// A *durable* widget identity — stable across tree rebuilds, unlike [`NodeId`].
///
/// `NodeId` is a transient per-frame handle: generational, correct for rendering
/// and mutation within a build, but an id minted in one build does not validate
/// against a later build (that is the whole point — it makes stale ids inert).
/// The graph editor rebuilds its entire tree every frame, so any id held *across*
/// frames — the input system's pressed / hovered / focused targets — goes stale
/// immediately. Comparing a held `NodeId` to a freshly-built one then always
/// fails, which is the bug class this type removes.
///
/// `WidgetId` is derived from a node's place in the build: the parent's
/// `WidgetId` mixed with a per-sibling *salt*. So the **same logical widget gets
/// the same `WidgetId` on every rebuild** as long as the build is structurally
/// stable — which the editor's deterministic per-frame rebuild always is.
/// Identity-bearing widgets can override the auto salt with an explicit key (the
/// `*_keyed` builders), so their identity also survives sibling reordering (e.g.
/// arming a modulator on one row must not renumber another row's controls).
///
/// The input system tracks interaction by `WidgetId` and resolves to the live
/// `NodeId` only at the moment it emits an event. So it never depends on whether
/// the tree it serves is dirty-tracked (the timeline) or rebuilt every frame (the
/// editor) — both satisfy one neutral identity contract. See
/// `docs/INPUT_IDENTITY_UNIFICATION.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WidgetId(u64);

impl WidgetId {
    /// Seed for root-level widgets. Non-zero so a real node never derives to
    /// [`WidgetId::NONE`] in practice.
    pub const ROOT: WidgetId = WidgetId(0x9E37_79B9_7F4A_7C15);

    /// The "no widget" sentinel — a node that carries no durable identity (or a
    /// resolution miss). Never produced by [`with`](WidgetId::with) from a real
    /// seed in practice.
    pub const NONE: WidgetId = WidgetId(0);

    /// Derive a child id by mixing this id with `salt` (a stable sibling
    /// discriminator: the sibling index for auto ids, or an explicit key). The
    /// salt is mixed through a splitmix64 finalizer, so sibling 0 / 1 / 2 land far
    /// apart and a deep path never degenerates into clustering.
    #[inline]
    pub(crate) fn with(self, salt: u64) -> WidgetId {
        let mut z = self
            .0
            .wrapping_add(salt)
            .wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        WidgetId(z ^ (z >> 31))
    }

    /// The raw 64-bit value — for debugging / stable serialization only. Identity
    /// comparisons should use `WidgetId` directly.
    #[inline]
    pub fn raw(self) -> u64 {
        self.0
    }
}

// ── Node types ──────────────────────────────────────────────────────

/// Node type — determines default rendering behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UINodeType {
    Panel,
    Button,
    Label,
    Slider,
    Toggle,
    Image,
    ClipRegion,
    Custom,
}

// ── Flags ────────────────────────────────────────────────────────────

bitflags! {
    /// Per-node state flags (matches Unity UIFlags).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UIFlags: u16 {
        const VISIBLE        = 1 << 0;
        const INTERACTIVE    = 1 << 1;
        const DIRTY          = 1 << 2;
        const FOCUSED        = 1 << 3;
        const HOVERED        = 1 << 4;
        const PRESSED        = 1 << 5;
        const DISABLED       = 1 << 6;
        const CLIPS_CHILDREN = 1 << 7;
    }
}

// ── Text alignment / weight ──────────────────────────────────────────

/// Horizontal text alignment within a node's bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// Font weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontWeight {
    Regular,
    #[default]
    Medium,
    Bold,
}

// ── UIStyle ──────────────────────────────────────────────────────────

/// Visual style for a UI node.
/// Hover/pressed bg colors with a=0 mean "inherit from BackgroundColor".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UIStyle {
    pub bg_color: Color32,
    pub hover_bg_color: Color32,
    pub pressed_bg_color: Color32,
    pub text_color: Color32,
    pub border_color: Color32,
    pub corner_radius: f32,
    pub border_width: f32,
    pub font_size: u16,
    pub font_weight: FontWeight,
    pub text_align: TextAlign,
    /// When true, the renderer paints a dim dropdown caret (▼) pinned to the
    /// node's right edge, independent of (and after) the main text. Lets a value
    /// chip read as "opens a list" — the mockup's `.sel::after` — without baking a
    /// glyph into the text string (which would left-align with the value and sit
    /// at full weight). The main value text stays left-aligned and ellipsis-free.
    pub dropdown_caret: bool,
    /// Optional dim label painted BEFORE the (left-aligned) main text, with the
    /// value shifted right past it — the mockup's `.blend <b>BLEND</b> Normal`
    /// label/value chip. Static because it's a fixed control name (BLEND, GAIN);
    /// the value is the node's own text. Painted in `prefix_color`; ignored for
    /// non-left alignment.
    pub prefix_label: Option<&'static str>,
    pub prefix_color: Color32,
    /// Horizontal inset (px) for the node's text from the leading/trailing edge —
    /// the chip's internal padding (mockup `.sel{padding:2px 7px}`). Applied to
    /// Left-aligned text (and the prefix) from the left edge, and to Right-aligned
    /// text from the right edge; Centre is unaffected. 0 = text flush to the edge.
    pub text_inset_x: f32,
}

impl Default for UIStyle {
    fn default() -> Self {
        Self {
            bg_color: Color32::TRANSPARENT,
            hover_bg_color: Color32::TRANSPARENT,
            pressed_bg_color: Color32::TRANSPARENT,
            text_color: Color32::new(224, 224, 224, 255),
            border_color: Color32::TRANSPARENT,
            corner_radius: 0.0,
            border_width: 0.0,
            font_size: crate::color::FONT_HEADING,
            font_weight: crate::color::FONT_WEIGHT_DEFAULT,
            text_align: TextAlign::Left,
            dropdown_caret: false,
            prefix_label: None,
            prefix_color: Color32::TRANSPARENT,
            text_inset_x: 0.0,
        }
    }
}

// ── UINode ──────────────────────────────────────────────────────────

/// Opaque texture handle for image nodes.
/// Matches Unity UINode.Texture field — the renderer resolves this to a GPU texture.
pub type TextureHandle = u64;

/// A single node in the UI tree.
///
/// Invariant: `id` == array index in `UITree.nodes`.
pub struct UINode {
    pub id: NodeId,
    /// Parent node, or `None` for a root-level node (formerly `parent_id == -1`).
    pub parent_id: Option<NodeId>,
    pub bounds: Rect,
    pub node_type: UINodeType,
    pub flags: UIFlags,
    pub style: UIStyle,
    pub text: Option<String>,
    /// Optional texture for Image nodes (thumbnails, icons).
    /// Port of Unity UINode.Texture field.
    pub texture: Option<TextureHandle>,
    pub draw_order: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_contains() {
        let r = Rect::new(10.0, 20.0, 100.0, 50.0);
        assert!(r.contains(Vec2::new(10.0, 20.0)));
        assert!(r.contains(Vec2::new(50.0, 40.0)));
        assert!(!r.contains(Vec2::new(110.0, 20.0))); // x_max exclusive
        assert!(!r.contains(Vec2::new(10.0, 70.0))); // y_max exclusive
        assert!(!r.contains(Vec2::new(9.0, 20.0)));
    }

    #[test]
    fn color32_conversions() {
        let c = Color32::from_f32(1.0, 0.5, 0.0, 0.75);
        assert_eq!(c.r, 255);
        assert_eq!(c.g, 128);
        assert_eq!(c.b, 0);
        assert_eq!(c.a, 191);

        // to_f32() returns sRGB-to-linear values (for GPU rendering).
        // 128/255 ≈ 0.502 sRGB → ~0.214 linear via IEC 61966-2-1.
        let f = Color32::new(255, 128, 0, 191).to_f32();
        assert!((f[0] - 1.0).abs() < 0.01, "red: {}", f[0]);
        assert!((f[1] - 0.214).abs() < 0.02, "green (linear): {}", f[1]);
        assert!((f[2] - 0.0).abs() < 0.01, "blue: {}", f[2]);
        assert!((f[3] - 0.749).abs() < 0.01, "alpha (linear): {}", f[3]);
    }

    #[test]
    fn vec2_distance() {
        let a = Vec2::new(0.0, 0.0);
        let b = Vec2::new(3.0, 4.0);
        assert!((a.distance(b) - 5.0).abs() < 0.001);
    }

    #[test]
    fn style_default() {
        let s = UIStyle::default();
        assert_eq!(s.bg_color, Color32::TRANSPARENT);
        assert_eq!(s.text_color, Color32::new(224, 224, 224, 255));
        assert_eq!(s.font_size, 14);
    }
}
