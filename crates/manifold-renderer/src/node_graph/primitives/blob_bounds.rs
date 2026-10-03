//! Exact bounds for indexed blob gathers. Shared by the field and its sparse
//! schedule; spatial bins never impose a radius or quality limit.
use manifold_gpu::{GpuBinding, GpuComputePipeline};
use crate::node_graph::effect_node::{EffectNodeContext, ParamValues};
use crate::node_graph::fluid_particles::FluidBlob;
use crate::node_graph::primitive::Primitive;

const SHADER: &str = include_str!("shaders/blob_bounds.wgsl");

crate::primitive! {
    name: BlobBounds,
    type_id: "node.blob_bounds",
    purpose: "Reduce surface blobs to two exact conservative bounds: the largest kernel axis, and the largest 1.5-axis support plus centre displacement from the sorted particle. A barriered maximum reduction; no size cap or atomic operations.",
    inputs: { blobs: Array(FluidBlob) required, },
    outputs: { bounds: Array(f32), },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Wire the same shaped blobs into this node, particle_volume and lattice_bricks. Both consumers use these two words to search all bins that may contain an influencing blob, independently of bin width. Unwired consumers compute the same maximum directly; this node shares the reduction across lattice samples.",
    examples: [],
    picker: { label: "Blob Bounds", category: Atom },
    summary: "Measures kernel reach for exact particle surface searches.",
    category: Particles3D,
    role: Filter,
    aliases: ["kernel bounds", "surface support"],
    boundary_reason: BarrieredReduction,
    extra_fields: { reduction: Option<GpuComputePipeline> = None, },
}

impl Primitive for BlobBounds {
    fn prepare_pipelines(&mut self, device: &manifold_gpu::GpuDevice) {
        self.reduction.get_or_insert_with(|| device.create_compute_pipeline(SHADER, "main", "node.blob_bounds"));
    }
    fn array_output_capacity(&self, port: &str, _: &ParamValues, _: &[(&str, u32)]) -> Option<u32> {
        (port == "bounds").then_some(2)
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(blobs), Some(bounds)) = (ctx.inputs.array("blobs"), ctx.outputs.array("bounds")) else { return };
        let count = (blobs.size / size_of::<FluidBlob>() as u64) as u32;
        let params = [count, 0, 0, 0];
        let gpu = ctx.gpu_encoder();
        gpu.native_enc.dispatch_compute(self.reduction.as_ref().expect("installed blob bounds"), &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::cast_slice(&params) },
            GpuBinding::Buffer { binding: 1, buffer: blobs, offset: 0 },
            GpuBinding::Buffer { binding: 2, buffer: bounds, offset: 0 },
        ], [1, 1, 1], "node.blob_bounds");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn liquid_blob_bounds_shader_validates() {
        let module = naga::front::wgsl::parse_str(super::SHADER).expect("blob bounds WGSL");
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module).expect("blob bounds validates");
    }
}
