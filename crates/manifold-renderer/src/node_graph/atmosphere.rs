//! `Atmosphere` — port-data type carried on
//! [`PortType::Atmosphere`](crate::node_graph::ports::PortType::Atmosphere) wires.
//!
//! Scene-wide fog + sky tint. CPU-only wire value (like
//! [`Camera`](crate::node_graph::camera::Camera),
//! [`Light`](crate::node_graph::light::Light),
//! [`Material`](crate::node_graph::material::Material), and
//! [`Transform`](crate::node_graph::transform::Transform)) — no GPU resource on
//! the wire; `render_scene` folds these scalars into its per-object uniform and
//! applies exponential depth fog in each lit fragment shader.
//!
//! Produced by `node.atmosphere`, consumed by `render_scene`'s optional
//! `atmosphere` input (REALTIME_3D_DESIGN D5 / §5 P3). Unwired =
//! [`Atmosphere::default`] = `fog_density 0` = **no fog, byte-identical to no
//! atmosphere** — the "unwired = zero cost" contract. Fog composits OVER the
//! premultiplied-alpha contract rather than replacing it: it lerps the lit
//! fragment's straight (non-premultiplied) rgb toward `fog_color`, leaving
//! alpha untouched, so downstream compositing still keys transparency.

/// Scene-wide atmosphere: exponential depth fog + ambient/sky tint. CPU-only
/// wire value (`PortType::Atmosphere`). `fog_density == 0` means no fog.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Atmosphere {
    /// Fog colour (rgb; a reserved, currently unused). The colour distant
    /// geometry fades toward.
    pub fog_color: [f32; 4],
    /// Exponential depth-fog density. `0` = off (unwired default). Fog factor
    /// at distance `d` is `1 - exp(-density * d)`.
    pub fog_density: f32,
    /// Height falloff. `0` = uniform fog everywhere; `> 0` concentrates fog
    /// near the ground (`y = 0`) and thins it with altitude — the "ground
    /// haze" look. Density is scaled by `exp(-height_falloff * world_y)`.
    pub height_falloff: f32,
    /// Scene-wide ambient/sky tint (rgb multiplier on each object's ambient
    /// term; a reserved). `[1,1,1,1]` = neutral (unwired default).
    pub ambient_tint: [f32; 4],
    /// Light-shaft (volumetric inscatter) master gain
    /// (VOLUMETRIC_LIGHT_DESIGN.md D1). `0` = off (default): the march
    /// never runs, output byte-identical to today. THE fader.
    pub shaft_intensity: f32,
    /// Henyey–Greenstein anisotropy `g` in `[-0.9, 0.9]`. `0.6` default —
    /// forward-scattering, sun-shaft look. Negative = backscatter halo.
    pub shaft_anisotropy: f32,
    /// March step count: `0` = Low (16), `1` = Med (24, default), `2` =
    /// High (32).
    pub shaft_quality: u32,
}

impl Default for Atmosphere {
    /// Unwired default = **no atmosphere**: `fog_density 0` (off), neutral
    /// white ambient tint, `shaft_intensity 0` (light shafts off).
    /// Consumers treat this as byte-identical to having no `atmosphere`
    /// input at all.
    fn default() -> Self {
        Self {
            fog_color: [0.5, 0.55, 0.65, 1.0],
            fog_density: 0.0,
            height_falloff: 0.0,
            ambient_tint: [1.0, 1.0, 1.0, 1.0],
            shaft_intensity: 0.0,
            shaft_anisotropy: 0.6,
            shaft_quality: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_fog_off_neutral_tint() {
        let a = Atmosphere::default();
        assert_eq!(a.fog_density, 0.0, "default must be fog-off");
        assert_eq!(a.height_falloff, 0.0);
        assert_eq!(a.ambient_tint, [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn default_is_shafts_off_med_quality() {
        let a = Atmosphere::default();
        assert_eq!(a.shaft_intensity, 0.0, "default must be shafts-off (THE fader)");
        assert_eq!(a.shaft_anisotropy, 0.6);
        assert_eq!(a.shaft_quality, 1, "default quality is Med (1)");
    }

    #[test]
    fn atmosphere_is_copy() {
        let a = Atmosphere::default();
        let _b = a;
        let _c = a;
    }
}
