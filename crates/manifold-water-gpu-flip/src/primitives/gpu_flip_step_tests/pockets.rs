//! Sealed pocket proofs against CPU labels, rounds and sums, on world tiles
//! across the origin. The tank box sits on negative and positive cells in
//! whole tiles held in a shuffled pool, so a label (the lowest pool index of
//! its pocket) does not follow cell order, and pockets cross tile faces.

use super::super::gpu_flip_step::{POCKET_GATE_WORDS, StepParams, dispatch_pass};
use super::Stream;
use manifold_gpu::BufferUse;
use manifold_gpu::GpuBuffer;
use manifold_node_engine::scene::world_tiles::{
    EMPTY, NONE, find, insert, local_index, neighbour_row_word, pool_len, slot_row_word,
    table_entries, table_words, tile_key, tile_of_cell,
};
use manifold_node_engine::testkit::array_harness::read;
use manifold_water_liquid::fluid_particles::FaceSample;

/// The tank box: cells [LO, LO + N), unequal sides so a swapped axis shows,
/// straddling the origin in every axis. Its tiles are -1 and 0 on each axis.
const N: [usize; 3] = [6, 5, 4];
const LO: [i32; 3] = [-3, -2, -2];
const H: f32 = 0.25;

const POCKET_DRY: u32 = 0;
const POCKET_SEALED: u32 = 1;
const POCKET_AIR: u32 = 2;

fn padded() -> [usize; 3] {
    N.map(|n| n + 1)
}

fn face_len() -> usize {
    padded().iter().product()
}

fn cell_len() -> usize {
    N.iter().product()
}

fn pad_index(p: [usize; 3]) -> usize {
    let m = padded();
    p[0] + m[0] * (p[1] + m[1] * p[2])
}

fn pad_coords(i: usize) -> [usize; 3] {
    let m = padded();
    [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])]
}

fn cell_index(p: [usize; 3]) -> usize {
    p[0] + N[0] * (p[1] + N[1] * p[2])
}

fn cell_coords(c: usize) -> [usize; 3] {
    [c % N[0], (c / N[0]) % N[1], c / (N[0] * N[1])]
}

/// Face a of padded cell p exists when its other two indices are inside.
fn face_exists(p: [usize; 3], a: usize) -> bool {
    (0..3).all(|b| b == a || p[b] < N[b])
}

/// The world cell of box cell p.
fn world(p: [usize; 3]) -> [i32; 3] {
    std::array::from_fn(|a| LO[a] + p[a] as i32)
}

/// The box coordinates of world cell c when they are below `extent`.
fn within(c: [i32; 3], extent: [usize; 3]) -> Option<[usize; 3]> {
    let mut out = [0; 3];
    for a in 0..3 {
        let r = c[a] - LO[a];
        if r < 0 || r as usize >= extent[a] {
            return None;
        }
        out[a] = r as usize;
    }
    Some(out)
}

/// The step's params over the box: the cell size and origin place the world
/// lattice.
fn lattice(tiles: &Tiles) -> StepParams {
    StepParams {
        cell_size: H,
        inv_cell: 1.0 / H,
        origin: [-0.5 * H; 3],
        ring_max: 1,
        impulse_tick: -1,
        pool_tiles: tiles.tiles.len() as u32,
        table_entries: table_entries(tiles.tiles.len() as u32),
        ..StepParams::default()
    }
}

/// A fixed shuffle of `tiles` from a xorshift stream.
fn shuffled<T>(mut items: Vec<T>, seed: u64) -> Vec<T> {
    let mut rng = Stream::new(seed);
    for i in (1..items.len()).rev() {
        let j = ((rng.unit() * (i + 1) as f32) as usize).min(i);
        items.swap(i, j);
    }
    items
}

/// The tiles holding cells [LO, LO + N] on each axis (the records past the
/// box live in them too).
fn box_tiles() -> Vec<[i32; 3]> {
    let lo = tile_of_cell(LO);
    let hi = tile_of_cell(std::array::from_fn(|a| LO[a] + N[a] as i32));
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

/// The held tiles of a world-tiled lattice and the table words that find
/// them: slot i holds `tiles[i]`.
struct Tiles {
    tiles: Vec<[i32; 3]>,
    words: Vec<u32>,
}

impl Tiles {
    /// The table of `tiles`: keys by ordered insert, the slot of each entry,
    /// each slot's tile and entry, and its 27 neighbour slots.
    fn new(tiles: Vec<[i32; 3]>) -> Self {
        let p = tiles.len() as u32;
        let entries = table_entries(p);
        let h = entries as usize;
        let mut words = vec![NONE; table_words(p) as usize];
        words[..h].fill(EMPTY);
        for &t in &tiles {
            assert!(
                insert(&mut words[..h], tile_key(t)),
                "the table holds every tile"
            );
        }
        for (slot, t) in tiles.iter().enumerate() {
            let entry =
                find(&words[..h], tile_key(*t)).expect("an inserted tile is found") as usize;
            words[h + entry] = slot as u32;
            let row = slot_row_word(entries, slot as u32) as usize;
            for a in 0..3 {
                words[row + a] = t[a] as u32;
            }
            words[row + 3] = entry as u32;
            let near = neighbour_row_word(entries, p, slot as u32) as usize;
            for k in 0..27i32 {
                let d = [k % 3 - 1, k / 3 % 3 - 1, k / 9 - 1];
                let other: [i32; 3] = std::array::from_fn(|a| t[a] + d[a]);
                words[near + k as usize] = tiles
                    .iter()
                    .position(|o| *o == other)
                    .map_or(NONE, |s| s as u32);
            }
        }
        Self { tiles, words }
    }

    fn pool_tiles(&self) -> u32 {
        self.tiles.len() as u32
    }

    /// Elements of every pooled field: 512 a slot and the canonical one.
    fn pool_len(&self) -> usize {
        pool_len(self.pool_tiles()) as usize
    }

    /// The pool index of world cell c, None when its tile is not held.
    fn pool_index(&self, c: [i32; 3]) -> Option<usize> {
        let slot = self.tiles.iter().position(|t| *t == tile_of_cell(c))?;
        Some(slot * 512 + local_index(c) as usize)
    }

    /// A pooled field with `value(c)` at every cell of every held tile.
    fn pooled<T: bytemuck::Pod>(&self, value: impl Fn([i32; 3]) -> T) -> Vec<T> {
        let mut pool = vec![T::zeroed(); self.pool_len()];
        for (slot, t) in self.tiles.iter().enumerate() {
            for l in 0..512i32 {
                let c = [
                    t[0] * 8 + (l & 7),
                    t[1] * 8 + ((l >> 3) & 7),
                    t[2] * 8 + (l >> 6),
                ];
                pool[slot * 512 + l as usize] = value(c);
            }
        }
        pool
    }

    /// A pooled cell field from the dense box (zero outside it).
    fn pooled_cells<T: bytemuck::Pod>(&self, dense: &[T]) -> Vec<T> {
        self.pooled(|c| within(c, N).map_or_else(T::zeroed, |p| dense[cell_index(p)]))
    }

    /// A pooled record field from the dense padded box (zero outside it).
    fn pooled_records<T: bytemuck::Pod>(&self, dense: &[T]) -> Vec<T> {
        self.pooled(|c| within(c, padded()).map_or_else(T::zeroed, |p| dense[pad_index(p)]))
    }

    /// The dense box of a pooled cell field.
    fn cells<T: bytemuck::Pod>(&self, pool: &[T]) -> Vec<T> {
        (0..cell_len())
            .map(|i| pool[self.pool_index(world(cell_coords(i))).expect("held")])
            .collect()
    }

    /// The dense padded box of a pooled record field.
    fn records<T: bytemuck::Pod>(&self, pool: &[T]) -> Vec<T> {
        (0..face_len())
            .map(|i| pool[self.pool_index(world(pad_coords(i))).expect("held")])
            .collect()
    }

    /// The list of every slot, in an order of its own: list order is free.
    fn list(&self, seed: u64) -> Vec<u32> {
        shuffled((0..self.pool_tiles()).collect(), seed)
    }

    /// The step's indirect triples over `ring_max` 1: every list holds every slot.
    fn args(&self) -> Vec<u32> {
        [2 * self.pool_tiles(), 1, 1].repeat(3)
    }
}

/// Buffers by binding, and the step's shader entries run over them.
struct Rig {
    device: manifold_gpu::testkit::TestDevice,
    bound: Vec<(u32, GpuBuffer)>,
}

impl Rig {
    fn new() -> Self {
        Self {
            device: manifold_gpu::testkit::test_device(),
            bound: Vec::new(),
        }
    }

    /// A rig with the tiles' table at 51, the slot list at 29 and the
    /// triples at 31.
    fn over(tiles: &Tiles) -> Self {
        let mut rig = Self::new();
        rig.bind(51, &tiles.words)
            .bind(29, &tiles.list(0x11a7))
            .bind(31, &tiles.args());
        rig
    }

    /// Binds a shared buffer holding `values`, replacing the one at `binding`.
    fn bind<T: bytemuck::Pod>(&mut self, binding: u32, values: &[T]) -> &mut Self {
        let bytes: &[u8] = bytemuck::cast_slice(values);
        let buffer = self
            .device
            .create_buffer((bytes.len() as u64).max(16), BufferUse::Upload);
        buffer.zero_fill();
        // SAFETY: shared buffer sized for `bytes`; no GPU work in flight.
        unsafe { buffer.write(0, bytes) };
        self.bound.retain(|(b, _)| *b != binding);
        self.bound.push((binding, buffer));
        self
    }

    fn run(&self, entry: &str, params: &StepParams, threads: usize) {
        let buffers: Vec<(u32, &GpuBuffer)> = self.bound.iter().map(|(b, buf)| (*b, buf)).collect();
        dispatch_pass(&self.device, entry, params, &buffers, threads as u64);
    }

    fn read<T: bytemuck::Pod>(&self, binding: u32, len: usize) -> Vec<T> {
        let buffer = &self
            .bound
            .iter()
            .find(|(b, _)| *b == binding)
            .expect("bound")
            .1;
        read(buffer, len)
    }

    fn gate(&self) -> Vec<u32> {
        self.read(24, POCKET_GATE_WORDS as usize)
    }

    /// The seed and start, then rounds of sweeps until one changes nothing
    /// or `limit` rounds ran; returns the rounds that changed a cell.
    fn spread(&self, tiles: &Tiles, params: &StepParams, limit: usize) -> usize {
        let cells = 512 * tiles.tiles.len();
        let lines = 64 * tiles.tiles.len();
        self.run("pocket_seed", params, cells);
        self.run("pocket_start", params, 1);
        let mut rounds = 0;
        loop {
            self.run("pocket_round", params, 1);
            for sweep in ["pocket_sweep_x", "pocket_sweep_y", "pocket_sweep_z"] {
                self.run(sweep, params, lines);
            }
            if self.gate()[9] == 0 {
                return rounds;
            }
            rounds += 1;
            if rounds >= limit {
                return rounds;
            }
        }
    }
}

/// The open fractions pass's output over the padded box: inner faces open
/// (`solid` false), or one in four closed, one in four whole and the rest a
/// fraction; the faces on the box's border closed; each cell's open volume
/// in weight w.
fn open_faces(seed: u64, solid: bool) -> Vec<FaceSample> {
    let mut rng = Stream::new(seed);
    (0..face_len())
        .map(|i| {
            let p = pad_coords(i);
            let mut face = FaceSample::default();
            for a in 0..3 {
                if face_exists(p, a) && p[a] > 0 && p[a] < N[a] {
                    let r = rng.unit();
                    face.weight[a] = if !solid || (0.25..0.5).contains(&r) {
                        1.0
                    } else if r < 0.25 {
                        0.0
                    } else {
                        0.05 + 0.95 * rng.unit()
                    };
                }
            }
            if (0..3).all(|a| p[a] < N[a]) {
                face.weight[3] = if solid { rng.unit() } else { 1.0 };
            }
            face
        })
        .collect()
}

/// Opens the box's border faces on the sides `closed` leaves clear (bits −X
/// +X −Y +Y −Z +Z): the cells beyond the box hold no water, so water there
/// touches air.
fn open_tank_faces(open: &mut [FaceSample], closed: u32) {
    for (i, face) in open.iter_mut().enumerate() {
        let p = pad_coords(i);
        for a in 0..3 {
            if face_exists(p, a)
                && ((p[a] == 0 && closed >> (2 * a) & 1 == 0)
                    || (p[a] == N[a] && closed >> (2 * a + 1) & 1 == 0))
            {
                face.weight[a] = 1.0;
            }
        }
    }
}

/// The engine's sealed pockets on CPU (PressureSolver::_conditionSolidVelocityField):
/// water cells link through a face open at least 1e-6; a region reaches air
/// through a linked dry neighbour, in the box or beyond its edge; a cell of
/// an unreached region of more than one cell is isolated. Returns the
/// isolated cells, then every sealed (wet, unreached) cell.
fn cpu_isolated(water: &[f32], open: &[FaceSample]) -> (Vec<bool>, Vec<bool>) {
    let wet = |c: [usize; 3]| water[cell_index(c)] > 0.5;
    let neighbours = |c: [usize; 3]| {
        let mut out = Vec::new();
        for a in 0..3 {
            for d in [c[a].checked_sub(1), Some(c[a] + 1).filter(|&q| q < N[a])]
                .into_iter()
                .flatten()
            {
                let mut q = c;
                q[a] = d;
                let mut f = c;
                f[a] = c[a].max(d);
                if open[pad_index(f)].weight[a] >= 1e-6 {
                    out.push(q);
                }
            }
        }
        out
    };
    let mut reach: Vec<bool> = (0..cell_len())
        .map(|i| {
            let c = cell_coords(i);
            let beyond = (0..3).any(|a| {
                let mut high = c;
                high[a] += 1;
                (c[a] == 0 && open[pad_index(c)].weight[a] >= 1e-6)
                    || (c[a] == N[a] - 1 && open[pad_index(high)].weight[a] >= 1e-6)
            });
            wet(c) && (beyond || neighbours(c).iter().any(|&q| !wet(q)))
        })
        .collect();
    let mut queue: Vec<usize> = (0..cell_len()).filter(|&i| reach[i]).collect();
    while let Some(i) = queue.pop() {
        for q in neighbours(cell_coords(i)) {
            let j = cell_index(q);
            if wet(q) && !reach[j] {
                reach[j] = true;
                queue.push(j);
            }
        }
    }
    let isolated = (0..cell_len())
        .map(|i| {
            let c = cell_coords(i);
            wet(c) && !reach[i] && neighbours(c).iter().any(|&q| wet(q))
        })
        .collect();
    let sealed = (0..cell_len())
        .map(|i| wet(cell_coords(i)) && !reach[i])
        .collect();
    (isolated, sealed)
}

/// The sealed water's pockets on CPU: each cell of `sealed` labelled by the
/// lowest pool index among the cells it links to through sealed cells; other
/// cells None.
fn cpu_pocket_labels(tiles: &Tiles, sealed: &[bool], open: &[FaceSample]) -> Vec<Option<usize>> {
    let pool = |i: usize| tiles.pool_index(world(cell_coords(i))).expect("held");
    let mut label = vec![None; cell_len()];
    for start in 0..cell_len() {
        if !sealed[start] || label[start].is_some() {
            continue;
        }
        let mut members = vec![start];
        label[start] = Some(0);
        let mut queue = vec![start];
        while let Some(i) = queue.pop() {
            let c = cell_coords(i);
            for a in 0..3 {
                for d in [c[a].checked_sub(1), Some(c[a] + 1).filter(|&q| q < N[a])]
                    .into_iter()
                    .flatten()
                {
                    let mut q = c;
                    q[a] = d;
                    let mut f = c;
                    f[a] = c[a].max(d);
                    let j = cell_index(q);
                    if sealed[j] && label[j].is_none() && open[pad_index(f)].weight[a] >= 1e-6 {
                        label[j] = Some(0);
                        members.push(j);
                        queue.push(j);
                    }
                }
            }
        }
        let lowest = members.iter().map(|&m| pool(m)).min().expect("a member");
        for m in members {
            label[m] = Some(lowest);
        }
    }
    label
}

/// The fine pockets' pooled states and labels after a settled spread over
/// `water` and `open`, every tank face closed.
fn fine_pockets(tiles: &Tiles, water: &[f32], open: &[FaceSample]) -> (Vec<u32>, Vec<u32>) {
    let params = lattice(tiles);
    let mut rig = Rig::over(tiles);
    rig.bind(6, &tiles.pooled_cells(water))
        .bind(10, &tiles.pooled_records(open))
        .bind(23, &vec![0u32; tiles.pool_len()])
        .bind(24, &[0u32; POCKET_GATE_WORDS as usize])
        .bind(25, &vec![0u32; tiles.pool_len()]);
    rig.spread(tiles, &params, cell_len() + 1);
    rig.run("pocket_check", &params, 512 * tiles.tiles.len());
    assert_eq!(
        rig.gate()[10],
        0,
        "a settled spread leaves no sealed cell linked to air"
    );
    (
        rig.read(23, tiles.pool_len()),
        rig.read(25, tiles.pool_len()),
    )
}

/// Everything one run of the pocket passes leaves.
struct Outcome {
    /// Dense box: each cell's state and label (a pool index).
    state: Vec<u32>,
    label: Vec<u32>,
    /// Dense padded box: the conditioned solid velocity.
    velocity: Vec<FaceSample>,
    /// Rounds that changed a cell.
    rounds: usize,
    /// Dense box: `rhs` with each sealed pocket's mean removed.
    rhs: Vec<f32>,
    /// The solver words after the pressure flux.
    words: Vec<u32>,
    /// The removed total's two words after the flux took it.
    total_left: Vec<u32>,
}

/// Runs the pocket passes over `water`, `open` and the solid velocity
/// `moving`, rounds until one changes nothing.
fn gpu_pockets(
    tiles: &Tiles,
    water: &[f32],
    open: &[FaceSample],
    moving: &[FaceSample],
    rhs: &[f32],
) -> Outcome {
    let params = lattice(tiles);
    let cells = 512 * tiles.tiles.len();
    let mut rig = Rig::over(tiles);
    rig.bind(6, &tiles.pooled_cells(water))
        .bind(10, &tiles.pooled_records(open))
        .bind(23, &vec![0u32; tiles.pool_len()])
        .bind(24, &[0u32; POCKET_GATE_WORDS as usize])
        .bind(4, &tiles.pooled_records(moving))
        .bind(25, &vec![0u32; tiles.pool_len()])
        .bind(5, &tiles.pooled_cells(rhs))
        .bind(26, &vec![0u32; 2 + 3 * tiles.pool_len()])
        .bind(22, &[9u32; 6]);
    let rounds = rig.spread(tiles, &params, cell_len() + 1);
    rig.run("pocket_check", &params, cells);
    assert_eq!(
        rig.gate()[10],
        0,
        "a settled spread leaves no sealed cell linked to air"
    );
    rig.run("pocket_condition", &params, cells);
    rig.run("pocket_clear", &params, cells);
    rig.run("pocket_accumulate", &params, cells);
    rig.run("pocket_remove", &params, cells);
    rig.run(
        "pocket_flux_pressure",
        &StepParams {
            step_in_tick: 0,
            ..params
        },
        1,
    );
    let pool = tiles.pool_len();
    Outcome {
        state: tiles.cells(&rig.read::<u32>(23, pool)),
        label: tiles.cells(&rig.read::<u32>(25, pool)),
        velocity: tiles.records(&rig.read::<FaceSample>(4, pool)),
        rounds,
        rhs: tiles.cells(&rig.read::<f32>(5, pool)),
        words: rig.read(22, 6),
        total_left: rig.read::<u32>(26, 2),
    }
}

#[test]
pub(super) fn gpu_flip_sealed_pockets_zero_the_solid_velocity_as_the_engine() {
    let mut zeroed = 0;
    // A random solid seals nothing off, so half its faces are closed again
    // and nearly every cell is water: walls of closed faces split the water
    // into pockets, some reaching a dry cell or an open face of the box's
    // border and some not. `mask` is the border faces that stay closed.
    for (seed, mask) in [
        (0x5e1, 63),
        (0x5e2, 63),
        (0x5e3, 63 & !(1 << 3)),
        (0x5e4, 63 & !1),
    ] {
        // Each case holds its tiles in another slot order.
        let tiles = Tiles::new(shuffled(box_tiles(), seed));
        let mut rng = Stream::new(seed);
        let water: Vec<f32> = (0..cell_len())
            .map(|_| f32::from(u8::from(rng.unit() < 0.97)))
            .collect();
        let mut open = open_faces(seed + 1, true);
        for face in &mut open {
            for a in 0..3 {
                if rng.unit() < 0.5 {
                    face.weight[a] = 0.0;
                }
            }
        }
        open_tank_faces(&mut open, mask);
        let moving: Vec<FaceSample> = (0..face_len())
            .map(|i| {
                let p = pad_coords(i);
                let mut face = FaceSample::default();
                for a in 0..3 {
                    if face_exists(p, a) {
                        face.velocity[a] = 0.5 + rng.unit();
                    }
                }
                face.velocity[3] = 7.0;
                face
            })
            .collect();
        let (isolated, sealed) = cpu_isolated(&water, &open);
        let rhs: Vec<f32> = (0..cell_len()).map(|_| 4.0 * rng.unit() - 1.0).collect();
        let got = gpu_pockets(&tiles, &water, &open, &moving, &rhs);
        let labels = cpu_pocket_labels(&tiles, &sealed, &open);
        for i in 0..cell_len() {
            let want = if water[i] > 0.5 {
                if sealed[i] { POCKET_SEALED } else { POCKET_AIR }
            } else {
                POCKET_DRY
            };
            assert_eq!(
                got.state[i], want,
                "seed {seed:#x} mask {mask}: cell {i} state"
            );
            if let Some(label) = labels[i] {
                assert_eq!(
                    got.label[i] as usize, label,
                    "seed {seed:#x} mask {mask}: cell {i} label is its pocket's lowest pool index"
                );
            }
        }
        // Each sealed pocket loses its mean, so it sums to 0; every other
        // cell keeps its value. The GPU rounds each value to 1/65536 first.
        let mut sums = std::collections::BTreeMap::<usize, (f64, usize)>::new();
        for (i, label) in labels.iter().enumerate() {
            if let Some(l) = label {
                let entry = sums.entry(*l).or_default();
                entry.0 += f64::from(rhs[i]);
                entry.1 += 1;
            }
        }
        let mut total = 0.0;
        let leaders: std::collections::BTreeSet<usize> = sums.keys().copied().collect();
        for (i, label) in labels.iter().enumerate() {
            let pool = tiles.pool_index(world(cell_coords(i))).expect("held");
            let want = match label {
                Some(l) => {
                    let (sum, count) = sums[l];
                    if pool == *l {
                        total += sum.abs();
                    }
                    f64::from(rhs[i]) - sum / count as f64
                }
                None => f64::from(rhs[i]),
            };
            let tolerance = if label.is_some() { 1e-4 } else { 0.0 };
            assert!(
                (f64::from(got.rhs[i]) - want).abs() <= tolerance,
                "seed {seed:#x} mask {mask}: cell {i} rhs {} want {want}",
                got.rhs[i]
            );
        }
        for l in &leaders {
            let residual: f64 = (0..cell_len())
                .filter(|&i| labels[i] == Some(*l))
                .map(|i| f64::from(got.rhs[i]))
                .sum();
            assert!(
                residual.abs() < 1e-3,
                "seed {seed:#x} mask {mask}: pocket {l} sums to {residual} after its mean is removed"
            );
        }
        let h = f64::from(H);
        let flux = f64::from(f32::from_bits(got.words[4]));
        assert!(
            (flux - total * h * h * h).abs() <= 1e-4 * (total * h * h * h).max(1e-9),
            "seed {seed:#x} mask {mask}: removed flux {flux}, want {}",
            total * h * h * h
        );
        assert_eq!(got.total_left, [0, 0], "the flux takes the removed total");
        assert_eq!(
            got.words[5], 0,
            "the tick's first step clears the density word"
        );
        assert_eq!(
            &got.words[..4],
            &[9, 9, 9, 9],
            "the other solver words stay"
        );
        for (i, moved) in moving.iter().enumerate().take(face_len()) {
            let p = pad_coords(i);
            for a in 0..3 {
                if !face_exists(p, a) {
                    continue;
                }
                let mut below = p;
                let low = p[a] > 0 && {
                    below[a] = p[a] - 1;
                    isolated[cell_index(below)]
                };
                let high = p[a] < N[a] && isolated[cell_index(p)];
                let want = if low || high { 0.0 } else { moved.velocity[a] };
                zeroed += usize::from(low || high);
                assert_eq!(
                    got.velocity[i].velocity[a], want,
                    "seed {seed:#x} mask {mask}: record {p:?} axis {a}"
                );
            }
            assert_eq!(got.velocity[i].velocity[3], 7.0, "the owner code stays");
            assert_eq!(got.velocity[i].weight, moved.weight, "the friction stays");
        }
        println!(
            "GPU FLIP pockets seed {seed:#x} mask {mask}: {} isolated cells, {} pockets, {} rounds",
            isolated.iter().filter(|&&s| s).count(),
            sums.len(),
            got.rounds
        );
    }
    assert!(zeroed > 0, "some pocket was sealed");
}

/// A sealed channel along x through the tile face at x = -1 | 0: one round
/// of sweeps labels it whole, whichever side of the face holds the pocket's
/// lowest pool index, and the next round changes nothing.
#[test]
pub(super) fn gpu_flip_pocket_across_a_tile_face_settles_in_one_round() {
    // The channel at y = z = 0 runs the box's whole x extent: tiles (-1,0,0)
    // and (0,0,0). Slot 0 is the +x tile in one order, the -x tile in the other.
    for first in [[0, 0, 0], [-1, 0, 0]] {
        let mut order = box_tiles();
        order.sort_by_key(|t| (*t != first, t[2], t[1], t[0]));
        assert_eq!(order[0], first);
        let tiles = Tiles::new(order);
        let channel = |c: [usize; 3]| world(c)[1] == 0 && world(c)[2] == 0;
        let water: Vec<f32> = (0..cell_len())
            .map(|i| f32::from(u8::from(channel(cell_coords(i)))))
            .collect();
        // Only the faces between channel cells are open: no cell links out.
        let open: Vec<FaceSample> = (0..face_len())
            .map(|i| {
                let p = pad_coords(i);
                let mut face = FaceSample::default();
                if channel(p) && p[0] > 0 && p[0] < N[0] {
                    face.weight[0] = 1.0;
                }
                face
            })
            .collect();
        let params = lattice(&tiles);
        let mut rig = Rig::over(&tiles);
        rig.bind(6, &tiles.pooled_cells(&water))
            .bind(10, &tiles.pooled_records(&open))
            .bind(23, &vec![0u32; tiles.pool_len()])
            .bind(24, &[0u32; POCKET_GATE_WORDS as usize])
            .bind(25, &vec![0u32; tiles.pool_len()]);
        let rounds = rig.spread(&tiles, &params, 1);
        assert_eq!(
            rounds, 1,
            "the first round changes a cell and the limit stops there"
        );
        let want = (0..cell_len())
            .filter(|&i| channel(cell_coords(i)))
            .map(|i| tiles.pool_index(world(cell_coords(i))).expect("held"))
            .min()
            .expect("a channel");
        let state = tiles.cells(&rig.read::<u32>(23, tiles.pool_len()));
        let label = tiles.cells(&rig.read::<u32>(25, tiles.pool_len()));
        for i in 0..cell_len() {
            if channel(cell_coords(i)) {
                assert_eq!(state[i], POCKET_SEALED, "slot 0 {first:?}: cell {i} sealed");
                assert_eq!(
                    label[i] as usize, want,
                    "slot 0 {first:?}: cell {i} holds the channel's lowest pool index after one round"
                );
            } else {
                assert_eq!(state[i], POCKET_DRY, "slot 0 {first:?}: cell {i} dry");
            }
        }
        // A second round has nothing left to change.
        rig.run("pocket_round", &params, 1);
        for sweep in ["pocket_sweep_x", "pocket_sweep_y", "pocket_sweep_z"] {
            rig.run(sweep, &params, 64 * tiles.tiles.len());
        }
        assert_eq!(
            rig.gate()[9],
            0,
            "slot 0 {first:?}: the second round is quiet"
        );
    }
}

/// Pocket sweeps' original triples and appended replay range agree through
/// changed, quiet and inactive rounds, even with a stale active range, and
/// the sweeps' bound is the held slots.
#[test]
pub(super) fn gpu_flip_pocket_round_initializes_replay_gate() {
    let tiles = Tiles::new(shuffled(box_tiles(), 0x6a7e));
    let params = lattice(&tiles);
    let words = POCKET_GATE_WORDS as usize;
    assert_eq!(
        words, 17,
        "eleven original words, the held slots, two replay range words and setup triple"
    );
    let held = tiles.pool_tiles();
    let mut clock = [0u32; 12];
    clock[0] = (1.0f32 / 60.0).to_bits();
    clock[11] = 1;
    let mut rig = Rig::over(&tiles);
    rig.bind(24, &vec![73u32; words]).bind(46, &clock);
    let mut expected = vec![73u32; words];
    expected[9] = 1;
    expected[10] = 0;
    expected[11] = held;
    expected[12..14].copy_from_slice(&[0, 0]);
    expected[14..17].copy_from_slice(&[0, 1, 1]);
    rig.run("pocket_start", &params, 1);
    assert_eq!(
        rig.gate(),
        expected,
        "active start clears the range and preserves original triple words"
    );
    // 64 lines a held slot, 256 threads a group.
    let groups = (held * 64).div_ceil(256);
    for axis in 0..3 {
        expected[3 * axis..3 * axis + 3].copy_from_slice(&[groups, 1, 1]);
    }
    expected[9] = 0;
    expected[12..14].copy_from_slice(&[0, 4]);
    expected[14] = 1;
    rig.run("pocket_round", &params, 1);
    assert_eq!(
        rig.gate(),
        expected,
        "changed round enables three sweeps and next setup"
    );
    for axis in 0..3 {
        expected[3 * axis] = 0;
    }
    expected[12..14].copy_from_slice(&[0, 0]);
    expected[14] = 0;
    rig.run("pocket_round", &params, 1);
    assert_eq!(rig.gate(), expected, "quiet round enables no sweep");

    // An inactive slot always starts before its rounds. Start clears the
    // preceding active slot's range; each inactive round keeps it cleared.
    let mut stale = vec![73u32; words];
    stale[9] = 1;
    stale[12..14].copy_from_slice(&[0, 4]);
    stale[14..17].copy_from_slice(&[1, 1, 1]);
    rig.bind(24, &stale);
    clock[0] = 0;
    rig.bind(46, &clock);
    for axis in 0..3 {
        stale[3 * axis] = 0;
    }
    stale[9] = 0;
    stale[10] = 0;
    stale[11] = held;
    stale[12..14].copy_from_slice(&[0, 0]);
    stale[14] = 0;
    rig.run("pocket_start", &params, 1);
    assert_eq!(
        rig.gate(),
        stale,
        "inactive start clears stale execution and retains original word semantics"
    );
    rig.run("pocket_round", &params, 1);
    assert_eq!(
        rig.gate(),
        stale,
        "inactive round preserves the cleared execution range"
    );
}

#[test]
pub(super) fn gpu_flip_pocket_spread_reports_an_unfinished_cap() {
    // Every cell water and every link open, air only past the open -X
    // border faces: with no round run, sealed cells still link to air.
    let tiles = Tiles::new(shuffled(box_tiles(), 0x5e9));
    let params = lattice(&tiles);
    let water = vec![1.0f32; cell_len()];
    let mut open = open_faces(0x5e9, false);
    open_tank_faces(&mut open, 63 & !1);
    let mut rig = Rig::over(&tiles);
    rig.bind(6, &tiles.pooled_cells(&water))
        .bind(10, &tiles.pooled_records(&open))
        .bind(23, &vec![0u32; tiles.pool_len()])
        .bind(24, &[0u32; POCKET_GATE_WORDS as usize])
        .bind(25, &vec![0u32; tiles.pool_len()]);
    let cells = 512 * tiles.tiles.len();
    rig.run("pocket_seed", &params, cells);
    rig.run("pocket_start", &params, 1);
    rig.run("pocket_check", &params, cells);
    assert_eq!(rig.gate()[10], 1, "an unfinished spread is flagged");
    // The seed classes the cells of every held tile: the water of the box
    // and the dry cells its tiles hold around it.
    let pocket = rig.read::<u32>(23, tiles.pool_len());
    let held = 512 * tiles.tiles.len();
    let counts: Vec<u32> = (0..3)
        .map(|k| pocket[..held].iter().filter(|&&s| s == k).count() as u32)
        .collect();
    assert_eq!(
        counts,
        [
            (held - cell_len()) as u32,
            (cell_len() - N[1] * N[2]) as u32,
            (N[1] * N[2]) as u32
        ],
        "every box cell water, the -X layer touching air, the rest of the tiles dry"
    );
    rig.bind(22, &[5u32, 6, 7, 9]);
    rig.run(
        "pocket_tally",
        &StepParams {
            step_in_tick: 0,
            ..params
        },
        1,
    );
    let first: Vec<u32> = rig.read(22, 4);
    assert_eq!(first, [5, 6, 7, 1], "the tick's first step sets the word");
    rig.run(
        "pocket_tally",
        &StepParams {
            step_in_tick: 1,
            ..params
        },
        1,
    );
    let second: Vec<u32> = rig.read(22, 4);
    assert_eq!(second, [5, 6, 7, 2], "later steps add to it");
}

/// The coarse pockets at solve level 1 as the CPU coarsens them: a level
/// cell (world-aligned: it covers fine cells 2C and 2C + 1 on each axis) is
/// sealed when every in-box child that is not solid is sealed under one fine
/// label, and takes the lowest level pool index of that fine label as its
/// own. Per level pool index the label, None when dry.
fn cpu_coarse_pockets(
    level: &Tiles,
    labels: &[Option<usize>],
    solid: &[bool],
) -> Vec<Option<usize>> {
    let mut fine_label = vec![None; level.pool_len()];
    for (slot, t) in level.tiles.iter().enumerate() {
        for l in 0..512i32 {
            let c = [
                t[0] * 8 + (l & 7),
                t[1] * 8 + ((l >> 3) & 7),
                t[2] * 8 + (l >> 6),
            ];
            let mut shared = None;
            let mut sealed = true;
            for child in 0..8 {
                let q: [i32; 3] = std::array::from_fn(|a| 2 * c[a] + ((child >> a) & 1));
                let Some(p) = within(q, N) else { continue };
                if solid[cell_index(p)] {
                    continue;
                }
                match (labels[cell_index(p)], shared) {
                    (None, _) => sealed = false,
                    (Some(l), None) => shared = Some(l),
                    (Some(l), Some(s)) if l != s => sealed = false,
                    _ => {}
                }
            }
            fine_label[slot * 512 + l as usize] = shared.filter(|_| sealed);
        }
    }
    (0..level.pool_len())
        .map(|i| {
            fine_label[i].map(|l| {
                (0..level.pool_len())
                    .find(|&j| fine_label[j] == Some(l))
                    .expect("itself at the latest")
            })
        })
        .collect()
}

/// At Solve Level 1 the pockets are coarsened with the water
/// (docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 11 (Solve Level)): a
/// level cell is sealed when all its fine children are sealed under one
/// label, its label the lowest level cell of that pocket, and the mean
/// comes off the coarse right-hand side pocket by pocket so each sums to
/// zero again; what it removes is h³ at the coarse cell size. The level is a
/// world-tiled lattice of its own, its cells aligned to the world's, so the
/// box's edge cells have one child on an axis where the box starts at an odd
/// cell. A tank of water, whole; split by a closed plane on a coarse boundary
/// (two coarse pockets); split off it (the straddling coarse cells stay
/// dry); with one solid cell (not water, every face closed) inside a coarse
/// cell, which stays sealed with its seven water children.
#[test]
pub(super) fn gpu_flip_pocket_mean_at_solve_level_one_sums_to_zero() {
    let tiles = Tiles::new(shuffled(box_tiles(), 0xc0a5));
    // The level's tiles are the parents of the fine tiles, in a slot order
    // of their own.
    let mut parents: Vec<[i32; 3]> = tiles
        .tiles
        .iter()
        .map(|t| t.map(|v| v >> 1))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    parents = shuffled(parents, 0x1e7e1);
    let level = Tiles::new(parents);
    let mut sealed_coarse_total = 0;
    for (case, split_x, solid_cell) in [
        ("whole", None, None),
        // Fine cells 1 | 2 are coarse cells 0 | 1: face 2 is a coarse boundary.
        ("split on the coarse boundary", Some(2), None),
        // Fine cells 0 | 1 share coarse cell 0: face 1 cuts through it.
        ("split through a coarse cell", Some(1), None),
        ("a solid cell inside a coarse cell", None, Some([1, 1, 1])),
    ] {
        let mut water = vec![1.0f32; cell_len()];
        let mut open = open_faces(0x5ea, false);
        if let Some(x) = split_x {
            for (i, face) in open.iter_mut().enumerate() {
                if world(pad_coords(i))[0] == x {
                    face.weight[0] = 0.0;
                }
            }
        }
        let solid: Vec<bool> = (0..cell_len())
            .map(|i| solid_cell == Some(world(cell_coords(i))))
            .collect();
        if let Some(c) = solid_cell {
            water[cell_index(within(c, N).expect("in the box"))] = 0.0;
            for (i, face) in open.iter_mut().enumerate() {
                let p = world(pad_coords(i));
                for a in 0..3 {
                    if (0..3).all(|b| b == a || p[b] == c[b]) && (p[a] == c[a] || p[a] == c[a] + 1)
                    {
                        face.weight[a] = 0.0;
                    }
                }
            }
        }
        let (state, label) = fine_pockets(&tiles, &water, &open);
        let (_, sealed) = cpu_isolated(&water, &open);
        assert!(
            (0..cell_len()).all(|i| sealed[i] || solid[i]),
            "{case}: every water cell is sealed"
        );
        let fine_labels = cpu_pocket_labels(&tiles, &sealed, &open);
        let dense_state = tiles.cells(&state);
        let dense_label = tiles.cells(&label);
        for i in 0..cell_len() {
            if solid[i] {
                assert_ne!(dense_state[i], 1, "{case}: the solid cell {i} is no pocket");
                continue;
            }
            assert_eq!(dense_state[i], 1, "{case}: fine cell {i} sealed");
            assert_eq!(
                Some(dense_label[i] as usize),
                fine_labels[i],
                "{case}: fine cell {i} label"
            );
        }
        let want = cpu_coarse_pockets(&level, &fine_labels, &solid);
        let level_len = level.pool_len();
        let params = StepParams {
            solve_level: 1,
            ..lattice(&tiles)
        };
        // The fine passes read the fine table and its list; the level's
        // passes run over the level's own list.
        let mut rig = Rig::over(&tiles);
        rig.bind(6, &tiles.pooled_cells(&water))
            .bind(10, &tiles.pooled_records(&open))
            .bind(23, &state)
            .bind(25, &label)
            .bind(39, &vec![7u32; level_len])
            .bind(40, &vec![7u32; level_len])
            .bind(41, &vec![0u32; tiles.pool_len()])
            .bind(63, &[table_entries(level.pool_tiles()), 0, 0, 0])
            .bind(64, &level.words);
        rig.run("pocket_leader_clear", &params, 512 * tiles.tiles.len());
        rig.bind(29, &level.list(0x1e7e2));
        rig.run("pocket_coarsen", &params, 512 * level.tiles.len());
        rig.run("pocket_relabel", &params, 512 * level.tiles.len());
        let got_state: Vec<u32> = rig.read(39, level_len);
        let got_label: Vec<u32> = rig.read(40, level_len);
        if solid_cell.is_some() {
            assert!(
                want.iter().flatten().count() > 0,
                "{case}: the coarse cell over the solid cell is sealed"
            );
        }
        for i in 0..level.tiles.len() * 512 {
            assert_eq!(
                got_state[i] == 1,
                want[i].is_some(),
                "{case}: coarse cell {i} sealed"
            );
            if let Some(l) = want[i] {
                assert_eq!(got_label[i] as usize, l, "{case}: coarse cell {i} label");
            }
        }
        let sealed_count = want.iter().flatten().count();
        sealed_coarse_total += sealed_count;
        let pockets: std::collections::BTreeSet<usize> = want.iter().flatten().copied().collect();
        println!(
            "coarse pockets {case}: {sealed_count} coarse cells sealed in {} pockets {pockets:?}",
            pockets.len()
        );
        // The mean off the coarse right-hand side, on the coarse lattice at
        // its cell size, the flux into the pressure word of a later substep.
        let mut rng = Stream::new(0x5eb);
        let rhs: Vec<f32> = (0..level_len).map(|_| 4.0 * rng.unit() - 1.0).collect();
        let coarse_params = StepParams {
            cell_size: 2.0 * H,
            step_in_tick: 1,
            ..params
        };
        let mut mean = Rig::new();
        mean.bind(23, &got_state)
            .bind(25, &got_label)
            .bind(5, &rhs)
            .bind(26, &vec![0u32; 2 + 3 * level_len])
            .bind(29, &level.list(0x1e7e3))
            .bind(22, &[9u32; 6]);
        let cells = 512 * level.tiles.len();
        mean.run("pocket_clear", &coarse_params, cells);
        mean.run("pocket_accumulate", &coarse_params, cells);
        mean.run("pocket_remove", &coarse_params, cells);
        mean.run("pocket_flux_pressure", &coarse_params, 1);
        let removed: Vec<f32> = mean.read(5, level_len);
        let words: Vec<u32> = mean.read(22, 6);
        let mut total = 0.0;
        for &l in &pockets {
            let members: Vec<usize> = (0..level_len).filter(|&i| want[i] == Some(l)).collect();
            let sum: f64 = members.iter().map(|&i| f64::from(rhs[i])).sum();
            total += sum.abs();
            for &i in &members {
                let want_value = f64::from(rhs[i]) - sum / members.len() as f64;
                assert!(
                    (f64::from(removed[i]) - want_value).abs() <= 1e-4,
                    "{case}: coarse cell {i} rhs {} want {want_value}",
                    removed[i]
                );
            }
            let left: f64 = members.iter().map(|&i| f64::from(removed[i])).sum();
            assert!(
                left.abs() < 1e-3,
                "{case}: coarse pocket {l} sums to {left} after its mean is removed"
            );
        }
        for i in (0..level_len).filter(|&i| want[i].is_none()) {
            assert_eq!(
                removed[i], rhs[i],
                "{case}: dry coarse cell {i} keeps its value"
            );
        }
        let h = 2.0 * f64::from(H);
        let flux = f64::from(f32::from_bits(words[4])) - f64::from(f32::from_bits(9));
        let want_flux = total * h * h * h;
        assert!(
            (flux - want_flux).abs() <= 1e-4 * want_flux.max(1e-9),
            "{case}: removed flux {flux}, want {want_flux} at the coarse cell size"
        );
    }
    assert!(sealed_coarse_total > 0);
}
