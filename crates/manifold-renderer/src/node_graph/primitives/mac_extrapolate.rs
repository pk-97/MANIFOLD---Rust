//! One six-neighbor layer of MAC velocity extrapolation into invalid faces.
//! Known projected faces are copied exactly; this is not a velocity smoother.
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;
use manifold_gpu::GpuBinding;

pub const FACE_COUNT: u32 = 65 * 65 * 65;
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ExtrapolateUniforms {
    pub dispatch_count: u32,
    pub padding: [u32; 3],
}

pub fn shader_source() -> String {
    crate::node_graph::freeze::codegen::standalone_for_spec::<MacExtrapolate>()
        .expect("MAC extrapolate codegen")
}

crate::primitive! {
    name: MacExtrapolate,
    type_id: "node.mac_extrapolate",
    purpose: "Extend a staggered velocity field by one six-neighbor layer into invalid faces. Valid faces are copied without changing their velocity. Repeating the node supplies the support needed by APIC transfer and particle advection.",
    inputs: { in: Channels["mac_velocity": Vec4F, "mac_valid": Vec4F] required },
    outputs: { out: Channels["mac_velocity": Vec4F, "mac_valid": Vec4F] },
    params: [],
    depth_rule: Terminal,
    composition_notes: "One extrapolation layer, matching the FLIP Fluids six-neighbor extension. Fixed 64-cubed cell grid stored in padded 65-cubed entries; component faces use half-cell offsets in the other axes. Chain five layers at CFL <= 1. Known velocity is never averaged. Pressure validity must be rebuilt after projection.",
    examples: [],
    picker: { label: "MAC Extrapolate", category: Atom },
    summary: "Extends valid MAC face velocities into an adjacent empty layer.",
    category: Particles3D,
    role: Filter,
    aliases: ["MAC velocity extension"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/mac_extrapolate_body.wgsl"),
    input_access: [BufferGather],
    extra_fields: { source: String = shader_source(), },
}
impl Primitive for MacExtrapolate {
    fn array_output_capacity(
        &self,
        port: &str,
        _: &crate::node_graph::effect_node::ParamValues,
        _: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out").then_some(FACE_COUNT)
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let Some(input) = ctx.inputs.array("in") else {
            return;
        };
        let Some(output) = ctx.outputs.array("out") else {
            return;
        };
        if input.size < u64::from(FACE_COUNT) * 32 || output.size < u64::from(FACE_COUNT) * 32 {
            ctx.error("MAC extrapolate requires the full 65-cubed face grid");
            return;
        }
        let u = ExtrapolateUniforms {
            dispatch_count: FACE_COUNT,
            padding: [0; 3],
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device
                .create_compute_pipeline(&self.source, "cs_main", "node.mac_extrapolate")
        });
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
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
            [FACE_COUNT.div_ceil(256), 1, 1],
            "node.mac_extrapolate",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mac_extrapolate_generated_shader_validates() {
        let source = shader_source();
        let module =
            naga::front::wgsl::parse_str(&source).expect("generated MAC extrapolation WGSL");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("valid extrapolation shader");
    }
}
