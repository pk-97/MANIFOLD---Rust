//! `node.swarm_to_copies` — one copy transform per swarm particle, so a
//! swarm draws as copies of a mesh.

use std::borrow::Cow;

use manifold_gpu::GpuBinding;
use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::InstanceTransform;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::particles::Particle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct Uniforms {
    pub(crate) size: f32,
    pub(crate) dispatch_count: u32,
    pub(crate) _pad: [u32; 2],
}

manifold_node_engine::primitive! {
    name: SwarmToCopies,
    type_id: "node.swarm_to_copies",
    purpose: "Turn each 3D particle into a copy transform: positioned at the particle, uniformly scaled by `size`, unrotated. Dead particles (life <= 0) become zero-scale holes.",
    inputs: {
        particles: Array(Particle) required,
        size: ScalarF32 optional,
    },
    outputs: {
        copies: Array(InstanceTransform),
    },
    params: [
        ParamDef { name: Cow::Borrowed("size"), label: "Size", ty: ParamType::Float, default: ParamValue::Float(0.05), range: Some((0.0, 10.0)), enum_values: &[] },
    ],
    depth_rule: Terminal,
    composition_notes: "The swarm's draw step: particles from node.move_swarm, copies into render_copies or a scene object's instances. Output capacity follows particles. Liquid particles (FluidParticle) use node.particles_to_copies instead.",
    examples: ["Swarm"],
    picker: { label: "Swarm To Copies", category: Atom },
    summary: "Places a copy of a shape at every swarm particle.",
    category: Particles3D,
    role: Map,
    aliases: ["swarm copies", "particles to copies 3d", "particle instances", "draw swarm"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/swarm_to_copies_body.wgsl"),
}

impl Primitive for SwarmToCopies {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        (port_name == "copies").then(|| input_capacities.iter().find(|(p, _)| *p == "particles").map(|&(_, n)| n)).flatten()
    }

    /// A copy stands where its particle is: a force on the copies moves the
    /// particles.
    fn physical_sources(&self, output: &str) -> Option<&'static [&'static str]> {
        (output == "copies").then_some(&["particles"])
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let size = ctx.scalar_or_param("size", 0.05);
        let (Some(particles), Some(copies)) = (ctx.inputs.array("particles"), ctx.outputs.array("copies")) else {
            return;
        };
        let count = ((particles.size / std::mem::size_of::<Particle>() as u64) as u32)
            .min((copies.size / std::mem::size_of::<InstanceTransform>() as u64) as u32);
        if count == 0 {
            return;
        }
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let uniforms = Uniforms { size, dispatch_count: count, _pad: [0; 2] };
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&uniforms) },
                GpuBinding::Buffer { binding: 1, buffer: particles, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: copies, offset: 0 },
            ],
            [count.div_ceil(256), 1, 1],
            "node.swarm_to_copies",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swarm_to_copies_generates_a_valid_map_kernel() {
        let wgsl = manifold_node_engine::freeze::codegen::standalone_for_spec::<SwarmToCopies>().expect("swarm_to_copies codegen");
        let module = naga::front::wgsl::parse_str(&wgsl).unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&wgsl)));
        assert!(wgsl.contains("size: f32,\n    dispatch_count: u32,"), "{wgsl}");
        assert_eq!(std::mem::size_of::<Uniforms>(), 16);
    }
}
