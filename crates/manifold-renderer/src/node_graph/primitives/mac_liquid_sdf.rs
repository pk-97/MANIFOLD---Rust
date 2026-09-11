use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::primitive::Primitive;
use crate::node_graph::water::WaterParticle;
use manifold_gpu::GpuBinding;

pub fn shader_source() -> String {
    crate::node_graph::freeze::codegen::standalone_for_spec::<MacLiquidSdf>()
        .expect("node.mac_liquid_sdf codegen")
}

crate::primitive! {
    name: MacLiquidSdf,
    type_id: "node.mac_liquid_sdf",
    purpose: "Reconstructs a signed liquid SDF on the 64³ cell-centre lattice from binned water particles and padded MAC open-cell geometry.",
    inputs: {
        particles: Array(WaterParticle) required,
        heads: Array(u32) required,
        next: Array(u32) required,
        geometry: Channels["mac_open": Vec4F] required,
    },
    outputs: { out: Array(f32), },
    params: [],
    depth_rule: Terminal,
    composition_notes: "Consumes node.water_particle_bins linked heads/next (1-based links) and node.mac_box_fractions geometry with exactly 65³ entries. The output is exactly 64³ cell centres; invalid links or non-finite particle data produce NaN.",
    examples: [],
    picker: { label: "MAC Liquid SDF", category: Atom },
    summary: "Builds a signed liquid surface distance field from particles and open-cell volume fractions.",
    category: Particles3D,
    role: Filter,
    aliases: ["liquid sdf", "water sdf", "mac sdf"],
    fusion_kind: Source,
    wgsl_body: include_str!("shaders/mac_liquid_sdf_body.wgsl"),
    input_access: [BufferGather, BufferGather, BufferGather, BufferGather],
    wgsl_includes: [include_str!("shaders/water_common.wgsl")],
    extra_fields: { source: String = shader_source(), },
}

impl Primitive for MacLiquidSdf {
    fn array_output_capacity(
        &self,
        port: &str,
        _: &crate::node_graph::effect_node::ParamValues,
        _: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "out").then_some(64 * 64 * 64)
    }
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let (Some(p), Some(h), Some(n), Some(g), Some(out)) = (
            ctx.inputs.array("particles"),
            ctx.inputs.array("heads"),
            ctx.inputs.array("next"),
            ctx.inputs.array("geometry"),
            ctx.outputs.array("out"),
        ) else {
            return;
        };
        if p.size < 96
            || h.size < 32768 * 4
            || n.size < p.size / 96 * 4
            || g.size < 65 * 65 * 65 * 16
            || out.size < 64 * 64 * 64 * 4
        {
            ctx.error("node.mac_liquid_sdf: insufficient fixed lattice capacity");
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipe = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                &self.source,
                crate::node_graph::freeze::codegen::ENTRY,
                "node.mac_liquid_sdf",
            )
        });
        let u = [262144u32, 0, 0, 0];
        gpu.native_enc.dispatch_compute(
            pipe,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&u),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: p,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: h,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: n,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: g,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: out,
                    offset: 0,
                },
            ],
            [262144u32.div_ceil(256), 1, 1],
            "node.mac_liquid_sdf",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_shader_validates() {
        let s = shader_source();
        let m =
            naga::front::wgsl::parse_str(&s).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&s)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&m)
        .unwrap();
    }
}
