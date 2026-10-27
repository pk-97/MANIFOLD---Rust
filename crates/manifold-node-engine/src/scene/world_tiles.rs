//! World tiles, the read side:
//! the pooled lattice keyed by world tile through a hash table, with a row of
//! 27 neighbour slots per pool slot so hot kernels rarely hash. The sizes and
//! the table layout live here, beside the WGSL helpers ([`READ_SIDE`]), because
//! both the water build (`manifold-water-liquid`, which writes the table) and
//! the scene renderer read the field and neither depends on the other.
//!
//! Cell p (i32 per axis, any sign) is centred on p·h and spans
//! [(p − ½)·h, (p + ½)·h), so the lattice origin is −h/2 on every axis. A
//! tile is 8³ cells, tile t = p >> 3 (floor for negatives too).
//!
//! The table's keys use ordered linear probing (Amble and Knuth 1974): along a
//! probe run keys sit in decreasing order, so a lookup stops at the first
//! smaller key and the layout depends on the key set alone, never on the
//! order keys arrive in. `insert` and `find` (testkit) are the CPU twins of
//! the GPU build's insert and the read side's lookup.

/// Cells per tile side.
pub const TILE: u32 = 8;

/// Cells in one tile: the stride of a pool slot.
pub const TILE_CELLS: u32 = TILE * TILE * TILE;

/// Elements after the last slot of a pooled field: the one canonical
/// element, the field's value wherever no tile is stored.
pub const CANON_ELEMENTS: u32 = 1;

/// Words per slot in the slot rows: the tile's x, y, z (as i32) and its entry.
pub const SLOT_WORDS: u32 = 4;

/// Slots per neighbour row: the 3³ tiles around a slot's tile, itself at 13.
pub const NEIGHBOURS: u32 = 27;

/// No slot, no entry.
pub const NONE: u32 = u32::MAX;

/// The key of an empty table word.
pub const EMPTY: u32 = 0;

/// Knuth's multiplicative hash constant, 2³² / φ.
pub const HASH_MULTIPLIER: u32 = 2_654_435_769;

/// The WGSL helpers (`world_tiles_read.wgsl`): pack, hash, lookup, home and
/// the `ts_*` cell helpers. The including shader defines `ts_pool_tiles()`,
/// `ts_table_entries()` and `ts_table_word(i)`.
pub const READ_SIDE: &str = include_str!("world_tiles_read.wgsl");

/// Elements of a pooled field with room for `pool_tiles` tiles.
pub fn pool_len(pool_tiles: u32) -> u64 {
    u64::from(pool_tiles) * u64::from(TILE_CELLS) + u64::from(CANON_ELEMENTS)
}

/// Table entries (H) for a pool of `pool_tiles`: the power of two at or
/// above 4P, so the table is at most a quarter full of held tiles.
pub fn table_entries(pool_tiles: u32) -> u32 {
    (4 * pool_tiles.max(1)).next_power_of_two()
}

/// Word of the table where slot `slot`'s row starts (its tile, then its
/// entry).
pub fn slot_row_word(entries: u32, slot: u32) -> u64 {
    2 * u64::from(entries) + u64::from(SLOT_WORDS) * u64::from(slot)
}

/// Word of the table where slot `slot`'s 27 neighbour slots start.
pub fn neighbour_row_word(entries: u32, pool_tiles: u32, slot: u32) -> u64 {
    slot_row_word(entries, pool_tiles) + u64::from(NEIGHBOURS) * u64::from(slot)
}

/// Words of the whole table: keys and entry slots (2H), then the slot rows
/// and neighbour rows (31P).
pub fn table_words(pool_tiles: u32) -> u64 {
    neighbour_row_word(table_entries(pool_tiles), pool_tiles, pool_tiles)
}

/// The lattice origin for cells of `cell_size` metres: cell p is centred on
/// p·h, so cells are counted from −h/2.
pub fn lattice_origin(cell_size: f32) -> [f32; 3] {
    [-0.5 * cell_size; 3]
}

/// The tile holding cell p.
pub fn tile_of_cell(p: [i32; 3]) -> [i32; 3] {
    p.map(|c| c >> 3)
}

/// The cell's place inside its tile.
pub fn local_index(p: [i32; 3]) -> u32 {
    let l = p.map(|c| (c & 7) as u32);
    l[0] + TILE * (l[1] + TILE * l[2])
}

/// Tiles along each axis before two share a key: [`tile_key`]'s 11, 9 and
/// 11 bits. Tiles this far apart alias; nearer ones never do.
pub const KEY_PERIOD_TILES: [u32; 3] = [2048, 512, 2048];

/// The key of tile t: x in 11 bits, y in 9, z in 11, each wrapped in two's
/// complement, plus 1 so [`EMPTY`] is no tile. [`KEY_PERIOD_TILES`] fit
/// before two tiles share a key.
pub fn tile_key(t: [i32; 3]) -> u32 {
    let [x, y, z] = t.map(|c| c as u32);
    ((x & 0x7ff) | ((y & 0x1ff) << 11) | ((z & 0x7ff) << 20)) + 1
}

/// The entry a key's probe run starts at in a table of `entries` (a power of
/// two, at least 2).
pub fn key_hash(key: u32, entries: u32) -> u32 {
    key.wrapping_mul(HASH_MULTIPLIER) >> (32 - entries.trailing_zeros())
}

/// The neighbour row index of tile offset d (each axis -1..=1).
pub fn neighbour_index(d: [i32; 3]) -> u32 {
    ((d[0] + 1) + 3 * (d[1] + 1) + 9 * (d[2] + 1)) as u32
}

/// Ordered insert into the key words of a table (`keys.len()` a power of
/// two), as the GPU build does it: walk from the key's hash; an empty word or
/// a smaller key is replaced by the key in hand and a displaced key walks on
/// from the next word; an equal key stops the walk. False when a walk covered
/// the whole table: the table is full and the key in hand (perhaps a
/// displaced one) is dropped.
#[cfg(any(test, feature = "testkit"))]
pub fn insert(keys: &mut [u32], key: u32) -> bool {
    let entries = keys.len() as u32;
    let mut k = key;
    let mut i = key_hash(k, entries);
    for _ in 0..entries {
        let c = keys[i as usize];
        if c == k {
            return true;
        }
        if c < k {
            keys[i as usize] = k;
            if c == EMPTY {
                return true;
            }
            k = c;
        }
        i = (i + 1) & (entries - 1);
    }
    false
}

/// The entry holding `key`, as the read side's `ts_entry_of` finds it: the
/// walk stops at the first smaller key and never covers more than the table.
#[cfg(any(test, feature = "testkit"))]
pub fn find(keys: &[u32], key: u32) -> Option<u32> {
    let entries = keys.len() as u32;
    let mut i = key_hash(key, entries);
    for _ in 0..entries {
        let c = keys[i as usize];
        if c == key {
            return Some(i);
        }
        if c < key {
            return None;
        }
        i = (i + 1) & (entries - 1);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed permutation of `0..n` from a xorshift seed.
    fn shuffled(n: usize, mut seed: u32) -> Vec<usize> {
        let mut order: Vec<usize> = (0..n).collect();
        for i in (1..n).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            order.swap(i, seed as usize % (i + 1));
        }
        order
    }

    fn table_of(keys: &[u32], order: &[usize], entries: usize) -> Vec<u32> {
        let mut table = vec![EMPTY; entries];
        for &i in order {
            assert!(insert(&mut table, keys[i]), "key {:#x} fits", keys[i]);
        }
        table
    }

    /// The far corners of the key range, a tile far below the origin, then a
    /// 4³ block of tiles straddling the origin: 67 tiles, no two sharing a
    /// key.
    fn tiles() -> Vec<[i32; 3]> {
        let mut tiles = vec![[1023, 255, 1023], [-1024, -256, -1024], [-3, -40, 700]];
        for z in -2..2 {
            for y in -2..2 {
                for x in -2..2 {
                    tiles.push([x, y, z]);
                }
            }
        }
        tiles
    }

    #[test]
    fn keys_wrap_by_axis_and_never_collide_with_empty() {
        assert_eq!(tile_key([0, 0, 0]), 1);
        // (-1, -1, -1) packs to all 31 bits set: the largest key.
        assert_eq!(tile_key([-1, -1, -1]), 1 << 31);
        let [px, py, pz] = KEY_PERIOD_TILES.map(|t| t as i32);
        assert_eq!(tile_key([px, 0, 0]), tile_key([0, 0, 0]));
        assert_eq!(tile_key([5, py - 3, -7]), tile_key([5, -3, -7]));
        assert_eq!(tile_key([5, -3, -7 - pz]), tile_key([5, -3, -7]));
        assert_ne!(tile_key([px - 1, 0, 0]), tile_key([0, 0, 0]));
        let distinct: std::collections::BTreeSet<u32> =
            tiles().iter().map(|&t| tile_key(t)).collect();
        assert_eq!(distinct.len(), tiles().len());
        assert!(!distinct.contains(&EMPTY));
        for key in distinct {
            assert!(key_hash(key, 64) < 64);
        }
    }

    /// The same key set in three shuffled orders leaves the same words, at a
    /// quarter load, at a load where probe runs wrap past the last word, and
    /// with every word full. Every key is found where it sits; absent keys
    /// are not found, even in the full table.
    #[test]
    fn ordered_insert_layout_is_the_same_in_every_order() {
        let keys: Vec<u32> = tiles().iter().map(|&t| tile_key(t)).collect();
        for entries in [256usize, 128, 64] {
            let set = &keys[..keys.len().min(entries)];
            let first = table_of(set, &shuffled(set.len(), 0x9e37_79b9), entries);
            for seed in [0x1234_5678, 0xdead_beef] {
                assert_eq!(
                    table_of(set, &shuffled(set.len(), seed), entries),
                    first,
                    "{entries} entries, seed {seed:#x}"
                );
            }
            // Inserting in decreasing key order builds the ordered table
            // directly: each key lands at the first free word of its run.
            let mut decreasing: Vec<usize> = (0..set.len()).collect();
            decreasing.sort_by_key(|&i| std::cmp::Reverse(set[i]));
            assert_eq!(table_of(set, &decreasing, entries), first);
            for &key in set {
                let at = find(&first, key).expect("an inserted key is found");
                assert_eq!(first[at as usize], key);
            }
            for absent in [tile_key([9, 9, 9]), tile_key([-9, 3, 40])] {
                assert_eq!(find(&first, absent), None);
            }
        }
        // 64 keys in 64 words: every word holds one, and some run wraps from
        // the last word to the first.
        let full = table_of(&keys[..64], &shuffled(64, 7), 64);
        assert!(full.iter().all(|&k| k != EMPTY));
        let wraps = keys[..64].iter().any(|&k| {
            let home = key_hash(k, 64);
            find(&full, k).is_some_and(|at| at < home)
        });
        assert!(wraps, "a probe run wraps past the last word");
    }

    /// A full table drops the key in hand and says so; the keys it kept are
    /// still found.
    #[test]
    fn a_full_table_drops_one_key() {
        let keys: Vec<u32> = tiles().iter().map(|&t| tile_key(t)).collect();
        let mut table = vec![EMPTY; 4];
        let placed: Vec<bool> = keys[..5].iter().map(|&k| insert(&mut table, k)).collect();
        assert_eq!(placed, [true, true, true, true, false]);
        let kept = keys[..5]
            .iter()
            .filter(|&&k| find(&table, k).is_some())
            .count();
        assert_eq!(kept, 4);
    }

    #[test]
    fn sizes_follow_the_pool() {
        assert_eq!(table_entries(8_456), 65_536);
        assert_eq!(table_entries(1), 4);
        assert_eq!(table_entries(16), 64);
        assert_eq!(pool_len(3), 3 * 512 + 1);
        assert_eq!(table_words(16), 2 * 64 + 31 * 16);
        assert_eq!(neighbour_row_word(64, 16, 2), 2 * 64 + 4 * 16 + 27 * 2);
        assert_eq!(tile_of_cell([-1, 7, 8]), [-1, 0, 1]);
        assert_eq!(local_index([-1, 7, 8]), 7 + 8 * 7);
        assert_eq!(neighbour_index([0, 0, 0]), 13);
        assert_eq!(lattice_origin(0.0625), [-0.03125; 3]);
    }

    const STUB: &str = "@group(0) @binding(0) var<storage, read> ts_table: array<u32>;\n\
        fn ts_pool_tiles() -> u32 { return 3u; }\n\
        fn ts_table_entries() -> u32 { return 16u; }\n\
        fn ts_table_word(i: u32) -> u32 { return ts_table[i]; }\n";

    #[test]
    fn read_side_validates() {
        let shader = format!(
            "{READ_SIDE}\n{STUB}\
             @compute @workgroup_size(1) fn main() {{ ts_home(0u); \
             _ = ts_list_cell(0u, 0u) + ts_cell_at(vec3<i32>(-1)) + ts_face(vec3<i32>(1)) \
             + ts_pool_len() + ts_canon() + ts_slot_entry(0u); \
             _ = ts_cell_stored(vec3<i32>(1)) && ts_record_stored(vec3<i32>(-9)); \
             _ = ts_cell_coord(0u) + ts_face_coord(0u) \
             + ts_world_cell(vec3<f32>(1.0), vec3<f32>(-0.5), 2.0); }}\n"
        );
        let module = naga::front::wgsl::parse_str(&shader).expect("world_tiles_read.wgsl parses");
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("world_tiles_read.wgsl validates");
    }
}
