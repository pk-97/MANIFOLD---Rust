//! `node.gas_domain` — a Smoke or Fire's place in its World: the box, the
//! World's clock it follows, the scene's solids, forces and hits it takes,
//! and the flames' light read back. It runs once a frame before the World's
//! tick loop and owns the clock of the gas's ticks: `node.gas_ticks` repeats
//! `node.gas_volume` once a tick, after the water's and the Matter's tick, so
//! the gas meets both as that tick left them (docs/SMOKE_AND_FIRE_MAP.md).

use std::borrow::Cow;

use manifold_core::fluid_domain::{CELL_SIZE_METRES, DEFAULT_CELL_SIZE};
use manifold_gpu::{BufferUse, GpuBuffer, GpuDevice};
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues, SimStep};
use manifold_node_engine::exec::substeps::{SubstepClockOutput, SubstepInterval};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_nodes_physics::force_lattice::{LatticeBuffers, LatticeUpload, felt_field};
use manifold_nodes_physics::hits::{TakesHits, TakesHitsRegistration};
use manifold_nodes_physics::tick_sampled::{TickSampled, TickSampledRegistration};
use manifold_physics::clock::{ClockFrame, LiveLoad, SimulationClock};
use manifold_physics::force_lattice::{ForceLattice, TickForces};
use manifold_physics::input::{AppliedEvent, EventStamp};
use manifold_physics::tick_hits::TickHits;
use manifold_physics::{FieldValue, PlayedTime, Seconds, TickStamp, VectorField};
use manifold_water_liquid::bodies::{BodiesStatus, LiquidBodies, LiquidBody, LiquidShape};
use manifold_water_liquid::body_buffers::LiquidBodyBuffers;
use manifold_water_liquid::coupling::{Followed, LiquidRigidOwner, SharedSolids, SharesSolids};

use super::gas_volume::{
    GasGeometry, LIGHT_BYTES, LIGHT_WORDS, LightReadout, RING, force_lattice, gas_geometry,
};

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
mod extent;

/// The longest tick the gas steps, in seconds: the solver stays stable
/// under a slow frame.
const MAX_STEP_SECONDS: f32 = 1.0 / 20.0;

/// The clock output scalars each tick publishes, in this order.
const TICK_SCALARS: [&str; 7] = [
    "tick_dt",
    "tick_index",
    "tick_last",
    "tick_forces",
    "tick_impulses",
    "tick_solid_base",
    "tick_solid_count",
];

/// One tick of the frame: its interval, and what the step reads in it (its
/// simulated seconds, its place in the frame, the force and impulse lattice
/// it reads, -1 for none, and its body rows: `count` of them from row `base`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct GasTick {
    timing: SubstepInterval,
    scalars: [(&'static str, f32); 7],
}

/// The ring slot frame `frame` writes and the one it reads (written two
/// frames earlier).
fn ring_slots(frame: u64) -> (usize, usize) {
    let ring = RING as u64;
    ((frame % ring) as usize, ((frame + 1) % ring) as usize)
}

/// A written light slot: when its frame retires, and the box origin the sums
/// are relative to.
#[derive(Clone, Copy)]
pub struct LightSlot {
    stamp: u64,
    origin: [f32; 3],
}

/// Decode a light sum buffer written by `cs_publish`: word 0 is the emission
/// sum, words 1 to 3 the emission times the cell centre relative to `origin`,
/// word 4 the emission times the temperature, all in
/// [`super::gas_volume::LIGHT_FIXED_SCALE`] fixed point. No emission reads as
/// no light.
pub(crate) fn decode_light(words: &[u32; LIGHT_WORDS], origin: [f32; 3]) -> LightReadout {
    let value = |word: u32| f64::from(word) / f64::from(super::gas_volume::LIGHT_FIXED_SCALE);
    let power = value(words[0]);
    if power <= 0.0 {
        return LightReadout::default();
    }
    LightReadout {
        power: power as f32,
        position: std::array::from_fn(|axis| {
            (f64::from(origin[axis]) + value(words[1 + axis]) / power) as f32
        }),
        temperature: (value(words[4]) / power) as f32,
    }
}

fn read_light_words(buffer: &GpuBuffer) -> [u32; LIGHT_WORDS] {
    let source = buffer
        .mapped_ptr()
        .expect("gas light sums must be CPU-visible")
        .cast::<u32>()
        .cast_const();
    let mut words = [0u32; LIGHT_WORDS];
    // SAFETY: shared, 4-byte aligned storage of LIGHT_WORDS words whose last
    // GPU writer has retired (the caller checked the frame clock).
    unsafe { std::ptr::copy_nonoverlapping(source, words.as_mut_ptr(), LIGHT_WORDS) };
    words
}

/// The Collider meshes as the first tick places them, every shape, the
/// trees' sizes and the box: a change of any rewrites the static solids.
/// Without bodies only the box counts.
fn static_hash(bodies: Option<&LiquidBodies>, geometry: &GasGeometry, moving: [u32; 8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    geometry.origin.map(f32::to_bits).hash(&mut hasher);
    geometry.dims.hash(&mut hasher);
    geometry.cell.to_bits().hash(&mut hasher);
    let Some(bodies) = bodies else {
        return hasher.finish();
    };
    let shapes = bodies.shapes();
    bytemuck::cast_slice::<LiquidShape, u8>(shapes).hash(&mut hasher);
    bodies.collider_vec4s().hash(&mut hasher);
    // A moving Collider is laid each tick, so only which ones move counts.
    moving.hash(&mut hasher);
    for (index, row) in tree_rows(bodies, &bodies.last_rows()[..first_tick_rows(bodies)]) {
        if !is_moving(moving, index) {
            bytemuck::bytes_of(row).hash(&mut hasher);
        }
    }
    hasher.finish()
}

/// Which Colliders read through their trees move this frame, one bit a body:
/// a row with motion, or a pose at any of the frame's ticks other than its
/// pose at the last frame's first tick (`last`). A Collider driven by a
/// param can take a new pose with no motion in its row.
fn moving_trees(bodies: &LiquidBodies, last: &[[f32; 7]]) -> [u32; 8] {
    let mut moving = [0; 8];
    let count = bodies.count();
    if count == 0 {
        return moving;
    }
    for (index, row) in tree_rows(bodies, bodies.last_rows()) {
        let body = index % count;
        if row_moves(row) || last.get(body) != Some(&pose(row)) {
            moving[body / 32] |= 1 << (body % 32);
        }
    }
    moving
}

fn is_moving(moving: [u32; 8], body: usize) -> bool {
    moving[body / 32] & (1 << (body % 32)) != 0
}

/// A row's position and rotation.
fn pose(row: &LiquidBody) -> [f32; 7] {
    let (p, q) = (row.position_inv_mass, row.rotation);
    [p[0], p[1], p[2], q[0], q[1], q[2], q[3]]
}

/// The rows of the frame's first tick, which the static solids are laid
/// from.
fn first_tick_rows(bodies: &LiquidBodies) -> usize {
    bodies.count().min(bodies.last_rows().len())
}

/// The rows among `rows` of Colliders read through their trees, with their
/// index.
fn tree_rows<'a>(
    bodies: &'a LiquidBodies,
    rows: &'a [LiquidBody],
) -> impl Iterator<Item = (usize, &'a LiquidBody)> {
    let shapes = bodies.shapes();
    rows.iter().enumerate().filter(move |(_, row)| {
        let shape = row.accel_shape[3];
        shape >= 0.0 && shapes.get(shape as usize).is_some_and(|s| s.tree == 1)
    })
}

/// A body's row carries motion.
fn row_moves(row: &LiquidBody) -> bool {
    row.linear_velocity[..3] != [0.0; 3] || row.angular_velocity[..3] != [0.0; 3]
}

/// Simulated seconds of the frame's tick `tick`.
fn tick_seconds(frame: &ClockFrame, tick: u32) -> f32 {
    frame
        .interval(u64::from(tick))
        .map_or(0.0, |interval| interval.duration().0) as f32
}

/// The arrays the domain hands the step, by port.
const PROVIDED: [&str; 9] = [
    "forces",
    "impulses",
    "bodies",
    "shapes",
    "atlas",
    "collider_nodes",
    "collider_triangles",
    "moving",
    "light",
];

manifold_node_engine::primitive! {
    name: GasDomain,
    type_id: "node.gas_domain",
    purpose: "A Smoke or Fire's place in its World, once a frame before the World's tick loop: the box (the domain Transform's position the centre, its scale the size; rotation is ignored) on whole cells of cell_metres, the World's fixed ticks (each tick's length times speed, at most 1/20 s; it runs the ticks of the water or Matter that leads the World, else its own), the scene's forces on acceleration_field laid on a lattice over the box at each tick's start, the hits landing on each tick, the scene's Colliders and Bodies a row per body per tick, and the flames' light read back two frames late. It is the clock owner of node.gas_ticks, which repeats node.gas_volume once a tick: each tick publishes tick_dt, tick_index, tick_last, the force and impulse lattice the tick reads (tick_forces, tick_impulses, -1 for none) and its body rows (tick_solid_base, tick_solid_count). Publishes the box's fine grid (cell_size, origin_x, origin_y, origin_z, dims_x, dims_y, dims_z), the frame's tick count (ticks) and whether the clock restarted (restarted), whether a solid moves (solids_move), static_epoch (changes when the still solids change), the lattices, body rows, shapes and trees as provided arrays, moving (one bit a body row: a Collider that moves this frame) and light (the sums buffer this frame's publish writes), and light_power, light_x, light_y, light_z and light_temperature.",
    inputs: {
        domain: Transform optional,
        cell_metres: ScalarF32 optional,
        speed: ScalarF32 optional,
        reset: ScalarF32 optional,
        acceleration_field: VectorField optional,
    },
    outputs: {
        interval_duration: ScalarF32,
        ticks: ScalarF32,
        restarted: ScalarF32,
        tick_dt: ScalarF32,
        tick_index: ScalarF32,
        tick_last: ScalarF32,
        tick_forces: ScalarF32,
        tick_impulses: ScalarF32,
        tick_solid_base: ScalarF32,
        tick_solid_count: ScalarF32,
        solids_move: ScalarF32,
        static_epoch: ScalarF32,
        forces: Array(f32),
        impulses: Array(f32),
        bodies: Array(LiquidBody),
        shapes: Array(LiquidShape),
        atlas: Array(u32),
        collider_nodes: Array(manifold_node_engine::mesh::Vec4Vertex),
        collider_triangles: Array(manifold_node_engine::mesh::Vec4Vertex),
        moving: Array(u32),
        light: Array(u32),
        cell_size: ScalarF32,
        origin_x: ScalarF32, origin_y: ScalarF32, origin_z: ScalarF32,
        dims_x: ScalarF32, dims_y: ScalarF32, dims_z: ScalarF32,
        light_power: ScalarF32,
        light_x: ScalarF32, light_y: ScalarF32, light_z: ScalarF32,
        light_temperature: ScalarF32,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("cell_metres"),
            label: "Cell Size",
            ty: ParamType::Float,
            default: ParamValue::Float(DEFAULT_CELL_SIZE),
            range: Some(CELL_SIZE_METRES),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire the Smoke or Fire item's box Transform into domain, the cell into cell_metres, and the World's speed and reset into their inputs. interval_duration feeds node.gas_ticks' clock and ticks its ticks; every tick_* scalar, the fine grid, restarted, solids_move, static_epoch and the provided arrays feed node.gas_volume, the step inside the tick loop. The light_* scalars and the fine grid feed the scene object. A change of cell_metres or of the box's size past a whole cell restarts the gas; moving the box does not.",
    examples: [],
    picker: { label: "Gas Domain", category: Atom },
    summary: "A Smoke or Fire's box, clock, solids, forces and light.",
    category: Particles3D,
    role: Source,
    aliases: ["smoke domain", "fire domain", "gas clock"],
    boundary_reason: CrossFrameState,
    extra_fields: {
        // The World's fixed ticks: the gas steps once a tick, so a force or
        // a hit lands on the same step at any frame rate, as water does.
        clock: SimulationClock = SimulationClock::default(),
        schedule: Vec<GasTick> = Vec::new(),
        restarted: bool = false,
        // The scene's forces laid at each tick's start on a lattice over the
        // box, the hits landing on the frame's ticks, and their GPU copies.
        forces: TickForces = TickForces::default(),
        hits: TickHits<FieldValue> = TickHits::new("Smoke and Fire impulses"),
        impulses: Vec<[f32; 4]> = Vec::new(),
        impulses_dirty: bool = false,
        lattices: LatticeBuffers = LatticeBuffers::default(),
        // The scene's Colliders and Bodies, handed over before the gas runs;
        // their shapes and lattices at the gas's cell, a row per body per
        // tick, and the GPU copies.
        solids: SharedSolids = SharedSolids::default(),
        bodies: LiquidBodies = LiquidBodies::default(),
        body_buffers: LiquidBodyBuffers = LiquidBodyBuffers::default(),
        // The first tick's body poses last frame, to tell which Colliders
        // moved since.
        tree_poses: Vec<[f32; 7]> = Vec::new(),
        // The moving Colliders' bits as the step reads them.
        moving: [u32; 8] = [0; 8],
        moving_buffer: Option<GpuBuffer> = None,
        // The still solids the step lays, by `static_hash`, and a count of
        // their changes.
        static_hash: Option<u64> = None,
        static_epoch: u32 = 0,
        solids_move: bool = false,
        // The light sums ring: this frame's write, the one still in flight and
        // the one the CPU reads.
        lights: Option<[GpuBuffer; RING]> = None,
        light_write: usize = 0,
        light_slots: [Option<LightSlot>; RING] = [None; RING],
        light: LightReadout = LightReadout::default(),
        // Frames whose fields the step published since the last clear.
        frames: u64 = 0,
        dims: [u32; 3] = [0; 3],
    },
}

/// Set every scalar output, so no consumer reads a slot this node left
/// unwritten; the tick scalars are each tick's, published before it.
fn publish(ctx: &mut EffectNodeContext<'_, '_>, values: impl IntoIterator<Item = (&'static str, f32)>) {
    for (name, value) in values {
        ctx.outputs.set_scalar(name, ParamValue::Float(value));
    }
}

impl GasDomain {
    /// Follow the World's clock (or advance its own) and lay the frame's
    /// ticks: each tick's length, the force lattice it reads (one while the
    /// field holds still, else one a tick, none without a field), the impulse
    /// lattice of the hits landing on it and its body rows. Returns whether
    /// the scene has solids this frame.
    fn plan_ticks(
        &mut self,
        ctx: &EffectNodeContext<'_, '_>,
        geometry: &GasGeometry,
    ) -> Result<bool, String> {
        self.schedule.clear();
        self.restarted = false;
        // The air itself: no still air of its own (Air Resistance is the
        // air), wind felt at the full drag rate.
        let field = felt_field(ctx, "Gas Volume")?;
        // The gas runs the ticks of the World's lead; alone, it advances its
        // own clock.
        let frame = match self.solids.followed_frame(&mut self.clock, false) {
            Followed::Own => self.clock.advance(
                ctx.played(),
                ctx.sim_step.interval.0,
                ctx.scalar_or_param("speed", 1.0),
                ctx.scalar_or_param("reset", 0.0),
                false,
                ctx.sim_step.offline(),
            ),
            Followed::Hold => {
                // The lead advanced nothing this frame, so no tick runs.
                self.solids.used();
                return Ok(false);
            }
            Followed::Frame(frame) => frame,
        };
        // Hits follow the clock before anything else can fail, so a refused
        // frame never leaves them a frame behind it.
        self.hits.observe_frame(ctx.played(), &frame)?;
        let lattice = force_lattice(geometry);
        self.forces
            .prepare(lattice, field.as_ref(), &self.clock, &frame)?;
        let hit_ticks = self.lay_impulses(lattice, frame.first_sequence, frame.ticks)?;
        let solids = self.lay_solids(geometry.cell, &frame, ctx.sim_step.offline());
        // The next frame takes the solids anew.
        self.solids.used();
        let solids = solids?;
        let lattices = self.forces.lattices() as u32;
        for tick in 0..frame.ticks {
            let interval = frame
                .interval(u64::from(tick))
                .expect("an accepted gas tick");
            let index = |on: bool, at: u32| if on { at as f32 } else { -1.0 };
            let (base, count) = solids.map_or((0, 0), |count| (tick * count, count));
            self.schedule.push(GasTick {
                timing: SubstepInterval {
                    start: interval.start,
                    end: interval.end,
                    ordinal: tick,
                    first_iteration: tick,
                    iterations: 1,
                    total_iterations: frame.ticks,
                },
                scalars: [
                    (
                        TICK_SCALARS[0],
                        tick_seconds(&frame, tick).clamp(0.0, MAX_STEP_SECONDS),
                    ),
                    (TICK_SCALARS[1], tick as f32),
                    (TICK_SCALARS[2], f32::from(u8::from(tick + 1 == frame.ticks))),
                    (
                        TICK_SCALARS[3],
                        index(lattices > 0, tick.min(lattices.saturating_sub(1))),
                    ),
                    (TICK_SCALARS[4], index(hit_ticks, tick)),
                    (TICK_SCALARS[5], base as f32),
                    (TICK_SCALARS[6], count as f32),
                ],
            });
        }
        self.restarted = frame.restarted;
        Ok(solids.is_some())
    }

    /// Lay this frame's solids: their shapes at the gas's cell and a row per
    /// body per tick, each carried to its tick's start. Returns the bodies a
    /// tick, or None when the scene has none, the frame has no tick, or
    /// (live) while a shape's tree is still building.
    fn lay_solids(
        &mut self,
        cell: f32,
        frame: &ClockFrame,
        offline: bool,
    ) -> Result<Option<u32>, String> {
        if let Some(reason) = self.solids.refused() {
            return Err(format!("Gas Volume: the scene's solids: {reason}"));
        }
        if self.solids.geometries().is_empty() || frame.ticks == 0 {
            return Ok(None);
        }
        if self
            .bodies
            .prepare(&[], self.solids.geometries(), cell, offline)?
            == BodiesStatus::Pending
        {
            return Ok(None);
        }
        self.bodies
            .rows(frame.first_sequence, frame.ticks, self.solids.rows())?;
        // The owner water's rows sit at the first tick's start; a
        // free-running World's at the frame's end.
        let mut seconds = if self.solids.at_frame_end() {
            -(0..frame.ticks)
                .map(|tick| tick_seconds(frame, tick))
                .sum::<f32>()
        } else {
            0.0
        };
        for tick in 0..frame.ticks {
            if seconds != 0.0 {
                self.bodies
                    .set_coupled_rows(tick as usize, self.solids.rows_after(seconds))?;
            }
            seconds += tick_seconds(frame, tick);
        }
        Ok(Some(self.bodies.count() as u32))
    }

    /// Lay this frame's hits, one impulse lattice per tick of the frame from
    /// `first`, each the sum of the hits landing on that tick (zero where
    /// none did). False when no hit landed: nothing is laid or read.
    fn lay_impulses(
        &mut self,
        lattice: ForceLattice,
        first: u64,
        ticks: u32,
    ) -> Result<bool, String> {
        if self.hits.impulse_tick().is_none() {
            return Ok(false);
        }
        let events = self.hits.frame_events();
        let count = lattice.node_count();
        self.impulses.clear();
        self.impulses.resize(ticks as usize * count, [0.0; 4]);
        for (tick, values) in (first..).zip(self.impulses.chunks_exact_mut(count)) {
            if !events.iter().any(|event| event.applied.tick == tick) {
                continue;
            }
            lattice
                .fill(values, |x| {
                    events
                        .iter()
                        .filter(|event| event.applied.tick == tick)
                        .fold([0.0; 3], |sum, event| {
                            let kick = event.value.sample(x);
                            std::array::from_fn(|axis| sum[axis] + kick[axis])
                        })
                })
                .map_err(|_| "Gas Volume: a hit is not finite inside the box".to_string())?;
        }
        self.impulses_dirty = true;
        Ok(true)
    }

    /// Copy the frame's force and impulse lattices to the GPU when any tick
    /// reads one; with neither, nothing is laid or uploaded.
    fn upload_lattices(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        lattice: ForceLattice,
        offline: bool,
    ) -> Result<(), String> {
        let reads = self
            .schedule
            .iter()
            .any(|tick| tick.scalars[3].1 >= 0.0 || tick.scalars[4].1 >= 0.0);
        if !reads {
            return Ok(());
        }
        self.lattices.upload(
            gpu,
            offline,
            LatticeUpload {
                lattice,
                forces: self.forces.forces(),
                forces_dirty: self.forces.is_dirty(),
                impulses: &self.impulses,
                impulses_dirty: self.impulses_dirty,
                hits: &[],
                tick_seconds: &[],
                far: &[],
            },
        )?;
        self.forces.mark_uploaded();
        self.impulses_dirty = false;
        Ok(())
    }

    /// The bodies the step reads: which Colliders move, whether the still
    /// solids changed, and the body rows and shapes on the GPU when a step
    /// lays them. Colliders read through their trees are laid only when they
    /// or the box change; the moving solids, a lattice each, every tick.
    fn lay_bodies(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        geometry: &GasGeometry,
        solids: bool,
    ) -> Result<(), String> {
        let mut moving = [0; 8];
        if solids {
            moving = moving_trees(&self.bodies, &self.tree_poses);
            let first = &self.bodies.last_rows()[..first_tick_rows(&self.bodies)];
            self.tree_poses.clear();
            self.tree_poses.extend(first.iter().map(pose));
        }
        let hash = static_hash(solids.then_some(&self.bodies), geometry, moving);
        let changed = self.static_hash != Some(hash);
        if changed {
            self.static_hash = Some(hash);
            // An f32 scalar holds every whole number below 2^24 exactly.
            self.static_epoch = (self.static_epoch + 1) % (1 << 24);
        }
        self.solids_move = solids
            && (self.bodies.shapes().iter().any(|s| s.tree != 1) || moving.iter().any(|&w| w != 0));
        if solids && (changed || self.solids_move) {
            self.body_buffers
                .upload(gpu, &self.bodies, true, "node.gas_domain.bodies")?;
        }
        let fresh = self.moving_buffer.is_none();
        if fresh || moving != self.moving {
            let buffer = self
                .moving_buffer
                .get_or_insert_with(|| gpu.device.create_buffer(32, BufferUse::Gpu));
            let rows = [
                [moving[0], moving[1], moving[2], moving[3]],
                [moving[4], moving[5], moving[6], moving[7]],
            ];
            self.body_buffers
                .write_rows(gpu, buffer, &rows, "node.gas_domain.moving");
            self.moving = moving;
        }
        Ok(())
    }

    /// The light ring: cleared with the gas, read back once the slot written
    /// two frames ago has retired, and this frame's slot handed to the step
    /// when a tick runs.
    fn keep_light(&mut self, device: &GpuDevice, geometry: &GasGeometry) {
        let lights = self.lights.get_or_insert_with(|| {
            std::array::from_fn(|_| device.create_buffer(LIGHT_BYTES, BufferUse::Readback))
        });
        if self.restarted || self.dims != geometry.dims {
            self.dims = geometry.dims;
            self.frames = 0;
            self.light_slots = [None; RING];
            self.light = LightReadout::default();
        }
        let clock = device.frame_clock();
        let (write_slot, read_slot) = ring_slots(self.frames);
        if self.frames >= RING as u64 - 1
            && let Some(slot) = self.light_slots[read_slot]
            && clock
                .as_ref()
                .is_none_or(|clock| clock.is_complete(slot.stamp))
        {
            self.light = decode_light(&read_light_words(&lights[read_slot]), slot.origin);
        }
        self.light_write = write_slot;
        if !self.schedule.is_empty() {
            self.light_slots[write_slot] = Some(LightSlot {
                stamp: clock.as_ref().map_or(0, |clock| clock.stamp()),
                origin: geometry.origin,
            });
            self.frames += 1;
        }
    }

    /// Plan the frame's ticks and lay what they read. On an error no tick
    /// runs and the frame's hits are discarded with a receipt, never
    /// replayed.
    fn frame(
        &mut self,
        ctx: &mut EffectNodeContext<'_, '_>,
        geometry: &GasGeometry,
    ) -> Result<(), String> {
        let solids = self.plan_ticks(ctx, geometry)?;
        let offline = ctx.sim_step.offline();
        let gpu = ctx.gpu_encoder();
        self.lay_bodies(gpu, geometry, solids)?;
        self.upload_lattices(gpu, force_lattice(geometry), offline)?;
        self.keep_light(gpu.device, geometry);
        // A hit is applied once its tick's lattice is on the GPU.
        self.hits.commit_frame();
        Ok(())
    }

    /// The scalars this frame publishes, every tick scalar at 0 until its
    /// tick publishes it.
    fn outputs(&self, geometry: &GasGeometry) -> impl Iterator<Item = (&'static str, f32)> {
        let frame = [
            ("interval_duration", 0.0),
            ("ticks", self.schedule.len() as f32),
            ("restarted", f32::from(u8::from(self.restarted))),
            ("solids_move", f32::from(u8::from(self.solids_move))),
            ("static_epoch", self.static_epoch as f32),
        ];
        geometry
            .outputs()
            .into_iter()
            .chain(self.light.outputs())
            .chain(frame)
            .chain(TICK_SCALARS.map(|name| (name, 0.0)))
    }
}

impl TickSampled for GasDomain {
    fn request_physics_samples(
        &mut self,
        from: PlayedTime,
        until: PlayedTime,
        out: &mut Vec<PlayedTime>,
    ) {
        self.forces.request_samples(&self.clock, from, until, out);
    }
}

inventory::submit! { TickSampledRegistration::new::<GasDomain>() }

impl TakesHits for GasDomain {
    fn hit_epoch(&self) -> Option<u64> {
        self.hits.epoch()
    }

    /// A gas whose clock has not started makes a hit wait; a broken clock
    /// refuses it.
    fn hit_readiness(&self) -> Result<bool, String> {
        match self.hits.failure() {
            Some(error) => Err(error.into()),
            None => Ok(self.hits.epoch().is_some()),
        }
    }

    /// A hit is stamped at the fire's own simulated time, so it lands on the
    /// tick whose interval holds that time, the same tick at any frame rate.
    fn hit_stamp(
        &self,
        played: PlayedTime,
        sequence: u64,
        step: SimStep,
    ) -> Result<EventStamp, String> {
        if !self.hit_readiness()? {
            return Err(
                "Smoke and Fire impulses: the gas has not started; fire waits for it".into(),
            );
        }
        let mut stamp = self.hits.stamp(played, sequence)?;
        let load = step.load().map(|load| LiveLoad {
            previous: load.previous,
            budget: load.budget,
        });
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

inventory::submit! { TakesHitsRegistration::new::<GasDomain>() }

/// A gas owns no rigid world: it takes the scene's solids from the water
/// that does, or from the World.
impl SharesSolids for GasDomain {
    fn rigid_owner(&self) -> Option<&LiquidRigidOwner> {
        None
    }

    fn shared_solids_mut(&mut self) -> &mut SharedSolids {
        &mut self.solids
    }
}

impl Primitive for GasDomain {
    fn provides_array_output(&self, port: &str) -> bool {
        PROVIDED.contains(&port)
    }

    fn provided_array_output(&self, port: &str) -> Option<&GpuBuffer> {
        match port {
            "forces" => self.lattices.forces(),
            "impulses" => self.lattices.impulses(),
            "moving" => self.moving_buffer.as_ref(),
            "light" => self.lights.as_ref().map(|lights| &lights[self.light_write]),
            _ => self.body_buffers.output(port),
        }
    }

    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        _inputs: &[(&str, u32)],
    ) -> Option<u32> {
        // Provided storage: a one-record hint, sized at run time.
        PROVIDED.contains(&port).then_some(1)
    }

    fn clear_state(&mut self) {
        self.clock.restart();
        self.solids.reseed();
    }

    fn acceleration_input(&self) -> Option<&'static str> {
        Some("acceleration_field")
    }

    fn substep_clock_interval(&self, iteration: u32) -> Option<SubstepClockOutput<'_>> {
        let tick = self.schedule.get(iteration as usize)?;
        Some(SubstepClockOutput {
            duration_port: "interval_duration",
            timing: tick.timing,
            scalars: &tick.scalars,
        })
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        self.clock
            .set_live_load(ctx.sim_step.load().map(|load| LiveLoad {
                previous: load.previous,
                budget: load.budget,
            }));
        self.clock.set_played_target(ctx.sim_step.played_target);
        // A physics sample reads the field at a tick's start; it never
        // advances time.
        if ctx.sim_step.authored_sample_only {
            self.schedule.clear();
            self.clock
                .observe_speed(ctx.played(), ctx.scalar_or_param("speed", 1.0));
            match felt_field(ctx, "Gas Volume") {
                Ok(field) => self
                    .forces
                    .observe_sample(&self.clock, ctx.played(), field.as_ref()),
                Err(error) => ctx.error(error),
            }
            return;
        }
        let read = |name: &str, default: f32| ctx.scalar_or_param(name, default);
        let outcome = gas_geometry(read, ctx.inputs.transform("domain"))
            .and_then(|geometry| self.frame(ctx, &geometry).map(|()| geometry));
        match outcome {
            Ok(geometry) => {
                let outputs: Vec<_> = self.outputs(&geometry).collect();
                publish(ctx, outputs);
            }
            Err(error) => {
                self.schedule.clear();
                self.hits.abandon_frame();
                ctx.error(error);
                let outputs: Vec<_> = self.outputs(&GasGeometry::default()).collect();
                publish(ctx, outputs);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_sums_decode_to_the_emission_centre_and_temperature() {
        let encode = |value: f64| (value * f64::from(super::super::gas_volume::LIGHT_FIXED_SCALE)).round() as u32;
        // Three workgroups' (emission, centre relative to the origin,
        // temperature), summed as the shader's atomics do.
        let groups = [
            (0.5_f64, [1.0_f64, 2.0, 0.5], 0.9_f64),
            (0.25, [1.5, 2.5, 1.0], 0.5),
            (0.125, [0.5, 1.0, 0.25], 1.0),
        ];
        let mut words = [0u32; LIGHT_WORDS];
        for (power, centre, temperature) in groups {
            words[0] = words[0].wrapping_add(encode(power));
            for (word, c) in words[1..4].iter_mut().zip(centre) {
                *word = word.wrapping_add(encode(power * c));
            }
            words[4] = words[4].wrapping_add(encode(power * temperature));
        }
        // A box whose corner is at a negative position, so the centre comes
        // out negative on x.
        let origin = [-3.5_f32, 0.25, -1.0];
        let light = decode_light(&words, origin);
        let power: f64 = groups.iter().map(|g| g.0).sum();
        assert!((f64::from(light.power) - power).abs() < 2.0 / 16384.0);
        for (axis, (&o, &got)) in origin.iter().zip(&light.position).enumerate() {
            let expected =
                f64::from(o) + groups.iter().map(|g| g.0 * g.1[axis]).sum::<f64>() / power;
            assert!(
                (f64::from(got) - expected).abs() < 2e-4,
                "axis {axis}: {got} vs {expected}"
            );
        }
        assert!(light.position[0] < 0.0);
        let temperature = groups.iter().map(|g| g.0 * g.2).sum::<f64>() / power;
        assert!((f64::from(light.temperature) - temperature).abs() < 2e-4);
        assert_eq!(
            decode_light(&[0; LIGHT_WORDS], origin),
            LightReadout::default()
        );
    }

    #[test]
    fn the_light_ring_reads_what_was_written_two_frames_ago() {
        for frame in 2..30u64 {
            let (_, read) = ring_slots(frame);
            let (written_then, _) = ring_slots(frame - 2);
            assert_eq!(read, written_then, "frame {frame}");
            assert_ne!(ring_slots(frame).0, read, "frame {frame}");
        }
    }

    /// A frame's ticks publish their own seconds, place and lattices, and
    /// only the last says so.
    #[test]
    fn each_tick_publishes_its_own_scalars() {
        let tick = |dt: f32, index: u32, last: bool| GasTick {
            timing: SubstepInterval {
                start: manifold_core::Seconds(0.0),
                end: manifold_core::Seconds(f64::from(dt)),
                ordinal: index,
                first_iteration: index,
                iterations: 1,
                total_iterations: 3,
            },
            scalars: [
                (TICK_SCALARS[0], dt),
                (TICK_SCALARS[1], index as f32),
                (TICK_SCALARS[2], f32::from(u8::from(last))),
                (TICK_SCALARS[3], -1.0),
                (TICK_SCALARS[4], -1.0),
                (TICK_SCALARS[5], 0.0),
                (TICK_SCALARS[6], 0.0),
            ],
        };
        let domain = GasDomain {
            schedule: vec![tick(0.02, 0, false), tick(0.015, 1, false), tick(0.01, 2, true)],
            ..GasDomain::default()
        };
        let second = Primitive::substep_clock_interval(&domain, 1).expect("tick 1");
        assert_eq!(second.timing.ordinal, 1);
        assert_eq!(second.scalars[0].1, 0.015);
        assert_eq!(second.scalars[2].1, 0.0);
        let last = Primitive::substep_clock_interval(&domain, 2).expect("tick 2");
        assert_eq!(last.scalars[2].1, 1.0);
        assert!(Primitive::substep_clock_interval(&domain, 3).is_none());
    }
}
