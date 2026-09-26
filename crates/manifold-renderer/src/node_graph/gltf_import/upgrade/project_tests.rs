use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::effects::{AutomationLane, PresetInstance};
use manifold_core::id::NodeId;
use manifold_core::layer::Layer;
use manifold_core::preset_def::PresetKind;
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::project::{EmbeddedOrigin, EmbeddedPreset, Project};
use manifold_core::scene_modifier_preset::{
    SceneModifierInstanceDef, SceneNodeRef, SceneTargetSelection,
};

use super::upgrade_project_materials;

fn physics_boxes() -> EffectGraphDef {
    serde_json::from_str(include_str!(
        "../../../../assets/generator-presets/PhysicsBoxes.json"
    ))
    .expect("PhysicsBoxes preset fixture")
}

fn remove_subsurface_exposure(def: &mut EffectGraphDef) {
    let Some(meta) = def.preset_metadata.as_mut() else {
        return;
    };
    let mut removed = std::collections::HashSet::new();
    for binding in &meta.bindings {
        if let BindingTarget::Node { param, .. } = &binding.target
            && param.starts_with("subsurface_")
        {
            removed.insert(binding.id.clone());
        }
    }
    meta.bindings
        .retain(|binding| !removed.contains(&binding.id));
    meta.params.retain(|param| !removed.contains(&param.id));
}

fn graphless_project(def: EffectGraphDef) -> Project {
    let id = def
        .preset_metadata
        .as_ref()
        .expect("fixture metadata")
        .id
        .clone();
    let mut layer = Layer::new_generator("Physics".into(), id.clone(), 0);
    let instance = layer.gen_params_mut().expect("generator instance");
    instance.graph = Some(def.clone());
    instance.refresh_manifest_from_graph();
    instance.graph = None;
    let preserved_id = instance
        .params
        .iter()
        .find(|param| param.id() == "40_copy_count")
        .map(|param| param.id().to_string())
        .expect("PhysicsBoxes count control");
    instance.set_base_param(&preserved_id, 3.0);
    instance.automation_lanes = Some(vec![AutomationLane {
        param_id: preserved_id.into(),
        enabled: true,
        points: Vec::new(),
    }]);

    let mut project = Project::default();
    project.timeline.layers.push(layer);
    project.embedded_presets.push(EmbeddedPreset {
        kind: PresetKind::Generator,
        def,
        origin: EmbeddedOrigin::Saved,
    });
    project
}

#[test]
fn graphless_instance_receives_new_material_controls_and_preserves_owned_values() {
    let mut def = physics_boxes();
    remove_subsurface_exposure(&mut def);
    let mut project = graphless_project(def);

    let first = upgrade_project_materials(&mut project);
    assert!(
        first.changed_graphs > 0,
        "missing PBR exposures should be restored"
    );
    let instance = project.timeline.layers[0]
        .gen_params()
        .expect("generator instance");
    assert!(
        instance
            .params
            .iter()
            .any(|param| param.id().contains("subsurface_weight")),
        "graph-less instances must gain the upgraded material controls"
    );
    assert!((instance.get_base_param("40_copy_count") - 3.0).abs() < 1e-6);
    assert!(instance.automation_lanes.as_ref().is_some_and(|lanes| {
        lanes
            .iter()
            .any(|lane| lane.param_id == "40_copy_count" && lane.enabled)
    }));

    let encoded = serde_json::to_string(&project).expect("serialize upgraded project");
    let mut reloaded: Project = serde_json::from_str(&encoded).expect("reload upgraded project");
    // The app installs embedded definitions before reconciling saved wire
    // values. Supply that same descriptor authority locally, without changing
    // the global catalog used by other tests.
    reloaded.timeline.layers[0].gen_params_mut().unwrap().graph =
        Some(reloaded.embedded_presets[0].def.clone());
    assert_eq!(reloaded.reconcile_param_manifests(), 0);
    reloaded.timeline.layers[0].gen_params_mut().unwrap().graph = None;
    let second = upgrade_project_materials(&mut reloaded);
    assert_eq!(
        second.changed_graphs, 0,
        "project material upgrade is idempotent"
    );
    let before: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    let after: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&reloaded).unwrap()).unwrap();
    // Project deserialize also initializes unrelated recording provenance.
    // Compare the complete scene/instance payloads owned by this migration.
    for field in ["timeline", "embeddedPresets"] {
        assert!(before.get(field) == after.get(field), "saved project field {field} changed on reload");
    }
}

#[test]
fn nested_material_upgrade_bumps_owner_structure_version() {
    let mut outer = physics_boxes();
    let mut nested = physics_boxes();
    remove_subsurface_exposure(&mut nested);
    outer.scene_modifiers.push(SceneModifierInstanceDef {
        id: NodeId::new("modifier"),
        scene: SceneNodeRef {
            scope: Vec::new(),
            node: NodeId::new("scene"),
        },
        targets: SceneTargetSelection::AllObjects,
        mesh_frames: Vec::new(),
        legacy_math_view_carrier: None,
        graph: Box::new(nested),
    });

    let id = outer
        .preset_metadata
        .as_ref()
        .expect("outer metadata")
        .id
        .clone();
    let mut instance = PresetInstance::new_generator(id);
    instance.graph = Some(outer);
    let mut project = Project::default();
    let mut layer = Layer::new_generator("Physics".into(), PresetTypeId::new("PhysicsBoxes"), 0);
    *layer.gen_params_mut().expect("generator instance") = instance;
    project.timeline.layers.push(layer);

    let report = upgrade_project_materials(&mut project);
    assert!(
        report.changed_graphs > 0,
        "nested graph upgrade should be observed"
    );
    assert_eq!(
        project.timeline.layers[0]
            .gen_params()
            .unwrap()
            .graph_structure_version,
        1,
        "nested graph changes must invalidate the owning instance structure"
    );
}

#[test]
fn corrected_import_defaults_preserve_custom_and_automated_instance_values() {
    for (base, automated, expected) in [(0.2, false, 1.0), (0.4, false, 0.4), (0.2, true, 0.2)] {
        let mut project = graphless_project(physics_boxes());
        upgrade_project_materials(&mut project);
        let instance = project.timeline.layers[0].gen_params_mut().unwrap();
        let id = "103_specular";
        instance.set_base_param(id, base);
        if automated {
            instance.automation_lanes = Some(vec![AutomationLane {
                param_id: id.into(),
                enabled: true,
                points: Vec::new(),
            }]);
        }
        super::apply_binding_updates(
            instance,
            &[super::MaterialBindingUpdate {
                id: id.into(),
                old_value: 0.2,
                new_value: 1.0,
            }],
        );
        assert_eq!(
            instance.get_base_param(id),
            expected,
            "only an unowned value equal to the old importer default should change"
        );
    }
}
