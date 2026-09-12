//! Undoable edits to a generator's prepared scene-modifier stack.
//!
//! The candidate is built entirely by `manifold-core`; execution here only
//! performs the project transaction and keeps the generator's other live
//! parameter state reversible.

use std::fmt;

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::project::Project;
use manifold_core::scene_modifier_edit::{
    SceneModifierEditError, SceneModifierGraphEdit, delete_scene_modifier, insert_scene_modifier,
    move_scene_modifier, retarget_scene_modifier, set_scene_modifier_preparation_param,
};
use manifold_core::scene_modifier_preset::{
    SceneMeshReferenceFrame, SceneModifierInstanceDef, SceneTargetSelection,
};
use manifold_core::{GraphTarget, NodeId, PresetTypeId};

use crate::command::Command;

use super::scene_modifier::{InstanceLayerSnapshot, prune_instance_params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneModifierStackError {
    UnsupportedOwner,
    MissingOwner,
    MissingOwnerGraph,
    GeneratorTypeChanged,
    StaleOwner,
    Noop,
    Pure(SceneModifierEditError),
}

impl fmt::Display for SceneModifierStackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedOwner => {
                write!(f, "scene modifier stack edits require a generator owner")
            }
            Self::MissingOwner => write!(f, "generator owner is no longer present"),
            Self::MissingOwnerGraph => write!(f, "generator graph does not resolve"),
            Self::GeneratorTypeChanged => write!(
                f,
                "generator type changed while preparing scene modifier edit"
            ),
            Self::StaleOwner => write!(
                f,
                "generator graph changed while preparing scene modifier edit"
            ),
            Self::Noop => write!(f, "scene modifier edit makes no change"),
            Self::Pure(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for SceneModifierStackError {}

impl From<SceneModifierEditError> for SceneModifierStackError {
    fn from(error: SceneModifierEditError) -> Self {
        Self::Pure(error)
    }
}

#[derive(Debug)]
struct StackTransaction {
    owner: GraphTarget,
    expected_graph: Option<EffectGraphDef>,
    expected_generator_type: PresetTypeId,
    before_resolved: EffectGraphDef,
    candidate: EffectGraphDef,
    removed_param_ids: Vec<String>,
    previous_layer: Option<InstanceLayerSnapshot>,
    applied: bool,
    last_error: Option<SceneModifierStackError>,
    description: &'static str,
}

impl StackTransaction {
    fn prepare<F>(
        project: &Project,
        owner: GraphTarget,
        owner_default: &EffectGraphDef,
        description: &'static str,
        edit: F,
    ) -> Result<Self, SceneModifierStackError>
    where
        F: FnOnce(&EffectGraphDef) -> Result<SceneModifierGraphEdit, SceneModifierEditError>,
    {
        if !matches!(owner, GraphTarget::Generator(_)) {
            return Err(SceneModifierStackError::UnsupportedOwner);
        }
        let host = project
            .graph_target_owner(&owner)
            .ok_or(SceneModifierStackError::MissingOwner)?;
        let before_resolved = project
            .graph_for_target(&owner, Some(owner_default))
            .ok_or(SceneModifierStackError::MissingOwnerGraph)?
            .clone();
        let result = edit(&before_resolved)?;
        Ok(Self {
            owner,
            expected_graph: host.graph.clone(),
            expected_generator_type: host.generator_type().clone(),
            before_resolved,
            candidate: result.graph,
            removed_param_ids: result.removed_param_ids,
            previous_layer: None,
            applied: false,
            last_error: None,
            description,
        })
    }

    fn prepared_graph(&self) -> &EffectGraphDef {
        &self.candidate
    }

    fn reject(&mut self, error: SceneModifierStackError) {
        self.last_error = Some(error);
        self.applied = false;
    }

    fn execute(&mut self, project: &mut Project) {
        let Some(host) = project.graph_target_owner_mut(&self.owner) else {
            self.reject(SceneModifierStackError::MissingOwner);
            return;
        };
        if host.generator_type() != &self.expected_generator_type {
            self.reject(SceneModifierStackError::GeneratorTypeChanged);
            return;
        }
        if host.graph != self.expected_graph {
            self.reject(SceneModifierStackError::StaleOwner);
            return;
        }
        if self.candidate == self.before_resolved {
            self.reject(SceneModifierStackError::Noop);
            return;
        }
        if self.previous_layer.is_none() {
            self.previous_layer = Some(InstanceLayerSnapshot::capture(host));
        }
        let metadata_changed =
            self.before_resolved.preset_metadata != self.candidate.preset_metadata;
        host.graph = Some(self.candidate.clone());
        if metadata_changed {
            prune_instance_params(host, &self.removed_param_ids);
            host.refresh_manifest_from_graph();
        }
        host.bump_graph_structure_version();
        self.last_error = None;
        self.applied = true;
    }

    fn undo(&mut self, project: &mut Project) {
        if !self.applied {
            return;
        }
        let Some(host) = project.graph_target_owner_mut(&self.owner) else {
            self.reject(SceneModifierStackError::MissingOwner);
            return;
        };
        if host.generator_type() != &self.expected_generator_type
            || host.graph.as_ref() != Some(&self.candidate)
        {
            self.reject(SceneModifierStackError::StaleOwner);
            return;
        }
        host.graph = self.expected_graph.clone();
        if let Some(snapshot) = self.previous_layer.take() {
            snapshot.restore(host);
        }
        host.bump_graph_structure_version();
        self.applied = false;
    }

    fn was_applied(&self) -> bool {
        self.applied
    }

    fn error(&self) -> Option<&SceneModifierStackError> {
        self.last_error.as_ref()
    }
}

/// Insert a prepared scene modifier into a generator graph.
#[derive(Debug)]
pub struct InsertSceneModifierCommand {
    transaction: StackTransaction,
}

impl InsertSceneModifierCommand {
    pub fn new(
        project: &Project,
        owner: GraphTarget,
        owner_default: &EffectGraphDef,
        index: usize,
        instance: SceneModifierInstanceDef,
    ) -> Result<Self, SceneModifierStackError> {
        Ok(Self {
            transaction: StackTransaction::prepare(
                project,
                owner,
                owner_default,
                "Insert Scene Modifier",
                move |graph| insert_scene_modifier(graph, index, instance),
            )?,
        })
    }

    pub fn prepared_graph(&self) -> &EffectGraphDef {
        self.transaction.prepared_graph()
    }

    pub fn error(&self) -> Option<&SceneModifierStackError> {
        self.transaction.error()
    }
}

impl Command for InsertSceneModifierCommand {
    fn execute(&mut self, project: &mut Project) {
        self.transaction.execute(project);
    }
    fn undo(&mut self, project: &mut Project) {
        self.transaction.undo(project);
    }
    fn description(&self) -> &str {
        self.transaction.description
    }
    fn was_applied(&self) -> bool {
        self.transaction.was_applied()
    }
}

/// Delete a prepared scene modifier from a generator graph.
#[derive(Debug)]
pub struct DeleteSceneModifierCommand {
    transaction: StackTransaction,
}

impl DeleteSceneModifierCommand {
    pub fn new(
        project: &Project,
        owner: GraphTarget,
        owner_default: &EffectGraphDef,
        id: NodeId,
    ) -> Result<Self, SceneModifierStackError> {
        Ok(Self {
            transaction: StackTransaction::prepare(
                project,
                owner,
                owner_default,
                "Delete Scene Modifier",
                move |graph| delete_scene_modifier(graph, &id),
            )?,
        })
    }

    pub fn prepared_graph(&self) -> &EffectGraphDef {
        self.transaction.prepared_graph()
    }
    pub fn error(&self) -> Option<&SceneModifierStackError> {
        self.transaction.error()
    }
}

impl Command for DeleteSceneModifierCommand {
    fn execute(&mut self, project: &mut Project) {
        self.transaction.execute(project);
    }
    fn undo(&mut self, project: &mut Project) {
        self.transaction.undo(project);
    }
    fn description(&self) -> &str {
        self.transaction.description
    }
    fn was_applied(&self) -> bool {
        self.transaction.was_applied()
    }
}

/// Move a prepared scene modifier to a final stack index.
#[derive(Debug)]
pub struct MoveSceneModifierCommand {
    transaction: StackTransaction,
}

impl MoveSceneModifierCommand {
    pub fn new(
        project: &Project,
        owner: GraphTarget,
        owner_default: &EffectGraphDef,
        id: NodeId,
        index: usize,
    ) -> Result<Self, SceneModifierStackError> {
        Ok(Self {
            transaction: StackTransaction::prepare(
                project,
                owner,
                owner_default,
                "Move Scene Modifier",
                move |graph| move_scene_modifier(graph, &id, index),
            )?,
        })
    }

    pub fn prepared_graph(&self) -> &EffectGraphDef {
        self.transaction.prepared_graph()
    }
    pub fn error(&self) -> Option<&SceneModifierStackError> {
        self.transaction.error()
    }
}

impl Command for MoveSceneModifierCommand {
    fn execute(&mut self, project: &mut Project) {
        self.transaction.execute(project);
    }
    fn undo(&mut self, project: &mut Project) {
        self.transaction.undo(project);
    }
    fn description(&self) -> &str {
        self.transaction.description
    }
    fn was_applied(&self) -> bool {
        self.transaction.was_applied()
    }
}

/// Retarget a prepared scene modifier using renderer-resolved calibration
/// frames. The core candidate helper preserves surviving frames exactly.
#[derive(Debug)]
pub struct RetargetSceneModifierCommand {
    transaction: StackTransaction,
}

impl RetargetSceneModifierCommand {
    pub fn new(
        project: &Project,
        owner: GraphTarget,
        owner_default: &EffectGraphDef,
        id: NodeId,
        targets: SceneTargetSelection,
        mesh_frames: Vec<SceneMeshReferenceFrame>,
    ) -> Result<Self, SceneModifierStackError> {
        Ok(Self {
            transaction: StackTransaction::prepare(
                project,
                owner,
                owner_default,
                "Retarget Scene Modifier",
                move |graph| retarget_scene_modifier(graph, &id, targets, mesh_frames),
            )?,
        })
    }

    pub fn prepared_graph(&self) -> &EffectGraphDef {
        self.transaction.prepared_graph()
    }
    pub fn error(&self) -> Option<&SceneModifierStackError> {
        self.transaction.error()
    }
}

impl Command for RetargetSceneModifierCommand {
    fn execute(&mut self, project: &mut Project) {
        self.transaction.execute(project);
    }
    fn undo(&mut self, project: &mut Project) {
        self.transaction.undo(project);
    }
    fn description(&self) -> &str {
        self.transaction.description
    }
    fn was_applied(&self) -> bool {
        self.transaction.was_applied()
    }
}

/// Set a preparation-only local modifier parameter and preserve it as one
/// undoable stack transaction. The command is prepared against the complete
/// resolved owner graph, just like structural modifier edits.
#[derive(Debug)]
pub struct SetSceneModifierPreparationParamCommand {
    transaction: StackTransaction,
}

impl SetSceneModifierPreparationParamCommand {
    pub fn new(
        project: &Project,
        owner: GraphTarget,
        owner_default: &EffectGraphDef,
        id: NodeId,
        param_id: String,
        value: f32,
    ) -> Result<Self, SceneModifierStackError> {
        Ok(Self {
            transaction: StackTransaction::prepare(
                project,
                owner,
                owner_default,
                "Set Scene Modifier Preparation Parameter",
                move |graph| set_scene_modifier_preparation_param(graph, &id, &param_id, value),
            )?,
        })
    }

    pub fn prepared_graph(&self) -> &EffectGraphDef {
        self.transaction.prepared_graph()
    }

    pub fn error(&self) -> Option<&SceneModifierStackError> {
        self.transaction.error()
    }
}

impl Command for SetSceneModifierPreparationParamCommand {
    fn execute(&mut self, project: &mut Project) {
        self.transaction.execute(project);
    }
    fn undo(&mut self, project: &mut Project) {
        self.transaction.undo(project);
    }
    fn description(&self) -> &str {
        self.transaction.description
    }
    fn was_applied(&self) -> bool {
        self.transaction.was_applied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::{BindingDef, BindingTarget};
    use manifold_core::effects::ParamConvert;
    use std::borrow::Cow;

    use manifold_core::ableton_mapping::{
        AbletonDeviceIdentity, AbletonMacroAddress, AbletonMappingStatus, AbletonParamMapping,
    };
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
    use manifold_core::effects::{
        AutomationLane, AutomationPoint, ParamEnvelope, ParameterDriver, SegmentShape,
    };
    use manifold_core::layer::Layer;
    use manifold_core::scene_modifier_preset::SceneNodeRef;
    use manifold_core::types::{BeatDivision, DriverWaveform};
    use manifold_core::units::Beats;

    fn owner_default() -> EffectGraphDef {
        serde_json::from_value(serde_json::json!({
            "version": 3,
            "presetMetadata": {
                "id": "stack-host", "displayName": "Stack Host", "category": "Geometry",
                "oscPrefix": "stack_host", "params": [], "bindings": [],
                "stringParams": [], "stringBindings": []
            },
            "nodes": [], "wires": []
        }))
        .unwrap()
    }

    fn modifier(id: &str) -> SceneModifierInstanceDef {
        let graph: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 3,
            "presetMetadata": {
                "id": format!("recipe-{id}"), "displayName": "Modifier", "category": "Geometry",
                "oscPrefix": format!("modifier_{id}"), "params": [
                    {"id":"enabled","name":"Enabled","min":0.0,"max":1.0,"defaultValue":1.0,"isToggle":true},
                    {"id":"gain","name":"Gain","min":0.0,"max":1.0,"defaultValue":0.5}
                ],
                "bindings": [], "stringParams": [], "stringBindings": [],
                "sceneModifier": {"schemaVersion":1,"singleton":false,"enabledParam":"enabled"}
            },
            "nodes": [], "wires": []
        }))
        .unwrap();
        SceneModifierInstanceDef {
            id: NodeId::new(id),
            scene: SceneNodeRef {
                scope: vec![],
                node: NodeId::new("scene"),
            },
            targets: SceneTargetSelection::AllObjects,
            mesh_frames: vec![],
            graph: Box::new(graph),
        }
    }

    fn preparation_modifier(id: &str) -> SceneModifierInstanceDef {
        let mut instance = modifier(id);
        let metadata = instance.graph.preset_metadata.as_mut().unwrap();
        metadata
            .scene_modifier
            .as_mut()
            .unwrap()
            .preparation_params
            .push("gain".into());
        metadata.bindings.push(BindingDef {
            id: "gain".into(),
            label: "Gain".into(),
            default_value: 0.5,
            target: BindingTarget::Node {
                node_id: NodeId::new("leaf"),
                param: "gain".into(),
            },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: true,
        });
        instance
    }

    fn project_with_graph(graph: Option<EffectGraphDef>) -> (Project, GraphTarget, EffectGraphDef) {
        let mut project = Project::default();
        let mut layer = Layer::new_generator("Stack Test".into(), PresetTypeId::new("stack"), 0);
        layer.gen_params_or_init().graph = graph;
        let target = GraphTarget::Generator(layer.layer_id.clone());
        project.timeline.layers.push(layer);
        (project, target, owner_default())
    }

    fn instance_state(instance: &manifold_core::effects::PresetInstance) -> String {
        format!(
            "params={:?};drivers={:?};envelopes={:?};ableton={:?};audio={:?};automation={:?}",
            instance.params,
            instance.drivers,
            instance.envelopes,
            instance.ableton_mappings,
            instance.audio_mods,
            instance.automation_lanes,
        )
    }

    fn mapping_fixture(project: &mut Project, target: &GraphTarget) {
        let host = project.graph_target_owner_mut(target).unwrap();
        let removed: manifold_core::effects::ParamId =
            Cow::Owned("sceneModifier:[\"first\",\"gain\"]".to_string());
        let survivor: manifold_core::effects::ParamId =
            Cow::Owned("sceneModifier:[\"second\",\"gain\"]".to_string());
        host.drivers = Some(vec![
            ParameterDriver {
                param_id: removed.clone(),
                beat_division: BeatDivision::Quarter,
                waveform: DriverWaveform::Sine,
                enabled: true,
                phase: 0.25,
                base_value: 0.4,
                trim_min: 0.0,
                trim_max: 1.0,
                reversed: false,
                free_period_beats: None,
                frame_aligned: false,
                legacy_param_index: None,
                is_paused_by_user: false,
            },
            ParameterDriver {
                param_id: survivor.clone(),
                beat_division: BeatDivision::Half,
                waveform: DriverWaveform::Triangle,
                enabled: false,
                phase: 0.75,
                base_value: 0.8,
                trim_min: 0.1,
                trim_max: 0.9,
                reversed: true,
                free_period_beats: None,
                frame_aligned: true,
                legacy_param_index: None,
                is_paused_by_user: false,
            },
        ]);
        host.envelopes = Some(vec![
            ParamEnvelope::new(removed.clone()),
            ParamEnvelope::new(survivor.clone()),
        ]);
        let address = AbletonMacroAddress {
            track_id: 1,
            device_id: 2,
            param_id: 3,
            device_identity: AbletonDeviceIdentity {
                device_class_name: "Rack".into(),
            },
            track_name: "Track".into(),
            device_name: "Device".into(),
            macro_name: "Macro".into(),
        };
        host.ableton_mappings = Some(vec![
            AbletonParamMapping {
                param_id: removed.clone(),
                address: address.clone(),
                range_min: 0.2,
                range_max: 0.8,
                inverted: false,
                legacy_param_index: None,
                last_value: 0.3,
                status: AbletonMappingStatus::Active,
            },
            AbletonParamMapping {
                param_id: survivor.clone(),
                address,
                range_min: 0.1,
                range_max: 0.9,
                inverted: true,
                legacy_param_index: None,
                last_value: 0.7,
                status: AbletonMappingStatus::Dormant,
            },
        ]);
        host.audio_mods = Some(vec![
            ParameterAudioMod::new(
                removed.clone(),
                manifold_core::AudioSendId::new("send"),
                AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Low),
            ),
            ParameterAudioMod::new(
                survivor.clone(),
                manifold_core::AudioSendId::new("send"),
                AudioFeature::new(AudioFeatureKind::Centroid, AudioBand::High),
            ),
        ]);
        host.automation_lanes = Some(vec![
            AutomationLane {
                param_id: removed,
                enabled: true,
                points: vec![AutomationPoint {
                    beat: Beats(1.0),
                    value: 0.25,
                    shape: SegmentShape::Linear,
                }],
            },
            AutomationLane {
                param_id: survivor,
                enabled: false,
                points: vec![AutomationPoint {
                    beat: Beats(2.0),
                    value: 0.75,
                    shape: SegmentShape::Hold,
                }],
            },
        ]);
    }

    fn assert_only_survivor(instance: &manifold_core::effects::PresetInstance) {
        let expected = "sceneModifier:[\"second\",\"gain\"]";
        assert_eq!(instance.drivers.as_ref().unwrap().len(), 1);
        assert_eq!(
            instance.drivers.as_ref().unwrap()[0].param_id.as_ref(),
            expected
        );
        assert_eq!(instance.envelopes.as_ref().unwrap().len(), 1);
        assert_eq!(
            instance.envelopes.as_ref().unwrap()[0].param_id.as_ref(),
            expected
        );
        assert_eq!(instance.ableton_mappings.as_ref().unwrap().len(), 1);
        assert_eq!(
            instance.ableton_mappings.as_ref().unwrap()[0]
                .param_id
                .as_ref(),
            expected
        );
        assert_eq!(instance.audio_mods.as_ref().unwrap().len(), 1);
        assert_eq!(
            instance.audio_mods.as_ref().unwrap()[0].param_id.as_ref(),
            expected
        );
        assert_eq!(instance.automation_lanes.as_ref().unwrap().len(), 1);
        assert_eq!(
            instance.automation_lanes.as_ref().unwrap()[0]
                .param_id
                .as_ref(),
            expected
        );
    }

    #[test]
    fn scene_modifier_stack_insert_undo_redo_restores_generator_state() {
        let (mut project, target, default) = project_with_graph(None);
        let before = project.timeline.layers[0].gen_params().unwrap().clone();
        let before_state = instance_state(&before);
        let mut command = InsertSceneModifierCommand::new(
            &project,
            target.clone(),
            &default,
            0,
            modifier("inserted"),
        )
        .unwrap();
        assert_eq!(command.prepared_graph().scene_modifiers.len(), 1);

        command.execute(&mut project);
        assert!(command.was_applied());
        assert_eq!(
            project
                .graph_for_target(&target, Some(&default))
                .unwrap()
                .scene_modifiers
                .len(),
            1
        );
        command.undo(&mut project);
        assert!(!command.was_applied());
        let restored = project.timeline.layers[0].gen_params().unwrap();
        assert_eq!(restored.graph, before.graph);
        assert_eq!(instance_state(restored), before_state);
        command.execute(&mut project);
        assert!(command.was_applied());
        assert_eq!(
            project
                .graph_for_target(&target, Some(&default))
                .unwrap()
                .scene_modifiers
                .len(),
            1
        );
    }

    #[test]
    fn scene_modifier_stack_rejects_unsupported_and_stale_owners() {
        let (mut project, target, default) = project_with_graph(None);
        let effect_target = GraphTarget::Effect(manifold_core::EffectId::new("missing"));
        let unsupported = InsertSceneModifierCommand::new(
            &project,
            effect_target,
            &default,
            0,
            modifier("unsupported"),
        )
        .unwrap_err();
        assert_eq!(unsupported, SceneModifierStackError::UnsupportedOwner);

        let mut command = InsertSceneModifierCommand::new(
            &project,
            target.clone(),
            &default,
            0,
            modifier("stale"),
        )
        .unwrap();
        project.timeline.layers[0].gen_params_or_init().graph = Some(owner_default());
        command.execute(&mut project);
        assert!(!command.was_applied());
        assert_eq!(command.error(), Some(&SceneModifierStackError::StaleOwner));
    }

    #[test]
    fn scene_modifier_stack_rejection_preserves_redo_and_dirty_version() {
        let (mut project, target, default) = project_with_graph(None);
        let mut service = crate::service::EditingService::new();
        let insert =
            InsertSceneModifierCommand::new(&project, target.clone(), &default, 0, modifier("a"))
                .unwrap();
        service.execute(Box::new(insert), &mut project);
        assert!(service.undo(&mut project));
        let version = service.data_version();
        assert!(service.can_redo());
        let stale = InsertSceneModifierCommand::new(
            &project,
            target.clone(),
            &default,
            0,
            modifier("stale"),
        )
        .unwrap();
        project.graph_target_owner_mut(&target).unwrap().graph = Some(default.clone());
        service.execute(Box::new(stale), &mut project);
        assert_eq!(service.data_version(), version);
        assert!(service.can_redo());
        assert!(!service.can_undo());
        assert_eq!(
            project.graph_target_owner(&target).unwrap().graph,
            Some(default.clone())
        );
        project.graph_target_owner_mut(&target).unwrap().graph = None;
        assert!(service.redo(&mut project));
        let version = service.data_version();
        let noop =
            MoveSceneModifierCommand::new(&project, target.clone(), &default, NodeId::new("a"), 0)
                .unwrap();
        service.execute(Box::new(noop), &mut project);
        assert_eq!(service.data_version(), version);
        assert!(service.can_undo());
        assert!(!service.can_redo());
        let mut noop =
            MoveSceneModifierCommand::new(&project, target, &default, NodeId::new("a"), 0).unwrap();
        noop.execute(&mut project);
        service.record(Box::new(noop));
        assert_eq!(service.data_version(), version);
        assert!(service.undo(&mut project));
        assert!(
            project.timeline.layers[0]
                .gen_params()
                .unwrap()
                .graph
                .is_none()
        );
    }

    #[test]
    fn scene_modifier_stack_noop_move_preserves_data() {
        let default = owner_default();
        let graph = manifold_core::scene_modifier_edit::insert_scene_modifier(
            &default,
            0,
            modifier("same"),
        )
        .unwrap()
        .graph;
        let (mut project, target, default) = project_with_graph(Some(graph));
        let before = project.timeline.layers[0].gen_params().unwrap().clone();
        let before_state = instance_state(&before);
        let mut command =
            MoveSceneModifierCommand::new(&project, target, &default, NodeId::new("same"), 0)
                .unwrap();
        command.execute(&mut project);
        assert!(!command.was_applied());
        assert_eq!(command.error(), Some(&SceneModifierStackError::Noop));
        let after = project.timeline.layers[0].gen_params().unwrap();
        assert_eq!(after.graph, before.graph);
        assert_eq!(instance_state(after), before_state);
    }

    #[test]
    fn scene_modifier_preparation_stack_execute_undo_redo_and_stale_rejection() {
        let default = owner_default();
        let graph = manifold_core::scene_modifier_edit::insert_scene_modifier(
            &default,
            0,
            preparation_modifier("prep"),
        )
        .unwrap()
        .graph;
        let (mut project, target, default) = project_with_graph(Some(graph));
        let before = project.graph_target_owner(&target).unwrap().graph.clone();
        let mut command = SetSceneModifierPreparationParamCommand::new(
            &project,
            target.clone(),
            &default,
            NodeId::new("prep"),
            "gain".into(),
            0.8,
        )
        .unwrap();
        assert_eq!(command.prepared_graph().scene_modifiers[0].graph.preset_metadata.as_ref().unwrap().params.iter().find(|p| p.id == "gain").unwrap().default_value, 0.8);
        command.execute(&mut project);
        assert!(command.was_applied());
        assert_eq!(project.graph_for_target(&target, Some(&default)).unwrap().scene_modifiers[0].graph.preset_metadata.as_ref().unwrap().params.iter().find(|p| p.id == "gain").unwrap().default_value, 0.8);
        command.undo(&mut project);
        assert!(!command.was_applied());
        assert_eq!(project.graph_target_owner(&target).unwrap().graph, before);
        command.execute(&mut project);
        assert!(command.was_applied());

        let mut stale = SetSceneModifierPreparationParamCommand::new(
            &project,
            target.clone(),
            &default,
            NodeId::new("prep"),
            "gain".into(),
            0.9,
        )
        .unwrap();
        project.graph_target_owner_mut(&target).unwrap().graph = Some(default.clone());
        stale.execute(&mut project);
        assert!(!stale.was_applied());
        assert_eq!(stale.error(), Some(&SceneModifierStackError::StaleOwner));
    }

    #[test]
    fn scene_modifier_stack_delete_and_retarget_roundtrip() {
        let default = owner_default();
        let graph = manifold_core::scene_modifier_edit::insert_scene_modifier(
            &manifold_core::scene_modifier_edit::insert_scene_modifier(
                &default,
                0,
                modifier("first"),
            )
            .unwrap()
            .graph,
            1,
            modifier("second"),
        )
        .unwrap()
        .graph;
        let (mut project, target, default) = project_with_graph(Some(graph));
        mapping_fixture(&mut project, &target);
        let before_state = instance_state(project.graph_target_owner(&target).unwrap());
        let mut delete = DeleteSceneModifierCommand::new(
            &project,
            target.clone(),
            &default,
            NodeId::new("first"),
        )
        .unwrap();
        delete.execute(&mut project);
        assert!(delete.was_applied());
        assert_only_survivor(project.graph_target_owner(&target).unwrap());
        assert_eq!(
            project
                .graph_for_target(&target, Some(&default))
                .unwrap()
                .scene_modifiers[0]
                .id,
            NodeId::new("second")
        );
        delete.undo(&mut project);
        assert!(!delete.was_applied());
        assert_eq!(
            instance_state(project.graph_target_owner(&target).unwrap()),
            before_state
        );
        assert_eq!(
            project
                .graph_for_target(&target, Some(&default))
                .unwrap()
                .scene_modifiers
                .len(),
            2
        );

        let frame = SceneMeshReferenceFrame {
            target: SceneNodeRef {
                scope: vec![],
                node: NodeId::new("object"),
            },
            source: SceneNodeRef {
                scope: vec![],
                node: NodeId::new("mesh"),
            },
            source_definition_hash: "hash".into(),
            source_offset: [1.0, 2.0, 3.0],
            scene_radius: 4.0,
        };
        let mut retarget = RetargetSceneModifierCommand::new(
            &project,
            target.clone(),
            &default,
            NodeId::new("first"),
            SceneTargetSelection::Explicit {
                objects: vec![frame.target.clone()],
            },
            vec![frame.clone()],
        )
        .unwrap();
        retarget.execute(&mut project);
        assert!(retarget.was_applied());
        assert_eq!(
            project
                .graph_for_target(&target, Some(&default))
                .unwrap()
                .scene_modifiers[0]
                .mesh_frames,
            vec![frame]
        );
        retarget.undo(&mut project);
        assert!(!retarget.was_applied());
        assert!(
            project
                .graph_for_target(&target, Some(&default))
                .unwrap()
                .scene_modifiers[0]
                .mesh_frames
                .is_empty()
        );
    }

    #[test]
    fn scene_modifier_stack_move_reorders_and_roundtrips() {
        let default = owner_default();
        let graph = manifold_core::scene_modifier_edit::insert_scene_modifier(
            &manifold_core::scene_modifier_edit::insert_scene_modifier(
                &default,
                0,
                modifier("first"),
            )
            .unwrap()
            .graph,
            1,
            modifier("second"),
        )
        .unwrap()
        .graph;
        let (mut project, target, default) = project_with_graph(Some(graph));
        let mut command = MoveSceneModifierCommand::new(
            &project,
            target.clone(),
            &default,
            NodeId::new("first"),
            1,
        )
        .unwrap();
        command.execute(&mut project);
        assert_eq!(
            project
                .graph_for_target(&target, Some(&default))
                .unwrap()
                .scene_modifiers
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
        command.undo(&mut project);
        assert_eq!(
            project
                .graph_for_target(&target, Some(&default))
                .unwrap()
                .scene_modifiers
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        command.execute(&mut project);
        assert!(command.was_applied());
        assert_eq!(
            project
                .graph_for_target(&target, Some(&default))
                .unwrap()
                .scene_modifiers
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            vec!["second", "first"]
        );
    }
}
