//! f64 CPU reference of one water substep, the oracle the GPU matter atoms
//! are proven against (`docs/GPU_MPM_SOLVER_DESIGN.md` section 4.1 (One
//! substep) and section 12 (Invariants and enforcement)). Built for unit
//! tests and the `gpu-proofs` binary only.
//! Accumulation is continuous, or with `fixed_point` rounds every grid word
//! exactly as the GPU does (D5: scales, hashed unbiased rounding).
// Row/column indices mirror the section 4.1 formulas term for term.
#![allow(clippy::needless_range_loop)]

use super::{MatterPoint, VELOCITY_CLAMP_CFL};
use crate::water::liquid::lattice::{LiquidLattice, PADDING_NODES};

/// A point's stencil: base node, per-axis weights, per-axis fraction.
pub type Stencil = ([i64; 3], [[f64; 3]; 3], [f64; 3]);

#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub dt: f64,
    pub gravity: [f64; 3],
    pub lambda: f64,
    pub cohesion: f64,
    pub density: f64,
    pub liveliness: f64,
    /// Closed faces: −X, +X, −Y, +Y, −Z, +Z.
    pub closed: [bool; 6],
    /// Round every grid contribution to the GPU's fixed point (D5),
    /// isolating f32-versus-f64 error from quantization error.
    pub fixed_point: bool,
    /// Tick and substep that key D5's rounding hash.
    pub tick_index: u32,
    pub substep_in_tick: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: [f64; 3],
    pub v: [f64; 3],
    /// Affine velocity C, row-major.
    pub c: [[f64; 3]; 3],
    pub j: f64,
    pub v0: f64,
    pub id: u32,
}

impl From<&MatterPoint> for Point {
    fn from(p: &MatterPoint) -> Self {
        let row = |r: [f32; 4]| [f64::from(r[0]), f64::from(r[1]), f64::from(r[2])];
        Self {
            x: p.position.map(f64::from),
            v: p.velocity.map(f64::from),
            c: [row(p.affine_x), row(p.affine_y), row(p.affine_z)],
            j: f64::from(p.volume_ratio),
            v0: f64::from(p.affine_y[3]),
            id: p.id,
        }
    }
}

pub struct Grid {
    pub mass: Vec<f64>,
    pub momentum: Vec<[f64; 3]>,
    pub velocity: Vec<[f64; 3]>,
    pub velocity_before: Vec<[f64; 3]>,
    pub clamped: Vec<bool>,
}

/// Quadratic B-spline stencil along one axis: the base node and its three
/// weights (section 4.1).
pub fn stencil(q: f64) -> (i64, [f64; 3], f64) {
    let base = (q - 0.5).floor();
    let f = q - base;
    let w = [
        0.5 * (1.5 - f) * (1.5 - f),
        0.75 - (f - 1.0) * (f - 1.0),
        0.5 * (f - 0.5) * (f - 0.5),
    ];
    (base as i64, w, f)
}

fn node_index(lat: &LiquidLattice, i: [i64; 3]) -> usize {
    let n = lat.nodes().map(|v| v as i64);
    ((i[2] * n[1] + i[1]) * n[0] + i[0]) as usize
}

/// Stencil base of a point, or `None` when its 3×3×3 stencil leaves the
/// lattice.
pub fn base_of(lat: &LiquidLattice, x: [f64; 3]) -> Option<Stencil> {
    let mut base = [0i64; 3];
    let mut w = [[0.0; 3]; 3];
    let mut f = [0.0; 3];
    for d in 0..3 {
        let q = (x[d] - f64::from(lat.min()[d])) / f64::from(lat.cell_size());
        let (b, wd, fd) = stencil(q);
        if b < 0 || b + 2 > i64::from(lat.nodes()[d]) - 1 {
            return None;
        }
        base[d] = b;
        w[d] = wd;
        f[d] = fd;
    }
    Some((base, w, f))
}

/// Kirchhoff pressure of the water branch (D3): full for compression,
/// scaled by Cohesion for tension.
pub fn water_stress(p: &Params, j: f64) -> f64 {
    let tau = p.lambda * j * (j - 1.0);
    if j >= 1.0 { tau * p.cohesion } else { tau }
}

/// Clear, P2G, grid update and G2P over `points` (section 4.1 steps 1, 3, 4
/// and 6). Points leaving the lattice are removed (id 0), as the GPU does.
pub fn substep(points: &mut [Point], lat: &LiquidLattice, p: &Params) -> Grid {
    let count = lat.node_count() as usize;
    let dx = f64::from(lat.cell_size());
    let inv_dx = 1.0 / dx;
    let mut grid = Grid {
        mass: vec![0.0; count],
        momentum: vec![[0.0; 3]; count],
        velocity: vec![[0.0; 3]; count],
        velocity_before: vec![[0.0; 3]; count],
        clamped: vec![false; count],
    };

    let m_unit = f64::from(super::mass_unit(lat.cell_size()));
    let to_mass = f64::from(super::MASS_SCALE) / m_unit;
    let to_momentum =
        f64::from(super::MOMENTUM_SCALE) / m_unit / f64::from(super::momentum_unit(lat.cell_size(), p.dt));
    let quantize = |value: f64, scale: f64, key: u32, slot: usize| {
        if p.fixed_point {
            super::encode_fixed(value * scale, key, slot as u32) as f64 / scale
        } else {
            value
        }
    };
    for pt in points.iter().filter(|pt| pt.id != 0) {
        let key = super::rounding_point_key(pt.id, p.tick_index, p.substep_in_tick);
        let Some((base, w, f)) = base_of(lat, pt.x) else { continue };
        let mass = pt.v0 * p.density;
        let stress = p.dt * pt.v0 * 4.0 * inv_dx * inv_dx * water_stress(p, pt.j);
        let mut affine = [[0.0; 3]; 3];
        for r in 0..3 {
            for c in 0..3 {
                affine[r][c] = mass * pt.c[r][c] - if r == c { stress } else { 0.0 };
            }
        }
        for a in 0..3 {
            for b in 0..3 {
                for cc in 0..3 {
                    let weight = w[0][a] * w[1][b] * w[2][cc];
                    let d = [
                        (a as f64 - f[0]) * dx,
                        (b as f64 - f[1]) * dx,
                        (cc as f64 - f[2]) * dx,
                    ];
                    let idx = node_index(lat, [base[0] + a as i64, base[1] + b as i64, base[2] + cc as i64]);
                    grid.mass[idx] += quantize(weight * mass, to_mass, key, idx * 4 + 3);
                    for r in 0..3 {
                        let ad = affine[r][0] * d[0] + affine[r][1] * d[1] + affine[r][2] * d[2];
                        grid.momentum[idx][r] +=
                            quantize(weight * (mass * pt.v[r] + ad), to_momentum, key, idx * 4 + r);
                    }
                }
            }
        }
    }

    let limit = f64::from(VELOCITY_CLAMP_CFL) * dx / p.dt;
    let n = lat.nodes().map(|v| v as i64);
    let pad = i64::from(PADDING_NODES);
    for k in 0..n[2] {
        for jj in 0..n[1] {
            for i in 0..n[0] {
                let idx = node_index(lat, [i, jj, k]);
                let m = grid.mass[idx];
                if m <= 0.0 {
                    continue;
                }
                let vb = grid.momentum[idx].map(|mo| mo / m);
                let mut v: [f64; 3] = std::array::from_fn(|d| vb[d] + p.dt * p.gravity[d]);
                let coord = [i, jj, k];
                // The face node (index `pad`) and the padding beyond it.
                for d in 0..3 {
                    if p.closed[2 * d] && coord[d] <= pad && v[d] < 0.0 {
                        v[d] = 0.0;
                    }
                    if p.closed[2 * d + 1] && coord[d] >= n[d] - pad - 1 && v[d] > 0.0 {
                        v[d] = 0.0;
                    }
                }
                let mut clamped = false;
                for vd in &mut v {
                    if vd.abs() > limit {
                        *vd = vd.clamp(-limit, limit);
                        clamped = true;
                    }
                }
                grid.velocity[idx] = v;
                grid.velocity_before[idx] = vb;
                grid.clamped[idx] = clamped;
            }
        }
    }

    for pt in points.iter_mut().filter(|pt| pt.id != 0) {
        let Some((base, w, f)) = base_of(lat, pt.x) else {
            pt.id = 0;
            continue;
        };
        let mut v_pic = [0.0; 3];
        let mut flip_delta = [0.0; 3];
        let mut b_mat = [[0.0; 3]; 3];
        for a in 0..3 {
            for b in 0..3 {
                for cc in 0..3 {
                    let weight = w[0][a] * w[1][b] * w[2][cc];
                    let d = [
                        (a as f64 - f[0]) * dx,
                        (b as f64 - f[1]) * dx,
                        (cc as f64 - f[2]) * dx,
                    ];
                    let idx = node_index(lat, [base[0] + a as i64, base[1] + b as i64, base[2] + cc as i64]);
                    let vi = grid.velocity[idx];
                    let vbi = grid.velocity_before[idx];
                    for r in 0..3 {
                        v_pic[r] += weight * vi[r];
                        flip_delta[r] += weight * (vi[r] - vbi[r]);
                        for c in 0..3 {
                            b_mat[r][c] += weight * vi[r] * d[c];
                        }
                    }
                }
            }
        }
        let beta = p.liveliness;
        let mut trace = 0.0;
        for r in 0..3 {
            for c in 0..3 {
                pt.c[r][c] = 4.0 * inv_dx * inv_dx * b_mat[r][c];
            }
            trace += pt.c[r][r];
            pt.v[r] = beta * (pt.v[r] + flip_delta[r]) + (1.0 - beta) * v_pic[r];
            pt.x[r] += p.dt * v_pic[r];
        }
        pt.j = (pt.j * (1.0 + p.dt * trace)).min(f64::from(super::j_max(p.cohesion as f32)));
        if base_of(lat, pt.x).is_none() {
            pt.id = 0;
            pt.v = [0.0; 3];
        }
    }
    grid
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lattice() -> LiquidLattice {
        let layout = crate::water::fluid::domain_layout(None, 1.0, 16).unwrap();
        LiquidLattice::from_layout(&layout)
    }

    /// A 4×4×4-cell blob of 8 points per cell at the domain centre, moving
    /// with a uniform velocity plus a shear, at rest volume.
    fn blob(lat: &LiquidLattice) -> Vec<Point> {
        let dx = f64::from(lat.cell_size());
        let v0 = dx * dx * dx / 8.0;
        let mut points = Vec::new();
        let centre: [f64; 3] = std::array::from_fn(|d| {
            f64::from(lat.min()[d]) + 0.5 * f64::from(lat.nodes()[d] - 1) * dx
        });
        let mut id = 1;
        for i in 0..8 {
            for j in 0..8 {
                for k in 0..8 {
                    let x = [
                        centre[0] + (i as f64 - 3.5) * 0.5 * dx + 0.013 * dx * (j as f64),
                        centre[1] + (j as f64 - 3.5) * 0.5 * dx,
                        centre[2] + (k as f64 - 3.5) * 0.5 * dx,
                    ];
                    points.push(Point {
                        x,
                        v: [0.4, -0.2 + 0.05 * x[0], 0.1],
                        c: [[0.0; 3]; 3],
                        j: 1.0,
                        v0,
                        id,
                    });
                    id += 1;
                }
            }
        }
        points
    }

    fn params() -> Params {
        Params {
            dt: 1.0e-4,
            gravity: [0.0; 3],
            lambda: water_lambda_for_tests(),
            cohesion: 0.0,
            density: 1000.0,
            liveliness: 0.0,
            closed: [true; 6],
            fixed_point: false,
            tick_index: 0,
            substep_in_tick: 0,
        }
    }

    fn water_lambda_for_tests() -> f64 {
        super::super::water_lambda(1.0, 1.0)
    }

    fn momentum(points: &[Point], density: f64) -> [f64; 3] {
        let mut m = [0.0; 3];
        for p in points.iter().filter(|p| p.id != 0) {
            for d in 0..3 {
                m[d] += p.v0 * density * p.v[d];
            }
        }
        m
    }

    #[test]
    fn matter_reference_grid_mass_is_particle_mass() {
        let lat = lattice();
        let mut points = blob(&lat);
        let p = params();
        let total: f64 = points.iter().map(|pt| pt.v0 * p.density).sum();
        let grid = substep(&mut points, &lat, &p);
        let grid_mass: f64 = grid.mass.iter().sum();
        assert!((grid_mass - total).abs() <= 1e-12 * total, "{grid_mass} vs {total}");
    }

    #[test]
    fn matter_reference_conserves_momentum_in_free_flight() {
        let lat = lattice();
        let mut points = blob(&lat);
        let p = params();
        let before = momentum(&points, p.density);
        for _ in 0..20 {
            substep(&mut points, &lat, &p);
        }
        let after = momentum(&points, p.density);
        for d in 0..3 {
            assert!((after[d] - before[d]).abs() < 1e-9, "axis {d}: {} vs {}", after[d], before[d]);
        }
    }

    /// The GPU's fixed point at the Dam Break's cell size and substep
    /// (dx = 1/16 m, n = 34): a translating blob keeps its momentum within
    /// 1e-4 over 300 substeps, as it does without rounding.
    #[test]
    fn matter_reference_fixed_point_free_flight_momentum() {
        let lat = lattice();
        let velocity = [1.0, 0.5, -0.25];
        let run = |fixed_point: bool| {
            let mut points = blob(&lat);
            for pt in &mut points {
                pt.v = velocity;
            }
            let p = Params { dt: 1.0 / (60.0 * 34.0), fixed_point, ..params() };
            let before = momentum(&points, p.density);
            for step in 0..300u32 {
                substep(&mut points, &lat, &Params { tick_index: step / 34, substep_in_tick: step % 34, ..p });
            }
            let after = momentum(&points, p.density);
            let j = points.iter().map(|pt| pt.j).fold(1.0f64, f64::max);
            (std::array::from_fn::<f64, 3, _>(|d| (after[d] - before[d]) / before[d]), j)
        };
        let (exact, exact_j) = run(false);
        let (fixed, fixed_j) = run(true);
        eprintln!("relative momentum change: f64 {exact:?} (max J {exact_j}), fixed point {fixed:?} (max J {fixed_j})");
        assert!(exact.iter().all(|c| c.abs() < 1e-9), "{exact:?}");
        assert!(fixed.iter().all(|c| c.abs() < 1e-4), "{fixed:?}");
    }

    #[test]
    fn matter_reference_free_fall_gains_gravity() {
        let lat = lattice();
        let mut points = blob(&lat);
        for pt in &mut points {
            pt.v = [0.0; 3];
        }
        let p = Params { gravity: [0.0, -9.81, 0.0], ..params() };
        substep(&mut points, &lat, &p);
        for pt in &points {
            assert!((pt.v[1] + 9.81 * p.dt).abs() < 1e-12, "{:?}", pt.v);
            assert!(pt.v[0].abs() < 1e-12 && pt.v[2].abs() < 1e-12);
        }
    }
}
