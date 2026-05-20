//! `node.wgsl_compute_0in_1tex` — escape-hatch pure-generator primitive
//! (zero inputs, one Texture2D output).
//!
//! Same identity-level `wgsl_source` field + `f0..f7` slider params as
//! [`node.wgsl_compute_1tex_1tex`]. The shader contract differs in that
//! there's no input texture or sampler:
//!
//! ```wgsl
//! struct U {
//!     f0: f32, f1: f32, f2: f32, f3: f32,
//!     f4: f32, f5: f32, f6: f32, f7: f32,
//! };
//! @group(0) @binding(0) var<uniform> u: U;
//! @group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;
//!
//! @compute @workgroup_size(16, 16)
//! fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
//!     // ... write output_tex using u.f0 etc ...
//! }
//! ```

#![allow(private_interfaces)]

use std::hash::{Hash, Hasher};

use manifold_gpu::GpuBinding;

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

pub const DEFAULT_WGSL_0IN_1TEX: &str = r"
struct U {
    f0: f32, f1: f32, f2: f32, f3: f32,
    f4: f32, f5: f32, f6: f32, f7: f32,
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var output_tex: texture_storage_2d<rgba16float, write>;

@compute @workgroup_size(16, 16)
fn cs_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let dims = textureDimensions(output_tex);
    if id.x >= dims.x || id.y >= dims.y { return; }
    // Default: write a neutral grey so the node is visibly alive.
    textureStore(output_tex, vec2<i32>(id.xy), vec4<f32>(0.5, 0.5, 0.5, 1.0));
}
";

crate::primitive! {
    name: WgslCompute0In1Tex,
    type_id: "node.wgsl_compute_0in_1tex",
    purpose: "WGSL escape hatch — zero inputs, one Texture2D output. Pure generator. Reserve for irreducible procedural generators (e.g. BlackHole). For ordinary procedural textures the math family (uv_field, distance_to_point, polar_field, simplex/perlin/fbm/voronoi noise, sin/cos/fract on textures) composes the same shapes without raw shader code.",
    inputs: {},
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
    composition_notes: "Bindings: @binding(0) var<uniform> u: U (8 f32 slots); @binding(1) output_tex (rgba16float, write). Entry point cs_main, @workgroup_size(16, 16). No input texture / sampler. The default kernel writes solid 50% grey so a fresh node is visually distinguishable from a dead-output state.",
    examples: [],
    picker: { label: "WGSL Compute (0 in → 1 tex)", category: Atom },
    extra_fields: {
        source: String = DEFAULT_WGSL_0IN_1TEX.to_string(),
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

impl Primitive for WgslCompute0In1Tex {
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
                        "node.wgsl_compute_0in_1tex",
                    );
                    self.pipeline = Some(pipeline);
                    self.compiled_source_hash = Some(current_hash);
                    self.compile_failed = false;
                }
                Err(msg) => {
                    log::warn!(
                        "[node.wgsl_compute_0in_1tex] WGSL validation failed; dispatch skipped:\n{}",
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

        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&slots),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: out_tex,
                },
            ],
            [w.div_ceil(16), h.div_ceil(16), 1],
            "node.wgsl_compute_0in_1tex",
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
    fn wgsl_0in_1tex_declares_zero_inputs_and_one_output() {
        use crate::node_graph::ports::PortType;
        assert_eq!(WgslCompute0In1Tex::TYPE_ID, "node.wgsl_compute_0in_1tex");
        assert!(WgslCompute0In1Tex::INPUTS.is_empty());
        assert_eq!(WgslCompute0In1Tex::OUTPUTS.len(), 1);
        assert_eq!(WgslCompute0In1Tex::OUTPUTS[0].name, "out");
        assert_eq!(WgslCompute0In1Tex::OUTPUTS[0].ty, PortType::Texture2D);
    }

    #[test]
    fn wgsl_0in_1tex_has_eight_float_slots() {
        let names: Vec<&str> = WgslCompute0In1Tex::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["f0", "f1", "f2", "f3", "f4", "f5", "f6", "f7"]);
        for p in WgslCompute0In1Tex::PARAMS {
            assert!(matches!(p.ty, ParamType::Float));
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = WgslCompute0In1Tex::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.wgsl_compute_0in_1tex");
    }

    #[test]
    fn default_source_is_a_valid_wgsl_kernel() {
        assert!(validate_wgsl(DEFAULT_WGSL_0IN_1TEX).is_ok());
    }

    #[test]
    fn set_wgsl_source_round_trips_through_trait() {
        let mut prim = WgslCompute0In1Tex::new();
        let node: &mut dyn EffectNode = &mut prim;
        let custom = "// custom\n";
        node.set_wgsl_source(custom);
        assert_eq!(node.wgsl_source(), Some(custom));
    }
}
