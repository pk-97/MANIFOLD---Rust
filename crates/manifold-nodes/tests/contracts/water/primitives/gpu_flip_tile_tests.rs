//! The GPU FLIP step on world tiles (docs/WATER_INFINITE_DOMAIN_DESIGN.md):
//! the table the step builds from its particles, read back through the
//! node's `tiles` output, and the step under the NaN poison (the
//! defined-value rule).

use manifold_gpu::{GpuBuffer, GpuDevice};

use crate::contracts::water::primitives::gpu_flip_scene_tests::Run;
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::world_tiles::{
    EMPTY, TILE, find, neighbour_index, neighbour_row_word, slot_row_word, table_entries, tile_key,
};
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_nodes_water::presets::gpu_flip::WaterScene;
use manifold_water_gpu_flip::primitives::gpu_flip_step::{GpuFlipStep, pool_tiles, set_poison};
use manifold_water_liquid::fluid_particles::FaceSample;
use manifold_water_liquid::lattice::PADDING_NODES;
use manifold_water_liquid::primitives::liquid_stats::SOLVER_WORDS;
use manifold_water_liquid::testkit::world_box::pool_index_under;

/// Every word of a buffer the step allocated (private storage).
fn words_of(device: &GpuDevice, buffer: &GpuBuffer) -> Vec<u32> {
    let staged = device.create_buffer_shared(buffer.size);
    let mut enc = device.create_encoder("world tiles proof readback");
    enc.copy_buffer_to_buffer(buffer, &staged, buffer.size);
    enc.commit_and_wait_completed();
    read(&staged, (buffer.size / 4) as usize)
}

/// Two particles side by side across the world origin on x, in world cells
/// (-1, 0, 0) and (0, 0, 0), so in tiles -1 and 0. After one tick the step's
/// table holds both tiles, each slot's row names its tile, each is the
/// other's x neighbour, and the step's distance reads water at both cells
/// through the table, air three cells past them, beyond the distance's
/// reach.
#[test]
fn gpu_flip_step_tiles_two_particles_across_the_origin() {
    const H: f32 = 0.25;
    // An authored box of 8 cells from world cell -4's centre: the solver
    // grid, 1.5 cells outside it, holds world cells -5..=5.
    const CELLS: u32 = 8;
    const MIN: f32 = -4.0 * H;
    let pad = PADDING_NODES as f32;
    let nodes = CELLS as f32 + 1.0 + 2.0 * pad;
    let lattice = [
        ("nodes_x", nodes),
        ("nodes_y", nodes),
        ("nodes_z", nodes),
        ("lattice_min_x", MIN - pad * H),
        ("lattice_min_y", MIN - pad * H),
        ("lattice_min_z", MIN - pad * H),
        ("cell_size", H),
        ("interval_duration", 1.0 / 60.0),
        ("volume_projection", 0.0),
    ];
    // Cell p is centred on p·h.
    let particles: Vec<FluidParticle> = [-1.0f32, 0.0]
        .iter()
        .enumerate()
        .map(|(i, &x)| FluidParticle {
            position_radius: [x * H, 0.0, 0.0, 0.31017524 * H],
            velocity: [0.0; 3],
            id: i as u32 + 1,
        })
        .collect();
    let mut h = Harness::new();
    let mut node = GpuFlipStep::new();
    node.prepare_pipelines(&h.device);
    let slots = particles.len();
    let out = h.array(&particles, slots);
    let identity = h.array::<u32>(&[3, 1, 0, 0, 1, 1, 1, slots as u32], 8);
    let capped = h.array::<u32>(&[], 2 * slots + SOLVER_WORDS as usize);
    let faces = h.array::<FaceSample>(&[], 1);
    let distance = h.array::<f32>(&[], 1);
    let tiles = h.array::<u32>(&[], 1);
    let (_, errors) = h.run(
        &mut node,
        &[("particles", out.0), ("identity", identity.0)],
        &[
            ("out", out.0),
            ("faces", faces.0),
            ("distance", distance.0),
            ("tiles", tiles.0),
            ("capped", capped.0),
        ],
        &params(&lattice),
    );
    assert!(errors.is_empty(), "{errors:?}");

    let pool = pool_tiles([CELLS + 3; 3]);
    let entries = table_entries(pool);
    let table = words_of(&h.device, &h.buffer(tiles.0));
    let slot_of = |tile: [i32; 3]| -> u32 {
        let entry = find(&table[..entries as usize], tile_key(tile))
            .unwrap_or_else(|| panic!("tile {tile:?} holds a slot"));
        table[(entries + entry) as usize]
    };
    let (low, high) = (slot_of([-1, 0, 0]), slot_of([0, 0, 0]));
    assert_ne!(low, high, "one slot a tile");
    for (slot, tile) in [(low, [-1, 0, 0]), (high, [0, 0, 0])] {
        let row = slot_row_word(entries, slot) as usize;
        assert_eq!(
            table[row..row + 3]
                .iter()
                .map(|&c| c as i32)
                .collect::<Vec<_>>(),
            tile,
            "slot {slot}'s row names its tile"
        );
    }
    let neighbour = |slot: u32, d: [i32; 3]| {
        table[neighbour_row_word(entries, pool, slot) as usize + neighbour_index(d) as usize]
    };
    assert_eq!(
        neighbour(low, [1, 0, 0]),
        high,
        "tile 0 is tile -1's +x neighbour"
    );
    assert_eq!(
        neighbour(high, [-1, 0, 0]),
        low,
        "tile -1 is tile 0's -x neighbour"
    );
    assert_eq!(neighbour(low, [0, 0, 0]), low, "a slot is its own centre");

    let phi: Vec<f32> = words_of(&h.device, &h.buffer(distance.0))
        .into_iter()
        .map(f32::from_bits)
        .collect();
    let at = |p: [i32; 3]| phi[pool_index_under(&table, pool, p)];
    for p in [[-1, 0, 0], [0, 0, 0]] {
        assert!(at(p) < 0.0, "cell {p:?} is water: {}", at(p));
    }
    assert!(
        at([3, 0, 0]) > 0.0,
        "cell (3, 0, 0) is air: {}",
        at([3, 0, 0])
    );
}

/// A closed Dam Break keeps its volume on world tiles: the pool holds every
/// tile the water can reach, so the move never removes a particle for want
/// of a slot, and the live count stays the seeded count, every particle
/// live and finite, frame after frame.
#[test]
fn gpu_flip_dam_break_keeps_its_volume_on_world_tiles() {
    let mut run = Run::new(WaterScene::dam_break(16));
    let seeded = run.particles().len();
    assert!(seeded > 0, "the fill seeds the dam");
    for frame in 0..30 {
        run.frame();
        let particles = run.particles();
        assert_eq!(particles.len(), seeded, "frame {frame}: the live count");
        assert!(
            particles.iter().all(|p| p.position_radius[3] > 0.0
                && p.position_radius
                    .iter()
                    .chain(&p.velocity)
                    .all(|v| v.is_finite())),
            "frame {frame}: a dead or broken particle among the live ones"
        );
    }
}

/// A run of `dam_break(64)` whose every frame, the fill included, goes
/// through the `poison` lever.
struct Twin {
    run: Run,
    poison: bool,
}

impl Twin {
    fn new(poison: bool) -> Self {
        set_poison(poison);
        let run = Run::new(WaterScene::dam_break(64));
        set_poison(false);
        Self { run, poison }
    }

    fn frame(&mut self) {
        set_poison(self.poison);
        self.run.frame();
        set_poison(false);
    }
}

/// The first particle and, with `faces` (the step has run: not on the fill
/// frame, whose face output is unsized), the first face record that differ
/// between the two runs, as none or where and what.
fn first_difference(a: &Run, b: &Run, faces: bool) -> Option<String> {
    let (pa, pb) = (a.particles(), b.particles());
    if let Some(i) =
        (0..pa.len()).find(|&i| bytemuck::bytes_of(&pa[i]) != bytemuck::bytes_of(&pb[i]))
    {
        return Some(format!("particle {i}: {:?} vs {:?}", pa[i], pb[i]));
    }
    if !faces {
        return None;
    }
    let (fa, fb) = (a.faces(), b.faces());
    let m = a.n() + 1;
    if let Some(i) =
        (0..fa.len()).find(|&i| bytemuck::bytes_of(&fa[i]) != bytemuck::bytes_of(&fb[i]))
    {
        let p = [i % m, (i / m) % m, i / (m * m)];
        let tile = p.map(|v| v / TILE as usize);
        return Some(format!(
            "face record {i} at {p:?} (grid tile {tile:?}): {:?} vs {:?}",
            fa[i], fb[i]
        ));
    }
    None
}

/// NaN in every free slot and in the held slots outside rings 0 and 1 never
/// reaches the particles or the output faces: both bitwise equal to the
/// unpoisoned run over 4 frames. No pass writes the canonical element
/// either: the pooled `faces` and `distance` end with the element
/// `pool_canon` wrote (every face absent, φ 3h), which is what reads of the
/// absent tiles return.
#[test]
fn gpu_flip_sparse_step_survives_poison() {
    let mut clean = Twin::new(false);
    let mut poisoned = Twin::new(true);
    for frame in 0..=4 {
        if frame > 0 {
            clean.frame();
            poisoned.frame();
        }
        if let Some(diff) = first_difference(&clean.run, &poisoned.run, frame > 0) {
            panic!("frame {frame} differs under the poison: {diff}");
        }
        let particles = poisoned.run.particles();
        assert!(
            particles.iter().all(|p| p
                .position_radius
                .iter()
                .chain(&p.velocity)
                .all(|v| v.is_finite())),
            "frame {frame}: a poisoned particle"
        );
    }
    let (faces, distance, table) = poisoned.run.pooled_outputs();
    let h = WaterScene::dam_break(64).cell_size() as f32;
    assert_eq!(
        faces[faces.len() - 1],
        FaceSample::default(),
        "the canonical faces"
    );
    assert_eq!(
        distance[distance.len() - 1].to_bits(),
        (3.0 * h).to_bits(),
        "the canonical distance"
    );
    let pool = (faces.len() as u32 - 1) / TILE.pow(3);
    let held = table[..table_entries(pool) as usize]
        .iter()
        .filter(|&&key| key != EMPTY)
        .count();
    assert!(
        held > 0 && held < pool as usize,
        "{held} of {pool} slots held: the water holds some, not all"
    );
}
