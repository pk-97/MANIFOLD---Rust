//! Sheet seeding inside node.gpu_flip_step (BUG-j9l9w): births join the
//! substep they are seeded in, survive what follows, share the pool and the
//! identity counter with inflow and removal, and an inactive clock slot
//! writes nothing. The splash fixture (`manifold_fluids::sheeter::fixtures`)
//! is the particles; the step's own surface distance is the level set.

use super::gpu_flip_sheeting::{FILL_THRESHOLD, GpuSheeting, SheetInputs, StepBirths};
use super::gpu_flip_step::GpuFlipStep;
use super::gpu_flip_step_tests::cpu_sample_on;
use crate::testkit::liquid_surface::{Harness, params, read};
use super::particle_identity::{BirthReservation, ParticleIdentity};
use super::sort_particles_into_cells::{LIQUID_PARTICLE_READ, ParticleSorter, SortJob, SortLabels};
use super::prefix_scan::ScanLabels;
use super::whitewater_engine_gpu_tests::marker_phi;
use crate::water::fluid_particles::{FaceSample, FluidParticle};
use crate::water::liquid::bodies::{LiquidBody, LiquidShape, pack_distance_atlas};
use crate::water::liquid::lattice::PADDING_NODES;
use crate::primitive::Primitive;
use manifold_fluids::sheeter;
use manifold_gpu::{GpuBuffer, GpuDevice};

const H: f32 = 0.25;
/// The splash's grid is the step's solver grid: 37 box cells and the step's
/// three, its minimum at the origin.
const SOLVER: [usize; 3] = [40; 3];
const DT: f32 = 1.0 / 120.0;
const RADIUS: f32 = 0.31017524 * H;
const V0: [f32; 3] = [0.4, 0.1, -0.3];

fn copy_out(device: &GpuDevice, buffer: &GpuBuffer) -> Vec<u32> {
    let staged = device.create_buffer_shared(buffer.size);
    let mut enc = device.create_encoder("sheeting step proof readback");
    enc.copy_buffer_to_buffer(buffer, &staged, buffer.size);
    enc.commit_and_wait_completed();
    read(&staged, (buffer.size / 4) as usize)
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

struct Splash {
    h: Harness,
    node: GpuFlipStep,
    capacity: usize,
    particles: (crate::bindings::Slot, GpuBuffer),
    identity: (crate::bindings::Slot, GpuBuffer),
    faces: crate::bindings::Slot,
    capped: crate::bindings::Slot,
    regions: Option<[crate::bindings::Slot; 4]>,
    region_count: usize,
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
            .map(|(i, m)| FluidParticle { position_radius: [m[0], m[1], m[2], RADIUS], velocity: V0, id: i as u32 + 1 })
            .collect();
        let mut h = Harness::new();
        let mut node = GpuFlipStep::new();
        node.prepare_pipelines(&h.device);
        let particles = h.array(&records, capacity);
        let identity = h.array::<u32>(&[next_id, 1, 0, 0], 4);
        let faces = h.array::<FaceSample>(&[], SOLVER.map(|n| n + 1).iter().product()).0;
        let capped = h.array::<u32>(&[], 2 * capacity + super::liquid_stats::SOLVER_WORDS as usize).0;
        let region_count = regions.len();
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
            };
            [
                h.array(regions, regions.len()).0,
                h.array(&[shape], 1).0,
                h.array(&atlas, atlas.len()).0,
                h.array::<LiquidBody>(&[], 1).0,
            ]
        });
        Self { h, node, capacity, particles, identity, faces, capped, regions: region_slots, region_count, markers: markers.len() }
    }

    fn tick(&mut self, tick: u32, steps: u32, rate: f32) -> Tick {
        let pad = PADDING_NODES as f32;
        let n = SOLVER[0] as f32 - 3.0;
        let min = 1.5 * H;
        let mut values = vec![
            ("nodes_x", n + 1.0 + 2.0 * pad),
            ("nodes_y", n + 1.0 + 2.0 * pad),
            ("nodes_z", n + 1.0 + 2.0 * pad),
            ("lattice_min_x", min - pad * H),
            ("lattice_min_y", min - pad * H),
            ("lattice_min_z", min - pad * H),
            ("cell_size", H),
            ("gravity_y", -9.81),
            ("interval_duration", DT * steps as f32),
            ("steps", steps as f32),
            ("flip", 0.95),
            ("iterations", 0.0),
            ("volume_projection", 0.0),
            ("sheet_fill_rate", rate),
            ("tick_index", tick as f32),
        ];
        let mut inputs = vec![("particles", self.particles.0), ("identity", self.identity.0)];
        if let Some([regions, shapes, atlas, bodies]) = self.regions {
            values.extend([("region_count", self.region_count as f32), ("first_tick", tick as f32)]);
            inputs.extend([("regions", regions), ("shapes", shapes), ("atlas", atlas), ("bodies", bodies)]);
        }
        let p = params(&values);
        let outputs = [("out", self.particles.0), ("faces", self.faces), ("capped", self.capped)];
        let (_, errors) = self.h.run(&mut self.node, &inputs, &outputs, &p);
        assert!(errors.is_empty(), "{errors:?}");
        let stats = if rate > 0.0 || self.node_has_sheeting() {
            let words: Vec<u32> = read(self.node.sheeting().stats(), 4);
            [words[0], words[1], words[2], words[3]]
        } else {
            [0; 4]
        };
        let identity: Vec<u32> = read(&self.identity.1, 4);
        Tick { particles: read(&self.particles.1, self.capacity), identity: [identity[0], identity[1], identity[2], identity[3]], stats }
    }

    fn node_has_sheeting(&self) -> bool {
        self.node.sheeting().reserved()
    }

    fn faces(&self) -> Vec<FaceSample> {
        read(&self.h.buffer(self.faces), SOLVER.map(|n| n + 1).iter().product())
    }

    /// Entry i: the seed written at birth index i, grid-local.
    fn births(&self, written: u32) -> Vec<[f32; 3]> {
        let words = copy_out(&self.h.device, self.node.sheeting().births().0);
        (0..written as usize).map(|i| [0, 1, 2].map(|a| f32::from_bits(words[4 * i + a]))).collect()
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
    assert!((f64::from(got) - want).abs() <= tolerance, "{label}: {got} vs {want}");
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
    assert_eq!((requested, written), (requested_total, written_total), "one active substep");
    assert_eq!(requested, written, "the pool has room");
    let base = 4001;
    assert_eq!(first.identity[2], base);
    assert_eq!(first.identity[0], base + written);
    assert_unique_ids(&first.particles, "first tick");
    let field = splash.faces();
    let seeds = splash.births(written);
    let born: Vec<&FluidParticle> = alive(&first.particles).filter(|p| p.id >= base).collect();
    assert_eq!(born.len(), written as usize, "every birth is alive after its substep");
    let dt = f64::from(DT);
    let mut worst = [0.0f64; 2];
    for p in &born {
        let seed = seeds[(p.id - base) as usize];
        let q: [f64; 3] = seed.map(|c| f64::from(c / H));
        let v1 = cpu_sample_on(q, &field, SOLVER);
        let v2 = cpu_sample_on(std::array::from_fn(|a| q[a] + 0.5 * dt / f64::from(H) * v1[a]), &field, SOLVER);
        let v3 = cpu_sample_on(std::array::from_fn(|a| q[a] + 0.75 * dt / f64::from(H) * v2[a]), &field, SOLVER);
        for a in 0..3 {
            let x = f64::from(seed[a]) + dt * (2.0 / 9.0 * v1[a] + 3.0 / 9.0 * v2[a] + 4.0 / 9.0 * v3[a]);
            worst[0] = worst[0].max((f64::from(p.position_radius[a]) - x).abs());
            worst[1] = worst[1].max((f64::from(p.velocity[a]) - v1[a]).abs());
            close(p.position_radius[a], x, 1e-5, "birth RK3 move");
            close(p.velocity[a], v1[a], 1e-4 * (1.0 + v1[a].abs()), "birth velocity");
        }
        close(p.position_radius[3], f64::from(RADIUS), 0.0, "birth radius");
    }
    eprintln!("SHEETING STEP: {written} births, worst move {:e} m, worst velocity {:e} m/s", worst[0], worst[1]);

    let born_ids: Vec<u32> = born.iter().map(|p| p.id).collect();
    for (tick, label) in [(1, "the following two-substep tick"), (2, "the tick after")] {
        let next = splash.tick(tick, 2, 0.0);
        let ids: std::collections::HashSet<u32> = alive(&next.particles).map(|p| p.id).collect();
        let lost = born_ids.iter().filter(|id| !ids.contains(id)).count();
        assert_eq!(lost, 0, "{label}: {lost} births lost");
        assert_eq!(next.identity[0], base + written, "{label}: rate 0 seeds nothing");
    }
}

/// Births share the pool and the identity counter: a short pool drops the
/// excess and counts it; inflow in the same substep reserves after the
/// births; an outflow removes what ends inside it; a rollover renumbers the
/// live markers and births follow them.
#[test]
fn gpu_flip_sheeting_births_share_the_pool() {
    let requested = Splash::new(8000, 4001, &[]).tick(0, 1, 1.0).stats[0];

    // Forty slots of room: forty births, the rest dropped and counted.
    let mut short = Splash::new(40, 4001, &[]);
    let tick = short.tick(0, 1, 1.0);
    assert_eq!(tick.stats[..2], [requested, 40], "requested and written");
    assert_eq!(tick.identity[0], 4001 + 40);
    assert_eq!(alive(&tick.particles).count(), short.markers + 40, "the pool is full");
    assert_unique_ids(&tick.particles, "short pool");

    // Inflow clear of the water, outflow through the shell's top.
    let inflow = [1.0, 1.0, 1.0];
    let outflow = [4.5, 6.2, 4.5];
    let mut shared = Splash::new(8000, 4001, &[region(2.0, inflow), region(3.0, outflow)]);
    let tick = shared.tick(0, 1, 1.0);
    let written = tick.stats[1];
    assert!(written > 100);
    let sheet = 4001..4001 + written;
    let emitted: Vec<&FluidParticle> = alive(&tick.particles).filter(|p| p.id >= sheet.end).collect();
    assert!(!emitted.is_empty(), "the inflow emits beside the births");
    assert!(emitted.iter().all(|p| inside(p, inflow)), "ids after the births are the inflow's");
    assert!(alive(&tick.particles).any(|p| sheet.contains(&p.id)));
    assert!(!alive(&tick.particles).any(|p| inside(p, outflow)), "the outflow holds nothing");
    assert!(
        tick.particles.iter().any(|p| p.position_radius[3] > 0.0 && p.id <= 4000)
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
    assert_eq!(ids, (1..=live + tick.stats[1]).collect::<Vec<_>>(), "markers then births, no gap");
}

/// The in-step passes on their own: an inactive clock slot writes nothing,
/// to the particles, the identity, the stats or the stage.
#[test]
fn gpu_flip_sheeting_step_passes_inactive_slot_write_nothing() {
    let device = manifold_gpu::testkit::test_device();
    let (markers, _, cells, dx) = sheeter::fixtures::splash();
    let h = dx as f32;
    let capacity = markers.len() + 4000;
    let records: Vec<FluidParticle> = markers
        .iter()
        .enumerate()
        .map(|(i, m)| FluidParticle { position_radius: [m[0], m[1], m[2], RADIUS], velocity: V0, id: i as u32 + 1 })
        .collect();
    let shared = |bytes: &[u8], size: usize| {
        let buffer = device.create_buffer_shared(size.max(16) as u64);
        buffer.zero_fill();
        // SAFETY: a fresh shared buffer at least `bytes` long; no GPU work.
        unsafe { buffer.write(0, bytes) };
        buffer
    };
    let source = shared(bytemuck::cast_slice(&records), 32 * capacity);
    let particles = device.create_buffer_shared(32 * capacity as u64);
    let order = device.create_buffer_shared(4 * capacity as u64);
    let mut sorter = ParticleSorter::default();
    sorter.prepare(&device);
    sorter.reserve_ranges(&device, cells).unwrap();
    let labels = SortLabels {
        clear: "clear",
        count: "count",
        scan: ScanLabels { blocks: "blocks", add: "add" },
        ranges: "ranges",
        tail: "tail",
        scatter: "scatter",
        stabilise: "stabilise",
    };
    let mut enc = device.create_encoder("sheeting step sort");
    sorter
        .encode(&device, &mut enc, &SortJob {
            particles: &source, read: LIQUID_PARTICLE_READ, capacity: capacity as u32, count: capacity as u32,
            bin_min: [0.0; 3], inv_cell: 1.0 / h, bins: cells, sorted: Some(&particles), order: Some(&order), gate: None,
        }, &labels)
        .unwrap();
    enc.commit_and_wait_completed();
    let phi = shared(bytemuck::cast_slice(&marker_phi(&markers, cells, h)), 4 * 64000);
    let faces = vec![FaceSample { velocity: [0.3, -0.2, 0.1, 0.0], weight: [1.0; 4] }; 41 * 41 * 41];
    let old = shared(bytemuck::cast_slice(&faces), 32 * faces.len());
    let identity = shared(bytemuck::cast_slice(&[5000u32, 1, 0, 0]), 16);
    let mut stage = GpuSheeting::default();
    stage.prepare(&device);
    stage.reserve(&device, cells, capacity as u32).unwrap();
    let mut ids = ParticleIdentity::default();
    ids.prepare(&device);
    let ranges = sorter.ranges().unwrap();
    let run = |stage: &GpuSheeting, plan: &GpuBuffer, phi: &GpuBuffer| {
        let inputs = SheetInputs { particles: &particles, order: &order, ranges, count: capacity as u32, phi, origin: [0.0; 3], h, threshold: FILL_THRESHOLD };
        let births = StepBirths { old: &old, identity: &identity, slots: capacity as u32, rate: 1.0, tick: 0, substep: 0 };
        let mut enc = device.create_encoder("sheeting step passes");
        stage.encode_draw(&mut enc, &inputs, plan, &births);
        ids.reserve(&mut enc, BirthReservation {
            particles: &particles, identity: &identity, ranges, scan: stage.winners(), plan,
            params: [capacity as u32, 64000, stage.ranks(), 1],
        });
        enc.compute_memory_barrier_buffers();
        stage.encode_write(&mut enc, &inputs, plan, &births);
        enc.commit_and_wait_completed();
    };
    let plan = |step_dt: f32| {
        let mut words = [0u32; 12];
        words[0] = step_dt.to_bits();
        words[11] = 1;
        shared(bytemuck::cast_slice(&words), 48)
    };
    run(&stage, &plan(0.01), &phi);
    let stats: Vec<u32> = read(stage.stats(), 4);
    assert!(stats[1] > 100, "the active slot seeds: {stats:?}");
    let snapshot = |stage: &GpuSheeting| -> Vec<Vec<u32>> {
        let mut words: Vec<Vec<u32>> = stage.scratch().iter().map(|b| copy_out(&device, b)).collect();
        words.push(copy_out(&device, stage.winners()));
        words.push(copy_out(&device, &particles));
        words.push(copy_out(&device, &identity));
        words
    };
    let before = snapshot(&stage);
    let other: Vec<f32> = marker_phi(&markers, cells, h).iter().map(|v| -v).collect();
    run(&stage, &plan(0.0), &shared(bytemuck::cast_slice(&other), 4 * 64000));
    assert!(before == snapshot(&stage), "an inactive slot wrote");
}

/// Rate 0 is off, and any rate with Narrow Band, or outside 0 to 1, is a
/// named refusal.
#[test]
fn gpu_flip_sheeting_rate_is_validated() {
    use super::gpu_flip_step::read_sheet_fill_rate;
    assert_eq!(read_sheet_fill_rate(0.0, true), Ok(0.0));
    assert_eq!(read_sheet_fill_rate(0.5, false), Ok(0.5));
    for bad in [-0.1, 1.5, f32::NAN, f32::INFINITY] {
        assert!(read_sheet_fill_rate(bad, false).is_err(), "{bad}");
    }
    assert!(read_sheet_fill_rate(1.0, true).is_err());
}
