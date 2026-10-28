//! GPU value proofs for the whitewater pool atoms against their CPU
//! statements (`whitewater_pool_cpu`), and each atom folded by the codegen
//! against its standalone kernel. The CPU fixture's tank sits `MARGIN` cells
//! inside a larger box of world tiles, placed so the tank crosses tiles -1
//! and 0 on x, and the margin is solid: no particle reaches the box rule of
//! the CPU statements, which the pooled atoms do not have (a wall is a
//! solid there).

use super::advect_whitewater::AdvectWhitewater;
use super::age_whitewater::AgeWhitewater;
use super::preserve_foam::PreserveFoam;
use super::retype_whitewater::RetypeWhitewater;
use super::whitewater_cpu::{CANONICAL_SOLID, Rng, canonical_distance};
use super::whitewater_particle_cpu::{Box3, face_index};
use super::whitewater_pool_cpu::fixture::{FACE_CELLS, H, NODES, ORIGIN, faces, pool, tank};
use manifold_gpu::GpuBuffer;
use manifold_node_engine::bindings::Slot;
use manifold_node_engine::exec::effect_node::NodeInstanceId;
use manifold_node_engine::freeze::codegen::InputSource;
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::ports::KnownItem;
use manifold_node_engine::scene::world_tiles::{lattice_origin, pool_len};
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_node_engine::testkit::fused_atoms::{fused, member, run};
use manifold_water_liquid::fluid_particles::{CellRange, FaceSample};
use manifold_water_liquid::primitives::prefix_scan::ScanLabels;
use manifold_water_liquid::primitives::sort_particles_into_cells::{
    LIQUID_PARTICLE_READ, ParticleSorter, RecordRead, SortCount, SortJob, SortLabels,
    whitewater_record_read,
};
use manifold_water_liquid::testkit::world_box::BoxTable;
use manifold_water_liquid::whitewater::{CELL_AIR, WHITEWATER_EMPTY, WhitewaterParticle};
use {
    super::whitewater_pool_cpu::Advect, super::whitewater_pool_cpu::Age,
    super::whitewater_pool_cpu::DEAD, super::whitewater_pool_cpu::Fields,
    super::whitewater_pool_cpu::Preserve, crate::primitives::whitewater_pool_cpu as cpu,
};

const SLOTS: usize = 4000;
/// Cells of solid round the tank in the world box, past the reach of the
/// CPU statements' boundary box (1.625 cells in from the box's sides).
const MARGIN: u32 = 4;
/// The world cell of the world box's first cell: the tank starts `MARGIN`
/// cells on, so it crosses tiles -1 and 0 on x and has its y and z in
/// positive and negative cells.
const WORLD_ORIGIN: [i32; 3] = [-27, 3, -6];

const LABELS: SortLabels = SortLabels {
    clear: "whitewater.sort.clear",
    count: "whitewater.sort.count",
    scan: ScanLabels {
        blocks: "whitewater.sort.scan.blocks",
        add: "whitewater.sort.scan.add",
    },
    ranges: "whitewater.sort.ranges",
    tail: "whitewater.sort.tail",
    scatter: "whitewater.sort.scatter",
    stabilise: "whitewater.sort.stabilise",
};

/// The scene position of the world box's first node, the low corner of its
/// first cell.
fn first_node() -> [f32; 3] {
    WORLD_ORIGIN.map(|c| (c as f32 - 0.5) * H)
}

/// The world box: the tank and its margin as pooled arrays under one table,
/// and the same box as the dense grid the CPU statements read.
struct World {
    /// Node arrays (the solid, the face records).
    nodes: BoxTable,
    /// Cell arrays, on the same tiles.
    cells: BoxTable,
    grid: Box3,
}

impl World {
    fn new() -> Self {
        let cells = NODES.map(|n| n - 1 + 2 * MARGIN);
        let nodes = BoxTable::new(WORLD_ORIGIN, cells.map(|n| n + 1));
        let size = cells.map(|n| n as f32 * H);
        let first = first_node();
        Self {
            cells: nodes.with_cells(cells),
            grid: Box3 {
                cells,
                center: std::array::from_fn(|a| first[a] + 0.5 * size[a]),
                size,
            },
            nodes,
        }
    }

    /// The tank's low corner: where the fixture's origin lands.
    fn tank_low(&self) -> [f32; 3] {
        WORLD_ORIGIN.map(|c| ((c + MARGIN as i32) as f32 - 0.5) * H)
    }

    /// Moves a fixture pool, built round `ORIGIN`, into the tank.
    fn shift(&self, pool: &mut [WhitewaterParticle]) {
        let low = self.tank_low();
        for p in pool {
            for (a, x) in p.position_lifetime[..3].iter_mut().enumerate() {
                *x += low[a] - ORIGIN[a];
            }
        }
    }

    /// The CPU fixture's tank over the world box, nodes counted from its
    /// first: the walls one cell thick and the ball in place, the margin
    /// solid all the way out.
    fn solid(&self) -> Vec<f32> {
        let tank_cells = NODES.map(|n| n - 1);
        let ball: [f32; 3] = std::array::from_fn(|a| 0.45 * tank_cells[a] as f32 * H);
        let [nx, ny, nz] = self.nodes.cells;
        let mut solid = Vec::with_capacity((nx * ny * nz) as usize);
        for k in 0..nz {
            for j in 0..ny {
                for i in 0..nx {
                    let x = [i, j, k].map(|n| (n as i32 - MARGIN as i32) as f32 * H);
                    let wall = (0..3)
                        .map(|a| (x[a] - H).min(tank_cells[a] as f32 * H - H - x[a]))
                        .fold(f32::INFINITY, f32::min);
                    let d: [f32; 3] = std::array::from_fn(|a| x[a] - ball[a]);
                    solid
                        .push(wall.min((d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() - 3.0 * H));
                }
            }
        }
        let fixture = tank();
        for k in 0..NODES[2] {
            for j in 0..NODES[1] {
                for i in 0..NODES[0] {
                    let at = [i + MARGIN, j + MARGIN, k + MARGIN];
                    assert_eq!(
                        solid[(at[0] + nx * (at[1] + ny * at[2])) as usize],
                        fixture[(i + NODES[0] * (j + NODES[1] * k)) as usize],
                        "the tank's node {:?}",
                        [i, j, k]
                    );
                }
            }
        }
        solid
    }

    /// One record per node of the world box, the fixture's faces `MARGIN`
    /// cells in, as the seam places them: a face has weight where the
    /// fixture holds it.
    fn face_records(&self, faces: &[Vec<f32>; 3]) -> Vec<FaceSample> {
        let pad = (self.grid.cells[0] as i32 - FACE_CELLS[0] as i32) / 2;
        let [nx, ny, nz] = self.nodes.cells;
        let mut records = Vec::with_capacity((nx * ny * nz) as usize);
        for z in 0..nz as i32 {
            for y in 0..ny as i32 {
                for x in 0..nx as i32 {
                    let mut record = FaceSample::default();
                    for (axis, axis_faces) in faces.iter().enumerate() {
                        if let Some(i) = face_index([x, y, z], axis, pad, FACE_CELLS) {
                            record.velocity[axis] = axis_faces[i];
                            record.weight[axis] = 1.0;
                        }
                    }
                    records.push(record);
                }
            }
        }
        records
    }

    /// The `cell_size` and `pool_tiles` params of every pooled atom.
    fn pool_values(&self) -> [(&'static str, f32); 2] {
        [
            ("cell_size", H),
            ("pool_tiles", self.nodes.pool_tiles as f32),
        ]
    }

    /// The table words the atoms read the pools through.
    fn tiles(&self, harness: &mut Harness) -> (Slot, GpuBuffer) {
        harness.array(&self.nodes.words, self.nodes.words.len())
    }

    /// A cell array over the world box's cells (x fastest), `canon` elsewhere.
    fn cell_pool<T: KnownItem + Copy>(
        &self,
        harness: &mut Harness,
        dense: &[T],
        canon: T,
    ) -> (Slot, GpuBuffer) {
        let pool = self.cells.to_pool(dense, canon);
        harness.array(&pool, pool.len())
    }

    /// A node array over the world box's nodes (x fastest), `canon` elsewhere.
    fn node_pool<T: KnownItem + Copy>(
        &self,
        harness: &mut Harness,
        dense: &[T],
        canon: T,
    ) -> (Slot, GpuBuffer) {
        let pool = self.nodes.to_pool(dense, canon);
        harness.array(&pool, pool.len())
    }
}

fn advect_values(s: Advect, world: &World) -> Vec<(&'static str, f32)> {
    let low = first_node();
    let [cell_size, pool_tiles] = world.pool_values();
    vec![
        ("bounds_lo_x", low[0]),
        ("bounds_lo_y", low[1]),
        ("bounds_lo_z", low[2]),
        ("gravity_x", s.gravity[0]),
        ("gravity_y", s.gravity[1]),
        ("gravity_z", s.gravity[2]),
        ("dt", s.dt),
        ("foam_advection", s.foam_advection),
        ("bubble_buoyancy", s.bubble_buoyancy),
        ("bubble_drag", s.bubble_drag),
        ("spray_drag", s.spray_drag),
        ("spray_drag_variance", s.spray_drag_variance),
        ("spray_restitution", s.spray_restitution),
        ("spray_friction", s.spray_friction),
        ("substep_count", 0.0),
        ("field_nodes_x", 2.0),
        ("field_nodes_y", 2.0),
        ("field_nodes_z", 2.0),
        ("field_spacing", 0.25),
        ("force_lattices", 0.0),
        ("tick_index", 0.0),
        ("first_tick", 0.0),
        cell_size,
        pool_tiles,
    ]
}

/// Drag, friction and a sideways gravity on, so every term shows.
fn settings() -> Advect {
    Advect {
        gravity: [0.7, -9.81, -0.4],
        spray_drag: 0.5,
        spray_friction: 0.1,
        ..Advect::flip()
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 2e-5 * b.abs().max(1.0)
}

/// A velocity read from the faces is a weighted sum of values up to the
/// face speed, so its rounding scales with that speed, not with the result.
fn velocity_close(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-5 * (5.0 * H * 60.0).max(b.abs())
}

struct Fixture {
    world: World,
    /// The solid nodes of the world box.
    solid: Vec<f32>,
    /// The fixture's faces, for the CPU statements.
    faces: [Vec<f32>; 3],
    /// The same faces as one record per node of the world box.
    records: Vec<FaceSample>,
    pool: Vec<WhitewaterParticle>,
}

impl Fixture {
    fn new(seed: u64) -> Self {
        let world = World::new();
        let mut rng = Rng(seed);
        let speed = 5.0 * H * 60.0;
        let faces = faces(&mut rng, speed / 3f32.sqrt());
        let mut pool = pool(&mut rng, SLOTS, speed);
        world.shift(&mut pool);
        Self {
            solid: world.solid(),
            records: world.face_records(&faces),
            world,
            faces,
            pool,
        }
    }

    fn fields(&self) -> Fields<'_> {
        Fields {
            faces: self.faces.each_ref().map(Vec::as_slice),
            face_cells: FACE_CELLS,
            solid: &self.solid,
        }
    }

    fn advect(
        &self,
        harness: &mut Harness,
        values: &[(&'static str, f32)],
    ) -> Vec<WhitewaterParticle> {
        let pool = harness.array(&self.pool, SLOTS);
        let faces = self
            .world
            .node_pool(harness, &self.records, FaceSample::default());
        let solid = self.world.node_pool(harness, &self.solid, CANONICAL_SOLID);
        let tiles = self.world.tiles(harness);
        run(
            harness,
            &mut AdvectWhitewater::new(),
            &[
                ("pool", pool.0),
                ("faces", faces.0),
                ("solid", solid.0),
                ("tiles", tiles.0),
            ],
            SLOTS,
            &params(values),
        )
    }
}

/// Every type moved, collided and killed as the CPU does it.
#[test]
fn advect_whitewater_matches_cpu() {
    let fixture = Fixture::new(0xad7e_0001);
    let s = settings();
    let mut harness = Harness::new();
    let got = fixture.advect(&mut harness, &advect_values(s, &fixture.world));
    let g = fixture.world.grid;
    let (mut checked, mut edges, mut killed, mut bounced) = (0, 0, 0, 0);
    let mut kinds = [0; 3];
    for (i, (got, particle)) in got.iter().zip(&fixture.pool).enumerate() {
        let (want, margin) = cpu::advect(*particle, &fixture.fields(), &g, s, None);
        if margin < 1e-5 {
            edges += 1;
            continue;
        }
        assert_eq!((got.kind, got.id), (particle.kind, particle.id), "slot {i}");
        assert_eq!(
            got.position_lifetime[3], want.position_lifetime[3],
            "slot {i}: GPU {got:?} CPU {want:?} from {particle:?}"
        );
        let near = got.position_lifetime[..3]
            .iter()
            .zip(&want.position_lifetime[..3])
            .all(|(&a, &b)| close(a, b));
        let moving = got
            .velocity
            .iter()
            .zip(&want.velocity)
            .all(|(&a, &b)| velocity_close(a, b));
        assert!(
            near && moving,
            "slot {i}: GPU {got:?} CPU {want:?} from {particle:?}"
        );
        if particle.kind < 3 {
            checked += 1;
            kinds[particle.kind as usize] += 1;
            killed += usize::from(want.position_lifetime[3] == DEAD);
            let free: [f32; 3] =
                std::array::from_fn(|a| particle.position_lifetime[a] + want.velocity[a] * s.dt);
            bounced += usize::from(
                particle.kind == 1
                    && (0..3).any(|a| (free[a] - want.position_lifetime[a]).abs() > 1e-4),
            );
        }
    }
    println!(
        "{checked} checked {kinds:?}, {edges} on an edge, {killed} killed, {bounced} foam stopped by a solid"
    );
    assert!(kinds.iter().all(|&n| n > 500) && edges < SLOTS / 50 && killed > 50 && bounced > 50);
}

/// The advect folded by the codegen alone in its region, bit for bit the
/// standalone kernel.
#[test]
fn advect_whitewater_fused_matches_unfused() {
    let fixture = Fixture::new(0xad7e_0002);
    let values = advect_values(settings(), &fixture.world);
    let mut harness = Harness::new();
    let unfused = fixture.advect(&mut harness, &values);

    let pool = harness.array(&fixture.pool, SLOTS);
    let faces = fixture
        .world
        .node_pool(&mut harness, &fixture.records, FaceSample::default());
    let solid = fixture
        .world
        .node_pool(&mut harness, &fixture.solid, CANONICAL_SOLID);
    let tiles = fixture.world.tiles(&mut harness);
    let external = InputSource::External;
    // Pool, faces, solid, the four optional arrays unwired (any array
    // satisfies them: the substep count is 0), tiles.
    let inputs = [0, 1, 2, 2, 2, 2, 2, 3].map(external).to_vec();
    let fused_out: Vec<WhitewaterParticle> = fused(
        &mut harness,
        vec![member::<AdvectWhitewater>(0, inputs)],
        &[&pool.1, &faces.1, &solid.1, &tiles.1],
        SLOTS,
        &values,
    );
    for j in 0..SLOTS {
        assert_eq!(
            bytemuck::bytes_of(&fused_out[j]),
            bytemuck::bytes_of(&unfused[j]),
            "slot {j}: fused {:?} standalone {:?}",
            fused_out[j],
            unfused[j]
        );
    }
    assert!(
        unfused
            .iter()
            .zip(&fixture.pool)
            .filter(|(a, b)| a != b)
            .count()
            > SLOTS / 2,
        "the advect moved the pool"
    );
}

fn age_values(s: Age) -> Vec<(&'static str, f32)> {
    vec![
        ("dt", s.dt),
        ("bubble_lifetime_modifier", s.bubble),
        ("foam_lifetime_modifier", s.foam),
        ("spray_lifetime_modifier", s.spray),
    ]
}

/// The surface height in cells at tank cell column (x, z).
fn level(x: f32, z: f32) -> f32 {
    0.5 * (NODES[1] - 1) as f32 + 4.0 * (0.3 * x).sin() + 3.0 * (0.2 * z).cos()
}

/// A rolling liquid surface sampled at the world box's cell centres
/// (negative in the liquid, metres), and its cell types: liquid below, air
/// above, with a third of the air cells walled off as solid so some foam and
/// spray sit with no air around them. The surface continues over the margin.
fn surface(world: &World, rng: &mut Rng) -> (Vec<f32>, Vec<u32>) {
    let [nx, ny, nz] = world.cells.cells;
    let total = (nx * ny * nz) as usize;
    let (mut distance, mut cells) = (Vec::with_capacity(total), Vec::with_capacity(total));
    // Cell n of the world box, counted in tank cells.
    let rel = |n: u32| (n as i32 - MARGIN as i32) as f32;
    for k in 0..nz {
        for j in 0..ny {
            for i in 0..nx {
                let d = (rel(j) + 0.5 - level(rel(i), rel(k))) * H;
                distance.push(d);
                cells.push(if d < 0.0 {
                    1
                } else if rng.unit() < 0.33 {
                    2
                } else {
                    0
                });
            }
        }
    }
    (distance, cells)
}

/// Moves every particle to within six cells of the surface, so every branch
/// of the retype fires.
fn near_surface(world: &World, rng: &mut Rng, pool: &mut [WhitewaterParticle]) {
    let low = world.tank_low();
    let cells = NODES.map(|n| n - 1);
    for p in pool.iter_mut() {
        let x = cells[0] as f32 * rng.unit();
        let z = cells[2] as f32 * rng.unit();
        let y = level(x - 0.5, z - 0.5) + 6.0 * (2.0 * rng.unit() - 1.0);
        p.position_lifetime[0] = low[0] + x * H;
        p.position_lifetime[1] = low[1] + y * H;
        p.position_lifetime[2] = low[2] + z * H;
    }
}

struct Typing {
    base: Fixture,
    distance: Vec<f32>,
    cells: Vec<u32>,
}

impl Typing {
    fn new(seed: u64) -> Self {
        let mut base = Fixture::new(seed);
        let mut rng = Rng(seed ^ 0x5eed);
        near_surface(&base.world, &mut rng, &mut base.pool);
        let (distance, cells) = surface(&base.world, &mut rng);
        Self {
            base,
            distance,
            cells,
        }
    }

    fn retype(
        &self,
        harness: &mut Harness,
        pool: &[WhitewaterParticle],
    ) -> Vec<WhitewaterParticle> {
        let world = &self.base.world;
        let pool = harness.array(pool, SLOTS);
        let faces = world.node_pool(harness, &self.base.records, FaceSample::default());
        let distance = world.cell_pool(harness, &self.distance, canonical_distance(H));
        let cells = world.cell_pool(harness, &self.cells, CELL_AIR);
        let tiles = world.tiles(harness);
        run(
            harness,
            &mut RetypeWhitewater::new(),
            &[
                ("pool", pool.0),
                ("distance", distance.0),
                ("cells", cells.0),
                ("faces", faces.0),
                ("tiles", tiles.0),
            ],
            SLOTS,
            &params(&world.pool_values()),
        )
    }
}

/// Every retype branch as the CPU decides it, the foam buffer and the
/// bubble's velocity pickup included.
#[test]
fn retype_whitewater_matches_cpu() {
    let fixture = Typing::new(0x2e7f_0001);
    let mut harness = Harness::new();
    let got = fixture.retype(&mut harness, &fixture.base.pool);
    let g = fixture.base.world.grid;
    let fields = fixture.base.fields();
    let (mut edges, mut buffered, mut picked) = (0, 0, 0);
    let mut moves = [[0; 3]; 3];
    for (i, (got, particle)) in got.iter().zip(&fixture.base.pool).enumerate() {
        let (want, margin) = cpu::retype(*particle, &fields, &fixture.distance, &fixture.cells, &g);
        if margin < 1e-4 {
            edges += 1;
            continue;
        }
        assert_eq!(
            (got.kind, got.id, got.position_lifetime),
            (want.kind, want.id, want.position_lifetime),
            "slot {i}: GPU {got:?} CPU {want:?}"
        );
        assert!(
            got.velocity
                .iter()
                .zip(&want.velocity)
                .all(|(&a, &b)| velocity_close(a, b)),
            "slot {i}: GPU {got:?} CPU {want:?}"
        );
        if particle.kind < 3 {
            moves[particle.kind as usize][want.kind as usize] += 1;
            picked += usize::from(particle.kind == 0 && want.kind != 0);
            if particle.kind == 1 && want.kind == 1 {
                let fresh = cpu::retype(
                    WhitewaterParticle {
                        kind: 2,
                        ..*particle
                    },
                    &fields,
                    &fixture.distance,
                    &fixture.cells,
                    &g,
                )
                .0;
                buffered += usize::from(fresh.kind == 0);
            }
        }
    }
    println!(
        "{moves:?} (from row to column), {edges} on an edge, {buffered} kept as foam by the buffer, {picked} bubbles took the liquid velocity"
    );
    assert!(edges < SLOTS / 20 && buffered > 20 && picked > 100);
    assert!(
        moves.iter().flatten().all(|&n| n > 10),
        "every type change happens: {moves:?}"
    );
}

/// Each type ages at its own rate.
#[test]
fn age_whitewater_matches_cpu() {
    let fixture = Fixture::new(0xa9e0_0001);
    let s = Age {
        dt: 1.0 / 50.0,
        bubble: 0.25,
        foam: 1.5,
        spray: 3.0,
    };
    let mut harness = Harness::new();
    let pool = harness.array(&fixture.pool, SLOTS);
    let got: Vec<WhitewaterParticle> = run(
        &mut harness,
        &mut AgeWhitewater::new(),
        &[("pool", pool.0)],
        SLOTS,
        &params(&age_values(s)),
    );
    for (i, (got, particle)) in got.iter().zip(&fixture.pool).enumerate() {
        let want = cpu::age(*particle, s);
        assert_eq!(
            bytemuck::bytes_of(got),
            bytemuck::bytes_of(&want),
            "slot {i}: GPU {got:?} CPU {want:?}"
        );
    }
    assert!(
        got.iter()
            .zip(&fixture.pool)
            .filter(|(a, b)| a != b)
            .count()
            > SLOTS / 2,
        "the age changed the pool"
    );
}

/// Advect, retype and age folded into one kernel by the codegen, bit for
/// bit the three standalone kernels in a row.
#[test]
fn whitewater_tick_fused_matches_unfused() {
    let fixture = Typing::new(0x71c4_0001);
    let world = &fixture.base.world;
    let s = settings();
    let a = Age::flip();
    let mut harness = Harness::new();
    let advect = advect_values(s, world);
    let advected = fixture.base.advect(&mut harness, &advect);
    let retyped = fixture.retype(&mut harness, &advected);
    let pool_in = harness.array(&retyped, SLOTS);
    let unfused: Vec<WhitewaterParticle> = run(
        &mut harness,
        &mut AgeWhitewater::new(),
        &[("pool", pool_in.0)],
        SLOTS,
        &params(&age_values(a)),
    );

    let pool = harness.array(&fixture.base.pool, SLOTS);
    let faces = world.node_pool(&mut harness, &fixture.base.records, FaceSample::default());
    let solid = world.node_pool(&mut harness, &fixture.base.solid, CANONICAL_SOLID);
    let tiles = world.tiles(&mut harness);
    let distance = world.cell_pool(&mut harness, &fixture.distance, canonical_distance(H));
    let cells = world.cell_pool(&mut harness, &fixture.cells, CELL_AIR);
    let external = InputSource::External;
    let node = |n: u32| InputSource::Node(NodeInstanceId(n));
    // The advect and the age both step by `dt`: the fused kernel takes one
    // value for both.
    assert_eq!(s.dt, a.dt);
    let values = [advect, age_values(a)].concat();
    let fused_out: Vec<WhitewaterParticle> = fused(
        &mut harness,
        vec![
            member::<AdvectWhitewater>(0, [0, 1, 2, 2, 2, 2, 2, 3].map(external).to_vec()),
            member::<RetypeWhitewater>(
                1,
                vec![node(0), external(4), external(5), external(1), external(3)],
            ),
            member::<AgeWhitewater>(2, vec![node(1)]),
        ],
        &[&pool.1, &faces.1, &solid.1, &tiles.1, &distance.1, &cells.1],
        SLOTS,
        &values,
    );
    for j in 0..SLOTS {
        assert_eq!(
            bytemuck::bytes_of(&fused_out[j]),
            bytemuck::bytes_of(&unfused[j]),
            "slot {j}: fused {:?} standalone {:?}",
            fused_out[j],
            unfused[j]
        );
    }
    let changed = advected
        .iter()
        .zip(&retyped)
        .filter(|(a, b)| a.kind != b.kind)
        .count();
    assert!(changed > SLOTS / 20, "{changed} retyped");
}

/// Foam crowded into 70 cells, cell k holding k foam particles (a quarter of
/// them dead, which FLIP still counts), plus bubbles, spray, empty slots and
/// foam with no finite position, shuffled. Every position sits clear of its
/// cell's faces, so the sort's bin and FLIP's cell agree.
fn crowded(world: &World, rng: &mut Rng) -> Vec<WhitewaterParticle> {
    let cells = NODES.map(|n| n - 1);
    let low = world.tank_low();
    let mut pool = Vec::new();
    let add = |pool: &mut Vec<WhitewaterParticle>,
               rng: &mut Rng,
               cell: [u32; 3],
               kind: u32,
               lifetime: f32| {
        let p: [f32; 3] =
            std::array::from_fn(|a| low[a] + (cell[a] as f32 + 0.02 + 0.96 * rng.unit()) * H);
        pool.push(WhitewaterParticle {
            position_lifetime: [p[0], p[1], p[2], lifetime],
            kind,
            id: pool.len() as u32 % 256,
            ..Default::default()
        });
    };
    for k in 0..70u32 {
        let cell: [u32; 3] =
            std::array::from_fn(|a| (rng.unit() * cells[a] as f32) as u32 % cells[a]);
        for n in 0..k {
            let lifetime = if n % 4 == 3 { -0.01 } else { 0.2 + rng.unit() };
            add(&mut pool, rng, cell, 1, lifetime);
        }
        for kind in [0, 0, 0, 2, 2, WHITEWATER_EMPTY] {
            add(&mut pool, rng, cell, kind, 0.5);
        }
    }
    for _ in 0..5 {
        add(&mut pool, rng, [1, 1, 1], 1, 0.5);
        pool.last_mut().expect("added").position_lifetime[1] = f32::NAN;
    }
    for i in (1..pool.len()).rev() {
        let j = (rng.unit() * (i + 1) as f32) as usize % (i + 1);
        pool.swap(i, j);
    }
    pool
}

fn preserve_values(world: &World, s: Preserve, enabled: bool) -> Vec<(&'static str, f32)> {
    let [cell_size, pool_tiles] = world.pool_values();
    vec![
        ("enabled", if enabled { 1.0 } else { 0.0 }),
        ("dt", s.dt),
        ("rate", s.rate),
        ("min_density", s.min_density),
        ("max_density", s.max_density),
        cell_size,
        pool_tiles,
    ]
}

/// A pool and its bins from the pooled sort over the world box.
struct Binned {
    particles: (Slot, GpuBuffer),
    ranges: (Slot, GpuBuffer),
    order: (Slot, GpuBuffer),
}

/// `particles` sorted into the world box's pool elements: the ranges (one
/// per element, the canonical element last) and the order of the slots.
fn sort_into_world<T: KnownItem + bytemuck::Pod>(
    harness: &mut Harness,
    world: &World,
    particles: &[T],
    record: RecordRead,
) -> Binned {
    let input = harness.array(particles, particles.len());
    let order = harness.array::<u32>(&[], particles.len());
    let tiles = world.tiles(harness);
    let pool_tiles = world.nodes.pool_tiles;
    let mut sorter = ParticleSorter::default();
    sorter.prepare_pooled(&harness.device);
    sorter
        .reserve_keys(&harness.device, pool_len(pool_tiles))
        .expect("the cell ranges");
    let mut enc = harness.device.create_encoder("whitewater pooled sort");
    sorter
        .encode_pooled(
            &harness.device,
            &mut enc,
            &SortJob {
                particles: &input.1,
                read: record,
                capacity: particles.len() as u32,
                count: SortCount::Slots(particles.len() as u32),
                bin_min: lattice_origin(H),
                inv_cell: 1.0 / H,
                bins: [0; 3],
                sorted: None,
                order: Some(&order.1),
                gate: None,
            },
            &tiles.1,
            pool_tiles,
            &LABELS,
        )
        .expect("the pooled sort encodes");
    enc.commit_and_wait().expect("GPU work completes");
    let len = pool_len(pool_tiles) as usize;
    let ranges: Vec<CellRange> = read(sorter.ranges().expect("the cell ranges"), len);
    let ranges = harness.array(&ranges, len);
    Binned {
        particles: input,
        ranges,
        order,
    }
}

/// Whitewater particles sort in place: ranges and order are byte-identical to
/// sorting liquid particle records at the same positions, live exactly when
/// the slot holds a particle (dead ones too) at a finite position.
#[test]
fn sort_bins_whitewater_particles_dead_and_alive() {
    let mut harness = Harness::new();
    let world = World::new();
    let pool = crowded(&world, &mut Rng(0x5047_0001));
    let liquid: Vec<FluidParticle> = pool
        .iter()
        .map(|p| {
            let live = p.kind < 3 && p.position_lifetime[..3].iter().all(|v| v.is_finite());
            let [x, y, z, _] = p.position_lifetime;
            FluidParticle {
                position_radius: [x, y, z, if live { 0.02 } else { 0.0 }],
                velocity: [0.0; 3],
                id: 1,
            }
        })
        .collect();
    let len = pool_len(world.nodes.pool_tiles) as usize;
    let bins = |binned: &Binned| {
        (
            read::<CellRange>(&binned.ranges.1, len),
            read::<u32>(&binned.order.1, pool.len()),
        )
    };
    let whitewater = {
        let binned = sort_into_world(&mut harness, &world, &pool, whitewater_record_read());
        bins(&binned)
    };
    let expected = {
        let binned = sort_into_world(&mut harness, &world, &liquid, LIQUID_PARTICLE_READ);
        bins(&binned)
    };
    let live: u32 = expected.0.iter().map(|r| r.count).sum();
    let dead = pool
        .iter()
        .filter(|p| p.kind < 3 && p.position_lifetime[3] <= 0.0)
        .count();
    assert!(
        dead > 100 && (live as usize) < pool.len() && live > 2000,
        "live {live}, dead {dead}"
    );
    assert!(
        whitewater.0 == expected.0 && whitewater.1 == expected.1,
        "whitewater bins and order exactly as the equivalent liquid particles"
    );
    // The crowd lies in held tiles: nothing is in the canonical bin.
    assert_eq!(expected.0.last().map(|r| r.count), Some(0));
}

fn preserve_run(
    harness: &mut Harness,
    world: &World,
    pool: &[WhitewaterParticle],
    values: &[(&'static str, f32)],
) -> Vec<WhitewaterParticle> {
    let binned = sort_into_world(harness, world, pool, whitewater_record_read());
    let tiles = world.tiles(harness);
    run(
        harness,
        &mut PreserveFoam::new(),
        &[
            ("pool", binned.particles.0),
            ("binned", binned.particles.0),
            ("cell_ranges", binned.ranges.0),
            ("order", binned.order.0),
            ("tiles", tiles.0),
        ],
        pool.len(),
        &params(values),
    )
}

/// The sort then the preservation, against FLIP's own statement: every foam
/// particle, dead or alive, gains by its cell's foam count, saturating
/// above the max density and nothing below the min; off, nothing changes.
#[test]
fn preserve_foam_matches_flip() {
    let mut harness = Harness::new();
    let world = World::new();
    let pool = crowded(&world, &mut Rng(0x9e5e_0001));
    let s = Preserve {
        dt: 1.0 / 50.0,
        rate: 0.9,
        min_density: 15.0,
        max_density: 50.0,
    };
    let got = preserve_run(
        &mut harness,
        &world,
        &pool,
        &preserve_values(&world, s, true),
    );
    let want = cpu::preserve(&pool, world.tank_low(), H, NODES.map(|n| n - 1), s);
    let (mut partial, mut full, mut none, mut revived) = (0, 0, 0, 0);
    for (i, ((got, want), before)) in got.iter().zip(&want).zip(&pool).enumerate() {
        let (mut a, mut b) = (*got, *want);
        assert!(
            close(a.position_lifetime[3], b.position_lifetime[3])
                || a.position_lifetime[3].is_nan() && b.position_lifetime[3].is_nan(),
            "slot {i}: GPU {got:?} CPU {want:?}"
        );
        a.position_lifetime[3] = 0.0;
        b.position_lifetime[3] = 0.0;
        assert_eq!(
            bytemuck::bytes_of(&a),
            bytemuck::bytes_of(&b),
            "slot {i}: GPU {got:?} CPU {want:?}"
        );
        if before.kind == 1 {
            let gain = want.position_lifetime[3] - before.position_lifetime[3];
            full += usize::from((gain - s.rate * s.dt).abs() < 1e-6);
            none += usize::from(gain == 0.0);
            partial += usize::from(gain > 1e-6 && gain < s.rate * s.dt - 1e-6);
            revived +=
                usize::from(before.position_lifetime[3] <= 0.0 && want.position_lifetime[3] > 0.0);
        }
    }
    println!("{partial} partial gains, {full} saturated, {none} none, {revived} dead foam revived");
    assert!(
        partial > 300 && full > 300 && none > 60 && revived > 50,
        "{partial} {full} {none} {revived}"
    );

    let off = preserve_run(
        &mut harness,
        &world,
        &pool,
        &preserve_values(&world, s, false),
    );
    assert_eq!(
        bytemuck::cast_slice::<_, u8>(&off),
        bytemuck::cast_slice::<_, u8>(&pool),
        "off, the pool passes whole"
    );
}

/// The preservation folded by the codegen alone in its region, bit for bit
/// the standalone kernel.
#[test]
fn preserve_foam_fused_matches_unfused() {
    let mut harness = Harness::new();
    let world = World::new();
    let pool = crowded(&world, &mut Rng(0x9e5e_0002));
    let values = preserve_values(&world, Preserve::flip(), true);
    let unfused = preserve_run(&mut harness, &world, &pool, &values);

    let binned = sort_into_world(&mut harness, &world, &pool, whitewater_record_read());
    let tiles = world.tiles(&mut harness);
    // The pool read in place and as the bins' records: two externals over
    // one buffer.
    let inputs = (0..5).map(InputSource::External).collect();
    let fused_out: Vec<WhitewaterParticle> = fused(
        &mut harness,
        vec![member::<PreserveFoam>(0, inputs)],
        &[
            &binned.particles.1,
            &binned.particles.1,
            &binned.ranges.1,
            &binned.order.1,
            &tiles.1,
        ],
        pool.len(),
        &values,
    );
    for j in 0..pool.len() {
        assert_eq!(
            bytemuck::bytes_of(&fused_out[j]),
            bytemuck::bytes_of(&unfused[j]),
            "slot {j}: fused {:?} standalone {:?}",
            fused_out[j],
            unfused[j]
        );
    }
    assert!(
        unfused.iter().zip(&pool).filter(|(a, b)| a != b).count() > 300,
        "the preservation changed the pool"
    );
}
