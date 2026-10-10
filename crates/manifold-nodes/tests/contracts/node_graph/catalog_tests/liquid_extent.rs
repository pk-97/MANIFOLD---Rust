    use manifold_nodes_water::liquid::extent::*;
use manifold_node_engine::exec::extent::*;
use manifold_nodes_water::liquid::lattice::LiquidLattice;
use manifold_core::effect_graph_def::EffectGraphDef;
    use manifold_nodes::bundled_presets::{bundled_preset_def, bundled_preset_type_ids};
    use manifold_node_engine::scene::fluid_domain::domain_layout;


    use manifold_core::preset_def::PresetKind;



    #[test]
    fn liquid_mesh_grid_native_dispatch_extents_are_covered() {
        for (size, resolution) in [(4.0, 64), (1.0, 8), (6.0, 32)] {
            let layout = domain_layout(None, size, resolution).unwrap();
            let mesh = LiquidLattice::from_layout(&layout).surface();
            let solid_nodes = mesh.nodes().map(u64::from);
            let solid_count = solid_nodes.into_iter().product::<u64>();
            assert_eq!(mesh.solid_bytes(), solid_count * 4);
            for scale in [1u64, 2, 3, 4] {
                let nodes = solid_nodes.map(|n| (n - 1) * scale + 1);
                let count = nodes.into_iter().product::<u64>();
                let cells = nodes.map(|n| n - 1);
                let cell_count = cells.into_iter().product::<u64>();
                // Node dispatches (splat, smoothing, offsets, redistance,
                // clamp) use count; MC dispatches use cell_count. The last
                // MC cell's +1 corner is exactly the last allocated node.
                let last_corner = cells[0] + nodes[0] * (cells[1] + nodes[1] * cells[2]);
                assert_eq!(last_corner, count - 1);
                assert!(count * 4 <= u64::from(u32::MAX));
                assert!(cell_count * 15 <= u64::from(u32::MAX));
                assert!(count <= count.div_ceil(256) * 256);
                assert!(count.div_ceil(256) * 256 - count < 256);
            }
        }
        // Walk actual producers and consumers with extent.rs, including
        // provided solids, frame rings, sparse schedules and mesh outputs.
        let (_, def) = liquid_presets().into_iter().find(|(id, _)| id == "WaterDamBreakGpuFlip").unwrap();
        for resolution in [8, 32, 64] {
            let report = check_preset_extents(def.as_ref(), resolution).unwrap();
            assert!(report.checked > 20);
        }
    }

    /// Every bundled generator preset holding a liquid domain.
    fn liquid_presets() -> Vec<(String, std::sync::Arc<EffectGraphDef>)> {
        let holds_liquid = |def: &EffectGraphDef| {
            let flat = manifold_core::flatten::flatten_groups(def).expect("flattens");
            flat.nodes.iter().any(|node| is_liquid_domain(&node.type_id))
        };
        bundled_preset_type_ids(PresetKind::Generator)
            .filter_map(|id| bundled_preset_def(&id).filter(|def| holds_liquid(def.as_ref())).map(|def| (id.to_string(), def)))
            .collect()
    }

    fn particle_blend_preset() -> EffectGraphDef {
        serde_json::from_str(include_str!("../../../../assets/generator-presets/WaterDamBreakParticles.json")).unwrap()
    }

    #[test]
    fn particle_blend_presets_have_complete_extent_rules() {
        for text in [
            include_str!("../../../../assets/generator-presets/WaterDamBreakGpuFlip.json"),
            include_str!("../../../../assets/generator-presets/WaterDamBreakParticles.json"),
        ] {
            let def = serde_json::from_str(text).unwrap();
            for resolution in [16, 32] {
                check_preset_extents(&def, resolution).unwrap();
            }
        }
    }

    #[test]
    fn particle_blend_counts_and_solid_storage_are_checked() {
        use manifold_core::effect_graph_def::SerializedParamValue;
        for (type_id, port, value) in [
            ("node.interpolate_particle_frames", "count_a", 16_777_216.0),
            ("node.interpolate_particle_frames", "count_b", 16_777_216.0),
            ("node.push_out_of_solid", "nodes_x", 4096.0),
        ] {
            // Corrupt the compiled topology, including nodes inside Water.
            let mut def = manifold_core::flatten::flatten_groups(&particle_blend_preset()).expect("preset flattens");
            let node = def.nodes.iter_mut().find(|node| node.type_id == type_id).unwrap();
            let id = node.id;
            node.params.insert(port.into(), SerializedParamValue::Float { value });
            def.wires.retain(|wire| !(wire.to_node == id && wire.to_port == port));
            match check_preset_extents(&def, 16) {
                Err(ExtentError::Uncovered { node, .. }) => assert!(node.contains(type_id), "{node}"),
                other => panic!("{type_id}.{port}: expected uncovered storage, got {other:?}"),
            }
        }
    }

    #[test]
    fn particle_blend_mix_refuses_unequal_capacities() {
        let mut preset = LiquidPreset::build(&particle_blend_preset()).unwrap();
        let rules: Vec<_> = EXTENT_RULES.iter().map(|rule| {
            if rule.type_id == "node.liquid_frame" {
                ExtentRule { type_id: rule.type_id, check: manifold_node_engine::exec::extent::testkit::malformed_frame }
            } else { *rule }
        }).collect();
        match manifold_nodes_water::liquid::extent::testkit::check_with_rules(&mut preset, &rules) {
            Err(ExtentError::Refused { node, reason }) => {
                assert!(node.contains("node.mix_arrays"), "{node}");
                assert!(reason.contains("input capacities must match"), "{reason}");
            }
            other => panic!("expected unequal arrays to be refused, got {other:?}"),
        }
    }

    /// Section 3.7 (Safety rails) rule 1, I9: every liquid preset at every
    /// resolution its domain admits either refuses by name before any GPU
    /// work, or every buffer covers every dispatch.
    #[test]
    fn narrow_band_preset_small_extent_checked() {
        let (id, def) = liquid_presets().into_iter().find(|(id, _)| id.contains("WaterDamBreakGpuFlip")).expect("GPU FLIP preset");
        let mut preset = LiquidPreset::build(def.as_ref()).unwrap_or_else(|error| panic!("{id}: {error}"));
        for resolution in [8, 16] {
            let report = preset.check(resolution).unwrap_or_else(|error| panic!("{id} at {resolution}: {error}"));
            assert!(report.checked > 0);
        }
    }

    #[test]
    fn liquid_blob_bounds_reject_wrong_extent_before_gpu_work() {
        let (_, def) = liquid_presets().into_iter().find(|(id, _)| id == "WaterDamBreakGpuFlip").expect("preset");
        for consumer in ["node.particle_volume", "node.lattice_bricks"] {
            let mut flat = manifold_core::flatten::flatten_groups(def.as_ref()).expect("flattens");
            let id = flat.nodes.iter().find(|n| n.type_id == consumer).expect("consumer").id;
            // Both ports are Array<f32>, so the graph type check accepts this
            // deliberately wrong wire. The extent contract must reject it.
            let solid = flat.wires.iter().find(|w| w.to_node == id && w.to_port == "solid").expect("solid wire").clone();
            let bound = flat.wires.iter_mut().find(|w| w.to_node == id && w.to_port == "bounds").expect("bounds wire");
            bound.from_node = solid.from_node;
            bound.from_port = solid.from_port;
            match check_preset_extents(&flat, 8) {
                Err(ExtentError::Uncovered { detail, .. }) => assert!(detail.contains("bounds must contain exactly two"), "{consumer}: {detail}"),
                other => panic!("{consumer}: expected a malformed bounds refusal, got {other:?}"),
            }
        }
    }

    #[test]
    fn liquid_mesh_contact_rejects_short_solid_before_gpu_work() {
        let (_, def) = liquid_presets().into_iter().find(|(id, _)| id == "WaterDamBreakGpuFlip").expect("preset");
        let mut flat = manifold_core::flatten::flatten_groups(def.as_ref()).expect("flattens");
        let mesh = flat.nodes.iter().find(|n| n.type_id == "node.volume_surface_mesh").expect("mesh").id;
        let bounds = flat.nodes.iter().find(|n| n.type_id == "node.blob_bounds").expect("two-float source").id;
        let solid = flat.wires.iter_mut().find(|w| w.to_node == mesh && w.to_port == "solid").expect("solid wire");
        solid.from_node = bounds;
        solid.from_port = "bounds".into();
        match check_preset_extents(&flat, 8) {
            Err(ExtentError::Uncovered { node, detail }) => {
                // The early solid refusal leaves owned outputs unsized, so
                // the walk may report that before its final coverage pass.
                assert!(node.contains("volume_surface_mesh"), "{node}: {detail}");
            }
            other => panic!("expected a short solid lattice refusal, got {other:?}"),
        }
    }

    #[test]
    fn liquid_presets_all_extent_checked() {
        let presets = liquid_presets();
        assert!(presets.len() >= 7, "liquid presets: {:?}", presets.iter().map(|(id, _)| id).collect::<Vec<_>>());
        let mut refusals: AHashMap<String, (u32, String)> = AHashMap::default();
        for (id, def) in &presets {
            let mut preset = LiquidPreset::build(def.as_ref()).unwrap_or_else(|error| panic!("{id}: {error}"));
            let resolutions = preset.resolutions();
            assert_eq!(resolutions, 8..=512, "{id}");
            let mut largest = None;
            for resolution in resolutions {
                match preset.check(resolution) {
                    Ok(report) => {
                        assert!(report.checked > 0, "{id} at {resolution}");
                        largest = Some((resolution, report));
                    }
                    Err(ExtentError::Refused { node, reason }) => {
                        refusals.entry(id.clone()).or_insert((resolution, format!("{node}: {reason}")));
                    }
                    Err(error) => panic!("{id} at resolution {resolution}: {error}"),
                }
            }
            let (resolution, report) = largest.unwrap_or_else(|| panic!("{id}: no resolution runs"));
            println!(
                "{id}: runs to resolution {resolution} ({} nodes checked, {:.2} GB at the top)",
                report.checked,
                report.scene_bytes as f64 / 1e9
            );
        }
        let mut refusals: Vec<_> = refusals.into_iter().collect();
        refusals.sort();
        for (id, (resolution, reason)) in &refusals {
            println!("{id}: first refused at resolution {resolution}: {reason}");
            assert!(reason.contains("Resolution") || reason.contains("Grid Budget"), "{id}: {reason}");
        }
    }

use ahash::AHashMap;
use manifold_core::liquid_domain::is_liquid_domain;

    /// A graph with a GPU atom no rule knows fails by name.
    #[test]
    fn an_atom_without_a_rule_fails_by_name() {
        let (_, def) = liquid_presets().into_iter().find(|(id, _)| id == "WaterDamBreakMatter").expect("preset");
        let mut preset = LiquidPreset::build(def.as_ref()).expect("builds");
        let rules: Vec<ExtentRule> =
            EXTENT_RULES.iter().filter(|rule| rule.type_id != "node.matter_to_grid").copied().collect();
        match manifold_nodes_water::liquid::extent::testkit::check_with_rules(&mut preset, &rules) {
            Err(ExtentError::NoRule { type_id, .. }) => assert_eq!(type_id, "node.matter_to_grid"),
            other => panic!("expected a missing rule, got {other:?}"),
        }
    }

    /// A mesher told its lattice is larger than the level set it reads is
    /// caught before the GPU: Dam Break Matter with count_surface_triangles
    /// unwired from the volume's lattice and set to 4096 nodes per axis.
    #[test]
    fn a_lattice_past_its_storage_is_caught() {
        use manifold_core::effect_graph_def::SerializedParamValue;
        let (_, def) = liquid_presets().into_iter().find(|(id, _)| id == "WaterDamBreakMatter").expect("preset");
        let mut flat = manifold_core::flatten::flatten_groups(def.as_ref()).expect("flattens");
        let counter = flat.nodes.iter().find(|node| node.type_id == "node.count_surface_triangles").map(|node| node.id).expect("a counter");
        let lattice = ["nodes_x", "nodes_y", "nodes_z"];
        // Exercise the dense level-set bound specifically. With a sparse
        // schedule wired, its smaller brick bound correctly refuses first.
        flat.wires.retain(|wire| !(wire.to_node == counter
            && (lattice.contains(&wire.to_port.as_str()) || wire.to_port == "bricks")));
        let node = flat.nodes.iter_mut().find(|node| node.id == counter).expect("counter");
        for port in lattice {
            node.params.insert(port.into(), SerializedParamValue::Float { value: 4096.0 });
        }
        match check_preset_extents(&flat, 64) {
            Err(ExtentError::Uncovered { node, detail }) => {
                assert!(node.contains("count_surface_triangles") && detail.starts_with("levelset holds"), "{node}: {detail}");
            }
            other => panic!("expected the level set to be short, got {other:?}"),
        }
    }

    #[test]
    fn ocean_cliff_authored_extent_checked() {
        let (_, def) = liquid_presets().into_iter().find(|(id, _)| id == "OceanCliff").expect("preset");
        let mut preset = LiquidPreset::build(def.as_ref()).unwrap();
        let report = preset.check_authored().unwrap();
        println!("OceanCliff authored {:?}: {} nodes checked, {} array/private bytes", preset.domains(), report.checked, report.scene_bytes);
    }

    #[test]
    fn inverse_fft_extent_counts_retained_full_buffers_at_rebind_peak() {
        let (_, def) = liquid_presets().into_iter().find(|(id, _)| id == "OceanCliff").expect("preset");
        let preset = LiquidPreset::build(def.as_ref()).unwrap();
        for padding in [0, 4096] {
            let (bound, held) = manifold_nodes_water::liquid::extent::testkit::inverse_fft_rebind_bytes(&preset, padding);
            // Four cached pairs plus a distinct incoming pair before eviction.
            assert_eq!(bound + held, 5 * bound);
        }
    }

    #[test]
    fn ocean_gathers_reject_undersized_inputs() {
        use manifold_core::effect_graph_def::SerializedParamValue;
        let (_, def) = liquid_presets().into_iter().find(|(id, _)| id == "OceanCliff").expect("preset");
        for (type_id, param, value, port) in [
            ("node.inverse_fft_2d", "size", 512.0, "spectrum"),
            ("node.ocean_displace", "size_0", 512.0, "field_0"),
            ("node.make_triangles", "src_cols", 4096.0, "in"),
        ] {
            let mut flat = manifold_core::flatten::flatten_groups(def.as_ref()).expect("preset flattens");
            // Keep the producer's capacity unchanged while the consumer's
            // gather footprint grows; a size-bounded exemption would miss it.
            let node = flat.nodes.iter_mut().find(|node| node.type_id == type_id).expect("consumer");
            node.params.insert(param.into(), SerializedParamValue::Float { value });
            match check_preset_extents(&flat, 8) {
                Err(ExtentError::Uncovered { node, detail }) => {
                    assert!(node.contains(type_id), "{node}");
                    assert!(detail.starts_with(&format!("{port} holds ")), "{detail}");
                }
                other => panic!("{type_id}.{param}: expected uncovered {port}, got {other:?}"),
            }
        }
    }

#[test]
fn extent_rule_inventory_preserves_the_rule_table() {
    let mut expected = vec![
        "node.ocean_spectrum",
        "node.inverse_fft_2d",
        "node.ocean_displace",
        "node.make_triangles",
        "node.projected_grid",
        "node.interpolate_particle_frames",
        "node.push_out_of_solid",
        "node.mix_arrays",
        manifold_core::liquid_domain::MATTER_DOMAIN_TYPE_ID,
        "node.matter_fill",
        "node.matter_state",
        "node.zero_array",
        "node.matter_move_bodies",
        "node.matter_to_grid",
        "node.matter_grid_update",
        "node.matter_body_reaction",
        "node.grid_to_matter",
        "node.matter_stats",
        "node.liquid_solid_distance",
        "node.matter_frame",
        "node.matter_face_component",
        "node.face_sample_component",
        manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID,
        "node.liquid_fill",
        "node.liquid_state",
        "node.liquid_stats",
        "node.liquid_frame",
        "node.gpu_flip_step",
        "node.dot_products",
        "node.divide_by_value",
        "node.sort_particles_into_cells",
        "node.shape_particle_blobs",
        "node.blob_bounds",
        "node.particle_volume",
        "node.offset_lattice",
        "node.redistance_lattice",
        "node.lattice_bricks",
        "node.smooth_lattice",
        "node.clamp_liquid_to_solids",
        "node.count_surface_triangles",
        "node.count_surface_edges",
        "node.running_total",
        "node.volume_surface_mesh",
        "node.relax_surface_mesh",
        "node.smooth_surface_mesh",
        "node.surface_mesh_normals",
        "node.render_scene",
        "node.scene_object",
        "node.physics_world",
        "node.cube_mesh",
        "node.platonic_solid_mesh",
        "node.gltf_mesh_source",
        "node.bake_environment",
        "node.exposure",
        "node.hdri_source",
        "node.gltf_texture_source",
        "node.sea_horizon_env",
        "node.camera_sky",
        "node.over",
        "node.coc_from_depth",
        "node.bokeh_gather",
        "node.motion_blur",
        "node.switch_texture",
        "node.tone_map",
        "node.surface_crossings",
        "node.nearest_crossing",
        "node.crossing_distance",
        "node.liquid_cells",
        "node.lattice_curvature",
        "node.turbulence_field",
        "node.inside_turbulence_potential",
        "node.turbulence_emission_count",
        "node.whitewater_emitter_velocity",
        "node.whitewater_obstacle_source",
        "node.whitewater_influence",
        "node.dust_potential",
        "node.extend_lattice",
        "node.jitter_particles",
        "node.sample_faces_at_particles",
        "node.energy_potential",
        "node.wavecrest_potential",
        "node.emission_count",
        "node.spawn_whitewater",
        "node.whitewater_type",
        "node.advect_whitewater",
        "node.retype_whitewater",
        "node.age_whitewater",
        "node.preserve_foam",
        "node.keep_whitewater",
        "node.whitewater_lifecycle",
        "node.whitewater_step",
        "node.upwind_distance",
        "node.particles_to_copies",
    ];
    expected.sort_unstable();
    let registered: Vec<_> = EXTENT_RULES.iter().map(|rule| rule.type_id).collect();
    assert_eq!(registered, expected);
    assert!(registered.windows(2).all(|pair| pair[0] < pair[1]), "duplicate extent rule");
}
