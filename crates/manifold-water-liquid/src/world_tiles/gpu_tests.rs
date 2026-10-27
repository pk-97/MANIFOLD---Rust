//! GPU proofs of the world tiles build against its CPU mirror (`mirror.rs`),
//! and of the read side's three lookup paths over a built table.

use std::collections::BTreeSet;

use manifold_gpu::testkit::test_device;
use manifold_node_engine::scene::world_tiles::{
    NEIGHBOURS, find, key_hash, lattice_origin, local_index, neighbour_index, neighbour_row_word,
    slot_row_word, table_entries, tile_key, tile_of_cell,
};

use super::mirror::{Built, Mirror};
use super::*;

/// Cells of a quarter metre: the centre p·h of cell p bins to p exactly.
const CELL: f32 = 0.25;

fn read(buffer: &GpuBuffer, words: usize) -> Vec<u32> {
    let ptr = buffer.mapped_ptr().expect("shared buffer");
    // SAFETY: a shared buffer of at least `words` words; its GPU work is done.
    bytemuck::cast_slice(unsafe { std::slice::from_raw_parts(ptr, words * 4) }).to_vec()
}

fn shared<T: bytemuck::Pod>(device: &GpuDevice, data: &[T]) -> GpuBuffer {
    let bytes: &[u8] = bytemuck::cast_slice(data);
    let buffer = device.create_buffer_shared(bytes.len().max(4) as u64);
    // SAFETY: a new shared buffer at least `bytes` long; no GPU work in flight.
    unsafe { buffer.write(0, bytes) };
    buffer
}

/// A clock plan: zeros run; live with no time to step (word 11 set,
/// step_dt 0) builds nothing.
fn plan(device: &GpuDevice, running: bool) -> GpuBuffer {
    let mut words = [0u32; 12];
    words[11] = u32::from(!running);
    shared(device, &words)
}

/// Liquid particles at the centres of `cells`, then a dead one (radius 0)
/// in a tile no live particle uses.
fn particles(device: &GpuDevice, cells: &[[i32; 3]]) -> (GpuBuffer, u32) {
    let mut records: Vec<FluidParticle> = cells
        .iter()
        .enumerate()
        .map(|(i, p)| FluidParticle {
            position_radius: [
                p[0] as f32 * CELL,
                p[1] as f32 * CELL,
                p[2] as f32 * CELL,
                0.05,
            ],
            velocity: [0.0; 3],
            id: i as u32 + 1,
        })
        .collect();
    records.push(FluidParticle {
        position_radius: [250.0, 250.0, 250.0, 0.0],
        ..FluidParticle::default()
    });
    (shared(device, &records), records.len() as u32)
}

fn build(
    device: &GpuDevice,
    tiles: &WorldTiles,
    cells: &[[i32; 3]],
    gate: &GpuBuffer,
    replay: u32,
) {
    let (records, count) = particles(device, cells);
    // The slots in use, as the step's slot record holds them.
    let record = shared(device, &[count.div_ceil(256).max(1), 1, 1, count]);
    let mut enc = device.create_encoder("world tiles proof");
    tiles.encode_allocation(
        &mut enc,
        &Allocation {
            points: &records,
            count: PointCount::Record(&record),
            origin: lattice_origin(CELL),
            inv_cell: 1.0 / CELL,
            replay_count: replay,
            gate,
        },
    );
    enc.commit_and_wait_completed();
}

/// Every cell of the box from `min`, `size` cells a side.
fn cells_of(min: [i32; 3], size: [i32; 3]) -> Vec<[i32; 3]> {
    let mut cells = Vec::new();
    for z in 0..size[2] {
        for y in 0..size[1] {
            for x in 0..size[0] {
                cells.push([min[0] + x, min[1] + y, min[2] + z]);
            }
        }
    }
    cells
}

/// The 27 tiles of the block [-1, 1]³ around `centre`, in neighbour order.
fn block_tiles(centre: [i32; 3]) -> Vec<[i32; 3]> {
    let mut tiles = vec![[0; 3]; NEIGHBOURS as usize];
    for z in -1..=1 {
        for y in -1..=1 {
            for x in -1..=1 {
                let at = neighbour_index([x, y, z]) as usize;
                tiles[at] = [centre[0] + x, centre[1] + y, centre[2] + z];
            }
        }
    }
    tiles
}

/// The GPU's words equal the mirror's: keys, entry slots, every slot row
/// and neighbour row, the counts, each rank's part of the list, the
/// retired and fresh lists, the free stack and the triples.
fn check(tiles: &WorldTiles, want: &Built, replay: u32, step: &str) {
    let h = tiles.table_entries();
    let p = tiles.pool_tiles();
    let r = tiles.reach().ring_max;
    let (hu, pu) = (h as usize, p as usize);
    let table = read(tiles.table(), table_words(p) as usize);
    assert_eq!(table[..hu], want.keys[..], "{step}: keys");
    assert_eq!(
        table[hu..2 * hu],
        want.entry_slot[..],
        "{step}: entry slots"
    );
    for s in 0..p {
        let row = slot_row_word(h, s) as usize;
        let entry = table[row + 3];
        assert_eq!(
            entry, want.slot_entry[s as usize],
            "{step}: slot {s}'s entry"
        );
        if let Some(t) = want.slot_tile[s as usize] {
            assert_eq!(
                table[row..row + 3],
                t.map(|c| c as u32),
                "{step}: slot {s}'s tile"
            );
            assert_eq!(
                table[hu + entry as usize],
                s,
                "{step}: slot {s}'s entry points back"
            );
        }
        let rows = neighbour_row_word(h, p, s) as usize;
        assert_eq!(
            table[rows..rows + NEIGHBOURS as usize],
            want.neighbours[s as usize],
            "{step}: slot {s}'s neighbours"
        );
    }
    let counts = read(tiles.counts(), count_words(r) as usize);
    assert_eq!(counts, want.counts, "{step}: counts");
    let by_rank = read(&tiles.lists().by_rank, pu);
    let mut start = 0;
    for (rank, set) in want.by_rank.iter().enumerate() {
        let end = counts[rank_count_word(rank as u32)] as usize;
        let got: BTreeSet<u32> = by_rank[start..end].iter().copied().collect();
        assert_eq!(&got, set, "{step}: rank {rank}'s slots");
        start = end;
    }
    let set = |buffer: &GpuBuffer, n: u32| -> BTreeSet<u32> {
        read(buffer, n as usize).into_iter().collect()
    };
    let retired = counts[retired_word(r)];
    let fresh = counts[fresh_word(r)];
    assert_eq!(
        set(&tiles.lists().retired, retired),
        want.retired,
        "{step}: retired"
    );
    assert_eq!(
        set(&tiles.lists().fresh, fresh),
        want.fresh,
        "{step}: fresh"
    );
    let slots = read(&tiles.slot_scratch, 3 * pu);
    assert_eq!(
        slots[2 * pu..2 * pu + want.stack.len()],
        want.stack[..],
        "{step}: free stack"
    );
    let args = read(tiles.args(), args_words(r) as usize);
    let triple = |offset: u64| &args[offset as usize / 4..offset as usize / 4 + 3];
    for k in 0..=r {
        assert_eq!(
            triple(list_args_offset(k)),
            [2 * counts[k as usize], 1, 1],
            "{step}: list {k}"
        );
    }
    assert_eq!(
        triple(retired_args_offset(r)),
        [2 * retired, 1, 1],
        "{step}: retired triple"
    );
    assert_eq!(
        triple(fresh_args_offset(r)),
        [2 * fresh, 1, 1],
        "{step}: fresh triple"
    );
    let range = 2 * replay_range_index(r) as usize;
    assert_eq!(args[range..range + 2], [0, replay], "{step}: replay range");
}

/// Three steps of a set that moves across the origin: a 2³-tile body
/// sliding along x, a pair of particles that leaves and comes back, a small
/// body that appears. Each build equals the mirror's, so held slots keep
/// their tiles, freed slots go on the stack in slot order, and new tiles get
/// slots in table order. An inactive clock slot then keeps every word and
/// switches the triples off, and a parents build over the last table equals
/// its mirror with each slot's children found in the level below.
#[test]
fn a_moving_set_matches_the_mirror_over_three_steps() {
    let device = test_device();
    let reach = Reach {
        ring_max: 2,
        alloc_ring: 2,
        cell_reach: 2,
    };
    let mut tiles = WorldTiles::new(&device, 512, reach, LIQUID_POINTS).expect("world tiles");
    tiles.prepare(&device);
    let mut mirror = Mirror::new(512, reach, false);
    let run = plan(&device, true);
    let body = |dx: i32| cells_of([-10 + dx, -3, 4], [6, 6, 6]);
    let pair = vec![[20, 30, -17], [21, 30, -17]];
    let small = cells_of([-40, -2, 0], [2, 2, 2]);
    let steps = [
        [body(0), pair.clone()].concat(),
        [body(6), small.clone()].concat(),
        [body(12), small, pair].concat(),
    ];
    let mut last = None;
    for (i, cells) in steps.iter().enumerate() {
        build(&device, &tiles, cells, &run, 3);
        let want = mirror.build(cells);
        assert_eq!(
            want.exhausted(2),
            0,
            "step {i}: the pool and table hold it all"
        );
        check(&tiles, &want, 3, &format!("step {i}"));
        if i == 1 {
            assert!(!want.freed.is_empty(), "step 1 frees slots");
            assert!(
                want.freed.windows(2).all(|w| w[0] < w[1]),
                "frees in slot order"
            );
            assert!(!want.retired.is_empty(), "step 1 retires slots out of C");
            assert!(!want.fresh.is_empty(), "step 1 hands out slots");
        }
        last = Some(want);
    }
    let child = last.expect("three steps");

    let table = read(tiles.table(), table_words(512) as usize);
    let counts = read(tiles.counts(), count_words(2) as usize);
    build(&device, &tiles, &steps[0], &plan(&device, false), 3);
    assert_eq!(
        read(tiles.table(), table.len()),
        table,
        "inactive: table kept"
    );
    assert_eq!(
        read(tiles.counts(), counts.len()),
        counts,
        "inactive: counts kept"
    );
    let args = read(tiles.args(), args_words(2) as usize);
    for offset in [
        list_args_offset(0),
        list_args_offset(1),
        list_args_offset(2),
        retired_args_offset(2),
        fresh_args_offset(2),
    ] {
        assert_eq!(
            args[offset as usize / 4],
            0,
            "inactive: triple at {offset} off"
        );
    }
    let range = 2 * replay_range_index(2) as usize;
    assert_eq!(args[range..range + 2], [0, 0], "inactive: no replay");

    let mut parents = WorldTiles::new_parents(&device, 256).expect("parents");
    parents.prepare(&device);
    let mut enc = device.create_encoder("world tiles parents proof");
    parents.encode_parents(&mut enc, &tiles, &run);
    enc.commit_and_wait_completed();
    let cells: Vec<[i32; 3]> = child
        .slot_tile
        .iter()
        .flatten()
        .map(|t| t.map(|c| (c >> 1) * 8))
        .collect();
    let mut parent_mirror = Mirror::new(256, parents.reach(), true);
    let want = parent_mirror.build(&cells);
    assert_eq!(want.exhausted(0), 0, "parents: the pool holds them all");
    check(&parents, &want, 0, "parents");
    let rows = read(parents.child_rows().expect("children rows"), 8 * 256);
    for s in 0..256usize {
        for c in 0..8 {
            let expected = match want.slot_tile[s] {
                Some(t) => child.slot_of(std::array::from_fn(|a| 2 * t[a] + ((c >> a) & 1))),
                None => NONE,
            };
            assert_eq!(
                rows[8 * s + c as usize],
                expected,
                "parent slot {s}, child {c}"
            );
        }
    }
}

/// The tiles within `reach` cells of a cell of `cells`: the cell set C.
fn cell_set(cells: &[[i32; 3]], reach: i32) -> BTreeSet<[i32; 3]> {
    let mut set = BTreeSet::new();
    for &c in cells {
        let (lo, hi) = (
            tile_of_cell(c.map(|v| v - reach)),
            tile_of_cell(c.map(|v| v + reach)),
        );
        for z in lo[2]..=hi[2] {
            for y in lo[1]..=hi[1] {
                for x in lo[0]..=hi[0] {
                    set.insert([x, y, z]);
                }
            }
        }
    }
    set
}

/// A pool of 64 slots for a lone water tile's 125: the cell set C (every tile
/// within cell_reach of a point, 8 here, as the water sits in a corner of its
/// tile) is handed out before the rest of rings 1 and 2, and the pool-short
/// bit is set, as the mirror says, and again when the water moves one tile
/// over. A short pool so never spends the slot a tile holding points needs
/// on an empty band tile.
#[test]
fn a_short_pool_hands_out_the_cell_set_first() {
    let device = test_device();
    let reach = Reach {
        ring_max: 2,
        alloc_ring: 2,
        cell_reach: 2,
    };
    let mut tiles = WorldTiles::new(&device, 64, reach, LIQUID_POINTS).expect("world tiles");
    tiles.prepare(&device);
    let mut mirror = Mirror::new(64, reach, false);
    let run = plan(&device, true);
    for (step, min) in [[-7, 1, 17], [1, 1, 17]].into_iter().enumerate() {
        let cells = cells_of(min, [3, 3, 3]);
        build(&device, &tiles, &cells, &run, 0);
        let want = mirror.build(&cells);
        assert_eq!(
            want.exhausted(2),
            POOL_SHORT,
            "step {step}: pool short, table whole"
        );
        check(&tiles, &want, 0, &format!("step {step}"));
        let set = cell_set(&cells, reach.cell_reach as i32);
        assert_eq!(set.len(), 8, "step {step}: the water's cell set");
        assert!(
            set.iter().all(|&t| want.slot_of(t) != NONE),
            "step {step}: a tile of the cell set has no slot"
        );
        if step == 0 {
            let beyond = want.fresh.iter().any(|&s| {
                let t = want.slot_tile[s as usize].expect("a fresh slot is held");
                !set.contains(&t)
            });
            assert!(beyond, "step 0 fills the cell set, then part of the rest");
        }
    }
}

/// A band tile whose key outranks a water tile's at the same home entry
/// takes that entry in the dilation, after the seed pass found the water
/// tile there, and moves the water tile's key on. Water in tile `w` and two
/// tiles past the band tile `b`, so `b` is in the second tile's ring 2: the
/// build equals the mirror over two steps, one slot a tile under its own key
/// and no alias.
#[test]
fn a_band_tile_taking_a_seed_entry_keeps_one_slot_a_tile() {
    let device = test_device();
    let reach = Reach {
        ring_max: 2,
        alloc_ring: 2,
        cell_reach: 2,
    };
    let pool = 256;
    let entries = table_entries(pool);
    let home = |t: [i32; 3]| key_hash(tile_key(t), entries);
    let box_of = |lo: i32, hi: i32| {
        (lo..hi).flat_map(move |z| (lo..hi).flat_map(move |y| (lo..hi).map(move |x| [x, y, z])))
    };
    let (w, b) = box_of(-2, 2)
        .find_map(|w| {
            box_of(-12, 12)
                .find(|&b| b != w && home(b) == home(w) && tile_key(b) > tile_key(w))
                .map(|b| (w, b))
        })
        .expect("a water tile a band tile displaces");
    let second = [b[0] + 2, b[1], b[2]];
    let mut tiles = WorldTiles::new(&device, pool, reach, LIQUID_POINTS).expect("world tiles");
    tiles.prepare(&device);
    let mut mirror = Mirror::new(pool, reach, false);
    let run = plan(&device, true);
    let cells = [w, second].map(|t| t.map(|v| 8 * v + 3));
    for step in 0..2 {
        build(&device, &tiles, &cells, &run, 0);
        let want = mirror.build(&cells);
        assert_eq!(
            want.exhausted(2),
            0,
            "step {step}: the pool holds both blocks"
        );
        check(&tiles, &want, 0, &format!("step {step}"));
    }
}

/// Two live tiles a key span apart set the alias bit; five tiles in a
/// four-entry table set the table-full bit and leave four of their keys;
/// neither hangs the GPU.
#[test]
fn alias_and_full_table_are_reported() {
    let device = test_device();
    let reach = Reach {
        ring_max: 0,
        alloc_ring: 0,
        cell_reach: 0,
    };
    let run = plan(&device, true);

    let mut tiles = WorldTiles::new(&device, 64, reach, LIQUID_POINTS).expect("world tiles");
    tiles.prepare(&device);
    build(&device, &tiles, &[[0, 0, 0], [8 * 2048, 0, 0]], &run, 0);
    let counts = read(tiles.counts(), count_words(0) as usize);
    assert_ne!(counts[exhausted_word(0)] & ALIAS, 0, "alias reported");
    let keys = read(tiles.table(), tiles.table_entries() as usize);
    let held: Vec<u32> = keys.into_iter().filter(|&k| k != 0).collect();
    assert_eq!(held, [tile_key([0, 0, 0])], "one key for both tiles");

    let mut tiny = WorldTiles::new(&device, 1, reach, LIQUID_POINTS).expect("world tiles");
    tiny.prepare(&device);
    let cells = [[0, 0, 0], [8, 0, 0], [16, 0, 0], [0, 8, 0], [0, 0, -8]];
    build(&device, &tiny, &cells, &run, 0);
    let counts = read(tiny.counts(), count_words(0) as usize);
    assert_eq!(
        counts[exhausted_word(0)],
        TABLE_FULL | POOL_SHORT,
        "full table and short pool reported"
    );
    let all: BTreeSet<u32> = cells.iter().map(|&c| tile_key(tile_of_cell(c))).collect();
    let keys = read(tiny.table(), tiny.table_entries() as usize);
    assert_eq!(keys.len(), 4);
    assert!(
        keys.iter().all(|k| all.contains(k)),
        "four of the five keys kept"
    );
    assert_eq!(keys.iter().collect::<BTreeSet<_>>().len(), 4);
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ProbeParams {
    pool_tiles: u32,
    entries: u32,
    homes: u32,
    cells: u32,
    block_min: [i32; 3],
    side: u32,
}

/// Per (home, cell): `ts_cell_at` of the cell after `ts_home` of the home
/// (none for WT_NONE), and 1 when `ts_cell_stored` and `ts_cell_coord` agree
/// with it.
const PROBE: &str = "struct Probe { pool_tiles: u32, entries: u32, homes: u32, cells: u32, block_min: vec3<i32>, side: u32 }\n\
@group(0) @binding(0) var<uniform> probe: Probe;\n\
@group(0) @binding(1) var<storage, read> table: array<u32>;\n\
@group(0) @binding(2) var<storage, read> homes: array<u32>;\n\
@group(0) @binding(3) var<storage, read_write> out: array<u32>;\n\
fn ts_pool_tiles() -> u32 { return probe.pool_tiles; }\n\
fn ts_table_entries() -> u32 { return probe.entries; }\n\
fn ts_table_word(i: u32) -> u32 { return table[i]; }\n\
@compute @workgroup_size(256)\n\
fn probe_cells(@builtin(global_invocation_id) gid: vec3<u32>) {\n\
    let i = gid.x;\n\
    if i >= probe.homes * probe.cells { return; }\n\
    let home = homes[i / probe.cells];\n\
    let c = i % probe.cells;\n\
    let side = probe.side;\n\
    let p = probe.block_min + vec3<i32>(vec3<u32>(c % side, (c / side) % side, c / (side * side)));\n\
    if home != WT_NONE { ts_home(home); }\n\
    let idx = ts_cell_at(p);\n\
    out[2u * i] = idx;\n\
    var agrees = !ts_cell_stored(p);\n\
    if idx != ts_canon() { agrees = ts_cell_stored(p) && all(ts_cell_coord(idx) == p); }\n\
    out[2u * i + 1u] = select(0u, 1u, agrees);\n\
}\n";

/// For every cell of a 3³ block of tiles across the origin, half of them
/// stored: `ts_cell_at` gives the same index from no home (the hash), from
/// its own tile's home, from a neighbour tile's home (the neighbour row) and
/// from a home two tiles away (the hash again), and that index is the
/// slot the CPU finds in the table words.
#[test]
fn cell_lookup_paths_agree_over_a_block_of_tiles() {
    let device = test_device();
    let reach = Reach {
        ring_max: 0,
        alloc_ring: 0,
        cell_reach: 0,
    };
    let pool = 64;
    let mut tiles = WorldTiles::new(&device, pool, reach, LIQUID_POINTS).expect("world tiles");
    tiles.prepare(&device);
    let block = block_tiles([0, 0, 0]);
    let stored: Vec<[i32; 3]> = block
        .iter()
        .copied()
        .filter(|t| (t[0] + t[1] + t[2]).rem_euclid(2) == 0)
        .collect();
    let cells: Vec<[i32; 3]> = stored.iter().map(|t| t.map(|c| 8 * c + 3)).collect();
    build(&device, &tiles, &cells, &plan(&device, true), 0);

    let h = tiles.table_entries();
    let table = read(tiles.table(), table_words(pool) as usize);
    let slot_of = |t: [i32; 3]| match find(&table[..h as usize], tile_key(t)) {
        Some(e) => table[(h + e) as usize],
        None => NONE,
    };
    for t in &block {
        assert_eq!(
            slot_of(*t) != NONE,
            stored.contains(t),
            "tile {t:?} stored as placed"
        );
    }
    let canon = pool * 512;
    let mut homes: Vec<u32> = block
        .iter()
        .map(|&t| match slot_of(t) {
            NONE => NONE,
            slot => slot * 512 + 37,
        })
        .collect();
    homes.extend([NONE, canon]);
    let side = 24u32;
    let n = side * side * side;
    let params = ProbeParams {
        pool_tiles: pool,
        entries: h,
        homes: homes.len() as u32,
        cells: n,
        block_min: [-8; 3],
        side,
    };
    let homes_buffer = shared(&device, &homes);
    let out = shared(&device, &vec![0u32; 2 * homes.len() * n as usize]);
    let pipeline = device.create_compute_pipeline(
        &format!("{READ_SIDE}\n{PROBE}"),
        "probe_cells",
        "world tiles probe",
    );
    let mut enc = device.create_encoder("world tiles lookup proof");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&params),
            },
            buffer(1, tiles.table()),
            buffer(2, &homes_buffer),
            buffer(3, &out),
        ],
        groups(u64::from(params.homes * n)),
        "world tiles probe",
    );
    enc.commit_and_wait_completed();
    let got = read(&out, 2 * homes.len() * n as usize);
    for (k, &home) in homes.iter().enumerate() {
        for i in 0..n {
            let p = [
                -8 + (i % side) as i32,
                -8 + (i / side % side) as i32,
                -8 + (i / (side * side)) as i32,
            ];
            let slot = slot_of(tile_of_cell(p));
            let want = if slot == NONE {
                canon
            } else {
                slot * 512 + local_index(p)
            };
            let at = 2 * (k * n as usize + i as usize);
            assert_eq!(got[at], want, "home {k} ({home:#x}), cell {p:?}");
            assert_eq!(
                got[at + 1],
                1,
                "home {k}, cell {p:?}: stored and coordinate agree"
            );
        }
    }
    // The centre tile is stored, so its home reads all 27 tiles through its
    // neighbour row; a corner tile's home reaches the far corner by hash.
    assert_ne!(homes[13], NONE);
    assert_ne!(homes[neighbour_index([1, 1, 0]) as usize], NONE);
}
