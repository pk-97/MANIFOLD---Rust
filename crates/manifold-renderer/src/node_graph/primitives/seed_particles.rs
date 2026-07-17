//! `node.spawn_particles` — emit a freshly-initialised
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

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::generators::compute_common::Particle;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::state_store::NodeState;

/// `seed_mode` enum labels.
/// `EveryFrame` (0): rewrite the buffer each frame — the legacy
///   behaviour, suited to "spawning rain" advection pipelines where
///   the integrator only deflects per-frame and never accumulates
///   state.
/// `OnceOnReset` (1): seed once after a state-store reset (project
///   load, layer resume, seek). Subsequent frames are no-ops, so the
///   buffer persists whatever downstream simulators (integrate,
///   attractor, scatter) write into it. Required for any sim that
///   wants particle state to evolve across frames.
pub const SEED_MODES: &[&str] = &["EveryFrame", "OnceOnReset"];

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
    type_id: "node.spawn_particles",
    purpose: "Emit a fresh Array<Particle> sized by `max_capacity` (chain-build-time ceiling). `active_count` particles initialise alive at Wang-hash uniform positions in [0,1]²; the remaining capacity sits dead at center. `seed_mode` picks when the seed kernel fires: EveryFrame (legacy — overwrite each tick; suited to advection 'rain' effects where the integrator never accumulates state) or OnceOnReset (seed once after a state-store reset; the buffer persists whatever the downstream sim writes into it — required for any sim where particle state must evolve across frames, e.g. StrangeAttractor + FluidSim2D).",
    inputs: {
        active_count: ScalarF32 optional,
    },
    outputs: {
        particles: Array(Particle),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Max Capacity",
            ty: ParamType::Int,
            default: ParamValue::Float(1_048_576.0),
            range: Some((1024.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("active_count"),
            label: "Active Count",
            ty: ParamType::Int,
            default: ParamValue::Float(100_000.0),
            range: Some((0.0, 16_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("seed_offset"),
            label: "Seed",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("seed_mode"),
            label: "Seed Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0), // EveryFrame — legacy default
            range: None,
            enum_values: SEED_MODES,
        },
    ],
    depth_rule: Terminal,
    composition_notes: "max_capacity is read by the chain build at allocation time and triggers a rebuild when changed — set it once when authoring the preset. active_count is a free slider (port-shadowed). Pick `seed_mode = OnceOnReset` for any pipeline where the buffer must persist across frames so the downstream simulator can accumulate state; pick `EveryFrame` for advection-style effects where each frame starts from a fresh random scatter.",
    examples: [],
    picker: { label: "Spawn Particles", category: Atom },
    summary: "Creates a fresh batch of particles to start a simulation, with a count you set. The first node in a particle chain.",
    category: Particles2D,
    role: Source,
    aliases: ["spawn particles", "seed particles", "seed", "emit", "birth"],
    boundary_reason: BarrieredReduction,
}

/// Persistent state for `seed_mode = OnceOnReset` — tracks whether
/// we've already seeded this (node, owner). Cleared by the runtime
/// on any StateStore reset (layer resume, seek, project load) so the
/// next frame re-seeds.
struct SeedParticlesState {
    has_seeded: bool,
}

impl NodeState for SeedParticlesState {}

impl Primitive for SeedParticles {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let active_count = ctx
            .scalar_or_param("active_count", 100_000.0)
            .round()
            .max(0.0) as u32;
        let seed_offset = match ctx.params.get("seed_offset") {
            Some(ParamValue::Float(n)) => n.round() as u32,
            _ => 0,
        };
        let seed_mode = match ctx.params.get("seed_mode") {
            Some(ParamValue::Enum(n)) => *n,
            _ => 0,
        };

        let Some(out_buf) = ctx.outputs.array("particles") else {
            return;
        };
        let particle_size = std::mem::size_of::<Particle>() as u64;
        let capacity = (out_buf.size / particle_size) as u32;
        let active_count = active_count.min(capacity);

        let node_id = ctx.node_id;
        let owner_key = ctx.owner_key;

        // Split-borrow `gpu` and `state` directly so we can both
        // dispatch and update state in one pass (mirror of array_feedback).
        ctx.mark_gpu_accessed();
        let gpu = ctx
            .gpu
            .as_deref_mut()
            .expect("SeedParticles::run requires a GpuEncoder");
        let mut state = ctx.state.as_deref_mut();

        // OnceOnReset: skip the dispatch if we've already seeded this
        // (node, owner). State is cleared by the runtime on any
        // StateStore reset, so the next frame re-seeds.
        if seed_mode == 1 {
            let already_seeded = match state.as_deref_mut() {
                Some(store) => store
                    .get::<SeedParticlesState>(node_id, owner_key)
                    .is_some_and(|s| s.has_seeded),
                None => false,
            };
            if already_seeded {
                return;
            }
        }

        let pipeline = self.pipeline.get_or_insert_with(|| {
            gpu.device.create_compute_pipeline(
                include_str!("shaders/seed_particles.wgsl"),
                "cs_main",
                "node.spawn_particles",
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
            "node.spawn_particles",
        );

        // Record that we seeded so OnceOnReset skips subsequent frames.
        if seed_mode == 1
            && let Some(store) = state
        {
            store.insert(
                node_id,
                owner_key,
                SeedParticlesState { has_seeded: true },
            );
        }
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
    fn seed_particles_declares_active_count_port_shadow_and_one_array_output() {
        use crate::node_graph::ports::{ArrayType, PortType, ScalarType};

        let particle_layout = ArrayType::of_known::<Particle>();

        assert_eq!(SeedParticles::TYPE_ID, "node.spawn_particles");
        // Port-shadow on active_count so a math chain (e.g.
        // count_m × 1_000_000) can drive the count at runtime.
        let active_in = SeedParticles::INPUTS
            .iter()
            .find(|p| p.name == "active_count")
            .expect("active_count port-shadow input must exist");
        assert_eq!(active_in.ty, PortType::Scalar(ScalarType::F32));
        assert!(!active_in.required);

        assert_eq!(SeedParticles::OUTPUTS.len(), 1);
        assert_eq!(SeedParticles::OUTPUTS[0].name, "particles");
        assert_eq!(
            SeedParticles::OUTPUTS[0].ty,
            PortType::Array(particle_layout)
        );
    }

    #[test]
    fn seed_particles_has_full_param_surface_including_seed_mode() {
        let names: Vec<&str> = SeedParticles::PARAMS
            .iter()
            .map(|p| p.name.as_ref())
            .collect();
        assert_eq!(
            names,
            vec!["max_capacity", "active_count", "seed_offset", "seed_mode"],
        );

        let max_cap = SeedParticles::PARAMS
            .iter()
            .find(|p| p.name == "max_capacity")
            .unwrap();
        assert!(matches!(max_cap.ty, ParamType::Int));
        if let ParamValue::Float(default) = max_cap.default {
            assert_eq!(default as u32, 1_048_576, "default capacity is 1M");
        } else {
            panic!("max_capacity default should be Float (Int presentation hint)");
        }

        let mode = SeedParticles::PARAMS
            .iter()
            .find(|p| p.name == "seed_mode")
            .unwrap();
        assert_eq!(mode.ty, ParamType::Enum);
        // Default is EveryFrame (0) for backward compat with legacy
        // advection presets — only the new attractor / fluid sims need
        // OnceOnReset.
        assert!(matches!(mode.default, ParamValue::Enum(0)));
        assert_eq!(mode.enum_values, &["EveryFrame", "OnceOnReset"]);
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = SeedParticles::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.spawn_particles");
    }
}
