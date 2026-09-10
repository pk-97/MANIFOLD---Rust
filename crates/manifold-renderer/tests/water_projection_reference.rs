//! Small, deterministic CPU reference for the MAC pressure projection.
//!
//! This is a test-only, cell-centred free-surface discretization with fixed
//! solid walls. `phi = dt * pressure / density` is a velocity potential,
//! not pressure in pascals. It does not implement particle transfers,
//! moving boundaries, or the production water solver.

const N: usize = 16;
const H: f64 = 1.0 / N as f64;
const G: f64 = -9.81 / 120.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cell {
    Solid,
    Air,
    Fluid,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Face {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) z: f64,
}

fn at(i: usize, j: usize, k: usize) -> usize {
    (k * N + j) * N + i
}
pub(crate) fn cell(i: usize, j: usize, k: usize) -> Cell {
    if i == 0 || j == 0 || k == 0 || i + 1 == N || j + 1 == N || k + 1 == N {
        Cell::Solid
    } else if (1..=7).contains(&j) {
        Cell::Fluid
    } else {
        Cell::Air
    }
}
fn delta() -> [(isize, isize, isize); 6] {
    [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ]
}

pub(crate) fn fixture() -> Vec<Face> {
    let mut v = vec![Face::default(); N * N * N];
    for k in 0..N {
        for j in 0..N {
            for i in 0..N {
                if cell(i, j, k) != Cell::Solid {
                    let lower = if j > 0 {
                        cell(i, j - 1, k)
                    } else {
                        Cell::Solid
                    };
                    let upper = cell(i, j, k);
                    if lower != Cell::Solid
                        && upper != Cell::Solid
                        && (lower == Cell::Fluid || upper == Cell::Fluid)
                    {
                        v[at(i, j, k)].y = G;
                    }
                }
            }
        }
    }
    v
}

fn divergence(v: &[Face], i: usize, j: usize, k: usize) -> f64 {
    (v[at(i + 1, j, k)].x - v[at(i, j, k)].x + v[at(i, j + 1, k)].y - v[at(i, j, k)].y
        + v[at(i, j, k + 1)].z
        - v[at(i, j, k)].z)
        / H
}

pub(crate) fn project(mut v: Vec<Face>) -> (Vec<Face>, Vec<f64>) {
    let mut phi = vec![0.0; v.len()];
    let omega = 1.7;
    for _ in 0..64 {
        for color in 0..2 {
            for k in 1..N - 1 {
                for j in 1..N - 1 {
                    for i in 1..N - 1 {
                        if cell(i, j, k) != Cell::Fluid || (i + j + k) % 2 != color {
                            continue;
                        }
                        let mut sum = 0.0;
                        let mut count = 0.0;
                        for (di, dj, dk) in delta() {
                            let (a, b, c) = (
                                (i as isize + di) as usize,
                                (j as isize + dj) as usize,
                                (k as isize + dk) as usize,
                            );
                            match cell(a, b, c) {
                                Cell::Solid => {}
                                Cell::Fluid => {
                                    sum += phi[at(a, b, c)];
                                    count += 1.0;
                                }
                                Cell::Air => {
                                    count += 1.0;
                                }
                            }
                        }
                        if count > 0.0 {
                            let target = (sum - H * H * divergence(&v, i, j, k)) / count;
                            phi[at(i, j, k)] += omega * (target - phi[at(i, j, k)]);
                        }
                    }
                }
            }
        }
    }
    for k in 0..N {
        for j in 0..N {
            for i in 0..N {
                let p = at(i, j, k);
                if cell(i, j, k) == Cell::Solid {
                    v[p] = Face::default();
                    continue;
                }
                let pressure = |a: isize, b: isize, c: isize| -> f64 {
                    if a < 0
                        || b < 0
                        || c < 0
                        || a >= N as isize
                        || b >= N as isize
                        || c >= N as isize
                    {
                        0.0
                    } else if cell(a as usize, b as usize, c as usize) == Cell::Fluid {
                        phi[at(a as usize, b as usize, c as usize)]
                    } else {
                        0.0
                    }
                };
                let (ii, jj, kk) = (i as isize, j as isize, k as isize);
                if i > 0 && (cell(i - 1, j, k) == Cell::Solid || cell(i, j, k) == Cell::Solid) {
                    v[p].x = 0.0
                } else {
                    v[p].x -= (pressure(ii, jj, kk) - pressure(ii - 1, jj, kk)) / H;
                }
                if j > 0 && (cell(i, j - 1, k) == Cell::Solid || cell(i, j, k) == Cell::Solid) {
                    v[p].y = 0.0
                } else {
                    v[p].y -= (pressure(ii, jj, kk) - pressure(ii, jj - 1, kk)) / H;
                }
                if k > 0 && (cell(i, j, k - 1) == Cell::Solid || cell(i, j, k) == Cell::Solid) {
                    v[p].z = 0.0
                } else {
                    v[p].z -= (pressure(ii, jj, kk) - pressure(ii, jj, kk - 1)) / H;
                }
            }
        }
    }
    (v, phi)
}

pub(crate) fn rms(v: &[Face]) -> f64 {
    let mut sum = 0.0;
    let mut count = 0.0;
    for k in 1..N - 1 {
        for j in 1..8 {
            for i in 1..N - 1 {
                let d = divergence(v, i, j, k);
                sum += d * d;
                count += 1.0;
            }
        }
    }
    (sum / count).sqrt()
}

#[test]
fn pressure_projection_reference_is_bounded_and_finite() {
    let input = fixture();
    let before = rms(&input);
    let (output, phi) = project(input);
    let after = rms(&output);
    println!("before divergence RMS={before:.9e}, after={after:.9e}");
    assert!(
        after <= before * 0.01,
        "projection RMS ratio {}",
        after / before
    );
    assert!(
        output
            .iter()
            .all(|f| f.x.is_finite() && f.y.is_finite() && f.z.is_finite())
    );
    assert!(output.iter().enumerate().all(|(p, f)| {
        let i = p % N;
        let j = (p / N) % N;
        let k = p / (N * N);
        let wall_x = (i > 0 && cell(i - 1, j, k) == Cell::Solid) || cell(i, j, k) == Cell::Solid;
        let wall_y = (j > 0 && cell(i, j - 1, k) == Cell::Solid) || cell(i, j, k) == Cell::Solid;
        let wall_z = (k > 0 && cell(i, j, k - 1) == Cell::Solid) || cell(i, j, k) == Cell::Solid;
        (!wall_x || f.x.abs() <= 1e-10)
            && (!wall_y || f.y.abs() <= 1e-10)
            && (!wall_z || f.z.abs() <= 1e-10)
    }));
    let mut max_profile_error: f64 = 0.0;
    for j in 1..=7 {
        let expected = (-G) * H * (8 - j) as f64;
        max_profile_error =
            max_profile_error.max((phi[at(8, j, 8)] - expected).abs() / expected.abs());
        assert!(phi[at(8, j, 8)] > 0.0);
        assert!((phi[at(8, j, 8)] - expected).abs() <= expected.abs() * 0.01);
    }
    println!("max hydrostatic profile relative error={max_profile_error:.9e}");
}

#[test]
fn zero_input_stays_zero() {
    let (v, p) = project(vec![Face::default(); N * N * N]);
    assert!(v.iter().all(|f| f.x == 0.0 && f.y == 0.0 && f.z == 0.0));
    assert!(p.iter().all(|x| *x == 0.0));
}
