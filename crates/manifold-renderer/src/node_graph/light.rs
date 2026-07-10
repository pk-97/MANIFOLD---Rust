//! `Light` — port-data type carried on [`PortType::Light`](crate::node_graph::ports::PortType::Light) wires.
//!
//! One `Light` source primitive (`node.light`, with `mode` enum picking Sun
//! or Point) emits a fully-populated struct each frame; downstream consumers
//! — shading atoms ([`lambert_directional`], [`blinn_specular`], …)
//! and shadow-aware mesh renderers — take it as a single `light: Light`
//! input instead of scattered `light_x/y/z/intensity` scalars.
//!
//! Like [`Camera`](crate::node_graph::camera::Camera), this is plain CPU
//! data — no GPU resource. Backends carry it through the same
//! `(Slot → value)` map shape that scalars and cameras use; the executor
//! drains `pending_light_writes` after each node's `evaluate` returns,
//! parallel to the scalar and camera drains.
//!
//! Two modes, distinguished by [`LightMode`]:
//!
//! - **Sun** — parallel rays from a directional source. `pos` anchors the
//!   shadow ortho frustum (the shadow pass needs a camera origin); the
//!   actual lighting direction is `normalize(aim - pos)`. `range` is the
//!   ortho half-extent (default 30.0).
//! - **Point** — omnidirectional source at `pos`. `aim - pos` gives the
//!   shadow camera's forward direction (single-cubemap-face approximation
//!   for v1; full cubemap shadows are a v2 ask). `range` is the
//!   attenuation half-distance: `intensity = 1 / (1 + d²/range²)` →
//!   intensity is 50% at `d = range`.
//!
//! Colour is stored premultiplied with intensity (rgb × intensity) so the
//! consumer-side shading math is one multiply lighter. Outer-card param
//! `intensity` on `node.light` is applied at emission; downstream
//! reads see the already-multiplied colour.
//!
//! Shadow params (`cast_shadows`, `shadow_softness`, `shadow_bias`,
//! `shadow_resolution`) are always present on the struct; `cast_shadows ==
//! false` means renderers skip the depth pass entirely.

use crate::generators::mesh_pipeline::{look_at_rh, mat4_mul, ortho_rh, perspective_rh};

/// Discriminator for the light's geometric kind. Both modes share the same
/// `pos` / `aim` / `colour` / `range` / shadow fields on [`Light`] — only
/// the interpretation of `pos` and `range` differs, and the consumer-side
/// math dispatches on this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LightMode {
    /// Parallel-ray directional light (sun-style). `pos` anchors the shadow
    /// ortho frustum; `aim` defines what the sun illuminates; `range` is the
    /// ortho half-extent (how big an area is lit).
    Sun,
    /// Omnidirectional point light. `pos` is the light source; `aim` gives
    /// the shadow camera's forward direction; `range` is the attenuation
    /// half-distance.
    Point,
}

/// Stepped PCF kernel size — the user-facing softness knob on `node.light`.
/// Three discrete levels matching the variable-tap-count decision in the
/// design audit (the `Hard` 3×3, `Soft` 5×5, `VerySoft` 7×7 PCF taps).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShadowSoftness {
    /// 3×3 PCF kernel (9 taps). Sharp shadow edges.
    Hard,
    /// 5×5 PCF kernel (25 taps). Default; matches the perceptual softness
    /// the legacy DigitalPlants shader's 5-tap cross was reaching for.
    Soft,
    /// 7×7 PCF kernel (49 taps). Very soft shadows; visible noise on
    /// thin features.
    VerySoft,
}

impl ShadowSoftness {
    /// PCF kernel half-width — the loop bound for the (-N..=N, -N..=N)
    /// sample pattern. Hard → 1 (3×3), Soft → 2 (5×5), VerySoft → 3 (7×7).
    pub fn kernel_half_width(self) -> i32 {
        match self {
            Self::Hard => 1,
            Self::Soft => 2,
            Self::VerySoft => 3,
        }
    }

    /// Total tap count = (2N+1)². Useful for the divisor when averaging
    /// PCF results.
    pub fn tap_count(self) -> u32 {
        let n = self.kernel_half_width() as u32;
        let k = 2 * n + 1;
        k * k
    }
}

/// Light struct flowing through [`PortType::Light`](crate::node_graph::ports::PortType::Light)
/// wires. Built once per frame in `node.light::run()`; passed by value to
/// every downstream consumer.
///
/// ~80 bytes — trivially cheap to clone per wire per frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Light {
    /// Sun vs Point dispatch.
    pub mode: LightMode,
    /// World-space position. Sun: shadow-frustum anchor. Point: light source.
    pub pos: [f32; 3],
    /// World-space point the light aims at. Direction derived as
    /// `normalize(aim - pos)`; consumers should use [`Self::dir`] which
    /// caches that normalisation.
    pub aim: [f32; 3],
    /// Pre-normalised forward direction (`normalize(aim - pos)`). Cached at
    /// build time so per-frame consumers don't redo the normalise.
    pub dir: [f32; 3],
    /// Linear RGB colour PREMULTIPLIED with intensity. A consumer reading
    /// `color.rgb` gets the final emission directly — no second intensity
    /// multiply needed. `color.a` is reserved (currently 1.0).
    pub color: [f32; 4],
    /// Sun: shadow ortho half-extent (world units). Point: attenuation
    /// half-distance (world units, `intensity = 1 / (1 + d²/range²)`).
    pub range: f32,
    /// Whether this light casts shadows. When `false`, the renderer skips
    /// the depth pass and downstream PCF math entirely — pure lighting.
    pub cast_shadows: bool,
    /// PCF kernel size. Hard / Soft / VerySoft.
    pub shadow_softness: ShadowSoftness,
    /// Depth bias added to the comparison sample (light NDC.z units).
    /// Matches the legacy DigitalPlants `0.003` default.
    pub shadow_bias: f32,
    /// Shadow map edge resolution (power of two; default 2048).
    pub shadow_resolution: u32,
}

impl Light {
    /// Identity-ish default — a downward sun at origin with white colour
    /// and shadows disabled. Provided so renderers can fall back to a
    /// sane no-op when nothing is wired (in practice they'll prefer their
    /// scattered `light_x/y/z` params via the no-light path).
    pub fn default_sun() -> Self {
        let pos = [0.0, 30.0, 0.0];
        let aim = [0.0, 0.0, 0.0];
        let dir = normalize3(sub3(aim, pos));
        Self {
            mode: LightMode::Sun,
            pos,
            aim,
            dir,
            color: [1.0, 1.0, 1.0, 1.0],
            range: 30.0,
            cast_shadows: false,
            shadow_softness: ShadowSoftness::Soft,
            shadow_bias: 0.003,
            shadow_resolution: 2048,
        }
    }

    /// Build a Sun light from the standard outer-card surface.
    /// `color_rgb` is the un-multiplied colour; this function premultiplies
    /// `intensity` into the stored `color`.
    #[allow(clippy::too_many_arguments)]
    pub fn sun(
        pos: [f32; 3],
        aim: [f32; 3],
        color_rgb: [f32; 3],
        intensity: f32,
        range: f32,
        cast_shadows: bool,
        shadow_softness: ShadowSoftness,
        shadow_bias: f32,
        shadow_resolution: u32,
    ) -> Self {
        let dir = normalize3(sub3(aim, pos));
        Self {
            mode: LightMode::Sun,
            pos,
            aim,
            dir,
            color: [
                color_rgb[0] * intensity,
                color_rgb[1] * intensity,
                color_rgb[2] * intensity,
                1.0,
            ],
            range,
            cast_shadows,
            shadow_softness,
            shadow_bias,
            shadow_resolution,
        }
    }

    /// Build a Point light from the standard outer-card surface.
    /// `range` here is the attenuation half-distance (intensity reaches 0.5
    /// at `d = range`).
    #[allow(clippy::too_many_arguments)]
    pub fn point(
        pos: [f32; 3],
        aim: [f32; 3],
        color_rgb: [f32; 3],
        intensity: f32,
        range: f32,
        cast_shadows: bool,
        shadow_softness: ShadowSoftness,
        shadow_bias: f32,
        shadow_resolution: u32,
    ) -> Self {
        let dir = normalize3(sub3(aim, pos));
        Self {
            mode: LightMode::Point,
            pos,
            aim,
            dir,
            color: [
                color_rgb[0] * intensity,
                color_rgb[1] * intensity,
                color_rgb[2] * intensity,
                1.0,
            ],
            range,
            cast_shadows,
            shadow_softness,
            shadow_bias,
            shadow_resolution,
        }
    }

    /// Unit vector FROM `world_pos` TOWARD the light — the canonical L
    /// vector in `max(dot(N, L), 0)` lighting math.
    ///
    /// Sun: `-self.dir` (parallel rays come from the opposite direction
    /// of the light's forward).
    /// Point: `normalize(self.pos - world_pos)`.
    pub fn light_dir_at(&self, world_pos: [f32; 3]) -> [f32; 3] {
        match self.mode {
            LightMode::Sun => [-self.dir[0], -self.dir[1], -self.dir[2]],
            LightMode::Point => normalize3(sub3(self.pos, world_pos)),
        }
    }

    /// Per-point attenuation factor in [0, 1].
    ///
    /// Sun: always 1.0 (parallel rays don't fall off).
    /// Point: `1 / (1 + d²/range²)`. Matches the well-behaved
    /// (non-divergent) inverse-square variant used by the legacy
    /// DigitalPlants shader and the mesh renderers' PBR path.
    pub fn attenuation_at(&self, world_pos: [f32; 3]) -> f32 {
        match self.mode {
            LightMode::Sun => 1.0,
            LightMode::Point => {
                let d = sub3(self.pos, world_pos);
                let d_sq = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                let r_sq = self.range * self.range;
                if r_sq < 1e-10 {
                    0.0
                } else {
                    1.0 / (1.0 + d_sq / r_sq)
                }
            }
        }
    }

    /// Light-space view matrix for the shadow pass. Built via right-handed
    /// look-at from `pos` toward `aim`, with a Y-up convention (degenerates
    /// to Z-up if the light is pointing exactly up/down — same fallback
    /// `look_at_rh` uses).
    ///
    /// View doesn't depend on aspect, so it's cheap to compute once per
    /// frame per consumer.
    pub fn shadow_view(&self) -> [[f32; 4]; 4] {
        let up = pick_up(self.dir);
        look_at_rh(self.pos, self.aim, up)
    }

    /// Light-space projection matrix for the shadow pass (right-handed,
    /// Metal depth [0,1]). The frustum is fitted to `range`, NOT a
    /// hard-coded near/far — inheriting a fixed span (e.g. DigitalPlants'
    /// `0.1..200`) either clips a large scene or wastes depth precision on
    /// a small one.
    ///
    /// - **Sun** → orthographic cube of half-extent `range` centred on
    ///   `aim`, viewed down the light axis from `pos`. Depth spans
    ///   `[d - range, d + range]` where `d = |pos - aim|`; X/Y span
    ///   `±range`. This is the tight fit for a `range`-sized scene sitting
    ///   around `aim`.
    /// - **Point** → single-face 90° perspective from `pos` toward `aim`
    ///   (the v1 approximation; full cubemap shadows are v2). `far` must
    ///   comfortably exceed `range` because attenuation is only 50% at
    ///   `d = range` — lit geometry extends well past it, so `far = 4·range`
    ///   (≈6% attenuation) keeps casters inside the frustum.
    pub fn shadow_proj(&self) -> [[f32; 4]; 4] {
        match self.mode {
            LightMode::Sun => {
                let d = dist3(self.pos, self.aim);
                let near = (d - self.range).max(0.05);
                let far = d + self.range;
                ortho_rh(
                    -self.range,
                    self.range,
                    -self.range,
                    self.range,
                    near,
                    far,
                )
            }
            LightMode::Point => {
                let near = (self.range * 0.05).max(0.1);
                let far = self.range * 4.0;
                perspective_rh(std::f32::consts::FRAC_PI_2, 1.0, near, far)
            }
        }
    }

    /// Composed light-space view-projection (`proj · view`) — the single
    /// matrix a shadow depth pass renders geometry through, and the one the
    /// main pass reconstructs a fragment's light-space position with for the
    /// PCF depth compare. `mat4_mul(proj, view)` produces `P · V` in the
    /// column-major `m[col][row]` convention the shaders use.
    pub fn shadow_view_proj(&self) -> [[f32; 4]; 4] {
        mat4_mul(self.shadow_proj(), self.shadow_view())
    }
}

fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dist3(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = sub3(a, b);
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-10 {
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

/// Pick a stable up vector for `look_at_rh`. World up (0, 1, 0) unless the
/// forward direction is nearly parallel to it (sun pointing straight down /
/// up), in which case fall back to world Z so the view matrix stays
/// non-degenerate.
fn pick_up(dir: [f32; 3]) -> [f32; 3] {
    if dir[1].abs() > 0.999 {
        [0.0, 0.0, 1.0]
    } else {
        [0.0, 1.0, 0.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() < eps
    }

    fn approx_vec3(a: [f32; 3], b: [f32; 3], eps: f32) -> bool {
        approx_eq(a[0], b[0], eps) && approx_eq(a[1], b[1], eps) && approx_eq(a[2], b[2], eps)
    }

    #[test]
    fn default_sun_populates_all_fields_with_shadows_off() {
        let l = Light::default_sun();
        assert_eq!(l.mode, LightMode::Sun);
        assert!(!l.cast_shadows);
        // Looking from (0, 30, 0) at origin → dir is straight down.
        assert!(approx_vec3(l.dir, [0.0, -1.0, 0.0], 1e-5));
        assert_eq!(l.color, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(l.range, 30.0);
        assert_eq!(l.shadow_softness, ShadowSoftness::Soft);
    }

    #[test]
    fn sun_premultiplies_intensity_into_color() {
        let l = Light::sun(
            [0.0, 10.0, 0.0],
            [0.0, 0.0, 0.0],
            [0.5, 0.4, 0.3],
            2.0,
            10.0,
            true,
            ShadowSoftness::Soft,
            0.003,
            2048,
        );
        assert!(approx_eq(l.color[0], 1.0, 1e-5));
        assert!(approx_eq(l.color[1], 0.8, 1e-5));
        assert!(approx_eq(l.color[2], 0.6, 1e-5));
        assert_eq!(l.color[3], 1.0);
    }

    #[test]
    fn point_premultiplies_intensity_into_color() {
        let l = Light::point(
            [1.0, 2.0, 3.0],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            0.5,
            25.0,
            false,
            ShadowSoftness::Hard,
            0.001,
            1024,
        );
        assert_eq!(l.color, [0.5, 0.5, 0.5, 1.0]);
        assert_eq!(l.mode, LightMode::Point);
    }

    #[test]
    fn sun_light_dir_is_negated_forward_everywhere() {
        // Sun is parallel rays: every world point sees the same L vector.
        let l = Light::sun(
            [10.0, 10.0, 10.0],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            1.0,
            30.0,
            false,
            ShadowSoftness::Soft,
            0.003,
            2048,
        );
        let l_at_a = l.light_dir_at([5.0, 5.0, 5.0]);
        let l_at_b = l.light_dir_at([-100.0, 0.0, 50.0]);
        assert!(approx_vec3(l_at_a, l_at_b, 1e-5));
        // And it points back toward the light (opposite of fwd).
        let expected = [-l.dir[0], -l.dir[1], -l.dir[2]];
        assert!(approx_vec3(l_at_a, expected, 1e-5));
    }

    #[test]
    fn point_light_dir_varies_by_world_pos() {
        let l = Light::point(
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            1.0,
            10.0,
            false,
            ShadowSoftness::Soft,
            0.003,
            2048,
        );
        // From (5, 0, 0), the light is back at origin → L = -x.
        let l_dir = l.light_dir_at([5.0, 0.0, 0.0]);
        assert!(approx_vec3(l_dir, [-1.0, 0.0, 0.0], 1e-5));
        // From (0, 5, 0), light is below → L = -y.
        let l_dir = l.light_dir_at([0.0, 5.0, 0.0]);
        assert!(approx_vec3(l_dir, [0.0, -1.0, 0.0], 1e-5));
    }

    #[test]
    fn sun_attenuation_is_unity_everywhere() {
        let l = Light::sun(
            [0.0, 10.0, 0.0],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            1.0,
            30.0,
            false,
            ShadowSoftness::Soft,
            0.003,
            2048,
        );
        assert_eq!(l.attenuation_at([0.0, 0.0, 0.0]), 1.0);
        assert_eq!(l.attenuation_at([100.0, 100.0, 100.0]), 1.0);
    }

    #[test]
    fn point_attenuation_is_half_at_range() {
        let l = Light::point(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, -1.0],
            [1.0, 1.0, 1.0],
            1.0,
            10.0,
            false,
            ShadowSoftness::Soft,
            0.003,
            2048,
        );
        // At distance = range, intensity should be 1/(1+1) = 0.5.
        let at_range = l.attenuation_at([10.0, 0.0, 0.0]);
        assert!(approx_eq(at_range, 0.5, 1e-5));
        // At distance 0 it should be 1.0.
        let at_zero = l.attenuation_at([0.0, 0.0, 0.0]);
        assert!(approx_eq(at_zero, 1.0, 1e-5));
        // At 2× range it should be 1/(1+4) = 0.2.
        let at_double = l.attenuation_at([20.0, 0.0, 0.0]);
        assert!(approx_eq(at_double, 0.2, 1e-5));
    }

    #[test]
    fn shadow_softness_kernel_sizes_match_design() {
        assert_eq!(ShadowSoftness::Hard.kernel_half_width(), 1);
        assert_eq!(ShadowSoftness::Hard.tap_count(), 9); // 3×3
        assert_eq!(ShadowSoftness::Soft.kernel_half_width(), 2);
        assert_eq!(ShadowSoftness::Soft.tap_count(), 25); // 5×5
        assert_eq!(ShadowSoftness::VerySoft.kernel_half_width(), 3);
        assert_eq!(ShadowSoftness::VerySoft.tap_count(), 49); // 7×7
    }

    #[test]
    fn shadow_view_is_orthonormal() {
        let l = Light::sun(
            [10.0, 20.0, 30.0],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            1.0,
            30.0,
            true,
            ShadowSoftness::Soft,
            0.003,
            2048,
        );
        let v = l.shadow_view();
        // The upper-left 3×3 of a view matrix is orthonormal — each row's
        // squared magnitude is 1, and each pair of rows is orthogonal.
        let row = |i: usize| [v[0][i], v[1][i], v[2][i]];
        for i in 0..3 {
            let r = row(i);
            let mag_sq = r[0] * r[0] + r[1] * r[1] + r[2] * r[2];
            assert!((mag_sq - 1.0).abs() < 1e-4, "row {i} not unit: {mag_sq}");
        }
    }

    /// Transform a world point through a column-major (`m[col][row]`)
    /// 4×4, returning clip-space `[x, y, z, w]`.
    fn transform_point(m: [[f32; 4]; 4], p: [f32; 3]) -> [f32; 4] {
        let v = [p[0], p[1], p[2], 1.0];
        let mut out = [0.0f32; 4];
        for row in 0..4 {
            for col in 0..4 {
                out[row] += m[col][row] * v[col];
            }
        }
        out
    }

    #[test]
    fn sun_shadow_proj_is_orthographic() {
        // Ortho preserves w=1 (no perspective divide): m[3][3] == 1,
        // m[2][3] == 0. This is the discriminator vs. a perspective proj.
        let l = Light::default_sun();
        let p = l.shadow_proj();
        assert!((p[3][3] - 1.0).abs() < 1e-6, "ortho m[3][3] should be 1");
        assert!(p[2][3].abs() < 1e-6, "ortho m[2][3] should be 0");
    }

    #[test]
    fn point_shadow_proj_is_perspective() {
        // Perspective writes -1 into m[2][3] so clip.w = -view.z. The
        // single-face point-shadow frustum must be perspective.
        let l = Light::point(
            [0.0, 5.0, 0.0],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            1.0,
            10.0,
            true,
            ShadowSoftness::Soft,
            0.003,
            2048,
        );
        let p = l.shadow_proj();
        assert!((p[2][3] + 1.0).abs() < 1e-6, "perspective m[2][3] should be -1");
    }

    #[test]
    fn sun_shadow_view_proj_centers_the_aim_point() {
        // `aim` sits on the light's view axis, so it must land at the
        // centre of the shadow map (clip x/y ≈ 0). Proves view and proj
        // compose in the right order and orientation.
        let l = Light::sun(
            [10.0, 20.0, 30.0],
            [1.0, 2.0, 3.0],
            [1.0, 1.0, 1.0],
            1.0,
            25.0,
            true,
            ShadowSoftness::Soft,
            0.003,
            2048,
        );
        let clip = transform_point(l.shadow_view_proj(), l.aim);
        assert!((clip[3] - 1.0).abs() < 1e-4, "ortho w should be 1, got {}", clip[3]);
        assert!((clip[0] / clip[3]).abs() < 1e-3, "aim not centred in x: {}", clip[0]);
        assert!((clip[1] / clip[3]).abs() < 1e-3, "aim not centred in y: {}", clip[1]);
    }

    #[test]
    fn sun_shadow_depth_increases_away_from_the_light() {
        // A caster nearer the sun must record a smaller shadow-map depth
        // than one farther, or occlusion resolves backwards. Sun at
        // (0,30,0) looking down: y=25 is nearer than y=5.
        let l = Light::default_sun();
        let near_pt = transform_point(l.shadow_view_proj(), [0.0, 25.0, 0.0]);
        let far_pt = transform_point(l.shadow_view_proj(), [0.0, 5.0, 0.0]);
        let near_depth = near_pt[2] / near_pt[3];
        let far_depth = far_pt[2] / far_pt[3];
        assert!(
            near_depth < far_depth,
            "nearer point should have smaller depth: near {near_depth} vs far {far_depth}"
        );
    }

    #[test]
    fn point_with_zero_range_attenuation_is_zero() {
        // Guard the divide — `range = 0` would otherwise blow up.
        let l = Light::point(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, -1.0],
            [1.0, 1.0, 1.0],
            1.0,
            0.0,
            false,
            ShadowSoftness::Soft,
            0.003,
            2048,
        );
        assert_eq!(l.attenuation_at([1.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn sun_pointing_straight_down_gets_non_degenerate_view() {
        // Sun pointing exactly along ±Y would degenerate the standard
        // (0,1,0) up vector — `pick_up` should fall back to Z.
        let l = Light::sun(
            [0.0, 50.0, 0.0],
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            1.0,
            30.0,
            true,
            ShadowSoftness::Soft,
            0.003,
            2048,
        );
        let v = l.shadow_view();
        // The view's "up" row (row 1, column 1) should not be zero.
        let row_up_mag = (v[0][1] * v[0][1] + v[1][1] * v[1][1] + v[2][1] * v[2][1]).sqrt();
        assert!(row_up_mag > 0.9, "view matrix degenerated: {v:?}");
    }
}
