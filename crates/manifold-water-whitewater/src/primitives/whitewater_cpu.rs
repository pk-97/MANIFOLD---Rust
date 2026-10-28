//! CPU statements of the whitewater grid atoms' contracts, line for line
//! with their WGSL bodies, for the GPU value proofs and the O1 tests. A grid
//! here is a dense box of cells; what the pooled atoms read outside the box
//! (a tile the table does not hold, or cells of a held tile the box leaves
//! out) is the canonical element, so each statement takes that element where
//! a neighbour leaves the box.
//!
//! Ported from FLIP Fluids diffuseparticlesimulation.cpp, particlelevelset.cpp and gridutils.h (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender); see THIRD_PARTY_NOTICES.md.

// The GPU proofs (`whitewater_grid_tests`, feature gpu-proofs) call every
// item here; a default test build compiles only the extrapolation check.

#[cfg(all(test, feature = "gpu-proofs"))]
use manifold_water_liquid::fluid_particles::FaceSample;
#[cfg(all(test, feature = "gpu-proofs"))]
use manifold_water_liquid::whitewater::{CELL_AIR, CELL_LIQUID, CELL_SOLID};
use manifold_water_liquid::whitewater::KnownValue;

/// The canonical liquid distance for cells of `h` metres: air, three cells out.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(super) fn canonical_distance(h: f32) -> f32 {
    3.0 * h
}

/// The canonical surface distance, five cells out.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(super) fn canonical_surface(h: f32) -> f32 {
    5.0 * h
}

/// The canonical solid distance: far from every wall.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(super) const CANONICAL_SOLID: f32 = 1e30;

/// A whitewater grid over a solid lattice of `nodes`.
#[derive(Clone, Copy, Debug)]
pub(super) struct Grid {
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub nodes: [u32; 3],
    pub cells: [u32; 3],
}

impl Grid {
    pub fn new(nodes: [u32; 3]) -> Self {
        Self {
            #[cfg(all(test, feature = "gpu-proofs"))]
            nodes,
            cells: nodes.map(|n| n - 1),
        }
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
        (0..3)
            .all(|a| n[a] >= 0 && n[a] < i64::from(self.cells[a]))
            .then(|| n.map(|v| v as u32))
    }

    /// The solid at cell `c`'s eight corners, bit 0 x, bit 1 y, bit 2 z.
    #[cfg(all(test, feature = "gpu-proofs"))]
    pub fn corners(&self, solid: &[f32], c: [u32; 3]) -> [f32; 8] {
        let [nx, ny, _] = self.nodes.map(|n| n as usize);
        std::array::from_fn(|corner| {
            let n: [usize; 3] =
                std::array::from_fn(|a| (c[a] + ((corner >> a) & 1) as u32) as usize);
            solid[n[0] + nx * (n[1] + ny * n[2])]
        })
    }

    #[cfg(all(test, feature = "gpu-proofs"))]
    pub fn centre(c: [u32; 3]) -> [f32; 3] {
        c.map(|v| v as f32 + 0.5)
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
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

/// `node.liquid_cells` for one cell. A face neighbour outside the grid is the
/// canonical element, air.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(super) fn liquid_cell(grid: &Grid, distance: &[f32], solid: &[f32], c: [u32; 3]) -> u32 {
    let own = cell_kind(grid, distance, solid, c);
    if own != CELL_LIQUID {
        return own;
    }
    let bordering_air = (0..6).any(|face| {
        grid.face(c, face)
            .is_none_or(|n| cell_kind(grid, distance, solid, n) == CELL_AIR)
    });
    if bordering_air { CELL_AIR } else { CELL_LIQUID }
}

/// `node.lattice_curvature` for one cell. A cell outside the grid has the
/// canonical surface distance, outside the band.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(super) fn lattice_curvature(grid: &Grid, distance: &[f32], h: f32, c: [u32; 3]) -> KnownValue {
    let unknown = KnownValue {
        value: 0.0,
        known: 0.0,
    };
    let phi = |d: [i64; 3]| {
        let n: [i64; 3] = std::array::from_fn(|a| i64::from(c[a]) + d[a]);
        if (0..3).all(|a| (0..i64::from(grid.cells[a])).contains(&n[a])) {
            distance[grid.index(n.map(|v| v as u32))]
        } else {
            canonical_surface(h)
        }
    };
    // False for NaN too, as the body's `abs(v) < band` is.
    let near = |v: f32| v.abs() < 2.0 * h;
    let p = phi([0, 0, 0]);
    let faces = [
        [1, 0, 0],
        [-1, 0, 0],
        [0, 1, 0],
        [0, -1, 0],
        [0, 0, 1],
        [0, 0, -1],
    ];
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
        return KnownValue {
            value: 0.0,
            known: 1.0,
        };
    }
    let k = ((xx * (y * y + z * z) + yy * (x * x + z * z) + zz * (x * x + y * y)
        - 2.0 * xy * x * y
        - 2.0 * xz * x * z
        - 2.0 * yz * y * z)
        / denominator)
        / h;
    KnownValue {
        value: k.clamp(-1.0 / h, 1.0 / h),
        known: 1.0,
    }
}

/// `node.extend_lattice` for one cell. A face neighbour outside the grid is
/// the canonical element, unknown.
pub(super) fn extend_lattice(grid: &Grid, values: &[KnownValue], c: [u32; 3]) -> KnownValue {
    let own = values[grid.index(c)];
    if own.known > 0.0 {
        return own;
    }
    let (mut sum, mut hits) = (0.0f32, 0.0f32);
    for face in 0..6 {
        let Some(n) = grid.face(c, face) else {
            continue;
        };
        let other = values[grid.index(n)];
        if other.known > 0.0 {
            sum += other.value;
            hits += 1.0;
        }
    }
    if hits > 0.0 {
        KnownValue {
            value: sum / hits,
            known: 1.0,
        }
    } else {
        own
    }
}

/// `node.turbulence_field` for one cell. `faces` holds the pooled face
/// record of each cell of the grid's nodes box (cell `n` of the cells box is
/// record `n`, its low faces); a record outside the box is the canonical one,
/// no weight and so no velocity.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(super) fn turbulence_field(
    grid: &Grid,
    distance: &[f32],
    faces: &[FaceSample],
    h: f32,
    c: [u32; 3],
) -> f32 {
    if distance[grid.index(c)] >= 0.0 {
        return 0.0;
    }
    let [nx, ny, _] = grid.nodes.map(|n| n as usize);
    let face = |p: [i64; 3], axis: usize| {
        if !(0..3).all(|a| (0..i64::from(grid.nodes[a])).contains(&p[a])) {
            return 0.0;
        }
        let s = faces[p[0] as usize + nx * (p[1] as usize + ny * p[2] as usize)];
        if s.weight[axis] > 0.0 {
            s.velocity[axis]
        } else {
            0.0
        }
    };
    // The cell-centre velocity: the mean of each axis's low and high face.
    let velocity = |p: [i64; 3]| -> [f32; 3] {
        std::array::from_fn(|a| {
            let mut next = p;
            next[a] += 1;
            0.5 * (face(p, a) + face(next, a))
        })
    };
    let here: [i64; 3] = c.map(i64::from);
    let vi = velocity(here);
    let mut total = 0.0f32;
    for z in here[2] - 2..here[2] + 2 {
        for y in here[1] - 2..here[1] + 2 {
            for x in here[0] - 2..here[0] + 2 {
                let n = [x, y, z];
                let vn = velocity(n);
                let dv: [f32; 3] = std::array::from_fn(|a| vi[a] - vn[a]);
                let speed = (dv[0] * dv[0] + dv[1] * dv[1] + dv[2] * dv[2]).sqrt();
                if speed < 1e-5 {
                    continue;
                }
                let delta: [f32; 3] = std::array::from_fn(|a| (here[a] - n[a]) as f32 * h);
                let r = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
                let along: f32 = (0..3).map(|a| dv[a] / speed * (delta[a] / r)).sum();
                total += speed * (1.0 - along) * (1.0 - r / (12.0f32.sqrt() * h));
            }
        }
    }
    total
}

/// FLIP's extrapolation as FLIP runs it (gridutils.h, extrapolateGridWithObserver):
/// border cells settled from the start, `layers` passes over a status grid.
/// The whole-grid statement `extend_lattice` must match pass by pass.
pub(super) fn flip_extrapolate(
    grid: &Grid,
    values: &[f32],
    valid: &[bool],
    layers: usize,
) -> Vec<f32> {
    const UNKNOWN: u8 = 0;
    const WAITING: u8 = 1;
    const KNOWN: u8 = 2;
    const DONE: u8 = 3;
    let mut out = values.to_vec();
    let mut status: Vec<u8> = (0..grid.total())
        .map(|i| {
            if grid.on_border(grid.coords(i)) {
                DONE
            } else if valid[i] {
                KNOWN
            } else {
                UNKNOWN
            }
        })
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
                let n = grid.index(
                    grid.face(c, face)
                        .expect("a waiting cell is off the border"),
                );
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

    /// Three `extend_lattice` passes are FLIP's three-layer extrapolation
    /// wherever FLIP's settled border cells stay out of it: the known cells
    /// sit at least five cells from every side, so three layers never reach
    /// a cell next to the border.
    #[test]
    fn extend_lattice_passes_match_flip_extrapolation() {
        let grid = Grid::new([16, 15, 14]);
        let mut rng = Rng(0x00e4_7e4d);
        let mut values = Vec::with_capacity(grid.total());
        for i in 0..grid.total() {
            let c = grid.coords(i);
            let deep = (0..3).all(|a| c[a] >= 5 && c[a] + 6 <= grid.cells[a]);
            let known = deep && rng.unit() < 0.4;
            values.push(KnownValue {
                value: rng.unit() * 4.0 - 2.0,
                known: if known { 1.0 } else { 0.0 },
            });
        }
        let valid: Vec<bool> = values.iter().map(|v| v.known > 0.0).collect();
        let raw: Vec<f32> = values.iter().map(|v| v.value).collect();
        let flip = flip_extrapolate(&grid, &raw, &valid, 3);
        let mut ours = values.clone();
        for _ in 0..3 {
            ours = (0..grid.total())
                .map(|i| extend_lattice(&grid, &ours, grid.coords(i)))
                .collect();
        }
        let filled = ours
            .iter()
            .zip(&values)
            .filter(|(o, v)| o.known > 0.0 && v.known == 0.0)
            .count();
        assert!(filled > 100, "the fixture extends into {filled} cells");
        for (i, (o, f)) in ours.iter().zip(&flip).enumerate() {
            assert!(
                (o.value - f).abs() <= 1e-5,
                "cell {:?}: ours {} FLIP {f}",
                grid.coords(i),
                o.value
            );
        }
    }
}
