use super::ChangeGraphParamCommand;
use crate::command::Command;
use manifold_core::GraphTarget;
use manifold_core::effects::ParamId;
use manifold_core::project::Project;

const TARGET_UNAVAILABLE: &str = "fire parameter target is unavailable";
const PARAM_UNAVAILABLE: &str = "fire parameter does not exist";
const NOT_TRIGGER: &str = "parameter is not a trigger";
const COUNTER_EXHAUSTED: &str = "trigger counter is exhausted";

/// Increment a trigger parameter on the content project.
///
/// The first execution resolves the current base value and captures the
/// resulting `ChangeGraphParamCommand`. This keeps multiple clicks prepared
/// from one stale UI snapshot sequential while making redo deterministic.
#[derive(Debug)]
pub struct FireGraphParamCommand {
    target: GraphTarget,
    param_id: ParamId,
    resolved: Option<ChangeGraphParamCommand>,
    applied: bool,
    rejection: Option<&'static str>,
}

impl FireGraphParamCommand {
    pub fn new(target: GraphTarget, param_id: impl Into<ParamId>) -> Self {
        Self {
            target,
            param_id: param_id.into(),
            resolved: None,
            applied: false,
            rejection: None,
        }
    }

    fn validate_target(
        project: &Project,
        target: &GraphTarget,
        param_id: &str,
    ) -> Result<f32, &'static str> {
        if !matches!(target, GraphTarget::SceneModifier { .. })
            && let Some(graph) = project.graph_for_target(target, None)
            && let Some(reason) =
                manifold_core::scene_modifier_preset::scene_modifier_macro_lock_reason(
                    graph, param_id,
                )
        {
            return Err(reason);
        }

        let Some(instance) = project.preset_instance(target) else {
            return Err(TARGET_UNAVAILABLE);
        };
        let Some(param) = instance.params.get(param_id) else {
            return Err(PARAM_UNAVAILABLE);
        };
        if !param.spec.is_trigger {
            return Err(NOT_TRIGGER);
        }
        Ok(instance.get_base_param(param_id))
    }

    fn next_value(value: f32) -> Result<f32, &'static str> {
        let next = value + 1.0;
        if !value.is_finite() || !next.is_finite() || next <= value || next - value != 1.0 {
            return Err(COUNTER_EXHAUSTED);
        }
        Ok(next)
    }
}

impl Command for FireGraphParamCommand {
    fn execute(&mut self, project: &mut Project) {
        self.applied = false;
        self.rejection = None;

        if let Some(command) = self.resolved.as_mut() {
            if let Err(reason) =
                Self::validate_target(project, &self.target, self.param_id.as_ref())
            {
                self.rejection = Some(reason);
                return;
            }
            command.execute(project);
            if command.was_applied() {
                self.applied = true;
            } else {
                self.rejection = command.rejection;
            }
            return;
        }

        let before = match Self::validate_target(project, &self.target, self.param_id.as_ref()) {
            Ok(value) => value,
            Err(reason) => {
                self.rejection = Some(reason);
                return;
            }
        };
        let after = match Self::next_value(before) {
            Ok(value) => value,
            Err(reason) => {
                self.rejection = Some(reason);
                return;
            }
        };

        let mut command =
            ChangeGraphParamCommand::new(self.target.clone(), self.param_id.clone(), before, after);
        command.execute(project);
        if !command.was_applied() {
            self.rejection = command.rejection;
            return;
        }
        self.resolved = Some(command);
        self.applied = true;
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        if let Some(command) = self.resolved.as_mut() {
            command.undo(project);
            self.applied = false;
        }
    }

    fn description(&self) -> &str {
        "Fire Param"
    }

    fn rejection_reason(&self) -> Option<&str> {
        self.rejection
    }

    fn was_applied(&self) -> bool {
        self.applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::EditingService;
    use manifold_core::effect_graph_def::ParamSpecDef;
    use manifold_core::effects::PresetInstance;
    use manifold_core::params::{Param, ParamManifest};
    use manifold_core::{EffectId, PresetTypeId};

    fn fixture(value: f32, is_trigger: bool) -> (Project, GraphTarget, ParamId) {
        let mut project = Project::default();
        let mut instance = PresetInstance::new(PresetTypeId::BLOOM);
        let target = GraphTarget::Effect(instance.id.clone());
        let param_id = ParamId::Owned("fire".to_string());
        instance.params = ParamManifest::from_params(vec![Param::bundled(ParamSpecDef {
            id: param_id.to_string(),
            name: "Fire".to_string(),
            default_value: value,
            min: f32::MIN,
            max: f32::MAX,
            is_trigger,
            ..ParamSpecDef::default()
        })]);
        instance.base_tracked = true;
        project.settings.master_effects.push(instance);
        (project, target, param_id)
    }

    fn value(project: &Project, target: &GraphTarget, param_id: &ParamId) -> f32 {
        project
            .preset_instance(target)
            .expect("fixture target")
            .get_base_param(param_id.as_ref())
    }

    #[test]
    fn stale_commands_resolve_sequentially_and_round_trip_through_service() {
        let (mut project, target, param_id) = fixture(10.0, true);
        let mut service = EditingService::new();
        service.execute(
            Box::new(FireGraphParamCommand::new(target.clone(), param_id.clone())),
            &mut project,
        );
        service.execute(
            Box::new(FireGraphParamCommand::new(target.clone(), param_id.clone())),
            &mut project,
        );
        assert_eq!(value(&project, &target, &param_id), 12.0);
        assert!(service.undo(&mut project));
        assert_eq!(value(&project, &target, &param_id), 11.0);
        assert!(service.undo(&mut project));
        assert_eq!(value(&project, &target, &param_id), 10.0);
        assert!(service.redo(&mut project));
        assert_eq!(value(&project, &target, &param_id), 11.0);
        assert!(service.redo(&mut project));
        assert_eq!(value(&project, &target, &param_id), 12.0);
    }

    #[test]
    fn redo_restores_the_captured_increment_after_an_external_value_change() {
        let (mut project, target, param_id) = fixture(10.0, true);
        let mut command = FireGraphParamCommand::new(target.clone(), param_id.clone());
        command.execute(&mut project);
        command.undo(&mut project);
        project
            .preset_instance_mut(&target)
            .unwrap()
            .set_base_param(param_id.as_ref(), 30.0);
        command.execute(&mut project);
        assert!(command.was_applied());
        assert_eq!(value(&project, &target, &param_id), 11.0);
        command.undo(&mut project);
        assert_eq!(value(&project, &target, &param_id), 10.0);
    }

    #[test]
    fn rejects_non_trigger_without_mutating_project() {
        let (mut project, target, param_id) = fixture(10.0, false);
        let before = serde_json::to_value(&project).unwrap();
        let mut command = FireGraphParamCommand::new(target, param_id);
        command.execute(&mut project);
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
        assert!(!command.was_applied());
        assert_eq!(command.rejection_reason(), Some(NOT_TRIGGER));
    }

    #[test]
    fn rejects_exhausted_counter_without_mutating_project() {
        let (mut project, target, param_id) = fixture(f32::MAX, true);
        let before = serde_json::to_value(&project).unwrap();
        let mut command = FireGraphParamCommand::new(target, param_id);
        command.execute(&mut project);
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
        assert!(!command.was_applied());
        assert_eq!(command.rejection_reason(), Some(COUNTER_EXHAUSTED));
    }

    #[test]
    fn rejects_counter_when_float_would_skip_two_events() {
        let (mut project, target, param_id) = fixture(16_777_218.0, true);
        let before = serde_json::to_value(&project).unwrap();
        let mut command = FireGraphParamCommand::new(target, param_id);
        command.execute(&mut project);
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
        assert!(!command.was_applied());
        assert_eq!(command.rejection_reason(), Some(COUNTER_EXHAUSTED));
    }

    #[test]
    fn rejects_nonfinite_counter_without_mutating_project() {
        let (mut project, target, param_id) = fixture(f32::NAN, true);
        let mut command = FireGraphParamCommand::new(target.clone(), param_id.clone());
        command.execute(&mut project);
        assert!(value(&project, &target, &param_id).is_nan());
        assert!(!command.was_applied());
        assert_eq!(command.rejection_reason(), Some(COUNTER_EXHAUSTED));
    }

    #[test]
    fn rejects_missing_parameter_without_mutating_project() {
        let (mut project, target, _) = fixture(10.0, true);
        let before = serde_json::to_value(&project).unwrap();
        let mut command = FireGraphParamCommand::new(target, ParamId::Owned("missing".to_string()));
        command.execute(&mut project);
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
        assert!(!command.was_applied());
        assert_eq!(command.rejection_reason(), Some(PARAM_UNAVAILABLE));
    }

    #[test]
    fn rejects_missing_target_without_mutating_project() {
        let (mut project, _, param_id) = fixture(10.0, true);
        let before = serde_json::to_value(&project).unwrap();
        let target = GraphTarget::Effect(EffectId::new("missing"));
        let mut command = FireGraphParamCommand::new(target, param_id);
        command.execute(&mut project);
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
        assert!(!command.was_applied());
        assert_eq!(command.rejection_reason(), Some(TARGET_UNAVAILABLE));
    }

    #[test]
    fn calibrated_source_rejection_preserves_project_version_and_redo() {
        let (mut project, target, param_id) = fixture(0.0, true);
        let mut service = EditingService::new();
        service.execute(
            Box::new(FireGraphParamCommand::new(target.clone(), param_id.clone())),
            &mut project,
        );
        assert!(service.undo(&mut project));
        let graph = serde_json::from_value(serde_json::json!({
            "version": 3,
            "nodes": [{"id": 1, "nodeId": "source", "typeId": "node.gltf_mesh_source"}],
            "wires": [],
            "presetMetadata": {
                "id": "FireFixture", "displayName": "Fire", "category": "Test", "oscPrefix": "fire",
                "params": [],
                "bindings": [{
                    "id": "fire", "label": "Fire", "defaultValue": 0.0,
                    "target": {"kind": "node", "nodeId": "source", "param": "reload"}
                }]
            },
            "sceneModifiers": [{
                "id": "modifier", "scene": {"node": "scene"}, "targets": "allObjects",
                "meshFrames": [{
                    "target": {"node": "object"}, "source": {"node": "source"},
                    "sourceDefinitionHash": "hash", "sourceOffset": [0.0, 0.0, 0.0],
                    "sceneRadius": 1.0
                }],
                "graph": {"version": 3, "nodes": [], "wires": []}
            }]
        }))
        .unwrap();
        project.preset_instance_mut(&target).unwrap().graph = Some(graph);
        let before = serde_json::to_value(&project).unwrap();
        let version = service.data_version();
        service.execute(
            Box::new(FireGraphParamCommand::new(target, param_id)),
            &mut project,
        );
        assert!(service.take_rejection().unwrap().contains("locked"));
        assert_eq!(serde_json::to_value(&project).unwrap(), before);
        assert_eq!(service.data_version(), version);
        assert!(!service.can_undo());
        assert!(service.can_redo());
    }
}
