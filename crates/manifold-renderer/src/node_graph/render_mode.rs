//! `RenderMode` — port-data type carried on
//! [`PortType::RenderMode`](crate::node_graph::ports::PortType::RenderMode) wires.
//!
//! Scene-wide viewport shading mode (Blender's viewport shading modes as a
//! performable scene modifier — `docs/SCENE_RENDER_MODE_DESIGN.md`). CPU-only
//! wire value, same lifetime model as
//! [`Atmosphere`](crate::node_graph::atmosphere::Atmosphere): no GPU resource
//! on the wire; `render_scene` folds the scalars into its raster pass (fill
//! mode + a synthesized unlit material). Unwired =
//! [`RenderMode::default`] = `mode: Rendered` = **byte-identical to no
//! `render_mode` input at all** — the zero-cost contract, same as fog.
//!
//! Inert-member precedent: [`Material`](crate::node_graph::material::Material)
//! ("metallic is unread when kind = Phong"). One struct serves all modes;
//! per-mode params that don't apply are simply unread.

/// Scene-wide render mode: 0 = Rendered, 1 = Solid, 2 = Wireframe,
/// 3 = Points (`docs/SCENE_RENDER_MODE_DESIGN.md` D2).
pub const RENDER_MODE_RENDERED: u32 = 0;
/// Scene-wide render mode: 0 = Rendered, 1 = Solid, 2 = Wireframe, 3 = Points.
pub const RENDER_MODE_SOLID: u32 = 1;
/// Scene-wide render mode: 0 = Rendered, 1 = Solid, 2 = Wireframe, 3 = Points.
pub const RENDER_MODE_WIREFRAME: u32 = 2;
/// Scene-wide render mode: 0 = Rendered, 1 = Solid, 2 = Wireframe, 3 = Points.
pub const RENDER_MODE_POINTS: u32 = 3;

/// Enum-label table for `mode` — `RENDER_MODE_LABELS[mode as usize]` is the
/// human name. Index IS the wire value; INV-R2 (Rendered = index 0 forever)
/// is enforced by the const assertion below and the atom tests.
pub const RENDER_MODE_LABELS: &[&str] = &["Rendered", "Solid", "Wireframe", "Points"];

// INV-R2: Rendered must stay mode 0 — the enable gate multiplies
// (`enabled × mode`, D5), so a shifted index would make `enabled = 0`
// resolve to a non-Rendered mode instead of Rendered.
const fn str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}
const _: () = assert!(str_eq(RENDER_MODE_LABELS[RENDER_MODE_RENDERED as usize], "Rendered"));

/// Scene-wide render mode (viewport shading mode). CPU-only wire value
/// (`PortType::RenderMode`). `mode == 0` (Rendered) means "render exactly as
/// today"; consumers treat the unwired default as byte-identical to having no
/// `render_mode` input at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderMode {
    /// 0 = Rendered, 1 = Solid, 2 = Wireframe, 3 = Points.
    /// RENDERED MUST STAY 0 — the enable gate multiplies (INV-R2).
    pub mode: u32,
    /// Solid: flat clay color (rgb; a reserved). Inert in other modes.
    pub clay_color: [f32; 4],
    /// Wireframe/Points: line/point color (rgb; a reserved). Inert in Rendered/Solid.
    pub line_color: [f32; 4],
    /// Wireframe/Points: brightness gain, `[0, 4]`, default 1. Inert elsewhere.
    pub line_brightness: f32,
    /// Points: point size in px, `[1, 16]`, default 2. Inert elsewhere.
    pub point_size: f32,
}

impl Default for RenderMode {
    /// Unwired default = **Rendered**: mode 0, inert colors, brightness 1,
    /// point size 2. Consumers treat this as byte-identical to having no
    /// `render_mode` input at all.
    fn default() -> Self {
        Self {
            mode: RENDER_MODE_RENDERED,
            clay_color: [0.8, 0.8, 0.8, 1.0],
            line_color: [0.1, 0.9, 1.0, 1.0],
            line_brightness: 1.0,
            point_size: 2.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_rendered() {
        let m = RenderMode::default();
        assert_eq!(m.mode, RENDER_MODE_RENDERED, "unwired default must be Rendered (INV-R1)");
        assert_eq!(m.line_brightness, 1.0);
        assert_eq!(m.point_size, 2.0);
    }

    #[test]
    fn rendered_is_index_zero() {
        assert_eq!(RENDER_MODE_LABELS[0], "Rendered");
        assert_eq!(RENDER_MODE_LABELS[1], "Solid");
        assert_eq!(RENDER_MODE_LABELS[2], "Wireframe");
        assert_eq!(RENDER_MODE_LABELS[3], "Points");
    }

    #[test]
    fn render_mode_is_copy() {
        let m = RenderMode::default();
        let _b = m;
        let _c = m;
    }
}
