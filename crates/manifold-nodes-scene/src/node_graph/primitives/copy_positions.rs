//! `node.copy_positions` — extract instance positions as homogeneous Vec4s.

use manifold_gpu::GpuBinding;

use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use manifold_node_engine::mesh::{InstanceTransform, Vec4Vertex};
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::primitive::Primitive;

manifold_core::testkit_visible! {
    testkit {
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Uniforms {
    pub dispatch_count: u32,
    pub _pad0: u32,
    pub _pad1: u32,
    pub _pad2: u32,
}
    }
    production {
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}
    }
}

manifold_node_engine::primitive! {
    name: CopyPositions,
    type_id: "node.copy_positions",
    purpose: "Extract each InstanceTransform's world position into an Array<Vec4Vertex>. The output is (pos_scale.x, pos_scale.y, pos_scale.z, 1), so it can feed point-field atoms while ignoring scale, rotation, and marker data.",
    inputs: {
        instances: Array(InstanceTransform) required,
    },
    outputs: {
        out: Array(Vec4Vertex),
    },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Output capacity follows `instances`. Every source slot, including inactive zero-scale holes, produces its position with homogeneous w=1; this is a positional view rather than a liveness filter. Pair with node.wave_field_3d for a point-sampled mathematical field.",
    examples: [],
    picker: { label: "Copy Positions", category: Atom },
    summary: "Turns copy transforms into homogeneous XYZ positions for downstream fields and geometry math.",
    category: Geometry3D,
    role: Map,
    aliases: ["copy positions", "instance positions", "positions"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/copy_positions_body.wgsl"),
}

impl Primitive for CopyPositions {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        input_capacities
            .iter()
            .find(|(name, _)| *name == "instances")
            .map(|(_, capacity)| *capacity)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(instances) = ctx.inputs.array("instances") else {
            return;
        };
        let Some(out) = ctx.outputs.array("out") else {
            return;
        };
        let instance_size = std::mem::size_of::<InstanceTransform>() as u64;
        let vertex_size = std::mem::size_of::<Vec4Vertex>() as u64;
        let count = ((instances.size / instance_size) as u32).min((out.size / vertex_size) as u32);
        if count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = Uniforms {
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: instances,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: out,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.copy_positions",
        );
    }
}
