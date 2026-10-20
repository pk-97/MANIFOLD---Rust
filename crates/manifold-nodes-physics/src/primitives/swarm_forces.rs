//! `node.swarm_forces` — a swarm's fixed simulation clock and the scene's
//! force field laid onto a lattice over its box, once per tick, for
//! `node.move_swarm`. The same `acceleration_field` wire water and rigid
//! bodies take, so one scene force list moves all three.

use std::borrow::Cow;

use manifold_gpu::GpuBuffer;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_physics::FieldValue;
use manifold_physics::clock::{LiveLoad, SimulationClock};
use manifold_physics::force_lattice::{ForceLattice, TickForces};

use crate::force_lattice::{LatticeBuffers, LatticeUpload};

pub const SWARM_FORCES_TYPE_ID: &str = "node.swarm_forces";

/// Force lattices the buffer keeps room for: a live frame's ticks.
const RESERVE_LATTICES: usize = 3;
/// The most lattice nodes one tick lays out on the CPU.
const MAX_LATTICE_NODES: usize = 262_144;

const OUTPUTS: [&str; 13] = [
    "ticks", "first_tick", "epoch", "tick_seconds", "force_lattices",
    "field_nodes_x", "field_nodes_y", "field_nodes_z", "field_spacing",
    "field_min_x", "field_min_y", "field_min_z", "simulation_time",
];

const fn float(name: &'static str, label: &'static str, default: f32, range: (f32, f32)) -> ParamDef {
    ParamDef { name: Cow::Borrowed(name), label, ty: ParamType::Float, default: ParamValue::Float(default), range: Some(range), enum_values: &[] }
}

manifold_node_engine::primitive! {
    name: SwarmForces,
    type_id: "node.swarm_forces",
    purpose: "Run a swarm's fixed simulation clock (the same ticks water runs, so live and export match) and lay the scene's acceleration field onto a lattice over the swarm's box at each tick's start: one lattice while the frame's ticks agree, one per tick otherwise. Publishes the frame's ticks, the field scalars and the forces buffer node.move_swarm reads. Paused or at Speed 0 the clock holds and runs no ticks.",
    inputs: {
        bounds: Transform optional,
        acceleration_field: VectorField optional,
        speed: ScalarF32 optional,
    },
    outputs: {
        ticks: ScalarF32,
        first_tick: ScalarF32,
        epoch: ScalarF32,
        tick_seconds: ScalarF32,
        force_lattices: ScalarF32,
        field_nodes_x: ScalarF32, field_nodes_y: ScalarF32, field_nodes_z: ScalarF32,
        field_spacing: ScalarF32,
        field_min_x: ScalarF32, field_min_y: ScalarF32, field_min_z: ScalarF32,
        simulation_time: ScalarF32,
        forces: Array(f32),
    },
    params: [
        float("field_spacing", "Force Detail", 0.25, (0.02, 10.0)),
        float("speed", "Speed", 1.0, (0.0, 4.0)),
    ],
    depth_rule: Terminal,
    composition_notes: "The swarm's source of truth. Every output but forces and simulation_time wires into node.move_swarm's port of the same name, forces into its forces. Scene forces arrive on acceleration_field (the scene force list expands into it, as for a liquid domain). The bounds transform (position and scale) is the box forces are laid out over; a particle outside it reads the nearest edge of the lattice. Force Detail is the lattice spacing in metres.",
    examples: ["Swarm"],
    picker: { label: "Swarm Forces", category: Atom },
    summary: "Runs a swarm's simulation clock and lays the scene's forces over its box.",
    category: Particles3D,
    role: Source,
    aliases: ["swarm", "swarm forces", "swarm clock", "force field"],
    boundary_reason: NonGpu,
    extra_fields: {
        clock: SimulationClock = SimulationClock::default(),
        forces: TickForces = TickForces::default(),
        buffers: LatticeBuffers = LatticeBuffers::default(),
        published: [f32; OUTPUTS.len()] = [0.0; OUTPUTS.len()],
    },
}

impl SwarmForces {
    /// Each not-yet-run tick's start in `(from, until]`, for the physics
    /// history replay.
    pub fn request_physics_samples(&mut self, from: f64, until: f64, out: &mut Vec<f64>) {
        self.forces.request_samples(&self.clock, from, until, out);
    }

    fn lattice(ctx: &EffectNodeContext<'_, '_>) -> Result<ForceLattice, String> {
        // The bounds transform places and sizes the box; unwired, a 4 m cube
        // standing on the floor.
        let (center, size) = ctx.inputs.transform("bounds").map_or(([0.0, 2.0, 0.0], [4.0; 3]), |t| (t.pos, t.scale.map(f32::abs)));
        let spacing = ctx.scalar_or_param("field_spacing", 0.25);
        if !(spacing.is_finite() && spacing > 0.0) || size.iter().chain(&center).any(|v| !v.is_finite()) || size.iter().any(|&s| s <= 0.0) {
            return Err("Swarm Forces: the bounds and Force Detail must be finite and positive".into());
        }
        let lattice = ForceLattice::over_box(std::array::from_fn(|i| center[i] - 0.5 * size[i]), size, spacing);
        if lattice.node_count() > MAX_LATTICE_NODES {
            return Err(format!(
                "Swarm Forces: the force lattice needs {} nodes, over {MAX_LATTICE_NODES}; raise Force Detail or shrink the box",
                lattice.node_count()
            ));
        }
        Ok(lattice)
    }

    fn compute(&mut self, ctx: &mut EffectNodeContext<'_, '_>) -> Result<[f32; OUTPUTS.len()], String> {
        let lattice = Self::lattice(ctx)?;
        let field = ctx.inputs.cpu_value::<FieldValue>("acceleration_field");
        let frame = self.clock.advance(
            ctx.time.seconds.0,
            ctx.sim_step.interval.0,
            ctx.scalar_or_param("speed", 1.0),
            0.0,
            false,
            ctx.sim_step.offline(),
        );
        if frame.numerical_error {
            ctx.sim_metrics.record_simulation(0.0, 0.0, false, true);
        }
        self.forces.prepare(lattice, field.as_ref(), &self.clock, &frame)?;
        let [nx, ny, nz] = lattice.nodes().map(|n| n as f32);
        let min = lattice.min();
        Ok([
            frame.ticks as f32,
            frame.first_sequence as f32,
            frame.epoch as f32,
            ctx.sim_step.interval.0 as f32,
            self.forces.lattices() as f32,
            nx, ny, nz,
            lattice.spacing(),
            min[0], min[1], min[2],
            frame.simulation_time as f32,
        ])
    }

    fn upload(&mut self, ctx: &mut EffectNodeContext<'_, '_>) -> Result<(), String> {
        let lattice = Self::lattice(ctx)?;
        let offline = ctx.sim_step.offline();
        let Some(gpu) = ctx.gpu.as_deref_mut() else { return Ok(()) };
        let data = LatticeUpload {
            lattice,
            reserve_lattices: RESERVE_LATTICES,
            forces: self.forces.forces(),
            forces_dirty: self.forces.is_dirty(),
            impulses: &[],
            impulses_dirty: false,
            hits: &[],
        };
        self.buffers.upload(gpu, offline, data)?;
        self.forces.mark_uploaded();
        Ok(())
    }
}

impl Primitive for SwarmForces {
    fn provides_array_output(&self, port: &str) -> bool {
        port == "forces"
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        (port == "forces").then(|| self.buffers.forces()).flatten()
    }

    fn array_output_capacity(&self, port_name: &str, _params: &ParamValues, _input_capacities: &[(&str, u32)]) -> Option<u32> {
        (port_name == "forces").then_some(1)
    }

    fn clear_state(&mut self) {
        self.clock.restart();
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.clock.set_live_load(ctx.sim_step.load().map(|load| LiveLoad { previous: load.previous, budget: load.budget }));
        // A physics sample reads the field at a tick's start; it never
        // advances time.
        if ctx.sim_step.authored_sample_only {
            self.clock.observe_speed(ctx.time.seconds.0, ctx.scalar_or_param("speed", 1.0));
            let field = ctx.inputs.cpu_value::<FieldValue>("acceleration_field");
            self.forces.observe_sample(ctx.time.seconds.0, field.as_ref());
            return;
        }
        // On an error the swarm holds: the last outputs repeat with no ticks.
        let result = self.compute(ctx).and_then(|values| self.upload(ctx).map(|()| values));
        match result {
            Ok(values) => self.published = values,
            Err(error) => {
                self.published[0] = 0.0;
                ctx.error(error);
            }
        }
        for (name, value) in OUTPUTS.iter().zip(self.published) {
            ctx.outputs.set_scalar(name, ParamValue::Float(value));
        }
    }
}
