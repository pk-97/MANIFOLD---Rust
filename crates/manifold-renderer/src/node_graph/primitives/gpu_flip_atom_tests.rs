//! GPU value proofs for the GPU FLIP pressure solve's atoms
//! (docs/GPU_FLIP_PRESSURE_SOLVE.md) against CPU f64 references, and the
//! fused-vs-unfused proofs of the ones that can fuse. Every lattice here is
//! a few hundred cells, sized exactly; each atom's run() refuses arrays
//! shorter than its lattice before dispatch.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuBuffer, GpuTextureFormat};
use serde_json::json;

use super::coarse_inverse::CoarseInverse;
use super::coarsen_water::CoarsenWater;
use super::combine_rows::CombineRows;
use super::divide_by_value::DivideByValue;
use super::dot_products::DotProducts;
use super::liquid_surface_tests::{Harness, params, read};
use super::pressure_residual::PressureResidual;
use super::pressure_smooth::PressureSmooth;
use super::prolong_lattice::ProlongLattice;
use super::restrict_lattice::RestrictLattice;
use super::zero_lattice::ZeroLattice;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::backend::Backend;
use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
use crate::node_graph::effect_node::{EffectNodeContext, FrameTime, ParamValues};
use crate::node_graph::primitive::Primitive;
use crate::node_graph::substeps::test_nodes::register_substep_test_nodes;
use crate::node_graph::{
    EffectGraphDefExt, Executor, MetalBackend, PrimitiveRegistry, StateStore, compile, pre_allocate_resources,
};

pub(super) fn random_values(count: usize, seed: u64) -> Vec<f32> {
    let mut state = seed | 1;
    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        })
        .collect()
}

/// About seven cells in ten are water.
fn random_water(cells: usize, seed: u64) -> Vec<f32> {
    random_values(cells, seed).iter().map(|&v| f32::from(u8::from(v > -0.2))).collect()
}

fn lattice_params(nodes: [usize; 3], extra: &[(&'static str, f32)]) -> ParamValues {
    let mut all = vec![("nodes_x", nodes[0] as f32), ("nodes_y", nodes[1] as f32), ("nodes_z", nodes[2] as f32)];
    all.extend_from_slice(extra);
    params(&all)
}

/// One atom's `run()` with several array ports, into an open encoder.
fn step_ports<P: Primitive>(
    prim: &mut P,
    gpu: &mut GpuEncoder<'_>,
    backend: &dyn Backend,
    errors: &mut Vec<String>,
    inputs: &[(&'static str, Slot)],
    outputs: &[(&'static str, Slot)],
    step_params: &ParamValues,
) {
    let generations = [0_u64; 64];
    let (mut scalars, mut camera, mut light, mut material, mut transform) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let (mut atmosphere, mut render_mode, mut object) = (Vec::new(), Vec::new(), Vec::new());
    let node_inputs = NodeInputs::new(inputs, backend, &generations);
    let node_outputs = NodeOutputs::new(
        outputs,
        backend,
        &mut scalars,
        &mut camera,
        &mut light,
        &mut material,
        &mut transform,
        &mut atmosphere,
        &mut render_mode,
        &mut object,
    );
    let time = FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(1.0 / 60.0), frame_count: 0 };
    let mut ctx = EffectNodeContext::new(time, step_params, node_inputs, node_outputs, Some(gpu)).with_errors(errors);
    Primitive::run(prim, &mut ctx);
}

/// Run one atom on fresh arrays and read its output back.
fn run_atom<P: Primitive>(prim: &mut P, inputs: &[(&'static str, &[f32])], output_len: usize, step_params: &ParamValues) -> Vec<f32> {
    let mut harness = Harness::new();
    let slots: Vec<(&'static str, (Slot, GpuBuffer))> =
        inputs.iter().map(|&(name, values)| (name, harness.array(values, values.len().max(1)))).collect();
    let output = harness.array::<f32>(&[], output_len);
    let ports: Vec<(&'static str, Slot)> = slots.iter().map(|(name, (slot, _))| (*name, *slot)).collect();
    let mut errors = Vec::new();
    let mut native = harness.device.create_encoder("gpu flip atom");
    {
        let mut gpu = GpuEncoder::new(&mut native, &harness.device);
        let backend: &dyn Backend = &harness.backend;
        step_ports(prim, &mut gpu, backend, &mut errors, &ports, &[("out", output.0)], step_params);
    }
    native.commit_and_wait_completed();
    assert!(errors.is_empty(), "{errors:?}");
    read(&output.1, output_len)
}

fn assert_close(actual: &[f32], expected: &[f64], what: &str) {
    assert_eq!(actual.len(), expected.len(), "{what}: length");
    let scale = expected.iter().fold(1.0_f64, |m, v| m.max(v.abs()));
    for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
        assert!((f64::from(*a) - e).abs() <= 1e-5 * scale, "{what}[{i}]: {a} vs {e}");
    }
}

// ── CPU references ─────────────────────────────────────────────────────────

fn coords(c: usize, n: [usize; 3]) -> [usize; 3] {
    [c % n[0], (c / n[0]) % n[1], c / (n[0] * n[1])]
}

fn cell(p: [usize; 3], n: [usize; 3]) -> usize {
    p[0] + n[0] * (p[1] + n[1] * p[2])
}

/// In-box neighbours of `c`.
fn neighbours(c: usize, n: [usize; 3]) -> Vec<usize> {
    let p = coords(c, n);
    let mut out = Vec::with_capacity(6);
    for a in 0..3 {
        if p[a] > 0 {
            let mut q = p;
            q[a] -= 1;
            out.push(cell(q, n));
        }
        if p[a] + 1 < n[a] {
            let mut q = p;
            q[a] += 1;
            out.push(cell(q, n));
        }
    }
    out
}

fn cpu_sweep(water: &[f32], rhs: &[f32], value: &[f64], n: [usize; 3], h: f64, color: usize) -> Vec<f64> {
    (0..value.len())
        .map(|c| {
            let p = coords(c, n);
            if water[c] <= 0.5 || (p[0] + p[1] + p[2]) % 2 != color {
                return value[c];
            }
            let around = neighbours(c, n);
            let sum: f64 = around.iter().filter(|&&q| water[q] > 0.5).map(|&q| value[q]).sum();
            (sum - h * h * f64::from(rhs[c])) / around.len() as f64
        })
        .collect()
}

fn cpu_residual(water: &[f32], rhs: &[f32], value: &[f32], n: [usize; 3], h: f64) -> Vec<f64> {
    (0..value.len())
        .map(|c| {
            if water[c] <= 0.5 {
                return 0.0;
            }
            let own = f64::from(value[c]);
            let lap: f64 = neighbours(c, n)
                .iter()
                .map(|&q| if water[q] > 0.5 { f64::from(value[q]) - own } else { -own })
                .sum();
            f64::from(rhs[c]) - lap / (h * h)
        })
        .collect()
}

/// Distances as the ghost rows can see them: water cells from −0.6h to 0.2h
/// (some above the −0.005h the rows take at most), air cells from −0.3h to
/// 2.9h (some below the 0 the rows take at least).
pub(super) fn random_phi(water: &[f32], h: f64, seed: u64) -> Vec<f32> {
    random_values(water.len(), seed)
        .iter()
        .zip(water)
        .map(|(&u, &w)| {
            let t = f64::from(u) + 0.5;
            let cells = if w > 0.5 { -0.6 + 0.8 * t } else { -0.3 + 3.2 * t };
            (cells * h) as f32
        })
        .collect()
}

/// An air neighbour's ghost ratio, clamp(φ_air / (φ_water + eps), ±25), φ_water
/// taken at most −0.005h and φ_air at least 0: the engine's theta
/// (pressuresolver.cpp).
fn theta(air: f32, water: f32, h: f64, eps: f64) -> f64 {
    (f64::from(air).max(0.0) / (f64::from(water).min(-0.005 * h) + eps)).clamp(-25.0, 25.0)
}

/// Water cell `c`'s diagonal in node.pressure_smooth's ghost-fluid rows: 1
/// per neighbour inside the box, less θ per air neighbour, floored at 0.
fn cpu_diag(c: usize, water: &[f32], phi: &[f32], n: [usize; 3], h: f64) -> f64 {
    let diag: f64 =
        neighbours(c, n).iter().map(|&q| if water[q] > 0.5 { 1.0 } else { 1.0 - theta(phi[q], phi[c], h, 1e-9) }).sum();
    diag.max(0.0)
}

fn cpu_ghost_sweep(water: &[f32], rhs: &[f32], value: &[f64], phi: &[f32], n: [usize; 3], h: f64, color: usize) -> Vec<f64> {
    (0..value.len())
        .map(|c| {
            let p = coords(c, n);
            let diag = cpu_diag(c, water, phi, n, h);
            if water[c] <= 0.5 || (p[0] + p[1] + p[2]) % 2 != color || diag == 0.0 {
                return value[c];
            }
            let sum: f64 = neighbours(c, n).iter().filter(|&&q| water[q] > 0.5).map(|&q| value[q]).sum();
            (sum - h * h * f64::from(rhs[c])) / diag
        })
        .collect()
}

fn cpu_ghost_residual(water: &[f32], rhs: &[f32], value: &[f32], phi: &[f32], n: [usize; 3], h: f64) -> Vec<f64> {
    (0..value.len())
        .map(|c| {
            if water[c] <= 0.5 {
                return 0.0;
            }
            let sum: f64 = neighbours(c, n).iter().filter(|&&q| water[q] > 0.5).map(|&q| f64::from(value[q])).sum();
            f64::from(rhs[c]) - (sum - cpu_diag(c, water, phi, n, h) * f64::from(value[c])) / (h * h)
        })
        .collect()
}

/// Share fine cell `f` takes from coarse cell `c` along one axis.
fn weight(f: usize, c: usize, coarse: usize) -> f64 {
    let parent = f / 2;
    let other = if f % 2 == 1 { (parent + 1).min(coarse - 1) } else { parent.saturating_sub(1) };
    (if parent == c { 0.75 } else { 0.0 }) + if other == c { 0.25 } else { 0.0 }
}

fn cpu_restrict(fine: &[f32], water: &[f32], n: [usize; 3]) -> Vec<f64> {
    let m = n.map(|v| 2 * v);
    (0..water.len())
        .map(|c| {
            if water[c] <= 0.5 {
                return 0.0;
            }
            let p = coords(c, n);
            let sum: f64 = fine
                .iter()
                .enumerate()
                .map(|(f, &v)| {
                    let q = coords(f, m);
                    (0..3).map(|a| weight(q[a], p[a], n[a])).product::<f64>() * f64::from(v)
                })
                .sum();
            sum / 8.0
        })
        .collect()
}

fn cpu_prolong(value: &[f32], coarse: &[f32], water: &[f32], m: [usize; 3]) -> Vec<f64> {
    let n = m.map(|v| v / 2);
    (0..value.len())
        .map(|f| {
            let own = f64::from(value[f]);
            if water[f] <= 0.5 {
                return own;
            }
            let q = coords(f, m);
            let add: f64 = (0..coarse.len())
                .map(|c| {
                    let p = coords(c, n);
                    (0..3).map(|a| weight(q[a], p[a], n[a])).product::<f64>() * f64::from(coarse[c])
                })
                .sum();
            own + add
        })
        .collect()
}

fn cpu_coarsen(fine: &[f32], n: [usize; 3]) -> Vec<f64> {
    let m = n.map(|v| 2 * v);
    (0..n.iter().product::<usize>())
        .map(|c| {
            let p = coords(c, n);
            let all = (0..8).all(|k| {
                let q = [2 * p[0] + (k & 1), 2 * p[1] + ((k >> 1) & 1), 2 * p[2] + ((k >> 2) & 1)];
                fine[cell(q, m)] > 0.5
            });
            f64::from(u8::from(all))
        })
        .collect()
}

/// The masked Poisson matrix A (L = −A / h²) and its inverse by the
/// kernel's symmetric sweep, in f64: a pivot under 1e-4 of its diagonal pins
/// its cell. Returns (A, A⁻¹, pinned cells).
fn cpu_inverse(water: &[f32], n: [usize; 3]) -> (Vec<f64>, Vec<f64>, Vec<usize>) {
    let cells = water.len();
    let mut a = vec![0.0; cells * cells];
    for i in (0..cells).filter(|&i| water[i] > 0.5) {
        let around = neighbours(i, n);
        a[i * cells + i] = around.len() as f64;
        for j in around.into_iter().filter(|&j| water[j] > 0.5) {
            a[i * cells + j] = -1.0;
        }
    }
    let mut m = a.clone();
    let mut pinned = Vec::new();
    for k in 0..cells {
        let d = m[k * cells + k];
        if d.is_nan() || d <= 1e-4 * neighbours(k, n).len() as f64 {
            if water[k] > 0.5 {
                pinned.push(k);
            }
            for e in 0..cells {
                m[k * cells + e] = 0.0;
                m[e * cells + k] = 0.0;
            }
            continue;
        }
        let before = m.clone();
        for i in (0..cells).filter(|&i| i != k) {
            for j in (0..cells).filter(|&j| j != k) {
                m[i * cells + j] = before[i * cells + j] - before[i * cells + k] * before[k * cells + j] / d;
            }
        }
        for e in (0..cells).filter(|&e| e != k) {
            m[k * cells + e] = before[k * cells + e] / d;
            m[e * cells + k] = before[e * cells + k] / d;
        }
        m[k * cells + k] = -1.0 / d;
    }
    (a, m.iter().map(|v| -v).collect(), pinned)
}

// ── Atom value proofs ──────────────────────────────────────────────────────

const FINE: [usize; 3] = [6, 4, 8];
const COARSE: [usize; 3] = [3, 2, 4];

fn run_sweep(water: &[f32], rhs: &[f32], value: &[f32], phi: &[f32], n: [usize; 3], h: f64, color: usize) -> Vec<f32> {
    run_atom(
        &mut PressureSmooth::new(),
        &[("water", water), ("rhs", rhs), ("value", value), ("phi", phi)],
        value.len(),
        &lattice_params(n, &[("cell_size", h as f32), ("color", color as f32)]),
    )
}

fn run_residual(water: &[f32], rhs: &[f32], value: &[f32], phi: &[f32], n: [usize; 3], h: f64) -> Vec<f32> {
    run_atom(
        &mut PressureResidual::new(),
        &[("water", water), ("rhs", rhs), ("value", value), ("phi", phi)],
        value.len(),
        &lattice_params(n, &[("cell_size", h as f32)]),
    )
}

/// Zero distances give the plain Dirichlet rows the coarse levels use, bit for
/// bit in the sweep; real distances give the ghost-fluid rows.
#[test]
fn gpu_flip_smooth_sweeps_each_color() {
    let cells: usize = FINE.iter().product();
    let (water, rhs, value) = (random_water(cells, 0x5e1), random_values(cells, 0x5e2), random_values(cells, 0x5e3));
    let h = 0.3;
    let value64: Vec<f64> = value.iter().map(|&v| f64::from(v)).collect();
    let (zeros, phi) = (vec![0.0; cells], random_phi(&water, h, 0x5e4));
    for color in [0, 1] {
        let plain = run_sweep(&water, &rhs, &value, &zeros, FINE, h, color);
        assert_close(&plain, &cpu_sweep(&water, &rhs, &value64, FINE, h, color), &format!("plain sweep color {color}"));
        let ghost = run_sweep(&water, &rhs, &value, &phi, FINE, h, color);
        assert_close(&ghost, &cpu_ghost_sweep(&water, &rhs, &value64, &phi, FINE, h, color), &format!("ghost sweep color {color}"));
    }
}

#[test]
fn gpu_flip_residual_is_rhs_minus_the_masked_laplacian() {
    let cells: usize = FINE.iter().product();
    let (water, rhs, value) = (random_water(cells, 0x7e1), random_values(cells, 0x7e2), random_values(cells, 0x7e3));
    let h = 0.3;
    let plain = run_residual(&water, &rhs, &value, &vec![0.0; cells], FINE, h);
    assert_close(&plain, &cpu_residual(&water, &rhs, &value, FINE, h), "plain residual");
    let phi = random_phi(&water, h, 0x7e4);
    let ghost = run_residual(&water, &rhs, &value, &phi, FINE, h);
    assert_close(&ghost, &cpu_ghost_residual(&water, &rhs, &value, &phi, FINE, h), "ghost residual");
}

/// A lone water cell whose air neighbours all read φ = −h, inside the liquid
/// by the engine's level set: the air floor takes their φ at 0, so θ is 0
/// and the row is the plain one (diagonal 6). Without the floor θ would be 2
/// and the diagonal −6. A lone cell with no neighbours has diagonal 0: the
/// sweep keeps its value and the residual is the rhs.
#[test]
fn gpu_flip_ghost_rows_floor_the_air_and_the_diagonal() {
    let h = 0.3;
    let n = [3, 3, 3];
    let centre = cell([1, 1, 1], n);
    let mut water = vec![0.0; 27];
    water[centre] = 1.0;
    let mut phi = vec![-h as f32; 27];
    phi[centre] = (-0.5 * h) as f32;
    let (rhs, value) = (random_values(27, 0xf10), random_values(27, 0xf11));
    assert_eq!(cpu_diag(centre, &water, &phi, n, h), 6.0, "air inside the level set adds nothing");
    let swept = run_sweep(&water, &rhs, &value, &phi, n, h, 1);
    let want = -h * h * f64::from(rhs[centre]) / 6.0;
    assert!((f64::from(swept[centre]) - want).abs() <= 1e-6, "the plain row: {} vs {want}", swept[centre]);
    let (one, lone) = ([1, 1, 1], vec![1.0f32]);
    let phi = vec![(-0.5 * h) as f32];
    assert_eq!(cpu_diag(0, &lone, &phi, one, h), 0.0, "no neighbours, no diagonal");
    let swept = run_sweep(&lone, &rhs[..1], &value[..1], &phi, one, h, 0);
    assert_eq!(swept[0], value[0], "a zero diagonal keeps the value");
    let residual = run_residual(&lone, &rhs[..1], &value[..1], &phi, one, h);
    assert_eq!(residual[0], rhs[0], "a zero diagonal leaves the rhs");
}

#[test]
fn gpu_flip_restrict_is_the_prolongation_transpose() {
    let (fine_cells, coarse_cells) = (FINE.iter().product(), COARSE.iter().product());
    let (fine, water) = (random_values(fine_cells, 0x4e1), random_water(coarse_cells, 0x4e2));
    let got = run_atom(&mut RestrictLattice::new(), &[("fine", &fine), ("water", &water)], coarse_cells, &lattice_params(COARSE, &[]));
    assert_close(&got, &cpu_restrict(&fine, &water, COARSE), "restrict");
}

#[test]
fn gpu_flip_prolong_adds_the_trilinear_correction() {
    let (fine_cells, coarse_cells) = (FINE.iter().product(), COARSE.iter().product());
    let (value, coarse, water) = (random_values(fine_cells, 0x9e1), random_values(coarse_cells, 0x9e2), random_water(fine_cells, 0x9e3));
    let got = run_atom(
        &mut ProlongLattice::new(),
        &[("value", &value), ("coarse", &coarse), ("water", &water)],
        fine_cells,
        &lattice_params(FINE, &[]),
    );
    assert_close(&got, &cpu_prolong(&value, &coarse, &water, FINE), "prolong");
}

#[test]
fn gpu_flip_coarsen_water_needs_every_child() {
    let fine_cells: usize = FINE.iter().product();
    // Mostly water, so some coarse cells are all-water.
    let fine: Vec<f32> = random_values(fine_cells, 0xce1).iter().map(|&v| f32::from(u8::from(v > -0.45))).collect();
    let got = run_atom(&mut CoarsenWater::new(), &[("fine", &fine)], COARSE.iter().product(), &lattice_params(COARSE, &[]));
    let want = cpu_coarsen(&fine, COARSE);
    assert!(want.contains(&1.0) && want.contains(&0.0), "the fixture has both kinds");
    assert_close(&got, &want, "coarsen");
}

#[test]
fn gpu_flip_zero_lattice_is_zero() {
    let cells: usize = FINE.iter().product();
    let got = run_atom(&mut ZeroLattice::new(), &[], cells, &lattice_params(FINE, &[]));
    assert!(got.iter().all(|&v| v == 0.0));
}

/// The coarsest level's inverse against the f64 sweep: random water, a deep
/// pool (air only in the top row, the slowest to converge by sweeps), an odd
/// lattice, and a box all water, where one cell is pinned. The GPU result is
/// exactly symmetric, matches f64 to f32 precision, and is the inverse:
/// A·M is the identity on the unpinned water and zero elsewhere.
#[test]
fn gpu_flip_coarse_inverse_matches_cpu() {
    let n4 = [4, 4, 4];
    let deep: Vec<f32> = (0..64).map(|c| f32::from(u8::from(coords(c, n4)[1] < 3))).collect();
    let cases: [(&str, [usize; 3], Vec<f32>); 4] = [
        ("random 4³", n4, random_water(64, 0xc5)),
        ("deep pool 4³", n4, deep),
        ("random 5×4×3", [5, 4, 3], random_water(60, 0xc6)),
        ("all water 4³", n4, vec![1.0; 64]),
    ];
    for (name, n, water) in cases {
        let cells: usize = n.iter().product();
        let got = run_atom(&mut CoarseInverse::new(), &[("water", &water)], cells * cells, &lattice_params(n, &[]));
        let (a, want, pinned) = cpu_inverse(&water, n);
        let all_water = water.iter().all(|&w| w > 0.5);
        assert_eq!(pinned.len(), usize::from(all_water), "{name}: pinned {pinned:?}");
        for i in 0..cells {
            for j in 0..cells {
                assert_eq!(got[i * cells + j].to_bits(), got[j * cells + i].to_bits(), "{name}: not symmetric at {i}, {j}");
            }
        }
        let scale = want.iter().fold(1.0_f64, |m, v| m.max(v.abs()));
        let worst = got.iter().zip(&want).map(|(g, w)| (f64::from(*g) - w).abs()).fold(0.0, f64::max);
        assert!(worst <= 1e-4 * scale, "{name}: worst {worst:.3e} of {scale:.3e}");
        let solved = |i: usize| water[i] > 0.5 && !pinned.contains(&i);
        for i in 0..cells {
            for j in 0..cells {
                if !solved(j) {
                    assert_eq!(got[i * cells + j], 0.0, "{name}: column {j} is air or pinned");
                }
                if solved(i) {
                    let product: f64 = (0..cells).map(|k| a[i * cells + k] * f64::from(got[k * cells + j])).sum();
                    let expected = f64::from(u8::from(i == j));
                    assert!((product - expected).abs() <= 1e-3, "{name}: (A·M)[{i}][{j}] = {product:.3e}");
                }
            }
        }
    }
}

#[test]
fn gpu_flip_dot_products_match_cpu() {
    let (len, rows) = (3000usize, 5usize);
    let matrix = random_values(len * rows, 0xd07);
    let vector = random_values(len, 0x7ec);
    let dot = |r: usize, v: &[f32]| (0..len).map(|e| f64::from(matrix[r * len + e]) * f64::from(v[e])).sum::<f64>();
    let got = run_atom(
        &mut DotProducts::new(),
        &[("matrix", &matrix), ("vector", &vector)],
        6,
        &params(&[("row_length", len as f32), ("rows", 4.0), ("max_rows", 6.0)]),
    );
    let want: Vec<f64> = (0..6).map(|r| if r < 4 { dot(r, &vector) } else { 0.0 }).collect();
    assert_close(&got, &want, "dots");
    // No vector: each row's plain sum.
    let sums = run_atom(&mut DotProducts::new(), &[("matrix", &matrix)], 2, &params(&[("row_length", len as f32), ("rows", 2.0), ("max_rows", 2.0)]));
    let ones = vec![1.0_f32; len];
    assert_close(&sums, &[dot(0, &ones), dot(1, &ones)], "sums");
    // A length: the vector against itself, square-rooted.
    let row0 = matrix[..len].to_vec();
    let length = run_atom(
        &mut DotProducts::new(),
        &[("matrix", &row0), ("vector", &row0)],
        1,
        &params(&[("row_length", len as f32), ("rows", 1.0), ("max_rows", 1.0), ("root", 1.0)]),
    );
    assert_close(&length, &[dot(0, &row0).sqrt()], "length");
}

#[test]
fn gpu_flip_combine_rows_match_cpu() {
    let (len, rows) = (1000usize, 4usize);
    let base = random_values(len, 0xba5e);
    let matrix = random_values(len * rows, 0x3a7);
    let coef = random_values(rows, 0xc0ef);
    let got = run_atom(
        &mut CombineRows::new(),
        &[("base", &base), ("matrix", &matrix), ("coef", &coef)],
        len,
        &params(&[("row_length", len as f32), ("rows", 3.0), ("scale", -1.0), ("base_scale", 0.5)]),
    );
    let want: Vec<f64> = (0..len)
        .map(|e| 0.5 * f64::from(base[e]) - (0..3).map(|i| f64::from(coef[i]) * f64::from(matrix[i * len + e])).sum::<f64>())
        .collect();
    assert_close(&got, &want, "combine");
}

#[test]
fn gpu_flip_divide_by_value_matches_cpu_and_guards_zero() {
    let values = random_values(700, 0xd1f0);
    let got = run_atom(&mut DivideByValue::new(), &[("values", &values), ("divisor", &[0.25])], 700, &params(&[]));
    let want: Vec<f64> = values.iter().map(|&v| f64::from(v) / 0.25).collect();
    assert_close(&got, &want, "divide");
    let zero = run_atom(&mut DivideByValue::new(), &[("values", &values), ("divisor", &[0.0])], 700, &params(&[]));
    assert!(zero.iter().all(|&v| v == 0.0), "a zero divisor must give zeros");
}

// ── Fused vs unfused ───────────────────────────────────────────────────────

/// A small graph of test sources, atoms and one sink, run once fused and
/// once unfused; each run's sink input is read back.
struct Chain {
    nodes: Vec<serde_json::Value>,
    wires: Vec<serde_json::Value>,
    sources: Vec<(&'static str, Vec<f32>)>,
}

impl Chain {
    fn new() -> Self {
        Self { nodes: Vec::new(), wires: Vec::new(), sources: Vec::new() }
    }

    fn node(&mut self, name: &str, type_id: &str, params: serde_json::Value) -> usize {
        let id = self.nodes.len();
        self.nodes.push(json!({"id": id, "typeId": type_id, "nodeId": name, "params": params}));
        id
    }

    fn source(&mut self, name: &'static str, values: Vec<f32>) -> usize {
        let items = values.len();
        self.typed_source(name, "test.value_source", values, items)
    }

    /// A source of `items` elements of any type, given as their raw words.
    fn typed_source(&mut self, name: &'static str, type_id: &str, values: Vec<f32>, items: usize) -> usize {
        let id = self.node(name, type_id, json!({"max_capacity": {"type": "Int", "value": items}}));
        self.sources.push((name, values));
        id
    }

    fn wire(&mut self, from: usize, from_port: &str, to: usize, to_port: &str) {
        self.wires.push(json!({"fromNode": from, "fromPort": from_port, "toNode": to, "toPort": to_port}));
    }

    fn def(&self, into: usize) -> EffectGraphDef {
        let mut nodes = self.nodes.clone();
        let mut wires = self.wires.clone();
        let sink = nodes.len();
        nodes.push(json!({"id": sink, "typeId": "test.value_sink", "nodeId": "sink", "params": {}}));
        nodes.push(json!({"id": sink + 1, "typeId": "system.final_output", "nodeId": "output", "params": {}}));
        wires.push(json!({"fromNode": into, "fromPort": "out", "toNode": sink, "toPort": "values"}));
        wires.push(json!({"fromNode": sink, "fromPort": "out", "toNode": sink + 1, "toPort": "in"}));
        serde_json::from_value(json!({"version": 3, "nodes": nodes, "wires": wires})).expect("chain def")
    }

    /// The sink's input after one frame, `len` values; `fused` regions in the
    /// graph that ran.
    fn run(&self, def: &EffectGraphDef, len: usize) -> (Vec<f32>, usize) {
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        let mut graph = def.clone().into_graph(&registry, &Default::default()).expect("chain builds");
        // The host fills the sources and reads the sink's input outside the frame.
        let sink = super::gpu_flip_solve_tests::node_named(&graph, "sink");
        let (into_node, into_port) = graph.wires_into(sink).map(|w| w.from).next().expect("the sink is wired");
        for name in self.sources.iter().map(|(name, _)| name) {
            graph.add_external_output(super::gpu_flip_solve_tests::node_named(&graph, name), "out").expect("a source port");
        }
        graph.add_external_output(into_node, into_port).expect("the sink's producer port");
        let plan = compile(&graph).expect("chain compiles");
        let fused = graph.nodes().filter(|node| node.node.type_id().as_str() == "node.wgsl_compute").count();
        let device = crate::test_device();
        let mut backend = MetalBackend::new(device.arc(), 8, 8, GpuTextureFormat::Rgba16Float);
        pre_allocate_resources(&graph, &plan, &device, &mut backend).expect("pre-allocate");
        let output = |backend: &MetalBackend, name: &str, port: &str| {
            let node = super::gpu_flip_solve_tests::node_named(&graph, name);
            let slot = backend.slot_for(super::gpu_flip_solve_tests::output_of(&plan, node, port)).expect("bound");
            Backend::array_buffer(backend, slot).expect("buffer").clone()
        };
        for (name, values) in &self.sources {
            let buffer = output(&backend, name, "out");
            assert!(buffer.size as usize >= values.len() * 4, "{name} holds its values");
            // SAFETY: a shared buffer at least this long; nothing runs yet.
            unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
        }
        let step = plan.steps().iter().find(|s| s.node == sink).expect("sink compiled");
        let input = step.inputs.iter().find(|(name, _)| *name == "values").map(|&(_, r)| r).expect("sink input");
        let mut exec = Executor::new(Box::new(backend));
        let mut enc = device.create_encoder("gpu flip chain");
        {
            let mut gpu = GpuEncoder::new(&mut enc, &device);
            let time = FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(1.0 / 60.0), frame_count: 0 };
            exec.execute_frame_with_state(&mut graph, &plan, time, &mut gpu, &mut StateStore::new(), 0);
        }
        enc.commit_and_wait_completed();
        let buffer = exec.host_array_buffer(&graph, &plan, input).expect("the sink's input keeps its own storage");
        assert!(buffer.size as usize >= len * 4, "the sink reads {} bytes, not {len} values ({fused} fused regions)", buffer.size);
        (read(buffer, len), fused)
    }

    /// Fused and unfused give the same values bit for bit, and the fused graph
    /// ran one fused kernel.
    fn fused_matches_unfused(&self, into: usize, len: usize) -> Vec<f32> {
        let def = self.def(into);
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        let Some(fused_def) = crate::node_graph::freeze::install::fuse_canonical_def(&def, &registry).map(|fused| fused.def) else {
            let report = crate::node_graph::fusion_report(&def, &registry);
            let cuts: Vec<String> =
                report.nodes.iter().map(|n| format!("{} {}: {:?}", n.type_id, n.kind, n.cut_reason)).collect();
            panic!("the chain does not fuse: {cuts:#?}");
        };
        let (unfused, none) = self.run(&def, len);
        let (fused, regions) = self.run(&fused_def, len);
        assert_eq!((none, regions), (0, 1), "one fused region");
        let differ = unfused.iter().zip(&fused).filter(|(a, b)| a.to_bits() != b.to_bits()).count();
        assert_eq!(differ, 0, "fused differs from unfused in {differ} of {len}");
        fused
    }
}

fn lattice_json(nodes: [usize; 3], extra: &[(&str, f64)]) -> serde_json::Value {
    let mut params = json!({});
    let axes = [("nodes_x", nodes[0] as f64), ("nodes_y", nodes[1] as f64), ("nodes_z", nodes[2] as f64)];
    for &(name, value) in axes.iter().chain(extra) {
        params[name] = json!({"type": "Float", "value": value});
    }
    params
}

/// The residual fused into a sweep's rhs: the V-cycle's shape one level
/// down, on one lattice.
#[test]
fn gpu_flip_residual_into_sweep_fuses() {
    let cells: usize = FINE.iter().product();
    let h = 0.3;
    let (water, rhs, value, start) =
        (random_water(cells, 0xf1), random_values(cells, 0xf2), random_values(cells, 0xf3), random_values(cells, 0xf4));
    let phi = random_phi(&water, h, 0xf5);
    let mut chain = Chain::new();
    let w = chain.source("water", water.clone());
    let r = chain.source("rhs", rhs.clone());
    let v = chain.source("value", value.clone());
    let s = chain.source("start", start.clone());
    let d = chain.source("phi", phi.clone());
    let residual = chain.node("residual", "node.pressure_residual", lattice_json(FINE, &[("cell_size", h)]));
    chain.wire(w, "out", residual, "water");
    chain.wire(r, "out", residual, "rhs");
    chain.wire(v, "out", residual, "value");
    chain.wire(d, "out", residual, "phi");
    let sweep = chain.node("sweep", "node.pressure_smooth", lattice_json(FINE, &[("cell_size", h), ("color", 1.0)]));
    chain.wire(w, "out", sweep, "water");
    chain.wire(residual, "out", sweep, "rhs");
    chain.wire(s, "out", sweep, "value");
    chain.wire(d, "out", sweep, "phi");
    let got = chain.fused_matches_unfused(sweep, cells);
    let mid: Vec<f32> = cpu_ghost_residual(&water, &rhs, &value, &phi, FINE, h).iter().map(|&v| v as f32).collect();
    let start64: Vec<f64> = start.iter().map(|&v| f64::from(v)).collect();
    assert_close(&got, &cpu_ghost_sweep(&water, &mid, &start64, &phi, FINE, h, 1), "fused residual into sweep");
}

/// A residual fused into a divide by one GPU value: the fused count follows
/// the residual, not the one-element divisor (BUG-sk62, divide_by_value
/// fused region shrinks to its divisor).
#[test]
fn gpu_flip_residual_into_divide_fuses() {
    let cells: usize = FINE.iter().product();
    let h = 0.3;
    let (water, rhs, value) = (random_water(cells, 0xd1), random_values(cells, 0xd2), random_values(cells, 0xd3));
    let mut chain = Chain::new();
    let w = chain.source("water", water.clone());
    let r = chain.source("rhs", rhs.clone());
    let v = chain.source("value", value.clone());
    let d = chain.source("divisor", vec![-2.5]);
    let phi = random_phi(&water, h, 0xd4);
    let p = chain.source("phi", phi.clone());
    let residual = chain.node("residual", "node.pressure_residual", lattice_json(FINE, &[("cell_size", h)]));
    chain.wire(w, "out", residual, "water");
    chain.wire(r, "out", residual, "rhs");
    chain.wire(v, "out", residual, "value");
    chain.wire(p, "out", residual, "phi");
    let divide = chain.node("divide", "node.divide_by_value", json!({}));
    chain.wire(residual, "out", divide, "values");
    chain.wire(d, "out", divide, "divisor");
    let got = chain.fused_matches_unfused(divide, cells);
    let want: Vec<f64> = cpu_ghost_residual(&water, &rhs, &value, &phi, FINE, h).iter().map(|&v| v / -2.5).collect();
    assert_close(&got, &want, "fused residual into divide");
}

/// The particles' distance fused into a divide: the sort bins the particles
/// on the lattice, the distance gathers its bins and the divide takes the
/// distance coincident. Checked against the engine's scatter (each particle
/// reaches the cells within 2r of it along each axis, r = √3·h/2), with 3h
/// where no live particle is within one cell.
#[test]
fn gpu_flip_particle_distance_into_divide_fuses() {
    use crate::node_graph::fluid_particles::FluidParticle;
    let cells: usize = FINE.iter().product();
    let h = 0.25_f64;
    let min = [-0.4_f64, 0.2, 0.1];
    let size: [f64; 3] = std::array::from_fn(|a| FINE[a] as f64 * h);
    let jitter = random_values(3 * 60, 0xd15);
    let particles: Vec<FluidParticle> = (0..60)
        .map(|i| {
            let position: [f32; 3] = std::array::from_fn(|a| (min[a] + (f64::from(jitter[3 * i + a]) + 0.5) * size[a]) as f32);
            let radius = if i % 13 == 4 { 0.0 } else { 0.08 };
            FluidParticle { position_radius: [position[0], position[1], position[2], radius], velocity: [0.0; 3], id: i as u32 + 1 }
        })
        .collect();
    let mut chain = Chain::new();
    let source = chain.typed_source("particles", "test.liquid_source", bytemuck::cast_slice(&particles).to_vec(), particles.len());
    let sort = chain.node(
        "sort",
        "node.sort_particles_into_cells",
        json!({
            "center_x": {"type": "Float", "value": min[0] + 0.5 * size[0]},
            "center_y": {"type": "Float", "value": min[1] + 0.5 * size[1]},
            "center_z": {"type": "Float", "value": min[2] + 0.5 * size[2]},
            "size_x": {"type": "Float", "value": size[0]},
            "size_y": {"type": "Float", "value": size[1]},
            "size_z": {"type": "Float", "value": size[2]},
            "cell_size": {"type": "Float", "value": h},
        }),
    );
    chain.wire(source, "out", sort, "particles");
    let distance = chain.node(
        "distance",
        "node.particle_distance",
        lattice_json(FINE, &[("cell_size", h), ("lattice_min_x", min[0]), ("lattice_min_y", min[1]), ("lattice_min_z", min[2])]),
    );
    chain.wire(sort, "sorted", distance, "sorted");
    chain.wire(sort, "cell_ranges", distance, "cell_ranges");
    let d = chain.source("divisor", vec![0.5]);
    let divide = chain.node("divide", "node.divide_by_value", json!({}));
    chain.wire(distance, "out", divide, "values");
    chain.wire(d, "out", divide, "divisor");
    let got = chain.fused_matches_unfused(divide, cells);
    let r = 0.5 * 3f64.sqrt() * h;
    let reaches = |q: f32, a: usize, p: usize| {
        let q = f64::from(q) - min[a];
        ((q - 2.0 * r) / h).floor() as i64 <= p as i64 && p as i64 <= ((q + 2.0 * r) / h).floor() as i64
    };
    let want: Vec<f64> = (0..cells)
        .map(|c| {
            let p = coords(c, FINE);
            let centre: [f64; 3] = std::array::from_fn(|a| min[a] + (p[a] as f64 + 0.5) * h);
            let mut phi = 3.0 * h;
            let bin = |q: f32, a: usize| ((f64::from(q) - min[a]) / h).floor() as i64;
            let live = || particles.iter().filter(|q| q.position_radius[3] > 0.0);
            let near = live().any(|q| (0..3).all(|a| (bin(q.position_radius[a], a) - p[a] as i64).abs() <= 1));
            for q in live().filter(|_| near) {
                if (0..3).all(|a| reaches(q.position_radius[a], a, p[a])) {
                    let d = (0..3).map(|a| (centre[a] - f64::from(q.position_radius[a])).powi(2)).sum::<f64>().sqrt();
                    phi = phi.min(d - r);
                }
            }
            if phi.abs() < 0.005 * h {
                phi = if phi > 0.0 { 0.005 * h } else { -0.005 * h };
            }
            phi / 0.5
        })
        .collect();
    assert!(want.iter().any(|&v| v < 0.0) && want.iter().any(|&v| v > 0.0), "the draw has water and air");
    assert_close(&got, &want, "fused distance into divide");
}

/// The coarse water fused into the restriction's mask.
#[test]
fn gpu_flip_coarsen_into_restrict_fuses() {
    let (fine_cells, coarse_cells) = (FINE.iter().product::<usize>(), COARSE.iter().product::<usize>());
    let fine_water: Vec<f32> = random_values(fine_cells, 0xa1).iter().map(|&v| f32::from(u8::from(v > -0.45))).collect();
    let fine = random_values(fine_cells, 0xa2);
    let mut chain = Chain::new();
    let w = chain.source("water", fine_water.clone());
    let f = chain.source("fine", fine.clone());
    let coarsen = chain.node("coarsen", "node.coarsen_water", lattice_json(COARSE, &[]));
    chain.wire(w, "out", coarsen, "fine");
    let restrict = chain.node("restrict", "node.restrict_lattice", lattice_json(COARSE, &[]));
    chain.wire(f, "out", restrict, "fine");
    chain.wire(coarsen, "out", restrict, "water");
    let got = chain.fused_matches_unfused(restrict, coarse_cells);
    let mask: Vec<f32> = cpu_coarsen(&fine_water, COARSE).iter().map(|&v| v as f32).collect();
    assert_close(&got, &cpu_restrict(&fine, &mask, COARSE), "fused coarsen into restrict");
}

/// A sweep into the prolongation's value, with the water read from outside,
/// stays unfused: the freeze compiler can't count a region by the shorter of
/// two coincident inputs while a third input is gathered, so it refuses
/// rather than guess (fail closed). The V-cycle never fuses there anyway:
/// every sweep's output fans out.
#[test]
fn gpu_flip_sweep_into_prolong_stays_unfused() {
    let (fine_cells, coarse_cells) = (FINE.iter().product::<usize>(), COARSE.iter().product::<usize>());
    let mut chain = Chain::new();
    let c = chain.source("coarse", random_values(coarse_cells, 0xb1));
    let w = chain.source("water", random_water(fine_cells, 0xb2));
    let r = chain.source("rhs", random_values(fine_cells, 0xb3));
    let s = chain.source("start", random_values(fine_cells, 0xb4));
    let p = chain.source("phi", vec![0.0; fine_cells]);
    let sweep = chain.node("sweep", "node.pressure_smooth", lattice_json(FINE, &[("cell_size", 0.3), ("color", 0.0)]));
    chain.wire(w, "out", sweep, "water");
    chain.wire(r, "out", sweep, "rhs");
    chain.wire(s, "out", sweep, "value");
    chain.wire(p, "out", sweep, "phi");
    let prolong = chain.node("prolong", "node.prolong_lattice", lattice_json(FINE, &[]));
    chain.wire(sweep, "out", prolong, "value");
    chain.wire(c, "out", prolong, "coarse");
    chain.wire(w, "out", prolong, "water");
    let mut registry = PrimitiveRegistry::with_builtin();
    register_substep_test_nodes(&mut registry);
    assert!(crate::node_graph::freeze::install::fuse_canonical_def(&chain.def(prolong), &registry).is_none());
}
