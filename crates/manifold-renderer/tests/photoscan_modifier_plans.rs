//! Photoscan modifiers through the authored v3 file and preparation seams.
//!
//! The former tests asserted private factory plans. Stock qualification and
//! load migration now own those structural checks; this file keeps the real
//! import, frame capture, selection, and graph preparation behavior covered.

use std::path::Path;
use std::collections::BTreeSet;

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, EffectGraphNode};
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
use manifold_core::NodeId;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::node_graph::scene_modifier_authoring::prepare_new_scene_modifier;
use manifold_renderer::node_graph::scene_modifier_expand::prepare_scene_modifiers;
use manifold_renderer::node_graph::scene_modifier_legacy_migration::migrate_legacy_scene_modifiers;
use manifold_renderer::node_graph::PrimitiveRegistry;

#[path = "common/scene_modifier.rs"]
mod common;

const MUSHROOM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/cc0___mushroom.glb"
);
const PHOTOSCAN_FIXTURE: &str =
    include_str!("fixtures/scene-modifiers/photoscan_stack_applied_v2.json");

fn imported_host() -> EffectGraphDef {
    assemble_import_graph(Path::new(MUSHROOM))
        .unwrap_or_else(|error| panic!("mushroom import failed: {error}"))
        .0
}

fn scene_ref(host: &EffectGraphDef) -> SceneNodeRef {
    common::render_scene(host)
}

fn find_scoped_node<'a>(
    nodes: &'a [EffectGraphNode],
    scope: &[NodeId],
    node_id: &NodeId,
) -> Option<&'a EffectGraphNode> {
    if let Some((head, tail)) = scope.split_first() {
        let group = nodes.iter().find(|node| &node.node_id == head)?.group.as_deref()?;
        find_scoped_node(&group.nodes, tail, node_id)
    } else {
        nodes.iter().find(|node| &node.node_id == node_id)
    }
}

#[test]
fn photoscan_stock_recipes_capture_real_mesh_frames_and_prepare() {
    let host = imported_host();
    let registry = PrimitiveRegistry::with_builtin();
    for preset in [
        "ElasticSculpture", "SurfacePeel", "VortexFragments",
        "SurfaceWaves", "OrderedRecon", "SpatialEchoes",
    ] {
        let instance = prepare_new_scene_modifier(
            &host,
            &common::stock_recipe(preset),
            NodeId::new(format!("photoscan-{preset}")),
            scene_ref(&host),
            SceneTargetSelection::AllObjects,
        )
        .unwrap_or_else(|error| panic!("{preset} preparation failed: {error}"));
        assert!(!instance.mesh_frames.is_empty(), "{preset} captures source frames");
        let attached = manifold_core::scene_modifier_edit::insert_scene_modifier(
            &host, 0, instance,
        )
        .unwrap_or_else(|error| panic!("{preset} insertion failed: {error}"))
        .graph;
        let prepared = prepare_scene_modifiers(&attached, &registry)
            .unwrap_or_else(|error| panic!("{preset} expansion failed: {error}"));
        assert!(prepared.def.scene_modifiers.is_empty());
        assert!(prepared
            .def
            .nodes
            .iter()
            .any(|node| node.type_id == "node.render_scene"));
    }
}

#[test]
fn photoscan_structured_stack_roundtrips_and_prepares_in_both_orders() {
    let registry = PrimitiveRegistry::with_builtin();
    for names in [
        ["SurfaceWaves", "OrderedRecon", "SpatialEchoes"],
        ["SpatialEchoes", "OrderedRecon", "SurfaceWaves"],
    ] {
        let mut host = imported_host();
        for name in names {
            host = common::attach(&host, name, name);
        }
        let saved = serde_json::to_string(&host).expect("stack serializes");
        let reopened: EffectGraphDef = serde_json::from_str(&saved).expect("stack reopens");
        assert_eq!(host, reopened);
        let prepared = prepare_scene_modifiers(&reopened, &registry)
            .unwrap_or_else(|error| panic!("{names:?} expansion failed: {error}"));
        assert!(prepared.def.scene_modifiers.is_empty());
        manifold_renderer::preset_runtime::PresetRuntime::from_def(reopened, &registry, None)
            .unwrap_or_else(|error| panic!("{names:?} runtime preparation failed: {error}"));
    }
}

#[test]
fn photoscan_v2_file_migration_preserves_source_nodes_and_bindings() {
    let mut def: EffectGraphDef = serde_json::from_str(PHOTOSCAN_FIXTURE).expect("fixture parses");
    let before = def.clone();
    let report = migrate_legacy_scene_modifiers(&mut def, &PrimitiveRegistry::with_builtin());
    assert!(report.changed, "migration diagnostics: {:?}", report.diagnostics);
    assert_eq!(def.scene_modifiers.len(), 3);

    for instance in &def.scene_modifiers {
        assert!(!instance.mesh_frames.is_empty(), "each migrated modifier keeps source frames");
        let mut matched_sources = 0;
        for frame in &instance.mesh_frames {
            assert!(!frame.source_definition_hash.is_empty());
            assert!(
                find_scoped_node(&before.nodes, &frame.source.scope, &frame.source.node).is_some(),
                "source {:?} must refer to an imported host node",
                frame.source
            );
            assert!(
                find_scoped_node(&def.nodes, &frame.source.scope, &frame.source.node).is_some(),
                "source {:?} must remain in the migrated host graph",
                frame.source
            );
            matched_sources += 1;
        }
        assert!(matched_sources > 0);
    }
    let metadata = def.preset_metadata.as_ref().expect("host metadata");
    let original_bindings = before
        .preset_metadata
        .as_ref()
        .expect("original host metadata")
        .bindings
        .iter()
        .filter(|binding| matches!(binding.target, BindingTarget::Node { .. }))
        .collect::<Vec<_>>();
    let expected_ids = original_bindings
        .iter()
        .map(|binding| binding.id.as_str())
        .collect::<BTreeSet<_>>();
    let migrated = metadata
        .bindings
        .iter()
        .filter(|binding| matches!(binding.target, BindingTarget::SceneModifier { .. }))
        .collect::<Vec<_>>();
    assert!(!migrated.is_empty(), "migration retains public photo-scan macros");
    let migrated_ids = migrated
        .iter()
        .map(|binding| binding.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(migrated_ids, expected_ids, "all public macro IDs are remapped");
    for original in original_bindings {
        let migrated = migrated
            .iter()
            .find(|binding| binding.id == original.id)
            .expect("each original public macro remains present");
        assert_eq!(migrated.label, original.label);
        assert_eq!(migrated.default_value, original.default_value);
        assert_eq!(migrated.convert, original.convert);
        assert_eq!(migrated.scale, original.scale);
        assert_eq!(migrated.offset, original.offset);
        assert_eq!(migrated.user_added, original.user_added);
        assert_eq!(migrated.default_mirrors_node_param, original.default_mirrors_node_param);
        assert!(matches!(&migrated.target, BindingTarget::SceneModifier { .. }));
    }
    let saved = serde_json::to_string(&def).expect("migrated graph serializes");
    let reloaded: EffectGraphDef = serde_json::from_str(&saved).expect("migrated graph reloads");
    assert_eq!(reloaded, def);
    assert!(!migrate_legacy_scene_modifiers(&mut def, &PrimitiveRegistry::with_builtin()).changed);
}

#[test]
fn photoscan_explicit_selection_keeps_only_selected_source_frames() {
    let host = imported_host();
    let objects = common::scene_objects(&host);
    assert!(objects.len() >= 2, "mushroom import has multiple objects");
    let instance = prepare_new_scene_modifier(
        &host,
        &common::stock_recipe("SurfacePeel"),
        NodeId::new("photoscan-selected"),
        scene_ref(&host),
        SceneTargetSelection::Explicit {
            objects: vec![objects[0].clone()],
        },
    )
    .expect("explicit photoscan selection prepares");
    assert_eq!(instance.mesh_frames.len(), 1);
    assert_eq!(instance.mesh_frames[0].target, objects[0]);
}
