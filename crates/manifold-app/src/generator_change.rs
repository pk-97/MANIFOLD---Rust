//! Transactional generator replacement.
//!
//! Generator replacement is prepared against a cloned project so that graph
//! compatibility errors leave the live project untouched.  The command then
//! swaps the complete generator instance, which also makes undo restore all
//! modulation and runtime authored state in one operation.

use manifold_core::effects::PresetInstance;
use manifold_core::project::Project;
use manifold_core::{GraphTarget, LayerId, NodeId, PresetTypeId};
use manifold_editing::command::Command;
use std::collections::HashSet;

/// Prepare a generator type change, preserving every compatible scene
/// modifier in the destination graph.
pub(crate) fn build_change(
    project: &Project,
    layer_id: LayerId,
    new_type: PresetTypeId,
) -> Result<Box<dyn Command>, String> {
    let target = GraphTarget::Generator(layer_id.clone());
    let source_host = project
        .graph_target_owner(&target)
        .ok_or_else(|| "Generator layer is no longer available".to_string())?;
    let before = source_host.clone();
    let source_graph = crate::graph_target::resolve(project, &target);
    let selected: Vec<NodeId> = source_graph
        .as_ref()
        .map(|graph| {
            graph
                .scene_modifiers
                .iter()
                .map(|modifier| modifier.id.clone())
                .collect()
        })
        .unwrap_or_default();

    let mut candidate = project.clone();
    {
        let (_, candidate_layer) = candidate
            .timeline
            .find_layer_by_id_mut(&layer_id)
            .ok_or_else(|| "Generator layer is no longer available".to_string())?;
        candidate_layer.change_generator_type(new_type);
        if let Some(host) = candidate_layer.gen_params_mut() {
            let valid: HashSet<String> = host
                .params
                .iter()
                .map(|param| param.id().to_string())
                .collect();
            if let Some(mappings) = &mut host.ableton_mappings {
                mappings.retain(|mapping| valid.contains(mapping.param_id.as_ref()));
            }
            if let Some(mods) = &mut host.audio_mods {
                mods.retain(|modulation| valid.contains(modulation.param_id.as_ref()));
            }
            if let Some(automation) = &mut host.automation_lanes {
                automation.retain(|lane| valid.contains(lane.param_id.as_ref()));
            }
        }
    }
    let after = {
        let destination_graph = if selected.is_empty() {
            None
        } else {
            Some(
                crate::graph_target::resolve(&candidate, &target)
                    .ok_or_else(|| {
                        "Destination generator graph is no longer available".to_string()
                    })?
                    .clone(),
            )
        };
        let candidate_host = candidate
            .timeline
            .find_layer_by_id_mut(&layer_id)
            .and_then(|(_, layer)| layer.gen_params_mut())
            .ok_or_else(|| "Destination generator is no longer available".to_string())?;
        if !selected.is_empty() {
            let source_graph = source_graph
                .as_ref()
                .expect("selected modifiers require a source graph");
            crate::scene_modifier_transfer::transfer(
                source_host,
                source_graph,
                candidate_host,
                destination_graph
                    .as_ref()
                    .expect("selected modifiers have a destination graph"),
                &selected,
                true,
            )?;
        }
        candidate_host.clone()
    };

    Ok(Box::new(ReplaceGeneratorStateCommand::new(
        layer_id,
        before,
        after,
        "Change Generator Type",
    )))
}

/// Reusable full-state replacement used by generator changes and generator
/// paste.  The snapshots include the graph, parameter manifest, drivers,
/// envelopes, mappings, automation and audio modulation state.
#[derive(Debug)]
pub(crate) struct ReplaceGeneratorStateCommand {
    layer_id: LayerId,
    before: PresetInstance,
    after: PresetInstance,
    description: &'static str,
    applied: bool,
    rejection: Option<String>,
}

impl ReplaceGeneratorStateCommand {
    pub(crate) fn new(
        layer_id: LayerId,
        before: PresetInstance,
        after: PresetInstance,
        description: &'static str,
    ) -> Self {
        Self {
            layer_id,
            before,
            after,
            description,
            applied: false,
            rejection: None,
        }
    }

    fn current<'a>(&self, project: &'a Project) -> Option<&'a PresetInstance> {
        project
            .timeline
            .find_layer_by_id(&self.layer_id)
            .and_then(|(_, layer)| layer.gen_params())
    }

    fn replace(&self, project: &mut Project, state: PresetInstance) -> Result<(), String> {
        let (_, layer) = project
            .timeline
            .find_layer_by_id_mut(&self.layer_id)
            .ok_or_else(|| "Generator layer is no longer available".to_string())?;
        let destination = layer
            .gen_params_mut()
            .ok_or_else(|| "Generator is no longer available".to_string())?;
        let next_graph_version = destination.graph_version.wrapping_add(1);
        let next_structure_version = destination.graph_structure_version.wrapping_add(1);
        *destination = state;
        // The snapshot's counters describe the prepared state.  Bump them so
        // the renderer rehydrates even when the replacement has equal values.
        destination.graph_version = next_graph_version;
        destination.graph_structure_version = next_structure_version;
        Ok(())
    }

    fn same_authored_state(a: &PresetInstance, b: &PresetInstance) -> bool {
        // PresetInstance deliberately omits cache/version counters from its
        // wire representation.  Comparing the complete wire state therefore
        // permits renderer/admission version bumps while still guarding every
        // persisted parameter, modulation, graph and selection field.
        match (serde_json::to_vec(a), serde_json::to_vec(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    fn reject(&mut self, reason: impl Into<String>) {
        self.applied = false;
        self.rejection = Some(reason.into());
    }
}

impl Command for ReplaceGeneratorStateCommand {
    fn execute(&mut self, project: &mut Project) {
        self.rejection = None;
        let expected = if self.applied {
            &self.after
        } else {
            &self.before
        };
        let Some(current) = self.current(project) else {
            self.reject("Generator layer is no longer available");
            return;
        };
        if !Self::same_authored_state(current, expected) {
            self.reject("Generator changed while preparing this edit");
            return;
        }
        if let Err(error) = self.replace(project, self.after.clone()) {
            self.reject(error);
            return;
        }
        self.applied = true;
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        if let Err(error) = self.replace(project, self.before.clone()) {
            self.reject(error);
            return;
        }
        self.applied = false;
        self.rejection = None;
    }

    fn description(&self) -> &str {
        self.description
    }

    fn graph_admission_targets(&self, targets: &mut Vec<GraphTarget>) {
        targets.push(GraphTarget::Generator(self.layer_id.clone()));
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection.as_deref()
    }

    fn was_applied(&self) -> bool {
        self.applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::layer::Layer;

    fn project_with_generator() -> (Project, LayerId) {
        let layer = Layer::new_generator("Generator".into(), PresetTypeId::new("BasicShapes"), 0);
        let id = layer.layer_id.clone();
        let mut project = Project::default();
        project.timeline.layers.push(layer);
        (project, id)
    }

    #[test]
    fn replacement_restores_full_snapshot_on_undo_and_redo() {
        let (mut project, layer_id) = project_with_generator();
        let before = project
            .timeline
            .find_layer_by_id(&layer_id)
            .unwrap()
            .1
            .gen_params()
            .unwrap()
            .clone();
        let mut after = PresetInstance::new_generator(PresetTypeId::new("Plasma"));
        after.graph = before.graph.clone();
        let expected_after = after.clone();
        let mut command = ReplaceGeneratorStateCommand::new(
            layer_id.clone(),
            before.clone(),
            after,
            "Test generator replacement",
        );

        command.execute(&mut project);
        assert!(command.was_applied());
        assert_eq!(
            project
                .timeline
                .find_layer_by_id(&layer_id)
                .unwrap()
                .1
                .gen_params()
                .unwrap()
                .generator_type(),
            expected_after.generator_type()
        );
        command.undo(&mut project);
        assert!(ReplaceGeneratorStateCommand::same_authored_state(
            project
                .timeline
                .find_layer_by_id(&layer_id)
                .unwrap()
                .1
                .gen_params()
                .unwrap(),
            &before
        ));
        command.execute(&mut project);
        assert!(command.was_applied());
        assert!(ReplaceGeneratorStateCommand::same_authored_state(
            project
                .timeline
                .find_layer_by_id(&layer_id)
                .unwrap()
                .1
                .gen_params()
                .unwrap(),
            &expected_after
        ));
    }

    #[test]
    fn replacement_rejects_stale_generator_without_mutating_it() {
        let (mut project, layer_id) = project_with_generator();
        let before = project
            .timeline
            .find_layer_by_id(&layer_id)
            .unwrap()
            .1
            .gen_params()
            .unwrap()
            .clone();
        let after = PresetInstance::new_generator(PresetTypeId::new("Plasma"));
        let mut command = ReplaceGeneratorStateCommand::new(
            layer_id.clone(),
            before,
            after,
            "Test stale replacement",
        );
        project
            .timeline
            .find_layer_by_id_mut(&layer_id)
            .unwrap()
            .1
            .gen_params_mut()
            .unwrap()
            .set_preset_id(PresetTypeId::new("ChangedElsewhere"));

        command.execute(&mut project);
        assert!(!command.was_applied());
        assert_eq!(
            command.rejection_reason(),
            Some("Generator changed while preparing this edit")
        );
        assert_eq!(
            project
                .timeline
                .find_layer_by_id(&layer_id)
                .unwrap()
                .1
                .gen_params()
                .unwrap()
                .generator_type(),
            &PresetTypeId::new("ChangedElsewhere")
        );
    }
}
