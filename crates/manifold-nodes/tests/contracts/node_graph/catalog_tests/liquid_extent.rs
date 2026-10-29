use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_node_engine::exec::extent::*;
use manifold_nodes_water::testkit::preset_extents::{LiquidPreset, check_preset_extents};

#[test]
fn gpu_flip_preset_extents_are_covered() {
    // Walk actual producers and consumers with extent.rs, including
    // provided solids, frame rings, sparse schedules and mesh outputs.
    let (_, def) = liquid_presets()
        .into_iter()
        .find(|(id, _)| id == "WaterDamBreakGpuFlip")
        .unwrap();
    for resolution in [8, 32, 64] {
        let report = check_preset_extents(def.as_ref(), resolution).unwrap();
        assert!(report.checked > 20);
    }
}

/// Every engine scene holding a liquid domain.
fn liquid_presets() -> Vec<(String, std::sync::Arc<EffectGraphDef>)> {
    let holds_liquid = |def: &EffectGraphDef| {
        let flat = manifold_node_engine::load::expand::flatten_expanded(
            def,
            &manifold_node_engine::persistence::PrimitiveRegistry::with_builtin(),
        )
        .expect("flattens");
        flat.nodes
            .iter()
            .any(|node| is_liquid_domain(&node.type_id))
    };
    manifold_nodes::engine_scenes::all()
        .filter(|(_, def)| holds_liquid(def.as_ref()))
        .map(|(id, def)| (id.to_string(), def))
        .collect()
}

fn particle_blend_preset() -> EffectGraphDef {
    serde_json::from_str(include_str!(
        "../../../fixtures/presets/WaterDamBreakParticles.json"
    ))
    .unwrap()
}

#[test]
fn particle_blend_presets_have_complete_extent_rules() {
    for text in [
        include_str!("../../../fixtures/presets/WaterDamBreakGpuFlip.json"),
        include_str!("../../../fixtures/presets/WaterDamBreakParticles.json"),
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
        ("node.push_out_of_solid", "pool_tiles", 4096.0),
    ] {
        // Corrupt the compiled topology, including nodes inside Water.
        let mut def = manifold_core::flatten::flatten_groups(&particle_blend_preset())
            .expect("preset flattens");
        let node = def
            .nodes
            .iter_mut()
            .find(|node| node.type_id == type_id)
            .unwrap();
        let id = node.id;
        node.params
            .insert(port.into(), SerializedParamValue::Float { value });
        def.wires
            .retain(|wire| !(wire.to_node == id && wire.to_port == port));
        match check_preset_extents(&def, 16) {
            Err(ExtentError::Uncovered { node, .. }) => assert!(node.contains(type_id), "{node}"),
            other => panic!("{type_id}.{port}: expected uncovered storage, got {other:?}"),
        }
    }
}

/// Section 3.7 (Safety rails) rule 1, I9: every liquid preset at every
/// resolution its domain admits either refuses by name before any GPU
/// work, or every buffer covers every dispatch.
#[test]
fn liquid_preset_small_extent_checked() {
    let (id, def) = liquid_presets()
        .into_iter()
        .find(|(id, _)| id.contains("WaterDamBreakGpuFlip"))
        .expect("GPU FLIP preset");
    let mut preset =
        LiquidPreset::build(def.as_ref()).unwrap_or_else(|error| panic!("{id}: {error}"));
    for resolution in [8, 16] {
        let report = preset
            .check(resolution)
            .unwrap_or_else(|error| panic!("{id} at {resolution}: {error}"));
        assert!(report.checked > 0);
    }
}

#[test]
fn liquid_carry_rejects_short_solid_before_gpu_work() {
    let (_, def) = liquid_presets()
        .into_iter()
        .find(|(id, _)| id == "WaterDamBreakGpuFlip")
        .expect("preset");
    let mut flat = manifold_core::flatten::flatten_groups(def.as_ref()).expect("flattens");
    let carry = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.carry_liquid_into_solids")
        .expect("carry")
        .id;
    // The display table's extent is eight floats, far short of a solid.
    let table = flat
        .nodes
        .iter()
        .find(|n| n.type_id == "node.surface_tiles")
        .expect("an eight-float source")
        .id;
    let solid = flat
        .wires
        .iter_mut()
        .find(|w| w.to_node == carry && w.to_port == "solid")
        .expect("solid wire");
    solid.from_node = table;
    solid.from_port = "extent".into();
    match check_preset_extents(&flat, 8) {
        Err(ExtentError::Uncovered { node, detail }) => {
            assert!(
                node.contains("carry_liquid_into_solids"),
                "{node}: {detail}"
            );
        }
        other => panic!("expected a short solid lattice refusal, got {other:?}"),
    }
}

#[test]
fn liquid_presets_all_extent_checked() {
    let presets = liquid_presets();
    assert!(
        presets.len() >= 6,
        "liquid presets: {:?}",
        presets.iter().map(|(id, _)| id).collect::<Vec<_>>()
    );
    let mut refusals: AHashMap<String, (u32, String)> = AHashMap::default();
    for (id, def) in &presets {
        let mut preset =
            LiquidPreset::build(def.as_ref()).unwrap_or_else(|error| panic!("{id}: {error}"));
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
                    refusals
                        .entry(id.clone())
                        .or_insert((resolution, format!("{node}: {reason}")));
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
        assert!(
            reason.contains("Cell Size") || reason.contains("Grid Budget"),
            "{id}: {reason}"
        );
    }
}

use ahash::AHashMap;
use manifold_core::liquid_domain::is_liquid_domain;

/// A graph with a GPU atom no rule knows fails by name.
#[test]
fn an_atom_without_a_rule_fails_by_name() {
    let (_, def) = liquid_presets()
        .into_iter()
        .find(|(id, _)| id == "SandColumn")
        .expect("preset");
    let mut preset = LiquidPreset::build(def.as_ref()).expect("builds");
    let rules: Vec<ExtentRule> = EXTENT_RULES
        .iter()
        .filter(|rule| rule.type_id != "node.matter_to_grid")
        .copied()
        .collect();
    match manifold_nodes_water::testkit::preset_extents::testkit::check_with_rules(
        &mut preset,
        &rules,
    ) {
        Err(ExtentError::NoRule { type_id, .. }) => assert_eq!(type_id, "node.matter_to_grid"),
        other => panic!("expected a missing rule, got {other:?}"),
    }
}

/// A smoothing step told its pool is larger than the level set it reads is
/// caught before the GPU: Dam Break Matter with the first field_smooth_blend
/// unwired from the display table's pool and set to 4096 tiles.
#[test]
fn a_lattice_past_its_storage_is_caught() {
    use manifold_core::effect_graph_def::SerializedParamValue;
    let (_, def) = liquid_presets()
        .into_iter()
        .find(|(id, _)| id == "SandColumn")
        .expect("preset");
    let mut flat = manifold_core::flatten::flatten_groups(def.as_ref()).expect("flattens");
    let counter = flat
        .nodes
        .iter()
        .find(|node| node.type_id == "node.field_smooth_blend")
        .map(|node| node.id)
        .expect("a smoothing step");
    let lattice = ["pool_tiles"];
    flat.wires
        .retain(|wire| !(wire.to_node == counter && lattice.contains(&wire.to_port.as_str())));
    let node = flat
        .nodes
        .iter_mut()
        .find(|node| node.id == counter)
        .expect("counter");
    for port in lattice {
        node.params
            .insert(port.into(), SerializedParamValue::Float { value: 4096.0 });
    }
    match check_preset_extents(&flat, 64) {
        Err(ExtentError::Uncovered { node, detail }) => {
            assert!(
                // The table is checked first: it is the first array too short.
                node.contains("field_smooth_blend") && detail.starts_with("tiles holds"),
                "{node}: {detail}"
            );
        }
        other => panic!("expected the level set to be short, got {other:?}"),
    }
}

#[test]
fn ocean_cliff_authored_extent_checked() {
    let (_, def) = liquid_presets()
        .into_iter()
        .find(|(id, _)| id == "OceanCliff")
        .expect("preset");
    let mut preset = LiquidPreset::build(def.as_ref()).unwrap();
    let report = preset.check_authored().unwrap();
    println!(
        "OceanCliff authored {:?}: {} nodes checked, {} array/private bytes",
        preset.domains(),
        report.checked,
        report.scene_bytes
    );
}

#[test]
fn inverse_fft_extent_counts_retained_full_buffers_at_rebind_peak() {
    let (_, def) = liquid_presets()
        .into_iter()
        .find(|(id, _)| id == "OceanCliff")
        .expect("preset");
    let preset = LiquidPreset::build(def.as_ref()).unwrap();
    for padding in [0, 4096] {
        let (bound, held) =
            manifold_nodes_water::testkit::preset_extents::testkit::inverse_fft_rebind_bytes(
                &preset, padding,
            );
        // Four cached pairs plus a distinct incoming pair before eviction.
        assert_eq!(bound + held, 5 * bound);
    }
}

#[test]
fn ocean_gathers_reject_undersized_inputs() {
    use manifold_core::effect_graph_def::SerializedParamValue;
    let (_, def) = liquid_presets()
        .into_iter()
        .find(|(id, _)| id == "OceanCliff")
        .expect("preset");
    for (type_id, param, value, port) in [
        ("node.inverse_fft_2d", "size", 512.0, "spectrum"),
        ("node.ocean_displace", "size_0", 512.0, "field_0"),
        ("node.make_triangles", "src_cols", 4096.0, "in"),
    ] {
        let mut flat =
            manifold_core::flatten::flatten_groups(def.as_ref()).expect("preset flattens");
        // Keep the producer's capacity unchanged while the consumer's
        // gather footprint grows; a size-bounded exemption would miss it.
        let node = flat
            .nodes
            .iter_mut()
            .find(|node| node.type_id == type_id)
            .expect("consumer");
        node.params
            .insert(param.into(), SerializedParamValue::Float { value });
        match check_preset_extents(&flat, 8) {
            Err(ExtentError::Uncovered { node, detail }) => {
                assert!(node.contains(type_id), "{node}");
                assert!(detail.starts_with(&format!("{port} holds ")), "{detail}");
            }
            other => panic!("{type_id}.{param}: expected uncovered {port}, got {other:?}"),
        }
    }
}

/// BUG-ju9cb (the water column can't be removed): a preset saved with the
/// old column wired into the domain's `initial_volume`, inside a group whose
/// nodes carry handles, builds. The old shape migrates before the flatten
/// that gives those nodes group paths for handles; migrating the flat
/// document would add a group the next flatten refuses.
#[test]
fn an_old_column_inside_a_group_builds_after_the_migration() {
    use manifold_core::effect_graph_def::EffectGraphWire;
    use manifold_core::initial_fill::{
        INITIAL_FILL_NAME, INITIAL_VOLUME_PORT, TRANSFORM_3D_TYPE_ID, carries_initial_volume,
    };
    use manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
    let (_, def) = liquid_presets()
        .into_iter()
        .find(|(id, _)| id == "WaterDamBreakGpuFlip")
        .expect("preset");
    let mut old = (*def).clone();
    let body = old
        .nodes
        .iter_mut()
        .find_map(|node| {
            node.group
                .as_deref_mut()
                .filter(|_| node.handle.as_deref() == Some("Water"))
        })
        .expect("the Water group");
    let at = body
        .nodes
        .iter()
        .position(|node| node.handle.as_deref() == Some(INITIAL_FILL_NAME) && node.group.is_some())
        .expect("the Initial Fill group");
    let fill = body.nodes.remove(at);
    let mut column = fill
        .group
        .expect("a group")
        .nodes
        .into_iter()
        .find(|node| node.type_id == TRANSFORM_3D_TYPE_ID)
        .expect("the column");
    column.id = body.nodes.iter().map(|node| node.id).max().unwrap_or(0) + 1;
    let domain = body
        .nodes
        .iter()
        .find(|node| node.type_id == GPU_FLIP_DOMAIN_TYPE_ID)
        .expect("the domain")
        .id;
    body.wires.retain(|wire| wire.from_node != fill.id);
    body.wires.push(EffectGraphWire {
        from_node: column.id,
        from_port: "transform".into(),
        to_node: domain,
        to_port: INITIAL_VOLUME_PORT.into(),
    });
    body.nodes.push(column);
    assert!(
        carries_initial_volume(&old),
        "the preset is back in its old shape"
    );
    let mut preset = LiquidPreset::build(&old).unwrap_or_else(|error| panic!("{error}"));
    assert!(preset.check(16).expect("the migrated preset walks").checked > 0);
}
