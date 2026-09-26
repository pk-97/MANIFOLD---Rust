//! Load-time repair for the stock fragment mask sampling regression.

use std::path::Path;

use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, SerializedParamValue,
};
use manifold_core::{NodeId, PresetTypeId};
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::node_graph::scene_modifier_legacy_migration::migrate_legacy_scene_modifiers;

#[path = "common/scene_modifier.rs"]
mod common;

const STOCK_WITH_MASKS: &[&str] = &[
    "MaskedPeel",
    "SurfacePeel",
    "VortexFragments",
    "OrderedRecon",
    "OrderedReconHit",
];

fn imported_owner() -> EffectGraphDef {
    assemble_import_graph(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf/cc0___mushroom.glb"),
    )
    .expect("import fixture")
    .0
}

fn find_mut<'a>(nodes: &'a mut [EffectGraphNode], id: &str) -> Option<&'a mut EffectGraphNode> {
    for node in nodes {
        if node.node_id.as_str() == id {
            return Some(node);
        }
        if let Some(group) = node.group.as_deref_mut()
            && let Some(found) = find_mut(&mut group.nodes, id)
        {
            return Some(found);
        }
    }
    None
}

fn every_node<'a>(nodes: &'a [EffectGraphNode], out: &mut Vec<&'a EffectGraphNode>) {
    for node in nodes {
        out.push(node);
        if let Some(group) = node.group.as_deref() {
            every_node(&group.nodes, out);
        }
    }
}

fn set_mode(owner: &mut EffectGraphDef, node_id: &str, mode: u32) {
    find_mut(&mut owner.scene_modifiers[0].graph.nodes, node_id)
        .unwrap_or_else(|| panic!("missing {node_id}"))
        .params
        .insert(
            "sample_mode".into(),
            SerializedParamValue::Enum { value: mode },
        );
}

fn legacy_owner(recipe: &str, instance_id: &str) -> EffectGraphDef {
    let mut owner = common::attach(&imported_owner(), recipe, instance_id);
    set_mode(&mut owner, "mask", 1);
    if matches!(recipe, "OrderedRecon" | "OrderedReconHit") {
        set_mode(&mut owner, "stagger", 1);
    }
    owner
}

#[test]
fn stock_defaults_are_vertex_sampled_and_surface_peel_hit_is_unchanged() {
    for id in STOCK_WITH_MASKS {
        let recipe = common::stock_recipe(id);
        let mut nodes = Vec::new();
        every_node(&recipe.nodes, &mut nodes);
        let expected = if matches!(*id, "OrderedRecon" | "OrderedReconHit") {
            2
        } else {
            1
        };
        let modes: Vec<_> = nodes
            .iter()
            .filter(|node| matches!(node.node_id.as_str(), "mask" | "stagger"))
            .map(|node| {
                assert_eq!(
                    node.params.get("sample_mode"),
                    Some(&SerializedParamValue::Enum { value: 0 })
                );
                node.node_id.as_str()
            })
            .collect();
        assert_eq!(modes.len(), expected, "{id}");
    }

    let mut owner = common::attach(&imported_owner(), "SurfacePeelHit", "surface_peel_hit");
    let before = owner.clone();
    let report = migrate_legacy_scene_modifiers(&mut owner, &PrimitiveRegistry::with_builtin());
    assert!(!report.changed);
    assert_eq!(owner, before);
}

#[test]
fn old_stock_snapshots_repair_once_and_preserve_tuned_values() {
    let registry = PrimitiveRegistry::with_builtin();
    for id in STOCK_WITH_MASKS {
        let mut owner = legacy_owner(id, &format!("{id}_instance"));
        find_mut(&mut owner.scene_modifiers[0].graph.nodes, "mask")
            .expect("mask")
            .params
            .insert(
                "center_x".into(),
                SerializedParamValue::Float { value: 0.37 },
            );
        let mut expected = owner.clone();
        set_mode(&mut expected, "mask", 0);
        if matches!(*id, "OrderedRecon" | "OrderedReconHit") {
            set_mode(&mut expected, "stagger", 0);
        }

        let saved = serde_json::to_vec(&owner).expect("serialize old snapshot");
        let mut owner: EffectGraphDef =
            serde_json::from_slice(&saved).expect("reload old snapshot");
        let report = migrate_legacy_scene_modifiers(&mut owner, &registry);
        assert!(report.changed, "{id}: {:?}", report.diagnostics);
        assert_eq!(owner, expected, "{id}: repair changed unrelated state");
        let repaired = serde_json::to_vec(&owner).expect("serialize repaired snapshot");
        let reloaded: EffectGraphDef =
            serde_json::from_slice(&repaired).expect("reload repaired snapshot");
        assert_eq!(
            reloaded, expected,
            "{id}: repaired snapshot must round-trip"
        );
        let once = owner.clone();
        assert!(!migrate_legacy_scene_modifiers(&mut owner, &registry).changed);
        assert_eq!(owner, once, "{id}: second migration must be a no-op");
    }
}

#[test]
fn custom_modes_exposure_binding_and_rewires_are_left_untouched() {
    let registry = PrimitiveRegistry::with_builtin();
    for id in STOCK_WITH_MASKS {
        for mode in [0, 2] {
            let mut owner = legacy_owner(id, "custom_mode");
            set_mode(&mut owner, "mask", mode);
            let before = owner.clone();
            assert!(!migrate_legacy_scene_modifiers(&mut owner, &registry).changed);
            assert_eq!(owner, before, "{id} mode {mode}");
        }
    }

    for id in STOCK_WITH_MASKS {
        let mut exposed = legacy_owner(id, "exposed");
        find_mut(&mut exposed.scene_modifiers[0].graph.nodes, "mask")
            .unwrap()
            .exposed_params
            .insert("sample_mode".into());
        let before = exposed.clone();
        assert!(!migrate_legacy_scene_modifiers(&mut exposed, &registry).changed);
        assert_eq!(exposed, before, "{id} exposed mode");
    }

    for id in STOCK_WITH_MASKS {
        let mut bound = legacy_owner(id, "bound");
        let mask_id = find_mut(&mut bound.scene_modifiers[0].graph.nodes, "mask")
            .unwrap()
            .node_id
            .clone();
        let metadata = bound.scene_modifiers[0]
            .graph
            .preset_metadata
            .as_mut()
            .unwrap();
        let mut binding = metadata.bindings.first().cloned().expect("stock binding");
        binding.id = "bound_sample_mode".into();
        binding.label = "Bound sample mode".into();
        binding.target = BindingTarget::Node {
            node_id: mask_id,
            param: "sample_mode".into(),
        };
        metadata.bindings.push(binding);
        let before = bound.clone();
        assert!(!migrate_legacy_scene_modifiers(&mut bound, &registry).changed);
        assert_eq!(bound, before, "{id} bound mode");
    }

    for id in STOCK_WITH_MASKS {
        let mut rewired = legacy_owner(id, "rewired");
        let group_id = if *id == "VortexFragments" {
            "vortex_stage"
        } else {
            "peel_stage"
        };
        let group = rewired.scene_modifiers[0]
            .graph
            .nodes
            .iter_mut()
            .find(|node| node.node_id == NodeId::new(group_id))
            .and_then(|node| node.group.as_deref_mut())
            .expect("fragment group");
        group.wires[0].from_port = "rewired".into();
        let before = rewired.clone();
        assert!(!migrate_legacy_scene_modifiers(&mut rewired, &registry).changed);
        assert_eq!(rewired, before, "{id} rewired");
    }

    let mut handled = legacy_owner("MaskedPeel", "handled");
    find_mut(&mut handled.scene_modifiers[0].graph.nodes, "group_current")
        .unwrap()
        .handle = Some("custom boundary".into());
    let before = handled.clone();
    assert!(!migrate_legacy_scene_modifiers(&mut handled, &registry).changed);
    assert_eq!(handled, before, "boundary handle is structural");
}

#[test]
fn custom_preset_id_with_stock_shape_is_not_repaired() {
    let registry = PrimitiveRegistry::with_builtin();
    let mut owner = legacy_owner("MaskedPeel", "custom_id");
    owner.scene_modifiers[0]
        .graph
        .preset_metadata
        .as_mut()
        .unwrap()
        .id = PresetTypeId::from_string("MyMaskedPeel".into());
    let before = owner.clone();
    assert!(!migrate_legacy_scene_modifiers(&mut owner, &registry).changed);
    assert_eq!(owner, before);
}
