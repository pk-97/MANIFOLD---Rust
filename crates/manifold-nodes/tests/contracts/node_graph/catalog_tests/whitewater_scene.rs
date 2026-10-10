//! The Whitewater chain on GPU FLIP's Dam Break (docs/GPU_WHITEWATER_DESIGN.md
//! P5, P6). Until the chain moves into the GPU FLIP Dam Break preset
//! (BUG-imy3.4 (whitewater P5 preset remainder)) it is wired straight to its
//! atoms on the Rust scene builder and drawn by the engine preset's own foam,
//! bubble and spray objects. `gpu_flip_builder_whitewater_emits` runs it as the
//! app would, frozen; `whitewater_emitter_matches_flip` (O2,
//! `whitewater-oracle`) holds the GPU emitter to FLIP's on fields the scene
//! captures.


use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::{Param, ParamManifest};
use serde_json::{Value, json};

use manifold_nodes_water::presets::gpu_flip::{WaterScene, render_def};
use manifold_nodes_water::liquid::grid::face_bytes;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::testkit::substep_nodes::register_substep_test_nodes;
use manifold_node_engine::persistence::PrimitiveRegistry;



const LIFECYCLE_REPORTS: [&str; 8] =
    ["foam_count", "bubble_count", "spray_count", "emitted", "thinned", "dropped_ticks", "lifecycle_ms", "worker_ms"];


/// CPU-only regression: the probed scene must remain compilable after fusion.
#[test]
fn whitewater_scene_fuses_without_gpu() {
    use manifold_node_engine::{persistence::EffectGraphDefExt, exec::execution_plan::compile};
    use manifold_node_engine::freeze::install::fuse_generator_view;

    let def = whitewater_render_def(WaterScene::dam_break(64));
    let mut registry = PrimitiveRegistry::with_builtin();
    register_substep_test_nodes(&mut registry);
    registry.register(PROBE, || Box::new(Probe::new()));
    registry.register(COUNTS_PROBE, || Box::new(Probe::whitewater_counts()));
    let report = manifold_node_engine::freeze::fusion_report(&def, &registry);
    assert_eq!(report.regions.len(), 3, "the surface chain, Fill Pits and the display blend must remain fusable");
    let members: Vec<_> = report.nodes.iter().filter(|node| node.fused).map(|node| node.type_id.as_str()).collect();
    assert_eq!(members, [
        "node.smooth_lattice", "node.clamp_liquid_to_solids",
        "node.redistance_lattice", "node.offset_lattice",
        "node.interpolate_particle_frames", "node.push_out_of_solid",
    ]);
    let graph = def.clone().into_graph(&registry, &Default::default()).expect("authored scene loads");
    let plan = compile(&graph).expect("probes must not escape the authored tick region");
    let state = graph.nodes().find(|n| n.node_id.as_str() == "state").expect("the liquid boundary").id;
    assert!(plan.steps().iter().find(|s| s.node == state).unwrap().outputs.iter().any(|(port, _)| *port == "whitewater_counts"),
        "the counts must stay bound for late capture");
    let fused = fuse_generator_view(&def, &registry).expect("the whitewater scene must fuse");
    let graph = (*fused.def).clone().into_graph(&registry, &fused.mesh_rules).expect("fused scene loads");
    let plan = compile(&graph).expect("probes must not escape the fused tick region");
    let state = graph.nodes().find(|n| n.node_id.as_str() == "state").expect("the liquid boundary survives fusion").id;
    assert!(plan.steps().iter().find(|s| s.node == state).unwrap().outputs.iter().any(|(port, _)| *port == "whitewater_counts"),
        "fusion must keep the counts bound for late capture");
}

/// The vendored lifecycle replaces the preset's `node.whitewater_step` at the
/// same document id. The vendored lifecycle takes Capacity as a param, while
/// the current step takes a scalar port: the splice must adapt that interface
/// and retain the card binding to the lifecycle's real capacity control.
#[test]
fn vendored_whitewater_scene_loads_and_compiles_without_gpu() {
    use manifold_node_engine::{persistence::EffectGraphDefExt, exec::execution_plan::compile};
    use manifold_node_engine::freeze::install::fuse_generator_view;

    let def = vendored_render_def(WaterScene::dam_break(16));
    let group = manifold_core::effect_graph_def::find_node(&def.nodes, "whitewater").expect("the vendored whitewater group");
    let body = group.group.as_ref().expect("whitewater is a group");
    assert!(!body.interface.inputs.iter().any(|input| input.name == "capacity"),
        "the lifecycle uses a param, not a silently unused capacity input");
    let scope_wires = node_scope_wires(&def.nodes, &def.wires, "whitewater").expect("vendored group scope");
    assert!(!scope_wires.iter().any(|wire| wire.to_node == group.id && wire.to_port == "capacity"),
        "the step-only capacity wire must be removed by the fixture splice");
    let budget = def.preset_metadata.as_ref().unwrap().bindings.iter()
        .find(|binding| binding.id == "whitewater_capacity").expect("budget binding");
    assert_eq!(budget.target, manifold_core::effect_graph_def::BindingTarget::Node {
        node_id: "ww.lifecycle".into(), param: "capacity".into(),
    }, "the budget card must still drive the lifecycle");
    let capacity = body.interface.params.iter().find(|param| param.name == "capacity").expect("the Capacity group param");
    assert_eq!(capacity.target_handle, "Whitewater Lifecycle");
    assert_eq!(capacity.target_param, "capacity");

    let mut registry = PrimitiveRegistry::with_builtin();
    register_substep_test_nodes(&mut registry);
    registry.register(PROBE, || Box::new(Probe::new()));
    registry.register(COUNTS_PROBE, || Box::new(Probe::whitewater_counts()));
    let graph = def.clone().into_graph(&registry, &Default::default()).expect("vendored whitewater scene loads");
    let lifecycle = graph.nodes().find(|node| node.node_id.as_str() == "ww.lifecycle").expect("the lifecycle survives flattening");
    assert_eq!(lifecycle.params.get("capacity").and_then(ParamValue::as_scalar), Some(100_000.0),
        "the group's Capacity param must route to the lifecycle");
    let plan = compile(&graph).expect("vendored whitewater scene compiles");
    assert!(plan.steps().iter().any(|step| step.node == lifecycle.id), "the lifecycle must remain in the compiled plan");
    let fused = fuse_generator_view(&def, &registry).expect("the vendored whitewater scene must fuse");
    let fused_graph = (*fused.def).clone().into_graph(&registry, &fused.mesh_rules).expect("fused vendored whitewater scene loads");
    compile(&fused_graph).expect("fused vendored whitewater scene compiles");
}

/// The preset's Whitewater group before `node.whitewater_step` replaced it:
/// the GPU emitter atoms feeding the vendored lifecycle (`ww.lifecycle`). Its
/// frame-based ports are adapted by `vendored_render_def`. Kept for
/// L5's side-by-side and O2, which reads the group's inner arrays.
const VENDORED_GROUP: &str = include_str!("../../../fixtures/whitewater_vendored_group.json");

/// `whitewater_render_def` with the vendored group in place of the node, on a
/// frame that publishes the simulation lattice, its lifecycle reports probed
/// by name.
fn vendored_render_def(scene: WaterScene) -> EffectGraphDef {
    let mut g = Appender::new(render_def(scene.with_faces()));
    let mut group: Value = serde_json::from_str(VENDORED_GROUP).expect("the vendored group parses");
    let id = g.id("whitewater");
    // The render ids move with the water def's node count; the group takes the node's.
    group["id"] = json!(id);
    let outputs: Vec<String> = group["group"]["interface"]["outputs"]
        .as_array()
        .expect("the group's outputs")
        .iter()
        .map(|output| output["name"].as_str().expect("output name").to_owned())
        .collect();
    g.replace("whitewater", group);
    // The step runs inside the tick region on pool state and a distance
    // lattice; the vendored lifecycle is a post-frame observer. It takes its
    // original frame and surface inputs and drives the render directly,
    // never the step's tick interface.
    g.retain_wires(id, |wire| wire["toNode"] != id && wire["fromNode"] != id);
    // Corrected FLIP faces and obstacle lattices share native coordinates.
    let frame = g.id("frame");
    for (source, input) in [("particles_b", "particles"), ("count_b", "count"), ("solid_b", "solid")] {
        g.wire((frame, source), id, input);
    }
    for port in ["grid_bounds", "grid_nodes_x", "grid_nodes_y", "grid_nodes_z",
        "face_u", "face_v", "face_w", "face_cells_x", "face_cells_y", "face_cells_z", "face_valid_layers"] {
        g.wire((frame, port), id, port);
    }
    let surface = g.id("surface");
    for port in ["level_set", "level_set_nodes_x", "level_set_nodes_y", "level_set_nodes_z"] {
        g.wire((surface, port), id, port);
    }
    let domain = g.id("domain");
    for port in ["ticks", "epoch", "gravity_x", "gravity", "gravity_z"] {
        g.wire((domain, port), id, port);
    }
    g.wire((domain, "simulation_time"), id, "seed");
    // Without the step the state captures no whitewater, and an optional
    // result is wired at both ends or neither: each population's display
    // blend, the state result's only reader, goes too. The group's
    // populations are drawn directly; one it does not publish is not drawn.
    for kind in WHITEWATER_KINDS {
        let frame_in = format!("{kind}_in");
        g.retain_wires(frame, |w| !(w["toNode"] == frame && w["toPort"] == frame_in.as_str()));
    }
    for kind in ["foam", "spray", "bubble"] {
        g.remove(&[format!("{kind}_blend").as_str()]);
        let render = ["copies", "object", "mesh", "material"].map(|part| format!("{kind}_{part}"));
        let particles = format!("{kind}_particles");
        if !outputs.contains(&particles) {
            g.remove(&render.each_ref().map(String::as_str));
            continue;
        }
        let (copies, object) = (g.id(&render[0]), g.id(&render[1]));
        let count = format!("{kind}_count");
        g.wire((id, &particles), copies, "particles");
        g.wire((id, &count), copies, "live_count");
        g.wire((id, &count), object, "instance_count");
    }
    let target = json!({"kind": "node", "nodeId": "ww.lifecycle", "param": "capacity"});
    g.retarget_binding("whitewater_capacity", target);
    for report in LIFECYCLE_REPORTS {
        g.probe(report, (id, report));
    }
    g.probe("count", (frame, "count_b"));
    g.finish()
}

/// Resolution is a live card (BUG-9an1 (resolution change), BUG-o65k (GPU
/// FLIP lattice wiring)): the shipped preset moves 64 → 32 → 48 under a
/// running clip. On the first frame at each size the state already holds that
/// lattice's face grid and the published population matches the new fill.
/// During the following 0.5 s, count_b exactly describes the published live
/// particles; after completion it matches the solver state. The initial fill
/// is not a retention oracle: native-style marker cleanup can remove water.
/// The step's face grid resizes and the new water throws whitewater.
#[test]
fn gpu_flip_resolution_card_resizes_at_runtime() {
    use manifold_node_engine::particles::FluidParticle;
    fn particles(buffer: &manifold_gpu::GpuBuffer, bytes: u64) -> &[FluidParticle] {
        assert!(buffer.size() >= bytes);
        let len = bytes as usize / std::mem::size_of::<FluidParticle>();
        let ptr = buffer.mapped_ptr().expect("shared particle storage");
        // SAFETY: callers wait for the frame; the buffer contains len records.
        // A resized allocation may retain storage beyond the current prefix.
        unsafe { std::slice::from_raw_parts(ptr.cast::<FluidParticle>(), len) }
    }
    let scene = WaterScene::dam_break(64);
    let def = whitewater_render_def(scene.with_faces());
    let spec = def
        .preset_metadata
        .as_ref()
        .and_then(|cards| cards.params.iter().find(|card| card.id == "resolution"))
        .expect("the Resolution card")
        .clone();
    let mut show = Show::new(def, (320, 180), true, &["state".to_string()]);
    let live_in = |buffer: &manifold_gpu::GpuBuffer, bytes: u64| {
        particles(buffer, bytes).iter().filter(|p| p.position_radius[3] > 0.0).count() as u64
    };
    let state_live = |show: &Show| {
        let arrays = show.runtime().dump_arrays_all();
        let array = arrays.iter().find(|a| a.name == "state" && a.port == "out").expect("held state.out");
        live_in(array.buffer, show.provided_bytes("fill", "particles"))
    };
    let published_live = |show: &Show| {
        let frame = show.runtime().graph.nodes().find(|node| node.node_id.as_str() == "frame").expect("liquid frame");
        live_in(frame.node.provided_array_output("particles_b").expect("published frame B"), show.provided_bytes("fill", "particles"))
    };
    show.restart();
    let step = manifold_nodes_water::presets::gpu_flip::STEP_NODE;
    for n in [64u32, 32, 48] {
        let mut card = Param::bundled(spec.clone());
        card.value = n as f32;
        card.base = n as f32;
        show.set_cards(ParamManifest::from_params(vec![card]));
        show.frame(false);
        let seeded = show.provided_live("fill", "particles");
        assert_eq!(state_live(&show), seeded, "Resolution {n}: the resized state is reseeded");
        assert_eq!(published_live(&show), seeded, "Resolution {n}: the first published frame is reseeded");
        assert_eq!(show.probes(["count"])[0] as u64, seeded, "Resolution {n}: the first published count");
        {
            let bytes = show.provided_bytes("fill", "particles");
            let fill = show.runtime().graph.nodes().find(|node| node.node_id.as_str() == "fill").unwrap();
            let seed = particles(fill.node.provided_array_output("particles").unwrap(), bytes);
            let arrays = show.runtime().dump_arrays_all();
            let state = arrays.iter().find(|a| a.name == "state" && a.port == "out").unwrap();
            let mut seen = vec![false; seed.len()];
            for p in particles(state.buffer, bytes).iter().filter(|p| p.position_radius[3] > 0.0) {
                let index = p.id.checked_sub(1).expect("seed identity is nonzero") as usize;
                assert!(index < seed.len() && !seen[index], "Resolution {n}: stale or duplicate seed identity {}", p.id);
                seen[index] = true;
                assert_eq!(bytemuck::bytes_of(p), bytemuck::bytes_of(&seed[index]), "Resolution {n}: reseeded record {}", p.id);
            }
            let frame = show.runtime().graph.nodes().find(|node| node.node_id.as_str() == "frame").unwrap();
            let published = particles(frame.node.provided_array_output("particles_b").unwrap(), bytes);
            // Publication sorts by birth id; the fill assigns id = site + 1.
            for (p, expected) in published.iter().filter(|p| p.position_radius[3] > 0.0).zip(seed.iter().filter(|p| p.position_radius[3] > 0.0)) {
                assert_eq!(bytemuck::bytes_of(p), bytemuck::bytes_of(expected), "Resolution {n}: published seed record {}", expected.id);
            }
        }
        // The native solver grid: three cells more than the authored box.
        let faces = face_bytes([n + 3; 3]);
        assert_eq!(show.provided_bytes("state", "faces"), faces, "Resolution {n}: the state's faces on its first frame");
        let mut last = [0.0; 6];
        let mut gpu_ms = Vec::new();
        for frame in 0..30 {
            gpu_ms.push(show.frame(false).gpu_ms);
            last = show.probes(STEP_REPORTS);
            assert_eq!(show.probes(["count"])[0] as u64, published_live(&show), "Resolution {n}, frame {frame}: published live count");
        }
        // Retire any completed publication without advancing simulation, so
        // the source and published frame refer to the same accepted endpoint.
        show.set_paused(true);
        show.frame(false);
        show.set_paused(false);
        let [count] = show.probes(["count"]);
        let live = state_live(&show);
        println!("Resolution {n}: {count} published, {live} live in state, {seeded} initially seeded, GPU p50 {:.2} ms; foam {} bubble {} spray {}", percentile(&gpu_ms, 0.5), last[0], last[1], last[2]);
        assert_eq!(show.provided_bytes(step, "faces"), faces, "Resolution {n}: the step's faces");
        let record = std::mem::size_of::<manifold_node_engine::particles::FluidParticle>() as u64;
        assert_eq!(show.provided_bytes("fill", "particles"), WaterScene::dam_break(n as usize).particles() * record, "Resolution {n}: the fill");
        assert_eq!(count as u64, live, "Resolution {n}: the frame's live water");
        assert_eq!(count as u64, published_live(&show), "Resolution {n}: completed publication");
        assert!(last[0] + last[1] + last[2] > 0.0, "Resolution {n}: no whitewater by 0.5 s: {last:?}");
    }
    let errors = show.errors();
    assert!(errors.is_empty(), "the resize ran with errors: {errors:#?}");
}

/// The shipped GPU FLIP Dam Break with its `node.whitewater_step`, as the app
/// renders it, 90 frames: no node refuses and foam is up by 1.5 s.
#[test]
fn gpu_flip_whitewater_emits() {
    let scene = WaterScene::dam_break(64);
    let mut show = Show::new(whitewater_render_def(scene), (320, 180), true, &[]);
    show.restart();
    let mut last = [0.0; 6];
    for frame in 1..=90 {
        show.frame(false);
        last = show.probes(STEP_REPORTS);
        if frame % 15 == 0 {
            let [foam, bubble, spray, emitted, thinned, pool_full] = last;
            let [count] = show.probes(["count"]);
            println!(
                "frame {frame}: {count} particles; foam {foam} bubble {bubble} spray {spray}, emitted {emitted}, thinned {thinned}, pool full {pool_full}"
            );
        }
    }
    let errors = show.errors();
    assert!(errors.is_empty(), "the chain ran with errors: {errors:#?}");
    assert!(last[0] > 0.0, "no foam by 1.5 s: {last:?}");
}

/// D11 at 64, live: the scene's whitewater is updated on the lifecycle's
/// thread (the update refuses any other, so a content-thread update fails
/// here), the population it produces reaches the outputs, and no tick drops
/// while the GPU keeps up. The content thread's and the worker's ms are
/// printed for the cost table, never asserted. Runs the vendored group, the
/// only whitewater with a lifecycle thread.
#[test]
fn whitewater_live_scene_updates_on_the_lifecycle_thread() {
    let scene = WaterScene::dam_break(64);
    let _live = manifold_nodes_water::physics::PhysicsStepScope::for_render(false);
    let mut show = Show::new(vendored_render_def(scene), (320, 180), true, &[]);
    show.restart();
    let (mut content, mut worker) = (Vec::new(), Vec::new());
    let mut last = [0.0; 8];
    for frame in 1..=180 {
        show.frame(false);
        last = show.probes(LIFECYCLE_REPORTS);
        // The first frames compile pipelines and hold little water.
        if frame > 30 {
            content.push(f64::from(last[6]));
            worker.push(f64::from(last[7]));
        }
    }
    let errors = show.errors();
    assert!(errors.is_empty(), "the chain ran with errors: {errors:#?}");
    let row = |values: &[f64]| format!("p50 {:.3} p95 {:.3} max {:.3}", percentile(values, 0.5), percentile(values, 0.95), percentile(values, 1.0));
    println!("WHITEWATER live, frames 31-180: content thread {} ms; lifecycle thread {} ms", row(&content), row(&worker));
    println!("WHITEWATER live at frame 180: {last:?}");
    assert!(last[0] > 0.0, "no foam by 3 s: {last:?}");
    assert!(worker.iter().any(|&ms| ms > 0.0), "the lifecycle thread never reported work");
    assert_eq!(last[5], 0.0, "live, with the GPU waited each frame, no tick drops");
}

/// The pause gesture on the shipped preset: paused mid-splash, the
/// whitewater holds (no emission, the same population, the same picture)
/// and moves on when play resumes.
#[test]
fn gpu_flip_whitewater_holds_while_paused() {
    let scene = WaterScene::dam_break(64);
    let mut show = Show::new(whitewater_render_def(scene), (320, 180), true, &[]);
    show.restart();
    for _ in 0..60 {
        show.frame(false);
    }
    let playing = show.probes(STEP_REPORTS);
    assert!(playing[0] > 0.0, "no foam by 1 s: {playing:?}");
    show.set_paused(true);
    show.frame(false);
    let (held, image) = (show.probes(STEP_REPORTS), show.readback());
    for _ in 0..3 {
        show.frame(false);
    }
    let (still, still_image) = (show.probes(STEP_REPORTS), show.readback());
    let changed = image.chunks_exact(4).zip(still_image.chunks_exact(4)).filter(|(a, b)| a != b).count();
    println!("WHITEWATER pause: playing {playing:?}; paused {held:?} then {still:?}; {changed} pixels changed over 3 paused frames");
    // Counts are captured at each tick boundary, so even the first paused
    // frame must preserve the last completed playing tick.
    assert_eq!(held, playing, "the first paused frame changed whitewater counts");
    assert_eq!(still[..6], held[..6], "paused frames moved the whitewater");
    assert_eq!(changed, 0, "paused frames changed the picture");
    show.set_paused(false);
    for _ in 0..15 {
        show.frame(false);
    }
    let resumed = show.probes(STEP_REPORTS);
    assert!(resumed[3] > still[3], "no emission after play resumed: {resumed:?}");
    let errors = show.errors();
    assert!(errors.is_empty(), "the chain ran with errors: {errors:#?}");
}

fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// O2 (section 3.7): the GPU emitter against FLIP's own on the same inputs.
#[cfg(feature = "whitewater-oracle")]
mod emitter_oracle {
    use manifold_nodes_water::testkit::conformance::json_node_mut;
    use manifold_nodes_water::primitives::testkit::whitewater as whitewater_nodes;
    use manifold_fluids::{
        WhitewaterFields, WhitewaterGrid, WhitewaterKind, WhitewaterLifecycle as NativeLifecycle, WhitewaterParticle, WhitewaterSpawn,
        whitewater_oracle,
    };
    use manifold_gpu::GpuBuffer;

    use manifold_nodes_water::primitives::emission_count::EmissionCount;
    use manifold_nodes_water::primitives::jitter_particles::JitterParticles;
    use manifold_node_engine::testkit::array_harness::{Harness, params, read};
    use manifold_nodes_water::primitives::sample_faces_at_particles::SampleFacesAtParticles;
    use manifold_nodes_water::primitives::spawn_whitewater::SpawnWhitewater;
    use manifold_nodes_water::presets::gpu_flip::REST_PER_CELL;
    use manifold_nodes_water::primitives::wavecrest_potential::WavecrestPotential;
    use manifold_nodes_water::primitives::whitewater_type::WhitewaterType;
    use crate::contracts::node_graph::catalog_tests::whitewater_scene::*;
    use manifold_node_engine::bindings::Slot;
    use manifold_node_engine::particles::FluidParticle;
    use manifold_nodes_water::liquid::grid::face_len;
    use manifold_node_engine::primitive::Primitive;
    use manifold_nodes_water::whitewater::KnownValue;

    /// The whitewater grid: the surface's solid lattice, its cells and box, and
    /// the face grid centred in it.
    #[derive(Clone, Copy, Debug)]
    struct GridBox {
        center: [f64; 3],
        size: [f64; 3],
        nodes: f64,
        h: f64,
        face_cells: f64,
    }

    impl GridBox {
        /// The solid lattice the GPU FLIP domain publishes for `scene`, and its face
        /// grid.
        fn of(scene: WaterScene) -> Self {
            let n = scene.pressure.n;
            let lattice = manifold_nodes_water::liquid::lattice::LiquidLattice::from_layout(&scene.layout()).surface();
            let bounds = lattice.bounds();
            let nodes = lattice.nodes();
            assert!(nodes.iter().all(|&v| v == nodes[0]), "a cubic lattice: {nodes:?}");
            Self {
                center: bounds.pos.map(f64::from),
                size: bounds.scale.map(f64::from),
                nodes: f64::from(nodes[0]),
                h: f64::from(lattice.cell_size()),
                face_cells: (n + 3) as f64,
            }
        }

        fn values(&self) -> Vec<(&'static str, f64)> {
            let mut values = Vec::new();
            for axis in 0..3 {
                values.push((["center_x", "center_y", "center_z"][axis], self.center[axis]));
                values.push((["size_x", "size_y", "size_z"][axis], self.size[axis]));
                values.push((["nodes_x", "nodes_y", "nodes_z"][axis], self.nodes));
            }
            values
        }

        fn face_values(&self) -> [(&'static str, f64); 3] {
            [("face_cells_x", self.face_cells), ("face_cells_y", self.face_cells), ("face_cells_z", self.face_cells)]
        }
    }

    const FRAMES: [usize; 4] = [30, 60, 90, 120];
    const CAPACITY: u32 = 250_000;
    const TANK: f32 = 4.0;
    const SPACE_BINS: usize = 8;
    const LIFE_BINS: usize = 10;
    const MAX_LIFETIME: f32 = 7.0;
    const GRAVITY: [f32; 3] = [0.0, -9.81, 0.0];
    const DT: f64 = 1.0 / 60.0;
    const SEEDS: u32 = 16;
    /// Past this a frame fails as too noisy to judge rather than passing.
    /// The shipped preset's frame 90 emits under one particle a seed, so its
    /// histograms settle only past 4,096 seeds.
    const SEED_LIMIT: u32 = 16_384;
    const TOTAL_TOLERANCE: f64 = 0.05;
    const KIND_TOLERANCE: f64 = 0.10;
    const KIND_FLOOR: f64 = 200.0;
    const SPACE_TOLERANCE: f64 = 0.15;
    const LIFE_TOLERANCE: f64 = 0.10;

    /// One frame's emitter inputs, as the scene handed them to its chain.
    struct Captured {
        frame: usize,
        count: u32,
        particles: Vec<FluidParticle>,
        faces: [Vec<f32>; 3],
        distance: Vec<f32>,
        curvature: Vec<KnownValue>,
        cells: Vec<u32>,
        solid: Vec<f32>,
    }

    fn cells(grid: GridBox) -> u32 {
        grid.nodes as u32 - 1
    }

    fn capture(scene: WaterScene, grid: GridBox) -> Vec<Captured> {
        let held: Vec<String> = ["frame", "ww.distance", "ww.extend2", "ww.cells"].map(String::from).to_vec();
        let mut show = Show::new(vendored_render_def(scene), (320, 180), false, &held);
        show.restart();
        let lattice = (cells(grid) as usize).pow(3);
        let face_cells = [grid.face_cells as u32; 3];
        let mut captured = Vec::new();
        for frame in 1..=*FRAMES.last().expect("frames") {
            show.frame(false);
            if !FRAMES.contains(&frame) {
                continue;
            }
            captured.push(Captured {
                frame,
                count: show.probes(["count"])[0] as u32,
                particles: show.dumped("frame", "particles_b", scene.particles() as usize),
                faces: [0, 1, 2].map(|axis| show.dumped("frame", ["face_u", "face_v", "face_w"][axis], face_len(face_cells, axis) as usize)),
                distance: show.dumped("ww.distance", "out", lattice),
                curvature: show.dumped("ww.extend2", "out", lattice),
                cells: show.dumped("ww.cells", "out", lattice),
                solid: show.dumped("frame", "solid_b", (grid.nodes as usize).pow(3)),
            });
        }
        let errors = show.errors();
        assert!(errors.is_empty(), "the capture ran with errors: {errors:#?}");
        captured
    }

    /// What one seed of one side left after emission and one lifecycle step.
    #[derive(Clone)]
    struct Outcome {
        kinds: [f64; 3],
        space: Vec<f64>,
        life: Vec<f64>,
    }

    impl Outcome {
        fn of(particles: &[WhitewaterParticle], min: [f64; 3]) -> Self {
            let mut out = Self { kinds: [0.0; 3], space: vec![0.0; SPACE_BINS.pow(3)], life: vec![0.0; LIFE_BINS] };
            let bin = |x: f32, bins: usize| ((x * bins as f32).floor() as i64).clamp(0, bins as i64 - 1) as usize;
            for p in particles {
                let kind = match p.kind {
                    WhitewaterKind::Foam => 0,
                    WhitewaterKind::Bubble => 1,
                    WhitewaterKind::Spray => 2,
                };
                out.kinds[kind] += 1.0;
                let at = |a: usize| bin((p.position[a] - min[a] as f32) / TANK, SPACE_BINS);
                out.space[at(0) + SPACE_BINS * (at(1) + SPACE_BINS * at(2))] += 1.0;
                out.life[bin(p.lifetime / MAX_LIFETIME, LIFE_BINS)] += 1.0;
            }
            out
        }

        fn total(&self) -> f64 {
            self.kinds.iter().sum()
        }
    }

    type Array = (Slot, GpuBuffer);

    /// The GPU emitter chain as standalone atoms on one harness whose arrays
    /// are made once and rewritten each frame.
    struct Chain {
        harness: Harness,
        grid: GridBox,
        slots: usize,
        particles: Array,
        faces: [Array; 3],
        distance: Array,
        curvature: Array,
        cells: Array,
        solid: Array,
        jittered: Array,
        sampled: Array,
        energy: Array,
        wavecrest: Array,
        counts: Array,
        offsets: Array,
        spawns: Array,
        typed: Array,
    }

    fn write<T: bytemuck::Pod>(buffer: &GpuBuffer, values: &[T]) {
        assert!(buffer.size() as usize >= std::mem::size_of_val(values), "the harness array holds the frame");
        // SAFETY: shared storage at least this large; no GPU work in flight.
        unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
    }

    impl Chain {
        fn new(grid: GridBox, slots: usize, solid: &[f32]) -> Self {
            let mut h = Harness::new();
            let lattice = (cells(grid) as usize).pow(3);
            let face_cells = [grid.face_cells as u32; 3];
            let particles = h.array::<FluidParticle>(&[], slots);
            let faces = [0, 1, 2].map(|axis| h.array::<f32>(&[], face_len(face_cells, axis) as usize));
            let distance = h.array::<f32>(&[], lattice);
            let curvature = h.array::<KnownValue>(&[], lattice);
            let cells = h.array::<u32>(&[], lattice);
            let solid = h.array::<f32>(solid, solid.len());
            let jittered = h.array::<FluidParticle>(&[], slots);
            let sampled = h.array::<FluidParticle>(&[], slots);
            let energy = h.array::<f32>(&[], slots);
            let wavecrest = h.array::<f32>(&[], slots);
            let counts = h.array::<u32>(&[], slots);
            let offsets = h.array::<u32>(&[], slots);
            let spawns = h.array::<WhitewaterSpawn>(&[], CAPACITY as usize);
            let typed = h.array::<WhitewaterSpawn>(&[], CAPACITY as usize);
            Self {
                harness: h,
                grid,
                slots,
                particles,
                faces,
                distance,
                curvature,
                cells,
                solid,
                jittered,
                sampled,
                energy,
                wavecrest,
                counts,
                offsets,
                spawns,
                typed,
            }
        }

        fn load(&mut self, c: &Captured) {
            assert_eq!(c.particles.len(), self.slots);
            write(&self.particles.1, &c.particles);
            for (array, values) in self.faces.iter().zip(&c.faces) {
                write(&array.1, values);
            }
            write(&self.distance.1, &c.distance);
            write(&self.curvature.1, &c.curvature);
            write(&self.cells.1, &c.cells);
        }

        fn step<P: Primitive>(&mut self, mut prim: P, inputs: &[(&'static str, Slot)], out: Slot, values: &[(&'static str, f64)]) {
            let values: Vec<(&'static str, f32)> = values.iter().map(|&(name, v)| (name, v as f32)).collect();
            let (_, errors) = self.harness.run(&mut prim, inputs, &[("out", out)], &params(&values));
            assert!(errors.is_empty(), "{errors:?}");
        }

        /// This frame's typed spawns at `seed`, made as the graph makes them.
        fn spawns(&mut self, count: u32, seed: u32) -> Vec<WhitewaterSpawn> {
            let grid = self.grid;
            let boxed = grid.values();
            let faced: Vec<(&'static str, f64)> = [&boxed[..], &grid.face_values()[..]].concat();
            let seed = f64::from(seed);
            let faces = [("face_u", self.faces[0].0), ("face_v", self.faces[1].0), ("face_w", self.faces[2].0)];
            self.step(JitterParticles::new(), &[("particles", self.particles.0)], self.jittered.0, &[("cell_size", grid.h), ("seed", seed), ("epoch", 0.0)]);
            let sample = [&[("particles", self.jittered.0)][..], &faces[..]].concat();
            self.step(SampleFacesAtParticles::new(), &sample, self.sampled.0, &faced);
            self.step(whitewater_nodes::energy_potential(), &[("particles", self.sampled.0)], self.energy.0, &[]);
            let crest = [("particles", self.sampled.0), ("distance", self.distance.0), ("curvature", self.curvature.0), ("cells", self.cells.0)];
            self.step(WavecrestPotential::new(), &crest, self.wavecrest.0, &boxed);
            let emit = [("particles", self.sampled.0), ("energy", self.energy.0), ("wavecrest", self.wavecrest.0)];
            let emit_values = [("points_per_cell", REST_PER_CELL), ("ticks", 1.0), ("live_count", f64::from(count))];
            self.step(EmissionCount::new(), &emit, self.counts.0, &emit_values);
            let counts: Vec<u32> = read(&self.counts.1, count as usize);
            let offsets: Vec<u32> = counts
                .iter()
                .scan(0u32, |sum, &n| {
                    *sum += n;
                    Some(*sum)
                })
                .collect();
            write(&self.offsets.1, &offsets);
            let total = offsets.last().copied().unwrap_or(0);
            assert!(total <= CAPACITY, "{total} spawns past the oracle's capacity");
            let spawn = [&[("offsets", self.offsets.0), ("particles", self.sampled.0), ("energy", self.energy.0), ("solid", self.solid.0)][..], &faces[..]].concat();
            let mut spawn_values = faced.clone();
            spawn_values.extend([
                ("capacity", f64::from(CAPACITY)),
                ("emitters", f64::from(count)),
                ("seed", seed),
                ("epoch", 0.0),
                ("min_lifetime", 0.0),
                ("max_lifetime", f64::from(MAX_LIFETIME)),
                ("lifetime_variance", 0.0),
            ]);
            self.step(SpawnWhitewater::new(), &spawn, self.spawns.0, &spawn_values);
            let typing = [("spawns", self.spawns.0), ("distance", self.distance.0), ("cells", self.cells.0)];
            self.step(WhitewaterType::new(), &typing, self.typed.0, &boxed);
            read(&self.typed.1, total as usize)
        }
    }

    fn lifecycle(grid: GridBox, c: &Captured, solid: &[f32], seed: u32) -> NativeLifecycle {
        let origin = std::array::from_fn(|a| (grid.center[a] - 0.5 * grid.size[a]) as f32);
        let whitewater_grid = WhitewaterGrid { cells: [cells(grid); 3], cell_size: grid.h as f32, origin };
        let mut lifecycle = NativeLifecycle::new(whitewater_grid, CAPACITY, u64::from(seed)).expect("lifecycle");
        let fields = WhitewaterFields {
            face_u: &c.faces[0],
            face_v: &c.faces[1],
            face_w: &c.faces[2],
            face_cells: [grid.face_cells as u32; 3],
            face_offset: [(cells(grid) - grid.face_cells as u32) / 2; 3],
            level: &c.distance,
            solid,
            gravity: GRAVITY,
        };
        lifecycle.set_fields(&fields).expect("fields");
        lifecycle
    }

    fn mean(values: &[f64]) -> f64 {
        values.iter().sum::<f64>() / values.len() as f64
    }

    /// The standard error of the mean.
    fn error(values: &[f64]) -> f64 {
        let (m, n) = (mean(values), values.len() as f64);
        (values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (n - 1.0).max(1.0) / n).sqrt()
    }

    /// L1 between the two sides' pooled histograms, each normalised.
    fn l1(a: &[Outcome], b: &[Outcome], pick: fn(&Outcome) -> &[f64]) -> f64 {
        let pool = |side: &[Outcome]| {
            let mut sum = vec![0.0; pick(&side[0]).len()];
            for o in side {
                for (s, v) in sum.iter_mut().zip(pick(o)) {
                    *s += v;
                }
            }
            let total = sum.iter().sum::<f64>().max(1.0);
            sum.into_iter().map(|v| v / total).collect::<Vec<_>>()
        };
        pool(a).iter().zip(pool(b)).map(|(x, y)| (x - y).abs()).sum()
    }

    fn totals(side: &[Outcome]) -> Vec<f64> {
        side.iter().map(Outcome::total).collect()
    }

    fn kind(side: &[Outcome], k: usize) -> Vec<f64> {
        side.iter().map(|o| o.kinds[k]).collect()
    }

    /// The types the per-type gate covers: FLIP emitted at least 200 of
    /// them over the seeds.
    fn kinds_in_scope(flip: &[Outcome]) -> Vec<usize> {
        (0..3).filter(|&k| kind(flip, k).iter().sum::<f64>() >= KIND_FLOOR).collect()
    }

    /// How far each gated measure could move by chance at this many seeds,
    /// against half its tolerance: the total's and each type's standard
    /// error relative to FLIP's mean, and for a histogram the L1 between a
    /// side's odd and even seeds scaled to the whole pool (1/√2). Over 1
    /// means add seeds.
    fn noise(gpu: &[Outcome], flip: &[Outcome]) -> [f64; 4] {
        let spread = |g: &[f64], f: &[f64]| (error(g).powi(2) + error(f).powi(2)).sqrt() / mean(f).max(1e-9);
        let total = spread(&totals(gpu), &totals(flip)) / (TOTAL_TOLERANCE / 2.0);
        let kinds = kinds_in_scope(flip).into_iter().map(|k| spread(&kind(gpu, k), &kind(flip, k)) / (KIND_TOLERANCE / 2.0)).fold(0.0, f64::max);
        let halves = |pick: fn(&Outcome) -> &[f64]| {
            [gpu, flip]
                .iter()
                .map(|side| {
                    let (even, odd): (Vec<_>, Vec<_>) = side.iter().enumerate().partition(|(i, _)| i % 2 == 0);
                    let strip = |v: Vec<(usize, &Outcome)>| v.into_iter().map(|(_, o)| o.clone()).collect::<Vec<_>>();
                    l1(&strip(even), &strip(odd), pick) / std::f64::consts::SQRT_2
                })
                .fold(0.0, f64::max)
        };
        [total, kinds, halves(|o| &o.space) / (SPACE_TOLERANCE / 2.0), halves(|o| &o.life) / (LIFE_TOLERANCE / 2.0)]
    }

    /// GPU FLIP's Dam Break at 64, frames 30, 60, 90 and 120: the particles,
    /// faces, distance, curvature and solid the scene hands its chain go to
    /// FLIP's emitter through the oracle and to the GPU atoms, and both take
    /// one lifecycle step with lifetime variance 0. Over 16 seeds a side,
    /// more while any measure's seed spread is over half its tolerance:
    /// total within 5%; each type within 10% where FLIP emitted at least 200
    /// of it over the seeds (the design's floor on the mean, taken on the
    /// pool so the gate covers more); the 8³ spatial histogram over the tank
    /// within L1 0.15; the 10-bin lifetime histogram within L1 0.1.
    #[test]
    fn whitewater_emitter_matches_flip() {
        let scene = WaterScene::dam_break(64);
        let grid = GridBox::of(scene);
        let tank_min = scene.min();
        let captured = capture(scene, grid);
        let solid = captured[0].solid.clone();
        let mut chain = Chain::new(grid, scene.particles() as usize, &solid);
        let mut failures = Vec::new();
        let mut population = Vec::new();
        println!("O2 frame seeds |     GPU    FLIP   total |     foam (GPU FLIP) |   bubble (GPU FLIP) |    spray (GPU FLIP) | space L1 | life L1 | noise / half tolerance");
        for c in &captured {
            let started = Instant::now();
            chain.load(c);
            let positions: Vec<[f32; 3]> = c.particles[..c.count as usize]
                .iter()
                .filter(|p| p.position_radius[3] > 0.0)
                .map(|p| [p.position_radius[0], p.position_radius[1], p.position_radius[2]])
                .collect();
            let curvature: Vec<f32> = c.curvature.iter().map(|k| k.value).collect();
            let (mut gpu, mut flip) = (Vec::new(), Vec::new());
            let mut seeds = 0;
            loop {
                for seed in seeds + 1..=seeds + SEEDS {
                    let spawns = chain.spawns(c.count, seed);
                    let mut ours = lifecycle(grid, c, &solid, seed);
                    ours.load(&spawns).expect("load");
                    ours.step(DT).expect("step");
                    ours.particles(&mut population).expect("population");
                    gpu.push(Outcome::of(&population, tank_min));
                    let mut theirs = lifecycle(grid, c, &solid, seed);
                    whitewater_oracle::emit(&mut theirs, &curvature, &positions, DT).expect("FLIP emits");
                    theirs.particles(&mut population).expect("population");
                    flip.push(Outcome::of(&population, tank_min));
                }
                seeds += SEEDS;
                let spread = noise(&gpu, &flip);
                if spread.iter().all(|&s| s <= 1.0) {
                    break;
                }
                if seeds >= SEED_LIMIT {
                    failures.push(format!("frame {}: still too noisy to judge at {seeds} seeds: {spread:.2?}", c.frame));
                    break;
                }
            }
            let relative = |a: f64, b: f64| if b > 0.0 { (a - b) / b } else if a > 0.0 { f64::INFINITY } else { 0.0 };
            let (g, f) = (mean(&totals(&gpu)), mean(&totals(&flip)));
            let mut row = format!("O2 {:5} {seeds:5} | {g:7.2} {f:7.2} {:+6.1}%", c.frame, 100.0 * relative(g, f));
            if relative(g, f).abs() > TOTAL_TOLERANCE {
                failures.push(format!("frame {}: total {g:.2} against FLIP's {f:.2}", c.frame));
            }
            let scope = kinds_in_scope(&flip);
            for (k, name) in ["foam", "bubble", "spray"].into_iter().enumerate() {
                let (a, b) = (mean(&kind(&gpu, k)), mean(&kind(&flip, k)));
                let gated = if scope.contains(&k) { " " } else { "*" };
                row += &format!(" | {:+6.1}%{gated}{a:6.2} {b:6.2}", 100.0 * relative(a, b));
                if scope.contains(&k) && relative(a, b).abs() > KIND_TOLERANCE {
                    failures.push(format!("frame {}: {name} {a:.2} against FLIP's {b:.2}", c.frame));
                }
            }
            let (space, life) = (l1(&gpu, &flip, |o| &o.space), l1(&gpu, &flip, |o| &o.life));
            row += &format!(" | {space:8.3} | {life:7.3} | {:.2?} | {:.0} s", noise(&gpu, &flip), started.elapsed().as_secs_f64());
            println!("{row}");
            if space > SPACE_TOLERANCE {
                failures.push(format!("frame {}: spatial L1 {space:.3}", c.frame));
            }
            if life > LIFE_TOLERANCE {
                failures.push(format!("frame {}: lifetime L1 {life:.3}", c.frame));
            }
        }
        println!("O2 * marks a type outside the per-type gate: FLIP emitted fewer than {KIND_FLOOR} over the seeds");
        assert!(failures.is_empty(), "the GPU emitter strays from FLIP's: {failures:#?}");
    }

    const ENGINE_SEEDS: [u64; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
    /// The step's default capacity and obstacle influence, which the engine
    /// shares (`whitewater_step.rs` params, `fluidsimulation.h`).
    const STEP_CAPACITY: u32 = 100_000;
    const INFLUENCE_BASE: f64 = 1.0;
    const INFLUENCE_DECAY: f64 = 2.0;

    /// The solver's face-grid distance padded onto the whitewater grid, as
    /// `encode_pad_distance_lattice` pads it for the step: outside the solver
    /// grid, three cells of air.
    fn pad_distance(distance: &[f32], face_cells: u32, cells: u32, h: f32) -> Vec<f32> {
        let (f, c) = (face_cells as usize, cells as usize);
        let pad = (c - f) / 2;
        let mut out = vec![3.0 * h; c * c * c];
        for z in 0..f {
            for y in 0..f {
                for x in 0..f {
                    out[(x + pad) + c * ((y + pad) + c * (z + pad))] = distance[x + f * (y + f * z)];
                }
            }
        }
        out
    }

    /// The gate's window, frames: short enough that a timing error cannot
    /// cancel in a long sum.
    const WINDOW: usize = 30;
    /// The widest band a window may take, relative to the engine's mean. A
    /// reference noisier than this fails the window instead of widening it.
    const TOLERANCE_CAP: f64 = 0.10;
    /// A window is judged only where the engine's mean reaches this: below
    /// it the counting floor 3√m alone is wider than the cap ((3 / 0.10)²).
    const ACTIVITY_FLOOR: f64 = 900.0;

    /// The sample standard deviation: one draw's spread about the mean.
    fn sample_sd(values: &[f64]) -> f64 {
        let m = mean(values);
        (values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / (values.len() as f64 - 1.0).max(1.0)).sqrt()
    }

    /// One quantity per frame: the GPU's value and each engine seed's.
    struct Series {
        name: &'static str,
        gpu: Vec<f64>,
        engine: Vec<Vec<f64>>,
    }

    /// Each window of `s`: GPU sum, engine mean, band, and its verdict. The
    /// band is three times the sample standard deviation across engine seeds
    /// (the GPU run is one draw, so its spread is one run's) or the counting floor
    /// 3√mean, whichever is wider; over the cap the reference is rejected. A
    /// series no window of which reaches the activity floor fails as never
    /// active.
    fn gate(s: &Series, rows: &mut Vec<String>, failures: &mut Vec<String>) {
        let mut active = false;
        for (w, start) in (0..s.gpu.len()).step_by(WINDOW).enumerate() {
            let end = (start + WINDOW).min(s.gpu.len());
            let gpu: f64 = s.gpu[start..end].iter().sum();
            let seeds: Vec<f64> = s.engine.iter().map(|e| e[start..end].iter().sum()).collect();
            let engine = mean(&seeds);
            let sd = sample_sd(&seeds);
            let band = (3.0 * sd).max(3.0 * engine.sqrt());
            let label = format!("{} frames {}-{end}", s.name, start + 1);
            let verdict = if engine < ACTIVITY_FLOOR {
                // Too quiet to hold to the cap, but never free to over-emit:
                // the GPU may exceed the engine by at most the band a window
                // at the floor would allow.
                let ceiling = engine + (3.0 * sd).max(3.0 * ACTIVITY_FLOOR.sqrt());
                if gpu > ceiling {
                    failures.push(format!("{label}: quiet window, GPU {gpu:.0} over the ceiling {ceiling:.0}"));
                    "QUIET, OVER CEILING".to_owned()
                } else {
                    "below floor, under ceiling".to_owned()
                }
            } else if band > TOLERANCE_CAP * engine {
                active = true;
                failures.push(format!("{label}: reference too noisy, band {band:.0} over {:.0}% of {engine:.0}", 100.0 * TOLERANCE_CAP));
                "REFERENCE TOO NOISY".to_owned()
            } else if (gpu - engine).abs() > band {
                active = true;
                failures.push(format!("{label}: GPU {gpu:.0} against the engine's {engine:.0} ± {band:.0}"));
                "FAIL".to_owned()
            } else {
                active = true;
                "ok".to_owned()
            };
            rows.push(format!("{label:28} w{} | GPU {gpu:9.0} | engine {engine:9.0} (sd {sd:6.0}) | ratio {:.3} | band ±{band:6.0} | {verdict}",
                w + 1, gpu / engine.max(1.0)));
        }
        if !active {
            failures.push(format!("{}: never reached the activity floor of {ACTIVITY_FLOOR}", s.name));
        }
    }

    /// The gate's mechanisms, each shown to bite: a timing error that
    /// cancels over the run, a quantity that never happens, and a noisy
    /// reference all fail, where the whole-run sum, any-activity and an
    /// uncapped seed band would pass them.
    #[test]
    fn engine_parity_gate_mechanisms_bite() {
        let seeds = |v: Vec<f64>| vec![v.clone(), v.clone(), v];
        let judge = |s: Series| {
            let (mut rows, mut failures) = (Vec::new(), Vec::new());
            gate(&s, &mut rows, &mut failures);
            failures
        };
        let steady: Vec<f64> = vec![100.0; 150];
        assert!(judge(Series { name: "steady", gpu: steady.clone(), engine: seeds(steady.clone()) }).is_empty());
        // Windows: emission a window late, same total.
        let late: Vec<f64> = (0..150).map(|f| if f < 30 { 0.0 } else if f < 60 { 200.0 } else { 100.0 }).collect();
        assert_eq!(late.iter().sum::<f64>(), steady.iter().sum::<f64>());
        assert!(!judge(Series { name: "late", gpu: late, engine: seeds(steady.clone()) }).is_empty(), "a timing error must fail a window");
        // Floor: nothing on either side is not parity.
        let none = vec![0.0; 150];
        assert!(!judge(Series { name: "none", gpu: none.clone(), engine: seeds(none) }).is_empty(), "an inactive quantity must fail");
        // Cap: a reference whose seeds disagree by half is rejected.
        let noisy = vec![steady.clone(), steady.iter().map(|v| v * 1.5).collect(), steady.iter().map(|v| v * 0.5).collect()];
        assert!(!judge(Series { name: "noisy", gpu: steady.clone(), engine: noisy }).is_empty(), "a noisy reference must fail");
        // Ceiling: a quiet first window (engine 1 a frame, under the floor)
        // where the GPU emits 20 a frame fails though the rest match.
        let quiet: Vec<f64> = (0..150).map(|f| if f < 30 { 1.0 } else { 100.0 }).collect();
        let loud: Vec<f64> = (0..150).map(|f| if f < 30 { 20.0 } else { 100.0 }).collect();
        assert!(judge(Series { name: "quiet", gpu: quiet.clone(), engine: seeds(quiet.clone()) }).is_empty());
        assert!(!judge(Series { name: "quiet", gpu: loud, engine: seeds(quiet) }).is_empty(), "over-emission in a quiet window must fail");
    }

    /// One run's per-frame numbers: the GPU step's emitted and population by
    /// type, and each engine seed's on the same water.
    struct ParityRun {
        gpu_emitted: Vec<f64>,
        gpu_kinds: [Vec<f64>; 3],
        engine_emitted: Vec<Vec<f64>>,
        engine_kinds: [Vec<Vec<f64>>; 3],
    }

    /// The Dam Break at 64 for 150 frames with the step's params overridden
    /// by `params`, and FLIP's engine on the water the step saw each tick.
    /// Tick-region nodes hold no dump, so the step's inputs are read where
    /// the tick leaves them: the particles from `state.out` (the step's
    /// `out`), the faces from the `face_*` components of `state.faces` (the
    /// step's `faces`), the distance from the step's own `distance` storage,
    /// and `mesh_solid.solid`.
    fn parity_run(params: Value, engine_rates: whitewater_oracle::EmissionOptions, csv_name: &str) -> ParityRun {
        let scene = WaterScene::dam_break(64);
        let grid = GridBox::of(scene);
        let n = cells(grid);
        let face_cells = grid.face_cells as u32;
        let mut def = serde_json::to_value(whitewater_render_def(scene.with_faces())).expect("def serialises");
        let node = json_node_mut(&mut def, "whitewater").expect("the step in the Water group");
        for (key, value) in params.as_object().expect("params") {
            node["params"][key] = value.clone();
        }
        let def: EffectGraphDef = serde_json::from_value(def).expect("def");
        let held: Vec<String> = ["state", "mesh_solid", "face_u", "face_v", "face_w"].map(String::from).to_vec();
        let mut show = Show::new(def, (320, 180), false, &held);
        show.restart();
        let origin = std::array::from_fn(|a| (grid.center[a] - 0.5 * grid.size[a]) as f32);
        let whitewater_grid = WhitewaterGrid { cells: [n; 3], cell_size: grid.h as f32, origin };
        let mut engines: Vec<NativeLifecycle> = ENGINE_SEEDS
            .iter()
            .map(|&seed| {
                let mut engine = NativeLifecycle::new(whitewater_grid, STEP_CAPACITY, seed).expect("engine lifecycle");
                whitewater_oracle::set_emission_rates(&mut engine, engine_rates).expect("engine rates");
                engine
            })
            .collect();
        let seeds = ENGINE_SEEDS.len();
        let mut run = ParityRun {
            gpu_emitted: Vec::new(),
            gpu_kinds: Default::default(),
            engine_emitted: vec![Vec::new(); seeds],
            engine_kinds: std::array::from_fn(|_| vec![Vec::new(); seeds]),
        };
        let mut population = Vec::new();
        let mut csv = String::from("frame,gpu_emitted,engine_emitted_mean,gpu_foam,gpu_bubble,gpu_spray,engine_foam,engine_bubble,engine_spray\n");
        let mut previous_emitted = 0.0f32;
        for frame in 1..=EMISSION_FRAMES {
            show.frame(false);
            let gpu = show.probes(STEP_REPORTS);
            let particles: Vec<FluidParticle> = show.dumped("state", "out", scene.particles() as usize);
            let positions: Vec<[f32; 3]> = particles
                .iter()
                .filter(|p| p.position_radius[3] > 0.0)
                .map(|p| [p.position_radius[0], p.position_radius[1], p.position_radius[2]])
                .collect();
            let faces = [0, 1, 2].map(|axis| show.dumped::<f32>(["face_u", "face_v", "face_w"][axis], "out", face_len([face_cells; 3], axis) as usize));
            let distance: Vec<f32> = show.provided("step", "distance", (face_cells as usize).pow(3));
            let level = pad_distance(&distance, face_cells, n, grid.h as f32);
            let solid: Vec<f32> = show.dumped("mesh_solid", "solid", (grid.nodes as usize).pow(3));
            let fields = WhitewaterFields {
                face_u: &faces[0],
                face_v: &faces[1],
                face_w: &faces[2],
                face_cells: [face_cells; 3],
                face_offset: [(n - face_cells) / 2; 3],
                level: &level,
                solid: &solid,
                gravity: GRAVITY,
            };
            for (s, engine) in engines.iter_mut().enumerate() {
                engine.set_fields(&fields).expect("engine fields");
                let made = whitewater_oracle::emit_engine(engine, &positions, DT, INFLUENCE_BASE, INFLUENCE_DECAY).expect("engine emits");
                run.engine_emitted[s].push(f64::from(made.emitted));
                engine.particles(&mut population).expect("engine population");
                let mut kinds = [0.0f64; 3];
                for p in &population {
                    kinds[match p.kind {
                        WhitewaterKind::Foam => 0,
                        WhitewaterKind::Bubble => 1,
                        WhitewaterKind::Spray => 2,
                    }] += 1.0;
                }
                for (series, count) in run.engine_kinds.iter_mut().zip(kinds) {
                    series[s].push(count);
                }
            }
            run.gpu_emitted.push(f64::from(gpu[3] - previous_emitted));
            previous_emitted = gpu[3];
            for (series, &count) in run.gpu_kinds.iter_mut().zip(&gpu[..3]) {
                series.push(f64::from(count));
            }
            let f = frame - 1;
            let engine_mean = |series: &[Vec<f64>]| series.iter().map(|s| s[f]).sum::<f64>() / seeds as f64;
            csv.push_str(&format!("{frame},{},{:.1},{},{},{},{:.1},{:.1},{:.1}\n", run.gpu_emitted[f], engine_mean(&run.engine_emitted),
                gpu[0], gpu[1], gpu[2], engine_mean(&run.engine_kinds[0]), engine_mean(&run.engine_kinds[1]), engine_mean(&run.engine_kinds[2])));
        }
        let errors = show.errors();
        assert!(errors.is_empty(), "the chain ran with errors: {errors:#?}");
        let path = std::env::temp_dir().join(csv_name);
        std::fs::write(&path, csv).expect("parity csv");
        println!("L5E csv {}", path.display());
        run
    }

    /// Gate one run on emitted per tick (the emitters alone, on identical
    /// inputs). Population per type is printed through the same windows but
    /// not gated: its lifecycle step runs on unequal motion inputs (the GPU
    /// advects through its substep history, the engine once at the tick dt),
    /// so a gap there is not an emitter verdict.
    fn judge_run(label: &str, run: &ParityRun) -> Vec<String> {
        let (mut rows, mut failures) = (Vec::new(), Vec::new());
        gate(&Series { name: "emitted", gpu: run.gpu_emitted.clone(), engine: run.engine_emitted.clone() }, &mut rows, &mut failures);
        let mut ungated = Vec::new();
        for (k, name) in ["foam", "bubble", "spray"].into_iter().enumerate() {
            gate(&Series { name, gpu: run.gpu_kinds[k].clone(), engine: run.engine_kinds[k].clone() }, &mut rows, &mut ungated);
        }
        println!("L5E {label} (emitted gated; population printed only, {} windows outside their band):", ungated.len());
        for row in rows {
            println!("L5E   {row}");
        }
        failures
    }

    /// L5 emission parity: `node.whitewater_step` against FLIP Fluids'
    /// DiffuseParticleSimulation as FluidSimulation configures it
    /// (`whitewater_oracle::emit_engine`), on eight engine seeds, fed the
    /// water the step saw each tick at the tick dt. The engine's emitted
    /// count is taken inside its own emitter, before its lifecycle step.
    /// Every gate is the windowed [`gate`] (cap, floor, quiet-window ceiling):
    ///
    /// - the shipped step's emitted per tick;
    /// - turbulence emission alone: wavecrest off on both sides and the
    ///   turbulence thresholds lowered to 20–100 on both, so the dam break's
    ///   interior shear emits enough to judge (at the shipped 100–200 it
    ///   makes about 45 particles a run, too few to bound);
    /// - two controls that must fail: the GPU alone with wavecrest off, and
    ///   the turbulence fixture with the GPU's turbulence rate at 0.7 times.
    ///
    /// Not gated: population per type, printed and in the CSV, whose
    /// lifecycle step differs in its motion inputs (the GPU advects through
    /// `substep_schedule` and `substep_u/v/w`, the engine once on the
    /// end-of-tick faces; BUG-sipwn). Known emitter difference: the GPU skips
    /// emitters whose own particle velocity is under 1e-3 m/s
    /// (`shaders/turbulence_emission_count_body.wgsl`); FLIP samples the
    /// field velocity and has no such cut. The engine oracle covers constant
    /// default influence only (no obstacle sources).
    #[test]
    fn whitewater_step_against_engine_emitters_150() {
        use whitewater_oracle::EmissionOptions;
        let shipped = EmissionOptions::default();
        let turbulence_engine = EmissionOptions { wavecrest: 0.0, minimum: 20.0, maximum: 100.0, ..shipped };
        let turbulence_gpu = |rate: f64| json!({
            "wavecrest_emission": {"type": "Float", "value": 0.0},
            "turbulence_emission": {"type": "Float", "value": rate},
            "min_turbulence": {"type": "Float", "value": 20.0},
            "max_turbulence": {"type": "Float", "value": 100.0},
        });

        let base = parity_run(json!({}), shipped, "whitewater_step_vs_engine_150.csv");
        let mut failures = judge_run("shipped step", &base);
        let turbulence = parity_run(turbulence_gpu(shipped.turbulence), turbulence_engine, "whitewater_step_vs_engine_150_turbulence_only.csv");
        failures.extend(judge_run("turbulence only, both sides", &turbulence));

        let no_wavecrest = parity_run(json!({"wavecrest_emission": {"type": "Float", "value": 0.0}}), shipped, "whitewater_step_vs_engine_150_no_wavecrest.csv");
        let wavecrest_failures = judge_run("control: wavecrest off on the GPU only", &no_wavecrest);
        let reduced = parity_run(turbulence_gpu(0.7 * shipped.turbulence), turbulence_engine, "whitewater_step_vs_engine_150_turbulence_reduced.csv");
        let reduced_failures = judge_run("control: GPU turbulence rate x0.7", &reduced);
        println!("L5E controls: wavecrest off failed {} windows, turbulence x0.7 failed {} windows", wavecrest_failures.len(), reduced_failures.len());
        assert!(!wavecrest_failures.is_empty(), "the gate passed a step with its wavecrest emitter off");
        assert!(!reduced_failures.is_empty(), "the gate passed a step with its turbulence rate cut to 0.7");
        assert!(failures.is_empty(), "the step's emission strays from FLIP's engine on the same water: {failures:#?}");
    }
}

/// The Dam Break at 16 with the frame's presentation and the domain's
/// simulation time probed.
fn history_def() -> EffectGraphDef {
    let mut g = Appender::new(render_def(WaterScene::dam_break(16).with_faces()));
    let frame = g.id("frame");
    for port in HISTORY_PROBES[..HISTORY_PROBES.len() - 1].iter() {
        g.probe(port, (frame, port));
    }
    let domain = g.id("domain");
    g.probe("simulation_time", (domain, "simulation_time"));
    g.finish()
}

/// [`history_def`] with the frame's `display_cursor` authored to `cursor`,
/// which the loader keeps.
fn authored_cursor_history_def(cursor: f32) -> EffectGraphDef {
    let mut g = Appender::new(history_def());
    let frame = g.id("frame");
    g.retain_wires(frame, |w| !(w["toNode"] == frame && w["toPort"] == "display_cursor"));
    let exact = g.node_in_scope("exact", "node.value", json!({"value": {"type": "Float", "value": cursor}}), frame);
    g.wire((exact, "out"), frame, "display_cursor");
    g.finish()
}

/// The frame's presentation; the last is the domain's simulation time.
const HISTORY_PROBES: [&str; 9] =
    ["blend", "span", "count_a", "count_b", "identity_a", "identity_b", "presented_time", "publications_skipped", "simulation_time"];
/// The probes that describe the selected pair.
const PAIR: std::ops::Range<usize> = 0..7;


/// GPU_FLIP_DISPLAY_HISTORY_DESIGN.md section 4 (Conviction tests): a live
/// frame held until its publication retired shows what export shows at that
/// frame: the same endpoints and identities, A and B bytes, blend, span and
/// pixels. Export commit-waits each publication; live retires it on a later
/// frame, so after each frame live holds the clock until the presentation
/// matches, within eight frames.
#[test]
fn liquid_frame_live_held_frame_matches_offline() {
    const FRAMES: usize = 40;
    // Export with the cursor requested: liquid_frame's offline override must
    // still present exactly.
    let mut offline = Show::new(authored_cursor_history_def(1.0), (96, 54), false, &[]);
    offline.restart();
    let mut expected = Vec::new();
    for _ in 0..FRAMES {
        offline.frame(false);
        expected.push((
            offline.probes(HISTORY_PROBES),
            offline.provided_copy("frame", "particles_a"),
            offline.provided_copy("frame", "particles_b"),
            offline.readback(),
        ));
    }
    let _live = manifold_nodes_water::physics::PhysicsStepScope::with_preview_budget(false, std::time::Duration::from_secs(1));
    // Live runs exact: the cursor presents behind the request by design.
    let mut live = Show::new(authored_cursor_history_def(0.0), (96, 54), false, &[]);
    live.restart();
    let mut holds = 0;
    for (k, (probes, particles_a, particles_b, pixels)) in expected.iter().enumerate() {
        live.frame(false);
        live.set_paused(true);
        let mut held = 0;
        while live.probes(HISTORY_PROBES)[PAIR] != probes[PAIR] {
            assert!(held < 8, "frame {k}: live never presented export's pair: live {:?}, export {probes:?}", live.probes(HISTORY_PROBES));
            live.frame(false);
            held += 1;
        }
        holds += held;
        live.set_paused(false);
        assert_eq!(&live.provided_copy("frame", "particles_a"), particles_a, "frame {k}: the selected A differs");
        assert_eq!(&live.provided_copy("frame", "particles_b"), particles_b, "frame {k}: the selected B differs");
        assert!(live.readback() == *pixels, "frame {k}: pixels differ from export at the same presentation");
        assert_eq!(live.probes(HISTORY_PROBES)[7], 0.0, "no publication skipped");
    }
    println!("live held {holds} frames over {FRAMES} until its publications retired");
}

/// Each whitewater class is read from the selected slot: on every frame all
/// four classes equal the state's classes at the frame that published B,
/// found by B's time, and at least one frame shows a B older than the state.
#[test]
fn liquid_frame_whitewater_reads_the_selected_slot() {
    let _live = manifold_nodes_water::physics::PhysicsStepScope::with_preview_budget(false, std::time::Duration::from_secs(1));
    let mut show = Show::new(history_def(), (96, 54), false, &[]);
    show.restart();
    // Per frame: simulation time and the four classes the state then held.
    let mut published: Vec<(f32, [Vec<u8>; 4])> = Vec::new();
    let (mut behind, mut with_foam) = (0, 0);
    for frame in 0..180 {
        show.frame(false);
        let probes = show.probes(HISTORY_PROBES);
        let classes = WHITEWATER_KINDS.map(|kind| show.provided_copy("state", &format!("{kind}_particles")));
        let time = probes[8];
        if published.last().is_none_or(|(t, _)| *t != time) {
            published.push((time, classes.clone()));
        }
        let (blend, span, presented) = (probes[0], probes[1], probes[6]);
        let t_b = presented + (1.0 - blend) * span;
        let (at, expected) = published
            .iter()
            .min_by(|(a, _), (b, _)| (a - t_b).abs().total_cmp(&(b - t_b).abs()))
            .expect("a publication");
        assert!((at - t_b).abs() < 1e-4, "frame {frame}: B at {t_b} matches no publication");
        for (k, kind) in WHITEWATER_KINDS.iter().enumerate() {
            let shown = show.provided_copy("frame", &format!("{kind}_b"));
            assert!(shown == expected[k], "frame {frame}: {kind}_b is not B's publication at {at}");
        }
        let foam = &expected[0];
        with_foam += usize::from(foam.chunks_exact(32).any(|p| f32::from_ne_bytes([p[12], p[13], p[14], p[15]]) > 0.0));
        behind += usize::from(*at != time && expected != &classes);
    }
    assert!(with_foam > 0, "the scene throws foam");
    assert!(behind > 0, "some frame shows a B older than the state");
}

/// A publication that fails to encode still ends the frame with the frame's
/// outputs: the selection happens before publishing, so the failing frame
/// shows exactly what the same frame shows without the failure, arrays and
/// scalars together, and names the error.
#[test]
fn liquid_frame_encode_failure_publishes_the_selected_outputs() {
    let _live = manifold_nodes_water::physics::PhysicsStepScope::with_preview_budget(false, std::time::Duration::from_secs(1));
    let [mut clean, mut failing] = [(), ()].map(|()| {
        let mut show = Show::new(history_def(), (96, 54), false, &[]);
        show.restart();
        show
    });
    for frame in 0..30 {
        // Armed from frame 5 until a publication consumes it.
        let armed = frame >= 5;
        clean.frame(false);
        manifold_nodes_water::primitives::liquid_frame::FAIL_NEXT_PUBLICATION.set(armed);
        failing.expect_node_error(armed);
        failing.frame(false);
        failing.expect_node_error(false);
        let inject = armed && !manifold_nodes_water::primitives::liquid_frame::FAIL_NEXT_PUBLICATION.get();
        manifold_nodes_water::primitives::liquid_frame::FAIL_NEXT_PUBLICATION.set(false);
        if inject {
            assert!(failing.last_status().starts_with("Failed"), "frame {frame}: the failure is reported: {}", failing.last_status());
            assert_eq!(failing.probes(HISTORY_PROBES)[PAIR], clean.probes(HISTORY_PROBES)[PAIR], "frame {frame}: scalars follow the selection");
            for port in ["particles_a", "particles_b"] {
                assert_eq!(failing.provided_copy("frame", port), clean.provided_copy("frame", port), "frame {frame}: {port}");
            }
            // The failed endpoint is never retried, so the two runs part.
            return;
        }
    }
    panic!("no publication consumed the injected failure");
}

/// A smaller lattice keeps the solid's storage, wired or as walls: the
/// solid mix's output capacity is planned once and never shrinks.
#[test]
fn liquid_frame_solid_shrink_keeps_mix_capacity() {
    for wired in [true, false] {
        let mut g = Appender::new(render_def(WaterScene::dam_break(32).with_faces()));
        let frame = g.id("frame");
        if !wired {
            g.retain_wires(frame, |w| !(w["toNode"] == frame && w["toPort"] == "solid"));
        }
        let def = g.finish();
        let spec = def.preset_metadata.as_ref()
            .and_then(|cards| cards.params.iter().find(|card| card.id == "resolution"))
            .expect("the Resolution card")
            .clone();
        let mut show = Show::new(def, (96, 54), false, &[]);
        show.restart();
        for n in [32u32, 16] {
            let mut card = Param::bundled(spec.clone());
            card.value = n as f32;
            card.base = n as f32;
            show.set_cards(ParamManifest::from_params(vec![card]));
            for _ in 0..4 {
                show.frame(false);
            }
            assert!(show.errors().is_empty(), "solid wired {wired}, Resolution {n}: {:?}", show.errors());
        }
    }
}

use manifold_nodes_water::testkit::whitewater_scene::*;

const WHITEWATER_KINDS: [&str; 4] = ["foam", "bubble", "spray", "dust"];
