//! CPU size proof for the FFT water pressure solve, run before any GPU run of
//! it: at 64³ and 128³, every array the planner allocates covers the whole
//! extent its node dispatches over and indexes into. Extents are recomputed
//! here from each node's params, independently of the atoms' own guards.

use ahash::AHashMap;

use super::swash_preset::{PressureShape, pressure_def};
use crate::node_graph::effect_node::ParamValues;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::resource_allocation::plan_array_allocations;
use crate::node_graph::substeps::test_nodes::register_substep_test_nodes;
use crate::node_graph::{EffectGraphDefExt, ExecutionPlan, Graph, PrimitiveRegistry, ResourceId, compile};

fn registry() -> PrimitiveRegistry {
    let mut registry = PrimitiveRegistry::with_builtin();
    register_substep_test_nodes(&mut registry);
    registry
}

fn param(params: &ParamValues, name: &str) -> u64 {
    match params.get(name) {
        Some(ParamValue::Float(v)) => v.round() as u64,
        other => panic!("param {name} is {other:?}"),
    }
}

fn lattice(params: &ParamValues) -> [u64; 3] {
    ["nodes_x", "nodes_y", "nodes_z"].map(|name| param(params, name))
}

/// The half spectrum of a real FFT over x: (nx/2 + 1) · ny · nz, in either
/// transform mode (plane mode batches z, which still keeps every slice).
fn half(nodes: [u64; 3]) -> u64 {
    (nodes[0] / 2 + 1) * nodes[1] * nodes[2]
}

struct Sizes<'a> {
    graph: &'a Graph,
    plan: &'a ExecutionPlan,
    bytes: AHashMap<ResourceId, u64>,
}

impl Sizes<'_> {
    fn check(&self, shape: PressureShape) -> usize {
        let names: AHashMap<_, _> = self.graph.nodes().map(|n| (n.id, n)).collect();
        // Provided arrays: krylov_basis allocates them in run(), sized from its params.
        let mut provided = AHashMap::default();
        for step in self.plan.steps() {
            let node = names[&step.node];
            if node.node.type_id().as_str() == "node.krylov_basis" {
                let row = param(&node.params, "row_length") * 4;
                for (port, resource) in &step.outputs {
                    match *port {
                        "basis" => provided.insert(*resource, row * (param(&node.params, "passes") + 1)),
                        "current" => provided.insert(*resource, row),
                        _ => None,
                    };
                }
            }
        }
        let mut checked = 0;
        for step in self.plan.steps() {
            let node = names[&step.node];
            let ty = node.node.type_id().as_str();
            let p = &node.params;
            let size = |port: &str| -> Option<u64> {
                let resource = step.inputs.iter().chain(&step.outputs).find(|(name, _)| *name == port)?.1;
                Some(provided.get(&resource).or_else(|| self.bytes.get(&resource)).copied().unwrap_or(0))
            };
            let covers_if_bound = |port: &str, need: u64| {
                if let Some(have) = size(port) {
                    assert!(have >= need, "{} ({ty}) {port}: {have} bytes, reaches {need}", node.node_id.as_str());
                }
            };
            let covers = |port: &str, need: u64| {
                assert!(size(port).is_some(), "{} has no port {port}", node.node_id.as_str());
                covers_if_bound(port, need);
            };
            let cells = shape.cells() as u64 * 4;
            let row = shape.row_length() as u64 * 4;
            let entries = shape.capacity as u64;
            let planes = shape.planes() as u64 * 4;
            match ty {
                "test.value_source" | "test.value_sink" | "system.final_output" => continue,
                "node.collar_cells" => {
                    covers("water", cells);
                    covers("out", cells);
                }
                "node.running_total" => {
                    covers("in", cells);
                    covers("out", cells);
                }
                "node.select_flagged" => {
                    assert_eq!(param(p, "capacity"), entries);
                    covers("total", cells);
                    covers("out", entries * 4);
                }
                "node.smooth_lattice" => {
                    let n = lattice(p);
                    covers("levelset", n.iter().product::<u64>() * 4);
                    covers("smoothed", n.iter().product::<u64>() * 4);
                }
                "node.chart_entries" => {
                    assert_eq!(lattice(p).iter().product::<u64>() * 4, cells);
                    for port in ["water", "smoothed", "collar"] {
                        covers(port, cells);
                    }
                    covers("entries", entries * 4);
                    covers("out", entries * 32);
                }
                "node.chart_sums" => {
                    assert_eq!(lattice(p).iter().product::<u64>() * 4, cells);
                    covers("total", cells);
                    covers("entries", entries * 32);
                    covers("value", row);
                    let side = lattice(p).into_iter().max().unwrap();
                    covers("out", 6 * param(p, "sheets") * side * side * 4);
                }
                "node.chart_spread" => {
                    let side = lattice(p).into_iter().max().unwrap();
                    covers("planes", 6 * param(p, "sheets") * side * side * 4);
                    covers("entries", entries * 32);
                    covers("value", row);
                    covers("out", row);
                }
                "node.collar_source" => {
                    covers("total", cells);
                    covers("value", row);
                    covers("out", cells);
                }
                "node.collar_gather" => {
                    covers("entries", entries * 4);
                    covers("grid", cells);
                    covers("vector", 4);
                    covers("sum", 4);
                    covers("out", row);
                }
                "node.collar_pressure" => {
                    for port in ["water", "solved", "correction", "out"] {
                        covers(port, cells);
                    }
                    covers("vector", row);
                }
                "node.cosine_reorder" | "node.cosine_spectrum" | "node.cosine_half_spectrum"
                | "node.cosine_poisson_divide" | "node.cosine_surface_scale" | "node.fft_3d"
                | "node.inverse_fft_3d" => {
                    let n = lattice(p);
                    let (real, spectrum) = (n.iter().product::<u64>() * 4, half(n) * 8);
                    assert!(real == cells || real == planes, "{ty} on {n:?}");
                    let (input, output) = match ty {
                        "node.cosine_spectrum" => (("spectrum", spectrum), ("out", real)),
                        "node.cosine_half_spectrum" => (("values", real), ("spectrum", spectrum)),
                        "node.fft_3d" => (("values", real), ("spectrum", spectrum)),
                        "node.inverse_fft_3d" => (("spectrum", spectrum), ("values", real)),
                        _ => (("values", real), ("out", real)),
                    };
                    covers(input.0, input.1);
                    covers(output.0, output.1);
                }
                "node.dot_products" => {
                    let (length, max_rows) = (param(p, "row_length"), param(p, "max_rows"));
                    assert!(max_rows <= 64, "{}: more rows than the partials hold", node.node_id.as_str());
                    // A scalar-driven row count is capped at max_rows by run().
                    covers("matrix", max_rows * length * 4);
                    if step.inputs.iter().any(|(name, _)| *name == "vector") {
                        covers("vector", length * 4);
                    }
                    covers("out", max_rows * 4);
                }
                "node.combine_rows" => {
                    let length = param(p, "row_length");
                    assert_eq!(length * 4, row);
                    // Scalar-driven rows reach at most the basis height.
                    let rows = if step.inputs.iter().any(|(name, _)| *name == "rows") {
                        shape.passes as u64 + 1
                    } else {
                        param(p, "rows")
                    };
                    covers("base", row);
                    covers("out", row);
                    covers("matrix", rows * row);
                    covers("coef", rows.min(shape.passes as u64 + 1) * 4);
                }
                "node.divide_by_value" => {
                    covers("values", row);
                    covers("out", row);
                    covers("divisor", 4);
                }
                "node.krylov_basis" => {
                    let passes = param(p, "passes");
                    let state = (passes * passes + 4 * passes + 1) * 4;
                    assert_eq!(param(p, "row_length") * 4, row);
                    covers("seed", 4);
                    covers("start", row);
                    covers("in", state);
                    covers("next_in", row);
                    covers("out", state);
                    // Nothing reads the final vector, so the plan may leave it unbound.
                    covers_if_bound("last", row);
                    covers("basis", (passes + 1) * row);
                    covers("current", row);
                }
                "node.krylov_givens" => {
                    let passes = param(p, "passes");
                    let state = (passes * passes + 4 * passes + 1) * 4;
                    covers("state", state);
                    covers("out", state);
                    covers("first", (passes + 1) * 4);
                    covers("second", (passes + 1) * 4);
                    covers("norm", 4);
                }
                "node.krylov_solve" => {
                    let passes = param(p, "passes");
                    covers("state", (passes * passes + 4 * passes + 1) * 4);
                    covers("out", passes * 4);
                }
                other => panic!("no extent rule for {other}"),
            }
            checked += 1;
        }
        checked
    }
}

fn plan_for(shape: PressureShape) -> (Graph, ExecutionPlan) {
    let graph = pressure_def(shape).into_graph(&registry(), &Default::default()).expect("pressure def builds");
    let plan = compile(&graph).expect("pressure def compiles");
    (graph, plan)
}

#[test]
fn fft_water_pressure_arrays_cover_every_dispatch() {
    for n in [64, 128] {
        let shape = PressureShape::at(n);
        let (graph, plan) = plan_for(shape);
        assert_eq!(plan.substep_regions().len(), 1, "one Krylov region");
        let allocation = plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).expect("plan allocates");
        let bytes = allocation.storage.iter().map(|(&r, s)| (r, s.bytes)).collect();
        let checked = Sizes { graph: &graph, plan: &plan, bytes }.check(shape);
        assert!(checked > 60, "checked only {checked} nodes at {n}³");
    }
}

/// The Krylov region holds exactly one pass: the helper, the box solve, the
/// gather, two projection rounds, the norm and the Givens update.
#[test]
fn fft_water_pressure_region_is_one_pass() {
    let (graph, plan) = plan_for(PressureShape::at(64));
    let region = &plan.substep_regions()[0];
    let names: AHashMap<_, _> = graph.nodes().map(|n| (n.id, n.node_id.as_str().to_string())).collect();
    let mut body: Vec<String> = region.steps.iter().map(|&i| names[&plan.steps()[i].node].clone()).collect();
    body.sort();
    let mut want: Vec<String> = [
        "krylov", "helper_sums", "helper_spread", "pass_source", "sum_z", "w", "h1", "w1", "h2", "w2", "norm",
        "next", "givens",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .chain(["helper", "pass_box"].iter().flat_map(|prefix| {
        ["order", "fft", "cosine", "scale", "half", "ifft", "unorder"].iter().map(move |s| format!("{prefix}_{s}"))
    }))
    .collect();
    want.sort();
    assert_eq!(body, want);
}

/// `tests/fixtures/presets/fft_water_pressure.json` is the 64³ graph.
/// `UPDATE_SWASH_FRAGMENT=1` rewrites it.
#[test]
fn fft_water_pressure_fragment_is_current() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/presets/fft_water_pressure.json");
    let mut want = serde_json::to_string_pretty(&pressure_def(PressureShape::at(64))).expect("serialise");
    want.push('\n');
    if std::env::var("UPDATE_SWASH_FRAGMENT").is_ok() {
        std::fs::write(&path, &want).expect("write fragment");
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(have == want, "fft_water_pressure.json is stale; rerun with UPDATE_SWASH_FRAGMENT=1");
}
