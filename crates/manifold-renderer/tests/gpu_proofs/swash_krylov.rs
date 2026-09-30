//! The SWASH Krylov loop on the GPU (`docs/FFT_WATER_SOLVER_DESIGN.md` D10):
//! GMRES as a substep region under `node.krylov_basis`, the operator a dense
//! matrix applied by `node.combine_rows`, checked against the same algorithm
//! (classical Gram–Schmidt twice, Givens rotations) in f64 on the CPU.
//!
//! The graph: b → length β and start b/β → krylov_basis; body: w = A·current,
//! two projection rounds against the basis, the new vector normalised into
//! next_in, the Givens update into in; after the loop y = krylov_solve(state)
//! and x = basis·y.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::{Beats, Seconds};
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::node_graph::freeze::install::fuse_generator_view;
use manifold_renderer::node_graph::substeps::test_nodes::register_substep_test_nodes;
use manifold_renderer::node_graph::{
    Backend, EffectGraphDefExt, ExecutionPlan, Executor, FrameTime, Graph, MetalBackend,
    NodeInstanceId, PrimitiveRegistry, ResourceId, StateStore, compile, pre_allocate_resources,
};

use crate::harness;

fn def(n: usize, passes: usize) -> EffectGraphDef {
    let m = passes;
    let int = |v: usize| serde_json::json!({"type": "Int", "value": v});
    let float = |v: f64| serde_json::json!({"type": "Float", "value": v});
    serde_json::from_value(serde_json::json!({
        "version": 3,
        "nodes": [
            {"id": 0, "nodeId": "a", "typeId": "test.value_source", "params": {"max_capacity": int(n * n)}},
            {"id": 1, "nodeId": "b", "typeId": "test.value_source", "params": {"max_capacity": int(n)}},
            {"id": 2, "nodeId": "beta", "typeId": "node.dot_products",
             "params": {"row_length": int(n), "rows": int(1), "max_rows": int(1), "root": int(1)}},
            {"id": 3, "nodeId": "start", "typeId": "node.divide_by_value"},
            {"id": 4, "nodeId": "loop", "typeId": "node.krylov_basis",
             "params": {"passes": int(m), "row_length": int(n)}},
            {"id": 5, "nodeId": "apply", "typeId": "node.combine_rows",
             "params": {"row_length": int(n), "rows": int(n), "scale": float(1.0), "base_scale": float(0.0)}},
            {"id": 6, "nodeId": "h1", "typeId": "node.dot_products",
             "params": {"row_length": int(n), "max_rows": int(m + 1)}},
            {"id": 7, "nodeId": "w1", "typeId": "node.combine_rows",
             "params": {"row_length": int(n), "scale": float(-1.0), "base_scale": float(1.0)}},
            {"id": 8, "nodeId": "h2", "typeId": "node.dot_products",
             "params": {"row_length": int(n), "max_rows": int(m + 1)}},
            {"id": 9, "nodeId": "w2", "typeId": "node.combine_rows",
             "params": {"row_length": int(n), "scale": float(-1.0), "base_scale": float(1.0)}},
            {"id": 10, "nodeId": "norm", "typeId": "node.dot_products",
             "params": {"row_length": int(n), "rows": int(1), "max_rows": int(1), "root": int(1)}},
            {"id": 11, "nodeId": "next", "typeId": "node.divide_by_value"},
            {"id": 12, "nodeId": "givens", "typeId": "node.krylov_givens", "params": {"passes": int(m)}},
            {"id": 13, "nodeId": "solve", "typeId": "node.krylov_solve", "params": {"passes": int(m)}},
            {"id": 14, "nodeId": "x", "typeId": "node.combine_rows",
             "params": {"row_length": int(n), "rows": int(m), "scale": float(1.0), "base_scale": float(0.0)}},
            {"id": 15, "nodeId": "sink", "typeId": "test.value_sink"},
            {"id": 16, "nodeId": "output", "typeId": "system.final_output"}
        ],
        "wires": [
            {"fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "matrix"},
            {"fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "vector"},
            {"fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "values"},
            {"fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "divisor"},
            {"fromNode": 2, "fromPort": "out", "toNode": 4, "toPort": "seed"},
            {"fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "start"},
            {"fromNode": 4, "fromPort": "current", "toNode": 5, "toPort": "base"},
            {"fromNode": 0, "fromPort": "out", "toNode": 5, "toPort": "matrix"},
            {"fromNode": 4, "fromPort": "current", "toNode": 5, "toPort": "coef"},
            {"fromNode": 4, "fromPort": "basis", "toNode": 6, "toPort": "matrix"},
            {"fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "vector"},
            {"fromNode": 4, "fromPort": "rows", "toNode": 6, "toPort": "rows"},
            {"fromNode": 5, "fromPort": "out", "toNode": 7, "toPort": "base"},
            {"fromNode": 4, "fromPort": "basis", "toNode": 7, "toPort": "matrix"},
            {"fromNode": 6, "fromPort": "out", "toNode": 7, "toPort": "coef"},
            {"fromNode": 4, "fromPort": "rows", "toNode": 7, "toPort": "rows"},
            {"fromNode": 4, "fromPort": "basis", "toNode": 8, "toPort": "matrix"},
            {"fromNode": 7, "fromPort": "out", "toNode": 8, "toPort": "vector"},
            {"fromNode": 4, "fromPort": "rows", "toNode": 8, "toPort": "rows"},
            {"fromNode": 7, "fromPort": "out", "toNode": 9, "toPort": "base"},
            {"fromNode": 4, "fromPort": "basis", "toNode": 9, "toPort": "matrix"},
            {"fromNode": 8, "fromPort": "out", "toNode": 9, "toPort": "coef"},
            {"fromNode": 4, "fromPort": "rows", "toNode": 9, "toPort": "rows"},
            {"fromNode": 9, "fromPort": "out", "toNode": 10, "toPort": "matrix"},
            {"fromNode": 9, "fromPort": "out", "toNode": 10, "toPort": "vector"},
            {"fromNode": 9, "fromPort": "out", "toNode": 11, "toPort": "values"},
            {"fromNode": 10, "fromPort": "out", "toNode": 11, "toPort": "divisor"},
            {"fromNode": 11, "fromPort": "out", "toNode": 4, "toPort": "next_in"},
            {"fromNode": 4, "fromPort": "out", "toNode": 12, "toPort": "state"},
            {"fromNode": 6, "fromPort": "out", "toNode": 12, "toPort": "first"},
            {"fromNode": 8, "fromPort": "out", "toNode": 12, "toPort": "second"},
            {"fromNode": 10, "fromPort": "out", "toNode": 12, "toPort": "norm"},
            {"fromNode": 4, "fromPort": "pass", "toNode": 12, "toPort": "column"},
            {"fromNode": 12, "fromPort": "out", "toNode": 4, "toPort": "in"},
            {"fromNode": 4, "fromPort": "out", "toNode": 13, "toPort": "state"},
            {"fromNode": 4, "fromPort": "current", "toNode": 14, "toPort": "base"},
            {"fromNode": 4, "fromPort": "basis", "toNode": 14, "toPort": "matrix"},
            {"fromNode": 13, "fromPort": "out", "toNode": 14, "toPort": "coef"},
            {"fromNode": 14, "fromPort": "out", "toNode": 15, "toPort": "values"},
            {"fromNode": 15, "fromPort": "out", "toNode": 16, "toPort": "in"}
        ]
    }))
    .expect("krylov proof def")
}

fn node_named(graph: &Graph, name: &str) -> NodeInstanceId {
    graph
        .nodes()
        .find(|n| n.node_id.as_str() == name)
        .map(|n| n.id)
        .unwrap_or_else(|| panic!("graph has no node `{name}`"))
}

fn output_of(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
    let step = plan.steps().iter().find(|s| s.node == node).expect("node compiled");
    step.outputs
        .iter()
        .find(|(name, _)| *name == port)
        .map(|&(_, r)| r)
        .unwrap_or_else(|| panic!("no output `{port}`"))
}

/// A nonsymmetric system with a known spread of eigenvalues: `diagonal`
/// plus a small random part, and a random right-hand side.
fn system(n: usize, seed: u64, low_rank: bool) -> (Vec<f64>, Vec<f64>) {
    let mut state = seed | 1;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5
    };
    let mut a = vec![0.0; n * n];
    if low_rank {
        // Identity plus rank two: GMRES is exact after three passes.
        let (u1, u2, v1, v2): (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>) =
            ((0..n).map(|_| next()).collect(), (0..n).map(|_| next()).collect(), (0..n).map(|_| next()).collect(), (0..n).map(|_| next()).collect());
        for r in 0..n {
            for c in 0..n {
                a[r * n + c] = f64::from(u8::from(r == c)) + u1[r] * v1[c] + u2[r] * v2[c];
            }
        }
    } else {
        for r in 0..n {
            for c in 0..n {
                a[r * n + c] = next() * 2.0 / (n as f64).sqrt() + if r == c { 3.0 + r as f64 / n as f64 } else { 0.0 };
            }
        }
    }
    let b = (0..n).map(|_| next()).collect();
    (a, b)
}

fn mat_vec(a: &[f64], v: &[f64]) -> Vec<f64> {
    let n = v.len();
    (0..n).map(|r| (0..n).map(|c| a[r * n + c] * v[c]).sum()).collect()
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The GPU loop's algorithm in f64: CGS2 Arnoldi, Givens, back-substitution.
fn reference_gmres(a: &[f64], b: &[f64], passes: usize) -> Vec<f64> {
    let n = b.len();
    let beta = dot(b, b).sqrt();
    let mut basis: Vec<Vec<f64>> = vec![b.iter().map(|v| v / beta).collect()];
    let mut h = vec![vec![0.0; passes]; passes + 1];
    let (mut cs, mut sn) = (vec![0.0; passes], vec![0.0; passes]);
    let mut g = vec![0.0; passes + 1];
    g[0] = beta;
    for j in 0..passes {
        let mut w = mat_vec(a, &basis[j]);
        let mut col = vec![0.0; j + 2];
        for _round in 0..2 {
            let proj: Vec<f64> = basis.iter().map(|v| dot(&w, v)).collect();
            for (i, p) in proj.iter().enumerate() {
                col[i] += p;
                for e in 0..n {
                    w[e] -= p * basis[i][e];
                }
            }
        }
        let norm = dot(&w, &w).sqrt();
        col[j + 1] = norm;
        basis.push(if norm < 1e-30 { vec![0.0; n] } else { w.iter().map(|v| v / norm).collect() });
        for i in 0..j {
            let t = cs[i] * col[i] + sn[i] * col[i + 1];
            col[i + 1] = -sn[i] * col[i] + cs[i] * col[i + 1];
            col[i] = t;
        }
        let r = (col[j] * col[j] + col[j + 1] * col[j + 1]).sqrt();
        (cs[j], sn[j]) = if r > 1e-30 { (col[j] / r, col[j + 1] / r) } else { (1.0, 0.0) };
        col[j] = r;
        col[j + 1] = 0.0;
        g[j + 1] = -sn[j] * g[j];
        g[j] *= cs[j];
        for (i, value) in col.iter().enumerate().take(j + 1) {
            h[i][j] = *value;
        }
    }
    let mut y = vec![0.0; passes];
    for k in (0..passes).rev() {
        let acc = g[k] - (k + 1..passes).map(|l| h[k][l] * y[l]).sum::<f64>();
        y[k] = if h[k][k].abs() > 1e-30 { acc / h[k][k] } else { 0.0 };
    }
    (0..n).map(|e| (0..passes).map(|j| y[j] * basis[j][e]).sum()).collect()
}

fn registry() -> PrimitiveRegistry {
    let mut registry = PrimitiveRegistry::with_builtin();
    register_substep_test_nodes(&mut registry);
    registry
}

/// Run the proof graph for one frame and read x back.
fn run_gpu(n: usize, passes: usize, a: &[f64], b: &[f64]) -> Vec<f64> {
    run_graph(def(n, passes).into_graph(&registry(), &Default::default()).expect("proof def builds"), n, a, b)
}

fn run_graph(mut graph: Graph, n: usize, a: &[f64], b: &[f64]) -> Vec<f64> {
    let harness = harness::shared();
    let device = &harness.device;
    let plan = compile(&graph).expect("proof def compiles");
    assert_eq!(plan.substep_regions().len(), 1, "one Krylov region");
    let mut backend = MetalBackend::new(device.clone(), 64, 64, GpuTextureFormat::Rgba16Float);
    pre_allocate_resources(&graph, &plan, device, &mut backend).expect("pre-allocate");

    // The matrix source holds A's columns: row i of the input is column i.
    let columns: Vec<f32> = (0..n * n).map(|k| a[(k % n) * n + k / n] as f32).collect();
    let rhs: Vec<f32> = b.iter().map(|&v| v as f32).collect();
    for (name, values) in [("a", &columns), ("b", &rhs)] {
        let res = output_of(&plan, node_named(&graph, name), "out");
        let buffer = Backend::array_buffer(&backend, backend.slot_for(res).expect("bound")).expect("buffer");
        let bytes: &[u8] = bytemuck::cast_slice(values);
        assert!(buffer.size as usize >= bytes.len());
        // SAFETY: shared-storage buffer, no GPU work in flight yet.
        unsafe { buffer.write(0, bytes) };
    }

    let x_node = node_named(&graph, "x");
    let x_res = output_of(&plan, x_node, "out");
    let mut exec = Executor::new(Box::new(backend));
    exec.set_dump_set(Some(std::iter::once(x_node).collect()));
    let mut state = StateStore::new();
    let time = FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(1.0 / 60.0), frame_count: 0 };
    let mut enc = device.create_encoder("swash-krylov-proof");
    {
        let mut gpu = GpuEncoder::new(&mut enc, device);
        exec.execute_frame_with_state(&mut graph, &plan, time, &mut gpu, &mut state, 0);
    }
    enc.commit_and_wait_completed();

    let backend = exec.backend();
    let buffer = backend.array_buffer(backend.slot_for(x_res).expect("x pinned")).expect("x buffer");
    let ptr = buffer.mapped_ptr().expect("shared x buffer");
    // SAFETY: the frame completed; x holds n floats.
    let x = unsafe { std::slice::from_raw_parts(ptr.cast::<f32>().cast_const(), n) };
    x.iter().map(|&v| f64::from(v)).collect()
}

fn relative_residual(a: &[f64], b: &[f64], x: &[f64]) -> f64 {
    let ax = mat_vec(a, x);
    let r: Vec<f64> = ax.iter().zip(b).map(|(p, q)| p - q).collect();
    (dot(&r, &r) / dot(b, b)).sqrt()
}

fn check(n: usize, passes: usize, low_rank: bool, seed: u64) {
    let (a, b) = system(n, seed, low_rank);
    let want = reference_gmres(&a, &b, passes);
    let got = run_gpu(n, passes, &a, &b);
    assert!(got.iter().all(|v| v.is_finite()), "non-finite solution: {got:?}");
    let diff: Vec<f64> = got.iter().zip(&want).map(|(g, w)| g - w).collect();
    let rel = (dot(&diff, &diff) / dot(&want, &want)).sqrt();
    let (res_gpu, res_ref) = (relative_residual(&a, &b, &got), relative_residual(&a, &b, &want));
    println!("SWASH Krylov n {n}, {passes} passes: |x - x_ref|/|x_ref| {rel:.2e}, residual GPU {res_gpu:.2e} vs f64 {res_ref:.2e}");
    assert!(rel < 1e-4, "GPU GMRES differs from the f64 reference by {rel}");
    assert!(res_gpu < 2.0 * res_ref + 1e-5, "GPU residual {res_gpu} against f64 {res_ref}");
}

#[test]
fn swash_krylov_loop_matches_f64_gmres() {
    check(48, 12, false, 0x6d3e5);
}

/// A solve exact after three passes: the later passes see zero vectors, and
/// the guards keep them from turning into NaN.
#[test]
fn swash_krylov_loop_finishes_early_cleanly() {
    check(40, 8, true, 0x1a2b);
}

/// Nothing in the loop fuses: every vector a codegen atom writes is also read
/// by a reduction (node.dot_products), so it must be stored whole. When the
/// freeze compiler learns to fuse here, this fails: replace it with a frozen
/// against unfrozen comparison of x.
#[test]
fn swash_krylov_loop_has_no_fusable_pair() {
    assert!(
        fuse_generator_view(&def(48, 12), &registry()).is_none(),
        "the Krylov loop now fuses; prove the frozen loop matches the unfrozen one"
    );
}
