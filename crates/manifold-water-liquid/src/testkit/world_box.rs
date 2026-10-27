//! CPU world tables for test rigs: the words of a table
//! (`manifold_node_engine::scene::world_tiles` layout) for tiles the test
//! picks, and a dense box of cells placed in world cells as a pooled field
//! under such a table. The twin of what the GPU build writes, without the
//! build: the test chooses which slot holds which tile. A pooled field a
//! GPU build laid out reads back into a dense box through its own table
//! ([`from_pool_under`]).

use manifold_node_engine::scene::world_tiles::{
    EMPTY, NONE, TILE, TILE_CELLS, find, insert, local_index, neighbour_index, neighbour_row_word,
    pool_len, slot_row_word, table_entries, table_words, tile_key, tile_of_cell,
};

/// The table words of a pool of `pool_tiles` slots holding `held`, a (tile,
/// slot) each: the keys, each entry's slot, each held slot's tile and entry
/// and its 27 neighbour slots. Every other slot is free, every absent
/// neighbour `NONE`. Panics on a slot past the pool or a full table.
pub fn table_words_of(pool_tiles: u32, held: &[([i32; 3], u32)]) -> Vec<u32> {
    let entries = table_entries(pool_tiles);
    let h = entries as usize;
    let mut words = vec![NONE; table_words(pool_tiles) as usize];
    let mut keys = vec![EMPTY; h];
    for &(tile, slot) in held {
        assert!(
            slot < pool_tiles,
            "slot {slot} is past the {pool_tiles}-tile pool"
        );
        assert!(insert(&mut keys, tile_key(tile)), "the table is full");
    }
    words[..h].copy_from_slice(&keys);
    for &(tile, slot) in held {
        let entry = find(&keys, tile_key(tile)).expect("an inserted key is found");
        words[h + entry as usize] = slot;
        let row = slot_row_word(entries, slot) as usize;
        for (a, c) in tile.into_iter().enumerate() {
            words[row + a] = c as u32;
        }
        words[row + 3] = entry;
        let neighbours = neighbour_row_word(entries, pool_tiles, slot) as usize;
        for &(other, other_slot) in held {
            let d: [i32; 3] = std::array::from_fn(|a| other[a] - tile[a]);
            if d.iter().all(|c| c.abs() <= 1) {
                words[neighbours + neighbour_index(d) as usize] = other_slot;
            }
        }
    }
    words
}

/// A dense box of `cells` cells whose cell 0 is world cell `origin`, stored
/// as the tiles it touches: tile `tile_min` first, slots in x-fastest tile
/// order. Cells of those tiles outside the box, and every absent tile, hold
/// the canonical value.
#[derive(Clone, Debug)]
pub struct BoxTable {
    pub origin: [i32; 3],
    pub cells: [u32; 3],
    pub tile_min: [i32; 3],
    pub tiles: [u32; 3],
    /// P: one slot per tile the box touches.
    pub pool_tiles: u32,
    /// The table words, `table_words(pool_tiles)` of them.
    pub words: Vec<u32>,
}

impl BoxTable {
    pub fn new(origin: [i32; 3], cells: [u32; 3]) -> Self {
        assert!(cells.iter().all(|&n| n > 0), "a box holds a cell");
        let last: [i32; 3] = std::array::from_fn(|a| origin[a] + cells[a] as i32 - 1);
        let tile_min = tile_of_cell(origin);
        let tile_max = tile_of_cell(last);
        let tiles: [u32; 3] = std::array::from_fn(|a| (tile_max[a] - tile_min[a] + 1) as u32);
        let pool_tiles: u32 = tiles.iter().product();
        let mut table = Self {
            origin,
            cells,
            tile_min,
            tiles,
            pool_tiles,
            words: Vec::new(),
        };
        table.words = table_words_of(pool_tiles, &table.held());
        table
    }

    /// Every (tile, slot) the box holds: tile `tile_min` first, x fastest.
    pub fn held(&self) -> Vec<([i32; 3], u32)> {
        (0..self.pool_tiles)
            .map(|slot| {
                let t = [
                    slot % self.tiles[0],
                    slot / self.tiles[0] % self.tiles[1],
                    slot / (self.tiles[0] * self.tiles[1]),
                ];
                (
                    std::array::from_fn(|a| self.tile_min[a] + t[a] as i32),
                    slot,
                )
            })
            .collect()
    }

    /// The pool index of box cell `c` (counted from the box's cell 0).
    pub fn pool_index(&self, c: [u32; 3]) -> usize {
        let p: [i32; 3] = std::array::from_fn(|a| self.origin[a] + c[a] as i32);
        let t = tile_of_cell(p);
        let rel: [u32; 3] = std::array::from_fn(|a| (t[a] - self.tile_min[a]) as u32);
        let slot = rel[0] + self.tiles[0] * (rel[1] + self.tiles[1] * rel[2]);
        slot as usize * TILE_CELLS as usize + local_index(p) as usize
    }

    /// `dense` (x fastest) as a pooled field: `pool_len` elements, the box's
    /// cells in place, everything else `canon`.
    pub fn to_pool<T: Copy>(&self, dense: &[T], canon: T) -> Vec<T> {
        assert_eq!(
            dense.len() as u64,
            self.cells.iter().map(|&n| u64::from(n)).product::<u64>()
        );
        let mut pool = vec![canon; pool_len(self.pool_tiles) as usize];
        for (c, &value) in box_cells(self.cells).zip(dense) {
            pool[self.pool_index(c)] = value;
        }
        pool
    }

    /// The box's cells (x fastest) out of a pooled field.
    pub fn from_pool<T: Copy>(&self, pool: &[T]) -> Vec<T> {
        assert_eq!(pool.len() as u64, pool_len(self.pool_tiles));
        box_cells(self.cells)
            .map(|c| pool[self.pool_index(c)])
            .collect()
    }
}

/// The pool index of world cell `p` under the table `words` of a pool of
/// `pool_tiles` slots: its slot's element, or the canonical element (the
/// last) when the table holds no slot for its tile.
pub fn pool_index_under(words: &[u32], pool_tiles: u32, p: [i32; 3]) -> usize {
    let entries = table_entries(pool_tiles) as usize;
    match find(&words[..entries], tile_key(tile_of_cell(p))) {
        Some(entry) => {
            words[entries + entry as usize] as usize * TILE_CELLS as usize + local_index(p) as usize
        }
        None => pool_len(pool_tiles) as usize - 1,
    }
}

/// A box of `dims` cells whose cell 0 is world cell `origin` (x fastest) out
/// of a pooled field of a pool of `pool_tiles` slots, read through any
/// table's `words` (a GPU build's among them).
pub fn from_pool_under<T: Copy>(
    pool: &[T],
    words: &[u32],
    pool_tiles: u32,
    origin: [i32; 3],
    dims: [u32; 3],
) -> Vec<T> {
    assert!(pool.len() as u64 >= pool_len(pool_tiles), "a pooled field");
    assert!(
        words.len() as u64 >= table_words(pool_tiles),
        "the pool's table"
    );
    box_cells(dims)
        .map(|c| {
            let p: [i32; 3] = std::array::from_fn(|a| origin[a] + c[a] as i32);
            pool[pool_index_under(words, pool_tiles, p)]
        })
        .collect()
}

/// Every cell of a box of `n` cells, x fastest.
fn box_cells(n: [u32; 3]) -> impl Iterator<Item = [u32; 3]> {
    (0..n[2]).flat_map(move |z| (0..n[1]).flat_map(move |y| (0..n[0]).map(move |x| [x, y, z])))
}

const _: () = assert!(TILE == 8, "pool_index counts tiles of 8 cells");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_across_the_origin_round_trips_through_its_pool() {
        // Cells -3..=8 on x touch tiles -1, 0 and 1; y and z stay in one
        // tile each, z below the origin.
        let table = BoxTable::new([-3, 2, -5], [12, 5, 4]);
        assert_eq!(table.tile_min, [-1, 0, -1]);
        assert_eq!(table.tiles, [3, 1, 1]);
        assert_eq!(table.pool_tiles, 3);
        let dense: Vec<u32> = (0..12 * 5 * 4).collect();
        let pool = table.to_pool(&dense, u32::MAX);
        assert_eq!(pool.len() as u64, pool_len(3));
        assert_eq!(table.from_pool(&pool), dense);
        // Cell 0 is world cell (-3, 2, -5): tile (-1, 0, -1) is slot 0 at
        // local (5, 2, 3).
        assert_eq!(table.pool_index([0, 0, 0]), local_index([5, 2, 3]) as usize);
        // World cell (5, 2, -5) is in tile (0, 0, -1): slot 1.
        assert_eq!(
            table.pool_index([8, 0, 0]),
            512 + local_index([5, 2, 3]) as usize
        );
        // A cell of a touched tile outside the box keeps the canonical value.
        assert_eq!(pool[local_index([0, 0, 0]) as usize], u32::MAX);
    }

    #[test]
    fn the_words_hold_each_slots_tile_and_neighbours() {
        let words = table_words_of(3, &[([0, 0, 0], 2), ([-1, 0, 0], 0), ([5, 5, 5], 1)]);
        let h = table_entries(3);
        assert_eq!(words.len() as u64, table_words(3));
        let tile = |slot: u32| {
            let at = slot_row_word(h, slot) as usize;
            [words[at] as i32, words[at + 1] as i32, words[at + 2] as i32]
        };
        assert_eq!(
            [tile(0), tile(1), tile(2)],
            [[-1, 0, 0], [5, 5, 5], [0, 0, 0]]
        );
        let row = |slot: u32, d: [i32; 3]| {
            words[neighbour_row_word(h, 3, slot) as usize + neighbour_index(d) as usize]
        };
        assert_eq!(row(2, [-1, 0, 0]), 0);
        assert_eq!(row(0, [1, 0, 0]), 2);
        assert_eq!(row(2, [0, 0, 0]), 2);
        assert_eq!(row(1, [-1, 0, 0]), NONE);
        assert_eq!(row(2, [1, 1, 1]), NONE);
    }
}
