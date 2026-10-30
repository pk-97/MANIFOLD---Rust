// World tiles, the read side.
// Every pooled lattice field is stored per 8^3 tile of world cells in a pool
// of P tile slots, found through a hash table keyed by world tile. Cell p
// (i32 per axis, any sign) is centred on p·h; its tile is p >> 3 (floor for
// negatives too) and its place in the tile is p & 7. A pooled field has
// 512·P + 1 elements: slot s holds its tile's cells at s·512 + local(p), and
// the last element is the canonical one, the field's value wherever no tile
// is stored. A record lives with its cell, so a record index is its cell's.
//
// The including shader defines `fn ts_pool_tiles() -> u32` (P) and
// `fn ts_table_word(i: u32) -> u32`, word i of the table, with h = 4 the
// header's words:
//   [0, h)                      the header: H (a power of two at or above
//                               4P), P, then zeros
//   [h, h + H)                  keys, ts_tile_key(tile); 0 is empty
//   [h + H, h + 2H)             the slot of each entry, WT_NONE when it has
//                               none
//   [h + 2H, h + 2H + 4P)       per slot: its tile (x, y, z as i32) and its
//                               entry, the entry WT_NONE when the slot is free
//   [h + 2H + 4P, h + 2H + 31P) per slot: the slots of its 27 neighbour
//                               tiles, k = (dx + 1) + 3(dy + 1) + 9(dz + 1)
//                               (k = 13 itself), WT_NONE where absent
// The table carries its own H, so a reader needs only its own P whatever
// pools H was sized for.
// These helpers name no buffer. The constants carry a WT_ prefix so this
// composes beside `tile_grid.wgsl` (TS_TILE, TS_NONE).
//
// A kernel finds tiles through a home: `ts_home(idx)` keeps the slot and
// tile of a pool index in private variables. A list kernel calls it once
// after `ts_list_cell`, a particle kernel once on `ts_cell_at` of its cell.
// Then the home tile costs no read, its 26 neighbours one read of the home's
// neighbour row, and anything else (or no home) a hash lookup. A kernel that
// never sets a home is still right, only slower.

const WT_NONE: u32 = 0xffffffffu;
const WT_EMPTY: u32 = 0u;
// Knuth's multiplicative hash constant, 2^32 / φ.
const WT_HASH: u32 = 2654435769u;
const WT_SLOT_WORDS: u32 = 4u;
const WT_NEIGHBOURS: u32 = 27u;
// Words before the keys (world_tiles.rs HEADER_WORDS).
const WT_HEADER: u32 = 4u;

var<private> ts_home_slot: u32 = WT_NONE;
var<private> ts_home_tile: vec3<i32> = vec3<i32>(0);

// The world cell holding point x: cells of 1 / inv_cell metres counted from
// the lattice origin (−h/2 on every axis). Every pass that bins a point uses
// this rule, so a particle's cell is the same in every pass.
fn ts_world_cell(x: vec3<f32>, origin: vec3<f32>, inv_cell: f32) -> vec3<i32> {
    return vec3<i32>(floor((x - origin) * inv_cell));
}

// The key of tile t: x in 11 bits, y in 9, z in 11, each wrapped in two's
// complement, plus 1 so 0 means empty. Tiles 2048 apart in x or z, or 512
// apart in y, share a key (an alias, which the build reports).
fn ts_tile_key(t: vec3<i32>) -> u32 {
    let u = bitcast<vec3<u32>>(t);
    return ((u.x & 0x7ffu) | ((u.y & 0x1ffu) << 11u) | ((u.z & 0x7ffu) << 20u)) + 1u;
}

// H, from the table's header.
fn ts_table_entries() -> u32 {
    return ts_table_word(0u);
}

// The first entry a key's probe run starts at, in a table of `entries`
// (a power of two).
fn ts_hash(key: u32, entries: u32) -> u32 {
    return (key * WT_HASH) >> (32u - countTrailingZeros(entries));
}

// The entry holding `key`, or WT_NONE. Along a probe run keys sit in
// decreasing order, so the walk stops at the first smaller key (an empty
// word is 0) and never walks more than the whole table.
fn ts_entry_of(key: u32) -> u32 {
    let entries = ts_table_entries();
    var i = ts_hash(key, entries);
    for (var walked = 0u; walked < entries; walked = walked + 1u) {
        let c = ts_table_word(WT_HEADER + i);
        if c == key {
            return i;
        }
        if c < key {
            return WT_NONE;
        }
        i = (i + 1u) & (entries - 1u);
    }
    return WT_NONE;
}

fn ts_slot_word(slot: u32, w: u32) -> u32 {
    return ts_table_word(WT_HEADER + 2u * ts_table_entries() + WT_SLOT_WORDS * slot + w);
}

// The tile a held slot holds.
fn ts_slot_tile(slot: u32) -> vec3<i32> {
    return bitcast<vec3<i32>>(vec3<u32>(ts_slot_word(slot, 0u), ts_slot_word(slot, 1u), ts_slot_word(slot, 2u)));
}

// The entry of a slot, WT_NONE when the slot is free.
fn ts_slot_entry(slot: u32) -> u32 {
    return ts_slot_word(slot, 3u);
}

// The neighbour row index of tile offset d (each axis -1..=1).
fn ts_neighbour_index(d: vec3<i32>) -> u32 {
    return u32((d.x + 1) + 3 * (d.y + 1) + 9 * (d.z + 1));
}

// The slot of neighbour k of a held slot, WT_NONE where that tile is absent.
fn ts_neighbour_slot(slot: u32, k: u32) -> u32 {
    let rows = WT_HEADER + 2u * ts_table_entries() + WT_SLOT_WORDS * ts_pool_tiles();
    return ts_table_word(rows + WT_NEIGHBOURS * slot + k);
}

// The slot of tile t through the hash, WT_NONE when absent. A slot holding a
// different tile under t's key (an alias) does not count.
fn ts_hash_slot(t: vec3<i32>) -> u32 {
    let entry = ts_entry_of(ts_tile_key(t));
    if entry == WT_NONE {
        return WT_NONE;
    }
    let slot = ts_table_word(WT_HEADER + ts_table_entries() + entry);
    if slot == WT_NONE || any(ts_slot_tile(slot) != t) {
        return WT_NONE;
    }
    return slot;
}

// Keep the slot and tile of pool index idx as this thread's home. The
// canonical element (or anything past the slots) clears the home.
fn ts_home(idx: u32) {
    let slot = idx >> 9u;
    if slot >= ts_pool_tiles() {
        ts_home_slot = WT_NONE;
        return;
    }
    ts_home_slot = slot;
    ts_home_tile = ts_slot_tile(slot);
}

// The slot of tile t, WT_NONE when absent: the home, its neighbour row, or
// the hash.
fn ts_slot_of(t: vec3<i32>) -> u32 {
    if ts_home_slot != WT_NONE {
        let d = t - ts_home_tile;
        if all(d == vec3<i32>(0)) {
            return ts_home_slot;
        }
        if all(abs(d) <= vec3<i32>(1)) {
            return ts_neighbour_slot(ts_home_slot, ts_neighbour_index(d));
        }
    }
    return ts_hash_slot(t);
}

// Elements of every pooled field.
fn ts_pool_len() -> u32 {
    return ts_pool_tiles() * 512u + 1u;
}

// The canonical element of a pooled field.
fn ts_canon() -> u32 {
    return ts_pool_tiles() * 512u;
}

// The cell's place inside its tile.
fn ts_local(p: vec3<i32>) -> u32 {
    let l = p & vec3<i32>(7);
    return u32(l.x + 8 * (l.y + 8 * l.z));
}

fn ts_local_coord(local: u32) -> vec3<i32> {
    return vec3<i32>(i32(local & 7u), i32((local >> 3u) & 7u), i32(local >> 6u));
}

// The pool index of cell p: its element, or the canonical element when its
// tile is not stored.
fn ts_cell_at(p: vec3<i32>) -> u32 {
    let slot = ts_slot_of(p >> vec3<u32>(3u));
    if slot == WT_NONE {
        return ts_canon();
    }
    return slot * 512u + ts_local(p);
}

// Record p lives with cell p.
fn ts_face(p: vec3<i32>) -> u32 {
    return ts_cell_at(p);
}

fn ts_cell_stored(p: vec3<i32>) -> bool {
    return ts_slot_of(p >> vec3<u32>(3u)) != WT_NONE;
}

fn ts_record_stored(p: vec3<i32>) -> bool {
    return ts_cell_stored(p);
}

// The cell a pool index holds (idx is never canonical).
fn ts_cell_coord(idx: u32) -> vec3<i32> {
    let slot = idx >> 9u;
    var tile = ts_home_tile;
    if slot != ts_home_slot {
        tile = ts_slot_tile(slot);
    }
    return tile * 8 + ts_local_coord(idx & 511u);
}

// A stored record's index is its cell's.
fn ts_face_coord(idx: u32) -> vec3<i32> {
    return ts_cell_coord(idx);
}

// Thread `gid` of a list dispatched at 512 threads a slot: the pool index of
// the cell at local index gid & 511 of `slot`. Lists hold slots.
fn ts_list_cell(slot: u32, gid: u32) -> u32 {
    return slot * 512u + (gid & 511u);
}
