//! CPU statements of the whitewater grid atoms' contracts, line for line
//! with their WGSL bodies, for the GPU value proofs and the O1 tests
//! (`docs/GPU_WHITEWATER_DESIGN.md` section 3.7).

// The GPU proofs (`whitewater_grid_tests`, feature gpu-proofs) call every
// item here; a default test build compiles only the extrapolation check.
#![cfg_attr(not(feature = "gpu-proofs"), allow(dead_code))]

use crate::node_graph::whitewater::{CELL_AIR, CELL_LIQUID, CELL_SOLID, KnownValue, NO_CROSSING, SurfaceCrossing};

/// A whitewater grid over a solid lattice of `nodes`.
#[derive(Clone, Copy, Debug)]
pub(super) struct Grid {
    pub nodes: [u32; 3],
    pub cells: [u32; 3],
}

impl Grid {
    pub fn new(nodes: [u32; 3]) -> Self {
        Self { nodes, cells: nodes.map(|n| n - 1) }
    }

    pub fn total(&self) -> usize {
        self.cells.iter().map(|&n| n as usize).product()
    }

    pub fn index(&self, c: [u32; 3]) -> usize {
        let [nx, ny, _] = self.cells.map(|n| n as usize);
        c[0] as usize + nx * (c[1] as usize + ny * c[2] as usize)
    }

    pub fn coords(&self, index: usize) -> [u32; 3] {
        let [nx, ny, _] = self.cells.map(|n| n as usize);
        [index % nx, (index / nx) % ny, index / (nx * ny)].map(|n| n as u32)
    }

    pub fn on_border(&self, c: [u32; 3]) -> bool {
        (0..3).any(|a| c[a] == 0 || c[a] + 1 == self.cells[a])
    }

    /// The face neighbour `face` of `c` (−x, +x, −y, +y, −z, +z), if in the grid.
    pub fn face(&self, c: [u32; 3], face: usize) -> Option<[u32; 3]> {
        let mut n = c.map(i64::from);
        n[face / 2] += if face % 2 == 1 { 1 } else { -1 };
        (0..3).all(|a| n[a] >= 0 && n[a] < i64::from(self.cells[a])).then(|| n.map(|v| v as u32))
    }

    /// The solid at cell `c`'s eight corners, bit 0 x, bit 1 y, bit 2 z.
    pub fn corners(&self, solid: &[f32], c: [u32; 3]) -> [f32; 8] {
        let [nx, ny, _] = self.nodes.map(|n| n as usize);
        std::array::from_fn(|corner| {
            let n: [usize; 3] = std::array::from_fn(|a| (c[a] + ((corner >> a) & 1) as u32) as usize);
            solid[n[0] + nx * (n[1] + ny * n[2])]
        })
    }

    pub fn centre(c: [u32; 3]) -> [f32; 3] {
        c.map(|v| v as f32 + 0.5)
    }
}

fn corner_weight(f: [f32; 3], corner: usize) -> f32 {
    (0..3).map(|a| if (corner >> a) & 1 == 1 { f[a] } else { 1.0 - f[a] }).product()
}

fn dist2(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|i| (a[i] - b[i]) * (a[i] - b[i])).sum()
}

/// `node.surface_crossings` for one cell, with the squared distance (in
/// refined nodes) of the runner-up crossing, so a proof can allow a tie.
pub(super) fn surface_crossing(grid: &Grid, level: &[f32], solid: &[f32], s: u32, c: [u32; 3]) -> (SurfaceCrossing, f32, f32) {
    let levels = grid.cells.map(|n| (n * s + 1) as usize);
    let corners = grid.corners(solid, c);
    let side = (s + 1) as usize;
    let base = c.map(|v| v * s);
    let slot = |p: [usize; 3]| p[0] + side * (p[1] + side * p[2]);
    let mut phi = vec![0.0f32; side * side * side];
    let mut clear = vec![false; side * side * side];
    for z in 0..side {
        for y in 0..side {
            for x in 0..side {
                let q = [base[0] as usize + x, base[1] as usize + y, base[2] as usize + z];
                phi[slot([x, y, z])] = level[q[0] + levels[0] * (q[1] + levels[1] * q[2])];
                let f = [x, y, z].map(|v| v as f32 / s as f32);
                let solid: f32 = (0..8).map(|corner| corner_weight(f, corner) * corners[corner]).sum();
                clear[slot([x, y, z])] = solid >= 0.0;
            }
        }
    }
    let centre = [0.5 * s as f32; 3];
    let mut out = SurfaceCrossing { crossing: [NO_CROSSING; 3], level: NO_CROSSING };
    let (mut best, mut runner_up) = (3.0e38f32, 3.0e38f32);
    for a in 0..3 {
        let mut top = [s as usize; 3];
        top[a] = s as usize - 1;
        for z in 0..=top[2] {
            for y in 0..=top[1] {
                for x in 0..=top[0] {
                    let p0 = [x, y, z];
                    let mut p1 = p0;
                    p1[a] += 1;
                    let (i0, i1) = (slot(p0), slot(p1));
                    let (v0, v1) = (phi[i0], phi[i1]);
                    if !clear[i0] || !clear[i1] || (v0 < 0.0) == (v1 < 0.0) {
                        continue;
                    }
                    let t = v0 / (v0 - v1);
                    let mut root = p0.map(|v| v as f32);
                    root[a] += t;
                    let dd = dist2(root, centre);
                    if dd < best {
                        runner_up = best;
                        best = dd;
                        out.crossing = std::array::from_fn(|i| (base[i] as f32 + root[i]) / s as f32);
                    } else if dd < runner_up {
                        runner_up = dd;
                    }
                }
            }
        }
    }
    let low = centre.map(|v| (v.floor() as u32).min(s - 1) as usize);
    let f: [f32; 3] = std::array::from_fn(|i| centre[i] - low[i] as f32);
    out.level = (0..8)
        .map(|corner| {
            let q: [usize; 3] = std::array::from_fn(|i| low[i] + ((corner >> i) & 1));
            corner_weight(f, corner) * phi[slot(q)]
        })
        .sum();
    (out, best, runner_up)
}

/// `node.nearest_crossing` for one cell.
pub(super) fn nearest_crossing(grid: &Grid, crossings: &[SurfaceCrossing], c: [u32; 3]) -> SurfaceCrossing {
    let own = crossings[grid.index(c)];
    let centre = Grid::centre(c);
    let (mut best, mut nearest) = (own.crossing, dist2(own.crossing, centre));
    for dz in -1i64..=1 {
        for dy in -1i64..=1 {
            for dx in -1i64..=1 {
                let n = [i64::from(c[0]) + dx, i64::from(c[1]) + dy, i64::from(c[2]) + dz];
                if (dx, dy, dz) == (0, 0, 0) || (0..3).any(|a| n[a] < 0 || n[a] >= i64::from(grid.cells[a])) {
                    continue;
                }
                let other = crossings[grid.index(n.map(|v| v as u32))].crossing;
                let ee = dist2(other, centre);
                if ee < nearest {
                    nearest = ee;
                    best = other;
                }
            }
        }
    }
    SurfaceCrossing { crossing: best, level: own.level }
}

/// `node.crossing_distance` for one cell.
pub(super) fn crossing_distance(grid: &Grid, crossing: SurfaceCrossing, solid: &[f32], h: f32, c: [u32; 3]) -> f32 {
    let reach = dist2(crossing.crossing, Grid::centre(c)).sqrt() * h;
    let mut d = if crossing.level < 0.0 { -1.0 } else { 1.0 } * reach.min(4.0 * h);
    let solid: f32 = grid.corners(solid, c).iter().sum();
    if d < 0.5 * h && 0.125 * solid < 0.0 {
        d = -0.5 * h;
    }
    let eps = 0.005 * h;
    if d.abs() < eps {
        d = if d > 0.0 { eps } else { -eps };
    }
    d
}

fn cell_kind(grid: &Grid, distance: &[f32], solid: &[f32], c: [u32; 3]) -> u32 {
    let solid: f32 = grid.corners(solid, c).iter().sum();
    if 0.125 * solid < 0.0 {
        CELL_SOLID
    } else if distance[grid.index(c)] < 0.0 {
        CELL_LIQUID
    } else {
        CELL_AIR
    }
}

/// `node.liquid_cells` for one cell.
pub(super) fn liquid_cell(grid: &Grid, distance: &[f32], solid: &[f32], c: [u32; 3]) -> u32 {
    let own = cell_kind(grid, distance, solid, c);
    if own != CELL_LIQUID {
        return own;
    }
    let bordering_air = (0..6).filter_map(|face| grid.face(c, face)).any(|n| cell_kind(grid, distance, solid, n) == CELL_AIR);
    if bordering_air { CELL_AIR } else { CELL_LIQUID }
}

/// `node.lattice_curvature` for one cell.
pub(super) fn lattice_curvature(grid: &Grid, distance: &[f32], h: f32, c: [u32; 3]) -> KnownValue {
    let unknown = KnownValue { value: 0.0, known: 0.0 };
    if grid.on_border(c) {
        return unknown;
    }
    let phi = |d: [i64; 3]| distance[grid.index(std::array::from_fn(|a| (i64::from(c[a]) + d[a]) as u32))];
    // False for NaN too, as the body's `abs(v) < band` is.
    let near = |v: f32| v.abs() < 2.0 * h;
    let p = phi([0, 0, 0]);
    let faces = [[1, 0, 0], [-1, 0, 0], [0, 1, 0], [0, -1, 0], [0, 0, 1], [0, 0, -1]];
    if !near(p) || faces.iter().any(|&f| !near(phi(f))) {
        return unknown;
    }
    let x = 0.5 * (phi([1, 0, 0]) - phi([-1, 0, 0]));
    let y = 0.5 * (phi([0, 1, 0]) - phi([0, -1, 0]));
    let z = 0.5 * (phi([0, 0, 1]) - phi([0, 0, -1]));
    let xx = phi([1, 0, 0]) - 2.0 * p + phi([-1, 0, 0]);
    let yy = phi([0, 1, 0]) - 2.0 * p + phi([0, -1, 0]);
    let zz = phi([0, 0, 1]) - 2.0 * p + phi([0, 0, -1]);
    let xy = 0.25 * (phi([1, 1, 0]) - phi([-1, 1, 0]) - phi([1, -1, 0]) + phi([-1, -1, 0]));
    let xz = 0.25 * (phi([1, 0, 1]) - phi([-1, 0, 1]) - phi([1, 0, -1]) + phi([-1, 0, -1]));
    let yz = 0.25 * (phi([0, 1, 1]) - phi([0, -1, 1]) - phi([0, 1, -1]) + phi([0, -1, -1]));
    let g = x * x + y * y + z * z;
    let denominator = (g * g * g).sqrt();
    if denominator < 1e-9 {
        return KnownValue { value: 0.0, known: 1.0 };
    }
    let k = ((xx * (y * y + z * z) + yy * (x * x + z * z) + zz * (x * x + y * y) - 2.0 * xy * x * y - 2.0 * xz * x * z - 2.0 * yz * y * z)
        / denominator)
        / h;
    KnownValue { value: k.clamp(-1.0 / h, 1.0 / h), known: 1.0 }
}

/// `node.extend_lattice` for one cell.
pub(super) fn extend_lattice(grid: &Grid, values: &[KnownValue], c: [u32; 3]) -> KnownValue {
    let own = values[grid.index(c)];
    if own.known > 0.0 || grid.on_border(c) {
        return own;
    }
    let (mut sum, mut hits, mut reached) = (0.0f32, 0.0f32, false);
    for face in 0..6 {
        let n = grid.face(c, face).expect("an inner cell has six face neighbours");
        let other = values[grid.index(n)];
        let border = grid.on_border(n);
        reached |= other.known > 0.0 && !border;
        if other.known > 0.0 || border {
            sum += other.value;
            hits += 1.0;
        }
    }
    if reached { KnownValue { value: sum / hits, known: 1.0 } } else { own }
}

/// FLIP's extrapolation as FLIP runs it (gridutils.h, extrapolateGridWithObserver):
/// border cells settled from the start, `layers` passes over a status grid.
/// The whole-grid statement `extend_lattice` must match pass by pass.
pub(super) fn flip_extrapolate(grid: &Grid, values: &[f32], valid: &[bool], layers: usize) -> Vec<f32> {
    const UNKNOWN: u8 = 0;
    const WAITING: u8 = 1;
    const KNOWN: u8 = 2;
    const DONE: u8 = 3;
    let mut out = values.to_vec();
    let mut status: Vec<u8> = (0..grid.total())
        .map(|i| if grid.on_border(grid.coords(i)) { DONE } else if valid[i] { KNOWN } else { UNKNOWN })
        .collect();
    for layer in 0..layers {
        let mut waiting = Vec::new();
        for i in 0..grid.total() {
            if status[i] != KNOWN {
                continue;
            }
            let c = grid.coords(i);
            for face in [1, 0, 3, 2, 5, 4] {
                let n = grid.index(grid.face(c, face).expect("a known cell is off the border"));
                if status[n] == UNKNOWN {
                    status[n] = WAITING;
                    waiting.push(n);
                }
            }
            status[i] = DONE;
        }
        for &i in &waiting {
            let c = grid.coords(i);
            let (mut sum, mut count) = (0.0f32, 0.0f32);
            for face in [1, 0, 3, 2, 5, 4] {
                let n = grid.index(grid.face(c, face).expect("a waiting cell is off the border"));
                if status[n] == DONE {
                    sum += out[n];
                    count += 1.0;
                }
            }
            out[i] = sum / count;
        }
        if layer != layers - 1 {
            for &i in &waiting {
                status[i] = KNOWN;
            }
        }
    }
    out
}

/// A small xorshift for fixtures.
pub(super) struct Rng(pub u64);

impl Rng {
    pub fn unit(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three `extend_lattice` passes are FLIP's three-layer extrapolation,
    /// border cells included as settled contributors.
    #[test]
    fn extend_lattice_passes_match_flip_extrapolation() {
        let grid = Grid::new([10, 9, 8]);
        let mut rng = Rng(0x00e4_7e4d);
        let mut values = Vec::with_capacity(grid.total());
        for i in 0..grid.total() {
            let c = grid.coords(i);
            let known = !grid.on_border(c) && rng.unit() < 0.15;
            values.push(KnownValue { value: rng.unit() * 4.0 - 2.0, known: if known { 1.0 } else { 0.0 } });
        }
        let valid: Vec<bool> = values.iter().map(|v| v.known > 0.0).collect();
        let raw: Vec<f32> = values.iter().map(|v| v.value).collect();
        let flip = flip_extrapolate(&grid, &raw, &valid, 3);
        let mut ours = values.clone();
        for _ in 0..3 {
            ours = (0..grid.total()).map(|i| extend_lattice(&grid, &ours, grid.coords(i))).collect();
        }
        let filled = ours.iter().zip(&values).filter(|(o, v)| o.known > 0.0 && v.known == 0.0).count();
        assert!(filled > 100, "the fixture extends into {filled} cells");
        for (i, (o, f)) in ours.iter().zip(&flip).enumerate() {
            assert!((o.value - f).abs() <= 1e-5, "cell {:?}: ours {} FLIP {f}", grid.coords(i), o.value);
        }
    }
}
