//! A CPU mirror of the world tiles build (`world_tiles.wgsl`), one build at a
//! time, in sequence: the key set, rings and nearness, the carry, the free
//! stack and the handout in table order. The GPU proofs compare a build's
//! words against it.

use std::collections::BTreeSet;

use manifold_node_engine::scene::world_tiles::{
    EMPTY, NEIGHBOURS, NONE, find, insert, neighbour_index, table_entries, tile_key, tile_of_cell,
};

use super::{ALIAS, POOL_SHORT, Reach, TABLE_FULL};

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Free,
    Carried,
    Fresh,
}

/// The persistent side of a table: what each slot held after the last
/// build, its rank then, and the free stack.
pub(super) struct Mirror {
    reach: Reach,
    pool_tiles: u32,
    entries: u32,
    from_scratch: bool,
    held: Vec<Option<[i32; 3]>>,
    rank: Vec<u32>,
    stack: Vec<u32>,
}

/// What one build leaves.
pub(super) struct Built {
    pub keys: Vec<u32>,
    pub entry_slot: Vec<u32>,
    /// Per slot, its tile, `None` when free.
    pub slot_tile: Vec<Option<[i32; 3]>>,
    pub slot_entry: Vec<u32>,
    pub neighbours: Vec<[u32; NEIGHBOURS as usize]>,
    /// Held slots per rank, `ring_max + 2` ranks.
    pub by_rank: Vec<BTreeSet<u32>>,
    pub retired: BTreeSet<u32>,
    pub fresh: BTreeSet<u32>,
    pub counts: Vec<u32>,
    /// The free stack's live part, bottom first.
    pub stack: Vec<u32>,
    /// Slots freed this build, in the order they were pushed.
    pub freed: Vec<u32>,
}

impl Built {
    /// The slot holding tile t, or `NONE`.
    pub fn slot_of(&self, t: [i32; 3]) -> u32 {
        match find(&self.keys, tile_key(t)) {
            Some(e) => {
                let slot = self.entry_slot[e as usize];
                if slot != NONE && self.slot_tile[slot as usize] == Some(t) {
                    slot
                } else {
                    NONE
                }
            }
            None => NONE,
        }
    }

    pub fn exhausted(&self, ring_max: u32) -> u32 {
        self.counts[super::exhausted_word(ring_max)]
    }
}

fn chebyshev(d: [i32; 3]) -> u32 {
    d.iter()
        .map(|c| c.unsigned_abs())
        .max()
        .expect("three axes")
}

fn add(a: [i32; 3], b: [i32; 3]) -> [i32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// Every offset of the cube of half-side `r`.
fn cube(r: i32) -> impl Iterator<Item = [i32; 3]> {
    (-r..=r).flat_map(move |z| (-r..=r).flat_map(move |y| (-r..=r).map(move |x| [x, y, z])))
}

impl Mirror {
    pub fn new(pool_tiles: u32, reach: Reach, from_scratch: bool) -> Self {
        let p = pool_tiles as usize;
        Self {
            reach,
            pool_tiles,
            entries: table_entries(pool_tiles),
            from_scratch,
            held: vec![None; p],
            rank: vec![0; p],
            stack: (0..pool_tiles).rev().collect(),
        }
    }

    fn rank_of(&self, ring: u32, near: u32) -> u32 {
        if ring <= 1 {
            u32::from(near > self.reach.cell_reach)
        } else {
            ring
        }
    }

    /// One build from points in `cells`, its rings inside `clip`.
    pub fn build(&mut self, cells: &[[i32; 3]], clip: Option<super::TileClip>) -> Built {
        let [lo, hi] = clip.unwrap_or(super::NO_CLIP);
        let inside = |t: [i32; 3]| (0..3).all(|a| lo[a] <= t[a] && t[a] <= hi[a]);
        let h = self.entries as usize;
        let p = self.pool_tiles as usize;
        let r = self.reach.ring_max as i32;
        let mut exhausted = 0;
        let mut keys = vec![EMPTY; h];
        let water: BTreeSet<[i32; 3]> = cells.iter().map(|&c| tile_of_cell(c)).collect();
        for &t in &water {
            if !insert(&mut keys, tile_key(t)) {
                exhausted |= TABLE_FULL;
            }
        }
        let seeds: Vec<[i32; 3]> = water
            .iter()
            .copied()
            .filter(|&t| find(&keys, tile_key(t)).is_some())
            .collect();
        for &s in &seeds {
            for d in cube(r).filter(|&d| d != [0; 3] && inside(add(s, d))) {
                if keys.iter().filter(|&&k| k != EMPTY).count() >= h / 2 {
                    exhausted |= TABLE_FULL;
                    continue;
                }
                if !insert(&mut keys, tile_key(add(s, d))) {
                    exhausted |= TABLE_FULL;
                }
            }
        }
        let mut ring = vec![self.reach.ring_max + 1; h];
        let mut tile: Vec<Option<[i32; 3]>> = vec![None; h];
        for &s in &seeds {
            for d in cube(r) {
                let t = add(s, d);
                let Some(e) = find(&keys, tile_key(t)) else {
                    continue;
                };
                let e = e as usize;
                ring[e] = ring[e].min(chebyshev(d));
                let written = tile[e];
                match written {
                    None => tile[e] = Some(t),
                    Some(other) if other != t => exhausted |= ALIAS,
                    Some(_) => {}
                }
            }
        }
        let reach = self.reach.cell_reach as i32;
        let mut near = vec![self.reach.cell_reach + 1; h];
        for &c in cells {
            let first = tile_of_cell(c.map(|v| v - reach));
            let last = tile_of_cell(c.map(|v| v + reach));
            for z in first[2]..=last[2] {
                for y in first[1]..=last[1] {
                    for x in first[0]..=last[0] {
                        let q = [x, y, z];
                        let Some(e) = find(&keys, tile_key(q)) else {
                            continue;
                        };
                        let lo = q.map(|v| 8 * v);
                        let d: [i32; 3] =
                            std::array::from_fn(|a| (lo[a] - c[a]).max(c[a] - lo[a] - 7).max(0));
                        near[e as usize] = near[e as usize].min(chebyshev(d));
                    }
                }
            }
        }

        let mut entry_slot = vec![NONE; h];
        let mut state = vec![State::Free; p];
        let mut freed = Vec::new();
        if self.from_scratch {
            self.stack = (0..self.pool_tiles).rev().collect();
        } else {
            for (s, slot_state) in state.iter_mut().enumerate().take(p) {
                let Some(t) = self.held[s] else {
                    continue;
                };
                match find(&keys, tile_key(t)) {
                    Some(e)
                        if ring[e as usize] <= self.reach.alloc_ring
                            && tile[e as usize] == Some(t) =>
                    {
                        entry_slot[e as usize] = s as u32;
                        *slot_state = State::Carried;
                    }
                    _ => freed.push(s as u32),
                }
            }
            self.stack.extend(&freed);
        }

        let wants_class = |e: usize, class: u32| {
            keys[e] != EMPTY
                && entry_slot[e] == NONE
                && ring[e] <= self.reach.alloc_ring
                && u32::from(self.rank_of(ring[e], near[e]) != 0) == class
        };
        let wants: Vec<usize> = (0..h)
            .filter(|&e| wants_class(e, 0))
            .chain((0..h).filter(|&e| wants_class(e, 1)))
            .collect();
        let avail = self.stack.len();
        for (j, &e) in wants.iter().enumerate() {
            if j >= avail {
                exhausted |= POOL_SHORT;
                continue;
            }
            let slot = self.stack[avail - 1 - j];
            entry_slot[e] = slot;
            state[slot as usize] = State::Fresh;
        }
        self.stack.truncate(avail - wants.len().min(avail));

        let mut slot_entry = vec![NONE; p];
        let mut slot_tile = vec![None; p];
        for (e, &slot) in entry_slot.iter().enumerate() {
            if slot != NONE {
                slot_entry[slot as usize] = e as u32;
                slot_tile[slot as usize] = tile[e];
            }
        }
        let mut built = Built {
            keys,
            entry_slot,
            slot_tile,
            slot_entry,
            neighbours: vec![[NONE; NEIGHBOURS as usize]; p],
            by_rank: vec![BTreeSet::new(); self.reach.ring_max as usize + 2],
            retired: BTreeSet::new(),
            fresh: BTreeSet::new(),
            counts: Vec::new(),
            stack: self.stack.clone(),
            freed,
        };
        for (s, slot_state) in state.iter().enumerate().take(p) {
            let Some(t) = built.slot_tile[s] else {
                continue;
            };
            let mut row = [NONE; NEIGHBOURS as usize];
            for d in cube(1) {
                row[neighbour_index(d) as usize] = built.slot_of(add(t, d));
            }
            built.neighbours[s] = row;
            let e = built.slot_entry[s] as usize;
            let rank = self.rank_of(ring[e], near[e]);
            built.by_rank[rank as usize].insert(s as u32);
            if *slot_state == State::Carried && self.rank[s] == 0 && rank != 0 {
                built.retired.insert(s as u32);
            }
            if *slot_state == State::Fresh {
                built.fresh.insert(s as u32);
            }
            self.rank[s] = rank;
        }
        let mut held = 0;
        for set in &built.by_rank {
            held += set.len() as u32;
            built.counts.push(held);
        }
        built.counts.extend([
            built.retired.len() as u32,
            self.stack.len() as u32,
            exhausted,
            built.fresh.len() as u32,
        ]);
        self.held = built.slot_tile.clone();
        built
    }
}
