use manifold_nodes_water::presets::gpu_flip::testkit::surface_detail_offset;

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::*;
use manifold_node_engine::exec::extent::{ExtentError, ExtentReport};
use manifold_nodes_water::presets::gpu_flip::*;
use manifold_nodes_water::testkit::liquid_extents::*;
use serde_json::json;

use manifold_node_engine::{parameters::ParamValue, persistence::PrimitiveRegistry};

fn walked(def: &EffectGraphDef, frozen: bool, what: &str) -> ExtentReport {
    walk(def, frozen).unwrap_or_else(|error| panic!("{what}: {error}"))
}

/// Every lattice a scene may use, multiples of 16 from 16 to 256, the
/// sides between the powers of two included. Each is proven here before
/// any GPU run at it. 256 holds the pool and column only with a lower
/// fill: the Dam Break there places more particles than a count carries,
/// and the domain refuses it by name
/// (`gpu_flip_dam_break_past_the_count_rail_is_refused`).
const LATTICES: [usize; 6] = [16, 32, 48, 64, 96, 128];

/// Every running scene at every lattice, and the probes' variants, before
/// any GPU run of it: the tick region's steps, the stats, the frame and
/// the surface. The solves run inside each step, so the tick has no
/// inner region.
#[test]
fn gpu_flip_scenes_cover_every_dispatch() {
    let scenes = [
        WaterScene::dam_break,
        WaterScene::still_pool,
        WaterScene::deep_pool,
        WaterScene::deep_drop,
        WaterScene::free_fall,
    ];
    let all = LATTICES
        .into_iter()
        .flat_map(|n| scenes.map(|at| at(n)))
        .flat_map(|scene| [scene, scene.with_surface()]);
    // The splash probes' scenes.
    let refined = WaterScene::dam_break(128).with_surface();
    let step = WaterScene::dam_break(128);
    let probes = [
        refined.with_iterations(12),
        WaterScene {
            steps: 4,
            ..refined
        },
        step.with_iterations(4),
        WaterScene::dam_break(64).with_steps(1).with_surface(),
    ];
    // The open-face drain.
    let open = [WaterScene::still_pool(64).with_closed_faces(63 & !1)];
    for scene in all.chain(probes).chain(open) {
        let n = scene.pressure.n;
        let def = water_def(scene);
        let (graph, plan) = built(&def);
        let regions = plan.substep_regions();
        assert_eq!(regions.len(), 1, "one tick region");
        let report = walked(&def, false, &format!("scene {n}³, {} steps", scene.steps));
        assert!(
            report.checked >= 7,
            "checked only {} nodes at {n}³, {} steps",
            report.checked,
            scene.steps
        );
        let fielded = plan.steps().iter().any(|step| {
            graph.nodes().any(|node| {
                node.id == step.node && node.node.type_id().as_str() == "node.field_smooth_blend"
            })
        });
        assert_eq!(
            fielded, scene.surface,
            "the surface is in the plan exactly when asked for"
        );
    }
}

/// The tick region's body is the tick: the step and the stats, and
/// nothing the frame or the domain runs once a frame.
#[test]
fn gpu_flip_tick_region_is_the_tick() {
    let scene = WaterScene::dam_break(64).with_surface();
    let (graph, plan) = built(&water_def(scene));
    let region = &plan.substep_regions()[0];
    let name = |step: usize| {
        graph
            .get_node(plan.steps()[step].node)
            .expect("plan node")
            .node_id
            .as_str()
            .to_string()
    };
    assert_eq!(
        graph
            .get_node(region.boundary)
            .expect("boundary")
            .node_id
            .as_str(),
        "state"
    );
    let body: Vec<String> = region.steps.iter().map(|&step| name(step)).collect();
    assert_eq!(
        body.iter().filter(|node| *node == STEP_NODE).count(),
        1,
        "the tick runs one step node"
    );
    assert!(
        body.iter().any(|node| node == "stats"),
        "the stats run every tick"
    );
    let outside = ["domain", "fill", "frame", "initial_column"];
    assert!(
        !body
            .iter()
            .any(|node| outside.contains(&node.as_str()) || node.starts_with("surface")),
        "{body:?}"
    );
}

/// The pool the domain gives an inflow costs the rendered water about
/// 1 GB (BUG-3zq4r, inflow runs out): each slot's bytes across every
/// particle buffer, from two pools the walk sizes. The domain sizes the
/// pool, so the walk takes the fill's own count with its wire cut.
#[test]
fn gpu_flip_auto_inflow_pool_is_about_a_gigabyte() {
    use manifold_water_gpu_flip::primitives::gpu_flip_domain::AUTO_INFLOW_SLOTS;
    fn set_slots(nodes: &mut [EffectGraphNode], wires: &mut Vec<EffectGraphWire>, slots: i32) {
        let fills: Vec<u32> = nodes
            .iter()
            .filter(|node| node.type_id == "node.liquid_fill")
            .map(|node| node.id)
            .collect();
        wires
            .retain(|wire| !(fills.contains(&wire.to_node) && wire.to_port == "particle_capacity"));
        for node in nodes {
            if node.type_id == "node.liquid_fill" {
                node.params.insert(
                    "particle_capacity".into(),
                    serde_json::from_value(json!({"type": "Int", "value": slots})).unwrap(),
                );
            }
            if let Some(group) = node.group.as_mut() {
                set_slots(&mut group.nodes, &mut group.wires, slots);
            }
        }
    }
    let bytes = |slots: i32| {
        let mut def = render_def(WaterScene::dam_break(64));
        set_slots(&mut def.nodes, &mut def.wires, slots);
        walked(&def, false, "pool slots").scene_bytes
    };
    let per_slot = (bytes(3_000_000) - bytes(1_000_000)) as f64 / 2_000_000.0;
    let pool = per_slot * f64::from(AUTO_INFLOW_SLOTS);
    println!(
        "GPU FLIP auto inflow pool: {per_slot:.0} bytes a slot, {:.2} GB",
        pool / 1e9
    );
    assert!((0.5e9..1.5e9).contains(&pool), "{pool} bytes");
}

/// Cell Size is a card: the graph built at 64 cells across the tank runs at
/// any cell size, odd and uneven counts included, because every lattice
/// node reads the domain's wires and the step's faces follow them.
#[test]
fn gpu_flip_any_cell_size_walks_on_the_built_graph() {
    for n in [16, 24, 32, 63, 72, 100, 128] {
        let mut def = render_def(WaterScene::dam_break(64));
        let domain = find_node_mut(&mut def.nodes, "domain").expect("domain");
        domain.params.insert(
            "cell_metres".into(),
            manifold_core::effect_graph_def::SerializedParamValue::Float {
                value: 4.0 / n as f32,
            },
        );
        let report = walked(&def, false, &format!("the 64³ graph at {n} cells across"));
        assert!(report.scene_bytes > 0);
    }
}

/// Past the count a wire carries exactly, the domain refuses the Dam
/// Break by name before any GPU work.
#[test]
fn gpu_flip_dam_break_past_the_count_rail_is_refused() {
    let scene = WaterScene::dam_break(128);
    let mut def = water_def(scene);
    let domain = def
        .nodes
        .iter_mut()
        .find(|node| node.node_id.as_str() == "domain")
        .expect("domain");
    domain.params.insert(
        "cell_metres".into(),
        manifold_core::effect_graph_def::SerializedParamValue::Float { value: 4.0 / 256.0 },
    );
    match walk(&def, false) {
        Err(ExtentError::Refused { node, reason }) => {
            assert!(
                node.starts_with("domain")
                    && reason.contains("Cell Size")
                    && reason.contains("Fill Height"),
                "{node}: {reason}"
            );
        }
        other => panic!("expected the domain to refuse 256³, got {other:?}"),
    }
}

/// The scenes as the render smoke runs them, inside the shipped render
/// graph (`render_def`), at every lattice, and the shipped lattice at
/// every Surface Detail. No card overwrites a def param at build, so the
/// runtime runs the graph this walk checks.
#[test]
fn gpu_flip_rendered_scenes_cover_every_dispatch() {
    let scenes = [WaterScene::dam_break, WaterScene::still_pool];
    let coarser = LATTICES
        .into_iter()
        .flat_map(|n| [1, 2].map(|scale| WaterScene::dam_break(n).with_surface_scale(scale)));
    let detail = (surface_detail_offset()..=surface_detail_offset() + 2)
        .map(|scale| WaterScene::dam_break(64).with_surface_scale(scale));
    // The cadence probe: one step a tick.
    let cadence = [WaterScene::dam_break(64).with_steps(1)];
    let registry = PrimitiveRegistry::with_builtin();
    for scene in LATTICES
        .into_iter()
        .flat_map(|n| scenes.map(|at| at(n)))
        .chain(coarser)
        .chain(detail)
        .chain(cadence)
    {
        let n = scene.pressure.n;
        let def = render_def(scene);
        let (_, plan) = built(&def);
        assert_eq!(plan.substep_regions().len(), 1, "one tick region");
        let report = walked(&def, false, &format!("rendered {n}³"));
        assert!(
            report.checked >= 7,
            "checked only {} nodes at {n}³",
            report.checked
        );
        let runtime = manifold_node_engine::runtime::PresetRuntime::from_def(def, &registry, None)
            .expect("the rendered scene builds");
        let shadowed: Vec<_> = runtime.shadowed_def_params().collect();
        assert!(
            shadowed.is_empty(),
            "{n}³ at surface scale {}: cards overwrite def params: {shadowed:?}",
            scene.surface_scale
        );
    }
}

/// Each fused region of `def` as its members' node ids, `a + b`.
fn fused_regions(def: &EffectGraphDef) -> Vec<String> {
    let report = manifold_node_engine::freeze::fusion_report(def, &registry());
    let name = |id: u32| {
        def.nodes
            .iter()
            .find(|n| n.id == id)
            .map_or("?".to_string(), |n| n.node_id.as_str().to_string())
    };
    report
        .regions
        .iter()
        .map(|r| {
            r.member_node_ids
                .iter()
                .map(|&id| name(id))
                .collect::<Vec<_>>()
                .join(" + ")
        })
        .collect()
}

/// The frozen graphs at every lattice, before any GPU run of them: the
/// running scene bare and meshed, and the render graph.
#[test]
fn gpu_flip_frozen_graphs_cover_every_dispatch() {
    for n in LATTICES {
        let scene = WaterScene::dam_break(n);
        for scene in [scene.with_surface(), scene] {
            let report = walked(&render_def(scene), true, &format!("frozen render {n}³"));
            assert!(
                report.checked >= 7,
                "checked only {} nodes at {n}³",
                report.checked
            );
        }
    }
}

/// The step is one boundary node: nothing in the water fuses.
#[test]
fn gpu_flip_step_does_not_fuse() {
    assert_eq!(
        fused_regions(&water_def(WaterScene::dam_break(64))),
        Vec::<String>::new()
    );
}

#[test]
fn gpu_flip_sheet_fill_rate_card_binding_and_wire_round_trip() {
    use manifold_core::NodeId;
    use manifold_core::params::{Param, ParamManifest};
    use manifold_node_engine::runtime::PresetRuntime;

    for name in [SHIPPED_PRESET, PARTICLE_VIEW_PRESET] {
        let json = scene_json(&PresetTypeId::new(name)).unwrap();
        let def: EffectGraphDef = serde_json::from_str(&json).unwrap();
        let saved = serde_json::to_string(&def).unwrap();
        let def: EffectGraphDef = serde_json::from_str(&saved).unwrap();
        let value = serde_json::to_value(&def).unwrap();
        let card = value["presetMetadata"]["params"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "sheet_fill_rate")
            .expect("sheet card");
        assert_eq!(card["name"], "Sheet Fill Rate");
        assert_eq!(card["min"], 0.0);
        assert_eq!(card["max"], 1.0);
        assert_eq!(card["defaultValue"], SHEET_FILL_RATE);
        assert_eq!(card["wholeNumbers"], false);
        let bindings: Vec<_> = value["presetMetadata"]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["id"] == "sheet_fill_rate")
            .collect();
        assert_eq!(bindings.len(), 1);
        assert_eq!(
            bindings[0]["target"],
            json!({"kind":"node", "nodeId":"domain", "param":"sheet_fill_rate"})
        );
        assert_eq!(bindings[0]["defaultValue"], SHEET_FILL_RATE);
        let flat = manifold_core::flatten::flatten_groups(&def).unwrap();
        let domain = flat
            .nodes
            .iter()
            .find(|n| n.node_id.as_str() == "domain")
            .unwrap()
            .id;
        let step = flat
            .nodes
            .iter()
            .find(|n| n.node_id.as_str() == STEP_NODE)
            .unwrap()
            .id;
        assert!(flat.wires.iter().any(|w| w.from_node == domain
            && w.from_port == "sheet_fill_rate"
            && w.to_node == step
            && w.to_port == "sheet_fill_rate"));
        let mut params = ParamManifest::from_params(
            def.preset_metadata
                .as_ref()
                .unwrap()
                .params
                .iter()
                .cloned()
                .map(Param::bundled)
                .collect(),
        );
        let mut runtime =
            PresetRuntime::from_def(def, &PrimitiveRegistry::with_builtin(), None).unwrap();
        for rate in [0.0, 0.375, 1.0, 0.0] {
            let param = params.get_mut("sheet_fill_rate").unwrap();
            param.value = rate;
            param.base = rate;
            runtime.apply_param_values(&params);
            let id = runtime
                .graph
                .instance_by_node_id(&NodeId::new("domain"))
                .unwrap();
            let node = runtime.graph.get_node(id).unwrap();
            assert_eq!(
                node.params.get("sheet_fill_rate"),
                Some(&ParamValue::Float(rate))
            );
            let geometry = gpu_flip_geometry(
                |key, default| {
                    node.params
                        .get(key)
                        .map(manifold_node_engine::snapshot::param_default_to_f32)
                        .unwrap_or(default)
                },
                None,
                None,
            )
            .unwrap();
            assert_eq!(geometry.sheet_fill_rate_for_test(), rate);
        }
    }
}

/// Every per-step duration input in every graph the builder ships is fed
/// by its domain's accepted interval. Left on its param, one holds 1/60 s
/// at every Sim Rate: at 30 Hz whitewater emits, ages and moves at half
/// rate, and export poses moving solids at the wrong time.
#[test]
fn gpu_flip_builder_graphs_feed_every_interval_input() {
    let registry = PrimitiveRegistry::with_builtin();
    for (type_id, port) in INTERVAL_DURATION_INPUTS {
        let node = registry
            .construct(type_id)
            .unwrap_or_else(|| panic!("{type_id} is not registered"));
        assert!(
            node.inputs().iter().any(|input| input.name == port),
            "{type_id} has no {port} input"
        );
    }
    let bundled = |name: &'static str| -> EffectGraphDef {
        let json =
            scene_json(&PresetTypeId::new(name)).unwrap_or_else(|| panic!("{name} is bundled"));
        serde_json::from_str(&json).expect("the preset parses")
    };
    for (name, def) in [
        (SHIPPED_PRESET, bundled(SHIPPED_PRESET)),
        (PARTICLE_VIEW_PRESET, bundled(PARTICLE_VIEW_PRESET)),
        ("Add Fluid's liquid body", gpu_flip_liquid_body()),
    ] {
        let flat = manifold_core::flatten::flatten_groups(&def).expect("flattens");
        let fed_by_domain = |node: u32, port: &str| {
            let feeds: Vec<_> = flat
                .wires
                .iter()
                .filter(|w| w.to_node == node && w.to_port == port)
                .collect();
            feeds.len() == 1
                && feeds[0].from_port == "interval_duration"
                && flat.nodes.iter().any(|n| {
                    n.id == feeds[0].from_node
                        && manifold_core::liquid_domain::is_liquid_domain(&n.type_id)
                })
        };
        let mut checked = 0;
        for node in &flat.nodes {
            for (type_id, port) in INTERVAL_DURATION_INPUTS {
                if node.type_id == type_id {
                    assert!(
                        fed_by_domain(node.id, port),
                        "{name}: {}.{port} is not fed by its domain's interval_duration",
                        node.node_id.as_str()
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "{name} has no interval inputs");
    }
}

#[test]
fn liquid_presets_feed_state_dropped_time_from_their_clock_domain() {
    let mut graphs = vec![
        ("GPU FLIP builder", render_def(WaterScene::dam_break(16))),
        ("Add Fluid's liquid body", gpu_flip_liquid_body()),
    ];
    for name in [
        SHIPPED_PRESET,
        PARTICLE_VIEW_PRESET,
        "WaterDamBreakMatter",
        "WaterStillPoolMatter",
        "WaterFloatingBoxMatter",
    ] {
        let json =
            scene_json(&PresetTypeId::new(name)).unwrap_or_else(|| panic!("{name} is bundled"));
        graphs.push((
            name,
            serde_json::from_str(&json).expect("the preset parses"),
        ));
    }
    for (name, def) in graphs {
        let flat = manifold_core::flatten::flatten_groups(&def).expect("flattens");
        let mut checked = 0;
        for state in flat.nodes.iter().filter(|node| {
            matches!(
                node.type_id.as_str(),
                "node.liquid_state" | "node.matter_state"
            )
        }) {
            let feeds: Vec<_> = flat
                .wires
                .iter()
                .filter(|w| w.to_node == state.id && w.to_port == "dropped_seconds")
                .collect();
            assert_eq!(
                feeds.len(),
                1,
                "{name}: {} needs one dropped_seconds wire",
                state.node_id.as_str()
            );
            assert_eq!(feeds[0].from_port, "dropped_seconds");
            let tick_source = flat
                .wires
                .iter()
                .find(|w| w.to_node == state.id && w.to_port == "ticks")
                .expect("state has a clock");
            assert_eq!(
                feeds[0].from_node, tick_source.from_node,
                "{name}: dropped time belongs to the state's clock"
            );
            assert!(flat.nodes.iter().any(|node| node.id == feeds[0].from_node
                && manifold_core::liquid_domain::is_liquid_domain(&node.type_id)));
            checked += 1;
        }
        assert!(checked > 0, "{name} has no liquid state");
    }
}

/// Every GPU FLIP frame presents through the cursor: `display_cursor`
/// and `dropped_seconds` come from the domain that clocks it.
#[test]
fn gpu_flip_frames_take_the_cursor_from_their_clock_domain() {
    let mut graphs = vec![
        ("GPU FLIP builder", render_def(WaterScene::dam_break(16))),
        ("Add Fluid's liquid body", gpu_flip_liquid_body()),
    ];
    for name in [SHIPPED_PRESET, PARTICLE_VIEW_PRESET] {
        let json =
            scene_json(&PresetTypeId::new(name)).unwrap_or_else(|| panic!("{name} is bundled"));
        graphs.push((
            name,
            serde_json::from_str(&json).expect("the preset parses"),
        ));
    }
    for (name, def) in graphs {
        let flat = manifold_core::flatten::flatten_groups(&def).expect("flattens");
        let mut checked = 0;
        for frame in flat
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.liquid_frame")
        {
            let clock = flat
                .wires
                .iter()
                .find(|w| w.to_node == frame.id && w.to_port == "epoch")
                .expect("the frame has a clock");
            for port in ["display_cursor", "dropped_seconds"] {
                let feeds: Vec<_> = flat
                    .wires
                    .iter()
                    .filter(|w| w.to_node == frame.id && w.to_port == port)
                    .collect();
                assert_eq!(feeds.len(), 1, "{name}: one {port} wire");
                assert_eq!(
                    (feeds[0].from_node, feeds[0].from_port.as_str()),
                    (clock.from_node, port),
                    "{name}: {port} from the frame's clock"
                );
            }
            checked += 1;
        }
        assert!(checked > 0, "{name} has no liquid frame");
    }
}

/// BUG-215v: whitewater belongs to the liquid region, consumes the
/// solver phi, and every persistent or rendered result closes through
/// the boundary. This checks real preset compilation without a GPU.
#[test]
fn whitewater_per_tick_preset_closes_the_liquid_region() {
    let def = render_def(WaterScene::dam_break(16));
    let (graph, plan) = built(&def);
    let def = manifold_core::flatten::flatten_groups(&def).expect("grouped region topology");
    let whitewater_node = graph
        .nodes()
        .find(|n| n.node_id.as_str() == "whitewater")
        .unwrap();
    let whitewater = whitewater_node.id;
    let region = plan
        .substep_regions()
        .iter()
        .find(|r| r.steps.iter().any(|&i| plan.steps()[i].node == whitewater))
        .expect("whitewater is per tick");
    let wire = |from, from_port: &str, to, to_port: &str| {
        def.wires.iter().any(|w| {
            w.from_node == from
                && w.from_port == from_port
                && w.to_node == to
                && w.to_port == to_port
        })
    };
    let boundary_name = &graph.get_node(region.boundary).unwrap().node_id;
    let boundary = def
        .nodes
        .iter()
        .find(|n| &n.node_id == boundary_name)
        .unwrap()
        .id;
    let whitewater = manifold_node_engine::exec::effect_node::NodeInstanceId(
        def.nodes
            .iter()
            .find(|n| n.node_id.as_str() == "whitewater")
            .unwrap()
            .id,
    );
    let distance = def
        .wires
        .iter()
        .find(|w| w.to_node == whitewater.0 && w.to_port == "distance")
        .unwrap();
    assert_eq!(distance.from_port, "distance");
    assert!(
        def.nodes
            .iter()
            .any(|n| n.id == distance.from_node && n.type_id == "node.gpu_flip_step")
    );
    for port in [
        "faces",
        "tiles",
        "pool_tiles",
        "substep_schedule",
        "substep_faces",
        "substep_count",
    ] {
        assert!(
            wire(distance.from_node, port, whitewater.0, port),
            "missing accepted {port}"
        );
    }
    for port in [
        "forces",
        "impulses",
        "field_nodes_x",
        "field_nodes_y",
        "field_nodes_z",
        "field_spacing",
        "force_lattices",
        "first_tick",
        "regions",
        "region_count",
        "shapes",
        "atlas",
    ] {
        let water = def
            .wires
            .iter()
            .find(|w| w.to_node == distance.from_node && w.to_port == port)
            .expect(port);
        assert!(
            wire(water.from_node, &water.from_port, whitewater.0, port),
            "{port} must share the liquid source"
        );
    }
    for (output, capture, held) in [
        ("pool_out", "whitewater_pool_in", "whitewater_pool"),
        ("state_out", "whitewater_state_in", "whitewater_state"),
        ("counts_out", "whitewater_counts_in", "whitewater_counts"),
        ("foam_particles", "foam_particles_in", "foam_particles"),
        (
            "bubble_particles",
            "bubble_particles_in",
            "bubble_particles",
        ),
        ("spray_particles", "spray_particles_in", "spray_particles"),
        ("dust_particles", "dust_particles_in", "dust_particles"),
    ] {
        assert!(
            wire(whitewater.0, output, boundary, capture),
            "missing {capture}"
        );
        assert!(
            !def.wires.iter().any(|w| w.from_node == whitewater.0
                && w.from_port == output
                && w.to_node != boundary),
            "{held} escapes the boundary"
        );
    }
    for frozen in [false, true] {
        walked(&def, frozen, "16³ per-tick whitewater");
    }
}

/// Physics View swaps which side runs. With the card on, no surface node
/// is live while the dots' GPU chain, the water and the simulation are.
/// With it off the surface is live and nothing of the dots that does GPU
/// work is: the normal show pays nothing for the toggle.
#[test]
fn gpu_flip_physics_view_skips_the_surface() {
    use manifold_node_engine::exec::execution::Executor;
    fn names(nodes: &[EffectGraphNode], into: &mut Vec<String>) {
        // The atoms a flattened graph plans: groups and their boundaries dissolve.
        for node in nodes {
            match &node.group {
                Some(group) => names(&group.nodes, into),
                None if !node.type_id.starts_with("system.group_") => {
                    into.push(node.node_id.as_str().to_string())
                }
                None => {}
            }
        }
    }
    let def = render_def(WaterScene::dam_break(64));
    let family = find_node(&def.nodes, "water_family").expect("Water family");
    let surface = find_node(
        &family.group.as_ref().expect("family group").nodes,
        "surface",
    )
    .expect("surface");
    let mut surface_nodes = Vec::new();
    names(
        &surface.group.as_ref().expect("surface group").nodes,
        &mut surface_nodes,
    );
    assert!(surface_nodes.len() > 5, "{surface_nodes:?}");
    // Close Gaps starts off, so its closing runs in neither view.
    surface_nodes.retain(|name| name != "liquid_close");
    let card: Vec<_> = def
        .preset_metadata
        .as_ref()
        .unwrap()
        .bindings
        .iter()
        .filter(|binding| binding.id == "physics_view")
        .collect();
    for (view, surface_live) in [(1.0, false), (0.0, true)] {
        let (mut graph, plan) = built(&def);
        for binding in &card {
            let BindingTarget::Node { node_id, param } = &binding.target else {
                panic!("node target")
            };
            let id = graph.instance_by_node_id(node_id).expect("card target");
            graph.set_param(id, param, ParamValue::Float(view)).unwrap();
        }
        let mut executor = Executor::with_mock();
        let live = executor.live_steps_for(&graph, &plan);
        let ran: Vec<String> = plan
            .steps()
            .iter()
            .zip(live)
            .filter(|(_, live)| **live)
            .map(|(step, _)| {
                graph
                    .get_node(step.node)
                    .expect("planned node")
                    .node_id
                    .as_str()
                    .to_string()
            })
            .collect();
        for name in &surface_nodes {
            assert_eq!(
                ran.contains(name),
                surface_live,
                "physics view {view}: surface node {name} live"
            );
        }
        for name in [
            "water_object",
            "dots_object",
            "domain",
            STEP_NODE,
            "frame",
            "whitewater",
        ] {
            assert!(
                ran.iter().any(|ran| ran == name),
                "physics view {view}: {name} must be live, live {ran:?}"
            );
        }
        for name in ["dots_mesh", "dots_blend", "dots_copies"] {
            assert_eq!(
                ran.iter().any(|ran| ran == name),
                !surface_live,
                "physics view {view}: dots GPU node {name} live"
            );
        }
    }
}

use manifold_nodes::engine_scenes::scene_json;
use manifold_water_gpu_flip::primitives::gpu_flip_domain::gpu_flip_geometry;
use manifold_water_liquid::clock::INTERVAL_DURATION_INPUTS;
