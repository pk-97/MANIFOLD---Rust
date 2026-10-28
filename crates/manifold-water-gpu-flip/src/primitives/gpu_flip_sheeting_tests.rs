//! GPU sheeting against `manifold_fluids::sheeter` (the CPU port proven
//! against FLIP's own sheeter) on the same level set and ordered markers, on
//! lattices of world tiles that straddle the origin.
//!
//! The port's grid is a dense box from its own origin. The stage's grid is the
//! held tiles, with no border band, so a fixture sits inside a margin of air
//! and the box is whole tiles: the port's zero outside and its border then
//! fall where nothing happens, and the stage's tiles end where the port's box
//! does. Marker positions go to the stage as world positions; the port gets
//! them back in its own frame as the stage reads them, so every cell and
//! half-cell decision sees the same floats.
use super::gpu_flip_sheeting::{FILL_THRESHOLD, GpuSheeting, SheetInputs};
use super::gpu_flip_sheeting_cpu_tests::bucket_fixture;
use manifold_fluids::sheeter;
use manifold_gpu::BufferUse;
use manifold_gpu::{GpuBuffer, GpuDevice};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::scene::world_tiles::{
    NONE, lattice_origin, local_index, pool_len, slot_row_word, table_entries, table_words,
    tile_of_cell,
};
use manifold_node_engine::testkit::array_harness::read;
use manifold_water_liquid::testkit::marker_phi::marker_phi;
use manifold_water_liquid::world_tiles::{
    Allocation, LIQUID_POINTS, PointCount, Reach, WorldTiles, exhausted_word,
};
use std::collections::HashMap;

pub(super) fn shared(device: &GpuDevice, bytes: &[u8]) -> GpuBuffer {
    let buffer = device.create_buffer(bytes.len().max(16) as u64, BufferUse::Upload);
    buffer.zero_fill();
    // SAFETY: a fresh shared buffer sized for `bytes`; no GPU work in flight.
    unsafe { buffer.write(0, bytes) };
    buffer
}

pub(super) fn copy_out(device: &GpuDevice, buffer: &GpuBuffer) -> Vec<u32> {
    let staged = device.create_buffer(buffer.size, BufferUse::Upload);
    let mut enc = device.create_encoder("sheeting proof readback");
    enc.copy_buffer_to_buffer(buffer, &staged, buffer.size);
    enc.commit_and_wait().expect("GPU work completes");
    read(&staged, (buffer.size / 4) as usize)
}

/// A world tile table over the whole tiles of a box of cells, built by the
/// water's own build from one particle in each tile, so the slot order is the
/// build's (table order), not the tile order.
pub(super) struct World {
    pub tiles: WorldTiles,
    /// The box's first cell and its size in cells, both whole tiles.
    pub lo: [i32; 3],
    pub size: [i32; 3],
    slot_of: HashMap<[i32; 3], u32>,
}

impl World {
    pub(super) fn new(device: &GpuDevice, lo: [i32; 3], size: [i32; 3], h: f32) -> Self {
        assert!(
            lo.iter().chain(&size).all(|v| v % 8 == 0),
            "{lo:?} {size:?}: whole tiles"
        );
        let mut points = Vec::new();
        for tz in lo[2] / 8..(lo[2] + size[2]) / 8 {
            for ty in lo[1] / 8..(lo[1] + size[1]) / 8 {
                for tx in lo[0] / 8..(lo[0] + size[0]) / 8 {
                    // The tile's first cell, at its centre.
                    let cell = [tx * 8, ty * 8, tz * 8].map(|c| c as f32 * h);
                    points.push(FluidParticle {
                        position_radius: [cell[0], cell[1], cell[2], 0.05],
                        velocity: [0.0; 3],
                        id: points.len() as u32 + 1,
                    });
                }
            }
        }
        let count = points.len() as u32;
        let reach = Reach {
            ring_max: 0,
            alloc_ring: 0,
            cell_reach: 0,
        };
        let mut tiles = WorldTiles::new(device, count, reach, LIQUID_POINTS).expect("world tiles");
        tiles.prepare(device);
        let records = shared(device, bytemuck::cast_slice(&points));
        // The slots in use, as the step's slot record holds them.
        let record = shared(
            device,
            bytemuck::cast_slice(&[count.div_ceil(256), 1, 1, count]),
        );
        let gate = shared(device, &[0u8; 48]);
        let mut enc = device.create_encoder("sheeting proof tiles");
        tiles.encode_allocation(
            &mut enc,
            &Allocation {
                points: &records,
                count: PointCount::Record(&record),
                origin: lattice_origin(h),
                inv_cell: 1.0 / h,
                replay_counts: [0; 2],
                gate: &gate,
                clip: None,
                kept: None,
            },
        );
        enc.commit_and_wait().expect("GPU work completes");
        let words: Vec<u32> = read(tiles.table(), table_words(count) as usize);
        let entries = table_entries(count);
        let slot_of = (0..count)
            .map(|slot| {
                let row = slot_row_word(entries, slot) as usize;
                assert_ne!(words[row + 3], NONE, "slot {slot} is held");
                (
                    [words[row], words[row + 1], words[row + 2]].map(|w| w as i32),
                    slot,
                )
            })
            .collect::<HashMap<_, _>>();
        assert_eq!(slot_of.len(), count as usize, "every tile has its own slot");
        Self {
            tiles,
            lo,
            size,
            slot_of,
        }
    }

    pub(super) fn pool_tiles(&self) -> u32 {
        self.tiles.pool_tiles()
    }

    /// The pool index of cell `p`; None where its tile holds no slot.
    pub(super) fn index(&self, p: [i32; 3]) -> Option<usize> {
        let slot = *self.slot_of.get(&tile_of_cell(p))?;
        Some(slot as usize * 512 + local_index(p) as usize)
    }

    /// Every cell of the box, x fastest.
    pub(super) fn cells(&self) -> Vec<[i32; 3]> {
        let mut cells = Vec::new();
        for z in 0..self.size[2] {
            for y in 0..self.size[1] {
                for x in 0..self.size[0] {
                    cells.push([self.lo[0] + x, self.lo[1] + y, self.lo[2] + z]);
                }
            }
        }
        cells
    }

    /// A dense field over the box (x fastest) in pool layout; the canonical
    /// element holds `canon`.
    pub(super) fn to_pool<T: Copy>(&self, dense: &[T], canon: T) -> Vec<T> {
        let mut pool = vec![canon; pool_len(self.pool_tiles()) as usize];
        let cells = self.cells();
        assert_eq!(dense.len(), cells.len());
        for (value, cell) in dense.iter().zip(cells) {
            pool[self.index(cell).expect("a box cell is held")] = *value;
        }
        pool
    }

    /// A pooled field back to a dense one over the box.
    pub(super) fn dense_of<T: Copy>(&self, pool: &[T]) -> Vec<T> {
        self.cells()
            .into_iter()
            .map(|cell| pool[self.index(cell).expect("a box cell is held")])
            .collect()
    }
}

/// A clock plan (gpu_flip_step/common.wgsl ClockPlan): live, with `step_dt`.
pub(super) fn live_plan(device: &GpuDevice, step_dt: f32) -> GpuBuffer {
    let mut words = [0u32; 12];
    words[0] = step_dt.to_bits();
    words[11] = 1;
    shared(device, bytemuck::cast_slice(&words))
}

/// A record of `n` slots in use, as a step writes it: `[groups, 1, 1, n]`.
pub(super) fn slots_record(device: &GpuDevice, n: u32) -> GpuBuffer {
    shared(
        device,
        bytemuck::cast_slice(&[n.div_ceil(256).max(1), 1, 1, n]),
    )
}

/// Marker positions rounded to multiples of 2^-10 m: with a cell of 0.25 m,
/// whole-cell offsets and the half-cell lattice origin, every frame change in
/// the proofs is exact in f32.
pub(super) fn quantised(markers: &[[f32; 3]]) -> Vec<[f32; 3]> {
    markers
        .iter()
        .map(|m| m.map(|c| (c * 1024.0).round() / 1024.0))
        .collect()
}

/// Cells of a box holding `cells` with `margin` cells of room on the low side
/// and at least as much on the high side, in whole tiles.
pub(super) fn padded_cells(cells: [u32; 3], margin: u32) -> [u32; 3] {
    cells.map(|n| (n + 2 * margin).div_ceil(8) * 8)
}

/// The markers `margin` cells in from the box's low corner.
pub(super) fn padded_markers(markers: &[[f32; 3]], margin: u32, h: f32) -> Vec<[f32; 3]> {
    let shift = margin as f32 * h;
    markers.iter().map(|m| m.map(|c| c + shift)).collect()
}

/// A level set over `cells` placed `margin` cells in from the low corner of a
/// box of `big` cells, air (3h) everywhere else.
pub(super) fn padded_phi(
    phi: &[f32],
    cells: [u32; 3],
    big: [u32; 3],
    margin: u32,
    h: f32,
) -> Vec<f32> {
    let mut out = vec![3.0 * h; (big[0] * big[1] * big[2]) as usize];
    for z in 0..cells[2] {
        for y in 0..cells[1] {
            for x in 0..cells[0] {
                let at = (x + margin) + big[0] * ((y + margin) + big[1] * (z + margin));
                out[at as usize] = phi[(x + cells[0] * (y + cells[1] * z)) as usize];
            }
        }
    }
    out
}

/// Port-frame marker positions of a box whose first cell is `lo` as world
/// positions, and as the stage reads them back in the port's frame: the
/// lattice frame (world minus the origin) less the box's low corner.
pub(super) fn to_world(
    markers: &[[f32; 3]],
    lo: [i32; 3],
    h: f32,
) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
    let origin = lattice_origin(h);
    let corner = lo.map(|c| c as f32 * h);
    let world: Vec<[f32; 3]> = markers
        .iter()
        .map(|m| std::array::from_fn(|a| (m[a] + corner[a]) + origin[a]))
        .collect();
    let local = world
        .iter()
        .map(|w| std::array::from_fn(|a| (w[a] - origin[a]) - corner[a]))
        .collect();
    (world, local)
}

/// Markers sorted by cell (stable), and the order and ranges the stage reads.
pub(super) struct Sorted {
    pub particles: GpuBuffer,
    pub order: GpuBuffer,
    pub ranges: GpuBuffer,
    pub count: u32,
    /// The slots in use: every marker.
    pub record: GpuBuffer,
}

/// Sort `markers` (world positions, radius 0.1) by the pool index of their
/// cell, as the sort does, with room for `capacity` particles. A marker's cell
/// is the engine's f64 floor of its lattice-local position, the stage's own.
pub(super) fn sort(
    device: &GpuDevice,
    world: &World,
    markers: &[[f32; 3]],
    h: f32,
    capacity: usize,
) -> Sorted {
    let origin = lattice_origin(h);
    let inv = 1.0 / f64::from(h);
    let cell_of = |m: &[f32; 3]| -> [i32; 3] {
        std::array::from_fn(|a| (f64::from(m[a] - origin[a]) * inv).floor() as i32)
    };
    let mut keyed: Vec<(usize, usize)> = markers
        .iter()
        .enumerate()
        .map(|(i, m)| {
            (
                world.index(cell_of(m)).expect("a marker is in a held tile"),
                i,
            )
        })
        .collect();
    // By cell, then by input index: stable within a cell.
    keyed.sort_unstable();
    let len = pool_len(world.pool_tiles()) as usize;
    let mut ranges = vec![[0u32; 2]; len];
    for (rank, &(cell, _)) in keyed.iter().enumerate() {
        if ranges[cell][1] == 0 {
            ranges[cell][0] = rank as u32;
        }
        ranges[cell][1] += 1;
    }
    // The canonical key holds no marker: the live end is every marker.
    ranges[len - 1] = [markers.len() as u32, 0];
    let mut particles = vec![FluidParticle::default(); capacity];
    let mut order = vec![0u32; capacity];
    for (slot, &(_, i)) in keyed.iter().enumerate() {
        let m = markers[i];
        particles[slot] = FluidParticle {
            position_radius: [m[0], m[1], m[2], 0.1],
            velocity: [0.0; 3],
            id: i as u32 + 1,
        };
        order[slot] = i as u32;
    }
    Sorted {
        particles: shared(device, bytemuck::cast_slice(&particles)),
        order: shared(device, bytemuck::cast_slice(&order)),
        ranges: shared(device, bytemuck::cast_slice(&ranges)),
        count: markers.len() as u32,
        record: slots_record(device, markers.len() as u32),
    }
}

struct Run {
    /// Births: x, y, z, key bits.
    births: Vec<[u32; 4]>,
    count: u32,
    /// Births whose recomputed claim failed in `place`.
    unresolved: u32,
    /// Per pool cell: bit o marks offset o a candidate, bit 8 + o a claimant,
    /// bit 16 + o a birth the draw kept.
    flags: Vec<u32>,
    /// Per pool cell and half-cell: the winning key.
    claims: Vec<[u32; 8]>,
}

fn run(
    device: &GpuDevice,
    stage: &GpuSheeting,
    sorted: &Sorted,
    phi: &GpuBuffer,
    h: f32,
    plan: Option<&GpuBuffer>,
    world: &World,
) -> Run {
    let inputs = SheetInputs {
        particles: &sorted.particles,
        order: &sorted.order,
        ranges: &sorted.ranges,
        count: (&sorted.record, 0),
        phi,
        h,
        threshold: FILL_THRESHOLD,
    };
    let mut enc = device.create_encoder("sheeting proof");
    match plan {
        Some(plan) => stage.encode_gated(&mut enc, &inputs, plan, &world.tiles),
        None => stage.encode(&mut enc, &inputs, &world.tiles),
    }
    enc.commit_and_wait().expect("GPU work completes");
    let (births, count) = stage.births();
    let words = copy_out(device, count);
    let births: Vec<u32> = copy_out(device, births);
    Run {
        births: births
            .chunks_exact(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect(),
        count: words[0],
        unresolved: words[1],
        flags: copy_out(device, &stage.scratch()[6]),
        claims: bytemuck::cast_slice::<u32, [u32; 8]>(&copy_out(device, &stage.scratch()[5]))
            .to_vec(),
    }
}

/// The sheeter's key for offset `o` of world cell `cell`: the engine's
/// visiting order, as the shader's `site_key` (buckets biased by 128 and
/// wrapped at 256).
fn site_key(cell: [i32; 3], o: usize) -> u32 {
    let b = cell.map(|c| c >> 1);
    let in_bucket = ((cell[2] & 1) * 4 + (cell[1] & 1) * 2 + (cell[0] & 1)) as u32;
    let k = b.map(|v| ((v + 128) & 255) as u32);
    (((k[2] << 16) | (k[1] << 8) | k[0]) << 6) | (in_bucket << 3) | o as u32
}

/// The cell and offset a key names, for keys of buckets within 128 of the
/// origin.
fn decode_key(key: u32) -> ([i32; 3], usize) {
    let (o, in_bucket) = ((key & 7) as usize, ((key >> 3) & 7) as i32);
    let k = [key >> 6, key >> 14, key >> 22].map(|v| (v & 255) as i32 - 128);
    let cell = [
        2 * k[0] + (in_bucket & 1),
        2 * k[1] + ((in_bucket >> 1) & 1),
        2 * k[2] + ((in_bucket >> 2) & 1),
    ];
    (cell, o)
}

/// The half-cell site a port candidate centre (port frame) names, in world
/// cells of the box whose first cell is `lo`: the cell and the offset.
fn site_of(p: [f32; 3], lo: [i32; 3], h: f32) -> ([i32; 3], usize) {
    let s = p.map(|c| (c / (0.5 * h)).floor() as i32);
    let cell = std::array::from_fn(|a| (s[a] >> 1) + lo[a]);
    (
        cell,
        ((s[0] & 1) * 4 + (s[1] & 1) * 2 + (s[2] & 1)) as usize,
    )
}

/// Births come out numbered by slot, then the cell in the slot, then the
/// offset, whatever order the GPU's threads ran in.
fn assert_numbered(world: &World, births: &[[u32; 4]], name: &str) {
    let places: Vec<(usize, usize)> = births
        .iter()
        .map(|b| {
            let (cell, o) = decode_key(b[3]);
            (world.index(cell).expect("a birth is in a held tile"), o)
        })
        .collect();
    assert!(
        places.windows(2).all(|w| w[0] < w[1]),
        "{name}: births are not numbered by slot, cell and offset"
    );
}

/// GPU against the CPU port on one fixture: births, claimants (more than
/// births when projections compete), cells holding several thin markers.
/// `markers`, `phi` and `cells` are the fixture; the stage sees it `margin`
/// cells in from the low corner of a box of whole tiles whose first cell is
/// `lo`.
fn matches_port(
    device: &GpuDevice,
    name: &str,
    fixture: (&[[f32; 3]], Option<&[f32]>, [u32; 3]),
    h: f32,
    lo: [i32; 3],
) -> (u32, u32, usize) {
    const MARGIN: u32 = 8;
    let (fixture_markers, fixture_phi, fixture_cells) = fixture;
    let cells = padded_cells(fixture_cells, MARGIN);
    let markers = padded_markers(fixture_markers, MARGIN, h);
    let phi = match fixture_phi {
        Some(phi) => padded_phi(phi, fixture_cells, cells, MARGIN, h),
        None => marker_phi(&markers, cells, h),
    };
    let (world_markers, markers) = to_world(&markers, lo, h);
    let trace = sheeter::trace_sheet_particles(&markers, &phi, cells, f64::from(h), FILL_THRESHOLD)
        .expect("port");
    let world = World::new(device, lo, cells.map(|n| n as i32), h);
    let sorted = sort(device, &world, &world_markers, h, markers.len());
    let phi_buffer = shared(device, bytemuck::cast_slice(&world.to_pool(&phi, 3.0 * h)));
    let mut stage = GpuSheeting::default();
    stage.prepare(device);
    stage
        .reserve(device, 1 << 16, world.pool_tiles())
        .expect("reserve");
    let gpu = run(device, &stage, &sorted, &phi_buffer, h, None, &world);
    assert_numbered(&world, &gpu.births[..gpu.count as usize], name);

    // Candidates by identity. The port lists them as generated; the engine
    // visits them bucket by bucket (k, j, i) in that order within a bucket,
    // which is ascending key when each bucket's keys rise as generated.
    let want_sites: Vec<(usize, u32)> = trace
        .candidates
        .iter()
        .map(|&p| {
            let (cell, o) = site_of(p, lo, h);
            (8 * world.index(cell).unwrap() + o, site_key(cell, o))
        })
        .collect();
    let mut last = HashMap::new();
    for &(_, key) in &want_sites {
        let previous = last.insert(key / 64, key);
        assert!(
            previous.is_none_or(|p| p < key),
            "{name}: key {key} out of the engine's order in its bucket"
        );
    }
    let mut got_sites: Vec<usize> = world
        .cells()
        .into_iter()
        .flat_map(|cell| {
            let at = world.index(cell).unwrap();
            let flags = gpu.flags[at];
            (0..8)
                .filter(move |o| flags & (1 << o) != 0)
                .map(move |o| 8 * at + o)
        })
        .collect();
    let mut sorted_want: Vec<usize> = want_sites.iter().map(|s| s.0).collect();
    sorted_want.sort_unstable();
    got_sites.sort_unstable();
    assert_eq!(got_sites, sorted_want, "{name}: candidate sites differ");

    // The accepted set by source candidate, and the births' positions.
    let want: Vec<(u32, [f32; 3])> = trace
        .seed_candidates
        .iter()
        .zip(&trace.seeds)
        .map(|(&c, &p)| {
            let (cell, o) = site_of(trace.candidates[c], lo, h);
            (site_key(cell, o), p)
        })
        .collect();
    // The port seeds in visiting order, ascending key.
    assert!(
        want.windows(2).all(|w| w[0].0 < w[1].0),
        "{name}: the port seeds out of key order"
    );
    assert_eq!(
        gpu.count as usize,
        want.len(),
        "{name}: birth counts differ"
    );
    let corner = lo.map(|c| f64::from(c) * f64::from(h));
    let mut got: Vec<(u32, [f32; 3])> = gpu.births[..gpu.count as usize]
        .iter()
        .map(|b| (b[3], [b[0], b[1], b[2]].map(f32::from_bits)))
        .collect();
    got.sort_by_key(|g| g.0);
    assert_eq!(
        got.iter().map(|g| g.0).collect::<Vec<_>>(),
        want.iter().map(|w| w.0).collect::<Vec<_>>(),
        "{name}: births differ in identity"
    );
    // Every winning claim resolves exactly once, into its own sub-cell.
    assert_eq!(gpu.unresolved, 0, "{name}: a recomputed claim failed");
    let inv_sub = 1.0 / (0.5 * f64::from(h));
    let mut held = std::collections::HashSet::new();
    for (key, p) in &got {
        let s = p.map(|c| (f64::from(c) * inv_sub).floor() as i32);
        let at = world
            .index(s.map(|v| v >> 1))
            .expect("a birth is in a held tile");
        let sub = ((s[0] & 1) + 2 * (s[1] & 1) + 4 * (s[2] & 1)) as usize;
        assert_eq!(
            gpu.claims[at][sub], *key,
            "{name}: birth {key} does not hold its sub-cell"
        );
        assert!(
            held.insert(8 * at + sub),
            "{name}: two births in one sub-cell"
        );
    }
    // And the other way: every sub-cell a claim won holds a birth of that
    // key, so no winner is lost between the claim and the births.
    // The pool's last element is the canonical one, which no pass clears.
    let claimed: std::collections::HashSet<usize> = (0..8 * (gpu.claims.len() - 1))
        .filter(|&i| gpu.claims[i / 8][i % 8] != u32::MAX)
        .collect();
    assert_eq!(
        claimed.len(),
        got.len(),
        "{name}: claims won and births differ in number"
    );
    assert!(claimed == held, "{name}: a won claim has no birth");
    let claimants: u32 = world
        .cells()
        .into_iter()
        .map(|cell| ((gpu.flags[world.index(cell).unwrap()] >> 8) & 255).count_ones())
        .sum();
    // Several markers of one cell passing the depth walk: detect's shared flag.
    let mut thin_per_cell = std::collections::HashMap::new();
    for (_, p) in markers.iter().enumerate().filter(|(m, _)| trace.thin[*m]) {
        *thin_per_cell
            .entry(p.map(|c| (f64::from(c) / f64::from(h)).floor() as i64))
            .or_insert(0) += 1;
    }
    let shared_cells = thin_per_cell.values().filter(|&&n| n > 1).count();
    // The births come back in the lattice frame, so the box's corner and the
    // frame's magnitude cost a few float steps beyond the port's own.
    let reach = lo
        .iter()
        .zip(cells)
        .map(|(&c, n)| f64::from(c.abs().max(c + n as i32)))
        .fold(0.0, f64::max)
        * f64::from(h);
    let frame = 4.0 * f64::from(f32::EPSILON) * reach;
    let worst = got
        .iter()
        .zip(&want)
        .flat_map(|(g, w)| {
            (0..3).map(move |a| (f64::from(g.1[a]) - corner[a] - f64::from(w.1[a])).abs())
        })
        .fold(0.0f64, f64::max);
    // The port fuses multiply-adds as the engine build does; Metal fuses its own way.
    assert!(
        worst < 1e-5 * f64::from(h) + frame,
        "{name}: birth positions differ by {worst}"
    );
    assert!(gpu.count > 0, "{name}: no births");
    eprintln!(
        "SHEETING {name}: {} candidates, {claimants} claimants, {} births, {shared_cells} cells with several thin markers, worst position difference {worst:e} m",
        got_sites.len(),
        gpu.count
    );

    // One bounded timing: the GPU time of a command buffer holding only the
    // sheeting passes, on the warm stage, median of five.
    let inputs = SheetInputs {
        particles: &sorted.particles,
        order: &sorted.order,
        ranges: &sorted.ranges,
        count: (&sorted.record, 0),
        phi: &phi_buffer,
        h,
        threshold: FILL_THRESHOLD,
    };
    let mut times = Vec::new();
    for _ in 0..5 {
        let mut enc = device.create_encoder("sheeting timing");
        stage.encode(&mut enc, &inputs, &world.tiles);
        times.push(enc.commit_and_wait().expect("GPU work completes"));
    }
    times.sort_by(f64::total_cmp);
    eprintln!(
        "SHEETING {name}: GPU time median {:.3} ms over 5",
        times[2] * 1e3
    );
    (gpu.count, claimants, shared_cells)
}

#[test]
fn gpu_flip_sheeting_matches_the_port_on_a_splash() {
    let device = manifold_gpu::testkit::test_device();
    let (markers, analytic, cells, dx) = sheeter::fixtures::splash();
    let h = dx as f32;
    let markers = quantised(&markers);
    // 56 cells of whole tiles from cell -24 on every axis: the origin is inside.
    let lo = [-24; 3];
    let (births, claimants, shared) = matches_port(
        &device,
        "analytic",
        (&markers, Some(&analytic), cells),
        h,
        lo,
    );
    assert!(
        claimants > births,
        "projections compete for sub-cells: {claimants} claimants, {births} births"
    );
    assert!(shared > 0, "several thin markers share a cell");
    matches_port(&device, "markers", (&markers, None, cells), h, lo);
}

/// An inactive clock slot leaves every buffer the stage owns as the last
/// active slot left it; a rerun repeats the births exactly; a short birth list
/// keeps the first births and the true count.
#[test]
fn gpu_flip_sheeting_inactive_slot_and_capacity() {
    let device = manifold_gpu::testkit::test_device();
    let (markers, phi, cells, dx) = sheeter::fixtures::splash();
    let h = dx as f32;
    let lo = [-24; 3];
    let big = padded_cells(cells, 8);
    let (world_markers, _) = to_world(&padded_markers(&quantised(&markers), 8, h), lo, h);
    let phi = padded_phi(&phi, cells, big, 8, h);
    let world = World::new(&device, lo, big.map(|n| n as i32), h);
    let sorted = sort(&device, &world, &world_markers, h, world_markers.len());
    let phi_buffer = shared(&device, bytemuck::cast_slice(&world.to_pool(&phi, 3.0 * h)));
    let mut stage = GpuSheeting::default();
    stage.prepare(&device);
    stage.reserve(&device, 1 << 16, world.pool_tiles()).unwrap();
    let full = run(
        &device,
        &stage,
        &sorted,
        &phi_buffer,
        h,
        Some(&live_plan(&device, 0.01)),
        &world,
    );
    assert!(full.count > 3);
    assert_numbered(&world, &full.births[..full.count as usize], "full");
    let snapshot = |stage: &GpuSheeting| -> Vec<Vec<u32>> {
        stage.scratch()[..9]
            .iter()
            .chain([stage.winners()])
            .map(|b| copy_out(&device, b))
            .collect()
    };
    let before = snapshot(&stage);
    let other: Vec<f32> = phi.iter().map(|v| -v).collect();
    let other = shared(
        &device,
        bytemuck::cast_slice(&world.to_pool(&other, 3.0 * h)),
    );
    run(
        &device,
        &stage,
        &sorted,
        &other,
        h,
        Some(&live_plan(&device, 0.0)),
        &world,
    );
    assert!(
        before == snapshot(&stage),
        "an inactive slot wrote to the stage"
    );
    let again = run(&device, &stage, &sorted, &phi_buffer, h, None, &world);
    assert!(
        again.births == full.births && again.count == full.count,
        "a rerun changed the births"
    );

    let mut short = GpuSheeting::default();
    short.prepare(&device);
    short.reserve(&device, 3, world.pool_tiles()).unwrap();
    let three = run(&device, &short, &sorted, &phi_buffer, h, None, &world);
    assert_eq!(
        three.count, full.count,
        "a short list still counts every birth"
    );
    assert_eq!(
        three.births[..3],
        full.births[..3],
        "a short list keeps the first births"
    );
}

/// Cell and half-cell indices are the engine's f64 floor at non-binary
/// spacing, on the floats either side of every cell and half-cell face, and
/// of their mirrors below zero, where interpolation and the walk sample. A
/// plain f32 product gets some of them wrong (f32 0.3 at 1.5 m: cell 5, where
/// the engine's is 4), so the probe can tell. The probe takes positions as
/// lattice-local already, so it tests the floor alone.
#[test]
fn gpu_flip_sheeting_indexing_is_the_engine_floor_at_non_binary_spacing() {
    let device = manifold_gpu::testkit::test_device();
    for h in [0.3f32, 0.1, 0.7, 1.0] {
        let dx = f64::from(h);
        let mut points = vec![1.5f32];
        for k in 1..80 {
            let face = (f64::from(k) * 0.5 * dx) as f32;
            let mut x = face;
            for _ in 0..3 {
                x = x.next_down();
            }
            for _ in 0..7 {
                points.push(x);
                x = x.next_up();
            }
        }
        // Interpolation and the depth walk index points below the grid's low
        // faces: the same faces mirrored, down to a few cells under zero.
        let negative: Vec<f32> = points
            .iter()
            .filter(|&&x| x < 4.0 * h)
            .map(|&x| -x)
            .collect();
        points.extend(negative);
        points.extend([-1.5 * h, -0.5 * h, -f32::MIN_POSITIVE, -1e-30]);
        // Past the exact range and non-finite: clamped to +-2^30, NaN to +2^30.
        let limit = (1u64 << 30) as f32 * h;
        for x in [
            4294967296.0f32,
            1073741824.0,
            limit,
            limit.next_down(),
            limit.next_up(),
            1e30,
            f32::MAX,
            f32::INFINITY,
        ] {
            points.extend([x, -x]);
        }
        points.push(f32::NAN);
        let markers: Vec<[f32; 3]> = points.iter().map(|&x| [x, x, x]).collect();
        let records: Vec<FluidParticle> = markers
            .iter()
            .map(|p| FluidParticle {
                position_radius: [p[0], p[1], p[2], 0.1],
                velocity: [0.0; 3],
                id: 1,
            })
            .collect();
        let particles = shared(&device, bytemuck::cast_slice(&records));
        let zeros = shared(&device, &vec![0u8; 4 * 64000]);
        let record = slots_record(&device, records.len() as u32);
        let world = World::new(&device, [0; 3], [8; 3], h);
        let mut stage = GpuSheeting::default();
        stage.prepare(&device);
        stage
            .reserve(&device, 2 * records.len() as u32, world.pool_tiles())
            .unwrap();
        let mut enc = device.create_encoder("sheeting index probe");
        let inputs = SheetInputs {
            particles: &particles,
            order: &zeros,
            ranges: &zeros,
            count: (&record, 0),
            phi: &zeros,
            h,
            threshold: FILL_THRESHOLD,
        };
        stage.encode_index_probe(&device, &mut enc, &inputs, &world.tiles);
        enc.commit_and_wait().expect("GPU work completes");
        let words = copy_out(&device, stage.births().0);
        // In contract: finite with both floors inside +-2^30, the engine's floor.
        // Policy: the clamp for everything else, deliberately not native's.
        let (mut naive_wrong, mut checked, mut policy) = (0, 0, 0);
        for (s, &x) in points.iter().enumerate() {
            let engine = |inv: f64| {
                let f = (f64::from(x) * inv).floor();
                if f.is_nan() {
                    1 << 30
                } else {
                    f.clamp(-1073741824.0, 1073741824.0) as i32
                }
            };
            let (cell, sub) = (engine(1.0 / dx), engine(1.0 / (0.5 * dx)));
            assert_eq!(words[8 * s] as i32, cell, "h {h}: cell of {x}");
            assert_eq!(words[8 * s + 4] as i32, sub, "h {h}: half-cell of {x}");
            if x.is_finite() && cell.abs() < 1 << 30 && sub.abs() < 1 << 30 {
                let naive = (x * (1.0 / dx) as f32).floor() as i32;
                naive_wrong += usize::from(naive != cell);
                checked += 1;
            } else {
                policy += 1;
            }
        }
        eprintln!(
            "SHEETING INDEX h {h}: {checked} in-contract positions match the engine floor (a plain f32 product misses {naive_wrong}); {policy} saturation and NaN inputs match the clamp policy"
        );
        assert!(policy > 0, "the probe covers the clamp policy");
        if h == 0.3 {
            assert!(
                naive_wrong > 0,
                "the probe must be able to see the f32 error"
            );
        }
    }
}

/// A flat sheet on a regular marker lattice (exact distance ties among the
/// nearest three) with a hole, at non-binary spacing; and the splash scaled to
/// the same spacings. The box starts at the lattice origin here: a frame
/// change at these spacings would round the markers differently for the stage
/// and the port.
#[test]
fn gpu_flip_sheeting_matches_the_port_at_non_binary_spacing() {
    let device = manifold_gpu::testkit::test_device();
    let (markers, analytic, cells, dx) = sheeter::fixtures::splash();
    for h in [0.3f32, 0.1, 0.7] {
        let s = h / dx as f32;
        let scaled: Vec<[f32; 3]> = markers.iter().map(|p| p.map(|c| c * s)).collect();
        let phi: Vec<f32> = analytic.iter().map(|v| v * s).collect();
        matches_port(
            &device,
            &format!("splash h {h}"),
            (&scaled, Some(&phi), cells),
            h,
            [0; 3],
        );

        // Cell-centre phi |y − 8.25 h| − 0.75 h, markers four to a cell at
        // y 8.1 h, none within 1 h of the hole's centre: the oracle's holed
        // sheet (sheet_oracle.rs) scaled to h.
        let n = 16usize;
        let lattice_phi: Vec<f32> = (0..n * n * n)
            .map(|i| ((((i / n) % n) as f32 + 0.5) - 8.25).abs() * h - 0.75 * h)
            .collect();
        let mut sheet = Vec::new();
        for zi in 8..24 {
            for xi in 8..24 {
                let (x, z) = (0.25 + 0.5 * xi as f32, 0.25 + 0.5 * zi as f32);
                if (x - 8.0).hypot(z - 8.0) >= 1.0 {
                    sheet.push([x * h, 8.1 * h, z * h]);
                }
            }
        }
        matches_port(
            &device,
            &format!("lattice h {h}"),
            (&sheet, Some(&lattice_phi), [n as u32; 3]),
            h,
            [0; 3],
        );
    }
}

/// The depth walk below the grid's low x face: a level set rising along x,
/// so each marker walks toward −x, past the box, where native reads only
/// out-of-range corners (zero) and the walk ends; here the corners below the
/// box are in tiles that hold no slot. Markers swept across the first three
/// and a half cells, one to a cell: the near ones feel the zero corners and
/// turn thin, the far ones walk inside the grid and do not. The GPU's thin
/// cells after detect equal the port's thin markers' cells; the port is the
/// native sheeter's decisions bit for bit (sheet_oracle.rs), which exposes
/// only seeds, and nothing seeds this close to the border. The box sits across
/// the origin.
#[test]
fn gpu_flip_sheeting_depth_walk_below_the_box_matches_the_port() {
    let device = manifold_gpu::testkit::test_device();
    let cells = [16u32; 3];
    // The non-binary spacing starts at the lattice origin: a frame change
    // there would round its markers differently for the stage and the port.
    for (h, lo) in [(1.0f32, [-8; 3]), (0.3, [0; 3])] {
        let n = 16usize;
        let phi: Vec<f32> = (0..n * n * n)
            .map(|i| 0.3 * ((i % n) as f32 + 0.5 - 5.0) * h)
            .collect();
        let markers: Vec<[f32; 3]> = (0..48)
            .map(|k| {
                [
                    (0.02 + 0.072 * k as f32) * h,
                    (2 + k % 12) as f32 * h + 0.5 * h,
                    (2 + k / 12) as f32 * h + 0.5 * h,
                ]
            })
            .collect();
        let (world_markers, markers) = to_world(&markers, lo, h);
        let trace =
            sheeter::trace_sheet_particles(&markers, &phi, cells, f64::from(h), FILL_THRESHOLD)
                .expect("port");
        let cell = |p: &[f32; 3]| {
            let c = p.map(|v| (f64::from(v) * (1.0 / f64::from(h))).floor() as usize);
            c[0] + n * (c[1] + n * c[2])
        };
        let mut want: Vec<usize> = markers
            .iter()
            .zip(&trace.thin)
            .filter(|(_, t)| **t)
            .map(|(p, _)| cell(p))
            .collect();
        want.sort_unstable();
        let thin = want.len();
        assert!(
            thin > 0 && thin < markers.len(),
            "h {h}: the sweep crosses the walk's decision ({thin} thin)"
        );
        let world = World::new(&device, lo, cells.map(|n| n as i32), h);
        let sorted = sort(&device, &world, &world_markers, h, markers.len());
        let phi_buffer = shared(&device, bytemuck::cast_slice(&world.to_pool(&phi, 3.0 * h)));
        let mut stage = GpuSheeting::default();
        stage.prepare(&device);
        stage.reserve(&device, 16, world.pool_tiles()).unwrap();
        let detect = |record: &GpuBuffer| -> Vec<usize> {
            let mut enc = device.create_encoder("sheeting detect");
            stage.encode_detect(
                &mut enc,
                &SheetInputs {
                    particles: &sorted.particles,
                    order: &sorted.order,
                    ranges: &sorted.ranges,
                    count: (record, 0),
                    phi: &phi_buffer,
                    h,
                    threshold: FILL_THRESHOLD,
                },
                &world.tiles,
            );
            enc.commit_and_wait().expect("GPU work completes");
            let flags = world.dense_of(&copy_out(&device, &stage.scratch()[0]));
            (0..flags.len()).filter(|&c| flags[c] != 0).collect()
        };
        assert_eq!(detect(&sorted.record), want, "h {h}: thin cells differ");

        // The marker pass covers the record's first n slots: with the last thin
        // marker's slot and everything after it left out, its cell raises no flag.
        let order = copy_out(&device, &sorted.order);
        let thin_at = |slot: usize| trace.thin[order[slot] as usize];
        let n = (0..markers.len())
            .rev()
            .find(|&slot| thin_at(slot))
            .unwrap();
        let mut want_n: Vec<usize> = (0..n)
            .filter(|&slot| thin_at(slot))
            .map(|slot| cell(&markers[order[slot] as usize]))
            .collect();
        want_n.sort_unstable();
        assert!(
            want_n.len() < thin,
            "h {h}: the count leaves a thin marker out"
        );
        let short = slots_record(&device, n as u32);
        assert_eq!(
            detect(&short),
            want_n,
            "h {h}: a marker past the count raised a flag"
        );
        eprintln!(
            "SHEETING WALK h {h}: {thin} of {} markers thin, cells agree",
            markers.len()
        );
    }
}

#[test]
fn gpu_flip_sheeting_bucket_rows_match_cpu_merge() {
    let device = manifold_gpu::testkit::test_device();
    let mut stage = GpuSheeting::default();
    stage.prepare(&device);
    // One tile, a column of two across the origin, and a 2 x 2 x 2 block.
    for (lo, size) in [
        ([0, 0, 0], [8, 8, 8]),
        ([-8, 0, -8], [8, 8, 16]),
        ([-8, -8, -8], [16, 16, 16]),
    ] {
        let world = World::new(&device, lo, size, 1.0);
        let pool_tiles = world.pool_tiles();
        stage.reserve(&device, 1, pool_tiles).unwrap();
        for saturated in [true, false] {
            let fixture = bucket_fixture(
                &world.cells(),
                &|c| world.index(c).unwrap(),
                pool_tiles as usize,
                saturated,
            );
            let counts = shared(&device, bytemuck::cast_slice(&fixture.counts));
            let segments = shared(&device, bytemuck::cast_slice(&fixture.segments));
            let dummy = shared(&device, &[0; 32]);
            let inputs = SheetInputs {
                particles: &dummy,
                order: &dummy,
                ranges: &dummy,
                count: (&dummy, 0),
                phi: &dummy,
                h: 1.0,
                threshold: FILL_THRESHOLD,
            };
            let mut enc = device.create_encoder("sheeting bucket proof");
            enc.copy_buffer_to_buffer(&counts, &stage.scratch()[3], counts.size);
            enc.copy_buffer_to_buffer(&segments, &stage.scratch()[4], segments.size);
            stage.encode_buckets(&mut enc, &inputs, &world.tiles);
            enc.commit_and_wait().expect("GPU work completes");
            let lengths = copy_out(&device, &stage.scratch()[1]);
            let rows = copy_out(&device, &stage.scratch()[4]);
            for (r, want) in fixture.merged.iter().enumerate() {
                assert_eq!(lengths[r] as usize, want.len(), "{lo:?}, row {r}");
                let want: &[u32] = bytemuck::cast_slice(want);
                assert_eq!(
                    &rows[128 * r..128 * r + want.len()],
                    want,
                    "{lo:?}, row {r}"
                );
            }
        }
    }
}

/// Exact words at the stage boundary, including saved-velocity writes and
/// identity reservation. Repeated encodes exercise real GPU command replay;
/// rates/tick/substep change while bindings stay stable.
#[test]
fn gpu_flip_sheeting_rows_match_old_gpu_under_replay_and_fractional_rates() {
    rows_match_old_gpu_under_replay_and_fractional_rates(false);
}

/// The same with the scene one cell off the buckets' alignment, so the real
/// marker selection and segment bases take the other parity of every cell.
#[test]
fn gpu_flip_sheeting_odd_cell_rows_match_old_gpu_under_replay_and_fractional_rates() {
    rows_match_old_gpu_under_replay_and_fractional_rates(true);
}

fn rows_match_old_gpu_under_replay_and_fractional_rates(odd_cell: bool) {
    use super::gpu_flip_sheeting::StepBirths;
    use manifold_gpu::GpuReplayCache;
    use manifold_water_liquid::fluid_particles::FaceSample;
    use manifold_water_liquid::primitives::particle_identity::{
        BirthReservation, COUNT_OFFSET, ParticleIdentity,
    };

    let device = manifold_gpu::testkit::test_device();
    let (markers, analytic, cells, dx) = sheeter::fixtures::splash();
    let h = dx as f32;
    let margin = if odd_cell { 9 } else { 8 };
    let lo = [-24; 3];
    let big = padded_cells(cells, margin);
    let analytic = padded_phi(&analytic, cells, big, margin, h);
    let mut markers = padded_markers(&quantised(&markers), margin, h);
    // Break spatial input order without changing the geometry.
    let mut rng = 0x0005_1eed_u32;
    for i in (1..markers.len()).rev() {
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        markers.swap(i, rng as usize % (i + 1));
    }
    let (markers, _) = to_world(&markers, lo, h);
    let world = World::new(&device, lo, big.map(|n| n as i32), h);
    let capacity = markers.len() as u32 + 8000;
    let sorted = sort(&device, &world, &markers, h, capacity as usize);
    let original = copy_out(&device, &sorted.particles);
    let mut initial = vec![0u32; 8 * capacity as usize];
    initial[..original.len()].copy_from_slice(&original);
    let ranges = &sorted.ranges;
    let phi = shared(
        &device,
        bytemuck::cast_slice(&world.to_pool(&analytic, 3.0 * h)),
    );
    let face = FaceSample {
        velocity: [0.3, -0.2, 0.1, 0.0],
        weight: [1.0; 4],
    };
    let faces = vec![face; world.cells().len()];
    let old = shared(&device, bytemuck::cast_slice(&world.to_pool(&faces, face)));
    let capped = shared(
        &device,
        &[0u8; 4 * manifold_water_liquid::primitives::liquid_stats::SOLVER_WORDS as usize],
    );
    let plan = live_plan(&device, 0.01);
    let mut ids = ParticleIdentity::default();
    ids.prepare(&device);
    // The identity before each tick: the next id, epoch 1, and the record of
    // the slots in use, the sorted markers.
    let identity_words = [
        5000u32,
        1,
        0,
        0,
        sorted.count.div_ceil(256).max(1),
        1,
        1,
        sorted.count,
    ];
    // Reference/direct, rows/direct, rows/replayed, reference/replayed.
    let mut lanes: Vec<_> = (0..4)
        .map(|lane| {
            let mut stage = GpuSheeting::default();
            if lane == 0 || lane == 3 {
                stage.prepare_reference(&device);
            } else {
                stage.prepare(&device);
            }
            stage
                .reserve(&device, capacity, world.pool_tiles())
                .unwrap();
            (
                stage,
                shared(&device, bytemuck::cast_slice(&initial)),
                shared(&device, bytemuck::cast_slice(&identity_words)),
                (lane >= 2).then(GpuReplayCache::default),
            )
        })
        .collect();
    let mut full_count = 0;
    for (tick, &(rate, active)) in [
        (1.0, true),
        (0.25, true),
        (0.75, true),
        (0.5, false),
        (0.5, true),
        (1.0, true),
        (0.25, true),
        (0.75, true),
    ]
    .iter()
    .enumerate()
    {
        let mut clock = [0u32; 12];
        clock[0] = if active { 0.01f32.to_bits() } else { 0 };
        clock[11] = 1;
        // SAFETY: shared plan, all previous command buffers completed.
        unsafe {
            plan.write(0, bytemuck::cast_slice(&clock));
        }
        let mut outputs = Vec::new();
        for (stage, particles, identity, cache) in &mut lanes {
            // Same sorted input on every encode isolates this stage from later
            // solver passes; the stage's own scratch/stats retain their history.
            // No removal pass runs here, so the births owed (stats word 4)
            // are cleared too and the water budget never throttles the draw.
            // SAFETY: sized shared buffers, no GPU work in flight.
            unsafe {
                particles.write(0, bytemuck::cast_slice(&initial));
                identity.write(0, bytemuck::cast_slice(&identity_words));
                stage.stats().write(16, bytemuck::cast_slice(&[0u32]));
            }
            let inputs = SheetInputs {
                particles,
                order: &sorted.order,
                ranges,
                count: (&*identity, COUNT_OFFSET),
                phi: &phi,
                h,
                threshold: FILL_THRESHOLD,
            };
            let births = StepBirths {
                old: &old,
                identity,
                capped: &capped,
                capped_offset: 0,
                rate,
                tick: tick as u32,
                substep: (tick % 3) as u32,
            };
            let mut enc = device.create_encoder("sheeting old GPU parity");
            let replay = cache.take().map(|c| enc.begin_replay(&device, c)).is_some();
            stage.encode_draw(&mut enc, &inputs, &plan, &births, &world.tiles);
            ids.reserve(
                &mut enc,
                BirthReservation {
                    particles,
                    identity,
                    ranges,
                    scan: stage.winners(),
                    plan: &plan,
                    tile_counts: world.tiles.counts(),
                    step_record: None,
                    params: [
                        capacity,
                        pool_len(world.pool_tiles()) as u32,
                        stage.ranks(),
                        exhausted_word(world.tiles.reach().ring_max) as u32,
                    ],
                },
            );
            enc.compute_memory_barrier_buffers();
            stage.encode_write(&mut enc, &inputs, &plan, &births, &world.tiles);
            if replay {
                *cache = Some(enc.end_replay());
            }
            enc.commit_and_wait().expect("GPU work completes");
            let mut count = copy_out(&device, stage.births().1);
            count.truncate(2); // Only these words are defined by the stage.
            assert_eq!(count[1], 0, "recomputed claimant failed");
            if active && tick == 0 {
                full_count = count[0];
                assert!(full_count > 100, "fixture must seed sheets");
            } else if active && rate < 1.0 {
                assert!(
                    count[0] > 0 && count[0] < full_count,
                    "fractional draw must retain and reject births"
                );
            }
            let births = copy_out(&device, stage.births().0);
            outputs.push(vec![
                copy_out(&device, particles),
                copy_out(&device, identity),
                copy_out(&device, stage.stats()),
                copy_out(&device, stage.winners()),
                copy_out(&device, &stage.scratch()[3]),
                copy_out(&device, &stage.scratch()[5]),
                copy_out(&device, &stage.scratch()[6]),
                births[..4 * count[0].min(capacity) as usize].to_vec(),
                count,
            ]);
        }
        for lane in 1..outputs.len() {
            assert!(
                outputs[lane] == outputs[0],
                "tick {tick}, rate {rate}, active {active}, lane {lane}: output words differ"
            );
        }
    }
    for (_, _, _, cache) in &lanes[2..] {
        let stats = cache.as_ref().unwrap().stats();
        assert!(stats.replayed > 0, "nothing replayed: {stats:?}");
    }
}
