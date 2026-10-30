//! `Light` — port-data type carried on [`PortType::Light`](crate::node_graph::ports::PortType::Light) wires.
//!
//! One `Light` source primitive (`node.light`, with `mode` enum picking Sun,
//! Point, or Spot) emits a fully-populated struct each frame; downstream consumers
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
//! Three modes, distinguished by [`LightMode`]:
//!
//! - **Sun** — parallel rays from a directional source. The lighting
//!   direction is `normalize(aim - pos)`. A renderer shadows it through
//!   [`Light::cascades`]: boxes that follow the camera out to
//!   [`SHADOW_DISTANCE`], each a Sun light whose `pos` anchors its ortho
//!   frustum and whose `range` is its half-extent.
//! - **Point** — omnidirectional source at `pos`. A casting point light
//!   shadows through six cube faces ([`Light::cube_faces`]), one shadow
//!   view each.
//! - **Spot** — point source with a finite cone from `pos` along `dir`.
//!
//! Colour is stored premultiplied with intensity (rgb × intensity) so the
//! consumer-side shading math is one multiply lighter. Outer-card param
//! `intensity` on `node.light` is applied at emission; downstream
//! reads see the already-multiplied colour.
//!
//! Shadow params (`cast_shadows`, `shadow_softness`, `shadow_bias`,
//! `shadow_resolution`) are always present on the struct; `cast_shadows ==
//! false` means renderers skip the depth pass entirely.

use crate::mesh::pipeline::{look_at_rh, mat4_mul, ortho_rh, perspective_rh};
use crate::scene::camera::{Camera, CameraMode, cross3, dot3};

/// The shadow maps a Sun casts through, nearest the camera first
/// (`SUN_CASCADES` in `scene_shading.wgsl` and `shaft_march.wgsl`).
pub const CASCADES: usize = 4;
/// How far from the camera a Sun's shadows reach, in metres.
pub const SHADOW_DISTANCE: f32 = 2000.0;
/// How far toward the sun past its box a caster still shadows a cascade:
/// as far as the cascades reach, so a peak a valley away shades its floor.
const CASCADE_REACH: f32 = SHADOW_DISTANCE;
/// The share of each split between cascades spaced evenly over the
/// distance; the rest is spaced evenly in ratio from `CASCADE_NEAR`
/// metres, which keeps the near cascades sharp.
const CASCADE_EVEN: f32 = 0.03;
const CASCADE_NEAR: f32 = 1.0;
/// A Sun's default Shadow Bias, and the depth it holds back at it, in each
/// cascade's own texels.
const SUN_BIAS: f32 = 0.003;
const BIAS_TEXELS: f32 = 1.5;

/// Discriminator for the light's geometric kind. All modes share the same
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
    /// the shadow camera's forward direction.
    Point,
    /// Point source restricted to a cone around `dir`.
    Spot,
}

/// Point/spot attenuation policy. `Legacy` preserves the saved-project
/// half-distance curve; `InverseSquare` is the glTF physical falloff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LightFalloff {
    Legacy,
    InverseSquare,
}

/// Stepped PCF kernel size — the user-facing softness knob on `node.light`.
/// Four levels: the fixed-kernel `Hard` 3×3 / `Soft` 5×5 / `VerySoft` 7×7
/// PCF taps from the original design audit, plus `Contact` — PCSS
/// contact-hardening penumbra (REALTIME_3D_DESIGN section 11, D12): a per-fragment
/// dynamic kernel width driven by a blocker search instead of a fixed tap
/// count, so shadows harden at contact and soften with occluder distance.
/// `light_size` carries the world-units light diameter that drives the
/// blocker-search radius and penumbra width; enum growth is additive, so
/// projects saved before this variant existed load unchanged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShadowSoftness {
    /// 3×3 PCF kernel (9 taps). Sharp shadow edges.
    Hard,
    /// 5×5 PCF kernel (25 taps). Default; matches the perceptual softness
    /// the legacy DigitalPlants shader's 5-tap cross was reaching for.
    Soft,
    /// 7×7 PCF kernel (49 taps). Very soft shadows; visible noise on
    /// thin features.
    VerySoft,
    /// PCSS contact-hardening penumbra (D12). `light_size` is the
    /// world-units light diameter driving both the blocker-search radius
    /// and the penumbra-width estimate; the render-side kernel width is
    /// computed per-fragment, not fixed.
    Contact { light_size: f32 },
}

impl ShadowSoftness {
    /// PCF kernel half-width — the loop bound for the (-N..=N, -N..=N)
    /// sample pattern. Hard → 1 (3×3), Soft → 2 (5×5), VerySoft → 3 (7×7).
    /// `Contact` has no fixed width (it's computed per-fragment in the
    /// shader from the PCSS penumbra estimate); this returns the sentinel
    /// `-1`, consumed only by `render_scene.rs`'s caster-table build to
    /// select the PCSS branch in `render_scene.wgsl`.
    pub fn kernel_half_width(self) -> i32 {
        match self {
            Self::Hard => 1,
            Self::Soft => 2,
            Self::VerySoft => 3,
            Self::Contact { .. } => -1,
        }
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
    /// Point/spot attenuation policy.
    pub falloff: LightFalloff,
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
    /// Sun: shadow ortho half-extent. Point/spot: attenuation cutoff/range.
    pub range: f32,
    /// Spot inner cone angle in radians.
    pub inner_cone_angle: f32,
    /// Spot outer cone angle in radians.
    pub outer_cone_angle: f32,
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
            falloff: LightFalloff::Legacy,
            pos,
            aim,
            dir,
            color: [1.0, 1.0, 1.0, 1.0],
            range: 30.0,
            inner_cone_angle: 0.0,
            outer_cone_angle: 0.0,
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
            falloff: LightFalloff::Legacy,
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
            inner_cone_angle: 0.0,
            outer_cone_angle: 0.0,
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
            falloff: LightFalloff::Legacy,
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
            inner_cone_angle: 0.0,
            outer_cone_angle: 0.0,
            cast_shadows,
            shadow_softness,
            shadow_bias,
            shadow_resolution,
        }
    }

    /// Build a physical spot light. Angles are in radians and are clamped to
    /// a finite, ordered cone at construction time.
    #[allow(clippy::too_many_arguments)]
    pub fn spot(
        pos: [f32; 3],
        aim: [f32; 3],
        color_rgb: [f32; 3],
        intensity: f32,
        range: f32,
        inner_cone_angle: f32,
        outer_cone_angle: f32,
        cast_shadows: bool,
        shadow_softness: ShadowSoftness,
        shadow_bias: f32,
        shadow_resolution: u32,
    ) -> Self {
        let dir = normalize3(sub3(aim, pos));
        let outer = outer_cone_angle.clamp(0.0, std::f32::consts::FRAC_PI_2);
        let inner = inner_cone_angle.clamp(0.0, outer);
        Self {
            mode: LightMode::Spot,
            falloff: LightFalloff::InverseSquare,
            pos,
            aim,
            dir,
            color: [
                color_rgb[0] * intensity,
                color_rgb[1] * intensity,
                color_rgb[2] * intensity,
                1.0,
            ],
            range: range.max(0.0),
            inner_cone_angle: inner,
            outer_cone_angle: outer,
            cast_shadows,
            shadow_softness,
            shadow_bias,
            shadow_resolution,
        }
    }

    /// Pack this light into the four-vec4 storage ABI shared by raster
    /// lighting and shafts. `slot` is the compact caster-table index, or -1.
    pub fn packed(&self, slot: f32) -> [[f32; 4]; 4] {
        let (mode, pos_or_dir) = match self.mode {
            LightMode::Sun => (0.0, [-self.dir[0], -self.dir[1], -self.dir[2], 0.0]),
            LightMode::Point => (1.0, [self.pos[0], self.pos[1], self.pos[2], 1.0]),
            LightMode::Spot => (2.0, [self.pos[0], self.pos[1], self.pos[2], 2.0]),
        };
        [
            [pos_or_dir[0], pos_or_dir[1], pos_or_dir[2], mode],
            [self.color[0], self.color[1], self.color[2], slot],
            [
                self.range,
                match self.falloff {
                    LightFalloff::Legacy => 0.0,
                    LightFalloff::InverseSquare => 1.0,
                },
                self.inner_cone_angle.cos(),
                self.outer_cone_angle.cos(),
            ],
            [self.dir[0], self.dir[1], self.dir[2], 0.0],
        ]
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
            LightMode::Point | LightMode::Spot => normalize3(sub3(self.pos, world_pos)),
        }
    }

    /// Per-point attenuation factor in [0, 1].
    ///
    /// Sun: always 1.0 (parallel rays don't fall off).
    /// Point/Spot: the selected legacy or physical falloff, multiplied by
    /// the spot cone factor for Spot.
    pub fn attenuation_at(&self, world_pos: [f32; 3]) -> f32 {
        match self.mode {
            LightMode::Sun => 1.0,
            LightMode::Point | LightMode::Spot => {
                let d = sub3(self.pos, world_pos);
                let d_sq = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                let distance = d_sq.sqrt();
                let base = match self.falloff {
                    LightFalloff::Legacy => {
                        let r_sq = self.range * self.range;
                        if r_sq < 1e-10 {
                            0.0
                        } else {
                            1.0 / (1.0 + d_sq / r_sq)
                        }
                    }
                    LightFalloff::InverseSquare => {
                        if self.range > 0.0 {
                            (1.0 - (distance / self.range).powi(4)).max(0.0) / d_sq.max(1e-6)
                        } else {
                            1.0 / d_sq.max(1e-6)
                        }
                    }
                };
                base * self.spot_factor_at(world_pos)
            }
        }
    }

    /// Smooth squared cone factor for Spot lights; other modes are unity.
    pub fn spot_factor_at(&self, world_pos: [f32; 3]) -> f32 {
        if self.mode != LightMode::Spot {
            return 1.0;
        }
        let l = self.light_dir_at(world_pos);
        let c = self.dir[0] * -l[0] + self.dir[1] * -l[1] + self.dir[2] * -l[2];
        let inner = self.inner_cone_angle.cos();
        let outer = self.outer_cone_angle.cos();
        ((c - outer) / (inner - outer).max(0.001))
            .clamp(0.0, 1.0)
            .powi(2)
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
    /// - **Sun** → orthographic box of half-extent `range` round `aim`,
    ///   viewed down the light axis from `pos`: X/Y span `±range`, depth
    ///   from `pos` to `range` past `aim`, so everything between the sun's
    ///   anchor and the box casts into it (a cascade puts `pos` well back).
    /// - **Point** → one cube face: a perspective from `pos` toward `aim`,
    ///   a little wider than 90° so a filter near the face's edge stays on
    ///   its map. `far` must comfortably exceed `range` because attenuation
    ///   is only 50% at `d = range` — lit geometry extends well past it, so
    ///   `far = 4·range` (≈6% attenuation) keeps casters inside the frustum.
    pub fn shadow_proj(&self) -> [[f32; 4]; 4] {
        match self.mode {
            LightMode::Sun => {
                let far = dist3(self.pos, self.aim) + self.range;
                ortho_rh(-self.range, self.range, -self.range, self.range, 0.0, far)
            }
            LightMode::Point | LightMode::Spot => {
                let physical_infinite =
                    self.falloff == LightFalloff::InverseSquare && self.range <= 0.0;
                let extent = if physical_infinite {
                    dist3(self.pos, self.aim).max(1.0)
                } else {
                    self.range.max(0.01)
                };
                let near = (extent * 0.05).clamp(0.0001, extent * 0.5);
                let far = if physical_infinite {
                    extent
                } else {
                    extent * 4.0
                };
                let fov = match self.mode {
                    LightMode::Spot => {
                        (self.outer_cone_angle * 2.0).clamp(0.01, std::f32::consts::PI - 0.0001)
                    }
                    // A face spans tan 1 each side, plus CUBE_EDGE_TEXELS of
                    // its own `shadow_resolution` texels: 90° lands at NDC
                    // 1 - 2·CUBE_EDGE_TEXELS/side.
                    _ => {
                        let side = self.shadow_resolution.max(256) as f32;
                        2.0 * (1.0 / (1.0 - 2.0 * CUBE_EDGE_TEXELS / side)).atan()
                    }
                };
                perspective_rh(fov, 1.0, near, far)
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

    /// A Sun's shadow cascades for `camera` at `aspect` (width over height):
    /// boxes round successive slices of the view, nearest first, splitting
    /// `SHADOW_DISTANCE`. Each box holds its slice's bounding sphere, so it
    /// keeps its size as the camera turns, and moves in whole texels of its
    /// map, so shadow edges hold still. Shadow Bias keeps its meaning in
    /// each cascade's own texels.
    pub fn cascades(&self, camera: &Camera, aspect: f32) -> [Light; CASCADES] {
        let texels = self.shadow_resolution.max(1) as f32;
        let up = pick_up(self.dir);
        let side = normalize3(cross3(self.dir, up));
        let lift = cross3(side, self.dir);
        // How far a corner of the view lies off its axis: per metre ahead
        // squared (perspective), or fixed squared (orthographic).
        let (spread, flat) = match camera.mode {
            CameraMode::Perspective { fov_y } => {
                let t = (fov_y * 0.5).tan();
                (t * t * (1.0 + aspect * aspect), 0.0)
            }
            CameraMode::Orthographic { half_height } => {
                (0.0, half_height * half_height * (1.0 + aspect * aspect))
            }
        };
        let near = camera.near.max(1e-3);
        let far = SHADOW_DISTANCE.min(camera.far).max(2.0 * near);
        let from = CASCADE_NEAR.clamp(near, far);
        let split = |i: usize| {
            if i == 0 {
                return near;
            }
            let f = i as f32 / CASCADES as f32;
            let ratio = from * (far / from).powf(f);
            ratio + (near + (far - near) * f - ratio) * CASCADE_EVEN
        };
        std::array::from_fn(|i| {
            let (a, b) = (split(i), split(i + 1));
            // The sphere through the slice's near and far corners, its
            // centre on the view axis, or at the far face when that is
            // nearer; its radius rounded up so float noise never resizes it.
            let c = ((a + b) * 0.5 * (1.0 + spread)).min(b);
            let r = ((b - c) * (b - c) + b * b * spread + flat).sqrt();
            let r = (r * 16.0).ceil() / 16.0;
            let texel = 2.0 * r / texels;
            let mut centre: [f32; 3] = std::array::from_fn(|k| camera.pos[k] + camera.fwd[k] * c);
            for axis in [side, lift] {
                let d = dot3(centre, axis);
                let shift = (d / texel).round() * texel - d;
                for k in 0..3 {
                    centre[k] += axis[k] * shift;
                }
            }
            let back = r + CASCADE_REACH;
            Light {
                pos: std::array::from_fn(|k| centre[k] - self.dir[k] * back),
                aim: centre,
                range: r,
                shadow_bias: self.shadow_bias / SUN_BIAS * BIAS_TEXELS * texel / (back + r),
                ..*self
            }
        })
    }

    /// A point light's six shadow views, one per cube face, in the order
    /// `caster_face` in `scene_shading.wgsl` picks them: +X, -X, +Y, -Y, +Z,
    /// -Z. Each is this light aimed down its axis at the same distance, so
    /// the face's range matches the light's.
    pub fn cube_faces(&self) -> [Light; 6] {
        let distance = dist3(self.pos, self.aim).max(1e-3);
        [
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
        ]
        .map(|axis: [f32; 3]| Light {
            aim: std::array::from_fn(|k| self.pos[k] + axis[k] * distance),
            dir: axis,
            ..*self
        })
    }
}

/// Texels each cube face reaches past 90° on every side, so a filter at the
/// face's edge reads its own map, not the lit outside.
const CUBE_EDGE_TEXELS: f32 = 8.0;

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

    /// Every point the camera sees out to the shadow distance lies inside
    /// some cascade's map, the near cascades are sharp, and a cascade's
    /// box sits on its own texel grid wherever the camera stands.
    #[test]
    fn cascades_hold_the_view_on_their_texel_grid() {
        let sun = Light::sun(
            [300.0, 140.0, -260.0],
            [0.0, 0.0, 0.0],
            [1.0; 3],
            1.0,
            30.0,
            true,
            ShadowSoftness::Soft,
            SUN_BIAS,
            4096,
        );
        let aspect = 16.0 / 9.0;
        let fov_y = 0.9f32;
        let camera = Camera::orbit_perspective(2.5, 0.12, 150.0, fov_y, 200.0, 0.0, 0.05, 20000.0);
        let cascades = sun.cascades(&camera, aspect);
        assert!(cascades[0].range < 25.0, "{}", cascades[0].range);
        assert!(cascades.windows(2).all(|w| w[0].range < w[1].range));
        let t = (fov_y * 0.5).tan();
        for step in 1..=60 {
            let depth = 0.1 * (SHADOW_DISTANCE / 0.1f32).powf(step as f32 / 60.0) * 0.999;
            for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0), (0.0, 0.0)] {
                let p: [f32; 3] = std::array::from_fn(|k| {
                    camera.pos[k]
                        + camera.fwd[k] * depth
                        + camera.right[k] * sx * t * aspect * depth
                        + camera.up[k] * sy * t * depth
                });
                let held = cascades.iter().any(|c| {
                    let m = c.shadow_view_proj();
                    let clip: [f32; 4] = std::array::from_fn(|r| {
                        m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r]
                    });
                    clip[0].abs() <= 1.0 && clip[1].abs() <= 1.0 && (0.0..=1.0).contains(&clip[2])
                });
                assert!(held, "a point {depth} m ahead is in no cascade");
            }
        }
        let side = normalize3(cross3(sun.dir, pick_up(sun.dir)));
        let moved = Camera::orbit_perspective(2.5013, 0.1207, 150.3, fov_y, 200.0, 0.0, 0.05, 20000.0);
        for c in sun.cascades(&moved, aspect) {
            let texel = 2.0 * c.range / 4096.0;
            let along = dot3(c.aim, side) / texel;
            assert!((along - along.round()).abs() < 0.01, "{along}");
        }
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

    /// The face `caster_face` picks (largest axis from the light) holds
    /// every direction, corners included, inside its map with the edge
    /// margin to spare, and in front of its near plane.
    #[test]
    fn cube_faces_hold_every_direction_with_an_edge_margin() {
        let pos = [1.5, -2.0, 0.25];
        let l = Light::point(
            pos,
            [4.0, 1.0, -3.0],
            [1.0, 1.0, 1.0],
            1.0,
            6.0,
            true,
            ShadowSoftness::Soft,
            0.003,
            512,
        );
        let faces = l.cube_faces();
        let edge = 1.0 - 2.0 * CUBE_EDGE_TEXELS / 512.0;
        let dirs = [
            [1.0, 0.2, -0.3],
            [-1.0, 0.99, 0.99],
            [0.1, 1.0, -0.4],
            [0.3, -1.0, 0.98],
            [0.0, 0.0, 1.0],
            [-0.7, 0.5, -1.0],
            [1.0, 1.0, 1.0],
        ];
        for d in dirs {
            let a = d.map(f32::abs);
            let face = if a[0] >= a[1] && a[0] >= a[2] {
                usize::from(d[0] < 0.0)
            } else if a[1] >= a[2] {
                2 + usize::from(d[1] < 0.0)
            } else {
                4 + usize::from(d[2] < 0.0)
            };
            let p = std::array::from_fn::<f32, 3, _>(|k| pos[k] + d[k] * 3.0);
            let m = faces[face].shadow_view_proj();
            let clip: [f32; 4] =
                std::array::from_fn(|r| (0..3).map(|c| m[c][r] * p[c]).sum::<f32>() + m[3][r]);
            assert!(clip[3] > 0.0, "{d:?} behind face {face}");
            let ndc = [clip[0] / clip[3], clip[1] / clip[3], clip[2] / clip[3]];
            assert!(
                ndc[0].abs() <= edge && ndc[1].abs() <= edge,
                "{d:?} at {ndc:?} on face {face} reaches the edge margin"
            );
            assert!((0.0..=1.0).contains(&ndc[2]), "{d:?} depth {}", ndc[2]);
        }
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
    fn inverse_square_range_and_infinite_cutoff_follow_physical_curve() {
        let mut bounded = Light::point(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, -1.0],
            [1.0, 1.0, 1.0],
            1.0,
            4.0,
            false,
            ShadowSoftness::Soft,
            0.003,
            2048,
        );
        bounded.falloff = LightFalloff::InverseSquare;
        // max(1 - (d/r)^4, 0) / d².
        assert!(approx_eq(
            bounded.attenuation_at([1.0, 0.0, 0.0]),
            255.0 / 256.0,
            1e-6
        ));
        assert!(approx_eq(
            bounded.attenuation_at([2.0, 0.0, 0.0]),
            15.0 / 64.0,
            1e-6
        ));
        assert_eq!(bounded.attenuation_at([4.0, 0.0, 0.0]), 0.0);

        let mut infinite = bounded;
        infinite.range = 0.0;
        assert!(approx_eq(
            infinite.attenuation_at([2.0, 0.0, 0.0]),
            0.25,
            1e-6
        ));
    }

    #[test]
    fn spot_cone_has_center_edge_and_outside_regions() {
        let light = Light::spot(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, -1.0],
            [1.0, 1.0, 1.0],
            1.0,
            10.0,
            0.2,
            0.4,
            false,
            ShadowSoftness::Soft,
            0.003,
            2048,
        );
        assert!(approx_eq(light.spot_factor_at([0.0, 0.0, -1.0]), 1.0, 1e-5));
        let outer = light.outer_cone_angle;
        let edge = [outer.sin(), 0.0, -outer.cos()];
        assert!(light.spot_factor_at(edge) < 1e-5);
        let outside = [(outer + 0.1).sin(), 0.0, -(outer + 0.1).cos()];
        assert_eq!(light.spot_factor_at(outside), 0.0);
    }

    #[test]
    fn packed_light_uses_four_vec4s_and_preserves_spot_metadata() {
        let light = Light::spot(
            [1.0, 2.0, 3.0],
            [1.0, 2.0, 1.0],
            [0.2, 0.3, 0.4],
            2.0,
            6.0,
            0.1,
            0.5,
            false,
            ShadowSoftness::Hard,
            0.001,
            512,
        );
        let packed = light.packed(3.0);
        assert_eq!(packed.len(), 4);
        assert_eq!(packed[0][3], 2.0);
        assert!(approx_vec3(
            [packed[0][0], packed[0][1], packed[0][2]],
            light.pos,
            1e-6
        ));
        assert!(approx_vec3(
            [packed[1][0], packed[1][1], packed[1][2]],
            [0.4, 0.6, 0.8],
            1e-6
        ));
        assert_eq!(packed[1][3], 3.0);
        assert_eq!(packed[2][0], 6.0);
        assert_eq!(packed[2][1], 1.0);
        assert!(approx_eq(packed[2][2], 0.1f32.cos(), 1e-6));
        assert!(approx_eq(packed[2][3], 0.5f32.cos(), 1e-6));
        assert!(approx_vec3(
            [packed[3][0], packed[3][1], packed[3][2]],
            light.dir,
            1e-6
        ));
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
        assert!(
            (p[2][3] + 1.0).abs() < 1e-6,
            "perspective m[2][3] should be -1"
        );
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
        assert!(
            (clip[3] - 1.0).abs() < 1e-4,
            "ortho w should be 1, got {}",
            clip[3]
        );
        assert!(
            (clip[0] / clip[3]).abs() < 1e-3,
            "aim not centred in x: {}",
            clip[0]
        );
        assert!(
            (clip[1] / clip[3]).abs() < 1e-3,
            "aim not centred in y: {}",
            clip[1]
        );
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
