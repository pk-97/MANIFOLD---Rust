//! GPU value proofs for the GPU FLIP pressure solve's atoms
//! (docs/GPU_FLIP_PRESSURE_SOLVE.md) against CPU f64 references, and the
//! fused-vs-unfused proofs of the ones that can fuse. Every lattice here is
//! a few hundred cells, sized exactly; each atom's run() refuses arrays
//! shorter than its lattice before dispatch.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::{Beats, Seconds};
use manifold_gpu::{GpuBuffer, GpuTextureFormat};
use serde_json::json;

use super::coarse_pressure_solve::CoarsePressureSolve;
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

fn cpu_coarse_solve(water: &[f32], rhs: &[f32], n: [usize; 3], h: f64, sweeps: usize) -> Vec<f64> {
    let mut value = vec![0.0; water.len()];
    for order in [[0, 1], [1, 0]] {
        for _ in 0..sweeps {
            for color in order {
                value = cpu_sweep(water, rhs, &value, n, h, color);
            }
        }
    }
    value
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
    for color in [0, 1] {
        let got = run_atom(
            &mut PressureSmooth::new(),
            &[("water", &water), ("rhs", &rhs), ("value", &value)],
            cells,
            &lattice_params(FINE, &[("cell_size", h as f32), ("color", color as f32)]),
        );
        assert_close(&got, &cpu_sweep(&water, &rhs, &value64, FINE, h, color), &format!("sweep color {color}"));
    }
}

#[test]
fn gpu_flip_residual_is_rhs_minus_the_masked_laplacian() {
    let cells: usize = FINE.iter().product();
    let (water, rhs, value) = (random_water(cells, 0x7e1), random_values(cells, 0x7e2), random_values(cells, 0x7e3));
    let h = 0.3;
    let got = run_atom(
        &mut PressureResidual::new(),
        &[("water", &water), ("rhs", &rhs), ("value", &value)],
        cells,
        &lattice_params(FINE, &[("cell_size", h as f32)]),
    );
    assert_close(&got, &cpu_residual(&water, &rhs, &value, FINE, h), "residual");
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

/// A small lattice and one at the workgroup's full 4,096 cells.
#[test]
fn gpu_flip_coarse_solve_matches_cpu_sweeps() {
    for (n, seed) in [([5, 4, 6], 0xc5), ([16, 16, 16], 0xc6)] {
        let cells: usize = n.iter().product();
        let (water, rhs) = (random_water(cells, seed), random_values(cells, seed + 1));
        let (h, sweeps) = (0.5, 8);
        let got = run_atom(
            &mut CoarsePressureSolve::new(),
            &[("water", &water), ("rhs", &rhs)],
            cells,
            &lattice_params(n, &[("cell_size", h as f32), ("sweeps", sweeps as f32)]),
        );
        let want = cpu_coarse_solve(&water, &rhs, n, h, sweeps);
        let scale = want.iter().fold(1.0_f64, |m, v| m.max(v.abs()));
        let worst = got.iter().zip(&want).map(|(a, e)| (f64::from(*a) - e).abs()).fold(0.0, f64::max);
        assert!(worst <= 1e-4 * scale, "{n:?}: worst {worst:.3e} of {scale:.3e}");
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
        let id = self.node(name, "test.value_source", json!({"max_capacity": {"type": "Int", "value": values.len()}}));
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
    let mut chain = Chain::new();
    let w = chain.source("water", water.clone());
    let r = chain.source("rhs", rhs.clone());
    let v = chain.source("value", value.clone());
    let s = chain.source("start", start.clone());
    let residual = chain.node("residual", "node.pressure_residual", lattice_json(FINE, &[("cell_size", h)]));
    chain.wire(w, "out", residual, "water");
    chain.wire(r, "out", residual, "rhs");
    chain.wire(v, "out", residual, "value");
    let sweep = chain.node("sweep", "node.pressure_smooth", lattice_json(FINE, &[("cell_size", h), ("color", 1.0)]));
    chain.wire(w, "out", sweep, "water");
    chain.wire(residual, "out", sweep, "rhs");
    chain.wire(s, "out", sweep, "value");
    let got = chain.fused_matches_unfused(sweep, cells);
    let mid: Vec<f32> = cpu_residual(&water, &rhs, &value, FINE, h).iter().map(|&v| v as f32).collect();
    let start64: Vec<f64> = start.iter().map(|&v| f64::from(v)).collect();
    assert_close(&got, &cpu_sweep(&water, &mid, &start64, FINE, h, 1), "fused residual into sweep");
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
    let residual = chain.node("residual", "node.pressure_residual", lattice_json(FINE, &[("cell_size", h)]));
    chain.wire(w, "out", residual, "water");
    chain.wire(r, "out", residual, "rhs");
    chain.wire(v, "out", residual, "value");
    let divide = chain.node("divide", "node.divide_by_value", json!({}));
    chain.wire(residual, "out", divide, "values");
    chain.wire(d, "out", divide, "divisor");
    let got = chain.fused_matches_unfused(divide, cells);
    let want: Vec<f64> = cpu_residual(&water, &rhs, &value, FINE, h).iter().map(|&v| v / -2.5).collect();
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
    let prolong = chain.node("prolong", "node.prolong_lattice", lattice_json(FINE, &[]));
    chain.wire(sweep, "out", prolong, "value");
    chain.wire(c, "out", prolong, "coarse");
    chain.wire(w, "out", prolong, "water");
    let mut registry = PrimitiveRegistry::with_builtin();
    register_substep_test_nodes(&mut registry);
    assert!(crate::node_graph::freeze::install::fuse_canonical_def(&chain.def(prolong), &registry).is_none());
}
