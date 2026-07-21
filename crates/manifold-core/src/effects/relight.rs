//! Depth-relight stage params (`RelightField`, `RelightHeightFrom`,
//! `RelightParams`). Extracted from effects.rs (P2-E, design D4).

use serde::{Deserialize, Serialize};

/// One of the D3 relight-stage float knobs (`docs/DEPTH_RELIGHT_DESIGN.md`
/// D3). Lives in `manifold-core` because both the renderer (per-frame
/// uniform writes) and the editing commands need to address the same field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelightField {
    LightX,
    LightY,
    Relief,
    AoIntensity,
    ShadowSoftness,
    Gain,
}

impl RelightField {
    /// Read this field off a [`RelightParams`].
    pub fn get(self, p: &RelightParams) -> f32 {
        match self {
            Self::LightX => p.light_x,
            Self::LightY => p.light_y,
            Self::Relief => p.relief,
            Self::AoIntensity => p.ao_intensity,
            Self::ShadowSoftness => p.shadow_softness,
            Self::Gain => p.gain,
        }
    }

    /// Write this field on a [`RelightParams`].
    pub fn set(self, p: &mut RelightParams, value: f32) {
        match self {
            Self::LightX => p.light_x = value,
            Self::LightY => p.light_y = value,
            Self::Relief => p.relief = value,
            Self::AoIntensity => p.ao_intensity = value,
            Self::ShadowSoftness => p.shadow_softness = value,
            Self::Gain => p.gain = value,
        }
    }

    /// Every float field, in UI declaration order.
    pub const ALL: &[Self] = &[
        Self::LightX,
        Self::LightY,
        Self::Relief,
        Self::AoIntensity,
        Self::ShadowSoftness,
        Self::Gain,
    ];
}

/// D4's height-origin override for the "3D Shading" relight stage
/// (`docs/DEPTH_RELIGHT_DESIGN.md` D2/D4, phase P5): `Auto` runs the
/// compiler's D1 structural walk (falling back to luminance-of-output only
/// when no `SourceHeight` producer is reachable — the proven default the
/// whole probe sweep ran on); `Luminance`/`InvertedLuminance` force the
/// height tap onto the final color's luminance (inverted or not) regardless
/// of what the structural walk would find, for effects/generators whose
/// natural output reads better relit from its brightness than from its
/// nominal depth producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RelightHeightFrom {
    #[default]
    Auto,
    Luminance,
    InvertedLuminance,
}

/// The D3 relight-stage knobs, exposed as ordinary card params once the "3D
/// Shading" toggle (`PresetInstance::relight`) is on. Always present on the
/// instance regardless of the toggle's state — per the no-conditionally-
/// visible-UI rule the card renders these rows greyed rather than hidden
/// when the toggle is off, so the values must survive a toggle-off/toggle-on
/// round trip. Defaults are the probe sweep's proven v6 recipe (D3) /
/// `node.heightfield_shadow`'s own defaults (D5) — see `relight.rs`'s
/// template mint for where each field lands.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelightParams {
    /// Light direction X — fans out to `rl_lambert.light_x` AND
    /// `rl_shadow.light_x` (the shadow raymarch must track the same light
    /// direction as the Lambert term, or the shadow reads mismatched against
    /// the shading whenever this is dragged).
    #[serde(default = "RelightParams::default_light_x")]
    pub light_x: f32,
    /// Light direction Y — same fan-out as `light_x`.
    #[serde(default = "RelightParams::default_light_y")]
    pub light_y: f32,
    /// Bump/occlusion/shadow strength — fans out to `rl_bumps.z_scale`
    /// (rescaled ×12, so the proven default 0.25 lands on the proven
    /// z_scale default 3.0), `rl_ao.relief`, and `rl_shadow.relief`.
    #[serde(default = "RelightParams::default_relief")]
    pub relief: f32,
    /// `rl_ao.intensity`.
    #[serde(default = "RelightParams::default_ao_intensity")]
    pub ao_intensity: f32,
    /// `rl_shadow.softness`.
    #[serde(default = "RelightParams::default_shadow_softness")]
    pub shadow_softness: f32,
    /// `rl_exposure.gain`.
    #[serde(default = "RelightParams::default_gain")]
    pub gain: f32,
    /// D4 height-origin override.
    #[serde(default)]
    pub height_from: RelightHeightFrom,
}

impl RelightParams {
    fn default_light_x() -> f32 {
        0.4
    }
    fn default_light_y() -> f32 {
        0.6
    }
    fn default_relief() -> f32 {
        0.25
    }
    fn default_ao_intensity() -> f32 {
        1.3
    }
    fn default_shadow_softness() -> f32 {
        0.5
    }
    fn default_gain() -> f32 {
        1.4
    }

    /// Whether every field is at its proven-recipe default — the
    /// serialize-skip gate so an untouched instance's `relightParams`
    /// doesn't appear on the wire (byte-identical old projects, D2).
    pub(super) fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl Default for RelightParams {
    fn default() -> Self {
        Self {
            light_x: Self::default_light_x(),
            light_y: Self::default_light_y(),
            relief: Self::default_relief(),
            ao_intensity: Self::default_ao_intensity(),
            shadow_softness: Self::default_shadow_softness(),
            gain: Self::default_gain(),
            height_from: RelightHeightFrom::Auto,
        }
    }
}
