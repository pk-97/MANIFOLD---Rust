use manifold_nodes_water::primitives::gpu_flip_preset::testkit::{family_outputs, surface_detail_offset, assert_preset_root, rendered_scene_bytes};

use manifold_nodes_water::primitives::gpu_flip_preset::*;
use manifold_nodes_water::testkit::liquid_extents::*;
#[cfg(feature = "gpu-proofs")]
use manifold_nodes_water::primitives::gpu_flip_preset::testkit::surface_group;
use manifold_core::effect_graph_def::*;
use manifold_core::PresetTypeId;
use serde_json::{Value, json};
    use manifold_node_engine::exec::extent::{ExtentError, ExtentReport};

    use manifold_node_engine::{parameters::ParamValue, persistence::PrimitiveRegistry};

    #[test]
    fn gpu_flip_defaults_disable_optional_corrections_and_preserve_opt_ins() {
        use manifold_nodes_water::primitives::gpu_flip_step::GpuFlipStep;
        use manifold_node_engine::parameters::ParamValue;
        use manifold_node_engine::primitive::PrimitiveSpec;

        assert!(!WaterScene::dam_break(64).volume_projection);
        for name in ["volume_projection", "narrow_band", "solve_level"] {
            let param = GpuFlipStep::PARAMS.iter().find(|p| p.name == name).unwrap();
            assert_eq!(param.default, ParamValue::Float(0.0), "{name}");
        }
        for enabled in [false, true] {
            let def = water_def(WaterScene { volume_projection: enabled, ..WaterScene::dam_break(64) });
            let step = def.nodes.iter().find(|n| n.node_id.as_str() == STEP_NODE).unwrap();
            let params = serde_json::to_value(&step.params).unwrap();
            assert_eq!(params["volume_projection"]["value"], i32::from(enabled));
        }
    }











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

    #[test]
    fn gpu_flip_mesh_grid_uses_native_solid_coordinates() {
        for resolution in [8, 32, 64] {
            let scene = WaterScene::dam_break(resolution).with_surface();
            let geometry = scene.geometry();
            let outputs = geometry.outputs();
            let read = |name: &str| outputs.iter().find(|(port, _)| *port == name).unwrap().1;
            let surface = geometry.setup_for_test().lattice_for_test().surface();
            let def = water_def(scene);
            let id = |name: &str| def.nodes.iter().find(|n| n.node_id.as_str() == name).unwrap().id;
            let domain = id("domain");
            let solid = id("mesh_solid");
            let frame = id("frame");
            for (d, axis) in ["x", "y", "z"].into_iter().enumerate() {
                assert_eq!(read(&format!("mesh_nodes_{axis}")), (resolution + 4) as f32);
                assert_eq!(read(&format!("mesh_min_{axis}")), surface.min()[d]);
                for (source, target) in [(format!("mesh_min_{axis}"), format!("lattice_min_{axis}")),
                                         (format!("mesh_nodes_{axis}"), format!("nodes_{axis}"))] {
                    assert!(def.wires.iter().any(|w| w.from_node == domain && w.to_node == solid
                        && w.from_port == source && w.to_port == target));
                }
            }
            assert!(def.wires.iter().any(|w| w.from_node == solid && w.to_node == frame
                && w.from_port == "solid" && w.to_port == "solid"));
            // Frame's surface() uses the same source rule; the simulation
            // lattice continues to carry its original cells and padding.
            assert_eq!(geometry.setup_for_test().lattice_for_test().cells(), [resolution as u32; 3]);
            assert_eq!(geometry.setup_for_test().lattice_for_test().nodes(), [resolution as u32 + 7; 3]);
        }
    }

    /// Every running scene at every lattice, and the probes' variants, before
    /// any GPU run of it: the tick region's steps, the stats, the frame and
    /// the surface. The solves run inside each step, so the tick has no
    /// inner region.
    #[test]
    fn gpu_flip_scenes_cover_every_dispatch() {
        let scenes = [WaterScene::dam_break, WaterScene::still_pool, WaterScene::deep_pool, WaterScene::deep_drop, WaterScene::free_fall];
        let all = LATTICES.into_iter().flat_map(|n| scenes.map(|at| at(n))).flat_map(|scene| [scene, scene.with_surface()]);
        // The splash probes' scenes.
        let refined = WaterScene::dam_break(128).with_surface();
        let step = WaterScene::dam_break(128);
        let probes = [
            refined.with_iterations(12),
            WaterScene { steps: 4, ..refined },
            step.with_iterations(4),
            WaterScene::dam_break(64).with_steps(1).with_surface(),
        ];
        // The obstacle, bare and meshed, and in the still pool the kinematic
        // proof moves it through.
        let obstacle = LATTICES.into_iter().flat_map(|n| {
            let dam = WaterScene::dam_break(n).with_obstacle();
            [dam, dam.with_surface(), WaterScene::still_pool(n).with_obstacle()]
        });
        // The open-face drain.
        let open = [WaterScene::still_pool(64).with_closed_faces(63 & !1)];
        for scene in all.chain(probes).chain(obstacle).chain(open) {
            let n = scene.pressure.n;
            let def = water_def(scene);
            let (graph, plan) = built(&def);
            let regions = plan.substep_regions();
            assert_eq!(regions.len(), 1, "one tick region");
            let report = walked(&def, false, &format!("scene {n}³, {} steps", scene.steps));
            assert!(report.checked > 7, "checked only {} nodes at {n}³, {} steps", report.checked, scene.steps);
            let meshed = plan.steps().iter().any(|step| {
                graph.nodes().any(|node| node.id == step.node && node.node.type_id().as_str() == "node.volume_surface_mesh")
            });
            assert_eq!(meshed, scene.surface, "the surface is in the plan exactly when asked for");
        }
    }

    /// The tick region's body is the tick: the step and the stats, and
    /// nothing the frame or the domain runs once a frame.
    #[test]
    fn gpu_flip_tick_region_is_the_tick() {
        let scene = WaterScene::dam_break(64).with_surface();
        let (graph, plan) = built(&water_def(scene));
        let region = &plan.substep_regions()[0];
        let name = |step: usize| graph.get_node(plan.steps()[step].node).expect("plan node").node_id.as_str().to_string();
        assert_eq!(graph.get_node(region.boundary).expect("boundary").node_id.as_str(), "state");
        let body: Vec<String> = region.steps.iter().map(|&step| name(step)).collect();
        assert_eq!(body.iter().filter(|node| *node == STEP_NODE).count(), 1, "the tick runs one step node");
        assert!(body.iter().any(|node| node == "stats"), "the stats run every tick");
        let outside = ["domain", "fill", "mesh_solid", "frame", "initial_column"];
        assert!(!body.iter().any(|node| outside.contains(&node.as_str()) || node.starts_with("surface")), "{body:?}");
    }

    /// The rendered Dam Break's device bytes at every lattice, for the size
    /// ladder.
    #[test]
    fn gpu_flip_memory_at_every_lattice() {
        for n in LATTICES {
            for scale in [1, 2, 3] {
                let scene = WaterScene::dam_break(n).with_surface_scale(scale);
                let bytes = rendered_scene_bytes(scene);
                println!(
                    "GPU FLIP rendered Dam Break {n}³, surface scale {scale}: {} particles, {:.2} GB",
                    scene.particles(),
                    bytes as f64 / 1e9
                );
                assert!(bytes > 0);
            }
        }
    }

    /// Full array and held-buffer accounting at the shipped resolution. This
    /// excludes textures and later mesh growth; it is not a frame-time proof.
    #[test]
    fn gpu_flip_native_surface_detail_memory_at_64() {
        let coarse = rendered_scene_bytes(WaterScene::dam_break(64).with_surface_scale(1));
        let matched = rendered_scene_bytes(WaterScene::dam_break(64).with_surface_scale(2));
        assert!(matched > coarse);
        println!("GPU FLIP 64³ rendered arrays/held buffers: detail 0 {coarse} bytes; detail 1 {matched} bytes; increase {} bytes", matched - coarse);
        assert_eq!(WaterScene::dam_break(64).surface_scale, 1);
    }

    /// Resolution is a card: the graph built at 64 runs at any Resolution,
    /// odd and uneven sides included, because every lattice node reads the
    /// domain's wires and the step's faces follow them (BUG-o65k (GPU FLIP
    /// lattice wiring), BUG-9an1 (resolution change)).
    #[test]
    fn gpu_flip_any_resolution_walks_on_the_built_graph() {
        for n in [16, 24, 32, 63, 72, 100, 128] {
            let mut def = render_def(WaterScene::dam_break(64));
            let domain = find_node_mut(&mut def.nodes, "domain").expect("domain");
            domain.params.insert("resolution".into(), manifold_core::effect_graph_def::SerializedParamValue::Int { value: n });
            let report = walked(&def, false, &format!("the 64³ graph at Resolution {n}"));
            assert!(report.scene_bytes > 0);
        }
    }

    /// Past the count a wire carries exactly, the domain refuses the Dam
    /// Break by name before any GPU work.
    #[test]
    fn gpu_flip_dam_break_past_the_count_rail_is_refused() {
        let scene = WaterScene::dam_break(128);
        let mut def = water_def(scene);
        let domain = def.nodes.iter_mut().find(|node| node.node_id.as_str() == "domain").expect("domain");
        domain.params.insert("resolution".into(), manifold_core::effect_graph_def::SerializedParamValue::Int { value: 256 });
        match walk(&def, false) {
            Err(ExtentError::Refused { node, reason }) => {
                assert!(node.starts_with("domain") && reason.contains("Resolution") && reason.contains("Initial Fill Height"), "{node}: {reason}");
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
        let coarser = LATTICES.into_iter().flat_map(|n| [1, 2].map(|scale| WaterScene::dam_break(n).with_surface_scale(scale)));
        let detail = (surface_detail_offset()..=surface_detail_offset() + 2).map(|scale| WaterScene::dam_break(64).with_surface_scale(scale));
        // The cadence probe: one step a tick.
        let cadence = [WaterScene::dam_break(64).with_steps(1)];
        // The published face grid, at every lattice.
        let faces = LATTICES.into_iter().map(|n| WaterScene::dam_break(n).with_faces());
        let registry = PrimitiveRegistry::with_builtin();
        for scene in LATTICES.into_iter().flat_map(|n| scenes.map(|at| at(n))).chain(coarser).chain(detail).chain(cadence).chain(faces) {
            let n = scene.pressure.n;
            let def = render_def(scene);
            let (_, plan) = built(&def);
            assert_eq!(plan.substep_regions().len(), 1, "one tick region");
            let report = walked(&def, false, &format!("rendered {n}³"));
            assert!(report.checked > 7, "checked only {} nodes at {n}³", report.checked);
            let runtime = manifold_node_engine::runtime::PresetRuntime::from_def(def, &registry, None).expect("the rendered scene builds");
            let shadowed: Vec<_> = runtime.shadowed_def_params().collect();
            assert!(shadowed.is_empty(), "{n}³ at surface scale {}: cards overwrite def params: {shadowed:?}", scene.surface_scale);
        }
    }

    /// The fill is the engine's: its site rule on the engine's boxes, read
    /// by the domain.
    #[test]
    fn gpu_flip_dam_break_fill_matches_the_engine_boxes() {
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

    /// Each fused region of `def` as its members' node ids, `a + b`.
    fn fused_regions(def: &EffectGraphDef) -> Vec<String> {
        let report = manifold_node_engine::freeze::fusion_report(def, &registry());
        let name = |id: u32| def.nodes.iter().find(|n| n.id == id).map_or("?".to_string(), |n| n.node_id.as_str().to_string());
        report.regions.iter().map(|r| r.member_node_ids.iter().map(|&id| name(id)).collect::<Vec<_>>().join(" + ")).collect()
    }

    /// The frozen graphs at every lattice, before any GPU run of them: the
    /// running scene bare and meshed, and the render graph.
    #[test]
    fn gpu_flip_frozen_graphs_cover_every_dispatch() {
        for n in LATTICES {
            let scene = WaterScene::dam_break(n);
            for scene in [scene.with_surface(), scene.with_faces()] {
                let report = walked(&render_def(scene), true, &format!("frozen render {n}³"));
                assert!(report.checked > 7, "checked only {} nodes at {n}³", report.checked);
            }
        }
    }

    /// The step is one boundary node: nothing in the water fuses.
    #[test]
    fn gpu_flip_step_does_not_fuse() {
        assert_eq!(fused_regions(&water_def(WaterScene::dam_break(64))), Vec::<String>::new());
    }

    /// Nodes by id and wires sorted, so a hand edit's order does not count.
    fn canonical(def: &Value) -> Value {
        let mut def = def.clone();
        def["nodes"].as_array_mut().expect("nodes").sort_by_key(|node| node["id"].as_u64());
        def["wires"].as_array_mut().expect("wires").sort_by_key(|wire| {
            let end = |key: &str| wire[key].as_u64().expect("wire end");
            let port = |key: &str| wire[key].as_str().expect("wire port").to_string();
            (end("fromNode"), port("fromPort"), end("toNode"), port("toPort"))
        });
        def
    }

    /// The tick hands the state the step's faces, whatever the step count.
    #[test]
    fn gpu_flip_state_takes_the_last_steps_extended_faces() {
        for scene in [WaterScene::dam_break(64), WaterScene::dam_break(64).with_steps(2)] {
            let def = serde_json::to_value(water_def(scene)).expect("def");
            let name = |id: &Value| -> String {
                let nodes = def["nodes"].as_array().expect("nodes");
                nodes.iter().find(|n| n["id"] == *id).expect("wired node")["nodeId"].as_str().expect("name").to_string()
            };
            let wires = def["wires"].as_array().expect("wires");
            let into: Vec<_> = wires.iter().filter(|w| w["toPort"] == "faces_in").collect();
            assert_eq!(into.len(), 1, "one faces_in wire");
            assert_eq!(name(&into[0]["toNode"]), "state");
            assert_eq!(name(&into[0]["fromNode"]), STEP_NODE);
            assert_eq!(into[0]["fromPort"], "faces");
        }
    }

    /// Velocity extension follows the native configured CFL and covers the
    /// face grid's published valid layers.
    #[test]
    fn gpu_flip_band_uses_engine_cfl() {
        use manifold_nodes_water::primitives::gpu_flip_step::{ENGINE_CFL, band_layers};
        use manifold_nodes_water::liquid::conformance::FACE_GRID_GPU_FLIP_LAYERS;
        assert_eq!(FACE_GRID_GPU_FLIP_LAYERS, FACE_VALID_LAYERS);
        assert_eq!(band_layers(ENGINE_CFL), 12);
        assert!(band_layers(ENGINE_CFL) >= FACE_VALID_LAYERS);
    }

    /// Solver presets share the authored surface structure and the FLIP Fluids
    /// engine surface defaults: particle scale 3.0, Surface Detail 0.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn gpu_flip_surface_group_is_shared_with_all_water_presets() {
        let source = surface_group();
        let group = &source["group"];
        assert_eq!(group["nodes"].as_array().unwrap().len(), 33);
        let structure = |value: &Value| {
            let mut value = value.clone();
            for node in value["nodes"].as_array_mut().unwrap() {
                match node["nodeId"].as_str() {
                    Some("liquid_blobs") => node["params"]["particle_scale"]["value"] = json!(0.0),
                    Some("liquid_volume" | "liquid_mesh" | "liquid_bricks") => {
                        node["params"]["resolution_scale"]["value"] = json!(0);
                    }
                    _ => {}
                }
            }
            value
        };
        let registry = PrimitiveRegistry::with_cpu_flip_reference();
        for name in [SHIPPED_PRESET, "WaterDamBreakGpu", "WaterDamBreakMatter", "WaterStillPoolMatter", "WaterFloatingBoxMatter"] {
            let mut preset: Value = if name == "WaterDamBreakGpu" {
                let source = manifold_nodes::testkit::reference_fixtures::cpu_flip_preset_json("WaterDamBreakGpu.json");
                serde_json::from_str(source).unwrap()
            } else {
                let json = bundled_preset_json(&PresetTypeId::new(name)).unwrap();
                serde_json::from_str(&json).unwrap()
            };
            let surface_id = if name == SHIPPED_PRESET { "surface" } else { "liquid_surface" };
            let surface = manifold_nodes_water::liquid::conformance::json_node_mut(&mut preset, surface_id)
                .cloned().expect("nested surface");
            assert_eq!(surface["handle"], "Liquid Surface", "{name}: authored surface handle");
            assert_eq!(structure(&surface["group"]), structure(group), "{name}: authored surface drift");
            let particle_scale = 3.0_f32;
            let mut defaults = surface["params"].clone();
            assert_eq!(defaults["particle_scale"]["value"].as_f64().unwrap() as f32, particle_scale, "{name}: particle support");
            defaults["particle_scale"] = source["params"]["particle_scale"].clone();
            assert_eq!(defaults, source["params"], "{name}: other defaults drift");
            let blobs = surface["group"]["nodes"].as_array().unwrap().iter()
                .find(|n| n["nodeId"] == "liquid_blobs").unwrap();
            assert_eq!(blobs["params"]["particle_scale"]["value"].as_f64().unwrap() as f32, particle_scale);
            let surface_detail = 0.0;
            for node in surface["group"]["nodes"].as_array().unwrap().iter()
                .filter(|n| matches!(n["nodeId"].as_str(), Some("liquid_volume" | "liquid_mesh" | "liquid_bricks"))) {
                assert_eq!(node["params"]["resolution_scale"]["value"].as_f64().unwrap(), surface_detail + 1.0, "{name}: {}", node["nodeId"]);
            }
            for list in ["params", "bindings"] {
                let entries = preset["presetMetadata"][list].as_array().unwrap();
                let support: Vec<_> = entries.iter().filter(|entry| entry["id"] == "surface_particle_scale").collect();
                assert_eq!(support.len(), 1, "{name}: one particle support {list} entry");
                assert_eq!(support[0]["defaultValue"].as_f64().unwrap() as f32, particle_scale, "{name}: {list}");
                let detail: Vec<_> = entries.iter().filter(|entry| entry["id"] == "surface_detail").collect();
                assert_eq!(detail.len(), if list == "params" { 1 } else { 3 }, "{name}: surface detail {list} entries");
                for entry in detail {
                    assert_eq!(entry["defaultValue"].as_f64().unwrap(), surface_detail, "{name}: {list}");
                }
            }
            for param in ["stretch", "smoothing", "fill_pits", "smoothing_iterations"] {
                assert!(surface["params"][param]["value"].is_number(), "{name}: {param}");
            }
            let wires = group["wires"].as_array().unwrap();
            let nodes = group["nodes"].as_array().unwrap();
            let mesh = &nodes.iter().find(|n| n["nodeId"] == "liquid_mesh").unwrap()["id"];
            let clamp = &nodes.iter().find(|n| n["typeId"] == "node.clamp_liquid_to_solids").unwrap()["id"];
            for port in ["solid", "solid_nodes_x", "solid_nodes_y", "solid_nodes_z"] {
                let source = |id: &Value| {
                    let wire = wires.iter().find(|w| &w["toNode"] == id && w["toPort"] == port)
                        .unwrap_or_else(|| panic!("{name}: missing {port}"));
                    (wire["fromNode"].clone(), wire["fromPort"].clone())
                };
                assert_eq!(source(mesh), source(clamp), "{name}: mesh and clamp must share {port}");
            }
            let mut destinations = std::collections::HashSet::new();
            for wire in wires {
                assert!(destinations.insert((wire["toNode"].as_u64().unwrap(), wire["toPort"].as_str().unwrap())),
                    "{name}: duplicate input wire {wire}");
            }
            let preset_json = serde_json::to_string(&preset).expect("preset serializes");
            manifold_node_engine::runtime::PresetRuntime::from_json_str(&preset_json, &registry)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            let detail = preset["presetMetadata"]["bindings"].as_array().unwrap().iter()
                .filter(|b| b["id"] == "surface_detail").collect::<Vec<_>>();
            assert_eq!(detail.len(), 3, "{name}: detail reaches volume, mesh and bricks");
            assert!(detail.iter().all(|b| b["offset"] == 1.0));
        }
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn gpu_flip_surface_defaults_match_the_engine_on_both_dam_breaks() {
        let registry = PrimitiveRegistry::with_cpu_flip_reference();
        let native = manifold_nodes::testkit::reference_fixtures::cpu_flip_preset_json("WaterDamBreak.json");
        let native = manifold_node_engine::runtime::PresetRuntime::from_json_str(native, &registry).unwrap();
        let gpu = manifold_node_engine::runtime::PresetRuntime::from_def(
            render_def(WaterScene::dam_break(64)), &registry, None,
        ).unwrap();
        let param = |runtime: &manifold_node_engine::runtime::PresetRuntime, node: &str, name: &str| {
            let id = runtime.graph.instance_by_node_id(&manifold_core::NodeId::new(node)).unwrap();
            runtime.graph.get_node(id).unwrap().params.get(name).cloned().unwrap()
        };
        assert_eq!(param(&native, "fluid_surface", "surface_particle_scale"), ParamValue::Float(3.0));
        assert_eq!(param(&native, "fluid_surface", "surface_subdivisions"), ParamValue::Float(0.0));
        assert_eq!(param(&gpu, "liquid_blobs", "particle_scale"), param(&native, "fluid_surface", "surface_particle_scale"));
        for node in ["liquid_volume", "liquid_mesh", "liquid_bricks"] {
            assert_eq!(param(&gpu, node, "resolution_scale"), ParamValue::Float(1.0), "{node}");
        }
        assert_eq!(param(&gpu, "liquid_mesh_relaxation", "value"), param(&native, "fluid_surface", "surface_smoothing"));
        assert_eq!(param(&gpu, "liquid_smooth_mesh", "iterations"), param(&native, "fluid_surface", "surface_smoothing_iterations"));
        assert_eq!(param(&gpu, "liquid_blobs", "stretch"), ParamValue::Float(1.0));
        assert_eq!(param(&gpu, "liquid_blobs", "smoothing"), ParamValue::Float(0.0));
        assert_eq!(param(&gpu, "liquid_smoothing_passes", "value"), ParamValue::Float(0.0));
        assert!(gpu.shadowed_def_params().next().is_none(), "fresh defaults agree with the authored graph");
    }

    /// Native layer 0's water_material in flipEngineVSGPUFLIP.manifold:
    /// saved RGB (.37254903, .7294118, 1), roughness .5540391, absorption
    /// distance .1 and scattering .08826533 override its raw graph defaults.
    /// The other authored values and all omitted PBR defaults match that save.
    #[test]
    fn gpu_flip_water_material_matches_saved_native_reference() {
        let registry = PrimitiveRegistry::with_builtin();
        for def in [render_def(WaterScene::dam_break(64)), particle_view_def()] {
            let material = find_node(&def.nodes, "water_material").unwrap();
            assert_eq!(material.params.len(), 18, "the material retains its authored parameter surface");
            let runtime = manifold_node_engine::runtime::PresetRuntime::from_def(def, &registry, None).unwrap();
            let id = runtime.graph.instance_by_node_id(&manifold_core::NodeId::new("water_material")).unwrap();
            let material = runtime.graph.get_node(id).unwrap();
            for (name, expected) in [
                ("ambient", 0.0), ("color_r", 0.37254903), ("color_g", 0.7294118), ("color_b", 1.0),
                ("metallic", 0.0), ("roughness", 0.5540391), ("ior", 1.333), ("transmission", 1.0),
                ("volume_attenuation_color_r", 0.35), ("volume_attenuation_color_g", 0.72), ("volume_attenuation_color_b", 0.8),
                ("volume_attenuation_distance", 0.1), ("volume_geometry", 1.0), ("volume_thickness", 0.09),
                ("volume_scattering_color_r", 0.68), ("volume_scattering_color_g", 0.86), ("volume_scattering_color_b", 0.92),
                ("volume_scattering_density", 0.08826533),
            ] {
                assert_eq!(material.params.get(name), Some(&ParamValue::Float(expected)), "{name}");
            }
            assert!(runtime.shadowed_def_params().next().is_none(), "material defaults agree with the authored graph");
        }
        let preset = shipped_preset();
        for (card, target, expected) in [
            ("water_attenuation", "volume_attenuation_distance", 0.1_f32),
            ("water_scattering", "volume_scattering_density", 0.08826533_f32),
        ] {
            for list in ["params", "bindings"] {
                let entries: Vec<_> = preset["presetMetadata"][list].as_array().unwrap().iter()
                    .filter(|entry| entry["id"] == card).collect();
                assert_eq!(entries.len(), 1, "{card}: {list}");
                assert_eq!(entries[0]["defaultValue"].as_f64().unwrap() as f32, expected);
                if list == "bindings" {
                    assert_eq!(entries[0]["target"]["nodeId"], "water_material");
                    assert_eq!(entries[0]["target"]["param"], target);
                }
            }
        }
    }

    #[test]
    fn gpu_flip_sheet_fill_rate_card_binding_and_wire_round_trip() {
        use manifold_core::NodeId;
        use manifold_core::params::{Param, ParamManifest};
        use manifold_node_engine::runtime::PresetRuntime;

        for name in [SHIPPED_PRESET, PARTICLE_VIEW_PRESET] {
            let json = bundled_preset_json(&PresetTypeId::new(name)).unwrap();
            let def: EffectGraphDef = serde_json::from_str(&json).unwrap();
            let saved = serde_json::to_string(&def).unwrap();
            let def: EffectGraphDef = serde_json::from_str(&saved).unwrap();
            let value = serde_json::to_value(&def).unwrap();
            let card = value["presetMetadata"]["params"].as_array().unwrap().iter()
                .find(|p| p["id"] == "sheet_fill_rate").expect("sheet card");
            assert_eq!(card["name"], "Sheet Fill Rate");
            assert_eq!(card["min"], 0.0);
            assert_eq!(card["max"], 1.0);
            assert_eq!(card["defaultValue"], 0.0);
            assert_eq!(card["wholeNumbers"], false);
            let bindings: Vec<_> = value["presetMetadata"]["bindings"].as_array().unwrap().iter()
                .filter(|p| p["id"] == "sheet_fill_rate").collect();
            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0]["target"], json!({"kind":"node", "nodeId":"domain", "param":"sheet_fill_rate"}));
            assert_eq!(bindings[0]["defaultValue"], 0.0);
            let flat = manifold_core::flatten::flatten_groups(&def).unwrap();
            let domain = flat.nodes.iter().find(|n| n.node_id.as_str() == "domain").unwrap().id;
            let step = flat.nodes.iter().find(|n| n.node_id.as_str() == STEP_NODE).unwrap().id;
            assert!(flat.wires.iter().any(|w| w.from_node == domain && w.from_port == "sheet_fill_rate"
                && w.to_node == step && w.to_port == "sheet_fill_rate"));
            let mut params = ParamManifest::from_params(def.preset_metadata.as_ref().unwrap().params.iter().cloned().map(Param::bundled).collect());
            let mut runtime = PresetRuntime::from_def(def, &PrimitiveRegistry::with_builtin(), None).unwrap();
            for rate in [0.0, 0.375, 1.0, 0.0] {
                let param = params.get_mut("sheet_fill_rate").unwrap();
                param.value = rate;
                param.base = rate;
                runtime.apply_param_values(&params);
                let id = runtime.graph.instance_by_node_id(&NodeId::new("domain")).unwrap();
                let node = runtime.graph.get_node(id).unwrap();
                assert_eq!(node.params.get("sheet_fill_rate"), Some(&ParamValue::Float(rate)));
                let geometry = gpu_flip_geometry(|key, default| node.params.get(key)
                    .map(manifold_node_engine::snapshot::param_default_to_f32).unwrap_or(default), None, None).unwrap();
                assert_eq!(geometry.sheet_fill_rate_for_test(), rate);
            }
        }
    }

    /// Insertion and presets share the complete family, including after a
    /// serialization round trip. Comparison uses node identity, not local ids.
    #[test]
    fn water_family_builder_parity() {
        use std::collections::{BTreeMap, BTreeSet};
        fn facts(nodes: &[EffectGraphNode], wires: &[EffectGraphWire], particle_view: bool) -> (Value, Value) {
            let omitted = |name: &str| name == "fluid_input" || particle_view &&
                matches!(name, "surface" | "liquid_particle_mesh" | "liquid_particle_copies");
            let names: BTreeMap<_, _> = nodes.iter().map(|n| (n.id, n.node_id.as_str())).collect();
            let nodes: BTreeMap<_, _> = nodes.iter().filter(|n| !omitted(n.node_id.as_str())).map(|n| {
                let mut value = serde_json::to_value(n).unwrap();
                value.as_object_mut().unwrap().remove("id");
                (n.node_id.as_str(), value)
            }).collect();
            let wires: BTreeSet<_> = wires.iter().filter_map(|w| {
                let (from, to) = (names[&w.from_node], names[&w.to_node]);
                if omitted(from) || omitted(to) || particle_view && to == "water_object" &&
                    ["vertices", "indices", "instances", "instance_count"].contains(&w.to_port.as_str()) {
                    None
                } else { Some((from, w.from_port.as_str(), to, w.to_port.as_str())) }
            }).collect();
            (serde_json::to_value(nodes).unwrap(), serde_json::to_value(wires).unwrap())
        }
        let mut body = gpu_flip_liquid_body();
        let water = body.nodes.iter_mut().find(|node| node.node_id.as_str() == "water_object").unwrap();
        assert_eq!(water.handle.as_deref(), Some(""), "Add Fluid owns the bare fluid handle");
        water.handle = Some("Water".into());
        for (id, handle) in [("domain", "Simulation"), ("initial_column", "Initial Volume")] {
            let node = body.nodes.iter_mut().find(|node| node.node_id.as_str() == id).unwrap();
            assert_eq!(node.handle.as_deref(), Some(handle));
            // These handles belong to insertion; the preset recipe leaves them unnamed.
            node.handle = None;
        }
        for (def, particles, obstacle) in [
            (render_def(WaterScene::dam_break(64)), false, true),
            (particle_view_def(), true, true),
            (render_def(WaterScene { obstacle: false, ..WaterScene::dam_break(64) }), false, false),
        ] {
            let saved = serde_json::to_string(&def).unwrap();
            let def: EffectGraphDef = serde_json::from_str(&saved).unwrap();
            let family = find_node(&def.nodes, "water_family").unwrap();
            let group = family.group.as_ref().expect("ordinary family group");
            assert_eq!(group.interface.outputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), family_outputs());
            assert_eq!(group.interface.inputs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
                if obstacle { vec!["role_0"] } else { vec![] });
            assert_eq!(facts(&body.nodes, &body.wires, particles), facts(&group.nodes, &group.wires, particles));
            assert_eq!(group.nodes.iter().filter(|n| n.type_id == manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID).count(), 1);
            assert_eq!(group.nodes.iter().filter(|n| n.type_id == "node.whitewater_obstacle_source").count(), 1);
            assert!(!group.nodes.iter().any(|n| n.node_id.as_str().starts_with("dust_")));
            let boundary = group.nodes.iter().find(|n| n.node_id.as_str() == LIQUID_BODY_OUTPUT).unwrap().id;
            for (port, object) in family_outputs().into_iter().zip(["water_object", "foam_object", "spray_object", "bubble_object"]) {
                let id = group.nodes.iter().find(|n| n.node_id.as_str() == object).unwrap().id;
                assert!(group.wires.iter().any(|w| w.from_node == id && w.from_port == "object" && w.to_node == boundary && w.to_port == port));
                let physical: Vec<_> = def.wires.iter().filter(|w| w.from_node == family.id && w.from_port == port).collect();
                assert_eq!(physical.len(), 1);
                assert!(def.nodes.iter().any(|n| n.id == physical[0].to_node && n.type_id == "node.render_scene"));
            }
            let stage = group.nodes.iter().find(|n| n.node_id.as_str() == "whitewater").unwrap();
            assert_eq!(serde_json::to_value(&stage.params).unwrap()["amount"]["value"], 1.0);
            let flat = manifold_core::flatten::flatten_groups(&def).expect("nested surface flattens");
            for binding in &def.preset_metadata.as_ref().unwrap().bindings {
                let value = serde_json::to_value(binding).unwrap();
                if value["target"]["kind"] == "node" {
                    assert!(flat.nodes.iter().any(|n| n.node_id.as_str() == value["target"]["nodeId"].as_str().unwrap()),
                        "binding target absent: {value}");
                }
            }
            built(&def);
        }
    }

    #[test]
    fn gpu_flip_shipped_scene_rows_keep_authored_names() {
        use manifold_nodes_scene::node_graph::scene_vm::{SceneObjectVm, SceneVm};
        let def = manifold_nodes::bundled_presets::bundled_preset_def(&PresetTypeId::new(SHIPPED_PRESET)).unwrap();
        let vm = SceneVm::from_def(def.as_ref()).expect("shipped scene resolves");
        let names: Vec<_> = vm.objects.iter().filter_map(|object| match object {
            SceneObjectVm::Known(row) => Some(row.name.as_str()),
            _ => None,
        }).collect();
        for name in ["Water", "Foam", "Spray", "Bubbles", "Obstacle"] {
            assert_eq!(names.iter().filter(|&&found| found == name).count(), 1, "{name}: {names:?}");
        }
        assert!(!names.contains(&""), "scene rows must have authored names");
    }

    /// The shipped `WaterDamBreakGpuFlip.json` is the builder's Dam Break at 64,
    /// so the tests that build it run what ships. `UPDATE_GPU_FLIP_PRESET=1`
    /// rewrites it from the builder.
    #[test]
    fn gpu_flip_shipped_preset_is_the_builders_dam_break() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("assets/generator-presets/{SHIPPED_PRESET}.json"));
        let built = serde_json::to_value(render_def(WaterScene::dam_break(64))).expect("serialise");
        if std::env::var("UPDATE_GPU_FLIP_PRESET").is_ok() {
            let mut json = serde_json::to_string_pretty(&built).expect("serialise");
            json.push('\n');
            std::fs::write(&path, json).expect("write the shipped preset");
        }
        let shipped: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("the shipped preset reads")).expect("parses");
        assert!(canonical(&shipped) == canonical(&built), "{SHIPPED_PRESET}.json differs from the builder's Dam Break; rerun with UPDATE_GPU_FLIP_PRESET=1");
    }

    /// The shipped `WaterDamBreakParticles.json` is the builder's Particle
    /// View of the shipped Dam Break, and its cards bind the graph it ships.
    /// `UPDATE_GPU_FLIP_PRESET=1` rewrites it; regenerate the Dam Break
    /// first, since this view is built from it.
    #[test]
    fn gpu_flip_particle_view_is_built_from_the_dam_break() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("assets/generator-presets/{PARTICLE_VIEW_PRESET}.json"));
        let existing: EffectGraphDef = serde_json::from_str(&std::fs::read_to_string(&path).expect("Particle View reads")).expect("preset parses");
        assert_preset_root(&existing.nodes);
        let def = particle_view_def();
        let built = serde_json::to_value(&def).expect("serialise");
        if std::env::var("UPDATE_GPU_FLIP_PRESET").is_ok() {
            let mut json = serde_json::to_string_pretty(&built).expect("serialise");
            json.push('\n');
            std::fs::write(&path, json).expect("write the Particle View");
        }
        let shipped: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("the Particle View reads")).expect("parses");
        assert!(canonical(&shipped) == canonical(&built), "{PARTICLE_VIEW_PRESET}.json differs from the builder's Particle View; rerun with UPDATE_GPU_FLIP_PRESET=1");
        let runtime = manifold_node_engine::runtime::PresetRuntime::from_def(def, &PrimitiveRegistry::with_builtin(), None).expect("the Particle View builds");
        let shadowed: Vec<_> = runtime.shadowed_def_params().collect();
        assert!(shadowed.is_empty(), "the Particle View's cards overwrite def params: {shadowed:?}");
    }

    /// Every per-step duration input in every graph the builder ships is fed
    /// by its domain's accepted interval. Left on its param, one holds 1/60 s
    /// at every Sim Rate: at 30 Hz whitewater emits, ages and moves at half
    /// rate, and export poses moving solids at the wrong time.
    #[test]
    fn gpu_flip_builder_graphs_feed_every_interval_input() {
        let registry = PrimitiveRegistry::with_builtin();
        for (type_id, port) in INTERVAL_DURATION_INPUTS {
            let node = registry.construct(type_id).unwrap_or_else(|| panic!("{type_id} is not registered"));
            assert!(node.inputs().iter().any(|input| input.name == port), "{type_id} has no {port} input");
        }
        let bundled = |name: &'static str| -> EffectGraphDef {
            let json = bundled_preset_json(&PresetTypeId::new(name)).unwrap_or_else(|| panic!("{name} is bundled"));
            serde_json::from_str(&json).expect("the preset parses")
        };
        for (name, def) in [
            (SHIPPED_PRESET, bundled(SHIPPED_PRESET)),
            (PARTICLE_VIEW_PRESET, bundled(PARTICLE_VIEW_PRESET)),
            ("Add Fluid's liquid body", gpu_flip_liquid_body()),
        ] {
            let flat = manifold_core::flatten::flatten_groups(&def).expect("flattens");
            let fed_by_domain = |node: u32, port: &str| {
                let feeds: Vec<_> = flat.wires.iter().filter(|w| w.to_node == node && w.to_port == port).collect();
                feeds.len() == 1
                    && feeds[0].from_port == "interval_duration"
                    && flat.nodes.iter().any(|n| n.id == feeds[0].from_node && manifold_core::liquid_domain::is_liquid_domain(&n.type_id))
            };
            let mut checked = 0;
            for node in &flat.nodes {
                for (type_id, port) in INTERVAL_DURATION_INPUTS {
                    if node.type_id == type_id {
                        assert!(fed_by_domain(node.id, port), "{name}: {}.{port} is not fed by its domain's interval_duration", node.node_id.as_str());
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
            ("GPU FLIP builder", render_def(WaterScene::dam_break(16).with_faces())),
            ("Add Fluid's liquid body", gpu_flip_liquid_body()),
        ];
        for name in [SHIPPED_PRESET, PARTICLE_VIEW_PRESET, "WaterDamBreakMatter", "WaterStillPoolMatter", "WaterFloatingBoxMatter"] {
            let json = bundled_preset_json(&PresetTypeId::new(name)).unwrap_or_else(|| panic!("{name} is bundled"));
            graphs.push((name, serde_json::from_str(&json).expect("the preset parses")));
        }
        for (name, def) in graphs {
            let flat = manifold_core::flatten::flatten_groups(&def).expect("flattens");
            let mut checked = 0;
            for state in flat.nodes.iter().filter(|node| matches!(node.type_id.as_str(), "node.liquid_state" | "node.matter_state")) {
                let feeds: Vec<_> = flat.wires.iter().filter(|w| w.to_node == state.id && w.to_port == "dropped_seconds").collect();
                assert_eq!(feeds.len(), 1, "{name}: {} needs one dropped_seconds wire", state.node_id.as_str());
                assert_eq!(feeds[0].from_port, "dropped_seconds");
                let tick_source = flat.wires.iter().find(|w| w.to_node == state.id && w.to_port == "ticks").expect("state has a clock");
                assert_eq!(feeds[0].from_node, tick_source.from_node, "{name}: dropped time belongs to the state's clock");
                assert!(flat.nodes.iter().any(|node| node.id == feeds[0].from_node && manifold_core::liquid_domain::is_liquid_domain(&node.type_id)));
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
            ("GPU FLIP builder", render_def(WaterScene::dam_break(16).with_faces())),
            ("Add Fluid's liquid body", gpu_flip_liquid_body()),
        ];
        for name in [SHIPPED_PRESET, PARTICLE_VIEW_PRESET] {
            let json = bundled_preset_json(&PresetTypeId::new(name)).unwrap_or_else(|| panic!("{name} is bundled"));
            graphs.push((name, serde_json::from_str(&json).expect("the preset parses")));
        }
        for (name, def) in graphs {
            let flat = manifold_core::flatten::flatten_groups(&def).expect("flattens");
            let mut checked = 0;
            for frame in flat.nodes.iter().filter(|node| node.type_id == "node.liquid_frame") {
                let clock = flat.wires.iter().find(|w| w.to_node == frame.id && w.to_port == "epoch").expect("the frame has a clock");
                for port in ["display_cursor", "dropped_seconds"] {
                    let feeds: Vec<_> = flat.wires.iter().filter(|w| w.to_node == frame.id && w.to_port == port).collect();
                    assert_eq!(feeds.len(), 1, "{name}: one {port} wire");
                    assert_eq!((feeds[0].from_node, feeds[0].from_port.as_str()), (clock.from_node, port), "{name}: {port} from the frame's clock");
                }
                checked += 1;
            }
            assert!(checked > 0, "{name} has no liquid frame");
        }
    }

    #[test]
    fn gpu_flip_presets_feed_retired_speed_from_the_particles_state() {
        for def in [water_def(WaterScene::dam_break(16)), gpu_flip_liquid_body()] {
            let flat = manifold_core::flatten::flatten_groups(&def).expect("flattens");
            let mut checked = 0;
            for step in flat.nodes.iter().filter(|node| node.type_id == "node.gpu_flip_step") {
                let particles = flat.wires.iter().find(|wire| wire.to_node == step.id && wire.to_port == "particles").expect("step has particles");
                let speed: Vec<_> = flat.wires.iter().filter(|wire| wire.to_node == step.id && wire.to_port == "retired_max_speed").collect();
                assert_eq!(speed.len(), 1);
                assert_eq!(speed[0].from_node, particles.from_node);
                assert_eq!(speed[0].from_port, "retired_max_speed");
                assert!(flat.nodes.iter().any(|node| node.id == speed[0].from_node && node.type_id == "node.liquid_state"));
                checked += 1;
            }
            assert!(checked > 0);
        }
    }

    /// BUG-215v: whitewater belongs to the liquid region, consumes the
    /// solver phi, and every persistent or rendered result closes through
    /// the boundary. This checks real preset compilation without a GPU.
    #[test]
    fn whitewater_per_tick_preset_closes_the_liquid_region() {
        let def = render_def(WaterScene::dam_break(16).with_faces());
        let (graph, plan) = built(&def);
        let def = manifold_core::flatten::flatten_groups(&def).expect("grouped region topology");
        let whitewater_node = graph.nodes().find(|n| n.node_id.as_str() == "whitewater").unwrap();
        let whitewater = whitewater_node.id;
        let region = plan.substep_regions().iter().find(|r|
            r.steps.iter().any(|&i| plan.steps()[i].node == whitewater)).expect("whitewater is per tick");
        let wire = |from, from_port: &str, to, to_port: &str| def.wires.iter().any(|w|
            w.from_node == from && w.from_port == from_port && w.to_node == to && w.to_port == to_port);
        let boundary_name = &graph.get_node(region.boundary).unwrap().node_id;
        let boundary = def.nodes.iter().find(|n| &n.node_id == boundary_name).unwrap().id;
        let whitewater = manifold_node_engine::exec::effect_node::NodeInstanceId(def.nodes.iter()
            .find(|n| n.node_id.as_str() == "whitewater").unwrap().id);
        let distance = def.wires.iter().find(|w| w.to_node == whitewater.0 && w.to_port == "distance").unwrap();
        assert_eq!(distance.from_port, "distance");
        assert!(def.nodes.iter().any(|n| n.id == distance.from_node && n.type_id == "node.gpu_flip_step"));
        for port in ["substep_schedule", "substep_u", "substep_v", "substep_w", "substep_count"] {
            assert!(wire(distance.from_node, port, whitewater.0, port), "missing accepted {port}");
        }
        for port in ["forces", "impulses", "field_nodes_x", "field_nodes_y", "field_nodes_z", "field_spacing", "force_lattices", "first_tick", "regions", "region_count", "shapes", "atlas"] {
            let water=def.wires.iter().find(|w|w.to_node==distance.from_node && w.to_port==port).expect(port);
            assert!(wire(water.from_node, &water.from_port, whitewater.0, port), "{port} must share the liquid source");
        }
        for (output, capture, held) in [
            ("pool_out", "whitewater_pool_in", "whitewater_pool"),
            ("state_out", "whitewater_state_in", "whitewater_state"),
            ("counts_out", "whitewater_counts_in", "whitewater_counts"),
            ("foam_particles", "foam_particles_in", "foam_particles"),
            ("bubble_particles", "bubble_particles_in", "bubble_particles"),
            ("spray_particles", "spray_particles_in", "spray_particles"),
            ("dust_particles", "dust_particles_in", "dust_particles"),
        ] {
            assert!(wire(whitewater.0, output, boundary, capture), "missing {capture}");
            assert!(!def.wires.iter().any(|w| w.from_node == whitewater.0 && w.from_port == output && w.to_node != boundary), "{held} escapes the boundary");
        }
        for frozen in [false, true] {
            walked(&def, frozen, "16³ per-tick whitewater");
        }
    }

    /// The shipped preset loads, saves and reloads unchanged, the Whitewater
    /// group's params and its cards with it.
    #[test]
    fn gpu_flip_preset_round_trips_with_its_whitewater() {
        let shipped = shipped_preset();
        let def: EffectGraphDef = serde_json::from_value(shipped.clone()).expect("the preset loads");
        let loaded = serde_json::to_value(&def).expect("serialise");
        assert!(canonical(&loaded) == canonical(&shipped), "loading {SHIPPED_PRESET}.json dropped or changed a field");
        // A save prints each f32 at its shortest; the reload is the same f32s.
        let saved = serde_json::to_string_pretty(&def).expect("the preset saves");
        let again: EffectGraphDef = serde_json::from_str(&saved).expect("the saved preset reloads");
        let reloaded = serde_json::to_value(&again).expect("serialise");
        assert!(canonical(&reloaded) == canonical(&loaded), "a save and reload changed {SHIPPED_PRESET}.json");
        let reloaded_def: EffectGraphDef = serde_json::from_value(reloaded.clone()).expect("reloaded preset");
        let group = serde_json::to_value(find_node(&reloaded_def.nodes, "whitewater").expect("Whitewater step")).expect("whitewater");
        for param in ["capacity", "wavecrest_emission", "min_energy", "max_energy"] {
            assert!(group["params"][param]["value"].is_number(), "the group lost {param}");
        }
        let cards = reloaded["presetMetadata"]["bindings"].as_array().expect("bindings");
        for card in ["whitewater_capacity", "parent_visible", "bubble_density"] {
            assert!(cards.iter().any(|c| c["id"] == card), "the preset lost the {card} card");
        }
    }

use manifold_nodes_water::primitives::{gpu_flip_domain::gpu_flip_geometry, gpu_flip_step::FACE_VALID_LAYERS};
use manifold_nodes::bundled_presets::bundled_preset_json;
use manifold_nodes_water::liquid::clock::INTERVAL_DURATION_INPUTS;

fn shipped_preset() -> Value {
    let json = manifold_nodes::bundled_presets::bundled_preset_json(&PresetTypeId::new("WaterDamBreakGpuFlip")).expect("the GPU FLIP preset is bundled");
    serde_json::from_str(&json).expect("the GPU FLIP preset parses")
}
