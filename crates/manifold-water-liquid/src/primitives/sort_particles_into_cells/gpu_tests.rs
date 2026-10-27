use super::SortParticlesIntoCells;
use crate::fluid_particles::CellRange;
use crate::testkit::world_box::{BoxTable, pool_index_under, table_words_of};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::scene::world_tiles::pool_len;
use manifold_node_engine::testkit::array_harness::{Harness, params, read};

const H: f32 = 0.25;

/// A live particle at `position` whose id is `id`.
fn particle(position: [f32; 3], id: u32) -> FluidParticle {
    FluidParticle {
        position_radius: [position[0], position[1], position[2], 0.01],
        velocity: [0.0; 3],
        id,
    }
}

/// Cell `p` is centred on `p·H`, so a point's cell is `floor(x / H + 1/2)`.
/// The cell's pool index under the table, or `None` when its tile is not
/// held.
fn key_of(words: &[u32], pool_tiles: u32, p: &FluidParticle) -> Option<usize> {
    let cell: [i32; 3] =
        std::array::from_fn(|a| ((p.position_radius[a] + 0.5 * H) / H).floor() as i32);
    let key = pool_index_under(words, pool_tiles, cell);
    (key != pool_len(pool_tiles) as usize - 1).then_some(key)
}

struct Sorted {
    sorted: Vec<FluidParticle>,
    order: Vec<u32>,
    ranges: Vec<CellRange>,
}

/// The first `count` of `particles` through the node on the table `words`,
/// checked against a stable CPU sort by pool index with the particles of
/// absent tiles left out.
fn sort(
    h: &mut Harness,
    words: &[u32],
    pool_tiles: u32,
    particles: &[FluidParticle],
    count: Option<f32>,
) -> Sorted {
    let n = particles.len();
    let len = pool_len(pool_tiles) as usize;
    let (particles_slot, _) = h.array(particles, n);
    let (tiles_slot, _) = h.array(words, words.len());
    let (sorted_slot, sorted_buf) = h.array::<FluidParticle>(&[], n);
    let (order_slot, order_buf) = h.array::<u32>(&[], n);
    let (ranges_slot, _) = h.array::<CellRange>(&[], 1);
    let mut inputs = vec![("particles", particles_slot), ("tiles", tiles_slot)];
    if let Some(count) = count {
        inputs.push(("count", h.scalar_input(count)));
    }
    let (_, errors) = h.run(
        &mut SortParticlesIntoCells::new(),
        &inputs,
        &[
            ("sorted", sorted_slot),
            ("cell_ranges", ranges_slot),
            ("order", order_slot),
        ],
        &params(&[("cell_size", H), ("pool_tiles", pool_tiles as f32)]),
    );
    assert!(errors.is_empty(), "{errors:?}");
    let got = Sorted {
        sorted: read(&sorted_buf, n),
        order: read(&order_buf, n),
        ranges: read(&h.buffer(ranges_slot), len),
    };

    let sorted_count = count.map_or(n, |c| c as usize);
    let keys: Vec<Option<usize>> = particles[..sorted_count]
        .iter()
        .map(|p| key_of(words, pool_tiles, p))
        .collect();
    let mut kept: Vec<usize> = (0..sorted_count).filter(|&i| keys[i].is_some()).collect();
    kept.sort_by_key(|&i| keys[i]);
    for slot in 0..n {
        match kept.get(slot) {
            Some(&i) => {
                assert_eq!(got.order[slot], i as u32, "order slot {slot}");
                assert_eq!(got.sorted[slot], particles[i], "sorted slot {slot}");
            }
            None => {
                assert_eq!(got.order[slot], u32::MAX, "order slot {slot}");
                assert_eq!(
                    got.sorted[slot],
                    FluidParticle::default(),
                    "sorted slot {slot}"
                );
            }
        }
    }
    for (k, range) in got.ranges.iter().enumerate() {
        let want = CellRange {
            start: kept.iter().filter(|&&i| keys[i].unwrap() < k).count() as u32,
            count: kept.iter().filter(|&&i| keys[i] == Some(k)).count() as u32,
        };
        assert_eq!(*range, want, "range of pool element {k}");
    }
    got
}

/// The table of world cells -6..=5 on x (tiles -1 and 0) and 0..=7 on y and
/// z (one tile): two held tiles in slots 0 and 1 and a free slot 2.
fn table() -> (Vec<u32>, u32) {
    let table = BoxTable::new([-6, 0, 0], [12, 8, 8]);
    let pool_tiles = table.pool_tiles + 1;
    (table_words_of(pool_tiles, &table.held()), pool_tiles)
}

fn particles() -> Vec<FluidParticle> {
    [
        // Either side of the face between tiles -1 and 0 (x = -1/2 cell).
        [-0.12, 0.5, 0.5],
        [-0.13, 0.5, 0.5],
        // Tile (10, 0, 0) is not held.
        [20.0, 0.5, 0.5],
        // The same cell as the first, entered later.
        [-0.11, 0.55, 0.45],
        [-0.2, 0.5, 0.5],
        // Tile (0, -1, 0) is not held.
        [0.5, -1.0, 0.5],
        [1.0, 0.5, 0.5],
        // The last cell of the box.
        [1.2, 1.7, 1.7],
    ]
    .iter()
    .enumerate()
    .map(|(i, &position)| particle(position, i as u32 + 1))
    .collect()
}

#[test]
fn particles_sort_into_the_cells_of_their_tiles_and_absent_tiles_drop() {
    let (words, pool_tiles) = table();
    let mut h = Harness::new();
    let got = sort(&mut h, &words, pool_tiles, &particles(), None);
    // Cells (-1,2,2) and (0,2,2) are in slots 0 and 1 (tiles -1 and 0): pool
    // indices 7 + 8·18 and 512 + 0 + 8·18; the particles on either side of
    // the face are in different cells, the later ones in input order.
    assert_eq!(got.order[..6], [1_u32, 4, 0, 3, 6, 7]);
    assert_eq!(got.order[6..], [u32::MAX; 2]);
    let ids: Vec<u32> = got.sorted.iter().map(|p| p.id).collect();
    assert_eq!(ids, [2_u32, 5, 1, 4, 7, 8, 0, 0]);
    let range = |k: usize| (got.ranges[k].start, got.ranges[k].count);
    assert_eq!(range(151), (0, 2));
    assert_eq!(range(656), (2, 2));
    assert_eq!(range(660), (4, 1));
    assert_eq!(range(1021), (5, 1));
    // The two particles of absent tiles are in no range: the canonical
    // element's, and the free slot's before it, are empty.
    assert_eq!(range(1536), (6, 0));
    assert!(got.ranges[1024..1536].iter().all(|r| r.count == 0));
    assert_eq!(got.ranges.iter().map(|r| r.count).sum::<u32>(), 6);
}

#[test]
fn a_count_limits_the_sort_to_the_first_particles() {
    let (words, pool_tiles) = table();
    let mut h = Harness::new();
    let got = sort(&mut h, &words, pool_tiles, &particles(), Some(4.0));
    assert_eq!(got.order[..3], [1_u32, 0, 3]);
    assert_eq!(got.order[3..], [u32::MAX; 5]);
}
