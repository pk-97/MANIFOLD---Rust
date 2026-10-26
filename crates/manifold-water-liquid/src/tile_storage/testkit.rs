//! Dense <-> tile-pool conversion for test rigs that bind dense CPU arrays to
//! kernels that index the pool. `to_pool`/`from_pool` use the identity table
//! (every box tile holds the slot of its own index, `P = T`), so a rig's CPU
//! reference keeps working on dense arrays. The `_with` variants take the
//! slot table a real step allocated: slot of tile `t` is `table[t]`, and
//! `TS_NONE` (`u32::MAX`) tiles read the canonical entry.

use super::{TILE, TILE_CELLS, tile_counts};

const TS_NONE: u32 = u32::MAX;

/// The pool index of cell `p` (inside the lattice) for a tile in `slot`.
fn pool_cell(p: [u32; 3], cells: [u32; 3], slot_of: &[u32]) -> Option<usize> {
    let t = tile_counts(cells);
    let tile = (p[0] >> 3) + t[0] * ((p[1] >> 3) + t[1] * (p[2] >> 3));
    let slot = slot_of[tile as usize];
    if slot == TS_NONE {
        return None;
    }
    let local = (p[0] & 7) + TILE * ((p[1] & 7) + TILE * (p[2] & 7));
    Some(slot as usize * TILE_CELLS as usize + local as usize)
}

/// The canonical mask of record `p` (0..=n per axis): bit `a` set when face
/// `a` of the record exists and lies on a box wall.
fn wall_mask(p: [u32; 3], cells: [u32; 3]) -> usize {
    let mut mask = 0usize;
    for a in 0..3 {
        let mut other = p;
        other[a] = 0;
        let exists = (0..3).all(|b| other[b] < cells[b]);
        if exists && (p[a] == 0 || p[a] == cells[a]) {
            mask |= 1 << a;
        }
    }
    mask
}

fn tile_count(cells: [u32; 3]) -> u32 {
    tile_counts(cells).iter().product()
}

fn identity_slots(cells: [u32; 3]) -> Vec<u32> {
    (0..tile_count(cells)).collect()
}

/// The identity table words (`slot_of[t] = t`, then `tile_of[s] = s`) and the
/// pool capacity `P = T`.
pub fn identity_table(cells: [u32; 3]) -> (Vec<u32>, u32) {
    let t = tile_count(cells);
    let mut words = Vec::with_capacity(2 * t as usize);
    words.extend(0..t);
    words.extend(0..t);
    (words, t)
}

fn blank_pool<T: Copy>(pool_tiles: usize, canon: &[T]) -> Vec<T> {
    assert!(
        canon.len() == 1 || canon.len() == 8,
        "canon is 1 or 8 entries"
    );
    let base = pool_tiles * TILE_CELLS as usize;
    let mut pool = vec![canon[0]; base + 8];
    for m in 0..8 {
        pool[base + m] = if canon.len() == 1 { canon[0] } else { canon[m] };
    }
    pool
}

/// The slot_of half of `table` for `cells` and the pool capacity.
fn split_table(table: &[u32], cells: [u32; 3]) -> (&[u32], usize) {
    let t = tile_count(cells) as usize;
    assert!(table.len() >= t, "table holds slot_of for every tile");
    (&table[..t], table.len() - t)
}

fn cells_to_pool<T: Copy>(
    dense: &[T],
    cells: [u32; 3],
    canon: &[T],
    slot_of: &[u32],
    p: usize,
) -> Vec<T> {
    assert_eq!(dense.len(), (cells[0] * cells[1] * cells[2]) as usize);
    let mut pool = blank_pool(p, canon);
    let mut i = 0;
    for z in 0..cells[2] {
        for y in 0..cells[1] {
            for x in 0..cells[0] {
                if let Some(at) = pool_cell([x, y, z], cells, slot_of) {
                    pool[at] = dense[i];
                }
                i += 1;
            }
        }
    }
    pool
}

fn cells_from_pool<T: Copy>(pool: &[T], cells: [u32; 3], slot_of: &[u32]) -> Vec<T> {
    let canon_base = pool.len() - 8;
    let mut dense = Vec::with_capacity((cells[0] * cells[1] * cells[2]) as usize);
    for z in 0..cells[2] {
        for y in 0..cells[1] {
            for x in 0..cells[0] {
                let at = pool_cell([x, y, z], cells, slot_of).unwrap_or(canon_base);
                dense.push(pool[at]);
            }
        }
    }
    dense
}

fn records_into_pool<T: Copy>(
    dense: &[T],
    cells: [u32; 3],
    canon: &[T],
    slot_of: &[u32],
    p: usize,
) -> Vec<T> {
    assert_eq!(canon.len(), 8, "records need 8 canonical entries");
    let m = [cells[0] + 1, cells[1] + 1, cells[2] + 1];
    assert_eq!(dense.len(), (m[0] * m[1] * m[2]) as usize);
    let mut pool = blank_pool(p, canon);
    let mut i = 0;
    for z in 0..m[2] {
        for y in 0..m[1] {
            for x in 0..m[0] {
                if x < cells[0]
                    && y < cells[1]
                    && z < cells[2]
                    && let Some(at) = pool_cell([x, y, z], cells, slot_of)
                {
                    pool[at] = dense[i];
                }
                i += 1;
            }
        }
    }
    pool
}

fn records_out_of_pool<T: Copy>(pool: &[T], cells: [u32; 3], slot_of: &[u32]) -> Vec<T> {
    let m = [cells[0] + 1, cells[1] + 1, cells[2] + 1];
    let canon_base = pool.len() - 8;
    let mut dense = Vec::with_capacity((m[0] * m[1] * m[2]) as usize);
    for z in 0..m[2] {
        for y in 0..m[1] {
            for x in 0..m[0] {
                let stored = if x < cells[0] && y < cells[1] && z < cells[2] {
                    pool_cell([x, y, z], cells, slot_of)
                } else {
                    None
                };
                let at = stored.unwrap_or_else(|| canon_base + wall_mask([x, y, z], cells));
                dense.push(pool[at]);
            }
        }
    }
    dense
}

/// A cell field (`n^3` dense, x fastest) in pool layout. `canon` is the
/// field's defined value (`len() == 1`) or its 8 canonical records. Unused
/// cells of partial edge tiles hold `canon[0]`.
pub fn to_pool<T: Copy>(dense: &[T], cells: [u32; 3], canon: &[T]) -> Vec<T> {
    let p = tile_count(cells) as usize;
    cells_to_pool(dense, cells, canon, &identity_slots(cells), p)
}

/// [`to_pool`] under a given slot table (`T + P` words); tiles the table
/// leaves absent are dropped.
pub fn to_pool_with<T: Copy>(dense: &[T], cells: [u32; 3], canon: &[T], table: &[u32]) -> Vec<T> {
    let (slot_of, p) = split_table(table, cells);
    cells_to_pool(dense, cells, canon, slot_of, p)
}

/// A pooled cell field back to its dense `n^3` array.
pub fn from_pool<T: Copy>(pool: &[T], cells: [u32; 3]) -> Vec<T> {
    cells_from_pool(pool, cells, &identity_slots(cells))
}

/// A pooled cell field a real step allocated back to its dense array; cells
/// of absent tiles read the canonical entry.
pub fn from_pool_with<T: Copy>(pool: &[T], table: &[u32], cells: [u32; 3]) -> Vec<T> {
    cells_from_pool(pool, cells, split_table(table, cells).0)
}

/// A record field (`(n+1)^3` dense) in pool layout with 8 canonical records
/// (`canon.len() == 8`). Records past the lattice are not stored: the
/// canonical block answers them.
pub fn records_to_pool<T: Copy>(dense: &[T], cells: [u32; 3], canon: &[T]) -> Vec<T> {
    let p = tile_count(cells) as usize;
    records_into_pool(dense, cells, canon, &identity_slots(cells), p)
}

/// [`records_to_pool`] under a given slot table.
pub fn records_to_pool_with<T: Copy>(
    dense: &[T],
    cells: [u32; 3],
    canon: &[T],
    table: &[u32],
) -> Vec<T> {
    let (slot_of, p) = split_table(table, cells);
    records_into_pool(dense, cells, canon, slot_of, p)
}

/// A pooled record field back to its dense `(n+1)^3` array; records past the
/// lattice come from the canonical entry for their mask.
pub fn records_from_pool<T: Copy>(pool: &[T], cells: [u32; 3]) -> Vec<T> {
    records_out_of_pool(pool, cells, &identity_slots(cells))
}

/// A pooled record field a real step allocated back to its dense array;
/// records of absent tiles and past the lattice read the canonical entry for
/// their mask.
pub fn records_from_pool_with<T: Copy>(pool: &[T], table: &[u32], cells: [u32; 3]) -> Vec<T> {
    records_out_of_pool(pool, cells, split_table(table, cells).0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELLS: [u32; 3] = [17, 8, 3];

    /// slot_of = [2, NONE, 0] (tiles in reverse slot order, the middle one
    /// absent), tile_of = [2, NONE, 0]; P = 3.
    fn permuted_table() -> Vec<u32> {
        vec![2, TS_NONE, 0, 2, TS_NONE, 0]
    }

    #[test]
    fn cells_round_trip_through_a_permuted_table() {
        let table = permuted_table();
        let dense: Vec<f32> = (0..17 * 8 * 3).map(|i| i as f32 + 1.0).collect();
        let pool = to_pool_with(&dense, CELLS, &[-1.0], &table);
        assert_eq!(pool.len(), 3 * 512 + 8);
        let back = from_pool_with(&pool, &table, CELLS);
        for z in 0..3usize {
            for y in 0..8usize {
                for x in 0..17usize {
                    let i = x + 17 * (y + 8 * z);
                    // Tile 1 (x 8..16) is absent and reads canonical.
                    let want = if (8..16).contains(&x) { -1.0 } else { dense[i] };
                    assert_eq!(back[i], want, "cell ({x}, {y}, {z})");
                }
            }
        }
        // Cell (0, 0, 0) lives in slot 2, cell (16, 0, 0) in slot 0.
        assert_eq!(pool[2 * 512], dense[0]);
        assert_eq!(pool[16 & 7], dense[16]);
    }

    #[test]
    fn records_round_trip_and_absent_tiles_read_canonical() {
        let table = permuted_table();
        let canon: Vec<u32> = (100..108).collect();
        let m = [18usize, 9, 4];
        let dense: Vec<u32> = (0..(m[0] * m[1] * m[2]) as u32).collect();
        let pool = records_to_pool_with(&dense, CELLS, &canon, &table);
        let back = records_from_pool_with(&pool, &table, CELLS);
        for z in 0..m[2] {
            for y in 0..m[1] {
                for x in 0..m[0] {
                    let i = x + m[0] * (y + m[1] * z);
                    let inside = x < 17 && y < 8 && z < 3;
                    let want = if inside && !(8..16).contains(&x) {
                        dense[i]
                    } else {
                        canon[wall_mask([x as u32, y as u32, z as u32], CELLS)]
                    };
                    assert_eq!(back[i], want, "record ({x}, {y}, {z})");
                }
            }
        }
        // The absent tile's record (9, 0, 0) sits on the lower y and z walls.
        assert_eq!(back[9], canon[6]);
    }

    #[test]
    fn identity_variants_match_the_table_variants() {
        let (table, p) = identity_table(CELLS);
        assert_eq!(p, 3);
        let dense: Vec<f32> = (0..17 * 8 * 3).map(|i| i as f32).collect();
        let pool = to_pool(&dense, CELLS, &[0.0]);
        assert_eq!(pool, to_pool_with(&dense, CELLS, &[0.0], &table));
        assert_eq!(from_pool(&pool, CELLS), dense);
        assert_eq!(from_pool_with(&pool, &table, CELLS), dense);
    }
}
