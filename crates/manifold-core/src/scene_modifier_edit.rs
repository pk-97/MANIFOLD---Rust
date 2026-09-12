//! Pure, transactional edits to an owner's authored scene-modifier stack.
//!
//! Renderer preparation is deliberately outside this module. These helpers
//! only maintain the serialised stack and its host-facing parameter metadata.

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::NodeId;
use crate::effect_graph_def::{BindingDef, BindingTarget, EffectGraphDef, StringBindingDef};
use crate::effects::ParamConvert;
use crate::scene_modifier_preset::{
    SceneMeshReferenceFrame, SceneModifierInstanceDef, SceneNodeRef, SceneTargetSelection,
    validate_scene_modifier_schema,
};

/// Result of one stack edit. `graph` is always a complete cloned owner;
/// runtime mapping cleanup consumes the reported parameter IDs after commit.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneModifierGraphEdit {
    pub graph: EffectGraphDef,
    pub removed_param_ids: Vec<String>,
}

/// Rejections from pure scene-modifier graph edits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneModifierEditError {
    MissingHostMetadata,
    DuplicateModifierId {
        id: String,
    },
    MissingModifier {
        id: String,
    },
    IndexOutOfRange {
        index: usize,
        len: usize,
    },
    ParameterIdCollision {
        id: String,
    },
    RetargetChangedSavedFrame {
        target: String,
    },
    InvalidSchema {
        error: crate::scene_modifier_preset::SceneModifierSchemaError,
    },
}

impl fmt::Display for SceneModifierEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingHostMetadata => write!(f, "owner has no host preset metadata"),
            Self::DuplicateModifierId { id } => {
                write!(f, "scene modifier id `{id}` is already present")
            }
            Self::MissingModifier { id } => write!(f, "scene modifier `{id}` is not present"),
            Self::IndexOutOfRange { index, len } => {
                write!(
                    f,
                    "scene modifier index {index} is out of range for length {len}"
                )
            }
            Self::ParameterIdCollision { id } => {
                write!(
                    f,
                    "scene modifier parameter id `{id}` collides with host metadata"
                )
            }
            Self::RetargetChangedSavedFrame { target } => {
                write!(f, "retarget changed the saved frame for `{target}`")
            }
            Self::InvalidSchema { error } => error.fmt(f),
        }
    }
}

impl std::error::Error for SceneModifierEditError {}

fn schema(error: crate::scene_modifier_preset::SceneModifierSchemaError) -> SceneModifierEditError {
    SceneModifierEditError::InvalidSchema { error }
}

fn macro_id(instance: &SceneModifierInstanceDef, local_id: &str) -> String {
    let tuple = serde_json::to_string(&(instance.id.as_str(), local_id))
        .expect("NodeId and str are serializable");
    format!("sceneModifier:{tuple}")
}

fn metadata_id_in_use(metadata: &crate::effect_graph_def::PresetMetadata, id: &str) -> bool {
    metadata.params.iter().any(|param| param.id == id)
        || metadata.string_params.iter().any(|param| param.id == id)
        || metadata.bindings.iter().any(|binding| binding.id == id)
        || metadata
            .string_bindings
            .iter()
            .any(|binding| binding.id == id)
}

fn local_metadata(
    instance: &SceneModifierInstanceDef,
) -> Result<&crate::effect_graph_def::PresetMetadata, SceneModifierEditError> {
    instance
        .graph
        .preset_metadata
        .as_ref()
        .ok_or_else(|| SceneModifierEditError::InvalidSchema {
            error: crate::scene_modifier_preset::SceneModifierSchemaError::InvalidBinding {
                path: instance.id.to_string(),
                detail: "modifier instance graph requires preset metadata".into(),
            },
        })
}

fn validate(graph: &EffectGraphDef) -> Result<(), SceneModifierEditError> {
    validate_scene_modifier_schema(graph).map_err(schema)
}

/// Insert an initialized modifier at `index`, minting stable host parameter
/// IDs from the modifier and local parameter IDs.
pub fn insert_scene_modifier(
    owner: &EffectGraphDef,
    index: usize,
    instance: SceneModifierInstanceDef,
) -> Result<SceneModifierGraphEdit, SceneModifierEditError> {
    if index > owner.scene_modifiers.len() {
        return Err(SceneModifierEditError::IndexOutOfRange {
            index,
            len: owner.scene_modifiers.len(),
        });
    }
    if owner
        .scene_modifiers
        .iter()
        .any(|item| item.id == instance.id)
    {
        return Err(SceneModifierEditError::DuplicateModifierId {
            id: instance.id.to_string(),
        });
    }
    validate(&instance.graph)?;
    let mut graph = owner.clone();
    graph.version = graph.version.max(3);
    let id = instance.id.clone();
    graph.scene_modifiers.insert(index, instance);
    let graph = reconcile_scene_modifier_parameters(&graph, &id)?.graph;
    validate(&graph)?;
    Ok(SceneModifierGraphEdit {
        graph,
        removed_param_ids: Vec::new(),
    })
}

/// Delete one modifier and only the host metadata owned exclusively by it.
pub fn delete_scene_modifier(
    owner: &EffectGraphDef,
    id: &NodeId,
) -> Result<SceneModifierGraphEdit, SceneModifierEditError> {
    if !owner.scene_modifiers.iter().any(|item| &item.id == id) {
        return Err(SceneModifierEditError::MissingModifier { id: id.to_string() });
    }
    let mut graph = owner.clone();
    graph.scene_modifiers.retain(|item| &item.id != id);
    let mut removed_binding_ids = HashSet::new();
    if let Some(metadata) = graph.preset_metadata.as_mut() {
        metadata.bindings.retain(|binding| {
            let remove = matches!(
                &binding.target,
                BindingTarget::SceneModifier { modifier_id, .. } if modifier_id == id
            );
            if remove {
                removed_binding_ids.insert(binding.id.clone());
            }
            !remove
        });
        metadata.string_bindings.retain(|binding| {
            let remove = matches!(
                &binding.target,
                BindingTarget::SceneModifier { modifier_id, .. } if modifier_id == id
            );
            if remove {
                removed_binding_ids.insert(binding.id.clone());
            }
            !remove
        });
        let still_used: HashSet<_> = metadata
            .bindings
            .iter()
            .map(|binding| binding.id.as_str())
            .chain(
                metadata
                    .string_bindings
                    .iter()
                    .map(|binding| binding.id.as_str()),
            )
            .collect();
        let mut removed_param_ids = Vec::new();
        metadata.params.retain(|param| {
            let remove =
                removed_binding_ids.contains(&param.id) && !still_used.contains(param.id.as_str());
            if remove && !removed_param_ids.contains(&param.id) {
                removed_param_ids.push(param.id.clone());
            }
            !remove
        });
        metadata.string_params.retain(|param| {
            let remove =
                removed_binding_ids.contains(&param.id) && !still_used.contains(param.id.as_str());
            if remove && !removed_param_ids.contains(&param.id) {
                removed_param_ids.push(param.id.clone());
            }
            !remove
        });
        validate(&graph)?;
        return Ok(SceneModifierGraphEdit {
            graph,
            removed_param_ids,
        });
    }
    validate(&graph)?;
    Ok(SceneModifierGraphEdit {
        graph,
        removed_param_ids: Vec::new(),
    })
}

/// Reconcile a locally edited recipe with its owner's public controls.
/// Existing macro addresses and fan-out are retained; only orphaned addresses
/// disappear. Graph-editor drafts may be incomplete, so this checks metadata
/// identity without requiring the draft to compile.
pub fn reconcile_scene_modifier_parameters(
    owner: &EffectGraphDef,
    id: &NodeId,
) -> Result<SceneModifierGraphEdit, SceneModifierEditError> {
    let mut matches = owner.scene_modifiers.iter().filter(|item| &item.id == id);
    let instance = matches
        .next()
        .ok_or_else(|| SceneModifierEditError::MissingModifier { id: id.to_string() })?;
    if matches.next().is_some() {
        return Err(SceneModifierEditError::DuplicateModifierId { id: id.to_string() });
    }
    let local = local_metadata(instance)?;
    let is_live = |id: &str| {
        !local
            .scene_modifier
            .as_ref()
            .is_some_and(|recipe| recipe.preparation_params.iter().any(|param| param == id))
    };
    let numeric: HashSet<_> = local
        .params
        .iter()
        .filter(|param| is_live(&param.id))
        .map(|param| param.id.as_str())
        .collect();
    let strings: HashSet<_> = local
        .string_params
        .iter()
        .map(|param| param.id.as_str())
        .collect();
    let mut graph = owner.clone();
    let metadata = graph
        .preset_metadata
        .as_mut()
        .ok_or(SceneModifierEditError::MissingHostMetadata)?;
    let mut removed_bindings = HashSet::new();
    metadata.bindings.retain(|binding| {
        let remove = matches!(&binding.target, BindingTarget::SceneModifier { modifier_id, param_id }
            if modifier_id == id && !numeric.contains(param_id.as_str()));
        if remove { removed_bindings.insert(binding.id.clone()); }
        !remove
    });
    metadata.string_bindings.retain(|binding| {
        let remove = matches!(&binding.target, BindingTarget::SceneModifier { modifier_id, param_id }
            if modifier_id == id && !strings.contains(param_id.as_str()));
        if remove { removed_bindings.insert(binding.id.clone()); }
        !remove
    });
    let used: HashSet<_> = metadata
        .bindings
        .iter()
        .map(|binding| binding.id.as_str())
        .chain(
            metadata
                .string_bindings
                .iter()
                .map(|binding| binding.id.as_str()),
        )
        .collect();
    let mut removed_param_ids = Vec::new();
    metadata.params.retain(|param| {
        let remove = removed_bindings.contains(&param.id) && !used.contains(param.id.as_str());
        if remove {
            removed_param_ids.push(param.id.clone());
        }
        !remove
    });
    metadata.string_params.retain(|param| {
        let remove = removed_bindings.contains(&param.id) && !used.contains(param.id.as_str());
        if remove && !removed_param_ids.contains(&param.id) {
            removed_param_ids.push(param.id.clone());
        }
        !remove
    });
    for param in local.params.iter().filter(|param| is_live(&param.id)) {
        if metadata.bindings.iter().any(|binding| matches!(&binding.target,
            BindingTarget::SceneModifier { modifier_id, param_id } if modifier_id == id && param_id == &param.id)) {
            continue;
        }
        let macro_id = macro_id(instance, &param.id);
        if metadata_id_in_use(metadata, &macro_id) {
            return Err(SceneModifierEditError::ParameterIdCollision { id: macro_id });
        }
        let mut outer = param.clone();
        outer.id = macro_id.clone();
        metadata.params.push(outer);
        metadata.bindings.push(BindingDef {
            id: macro_id,
            label: param.name.clone(),
            default_value: param.default_value,
            target: BindingTarget::SceneModifier {
                modifier_id: id.clone(),
                param_id: param.id.clone(),
            },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        });
    }
    for param in &local.string_params {
        if metadata.string_bindings.iter().any(|binding| matches!(&binding.target,
            BindingTarget::SceneModifier { modifier_id, param_id } if modifier_id == id && param_id == &param.id)) {
            continue;
        }
        let macro_id = macro_id(instance, &param.id);
        if metadata_id_in_use(metadata, &macro_id) {
            return Err(SceneModifierEditError::ParameterIdCollision { id: macro_id });
        }
        let mut outer = param.clone();
        outer.id = macro_id.clone();
        metadata.string_params.push(outer);
        metadata.string_bindings.push(StringBindingDef {
            id: macro_id,
            label: param.name.clone(),
            default_value: param.default_value.clone(),
            target: BindingTarget::SceneModifier {
                modifier_id: id.clone(),
                param_id: param.id.clone(),
            },
        });
    }
    Ok(SceneModifierGraphEdit {
        graph,
        removed_param_ids,
    })
}

/// Move a modifier to its final post-removal index. `index == len - 1`
/// appends to the end; the original owner remains untouched.
pub fn move_scene_modifier(
    owner: &EffectGraphDef,
    id: &NodeId,
    index: usize,
) -> Result<SceneModifierGraphEdit, SceneModifierEditError> {
    let len = owner.scene_modifiers.len();
    if index >= len {
        return Err(SceneModifierEditError::IndexOutOfRange { index, len });
    }
    let position = owner
        .scene_modifiers
        .iter()
        .position(|item| &item.id == id)
        .ok_or_else(|| SceneModifierEditError::MissingModifier { id: id.to_string() })?;
    let mut graph = owner.clone();
    let item = graph.scene_modifiers.remove(position);
    if index >= graph.scene_modifiers.len() {
        graph.scene_modifiers.push(item);
    } else {
        graph.scene_modifiers.insert(index, item);
    }
    validate(&graph)?;
    Ok(SceneModifierGraphEdit {
        graph,
        removed_param_ids: Vec::new(),
    })
}

/// Replace targets with renderer-resolved frames without recalibrating them.
/// Frames belonging to surviving targets must remain byte-for-byte identical.
pub fn retarget_scene_modifier(
    owner: &EffectGraphDef,
    id: &NodeId,
    targets: SceneTargetSelection,
    mesh_frames: Vec<SceneMeshReferenceFrame>,
) -> Result<SceneModifierGraphEdit, SceneModifierEditError> {
    let modifier = owner
        .scene_modifiers
        .iter()
        .find(|item| &item.id == id)
        .ok_or_else(|| SceneModifierEditError::MissingModifier { id: id.to_string() })?;
    let prior: HashMap<&SceneNodeRef, &SceneMeshReferenceFrame> = modifier
        .mesh_frames
        .iter()
        .map(|frame| (&frame.target, frame))
        .collect();
    for frame in &mesh_frames {
        if let Some(saved) = prior.get(&frame.target)
            && *saved != frame
        {
            return Err(SceneModifierEditError::RetargetChangedSavedFrame {
                target: format!("{:?}", frame.target),
            });
        }
    }
    let mut graph = owner.clone();
    let updated = graph
        .scene_modifiers
        .iter_mut()
        .find(|item| &item.id == id)
        .expect("modifier was found above");
    updated.targets = targets;
    updated.mesh_frames = mesh_frames;
    validate(&graph)?;
    Ok(SceneModifierGraphEdit {
        graph,
        removed_param_ids: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipe(id: &str) -> SceneModifierInstanceDef {
        let mut graph: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 3,
            "presetMetadata": {
                "id": format!("recipe-{id}"), "displayName": "Same Label",
                "category": "Geometry", "oscPrefix": "same_label",
                "params": [
                    {"id":"enabled","name":"Enabled","min":0.0,"max":1.0,"defaultValue":1.0,"isToggle":true},
                    {"id":"gain","name":"Gain","min":-2.0,"max":2.0,"defaultValue":0.3,"wholeNumbers":false,"section":"Motion"}
                ],
                "stringParams": [{"id":"asset","name":"Asset","defaultValue":"scan.glb","isFilePicker":true}],
                "bindings": [], "stringBindings": [],
                "sceneModifier": {"schemaVersion":1,"singleton":false,"enabledParam":"enabled"}
            },
            "nodes": [], "wires": []
        }))
        .unwrap();
        graph.version = 3;
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

    fn owner() -> EffectGraphDef {
        serde_json::from_value(serde_json::json!({
            "version":3,
            "presetMetadata": {
                "id":"host", "displayName":"Host", "category":"Geometry", "oscPrefix":"host",
                "params":[], "bindings":[], "stringParams":[], "stringBindings":[]
            },
            "nodes":[], "wires":[]
        }))
        .unwrap()
    }

    fn insert(owner: &EffectGraphDef, id: &str) -> EffectGraphDef {
        insert_scene_modifier(owner, owner.scene_modifiers.len(), recipe(id))
            .unwrap()
            .graph
    }

    #[test]
    fn scene_modifier_edit_insert_preserves_metadata_and_mints_injective_ids() {
        let owner = owner();
        let once = insert(&owner, "first");
        let twice = insert(&once, "second");
        assert_eq!(twice.scene_modifiers.len(), 2);
        let ids: HashSet<_> = twice
            .preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .map(|param| param.id.as_str())
            .collect();
        assert!(ids.contains("sceneModifier:[\"first\",\"gain\"]"));
        assert!(ids.contains("sceneModifier:[\"second\",\"gain\"]"));
        assert!(twice.preset_metadata.as_ref().unwrap().string_params[1].is_file_picker);
        assert_eq!(owner.scene_modifiers.len(), 0);
    }

    #[test]
    fn scene_modifier_edit_move_delete_and_shared_macro_cleanup_are_transactional() {
        let graph = insert(&insert(&insert(&owner(), "a"), "b"), "c");
        let moved = move_scene_modifier(&graph, &NodeId::new("a"), 2)
            .unwrap()
            .graph;
        assert_eq!(
            moved
                .scene_modifiers
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "c", "a"]
        );
        let mut shared = moved.clone();
        let gain_a = "sceneModifier:[\"a\",\"gain\"]";
        let gain_b = "sceneModifier:[\"b\",\"gain\"]";
        let binding_b = shared
            .preset_metadata
            .as_mut()
            .unwrap()
            .bindings
            .iter_mut()
            .find(|b| b.id == gain_b)
            .unwrap();
        binding_b.id = gain_a.into();
        binding_b.target = BindingTarget::SceneModifier {
            modifier_id: NodeId::new("b"),
            param_id: "gain".into(),
        };
        let deleted = delete_scene_modifier(&shared, &NodeId::new("a")).unwrap();
        assert!(!deleted.removed_param_ids.iter().any(|id| id == gain_a));
        assert!(
            deleted
                .graph
                .preset_metadata
                .as_ref()
                .unwrap()
                .params
                .iter()
                .any(|p| p.id == gain_a)
        );
        assert_eq!(
            delete_scene_modifier(&deleted.graph, &NodeId::new("missing")).unwrap_err(),
            SceneModifierEditError::MissingModifier {
                id: "missing".into()
            }
        );
        assert_eq!(
            move_scene_modifier(&deleted.graph, &NodeId::new("b"), 2).unwrap_err(),
            SceneModifierEditError::IndexOutOfRange { index: 2, len: 2 }
        );
    }

    #[test]
    fn scene_modifier_edit_retarget_preserves_surviving_frame_and_rejects_recalibration() {
        let mut graph = insert(&owner(), "retarget");
        let frame = SceneMeshReferenceFrame {
            target: SceneNodeRef {
                scope: vec![],
                node: NodeId::new("object_a"),
            },
            source: SceneNodeRef {
                scope: vec![],
                node: NodeId::new("mesh_a"),
            },
            source_definition_hash: "hash".into(),
            source_offset: [1.0, 2.0, 3.0],
            scene_radius: 4.0,
        };
        graph.scene_modifiers[0].mesh_frames = vec![frame.clone()];
        let retargeted = retarget_scene_modifier(
            &graph,
            &NodeId::new("retarget"),
            SceneTargetSelection::AllObjects,
            vec![frame.clone()],
        )
        .unwrap();
        assert_eq!(
            retargeted.graph.scene_modifiers[0].mesh_frames,
            vec![frame.clone()]
        );
        let mut changed = frame;
        changed.source_offset[0] = 99.0;
        assert!(matches!(
            retarget_scene_modifier(
                &graph,
                &NodeId::new("retarget"),
                SceneTargetSelection::AllObjects,
                vec![changed]
            ),
            Err(SceneModifierEditError::RetargetChangedSavedFrame { .. })
        ));
        assert_eq!(
            graph.scene_modifiers[0].mesh_frames[0].source_offset[0],
            1.0
        );
    }

    #[test]
    fn scene_modifier_edit_rejects_duplicate_and_out_of_range_without_mutating_owner() {
        let graph = insert(&owner(), "same");
        let snapshot = graph.clone();
        assert!(matches!(
            insert_scene_modifier(&graph, 0, recipe("same")),
            Err(SceneModifierEditError::DuplicateModifierId { .. })
        ));
        assert!(matches!(
            insert_scene_modifier(&graph, 3, recipe("new")),
            Err(SceneModifierEditError::IndexOutOfRange { .. })
        ));
        assert_eq!(graph, snapshot);
    }

    #[test]
    fn scene_modifier_edit_reconcile_retains_addresses_and_omits_preparation_controls() {
        let mut local = recipe("a");
        local
            .graph
            .preset_metadata
            .as_mut()
            .unwrap()
            .scene_modifier
            .as_mut()
            .unwrap()
            .preparation_params
            .push("gain".into());
        let graph = insert_scene_modifier(&owner(), 0, local).unwrap().graph;
        assert!(
            !graph
                .preset_metadata
                .as_ref()
                .unwrap()
                .params
                .iter()
                .any(|p| p.name == "Gain")
        );
        let mut edited = graph.clone();
        let local = edited.scene_modifiers[0]
            .graph
            .preset_metadata
            .as_mut()
            .unwrap();
        local
            .scene_modifier
            .as_mut()
            .unwrap()
            .preparation_params
            .clear();
        local
            .params
            .iter_mut()
            .find(|p| p.id == "gain")
            .unwrap()
            .is_trigger_gate = true;
        let reconciled = reconcile_scene_modifier_parameters(&edited, &NodeId::new("a")).unwrap();
        let metadata = reconciled.graph.preset_metadata.as_ref().unwrap();
        assert!(
            metadata
                .params
                .iter()
                .find(|p| p.name == "Gain")
                .unwrap()
                .is_trigger_gate
        );
        assert_eq!(
            metadata.params[0],
            graph.preset_metadata.as_ref().unwrap().params[0]
        );
        let mut deleted_local_param = reconciled.graph.clone();
        deleted_local_param.scene_modifiers[0]
            .graph
            .preset_metadata
            .as_mut()
            .unwrap()
            .params
            .retain(|p| p.id != "gain");
        let removed =
            reconcile_scene_modifier_parameters(&deleted_local_param, &NodeId::new("a")).unwrap();
        assert_eq!(
            removed.removed_param_ids,
            vec!["sceneModifier:[\"a\",\"gain\"]"]
        );
        assert_eq!(removed.graph.preset_metadata, graph.preset_metadata);
    }
}
