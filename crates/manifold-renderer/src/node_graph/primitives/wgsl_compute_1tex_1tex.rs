//! `node.wgsl_compute_1tex_1tex` — escape-hatch primitive for kernels
//! that don't decompose cleanly into the rest of the vocabulary.
//!
//! Shape: one Texture2D input → one Texture2D output. The shader source
//! is set at node-construction time (or via persistence) and held on the
//! node as an identity-level field, NOT a parameter. The eight `f0`..`f7`
//! float sliders ARE parameters — they bind into a `struct U` uniform
//! that the kernel reads.
//!
//! ## WGSL contract (the kernel must declare exactly these bindings)
//!
//! ```wgsl
//! struct U {
//!     f0: f32, f1: f32, f2: f32, f3: f32,
//!     f4: f32, f5: f32, f6: f32, f7: f32,
//! };
//! @group(0) @binding(0) var<uniform> u: U;
//! @group(0) @binding(1) var source_tex: texture_2d<f32>;
//! @group(0) @binding(2) var tex_sampler: sampler;
//! @group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;
//!
//! @compute @workgroup_size(16, 16)
//! fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
//!     // ... use u.f0 etc, read source_tex, write output_tex ...
//! }
//! ```
//!
//! On `set_wgsl_source`, the pipeline is invalidated; next `evaluate`
//! re-compiles via naga + the Metal backend. If validation fails, an
//! error is logged once and dispatch is skipped (output preserves
//! whatever the runtime allocator gave us — typically zeros).

#![allow(private_interfaces)]

use std::hash::{Hash, Hasher};

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

/// 8 generic float slots → matches the WGSL `struct U` layout.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Slots {
    f0: f32,
    f1: f32,
    f2: f32,
    f3: f32,
    f4: f32,
    f5: f32,
    f6: f32,
    f7: f32,
}

/// Default kernel — bit-exact passthrough. Lets a fresh node be wired
/// into a graph and produce sensible output before `set_wgsl_source`
/// installs an agent-authored kernel.
pub const DEFAULT_WGSL_1TEX_1TEX: &str = r"
struct U {
    f0: f32, f1: f32, f2: f32, f3: f32,
    f4: f32, f5: f32, f6: f32, f7: f32,
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var source_tex: texture_2d<f32>;
@group(0) @binding(2) var tex_sampler: sampler;
@group(0) @binding(3) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let c = textureSampleLevel(source_tex, tex_sampler, uv, 0.0);
    textureStore(output_tex, vec2<i32>(id.xy), c);
}
";

#[cfg(test)]
const PARAM_NAMES: [&str; 8] = ["f0", "f1", "f2", "f3", "f4", "f5", "f6", "f7"];

crate::primitive! {
    name: WgslCompute1Tex1Tex,
    type_id: "node.wgsl_compute_1tex_1tex",
    purpose: "WGSL escape hatch — 1 Texture2D in, 1 Texture2D out. Reserve for genuinely irreducible kernels (e.g. BlackHole's relativistic geodesic tracing, OilyFluid's coupled reaction-diffusion) where no compositional path through the existing vocabulary expresses what you want. For simpler per-pixel math reach for the procedural texture math family (uv_field, sin/cos/fract/abs/scale_offset on textures, distance_to_point, polar_field, simplex/perlin/fbm/voronoi noise) FIRST.",
    inputs: {
        in: Texture2D required,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef { name: "f0", label: "f0", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0, 1.0)), enum_values: &[] },
        ParamDef { name: "f1", label: "f1", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0, 1.0)), enum_values: &[] },
        ParamDef { name: "f2", label: "f2", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0, 1.0)), enum_values: &[] },
        ParamDef { name: "f3", label: "f3", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0, 1.0)), enum_values: &[] },
        ParamDef { name: "f4", label: "f4", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0, 1.0)), enum_values: &[] },
        ParamDef { name: "f5", label: "f5", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0, 1.0)), enum_values: &[] },
        ParamDef { name: "f6", label: "f6", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0, 1.0)), enum_values: &[] },
        ParamDef { name: "f7", label: "f7", ty: ParamType::Float, default: ParamValue::Float(0.0), range: Some((-1.0, 1.0)), enum_values: &[] },
    ],
    composition_notes: "Bindings the kernel MUST declare: @binding(0) var<uniform> u: U with 8 f32 slots f0..f7; @binding(1) source_tex; @binding(2) tex_sampler; @binding(3) output_tex (rgba16float, write). Entry point fn cs_main, @workgroup_size(16, 16). Author wraps preset metadata around this to expose f0..f7 with friendly names (e.g. f0 → 'Rotation Speed'). If WGSL fails to parse, the error is logged once and dispatch is skipped — debug by checking the log for [node.wgsl_compute_1tex_1tex] entries.",
    examples: [],
    picker: { label: "WGSL Compute (1tex → 1tex)", category: Atom },
    extra_fields: {
        source: String = DEFAULT_WGSL_1TEX_1TEX.to_string(),
        compiled_source_hash: Option<u64> = None,
        compile_failed: bool = false,
        output_format_override: Option<manifold_gpu::GpuTextureFormat> = None,
    },
}

fn hash_source(source: &str) -> u64 {
    let mut h = ahash::AHasher::default();
    source.hash(&mut h);
    h.finish()
}

fn validate_wgsl(source: &str) -> Result<(), String> {
    // naga is the same parser the Metal backend uses internally, so
    // pre-validating here catches authoring errors before we hit the
    // panicky create_compute_pipeline path.
    match naga::front::wgsl::parse_str(source) {
        Ok(_) => Ok(()),
        Err(e) => Err(e.emit_to_string(source)),
    }
}

impl Primitive for WgslCompute1Tex1Tex {
    fn wgsl_source(&self) -> Option<&str> {
        Some(&self.source)
    }
    fn set_wgsl_source(&mut self, source: &str) {
        if self.source == source {
            return;
        }
        self.source = source.to_string();
        // Invalidate the pipeline; next run() will recompile lazily.
        self.pipeline = None;
        self.compiled_source_hash = None;
        self.compile_failed = false;
    }
    fn output_format(&self, port: &str) -> Option<manifold_gpu::GpuTextureFormat> {
        if port == "out" { self.output_format_override } else { None }
    }
    fn set_output_format(&mut self, port: &str, format: manifold_gpu::GpuTextureFormat) {
        if port == "out" {
            self.output_format_override = Some(format);
        }
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let slots = Slots {
            f0: read_f(ctx, "f0"),
            f1: read_f(ctx, "f1"),
            f2: read_f(ctx, "f2"),
            f3: read_f(ctx, "f3"),
            f4: read_f(ctx, "f4"),
            f5: read_f(ctx, "f5"),
            f6: read_f(ctx, "f6"),
            f7: read_f(ctx, "f7"),
        };

        let Some(in_tex) = ctx.inputs.texture_2d("in") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);

        // Decide whether to (re)compile.
        let current_hash = hash_source(&self.source);
        let needs_compile = self.pipeline.is_none()
            && !self.compile_failed
            || self.compiled_source_hash != Some(current_hash) && !self.compile_failed;

        let gpu = ctx.gpu_encoder();

        if needs_compile {
            match validate_wgsl(&self.source) {
                Ok(()) => {
                    let pipeline = gpu.device.create_compute_pipeline(
                        &self.source,
                        "cs_main",
                        "node.wgsl_compute_1tex_1tex",
                    );
                    self.pipeline = Some(pipeline);
                    self.compiled_source_hash = Some(current_hash);
                    self.compile_failed = false;
                }
                Err(msg) => {
                    log::warn!(
                        "[node.wgsl_compute_1tex_1tex] WGSL validation failed; dispatch skipped:\n{}",
                        msg
                    );
                    self.pipeline = None;
                    self.compile_failed = true;
                }
            }
        }

        let Some(pipeline) = self.pipeline.as_ref() else {
            return;
        };
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&slots),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: in_tex,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 3,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.wgsl_compute_1tex_1tex",
        );
    }
}

fn read_f(ctx: &EffectNodeContext<'_, '_>, name: &str) -> f32 {
    match ctx.params.get(name) {
        Some(ParamValue::Float(f)) => *f,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn wgsl_1tex_1tex_declares_one_texture_input_and_one_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(
            WgslCompute1Tex1Tex::TYPE_ID,
            "node.wgsl_compute_1tex_1tex"
        );
        assert_eq!(WgslCompute1Tex1Tex::INPUTS.len(), 1);
        assert_eq!(WgslCompute1Tex1Tex::INPUTS[0].name, "in");
        assert_eq!(
            WgslCompute1Tex1Tex::INPUTS[0].ty,
            PortType::Texture2D
        );
        assert_eq!(WgslCompute1Tex1Tex::OUTPUTS.len(), 1);
        assert_eq!(WgslCompute1Tex1Tex::OUTPUTS[0].name, "out");
    }

    #[test]
    fn wgsl_1tex_1tex_has_eight_float_slots() {
        let names: Vec<&str> = WgslCompute1Tex1Tex::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, PARAM_NAMES.to_vec());
        for p in WgslCompute1Tex1Tex::PARAMS {
            assert!(matches!(p.ty, ParamType::Float));
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = WgslCompute1Tex1Tex::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.wgsl_compute_1tex_1tex");
    }

    #[test]
    fn default_source_is_a_valid_wgsl_kernel() {
        assert!(
            validate_wgsl(DEFAULT_WGSL_1TEX_1TEX).is_ok(),
            "the constructor's default passthrough kernel must parse"
        );
    }

    #[test]
    fn set_wgsl_source_round_trips_through_trait() {
        let mut prim = WgslCompute1Tex1Tex::new();
        let node: &mut dyn EffectNode = &mut prim;
        let custom = "// custom kernel — does not have to be valid for the round-trip\n";
        node.set_wgsl_source(custom);
        assert_eq!(node.wgsl_source(), Some(custom));
    }

    #[test]
    fn invalid_wgsl_returns_a_human_readable_error() {
        let bad = "this is not WGSL";
        let result = validate_wgsl(bad);
        assert!(result.is_err());
        let msg = result.unwrap_err();
        // emit_to_string produces output that includes a snippet of the
        // offending source and a position. We don't pin a specific string
        // (naga's error formatting is its own thing) but assert it's
        // non-empty so we know the agent-facing error path is wired.
        assert!(!msg.is_empty());
    }
}
