//! Node-boundary proof for `node.whitewater_step`
//! (`docs/GPU_WHITEWATER_DESIGN.md` section 3.9): frame after frame, what the
//! node publishes is the CPU statements of its passes composed in the node's
//! order — grid fields, emitter, append, then per tick advect, retype, age,
//! remove and compact, then the split into foam, bubbles and spray. The
//! sequence covers the first frame (nothing published yet), ticks 0 (the
//! pool and outputs held) and a new epoch (the pool started over).
//!
//! The fixture is tie-free: no decision in the CPU run sits within [`TOL`]
//! of its threshold, checked by its own CPU test, so the GPU must match the
//! reference structurally — the same particles, in the same order, with the
//! same counts.

use crate::fluid_particles::WhitewaterSpawn;

use super::emission_count::WAVECREST_RATE;
use super::energy_potential::{MAX_ENERGY, MIN_ENERGY};
use super::keep_whitewater::MAX_PER_CELL;
use super::spawn_whitewater::{LIFETIME_VARIANCE, MAX_LIFETIME, MIN_LIFETIME};
use super::wavecrest_potential::{MAX_CURVATURE, MIN_CURVATURE, SHARPNESS};
use {crate::primitives::whitewater_cpu as grid_cpu, super::whitewater_cpu::Grid, super::whitewater_cpu::Rng};
use {crate::primitives::whitewater_particle_cpu as particle_cpu, super::whitewater_particle_cpu::Box3, super::whitewater_particle_cpu::Crest, super::whitewater_particle_cpu::Emission, super::whitewater_particle_cpu::Spawn, super::whitewater_particle_cpu::SpawnFields};
use {crate::primitives::whitewater_pool_cpu as pool_cpu, super::whitewater_pool_cpu::Advect, super::whitewater_pool_cpu::Age, super::whitewater_pool_cpu::PoolState, super::whitewater_pool_cpu::Preserve, super::whitewater_step::empty_slot};
use super::whitewater_step::{Report, StepShape};
use crate::clock::{TICK, whitewater_fade};
use manifold_node_engine::particles::FluidParticle;
use crate::liquid::grid::face_len;
use manifold_node_engine::scene::transform::Transform;
use crate::whitewater::{KnownValue, SPREAD_STEPS, WhitewaterParticle};

/// Unequal sides, so a swapped axis shows.
const NODES: [u32; 3] = [21, 23, 17];
const PAD: u32 = 2;
const H: f32 = 0.1;
const ORIGIN: [f32; 3] = [-1.0, 0.2, -0.8];
/// A liquid ball, in cells: curved enough to crest (2 / radius above the
/// minimum curvature).
const BALL_CENTRE: [f32; 3] = [9.3, 7.1, 7.7];
const BALL_RADIUS: f32 = 3.7;
/// Fast enough to emit, mostly upward so the ball's top crests.
const FLOW: [f32; 3] = [0.9, 5.9, -0.7];
const SLOTS: usize = 2000;
/// The emitters: the slots past it never emit.
const LIVE: u32 = SLOTS as u32 - 100;
const CAPACITY: u32 = 300;
const SEED: f32 = 0.37;
const GRAVITY: [f32; 3] = [0.0, -9.81, 0.0];
/// (ticks, epoch) per frame: emit and step, step three times, hold, hold
/// (with a pool written but unpublished), step, a new epoch, step twice, hold.
const FRAMES: [(u32, u32); 8] = [(1, 0), (3, 0), (0, 0), (0, 0), (1, 0), (1, 1), (2, 1), (0, 1)];
/// The closest a CPU decision may sit to its threshold: cells for anything
/// placed on the grid, else the quantity's own units.
const TOL: f32 = 1e-5;

fn cells() -> [u32; 3] {
    NODES.map(|n| n - 1)
}

fn face_cells() -> [u32; 3] {
    cells().map(|c| c - 2 * PAD)
}

fn bounds() -> Transform {
    let size: [f32; 3] = cells().map(|c| c as f32 * H);
    Transform { pos: std::array::from_fn(|a| ORIGIN[a] + 0.5 * size[a]), scale: size, ..Transform::default() }
}

fn shape() -> StepShape {
    StepShape::new(NODES, NODES, face_cells(), 1.0, Some(bounds()), CAPACITY).expect("fixture shape")
}

/// FLIP's _stepFluid calls _updateDiffuseMaterial(dt) on each tick, which
/// emits before advancing the pool. Grouping ticks into display frames must
/// therefore leave every pool row and counter unchanged.
#[test]
fn whitewater_per_tick_cpu_rows_match_at_every_frame_rate() {
    fn run(fps: u32, emit_per_frame: bool) -> Vec<(u32, u32, Vec<u8>)> {
        let grid = Box3 { cells: [8; 3], center: [4.0; 3], size: [8.0; 3] };
        let solid = vec![10.0; 9 * 9 * 9];
        let faces: [Vec<f32>; 3] = std::array::from_fn(|a| vec![0.2 * a as f32; face_len([8; 3], a) as usize]);
        let fields = pool_cpu::Fields { faces: faces.each_ref().map(Vec::as_slice), face_cells: [8; 3], solid: &solid };
        let mut pool = vec![empty_slot(); 64];
        let mut state = PoolState::default();
        let mut tick = 0;
        let mut rows = Vec::new();
        for frame in 1..=fps / 5 {
            let due = frame * 60 / fps;
            let ticks = due - tick;
            for iteration in 0..ticks {
                // Three hand-built emitters, one of each FLIP type. A changing
                // position exposes the old final-tick field reuse at 30 fps.
                let emission_tick = if emit_per_frame { due - 1 } else { tick };
                let batch = if emit_per_frame { if iteration == 0 { ticks } else { 0 } } else { 1 };
                let spawns: Vec<_> = (0..3 * batch).map(|i| WhitewaterSpawn {
                    position_lifetime: [3.0 + emission_tick as f32 * 0.01, 4.0, 4.0, 2.0],
                    velocity: [0.2, 0.1, 0.0], kind: i % 3,
                }).collect();
                pool_cpu::append(&mut pool, &spawns, spawns.len() as u32, &mut state);
                for p in &mut pool {
                    *p = pool_cpu::age(pool_cpu::advect(*p, &fields, &grid, Advect::flip(), None).0, Age::flip());
                }
                tick += 1;
            }
            if frame * 30 % fps == 0 {
                rows.push((state.emitted, state.next_id, bytemuck::cast_slice(&pool).to_vec()));
            }
        }
        assert_eq!(state.emitted, 36, "proof must exercise emission");
        rows
    }
    let at_60 = run(60, false);
    assert_eq!(run(30, false), at_60);
    assert_eq!(run(120, false), at_60);
    assert_ne!(run(30, true), at_60, "the old frame-batched emission must fail");
}

/// The accepted duration reaches every whitewater atom: emission count and
/// spawn travel use the full interval, lifecycle age/preserve scale by it,
/// and spray drag applies the same duration in its velocity update.
#[test]
fn live_interval_whitewater_duration() {
    let dt = 0.1f32;
    let particle = FluidParticle { position_radius: [4.0, 4.0, 4.0, 0.05], velocity: [1.0, 0.0, 0.0], id: 1 };
    let (emitted, margin) = particle_cpu::emission_count(particle, 1.0, 1.0, 0,
        Emission { dt, rate: 80.0, points_per_cell: 8.0, ticks: 1.0, live_count: 1.0 });
    assert!(margin > 1e-5);
    assert_eq!(emitted, 8, "80 particles/s over 0.1 seconds");

    let grid = Box3 { cells: [8; 3], center: [4.0; 3], size: [8.0; 3] };
    let faces: [Vec<f32>; 3] = std::array::from_fn(|axis| vec![0.0; face_len([8; 3], axis) as usize]);
    let solid = vec![10.0; 9 * 9 * 9];
    let spawn_fields = SpawnFields { offsets: &[1], particles: std::slice::from_ref(&particle), energy: &[1.0], faces: faces.each_ref().map(Vec::as_slice), face_cells: [8; 3], solid: &solid };
    let short = particle_cpu::spawn(0, &spawn_fields, &grid, Spawn { dt: 0.05, capacity: 1, emitters: 1, seed: 3.5, epoch: 2.0, min_lifetime: 1.0, max_lifetime: 1.0, variance: 0.0 }).0;
    let long = particle_cpu::spawn(0, &spawn_fields, &grid, Spawn { dt, capacity: 1, emitters: 1, seed: 3.5, epoch: 2.0, min_lifetime: 1.0, max_lifetime: 1.0, variance: 0.0 }).0;
    assert!(short.position_lifetime[3] > 0.0 && long.position_lifetime[3] > 0.0);
    assert!((long.position_lifetime[0] - short.position_lifetime[0]).abs() > 1e-4, "spawn travel must use accepted duration");

    let foam = WhitewaterParticle { position_lifetime: [4.0, 4.0, 4.0, 2.0], kind: 1, ..WhitewaterParticle::default() };
    let aged = pool_cpu::age(foam, Age { dt, ..Age::flip() });
    assert!((aged.position_lifetime[3] - 1.9).abs() < 1e-6, "foam age scales with dt");
    let dense = vec![WhitewaterParticle { position_lifetime: [4.0, 4.0, 4.0, 2.0], kind: 1, ..WhitewaterParticle::default() }; 45];
    let preserved = pool_cpu::preserve(&dense, [0.0; 3], 1.0, [8; 3], Preserve { dt, ..Preserve::flip() });
    assert!((preserved[0].position_lifetime[3] - 2.075).abs() < 1e-6, "foam preservation scales with dt");

    let spray = WhitewaterParticle { position_lifetime: [4.0, 4.0, 4.0, 2.0], velocity: [1.0, 0.0, 0.0], kind: 2, ..WhitewaterParticle::default() };
    let fields = pool_cpu::Fields { faces: faces.each_ref().map(Vec::as_slice), face_cells: [8; 3], solid: &solid };
    let dragged = pool_cpu::advect(spray, &fields, &grid, Advect { dt, gravity: [0.0; 3], spray_drag: 2.0, spray_drag_variance: 0.0, ..Advect::flip() }, None).0;
    assert!((dragged.velocity[0] - 0.8).abs() < 1e-6, "spray drag uses stretched dt");
}

fn box3(shape: &StepShape) -> Box3 {
    Box3 { cells: shape.cells, center: shape.center, size: shape.size }
}

/// A tank closed on every side, its walls a cell thick: positive in the
/// open, metres.
fn tank() -> Vec<f32> {
    let c = cells();
    let mut solid = Vec::new();
    for k in 0..NODES[2] {
        for j in 0..NODES[1] {
            for i in 0..NODES[0] {
                let x = [i, j, k].map(|n| n as f32 * H);
                solid.push((0..3).map(|a| (x[a] - H).min(c[a] as f32 * H - H - x[a])).fold(f32::INFINITY, f32::min));
            }
        }
    }
    solid
}

/// The liquid's inputs.
struct Scene {
    particles: Vec<FluidParticle>,
    solid: Vec<f32>,
    faces: [Vec<f32>; 3],
    level: Vec<f32>,
}

impl Scene {
    fn new() -> Self {
        let n = NODES.map(|v| v as usize);
        let level = (0..n[0] * n[1] * n[2])
            .map(|i| {
                let p = [i % n[0], (i / n[0]) % n[1], i / (n[0] * n[1])].map(|v| v as f32);
                let r = (0..3).map(|a| (p[a] - BALL_CENTRE[a]).powi(2)).sum::<f32>().sqrt();
                (r - BALL_RADIUS) * H
            })
            .collect();
        let mut rng = Rng(0x57e9_0004);
        let particles = (0..SLOTS)
            .map(|_| {
                let d: [f32; 3] = std::array::from_fn(|_| 2.0 * rng.unit() - 1.0);
                let l = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-3);
                let r = BALL_RADIUS - 1.5 * rng.unit();
                let p: [f32; 3] = std::array::from_fn(|a| ORIGIN[a] + (BALL_CENTRE[a] + r * d[a] / l) * H);
                FluidParticle { position_radius: [p[0], p[1], p[2], 0.05], velocity: [0.0; 3], id: 0 }
            })
            .collect();
        Self {
            particles,
            solid: tank(),
            faces: std::array::from_fn(|axis| vec![FLOW[axis]; face_len(face_cells(), axis) as usize]),
            level,
        }
    }

    fn faces(&self) -> [&[f32]; 3] {
        self.faces.each_ref().map(Vec::as_slice)
    }
}

/// The grid fields the emitter and the tick read.
struct GridFields {
    distance: Vec<f32>,
    cells: Vec<u32>,
    curvature: Vec<KnownValue>,
}

/// The closest decision so far, and how many fell within [`TOL`].
#[derive(Default)]
struct Margins {
    closest: Option<(f32, &'static str)>,
    near: usize,
}

impl Margins {
    fn note(&mut self, what: &'static str, margin: f32) {
        if self.closest.is_none_or(|(m, _)| margin < m) {
            self.closest = Some((margin, what));
        }
        if margin < TOL {
            self.near += 1;
        }
    }
}

fn grid_fields(scene: &Scene, h: f32, margins: &mut Margins) -> GridFields {
    let grid = Grid::new(NODES);
    let mut crossings: Vec<_> =
        (0..grid.total()).map(|i| grid_cpu::surface_crossing(&grid, &scene.level, &scene.solid, 1, grid.coords(i)).0).collect();
    for step in SPREAD_STEPS {
        crossings = (0..grid.total()).map(|i| grid_cpu::nearest_crossing(&grid, &crossings, grid.coords(i), step)).collect();
    }
    let distance: Vec<f32> =
        (0..grid.total()).map(|i| grid_cpu::crossing_distance(&grid, crossings[i], &scene.solid, h, grid.coords(i))).collect();
    for &d in &distance {
        margins.note("distance sign", d.abs() / h);
        margins.note("curvature band", (d.abs() - 2.0 * h).abs() / h);
    }
    let cells = (0..grid.total()).map(|i| grid_cpu::liquid_cell(&grid, &distance, &scene.solid, grid.coords(i))).collect();
    let mut curvature: Vec<KnownValue> = (0..grid.total()).map(|i| grid_cpu::lattice_curvature(&grid, &distance, h, grid.coords(i))).collect();
    for _ in 0..3 {
        curvature = (0..grid.total()).map(|i| grid_cpu::extend_lattice(&grid, &curvature, grid.coords(i))).collect();
    }
    GridFields { distance, cells, curvature }
}

/// What one published frame holds.
#[derive(Clone, Debug, Default)]
struct Snapshot {
    report: Report,
    populations: [Vec<FluidParticle>; 3],
}

/// The node's passes on the CPU, in the node's order.
struct Model {
    scene: Scene,
    shape: StepShape,
    pool: Vec<WhitewaterParticle>,
    state: PoolState,
    margins: Margins,
    /// Removed by the tick, over the run.
    removed: u32,
    /// The node's Preserve Foam toggle.
    preserve_foam: bool,
    /// Foam slots the preservation gave lifetime to, over the run.
    preserved: u32,
    /// The most foam in one cell when the preservation ran.
    foam_density: u32,
}

impl Model {
    fn new(preserve_foam: bool) -> Self {
        let shape = shape();
        Self {
            scene: Scene::new(),
            shape,
            pool: vec![empty_slot(); CAPACITY as usize],
            state: PoolState::default(),
            margins: Margins::default(),
            removed: 0,
            preserve_foam,
            preserved: 0,
            foam_density: 0,
        }
    }

    /// `node.preserve_foam` at FLIP's settings over the sort's bins (one a
    /// cell of the whitewater grid), and each foam slot's distance to its
    /// bin's nearest face in cells — the GPU counts the bin the sort put it
    /// in, so a slot on a face would be a tie.
    fn preserve(
        shape: &StepShape,
        margins: &mut Margins,
        preserved: &mut u32,
        foam_density: &mut u32,
        pool: Vec<WhitewaterParticle>,
    ) -> Vec<WhitewaterParticle> {
        let s = shape;
        let h = s.cell_size;
        let origin: [f32; 3] = std::array::from_fn(|a| s.center[a] - 0.5 * s.size[a]);
        let mut density = std::collections::HashMap::<[i64; 3], u32>::new();
        for p in pool.iter().filter(|p| p.kind == 1) {
            let g: [f32; 3] = std::array::from_fn(|a| (p.position_lifetime[a] - origin[a]) / h);
            let margin = (0..3)
                .map(|a| {
                    let f = g[a] - g[a].floor();
                    f.min(1.0 - f)
                })
                .fold(f32::INFINITY, f32::min);
            if margin.is_finite() {
                margins.note("preserve", margin);
                let cell = density.entry(g.map(|x| x.floor() as i64)).or_insert(0);
                *cell += 1;
                *foam_density = (*foam_density).max(*cell);
            }
        }
        let settings = pool_cpu::Preserve { dt: TICK as f32, ..pool_cpu::Preserve::flip() };
        let out = pool_cpu::preserve(&pool, origin, h, s.bins, settings);
        *preserved += out.iter().zip(&pool).filter(|(o, p)| o.position_lifetime[3] > p.position_lifetime[3]).count() as u32;
        out
    }

    fn reseed(&mut self) {
        self.pool = vec![empty_slot(); CAPACITY as usize];
        self.state = PoolState::default();
    }

    fn frame(&mut self, ticks: u32, epoch: u32) {
        let s = self.shape;
        let grid = box3(&s);
        let h = s.cell_size;
        let fields = grid_fields(&self.scene, h, &mut self.margins);
        let faces = self.scene.faces();
        let epoch_f = epoch as f32;
        let sampled: Vec<FluidParticle> = self
            .scene
            .particles
            .iter()
            .enumerate()
            .map(|(i, &p)| particle_cpu::sample_faces(particle_cpu::jitter(p, i as u32, h, SEED, epoch_f), faces, s.face_cells, &grid))
            .collect();
        let energy: Vec<f32> = sampled.iter().map(|&p| particle_cpu::energy(p, MIN_ENERGY, MAX_ENERGY)).collect();
        let crest = Crest { min_curvature: MIN_CURVATURE, max_curvature: MAX_CURVATURE, sharpness: SHARPNESS };
        let emission = Emission { dt: 1.0 / 60.0, rate: WAVECREST_RATE, points_per_cell: 8.0, ticks: ticks as f32, live_count: LIVE as f32 };
        let mut offsets = Vec::with_capacity(LIVE as usize);
        let mut total = 0;
        for i in 0..LIVE as usize {
            let (wavecrest, crest_margin) = particle_cpu::wavecrest(sampled[i], &fields.distance, &fields.curvature, &fields.cells, &grid, crest);
            self.margins.note("wavecrest", crest_margin);
            let (n, edge) = particle_cpu::emission_count(sampled[i], energy[i], wavecrest, i as u32, emission);
            self.margins.note("emission", edge);
            total += n;
            offsets.push(total);
        }
        let spawn_fields =
            SpawnFields { offsets: &offsets, particles: &sampled, energy: &energy, faces, face_cells: s.face_cells, solid: &self.scene.solid };
        let settings = Spawn {
            dt: 1.0 / 60.0,
            capacity: CAPACITY,
            emitters: LIVE,
            seed: SEED,
            epoch: epoch_f,
            min_lifetime: MIN_LIFETIME,
            max_lifetime: MAX_LIFETIME,
            variance: LIFETIME_VARIANCE,
        };
        let spawns: Vec<WhitewaterSpawn> = (0..CAPACITY)
            .map(|j| {
                let (mut spawn, margin) = particle_cpu::spawn(j, &spawn_fields, &grid, settings);
                self.margins.note("spawn", margin);
                let (kind, margin) = particle_cpu::kind(spawn, &fields.distance, &fields.cells, &grid);
                self.margins.note("kind", margin);
                spawn.kind = kind;
                spawn
            })
            .collect();
        pool_cpu::append(&mut self.pool, &spawns, total, &mut self.state);
        let at = pool_cpu::Fields { faces, face_cells: s.face_cells, solid: &self.scene.solid };
        let advect = Advect { gravity: GRAVITY, dt: TICK as f32, ..Advect::flip() };
        let age = Age { dt: TICK as f32, ..Age::flip() };
        for _ in 0..ticks {
            let mut stepped: Vec<WhitewaterParticle> = self
                .pool
                .iter()
                .map(|&p| {
                    let (p, margin) = pool_cpu::advect(p, &at, &grid, advect, None);
                    self.margins.note("advect", margin / h);
                    let (p, margin) = pool_cpu::retype(p, &at, &fields.distance, &fields.cells, &grid);
                    self.margins.note("retype", margin);
                    pool_cpu::age(p, age)
                })
                .collect();
            if self.preserve_foam {
                stepped = Self::preserve(&self.shape, &mut self.margins, &mut self.preserved, &mut self.foam_density, stepped);
            }
            let (flags, margins) = pool_cpu::keep(&stepped, &self.scene.solid, &grid, MAX_PER_CELL as u32);
            for margin in margins {
                self.margins.note("keep", margin / h);
            }
            let live = self.state.live;
            self.pool = pool_cpu::compact(&stepped, &flags, &mut self.state);
            self.removed += live - self.state.live;
        }
    }

    fn snapshot(&self) -> Snapshot {
        let populations = [1, 0, 2].map(|kind| {
            self.pool
                .iter()
                .filter(|p| p.kind == kind)
                .map(|p| {
                    let [x, y, z, lifetime] = p.position_lifetime;
                    FluidParticle { position_radius: [x, y, z, whitewater_fade(lifetime)], velocity: p.velocity, id: 0 }
                })
                .collect::<Vec<_>>()
        });
        let report = Report { dust: 0,
            counts: populations.each_ref().map(|p| p.len() as u32),
            emitted: self.state.emitted,
            thinned: self.state.thinned,
            pool_full: self.state.pool_full,
            live: self.state.live,
            next_id: self.state.next_id,
        };
        Snapshot { report, populations }
    }
}

/// What the node publishes after each of [`FRAMES`], offline: on a frame
/// with ticks, the snapshot of the previous frame with ticks; ticks 0 hold
/// what is shown; zeros on the first frame and on a new epoch.
fn expected(preserve_foam: bool) -> (Vec<Snapshot>, Model) {
    let mut model = Model::new(preserve_foam);
    let mut published = Snapshot::default();
    let mut pending = None;
    let mut epoch = None;
    let mut out = Vec::new();
    for (ticks, e) in FRAMES {
        if epoch != Some(e) {
            model.reseed();
            published = Snapshot::default();
            pending = None;
            epoch = Some(e);
        }
        if ticks > 0 {
            if let Some(snapshot) = pending.take() {
                published = snapshot;
            }
            model.frame(ticks, e);
            pending = Some(model.snapshot());
        }
        out.push(published.clone());
    }
    (out, model)
}

/// The fixture's own soundness: no decision within [`TOL`] of its
/// threshold, and every path the proof claims exercised.
#[test]
fn whitewater_step_reference_is_tie_free() {
    reference_is_tie_free(false);
}

/// The same fixture with Preserve Foam on: still tie-free, with foam for
/// the pass to count. The fixture's foam never crowds past the step's min
/// density (FLIP's 20 a cell), so no slot gains lifetime here and the
/// across-frames proof with the toggle on covers the routing, not the gain:
/// BUG-6zi1 (Preserve Foam gain proof). The gain itself is proven per pass
/// in `whitewater_pool_tests::preserve_foam_matches_flip`.
#[test]
fn whitewater_step_reference_is_tie_free_preserving_foam() {
    let model = reference_is_tie_free(true);
    assert!(model.foam_density > 0, "no foam for the preservation to count");
    assert_eq!(model.preserved, 0, "the fixture now exercises the gain: assert it and close BUG-6zi1");
}

fn reference_is_tie_free(preserve_foam: bool) -> Model {
    let (frames, model) = expected(preserve_foam);
    for (k, f) in frames.iter().enumerate() {
        println!("frame {k}: {:?}", f.report);
    }
    println!(
        "closest decision {:?}, removed {}, foam preserved {}, most foam in a cell {}",
        model.margins.closest,
        model.removed,
        model.preserved,
        model.foam_density
    );
    assert_eq!(model.margins.near, 0, "{} decisions within {TOL}; closest {:?}", model.margins.near, model.margins.closest);
    let last = |epoch: u32| frames.iter().zip(FRAMES).filter(|(_, (_, e))| *e == epoch).map(|(f, _)| f.report).next_back().expect("frame");
    let first = frames[4].report;
    assert!(frames[0].report == Report::default() && frames[5].report == Report::default(), "nothing published on frames 0 and 5");
    assert!(frames[1].report == frames[2].report && frames[2].report == frames[3].report, "ticks 0 hold what is shown");
    assert_ne!(frames[3].report, frames[4].report, "the pool written before the hold is published on the next tick");
    assert!((0..3).all(|p| frames.iter().any(|f| f.report.counts[p] > 0)), "every population published at least once");
    assert!(first.pool_full > 0 && first.thinned > 0, "the pool filled and the emitters were thinned: {first:?}");
    assert!(model.removed > 0, "the tick removed particles");
    let published = frames.iter().flat_map(|f| f.populations.iter().flatten());
    assert!(published.clone().all(|p| p.position_radius[3] > 0.0), "only living particles are published");
    assert!(last(1).emitted > 0 && last(1).emitted < first.emitted, "the new epoch counts from 0: {:?}", last(1));
    model
}

#[cfg(feature = "gpu-proofs")]
mod gpu {
    use std::cell::Cell;

    use manifold_gpu::GpuBuffer;

    use manifold_node_engine::testkit::array_harness::{Harness, read};
    use super::super::whitewater_step::{Step, StepFrame, StepInputs};
    use super::*;
    use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
    use crate::whitewater_handoff::Fence;

    /// A frame clock the test retires by hand; offline's wait retires.
    #[derive(Default)]
    struct HandFence {
        next: Cell<u64>,
        retired: Cell<u64>,
    }

    impl Fence for HandFence {
        fn stamp(&self) -> u64 {
            self.next.get()
        }

        fn is_complete(&self, stamp: u64) -> bool {
            stamp <= self.retired.get()
        }

        fn wait(&self, stamp: u64) -> bool {
            self.retired.set(self.retired.get().max(stamp));
            true
        }
    }

    fn close(a: f32, b: f32, tolerance: f32) -> bool {
        (a - b).abs() <= tolerance * (1.0 + a.abs().max(b.abs()))
    }

    fn particle_close(got: &FluidParticle, want: &FluidParticle) -> bool {
        (0..3).all(|a| close(got.position_radius[a], want.position_radius[a], 1e-4) && close(got.velocity[a], want.velocity[a], 1e-3))
            && close(got.position_radius[3], want.position_radius[3], 1e-4)
            && got.id == want.id
    }

    /// The node over [`FRAMES`], offline, against [`expected`]: the report
    /// and each population, particle for particle, with the slots past the
    /// count zeroed.
    #[test]
    fn whitewater_step_matches_cpu_across_frames() {
        across_frames(false);
    }

    /// The same with Preserve Foam on: the preservation pass runs between
    /// the age and the keep, and the tick's pools swap the other way.
    #[test]
    fn whitewater_step_matches_cpu_across_frames_preserving_foam() {
        across_frames(true);
    }

    /// Same-time rows at 30/60/120 fps, like the liquid conformance export
    /// proof. Capture every tick before another tick can reuse stage storage;
    /// a 30 fps frame encodes both ticks into one command buffer, without a
    /// host fence between them. The 120 fps run also checks held frames.
    #[test]
    fn whitewater_per_tick_gpu_rows_match_at_every_frame_rate() {
        let harness = Harness::new();
        let cells = [8u32; 3];
        let nodes = [13u32; 3];
        let h = 0.1;
        let shape = StepShape::new(nodes, nodes, cells, 1.0,
            Some(Transform { pos: [0.6; 3], scale: [1.2; 3], ..Transform::default() }), 256).unwrap();
        let shared = |bytes: &[u8]| {
            let b = harness.device.create_buffer_shared(bytes.len() as u64);
            // SAFETY: fresh shared storage, not submitted yet.
            unsafe { b.write(0, bytes) };
            b
        };
        // Analytic sphere sampled at the solver's cell centres. It exercises
        // the distance path directly, never the rendered surface level set.
        let phi: Vec<f32> = (0..512).map(|i| {
            let p = [i % 8, (i / 8) % 8, i / 64].map(|v| v as f32 + 0.5 - 4.0);
            ((p.iter().map(|v| v * v).sum::<f32>()).sqrt() - 2.5).min(3.0) * h
        }).collect();
        let distance = shared(bytemuck::cast_slice(&phi));
        let solid = shared(bytemuck::cast_slice(&vec![10.0f32; 13 * 13 * 13]));
        let mut rng = Rng(0x215);
        let particles: Vec<FluidParticle> = (0..256).map(|_| {
            let d: [f32; 3] = std::array::from_fn(|_| 2.0 * rng.unit() - 1.0);
            let norm = d.iter().map(|v| v * v).sum::<f32>().sqrt().max(0.001);
            let p = d.map(|v| 0.6 + 0.24 * v / norm);
            FluidParticle { position_radius: [p[0], p[1], p[2], 0.05], velocity: [0.0; 3], id: 0 }
        }).collect();
        let particles = shared(bytemuck::cast_slice(&particles));
        // Each tick owns immutable inputs, including when two ticks share an
        // encoder. CPU overwrites of a shared face array would hide this bug.
        let faces: Vec<[GpuBuffer; 3]> = (0..12).map(|tick| std::array::from_fn(|a| {
            let velocity = FLOW[a] * (1.0 + tick as f32 / 60.0);
            shared(bytemuck::cast_slice(&vec![velocity; face_len(cells, a) as usize]))
        })).collect();
        let run = |fps: u32| {
            let pool = shared(bytemuck::cast_slice(&vec![empty_slot(); 256]));
            let state = shared(bytemuck::cast_slice(&[0u32; 8]));
            let counts = shared(bytemuck::cast_slice(&[0u32; 8]));
            let populations: [GpuBuffer; 3] = std::array::from_fn(|_| shared(&vec![0u8; 256 * 32]));
            let mut stage = Step::default();
            let mut tick = 0u32;
            let mut rows = Vec::new();
            let mut previous = Vec::new();
            for frame in 1..=fps / 5 {
                let due = frame * 60 / fps;
                let held = tick == due;
                let mut native = harness.device.create_encoder("whitewater per tick frame");
                while tick < due {
                    let f = &faces[tick as usize];
                    let inputs = StepInputs { motion: None, obstacle_source: None, particles: &particles, solid: &solid,
                        faces: crate::primitives::whitewater_step::FaceSource::Axes([&f[0], &f[1], &f[2]]), level_set: &distance, distance: Some(&distance) };
                    let settings = StepFrame { shape, count: Some(256), ticks: 1, dt: TICK as f32, epoch: 0,
                        seed: tick as f32 * TICK as f32, gravity: GRAVITY,
                        wavecrest_emission: WAVECREST_RATE, turbulence_emission: 175.0, min_turbulence: 100.0, max_turbulence: 200.0, inside_emission: true, generation_rate: 1.0, spray_speed: 1.0, dust_emission: false, boundary_dust: false, dust_rate: 175.0, influence_base: 1.0, influence_decay: 2.0, min_energy: MIN_ENERGY,
                        max_energy: MAX_ENERGY, preserve_foam: false };
                    stage.advance_tick(&mut GpuEncoder::new(&mut native, &harness.device),
                        &settings, &inputs, &pool, &state, true).unwrap();
                    // The boundary's capture→state pairs, including counters,
                    // IDs and rendering outputs, close after EVERY tick.
                    for (port, destination) in [
                        ("pool_out", &pool), ("state_out", &state), ("counts_out", &counts),
                        ("foam_particles", &populations[0]), ("bubble_particles", &populations[1]),
                        ("spray_particles", &populations[2]),
                    ] {
                        let source = stage.tick_output(port).unwrap();
                        native.copy_buffer_to_buffer(source, destination, destination.size);
                    }
                    tick += 1;
                }
                native.commit_and_wait_completed();
                let words: Vec<u32> = [&pool, &state, &counts, &populations[0], &populations[1], &populations[2]]
                    .into_iter().flat_map(|b| read::<u32>(b, b.size as usize / 4)).collect();
                if held && !previous.is_empty() { assert_eq!(words, previous, "held frame {frame}"); }
                previous = words.clone();
                if frame * 30 % fps == 0 { rows.push(words); }
            }
            let state_words: Vec<u32> = read(&state, 8);
            assert!(state_words[3] > 0, "fixture must emit, got {state_words:?}");
            assert!(state_words[0] > 0, "fixture must retain live particles");
            rows
        };
        let at_60 = run(60);
        assert_eq!(run(30), at_60, "30 fps pool, IDs, counters and rendered particles");
        assert_eq!(run(120), at_60, "120 fps pool, IDs, counters and rendered particles");
    }

    fn across_frames(preserve_foam: bool) {
        let harness = Harness::new();
        let scene = Scene::new();
        let shared = |bytes: &[u8]| {
            let buffer = harness.device.create_buffer_shared(bytes.len() as u64);
            // SAFETY: a fresh shared buffer of exactly these bytes.
            unsafe { buffer.write(0, bytes) };
            buffer
        };
        let particles = shared(bytemuck::cast_slice(&scene.particles));
        let solid = shared(bytemuck::cast_slice(&scene.solid));
        let level = shared(bytemuck::cast_slice(&scene.level));
        let faces: [GpuBuffer; 3] = std::array::from_fn(|a| shared(bytemuck::cast_slice(&scene.faces[a])));
        let inputs = StepInputs { motion: None, obstacle_source: None, particles: &particles, solid: &solid, faces: crate::primitives::whitewater_step::FaceSource::Axes([&faces[0], &faces[1], &faces[2]]), level_set: &level, distance: None };
        let (want, _) = expected(preserve_foam);
        let fence = HandFence::default();
        let mut step = Step::default();
        let mut compared = 0;
        for (k, (&(ticks, epoch), want)) in FRAMES.iter().zip(&want).enumerate() {
            fence.next.set(fence.next.get() + 1);
            let frame = StepFrame {
                shape: shape(),
                count: Some(LIVE),
                ticks,
                dt: TICK as f32,
                epoch,
                seed: SEED,
                gravity: GRAVITY,
                wavecrest_emission: WAVECREST_RATE, turbulence_emission: 175.0, min_turbulence: 100.0, max_turbulence: 200.0, inside_emission: true, generation_rate: 1.0, spray_speed: 1.0, dust_emission: false, boundary_dust: false, dust_rate: 175.0, influence_base: 1.0, influence_decay: 2.0,
                min_energy: MIN_ENERGY,
                max_energy: MAX_ENERGY,
                preserve_foam,
            };
            let mut native = harness.device.create_encoder("whitewater step test");
            let report = {
                let mut gpu = GpuEncoder::new(&mut native, &harness.device);
                step.advance(&mut gpu, &fence, true, &frame, &inputs)
            };
            native.commit_and_wait_completed();
            let report = report.unwrap_or_else(|error| panic!("frame {k}: {error}"));
            println!("frame {k}: {report:?}");
            assert_eq!(report, want.report, "frame {k}");
            let Some(slot) = step.outputs.current() else {
                assert_eq!(want.report, Report::default(), "frame {k}: nothing published");
                continue;
            };
            for (p, wanted) in want.populations.iter().enumerate() {
                let got: Vec<FluidParticle> = read(&slot.buffers[p], CAPACITY as usize);
                for (i, (g, w)) in got.iter().zip(wanted).enumerate() {
                    assert!(particle_close(g, w), "frame {k} population {p} particle {i}: GPU {g:?} CPU {w:?}");
                }
                let zero = FluidParticle { position_radius: [0.0; 4], velocity: [0.0; 3], id: 0 };
                assert!(got[wanted.len()..].iter().all(|g| bytemuck::bytes_of(g) == bytemuck::bytes_of(&zero)), "frame {k} population {p}: tail not zeroed");
                compared += wanted.len();
            }
        }
        println!("{compared} particles compared");
    }
}
