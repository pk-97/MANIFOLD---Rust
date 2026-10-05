//! Ported from FLIP Fluids particlesheeter.cpp, interpolation.cpp,
//! particlemaskgrid.cpp and gridutils.cpp (MIT); see THIRD_PARTY_NOTICES.md.
//! A CPU reference of `ParticleSheeter::generateSheetParticles`, the spec the
//! GPU sheeting kernels are proven against. It runs the engine's single-thread
//! order and its precision: f32 vectors (vmath), f64 where the engine widens
//! (cell indexing, interpolation weights, `dx` products). Rust never fuses a
//! multiply-add; the engine build does, so results can differ from the native
//! sheeter by a few ulps (`sheet_oracle` tests name the bound).

use crate::FluidError;

const MAX_SHEET_DEPTH: f32 = 2.0;
const DEPTH_TEST_DISTANCE: f32 = 3.0;
const DEPTH_TEST_STEP_DISTANCE: f32 = 0.5;
const MAX_PARTICLES_PER_CELL: u8 = 6;
const MAX_SHEET_PARTICLES_PER_CELL: u8 = 4;
const MAX_SEED_CANDIDATE_DEPTH: f32 = 1.0;
const SHEET_SEARCH_RADIUS: f32 = 2.0;
const PROJECTION_FACTOR: f32 = 0.75;
const EPS: f32 = 1e-5;

type V = [f32; 3];

fn add(a: V, b: V) -> V {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: V, b: V) -> V {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(s: f32, v: V) -> V {
    [v[0] * s, v[1] * s, v[2] * s]
}
fn dot(a: V, b: V) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V, b: V) -> V {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn length(v: V) -> f32 {
    dot(v, v).sqrt()
}
/// vmath: `v / len` multiplies by `(float)(1.0 / len)`.
fn div(v: V, s: f32) -> V {
    scale((1.0 / s as f64) as f32, v)
}
fn normalize(v: V) -> V {
    div(v, length(v))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Cell([i32; 3]);

/// Grid3d::positionToGridIndex: floor of the f64 position times f64 1/dx.
fn cell_of(p: V, dx: f64) -> Cell {
    let inv = 1.0 / dx;
    Cell(p.map(|c| (c as f64 * inv).floor() as i32))
}

struct Grid<T> {
    n: [i32; 3],
    data: Vec<T>,
}

impl<T: Copy> Grid<T> {
    fn new(n: [i32; 3], fill: T) -> Self {
        Self { n, data: vec![fill; (n[0] * n[1] * n[2]) as usize] }
    }
    fn in_range(&self, c: [i32; 3]) -> bool {
        (0..3).all(|a| c[a] >= 0 && c[a] < self.n[a])
    }
    fn index(&self, c: [i32; 3]) -> usize {
        (c[0] + self.n[0] * (c[1] + self.n[1] * c[2])) as usize
    }
    fn get(&self, c: [i32; 3]) -> T {
        self.data[self.index(c)]
    }
    fn set(&mut self, c: [i32; 3], v: T) {
        let i = self.index(c);
        self.data[i] = v;
    }
    fn get_or(&self, c: [i32; 3], zero: T) -> T {
        if self.in_range(c) { self.get(c) } else { zero }
    }
}

/// Grid3d::GridIndexToPosition: `(float)i * dx` in f64, stored as f32.
fn cell_origin(c: Cell, dx: f64) -> V {
    c.0.map(|i| ((i as f32) as f64 * dx) as f32)
}

fn local(p: V, dx: f64) -> (Cell, [f64; 3]) {
    let g = cell_of(p, dx);
    let o = cell_origin(g, dx);
    let inv = 1.0 / dx;
    (g, [0, 1, 2].map(|a| (p[a] - o[a]) as f64 * inv))
}

/// Interpolation::trilinearInterpolate over a cell-centred f32 grid; corners
/// outside the grid read 0.
fn trilinear(p: V, dx: f64, grid: &Grid<f32>) -> f64 {
    let (Cell([i, j, k]), [x, y, z]) = local(p, dx);
    let v = |c: [i32; 3]| grid.get_or(c, 0.0) as f64;
    let p0 = v([i, j, k]);
    let p1 = v([i + 1, j, k]);
    let p2 = v([i, j + 1, k]);
    let p3 = v([i, j, k + 1]);
    let p4 = v([i + 1, j, k + 1]);
    let p5 = v([i, j + 1, k + 1]);
    let p6 = v([i + 1, j + 1, k]);
    let p7 = v([i + 1, j + 1, k + 1]);
    p0 * (1.0 - x) * (1.0 - y) * (1.0 - z)
        + p1 * x * (1.0 - y) * (1.0 - z)
        + p2 * (1.0 - x) * y * (1.0 - z)
        + p3 * (1.0 - x) * (1.0 - y) * z
        + p4 * x * (1.0 - y) * z
        + p5 * (1.0 - x) * y * z
        + p6 * x * y * (1.0 - z)
        + p7 * x * y * z
}

fn bilinear(v00: f32, v10: f32, v01: f32, v11: f32, ix: f64, iy: f64) -> f32 {
    let lerp1 = (1.0 - ix) * v00 as f64 + ix * v10 as f64;
    let lerp2 = (1.0 - ix) * v01 as f64 + ix * v11 as f64;
    ((1.0 - iy) * lerp1 + iy * lerp2) as f32
}

/// Interpolation::trilinearInterpolateGradient: the interpolant's derivative
/// in cell units, not divided by dx.
fn trilinear_gradient(p: V, dx: f64, grid: &Grid<f32>) -> V {
    let (Cell([i, j, k]), [ix, iy, iz]) = local(p, dx);
    let v = |c: [i32; 3]| grid.get_or(c, 0.0);
    let v000 = v([i, j, k]);
    let v100 = v([i + 1, j, k]);
    let v010 = v([i, j + 1, k]);
    let v001 = v([i, j, k + 1]);
    let v101 = v([i + 1, j, k + 1]);
    let v011 = v([i, j + 1, k + 1]);
    let v110 = v([i + 1, j + 1, k]);
    let v111 = v([i + 1, j + 1, k + 1]);
    [
        bilinear(v100 - v000, v110 - v010, v101 - v001, v111 - v011, iy, iz),
        bilinear(v010 - v000, v110 - v100, v011 - v001, v111 - v101, ix, iz),
        bilinear(v001 - v000, v101 - v100, v011 - v010, v111 - v110, ix, iy),
    ]
}

/// ParticleMaskGrid: one bit per half-cell. A particle's bit comes from its
/// half-cell index, its byte from its full-cell index (two separate floors).
struct Mask {
    dx: f64,
    sub_dx: f64,
    n: [i32; 3],
    bits: Grid<u8>,
}

impl Mask {
    fn bit(sub: Cell) -> u8 {
        let case = (sub.0[0] % 2 == 1) as u8 | ((sub.0[1] % 2 == 1) as u8) << 1 | ((sub.0[2] % 2 == 1) as u8) << 2;
        1 << case
    }
    fn add(&mut self, p: V) {
        let bit = Self::bit(cell_of(p, self.sub_dx));
        let g = cell_of(p, self.dx).0;
        let old = self.bits.get(g);
        self.bits.set(g, old | bit);
    }
    fn is_set(&self, p: V) -> bool {
        let sub = cell_of(p, self.sub_dx);
        self.bits.get(sub.0.map(|c| c / 2)) & Self::bit(sub) != 0
    }
    fn in_grid(&self, p: V) -> bool {
        (0..3).all(|a| p[a] >= 0.0 && (p[a] as f64) < self.dx * self.n[a] as f64)
    }
}

/// SortedParticleData: 2-cell buckets, each holding its particles in
/// insertion order; buckets with particles are visited k, j, i.
struct Buckets {
    cells: Grid<u32>,
    lists: Vec<Vec<V>>,
}

impl Buckets {
    fn new(points: &[V], n: [i32; 3], dx: f64, cap: usize) -> Self {
        let reduction = SHEET_SEARCH_RADIUS.ceil() as i32;
        let bn = n.map(|c| (c as f32 / reduction as f32).ceil() as i32);
        let bdx = reduction as f64 * dx;
        let mut cells = Grid::new(bn, u32::MAX);
        let mut lists: Vec<Vec<V>> = Vec::new();
        let mut valid = Grid::new(bn, false);
        for &p in points {
            valid.set(cell_of(p, bdx).0, true);
        }
        for k in 0..bn[2] {
            for j in 0..bn[1] {
                for i in 0..bn[0] {
                    if valid.get([i, j, k]) {
                        cells.set([i, j, k], lists.len() as u32);
                        lists.push(Vec::new());
                    }
                }
            }
        }
        for &p in points {
            let list = &mut lists[cells.get(cell_of(p, bdx).0) as usize];
            // The engine's flat buffer gives each bucket exactly `cap` slots.
            debug_assert!(list.len() < cap, "bucket overflow the engine would corrupt");
            list.push(p);
        }
        Self { cells, lists }
    }
    fn at(&self, c: [i32; 3]) -> Option<&[V]> {
        if !self.cells.in_range(c) {
            return None;
        }
        match self.cells.get(c) {
            u32::MAX => None,
            i => Some(&self.lists[i as usize]),
        }
    }
}

/// `ParticleSheeter::generateSheetParticles` for markers at `positions`
/// (grid-local, inside the grid) over the cell-centred surface level set
/// `phi` (x fastest), before the engine's fill-rate draw.
pub fn generate_sheet_particles(
    positions: &[[f32; 3]],
    phi: &[f32],
    cells: [u32; 3],
    dx: f64,
    fill_threshold: f32,
) -> Result<Vec<[f32; 3]>, FluidError> {
    Ok(trace_sheet_particles(positions, phi, cells, dx, fill_threshold)?.seeds)
}

/// Each level-set decision the sheeter made, for comparing two level sets.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SheetTrace {
    /// Per marker: passed phase 1 (sparse enough, near the surface, thin).
    pub thin: Vec<bool>,
    /// Per marker: kept by phase 2 (sheet cell, under the cap, in the band).
    pub kept: Vec<bool>,
    /// Seed candidates in visiting order, before the plane and mask tests.
    pub candidates: Vec<[f32; 3]>,
    pub seeds: Vec<[f32; 3]>,
    /// Per seed: the candidate it was projected from.
    pub seed_sources: Vec<[f32; 3]>,
}

/// [`generate_sheet_particles`] with its decisions recorded.
pub fn trace_sheet_particles(
    positions: &[[f32; 3]],
    phi: &[f32],
    cells: [u32; 3],
    dx: f64,
    fill_threshold: f32,
) -> Result<SheetTrace, FluidError> {
    let mut trace = SheetTrace { thin: vec![false; positions.len()], kept: vec![false; positions.len()], ..Default::default() };
    if cells.iter().any(|&c| c == 0 || c > (i32::MAX / 2) as u32) || !dx.is_finite() || dx <= 0.0 {
        return Err(FluidError::input("sheeter grid needs positive size and cell size"));
    }
    let n = cells.map(|c| c as i32);
    let count = cells.iter().try_fold(1usize, |a, &c| a.checked_mul(c as usize));
    if count != Some(phi.len()) {
        return Err(FluidError::input("sheeter level set length differs from the grid"));
    }
    let level = Grid { n, data: phi.to_vec() };
    let mut mask = Mask { dx, sub_dx: 0.5 * dx, n, bits: Grid::new(n, 0) };
    if positions.iter().any(|&p| !mask.in_grid(p)) {
        return Err(FluidError::input("sheeter marker outside the grid"));
    }
    let hdx = [(0.5 * dx) as f32; 3];
    let max_depth = (MAX_SHEET_DEPTH as f64 * dx) as f32;

    let mut counts = Grid::new(n, 0u8);
    for &p in positions {
        let g = cell_of(p, dx).0;
        let c = counts.get(g);
        if c != 255 {
            counts.set(g, c + 1);
        }
    }

    // Phase 1: a marker is on a sheet if walking inward along -∇φ finds φ
    // rising or reaching the surface within 2.5 cells.
    let test_distance = (DEPTH_TEST_DISTANCE as f64 * dx) as f32;
    let step_distance = (DEPTH_TEST_STEP_DISTANCE as f64 * dx) as f32;
    let steps = (test_distance / step_distance).ceil() as i32;
    let mut sheet = Grid::new(n, false);
    for (index, &p) in positions.iter().enumerate() {
        let g = cell_of(p, dx).0;
        if counts.get(g) >= MAX_PARTICLES_PER_CELL {
            continue;
        }
        let phi = trilinear(sub(p, hdx), dx, &level) as f32;
        if phi >= max_depth || phi < -max_depth {
            continue;
        }
        let dir = trilinear_gradient(sub(p, hdx), dx, &level).map(|c| -c);
        if length(dir) < EPS {
            continue;
        }
        let dir = normalize(dir);
        let mut current = phi;
        let mut thin = false;
        for step in 0..steps {
            let next = add(p, scale(step as f32 * step_distance, dir));
            let next_phi = trilinear(sub(next, hdx), dx, &level) as f32;
            if next_phi > current || next_phi >= 0.0 {
                thin = true;
                break;
            }
            current = next_phi;
        }
        if thin {
            trace.thin[index] = true;
            sheet.set(g, true);
        }
    }

    // featherGrid6 twice (each pass grows from the previous pass's cells),
    // then a 3-cell border band cleared.
    for _ in 0..2 {
        let seeds = sheet.data.clone();
        for k in 0..n[2] {
            for j in 0..n[1] {
                for i in 0..n[0] {
                    if !seeds[sheet.index([i, j, k])] {
                        continue;
                    }
                    for nb in [[i - 1, j, k], [i + 1, j, k], [i, j - 1, k], [i, j + 1, k], [i, j, k - 1], [i, j, k + 1]] {
                        if sheet.in_range(nb) {
                            sheet.set(nb, true);
                        }
                    }
                }
            }
        }
    }
    const BORDER: i32 = 3;
    for k in 0..n[2] {
        for j in 0..n[1] {
            for i in 0..n[0] {
                if i < BORDER || j < BORDER || k < BORDER || i >= n[0] - BORDER || j >= n[1] - BORDER || k >= n[2] - BORDER {
                    sheet.set([i, j, k], false);
                }
            }
        }
    }

    // Phase 2: the first four markers per sheet cell, in input order, near
    // the surface.
    let mut counts = Grid::new(n, 0u8);
    let mut sheet_particles = Vec::new();
    for (index, &p) in positions.iter().enumerate() {
        let g = cell_of(p, dx).0;
        if !sheet.get(g) || counts.get(g) >= MAX_SHEET_PARTICLES_PER_CELL {
            continue;
        }
        let phi = trilinear(sub(p, hdx), dx, &level) as f32;
        if phi >= max_depth || phi < -max_depth {
            continue;
        }
        trace.kept[index] = true;
        sheet_particles.push(p);
        counts.set(g, counts.get(g) + 1);
    }
    if sheet_particles.is_empty() {
        return Ok(trace);
    }

    for &p in positions {
        mask.add(p);
    }

    // Candidates: half-cell centres of sheet cells with φ in [-dx, 0), cells
    // in k, j, i order, the eight offsets with k fastest.
    let max_seed_depth = (MAX_SEED_CANDIDATE_DEPTH as f64 * dx) as f32;
    let sub_dx = 0.5 * dx;
    let hw = 0.5 * sub_dx;
    let mut candidates = Vec::new();
    for k in 0..n[2] {
        for j in 0..n[1] {
            for i in 0..n[0] {
                if !sheet.get([i, j, k]) {
                    continue;
                }
                for o in 0..8 {
                    let s = [2 * i + (o >> 2 & 1), 2 * j + (o >> 1 & 1), 2 * k + (o & 1)];
                    let seed = s.map(|c| ((c as f32) as f64 * sub_dx + hw) as f32);
                    let phi = trilinear(sub(seed, hdx), dx, &level) as f32;
                    if phi >= 0.0 || phi < -max_seed_depth {
                        continue;
                    }
                    candidates.push(seed);
                }
            }
        }
    }

    let r = SHEET_SEARCH_RADIUS.ceil() as usize;
    let sheet_buckets = Buckets::new(&sheet_particles, n, dx, r * r * r * MAX_SHEET_PARTICLES_PER_CELL as usize);
    let candidate_buckets = Buckets::new(&candidates, n, dx, r * r * r * 8);
    let max_radius = (SHEET_SEARCH_RADIUS as f64 * dx) as f32;

    let mut out = Vec::new();
    let mut neighbours = Vec::new();
    let mut nearest = Vec::new();
    let bn = candidate_buckets.cells.n;
    for bk in 0..bn[2] {
        for bj in 0..bn[1] {
            for bi in 0..bn[0] {
                let Some(bucket) = candidate_buckets.at([bi, bj, bk]) else { continue };
                neighbours.clear();
                for k in bk - 1..=bk + 1 {
                    for j in bj - 1..=bj + 1 {
                        for i in bi - 1..=bi + 1 {
                            if let Some(list) = sheet_buckets.at([i, j, k]) {
                                neighbours.extend_from_slice(list);
                            }
                        }
                    }
                }
                if neighbours.len() < 3 {
                    continue;
                }
                for &candidate in bucket {
                    let p = candidate;
                    nearest.clear();
                    nearest.extend(neighbours.iter().copied().filter(|&np| length(sub(np, p)) < max_radius));
                    if nearest.len() < 3 {
                        continue;
                    }
                    let mut centroid = [0.0f32; 3];
                    for &np in &nearest {
                        centroid = add(centroid, np);
                    }
                    centroid = div(centroid, nearest.len() as f32);
                    let (mut len1, mut len2, mut len3) = (1e6f32, 1e6f32, 1e6f32);
                    let (mut p1, mut p2, mut p3) = ([0.0f32; 3], [0.0f32; 3], [0.0f32; 3]);
                    for &np in &nearest {
                        let len = length(sub(np, p));
                        if len < len1 {
                            len3 = len2;
                            len2 = len1;
                            len1 = len;
                            p3 = p2;
                            p2 = p1;
                            p1 = np;
                        } else if len < len2 {
                            len3 = len2;
                            len2 = len;
                            p3 = p2;
                            p2 = np;
                        } else if len < len3 {
                            len3 = len;
                            p3 = np;
                        }
                    }
                    let vt1 = sub(p2, p1);
                    let vt2 = sub(p3, p1);
                    let c = cross(vt1, vt2);
                    if length(vt1) < EPS || length(vt2) < EPS || length(c) < EPS {
                        continue;
                    }
                    let normal = normalize(c);
                    let distance = -dot(normal, sub(p, p1));
                    let p = add(p, scale(PROJECTION_FACTOR * distance, normal));
                    if !mask.in_grid(p) || mask.is_set(p) {
                        continue;
                    }
                    let cdir = sub(centroid, p);
                    if length(cdir) < EPS {
                        continue;
                    }
                    let cdir = normalize(cdir);
                    let mut mindot = 1.01f32;
                    for &np in &nearest {
                        let ndir = sub(np, p);
                        if length(ndir) < EPS {
                            continue;
                        }
                        let d = dot(cdir, normalize(ndir));
                        if d < mindot {
                            mindot = d;
                        }
                    }
                    if mindot < fill_threshold {
                        out.push(p);
                        trace.seed_sources.push(candidate);
                        mask.add(p);
                    }
                }
            }
        }
    }
    trace.candidates = candidates;
    trace.seeds = out;
    Ok(trace)
}

/// Shared fixtures for the sheeter proofs (native oracle, CPU port, GPU stage).
pub mod fixtures {
    /// Test fixture: thin curved sheets with random holes on a dx 0.25 grid: a spherical
    /// shell and a rippled horizontal sheet, both 0.3 m thick, a few
    /// thousand markers, fixed seed.
    pub fn splash() -> (Vec<[f32; 3]>, Vec<f32>, [u32; 3], f64) {
        const M: u32 = 40;
        const H: f64 = 0.25;
        const T: f32 = 0.15;
        let centre = [5.0f32, 4.5, 5.0];
        let radius = 2.2f32;
        let ripple = |x: f32, z: f32| 8.3 + 0.4 * (1.3 * x).sin() * (0.9 * z).cos();
        let m = M as usize;
        let phi: Vec<f32> = (0..m * m * m)
            .map(|index| {
                let c = [index % m, (index / m) % m, index / (m * m)].map(|v| (v as f32 + 0.5) * H as f32);
                let r = ((c[0] - centre[0]).powi(2) + (c[1] - centre[1]).powi(2) + (c[2] - centre[2]).powi(2)).sqrt();
                let shell = (r - radius).abs() - T;
                let sheet = (c[1] - ripple(c[0], c[2])).abs() - T;
                shell.min(sheet)
            })
            .collect();
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut uniform = move || {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32
        };
        fn direction(u: &mut dyn FnMut() -> f32) -> [f32; 3] {
            let z = 2.0 * u() - 1.0;
            let a = std::f32::consts::TAU * u();
            let s = (1.0 - z * z).sqrt();
            [s * a.cos(), z, s * a.sin()]
        }
        let shell_holes: Vec<[f32; 3]> = (0..6).map(|_| direction(&mut uniform)).collect();
        // Sheet holes: centre x, radius 0.3–0.6 m, centre z.
        let mut sheet_holes = Vec::new();
        for _ in 0..6 {
            let x = 1.5 + 7.0 * uniform();
            let r = 0.3 + 0.3 * uniform();
            sheet_holes.push([x, r, 1.5 + 7.0 * uniform()]);
        }
        let mut markers = Vec::new();
        while markers.len() < 2500 {
            let d = direction(&mut uniform);
            let r = radius + T * 0.8 * (2.0 * uniform() - 1.0);
            if shell_holes.iter().any(|h| d[0] * h[0] + d[1] * h[1] + d[2] * h[2] > 0.25f32.cos()) {
                continue;
            }
            markers.push([centre[0] + r * d[0], centre[1] + r * d[1], centre[2] + r * d[2]]);
        }
        while markers.len() < 4000 {
            let (x, z) = (1.5 + 7.0 * uniform(), 1.5 + 7.0 * uniform());
            if sheet_holes.iter().any(|h| (x - h[0]).hypot(z - h[2]) < h[1]) {
                continue;
            }
            markers.push([x, ripple(x, z) + T * 0.8 * (2.0 * uniform() - 1.0), z]);
        }
        (markers, phi, [M; 3], H)
    }
}
