//! GPU value proofs for the whitewater emitter atoms against their CPU
//! statements (`whitewater_particle_cpu`), I7, and the chain folded into one
//! kernel against the atoms run one by one. The grid arrays sit on world
//! tiles under one table, the box placed so it straddles tiles -1 and 0 on x
//! and z. Particles lie well inside the box, where the dense statements and
//! the pooled atoms read the same cells, or in no stored tile.

use super::emission_count::EmissionCount;
use super::energy_potential::EnergyPotential;
use super::jitter_particles::JitterParticles;
use super::sample_faces_at_particles::SampleFacesAtParticles;
use super::spawn_whitewater::SpawnWhitewater;
use super::wavecrest_potential::WavecrestPotential;
use super::whitewater_cpu::{CANONICAL_SOLID, Rng, canonical_distance};
use super::whitewater_type::WhitewaterType;
use manifold_gpu::GpuBuffer;
use manifold_node_engine::bindings::Slot;
use manifold_node_engine::exec::effect_node::ParamValues;
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::ports::KnownItem;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_node_engine::testkit::fused_atoms::run;
use manifold_water_liquid::fluid_particles::{FaceSample, WhitewaterSpawn};
use manifold_water_liquid::grid::face_len;
use manifold_water_liquid::testkit::world_box::BoxTable;
use manifold_water_liquid::whitewater::{CELL_AIR, KnownValue, cell_coords};
use {
    super::whitewater_particle_cpu::Box3, super::whitewater_particle_cpu::Crest,
    super::whitewater_particle_cpu::Emission, crate::primitives::whitewater_particle_cpu as cpu,
};

/// Unequal sides, so a swapped axis shows; room for emitters well inside.
const NODES: [u32; 3] = [27, 25, 23];
const H: f32 = 0.25;
/// The world cell of the box's first cell: the box crosses tiles -1 and 0 on
/// x and z.
const ORIGIN: [i32; 3] = [-12, 2, -9];
const SLOTS: usize = 700;
/// Cells a particle keeps from the box's sides, past the reach of every
/// atom's stencil.
const REACH: f32 = 2.0;
/// Cells an emitter keeps from them: its spawns land up to 3.3 cells away.
const EMITTER_REACH: f32 = 6.0;
/// Metres along x that carry a particle into a tile the table does not hold.
const FAR: f32 = 64.0 * H;

fn cell_dims() -> [u32; 3] {
    NODES.map(|n| n - 1)
}

/// The scene position of the low corner of world cell `origin`, the first node
/// of a box whose first cell it is.
fn low_corner(origin: [i32; 3], h: f32) -> [f32; 3] {
    origin.map(|c| (c as f32 - 0.5) * h)
}

/// The scene position of the box's first node.
fn first_node() -> [f32; 3] {
    low_corner(ORIGIN, H)
}

/// The dense grid the CPU statements read over the box's cells.
fn grid() -> Box3 {
    let cells = cell_dims();
    let size = cells.map(|n| n as f32 * H);
    let first = first_node();
    Box3 {
        cells,
        center: std::array::from_fn(|a| first[a] + 0.5 * size[a]),
        size,
    }
}

/// The box's tile table, shared by every pooled array: node arrays over its
/// nodes, cell arrays over one fewer a side, on the same tiles. `h` is the
/// cell size in metres.
pub(super) struct Rig {
    pub(super) nodes: BoxTable,
    pub(super) cells: BoxTable,
    h: f32,
}

impl Rig {
    /// A box of `nodes` nodes whose first cell is world cell `origin`.
    pub(super) fn new(origin: [i32; 3], nodes: [u32; 3], h: f32) -> Self {
        let table = BoxTable::new(origin, nodes);
        let cells = table.with_cells(nodes.map(|n| n - 1));
        Self {
            nodes: table,
            cells,
            h,
        }
    }

    /// The scene position of the box's first node.
    pub(super) fn first_node(&self) -> [f32; 3] {
        low_corner(self.nodes.origin, self.h)
    }

    /// The scene position `offset` metres from the box's first node.
    pub(super) fn point(&self, offset: [f32; 3]) -> [f32; 3] {
        let first = self.first_node();
        std::array::from_fn(|a| first[a] + offset[a])
    }

    /// The `cell_size` and `pool_tiles` values beside `extra`.
    pub(super) fn values(&self, extra: &[(&'static str, f32)]) -> Vec<(&'static str, f32)> {
        let mut all = self.pool_values().to_vec();
        all.extend_from_slice(extra);
        all
    }

    /// [`Self::values`] as params.
    pub(super) fn params(&self, extra: &[(&'static str, f32)]) -> ParamValues {
        params(&self.values(extra))
    }

    pub(super) fn pool_values(&self) -> [(&'static str, f32); 2] {
        [
            ("cell_size", self.h),
            ("pool_tiles", self.cells.pool_tiles as f32),
        ]
    }

    /// The table words the atoms read the pool through.
    pub(super) fn tiles(&self, harness: &mut Harness) -> (Slot, GpuBuffer) {
        harness.array(&self.cells.words, self.cells.words.len())
    }

    /// A cell array over the box's cells (x fastest), `canon` elsewhere.
    pub(super) fn cell_pool<T: KnownItem + Copy>(
        &self,
        harness: &mut Harness,
        dense: &[T],
        canon: T,
    ) -> (Slot, GpuBuffer) {
        let pool = self.cells.to_pool(dense, canon);
        harness.array(&pool, pool.len())
    }

    /// A node array over the box's nodes (x fastest), `canon` elsewhere.
    pub(super) fn node_pool<T: KnownItem + Copy>(
        &self,
        harness: &mut Harness,
        dense: &[T],
        canon: T,
    ) -> (Slot, GpuBuffer) {
        let pool = self.nodes.to_pool(dense, canon);
        harness.array(&pool, pool.len())
    }
}

/// A position `reach` cells in from the box's sides; one in six is carried
/// `FAR` along x, into a tile the table does not hold.
fn position(rng: &mut Rng, reach: f32) -> [f32; 3] {
    let cells = cell_dims();
    let first = first_node();
    let mut p: [f32; 3] = std::array::from_fn(|a| {
        first[a] + (reach + (cells[a] as f32 - 2.0 * reach) * rng.unit()) * H
    });
    if rng.unit() < 1.0 / 6.0 {
        p[0] += FAR;
    }
    p
}

/// Particles at `position(reach)`, a tenth of the slots empty, velocities up
/// to `speed` on each axis.
fn particles(rng: &mut Rng, speed: f32, reach: f32) -> Vec<FluidParticle> {
    (0..SLOTS)
        .map(|i| {
            let p = position(rng, reach);
            let radius = if rng.unit() < 0.1 { 0.0 } else { 0.05 };
            let velocity = std::array::from_fn(|_| speed * (2.0 * rng.unit() - 1.0));
            FluidParticle {
                position_radius: [p[0], p[1], p[2], radius],
                velocity,
                id: i as u32 + 1,
            }
        })
        .collect()
}

/// The face arrays of the box's cells: the three dense arrays the CPU reads
/// (FLIP's seam layout, one face more along their axis than the box has
/// cells), and the pooled records that carry them (face f of an axis is cell
/// f's low face). A tenth of the faces have no weight: they read 0 whatever
/// velocity their record holds, and hold 0 in the dense array.
struct Faces {
    dense: [Vec<f32>; 3],
    records: Vec<FaceSample>,
}

impl Faces {
    fn new(rng: &mut Rng, speed: f32) -> Self {
        let cells = cell_dims();
        let mut dense: [Vec<f32>; 3] = std::array::from_fn(|axis| {
            (0..face_len(cells, axis))
                .map(|_| speed * (2.0 * rng.unit() - 1.0))
                .collect()
        });
        let total: usize = cells.iter().map(|&n| n as usize).product();
        let mut records = vec![FaceSample::default(); total];
        for (i, record) in records.iter_mut().enumerate() {
            let c = cell_coords(cells, i).map(|v| v as i32);
            for (axis, axis_dense) in dense.iter_mut().enumerate() {
                let face = cpu::face_index(c, axis, 0, cells).expect("a face of the box");
                if rng.unit() < 0.1 {
                    record.velocity[axis] = 7.0 * speed;
                    axis_dense[face] = 0.0;
                } else {
                    record.velocity[axis] = axis_dense[face];
                    record.weight[axis] = 1.0;
                }
            }
        }
        Self { dense, records }
    }

    fn arrays(&self) -> [&[f32]; 3] {
        self.dense.each_ref().map(Vec::as_slice)
    }
}

fn close(a: f32, b: f32, tolerance: f32) -> bool {
    (a - b).abs() <= tolerance * b.abs().max(1.0)
}

/// Every live slot moves by a uniform offset of up to a quarter cell per
/// axis, the same as the CPU draws; empty slots stay put.
#[test]
fn jitter_particles_matches_cpu() {
    let input = particles(&mut Rng(0x7177_0001), 1.0, REACH);
    let mut harness = Harness::new();
    let slot = harness.array(&input, SLOTS);
    let p = params(&[("cell_size", H), ("seed", 12.345), ("epoch", 3.0)]);
    let got: Vec<FluidParticle> = run(
        &mut harness,
        &mut JitterParticles::new(),
        &[("particles", slot.0)],
        SLOTS,
        &p,
    );
    let reach = 0.25 * (1.0 - 1e-3) * H;
    let mut moved = 0;
    for (i, (g, p)) in got.iter().zip(&input).enumerate() {
        let want = cpu::jitter(*p, i as u32, H, 12.345, 3.0);
        assert_eq!(
            (g.velocity, g.id, g.position_radius[3]),
            (p.velocity, p.id, p.position_radius[3]),
            "slot {i}"
        );
        for a in 0..3 {
            assert!(
                close(g.position_radius[a], want.position_radius[a], 1e-6),
                "slot {i}: GPU {g:?} CPU {want:?}"
            );
            assert!(
                (g.position_radius[a] - p.position_radius[a]).abs() <= reach * 1.0001,
                "slot {i} moved past the jitter"
            );
        }
        moved += usize::from(g.position_radius != p.position_radius);
    }
    let live = input.iter().filter(|p| p.position_radius[3] > 0.0).count();
    assert_eq!(moved, live, "every live slot moves, no empty one does");
    let other: Vec<FluidParticle> = run(
        &mut harness,
        &mut JitterParticles::new(),
        &[("particles", slot.0)],
        SLOTS,
        &params(&[("cell_size", H), ("seed", 12.345), ("epoch", 4.0)]),
    );
    assert!(
        other.iter().zip(&got).filter(|(a, b)| a != b).count() > live / 2,
        "a new epoch jitters afresh"
    );
}

/// FLIP's MAC trilinear of the pooled faces at each live particle; 0 in a
/// cell no stored tile holds.
#[test]
fn sample_faces_at_particles_matches_cpu() {
    let mut rng = Rng(0x5a4f_0001);
    let rig = Rig::new(ORIGIN, NODES, H);
    let input = particles(&mut rng, 1.0, REACH);
    let faces = Faces::new(&mut rng, 2.0);
    let mut harness = Harness::new();
    let slot = harness.array(&input, SLOTS);
    let records = rig.cell_pool(&mut harness, &faces.records, FaceSample::default());
    let tiles = rig.tiles(&mut harness);
    let got: Vec<FluidParticle> = run(
        &mut harness,
        &mut SampleFacesAtParticles::new(),
        &[
            ("particles", slot.0),
            ("faces", records.0),
            ("tiles", tiles.0),
        ],
        SLOTS,
        &rig.params(&[]),
    );
    let g = grid();
    let (mut outside, mut moving) = (0, 0);
    for (i, (got, p)) in got.iter().zip(&input).enumerate() {
        let want = cpu::sample_faces(*p, faces.arrays(), cell_dims(), &g);
        assert_eq!(
            (got.position_radius, got.id),
            (p.position_radius, p.id),
            "slot {i}"
        );
        for a in 0..3 {
            assert!(
                close(got.velocity[a], want.velocity[a], 1e-5),
                "slot {i}: GPU {got:?} CPU {want:?}"
            );
        }
        let q = g.position([
            p.position_radius[0],
            p.position_radius[1],
            p.position_radius[2],
        ]);
        let inside = (0..3).all(|a| q[a] >= 0.0 && q[a] < g.cells[a] as f32);
        outside += usize::from(p.position_radius[3] > 0.0 && !inside);
        moving += usize::from(want.velocity != [0.0; 3]);
    }
    assert!(
        outside > 30 && moving > 300,
        "{outside} live particles outside the stored tiles, {moving} moving"
    );
}

/// ½|v|², held and scaled, at energies below, inside and above the range.
#[test]
fn energy_potential_matches_cpu() {
    let input = particles(&mut Rng(0xe4e7_0001), 12.0, REACH);
    let mut harness = Harness::new();
    let slot = harness.array(&input, SLOTS);
    let p = params(&[("min_energy", 0.1), ("max_energy", 60.0)]);
    let got: Vec<f32> = run(
        &mut harness,
        &mut EnergyPotential::new(),
        &[("particles", slot.0)],
        SLOTS,
        &p,
    );
    let mut spread = [0; 3];
    for (i, (g, p)) in got.iter().zip(&input).enumerate() {
        let want = cpu::energy(*p, 0.1, 60.0);
        assert!((g - want).abs() <= 1e-6, "slot {i}: GPU {g} CPU {want}");
        spread[if want <= 0.0 {
            0
        } else if want >= 1.0 {
            2
        } else {
            1
        }] += 1;
    }
    assert!(
        spread.iter().all(|&n| n > 20),
        "below, inside and above the range: {spread:?}"
    );
}

/// Random fields and particles reach every wavecrest branch; the GPU and
/// CPU agree except where a value sits within 1e-4 of a threshold.
#[test]
fn wavecrest_potential_matches_cpu() {
    let mut rng = Rng(0xc4e5_0001);
    let rig = Rig::new(ORIGIN, NODES, H);
    let input = particles(&mut rng, 3.0, REACH);
    let g = grid();
    let total = g.cells.iter().map(|&n| n as usize).product::<usize>();
    let distance: Vec<f32> = (0..total).map(|_| (4.0 * rng.unit() - 2.0) * H).collect();
    let curvature: Vec<KnownValue> = (0..total)
        .map(|_| KnownValue {
            value: 1.6 * rng.unit() / H,
            known: 1.0,
        })
        .collect();
    let kinds: Vec<u32> = (0..total).map(|_| (3.0 * rng.unit()) as u32 % 3).collect();
    let mut harness = Harness::new();
    let slot = harness.array(&input, SLOTS);
    let d = rig.cell_pool(&mut harness, &distance, canonical_distance(H));
    let k = rig.cell_pool(&mut harness, &curvature, KnownValue::default());
    let c = rig.cell_pool(&mut harness, &kinds, CELL_AIR);
    let tiles = rig.tiles(&mut harness);
    let crest = Crest {
        min_curvature: 0.4,
        max_curvature: 1.0,
        sharpness: 0.4,
    };
    let p = rig.params(&[
        ("min_curvature", 0.4),
        ("max_curvature", 1.0),
        ("sharpness", 0.4),
    ]);
    let got: Vec<f32> = run(
        &mut harness,
        &mut WavecrestPotential::new(),
        &[
            ("particles", slot.0),
            ("distance", d.0),
            ("curvature", k.0),
            ("cells", c.0),
            ("tiles", tiles.0),
        ],
        SLOTS,
        &p,
    );
    let (mut crests, mut full, mut edges) = (0, 0, 0);
    for (i, (got, p)) in got.iter().zip(&input).enumerate() {
        let (want, margin) = cpu::wavecrest(*p, &distance, &curvature, &kinds, &g, crest);
        if margin < 1e-4 {
            edges += 1;
            continue;
        }
        assert!(
            (got - want).abs() <= 1e-5,
            "slot {i}: GPU {got} CPU {want} (margin {margin})"
        );
        crests += usize::from(want > 0.0);
        full += usize::from(want >= 1.0);
    }
    assert!(
        crests > 30 && full > 5 && edges < 10,
        "{crests} crests, {full} full, {edges} on an edge"
    );
}

/// The count per tick times the ticks, 0 past the live count, for empty
/// slots and slow particles.
#[test]
fn emission_count_matches_cpu() {
    let mut rng = Rng(0xe315_0001);
    let mut input = particles(&mut rng, 2.0, REACH);
    for p in input.iter_mut().step_by(7) {
        p.velocity = p.velocity.map(|v| v * 1e-4);
    }
    let energy: Vec<f32> = (0..SLOTS)
        .map(|_| if rng.unit() < 0.1 { 0.0 } else { rng.unit() })
        .collect();
    let wavecrest: Vec<f32> = (0..SLOTS)
        .map(|_| if rng.unit() < 0.2 { 0.0 } else { rng.unit() })
        .collect();
    let mut harness = Harness::new();
    let slot = harness.array(&input, SLOTS);
    let (e, w) = (
        harness.array(&energy, SLOTS),
        harness.array(&wavecrest, SLOTS),
    );
    let emission = Emission {
        dt: 1.0 / 60.0,
        rate: 175.0,
        points_per_cell: 4.0,
        ticks: 2.0,
        live_count: (SLOTS - 50) as f32,
    };
    let p = params(&[
        ("rate", emission.rate),
        ("points_per_cell", emission.points_per_cell),
        ("ticks", emission.ticks),
        ("live_count", emission.live_count),
        ("dt", emission.dt),
    ]);
    let got: Vec<u32> = run(
        &mut harness,
        &mut EmissionCount::new(),
        &[("particles", slot.0), ("energy", e.0), ("wavecrest", w.0)],
        SLOTS,
        &p,
    );
    let mut emitting = 0;
    for (i, g) in got.iter().enumerate() {
        let (want, edge) =
            cpu::emission_count(input[i], energy[i], wavecrest[i], i as u32, emission);
        if edge < 1e-5 {
            continue;
        }
        assert_eq!(*g, want, "slot {i}");
        emitting += usize::from(want > 0);
    }
    assert!(emitting > 200, "{emitting} slots emit");
    assert!(
        got[SLOTS - 50..].iter().all(|&n| n == 0),
        "slots past the live count emit nothing"
    );
}

/// A stretched live interval changes the emission count by its accepted
/// duration; compare the GPU value with the CPU statement at 0.1 seconds.
#[test]
fn live_interval_whitewater_emission_duration() {
    let input: Vec<FluidParticle> = (0..8)
        .map(|i| FluidParticle {
            position_radius: [0.0, 0.0, 0.0, 0.05],
            velocity: [1.0, 0.0, 0.0],
            id: i + 1,
        })
        .collect();
    let energy = vec![1.0f32; input.len()];
    let wavecrest = vec![1.0f32; input.len()];
    let emission = Emission {
        dt: 0.1,
        rate: 80.0,
        points_per_cell: 8.0,
        ticks: 1.0,
        live_count: input.len() as f32,
    };
    let mut harness = Harness::new();
    let particles = harness.array(&input, input.len());
    let energy_buffer = harness.array(&energy, energy.len());
    let crest_buffer = harness.array(&wavecrest, wavecrest.len());
    let got: Vec<u32> = run(
        &mut harness,
        &mut EmissionCount::new(),
        &[
            ("particles", particles.0),
            ("energy", energy_buffer.0),
            ("wavecrest", crest_buffer.0),
        ],
        input.len(),
        &params(&[
            ("rate", emission.rate),
            ("points_per_cell", emission.points_per_cell),
            ("ticks", emission.ticks),
            ("live_count", emission.live_count),
            ("dt", emission.dt),
        ]),
    );
    for (i, (&actual, &particle)) in got.iter().zip(&input).enumerate() {
        let (expected, margin) =
            cpu::emission_count(particle, energy[i], wavecrest[i], i as u32, emission);
        assert!(
            margin > 1e-5,
            "fixture sits on an emission threshold at slot {i}"
        );
        assert_eq!(actual, expected, "slot {i}: duration-aware emission");
    }
    assert!(
        got.iter().all(|&count| count == 8),
        "0.1 seconds emits eight particles per slot: {got:?}"
    );
}

/// The five emitter atoms folded into one kernel, as
/// `whitewater_emitter_chain_fuses` finds them: the particles read in place,
/// the faces, grid fields and the one table gathered, one register threaded
/// from jitter to count. The fused counts equal the atoms run one by one, bit for bit, and
/// the atoms run one by one equal the CPU.
#[test]
fn whitewater_emitter_chain_fused_matches_unfused() {
    use manifold_gpu::GpuBinding;
    use manifold_node_engine::exec::effect_node::NodeInstanceId;
    use manifold_node_engine::freeze::classify::CapacityExpr;
    use manifold_node_engine::freeze::codegen::{
        ENTRY, FusionRegion, InputSource, RegionNode, generate_fused,
    };
    use manifold_node_engine::primitive::PrimitiveSpec;

    let mut rng = Rng(0xf05e_0002);
    let rig = Rig::new(ORIGIN, NODES, H);
    let input = particles(&mut rng, 3.0, REACH);
    // Fast enough that most energy potentials reach 1.
    let faces = Faces::new(&mut rng, 12.0);
    let g = grid();
    let total = g.cells.iter().map(|&n| n as usize).product::<usize>();
    let distance: Vec<f32> = (0..total).map(|_| (4.0 * rng.unit() - 2.0) * H).collect();
    let curvature: Vec<KnownValue> = (0..total)
        .map(|_| KnownValue {
            value: 1.6 * rng.unit() / H,
            known: 1.0,
        })
        .collect();
    let kinds: Vec<u32> = (0..total).map(|_| (3.0 * rng.unit()) as u32 % 3).collect();
    let values: Vec<(&'static str, f32)> = vec![
        ("seed", 7.25),
        ("epoch", 2.0),
        ("min_energy", 0.1),
        ("max_energy", 60.0),
        ("min_curvature", 0.4),
        ("max_curvature", 1.0),
        ("sharpness", 0.4),
        ("rate", 175.0),
        ("points_per_cell", 2.0),
        ("ticks", 2.0),
        ("live_count", (SLOTS - 30) as f32),
        ("dt", 0.1),
    ];
    let all = rig.params(&values);

    let mut harness = Harness::new();
    let slot = harness.array(&input, SLOTS);
    let records = rig.cell_pool(&mut harness, &faces.records, FaceSample::default());
    let d = rig.cell_pool(&mut harness, &distance, canonical_distance(H));
    let k = rig.cell_pool(&mut harness, &curvature, KnownValue::default());
    let c = rig.cell_pool(&mut harness, &kinds, CELL_AIR);
    let tiles = rig.tiles(&mut harness);
    let jittered: Vec<FluidParticle> = run(
        &mut harness,
        &mut JitterParticles::new(),
        &[("particles", slot.0)],
        SLOTS,
        &all,
    );
    let jittered_in = harness.array(&jittered, SLOTS);
    let sampled: Vec<FluidParticle> = run(
        &mut harness,
        &mut SampleFacesAtParticles::new(),
        &[
            ("particles", jittered_in.0),
            ("faces", records.0),
            ("tiles", tiles.0),
        ],
        SLOTS,
        &all,
    );
    let sampled_in = harness.array(&sampled, SLOTS);
    let energy: Vec<f32> = run(
        &mut harness,
        &mut EnergyPotential::new(),
        &[("particles", sampled_in.0)],
        SLOTS,
        &all,
    );
    let crest: Vec<f32> = run(
        &mut harness,
        &mut WavecrestPotential::new(),
        &[
            ("particles", sampled_in.0),
            ("distance", d.0),
            ("curvature", k.0),
            ("cells", c.0),
            ("tiles", tiles.0),
        ],
        SLOTS,
        &all,
    );
    let (energy_in, crest_in) = (harness.array(&energy, SLOTS), harness.array(&crest, SLOTS));
    let unfused: Vec<u32> = run(
        &mut harness,
        &mut EmissionCount::new(),
        &[
            ("particles", sampled_in.0),
            ("energy", energy_in.0),
            ("wavecrest", crest_in.0),
        ],
        SLOTS,
        &all,
    );

    macro_rules! member {
        ($n:expr, $atom:ty, $inputs:expr) => {
            RegionNode {
                node_id: NodeInstanceId($n),
                fusion_kind: <$atom as PrimitiveSpec>::FUSION_KIND,
                body: <$atom as PrimitiveSpec>::WGSL_BODY.expect("body"),
                params: <$atom as PrimitiveSpec>::PARAMS,
                inputs: $inputs,
                input_access: <$atom as PrimitiveSpec>::INPUT_ACCESS.to_vec(),
                node_inputs: <$atom as PrimitiveSpec>::INPUTS,
                node_outputs: <$atom as PrimitiveSpec>::OUTPUTS,
                node_includes: <$atom as PrimitiveSpec>::WGSL_INCLUDES,
                derived_uniforms: <$atom as PrimitiveSpec>::DERIVED_UNIFORMS,
                type_id: <$atom as PrimitiveSpec>::TYPE_ID.to_string(),
                derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            }
        };
    }
    let node = |n: u32| InputSource::Node(NodeInstanceId(n));
    let region = FusionRegion {
        nodes: vec![
            member!(0, JitterParticles, vec![InputSource::External(0)]),
            member!(
                1,
                SampleFacesAtParticles,
                vec![node(0), InputSource::External(1), InputSource::External(2)]
            ),
            member!(2, EnergyPotential, vec![node(1)]),
            member!(
                3,
                WavecrestPotential,
                vec![
                    node(1),
                    InputSource::External(3),
                    InputSource::External(4),
                    InputSource::External(5),
                    InputSource::External(2)
                ]
            ),
            member!(4, EmissionCount, vec![node(1), node(2), node(3)]),
        ],
        num_external_inputs: 6,
        outputs: vec![(NodeInstanceId(4), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(),
        camera_externals: 0,
        live_anchor: None,
        output_capacity: Some(CapacityExpr::Slot(0)),
    };
    let fused = generate_fused(&region).expect("the emitter chain fuses");
    assert!(
        naga::front::wgsl::parse_str(&fused.wgsl).is_ok(),
        "fused WGSL parses:\n{}",
        fused.wgsl
    );
    let pool_values = rig.pool_values();
    let lookup = |name: &str| {
        values
            .iter()
            .chain(&pool_values)
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v)
            .or(match name {
                "spray_speed" => Some(1.0),
                "dust" => Some(0.0),
                _ => None,
            })
    };
    let mut words: Vec<u32> = fused
        .param_order
        .iter()
        .map(|&(member, name)| {
            lookup(name)
                .unwrap_or_else(|| panic!("unexpected fused param {name} on {member:?}"))
                .to_bits()
        })
        .collect();
    while !words.len().is_multiple_of(4) {
        words.push(0);
    }
    let dst = harness.array::<u32>(&[], SLOTS);
    let pipeline =
        harness
            .device
            .create_compute_pipeline(&fused.wgsl, ENTRY, "whitewater-emitter-fused");
    let mut enc = harness.device.create_encoder("whitewater-emitter-fused");
    let externals = [&slot.1, &records.1, &tiles.1, &d.1, &k.1, &c.1];
    let mut bindings = vec![GpuBinding::Bytes {
        binding: 0,
        data: bytemuck::cast_slice(&words),
    }];
    for (i, buffer) in externals.iter().enumerate() {
        bindings.push(GpuBinding::Buffer {
            binding: i as u32 + 1,
            buffer,
            offset: 0,
        });
    }
    bindings.push(GpuBinding::Buffer {
        binding: externals.len() as u32 + 1,
        buffer: &dst.1,
        offset: 0,
    });
    enc.dispatch_compute(
        &pipeline,
        &bindings,
        [(SLOTS as u32).div_ceil(256), 1, 1],
        "whitewater-emitter-fused",
    );
    enc.commit_and_wait().expect("GPU work completes");
    let fused_out: Vec<u32> = read(&dst.1, SLOTS);

    let emission = Emission {
        dt: 0.1,
        rate: 175.0,
        points_per_cell: 2.0,
        ticks: 2.0,
        live_count: (SLOTS - 30) as f32,
    };
    let crest_limits = Crest {
        min_curvature: 0.4,
        max_curvature: 1.0,
        sharpness: 0.4,
    };
    let mut emitting = 0;
    for i in 0..SLOTS {
        assert_eq!(
            fused_out[i], unfused[i],
            "slot {i}: fused {} standalone {}",
            fused_out[i], unfused[i]
        );
        let p = cpu::sample_faces(
            cpu::jitter(input[i], i as u32, H, 7.25, 2.0),
            faces.arrays(),
            cell_dims(),
            &g,
        );
        let (w, margin) = cpu::wavecrest(p, &distance, &curvature, &kinds, &g, crest_limits);
        let (want, edge) = cpu::emission_count(p, cpu::energy(p, 0.1, 60.0), w, i as u32, emission);
        if margin >= 1e-4 && edge >= 1e-5 {
            assert_eq!(unfused[i], want, "slot {i}: standalone against the CPU");
        }
        emitting += usize::from(unfused[i] > 0);
    }
    assert!(emitting > 20, "{emitting} slots emit");
}

/// Emitter slots of the spawn proofs, and the spawn fields' draw.
const EMITTERS: usize = 300;

struct SpawnFixture {
    particles: Vec<FluidParticle>,
    energy: Vec<f32>,
    offsets: Vec<u32>,
    faces: Faces,
    solid: Vec<f32>,
    distance: Vec<f32>,
    kinds: Vec<u32>,
}

impl SpawnFixture {
    /// Emitters with 0 to 3 spawns each, well inside the box or in no stored
    /// tile, a solid lattice whose distance runs from half a cell inside a
    /// wall to two cells clear, and random grid fields.
    fn new(rng: &mut Rng) -> Self {
        let g = grid();
        let mut particles = particles(rng, 3.0, EMITTER_REACH);
        particles.truncate(EMITTERS);
        let energy: Vec<f32> = (0..EMITTERS).map(|_| rng.unit()).collect();
        let mut total = 0;
        let offsets = particles
            .iter()
            .map(|p| {
                total += if p.position_radius[3] > 0.0 {
                    (4.0 * rng.unit()) as u32
                } else {
                    0
                };
                total
            })
            .collect();
        let faces = Faces::new(rng, 2.0);
        let nodes = NODES.iter().map(|&n| n as usize).product::<usize>();
        let solid = (0..nodes).map(|_| (2.5 * rng.unit() - 0.5) * H).collect();
        let cells = g.cells.iter().map(|&n| n as usize).product::<usize>();
        let distance = (0..cells).map(|_| (4.0 * rng.unit() - 2.0) * H).collect();
        let kinds = (0..cells).map(|_| (3.0 * rng.unit()) as u32 % 3).collect();
        Self {
            particles,
            energy,
            offsets,
            faces,
            solid,
            distance,
            kinds,
        }
    }

    fn fields(&self) -> cpu::SpawnFields<'_> {
        cpu::SpawnFields {
            offsets: &self.offsets,
            particles: &self.particles,
            energy: &self.energy,
            faces: self.faces.arrays(),
            face_cells: cell_dims(),
            solid: &self.solid,
        }
    }

    fn total(&self) -> u32 {
        *self.offsets.last().expect("emitters")
    }
}

fn spawn_values(s: cpu::Spawn) -> Vec<(&'static str, f32)> {
    vec![
        ("capacity", s.capacity as f32),
        ("emitters", s.emitters as f32),
        ("seed", s.seed),
        ("epoch", s.epoch),
        ("min_lifetime", s.min_lifetime),
        ("max_lifetime", s.max_lifetime),
        ("lifetime_variance", s.variance),
        ("dt", s.dt),
    ]
}

fn spawn_settings(capacity: u32) -> cpu::Spawn {
    cpu::Spawn {
        dt: 1.0 / 60.0,
        capacity,
        emitters: EMITTERS as u32,
        seed: 3.5,
        epoch: 2.0,
        min_lifetime: 0.0,
        max_lifetime: 7.0,
        variance: 3.0,
    }
}

fn spawn_close(got: &WhitewaterSpawn, want: &WhitewaterSpawn, fixture: &SpawnFixture) -> bool {
    let g = grid();
    if want.position_lifetime[3] <= 0.0 {
        return got.position_lifetime[3] <= 0.0;
    }
    let placed =
        (0..3).all(|a| (got.position_lifetime[a] - want.position_lifetime[a]).abs() <= 2e-5);
    // The velocity is FLIP's MAC trilinear at the GPU's own position.
    let probe = FluidParticle {
        position_radius: [
            got.position_lifetime[0],
            got.position_lifetime[1],
            got.position_lifetime[2],
            1.0,
        ],
        velocity: [0.0; 3],
        id: 0,
    };
    let v = cpu::sample_faces(probe, fixture.faces.arrays(), cell_dims(), &g).velocity;
    placed
        && (got.position_lifetime[3] - want.position_lifetime[3]).abs() <= 1e-5
        && (0..3).all(|a| (got.velocity[a] - v[a]).abs() <= 1e-5)
}

/// Every slot below the frame's emission count gets its emitter's spawn,
/// placed, dropped and aged as the CPU does it, at a capacity above the
/// count and at one below it, where the slots take an even subset.
#[test]
fn spawn_whitewater_matches_cpu() {
    let fixture = SpawnFixture::new(&mut Rng(0x5a4e_0001));
    let total = fixture.total();
    assert!(total > 300, "{total} spawns");
    let g = grid();
    let rig = Rig::new(ORIGIN, NODES, H);
    let mut harness = Harness::new();
    let offsets = harness.array(&fixture.offsets, EMITTERS);
    let slot = harness.array(&fixture.particles, EMITTERS);
    let energy = harness.array(&fixture.energy, EMITTERS);
    let records = rig.cell_pool(&mut harness, &fixture.faces.records, FaceSample::default());
    let solid = rig.node_pool(&mut harness, &fixture.solid, CANONICAL_SOLID);
    let tiles = rig.tiles(&mut harness);
    for capacity in [total + 100, total / 3] {
        let settings = spawn_settings(capacity);
        let got: Vec<WhitewaterSpawn> = run(
            &mut harness,
            &mut SpawnWhitewater::new(),
            &[
                ("offsets", offsets.0),
                ("particles", slot.0),
                ("energy", energy.0),
                ("faces", records.0),
                ("solid", solid.0),
                ("tiles", tiles.0),
            ],
            capacity as usize,
            &rig.params(&spawn_values(settings)),
        );
        let (mut placed, mut dropped, mut edges) = (0, 0, 0);
        for (j, got) in got.iter().enumerate() {
            let (want, margin) = cpu::spawn(j as u32, &fixture.fields(), &g, settings);
            if margin < 1e-4 {
                edges += 1;
                continue;
            }
            assert!(
                spawn_close(got, &want, &fixture),
                "capacity {capacity} slot {j}: GPU {got:?} CPU {want:?}"
            );
            assert_eq!(got.kind, 0, "kind is left for node.whitewater_type");
            placed += usize::from(want.position_lifetime[3] > 0.0);
            dropped +=
                usize::from(want.position_lifetime[3] <= 0.0 && (j as u32) < total.min(capacity));
        }
        println!(
            "capacity {capacity}: {placed} placed, {dropped} dropped, {edges} on an edge of {total} emitted"
        );
        assert!(
            placed > capacity.min(total) as usize / 3 && dropped > 20 && edges < 10,
            "capacity {capacity}"
        );
        assert!(
            got[total.min(capacity) as usize..]
                .iter()
                .all(|s| s.position_lifetime[3] == 0.0),
            "unused slots are empty"
        );
    }
}

/// A stretched live interval lengthens the emitter cylinder by the accepted
/// duration. Compare every GPU spawn value with the CPU reference at 0.1 s.
#[test]
fn live_interval_whitewater_spawn_duration() {
    let fixture = SpawnFixture::new(&mut Rng(0x5a4e_0007));
    let capacity = fixture.total().min(512);
    let settings = cpu::Spawn {
        dt: 0.1,
        capacity,
        emitters: EMITTERS as u32,
        seed: 3.5,
        epoch: 2.0,
        min_lifetime: 1.0,
        max_lifetime: 1.0,
        variance: 0.0,
    };
    let g = grid();
    let rig = Rig::new(ORIGIN, NODES, H);
    let mut harness = Harness::new();
    let offsets = harness.array(&fixture.offsets, EMITTERS);
    let particles = harness.array(&fixture.particles, EMITTERS);
    let energy = harness.array(&fixture.energy, EMITTERS);
    let records = rig.cell_pool(&mut harness, &fixture.faces.records, FaceSample::default());
    let solid = rig.node_pool(&mut harness, &fixture.solid, CANONICAL_SOLID);
    let tiles = rig.tiles(&mut harness);
    let got: Vec<WhitewaterSpawn> = run(
        &mut harness,
        &mut SpawnWhitewater::new(),
        &[
            ("offsets", offsets.0),
            ("particles", particles.0),
            ("energy", energy.0),
            ("faces", records.0),
            ("solid", solid.0),
            ("tiles", tiles.0),
        ],
        capacity as usize,
        &rig.params(&spawn_values(settings)),
    );
    let fields = fixture.fields();
    let mut placed = 0;
    for (j, actual) in got.iter().enumerate() {
        let (expected, margin) = cpu::spawn(j as u32, &fields, &g, settings);
        if margin < 1e-4 {
            continue;
        }
        assert!(
            spawn_close(actual, &expected, &fixture),
            "slot {j}: duration-aware spawn GPU {actual:?}, CPU {expected:?}"
        );
        placed += usize::from(expected.position_lifetime[3] > 0.0);
    }
    assert!(
        placed > 100,
        "fixture must exercise stretched-duration spawn placement: {placed}"
    );
}

/// Spray in a cell no stored tile holds, then foam, bubble or spray by depth,
/// and no foam or spray away from air.
#[test]
fn whitewater_type_matches_cpu() {
    let mut rng = Rng(0x7e9e_0001);
    let g = grid();
    let rig = Rig::new(ORIGIN, NODES, H);
    let mut fixture = SpawnFixture::new(&mut rng);
    // Deep enough and closed enough that bubbles show: most cells liquid.
    fixture
        .distance
        .iter_mut()
        .for_each(|d| *d = (5.0 * rng.unit() - 3.0) * H);
    fixture
        .kinds
        .iter_mut()
        .for_each(|k| *k = if rng.unit() < 0.15 { 0 } else { 1 });
    let spawns: Vec<WhitewaterSpawn> = (0..SLOTS)
        .map(|_| {
            let p = position(&mut rng, REACH);
            let lifetime = if rng.unit() < 0.1 {
                0.0
            } else {
                1.0 + rng.unit()
            };
            WhitewaterSpawn {
                position_lifetime: [p[0], p[1], p[2], lifetime],
                velocity: [0.5, 0.0, 0.0],
                kind: 0,
            }
        })
        .collect();
    let mut harness = Harness::new();
    let slot = harness.array(&spawns, SLOTS);
    let d = rig.cell_pool(&mut harness, &fixture.distance, canonical_distance(H));
    let c = rig.cell_pool(&mut harness, &fixture.kinds, CELL_AIR);
    let tiles = rig.tiles(&mut harness);
    let got: Vec<WhitewaterSpawn> = run(
        &mut harness,
        &mut WhitewaterType::new(),
        &[
            ("spawns", slot.0),
            ("distance", d.0),
            ("cells", c.0),
            ("tiles", tiles.0),
        ],
        SLOTS,
        &rig.params(&[]),
    );
    let mut seen = [0; 3];
    for (i, (got, spawn)) in got.iter().zip(&spawns).enumerate() {
        assert_eq!(
            (got.position_lifetime, got.velocity),
            (spawn.position_lifetime, spawn.velocity),
            "slot {i}"
        );
        let (want, margin) = cpu::kind(*spawn, &fixture.distance, &fixture.kinds, &g);
        if margin < 1e-4 {
            continue;
        }
        assert_eq!(got.kind, want, "slot {i}: {spawn:?}");
        if spawn.position_lifetime[3] > 0.0 {
            seen[want as usize] += 1;
        }
    }
    assert!(
        seen.iter().all(|&n| n > 30),
        "bubble, foam, spray: {seen:?}"
    );
}

/// Spawn and type folded into one kernel, as `whitewater_spawn_chain_fuses`
/// finds them: every array gathered, the spawn threaded to the type in a
/// register, the slots counted from Capacity. Bit for bit the atoms run one
/// by one, and those match the CPU.
#[test]
fn whitewater_spawn_chain_fused_matches_unfused() {
    use manifold_gpu::GpuBinding;
    use manifold_node_engine::exec::effect_node::NodeInstanceId;
    use manifold_node_engine::freeze::classify::CapacityExpr;
    use manifold_node_engine::freeze::codegen::{
        ENTRY, FusionRegion, InputSource, RegionNode, generate_fused,
    };
    use manifold_node_engine::primitive::PrimitiveSpec;

    let fixture = SpawnFixture::new(&mut Rng(0xf05e_0003));
    let g = grid();
    let rig = Rig::new(ORIGIN, NODES, H);
    let capacity = fixture.total() + 64;
    let settings = cpu::Spawn {
        dt: 0.1,
        ..spawn_settings(capacity)
    };
    let values = spawn_values(settings);
    let all = rig.params(&values);
    let mut harness = Harness::new();
    let offsets = harness.array(&fixture.offsets, EMITTERS);
    let slot = harness.array(&fixture.particles, EMITTERS);
    let energy = harness.array(&fixture.energy, EMITTERS);
    let records = rig.cell_pool(&mut harness, &fixture.faces.records, FaceSample::default());
    let solid = rig.node_pool(&mut harness, &fixture.solid, CANONICAL_SOLID);
    let d = rig.cell_pool(&mut harness, &fixture.distance, canonical_distance(H));
    let c = rig.cell_pool(&mut harness, &fixture.kinds, CELL_AIR);
    let tiles = rig.tiles(&mut harness);
    let spawned: Vec<WhitewaterSpawn> = run(
        &mut harness,
        &mut SpawnWhitewater::new(),
        &[
            ("offsets", offsets.0),
            ("particles", slot.0),
            ("energy", energy.0),
            ("faces", records.0),
            ("solid", solid.0),
            ("tiles", tiles.0),
        ],
        capacity as usize,
        &all,
    );
    let spawned_in = harness.array(&spawned, capacity as usize);
    let unfused: Vec<WhitewaterSpawn> = run(
        &mut harness,
        &mut WhitewaterType::new(),
        &[
            ("spawns", spawned_in.0),
            ("distance", d.0),
            ("cells", c.0),
            ("tiles", tiles.0),
        ],
        capacity as usize,
        &all,
    );

    macro_rules! member {
        ($n:expr, $atom:ty, $inputs:expr) => {
            RegionNode {
                node_id: NodeInstanceId($n),
                fusion_kind: <$atom as PrimitiveSpec>::FUSION_KIND,
                body: <$atom as PrimitiveSpec>::WGSL_BODY.expect("body"),
                params: <$atom as PrimitiveSpec>::PARAMS,
                inputs: $inputs,
                input_access: <$atom as PrimitiveSpec>::INPUT_ACCESS.to_vec(),
                node_inputs: <$atom as PrimitiveSpec>::INPUTS,
                node_outputs: <$atom as PrimitiveSpec>::OUTPUTS,
                node_includes: <$atom as PrimitiveSpec>::WGSL_INCLUDES,
                derived_uniforms: <$atom as PrimitiveSpec>::DERIVED_UNIFORMS,
                type_id: <$atom as PrimitiveSpec>::TYPE_ID.to_string(),
                derived_camera_ext: None,
                output_storage: "rgba16float",
                stencil_fetch: false,
                quantize_f16: false,
            }
        };
    }
    let external = InputSource::External;
    let region = FusionRegion {
        nodes: vec![
            member!(0, SpawnWhitewater, (0..6).map(external).collect()),
            member!(
                1,
                WhitewaterType,
                vec![
                    InputSource::Node(NodeInstanceId(0)),
                    external(6),
                    external(7),
                    external(5)
                ]
            ),
        ],
        num_external_inputs: 8,
        outputs: vec![(NodeInstanceId(1), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(),
        camera_externals: 0,
        live_anchor: None,
        output_capacity: Some(CapacityExpr::Product(vec![CapacityExpr::Param(
            "n0_capacity".to_string(),
        )])),
    };
    let fused = generate_fused(&region).expect("the spawn chain fuses");
    assert!(
        naga::front::wgsl::parse_str(&fused.wgsl).is_ok(),
        "fused WGSL parses:\n{}",
        fused.wgsl
    );
    let pool_values = rig.pool_values();
    let lookup = |name: &str| {
        values
            .iter()
            .chain(&pool_values)
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v)
            .or(match name {
                "spray_speed" => Some(1.0),
                "dust" => Some(0.0),
                _ => None,
            })
    };
    let mut words: Vec<u32> = fused
        .param_order
        .iter()
        .map(|&(member, name)| {
            lookup(name)
                .unwrap_or_else(|| panic!("unexpected fused param {name} on {member:?}"))
                .to_bits()
        })
        .collect();
    while !words.len().is_multiple_of(4) {
        words.push(0);
    }
    let dst = harness.array::<WhitewaterSpawn>(&[], capacity as usize);
    let pipeline =
        harness
            .device
            .create_compute_pipeline(&fused.wgsl, ENTRY, "whitewater-spawn-fused");
    let mut enc = harness.device.create_encoder("whitewater-spawn-fused");
    let externals = [
        &offsets.1, &slot.1, &energy.1, &records.1, &solid.1, &tiles.1, &d.1, &c.1,
    ];
    let mut bindings = vec![GpuBinding::Bytes {
        binding: 0,
        data: bytemuck::cast_slice(&words),
    }];
    for (i, buffer) in externals.iter().enumerate() {
        bindings.push(GpuBinding::Buffer {
            binding: i as u32 + 1,
            buffer,
            offset: 0,
        });
    }
    bindings.push(GpuBinding::Buffer {
        binding: externals.len() as u32 + 1,
        buffer: &dst.1,
        offset: 0,
    });
    enc.dispatch_compute(
        &pipeline,
        &bindings,
        [capacity.div_ceil(256), 1, 1],
        "whitewater-spawn-fused",
    );
    enc.commit_and_wait().expect("GPU work completes");
    let fused_out: Vec<WhitewaterSpawn> = read(&dst.1, capacity as usize);

    let mut typed = [0; 3];
    for j in 0..capacity as usize {
        assert_eq!(
            bytemuck::bytes_of(&fused_out[j]),
            bytemuck::bytes_of(&unfused[j]),
            "slot {j}: fused {:?} standalone {:?}",
            fused_out[j],
            unfused[j]
        );
        let (want, margin) = cpu::spawn(j as u32, &fixture.fields(), &g, settings);
        if margin < 1e-4 || want.position_lifetime[3] <= 0.0 {
            continue;
        }
        assert!(
            spawn_close(&unfused[j], &want, &fixture),
            "slot {j}: standalone {:?} CPU {want:?}",
            unfused[j]
        );
        let (kind, edge) = cpu::kind(unfused[j], &fixture.distance, &fixture.kinds, &g);
        if edge >= 1e-4 {
            assert_eq!(unfused[j].kind, kind, "slot {j}");
            typed[kind as usize] += 1;
        }
    }
    assert!(typed.iter().sum::<usize>() > 100, "{typed:?} typed spawns");
}
