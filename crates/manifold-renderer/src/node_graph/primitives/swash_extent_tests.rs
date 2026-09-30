//! CPU size proof for the FFT water pressure solve, run before any GPU run of
//! it: at 64³ and 128³, every array the planner allocates covers the whole
//! extent its node dispatches over and indexes into. Extents are recomputed
//! here from each node's params, independently of the atoms' own guards.

use ahash::AHashMap;

use super::sort_particles_into_cells::range_storage_bytes;
use super::swash_preset::{PressureShape, WaterScene, pressure_def, render_def, water_def};
use crate::node_graph::effect_node::ParamValues;
use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::fluid_particles::{CellRange, FaceSample, FluidBlob, FluidParticle, bin_counts};
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::resource_allocation::plan_array_allocations;
use crate::node_graph::substeps::test_nodes::register_substep_test_nodes;
use crate::node_graph::{EffectGraphDefExt, ExecutionPlan, Graph, PrimitiveRegistry, ResourceId, compile};

const PARTICLE: u64 = std::mem::size_of::<FluidParticle>() as u64;
const FACE: u64 = std::mem::size_of::<FaceSample>() as u64;
const RANGE: u64 = std::mem::size_of::<CellRange>() as u64;
const BLOB: u64 = std::mem::size_of::<FluidBlob>() as u64;
const VERTEX: u64 = std::mem::size_of::<MeshVertex>() as u64;

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

/// The sort's bin grid from its params, as its run() computes it.
fn sort_bins(params: &ParamValues) -> [u32; 3] {
    let size = ["size_x", "size_y", "size_z"].map(|name| match params.get(name) {
        Some(ParamValue::Float(v)) => *v,
        other => panic!("param {name} is {other:?}"),
    });
    let cell = match params.get("cell_size") {
        Some(ParamValue::Float(v)) => *v,
        other => panic!("param cell_size is {other:?}"),
    };
    bin_counts(size, cell)
}

impl Sizes<'_> {
    /// `particles` is the liquid's particle count; 0 for the bare solve.
    /// `corners` is the surface's solid lattice nodes per axis
    /// (`WaterScene::surface_nodes`).
    fn check(&self, shape: PressureShape, particles: u64, corners: u64) -> usize {
        let names: AHashMap<_, _> = self.graph.nodes().map(|n| (n.id, n)).collect();
        let wired = |step: &crate::node_graph::ExecutionStep, port: &str| step.inputs.iter().any(|(name, _)| *name == port);
        // Provided arrays are allocated in run(), sized from params:
        // krylov_basis's vectors, the fill's particles, the step sorts'
        // ranges. The surface's sort takes its bin size from a wire and
        // sizes its own ranges; its readers check them (searched_bins).
        let mut provided = AHashMap::default();
        for step in self.plan.steps() {
            let node = names[&step.node];
            let p = &node.params;
            for (port, resource) in &step.outputs {
                let bytes = match (node.node.type_id().as_str(), *port) {
                    ("node.krylov_basis", "basis") => param(p, "row_length") * 4 * (param(p, "passes") + 1),
                    ("node.krylov_basis", "current") => param(p, "row_length") * 4,
                    ("node.liquid_fill", "particles") => param(p, "max_capacity") * PARTICLE,
                    ("node.sort_particles_into_cells", "cell_ranges") if !wired(step, "cell_size") => {
                        range_storage_bytes(sort_bins(p))
                    }
                    _ => continue,
                };
                provided.insert(*resource, bytes);
            }
        }
        // The surface lattice: the solid lattice, refined resolution_scale
        // times per cell by particle_volume.
        let refined = self
            .graph
            .nodes()
            .find(|n| n.node.type_id().as_str() == "node.particle_volume")
            .map(|n| (corners - 1) * param(&n.params, "resolution_scale") + 1);
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
            let side = shape.n as u64;
            let faces = (side + 1).pow(3) * FACE;
            let ranges = side.pow(3) * RANGE;
            let particle_bytes = particles * PARTICLE;
            let on_lattice = || assert_eq!(lattice(p), [side; 3], "{} is off the lattice", node.node_id.as_str());
            let surface = || refined.expect("a surface node without particle_volume");
            match ty {
                "test.value_source" | "test.value_sink" | "test.liquid_sink" | "test.mesh_sink" | "system.final_output"
                | "node.transform_3d" | "node.transform_components" | "node.value" | "node.math" => continue,
                // The shipped render graph of WaterDamBreakGpu.json around the
                // surface (render_def): textures, scene objects and lights; the
                // mesh it draws is checked at volume_surface_mesh.
                "system.generator_input" | "node.orbit_camera" | "node.bake_environment" | "node.light"
                | "node.pbr_material" | "node.scene_object" | "node.cube_mesh" | "node.render_scene"
                | "node.tone_map" | "node.hdri_source" | "node.exposure" | "node.switch_texture" => continue,
                "node.sort_particles_into_cells" if wired(step, "cell_size") => {
                    covers("particles", particle_bytes);
                    covers("sorted", particle_bytes);
                    covers_if_bound("order", particles * 4);
                }
                "node.shape_particle_blobs" => {
                    covers("sorted", particle_bytes);
                    covers("blobs", particles * BLOB);
                }
                "node.particle_volume" => {
                    covers("solid", corners.pow(3) * 4);
                    covers("blobs", particles * BLOB);
                    covers("levelset", surface().pow(3) * 4);
                }
                "node.smooth_lattice" if wired(step, "nodes_x") => {
                    covers("levelset", surface().pow(3) * 4);
                    covers("smoothed", surface().pow(3) * 4);
                }
                "node.count_surface_triangles" => {
                    covers("levelset", surface().pow(3) * 4);
                    covers("counts", (surface() - 1).pow(3) * 4);
                }
                "node.volume_surface_mesh" => {
                    covers("levelset", surface().pow(3) * 4);
                    covers("scan", (surface() - 1).pow(3) * 4);
                    covers("vertices", param(p, "max_capacity") * VERTEX);
                }
                "node.liquid_fill" => {
                    on_lattice();
                    assert_eq!(param(p, "max_capacity"), particles, "the fill holds every particle it places");
                    covers("particles", particle_bytes);
                }
                "node.liquid_feedback" => {
                    for port in ["in", "seed", "out"] {
                        covers(port, particle_bytes);
                    }
                }
                "node.sort_particles_into_cells" => {
                    assert_eq!(sort_bins(p), [shape.n as u32; 3], "the sort bins by the lattice's cells");
                    covers("particles", particle_bytes);
                    covers("sorted", particle_bytes);
                    // Nothing reads the order, so the plan may leave it unbound.
                    covers_if_bound("order", particles * 4);
                    covers("cell_ranges", ranges);
                }
                "node.cells_with_particles" => {
                    on_lattice();
                    covers("cell_ranges", ranges);
                    covers("out", cells);
                }
                "node.particles_to_faces" => {
                    on_lattice();
                    covers("sorted", particle_bytes);
                    covers("cell_ranges", ranges);
                    covers("out", faces);
                }
                "node.extend_faces" | "node.face_gravity" => {
                    on_lattice();
                    covers("faces", faces);
                    covers("out", faces);
                }
                "node.density_source" => {
                    on_lattice();
                    covers("divergence", cells);
                    covers("cell_ranges", ranges);
                    covers("out", cells);
                }
                "node.face_divergence" => {
                    on_lattice();
                    covers("faces", faces);
                    covers("water", cells);
                    covers("out", cells);
                }
                "node.subtract_pressure" => {
                    on_lattice();
                    covers("faces", faces);
                    covers("pressure", cells);
                    covers("water", cells);
                    covers("out", faces);
                }
                "node.faces_to_particles" => {
                    on_lattice();
                    covers("particles", particle_bytes);
                    covers("out", particle_bytes);
                    covers("faces", faces);
                    covers("old", faces);
                }
                "node.collar_cells" => {
                    covers("water", cells);
                    covers("out", cells);
                }
                "node.running_total" => {
                    covers("in", cells);
                    covers("out", size("in").unwrap_or(0));
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

/// Every shape the GPU proofs run: both lattices, every pass count of the
/// pass-count trend.
#[test]
fn fft_water_pressure_arrays_cover_every_dispatch() {
    for (n, passes) in [64, 96, 128].into_iter().flat_map(|n| super::swash_preset::TREND_PASSES.map(|p| (n, p))) {
        let shape = PressureShape { passes, ..PressureShape::at(n) };
        let (graph, plan) = plan_for(shape);
        assert_eq!(plan.substep_regions().len(), 1, "one Krylov region");
        let allocation = plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).expect("plan allocates");
        let bytes = allocation.storage.iter().map(|(&r, s)| (r, s.bytes)).collect();
        let checked = Sizes { graph: &graph, plan: &plan, bytes }.check(shape, 0, shape.n as u64 + 1);
        assert!(checked > 60, "checked only {checked} nodes at {n}³");
    }
}

/// Every running scene the GPU proofs and probes run, at both lattices,
/// before any GPU run of it: each step's particle, face and cell arrays and
/// its solve.
#[test]
fn fft_water_scenes_cover_every_dispatch() {
    let scenes = [WaterScene::dam_break, WaterScene::still_pool, WaterScene::free_fall];
    let all = [64, 96, 128]
        .into_iter()
        .flat_map(|n| scenes.map(|at| at(n)))
        .flat_map(|scene| [scene, scene.with_surface(), scene.with_closed_surface()]);
    for scene in all {
        let n = scene.pressure.n;
        let graph = water_def(scene).into_graph(&registry(), &Default::default()).expect("water def builds");
        let plan = compile(&graph).expect("water def compiles");
        assert_eq!(plan.substep_regions().len(), scene.steps, "one Krylov region per step");
        let allocation = plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).expect("plan allocates");
        let bytes = allocation.storage.iter().map(|(&r, s)| (r, s.bytes)).collect();
        let checked = Sizes { graph: &graph, plan: &plan, bytes }.check(scene.pressure, scene.particles(), scene.surface_nodes() as u64);
        assert!(checked > 150, "checked only {checked} nodes at {n}³");
        let meshed = plan.steps().iter().any(|step| {
            graph.nodes().any(|node| node.id == step.node && node.node.type_id().as_str() == "node.volume_surface_mesh")
        });
        assert_eq!(meshed, scene.surface, "the surface is in the plan exactly when asked for");
    }
}

/// The scenes as the render smoke runs them, inside the shipped render graph
/// (`render_def`), at every lattice it runs: the same coverage as the bare
/// scenes, with the render around them.
#[test]
fn fft_water_rendered_scenes_cover_every_dispatch() {
    let scenes = [WaterScene::dam_break, WaterScene::still_pool];
    for scene in [64, 96, 128].into_iter().flat_map(|n| scenes.map(|at| at(n))) {
        let n = scene.pressure.n;
        let graph = render_def(scene).into_graph(&registry(), &Default::default()).expect("render def builds");
        let plan = compile(&graph).expect("render def compiles");
        assert_eq!(plan.substep_regions().len(), scene.steps, "one Krylov region per step");
        let allocation = plan_array_allocations(&graph, &plan, (1920, 1080), &AHashMap::default()).expect("plan allocates");
        let bytes = allocation.storage.iter().map(|(&r, s)| (r, s.bytes)).collect();
        let corners = scene.with_closed_surface().surface_nodes() as u64;
        let checked = Sizes { graph: &graph, plan: &plan, bytes }.check(scene.pressure, scene.particles(), corners);
        assert!(checked > 150, "checked only {checked} nodes at {n}³");
    }
}

/// The fill is the engine's: its site rule on the engine's boxes.
#[test]
fn fft_water_dam_break_fill_matches_the_engine_boxes() {
    let at64 = WaterScene::dam_break(64);
    assert_eq!((at64.pool_sites(), at64.box_sites()), (5, [[5, 43], [5, 67], [8, 120]]));
    assert_eq!(at64.particles(), 128 * 5 * 128 + 38 * 62 * 112);
    let at128 = WaterScene::dam_break(128);
    assert_eq!((at128.pool_sites(), at128.box_sites()), (10, [[10, 86], [10, 133], [16, 240]]));
    // The still pool is 1 m of floor and no box; the falling block is 1 m on a side.
    let pool = WaterScene::still_pool(64);
    assert_eq!((pool.pool_sites(), pool.particles()), (32, 128 * 32 * 128));
    let block = WaterScene::free_fall(64);
    assert_eq!((block.pool_sites(), block.particles()), (0, 32 * 32 * 32));
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

/// Nothing in the solve fuses yet. The design's two pairs, cosine_spectrum →
/// cosine_poisson_divide and cosine_spectrum → cosine_surface_scale, are
/// refused because cosine_spectrum's output is sized by its lattice params,
/// which the fused count anchor cannot express: BUG-u8io
/// (fft-water-fusion-param-capacity). When this fails, fusion has learned it:
/// replace this with a frozen-against-unfrozen run of fft_water_matches_reference.
#[test]
fn fft_water_pressure_has_no_fused_region() {
    let report = crate::node_graph::fusion_report(&pressure_def(PressureShape::at(64)), &registry());
    let fused: Vec<_> = report.regions.iter().map(|r| &r.member_node_ids).collect();
    assert!(fused.is_empty(), "the pressure solve now fuses {fused:?}; prove the frozen solve matches the unfrozen one");
}

/// Nothing in the water step fuses yet. Most of its edges end at a gather
/// input (particles_to_faces, extend_faces, face_divergence's faces,
/// subtract_pressure's pressure, faces_to_particles' faces), which is a
/// fusion cut by design; gravity → subtract_pressure is cut because the
/// pressure between them depends on gravity. The pairs codegen could fuse,
/// cells_with_particles → face_divergence's coincident water and
/// face_divergence → density_source, are refused because every one of them
/// is sized by lattice params: BUG-u8io
/// (fft-water-fusion-param-capacity). When this fails, fusion has learned
/// it: prove the frozen step matches the unfrozen one.
#[test]
fn fft_water_step_has_no_fused_region() {
    let report = crate::node_graph::fusion_report(&water_def(WaterScene::dam_break(64)), &registry());
    let fused: Vec<_> = report.regions.iter().map(|r| &r.member_node_ids).collect();
    assert!(fused.is_empty(), "the water step now fuses {fused:?}; prove the frozen step matches the unfrozen one");
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
