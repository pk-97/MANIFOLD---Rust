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
use crate::tick_sampled::{TickSampled, TickSampledRegistration};

/// Force Detail's top: 64³ lattice nodes laid on the CPU per tick at most.
const MAX_FORCE_DETAIL: f32 = 64.0;

const OUTPUTS: [&str; 13] = [
    "ticks", "first_tick", "epoch", "tick_seconds", "force_lattices",
    "field_nodes_x", "field_nodes_y", "field_nodes_z", "field_spacing",
    "field_min_x", "field_min_y", "field_min_z", "simulation_time",
];

const fn param(name: &'static str, label: &'static str, ty: ParamType, default: f32, range: (f32, f32)) -> ParamDef {
    ParamDef { name: Cow::Borrowed(name), label, ty, default: ParamValue::Float(default), range: Some(range), enum_values: &[] }
}

manifold_node_engine::primitive! {
    name: SwarmForces,
    type_id: "node.swarm_forces",
    purpose: "Run a swarm's fixed simulation clock (the same ticks water runs, so live and export match) and lay the scene's acceleration field onto a lattice over the swarm's box at each tick's start: one lattice while the frame's ticks agree, one per tick otherwise. Publishes the frame's ticks, each tick's simulated seconds (Speed scales them), the clock's epoch, the field scalars and the forces buffer node.move_swarm reads. Paused or at Speed 0 the clock holds and runs no ticks. A backward seek, a loop, a reset edge or a Sim Rate edit restarts the clock in a new epoch.",
    inputs: {
        bounds: Transform optional,
        acceleration_field: VectorField optional,
        speed: ScalarF32 optional,
        reset: ScalarF32 optional,
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
        param("force_detail", "Force Detail", ParamType::Int, 17.0, (2.0, MAX_FORCE_DETAIL)),
        param("speed", "Speed", ParamType::Float, 1.0, (0.0, 4.0)),
    ],
    depth_rule: Terminal,
    composition_notes: "The swarm's source of truth. Every output but epoch, forces and simulation_time wires into node.move_swarm's port of the same name, forces into its forces. Wire epoch into the particle loop's node.array_feedback reset_trigger (and the seeder's), so every clock restart reseeds the particles: live after a loop matches an export. Wire a trigger counter into reset to restart on a clip trigger. Scene forces arrive on acceleration_field (the scene force list expands into it, as for a liquid domain). The bounds transform (position and scale) is the box forces are laid out over; a particle outside it reads the nearest edge of the lattice. Force Detail is the lattice nodes along the box's longest side; every value in its range fits any box.",
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

impl TickSampled for SwarmForces {
    fn request_physics_samples(&mut self, from: f64, until: f64, out: &mut Vec<f64>) {
        self.forces.request_samples(&self.clock, from, until, out);
    }
}

inventory::submit! { TickSampledRegistration::new::<SwarmForces>() }

impl SwarmForces {
    fn lattice(ctx: &EffectNodeContext<'_, '_>) -> Result<ForceLattice, String> {
        // The bounds transform places and sizes the box; unwired, a 4 m cube
        // standing on the floor.
        let (center, size) = ctx.inputs.transform("bounds").map_or(([0.0, 2.0, 0.0], [4.0; 3]), |t| (t.pos, t.scale.map(f32::abs)));
        if size.iter().chain(&center).any(|v| !v.is_finite()) || size.iter().any(|&s| s <= 0.0) {
            return Err("Swarm Forces: the bounds box must be finite with every side above zero".into());
        }
        let detail = ctx.scalar_or_param("force_detail", 17.0).round().clamp(2.0, MAX_FORCE_DETAIL) as u32;
        Ok(ForceLattice::over_box(std::array::from_fn(|i| center[i] - 0.5 * size[i]), size, detail))
    }

    fn compute(&mut self, ctx: &mut EffectNodeContext<'_, '_>) -> Result<[f32; OUTPUTS.len()], String> {
        let lattice = Self::lattice(ctx)?;
        let field = ctx.inputs.cpu_value::<FieldValue>("acceleration_field");
        let frame = self.clock.advance(
            ctx.time.seconds.0,
            ctx.sim_step.interval.0,
            ctx.scalar_or_param("speed", 1.0),
            ctx.scalar_or_param("reset", 0.0),
            false,
            ctx.sim_step.offline(),
        );
        if frame.numerical_error {
            ctx.sim_metrics.record_simulation(0.0, 0.0, false, true);
        }
        self.forces.prepare(lattice, field.as_ref(), &self.clock, &frame)?;
        // Speed changes each tick's simulated length, not the tick count. A
        // frame without ticks reports one Sim Rate interval, as water does.
        let tick_seconds = if frame.ticks == 0 { ctx.sim_step.interval.0 } else { frame.duration().0 };
        let [nx, ny, nz] = lattice.nodes().map(|n| n as f32);
        let min = lattice.min();
        Ok([
            frame.ticks as f32,
            frame.first_sequence as f32,
            frame.epoch as f32,
            tick_seconds as f32,
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

    fn acceleration_input(&self) -> Option<&'static str> {
        Some("acceleration_field")
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

/// The clock and lattices through the history replay, with no GPU: what
/// node.move_swarm would be told each frame.
#[cfg(test)]
mod tests {
    use super::*;
    use manifold_foundation::{Beats, Seconds};
    use manifold_node_engine::bindings::{NodeInputs, NodeOutputs, Slot};
    use manifold_node_engine::exec::backend::{Backend, MockBackend};
    use manifold_node_engine::exec::effect_node::{EffectNode, FrameTime, SimStep};
    use manifold_node_engine::exec::execution_plan::ResourceId;
    use manifold_node_engine::ports::PortType;
    use manifold_physics::VectorField;
    use manifold_physics::clock::TICK;

    // manifold-water-rigid registers the field wire type in the app; this
    // test binary does not link it.
    inventory::submit! { manifold_node_engine::exec::cpu_values::CpuWireRegistration::new::<FieldValue>() }

    /// One frame's published outputs, by name.
    #[derive(Clone, Copy, Debug, PartialEq)]
    struct Frame([f32; OUTPUTS.len()]);

    impl Frame {
        fn get(&self, name: &str) -> f32 {
            self.0[OUTPUTS.iter().position(|output| *output == name).expect("an output")]
        }
    }

    /// The authored field at a transport time.
    type Field = fn(f64) -> FieldValue;

    struct Rig {
        node: SwarmForces,
        backend: MockBackend,
        field: Option<(Slot, Field)>,
        step: SimStep,
    }

    impl Rig {
        fn new(field: Option<Field>, step: SimStep) -> Self {
            let mut backend = MockBackend::new();
            let field = field.map(|at| (backend.acquire(ResourceId(1), PortType::VectorField, None, (0, 0)), at));
            Self { node: SwarmForces::new(), backend, field, step }
        }

        fn run(&mut self, seconds: f64, params: &[(&'static str, f32)], step: SimStep) -> Vec<String> {
            let params: ParamValues = params.iter().map(|&(name, value)| (Cow::Borrowed(name), ParamValue::Float(value))).collect();
            if let Some((slot, at)) = self.field {
                self.backend.cpu_values_mut().set(slot, at(seconds));
            }
            let bindings: Vec<(&'static str, Slot)> = self.field.map(|(slot, _)| ("acceleration_field", slot)).into_iter().collect();
            let inputs = NodeInputs::new(&bindings, &self.backend, &[]);
            let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let outputs = NodeOutputs::new(&[], &self.backend, &mut a, &mut b, &mut c, &mut d, &mut e, &mut f, &mut g, &mut h);
            let time = FrameTime { beats: Beats(0.0), seconds: Seconds(seconds), delta: Seconds(TICK), frame_count: 0 };
            let mut errors = Vec::new();
            let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None).with_sim_step(step).with_errors(&mut errors);
            Primitive::run(&mut self.node, &mut ctx);
            errors
        }

        /// The host's frame: the history replay samples each coming tick's
        /// start, then the frame runs.
        fn frame(&mut self, from: f64, until: f64, params: &[(&'static str, f32)]) -> Frame {
            let mut starts = Vec::new();
            self.node.request_physics_samples(from, until, &mut starts);
            for start in starts {
                assert!(self.run(start, params, self.step.authored_sample()).is_empty());
            }
            let errors = self.run(until, params, self.step);
            assert!(errors.is_empty(), "{errors:?}");
            Frame(self.node.published)
        }

        /// Frames at `fps` over `[0, seconds]`; every frame's outputs and
        /// force lattices, the lattices read per tick.
        fn play(&mut self, fps: f64, seconds: f64, from: f64, params: &[(&'static str, f32)]) -> (Vec<Frame>, Vec<Vec<[f32; 4]>>) {
            let (mut frames, mut ticks) = (Vec::new(), Vec::new());
            let mut previous = from;
            for index in 0..=(seconds * fps).round() as u64 {
                let now = from + index as f64 / fps;
                let frame = self.frame(previous, now, params);
                previous = now;
                let lattice_len = self.node.forces.forces().len() / self.node.forces.lattices().max(1);
                for tick in 0..frame.get("ticks") as usize {
                    let slot = tick.min(self.node.forces.lattices().saturating_sub(1));
                    ticks.push(self.node.forces.forces().get(slot * lattice_len..(slot + 1) * lattice_len).unwrap_or(&[]).to_vec());
                }
                frames.push(frame);
            }
            (frames, ticks)
        }
    }

    fn total(frames: &[Frame], name: &str) -> f64 {
        frames.iter().map(|frame| f64::from(frame.get(name))).sum()
    }

    fn vortex(_: f64) -> FieldValue {
        FieldValue::vortex([0.0, 2.0, 0.0], [0.0, 1.0, 0.0], 2.0, 1.0).unwrap()
    }

    /// A vortex that grows with transport time: every tick has its own field.
    fn swelling(seconds: f64) -> FieldValue {
        vortex(seconds).scaled(1.0 + seconds as f32).unwrap()
    }

    #[test]
    fn swarm_ticks_and_lattices_match_at_30_and_120_fps() {
        let step = SimStep::live(Seconds(TICK));
        let mut slow = Rig::new(Some(swelling), step);
        let mut fast = Rig::new(Some(swelling), step);
        let (slow_frames, slow_ticks) = slow.play(30.0, 2.0, 0.0, &[]);
        let (fast_frames, fast_ticks) = fast.play(120.0, 2.0, 0.0, &[]);
        assert_eq!(total(&slow_frames, "ticks"), 120.0);
        assert_eq!(total(&fast_frames, "ticks"), 120.0);
        assert_eq!(slow_ticks.len(), 120);
        assert_eq!(slow_ticks, fast_ticks, "every tick reads its own start's lattice at any frame rate");
        assert!(slow_ticks.iter().all(|lattice| lattice.len() == 17 * 17 * 17));
        assert!(slow_ticks.windows(2).all(|pair| pair[0] != pair[1]), "a moving field lays one lattice per tick");
        assert!(slow_frames.iter().all(|frame| frame.get("force_lattices") == frame.get("ticks")));
        // Tick k reads the field at its own start, k Sim Rate intervals in.
        let lattice = ForceLattice::over_box([-2.0, 0.0, -2.0], [4.0; 3], 17);
        let mut expected = vec![[0.0; 4]; lattice.node_count()];
        lattice.fill(&mut expected, |x| swelling(37.0 * TICK).sample(x)).unwrap();
        assert_eq!(slow_ticks[37], expected);
        // The 120 fps in-between frames run no tick and still report one
        // Sim Rate interval.
        assert!(fast_frames.iter().any(|frame| frame.get("ticks") == 0.0));
        for frame in slow_frames.iter().chain(&fast_frames) {
            assert!((frame.get("tick_seconds") - TICK as f32).abs() < 1e-7, "{frame:?}");
        }
        let last = |frames: &[Frame]| frames.last().unwrap().get("simulation_time");
        assert!((last(&slow_frames) - 2.0).abs() < 1e-5 && (last(&fast_frames) - 2.0).abs() < 1e-5);
        // The default box: a 4 m cube on the floor.
        let first = slow_frames[1];
        assert_eq!([first.get("field_min_x"), first.get("field_min_y"), first.get("field_min_z")], [-2.0, 0.0, -2.0]);
        assert_eq!(first.get("field_spacing"), 0.25);
    }

    /// The card's Speed is the clock's: half speed halves each tick's
    /// simulated seconds and the simulated time, never the tick count.
    #[test]
    fn swarm_speed_scales_tick_seconds_not_ticks() {
        let step = SimStep::export(Seconds(TICK));
        let (full, _) = Rig::new(None, step).play(60.0, 1.0, 0.0, &[("speed", 1.0)]);
        let (half, _) = Rig::new(None, step).play(60.0, 1.0, 0.0, &[("speed", 0.5)]);
        let (held, _) = Rig::new(None, step).play(60.0, 1.0, 0.0, &[("speed", 0.0)]);
        assert_eq!(total(&full, "ticks"), 60.0);
        assert_eq!(total(&half, "ticks"), 60.0);
        assert_eq!(total(&held, "ticks"), 0.0, "Speed 0 holds the swarm");
        for (full, half) in full.iter().zip(&half).filter(|(frame, _)| frame.get("ticks") > 0.0) {
            assert!((half.get("tick_seconds") - 0.5 * full.get("tick_seconds")).abs() < 1e-7, "{full:?} {half:?}");
        }
        let moved = |frames: &[Frame]| frames.iter().map(|frame| f64::from(frame.get("ticks") * frame.get("tick_seconds"))).sum::<f64>();
        assert!((moved(&full) - 1.0).abs() < 1e-5 && (moved(&half) - 0.5).abs() < 1e-5);
        assert!((half.last().unwrap().get("simulation_time") - 0.5).abs() < 1e-5);
    }

    /// A loop back (or any backward seek) restarts the clock in a new epoch
    /// with no ticks on the restart frame, so the particles reseed, and from
    /// there the swarm is told exactly what an export from the loop start is.
    #[test]
    fn swarm_loop_restarts_in_a_new_epoch_and_matches_an_export() {
        let mut live = Rig::new(Some(swelling), SimStep::live(Seconds(TICK)));
        let (before, _) = live.play(60.0, 1.0, 0.0, &[]);
        let epoch = before.last().unwrap().get("epoch");
        let (after, after_ticks) = live.play(60.0, 0.5, 0.25, &[]);
        assert_eq!(after[0].get("ticks"), 0.0, "the restart frame runs no tick");
        assert_eq!(after[0].get("epoch"), epoch + 1.0, "a loop is a new epoch: the particles reseed");
        assert!(after.iter().all(|frame| frame.get("epoch") == epoch + 1.0));
        let mut export = Rig::new(Some(swelling), SimStep::export(Seconds(TICK)));
        let (fresh, fresh_ticks) = export.play(60.0, 0.5, 0.25, &[]);
        // A frame without ticks reads no lattice, so its lattice count is
        // whatever the last tick left.
        let told = |frames: &[Frame]| -> Vec<[f32; 5]> {
            let lattices = |f: &Frame| if f.get("ticks") > 0.0 { f.get("force_lattices") } else { 0.0 };
            frames.iter().map(|f| [f.get("ticks"), f.get("first_tick"), f.get("tick_seconds"), lattices(f), f.get("simulation_time")]).collect()
        };
        assert_eq!(told(&after), told(&fresh));
        assert_eq!(after_ticks, fresh_ticks);
    }

    /// A reset edge (a clip trigger) restarts the clock too.
    #[test]
    fn swarm_reset_edge_starts_a_new_epoch() {
        let mut rig = Rig::new(None, SimStep::export(Seconds(TICK)));
        let (frames, _) = rig.play(60.0, 0.5, 0.0, &[("reset", 0.0)]);
        let epoch = frames.last().unwrap().get("epoch");
        let next = rig.frame(0.5, 0.5 + TICK, &[("reset", 1.0)]);
        assert_eq!((next.get("epoch"), next.get("ticks"), next.get("simulation_time")), (epoch + 1.0, 0.0, 0.0));
    }

    /// Every Force Detail the manifest allows fits any box.
    #[test]
    fn swarm_force_detail_range_fits_any_box() {
        let range = SwarmForces::new().parameters().iter().find(|p| p.name == "force_detail").and_then(|p| p.range).unwrap();
        assert_eq!(range, (2.0, MAX_FORCE_DETAIL));
        for size in [[4.0, 4.0, 4.0], [100.0, 0.01, 3.0], [0.02, 0.02, 0.02]] {
            let lattice = ForceLattice::over_box([0.0; 3], size, MAX_FORCE_DETAIL as u32);
            assert!(lattice.node_count() <= 64 * 64 * 64);
        }
    }
}
