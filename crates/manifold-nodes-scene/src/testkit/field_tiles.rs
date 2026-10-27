//! CPU world tables for the renderer's water-field proofs: the words of a
//! table (`manifold_node_engine::scene::world_tiles` layout) for the tiles a
//! test picks, a pooled field laid out under it, and the extent a GPU build
//! would publish. The test chooses which slot holds which tile, so a proof can
//! straddle the origin and leave a tile unheld between two that are.

use manifold_node_engine::scene::world_tiles::{
    EMPTY, NONE, TILE, TILE_CELLS, find, insert, local_index, neighbour_index, neighbour_row_word,
    pool_len, slot_row_word, table_entries, table_words, tile_key, tile_of_cell,
};

/// A pool of `pool_tiles` slots holding `held` (slot i holds `held[i]`), of
/// display cells `cell` metres wide: node p sits at `(p - 1/2) * cell`.
#[derive(Clone, Debug)]
pub struct FieldTable {
    pub cell: f32,
    pub pool_tiles: u32,
    pub held: Vec<[i32; 3]>,
    /// The table words, `table_words(pool_tiles)` of them. Every slot past
    /// `held` is free, every absent neighbour `NONE`.
    pub words: Vec<u32>,
}

impl FieldTable {
    /// Panics on more tiles than slots, a repeated tile or a full table.
    pub fn new(cell: f32, pool_tiles: u32, held: &[[i32; 3]]) -> Self {
        assert!(held.len() as u32 <= pool_tiles, "a slot for every tile");
        let entries = table_entries(pool_tiles);
        let h = entries as usize;
        let mut words = vec![NONE; table_words(pool_tiles) as usize];
        let mut keys = vec![EMPTY; h];
        for &tile in held {
            assert!(insert(&mut keys, tile_key(tile)), "the table is full");
        }
        words[..h].copy_from_slice(&keys);
        for (slot, &tile) in held.iter().enumerate() {
            let slot = slot as u32;
            let entry = find(&keys, tile_key(tile)).expect("an inserted key is found");
            assert_eq!(words[h + entry as usize], NONE, "tile {tile:?} held twice");
            words[h + entry as usize] = slot;
            let row = slot_row_word(entries, slot) as usize;
            for (axis, c) in tile.into_iter().enumerate() {
                words[row + axis] = c as u32;
            }
            words[row + 3] = entry;
            let neighbours = neighbour_row_word(entries, pool_tiles, slot) as usize;
            for (other_slot, other) in held.iter().enumerate() {
                let d: [i32; 3] = std::array::from_fn(|a| other[a] - tile[a]);
                if d.iter().all(|c| c.abs() <= 1) {
                    words[neighbours + neighbour_index(d) as usize] = other_slot as u32;
                }
            }
        }
        Self {
            cell,
            pool_tiles,
            held: held.to_vec(),
            words,
        }
    }

    /// Elements of a pooled field under this table.
    pub fn elements(&self) -> usize {
        pool_len(self.pool_tiles) as usize
    }

    /// The pool element of node `p`: its slot's, or the canonical (last) one
    /// when the table holds no slot for its tile.
    pub fn pool_index(&self, p: [i32; 3]) -> usize {
        let tile = tile_of_cell(p);
        match self.held.iter().position(|&held| held == tile) {
            Some(slot) => slot * TILE_CELLS as usize + local_index(p) as usize,
            None => self.elements() - 1,
        }
    }

    /// A pooled field: every node of a held tile is `node(p)`, everything else
    /// (free slots, the canonical element) is `canon`.
    pub fn pool<T: Copy>(&self, canon: T, node: impl Fn([i32; 3]) -> T) -> Vec<T> {
        let mut pool = vec![canon; self.elements()];
        for (slot, tile) in self.held.iter().enumerate() {
            for local in 0..TILE_CELLS {
                let l = [local % TILE, local / TILE % TILE, local / (TILE * TILE)];
                let p: [i32; 3] = std::array::from_fn(|a| tile[a] * TILE as i32 + l[a] as i32);
                pool[slot * TILE_CELLS as usize + local as usize] = node(p);
            }
        }
        pool
    }

    /// The position of node `p` in the object's space.
    pub fn position(&self, p: [i32; 3]) -> [f32; 3] {
        p.map(|c| (c as f32 - 0.5) * self.cell)
    }

    /// The extent a build publishes: the held tiles' node bounds (min xyz, 0,
    /// max xyz, 0), min above max when none is held.
    pub fn extent(&self) -> [f32; 8] {
        let Some(&first) = self.held.first() else {
            return [1e30, 1e30, 1e30, 0.0, -1e30, -1e30, -1e30, 0.0];
        };
        let (mut lo, mut hi) = (first, first);
        for tile in &self.held {
            for a in 0..3 {
                lo[a] = lo[a].min(tile[a]);
                hi[a] = hi[a].max(tile[a]);
            }
        }
        let low = self.position(lo.map(|t| t * TILE as i32));
        let high = self.position(hi.map(|t| t * TILE as i32 + TILE as i32 - 1));
        [low[0], low[1], low[2], 0.0, high[0], high[1], high[2], 0.0]
    }

    /// Every tile of the box `lo..=hi` (tile coordinates), x fastest.
    pub fn tiles_between(lo: [i32; 3], hi: [i32; 3]) -> Vec<[i32; 3]> {
        let mut tiles = Vec::new();
        for z in lo[2]..=hi[2] {
            for y in lo[1]..=hi[1] {
                for x in lo[0]..=hi[0] {
                    tiles.push([x, y, z]);
                }
            }
        }
        tiles
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pool_reads_through_its_table_and_leaves_absent_tiles_canonical() {
        // Tiles -1 and 1 along x are held, tile 0 between them is not.
        let held = [[-1, 0, 0], [1, 0, 0], [0, 0, -3]];
        let table = FieldTable::new(0.0625, 4, &held);
        assert_eq!(table.words.len() as u64, table_words(4));
        let pool = table.pool(-7.0f32, |p| (p[0] + 100 * p[1] + 10_000 * p[2]) as f32);
        assert_eq!(pool.len(), table.elements());
        // Node (-1, 2, 3) is in tile -1: slot 0, local (7, 2, 3).
        assert_eq!(
            table.pool_index([-1, 2, 3]),
            local_index([7, 2, 3]) as usize
        );
        assert_eq!(
            pool[table.pool_index([-1, 2, 3])],
            (-1 + 200 + 30_000) as f32
        );
        // Node (9, 0, 0) is in tile 1: slot 1.
        assert_eq!(
            table.pool_index([9, 0, 0]),
            TILE_CELLS as usize + local_index([1, 0, 0]) as usize
        );
        // Tile 0 is absent: its nodes are the canonical element.
        assert_eq!(table.pool_index([3, 3, 3]), table.elements() - 1);
        assert_eq!(pool[table.elements() - 1], -7.0);
        // The free fourth slot holds the canonical value too.
        assert!(pool[3 * TILE_CELLS as usize..].iter().all(|&v| v == -7.0));
        // The words find each tile where the build would put it.
        let h = table_entries(4) as usize;
        for (slot, &tile) in held.iter().enumerate() {
            let entry = find(&table.words[..h], tile_key(tile)).expect("held tile found");
            assert_eq!(table.words[h + entry as usize], slot as u32);
        }
        assert_eq!(find(&table.words[..h], tile_key([0, 0, 0])), None);
    }

    #[test]
    fn the_extent_spans_the_held_tiles_nodes() {
        let table = FieldTable::new(0.5, 3, &[[-1, 0, 0], [1, 2, -1], [0, 1, 0]]);
        // Lowest node x: tile -1 starts at node -8, at (-8 - 1/2) * 0.5.
        assert_eq!(table.extent()[0], -4.25);
        // Highest node y: tile 2 ends at node 23, at (23 - 1/2) * 0.5.
        assert_eq!(table.extent()[5], 11.25);
        let empty = FieldTable::new(0.5, 1, &[]).extent();
        assert!(empty[0] > empty[4] && empty[1] > empty[5] && empty[2] > empty[6]);
    }
}
