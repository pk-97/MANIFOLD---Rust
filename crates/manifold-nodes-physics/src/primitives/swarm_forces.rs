//! `node.swarm_forces` — a swarm's fixed simulation clock and the scene's
//! force field laid onto a lattice over its box, once per tick, for
//! `node.move_swarm`. The same `acceleration_field` wire water and rigid
//! bodies take, so one scene force list moves all three.

use std::borrow::Cow;

use manifold_gpu::GpuBuffer;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::exec::effect_node::SimStep;
use manifold_physics::clock::{LiveLoad, SimulationClock};
use manifold_physics::force_lattice::{FORCE_LATTICE_RESERVE, FarForces, ForceLattice, LatticeFrame, TickForces, impulse_reserve};
use manifold_physics::input::{AppliedEvent, EventStamp};
use manifold_physics::tick_hits::TickHits;
use manifold_physics::{FieldValue, PlayedTime, Seconds, TickStamp, VectorField};

use crate::force_lattice::{felt_field, LatticeBuffers, LatticeUpload};
use crate::hits::{TakesHits, TakesHitsRegistration};
use crate::tick_sampled::{TickSampled, TickSampledRegistration};

/// Force Detail's top: 64³ lattice nodes laid on the CPU per tick at most.
const MAX_FORCE_DETAIL: f32 = 64.0;

const OUTPUTS: [&str; 14] = [
    "ticks", "first_tick", "epoch", "force_lattices",
    "field_nodes_x", "field_nodes_y", "field_nodes_z", "field_spacing",
    "field_min_x", "field_min_y", "field_min_z", "simulation_time", "impulse_tick", "far_terms",
];
const IMPULSE_TICK: usize = 12;

const fn param(name: &'static str, label: &'static str, ty: ParamType, default: f32, range: (f32, f32)) -> ParamDef {
    ParamDef { name: Cow::Borrowed(name), label, ty, default: ParamValue::Float(default), range: Some(range), enum_values: &[] }
}

manifold_node_engine::primitive! {
    name: SwarmForces,
    type_id: "node.swarm_forces",
    purpose: "Run a swarm's fixed simulation clock (the same ticks water runs, so live and export match) and lay the scene's acceleration field onto a lattice over the swarm's box at each tick's start: one lattice while the frame's ticks agree, one per tick otherwise. Publishes the frame's ticks, each tick's own simulated seconds as a buffer (Speed scales them, so a Speed ramp moves the swarm the same at any frame rate), the clock's epoch, the field scalars and the forces buffer node.move_swarm reads. Scene hits (a force's Impulse) land once, on the tick whose simulated interval holds the fire: a frame with hits lays one impulse lattice per tick (zero where none landed), then each tick's kick as far terms for a particle past the lattice, and impulse_tick names the frame's first tick (−1 none). Paused or at Speed 0 the clock holds and runs no ticks. A backward seek, a loop, a reset edge or a Sim Rate edit restarts the clock in a new epoch.",
    inputs: {
        bounds: Transform optional,
        frame: Transform optional,
        acceleration_field: VectorField optional,
        speed: ScalarF32 optional,
        reset: ScalarF32 optional,
        air_drag: ScalarF32 optional,
        air_resistance: ScalarF32 optional,
        gravity_x: ScalarF32 optional,
        gravity: ScalarF32 optional,
        gravity_z: ScalarF32 optional,
    },
    outputs: {
        ticks: ScalarF32,
        first_tick: ScalarF32,
        epoch: ScalarF32,
        force_lattices: ScalarF32,
        field_nodes_x: ScalarF32, field_nodes_y: ScalarF32, field_nodes_z: ScalarF32,
        field_spacing: ScalarF32,
        field_min_x: ScalarF32, field_min_y: ScalarF32, field_min_z: ScalarF32,
        simulation_time: ScalarF32,
        impulse_tick: ScalarF32,
        far_terms: ScalarF32,
        forces: Array(f32),
        tick_seconds: Array(f32),
        impulses: Array(f32),
        far_forces: Array(f32),
    },
    params: [
        param("force_detail", "Force Detail", ParamType::Int, 17.0, (2.0, MAX_FORCE_DETAIL)),
        param("speed", "Speed", ParamType::Float, 1.0, (0.0, 4.0)),
        param("air_drag", "Air Drag", ParamType::Float, 1.0, (0.0, 4.0)),
    ],
    depth_rule: Terminal,
    composition_notes: "The swarm's source of truth. Every output but epoch and simulation_time wires into node.move_swarm's port of the same name (forces, tick_seconds, impulses and far_forces are buffers). Wire epoch into the particle loop's node.array_feedback reset_trigger (and the seeder's), so every clock restart reseeds the particles: live after a loop matches an export. Inserted from the Add menu, and in the Swarm preset, the Physics World drives gravity_x, gravity, gravity_z, air_resistance, speed and reset: the swarm falls under World gravity (metres per second squared, added to the scene forces it feels) and its Air Drag acts against the World's still air (air_resistance, 1/s), follows World Speed and restarts on World Reset, which clip triggers fire. Unwired, gravity is zero; elsewhere, wire a trigger counter into reset. Scene forces arrive on acceleration_field (the scene force list expands into it, as for a liquid domain). The bounds transform (position and scale) is the box forces are laid out over, in the swarm's own frame; past it a particle reads the far terms on far_forces (far_terms per lattice, 0 for none): radial and vortex forces from their formulas, uniform pushes and air at their constant value, gusts as still air. Wire the swarm's frame (its emitter transform, the frame its particles move in) into frame: forces are sampled where each lattice node sits in the world and turned back into the frame, so a wind blows the same way however the emitter turns. Unwired, the frame is the world. Force Detail is the lattice nodes along the box's longest side; every value in its range fits any box. Air Drag scales how strongly the swarm feels the air a wind force carries: 0 ignores the air, 1 takes the wind's own drag rate.",
    examples: ["Swarm"],
    picker: { label: "Swarm Forces", category: Atom },
    summary: "Runs a swarm's simulation clock and lays the scene's forces over its box.",
    category: Particles3D,
    role: Source,
    aliases: ["swarm", "swarm forces", "swarm clock", "force field"],
    boundary_reason: NonGpu,
    extra_fields: {
        clock: SimulationClock = SimulationClock::default(),
        forces: TickForces = TickForces::with_far(),
        buffers: LatticeBuffers = LatticeBuffers::default(),
        published: [f32; OUTPUTS.len()] = [0.0; OUTPUTS.len()],
        tick_seconds: Vec<f32> = Vec::with_capacity(FORCE_LATTICE_RESERVE as usize),
        hits: TickHits<FieldValue> = TickHits::new("Swarm impulses"),
        // The frame's impulse lattices, then their kicks' far terms.
        impulses: Vec<[f32; 4]> = Vec::new(),
        kick_far: FarForces = FarForces::default(),
        impulses_dirty: bool = false,
    },
}

/// The field the swarm feels: the scene's forces at its Air Drag, plus the
/// World's gravity (`gravity_x`, `gravity`, `gravity_z`, metres per second
/// squared), a uniform push every particle falls under (D-109). Unwired,
/// gravity is zero: a swarm outside a Physics World feels none.
fn felt(ctx: &EffectNodeContext<'_, '_>) -> Result<Option<FieldValue>, String> {
    let field = felt_field(ctx, "Swarm Forces")?;
    let gravity = ["gravity_x", "gravity", "gravity_z"].map(|port| ctx.scalar_or_param(port, 0.0));
    if gravity == [0.0; 3] {
        return Ok(field);
    }
    let falling = FieldValue::uniform(gravity).map_err(|error| format!("Swarm Forces: gravity: {error}"))?;
    match field {
        Some(field) => field.sum(&falling).map(Some).map_err(|error| format!("Swarm Forces: {error}")),
        None => Ok(Some(falling)),
    }
}

impl TickSampled for SwarmForces {
    fn request_physics_samples(&mut self, from: PlayedTime, until: PlayedTime, out: &mut Vec<PlayedTime>) {
        self.forces.request_samples(&self.clock, from, until, out);
    }
}

inventory::submit! { TickSampledRegistration::new::<SwarmForces>() }

impl TakesHits for SwarmForces {
    fn hit_epoch(&self) -> Option<u64> {
        self.hits.epoch()
    }

    /// A swarm whose clock has not started makes a hit wait; a broken clock
    /// refuses it.
    fn hit_readiness(&self) -> Result<bool, String> {
        match self.hits.failure() {
            Some(error) => Err(error.into()),
            None => Ok(self.hits.epoch().is_some()),
        }
    }

    /// A hit is stamped at the fire's own simulated time, so it lands on the
    /// tick whose interval holds that time, the same tick at any frame rate.
    fn hit_stamp(&self, played: PlayedTime, sequence: u64, step: SimStep) -> Result<EventStamp, String> {
        if !self.hit_readiness()? {
            return Err("Swarm impulses: the swarm has not started; fire waits for it".into());
        }
        let mut stamp = self.hits.stamp(played, sequence)?;
        let load = step.load().map(|load| LiveLoad { previous: load.previous, budget: load.budget });
        stamp.time = Seconds(self.clock.simulation_at(played, load));
        Ok(stamp)
    }

    fn enqueue_hit(&mut self, stamp: EventStamp, field: FieldValue) -> Result<TickStamp, String> {
        self.hits.enqueue(stamp, field)
    }

    fn drain_applied_hits(&mut self, consume: &mut dyn FnMut(AppliedEvent<FieldValue>)) {
        self.hits.drain_applied(consume);
    }

    fn drain_discarded_hits(&mut self, consume: &mut dyn FnMut(EventStamp)) -> u64 {
        self.hits.drain_discarded(consume)
    }
}

inventory::submit! { TakesHitsRegistration::new::<SwarmForces>() }

impl SwarmForces {
    fn lattice(ctx: &EffectNodeContext<'_, '_>) -> Result<ForceLattice, String> {
        // The bounds transform places and sizes the box; unwired, a 4 m cube
        // standing on the floor.
        let (center, size) = ctx.inputs.transform("bounds").map_or(([0.0, 2.0, 0.0], [4.0; 3]), |t| (t.pos, t.scale.map(f32::abs)));
        if size.iter().chain(&center).any(|v| !v.is_finite()) || size.iter().any(|&s| s <= 0.0) {
            return Err("Swarm Forces: the bounds box must be finite with every side above zero".into());
        }
        let detail = ctx.scalar_or_param("force_detail", 17.0).round().clamp(2.0, MAX_FORCE_DETAIL) as u32;
        let lattice = ForceLattice::over_box(std::array::from_fn(|i| center[i] - 0.5 * size[i]), size, detail);
        let Some(frame) = ctx.inputs.transform("frame") else {
            return Ok(lattice);
        };
        // Column-major: column i is local axis i in the world, column 3 the origin.
        let world = frame.world();
        let axes = std::array::from_fn(|axis| [world[axis][0], world[axis][1], world[axis][2]]);
        LatticeFrame::new(axes, [world[3][0], world[3][1], world[3][2]])
            .map(|frame| lattice.in_frame(frame))
            .ok_or_else(|| "Swarm Forces: the swarm's frame must be finite with no zero scale".into())
    }

    fn compute(&mut self, ctx: &mut EffectNodeContext<'_, '_>) -> Result<[f32; OUTPUTS.len()], String> {
        let lattice = Self::lattice(ctx)?;
        let field = felt(ctx)?;
        let frame = self.clock.advance(
            ctx.played(),
            ctx.sim_step.interval.0,
            ctx.scalar_or_param("speed", 1.0),
            ctx.scalar_or_param("reset", 0.0),
            false,
            ctx.sim_step.offline(),
        );
        // Hits follow the clock before anything else can fail, so a refused
        // frame never leaves them a frame behind it.
        self.hits.observe_frame(ctx.played(), &frame)?;
        if frame.numerical_error {
            ctx.sim_metrics.record_simulation(0.0, 0.0, false, true);
        }
        self.forces.prepare(lattice, field.as_ref(), &self.clock, &frame)?;
        let impulse_tick = self.lay_impulses(lattice, frame.first_sequence, frame.ticks)?;
        // Speed changes each tick's simulated length, not the tick count, and
        // a Speed ramp gives each tick its own: one length per tick, as water
        // steps each tick by its own interval.
        self.tick_seconds.clear();
        self.tick_seconds.extend((0..u64::from(frame.ticks)).map(|tick| frame.interval(tick).map_or(0.0, |i| i.duration().0) as f32));
        let [nx, ny, nz] = lattice.nodes().map(|n| n as f32);
        let min = lattice.min();
        Ok([
            frame.ticks as f32,
            frame.first_sequence as f32,
            frame.epoch as f32,
            self.forces.lattices() as f32,
            nx, ny, nz,
            lattice.spacing(),
            min[0], min[1], min[2],
            frame.simulation_time as f32,
            impulse_tick.map_or(-1.0, |tick| tick as f32),
            self.forces.far_terms() as f32,
        ])
    }

    /// Lay this frame's hits, one impulse lattice per tick of the frame from
    /// `first`, each the sum of the hits landing on that tick (zero where
    /// none did), as the liquids do offline. After the lattices come the
    /// kicks' far terms, one set per tick, read past the lattice
    /// (`move_swarm_far`); a lone header of no terms reads the lattice's
    /// edge there. Returns the first tick.
    fn lay_impulses(&mut self, lattice: ForceLattice, first: u64, ticks: u32) -> Result<Option<u64>, String> {
        // Room for a live frame's hit lattices and far terms, made when the
        // lattice changes, so a hit never allocates on stage; only offline
        // catch-up grows it.
        let reserve = impulse_reserve(lattice.node_count());
        if self.impulses.capacity() < reserve {
            self.impulses.reserve_exact(reserve - self.impulses.len());
            self.kick_far.reserve(FORCE_LATTICE_RESERVE as usize, 1);
        }
        if self.hits.impulse_tick().is_none() {
            return Ok(None);
        }
        let events = self.hits.frame_events();
        if events.iter().any(|event| !(first..first + u64::from(ticks)).contains(&event.applied.tick)) {
            return Err("Swarm impulses: a hit landed outside the frame's ticks".into());
        }
        let count = lattice.node_count();
        self.impulses.clear();
        self.impulses.resize(ticks as usize * count, [0.0; 4]);
        for (tick, values) in (first..).zip(self.impulses.chunks_exact_mut(count)) {
            if !events.iter().any(|event| event.applied.tick == tick) {
                continue;
            }
            lattice
                .fill(values, |x| {
                    events.iter().filter(|event| event.applied.tick == tick).fold([0.0; 3], |sum, event| {
                        let kick = event.value.sample(x);
                        std::array::from_fn(|axis| sum[axis] + kick[axis])
                    })
                })
                .map_err(|_| "Swarm impulses: the impulse field is not finite inside the swarm's box".to_string())?;
        }
        let on_tick = |tick: u64| events.iter().filter(move |event| event.applied.tick == tick).map(|event| &event.value);
        self.kick_far.lay_sums(&lattice, (first..first + u64::from(ticks)).map(on_tick));
        match self.kick_far.packed() {
            [] => self.impulses.push([0.0; 4]),
            block => self.impulses.extend_from_slice(block),
        }
        self.impulses_dirty = true;
        Ok(Some(first))
    }

    fn upload(&mut self, ctx: &mut EffectNodeContext<'_, '_>) -> Result<(), String> {
        let lattice = Self::lattice(ctx)?;
        let offline = ctx.sim_step.offline();
        let Some(gpu) = ctx.gpu.as_deref_mut() else { return Ok(()) };
        let data = LatticeUpload {
            lattice,
            forces: self.forces.forces(),
            forces_dirty: self.forces.is_dirty(),
            impulses: &self.impulses,
            impulses_dirty: self.impulses_dirty,
            hits: &[],
            tick_seconds: &self.tick_seconds,
            far: self.forces.far(),
        };
        self.buffers.upload(gpu, offline, data)?;
        self.forces.mark_uploaded();
        self.impulses_dirty = false;
        // A hit is applied once its tick's lattice is on the GPU.
        self.hits.commit_frame();
        Ok(())
    }
}

impl Primitive for SwarmForces {
    fn provides_array_output(&self, port: &str) -> bool {
        matches!(port, "forces" | "tick_seconds" | "impulses" | "far_forces")
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "forces" => self.buffers.forces(),
            "tick_seconds" => self.buffers.tick_seconds(),
            "impulses" => self.buffers.impulses(),
            "far_forces" => self.buffers.far(),
            _ => None,
        }
    }

    fn array_output_capacity(&self, port_name: &str, _params: &ParamValues, _input_capacities: &[(&str, u32)]) -> Option<u32> {
        self.provides_array_output(port_name).then_some(1)
    }

    fn acceleration_input(&self) -> Option<&'static str> {
        Some("acceleration_field")
    }

    /// A swarm follows every Physics World control as water does: Speed and
    /// Reset (D-107), gravity (D-109) and still air (D-114).
    fn world_control_inputs(&self) -> &'static [manifold_node_engine::primitive::WorldControlInput] {
        &manifold_node_engine::primitive::WORLD_INPUTS
    }

    fn clear_state(&mut self) {
        self.clock.restart();
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.clock.set_live_load(ctx.sim_step.load().map(|load| LiveLoad { previous: load.previous, budget: load.budget }));
        self.clock.set_played_target(ctx.sim_step.played_target);
        // A physics sample reads the field at a tick's start; it never
        // advances time.
        if ctx.sim_step.authored_sample_only {
            self.clock.observe_speed(ctx.played(), ctx.scalar_or_param("speed", 1.0));
            match felt(ctx) {
                Ok(field) => self.forces.observe_sample(&self.clock, ctx.played(), field.as_ref()),
                Err(error) => ctx.error(error),
            }
            return;
        }
        // On an error the swarm holds: the last outputs repeat with no ticks.
        let result = self.compute(ctx).and_then(|values| self.upload(ctx).map(|()| values));
        match result {
            Ok(values) => self.published = values,
            Err(error) => {
                // A frame that fails runs no tick: its hits are discarded
                // with a receipt, never replayed.
                self.hits.abandon_frame();
                self.published[0] = 0.0;
                self.published[IMPULSE_TICK] = -1.0;
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
    use manifold_physics::{FalloffCurve, FieldValue, VectorField};
    use manifold_physics::force_lattice::sample_packed;
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

    /// What a played run told node.move_swarm, tick by tick.
    struct Played {
        frames: Vec<Frame>,
        lattices: Vec<Vec<[f32; 4]>>,
        tick_seconds: Vec<f32>,
    }

    struct Rig {
        node: SwarmForces,
        backend: MockBackend,
        field: Option<(Slot, Field)>,
        step: SimStep,
        /// The Speed knob over transport time, when it moves.
        speed: Option<fn(f64) -> f32>,
    }

    impl Rig {
        fn new(field: Option<Field>, step: SimStep) -> Self {
            let mut backend = MockBackend::new();
            let field = field.map(|at| (backend.acquire(ResourceId(1), PortType::VectorField, None, (0, 0)), at));
            Self { node: SwarmForces::new(), backend, field, step, speed: None }
        }

        fn run(&mut self, seconds: f64, params: &[(&'static str, f32)], step: SimStep) -> Vec<String> {
            let speed = self.speed.map(|at| (Cow::Borrowed("speed"), ParamValue::Float(at(seconds))));
            let params: ParamValues =
                params.iter().map(|&(name, value)| (Cow::Borrowed(name), ParamValue::Float(value))).chain(speed).collect();
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
            self.node.request_physics_samples(self.step.to_played(Seconds(from)), self.step.to_played(Seconds(until)), &mut starts);
            for start in starts {
                assert!(self.run(self.step.to_transport(start).0, params, self.step.authored_sample().at_played(start)).is_empty());
            }
            let errors = self.run(until, params, self.step);
            assert!(errors.is_empty(), "{errors:?}");
            // The GPU upload commits the frame's hits; this rig has no GPU.
            self.node.hits.commit_frame();
            Frame(self.node.published)
        }

        /// Fire a scene hit at transport `at`, as the impulse runtime does.
        fn fire(&mut self, at: f64, sequence: u64, field: FieldValue) {
            let stamp = self.node.hit_stamp(self.step.to_played(Seconds(at)), sequence, self.step).expect("the swarm takes hits");
            self.node.enqueue_hit(stamp, field).expect("the hit is queued");
        }

        /// The last frame's kicks: each tick that a hit landed on, with its
        /// impulse lattice.
        fn kicks(&self, frame: &Frame) -> Vec<(u64, Vec<[f32; 4]>)> {
            let (first, ticks) = (frame.get("impulse_tick"), frame.get("ticks") as usize);
            if first < 0.0 {
                return Vec::new();
            }
            assert_eq!(first, frame.get("first_tick"), "a frame's impulse lattices start at its first tick");
            let nodes = ["field_nodes_x", "field_nodes_y", "field_nodes_z"].map(|name| frame.get(name) as usize).iter().product::<usize>();
            self.node.impulses[..ticks * nodes].chunks_exact(nodes).enumerate()
                .filter(|(_, lattice)| lattice.iter().any(|node| *node != [0.0; 4]))
                .map(|(slot, lattice)| (first as u64 + slot as u64, lattice.to_vec()))
                .collect()
        }

        /// Frames at `fps` from `from` to `until`, every kick by tick.
        fn kicks_until(&mut self, fps: f64, from: f64, until: f64) -> Vec<(u64, Vec<[f32; 4]>)> {
            let mut kicks = Vec::new();
            let mut previous = from;
            for index in 1..=((until - from) * fps).round() as u64 {
                let now = from + index as f64 / fps;
                let frame = self.frame(previous, now, &[]);
                kicks.extend(self.kicks(&frame));
                previous = now;
            }
            kicks
        }

        /// Frames at `fps` over `[from, from + seconds]`: every frame's
        /// outputs, and the lattice and length each tick reads.
        fn play(&mut self, fps: f64, seconds: f64, from: f64, params: &[(&'static str, f32)]) -> Played {
            let mut played = Played { frames: Vec::new(), lattices: Vec::new(), tick_seconds: Vec::new() };
            let mut previous = from;
            for index in 0..=(seconds * fps).round() as u64 {
                let now = from + index as f64 / fps;
                let frame = self.frame(previous, now, params);
                previous = now;
                let lattice_len = self.node.forces.forces().len() / self.node.forces.lattices().max(1);
                assert_eq!(self.node.tick_seconds.len(), frame.get("ticks") as usize, "one length per tick");
                for tick in 0..frame.get("ticks") as usize {
                    let slot = tick.min(self.node.forces.lattices().saturating_sub(1));
                    played.lattices.push(self.node.forces.forces().get(slot * lattice_len..(slot + 1) * lattice_len).unwrap_or(&[]).to_vec());
                }
                played.tick_seconds.extend(&self.node.tick_seconds);
                played.frames.push(frame);
            }
            played
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
        let slow = Rig::new(Some(swelling), step).play(30.0, 2.0, 0.0, &[]);
        let fast = Rig::new(Some(swelling), step).play(120.0, 2.0, 0.0, &[]);
        assert_eq!(total(&slow.frames, "ticks"), 120.0);
        assert_eq!(total(&fast.frames, "ticks"), 120.0);
        assert_eq!(slow.lattices.len(), 120);
        assert_eq!(slow.lattices, fast.lattices, "every tick reads its own start's lattice at any frame rate");
        assert!(slow.lattices.iter().all(|lattice| lattice.len() == 2 * 17 * 17 * 17), "two records a force node");
        assert!(slow.lattices.windows(2).all(|pair| pair[0] != pair[1]), "a moving field lays one lattice per tick");
        assert!(slow.frames.iter().all(|frame| frame.get("force_lattices") == frame.get("ticks")));
        // Tick k reads the field at its own start, k Sim Rate intervals in.
        let lattice = ForceLattice::over_box([-2.0, 0.0, -2.0], [4.0; 3], 17);
        let mut expected = vec![[0.0; 4]; 2 * lattice.node_count()];
        lattice.fill_forces(&mut expected, |x| swelling(37.0 * TICK).sample_force(x)).unwrap();
        assert_eq!(slow.lattices[37], expected);
        // The 120 fps in-between frames run no tick.
        assert!(fast.frames.iter().any(|frame| frame.get("ticks") == 0.0));
        assert_eq!(slow.tick_seconds, vec![TICK as f32; 120]);
        assert_eq!(fast.tick_seconds, slow.tick_seconds);
        let last = |frames: &[Frame]| frames.last().unwrap().get("simulation_time");
        assert!((last(&slow.frames) - 2.0).abs() < 1e-5 && (last(&fast.frames) - 2.0).abs() < 1e-5);
        // The default box: a 4 m cube on the floor.
        let first = slow.frames[1];
        assert_eq!([first.get("field_min_x"), first.get("field_min_y"), first.get("field_min_z")], [-2.0, 0.0, -2.0]);
        assert_eq!(first.get("field_spacing"), 0.25);
    }

    /// World gravity is a uniform push on every lattice node, added to the
    /// scene's forces, and the lattice follows a change to it mid-run
    /// (D-109). Unwired, a swarm feels none.
    #[test]
    fn swarm_falls_under_world_gravity_and_follows_a_change() {
        let step = SimStep::export(Seconds(TICK));
        let lattice = ForceLattice::over_box([-2.0, 0.0, -2.0], [4.0; 3], 17);
        let laid = |field: &FieldValue| {
            let mut values = vec![[0.0; 4]; 2 * lattice.node_count()];
            lattice.fill_forces(&mut values, |x| field.sample_force(x)).unwrap();
            values
        };
        let earth = FieldValue::uniform([0.0, -9.81, 0.0]).unwrap();
        let moon = FieldValue::uniform([0.0, -1.62, 0.0]).unwrap();
        let mut rig = Rig::new(None, step);
        let first = rig.play(60.0, 0.25, 0.0, &[("gravity", -9.81)]);
        assert!(!first.lattices.is_empty() && first.lattices.iter().all(|values| *values == laid(&earth)), "the swarm falls at g");
        let changed = rig.play(60.0, 0.25, 0.25 + TICK, &[("gravity", -1.62)]);
        assert_eq!(changed.lattices.last().unwrap(), &laid(&moon), "a gravity change reaches the next tick");

        let pushed = Rig::new(Some(vortex), step).play(60.0, 0.25, 0.0, &[("gravity", -9.81)]);
        assert_eq!(pushed.lattices[5], laid(&vortex(0.0).sum(&earth).unwrap()), "gravity adds to the scene forces");

        let weightless = Rig::new(None, step).play(60.0, 0.25, 0.0, &[]);
        assert!(weightless.lattices.iter().all(|values| values.iter().all(|node| *node == [0.0; 4])), "unwired gravity is zero");
    }

    /// The card's Speed is the clock's: half speed halves each tick's
    /// simulated seconds and the simulated time, never the tick count.
    #[test]
    fn swarm_speed_scales_tick_seconds_not_ticks() {
        let step = SimStep::export(Seconds(TICK));
        let full = Rig::new(None, step).play(60.0, 1.0, 0.0, &[("speed", 1.0)]);
        let half = Rig::new(None, step).play(60.0, 1.0, 0.0, &[("speed", 0.5)]);
        let held = Rig::new(None, step).play(60.0, 1.0, 0.0, &[("speed", 0.0)]);
        assert_eq!(total(&full.frames, "ticks"), 60.0);
        assert_eq!(total(&half.frames, "ticks"), 60.0);
        assert_eq!(total(&held.frames, "ticks"), 0.0, "Speed 0 holds the swarm");
        assert_eq!(half.tick_seconds.len(), full.tick_seconds.len());
        for (full, half) in full.tick_seconds.iter().zip(&half.tick_seconds) {
            assert!((half - 0.5 * full).abs() < 1e-7, "{full} {half}");
        }
        let moved = |played: &Played| played.tick_seconds.iter().map(|&s| f64::from(s)).sum::<f64>();
        assert!((moved(&full) - 1.0).abs() < 1e-5 && (moved(&half) - 0.5).abs() < 1e-5);
        assert!((half.frames.last().unwrap().get("simulation_time") - 0.5).abs() < 1e-5);
    }

    /// Speed rising smoothly under a playing swarm, changing between every
    /// pair of frames at any display rate.
    fn smooth_ramp(seconds: f64) -> f32 {
        0.5 + 0.75 * seconds as f32
    }

    /// While Speed moves, each tick runs for its own length, at the Speed of
    /// its own start: a 30 fps frame's two ticks differ, and every tick is
    /// told exactly the same at 60, 120 and 144 fps, live or exported.
    #[test]
    fn swarm_speed_ramp_ticks_match_at_30_60_120_144_fps_live_and_export() {
        let rig = |step| Rig { speed: Some(smooth_ramp), ..Rig::new(Some(swelling), step) };
        let live = SimStep::live(Seconds(TICK));
        let slow = rig(live).play(30.0, 2.0, 0.0, &[]);
        assert_eq!(slow.tick_seconds.len(), 120);
        let bits = |seconds: &[f32]| seconds.iter().map(|s| s.to_bits()).collect::<Vec<_>>();
        for (k, &seconds) in slow.tick_seconds.iter().enumerate() {
            let expected = TICK * f64::from(smooth_ramp(k as f64 * TICK));
            assert!((f64::from(seconds) - expected).abs() < 1e-8, "tick {k} runs at its start's Speed: {seconds} against {expected}");
        }
        for step in [live, SimStep::export(Seconds(TICK))] {
            for fps in [30.0, 60.0, 120.0, 144.0] {
                let run = rig(step).play(fps, 2.0, 0.0, &[]);
                assert_eq!(bits(&run.tick_seconds), bits(&slow.tick_seconds), "a Speed ramp ticks the same at {fps} fps, offline {}", step.offline());
                assert_eq!(run.lattices, slow.lattices, "{fps} fps lattices");
                assert_eq!(run.frames.last().unwrap().get("simulation_time"), slow.frames.last().unwrap().get("simulation_time"));
            }
        }
        // At 30 fps each frame after the first runs ticks 2k and 2k + 1.
        assert!(slow.frames[1..].iter().all(|frame| frame.get("ticks") == 2.0));
        assert!(slow.tick_seconds.chunks(2).all(|pair| pair[1] > pair[0]), "a frame's ticks keep their own lengths: {:?}", &slow.tick_seconds[..8]);
        let moved = slow.tick_seconds.iter().map(|&s| f64::from(s)).sum::<f64>();
        assert!((moved - f64::from(slow.frames.last().unwrap().get("simulation_time"))).abs() < 1e-4);
    }

    /// A loop back (or any backward seek) restarts the clock in a new epoch
    /// with no ticks on the restart frame, so the particles reseed, and from
    /// there the swarm is told exactly what an export from the loop start is.
    #[test]
    fn swarm_loop_restarts_in_a_new_epoch_and_matches_an_export() {
        let mut live = Rig::new(Some(swelling), SimStep::live(Seconds(TICK)));
        let before = live.play(60.0, 1.0, 0.0, &[]);
        let epoch = before.frames.last().unwrap().get("epoch");
        let after = live.play(60.0, 0.5, 0.25, &[]);
        assert_eq!(after.frames[0].get("ticks"), 0.0, "the restart frame runs no tick");
        assert_eq!(after.frames[0].get("epoch"), epoch + 1.0, "a loop is a new epoch: the particles reseed");
        assert!(after.frames.iter().all(|frame| frame.get("epoch") == epoch + 1.0));
        let mut export = Rig::new(Some(swelling), SimStep::export(Seconds(TICK)));
        let fresh = export.play(60.0, 0.5, 0.25, &[]);
        // A frame without ticks reads no lattice, so its lattice count is
        // whatever the last tick left.
        let told = |frames: &[Frame]| -> Vec<[f32; 4]> {
            let lattices = |f: &Frame| if f.get("ticks") > 0.0 { f.get("force_lattices") } else { 0.0 };
            frames.iter().map(|f| [f.get("ticks"), f.get("first_tick"), lattices(f), f.get("simulation_time")]).collect()
        };
        assert_eq!(told(&after.frames), told(&fresh.frames));
        assert_eq!(after.lattices, fresh.lattices);
        assert_eq!(after.tick_seconds, fresh.tick_seconds);
    }

    /// A reset edge (a clip trigger) restarts the clock too.
    #[test]
    fn swarm_reset_edge_starts_a_new_epoch() {
        let mut rig = Rig::new(None, SimStep::export(Seconds(TICK)));
        let played = rig.play(60.0, 0.5, 0.0, &[("reset", 0.0)]);
        let epoch = played.frames.last().unwrap().get("epoch");
        let next = rig.frame(0.5, 0.5 + TICK, &[("reset", 1.0)]);
        assert_eq!((next.get("epoch"), next.get("ticks"), next.get("simulation_time")), (epoch + 1.0, 0.0, 0.0));
    }

    /// Two hits whose fires fall in different ticks of one frame each land
    /// once, on their own tick, and the same fires land the same at 30 and
    /// 120 fps.
    #[test]
    fn swarm_hits_land_on_their_own_ticks_at_any_frame_rate() {
        let push = |v: [f32; 3]| FieldValue::uniform(v).unwrap();
        let lattice = ForceLattice::over_box([-2.0, 0.0, -2.0], [4.0; 3], 17);
        let laid = |v: [f32; 3]| vec![[v[0], v[1], v[2], 0.0]; lattice.node_count()];
        let mut kicks = Vec::new();
        for fps in [30.0, 120.0] {
            let mut rig = Rig::new(None, SimStep::live(Seconds(TICK)));
            rig.play(fps, 1.0, 0.0, &[]);
            rig.fire(1.0 + 0.3 * TICK, 1, push([1.0, 0.0, 0.0]));
            rig.fire(1.0 + 1.4 * TICK, 2, push([0.0, 2.0, 0.0]));
            kicks.push(rig.kicks_until(fps, 1.0, 1.0 + 4.0 * TICK));
            let mut applied = Vec::new();
            rig.node.drain_applied_hits(&mut |event| applied.push(event.applied.tick));
            assert_eq!(applied.len(), 2, "each hit lands once");
        }
        let ticks: Vec<u64> = kicks[0].iter().map(|(tick, _)| *tick).collect();
        assert_eq!(ticks.len(), 2);
        assert_eq!(ticks[1], ticks[0] + 1, "two fires a tick apart land a tick apart");
        assert_eq!(kicks[0], vec![(ticks[0], laid([1.0, 0.0, 0.0])), (ticks[1], laid([0.0, 2.0, 0.0]))]);
        assert_eq!(kicks[0], kicks[1], "the same fires kick the same ticks at 30 and 120 fps");
    }

    /// The first hit, and a later frame with hits on more ticks, lay their
    /// lattices in the room reserved when the lattice was set: no allocation.
    #[test]
    fn swarm_hits_allocate_nothing() {
        let push = |v: [f32; 3]| FieldValue::uniform(v).unwrap();
        let mut rig = Rig::new(None, SimStep::live(Seconds(TICK)));
        rig.play(30.0, 0.5, 0.0, &[]);
        let room = (rig.node.impulses.as_ptr(), rig.node.impulses.capacity());
        assert!(room.1 >= FORCE_LATTICE_RESERVE as usize * 17 * 17 * 17);
        rig.fire(0.5 + 0.3 * TICK, 1, push([1.0, 0.0, 0.0]));
        let one = rig.kicks_until(30.0, 0.5, 0.5 + 2.0 * TICK);
        rig.fire(0.5 + 2.2 * TICK, 2, push([0.0, 1.0, 0.0]));
        rig.fire(0.5 + 3.4 * TICK, 3, push([0.0, 0.0, 1.0]));
        let two = rig.kicks_until(30.0, 0.5 + 2.0 * TICK, 0.5 + 4.0 * TICK);
        assert_eq!((one.len(), two.len()), (1, 2));
        assert_eq!((rig.node.impulses.as_ptr(), rig.node.impulses.capacity()), room);
    }

    /// BUG-cw4l4 (Radial impulses read the lattice edge past a swarm force
    /// lattice): past the swarm's box a Burst kicks a particle by the field
    /// at its own distance, read from the far terms packed after the frame's
    /// impulse lattices, never the box edge's value. A tick no hit landed on
    /// kicks nothing there. The swarm here feels no other force, so only the
    /// kick carries far terms.
    #[test]
    fn a_burst_past_the_lattice_kicks_by_its_distance() {
        let burst = FieldValue::radial_curve([0.0, 2.0, 0.0], 2.5, 1.0, FalloffCurve::Inverse).unwrap().scaled(6.0).unwrap();
        let mut rig = Rig::new(None, SimStep::live(Seconds(TICK)));
        rig.play(30.0, 0.5, 0.0, &[]);
        rig.fire(0.5 + 0.3 * TICK, 1, burst.clone());
        let frame = rig.frame(0.5, 0.5 + 2.0 * TICK, &[]);
        let ticks = frame.get("ticks") as usize;
        assert!(frame.get("impulse_tick") >= 0.0 && ticks == 2, "the hit lands in a frame of two ticks");
        let lattice = ForceLattice::over_box([-2.0, 0.0, -2.0], [4.0; 3], 17);
        let nodes = lattice.node_count();
        let (lattices, far) = rig.node.impulses.split_at(ticks * nodes);
        let hit = lattices.chunks_exact(nodes).position(|kicks| kicks.iter().any(|node| *node != [0.0; 4])).expect("one tick took the hit");
        for x in [[6.0, 2.0, 0.0], [0.0, 9.0, 0.5], [-5.0, -3.0, 4.0]] {
            for slot in 0..ticks {
                let kick = sample_packed(far, slot, x).push;
                let expected = if slot == hit { burst.sample(x) } else { [0.0; 3] };
                for axis in 0..3 {
                    assert!((kick[axis] - expected[axis]).abs() <= 1e-5 * (1.0 + expected[axis].abs()), "{x:?} tick {slot}: {kick:?} vs {expected:?}");
                }
            }
            let edge = lattice.sample(&lattices[hit * nodes..(hit + 1) * nodes], x);
            assert!((0..3).any(|axis| (edge[axis] - burst.sample(x)[axis]).abs() > 1e-2), "{x:?}: the box edge reads {edge:?}, a different kick");
        }
    }

    /// A fire while the swarm is paused (Speed 0) is discarded with a
    /// receipt, never held for later (D-91).
    #[test]
    fn swarm_hit_while_paused_is_discarded() {
        let mut rig = Rig::new(None, SimStep::live(Seconds(TICK)));
        rig.play(60.0, 0.5, 0.0, &[]);
        rig.frame(0.5, 0.5 + TICK, &[("speed", 0.0)]);
        rig.fire(0.5 + TICK, 1, FieldValue::uniform([1.0, 0.0, 0.0]).unwrap());
        let mut discarded = Vec::new();
        assert_eq!(rig.node.drain_discarded_hits(&mut |stamp| discarded.push(stamp.sequence)), 0);
        assert_eq!(discarded, [1]);
        let mut previous = 0.5 + TICK;
        for index in 1..=10 {
            let now = previous + TICK;
            let frame = rig.frame(previous, now, &[]);
            assert_eq!(frame.get("impulse_tick"), -1.0, "frame {index} after the pause kicks nothing");
            previous = now;
        }
        let mut applied = 0;
        rig.node.drain_applied_hits(&mut |_| applied += 1);
        assert_eq!(applied, 0);
    }

    /// A frame refused after the clock advanced (here a missed history
    /// replay) keeps the hits in step with the clock: the next frames still
    /// tick and take hits.
    #[test]
    fn swarm_recovers_from_a_refused_frame_and_still_takes_hits() {
        let mut rig = Rig::new(Some(swelling), SimStep::live(Seconds(TICK)));
        rig.play(60.0, 0.5, 0.0, &[]);
        let errors = rig.run(0.5 + 2.0 * TICK, &[], rig.step);
        assert!(errors.iter().any(|error| error.contains("never sampled")), "{errors:?}");
        assert_eq!(rig.node.published[0], 0.0, "a refused frame runs no tick");
        let mut kicked = Vec::new();
        let mut previous = 0.5 + 2.0 * TICK;
        rig.fire(previous, 1, FieldValue::uniform([0.0, 1.0, 0.0]).unwrap());
        for _ in 0..4 {
            let now = previous + TICK;
            let frame = rig.frame(previous, now, &[]);
            assert_eq!(frame.get("ticks"), 1.0);
            kicked.extend(rig.kicks(&frame).into_iter().map(|(tick, _)| tick));
            previous = now;
        }
        assert_eq!(kicked.len(), 1, "the hit lands once after the refused frame");
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
