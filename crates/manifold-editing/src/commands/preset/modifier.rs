//! Preset transactions for a local scene-modifier recipe.
//!
//! A scene modifier is a graph snapshot inside its generator owner, rather
//! than a `PresetInstance` of its own. These helpers keep the owner graph and
//! its instance-layer state in one undo unit while the public command shape
//! remains shared with ordinary effects and generators.

use manifold_core::GraphTarget;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::project::{EmbeddedOrigin, EmbeddedPreset, Project};
use manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters;
use manifold_core::scene_modifier_preset::validate_scene_modifier_schema;

use super::super::graph::{InstanceLayerSnapshot, prune_instance_params};
use super::{ForkPresetCommand, RevertToLibraryCommand};

#[derive(Debug)]
pub(super) struct ForkReverse {
    old_graph: EffectGraphDef,
    old_instance: InstanceLayerSnapshot,
    applied: bool,
}

#[derive(Debug)]
pub(super) struct RevertReverse {
    old_graph: EffectGraphDef,
    old_instance: InstanceLayerSnapshot,
    applied: bool,
}

pub(super) fn fork_was_applied(command: &ForkPresetCommand) -> bool {
    command
        .modifier_reverse
        .as_ref()
        .is_some_and(|reverse| reverse.applied)
}

pub(super) fn revert_was_applied(command: &RevertToLibraryCommand) -> bool {
    command
        .modifier_reverse
        .as_ref()
        .is_some_and(|reverse| reverse.applied)
}

pub(super) fn fork_execute(command: &mut ForkPresetCommand, project: &mut Project) {
    let Some(target) = scene_target(&command.target) else {
        return;
    };
    if !command.kind.is_scene_modifier() {
        eprintln!("[manifold-editing] scene modifier preset fork requires SceneModifier kind");
        return;
    }

    // Build the fork and the complete owner candidate before touching either
    // the embedded registry or the live owner. This makes malformed recipes,
    // missing modifiers, and stale targets atomic no-ops.
    let forked = if let Some(forked) = command.forked.clone() {
        forked
    } else {
        let Some(forked) = make_fork(project, command.kind, &command.source_def) else {
            return;
        };
        forked
    };
    let Some(candidate) = replacement_candidate(project, target, &forked.def) else {
        eprintln!(
            "[manifold-editing] scene modifier preset fork rejected for {}",
            command.target.label()
        );
        return;
    };

    project.upsert_embedded_preset(forked.clone());
    let Some(owner) = project.graph_target_owner_mut(target) else {
        return;
    };
    if command.modifier_reverse.is_none() {
        command.modifier_reverse = Some(ForkReverse {
            old_graph: owner
                .graph
                .clone()
                .expect("candidate preflight found owner graph"),
            old_instance: InstanceLayerSnapshot::capture(owner),
            applied: false,
        });
    }
    owner.graph = Some(candidate.graph);
    prune_instance_params(owner, &candidate.removed_param_ids);
    owner.refresh_manifest_from_graph();
    if command.reseed_values {
        let modifier_id = match target {
            GraphTarget::SceneModifier { modifier_id, .. } => modifier_id,
            _ => return,
        };
        let old_graph = command
            .modifier_reverse
            .as_ref()
            .map(|reverse| &reverse.old_graph);
        if let Some(old_graph) = old_graph {
            seed_imported_defaults(owner, old_graph, modifier_id, &forked.def);
        }
    }
    owner.bump_graph_structure_version();
    if command.forked.is_none() {
        command.forked = Some(forked);
    }
    if let Some(reverse) = command.modifier_reverse.as_mut() {
        reverse.applied = true;
    }
}

pub(super) fn fork_undo(command: &mut ForkPresetCommand, project: &mut Project) {
    let Some(target) = scene_target(&command.target) else {
        return;
    };
    let Some(reverse) = command.modifier_reverse.as_mut() else {
        return;
    };
    if !reverse.applied {
        return;
    }
    let Some(owner) = project.graph_target_owner_mut(target) else {
        return;
    };
    owner.graph = Some(reverse.old_graph.clone());
    reverse.old_instance.clone().restore(owner);
    owner.bump_graph_structure_version();
    if let Some(forked) = command.forked.as_ref().and_then(|item| item.id().cloned()) {
        project.remove_embedded_preset(&forked);
    }
    reverse.applied = false;
}

pub(super) fn revert_execute(command: &mut RevertToLibraryCommand, project: &mut Project) {
    let Some(target) = scene_target(&command.target) else {
        return;
    };
    if !command.resolves_in_catalog {
        eprintln!(
            "[manifold-editing] RevertToLibrary: {} no longer resolves in the catalog",
            command.target.label()
        );
        return;
    }
    let Some(resolved) = command.resolved_def.as_ref() else {
        eprintln!(
            "[manifold-editing] RevertToLibrary: no resolved recipe supplied for {}",
            command.target.label()
        );
        return;
    };
    let Some(candidate) = replacement_candidate(project, target, resolved) else {
        eprintln!(
            "[manifold-editing] RevertToLibrary: invalid recipe or target {}",
            command.target.label()
        );
        return;
    };
    let Some(owner) = project.graph_target_owner_mut(target) else {
        return;
    };
    if command.modifier_reverse.is_none() {
        command.modifier_reverse = Some(RevertReverse {
            old_graph: owner
                .graph
                .clone()
                .expect("candidate preflight found owner graph"),
            old_instance: InstanceLayerSnapshot::capture(owner),
            applied: false,
        });
    }
    owner.graph = Some(candidate.graph);
    prune_instance_params(owner, &candidate.removed_param_ids);
    owner.refresh_manifest_from_graph();
    owner.bump_graph_structure_version();
    if let Some(reverse) = command.modifier_reverse.as_mut() {
        reverse.applied = true;
    }
}

pub(super) fn revert_undo(command: &mut RevertToLibraryCommand, project: &mut Project) {
    let Some(target) = scene_target(&command.target) else {
        return;
    };
    let Some(reverse) = command.modifier_reverse.as_mut() else {
        return;
    };
    if !reverse.applied {
        return;
    }
    let Some(owner) = project.graph_target_owner_mut(target) else {
        return;
    };
    owner.graph = Some(reverse.old_graph.clone());
    reverse.old_instance.clone().restore(owner);
    owner.bump_graph_structure_version();
    reverse.applied = false;
}

fn scene_target(target: &GraphTarget) -> Option<&GraphTarget> {
    if matches!(target, GraphTarget::SceneModifier { .. }) {
        Some(target)
    } else {
        None
    }
}

fn make_fork(
    project: &Project,
    kind: manifold_core::preset_def::PresetKind,
    source: &EffectGraphDef,
) -> Option<EmbeddedPreset> {
    validate_recipe(source)?;
    let base = source
        .preset_metadata
        .as_ref()
        .map(|meta| meta.id.as_str())
        .filter(|id| !id.is_empty())
        .unwrap_or("modifier");
    let id = project.mint_forked_preset_id(base);
    let mut def = source.clone();
    let metadata = def.preset_metadata.as_mut()?;
    metadata.id = id.clone();
    metadata.display_name = id.as_str().to_string();
    Some(EmbeddedPreset {
        kind,
        def,
        origin: EmbeddedOrigin::Saved,
    })
}

struct Candidate {
    graph: EffectGraphDef,
    removed_param_ids: Vec<String>,
}

fn replacement_candidate(
    project: &Project,
    target: &GraphTarget,
    replacement: &EffectGraphDef,
) -> Option<Candidate> {
    validate_recipe(replacement)?;
    let owner = project.graph_target_owner(target)?;
    let old = owner.graph.as_ref()?;
    target.graph_in(old)?;
    let mut candidate = old.clone();
    *target.graph_in_mut(&mut candidate)? = replacement.clone();
    let modifier_id = match target {
        GraphTarget::SceneModifier { modifier_id, .. } => modifier_id,
        _ => return None,
    };
    let edit = reconcile_scene_modifier_parameters(&candidate, modifier_id).ok()?;
    validate_scene_modifier_schema(&edit.graph).ok()?;
    Some(Candidate {
        graph: edit.graph,
        removed_param_ids: edit.removed_param_ids,
    })
}

fn validate_recipe(def: &EffectGraphDef) -> Option<()> {
    if !def.scene_modifiers.is_empty()
        || def
            .preset_metadata
            .as_ref()
            .is_none_or(|metadata| metadata.scene_modifier.is_none())
    {
        return None;
    }
    validate_scene_modifier_schema(def).ok().map(|_| ())
}

fn seed_imported_defaults(
    owner: &mut manifold_core::effects::PresetInstance,
    old_graph: &EffectGraphDef,
    modifier_id: &manifold_core::NodeId,
    imported: &EffectGraphDef,
) {
    let Some(imported_meta) = imported.preset_metadata.as_ref() else {
        return;
    };
    let preparation: std::collections::HashSet<&str> = imported_meta
        .scene_modifier
        .as_ref()
        .map(|recipe| {
            recipe
                .preparation_params
                .iter()
                .map(String::as_str)
                .collect()
        })
        .unwrap_or_default();
    let Some(old_meta) = old_graph.preset_metadata.as_ref() else {
        return;
    };
    let old_ids: std::collections::HashSet<&str> = old_meta
        .bindings
        .iter()
        .filter_map(|binding| {
            matches!(&binding.target, BindingTarget::SceneModifier { modifier_id: id, .. } if id == modifier_id)
                .then_some(binding.id.as_str())
        })
        .collect();

    let Some(metadata) = owner
        .graph
        .as_ref()
        .and_then(|graph| graph.preset_metadata.as_ref())
    else {
        return;
    };
    let mut seeds = Vec::new();
    for binding in metadata.bindings.iter().filter(|binding| {
        matches!(&binding.target, BindingTarget::SceneModifier { modifier_id: id, param_id }
            if id == modifier_id && !preparation.contains(param_id.as_str()))
    }) {
        if !old_ids.contains(binding.id.as_str()) {
            continue;
        }
        let shared = metadata
            .bindings
            .iter()
            .filter(|other| other.id == binding.id)
            .count()
            + metadata
                .string_bindings
                .iter()
                .filter(|other| other.id == binding.id)
                .count()
            > 1;
        if shared {
            continue;
        }
        let BindingTarget::SceneModifier { param_id, .. } = &binding.target else {
            continue;
        };
        let Some(local) = imported_meta
            .params
            .iter()
            .find(|param| &param.id == param_id)
        else {
            continue;
        };
        seeds.push((binding.id.clone(), local.default_value));
    }
    if let Some(metadata) = owner
        .graph
        .as_mut()
        .and_then(|graph| graph.preset_metadata.as_mut())
    {
        for (id, default) in &seeds {
            if let Some(outer) = metadata.params.iter_mut().find(|param| param.id == *id) {
                outer.default_value = *default;
            }
        }
    }
    for (id, default) in seeds {
        if let Some(param) = owner.params.get_mut(&id) {
            param.spec.default_value = default;
            param.value = default;
            param.base = default;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::EffectGraphDef;
    use manifold_core::layer::Layer;
    use manifold_core::preset_def::PresetKind;
    use manifold_core::scene_modifier_preset::{
        SceneModifierInstanceDef, SceneNodeRef, SceneTargetSelection,
    };

    fn local(id: &str, gain_default: f32) -> EffectGraphDef {
        serde_json::from_value(serde_json::json!({
            "version": 3,
            "presetMetadata": {
                "id": id,
                "displayName": "Same Label",
                "category": "Geometry",
                "oscPrefix": "modifier",
                "bindings": [],
                "params": [
                    {"id":"enabled","name":"Enabled","min":0.0,"max":1.0,"defaultValue":1.0,"isToggle":true},
                    {"id":"gain","name":"Gain","min":0.0,"max":10.0,"defaultValue":gain_default}
                ],
                "sceneModifier": {"schemaVersion":1,"singleton":true,"enabledParam":"enabled"}
            },
            "nodes": [], "wires": []
        }))
        .expect("valid local scene modifier recipe")
    }

    fn owner_graph() -> EffectGraphDef {
        let a = SceneModifierInstanceDef {
            id: "a".into(),
            scene: SceneNodeRef {
                scope: Vec::new(),
                node: "scene".into(),
            },
            targets: SceneTargetSelection::AllObjects,
            mesh_frames: Vec::new(),
            legacy_math_view_carrier: None,
            graph: Box::new(local("recipe-a", 0.2)),
        };
        let b = SceneModifierInstanceDef {
            id: "b".into(),
            scene: a.scene.clone(),
            targets: a.targets.clone(),
            mesh_frames: Vec::new(),
            legacy_math_view_carrier: None,
            graph: Box::new(local("recipe-b", 0.7)),
        };
        serde_json::from_value(serde_json::json!({
            "version": 3,
            "presetMetadata": {
                "id":"Generator", "displayName":"Generator", "category":"", "oscPrefix":"",
                "params": [
                    {"id":"modifier_a_gain","name":"Gain","min":0.0,"max":10.0,"defaultValue":0.2},
                    {"id":"modifier_b_gain","name":"Gain","min":0.0,"max":10.0,"defaultValue":0.7}
                ],
                "bindings": [
                    {"id":"modifier_a_gain","label":"Gain","defaultValue":0.2,"target":{"kind":"sceneModifier","modifierId":"a","paramId":"gain"}},
                    {"id":"modifier_b_gain","label":"Gain","defaultValue":0.7,"target":{"kind":"sceneModifier","modifierId":"b","paramId":"gain"}}
                ]
            },
            "nodes": [], "wires": [], "sceneModifiers": []
        }))
        .map(|mut graph: EffectGraphDef| {
            graph.scene_modifiers = vec![a, b];
            graph
        })
        .expect("valid owner graph")
    }

    fn project_with_modifiers() -> (Project, GraphTarget) {
        let mut project = Project::default();
        let mut layer = Layer::new_generator("Generator".into(), PresetTypeId::new("Generator"), 0);
        let layer_id = layer.layer_id.clone();
        let owner = layer.gen_params_or_init();
        owner.graph = Some(owner_graph());
        owner.refresh_manifest_from_graph();
        project.timeline.layers.push(layer);
        let target = GraphTarget::SceneModifier {
            owner: Box::new(GraphTarget::Generator(layer_id)),
            modifier_id: "a".into(),
        };
        (project, target)
    }

    #[test]
    fn scene_modifier_preset_edit_make_unique_changes_only_selected_recipe() {
        let (mut project, target) = project_with_modifiers();
        let before = project.graph_target_owner(&target).unwrap().graph.clone();
        let source = project.graph_for_target(&target, None).unwrap().clone();
        let mut command = ForkPresetCommand::new(target.clone(), PresetKind::SceneModifier, source);
        command.execute(&mut project);

        assert!(command.was_applied());
        let fork_id = command.forked_id().cloned().expect("fork id");
        assert!(project.embedded_preset(&fork_id).is_some());
        let after = project
            .graph_target_owner(&target)
            .unwrap()
            .graph
            .as_ref()
            .unwrap();
        assert_eq!(
            after.scene_modifiers[1],
            before.as_ref().unwrap().scene_modifiers[1]
        );
        assert_eq!(
            after.scene_modifiers[0]
                .graph
                .preset_metadata
                .as_ref()
                .unwrap()
                .id,
            fork_id
        );

        command.undo(&mut project);
        assert_eq!(project.graph_target_owner(&target).unwrap().graph, before);
        assert!(project.embedded_preset(&fork_id).is_none());
        command.execute(&mut project);
        assert!(command.was_applied());
        assert!(project.embedded_preset(&fork_id).is_some());
    }

    #[test]
    fn scene_modifier_preset_edit_import_revert_and_invalid_are_atomic() {
        let (mut project, target) = project_with_modifiers();
        let before_graph = project.graph_target_owner(&target).unwrap().graph.clone();
        let before_embedded = project.embedded_presets.clone();
        let invalid = {
            let mut def = local("bad", 0.9);
            def.version = 99;
            def
        };
        let mut invalid_command =
            ForkPresetCommand::importing(target.clone(), PresetKind::SceneModifier, invalid);
        invalid_command.execute(&mut project);
        assert!(!invalid_command.was_applied());
        assert_eq!(
            project.graph_target_owner(&target).unwrap().graph,
            before_graph
        );
        assert_eq!(
            serde_json::to_value(&project.embedded_presets).unwrap(),
            serde_json::to_value(&before_embedded).unwrap()
        );

        let imported = local("imported", 0.9);
        let old_local = project.graph_for_target(&target, None).unwrap().clone();
        let mut import = ForkPresetCommand::importing(
            target.clone(),
            PresetKind::SceneModifier,
            imported.clone(),
        );
        import.execute(&mut project);
        assert!(import.was_applied());
        assert_eq!(
            project
                .graph_for_target(&target, None)
                .unwrap()
                .preset_metadata
                .as_ref()
                .unwrap()
                .id,
            import.forked_id().unwrap().clone()
        );
        import.undo(&mut project);
        assert_eq!(project.graph_for_target(&target, None), Some(&old_local));

        let mut revert =
            RevertToLibraryCommand::new(target.clone(), true).with_resolved_def(imported);
        revert.execute(&mut project);
        assert!(revert.was_applied());
        revert.undo(&mut project);
        assert_eq!(project.graph_for_target(&target, None), Some(&old_local));

        let before_missing_graph = project.graph_target_owner(&target).unwrap().graph.clone();
        let before_missing_embedded = project.embedded_presets.clone();
        let mut missing = RevertToLibraryCommand::new(target.clone(), true);
        missing.execute(&mut project);
        assert!(!missing.was_applied());
        assert_eq!(
            project.graph_target_owner(&target).unwrap().graph,
            before_missing_graph
        );
        assert_eq!(
            serde_json::to_value(&project.embedded_presets).unwrap(),
            serde_json::to_value(&before_missing_embedded).unwrap()
        );
    }
}
