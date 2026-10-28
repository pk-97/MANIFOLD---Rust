//! GPU proofs for `node.wet_map`: a particle against a solid stamps its texel
//! with the time and the height it touched, the stamp stays while the water
//! is gone, and the map starts over or moves, cleared, when it should.

use super::*;
use manifold_node_engine::bindings::Slot;
use manifold_node_engine::exec::backend::Backend;
use manifold_node_engine::scene::wet_map::{
    HEADER_WORDS, TEXEL_WORDS, height_bit, height_key, touch_key,
};
use manifold_node_engine::scene::world_tiles::{TILE_CELLS, pool_len};
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_liquid::testkit::world_box::table_words_of;

/// The display cell, and the water's: a 10 mm base texel.
const H: f32 = 0.1;
const SIM_CELL: f32 = 0.04;
const POOL: u32 = 4;
const RADIUS: f32 = 0.02;

type Held = [([i32; 3], u32)];

/// The solid's distance at every pool element of `held`: `solid` at the
/// node's position, 1e30 in a free slot and the canonical element.
fn solid_field(held: &Held, solid: impl Fn([f32; 3]) -> f32) -> Vec<f32> {
    (0..pool_len(POOL) as usize)
        .map(|e| {
            let slot = (e / TILE_CELLS as usize) as u32;
            let Some((tile, _)) = held.iter().find(|(_, s)| *s == slot) else {
                return 1.0e30;
            };
            let local = e % TILE_CELLS as usize;
            let node = [
                tile[0] * 8 + (local % 8) as i32,
                tile[1] * 8 + (local / 8 % 8) as i32,
                tile[2] * 8 + (local / 64) as i32,
            ];
            solid(node.map(|p| (p as f32 - 0.5) * H))
        })
        .collect()
}

/// The held tiles' node bounds in metres, as node.surface_tiles writes them.
fn extent_of(held: &Held) -> Vec<f32> {
    let lo = [0, 1, 2].map(|k| held.iter().map(|(t, _)| t[k]).min().unwrap());
    let hi = [0, 1, 2].map(|k| held.iter().map(|(t, _)| t[k]).max().unwrap());
    let low = lo.map(|t| (t as f32 * 8.0 - 0.5) * H);
    let high = hi.map(|t| ((t + 1) as f32 * 8.0 - 0.5) * H);
    vec![low[0], low[1], low[2], 0.0, high[0], high[1], high[2], 0.0]
}

/// The height a height key stands for, as the read side decodes it.
fn height_of(key: u32) -> f32 {
    f32::from_bits(if key & 0x8000_0000 != 0 {
        key & 0x7fff_ffff
    } else {
        !key
    })
}

fn particle(position: [f32; 3]) -> FluidParticle {
    FluidParticle {
        position_radius: [position[0], position[1], position[2], RADIUS],
        ..Default::default()
    }
}

/// No live particle: one unused slot.
fn none() -> Vec<FluidParticle> {
    vec![FluidParticle::default()]
}

struct Header {
    origin: [f32; 2],
    texel: f32,
    side: u32,
    now: f32,
    drying: f32,
}

struct Map {
    words: Vec<u32>,
}

impl Map {
    fn header(&self) -> Header {
        let f = |i: usize| f32::from_bits(self.words[i]);
        Header {
            origin: [f(0), f(1)],
            texel: f(2),
            side: self.words[3],
            now: f(4),
            drying: f(5),
        }
    }

    /// Every touched texel: its (x, z) index and its four words.
    fn touched(&self) -> Vec<([u32; 2], [u32; 4])> {
        let side = SIDE as usize;
        self.words[HEADER_WORDS as usize..]
            .chunks_exact(TEXEL_WORDS as usize)
            .enumerate()
            .filter(|(_, texel)| texel[0] != 0)
            .map(|(i, texel)| {
                (
                    [(i % side) as u32, (i / side) as u32],
                    [texel[0], texel[1], texel[2], texel[3]],
                )
            })
            .collect()
    }

    /// The texels a touch at (x, z) stamps: the 3 x 3 round the one under it.
    fn stamp_at(&self, x: f32, z: f32) -> Vec<[u32; 2]> {
        let [u, v] = self.texel_at(x, z);
        let mut texels: Vec<[u32; 2]> = (0..9).map(|k| [u + k % 3 - 1, v + k / 3 - 1]).collect();
        texels.sort_by_key(|t| (t[1], t[0]));
        texels
    }

    /// The texel under (x, z), as the shader finds it.
    fn texel_at(&self, x: f32, z: f32) -> [u32; 2] {
        let h = self.header();
        [
            ((x - h.origin[0]) / h.texel).floor() as u32,
            ((z - h.origin[1]) / h.texel).floor() as u32,
        ]
    }
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

    /// One run over `held` with the solid `solid` and `particles` at `time`
    /// and `epoch`; returns the map.
    fn run(
        &mut self,
        held: &Held,
        solid: impl Fn([f32; 3]) -> f32,
        particles: &[FluidParticle],
        time: f32,
        epoch: f32,
    ) -> Map {
        let words = table_words_of(POOL, held);
        let (tiles, _) = self.harness.array(&words, words.len());
        let field = solid_field(held, solid);
        let (solid, _) = self.harness.array(&field, field.len());
        let bounds = extent_of(held);
        let (extent, _) = self.harness.array(&bounds, bounds.len());
        let (particles, _) = self.harness.array(particles, particles.len());
        self.harness
            .backend
            .set_scalar(self.time, ParamValue::Float(time));
        self.harness
            .backend
            .set_scalar(self.epoch, ParamValue::Float(epoch));
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
                ("extent", extent),
                ("time", self.time),
                ("epoch", self.epoch),
            ],
            &[("map", self.out)],
            &run_params,
        );
        assert!(errors.is_empty(), "{errors:?}");
        Map {
            words: read(
                &self.harness.buffer(self.out),
                (map_bytes(SIDE) / 4) as usize,
            ),
        }
    }
}

fn floor(p: [f32; 3]) -> f32 {
    p[1]
}

/// A wall filling x below 4.7 mm, so its face sits inside a texel.
const WALL_FACE: f32 = 0.0047;

fn wall(p: [f32; 3]) -> f32 {
    p[0] - WALL_FACE
}

const ON_THE_FLOOR: &Held = &[([1, 0, 2], 0)];
const BY_THE_WALL: &Held = &[([-1, 0, 0], 0), ([0, 0, 0], 1)];

#[test]
fn a_particle_on_a_solid_stamps_round_its_contact_and_one_above_marks_nothing() {
    let mut rig = Rig::new();
    // A centimetre above the floor: it stamps the floor under it.
    let on = [1.003, 0.01, 2.004];
    let above = [1.303, 0.1, 2.204];
    let map = rig.run(
        ON_THE_FLOOR,
        floor,
        &[particle(on), particle(above)],
        3.0,
        0.0,
    );
    let header = map.header();
    assert_eq!(header.texel, base_texel(SIM_CELL));
    assert_eq!((header.side, header.now, header.drying), (SIDE, 3.0, 2.0));
    let touched = map.touched();
    let texels: Vec<[u32; 2]> = touched.iter().map(|&(texel, _)| texel).collect();
    assert_eq!(texels, map.stamp_at(on[0], on[2]));
    for (_, [touch, low, high, _]) in touched {
        assert_eq!(touch, touch_key(3.0));
        // The floor's height, to float noise.
        for (key, what) in [(!low, "lowest"), (high, "highest")] {
            let y = height_of(key);
            assert!(y.abs() < 1e-6, "{what} height {y}");
        }
    }
}

#[test]
fn a_touch_lands_on_the_face_and_stays_while_the_water_is_gone() {
    let mut rig = Rig::new();
    // 3 mm off the wall: the stamp lands on the face it touches.
    let at = [WALL_FACE + 0.003, 0.31, 0.404];
    let first = rig.run(BY_THE_WALL, wall, &[particle(at)], 2.0, 0.0);
    let later = rig.run(BY_THE_WALL, wall, &none(), 7.0, 0.0);
    let bin = 2.0 * base_texel(SIM_CELL);
    let words = [
        touch_key(2.0),
        !height_key(0.31),
        height_key(0.31),
        height_bit(0.31, bin, false),
    ];
    let marks: Vec<_> = first
        .stamp_at(WALL_FACE, at[2])
        .into_iter()
        .map(|texel| (texel, words))
        .collect();
    assert_eq!(first.touched(), marks);
    assert_eq!(later.touched(), marks);
    assert_eq!(later.header().origin, first.header().origin);
    assert_eq!(later.header().now, 7.0);
}

#[test]
fn a_new_epoch_or_time_running_back_clears_the_map() {
    let mut rig = Rig::new();
    let at = [WALL_FACE + 0.003, 0.31, 0.404];
    assert_eq!(
        rig.run(BY_THE_WALL, wall, &[particle(at)], 2.0, 0.0)
            .touched()
            .len(),
        9
    );
    assert!(
        rig.run(BY_THE_WALL, wall, &none(), 3.0, 1.0)
            .touched()
            .is_empty()
    );
    assert_eq!(
        rig.run(BY_THE_WALL, wall, &[particle(at)], 5.0, 1.0)
            .touched()
            .len(),
        9
    );
    assert!(
        rig.run(BY_THE_WALL, wall, &none(), 4.0, 1.0)
            .touched()
            .is_empty()
    );
}

#[test]
fn water_leaving_the_map_places_it_again_and_clears_it() {
    let mut rig = Rig::new();
    let first = rig.run(
        ON_THE_FLOOR,
        floor,
        &[particle([1.003, 0.0, 2.004])],
        1.0,
        0.0,
    );
    assert_eq!(first.touched().len(), 9);
    let far: &Held = &[([100, 0, 2], 0)];
    let moved = rig.run(far, floor, &none(), 2.0, 0.0);
    assert!(moved.touched().is_empty());
    // Centred on the new extent, 80 m on.
    let header = moved.header();
    let middle = 0.5 * (extent_of(far)[0] + extent_of(far)[4]);
    let map_middle = header.origin[0] + 0.5 * SIDE as f32 * header.texel;
    assert!(
        (map_middle - middle).abs() <= header.texel,
        "{map_middle} against {middle}"
    );
    assert_ne!(header.origin, first.header().origin);
}

#[test]
fn a_spread_wider_than_the_map_takes_a_coarser_texel() {
    let mut rig = Rig::new();
    // 20.8 m of water in x: a 10 mm map is 10.24 m across.
    let wide: &Held = &[([0, 0, 0], 0), ([25, 0, 0], 1)];
    let map = rig.run(wide, floor, &[particle([0.403, 0.0, 0.404])], 1.0, 0.0);
    let bounds = extent_of(wide);
    let header = map.header();
    let want = 2.0 * (bounds[4] - bounds[0]) / SIDE as f32;
    assert!(
        (header.texel - want).abs() < 1e-6,
        "{} against {want}",
        header.texel
    );
    assert_eq!(map.touched().len(), 9);
}
