//! Sheet seeding inside node.gpu_flip_step (BUG-j9l9w): births join the
//! substep they are seeded in, survive what follows, share the pool and the
//! identity counter with inflow and removal, and an inactive clock slot
//! writes nothing. The splash fixture (`manifold_fluids::sheeter::fixtures`)
//! is the particles; the step's own surface distance is the level set.

use super::gpu_flip_sheeting::{FILL_THRESHOLD, GpuSheeting, SheetInputs, StepBirths};
use super::gpu_flip_sheeting_tests::{World, padded_cells, padded_markers, sort, to_world};
use super::gpu_flip_step::{BOX_BAND_CELLS, GpuFlipStep, pool_tiles};
use super::gpu_flip_step_tests::{cpu_sample_on, step_faces};
use manifold_fluids::sheeter;
use manifold_gpu::BufferUse;
use manifold_gpu::{GpuBuffer, GpuDevice};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::{lattice_origin, pool_len};
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_liquid::bodies::{LiquidBody, LiquidShape, pack_distance_atlas};
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::primitives::liquid_stats::{
    LIQUID_STATS_WORDS, LiquidTickStats, SHEET_REFUSED_TAIL, SHEET_REFUSED_WORD,
    SOLVER_STATS_START, SOLVER_WORDS,
};
use manifold_water_liquid::primitives::particle_identity::{
    BirthReservation, COUNT_OFFSET, ParticleIdentity,
};
use manifold_water_liquid::testkit::marker_phi::marker_phi;
use manifold_water_liquid::tile_pool::solver_origin_cell;
use manifold_water_liquid::world_tiles::exhausted_word;

const H: f32 = 0.25;
/// The splash's grid is the step's solver grid: 37 box cells and the step's
/// three, world cells -20..20 on every axis, so it crosses the origin.
const SOLVER: [usize; 3] = [40; 3];
/// The solver grid's first node, world cell -20's low face.
const SOLVER_MIN: f32 = -20.5 * H;
const DT: f32 = 1.0 / 120.0;
const RADIUS: f32 = 0.31017524 * H;
const V0: [f32; 3] = [0.4, 0.1, -0.3];

/// A particle identity: the next id, epoch 1, and the record of `live` slots in
/// use (`[groups, 1, 1, n]` at 256 threads a group), as the state publishes it.
fn identity_words(next_id: u32, live: usize) -> [u32; 8] {
    let live = live as u32;
    [next_id, 1, 0, 0, live.div_ceil(256).max(1), 1, 1, live]
}

fn copy_out(device: &GpuDevice, buffer: &GpuBuffer) -> Vec<u32> {
    let staged = device.create_buffer(buffer.size, BufferUse::Upload);
    let mut enc = device.create_encoder("sheeting step proof readback");
    enc.copy_buffer_to_buffer(buffer, &staged, buffer.size);
    enc.commit_and_wait().expect("GPU work completes");
    read(&staged, (buffer.size / 4) as usize)
}

/// A point `grid` metres from the solver grid's first node, in the world.
fn world(grid: [f32; 3]) -> [f32; 3] {
    grid.map(|v| v + SOLVER_MIN)
}

/// An inflow (code 2) or outflow (code 3) over the world box `lo` to
/// `lo + 4 H` on each axis.
fn region(code: f32, lo: [f32; 3]) -> LiquidBody {
    LiquidBody {
        position_inv_mass: [lo[0], lo[1], lo[2], 0.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
        angular_velocity: [0.0, 0.0, 0.0, code],
        inv_inertia_x: [0.0, -0.5, 0.0, 0.0],
        accel_shape: [0.0; 4],
        ..LiquidBody::default()
    }
}

fn inside(p: &FluidParticle, lo: [f32; 3]) -> bool {
    (0..3).all(|a| p.position_radius[a] >= lo[a] && p.position_radius[a] <= lo[a] + 4.0 * H)
}

/// The region-site rows of `regions` (gpu_flip_domain's `RegionSites`, 8
/// words a row: first world half cell, first index in the tick, sites per
/// axis, count) and their total: an inflow's the half cells round its box,
/// one more each side (emission tests every site against the region), an
/// outflow none. Half cell J sits at (J/2 − 1/4)·h.
fn region_sites(regions: &[LiquidBody]) -> (Vec<u32>, u32) {
    let mut words = Vec::new();
    let mut total = 0u32;
    for row in regions {
        let lo: [f32; 3] = std::array::from_fn(|a| row.position_inv_mass[a]);
        let first = lo.map(|v| (2.0 * v / H).floor() as i32 - 1);
        let size: [u32; 3] = std::array::from_fn(|a| {
            ((2.0 * (lo[a] + 4.0 * H) / H).ceil() as i32 + 1 - first[a] + 1) as u32
        });
        let count = if row.angular_velocity[3] == 2.0 {
            size.iter().product()
        } else {
            0
        };
        words.extend(first.map(|j| j as u32));
        words.push(total);
        words.extend(size);
        words.push(count);
        total += count;
    }
    (words, total)
}

struct Splash {
    h: Harness,
    node: GpuFlipStep,
    capacity: usize,
    particles: (manifold_node_engine::bindings::Slot, GpuBuffer),
    identity: (manifold_node_engine::bindings::Slot, GpuBuffer),
    faces: manifold_node_engine::bindings::Slot,
    tiles: manifold_node_engine::bindings::Slot,
    capped: manifold_node_engine::bindings::Slot,
    regions: Option<[manifold_node_engine::bindings::Slot; 7]>,
    region_count: usize,
    site_total: u32,
    markers: usize,
}

struct Tick {
    particles: Vec<FluidParticle>,
    identity: [u32; 4],
    /// Last substep requested and written, then both summed.
    stats: [u32; 4],
}

impl Splash {
    fn new(room: usize, next_id: u32, regions: &[LiquidBody]) -> Self {
        let (markers, _, cells, dx) = sheeter::fixtures::splash();
        assert_eq!((cells, dx as f32), ([40; 3], H));
        let capacity = markers.len() + room;
        let records: Vec<FluidParticle> = markers
            .iter()
            .enumerate()
            .map(|(i, m)| FluidParticle {
                position_radius: [
                    m[0] + SOLVER_MIN,
                    m[1] + SOLVER_MIN,
                    m[2] + SOLVER_MIN,
                    RADIUS,
                ],
                velocity: V0,
                id: i as u32 + 1,
            })
            .collect();
        let mut h = Harness::new();
        let mut node = GpuFlipStep::new();
        node.prepare_pipelines(&h.device);
        let particles = h.array(&records, capacity);
        let identity = h.array::<u32>(&identity_words(next_id, markers.len()), 8);
        let faces = h
            .array::<FaceSample>(&[], SOLVER.map(|n| n + 1).iter().product())
            .0;
        let tiles = h.array::<u32>(&[], 1).0;
        let capped = h
            .array::<u32>(
                &[],
                2 * capacity
                    + manifold_water_liquid::primitives::liquid_stats::SOLVER_WORDS as usize,
            )
            .0;
        let region_count = regions.len();
        let (sites, site_total) = region_sites(regions);
        let region_slots = (!regions.is_empty()).then(|| {
            let dims = [5u32; 3];
            let mut atlas = Vec::new();
            pack_distance_atlas(&vec![-1.0; 125], &mut atlas);
            let shape = LiquidShape {
                origin_spacing: [0.0, 0.0, 0.0, H],
                dims_x: dims[0],
                dims_y: dims[1],
                dims_z: dims[2],
                atlas_offset: 0,
                scale_min: [1.0; 4],
                ..Default::default()
            };
            [
                h.array(regions, regions.len()).0,
                h.array(&sites, sites.len()).0,
                h.array(&[shape], 1).0,
                h.array(&atlas, atlas.len()).0,
                h.array::<manifold_node_engine::mesh::Vec4Vertex>(&[], 1).0,
                h.array::<manifold_node_engine::mesh::Vec4Vertex>(&[], 1).0,
                h.array::<LiquidBody>(&[], 1).0,
            ]
        });
        Self {
            h,
            node,
            capacity,
            particles,
            identity,
            faces,
            tiles,
            capped,
            regions: region_slots,
            region_count,
            site_total,
            markers: markers.len(),
        }
    }

    fn tick(&mut self, tick: u32, steps: u32, rate: f32) -> Tick {
        let n = SOLVER[0] as u32 - BOX_BAND_CELLS;
        let min = SOLVER_MIN + 1.5 * H;
        let max = min + n as f32 * H;
        let mut values = vec![
            ("pool_tiles", pool_tiles([n; 3], 63) as f32),
            ("cell_size", H),
            ("bounds_lo_x", min),
            ("bounds_lo_y", min),
            ("bounds_lo_z", min),
            ("bounds_hi_x", max),
            ("bounds_hi_y", max),
            ("bounds_hi_z", max),
            ("closed_faces", 63.0),
            ("gravity_y", -9.81),
            ("interval_duration", DT * steps as f32),
            ("steps", steps as f32),
            ("flip", 0.95),
            ("iterations", 0.0),
            ("volume_projection", 0.0),
            ("sheet_fill_rate", rate),
            ("tick_index", tick as f32),
        ];
        let mut inputs = vec![
            ("particles", self.particles.0),
            ("identity", self.identity.0),
        ];
        if let Some(
            [
                regions,
                sites,
                shapes,
                atlas,
                collider_nodes,
                collider_triangles,
                bodies,
            ],
        ) = self.regions
        {
            values.extend([
                ("region_count", self.region_count as f32),
                ("region_site_total", self.site_total as f32),
                ("first_tick", tick as f32),
            ]);
            inputs.extend([
                ("regions", regions),
                ("region_sites", sites),
                ("shapes", shapes),
                ("atlas", atlas),
                ("collider_nodes", collider_nodes),
                ("collider_triangles", collider_triangles),
                ("bodies", bodies),
            ]);
        }
        let p = params(&values);
        let outputs = [
            ("out", self.particles.0),
            ("faces", self.faces),
            ("tiles", self.tiles),
            ("capped", self.capped),
        ];
        let (_, errors) = self.h.run(&mut self.node, &inputs, &outputs, &p);
        assert!(errors.is_empty(), "{errors:?}");
        let stats = if rate > 0.0 || self.node_has_sheeting() {
            let words: Vec<u32> = read(self.node.sheeting().stats(), 4);
            [words[0], words[1], words[2], words[3]]
        } else {
            [0; 4]
        };
        let identity: Vec<u32> = read(&self.identity.1, 4);
        Tick {
            particles: read(&self.particles.1, self.capacity),
            identity: [identity[0], identity[1], identity[2], identity[3]],
            stats,
        }
    }

    /// The tick's refused sheet births: the capped tail word the step keeps
    /// for the published stats.
    fn refused_births(&self) -> u32 {
        let words: Vec<u32> = read(
            &self.h.buffer(self.capped),
            2 * self.capacity + SOLVER_WORDS as usize,
        );
        words[2 * self.capacity + SHEET_REFUSED_TAIL as usize]
    }

    fn node_has_sheeting(&self) -> bool {
        self.node.sheeting().reserved()
    }

    fn faces(&self) -> Vec<FaceSample> {
        step_faces(
            &self.h,
            self.faces,
            self.tiles,
            solver_origin_cell([SOLVER_MIN; 3], H),
            SOLVER.map(|n| n as u32),
        )
    }

    /// Entry i: the seed written at birth index i, lattice-local (the world
    /// less the lattice origin).
    fn births(&self, written: u32) -> Vec<[f32; 3]> {
        let words = copy_out(&self.h.device, self.node.sheeting().births().0);
        (0..written as usize)
            .map(|i| [0, 1, 2].map(|a| f32::from_bits(words[4 * i + a])))
            .collect()
    }
}

fn alive(particles: &[FluidParticle]) -> impl Iterator<Item = &FluidParticle> {
    particles.iter().filter(|p| p.position_radius[3] > 0.0)
}

fn assert_unique_ids(particles: &[FluidParticle], label: &str) {
    let mut ids: Vec<u32> = alive(particles).map(|p| p.id).collect();
    let n = ids.len();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), n, "{label}: duplicate ids");
}

fn close(got: f32, want: f64, tolerance: f64, label: &str) {
    assert!(
        (f64::from(got) - want).abs() <= tolerance,
        "{label}: {got} vs {want}"
    );
}

/// A birth takes its substep's FLIP update and move: its velocity is the new
/// field at its seed (saved velocity in, so FLIP's change gives the new one)
/// and it lands where RK3 on the published faces takes the seed. Births then
/// survive a two-substep tick with no new seeding, and another tick.
#[test]
fn gpu_flip_sheeting_births_move_in_their_substep_and_survive() {
    let mut splash = Splash::new(8000, 4001, &[]);
    let first = splash.tick(0, 1, 1.0);
    let [requested, written, requested_total, written_total] = first.stats;
    assert!(written > 100, "the splash seeds sheets: {written}");
    assert_eq!(
        (requested, written),
        (requested_total, written_total),
        "one active substep"
    );
    assert_eq!(requested, written, "the pool has room");
    let base = 4001;
    assert_eq!(first.identity[2], base);
    assert_eq!(first.identity[0], base + written);
    assert_unique_ids(&first.particles, "first tick");
    let field = splash.faces();
    let seeds = splash.births(written);
    let born: Vec<&FluidParticle> = alive(&first.particles).filter(|p| p.id >= base).collect();
    assert_eq!(
        born.len(),
        written as usize,
        "every birth is alive after its substep"
    );
    let dt = f64::from(DT);
    let origin = lattice_origin(H);
    let mut worst = [0.0f64; 2];
    for p in &born {
        let lattice_local = seeds[(p.id - base) as usize];
        let seed: [f32; 3] = std::array::from_fn(|a| lattice_local[a] + origin[a]);
        let q: [f64; 3] = seed.map(|c| f64::from((c - SOLVER_MIN) / H));
        let v1 = cpu_sample_on(q, &field, SOLVER);
        let v2 = cpu_sample_on(
            std::array::from_fn(|a| q[a] + 0.5 * dt / f64::from(H) * v1[a]),
            &field,
            SOLVER,
        );
        let v3 = cpu_sample_on(
            std::array::from_fn(|a| q[a] + 0.75 * dt / f64::from(H) * v2[a]),
            &field,
            SOLVER,
        );
        for a in 0..3 {
            let x = f64::from(seed[a])
                + dt * (2.0 / 9.0 * v1[a] + 3.0 / 9.0 * v2[a] + 4.0 / 9.0 * v3[a]);
            worst[0] = worst[0].max((f64::from(p.position_radius[a]) - x).abs());
            worst[1] = worst[1].max((f64::from(p.velocity[a]) - v1[a]).abs());
            close(p.position_radius[a], x, 1e-5, "birth RK3 move");
            close(
                p.velocity[a],
                v1[a],
                1e-4 * (1.0 + v1[a].abs()),
                "birth velocity",
            );
        }
        close(p.position_radius[3], f64::from(RADIUS), 0.0, "birth radius");
    }
    eprintln!(
        "SHEETING STEP: {written} births, worst move {:e} m, worst velocity {:e} m/s",
        worst[0], worst[1]
    );

    let born_ids: Vec<u32> = born.iter().map(|p| p.id).collect();
    for (tick, label) in [(1, "the following two-substep tick"), (2, "the tick after")] {
        let next = splash.tick(tick, 2, 0.0);
        let ids: std::collections::HashSet<u32> = alive(&next.particles).map(|p| p.id).collect();
        let lost = born_ids.iter().filter(|id| !ids.contains(id)).count();
        assert_eq!(lost, 0, "{label}: {lost} births lost");
        assert_eq!(
            next.identity[0],
            base + written,
            "{label}: rate 0 seeds nothing"
        );
    }
}

/// Births share the pool and the identity counter: a pool too short for the
/// whole stage refuses all of it and counts it; inflow in the same substep reserves after the
/// births; an outflow removes what ends inside it; a rollover renumbers the
/// live markers and births follow them.
#[test]
fn gpu_flip_sheeting_births_share_the_pool() {
    let requested = Splash::new(8000, 4001, &[]).tick(0, 1, 1.0).stats[0];

    // Forty slots of room for a larger stage: no births, all counted.
    let mut short = Splash::new(40, 4001, &[]);
    let tick = short.tick(0, 1, 1.0);
    assert_eq!(tick.stats[..2], [requested, 0], "requested and written");
    assert_eq!(tick.identity[0], 4001, "no ids are spent");
    assert_eq!(
        alive(&tick.particles).count(),
        short.markers,
        "the refused stage writes nothing"
    );
    assert_unique_ids(&tick.particles, "short pool");

    // Inflow clear of the water, outflow through the shell's top.
    let inflow = world([1.0, 1.0, 1.0]);
    let outflow = world([4.5, 6.2, 4.5]);
    let mut shared = Splash::new(8000, 4001, &[region(2.0, inflow), region(3.0, outflow)]);
    let tick = shared.tick(0, 1, 1.0);
    let written = tick.stats[1];
    assert!(written > 100);
    let sheet = 4001..4001 + written;
    let emitted: Vec<&FluidParticle> = alive(&tick.particles)
        .filter(|p| p.id >= sheet.end)
        .collect();
    assert!(!emitted.is_empty(), "the inflow emits beside the births");
    assert!(
        emitted.iter().all(|p| inside(p, inflow)),
        "ids after the births are the inflow's"
    );
    assert!(alive(&tick.particles).any(|p| sheet.contains(&p.id)));
    assert!(
        !alive(&tick.particles).any(|p| inside(p, outflow)),
        "the outflow holds nothing"
    );
    assert!(
        tick.particles
            .iter()
            .any(|p| p.position_radius[3] > 0.0 && p.id <= 4000)
            && alive(&tick.particles).count() < shared.markers + written as usize + emitted.len(),
        "the outflow removed markers or births"
    );
    assert_eq!(tick.identity[0], sheet.end + emitted.len() as u32);
    assert_unique_ids(&tick.particles, "inflow and outflow");

    // Next id near the end of u32: the reservation renumbers the live
    // markers 1..=live and the births follow.
    let mut rolled = Splash::new(8000, u32::MAX - 5, &[]);
    let tick = rolled.tick(0, 1, 1.0);
    let live = rolled.markers as u32;
    assert_eq!(tick.identity[1], 2, "the epoch moved");
    assert_eq!(tick.identity[2], live + 1);
    assert_eq!(tick.identity[0], live + 1 + tick.stats[1]);
    let mut ids: Vec<u32> = alive(&tick.particles).map(|p| p.id).collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        (1..=live + tick.stats[1]).collect::<Vec<_>>(),
        "markers then births, no gap"
    );
}

/// The in-step passes on their own: an inactive clock slot writes nothing,
/// to the particles, the identity, the stats or the stage.
#[test]
fn gpu_flip_sheeting_step_passes_inactive_slot_write_nothing() {
    step_passes_with_room(4000);
}

/// A pool with room for 40 births and a larger stage: the reservation grants
/// none, so the write stops at the live slots, the whole stage refused.
#[test]
fn gpu_flip_sheeting_write_stops_at_the_granted_slots() {
    step_passes_with_room(40);
}

/// One active slot, then an inactive one, over a pool with `room` free slots
/// past the splash's markers.
fn step_passes_with_room(room: usize) {
    let device = manifold_gpu::testkit::test_device();
    let (markers, _, cells, dx) = sheeter::fixtures::splash();
    let h = dx as f32;
    let live = markers.len();
    let capacity = live + room;
    // The splash inside a margin of air, on whole tiles across the origin.
    let lo = [-24; 3];
    let big = padded_cells(cells, 8);
    let (world_markers, local_markers) = to_world(&padded_markers(&markers, 8, h), lo, h);
    let world = World::new(&device, lo, big.map(|n| n as i32), h);
    let shared = |bytes: &[u8], size: usize| {
        let buffer = device.create_buffer(size.max(16) as u64, BufferUse::Upload);
        buffer.zero_fill();
        // SAFETY: a fresh shared buffer at least `bytes` long; no GPU work.
        unsafe { buffer.write(0, bytes) };
        buffer
    };
    let sorted = sort(&device, &world, &world_markers, h, capacity);
    let (particles, order, ranges) = (&sorted.particles, &sorted.order, &sorted.ranges);
    let pooled_phi = |phi: &[f32]| {
        let pooled = world.to_pool(phi, 3.0 * h);
        shared(bytemuck::cast_slice(&pooled), 4 * pooled.len())
    };
    let level_set = marker_phi(&local_markers, big, h);
    let phi = pooled_phi(&level_set);
    let face = FaceSample {
        velocity: [0.3, -0.2, 0.1, 0.0],
        weight: [1.0; 4],
    };
    let faces = vec![face; world.cells().len()];
    let pooled_faces = world.to_pool(&faces, face);
    let old = shared(bytemuck::cast_slice(&pooled_faces), 32 * pooled_faces.len());
    let identity = shared(bytemuck::cast_slice(&identity_words(5000, live)), 32);
    let capped = shared(&[], 4 * SOLVER_WORDS as usize);
    let mut stage = GpuSheeting::default();
    stage.prepare(&device);
    stage
        .reserve(&device, capacity as u32, world.pool_tiles())
        .unwrap();
    let mut ids = ParticleIdentity::default();
    ids.prepare(&device);
    let run = |stage: &GpuSheeting, plan: &GpuBuffer, phi: &GpuBuffer| {
        let inputs = SheetInputs {
            particles,
            order,
            ranges,
            count: (&identity, COUNT_OFFSET),
            phi,
            h,
            threshold: FILL_THRESHOLD,
        };
        let births = StepBirths {
            old: &old,
            identity: &identity,
            capped: &capped,
            capped_offset: 0,
            rate: 1.0,
            tick: 0,
            substep: 0,
        };
        let mut enc = device.create_encoder("sheeting step passes");
        stage.encode_draw(&mut enc, &inputs, plan, &births, &world.tiles);
        ids.reserve(
            &mut enc,
            BirthReservation {
                particles,
                identity: &identity,
                ranges,
                scan: stage.winners(),
                plan,
                tile_counts: world.tiles.counts(),
                step_record: None,
                params: [
                    capacity as u32,
                    pool_len(world.pool_tiles()) as u32,
                    stage.ranks(),
                    exhausted_word(world.tiles.reach().ring_max) as u32,
                ],
            },
        );
        enc.compute_memory_barrier_buffers();
        stage.encode_write(&mut enc, &inputs, plan, &births, &world.tiles);
        enc.commit_and_wait().expect("GPU work completes");
    };
    let plan = |step_dt: f32| {
        let mut words = [0u32; 12];
        words[0] = step_dt.to_bits();
        words[11] = 1;
        shared(bytemuck::cast_slice(&words), 48)
    };
    run(&stage, &plan(0.01), &phi);
    let stats: Vec<u32> = read(stage.stats(), 4);
    assert!(stats[0] > 100, "the active slot seeds: {stats:?}");
    // All or nothing: the whole stage when it fits, none when it does not.
    let granted = if stats[0] <= room as u32 { stats[0] } else { 0 };
    assert_eq!(stats[1], granted, "the whole stage or none");
    let written = live + granted as usize;
    // The record counts the live slots and the births granted.
    let record = copy_out(&device, &identity);
    assert_eq!(
        record[4..],
        identity_words(0, written)[4..],
        "the identity's record after the reservation"
    );
    let slots = copy_out(&device, particles);
    let radius = |slot: usize| f32::from_bits(slots[8 * slot + 3]);
    assert!(
        (live..written).all(|slot| radius(slot) > 0.0),
        "every grant is a particle"
    );
    assert!(
        (written..capacity).all(|slot| radius(slot) == 0.0),
        "nothing is written past the grant"
    );
    let tally = copy_out(&device, &capped);
    assert_eq!(
        tally[SHEET_REFUSED_TAIL as usize],
        stats[0] - granted,
        "the refused births"
    );
    let snapshot = |stage: &GpuSheeting| -> Vec<Vec<u32>> {
        let mut words: Vec<Vec<u32>> = stage
            .scratch()
            .iter()
            .map(|b| copy_out(&device, b))
            .collect();
        words.push(copy_out(&device, stage.winners()));
        words.push(copy_out(&device, particles));
        words.push(copy_out(&device, &identity));
        words.push(copy_out(&device, &capped));
        words
    };
    let before = snapshot(&stage);
    let other: Vec<f32> = level_set.iter().map(|v| -v).collect();
    run(&stage, &plan(0.0), &pooled_phi(&other));
    assert!(before == snapshot(&stage), "an inactive slot wrote");
}

/// Rate 0 is off, and any rate outside 0 to 1 is a named refusal.
#[test]
fn gpu_flip_sheeting_rate_is_validated() {
    use super::gpu_flip_step::read_sheet_fill_rate;
    assert_eq!(read_sheet_fill_rate(0.0), Ok(0.0));
    assert_eq!(read_sheet_fill_rate(0.5), Ok(0.5));
    assert_eq!(read_sheet_fill_rate(1.0), Ok(1.0));
    for bad in [-0.1, 1.5, f32::NAN, f32::INFINITY] {
        assert!(read_sheet_fill_rate(bad).is_err(), "{bad}");
    }
}

/// A full pool reports the sheet births it had no slot for: the step's capped
/// tail word is the tick's requested minus written, restarted every tick, and
/// the published word index it lands on is the stats' `sheet_refused`. Once
/// the pool cannot cover the last request the gate rests the stage, which
/// draws nothing and refuses that whole request every tick it stays full.
#[test]
fn a_full_pool_reports_refused_sheet_births() {
    let mut room = Splash::new(8000, 4001, &[]);
    let tick = room.tick(0, 1, 1.0);
    assert!(tick.stats[1] > 100, "the splash seeds sheets");
    assert_eq!(room.refused_births(), 0, "a pool with room refuses none");

    let mut full = Splash::new(40, 4001, &[]);
    let first = full.tick(0, 1, 1.0);
    let [requested, written, ..] = first.stats;
    assert!(requested > written, "the pool is out of room");
    assert_eq!(
        full.refused_births(),
        requested - written,
        "the tail word is this tick's refusals"
    );
    for t in 1..3 {
        let rested = full.tick(t, 1, 1.0);
        assert_eq!(
            rested.stats, first.stats,
            "tick {t}: a resting stage draws nothing"
        );
        assert_eq!(
            full.refused_births(),
            requested,
            "tick {t}: a full pool refuses the last request"
        );
    }
    let mut words = [0u32; LIQUID_STATS_WORDS as usize];
    words[(SOLVER_STATS_START + SHEET_REFUSED_TAIL) as usize] = full.refused_births();
    assert_eq!(SOLVER_STATS_START + SHEET_REFUSED_TAIL, SHEET_REFUSED_WORD);
    assert_eq!(
        LiquidTickStats::from_words(&words).sheet_refused,
        full.refused_births()
    );
}
