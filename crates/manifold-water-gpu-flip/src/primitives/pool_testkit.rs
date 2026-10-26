//! Dense <-> tile-pool conversion for test rigs that bind dense CPU arrays to
//! kernels that index the pool. The table is identity-allocated (every box
//! tile holds the slot of its own index, `P = T`), so a rig's CPU reference
//! keeps working on dense arrays.

/// Tiles per axis: the box tile grid `ceil(n / 8)`.
fn tile_dims(cells: [u32; 3]) -> [u32; 3] {
    [
        cells[0].div_ceil(8),
        cells[1].div_ceil(8),
        cells[2].div_ceil(8),
    ]
}

/// The pool index of cell `p` (inside the lattice) under the identity table.
fn pool_cell(p: [u32; 3], cells: [u32; 3]) -> usize {
    let t = tile_dims(cells);
    let tile = (p[0] >> 3) + t[0] * ((p[1] >> 3) + t[1] * (p[2] >> 3));
    let local = (p[0] & 7) + 8 * ((p[1] & 7) + 8 * (p[2] & 7));
    tile as usize * 512 + local as usize
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
    let t = tile_dims(cells);
    t[0] * t[1] * t[2]
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

fn blank_pool<T: Copy>(cells: [u32; 3], canon: &[T]) -> Vec<T> {
    assert!(
        canon.len() == 1 || canon.len() == 8,
        "canon is 1 or 8 entries"
    );
    let p = tile_count(cells) as usize;
    let mut pool = vec![canon[0]; p * 512 + 8];
    for m in 0..8 {
        pool[p * 512 + m] = if canon.len() == 1 { canon[0] } else { canon[m] };
    }
    pool
}

/// A cell field (`n^3` dense, x fastest) in pool layout. `canon` is the
/// field's defined value (`len() == 1`) or its 8 canonical records. Unused
/// cells of partial edge tiles hold `canon[0]`.
pub fn to_pool<T: Copy>(dense: &[T], cells: [u32; 3], canon: &[T]) -> Vec<T> {
    assert_eq!(dense.len(), (cells[0] * cells[1] * cells[2]) as usize);
    let mut pool = blank_pool(cells, canon);
    let mut i = 0;
    for z in 0..cells[2] {
        for y in 0..cells[1] {
            for x in 0..cells[0] {
                pool[pool_cell([x, y, z], cells)] = dense[i];
                i += 1;
            }
        }
    }
    pool
}

/// A pooled cell field back to its dense `n^3` array.
pub fn from_pool<T: Copy>(pool: &[T], cells: [u32; 3]) -> Vec<T> {
    let mut dense = Vec::with_capacity((cells[0] * cells[1] * cells[2]) as usize);
    for z in 0..cells[2] {
        for y in 0..cells[1] {
            for x in 0..cells[0] {
                dense.push(pool[pool_cell([x, y, z], cells)]);
            }
        }
    }
    dense
}

/// A record field (`(n+1)^3` dense) in pool layout with 8 canonical records
/// (`canon.len() == 8`). Records past the lattice are not stored: the
/// canonical block answers them.
pub fn records_to_pool<T: Copy>(dense: &[T], cells: [u32; 3], canon: &[T]) -> Vec<T> {
    assert_eq!(canon.len(), 8, "records need 8 canonical entries");
    let m = [cells[0] + 1, cells[1] + 1, cells[2] + 1];
    assert_eq!(dense.len(), (m[0] * m[1] * m[2]) as usize);
    let mut pool = blank_pool(cells, canon);
    let mut i = 0;
    for z in 0..m[2] {
        for y in 0..m[1] {
            for x in 0..m[0] {
                if x < cells[0] && y < cells[1] && z < cells[2] {
                    pool[pool_cell([x, y, z], cells)] = dense[i];
                }
                i += 1;
            }
        }
    }
    pool
}

/// A pooled record field back to its dense `(n+1)^3` array; records past the
/// lattice come from the canonical entry for their mask.
pub fn records_from_pool<T: Copy>(pool: &[T], cells: [u32; 3]) -> Vec<T> {
    let m = [cells[0] + 1, cells[1] + 1, cells[2] + 1];
    let canon_base = pool.len() - 8;
    let mut dense = Vec::with_capacity((m[0] * m[1] * m[2]) as usize);
    for z in 0..m[2] {
        for y in 0..m[1] {
            for x in 0..m[0] {
                if x < cells[0] && y < cells[1] && z < cells[2] {
                    dense.push(pool[pool_cell([x, y, z], cells)]);
                } else {
                    dense.push(pool[canon_base + wall_mask([x, y, z], cells)]);
                }
            }
        }
    }
    dense
}
