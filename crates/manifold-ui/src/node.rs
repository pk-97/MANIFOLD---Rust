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
    pub const ZERO: Rect = Rect { x: 0.0, y: 0.0, width: 0.0, height: 0.0 };

    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }

    pub fn x_max(&self) -> f32 {
        self.x + self.width
    }

    pub fn y_max(&self) -> f32 {
        self.y + self.height
    }

    pub fn contains(&self, pos: Vec2) -> bool {
        pos.x >= self.x && pos.x < self.x_max() && pos.y >= self.y && pos.y < self.y_max()
    }

    /// Offset the rect's Y position.
    pub fn offset_y(self, dy: f32) -> Self {
        Self { y: self.y + dy, ..self }
    }
}

// ── Color32 ──────────────────────────────────────────────────────────

/// 8-bit RGBA color — matches Unity's Color32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color32 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color32 {
    pub const TRANSPARENT: Color32 = Color32 { r: 0, g: 0, b: 0, a: 0 };
    pub const WHITE: Color32 = Color32 { r: 255, g: 255, b: 255, a: 255 };
    pub const BLACK: Color32 = Color32 { r: 0, g: 0, b: 0, a: 255 };

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FontWeight {
    #[default]
    Regular,
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
            font_size: 14,
            font_weight: FontWeight::Regular,
            text_align: TextAlign::Left,
        }
    }
}

// ── UINode ──────────────────────────────────────────────────────────

/// A single node in the UI tree.
///
/// Invariant: `id` == array index in `UITree.nodes`.
pub struct UINode {
    pub id: u32,
    pub parent_id: i32,
    pub bounds: Rect,
    pub node_type: UINodeType,
    pub flags: UIFlags,
    pub style: UIStyle,
    pub text: Option<String>,
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

        let f = Color32::new(255, 128, 0, 191).to_f32();
        assert!((f[0] - 1.0).abs() < 0.01);
        assert!((f[1] - 0.502).abs() < 0.01);
        assert!((f[2] - 0.0).abs() < 0.01);
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
