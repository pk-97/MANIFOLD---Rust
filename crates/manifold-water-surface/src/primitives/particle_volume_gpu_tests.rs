//! GPU proofs for `node.particle_volume` against the dense blended-sphere
//! rule (every blob against every node) on world tiles. Each box of nodes is
//! the whole of the tiles it touches as a pooled field; the blobs are sorted
//! into the display cells on the CPU, as `node.sort_particles_into_cells` does
//! over the same table.

use super::*;
use bytemuck::Zeroable;
use manifold_node_engine::scene::scene_object::FIELD_FAR_CELLS;
use manifold_node_engine::scene::world_tiles::{local_index, pool_len};
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_liquid::testkit::world_box::{BoxTable, table_words_of};

const H: f32 = 0.07;
/// The shader's PV_WALL_REACH: how far, in cells, water reaches into a solid.
const WALL_REACH_CELLS: f32 = 0.5;
/// The world node of the box's first node: the 27 tiles from tile (-1, 0, -1).
const ORIGIN: [i32; 3] = [-8, 0, -8];
const DIMS: [u32; 3] = [24, 24, 24];
/// More neighbours than any film has: past the smoothing fade at every
/// particle density.
const BULK_COUNT: f32 = 1000.0;

/// A far canonical solid.
const NO_SOLID: f32 = 1.0e30;

struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 >> 8) as f32 / (1u32 << 24) as f32
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }
}

fn box_table() -> BoxTable {
    BoxTable::new(ORIGIN, DIMS)
}

/// The world cell holding `x`, by the sort's rule.
fn cell_of(x: [f32; 3]) -> [i32; 3] {
    let inv = 1.0 / H;
    std::array::from_fn(|a| ((x[a] + 0.5 * H) * inv).floor() as i32)
}

/// The blobs in cell order with each cell's range, as the sort lays them out
/// over `index_of`, a world cell's pool index; `pool` elements of ranges.
fn sort_blobs(
    blobs: &[[f32; 4]],
    pool: usize,
    index_of: impl Fn([i32; 3]) -> usize,
) -> (Vec<FluidBlob>, Vec<CellRange>) {
    let key = |b: &[f32; 4]| index_of(cell_of([b[0], b[1], b[2]]));
    let mut order: Vec<usize> = (0..blobs.len()).collect();
    order.sort_by_key(|&i| key(&blobs[i]));
    let mut sorted: Vec<FluidBlob> = order
        .iter()
        .map(|&i| FluidBlob::round([blobs[i][0], blobs[i][1], blobs[i][2]], blobs[i][3]))
        .collect();
    let mut ranges = vec![CellRange::default(); pool];
    for (slot, blob) in sorted.iter().enumerate() {
        let range = &mut ranges[key(&blob.center_radius)];
        if range.count == 0 {
            range.start = slot as u32;
        }
        range.count += 1;
    }
    // Inactive slots follow the live blobs, in no range.
    sorted.extend([FluidBlob::default(); 3]);
    (sorted, ranges)
}

/// The pool index of world cell `w` in the box table.
fn box_index(table: &BoxTable, w: [i32; 3]) -> usize {
    let c: [u32; 3] = std::array::from_fn(|a| {
        let c = w[a] - ORIGIN[a];
        assert!(
            (0..DIMS[a] as i32).contains(&c),
            "cell {w:?} is outside the box"
        );
        c as u32
    });
    table.pool_index(c)
}

/// The shader's fixed-point scale and the least weight a node reads water at.
const FIXED: f64 = 1_048_576.0;
const MIN_WEIGHT: f64 = 1.0e-3;

/// One node under the dense rule.
#[derive(Clone, Copy)]
struct Level {
    /// The rule's value, NaN where the weight is too near the cutoff to say
    /// which side the shader lands.
    value: f32,
    /// The least and most the shader's rounded sums may give, both sides'
    /// values where the rule cannot say.
    lo: f64,
    hi: f64,
}

/// Whether a round blob centred at `c` with weight radius `blend` visits
/// world node `node`: in the shader's grid box, floor((c - R)/H + 1/2)
/// through floor((c + R)/H + 1/2) + 1 on each axis, the row's nodes within
/// blend + H of the centre, the span rounded out to whole nodes.
fn visits(c: [f32; 3], blend: f32, node: [i32; 3]) -> bool {
    let in_box = (0..3).all(|a| {
        let first = ((c[a] - blend) / H + 0.5).floor() as i32;
        let last = ((c[a] + blend) / H + 0.5).floor() as i32 + 1;
        (first..=last).contains(&node[a])
    });
    let p = |a: usize| (f64::from(node[a]) - 0.5) * f64::from(H);
    let shell = f64::from(blend) + f64::from(H);
    let rest = shell * shell
        - (f64::from(c[1]) - p(1)).powi(2)
        - (f64::from(c[2]) - p(2)).powi(2);
    if !in_box || rest < 0.0 {
        return false;
    }
    let mid = (f64::from(c[0]) + 0.5 * f64::from(H)) / f64::from(H);
    let half = rest.sqrt() / f64::from(H);
    ((mid - half).floor() as i32..=(mid + half).ceil() as i32).contains(&node[0])
}

/// The blended-sphere rule at world node `node` over every blob, R half the
/// band, and inside the solid at least the node's depth in it. A node no
/// blob's weight reaches reads its distance to the nearest round blob that
/// visits it, at most the band.
fn dense_level(blobs: &[[f32; 4]], node: [i32; 3], band: f32, solid: f32) -> Level {
    let p: [f64; 3] = std::array::from_fn(|a| (f64::from(node[a]) - 0.5) * f64::from(H));
    let blend = 0.5 * f64::from(band);
    let (mut weight, mut offset, mut radius, mut terms) = (0.0, [0.0f64; 3], 0.0, 0.0);
    let mut near = f64::from(band);
    for b in blobs.iter().filter(|b| b[3] > 0.0) {
        let d: [f64; 3] = std::array::from_fn(|a| (f64::from(b[a]) - p[a]) / blend);
        let s2 = d.iter().map(|x| x * x).sum::<f64>();
        if s2 >= 1.0 {
            if visits([b[0], b[1], b[2]], blend as f32, node) {
                near = near.min(s2.sqrt() * blend - f64::from(b[3]));
            }
            continue;
        }
        let w = (1.0 - s2).powi(3);
        weight += w;
        for a in 0..3 {
            offset[a] += w * d[a];
        }
        radius += w * f64::from(b[3]) / blend;
        terms += 1.0;
    }
    let slack = terms / FIXED;
    let far = (near, 1.0 / FIXED);
    let blended = if weight > 0.0 {
        let off = offset
            .iter()
            .map(|o| (o / weight).powi(2))
            .sum::<f64>()
            .sqrt();
        (
            ((off - radius / weight) * blend).min(f64::from(band)),
            3.0 * blend * slack / weight,
        )
    } else {
        far
    };
    let mut level = if (weight - MIN_WEIGHT).abs() <= slack {
        Level {
            value: f32::NAN,
            lo: (far.0 - far.1).min(blended.0 - blended.1),
            hi: (far.0 + far.1).max(blended.0 + blended.1),
        }
    } else {
        let (phi, tolerance) = if weight >= MIN_WEIGHT { blended } else { far };
        Level {
            value: phi as f32,
            lo: phi - tolerance,
            hi: phi + tolerance,
        }
    };
    if solid < 0.0 {
        level = level.at_least(f64::from(-solid - WALL_REACH_CELLS * H));
    }
    level
}

impl Level {
    /// The level raised to `floor`, as the wall rule raises a node in a solid.
    fn at_least(self, floor: f64) -> Self {
        Level {
            value: if self.value.is_nan() {
                f32::NAN
            } else {
                self.value.max(floor as f32)
            },
            lo: self.lo.max(floor),
            hi: self.hi.max(floor),
        }
    }
}

/// Every node of the box (x fastest) under the dense rule.
fn dense_box(blobs: &[[f32; 4]], solid: &[f32], band: f32) -> Vec<Level> {
    let mut out = Vec::with_capacity(solid.len());
    for z in 0..DIMS[2] {
        for y in 0..DIMS[1] {
            for x in 0..DIMS[0] {
                let node = [
                    ORIGIN[0] + x as i32,
                    ORIGIN[1] + y as i32,
                    ORIGIN[2] + z as i32,
                ];
                let i = (x + DIMS[0] * (y + DIMS[1] * z)) as usize;
                out.push(dense_level(blobs, node, band, solid[i]));
            }
        }
    }
    out
}

/// The least and most the shader's value may be at each node.
fn bounds(levels: &[Level]) -> Vec<(f64, f64)> {
    levels.iter().map(|l| (l.lo, l.hi)).collect()
}

/// The dense values, NaN where the rule cannot say.
fn values(levels: &[Level]) -> Vec<f32> {
    levels.iter().map(|l| l.value).collect()
}

/// The solid's distance at every node of the box: negative below `y_edge` cells.
fn floor_solid(y_edge: f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(DIMS.iter().product::<u32>() as usize);
    for _z in 0..DIMS[2] {
        for y in 0..DIMS[1] {
            for _x in 0..DIMS[0] {
                out.push(((ORIGIN[1] + y as i32) as f32 - 0.5 - y_edge) * H);
            }
        }
    }
    out
}

/// `node.particle_volume` over the sorted blobs and ranges on `words`' pool.
fn run(
    words: &[u32],
    pool_tiles: u32,
    blobs: &[FluidBlob],
    ranges: &[CellRange],
    solid: &[f32],
    blob_reach: f32,
) -> Vec<f32> {
    let len = pool_len(pool_tiles) as usize;
    let mut harness = Harness::new();
    let (blobs_slot, _) = harness.array(blobs, blobs.len());
    let (ranges_slot, _) = harness.array(ranges, ranges.len());
    let (solid_slot, _) = harness.array(solid, len);
    let (tiles_slot, _) = harness.array(words, words.len());
    // Bulk water's neighbour counts, so no blob beside the solid draws as a
    // film disc: the dense box models plain blobs.
    let (counts_slot, _) = harness.array(&vec![BULK_COUNT; blobs.len()], blobs.len());
    let (sorted_slot, _) = harness.array(&vec![FluidParticle::zeroed(); blobs.len()], blobs.len());
    // An element the node leaves unwritten stays NaN and fails every compare.
    let (out_slot, out_buffer) = harness.array(&vec![f32::NAN; len], len);
    let (weights_slot, _) = harness.array(&vec![f32::NAN; len], len);
    let (velocity_slot, _) = harness.array(&vec![f32::NAN; 3 * len], 3 * len);
    let (_, errors) = harness.run(
        &mut ParticleVolume::new(),
        &[
            ("blobs", blobs_slot),
            ("cell_ranges", ranges_slot),
            ("solid", solid_slot),
            ("tiles", tiles_slot),
            ("counts", counts_slot),
            ("sorted", sorted_slot),
        ],
        &[
            ("levelset", out_slot),
            ("weights", weights_slot),
            ("velocity", velocity_slot),
        ],
        &params(&[
            ("cell_size", H),
            // The reach is the product of the three; the scale carries it,
            // over a simulation cell of one display cell.
            ("particle_scale", blob_reach / H),
            ("sim_cell_size", H),
            ("particle_radius", 1.0),
            ("pool_tiles", pool_tiles as f32),
        ]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    read(&out_buffer, len)
}

/// Every GPU value within its bounds, give or take f32 rounding.
fn assert_all_close(got: &[f32], want: &[(f64, f64)]) {
    assert_eq!(got.len(), want.len());
    for (i, (&g, &(lo, hi))) in got.iter().zip(want).enumerate() {
        let slack = 1.0e-6 + 1.0e-5 * f64::from(g).abs();
        assert!(
            (lo - slack..=hi + slack).contains(&f64::from(g)),
            "node {i}: gpu {g} vs dense {lo}..{hi}"
        );
    }
}

/// One blob each side of the face between world cells -1 and 0 (the face
/// between tiles -1 and 0), with a solid clipping the left one: the field is
/// the dense box's.
#[test]
fn blobs_each_side_of_a_tile_face_match_a_dense_box() {
    let table = box_table();
    // Positions in cells, off every support-box edge. A sits in cell -1 of
    // tile -1, B in cell 0 of tile 0.
    let a = [-0.8 * H, 10.35 * H, 6.4 * H, 1.7 * H];
    let b = [-0.2 * H, 10.52 * H, 6.15 * H, 1.4 * H];
    let blob_reach = 1.7 * H;
    let blobs = [a, b];
    let (sorted, ranges) = sort_blobs(&blobs, pool_len(table.pool_tiles) as usize, |w| {
        box_index(&table, w)
    });
    // The solid fills x below -1 cell: the left blob's interior is clipped.
    let solid_dense: Vec<f32> = (0..DIMS.iter().product::<u32>())
        .map(|i| ((ORIGIN[0] + (i % DIMS[0]) as i32) as f32 - 0.5 + 1.0) * H)
        .collect();
    let got = table.from_pool(&run(
        &table.words,
        table.pool_tiles,
        &sorted,
        &ranges,
        &table.to_pool(&solid_dense, NO_SOLID),
        blob_reach,
    ));
    let band = 3.0 * blob_reach;
    let want = dense_box(&blobs, &solid_dense, band);
    assert_all_close(&got, &bounds(&want));

    // The face matters: A, centred in tile -1, reaches nodes of tile 0, and
    // B, centred in tile 0, nodes of tile -1; the solid clips and the rest is
    // band.
    let want = values(&want);
    let only = |blob: &[[f32; 4]]| values(&dense_box(blob, &solid_dense, band));
    let (only_a, only_b) = (only(&[a]), only(&[b]));
    let in_tile_zero = |i: usize| (i % DIMS[0] as usize) as i32 + ORIGIN[0] >= 0;
    assert!(
        (0..want.len()).any(|i| in_tile_zero(i) && only_a[i] < 0.0),
        "the left blob must reach across the face"
    );
    assert!((0..want.len()).any(|i| !in_tile_zero(i) && only_b[i] < band));
    assert!(want.iter().filter(|&&v| v < 0.0).count() > 10);
    let clipped = |solid: &[f32]| {
        (0..want.len())
            .filter(|&i| solid[i] < 0.0 && want[i] == -solid[i] - WALL_REACH_CELLS * H)
            .count()
    };
    assert!(clipped(&solid_dense) > 4, "the solid clips nothing");
    assert!(want.iter().filter(|&&v| v == band).count() > 1000);
}

/// A cloud of blobs of mixed radii, a floor clipping part of it, and the
/// reach set above the largest radius: the dense box's field, tile by tile.
#[test]
fn a_blob_cloud_matches_the_dense_box() {
    let table = box_table();
    let mut rng = Rng(0x2468_ace1);
    let blobs: Vec<[f32; 4]> = (0..300)
        .map(|_| {
            [
                rng.range(-5.4, 8.4) * H,
                rng.range(4.6, 21.4) * H,
                rng.range(-0.4, 12.4) * H,
                rng.range(0.4, 1.7) * H,
            ]
        })
        .collect();
    let (sorted, ranges) = sort_blobs(&blobs, pool_len(table.pool_tiles) as usize, |w| {
        box_index(&table, w)
    });
    let solid_dense = floor_solid(8.2);
    let blob_reach = 1.8 * H;
    let got = table.from_pool(&run(
        &table.words,
        table.pool_tiles,
        &sorted,
        &ranges,
        &table.to_pool(&solid_dense, NO_SOLID),
        blob_reach,
    ));
    let want = dense_box(&blobs, &solid_dense, 3.0 * blob_reach);
    assert_all_close(&got, &bounds(&want));
    let want = values(&want);
    assert!(want.iter().filter(|&&v| v < 0.0).count() > 500);
    // Nodes in the floor a blob reaches read their depth in it less the
    // half cell water reaches into a solid.
    let clipped = (0..want.len())
        .filter(|&i| solid_dense[i] < 0.0 && want[i] == -solid_dense[i] - WALL_REACH_CELLS * H)
        .count();
    assert!(clipped > 50, "{clipped}");
    assert!(want.iter().any(|&v| v > 0.0 && v < 3.0 * blob_reach));
}

/// Held tiles no blob visits read the band, a free slot and the canonical
/// element read the far value (three cells), and a tile's own nodes match
/// the dense rule for a blob in it.
#[test]
fn air_reads_the_band_and_free_slots_and_canon_read_far() {
    // Four slots, tile (0,0,0) in slot 0 and tile (-1,0,0) in slot 2; slots 1
    // and 3 are free.
    let words = table_words_of(4, &[([0, 0, 0], 0), ([-1, 0, 0], 2)]);
    let len = pool_len(4) as usize;
    let radius = 1.2 * H;
    let blob_reach = 1.5 * H;
    // A blob at the centre of cell (3, 3, 3): its support box stays in tile 0,
    // so tile -1 is held air.
    let blob = [3.0 * H, 3.0 * H, 3.0 * H, radius];
    let mut ranges = vec![CellRange::default(); len];
    ranges[local_index([3, 3, 3]) as usize] = CellRange { start: 0, count: 1 };
    let blobs = [
        FluidBlob::round([blob[0], blob[1], blob[2]], blob[3]),
        FluidBlob::default(),
    ];
    let got = run(&words, 4, &blobs, &ranges, &vec![NO_SOLID; len], blob_reach);
    let far = FIELD_FAR_CELLS * H;
    let band = 3.0 * blob_reach;
    for slot in [1usize, 3] {
        for (e, &value) in got.iter().enumerate().skip(slot * 512).take(512) {
            assert_eq!(value, far, "free slot {slot} element {e}");
        }
    }
    assert_eq!(got[len - 1], far, "the canonical element");
    // Tile (-1,0,0) is out of the blob's reach: all band.
    for (e, &value) in got.iter().enumerate().skip(2 * 512).take(512) {
        assert_eq!(value, band, "held air element {e}");
    }
    // Tile (0,0,0): the dense rule at each of its nodes.
    let mut touched = 0;
    for (e, &value) in got.iter().enumerate().take(512) {
        let node = [(e & 7) as i32, ((e >> 3) & 7) as i32, (e >> 6) as i32];
        let level = dense_level(&[blob], node, band, NO_SOLID);
        let slack = 1.0e-6 + 1.0e-5 * f64::from(value).abs();
        assert!(
            (level.lo - slack..=level.hi + slack).contains(&f64::from(value)),
            "tile node {node:?}: gpu {value} vs dense {}..{}",
            level.lo,
            level.hi
        );
        touched += usize::from(value < band);
    }
    assert!(touched > 20, "the blob must touch the tile");
}
