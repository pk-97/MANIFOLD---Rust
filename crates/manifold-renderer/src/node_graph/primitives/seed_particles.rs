//! `node.seed_particles` — emit a freshly-initialised
//! `Array<Particle>` each frame.
//!
//! Phase A.7 of `BUFFER_PORT_PLAN`. The first primitive in the
//! particle family — zero inputs, one Array output, params drive
//! how many particles are alive (`active_count`) and the max
//! buffer capacity (`max_capacity`) the chain build pre-allocates.
//!
//! This V1 minimal version uses a single uniform-random Wang-hash
//! seed pattern. FluidSim 2D's seven legacy patterns (CLT cluster,
//! lines, rings, cross, spiral, edge) port over in a follow-up
//! session — they need a `pattern: Enum` param plus the matching
//! shader branches from `fluid_seed.wgsl`. Active-count semantics
//! are already correct here: the slider sets how many `[0..N)`
//! particles initialise live, the rest sit at dead-center with
//! `life = 0`.
//!
//! Capacity contract: the chain build code reads the `max_capacity`
//! param and pre-binds an `(item_size × max_capacity)`-byte
//! GpuBuffer to the Array output slot via
//! [`MetalBackend::pre_bind_array`]. Editor changes to
//! `max_capacity` trigger a chain rebuild; the active-count
//! slider stays smooth across drags (uniform write only).

use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SeedUniforms {
    active_count: u32,
    capacity: u32,
    seed_offset: u32,
    _pad: u32,
}

crate::primitive! {
    name: SeedParticles,
    type_id: "node.seed_particles",
    purpose: "Emit a fresh Array<Particle> sized by `max_capacity` (chain-build-time ceiling). `active_count` particles initialise alive at Wang-hash uniform positions in [0,1]²; the remaining capacity sits dead at center. Pair with `node.array_feedback` to make the seeded set persist across frames, or wire directly into `node.simulate_particles` to advect on the fly.",
    inputs: {},
    outputs: {
        particles: Array(Particle),
    },
    params: [
        ParamDef {
            name: "max_capacity",
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Int(1_048_576),
            range: Some((1024.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "active_count",
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Int(100_000),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: "seed_offset",
            label: "Seed",
            ty: ParamType::Int,
            default: ParamValue::Int(0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
    ],
    composition_notes: "max_capacity is read by the chain build at allocation time and triggers a rebuild when changed — set it once when authoring the preset. active_count is a free slider; changing it just writes a uniform.",
    examples: [],
    picker: { label: "Seed Particles", category: Atom },
}

impl Primitive for SeedParticles {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = match ctx.params.get("active_count") {
            Some(ParamValue::Int(n)) => (*n).max(0) as u32,
            _ => 100_000,
        };
        let seed_offset = match ctx.params.get("seed_offset") {
            Some(ParamValue::Int(n)) => (*n) as u32,
            _ => 0,
        };

        let Some(out_buf) = ctx.outputs.array("particles") else {
            return;
        };
        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (out_buf.size / particle_size) as u32;
        let active_count = active_count.min(capacity);

        let gpu = ctx.gpu_encoder();
        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/seed_particles.wgsl"),
                "cs_main",
                "node.seed_particles",
            )
        });

        let uniforms = SeedUniforms {
            active_count,
            capacity,
            seed_offset,
            _pad: 0,
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
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [capacity.div_ceil(256), 1, 1],
            "node.seed_particles",
        );
    }
}

#[cfg(test)]
mod tests {
    //! Phase A.7.1 smoke tests. Port shape + param surface.
    //! End-to-end GPU dispatch + buffer readback test lives
    //! with the FluidSim parity work in Phase A.8 since both
    //! need the same shared-mode buffer readback helper.

    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn seed_particles_declares_zero_inputs_and_one_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType};

        let particle_layout = ArrayType {
            item_size: std::mem::size_of::<Particle>() as u32,
            item_align: std::mem::align_of::<Particle>() as u32,
        };

        assert_eq!(SeedParticles::TYPE_ID, "node.seed_particles");
        assert!(SeedParticles::INPUTS.is_empty());
        assert_eq!(SeedParticles::OUTPUTS.len(), 1);
        assert_eq!(SeedParticles::OUTPUTS[0].name, "particles");
        assert_eq!(
            SeedParticles::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );
    }

    #[test]
    fn seed_particles_has_max_capacity_active_count_and_seed_params() {
        let names: Vec<&str> = SeedParticles::PARAMS.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["max_capacity", "active_count", "seed_offset"]);

        let max_cap = SeedParticles::PARAMS
            .iter()
            .find(|p| p.name == "max_capacity")
            .unwrap();
        assert!(matches!(max_cap.ty, ParamType::Int));
        if let ParamValue::Int(default) = max_cap.default {
            assert_eq!(default, 1_048_576, "default capacity is 1M");
        } else {
            panic!("max_capacity default should be Int");
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SeedParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.seed_particles");
    }
}
