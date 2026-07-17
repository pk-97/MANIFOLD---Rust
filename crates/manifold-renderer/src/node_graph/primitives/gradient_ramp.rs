//! `node.gradient` — general N-stop gradient / LUT generator.
//!
//! Emits a 1D piecewise-linear gradient as a texture: output texel x maps to
//! `t = x / (width - 1) * domain` (endpoint-inclusive, so texel 0 is exactly the
//! first stop and the last texel is exactly `t = domain`), evaluated over a
//! `Table` of stops (each row `[position, r, g, b]`). The endpoint mapping is
//! what lets a pure-black input read back pure black through `node.color_lut` —
//! a centre mapping `(x + 0.5) / width` leaves texel 0 a half-step up the ramp,
//! a faint hue on the darkest palettes. The gradient is constant in y, so the output is
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

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// Maximum number of gradient stops packed into the uniform. Covers every
/// legacy Infrared palette (max 6 explicit stops) with headroom for
/// finely-broken ramps (e.g. a procedural rainbow laid out as breakpoints).
const MAX_STOPS: usize = 16;

/// Strip width for the LUT-shaped output (workstream 4 — right-sized outputs).
/// A gradient ramp is a 1D luminance LUT, constant in y; its natural size is a
/// W×1 strip, NOT the full canvas. 256 is the industry-standard LUT width — a
/// smooth ramp is visually indistinguishable from a canvas-wide one at this
/// resolution, while a canvas-wide ramp regenerates a multi-MB texture per
/// palette (Infrared bakes 10 of them, ~330 MB of pool at 4K). Every shipped
/// consumer reads the ramp resolution-independently — Infrared wires it through
/// `node.switch_texture` (samples at uv) into `node.color_lut`'s `lut` Gather input
/// (sampled at a normalized luminance coord) — so the strip is correct, never
/// texel-exact-read. See the `output_dims` override below.
const LUT_WIDTH: u32 = 256;

// Standalone-codegen uniform layout: the generated `Params` struct lays out the
// scalar params (domain) first, then one `_count` word per Table param, pads the
// header to 16 bytes, then appends each table's `array<vec4<f32>, 16>`. So the
// field order here is {domain, count, _pad, _pad, stops} — domain ahead of count,
// unlike the hand gradient_ramp.wgsl ({count, domain, …}). The body recovers the
// column t from uv.x and the codegen-supplied dims.x (textureDimensions of the
// output), so there is no width uniform field.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GradientRampUniforms {
    domain: f32,
    count: u32,
    _pad0: u32,
    _pad1: u32,
    /// Each stop is (position, r, g, b).
    stops: [[f32; 4]; MAX_STOPS],
}

crate::primitive! {
    name: GradientRamp,
    type_id: "node.gradient",
    purpose: "General N-stop gradient / LUT generator. Emits a 1D piecewise-linear gradient as a texture — texel x → t = x/(width-1) * domain (endpoint-inclusive: texel 0 is exactly the first stop, the last texel is exactly t=domain), evaluated over a Table of `[position, r, g, b]` stops (up to 16). Constant in y, so it's a luminance LUT for node.color_lut, and reusable as a gradient texture anywhere (false-colour, duotone, thermal palettes, gradient-map, UI ramps). Evaluation matches the classic gradient(): clamp below the first stop, lerp between stops, and EXTRAPOLATE the last segment past the last stop — the overshoot that paints HDR blowout highlights when domain > 1.",
    inputs: {
        domain: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("stops"),
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
            name: Cow::Borrowed("domain"),
            label: "Domain",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0, 8.0)),
            enum_values: &[],
        },
    ],
    depth_rule: SourceHeight,
    composition_notes: "Stop positions are in t-space; `domain` sets the max t the texture covers (texel x → t = x/(width-1) * domain, endpoint-inclusive so texel 0 = t=0 = the first stop and the last texel = t=domain). Stops in [0,1] with domain=2 reproduce Infrared's [0,2] LUT (the [1,2] tail is the extrapolated overshoot). Feed `out` into node.color_lut's `lut` input (sampled by luminance) for a gradient-map effect, or use it directly as a ramp texture. Stops are NOT clamped to [0,1] colour — negative / >1 channels are preserved (legacy Black Hot reaches negative past the last stop). Up to 16 stops; rows beyond that are ignored.",
    examples: ["preset.effect.infrared"],
    picker: { label: "Gradient", category: Atom },
    summary: "Builds a colour gradient as a strip you can use as a lookup table or feed into Gradient Map. Add as many colour stops as you like.",
    category: Generate,
    role: Source,
    aliases: ["gradient", "gradient ramp", "color ramp", "lut", "palette"],
    pure: true,
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/gradient_ramp_body.wgsl"),
}

impl Primitive for GradientRamp {
    /// Right-sized LUT output (workstream 4): a fixed `LUT_WIDTH × 1` strip
    /// regardless of canvas. The body maps texel x → `t = x/(width-1) * domain`
    /// (endpoint-inclusive, recovered from `uv.x` and `dims.x`), so endpoints are
    /// exact at any width and the interior shifts by at most half a texel — only
    /// the texel quantization changes, and 256 is the industry LUT width. The
    /// endpoint mapping is load-bearing for the dark end: with the old centre
    /// mapping `(x+0.5)/width`, texel 0 sat at `t≈0.004` (a faint navy on Arctic),
    /// and shrinking the strip to 256 widened that gap into the visible range —
    /// pure black no longer read back pure black. Endpoint puts texel 0 exactly on
    /// the first stop. Constant in y, so height 1. Every shipped consumer samples
    /// it at a normalized coord (see `LUT_WIDTH`), so the strip is read
    /// resolution-independently, never texel-exact.
    fn output_dims(
        &self,
        _port: &str,
        _canvas_dims: (u32, u32),
        _input_dims: &[(&str, (u32, u32))],
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        Some((LUT_WIDTH, 1))
    }

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
            // Source generator with a Table param: the generated kernel binds
            // uniform(0)/dst(1); the `stops` Table expands to a count word + a
            // 16-entry vec4 array. The body recovers the column t from uv.x.
            // gradient_ramp.wgsl is the parity oracle.
            gpu.device.create_compute_pipeline(
                &crate::node_graph::freeze::codegen::standalone_for_spec::<Self>()
                    .expect("node.gradient standalone codegen"),
                crate::node_graph::freeze::codegen::ENTRY,
                "node.gradient",
            )
        });

        let uniforms = GradientRampUniforms {
            domain,
            count,
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
            "node.gradient",
        );
    }
}
