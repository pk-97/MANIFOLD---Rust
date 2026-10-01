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
use crate::node_graph::liquid::bodies::{LiquidBody, LiquidShape, body_pose_at, pack_distance_atlas};
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
pub(super) fn random_water(cells: usize, seed: u64) -> Vec<f32> {
    random_values(cells, seed).iter().map(|&v| f32::from(u8::from(v > -0.2))).collect()
}

pub(super) fn lattice_params(nodes: [usize; 3], extra: &[(&'static str, f32)]) -> ParamValues {
    let mut all = vec![("nodes_x", nodes[0] as f32), ("nodes_y", nodes[1] as f32), ("nodes_z", nodes[2] as f32)];
    all.extend_from_slice(extra);
    params(&all)
}

/// One atom's `run()` with several array ports, into an open encoder.
pub(super) fn step_ports<P: Primitive>(
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
pub(super) fn run_atom<P: Primitive>(prim: &mut P, inputs: &[(&'static str, &[f32])], output_len: usize, step_params: &ParamValues) -> Vec<f32> {
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

pub(super) fn assert_close(actual: &[f32], expected: &[f64], what: &str) {
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

/// In-box neighbours of `c`, each with the axis and side of the face to it.
fn neighbours(c: usize, n: [usize; 3]) -> Vec<(usize, usize, isize)> {
    let p = coords(c, n);
    let mut out = Vec::with_capacity(6);
    for a in 0..3 {
        if p[a] > 0 {
            let mut q = p;
            q[a] -= 1;
            out.push((cell(q, n), a, -1));
        }
        if p[a] + 1 < n[a] {
            let mut q = p;
            q[a] += 1;
            out.push((cell(q, n), a, 1));
        }
    }
    out
}

/// Floats per face-grid record: velocity, then open fractions.
pub(super) const FACE_FLOATS: usize = 8;

pub(super) fn face_grid_len(n: [usize; 3]) -> usize {
    n.iter().map(|v| v + 1).product::<usize>() * FACE_FLOATS
}

/// The open fraction of the face on side `d` of cell `c` along axis `a`:
/// it sits on the padded cell of the higher of the two.
fn open(faces: &[f32], c: usize, a: usize, d: isize, n: [usize; 3]) -> f64 {
    let mut p = coords(c, n);
    if d > 0 {
        p[a] += 1;
    }
    let m = n.map(|v| v + 1);
    f64::from(faces[(p[0] + m[0] * (p[1] + m[1] * p[2])) * FACE_FLOATS + 4 + a])
}

/// Every inner face whole, the walls closed: node.solid_faces with no solid.
fn open_faces(n: [usize; 3]) -> Vec<f32> {
    let m = n.map(|v| v + 1);
    let mut faces = vec![0.0; face_grid_len(n)];
    for i in 0..m.iter().product::<usize>() {
        let p = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
        for a in 0..3 {
            if (0..3).all(|b| b == a || p[b] < n[b]) && p[a] > 0 && p[a] < n[a] {
                faces[i * FACE_FLOATS + 4 + a] = 1.0;
            }
        }
    }
    faces
}

/// Inner faces one in four closed, one in four whole, the rest a fraction;
/// every face of `isolated` closed, so that cell drops out of the system.
fn random_open_faces(n: [usize; 3], seed: u64, isolated: usize) -> Vec<f32> {
    let mut faces = open_faces(n);
    let draw = random_values(faces.len(), seed);
    for (i, v) in faces.iter_mut().enumerate() {
        if *v > 0.0 {
            let r = draw[i] + 0.5;
            *v = if r < 0.25 { 0.0 } else if r < 0.5 { 1.0 } else { 0.05 + 0.95 * (r - 0.5) * 2.0 };
        }
    }
    for (_, a, d) in neighbours(isolated, n) {
        let mut p = coords(isolated, n);
        if d > 0 {
            p[a] += 1;
        }
        let m = n.map(|v| v + 1);
        faces[(p[0] + m[0] * (p[1] + m[1] * p[2])) * FACE_FLOATS + 4 + a] = 0.0;
    }
    faces
}

/// Σ w over a cell's in-box faces: the weighted operator's diagonal.
fn diagonal(faces: &[f32], c: usize, n: [usize; 3]) -> f64 {
    neighbours(c, n).iter().map(|&(_, a, d)| open(faces, c, a, d, n)).sum()
}

fn cpu_sweep(water: &[f32], rhs: &[f32], value: &[f64], faces: &[f32], n: [usize; 3], h: f64, color: usize) -> Vec<f64> {
    (0..value.len())
        .map(|c| {
            let p = coords(c, n);
            if water[c] <= 0.5 || (p[0] + p[1] + p[2]) % 2 != color {
                return value[c];
            }
            let diag = diagonal(faces, c, n);
            if diag == 0.0 {
                return 0.0;
            }
            let sum: f64 = neighbours(c, n)
                .iter()
                .filter(|&&(q, _, _)| water[q] > 0.5)
                .map(|&(q, a, d)| open(faces, c, a, d, n) * value[q])
                .sum();
            (sum - h * h * f64::from(rhs[c])) / diag
        })
        .collect()
}

fn cpu_residual(water: &[f32], rhs: &[f32], value: &[f32], faces: &[f32], n: [usize; 3], h: f64) -> Vec<f64> {
    (0..value.len())
        .map(|c| {
            if water[c] <= 0.5 || diagonal(faces, c, n) == 0.0 {
                return 0.0;
            }
            let own = f64::from(value[c]);
            let lap: f64 = neighbours(c, n)
                .iter()
                .map(|&(q, a, d)| {
                    let w = open(faces, c, a, d, n);
                    if water[q] > 0.5 { w * (f64::from(value[q]) - own) } else { -w * own }
                })
                .sum();
            f64::from(rhs[c]) - lap / (h * h)
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

/// The weighted masked Poisson matrix A (L = −A / h²) and its inverse by the
/// kernel's symmetric sweep, in f64: a pivot under 1e-4 of its diagonal
/// (Σ w over the cell's faces) pins its cell. Returns (A, A⁻¹, pinned cells).
fn cpu_inverse(water: &[f32], faces: &[f32], n: [usize; 3]) -> (Vec<f64>, Vec<f64>, Vec<usize>) {
    let cells = water.len();
    let mut a = vec![0.0; cells * cells];
    for i in (0..cells).filter(|&i| water[i] > 0.5) {
        a[i * cells + i] = diagonal(faces, i, n);
        for (j, axis, d) in neighbours(i, n).into_iter().filter(|&(j, _, _)| water[j] > 0.5) {
            a[i * cells + j] = -open(faces, i, axis, d, n);
        }
    }
    let mut m = a.clone();
    let mut pinned = Vec::new();
    for k in 0..cells {
        let d = m[k * cells + k];
        if d.is_nan() || d <= 1e-4 * diagonal(faces, k, n) {
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

#[test]
fn gpu_flip_smooth_sweeps_each_color() {
    let cells: usize = FINE.iter().product();
    let (water, rhs, value) = (random_water(cells, 0x5e1), random_values(cells, 0x5e2), random_values(cells, 0x5e3));
    let h = 0.3;
    let value64: Vec<f64> = value.iter().map(|&v| f64::from(v)).collect();
    let isolated = (0..cells).find(|&c| water[c] > 0.5 && coords(c, FINE).iter().sum::<usize>() % 2 == 1).expect("a water cell");
    for (name, faces) in [("open", open_faces(FINE)), ("solid", random_open_faces(FINE, 0x5e4, isolated))] {
        for color in [0, 1] {
            let got = run_atom(
                &mut PressureSmooth::new(),
                &[("water", &water), ("rhs", &rhs), ("value", &value), ("solid_faces", &faces)],
                cells,
                &lattice_params(FINE, &[("cell_size", h as f32), ("color", color as f32)]),
            );
            assert_close(&got, &cpu_sweep(&water, &rhs, &value64, &faces, FINE, h, color), &format!("{name} sweep color {color}"));
            if name == "solid" && color == 1 {
                assert_eq!(got[isolated], 0.0, "a water cell with every face closed is out of the system");
            }
        }
    }
}

#[test]
fn gpu_flip_residual_is_rhs_minus_the_masked_laplacian() {
    let cells: usize = FINE.iter().product();
    let (water, rhs, value) = (random_water(cells, 0x7e1), random_values(cells, 0x7e2), random_values(cells, 0x7e3));
    let h = 0.3;
    let isolated = (0..cells).find(|&c| water[c] > 0.5).expect("a water cell");
    for (name, faces) in [("open", open_faces(FINE)), ("solid", random_open_faces(FINE, 0x7e4, isolated))] {
        let got = run_atom(
            &mut PressureResidual::new(),
            &[("water", &water), ("rhs", &rhs), ("value", &value), ("solid_faces", &faces)],
            cells,
            &lattice_params(FINE, &[("cell_size", h as f32)]),
        );
        assert_close(&got, &cpu_residual(&water, &rhs, &value, &faces, FINE, h), &format!("{name} residual"));
        if name == "solid" {
            assert_eq!(got[isolated], 0.0, "a water cell with every face closed is out of the system");
        }
    }
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

/// Name, lattice, water, faces.
type InverseCase = (&'static str, [usize; 3], Vec<f32>, Vec<f32>);

/// The coarsest level's inverse against the f64 sweep: random water, a deep
/// pool (air only in the top row, the slowest to converge by sweeps), an odd
/// lattice, and a box all water, where one cell is pinned. The GPU result is
/// exactly symmetric, matches f64 to f32 precision, and is the inverse:
/// A·M is the identity on the unpinned water and zero elsewhere.
#[test]
fn gpu_flip_coarse_inverse_matches_cpu() {
    let n4 = [4, 4, 4];
    let deep: Vec<f32> = (0..64).map(|c| f32::from(u8::from(coords(c, n4)[1] < 3))).collect();
    let random = random_water(64, 0xc5);
    let wet = (0..64).find(|&c| random[c] > 0.5).expect("a water cell");
    let cases: [InverseCase; 6] = [
        ("random 4³", n4, random.clone(), open_faces(n4)),
        ("deep pool 4³", n4, deep, open_faces(n4)),
        ("random 5×4×3", [5, 4, 3], random_water(60, 0xc6), open_faces([5, 4, 3])),
        ("all water 4³", n4, vec![1.0; 64], open_faces(n4)),
        ("random 4³ with solids", n4, random, random_open_faces(n4, 0xc7, wet)),
        ("all water 4³ with solids", n4, vec![1.0; 64], random_open_faces(n4, 0xc8, 21)),
    ];
    for (name, n, water, faces) in cases {
        let cells: usize = n.iter().product();
        let got =
            run_atom(&mut CoarseInverse::new(), &[("water", &water), ("solid_faces", &faces)], cells * cells, &lattice_params(n, &[]));
        let (a, want, pinned) = cpu_inverse(&water, &faces, n);
        let all_water = water.iter().all(|&w| w > 0.5);
        if name.ends_with("with solids") {
            // Closed faces cut off pockets with no air: each pins one cell.
            let isolated = if all_water { 21 } else { wet };
            assert!(pinned.contains(&isolated), "{name}: the closed-in cell is pinned, {pinned:?}");
        } else {
            assert_eq!(pinned.len(), usize::from(all_water), "{name}: pinned {pinned:?}");
        }
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
pub(super) struct Chain {
    nodes: Vec<serde_json::Value>,
    wires: Vec<serde_json::Value>,
    pub(super) sources: Vec<(&'static str, Vec<f32>)>,
    /// The sink's type: test.value_sink, or test.face_sink for a face grid.
    pub(super) sink: &'static str,
}

impl Chain {
    pub(super) fn new() -> Self {
        Self { nodes: Vec::new(), wires: Vec::new(), sources: Vec::new(), sink: "test.value_sink" }
    }

    pub(super) fn node(&mut self, name: &str, type_id: &str, params: serde_json::Value) -> usize {
        let id = self.nodes.len();
        self.nodes.push(json!({"id": id, "typeId": type_id, "nodeId": name, "params": params}));
        id
    }

    pub(super) fn source(&mut self, name: &'static str, values: Vec<f32>) -> usize {
        let id = self.node(name, "test.value_source", json!({"max_capacity": {"type": "Int", "value": values.len()}}));
        self.sources.push((name, values));
        id
    }

    /// A face grid source: `values` is FACE_FLOATS floats per record.
    pub(super) fn face_source(&mut self, name: &'static str, values: Vec<f32>) -> usize {
        let id = self.node(name, "test.face_source", json!({"max_capacity": {"type": "Int", "value": values.len() / FACE_FLOATS}}));
        self.sources.push((name, values));
        id
    }

    pub(super) fn wire(&mut self, from: usize, from_port: &str, to: usize, to_port: &str) {
        self.wires.push(json!({"fromNode": from, "fromPort": from_port, "toNode": to, "toPort": to_port}));
    }

    fn def(&self, into: usize) -> EffectGraphDef {
        let mut nodes = self.nodes.clone();
        let mut wires = self.wires.clone();
        let sink = nodes.len();
        nodes.push(json!({"id": sink, "typeId": self.sink, "nodeId": "sink", "params": {}}));
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
    pub(super) fn fused_matches_unfused(&self, into: usize, len: usize) -> Vec<f32> {
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
        let differ: Vec<(usize, f32, f32)> =
            unfused.iter().zip(&fused).enumerate().filter(|(_, (a, b))| a.to_bits() != b.to_bits()).map(|(i, (a, b))| (i, *a, *b)).collect();
        assert!(differ.is_empty(), "fused differs from unfused in {} of {len}: {:?}", differ.len(), &differ[..differ.len().min(8)]);
        fused
    }
}

pub(super) fn lattice_json(nodes: [usize; 3], extra: &[(&str, f64)]) -> serde_json::Value {
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
    let mut chain = Chain::new();
    let w = chain.source("water", water.clone());
    let r = chain.source("rhs", rhs.clone());
    let v = chain.source("value", value.clone());
    let s = chain.source("start", start.clone());
    let faces = random_open_faces(FINE, 0xf5, 0);
    let o = chain.face_source("open", faces.clone());
    let residual = chain.node("residual", "node.pressure_residual", lattice_json(FINE, &[("cell_size", h)]));
    chain.wire(w, "out", residual, "water");
    chain.wire(r, "out", residual, "rhs");
    chain.wire(v, "out", residual, "value");
    chain.wire(o, "out", residual, "solid_faces");
    let sweep = chain.node("sweep", "node.pressure_smooth", lattice_json(FINE, &[("cell_size", h), ("color", 1.0)]));
    chain.wire(w, "out", sweep, "water");
    chain.wire(residual, "out", sweep, "rhs");
    chain.wire(s, "out", sweep, "value");
    chain.wire(o, "out", sweep, "solid_faces");
    let got = chain.fused_matches_unfused(sweep, cells);
    let mid: Vec<f32> = cpu_residual(&water, &rhs, &value, &faces, FINE, h).iter().map(|&v| v as f32).collect();
    let start64: Vec<f64> = start.iter().map(|&v| f64::from(v)).collect();
    assert_close(&got, &cpu_sweep(&water, &mid, &start64, &faces, FINE, h, 1), "fused residual into sweep");
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
    let faces = random_open_faces(FINE, 0xd4, 0);
    let o = chain.face_source("open", faces.clone());
    let residual = chain.node("residual", "node.pressure_residual", lattice_json(FINE, &[("cell_size", h)]));
    chain.wire(w, "out", residual, "water");
    chain.wire(r, "out", residual, "rhs");
    chain.wire(v, "out", residual, "value");
    chain.wire(o, "out", residual, "solid_faces");
    let divide = chain.node("divide", "node.divide_by_value", json!({}));
    chain.wire(residual, "out", divide, "values");
    chain.wire(d, "out", divide, "divisor");
    let got = chain.fused_matches_unfused(divide, cells);
    let want: Vec<f64> = cpu_residual(&water, &rhs, &value, &faces, FINE, h).iter().map(|&v| v / -2.5).collect();
    assert_close(&got, &want, "fused residual into divide");
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
    let sweep = chain.node("sweep", "node.pressure_smooth", lattice_json(FINE, &[("cell_size", 0.3), ("color", 0.0)]));
    chain.wire(w, "out", sweep, "water");
    chain.wire(r, "out", sweep, "rhs");
    chain.wire(s, "out", sweep, "value");
    let o = chain.face_source("open", open_faces(FINE));
    chain.wire(o, "out", sweep, "solid_faces");
    let prolong = chain.node("prolong", "node.prolong_lattice", lattice_json(FINE, &[]));
    chain.wire(sweep, "out", prolong, "value");
    chain.wire(c, "out", prolong, "coarse");
    chain.wire(w, "out", prolong, "water");
    let mut registry = PrimitiveRegistry::with_builtin();
    register_substep_test_nodes(&mut registry);
    assert!(crate::node_graph::freeze::install::fuse_canonical_def(&chain.def(prolong), &registry).is_none());
}

// ── Solid faces ────────────────────────────────────────────────────────────

/// FLIP Fluids' LevelsetUtils::fractionInside for a segment, in f64.
fn engine_segment(left: f64, right: f64) -> f64 {
    if left < 0.0 && right < 0.0 {
        1.0
    } else if left < 0.0 {
        left / (left - right)
    } else if right < 0.0 {
        right / (right - left)
    } else {
        0.0
    }
}

/// FLIP Fluids' LevelsetUtils::fractionInside for a square, in f64, with the
/// branch it took (inside corners, and for two diagonal corners the middle's
/// sign) so a test can show it reached every case.
fn engine_square(bl: f64, br: f64, tl: f64, tr: f64) -> (f64, (usize, bool)) {
    let inside = [bl, tl, br, tr].iter().filter(|&&v| v < 0.0).count();
    let mut list = [bl, br, tr, tl];
    let mut middle_inside = false;
    let fraction = match inside {
        4 => 1.0,
        3 => {
            while list[0] < 0.0 {
                list.rotate_left(1);
            }
            let side0 = 1.0 - engine_segment(list[0], list[3]);
            let side1 = 1.0 - engine_segment(list[0], list[1]);
            1.0 - 0.5 * side0 * side1
        }
        2 => {
            while list[0] >= 0.0 || !(list[1] < 0.0 || list[2] < 0.0) {
                list.rotate_left(1);
            }
            if list[1] < 0.0 {
                0.5 * (engine_segment(list[0], list[3]) + engine_segment(list[1], list[2]))
            } else if 0.25 * (list[0] + list[1] + list[2] + list[3]) < 0.0 {
                middle_inside = true;
                let side1 = 1.0 - engine_segment(list[0], list[3]);
                let side3 = 1.0 - engine_segment(list[2], list[3]);
                let side2 = 1.0 - engine_segment(list[2], list[1]);
                let side0 = 1.0 - engine_segment(list[0], list[1]);
                1.0 - (0.5 * side1 * side3 + 0.5 * side0 * side2)
            } else {
                let side0 = engine_segment(list[0], list[1]);
                let side1 = engine_segment(list[0], list[3]);
                let side2 = engine_segment(list[2], list[1]);
                let side3 = engine_segment(list[2], list[3]);
                0.5 * side0 * side1 + 0.5 * side2 * side3
            }
        }
        1 => {
            while list[0] >= 0.0 {
                list.rotate_left(1);
            }
            0.5 * engine_segment(list[0], list[3]) * engine_segment(list[0], list[1])
        }
        _ => 0.0,
    };
    let diagonal = inside == 2 && !((bl < 0.0) == (br < 0.0) || (bl < 0.0) == (tl < 0.0));
    (fraction, (inside, diagonal && middle_inside))
}

/// FLIP Fluids' LevelsetUtils::volumeFraction for a tetrahedron, in f64,
/// sorted by the engine's five-swap network.
fn engine_tet(p: [f64; 4]) -> f64 {
    let [mut a, mut b, mut c, mut d] = p;
    for (x, y) in [(0, 1), (2, 3), (0, 2), (1, 3), (1, 2)] {
        let mut v = [a, b, c, d];
        if v[x] > v[y] {
            v.swap(x, y);
        }
        [a, b, c, d] = v;
    }
    let tet = |a: f64, b: f64, c: f64, d: f64| a * a * a / ((a - b) * (a - c) * (a - d));
    if d <= 0.0 {
        1.0
    } else if c <= 0.0 {
        1.0 - tet(d, c, b, a)
    } else if b <= 0.0 {
        let (p, q, r, s) = (a / (a - c), a / (a - d), b / (b - d), b / (b - c));
        p * q * (1.0 - s) + q * (1.0 - r) * s + r * s
    } else if a <= 0.0 {
        tet(a, b, c, d)
    } else {
        0.0
    }
}

/// FLIP Fluids' MeshLevelSet::_getCellWeight: the fraction of a cell inside
/// the solid, c[i + 2j + 4k] the distance at corner (i, j, k).
fn engine_cube(c: [f64; 8]) -> f64 {
    if c.iter().all(|&v| v < 0.0) {
        return 1.0;
    }
    if c.iter().all(|&v| v >= 0.0) {
        return 0.0;
    }
    let [p000, p100, p010, p110, p001, p101, p011, p111] = c;
    (engine_tet([p000, p001, p101, p011])
        + engine_tet([p000, p101, p100, p110])
        + engine_tet([p000, p010, p011, p110])
        + engine_tet([p101, p011, p111, p110])
        + 2.0 * engine_tet([p000, p011, p101, p110])
        + engine_tet([p100, p101, p001, p111])
        + engine_tet([p100, p001, p000, p010])
        + engine_tet([p100, p110, p111, p010])
        + engine_tet([p001, p111, p011, p010])
        + 2.0 * engine_tet([p100, p111, p001, p010]))
        / 12.0
}

/// The open fraction of every face from a corner lattice, as
/// FluidSimulation::_updateWeightGridThread takes MeshLevelSet's face
/// weights: U from (i,j,k), (i,j+1,k), (i,j,k+1), (i,j+1,k+1); V from
/// (i,j,k), (i,j,k+1), (i+1,j,k), (i+1,j,k+1); W from (i,j,k), (i,j+1,k),
/// (i+1,j,k), (i+1,j+1,k). Box walls closed.
fn cpu_solid_faces(phi: &[f32], n: [usize; 3], tolerance: f64) -> (Vec<f64>, Vec<(usize, bool)>) {
    let m = n.map(|v| v + 1);
    let at = |i: usize, j: usize, k: usize| f64::from(phi[i + m[0] * (j + m[1] * k)]);
    let mut out = vec![0.0; m.iter().product::<usize>() * FACE_FLOATS];
    let mut branches = Vec::new();
    for k in 0..m[2] {
        for j in 0..m[1] {
            for i in 0..m[0] {
                let p = [i, j, k];
                if (0..3).all(|b| p[b] < n[b]) {
                    let corners: [f64; 8] = std::array::from_fn(|c| at(i + (c & 1), j + ((c >> 1) & 1), k + (c >> 2)));
                    out[(i + m[0] * (j + m[1] * k)) * FACE_FLOATS + 7] = (1.0 - engine_cube(corners)).clamp(0.0, 1.0);
                }
                for a in 0..3 {
                    if !(0..3).all(|b| b == a || p[b] < n[b]) || p[a] == 0 || p[a] == n[a] {
                        continue;
                    }
                    let corners = match a {
                        0 => [at(i, j, k), at(i, j + 1, k), at(i, j, k + 1), at(i, j + 1, k + 1)],
                        1 => [at(i, j, k), at(i, j, k + 1), at(i + 1, j, k), at(i + 1, j, k + 1)],
                        _ => [at(i, j, k), at(i, j + 1, k), at(i + 1, j, k), at(i + 1, j + 1, k)],
                    };
                    let (mut inside, branch) = engine_square(corners[0], corners[1], corners[2], corners[3]);
                    if corners.iter().all(|c| c.abs() <= tolerance) {
                        inside = 0.5;
                    }
                    branches.push(branch);
                    out[(i + m[0] * (j + m[1] * k)) * FACE_FLOATS + 4 + a] = (1.0 - inside).clamp(0.0, 1.0);
                }
            }
        }
    }
    (out, branches)
}

/// node.solid_faces against FLIP Fluids' face weights ported to f64 here:
/// random corner distances reach every fractionInside case, and one face's
/// corners all within the interface tolerance is half open.
#[test]
fn gpu_flip_solid_faces_match_the_engine() {
    let n = [5usize, 4, 3];
    let m = n.map(|v| v + 1);
    let (h, offset) = (0.25_f32, 1.5_f32);
    let mut phi: Vec<f32> = random_values(m.iter().product(), 0x50f).iter().map(|v| v * 0.6).collect();
    // The U face at padded (2, 1, 1): corners (2,1,1), (2,2,1), (2,1,2), (2,2,2).
    for (i, j, k) in [(2, 1, 1), (2, 2, 1), (2, 1, 2), (2, 2, 2)] {
        phi[i + m[0] * (j + m[1] * k)] = 1.0e-7;
    }
    let tolerance = 8.0 * f64::from(f32::EPSILON) * (f64::from(h) * 5.0 + f64::from(offset));
    let got = run_atom(
        &mut super::solid_faces::SolidFaces::new(),
        &[("solid", &phi)],
        face_grid_len(n),
        &lattice_params(n, &[("cell_size", h), ("box_offset", offset)]),
    );
    let (want, branches) = cpu_solid_faces(&phi, n, tolerance);
    for case in [(0, false), (1, false), (2, false), (2, true), (3, false), (4, false)] {
        assert!(branches.contains(&case), "the fixture reaches fractionInside case {case:?}");
    }
    assert!(branches.iter().any(|&(inside, middle)| inside == 2 && !middle), "an adjacent or outside-middle pair");
    assert_eq!(want[(2 + m[0] * (1 + m[1])) * FACE_FLOATS + 4], 0.5, "the planted face is on the interface");
    let open_volume: Vec<f64> = want.chunks(FACE_FLOATS).map(|r| r[7]).collect();
    assert!(open_volume.iter().any(|&v| v > 0.0 && v < 1.0), "a cell cut by the solid");
    assert_close(&got, &want, "solid faces");
}

fn cpu_coarsen_faces(fine: &[f32], n: [usize; 3]) -> Vec<f64> {
    let (m, f) = (n.map(|v| v + 1), n.map(|v| 2 * v + 1));
    let mut out = vec![0.0; face_grid_len(n)];
    for c in 0..m.iter().product::<usize>() {
        let p = [c % m[0], (c / m[0]) % m[1], c / (m[0] * m[1])];
        for a in 0..3 {
            if !(0..3).all(|b| b == a || p[b] < n[b]) || p[a] == 0 || p[a] == n[a] {
                continue;
            }
            let cross: Vec<usize> = (0..3).filter(|&b| b != a).collect();
            let sum: f64 = (0..4)
                .map(|k| {
                    let mut q = p.map(|v| 2 * v);
                    q[cross[0]] += k & 1;
                    q[cross[1]] += k >> 1;
                    f64::from(fine[(q[0] + f[0] * (q[1] + f[1] * q[2])) * FACE_FLOATS + 4 + a])
                })
                .sum();
            out[c * FACE_FLOATS + 4 + a] = 0.25 * sum;
        }
    }
    out
}

#[test]
fn gpu_flip_coarsen_solid_faces_is_the_mean_of_four() {
    let fine = random_open_faces(FINE, 0xcf1, 0);
    let got = run_atom(
        &mut super::coarsen_solid_faces::CoarsenSolidFaces::new(),
        &[("fine", &fine)],
        face_grid_len(COARSE),
        &lattice_params(COARSE, &[]),
    );
    let want = cpu_coarsen_faces(&fine, COARSE);
    assert!(want.iter().any(|&w| w > 0.0 && w < 1.0), "the fixture has partial coarse faces");
    assert_close(&got, &want, "coarsen solid faces");
}

/// The weights fused into their coincident reader, node.subtract_pressure,
/// with random velocities, pressure and water: returns the fused faces and
/// those inputs.
fn project_through(chain: &mut Chain, open: usize, n: [usize; 3], seed: u64) -> (Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>) {
    let cells: usize = n.iter().product();
    let faces = random_values(face_grid_len(n), seed);
    let (pressure, water) = (random_values(cells, seed + 1), random_water(cells, seed + 2));
    let f = chain.face_source("faces", faces.clone());
    let p = chain.source("pressure", pressure.clone());
    let w = chain.source("water", water.clone());
    let project = chain.node("project", "node.subtract_pressure", lattice_json(n, &[("cell_size", 0.3)]));
    chain.wire(f, "out", project, "faces");
    chain.wire(p, "out", project, "pressure");
    chain.wire(w, "out", project, "water");
    chain.wire(open, "out", project, "solid_faces");
    chain.sink = "test.face_sink";
    let got = chain.fused_matches_unfused(project, face_grid_len(n));
    (got, faces, pressure, water)
}

/// node.subtract_pressure's rule in f64, `open` the solid's face records.
fn cpu_project(faces: &[f32], open: &[f64], pressure: &[f32], water: &[f32], n: [usize; 3], h: f64) -> Vec<f64> {
    let m = n.map(|v| v + 1);
    let mut out = vec![0.0; face_grid_len(n)];
    for i in 0..m.iter().product::<usize>() {
        let p = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
        for a in 0..3 {
            if !(0..3).all(|b| b == a || p[b] < n[b]) {
                continue;
            }
            let (v, w) = (i * FACE_FLOATS + a, i * FACE_FLOATS + 4 + a);
            out[v] = f64::from(faces[v]);
            if p[a] == 0 || p[a] == n[a] {
                out[w] = 1.0;
                continue;
            }
            let mut below = p;
            below[a] -= 1;
            let (up, down) = (cell(p, n), cell(below, n));
            if open[w] <= 0.0 {
                out[w] = 1.0;
            } else if water[up] > 0.5 || water[down] > 0.5 {
                out[v] -= (f64::from(pressure[up]) - f64::from(pressure[down])) / h;
                out[w] = 1.0;
            }
        }
    }
    out
}

#[test]
fn gpu_flip_solid_faces_into_projection_fuses() {
    let n = [5usize, 4, 3];
    let m = n.map(|v| v + 1);
    let phi: Vec<f32> = random_values(m.iter().product(), 0x5f1).iter().map(|v| v * 0.6).collect();
    let mut chain = Chain::new();
    let s = chain.source("solid", phi.clone());
    let open = chain.node("open", "node.solid_faces", lattice_json(n, &[("cell_size", 0.25), ("box_offset", 1.5)]));
    chain.wire(s, "out", open, "solid");
    let (got, faces, pressure, water) = project_through(&mut chain, open, n, 0x5f2);
    let (weights, _) = cpu_solid_faces(&phi, n, 8.0 * f64::from(f32::EPSILON) * 2.75);
    assert!(weights.contains(&0.0) && weights.iter().any(|&w| w > 0.0 && w < 1.0));
    assert_close(&got, &cpu_project(&faces, &weights, &pressure, &water, n, 0.3), "fused solid faces into projection");
}

/// The water step's one fused pair: the projection into the solids'
/// constraint, on random weights and solid velocities.
#[test]
fn gpu_flip_projection_into_constraint_fuses() {
    let n = SOLID_N;
    let open = random_open_faces(n, 0x9c1, 0);
    let moving: Vec<f32> = random_values(face_grid_len(n), 0x9c2)
        .chunks(FACE_FLOATS)
        .flat_map(|r| [r[0], r[1], r[2], r[3], r[4] + 0.5, r[5] + 0.5, r[6] + 0.5, r[7]])
        .collect();
    let cells: usize = n.iter().product();
    let faces = random_values(face_grid_len(n), 0x9c3);
    let (pressure, water) = (random_values(cells, 0x9c4), random_water(cells, 0x9c5));
    let mut chain = Chain::new();
    let o = chain.face_source("open", open.clone());
    let v = chain.face_source("moving", moving.clone());
    let f = chain.face_source("faces", faces.clone());
    let p = chain.source("pressure", pressure.clone());
    let w = chain.source("water", water.clone());
    let project = chain.node("project", "node.subtract_pressure", lattice_json(n, &[("cell_size", 0.3)]));
    chain.wire(f, "out", project, "faces");
    chain.wire(p, "out", project, "pressure");
    chain.wire(w, "out", project, "water");
    chain.wire(o, "out", project, "solid_faces");
    let constrain = chain.node("constrain", "node.constrain_solid_faces", lattice_json(n, &[]));
    chain.wire(project, "out", constrain, "faces");
    chain.wire(o, "out", constrain, "solid_faces");
    chain.wire(v, "out", constrain, "solid_velocity");
    chain.sink = "test.face_sink";
    let got = chain.fused_matches_unfused(constrain, face_grid_len(n));
    let open64: Vec<f64> = open.iter().map(|&x| f64::from(x)).collect();
    let moving64: Vec<f64> = moving.iter().map(|&x| f64::from(x)).collect();
    let projected: Vec<f32> = cpu_project(&faces, &open64, &pressure, &water, n, 0.3).iter().map(|&x| x as f32).collect();
    assert_close(&got, &cpu_constrain(&projected, &open64, &moving64, n), "fused projection into constraint");
}

#[test]
fn gpu_flip_coarsen_solid_faces_into_projection_fuses() {
    let fine = random_open_faces(FINE, 0xcf2, 0);
    let mut chain = Chain::new();
    let f = chain.face_source("fine", fine.clone());
    let open = chain.node("open", "node.coarsen_solid_faces", lattice_json(COARSE, &[]));
    chain.wire(f, "out", open, "fine");
    let (got, faces, pressure, water) = project_through(&mut chain, open, COARSE, 0xcf3);
    let weights = cpu_coarsen_faces(&fine, COARSE);
    assert_close(&got, &cpu_project(&faces, &weights, &pressure, &water, COARSE, 0.3), "fused coarsen into projection");
}

// ── Solids' velocity ───────────────────────────────────────────────────────

/// The moving-solid lattice: 5×4×3 cells a quarter metre apart.
const SOLID_N: [usize; 3] = [5, 4, 3];
const SOLID_H: f32 = 0.25;
const SOLID_MIN: [f32; 3] = [-0.6, 0.0, -0.4];
const SOLID_TICK: f32 = 0.05;

/// Two moving, turning bodies whose lattices overlap, one scaled, and a
/// disabled row; their distance lattices as the atlas stores them (half
/// precision) and as f32.
struct Solids {
    bodies: Vec<LiquidBody>,
    shapes: Vec<LiquidShape>,
    atlas: Vec<u32>,
    distances: Vec<f32>,
    /// node.face_impulse_to_bodies' sums, 16 per body: the second body's
    /// dv and dω are set.
    changes: Vec<f32>,
}

fn solids() -> Solids {
    let shapes = vec![
        LiquidShape {
            origin_spacing: [-0.47, -0.43, -0.41, 0.3],
            dims_x: 4,
            dims_y: 4,
            dims_z: 4,
            atlas_offset: 0,
            scale_min: [1.0, 1.0, 1.0, 1.0],
        },
        LiquidShape {
            origin_spacing: [-0.39, -0.42, -0.44, 0.4],
            dims_x: 3,
            dims_y: 3,
            dims_z: 3,
            atlas_offset: 64,
            scale_min: [1.2, 1.0, 1.1, 1.0],
        },
    ];
    let distances: Vec<f32> =
        random_values(64 + 27, 0x50d).iter().map(|v| half::f16::from_f32(v * 0.9).to_f32()).collect();
    let mut atlas = Vec::new();
    pack_distance_atlas(&distances, &mut atlas);
    let body = |position: [f32; 3], velocity: [f32; 4], angular: [f32; 3], shape: f32| LiquidBody {
        position_inv_mass: [position[0], position[1], position[2], 0.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
        linear_velocity: velocity,
        angular_velocity: [angular[0], angular[1], angular[2], 0.0],
        accel_shape: [0.0, 0.0, 0.0, shape],
        ..LiquidBody::default()
    };
    let bodies = vec![
        body([-0.21, 0.38, -0.02], [0.7, -0.3, 0.2, 0.4], [0.0, 0.3, 1.5], 0.0),
        body([0.33, 0.61, 0.03], [-0.5, 0.1, 0.1, 0.9], [0.8, 0.0, -0.2], 1.0),
        body([0.0, 0.5, 0.0], [3.0, 3.0, 3.0, 0.7], [0.0; 3], -1.0),
    ];
    // The second body is dynamic: it moves at its predicted velocity plus its change.
    let mut bodies = bodies;
    bodies[1].position_inv_mass[3] = 0.5;
    bodies[1].accel_shape = [0.4, -9.8, 0.2, 1.0];
    bodies[1].inv_inertia_x = [0.3, 0.0, 0.0, 1.1];
    bodies[1].inv_inertia_y = [0.0, 0.3, 0.0, -0.6];
    bodies[1].inv_inertia_z = [0.0, 0.0, 0.3, 0.4];
    let mut changes = vec![0.0; 16 * bodies.len()];
    changes[16 + 8..16 + 11].copy_from_slice(&[0.05, 0.31, -0.07]);
    changes[16 + 12..16 + 15].copy_from_slice(&[-0.2, 0.15, 0.09]);
    Solids { bodies, shapes, atlas, distances, changes }
}

fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let cross = |a: [f64; 3], b: [f64; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let u = [q[0], q[1], q[2]];
    let t = cross(u, v).map(|c| 2.0 * c);
    let c = cross(u, t);
    std::array::from_fn(|i| v[i] + q[3] * t[i] + c[i])
}

/// The closest enabled body at x after `SOLID_TICK` (its row and signed
/// distance), as liquid_collider.wgsl samples a lattice; the margin by which
/// x clears every lattice edge and the gap to the runner-up, so the fixture
/// can show no f32 rounding decides either.
fn closest(solids: &Solids, x: [f64; 3]) -> (Option<usize>, f64) {
    let mut best: Option<(usize, f64)> = None;
    let mut margin = f64::INFINITY;
    let mut gaps = Vec::new();
    for (row, body) in solids.bodies.iter().enumerate() {
        let shape_index = body.accel_shape[3];
        if shape_index < 0.0 {
            continue;
        }
        let shape = solids.shapes[shape_index as usize];
        let (position, q) = body_pose_at(body, SOLID_TICK);
        let inverse = [-f64::from(q[0]), -f64::from(q[1]), -f64::from(q[2]), f64::from(q[3])];
        let local = rotate(inverse, std::array::from_fn(|i| x[i] - f64::from(position[i])));
        let dims = [shape.dims_x, shape.dims_y, shape.dims_z].map(|d| d as usize);
        let g: [f64; 3] = std::array::from_fn(|i| {
            (local[i] / f64::from(shape.scale_min[i]) - f64::from(shape.origin_spacing[i])) / f64::from(shape.origin_spacing[3])
        });
        for i in 0..3 {
            margin = margin.min(g[i].abs()).min((g[i] - (dims[i] - 1) as f64).abs());
        }
        if !(0..3).all(|i| g[i] >= 0.0 && g[i] <= (dims[i] - 1) as f64) {
            continue;
        }
        let base: [usize; 3] = std::array::from_fn(|i| (g[i].floor() as usize).min(dims[i] - 2));
        let f: [f64; 3] = std::array::from_fn(|i| g[i] - base[i] as f64);
        let mut d = 0.0;
        for corner in 0..8 {
            let o = [corner & 1, (corner >> 1) & 1, corner >> 2];
            let w: f64 = (0..3).map(|i| if o[i] == 1 { f[i] } else { 1.0 - f[i] }).product();
            let at = shape.atlas_offset as usize + (base[0] + o[0]) + dims[0] * ((base[1] + o[1]) + dims[1] * (base[2] + o[2]));
            d += w * f64::from(solids.distances[at]);
        }
        d *= f64::from(shape.scale_min[3]);
        gaps.push(d);
        if best.is_none_or(|(_, nearest)| d < nearest) {
            best = Some((row, d));
        }
    }
    gaps.sort_by(f64::total_cmp);
    let gap = if gaps.len() > 1 { gaps[1] - gaps[0] } else { f64::INFINITY };
    (best.map(|(row, _)| row), margin.min(gap))
}

/// The velocity and spin a face sees on body `row`: as uploaded when
/// prescribed; predicted over `SOLID_TICK` plus its change when dynamic.
fn moving_velocity(solids: &Solids, row: usize) -> ([f64; 3], [f64; 3]) {
    let body = &solids.bodies[row];
    let mut v: [f64; 3] = std::array::from_fn(|a| f64::from(body.linear_velocity[a]));
    let mut w: [f64; 3] = std::array::from_fn(|a| f64::from(body.angular_velocity[a]));
    if body.position_inv_mass[3] > 0.0 {
        let t = f64::from(SOLID_TICK);
        let spin = [body.inv_inertia_x[3], body.inv_inertia_y[3], body.inv_inertia_z[3]];
        let c = &solids.changes[16 * row..16 * row + 16];
        for a in 0..3 {
            v[a] += f64::from(body.accel_shape[a]) * t + f64::from(c[8 + a]);
            w[a] += f64::from(spin[a]) * t + f64::from(c[12 + a]);
        }
    }
    (v, w)
}

/// node.solid_face_velocity in f64: the closest body's rigid velocity at
/// each cut inner face's centre, and the mean friction at its four corners.
/// Also the smallest margin any query had (see `closest`).
fn cpu_solid_face_velocity(open: &[f64], solids: &Solids) -> (Vec<f64>, f64, usize) {
    let n = SOLID_N;
    let m = n.map(|v| v + 1);
    let h = f64::from(SOLID_H);
    let min = SOLID_MIN.map(f64::from);
    let mut out = vec![0.0; face_grid_len(n)];
    let mut margin = f64::INFINITY;
    let mut moved = 0;
    for i in 0..m.iter().product::<usize>() {
        let p = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
        for a in 0..3 {
            if !(0..3).all(|b| b == a || p[b] < n[b]) || p[a] == 0 || p[a] == n[a] || open[i * FACE_FLOATS + 4 + a] >= 1.0 {
                continue;
            }
            let mut centre: [f64; 3] = std::array::from_fn(|b| min[b] + (p[b] as f64 + 0.5) * h);
            centre[a] = min[a] + p[a] as f64 * h;
            let (row, clear) = closest(solids, centre);
            margin = margin.min(clear);
            if let Some(row) = row {
                let body = &solids.bodies[row];
                let (position, _) = body_pose_at(body, SOLID_TICK);
                let r: [f64; 3] = std::array::from_fn(|b| centre[b] - f64::from(position[b]));
                let (v, w) = moving_velocity(solids, row);
                let spin = [w[1] * r[2] - w[2] * r[1], w[2] * r[0] - w[0] * r[2], w[0] * r[1] - w[1] * r[0]];
                out[i * FACE_FLOATS + a] = v[a] + spin[a];
                out[i * FACE_FLOATS + 3] += ((row + 1) << (8 * a)) as f64;
                moved += 1;
            }
            let (b, c) = match a {
                0 => (1, 2),
                1 => (2, 0),
                _ => (1, 0),
            };
            let mut friction = 0.0;
            for k in 0..4 {
                let mut q = p;
                q[b] += k & 1;
                q[c] += (k >> 1) & 1;
                let (row, clear) = closest(solids, std::array::from_fn(|d| min[d] + q[d] as f64 * h));
                margin = margin.min(clear);
                if let Some(row) = row {
                    friction += f64::from(solids.bodies[row].linear_velocity[3]);
                }
            }
            out[i * FACE_FLOATS + 4 + a] = 0.25 * friction;
        }
    }
    (out, margin, moved)
}

fn solid_velocity_params(solids: &Solids) -> Vec<(&'static str, f32)> {
    let rows = solids.bodies.len() as f32;
    vec![
        ("lattice_min_x", SOLID_MIN[0]),
        ("lattice_min_y", SOLID_MIN[1]),
        ("lattice_min_z", SOLID_MIN[2]),
        ("cell_size", SOLID_H),
        ("body_count", rows),
        ("rows", rows),
        ("tick_seconds", SOLID_TICK),
    ]
}

/// node.solid_face_velocity against the rigid velocity and corner friction
/// computed here, on random cut faces under two overlapping turning bodies.
#[test]
fn gpu_flip_solid_face_velocity_matches_cpu() {
    let solids = solids();
    let open = random_open_faces(SOLID_N, 0x5fa, 0);
    let bodies: &[f32] = bytemuck::cast_slice(&solids.bodies);
    let shapes: &[f32] = bytemuck::cast_slice(&solids.shapes);
    let atlas: &[f32] = bytemuck::cast_slice(&solids.atlas);
    let got = run_atom(
        &mut super::solid_face_velocity::SolidFaceVelocity::new(),
        &[("solid_faces", &open), ("bodies", bodies), ("shapes", shapes), ("atlas", atlas), ("changes", &solids.changes)],
        face_grid_len(SOLID_N),
        &lattice_params(SOLID_N, &solid_velocity_params(&solids)),
    );
    let open64: Vec<f64> = open.iter().map(|&v| f64::from(v)).collect();
    let (want, margin, moved) = cpu_solid_face_velocity(&open64, &solids);
    assert!(margin > 1.0e-4, "no query sits on a lattice edge or a tie between bodies: {margin}");
    assert!(moved > 10, "the bodies reach cut faces: {moved}");
    let frictions: Vec<f64> = want.chunks(FACE_FLOATS).flat_map(|r| r[4..7].to_vec()).collect();
    assert!(frictions.iter().any(|&f| f > 0.0 && f < 0.4), "a face with corners outside every body");
    assert!(frictions.iter().any(|&f| f > 0.4), "corners reaching the second body");
    assert_close(&got, &want, "solid face velocity");
}

/// node.constrain_solid_faces in f64.
fn cpu_constrain(faces: &[f32], open: &[f64], moving: &[f64], n: [usize; 3]) -> Vec<f64> {
    let m = n.map(|v| v + 1);
    let mut out: Vec<f64> = faces.iter().map(|&v| f64::from(v)).collect();
    for i in 0..m.iter().product::<usize>() {
        let p = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
        for a in 0..3 {
            if !(0..3).all(|b| b == a || p[b] < n[b]) || p[a] == 0 || p[a] == n[a] {
                continue;
            }
            let (v, w) = (i * FACE_FLOATS + a, i * FACE_FLOATS + 4 + a);
            let solid = moving[v];
            if open[w] <= 0.0 {
                out[v] = solid;
            } else if open[w] < 1.0 {
                out[v] = moving[w] * solid + (1.0 - moving[w]) * f64::from(faces[v]);
            }
        }
    }
    out
}

#[test]
fn gpu_flip_constrain_solid_faces_matches_cpu() {
    let n = SOLID_N;
    let faces = random_values(face_grid_len(n), 0xc51);
    let open = random_open_faces(n, 0xc52, 0);
    let moving: Vec<f32> = random_values(face_grid_len(n), 0xc53)
        .chunks(FACE_FLOATS)
        .flat_map(|r| [r[0], r[1], r[2], r[3], r[4] + 0.5, r[5] + 0.5, r[6] + 0.5, r[7]])
        .collect();
    let got = run_atom(
        &mut super::constrain_solid_faces::ConstrainSolidFaces::new(),
        &[("faces", &faces), ("solid_faces", &open), ("solid_velocity", &moving)],
        face_grid_len(n),
        &lattice_params(n, &[]),
    );
    let open64: Vec<f64> = open.iter().map(|&v| f64::from(v)).collect();
    let moving64: Vec<f64> = moving.iter().map(|&v| f64::from(v)).collect();
    let want = cpu_constrain(&faces, &open64, &moving64, n);
    assert!(open.contains(&0.0) && open.iter().any(|&w| w > 0.0 && w < 1.0), "closed and cut faces");
    assert_close(&got, &want, "constrain solid faces");
}

/// The solids' whole face pass fused: open fractions from a distance
/// lattice, the bodies' velocity on them, and the constraint on random
/// faces, against the CPU chain.
#[test]
fn gpu_flip_solid_velocity_into_constraint_fuses() {
    let n = SOLID_N;
    let m = n.map(|v| v + 1);
    let solids = solids();
    let phi: Vec<f32> = random_values(m.iter().product(), 0x5f7).iter().map(|v| v * 0.6).collect();
    let faces = random_values(face_grid_len(n), 0x5f8);
    let offset = 1.5;
    let mut chain = Chain::new();
    let s = chain.source("solid", phi.clone());
    let f = chain.face_source("faces", faces.clone());
    let typed = |chain: &mut Chain, name: &'static str, type_id: &str, records: usize, values: &[f32]| {
        let id = chain.node(name, type_id, json!({"max_capacity": {"type": "Int", "value": records}}));
        chain.sources.push((name, values.to_vec()));
        id
    };
    let bodies = typed(&mut chain, "bodies", "test.body_source", solids.bodies.len(), bytemuck::cast_slice(&solids.bodies));
    let shapes = typed(&mut chain, "shapes", "test.shape_source", solids.shapes.len(), bytemuck::cast_slice(&solids.shapes));
    let atlas = typed(&mut chain, "atlas", "test.word_source", solids.atlas.len(), bytemuck::cast_slice(&solids.atlas));
    let changes = chain.source("changes", solids.changes.clone());
    let open = chain.node("open", "node.solid_faces", lattice_json(n, &[("cell_size", f64::from(SOLID_H)), ("box_offset", offset)]));
    chain.wire(s, "out", open, "solid");
    let extra: Vec<(&str, f64)> = solid_velocity_params(&solids).into_iter().map(|(k, v)| (k, f64::from(v))).collect();
    let velocity = chain.node("velocity", "node.solid_face_velocity", lattice_json(n, &extra));
    chain.wire(open, "out", velocity, "solid_faces");
    chain.wire(bodies, "out", velocity, "bodies");
    chain.wire(shapes, "out", velocity, "shapes");
    chain.wire(atlas, "out", velocity, "atlas");
    chain.wire(changes, "out", velocity, "changes");
    let constrain = chain.node("constrain", "node.constrain_solid_faces", lattice_json(n, &[]));
    chain.wire(f, "out", constrain, "faces");
    chain.wire(open, "out", constrain, "solid_faces");
    chain.wire(velocity, "out", constrain, "solid_velocity");
    chain.sink = "test.face_sink";
    let got = chain.fused_matches_unfused(constrain, face_grid_len(n));
    let tolerance = 8.0 * f64::from(f32::EPSILON) * (f64::from(SOLID_H) * 5.0 + offset);
    let (open64, _) = cpu_solid_faces(&phi, n, tolerance);
    let (moving, margin, moved) = cpu_solid_face_velocity(&open64, &solids);
    assert!(margin > 1.0e-4 && moved > 5, "a clear fixture that moves faces: {margin} {moved}");
    assert_close(&got, &cpu_constrain(&faces, &open64, &moving, n), "fused solids into constraint");
}
