//! Single-dispatch scalar lattice operation; see GPU_FLUID_SURFACE_DESIGN.md, Fill Pits.
use crate::float_param;
use crate::primitives::standalone_pipeline::standalone_pipeline;
use crate::exec::effect_node::{EffectNodeContext, ParamValues};
use crate::parameters::{ParamDef, ParamType, ParamValue};
use crate::primitive::Primitive;
use manifold_gpu::GpuBinding;
use std::borrow::Cow;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    offset: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: OffsetLattice,
    type_id: "node.offset_lattice",
    purpose: "Add a distance to every scalar lattice sample. Zero returns the input bits unchanged. Negative offsets grow a negative-inside surface; positive offsets shrink it.",
    inputs: {
        levelset: Array(f32) required,
        offset: ScalarF32 optional,
    },
    outputs: { out: Array(f32), },
    params: [
        float_param!("offset", "Offset", 0.0, -1.0, 1.0),
    ],
    depth_rule: Terminal,
    composition_notes: "For morphological closing, offset by minus the grow distance, redistance with a band wider than the grow distance, then offset back by the grow distance before clipping to solids. Never substitute a blur for redistancing. The input band must cover the grown surface.",
    examples: [],
    picker: { label: "Offset Lattice", category: Atom },
    summary: "Add a distance to every scalar lattice sample. Zero returns the input bits unchanged. Negative offsets grow a negative-inside surface; positive offsets shrink it.",
    category: Particles3D,
    role: Filter,
    aliases: ["distance field", "morphological closing"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/offset_lattice_body.wgsl"),
    input_access: [Coincident],
}

impl Primitive for OffsetLattice {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "levelset")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(input), Some(output)) = (ctx.inputs.array("levelset"), ctx.outputs.array("out"))
        else {
            return;
        };
        let count = (input.size.min(output.size) / 4) as u32;
        if count == 0 {
            return;
        }
        let uniforms = Uniforms {
            offset: ctx.scalar_or_param("offset", 0.0),
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: input,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: output,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.offset_lattice",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(crate) mod extent;
