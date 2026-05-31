//! `node.gradient_ramp` — general N-stop gradient / LUT generator.
//!
//! Emits a 1D piecewise-linear gradient as a texture: output texel x maps to
//! `t = (x + 0.5) / width * domain`, evaluated over a `Table` of stops (each
//! row `[position, r, g, b]`). The gradient is constant in y, so the output is
//! a luminance LUT that `node.color_lut` samples directly — but it's reusable
//! as a gradient texture anywhere (false-colour, duotone, thermal palettes,
//! gradient-map, UI ramps).
//!
//! The evaluation matches the legacy Infrared `gradient()` bit-for-bit:
//! clamp-below-first-stop, linear between stops, and **extrapolate the last
//! segment past the last stop** — the overshoot that paints the HDR blowout
//! highlights when `domain > 1.0` (Infrared bakes its LUT over luma [0, 2]).
//!
//! Stops live in a JSON `Table` param (`{"type":"Table","rows":[[pos,r,g,b],
//! …]}`), up to 16 of them. `domain` (default 1.0) scales the t-range covered
//! by the texture; set it to 2.0 to reproduce Infrared's [0, 2] LUT.

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Maximum number of gradient stops packed into the uniform. Covers every
/// legacy Infrared palette (max 6 explicit stops) with headroom for
/// finely-broken ramps (e.g. a procedural rainbow laid out as breakpoints).
const MAX_STOPS: usize = 16;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GradientRampUniforms {
    count: u32,
    domain: f32,
    _pad0: u32,
    _pad1: u32,
    /// Each stop is (position, r, g, b).
    stops: [[f32; 4]; MAX_STOPS],
}

crate::primitive! {
    name: GradientRamp,
    type_id: "node.gradient_ramp",
    purpose: "General N-stop gradient / LUT generator. Emits a 1D piecewise-linear gradient as a texture — texel x → t = (x+0.5)/width * domain, evaluated over a Table of `[position, r, g, b]` stops (up to 16). Constant in y, so it's a luminance LUT for node.color_lut, and reusable as a gradient texture anywhere (false-colour, duotone, thermal palettes, gradient-map, UI ramps). Evaluation matches the classic gradient(): clamp below the first stop, lerp between stops, and EXTRAPOLATE the last segment past the last stop — the overshoot that paints HDR blowout highlights when domain > 1.",
    inputs: {
        domain: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: "stops",
            label: "Stops",
            ty: ParamType::Table,
            // Tables can't live in a const ParamValue (Arc isn't const-
            // constructible). Sentinel; the JSON preset supplies the real
            // `{"type":"Table","rows":[[pos,r,g,b], …]}`. With no table the
            // run path falls back to a black→white 2-stop ramp.
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: "domain",
            label: "Domain",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 8.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "Stop positions are in t-space; `domain` sets the max t the texture covers (texel x → t = (x+0.5)/width * domain). Stops in [0,1] with domain=2 reproduce Infrared's [0,2] LUT (the [1,2] tail is the extrapolated overshoot). Feed `out` into node.color_lut's `lut` input (sampled by luminance) for a gradient-map effect, or use it directly as a ramp texture. Stops are NOT clamped to [0,1] colour — negative / >1 channels are preserved (legacy Black Hot reaches negative past the last stop). Up to 16 stops; rows beyond that are ignored.",
    examples: ["preset.effect.infrared"],
    picker: { label: "Gradient", category: Atom },
    summary: "Builds a colour gradient as a strip you can use as a lookup table or feed into Gradient Map. Add as many colour stops as you like.",
    category: Generate,
    role: Source,
    aliases: ["gradient", "color ramp", "lut", "palette"],
}

impl Primitive for GradientRamp {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let domain = ctx.scalar_or_param("domain", 1.0);

        // Pack stops from the Table param. Fall back to a black→white
        // 2-stop ramp when no table is set (sentinel default).
        let mut stops = [[0.0f32; 4]; MAX_STOPS];
        let mut count: u32 = 0;
        if let Some(table) = ctx.params.get("stops").and_then(|p| p.as_table()) {
            for row in table.rows().iter().take(MAX_STOPS) {
                let pos = row.first().copied().unwrap_or(0.0);
                let r = row.get(1).copied().unwrap_or(0.0);
                let g = row.get(2).copied().unwrap_or(0.0);
                let b = row.get(3).copied().unwrap_or(0.0);
                stops[count as usize] = [pos, r, g, b];
                count += 1;
            }
        }
        if count == 0 {
            stops[0] = [0.0, 0.0, 0.0, 0.0];
            stops[1] = [1.0, 1.0, 1.0, 1.0];
            count = 2;
        }

        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);
        if w == 0 || h == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/gradient_ramp.wgsl"),
                "cs_main",
                "node.gradient_ramp",
            )
        });

        let uniforms = GradientRampUniforms {
            count,
            domain,
            _pad0: 0,
            _pad1: 0,
            stops,
        };

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.gradient_ramp",
        );
    }
}
