//! GPU proofs for `node.surface_tiles`: the tiles the build holds round the
//! displayed points, and how long a tile the water touched stays held in its
//! slot.

use super::*;
use manifold_node_engine::bindings::Slot;
use manifold_node_engine::exec::backend::Backend;
use manifold_node_engine::scene::world_tiles::{NONE, table_words, tile_of_cell};
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use std::collections::{BTreeMap, BTreeSet};

/// The water's cell size; the display runs at the same one (scale 1).
const H: f32 = 0.25;
/// The display's first pool.
const POOL: u32 = START_POOL_TILES;
const KEEP: f32 = 300.0;
/// Particle records the rig's array holds, live or not.
const CAPACITY: usize = 4;

fn particle(position: [f32; 3]) -> FluidParticle {
    FluidParticle {
        position_radius: [position[0], position[1], position[2], 0.05],
        velocity: [0.0; 3],
        id: 1,
    }
}

/// The tile the build puts a point in: its world cell counted from −h/2.
fn tile_of(position: [f32; 3]) -> [i32; 3] {
    let origin = lattice_origin(H);
    tile_of_cell(std::array::from_fn(|a| {
        ((position[a] - origin[a]) / H).floor() as i32
    }))
}

/// The tiles within one tile of any of `tiles`.
fn ring(tiles: &[[i32; 3]]) -> BTreeSet<[i32; 3]> {
    let mut held = BTreeSet::new();
    for tile in tiles {
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    held.insert([tile[0] + dx, tile[1] + dy, tile[2] + dz]);
                }
            }
        }
    }
    held
}

struct Built {
    /// Every held tile and its slot.
    held: BTreeMap<[i32; 3], u32>,
    extent: [f32; 8],
    pool_tiles: f32,
    cell_size: f32,
}

impl Built {
    fn tiles(&self) -> BTreeSet<[i32; 3]> {
        self.held.keys().copied().collect()
    }
}

struct Rig {
    harness: Harness,
    node: SurfaceTiles,
    time: Slot,
    epoch: Slot,
    tiles: Slot,
    extent: Slot,
    pool: Slot,
    cell: Slot,
}

impl Rig {
    fn new() -> Self {
        let mut harness = Harness::new();
        let time = harness.scalar_input(0.0);
        let epoch = harness.scalar_input(0.0);
        let (tiles, _) = harness.array::<u32>(&[], 1);
        let (extent, _) = harness.array::<f32>(&[], 1);
        let pool = harness.scalar();
        let cell = harness.scalar();
        Self {
            harness,
            node: SurfaceTiles::new(),
            time,
            epoch,
            tiles,
            extent,
            pool,
            cell,
        }
    }

    /// One frame at display `time` and `epoch` over `points`.
    fn run(&mut self, points: &[[f32; 3]], time: f32, epoch: f32) -> Built {
        self.run_with(points, &[], time, epoch, H, None)
    }

    /// [`Self::run`] with `spray`, for water of `sim_cell` metres at the
    /// display cell `H`, with a grid `interior` (its pool values, then its
    /// table words).
    fn run_with(
        &mut self,
        points: &[[f32; 3]],
        spray: &[[f32; 3]],
        time: f32,
        epoch: f32,
        sim_cell: f32,
        interior: Option<(&[f32], &[u32])>,
    ) -> Built {
        let records: Vec<FluidParticle> = points.iter().map(|&p| particle(p)).collect();
        let (particles, _) = self.harness.array(&records, CAPACITY);
        let spray: Vec<FluidParticle> = spray.iter().map(|&p| particle(p)).collect();
        let (spray, _) = self.harness.array(&spray, CAPACITY);
        let interior = interior.map(|(values, words)| {
            (
                self.harness.array(values, values.len()).0,
                self.harness.array(words, words.len()).0,
            )
        });
        self.harness
            .backend
            .set_scalar(self.time, ParamValue::Float(time));
        self.harness
            .backend
            .set_scalar(self.epoch, ParamValue::Float(epoch));
        let run_params = params(&[
            ("sim_cell_size", sim_cell),
            ("resolution_scale", (sim_cell / H).round()),
            ("keep_seconds", KEEP),
        ]);
        let mut inputs = vec![
            ("particles", particles),
            ("spray", spray),
            ("time", self.time),
            ("epoch", self.epoch),
        ];
        if let Some((values, words)) = interior {
            inputs.extend([("interior", values), ("interior_tiles", words)]);
        }
        let (scalars, errors) = self.harness.run(
            &mut self.node,
            &inputs,
            &[
                ("tiles", self.tiles),
                ("extent", self.extent),
                ("pool_tiles", self.pool),
                ("cell_size", self.cell),
            ],
            &run_params,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let scalar = |slot: Slot| match scalars.iter().find(|(s, _)| *s == slot) {
            Some((_, ParamValue::Float(value))) => *value,
            _ => panic!("the node wrote no scalar to that output"),
        };
        let pool = scalar(self.pool) as u32;
        let words = read::<u32>(&self.harness.buffer(self.tiles), table_words(pool) as usize);
        let entries = table_entries(pool);
        let held = (0..pool)
            .filter_map(|slot| {
                let row = slot_row_word(entries, slot) as usize;
                (words[row + 3] != NONE).then(|| {
                    let tile = std::array::from_fn(|a| words[row + a] as i32);
                    (tile, slot)
                })
            })
            .collect();
        let extent = read::<f32>(&self.harness.buffer(self.extent), 8)
            .try_into()
            .expect("eight floats");
        Built {
            held,
            extent,
            pool_tiles: scalar(self.pool),
            cell_size: scalar(self.cell),
        }
    }
}

#[test]
fn the_tiles_of_points_either_side_of_the_origin_and_their_ring_are_held() {
    // x = -0.2 is cell -1 (tile -1), x = 0.2 is cell 1 (tile 0).
    let points = [[-0.2, 0.1, 0.1], [0.2, 0.1, 0.1]];
    let tiles: Vec<_> = points.iter().map(|&p| tile_of(p)).collect();
    assert_eq!(tiles, [[-1, 0, 0], [0, 0, 0]]);
    let mut rig = Rig::new();
    let built = rig.run(&points, 10.0, 0.0);
    assert_eq!(built.pool_tiles, POOL as f32);
    assert_eq!(built.cell_size, H);
    assert_eq!(built.tiles(), ring(&tiles));
    assert_eq!(built.held.len(), 4 * 3 * 3);
    let slots: BTreeSet<u32> = built.held.values().copied().collect();
    assert_eq!(slots.len(), built.held.len(), "one slot a tile");
    // Node p sits at (p - 1/2)·h: tiles -2..=1 in x and -1..=1 in y and z run
    // from the low corner of the lowest to the high face of the highest.
    let low = |tile: i32| (8 * tile) as f32 * H - 0.5 * H;
    let high = |tile: i32| (8 * (tile + 1)) as f32 * H - 0.5 * H;
    assert_eq!(
        built.extent,
        [
            low(-2),
            low(-1),
            low(-1),
            0.0,
            high(1),
            high(1),
            high(1),
            0.0
        ]
    );
}

/// Narrow Band's grid water holds the display tiles over it with no
/// particle there: water three display cells to a water cell, in one octant
/// of a water tile across the origin, holds the ring round the display tile
/// of the octant's middle, which covers every display cell the octant's water
/// reaches; the air elsewhere holds nothing.
#[test]
fn the_grid_interior_holds_the_display_tiles_over_its_water() {
    let sim_cell = 3.0 * H;
    let water = manifold_water_liquid::testkit::world_box::BoxTable::new([-8, 0, 0], [16, 8, 8]);
    // World cells x -4..=-1, y and z 0..=3: octant (1, 0, 0) of water tile
    // (-1, 0, 0).
    let wet = |c: [i32; 3]| {
        (-4..=-1).contains(&c[0]) && (0..=3).contains(&c[1]) && (0..=3).contains(&c[2])
    };
    let cells = water.cells.map(|n| n as i32);
    let dense: Vec<f32> = (0..cells.iter().product::<i32>())
        .map(|i| {
            let c = [
                water.origin[0] + i % cells[0],
                water.origin[1] + (i / cells[0]) % cells[1],
                water.origin[2] + i / (cells[0] * cells[1]),
            ];
            if wet(c) { -sim_cell } else { sim_cell }
        })
        .collect();
    let values = water.to_pool(&dense, 3.0 * sim_cell);
    let mut rig = Rig::new();
    let built = rig.run_with(&[], &[], 10.0, 0.0, sim_cell, Some((&values, &water.words)));
    let middle = [-2.5, 1.5, 1.5].map(|c: f32| c * sim_cell);
    assert_eq!(built.tiles(), ring(&[tile_of(middle)]));
    // Water cell c spans (c ± 1/2)·h_water; every display cell centred in the
    // octant's water is held.
    let held = built.tiles();
    for z in 0..12 {
        for y in 0..12 {
            for x in 0..12 {
                let at = [
                    (-4.5 * sim_cell) + (x as f32 + 0.5) * H,
                    (-0.5 * sim_cell) + (y as f32 + 0.5) * H,
                    (-0.5 * sim_cell) + (z as f32 + 0.5) * H,
                ];
                assert!(
                    held.contains(&tile_of(at)),
                    "display cell at {at:?} is not held"
                );
            }
        }
    }
}

#[test]
fn no_points_hold_no_tiles_and_an_empty_extent() {
    let mut rig = Rig::new();
    let built = rig.run(&[], 10.0, 0.0);
    assert!(built.held.is_empty());
    assert!(built.extent[..3].iter().all(|&v| v > 0.0));
    assert!(built.extent[4..7].iter().all(|&v| v < 0.0));
    assert!((0..3).all(|a| built.extent[a] > built.extent[4 + a]));
}

#[test]
fn a_touched_tile_keeps_its_slot_for_the_keep_window_then_frees() {
    let (here, there) = ([0.1, 0.1, 0.1], [10.1, 0.1, 0.1]);
    assert_eq!(tile_of(there), [5, 0, 0]);
    let mut rig = Rig::new();
    let first = rig.run(&[here], 10.0, 0.0);
    assert_eq!(first.tiles(), ring(&[tile_of(here)]));

    // The water moves on: its old tiles stay held, each in its own slot,
    // beside the tiles of the new water.
    let moved = rig.run(&[there], 200.0, 0.0);
    assert_eq!(moved.tiles(), ring(&[tile_of(here), tile_of(there)]));
    for (tile, slot) in &first.held {
        assert_eq!(moved.held[tile], *slot, "tile {tile:?} keeps its slot");
    }

    // Still inside the window at 300: the record written at the end of the
    // frame at 300 keeps the old tiles for the next one.
    let later = rig.run(&[there], 300.0, 0.0);
    for (tile, slot) in &first.held {
        assert_eq!(later.held[tile], *slot);
    }
    // 311 s after the first touch is past the window, but the frame at 311
    // still builds from the record written at 300; the next frame does not.
    let past = rig.run(&[there], 311.0, 0.0);
    assert!(past.held.contains_key(&tile_of(here)));
    let gone = rig.run(&[there], 312.0, 0.0);
    assert_eq!(gone.tiles(), ring(&[tile_of(there)]));
    // The water's own tiles never left their slots.
    for (tile, slot) in &moved.held {
        if !first.held.contains_key(tile) {
            assert_eq!(gone.held[tile], *slot, "tile {tile:?}");
        }
    }
}

/// Spray holds its tiles while it is there and keeps none: only the liquid
/// leaves wetness behind.
#[test]
fn spray_holds_its_tiles_only_while_it_is_there() {
    let (water, spray) = ([0.1, 0.1, 0.1], [10.1, 0.1, 0.1]);
    let mut rig = Rig::new();
    let first = rig.run_with(&[water], &[spray], 10.0, 0.0, H, None);
    assert_eq!(first.tiles(), ring(&[tile_of(water), tile_of(spray)]));
    // Both gone: the frame at 20 builds from the keep records written at 10.
    let later = rig.run(&[], 20.0, 0.0);
    assert_eq!(later.tiles(), ring(&[tile_of(water)]));
}

#[test]
fn touching_a_tile_again_starts_its_window_over() {
    let here = [0.1, 0.1, 0.1];
    let mut rig = Rig::new();
    let first = rig.run(&[here], 10.0, 0.0);
    rig.run(&[here], 200.0, 0.0);
    // 250 s after the second touch, 440 s after the first.
    let kept = rig.run(&[], 450.0, 0.0);
    assert_eq!(kept.held, first.held);
    let kept = rig.run(&[], 460.0, 0.0);
    assert_eq!(kept.held, first.held);
    let kept = rig.run(&[], 500.0, 0.0);
    assert_eq!(kept.held, first.held);
    // The frame at 520 is 320 s past the touch and writes no record.
    rig.run(&[], 520.0, 0.0);
    let gone = rig.run(&[], 521.0, 0.0);
    assert!(gone.held.is_empty());
}

#[test]
fn a_new_epoch_or_time_running_back_past_the_window_drops_the_kept_tiles() {
    let here = [0.1, 0.1, 0.1];
    // The same epoch keeps them across an empty frame.
    let mut rig = Rig::new();
    rig.run(&[here], 1000.0, 3.0);
    assert_eq!(rig.run(&[], 1001.0, 3.0).held.len(), 27);
    // A new epoch starts over, though time moved on by a second.
    let mut rig = Rig::new();
    rig.run(&[here], 1000.0, 3.0);
    assert!(rig.run(&[], 1001.0, 4.0).held.is_empty());
    // Time back by less than the window keeps them.
    let mut rig = Rig::new();
    rig.run(&[here], 1000.0, 3.0);
    assert_eq!(rig.run(&[], 900.0, 3.0).held.len(), 27);
    // Time back by more than the window drops them.
    let mut rig = Rig::new();
    rig.run(&[here], 1000.0, 3.0);
    assert!(rig.run(&[], 600.0, 3.0).held.is_empty());
}

#[test]
fn a_pool_short_of_slots_grows_and_holds_every_tile() {
    // Points four tiles apart: 45 rings of 27 tiles, more than the first pool.
    let points: Vec<[f32; 3]> = (0..45).map(|i| [i as f32 * 8.0 + 0.1, 0.1, 0.1]).collect();
    let tiles: Vec<_> = points.iter().map(|&p| tile_of(p)).collect();
    let wanted = ring(&tiles);
    assert!(wanted.len() > POOL as usize);
    let mut rig = Rig::new();
    let first = rig.run(&points, 10.0, 0.0);
    assert_eq!(first.pool_tiles, POOL as f32);
    assert_eq!(first.held.len(), POOL as usize, "the first pool fills");
    // The next frame reads the short bit: twice the pool, every tile held.
    let grown = rig.run(&points, 10.1, 0.0);
    assert_eq!(grown.pool_tiles, 2.0 * POOL as f32);
    assert_eq!(grown.tiles(), wanted);
}
