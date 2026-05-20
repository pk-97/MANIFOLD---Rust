//! `node.wgsl_compute_2tex_1tex` — escape-hatch two-input composite
//! primitive (two Texture2D inputs, one Texture2D output).
//!
//! Shader contract:
//!
//! ```wgsl
//! struct U {
//!     f0: f32, f1: f32, f2: f32, f3: f32,
//!     f4: f32, f5: f32, f6: f32, f7: f32,
//! };
//! @group(0) @binding(0) var<uniform> u: U;
//! @group(0) @binding(1) var a_tex: texture_2d<f32>;
//! @group(0) @binding(2) var b_tex: texture_2d<f32>;
//! @group(0) @binding(3) var tex_sampler: sampler;
//! @group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;
//!
//! @compute @workgroup_size(16, 16)
//! fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
//!     // ... blend a_tex and b_tex into output_tex ...
//! }
//! ```

#![allow(private_interfaces)]

use std::hash::{Hash, Hasher};

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

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

pub const DEFAULT_WGSL_2TEX_1TEX: &str = r"
struct U {
    f0: f32, f1: f32, f2: f32, f3: f32,
    f4: f32, f5: f32, f6: f32, f7: f32,
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var a_tex: texture_2d<f32>;
@group(0) @binding(2) var b_tex: texture_2d<f32>;
@group(0) @binding(3) var tex_sampler: sampler;
@group(0) @binding(4) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y { return; }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(dims);
    let a = textureSampleLevel(a_tex, tex_sampler, uv, 0.0);
    let b = textureSampleLevel(b_tex, tex_sampler, uv, 0.0);
    // Default: 50/50 mix. Author overrides with their own kernel.
    textureStore(output_tex, vec2<i32>(id.xy), mix(a, b, 0.5));
}
";

crate::primitive! {
    name: WgslCompute2Tex1Tex,
    type_id: "node.wgsl_compute_2tex_1tex",
    purpose: "WGSL escape hatch — two Texture2D inputs, one Texture2D output. For binary composite kernels that don't decompose through node.compose / node.mix / node.masked_mix or other existing blend primitives. Default kernel is a 50/50 mix; agent override is the whole point.",
    inputs: {
        a: Texture2D required,
        b: Texture2D required,
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
    composition_notes: "Bindings: @binding(0) u (8 f32); @binding(1) a_tex; @binding(2) b_tex; @binding(3) tex_sampler; @binding(4) output_tex (rgba16float, write). Same sampler reads both input textures. Entry point cs_main, @workgroup_size(16, 16).",
    examples: [],
    picker: { label: "WGSL Compute (2 tex → 1 tex)", category: Atom },
    extra_fields: {
        source: String = DEFAULT_WGSL_2TEX_1TEX.to_string(),
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
    match naga::front::wgsl::parse_str(source) {
        Ok(_) => Ok(()),
        Err(e) => Err(e.emit_to_string(source)),
    }
}

impl Primitive for WgslCompute2Tex1Tex {
    fn wgsl_source(&self) -> Option<&str> {
        Some(&self.source)
    }
    fn set_wgsl_source(&mut self, source: &str) {
        if self.source == source {
            return;
        }
        self.source = source.to_string();
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

        let Some(a_tex) = ctx.inputs.texture_2d("a") else {
            return;
        };
        let Some(b_tex) = ctx.inputs.texture_2d("b") else {
            return;
        };
        let Some(out_tex) = ctx.outputs.texture_2d("out") else {
            return;
        };
        let (w, h) = (out_tex.width, out_tex.height);

        let current_hash = hash_source(&self.source);
        let needs_compile = !self.compile_failed
            && (self.pipeline.is_none() || self.compiled_source_hash != Some(current_hash));

        let gpu = ctx.gpu_encoder();

        if needs_compile {
            match validate_wgsl(&self.source) {
                Ok(()) => {
                    let pipeline = gpu.device.create_compute_pipeline(
                        &self.source,
                        "cs_main",
                        "node.wgsl_compute_2tex_1tex",
                    );
                    self.pipeline = Some(pipeline);
                    self.compiled_source_hash = Some(current_hash);
                    self.compile_failed = false;
                }
                Err(msg) => {
                    log::warn!(
                        "[node.wgsl_compute_2tex_1tex] WGSL validation failed; dispatch skipped:\n{}",
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
                    texture: a_tex,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: b_tex,
                },
                GpuBinding::Sampler {
                    binding: 3,
                    sampler,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.wgsl_compute_2tex_1tex",
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
    fn wgsl_2tex_1tex_declares_two_inputs_and_one_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(WgslCompute2Tex1Tex::TYPE_ID, "node.wgsl_compute_2tex_1tex");
        assert_eq!(WgslCompute2Tex1Tex::INPUTS.len(), 2);
        assert_eq!(WgslCompute2Tex1Tex::INPUTS[0].name, "a");
        assert_eq!(WgslCompute2Tex1Tex::INPUTS[0].ty, PortType::Texture2D);
        assert_eq!(WgslCompute2Tex1Tex::INPUTS[1].name, "b");
        assert_eq!(WgslCompute2Tex1Tex::INPUTS[1].ty, PortType::Texture2D);
        assert_eq!(WgslCompute2Tex1Tex::OUTPUTS.len(), 1);
    }

    #[test]
    fn wgsl_2tex_1tex_has_eight_float_slots() {
        let names: Vec<&str> = WgslCompute2Tex1Tex::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["f0", "f1", "f2", "f3", "f4", "f5", "f6", "f7"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = WgslCompute2Tex1Tex::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.wgsl_compute_2tex_1tex");
    }

    #[test]
    fn default_source_is_a_valid_wgsl_kernel() {
        assert!(validate_wgsl(DEFAULT_WGSL_2TEX_1TEX).is_ok());
    }

    #[test]
    fn set_wgsl_source_round_trips_through_trait() {
        let mut prim = WgslCompute2Tex1Tex::new();
        let node: &mut dyn EffectNode = &mut prim;
        let custom = "// custom 2tex\n";
        node.set_wgsl_source(custom);
        assert_eq!(node.wgsl_source(), Some(custom));
    }
}
