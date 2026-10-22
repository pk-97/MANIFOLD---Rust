//! Prepared whole-owner graph edits with reversible instance parameter state.

use manifold_core::change::ChangeSet;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::project::Project;
use manifold_core::scene_graph_edit::{SceneGraphEdit, SceneGraphEditError};
use manifold_core::{GraphTarget, PresetTypeId};

use crate::commands::graph::{InstanceLayerSnapshot, prune_instance_params};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SceneGraphRejection {
    MissingOwner,
    PresetTypeChanged,
    StaleOwner,
    Noop,
}

/// Copy every host-side value and modulation route addressed by a source
/// param. The graph candidate has already minted the destination metadata;
/// this keeps duplication's runtime state in the same transaction. Every
/// remap is a duplicate, so Ableton mappings stay on the source.
pub(crate) fn duplicate_runtime_parameter_state(
    host: &mut manifold_core::effects::PresetInstance,
    remaps: &[(String, String)],
) {
    let params: Vec<_> = remaps
        .iter()
        .filter_map(|(source, destination)| {
            host.params.get(source).map(|param| {
                let mut copy = param.clone();
                copy.spec.id = destination.clone();
                copy
            })
        })
        .collect();
    for param in params {
        if !host.params.contains(param.id()) {
            host.params.push(param);
        }
    }

    let source = host.clone();
    crate::param_routes::copy_param_routes(&source, host, remaps, crate::param_routes::RouteTransfer::Copy);
}

/// The remaps whose copy is a string control (a Model File), named by the
/// candidate graph's string specs or bindings.
fn string_remaps(graph: &EffectGraphDef, remaps: &[(String, String)]) -> Vec<(String, String)> {
    let Some(meta) = graph.preset_metadata.as_ref() else { return Vec::new() };
    let is_string = |id: &str| {
        meta.string_params.iter().any(|spec| spec.id == id)
            || meta.string_bindings.iter().any(|binding| binding.id == id)
    };
    remaps.iter().filter(|(_, copy)| is_string(copy)).cloned().collect()
}

/// A clip's string overrides as they were before a duplicate wrote them.
type ClipStringsBefore = Vec<(manifold_core::ClipId, Option<std::collections::BTreeMap<String, String>>)>;

/// Copy each clip's string override (the card's Model File choice lives per
/// clip) from a source id to its copy's id, so a duplicate starts at the file
/// its source shows on that clip. Returns every touched clip's prior map.
fn duplicate_clip_string_overrides(
    project: &mut Project,
    owner: &GraphTarget,
    remaps: &[(String, String)],
) -> ClipStringsBefore {
    let mut before = Vec::new();
    let Some(GraphTarget::Generator(layer_id)) = owner.host_target() else { return before };
    let Some((_, layer)) = project.timeline.find_layer_by_id_mut(layer_id.as_str()) else { return before };
    for clip in &mut layer.clips {
        let Some(values) = clip.string_params.as_ref() else { continue };
        let copies: Vec<(String, String)> = remaps
            .iter()
            .filter_map(|(source, copy)| values.get(source).map(|value| (copy.clone(), value.clone())))
            .collect();
        if copies.is_empty() {
            continue;
        }
        before.push((clip.id.clone(), clip.string_params.clone()));
        clip.string_params.get_or_insert_with(Default::default).extend(copies);
    }
    before
}

#[derive(Debug)]
pub(crate) struct SceneGraphTransaction {
    pub(crate) owner: GraphTarget,
    expected_graph: Option<EffectGraphDef>,
    expected_preset_type: PresetTypeId,
    before_resolved: EffectGraphDef,
    candidate: EffectGraphDef,
    /// The owner's graph as execute left it: refreshing the manifest
    /// normalizes the candidate's metadata, and undo checks against this.
    committed: Option<EffectGraphDef>,
    removed_param_ids: Vec<String>,
    parameter_id_remaps: Vec<(String, String)>,
    /// The string-control remaps, whose per-clip overrides the copy carries.
    string_remaps: Vec<(String, String)>,
    clip_strings_before: ClipStringsBefore,
    previous_layer: Option<InstanceLayerSnapshot>,
    touches: ChangeSet,
    applied: bool,
    last_error: Option<SceneGraphRejection>,
    pub(crate) description: &'static str,
}

impl SceneGraphTransaction {
    pub(crate) fn prepare<F>(
        project: &Project,
        owner: GraphTarget,
        owner_default: &EffectGraphDef,
        description: &'static str,
        edit: F,
    ) -> Result<Self, SceneGraphEditError>
    where
        F: FnOnce(&EffectGraphDef) -> Result<SceneGraphEdit, SceneGraphEditError>,
    {
        let error = |message: &str| SceneGraphEditError {
            at: None,
            message: message.into(),
        };
        if !matches!(owner, GraphTarget::Generator(_) | GraphTarget::Effect(_)) {
            return Err(error(
                "Scene graph transactions require an effect or generator owner",
            ));
        }
        let host = project
            .graph_target_owner(&owner)
            .ok_or_else(|| error("Graph owner is no longer present"))?;
        let before_resolved = project
            .graph_for_target(&owner, Some(owner_default))
            .ok_or_else(|| error("Owner graph does not resolve"))?
            .clone();
        let result = edit(&before_resolved)?;
        // A binding id names one value: an edit that would bind two values
        // under one id is refused, never saved (typed identity D5).
        if let Some(identity) = manifold_core::binding_identity::added_errors(
            before_resolved.preset_metadata.as_ref(),
            result.graph.preset_metadata.as_ref(),
        )
        .first()
        {
            return Err(error(&identity.to_string()));
        }
        // Removed or remapped params carry their controls, lanes included; a
        // metadata change alone rebuilds the card manifest.
        let touches = if !result.removed_param_ids.is_empty() || !result.parameter_id_remaps.is_empty() {
            crate::commands::WHOLE_INSTANCES
        } else if before_resolved.preset_metadata != result.graph.preset_metadata {
            crate::commands::GRAPH_AND_MANIFEST
        } else {
            crate::commands::GRAPH_EDIT
        };
        let string_remaps = string_remaps(&result.graph, &result.parameter_id_remaps);
        let touches = if string_remaps.is_empty() {
            touches
        } else {
            touches.union(ChangeSet::of(&[manifold_core::change::Part::Clips]))
        };
        Ok(Self {
            owner,
            expected_graph: host.graph.clone(),
            expected_preset_type: host.effect_type().clone(),
            before_resolved,
            candidate: result.graph,
            committed: None,
            removed_param_ids: result.removed_param_ids,
            parameter_id_remaps: result.parameter_id_remaps,
            string_remaps,
            clip_strings_before: Vec::new(),
            previous_layer: None,
            touches,
            applied: false,
            last_error: None,
            description,
        })
    }

    /// The parts execute and undo write, fixed when the edit is prepared.
    pub(crate) fn touches(&self) -> ChangeSet {
        self.touches
    }

    pub(crate) fn prepared_graph(&self) -> &EffectGraphDef {
        &self.candidate
    }

    fn reject(&mut self, error: SceneGraphRejection) {
        self.last_error = Some(error);
        self.applied = false;
    }

    pub(crate) fn execute(&mut self, project: &mut Project) {
        let Some(host) = project.graph_target_owner_mut(&self.owner) else {
            self.reject(SceneGraphRejection::MissingOwner);
            return;
        };
        if host.effect_type() != &self.expected_preset_type {
            self.reject(SceneGraphRejection::PresetTypeChanged);
            return;
        }
        if host.graph != self.expected_graph {
            self.reject(SceneGraphRejection::StaleOwner);
            return;
        }
        if self.candidate == self.before_resolved {
            self.reject(SceneGraphRejection::Noop);
            return;
        }
        if self.previous_layer.is_none() {
            self.previous_layer = Some(InstanceLayerSnapshot::capture(host));
        }
        duplicate_runtime_parameter_state(host, &self.parameter_id_remaps);
        let metadata_changed =
            self.before_resolved.preset_metadata != self.candidate.preset_metadata;
        host.graph = Some(self.candidate.clone());
        if metadata_changed {
            prune_instance_params(host, &self.removed_param_ids);
            host.refresh_manifest_from_graph();
        }
        host.bump_graph_structure_version();
        self.committed = host.graph.clone();
        self.clip_strings_before = duplicate_clip_string_overrides(project, &self.owner, &self.string_remaps);
        self.last_error = None;
        self.applied = true;
    }

    pub(crate) fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        let Some(host) = project.graph_target_owner_mut(&self.owner) else {
            self.reject(SceneGraphRejection::MissingOwner);
            return;
        };
        if host.effect_type() != &self.expected_preset_type
            || host.graph != self.committed
        {
            self.reject(SceneGraphRejection::StaleOwner);
            return;
        }
        host.graph = self.expected_graph.clone();
        if let Some(snapshot) = self.previous_layer.take() {
            snapshot.restore(host);
        }
        host.bump_graph_structure_version();
        for (clip_id, before) in self.clip_strings_before.drain(..) {
            if let Some(clip) = project.timeline.find_clip_by_id_mut(clip_id.as_str()) {
                clip.string_params = before;
            }
        }
        self.applied = false;
    }

    pub(crate) fn was_applied(&self) -> bool {
        self.applied
    }

    pub(crate) fn error(&self) -> Option<&SceneGraphRejection> {
        self.last_error.as_ref()
    }

}
#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effects::PresetInstance;
    use manifold_core::{NodeId, preset_def::PresetKind};

    fn effect_project() -> (Project, GraphTarget, EffectGraphDef) {
        let graph: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 2, "nodes": [], "wires": [],
            "presetMetadata": {
                "id": "transaction-effect", "displayName": "Transaction", "category": "Test",
                "oscPrefix": "transaction", "params": [
                    {"id":"gain", "name":"Gain", "min":0.0, "max":1.0, "defaultValue":0.4}
                ], "bindings": [], "stringParams": [], "stringBindings": []
            }
        }))
        .unwrap();
        let mut instance = PresetInstance::new(PresetTypeId::new("transaction-effect"));
        instance.graph = Some(graph.clone());
        instance.refresh_manifest_from_graph();
        instance.set_base_param("gain", 0.3);
        instance.set_param("gain", 0.7);
        let target = GraphTarget::Effect(instance.id.clone());
        let mut project = Project::default();
        project.settings.master_effects.push(instance);
        (project, target, graph)
    }

    fn rename(graph: &EffectGraphDef) -> Result<SceneGraphEdit, SceneGraphEditError> {
        let mut candidate = graph.clone();
        candidate.name = Some("Edited".into());
        Ok(SceneGraphEdit {
            graph: candidate,
            removed_param_ids: vec![],
            parameter_id_remaps: vec![],
        })
    }

    #[test]
    fn physics_boundary_transaction_atomic() {
        let (mut project, target, default) = effect_project();
        let before = project.graph_target_owner(&target).unwrap().clone();
        let failure = SceneGraphEditError {
            at: None,
            message: "candidate refused".into(),
        };
        let result =
            SceneGraphTransaction::prepare(&project, target.clone(), &default, "Rejected", |_| {
                Err(failure.clone())
            });
        assert_eq!(result.unwrap_err(), failure);
        let nested = GraphTarget::SceneModifier {
            owner: Box::new(target.clone()),
            modifier_id: NodeId::new("nested"),
        };
        assert!(
            SceneGraphTransaction::prepare(&project, nested, &default, "Unsupported", rename)
                .is_err()
        );
        let mut noop = SceneGraphTransaction::prepare(
            &project,
            target.clone(),
            &default,
            "No change",
            |graph| {
                Ok(SceneGraphEdit {
                    graph: graph.clone(),
                    removed_param_ids: vec![],
                    parameter_id_remaps: vec![],
                })
            },
        )
        .unwrap();
        noop.execute(&mut project);
        assert_eq!(noop.error(), Some(&SceneGraphRejection::Noop));
        assert!(!noop.was_applied());
        let after = project.graph_target_owner(&target).unwrap();
        assert_eq!(after.graph, before.graph);
        assert_eq!(after.params, before.params);
        assert_eq!(
            after.graph_structure_version,
            before.graph_structure_version
        );
    }

    #[test]
    fn physics_boundary_transaction_stale_owner() {
        for change_type in [false, true] {
            let (mut project, target, default) = effect_project();
            let mut edit = SceneGraphTransaction::prepare(
                &project,
                target.clone(),
                &default,
                "Rename",
                rename,
            )
            .unwrap();
            let host = project.graph_target_owner_mut(&target).unwrap();
            if change_type {
                host.set_preset_id(PresetTypeId::new("replacement"));
            } else {
                host.graph.as_mut().unwrap().description = Some("newer edit".into());
            }
            let before = host.clone();
            edit.execute(&mut project);
            assert!(!edit.was_applied());
            assert_eq!(
                edit.error(),
                Some(if change_type {
                    &SceneGraphRejection::PresetTypeChanged
                } else {
                    &SceneGraphRejection::StaleOwner
                })
            );
            let after = project.graph_target_owner(&target).unwrap();
            assert_eq!(after.graph, before.graph);
            assert_eq!(after.params, before.params);
            assert_eq!(
                after.graph_structure_version,
                before.graph_structure_version
            );
        }
        let (mut project, target, default) = effect_project();
        let mut edit =
            SceneGraphTransaction::prepare(&project, target, &default, "Rename", rename).unwrap();
        project.settings.master_effects.clear();
        edit.execute(&mut project);
        assert_eq!(edit.error(), Some(&SceneGraphRejection::MissingOwner));
        assert!(project.settings.master_effects.is_empty());
    }

    #[test]
    fn physics_boundary_transaction_undo_instance() {
        let (mut project, target, default) = effect_project();
        let before = project.graph_target_owner(&target).unwrap().clone();
        let mut edit = SceneGraphTransaction::prepare(
            &project,
            target.clone(),
            &default,
            "Remove control",
            |graph| {
                let mut candidate = graph.clone();
                candidate.preset_metadata.as_mut().unwrap().params.clear();
                Ok(SceneGraphEdit {
                    graph: candidate,
                    removed_param_ids: vec!["gain".into()],
                    parameter_id_remaps: vec![],
                })
            },
        )
        .unwrap();
        let candidate = edit.prepared_graph().clone();
        for _ in 0..2 {
            edit.execute(&mut project);
            assert!(edit.was_applied());
            let host = project.graph_target_owner(&target).unwrap();
            assert_eq!(host.kind, PresetKind::Effect);
            assert_eq!(host.graph.as_ref(), Some(&candidate));
            assert!(!host.params.contains("gain"));
            edit.undo(&mut project);
            assert!(!edit.was_applied());
            let restored = project.graph_target_owner(&target).unwrap();
            assert_eq!(restored.graph, before.graph);
            assert_eq!(restored.params, before.params);
            assert_eq!(restored.base_tracked, before.base_tracked);
        }
        edit.execute(&mut project);
        project
            .graph_target_owner_mut(&target)
            .unwrap()
            .graph
            .as_mut()
            .unwrap()
            .name = Some("later edit".into());
        let later = project.graph_target_owner(&target).unwrap().graph.clone();
        edit.undo(&mut project);
        assert_eq!(edit.error(), Some(&SceneGraphRejection::StaleOwner));
        assert_eq!(project.graph_target_owner(&target).unwrap().graph, later);
    }
}
