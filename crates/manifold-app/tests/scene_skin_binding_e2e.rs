//! SCENE_FX P4b end-to-end: the Skin row's binding survives save/reload, and
//! killing the source layer leaves the object rendering with a loud
//! missing-layer state (the stored id never clears).
//!
//! Lives in `manifold-app/tests/` because only this crate links the real
//! glTF import path, the editing commands, and the renderer's `SceneVm`
//! discovery together (same reason as `user_param_bindings_e2e.rs`).
//!
//! The panel round trip (row → dropdown → command → VM → row) is driven by
//! the L3 flow `scripts/ui-flows/scene-skin.json`; this file covers the two
//! phases the JSON harness cannot express: project save/reload, and source
//! layer deletion.

// Force the linker to keep manifold-renderer's inventory::submit! blocks.
use manifold_renderer as _;

use manifold_editing::command::Command;
use manifold_core::effect_graph_def::EffectGraphDef;

fn gltf_fixture_project() -> manifold_core::project::Project {
    use manifold_core::project::{EmbeddedOrigin, EmbeddedPreset};
    use manifold_editing::commands::layer::ImportModelLayerCommand;

    let embedded = EmbeddedPreset {
        kind: manifold_core::preset_def::PresetKind::Generator,
        def: imported_def(),
        origin: EmbeddedOrigin::Saved,
    };
    let display_name = embedded
        .def
        .preset_metadata
        .as_ref()
        .map(|m| m.display_name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Azalea".to_string());
    // Same ordering as the ui-snap gltfscene fixture: install the overlay
    // BEFORE the layer exists so init_defaults resolves against it. The
    // fixture helper is `pub(crate)` (no lib target), so this inlines it —
    // the whole body is the renderer's preset overlay call.
    let id = embedded.id().expect("preset id");
    let json = serde_json::to_string(&embedded.def).expect("serialize embedded preset");
    manifold_renderer::preset_loader::set_project_presets(
        Vec::new(),
        vec![(id.as_str().to_string(), json, embedded.origin)],
    );
    let mut project = manifold_core::project::Project::default();
    let mut cmd = ImportModelLayerCommand::new(display_name, embedded, 0, None);
    cmd.execute(&mut project);
    project.reconcile_param_manifests();
    project
}

fn imported_def() -> EffectGraphDef {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb");
    manifold_renderer::node_graph::gltf_import::assemble_import_graph(&path)
        .unwrap_or_else(|e| panic!("assemble_import_graph failed: {e}"))
        .0
}

/// The P4b contract the JSON flow can't reach: a skin set through the real
/// editing command survives a project save/reload, and deleting the source
/// layer leaves the object Known with `source_missing` set — the stored id is
/// never cleared (render-level fallback for a missing layer is P4a's
/// `preset_runtime::tests::layer_skin.rs` proof, not repeated here).
#[test]
fn skin_binding_survives_save_reload_and_missing_source() {
    use manifold_core::LayerId;
    use manifold_core::PresetTypeId;
    use manifold_core::types::LayerType;
    use manifold_editing::commands::graph::{
        SetSceneObjectSkinSourceCommand, SkinTargetMap as EditTarget,
    };
    use manifold_editing::commands::layer::{AddLayerCommand, DeleteLayerCommand};
    use manifold_renderer::node_graph::scene_vm::{SceneObjectVm, SceneVm};

    let mut project = gltf_fixture_project();
    let scene_lid = project.timeline.layers[0].layer_id.clone();

    // The skin's source layer — any second layer; the binding only needs its
    // id and name in the timeline.
    let mut add = AddLayerCommand::new(
        "Skin Source".to_string(),
        LayerType::Generator,
        PresetTypeId::from_string("plasma".to_string()),
        1,
        None,
    );
    add.execute(&mut project);
    let source_lid = project.timeline.layers[1].layer_id.clone();
    let layer_ids = |p: &manifold_core::project::Project| -> Vec<LayerId> {
        p.timeline.layers.iter().map(|l| l.layer_id.clone()).collect()
    };

    // Displacement precondition, observed on the imported def: the glb bakes
    // a texture into object 11's base_color_map. If the fixture ever stops
    // doing this the displacement contract below needs re-checking, not this
    // assert relaxing.
    let imported = imported_def();
    let group = imported
        .nodes
        .iter()
        .find(|n| n.id == 15)
        .and_then(|n| n.group.as_ref())
        .expect("fixture group 15");
    assert!(
        group.wires.iter().any(|w| w.to_node == 11 && w.to_port == "base_color_map"),
        "fixture no longer bakes a texture into base_color_map"
    );

    // Set the skin through the real command, production dispatch shape.
    let target = manifold_core::GraphTarget::Generator(scene_lid.clone());
    let mut set = SetSceneObjectSkinSourceCommand::new(
        target,
        vec![15],
        11,
        None,
        EditTarget::Emissive,
        Some(source_lid.to_string()),
        imported_def(),
    );
    set.execute(&mut project);

    let skin_of = |p: &manifold_core::project::Project| {
        let graph = p.timeline.layers[0]
            .generator_graph()
            .expect("layer graph materialized by the edit")
            .clone();
        let vm = SceneVm::from_def_with_layers(&graph, &layer_ids(p)).expect("scene vm");
        match &vm.objects[0] {
            SceneObjectVm::Known(row) => row.skin.clone().expect("skin discovered"),
            SceneObjectVm::Custom { .. } => panic!("object 11 must stay Known"),
        }
    };

    let skin = skin_of(&project);
    assert_eq!(skin.source_layer_id.as_deref(), Some(source_lid.as_ref()));
    assert_eq!(skin.target_map, manifold_renderer::node_graph::scene_vm::SkinTargetMap::Emissive);
    assert!(!skin.source_missing);

    // Save → reload (the same Project serde the .manifold writers use).
    let json = serde_json::to_string_pretty(&project).expect("serialize project");
    let mut reloaded: manifold_core::project::Project =
        serde_json::from_str(&json).expect("reload project");
    let skin = skin_of(&reloaded);
    assert_eq!(
        skin.source_layer_id.as_deref(),
        Some(source_lid.as_ref()),
        "binding survives save/reload"
    );
    assert!(!skin.source_missing);

    // Kill the source layer: the object stays Known, the stored id stays,
    // the VM flags it missing (the panel's D8 chip source).
    let source_layer = reloaded
        .timeline
        .layers
        .iter()
        .find(|l| l.layer_id == source_lid)
        .cloned()
        .expect("source layer");
    let mut delete = DeleteLayerCommand::new(source_layer);
    delete.execute(&mut reloaded);
    let skin = skin_of(&reloaded);
    assert_eq!(
        skin.source_layer_id.as_deref(),
        Some(source_lid.as_ref()),
        "the stored layer id is never cleared"
    );
    assert!(skin.source_missing, "missing source flags the D8 chip");
}
