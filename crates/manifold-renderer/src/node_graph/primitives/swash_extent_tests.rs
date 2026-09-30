//! CPU size proof for the FFT water pressure solve, run before any GPU run of
//! it: at 64³ and 128³, every array the planner allocates covers the whole
//! extent its node dispatches over and indexes into. Extents are recomputed
//! here from each node's params, independently of the atoms' own guards.

use ahash::AHashMap;

use super::sort_particles_into_cells::range_storage_bytes;
use super::swash_preset::{Clip, PressureShape, WaterScene, pressure_def, pressure_def_in, render_def, water_def};
use crate::node_graph::effect_node::ParamValues;
use crate::generators::mesh_common::MeshVertex;
use crate::node_graph::fluid_particles::{CellRange, FaceSample, FluidBlob, FluidParticle, bin_counts};
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::resource_allocation::{ArrayAllocationAction, ArrayAllocationPlan, plan_array_allocations};
use crate::node_graph::substeps::test_nodes::register_substep_test_nodes;
use crate::node_graph::validation::GraphError;
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
        let producer: AHashMap<ResourceId, _> =
            self.plan.steps().iter().flat_map(|step| step.outputs.iter().map(|(_, resource)| (*resource, step.node))).collect();
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
            // A wired lattice length (the active region, P3c) must stay within
            // the static one the arrays are sized for: it comes from a region
            // on the same lattice, or a constant no longer than it.
            let wired_lengths_fit = |ports: [&str; 3], lengths: [u64; 3]| {
                for (axis, port) in ports.into_iter().enumerate() {
                    let Some((_, resource)) = step.inputs.iter().find(|(name, _)| *name == port) else { continue };
                    let source = names[&producer[resource]];
                    match source.node.type_id().as_str() {
                        "node.active_region" => assert_eq!(
                            lattice(&source.params),
                            lengths,
                            "{} {port}: a region on another lattice",
                            node.node_id.as_str()
                        ),
                        "node.value" => {
                            let v = param(&source.params, "value");
                            assert!(v >= 2 && v.is_multiple_of(2) && v <= lengths[axis], "{} {port}: {v} past {lengths:?}", node.node_id.as_str());
                        }
                        other => panic!("{} {port} is wired from {other}", node.node_id.as_str()),
                    }
                }
            };
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
                "node.clamp_liquid_to_solids" => {
                    covers("levelset", surface().pow(3) * 4);
                    covers("clamped", surface().pow(3) * 4);
                    covers("solid", corners.pow(3) * 4);
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
                    covers("advect", faces);
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
                    // A window reads (forward) or writes (inverse) the whole
                    // lattice, which is this lattice.
                    if ty == "node.cosine_reorder" {
                        for (axis, name) in ["outer_x", "outer_y", "outer_z"].into_iter().enumerate() {
                            let outer = p.get(name).map_or(0, |_| param(p, name));
                            assert!(outer == 0 || outer == n[axis], "{} {name} {outer} is not its lattice {n:?}", node.node_id.as_str());
                        }
                    }
                    wired_lengths_fit(["nodes_x", "nodes_y", "nodes_z"], n);
                }
                // A frozen graph's fused cosine pair: member 0 is the
                // twiddle stage, gathering the half spectrum; the pair
                // counts, and writes, member 0's lattice.
                // The liquid surface's fused regions are the freeze compiler's
                // contract (BUG-2efy (fused output capacity probe)).
                "node.wgsl_compute" if !p.contains_key("n0_axes") => continue,
                "node.wgsl_compute" => {
                    let n = ["n0_nodes_x", "n0_nodes_y", "n0_nodes_z"].map(|name| param(p, name));
                    let real = n.iter().product::<u64>() * 4;
                    assert!(real == cells || real == planes, "{} on {n:?}", node.node_id.as_str());
                    covers("src_0", half(n) * 8);
                    covers("dst", real);
                    wired_lengths_fit(["n0_nodes_x", "n0_nodes_y", "n0_nodes_z"], n);
                    wired_lengths_fit(["n1_nodes_x", "n1_nodes_y", "n1_nodes_z"], n);
                }
                "node.occupied_bounds" => {
                    on_lattice();
                    covers("values", cells);
                }
                "node.active_region" => on_lattice(),
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
                    // Scalar-driven rows come from a Krylov region's boundary
                    // and reach at most its basis height.
                    let height = match step.inputs.iter().find(|(name, _)| *name == "rows") {
                        Some((_, resource)) => {
                            let source = names[&producer[resource]];
                            assert_eq!(source.node.type_id().as_str(), "node.krylov_basis", "{} rows", node.node_id.as_str());
                            Some(param(&source.params, "passes") + 1)
                        }
                        None => None,
                    };
                    let rows = height.unwrap_or_else(|| param(p, "rows"));
                    covers("base", row);
                    covers("out", row);
                    covers("matrix", rows * row);
                    covers("coef", rows * 4);
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

/// Every lattice a scene may use, 16 to 256, the mixed-radix sides between
/// the powers of two included. Each is proven here before any GPU run at it.
/// 512 is not runnable: its Dam Break is 176M particles, past
/// node.liquid_fill's 67M ceiling, and 257 GB of arrays.
const LATTICES: [usize; 8] = [16, 32, 48, 64, 80, 96, 128, 256];

fn plan_for(shape: PressureShape) -> (Graph, ExecutionPlan) {
    plan_in(shape, None)
}

fn plan_in(shape: PressureShape, clip: Option<Clip>) -> (Graph, ExecutionPlan) {
    let graph = pressure_def_in(shape, clip).into_graph(&registry(), &Default::default()).expect("pressure def builds");
    let plan = compile(&graph).expect("pressure def compiles");
    (graph, plan)
}

/// A window of half the lattice's side, a quarter in from the low corner.
fn half_clip(n: usize) -> Clip {
    let size = (n / 2).next_multiple_of(2);
    Clip { origin: [n / 4; 3], size: [size; 3] }
}

/// Every lattice at every pass count of the pass-count trend, on the whole
/// box and on a window.
#[test]
fn fft_water_pressure_arrays_cover_every_dispatch() {
    for (n, passes) in LATTICES.into_iter().flat_map(|n| super::swash_preset::TREND_PASSES.map(|p| (n, p))) {
        let shape = PressureShape { passes, ..PressureShape::at(n) };
        for clip in [None, Some(half_clip(n))] {
            let (graph, plan) = plan_in(shape, clip);
            assert_eq!(plan.substep_regions().len(), 1, "one Krylov region");
            let allocation = plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).expect("plan allocates");
            let bytes = allocation.storage.iter().map(|(&r, s)| (r, s.bytes)).collect();
            let checked = Sizes { graph: &graph, plan: &plan, bytes }.check(shape, 0, shape.n as u64 + 1);
            assert!(checked > 60, "checked only {checked} nodes at {n}³");
        }
    }
}

/// Every running scene at every lattice, and the probes' variants, before
/// any GPU run of it: each step's particle, face and cell arrays and its
/// solves.
#[test]
fn fft_water_scenes_cover_every_dispatch() {
    let scenes = [WaterScene::dam_break, WaterScene::still_pool, WaterScene::free_fall];
    let all = LATTICES.into_iter().flat_map(|n| scenes.map(|at| at(n))).flat_map(|scene| [scene, scene.with_surface()]);
    // The splash probes' scenes: the Krylov basis grows with passes, and
    // four steps a frame is four copies of the step.
    let refined = WaterScene::dam_break(128).with_surface();
    let bare = |n| WaterScene { spread_rate: 0.0, ..WaterScene::dam_break(n) };
    let step = WaterScene::dam_break(128);
    let probes = [
        refined.with_passes(16),
        refined.with_passes(32),
        WaterScene { steps: 4, ..refined },
        bare(64),
        bare(128).with_surface(),
        step.with_passes(32),
        step.with_passes(48),
        WaterScene { density_once: false, ..WaterScene::dam_break(64) }.with_surface(),
        WaterScene { steps: 1, spread_rate: super::swash_preset::SPREAD_PER_STEP * 60.0, ..WaterScene::dam_break(64) }.with_surface(),
    ];
    for scene in all.chain(probes) {
        let n = scene.pressure.n;
        let graph = water_def(scene).into_graph(&registry(), &Default::default()).expect("water def builds");
        let plan = compile(&graph).expect("water def compiles");
        assert_eq!(plan.substep_regions().len(), scene.steps + scene.density_solves(), "one Krylov region per solve");
        let allocation = plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).expect("plan allocates");
        let bytes = allocation.storage.iter().map(|(&r, s)| (r, s.bytes)).collect();
        let checked = Sizes { graph: &graph, plan: &plan, bytes }.check(scene.pressure, scene.particles(), scene.surface_nodes() as u64);
        assert!(checked > 70 * scene.steps, "checked only {checked} nodes at {n}³, {} steps", scene.steps);
        let meshed = plan.steps().iter().any(|step| {
            graph.nodes().any(|node| node.id == step.node && node.node.type_id().as_str() == "node.volume_surface_mesh")
        });
        assert_eq!(meshed, scene.surface, "the surface is in the plan exactly when asked for");
    }
}

/// A pass count past the Krylov kernels' local arrays is refused at build,
/// naming the Krylov node; it never runs as fewer passes.
#[test]
fn fft_water_refuses_passes_past_the_kernel_cap() {
    let scene = WaterScene::dam_break(64);
    let cap = super::krylov_givens::MAX_PASSES as usize;
    let build = |passes| water_def(scene.with_passes(passes)).into_graph(&registry(), &Default::default()).expect("water def builds");
    assert!(compile(&build(cap)).is_ok(), "{cap} passes compile");
    let graph = build(cap + 1);
    match compile(&graph) {
        Err(GraphError::IllegalParams { node, reason }) => {
            let kind = graph.get_node(node).expect("refused node exists").node.type_id().as_str().to_string();
            assert!(kind.contains("krylov") && reason.starts_with(&format!("passes {} ", cap + 1)), "refused by {kind}: {reason}");
        }
        other => panic!("{} passes must be refused at build, got {:?}", cap + 1, other.map(|_| "a plan")),
    }
}

/// Device bytes a scene holds inside the render graph at 1920×1080: every
/// array the planner allocates plus the Krylov bases and current vectors
/// each solve provides itself. Textures are not counted.
pub(super) fn rendered_scene_bytes(scene: WaterScene) -> u64 {
    let graph = render_def(scene).into_graph(&registry(), &Default::default()).expect("render def builds");
    let plan = compile(&graph).expect("render def compiles");
    let allocation = plan_array_allocations(&graph, &plan, (1920, 1080), &AHashMap::default()).expect("plan allocates");
    fresh(&allocation).values().sum::<u64>() + krylov_bytes(scene)
}

fn fresh_bytes_of(allocation: &ArrayAllocationPlan) -> u64 {
    fresh(allocation).values().sum()
}

/// Bytes of each fresh allocation, by the resource that owns it. A reuse or
/// alias shares an earlier allocation's memory, so summing `storage` would
/// count it twice.
fn fresh(allocation: &ArrayAllocationPlan) -> AHashMap<ResourceId, u64> {
    allocation
        .actions
        .iter()
        .filter_map(|action| match action {
            ArrayAllocationAction::Allocate(a) => Some((a.resource, a.bytes)),
            _ => None,
        })
        .collect()
}

fn krylov_bytes(scene: WaterScene) -> u64 {
    let row = (scene.pressure.capacity as u64 + 1) * 4;
    let pressure = scene.steps as u64 * (scene.pressure.passes as u64 + 2);
    let density = scene.density_solves() as u64 * (scene.density_passes as u64 + 2);
    row * (pressure + density)
}

/// The rendered Dam Break's arrays at every lattice, for the size ladder,
/// with the largest ports at the top one.
#[test]
fn fft_water_memory_at_every_lattice() {
    for n in LATTICES {
        for scale in [1, 2, 3] {
            let scene = WaterScene::dam_break(n).with_surface_scale(scale);
            let bytes = rendered_scene_bytes(scene);
            println!(
                "SWASH rendered Dam Break {n}³, surface scale {scale}: {} particles, arrays {:.2} GB",
                scene.particles(),
                bytes as f64 / 1e9
            );
            assert!(bytes > 0);
        }
    }
    let n = LATTICES[LATTICES.len() - 1];
    let scene = WaterScene::dam_break(n).with_surface_scale(1);
    let graph = render_def(scene).into_graph(&registry(), &Default::default()).expect("render def builds");
    let plan = compile(&graph).expect("render def compiles");
    let allocation = plan_array_allocations(&graph, &plan, (1920, 1080), &AHashMap::default()).expect("plan allocates");
    let names: AHashMap<_, _> = graph.nodes().map(|node| (node.id, node.node_id.as_str().to_string())).collect();
    let zeroed = allocation.actions.iter().filter(|a| matches!(a, ArrayAllocationAction::Allocate(a) if a.zero_init)).count();
    let aliased = allocation.actions.iter().filter(|a| matches!(a, ArrayAllocationAction::Alias { .. })).count();
    let freed: usize = plan.steps().iter().map(|step| step.free_after.len()).sum();
    let fresh = fresh(&allocation);
    println!("SWASH {n}³ surface scale 1: {} fresh arrays ({zeroed} zero-filled), {aliased} reused, {freed} frees in the plan", fresh.len());
    let bare = water_def(scene).into_graph(&registry(), &Default::default()).expect("water def builds");
    let bare_plan = compile(&bare).expect("water def compiles");
    let bare_allocation = plan_array_allocations(&bare, &bare_plan, (64, 64), &AHashMap::default()).expect("plan allocates");
    let bare_aliased = bare_allocation.actions.iter().filter(|a| matches!(a, ArrayAllocationAction::Alias { .. })).count();
    println!(
        "SWASH {n}³ surface scale 1 without the render: {:.2} GB fresh, {bare_aliased} reused",
        fresh_bytes_of(&bare_allocation) as f64 / 1e9
    );
    let ports: Vec<(String, u64)> = plan
        .steps()
        .iter()
        .flat_map(|step| step.outputs.iter().map(move |(port, resource)| (step.node, *port, *resource)))
        .filter_map(|(node, port, resource)| fresh.get(&resource).map(|&bytes| (format!("{}.{port}", names[&node]), bytes)))
        .collect();
    // Grouped by what the port is, across both steps' copies.
    let mut kinds: AHashMap<String, (u64, usize)> = AHashMap::default();
    for (port, bytes) in &ports {
        let kind = port.split_once('.').filter(|(p, _)| p.starts_with('s') && p[1..].parse::<u32>().is_ok()).map_or(port.as_str(), |(_, rest)| rest);
        let entry = kinds.entry(kind.to_string()).or_default();
        entry.0 += bytes;
        entry.1 += 1;
    }
    let mut kinds: Vec<_> = kinds.into_iter().collect();
    kinds.sort_by_key(|(_, (bytes, _))| std::cmp::Reverse(*bytes));
    for (kind, (bytes, copies)) in kinds.iter().take(16) {
        println!("SWASH {n}³ surface scale 1, {kind} ×{copies}: {:.2} GB", *bytes as f64 / 1e9);
    }
}

/// What the collar capacity costs in memory: every array the planner
/// allocates for the meshed Dam Break, plus the Krylov bases and current
/// vectors each solve provides itself, at today's 8n² and at the proven
/// bound 6n³/7 (a collar cell is air beside water, and at most six air
/// cells in seven can touch water).
#[test]
fn fft_water_collar_capacity_memory() {
    let mb = |b: u64| b as f64 / 1e6;
    for n in [64, 128] {
        let mut by_port: Vec<AHashMap<String, u64>> = Vec::new();
        for capacity in [8 * n * n, 6 * n * n * n / 7] {
            let scene = WaterScene::dam_break(n).with_surface();
            let scene = WaterScene { pressure: PressureShape { capacity, ..scene.pressure }, ..scene };
            let graph = water_def(scene).into_graph(&registry(), &Default::default()).expect("water def builds");
            let plan = compile(&graph).expect("water def compiles");
            let allocation = plan_array_allocations(&graph, &plan, (64, 64), &AHashMap::default()).expect("plan allocates");
            let fresh = fresh(&allocation);
            let planned: u64 = fresh.values().sum();
            let provided = krylov_bytes(scene);
            println!("{n}³ capacity {capacity}: planned {:.0} MB, Krylov bases {:.0} MB, total {:.0} MB", mb(planned), mb(provided), mb(planned + provided));
            let names: AHashMap<_, _> = graph.nodes().map(|node| (node.id, node.node.type_id().as_str().to_string())).collect();
            let mut ports = AHashMap::default();
            for step in plan.steps() {
                for (port, resource) in &step.outputs {
                    if let Some(bytes) = fresh.get(resource) {
                        *ports.entry(format!("{}.{port}", names[&step.node])).or_default() += bytes;
                    }
                }
            }
            by_port.push(ports);
        }
        let mut growth: Vec<(String, u64)> =
            by_port[1].iter().map(|(k, &v)| (k.clone(), v.saturating_sub(by_port[0].get(k).copied().unwrap_or(0)))).collect();
        growth.sort_by_key(|(_, g)| std::cmp::Reverse(*g));
        for (port, g) in growth.iter().take(8) {
            println!("{n}³ growth {port}: +{:.0} MB", mb(*g));
        }
    }
}

/// A lattice the FFT atoms can't transform is refused once, at build, naming
/// the transform, never frame by frame: an odd side can't pair the cosine
/// reorder's nodes.
#[test]
fn fft_water_refuses_an_illegal_lattice_at_build() {
    for n in [63, 81, 97] {
        let graph = water_def(WaterScene::dam_break(n)).into_graph(&registry(), &Default::default()).expect("water def builds");
        match compile(&graph) {
            Err(GraphError::IllegalParams { node, reason }) => {
                let kind = graph.get_node(node).expect("refused node exists").node.type_id().as_str().to_string();
                assert!(kind.contains("fft_3d") && reason.contains("even"), "{n}³ refused by {kind}: {reason}");
            }
            other => panic!("{n}³ must be refused at build, got {:?}", other.map(|_| "a plan")),
        }
    }
}

/// The scenes as the render smoke runs them, inside the shipped render graph
/// (`render_def`), at every lattice: the same coverage as the bare scenes,
/// with the render around them.
#[test]
fn fft_water_rendered_scenes_cover_every_dispatch() {
    let scenes = [WaterScene::dam_break, WaterScene::still_pool];
    let coarser = LATTICES.into_iter().flat_map(|n| [1, 2].map(|scale| WaterScene::dam_break(n).with_surface_scale(scale)));
    // The cadence probes: the density solve every step, one step a frame.
    let cadence = [
        WaterScene { density_once: false, ..WaterScene::dam_break(64) },
        WaterScene { steps: 1, spread_rate: super::swash_preset::SPREAD_PER_STEP * 60.0, ..WaterScene::dam_break(64) },
    ];
    for scene in LATTICES.into_iter().flat_map(|n| scenes.map(|at| at(n))).chain(coarser).chain(cadence) {
        let n = scene.pressure.n;
        let graph = render_def(scene).into_graph(&registry(), &Default::default()).expect("render def builds");
        let plan = compile(&graph).expect("render def compiles");
        assert_eq!(plan.substep_regions().len(), scene.steps + scene.density_solves(), "one Krylov region per solve");
        let allocation = plan_array_allocations(&graph, &plan, (1920, 1080), &AHashMap::default()).expect("plan allocates");
        let bytes = allocation.storage.iter().map(|(&r, s)| (r, s.bytes)).collect();
        let corners = scene.surface_nodes() as u64;
        let checked = Sizes { graph: &graph, plan: &plan, bytes }.check(scene.pressure, scene.particles(), corners);
        assert!(checked > 70 * scene.steps, "checked only {checked} nodes at {n}³, {} steps", scene.steps);
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

/// Each fused region of `def` as its members' node ids, `a + b`.
fn fused_regions(def: &manifold_core::effect_graph_def::EffectGraphDef) -> Vec<String> {
    let report = crate::node_graph::fusion_report(def, &registry());
    let name = |id: u32| def.nodes.iter().find(|n| n.id == id).map_or("?".to_string(), |n| n.node_id.as_str().to_string());
    report.regions.iter().map(|r| r.member_node_ids.iter().map(|&id| name(id)).collect::<Vec<_>>().join(" + ")).collect()
}

/// `def` frozen as the app renders it, built and planned.
fn frozen(def: &manifold_core::effect_graph_def::EffectGraphDef, size: (u32, u32)) -> (Graph, ExecutionPlan, AHashMap<ResourceId, u64>) {
    let view = crate::node_graph::freeze::install::fuse_generator_view(def, &registry()).expect("the def fuses and its fused def builds");
    let graph = (*view.def).clone().into_graph(&registry(), &view.mesh_rules).expect("fused def builds");
    let plan = compile(&graph).expect("fused def compiles");
    let allocation = plan_array_allocations(&graph, &plan, size, &AHashMap::default()).expect("plan allocates");
    let bytes = allocation.storage.iter().map(|(&r, s)| (r, s.bytes)).collect();
    (graph, plan, bytes)
}

/// The frozen graphs at every lattice, before any GPU run of them: the solve
/// alone, the running scene bare and meshed, and the render graph. Each
/// fused cosine pair's gathered half spectrum and its output cover the
/// lattice it counts.
#[test]
fn fft_water_frozen_graphs_cover_every_dispatch() {
    for n in LATTICES {
        let shape = PressureShape::at(n);
        for clip in [None, Some(half_clip(n))] {
            let (graph, plan, bytes) = frozen(&pressure_def_in(shape, clip), (64, 64));
            let fused = graph.nodes().filter(|node| node.node.type_id().as_str() == "node.wgsl_compute").count();
            assert_eq!(fused, 5, "the solve's five cosine pairs at {n}³");
            // A window reaches every box-solve atom, fused ones included.
            let windowed = plan
                .steps()
                .iter()
                .filter(|step| ["n0_nodes_x", "nodes_x"].iter().any(|port| step.inputs.iter().any(|(name, _)| name == port)))
                .count();
            assert_eq!(windowed, if clip.is_some() { 3 * 6 } else { 0 }, "windowed steps at {n}³");
            assert!(Sizes { graph: &graph, plan: &plan, bytes }.check(shape, 0, shape.n as u64 + 1) > 50);
        }
        let scene = WaterScene::dam_break(n);
        for scene in [scene, scene.with_surface()] {
            let (graph, plan, bytes) = frozen(&water_def(scene), (64, 64));
            let checked = Sizes { graph: &graph, plan: &plan, bytes }.check(scene.pressure, scene.particles(), scene.surface_nodes() as u64);
            assert!(checked > 70 * scene.steps, "checked only {checked} nodes at {n}³");
        }
        let (graph, plan, bytes) = frozen(&render_def(scene), (1920, 1080));
        assert!(Sizes { graph: &graph, plan: &plan, bytes }.check(scene.pressure, scene.particles(), scene.surface_nodes() as u64) > 140);
    }
}

/// The regions one solve fuses: every cosine transform's twiddle stage folds
/// into what reads its lattice-sized output, the box's eigenvalue divide or
/// the helper's plane scale. The count is the lattice's node product, a
/// uniform the fused kernel reads (BUG-u8io, fft-water-fusion-param-capacity).
fn solve_regions(prefix: &str) -> Vec<String> {
    ["rhs_box", "helper", "pass_box", "final_helper", "final_box"]
        .iter()
        .map(|stage| format!("{prefix}{stage}_cosine + {prefix}{stage}_scale"))
        .collect()
}

/// The solve's fused regions; `fft_water_frozen_solve_matches_unfrozen`
/// proves them on the GPU.
#[test]
fn fft_water_pressure_fused_regions() {
    let mut fused = fused_regions(&pressure_def(PressureShape::at(64)));
    let mut want = solve_regions("");
    fused.sort();
    want.sort();
    assert_eq!(fused, want);
}

/// The water step's fused regions: the solves' pairs and nothing else.
/// cells_with_particles → face_divergence stays apart, because the water
/// lattice fans out to the collar, the projections and the density source,
/// and a buffer region has one output. Every other edge ends at a gather or
/// crosses a solve. `fft_water_frozen_step_matches_unfrozen` proves them.
#[test]
fn fft_water_step_fused_regions() {
    let scene = WaterScene::dam_break(64);
    let mut fused = fused_regions(&water_def(scene));
    let mut want: Vec<String> = ["s0.", "s1.", "s1.density."].iter().flat_map(|p| solve_regions(p)).collect();
    fused.sort();
    want.sort();
    assert_eq!(fused, want, "density once a frame, on the last step");
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
