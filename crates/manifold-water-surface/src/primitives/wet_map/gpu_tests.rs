//! GPU proof for `node.wet_map`: a contact in a decal's merge cell facing its
//! way wets that decal again, a contact in the next cell or on another face
//! starts its own, and once the half is full a new decal overwrites the
//! oldest.

use super::*;
use manifold_node_engine::bindings::Slot;
use manifold_node_engine::exec::backend::Backend;
use manifold_node_engine::scene::wet_map::{DECAL_WORDS, HEADER_WORDS, touch_key};
use manifold_node_engine::scene::world_tiles::{TILE_CELLS, pool_len};
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_liquid::testkit::world_box::table_words_of;

/// The display cell, and the water's: decals 60 mm across in radius, merged
/// in 40 mm cells.
const H: f32 = 0.1;
const SIM_CELL: f32 = 0.04;
const POOL: u32 = 4;
const RADIUS: f32 = 0.02;

/// A wall filling x below 4.7 mm, beside the held tiles' floor.
const WALL_FACE: f32 = 0.0047;

/// The tiles by the wall and a floor tile a metre out.
const HELD: &[([i32; 3], u32)] = &[([-1, 0, 0], 0), ([0, 0, 0], 1), ([1, 0, 2], 2)];

/// The floor (y below 0) and the wall: the distance to the nearer.
fn corner(p: [f32; 3]) -> f32 {
    p[1].min(p[0] - WALL_FACE)
}

/// The solid's distance at every pool element: `corner` at the node's
/// position, 1e30 in a free slot and the canonical element.
fn solid_field() -> Vec<f32> {
    (0..pool_len(POOL) as usize)
        .map(|e| {
            let slot = (e / TILE_CELLS as usize) as u32;
            let Some((tile, _)) = HELD.iter().find(|(_, s)| *s == slot) else {
                return 1.0e30;
            };
            let local = e % TILE_CELLS as usize;
            let node = [
                tile[0] * 8 + (local % 8) as i32,
                tile[1] * 8 + (local / 8 % 8) as i32,
                tile[2] * 8 + (local / 64) as i32,
            ];
            corner(node.map(|p| (p as f32 - 0.5) * H))
        })
        .collect()
}

fn particle(position: [f32; 3]) -> FluidParticle {
    FluidParticle {
        position_radius: [position[0], position[1], position[2], RADIUS],
        ..Default::default()
    }
}

/// A unit normal from its octahedral word, as the layout's `wd_normal`.
fn normal_of(word: u32) -> [f32; 3] {
    let snorm = |bits: u32| (f32::from(bits as u16 as i16) / 32767.0).max(-1.0);
    let (x, y) = (snorm(word), snorm(word >> 16));
    let z = 1.0 - x.abs() - y.abs();
    let t = (-z).max(0.0);
    let n = [x - t.copysign(x), y - t.copysign(y), z];
    let length = n.iter().map(|v| v * v).sum::<f32>().sqrt();
    n.map(|v| v / length)
}

struct Decal {
    position: [f32; 3],
    normal: [f32; 3],
    touch: u32,
    amount: u32,
}

struct Rig {
    harness: Harness,
    node: WetMap,
    time: Slot,
    epoch: Slot,
    out: Slot,
}

impl Rig {
    fn new() -> Self {
        let mut harness = Harness::new();
        let time = harness.scalar_input(0.0);
        let epoch = harness.scalar_input(0.0);
        let (out, _) = harness.array::<u32>(&[], 1);
        Self {
            harness,
            node: WetMap::new(),
            time,
            epoch,
            out,
        }
    }

    /// One run with `particles` at `time`; returns the header and the decals
    /// in the half in use.
    fn run(&mut self, particles: &[FluidParticle], time: f32) -> (Vec<u32>, Vec<Decal>) {
        let words = table_words_of(POOL, HELD);
        let (tiles, _) = self.harness.array(&words, words.len());
        let field = solid_field();
        let (solid, _) = self.harness.array(&field, field.len());
        let (particles, _) = self.harness.array(particles, particles.len());
        self.harness
            .backend
            .set_scalar(self.time, ParamValue::Float(time));
        self.harness
            .backend
            .set_scalar(self.epoch, ParamValue::Float(0.0));
        let run_params = params(&[
            ("drying", 2.0),
            ("sim_cell_size", SIM_CELL),
            ("cell_size", H),
            ("pool_tiles", POOL as f32),
        ]);
        let (_, errors) = self.harness.run(
            &mut self.node,
            &[
                ("particles", particles),
                ("solid", solid),
                ("tiles", tiles),
                ("time", self.time),
                ("epoch", self.epoch),
            ],
            &[("map", self.out)],
            &run_params,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let map = read::<u32>(&self.harness.buffer(self.out), map_words() as usize);
        let header = map[..HEADER_WORDS as usize].to_vec();
        let half = u64::from(header[6]);
        let first = HEADER_WORDS + HASH_SLOTS + half * u64::from(CAPACITY) * DECAL_WORDS;
        let decals = (0..u64::from(header[0] / 3))
            .map(|i| {
                let word = &map[(first + i * DECAL_WORDS) as usize..];
                Decal {
                    position: [0, 1, 2].map(|k| f32::from_bits(word[k])),
                    normal: normal_of(word[3]),
                    touch: word[4],
                    amount: word[5],
                }
            })
            .collect();
        (header, decals)
    }
}

fn assert_near(got: [f32; 3], want: [f32; 3], tolerance: f32, what: &str) {
    let off = (0..3).map(|k| (got[k] - want[k]).abs()).fold(0.0, f32::max);
    assert!(off < tolerance, "{what}: {got:?} against {want:?}");
}

#[test]
fn a_contact_wets_its_cells_decal_others_start_their_own_and_a_full_half_overwrites_the_oldest() {
    let mut rig = Rig::new();
    // A centimetre above the floor, then 14 mm along it in the same 40 mm
    // cell a frame later, beside it in the next cell, and 3 mm off the wall.
    let frame = 1.0 / 60.0;
    let (second_time, third_time) = (1.0 + frame, 1.0 + 2.0 * frame);
    let first = [1.003, 0.01, 2.004];
    let second = [1.013, 0.01, 2.014];
    let next_cell = [1.045, 0.01, 2.014];
    let on_the_wall = [WALL_FACE + 0.003, 0.31, 0.404];
    rig.run(&[particle(first)], 1.0);
    let (header, decals) = rig.run(
        &[particle(second), particle(next_cell), particle(on_the_wall)],
        second_time,
    );
    assert_eq!(
        (header[0], header[1]),
        (9, 1),
        "three vertices a decal, one instance"
    );
    assert_eq!(f32::from_bits(header[4]), second_time);
    assert_eq!(f32::from_bits(header[5]), 2.0);
    assert_eq!(f32::from_bits(header[9]), DECAL_RADIUS_CELLS * SIM_CELL);
    assert_eq!(f32::from_bits(header[10]), MERGE_CELLS * SIM_CELL);
    // The first decal sits halfway from its contact to its cell's middle
    // (1.02, 2.02) and takes the second touch and a second quarter of water.
    let floor = &decals[0];
    assert_near(floor.position, [1.0115, 0.0, 2.012], 1e-5, "floor centre");
    assert_near(floor.normal, [0.0, 1.0, 0.0], 1e-3, "floor normal");
    assert_eq!((floor.touch, floor.amount), (touch_key(second_time), 128));
    let (walls, floors): (Vec<&Decal>, Vec<&Decal>) =
        decals[1..].iter().partition(|d| d.normal[0] > 0.5);
    let ([wall], [beside]) = (walls.as_slice(), floors.as_slice()) else {
        panic!("{} decals, not 3", decals.len());
    };
    // The next cell's middle is (1.06, 2.02); the wall cell's (y, z) middle
    // is (0.30, 0.42).
    assert_near(
        beside.position,
        [1.0525, 0.0, 2.017],
        1e-5,
        "next cell centre",
    );
    assert_eq!((beside.touch, beside.amount), (touch_key(second_time), 64));
    assert_near(
        wall.position,
        [WALL_FACE, 0.305, 0.412],
        1e-5,
        "wall centre",
    );
    assert_near(wall.normal, [1.0, 0.0, 0.0], 1e-3, "wall normal");
    assert_eq!((wall.touch, wall.amount), (touch_key(second_time), 64));

    // A full half: a contact in a fresh cell overwrites the oldest decal,
    // the first, and the half stays full.
    let map = rig.node.map.as_ref().expect("the map");
    let ptr = map.mapped_ptr().expect("a shared map") as *mut u32;
    // SAFETY: the run above was waited on, so the GPU no longer touches the
    // map, and word 0 lies inside it.
    unsafe { ptr.write(3 * CAPACITY) };
    let (header, decals) = rig.run(&[particle([1.21, 0.01, 2.11])], third_time);
    assert_eq!(header[0], 3 * CAPACITY, "the half stays full");
    assert_eq!(
        (header[7], header[11]),
        (1, 1),
        "one overwritten, from the front"
    );
    assert_near(
        decals[0].position,
        [1.215, 0.0, 2.105],
        1e-5,
        "the new decal",
    );
    assert_eq!(
        (decals[0].touch, decals[0].amount),
        (touch_key(third_time), 64)
    );
    assert_eq!(decals[1].touch, touch_key(second_time), "the rest stay");
}
