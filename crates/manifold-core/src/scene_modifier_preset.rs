//! Serializable scene-modifier recipes and their schema-level validation.
//!
//! This module deliberately contains no renderer or GPU types.  Expansion is
//! owned by the renderer; these values are the authored snapshot and recipe
//! contract that expansion consumes.

use std::collections::{BTreeSet, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::effect_graph_def::{BindingTarget, EffectGraphDef, ParamSpecDef};
use crate::id::NodeId;

/// Graph version required by a definition that carries `sceneModifiers`.
pub use crate::effect_graph_def::EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS;

/// Version of the scene-modifier recipe ABI.
pub const SCENE_MODIFIER_RECIPE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneNodeRef {
    #[serde(default)]
    pub scope: Vec<NodeId>,
    pub node: NodeId,
}

impl Ord for SceneNodeRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.scope
            .iter()
            .map(NodeId::as_str)
            .cmp(other.scope.iter().map(NodeId::as_str))
            .then_with(|| self.node.as_str().cmp(other.node.as_str()))
    }
}

impl PartialOrd for SceneNodeRef {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SceneTargetSelection {
    AllObjects,
    Explicit {
        #[serde(default)]
        objects: Vec<SceneNodeRef>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneModifierInstanceDef {
    pub id: NodeId,
    pub scene: SceneNodeRef,
    pub targets: SceneTargetSelection,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mesh_frames: Vec<SceneMeshReferenceFrame>,
    pub graph: Box<EffectGraphDef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneMeshReferenceFrame {
    pub target: SceneNodeRef,
    pub source: SceneNodeRef,
    pub source_definition_hash: String,
    pub source_offset: [f64; 3],
    pub scene_radius: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneModifierRecipe {
    pub schema_version: u32,
    pub singleton: bool,
    pub enabled_param: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preparation_params: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<SceneModifierStageDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub initializers: Vec<SceneNodeInitializer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calibrations: Vec<SceneParamCalibration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SceneAxis {
    X,
    Y,
    Z,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SceneScalarExpr {
    Constant {
        value: f64,
    },
    BoundsMin {
        axis: SceneAxis,
    },
    BoundsMax {
        axis: SceneAxis,
    },
    Add {
        a: Box<SceneScalarExpr>,
        b: Box<SceneScalarExpr>,
    },
    Subtract {
        a: Box<SceneScalarExpr>,
        b: Box<SceneScalarExpr>,
    },
    Multiply {
        a: Box<SceneScalarExpr>,
        b: Box<SceneScalarExpr>,
    },
    Max {
        a: Box<SceneScalarExpr>,
        b: Box<SceneScalarExpr>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneNodeInitializer {
    pub target: SceneNodeRef,
    pub param: String,
    pub value: SceneScalarExpr,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneParamCalibration {
    pub param_id: String,
    pub min: SceneScalarExpr,
    pub max: SceneScalarExpr,
    pub default_value: SceneScalarExpr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SceneStageScope {
    Scene,
    EachObject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SceneEndpoint {
    Camera,
    Atmosphere,
    Transform,
    Instances,
    Vertices,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SceneContextValue {
    Beat,
    Time,
    TriggerCount,
    ObjectOrdinal,
    ObjectCount,
    ObjectSeed,
    SceneMin,
    SceneMax,
    SceneRadius,
    SourceOffsetX,
    SourceOffsetY,
    SourceOffsetZ,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SceneStageSource {
    Previous { endpoint: SceneEndpoint },
    Reference { endpoint: SceneEndpoint },
    Context { value: SceneContextValue },
    StageOutput { stage: NodeId, port: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneStageInput {
    pub port: String,
    pub source: SceneStageSource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneStageOutput {
    pub port: String,
    pub endpoint: SceneEndpoint,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneModifierStageDef {
    pub group: NodeId,
    pub scope: SceneStageScope,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<SceneStageInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<SceneStageOutput>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneModifierSchemaError {
    UnsupportedVersion { path: String, detail: String },
    MissingTarget { path: String, detail: String },
    DuplicateIdentity { path: String, detail: String },
    InvalidRecipe { path: String, detail: String },
    UnsupportedCoordinateFrame { path: String, detail: String },
    RecursiveModifier { path: String, detail: String },
    InvalidBinding { path: String, detail: String },
    CapacityExceeded { path: String, detail: String },
}

impl SceneModifierSchemaError {
    pub fn path(&self) -> &str {
        match self {
            Self::UnsupportedVersion { path, .. }
            | Self::MissingTarget { path, .. }
            | Self::DuplicateIdentity { path, .. }
            | Self::InvalidRecipe { path, .. }
            | Self::UnsupportedCoordinateFrame { path, .. }
            | Self::RecursiveModifier { path, .. }
            | Self::InvalidBinding { path, .. }
            | Self::CapacityExceeded { path, .. } => path,
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Self::UnsupportedVersion { detail, .. }
            | Self::MissingTarget { detail, .. }
            | Self::DuplicateIdentity { detail, .. }
            | Self::InvalidRecipe { detail, .. }
            | Self::UnsupportedCoordinateFrame { detail, .. }
            | Self::RecursiveModifier { detail, .. }
            | Self::InvalidBinding { detail, .. }
            | Self::CapacityExceeded { detail, .. } => detail,
        }
    }
}

impl fmt::Display for SceneModifierSchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path(), self.detail())
    }
}

impl std::error::Error for SceneModifierSchemaError {}

fn check_ref(path: &str, reference: &SceneNodeRef) -> Result<(), SceneModifierSchemaError> {
    if reference.node.is_empty() {
        return Err(SceneModifierSchemaError::MissingTarget {
            path: path.into(),
            detail: "node reference is empty".into(),
        });
    }
    if reference.scope.iter().any(NodeId::is_empty) {
        return Err(SceneModifierSchemaError::MissingTarget {
            path: path.into(),
            detail: "scope contains an empty node id".into(),
        });
    }
    Ok(())
}

fn validate_expr(path: &str, expr: &SceneScalarExpr) -> Result<(), SceneModifierSchemaError> {
    fn visit(
        path: &str,
        expr: &SceneScalarExpr,
        depth: usize,
        nodes: &mut usize,
    ) -> Result<(), SceneModifierSchemaError> {
        if depth > 16 {
            return Err(SceneModifierSchemaError::CapacityExceeded {
                path: path.into(),
                detail: "expression exceeds depth 16".into(),
            });
        }
        *nodes += 1;
        if *nodes > 64 {
            return Err(SceneModifierSchemaError::CapacityExceeded {
                path: path.into(),
                detail: "expression exceeds 64 nodes".into(),
            });
        }
        match expr {
            SceneScalarExpr::Constant { value } if !value.is_finite() => {
                Err(SceneModifierSchemaError::InvalidRecipe {
                    path: path.into(),
                    detail: "constant must be finite".into(),
                })
            }
            SceneScalarExpr::Add { a, b }
            | SceneScalarExpr::Subtract { a, b }
            | SceneScalarExpr::Multiply { a, b }
            | SceneScalarExpr::Max { a, b } => {
                visit(path, a, depth + 1, nodes)?;
                visit(path, b, depth + 1, nodes)
            }
            _ => Ok(()),
        }
    }

    let mut nodes = 0;
    visit(path, expr, 1, &mut nodes)
}

fn find_param<'a>(params: &'a [ParamSpecDef], id: &str) -> Option<&'a ParamSpecDef> {
    params.iter().find(|param| param.id == id)
}

/// Validate schema-level scene modifier invariants. Resource admission,
/// primitive ports and endpoint applicability are renderer responsibilities.
pub fn has_scene_modifier_data(def: &EffectGraphDef) -> bool {
    !def.scene_modifiers.is_empty()
        || def.preset_metadata.as_ref().is_some_and(|meta| {
            meta.scene_modifier.is_some()
                || meta
                    .bindings
                    .iter()
                    .any(|b| matches!(b.target, BindingTarget::SceneModifier { .. }))
                || meta
                    .string_bindings
                    .iter()
                    .any(|b| matches!(b.target, BindingTarget::SceneModifier { .. }))
        })
}

pub fn validate_scene_modifier_schema(
    def: &EffectGraphDef,
) -> Result<(), SceneModifierSchemaError> {
    validate_def(def, "graph", false)
}

fn validate_def(
    def: &EffectGraphDef,
    path: &str,
    recipe_required: bool,
) -> Result<(), SceneModifierSchemaError> {
    if !(1..=EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS).contains(&def.version) {
        return Err(SceneModifierSchemaError::UnsupportedVersion {
            path: format!("{path}.version"),
            detail: format!(
                "graph version must be between 1 and {EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS}"
            ),
        });
    }
    let metadata = def.preset_metadata.as_ref();
    let recipe = metadata.and_then(|meta| meta.scene_modifier.as_ref());
    if recipe.is_some() && !def.scene_modifiers.is_empty() {
        return Err(SceneModifierSchemaError::RecursiveModifier {
            path: format!("{path}.sceneModifiers"),
            detail: "a graph cannot carry both an instance stack and a modifier recipe".into(),
        });
    }
    if recipe_required && !def.scene_modifiers.is_empty() {
        return Err(SceneModifierSchemaError::RecursiveModifier {
            path: format!("{path}.sceneModifiers"),
            detail: "nested modifier stacks are not supported in recipe version 1".into(),
        });
    }
    if has_scene_modifier_data(def) && def.version < EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS {
        return Err(SceneModifierSchemaError::UnsupportedVersion {
            path: format!("{path}.version"),
            detail: format!(
                "scene modifier fields require graph version {EFFECT_GRAPH_VERSION_WITH_SCENE_MODIFIERS}"
            ),
        });
    }
    if let Some(recipe) = recipe {
        validate_recipe(
            recipe,
            metadata.map(|meta| meta.params.as_slice()).unwrap_or(&[]),
            path,
        )?;
    } else if recipe_required {
        return Err(SceneModifierSchemaError::InvalidRecipe {
            path: format!("{path}.presetMetadata.sceneModifier"),
            detail: "modifier instance graph requires a recipe".into(),
        });
    }
    if let Some(meta) = metadata {
        for (target, is_string) in meta
            .bindings
            .iter()
            .map(|b| (&b.target, false))
            .chain(meta.string_bindings.iter().map(|b| (&b.target, true)))
        {
            let BindingTarget::SceneModifier {
                modifier_id,
                param_id,
            } = target
            else {
                continue;
            };
            let local_meta = def
                .scene_modifiers
                .iter()
                .find(|m| &m.id == modifier_id)
                .and_then(|m| m.graph.preset_metadata.as_ref());
            let valid = local_meta.is_some_and(|m| {
                if is_string {
                    m.string_params.iter().any(|p| &p.id == param_id)
                } else {
                    m.params.iter().any(|p| &p.id == param_id)
                        && !m
                            .scene_modifier
                            .as_ref()
                            .is_some_and(|r| r.preparation_params.contains(param_id))
                }
            });
            if !valid {
                return Err(SceneModifierSchemaError::InvalidBinding {
                    path: format!("{path}.presetMetadata"),
                    detail: format!(
                        "unresolved or preparation-only modifier parameter {modifier_id}/{param_id}"
                    ),
                });
            }
        }
    }
    let mut ids = HashSet::new();
    for (index, instance) in def.scene_modifiers.iter().enumerate() {
        let base = format!("{path}.sceneModifiers[{index}]");
        if !ids.insert(instance.id.clone()) || instance.id.is_empty() {
            return Err(SceneModifierSchemaError::DuplicateIdentity {
                path: format!("{base}.id"),
                detail: "modifier ids must be nonempty and unique".into(),
            });
        }
        check_ref(&format!("{base}.scene"), &instance.scene)?;
        match &instance.targets {
            SceneTargetSelection::AllObjects => {}
            SceneTargetSelection::Explicit { objects } => {
                let mut targets = BTreeSet::new();
                for (target_index, target) in objects.iter().enumerate() {
                    check_ref(&format!("{base}.targets[{target_index}]"), target)?;
                    if !targets.insert(target) {
                        return Err(SceneModifierSchemaError::DuplicateIdentity {
                            path: format!("{base}.targets[{target_index}]"),
                            detail: "explicit target identities must be unique".into(),
                        });
                    }
                }
            }
        }
        validate_def(&instance.graph, &format!("{base}.graph"), true)?;
        let mut frame_targets = BTreeSet::new();
        for (frame_index, frame) in instance.mesh_frames.iter().enumerate() {
            let frame_path = format!("{base}.meshFrames[{frame_index}]");
            check_ref(&format!("{frame_path}.target"), &frame.target)?;
            check_ref(&format!("{frame_path}.source"), &frame.source)?;
            if !frame_targets.insert(&frame.target) {
                return Err(SceneModifierSchemaError::DuplicateIdentity {
                    path: format!("{frame_path}.target"),
                    detail: "mesh frame targets must be unique".into(),
                });
            }
            if frame.source_definition_hash.trim().is_empty()
                || frame
                    .source_offset
                    .iter()
                    .any(|value| !value.is_finite() || !(*value as f32).is_finite())
                || !frame.scene_radius.is_finite()
                || !(frame.scene_radius as f32).is_finite()
                || frame.scene_radius <= 0.0
                || (frame.scene_radius as f32) <= 0.0
            {
                return Err(SceneModifierSchemaError::UnsupportedCoordinateFrame {
                    path: frame_path,
                    detail: "sourceDefinitionHash must be nonempty; frame offsets must remain finite after f32 conversion and sceneRadius must remain positive".into(),
                });
            }
        }
    }
    Ok(())
}

fn validate_recipe(
    recipe: &SceneModifierRecipe,
    params: &[ParamSpecDef],
    path: &str,
) -> Result<(), SceneModifierSchemaError> {
    if recipe.schema_version != SCENE_MODIFIER_RECIPE_VERSION {
        return Err(SceneModifierSchemaError::UnsupportedVersion {
            path: format!("{path}.presetMetadata.sceneModifier.schemaVersion"),
            detail: format!("expected recipe version {SCENE_MODIFIER_RECIPE_VERSION}"),
        });
    }
    if recipe.enabled_param.is_empty() || find_param(params, &recipe.enabled_param).is_none() {
        return Err(SceneModifierSchemaError::InvalidBinding {
            path: format!("{path}.presetMetadata.sceneModifier.enabledParam"),
            detail: "enabledParam must name a declared numeric parameter".into(),
        });
    }
    let mut preparation = BTreeSet::new();
    for id in &recipe.preparation_params {
        if !preparation.insert(id) {
            return Err(SceneModifierSchemaError::DuplicateIdentity {
                path: format!("{path}.presetMetadata.sceneModifier.preparationParams"),
                detail: format!("duplicate preparation parameter {id}"),
            });
        }
        let Some(param) = find_param(params, id) else {
            return Err(SceneModifierSchemaError::InvalidBinding {
                path: format!("{path}.presetMetadata.sceneModifier.preparationParams.{id}"),
                detail: "preparation parameter is not declared in preset metadata".into(),
            });
        };
        if !param.min.is_finite() || !param.max.is_finite() || !param.default_value.is_finite() {
            return Err(SceneModifierSchemaError::InvalidRecipe {
                path: format!("{path}.presetMetadata.params.{id}"),
                detail: "numeric parameter bounds and default must be finite".into(),
            });
        }
        if param.min > param.max || !(param.min..=param.max).contains(&param.default_value) {
            return Err(SceneModifierSchemaError::InvalidRecipe {
                path: format!("{path}.presetMetadata.params.{id}"),
                detail: "parameter default must lie within min/max".into(),
            });
        }
    }
    for (index, initializer) in recipe.initializers.iter().enumerate() {
        check_ref(
            &format!("{path}.presetMetadata.sceneModifier.initializers[{index}].target"),
            &initializer.target,
        )?;
        validate_expr(
            &format!("{path}.presetMetadata.sceneModifier.initializers[{index}].value"),
            &initializer.value,
        )?;
    }
    for (index, calibration) in recipe.calibrations.iter().enumerate() {
        if find_param(params, &calibration.param_id).is_none() {
            return Err(SceneModifierSchemaError::InvalidBinding {
                path: format!("{path}.presetMetadata.sceneModifier.calibrations[{index}].paramId"),
                detail: "calibration parameter is not declared in preset metadata".into(),
            });
        }
        for (name, expr) in [
            ("min", &calibration.min),
            ("max", &calibration.max),
            ("defaultValue", &calibration.default_value),
        ] {
            // A literal calibration is already its final scalar value. Bounds
            // expressions are evaluated at attachment time in f64; do not
            // constrain their intermediate constants to f32 here.
            if let SceneScalarExpr::Constant { value } = expr
                && !(*value as f32).is_finite()
            {
                return Err(SceneModifierSchemaError::InvalidRecipe {
                    path: format!(
                        "{path}.presetMetadata.sceneModifier.calibrations[{index}].{name}"
                    ),
                    detail: "literal calibration must remain finite after f32 conversion".into(),
                });
            }
            validate_expr(
                &format!("{path}.presetMetadata.sceneModifier.calibrations[{index}].{name}"),
                expr,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_graph_def::{BindingTarget, EffectGraphDef};
    use serde_json::{Value, json};

    fn recipe_json() -> Value {
        json!({
            "schemaVersion": 1,
            "singleton": false,
            "enabledParam": "enabled"
        })
    }

    fn metadata_json(recipe: Option<Value>) -> Value {
        let mut metadata = json!({
            "id": "sceneModifier.test",
            "displayName": "Test Modifier",
            "category": "Spatial",
            "oscPrefix": "scene_modifier_test",
            "params": [{
                "id": "enabled",
                "name": "Enabled",
                "min": 0.0,
                "max": 1.0,
                "defaultValue": 1.0
            }],
            "bindings": []
        });
        if let Some(recipe) = recipe {
            metadata["sceneModifier"] = recipe;
        }
        metadata
    }

    fn graph_json(recipe: Option<Value>, modifiers: Value) -> Value {
        json!({
            "version": 3,
            "presetMetadata": metadata_json(recipe),
            "sceneModifiers": modifiers,
            "nodes": [],
            "wires": []
        })
    }

    #[test]
    fn scene_modifier_v3_standalone_recipe_round_trips_and_validates() {
        let def: EffectGraphDef =
            serde_json::from_value(graph_json(Some(recipe_json()), json!([])))
                .expect("scene modifier fixture parses");
        validate_scene_modifier_schema(&def).expect("standalone recipe validates");
        let wire = serde_json::to_value(&def).expect("scene modifier serializes");
        assert_eq!(wire["version"], 3);
        assert_eq!(wire["presetMetadata"]["sceneModifier"]["schemaVersion"], 1);
        let back: EffectGraphDef = serde_json::from_value(wire).expect("scene modifier reparses");
        assert_eq!(def, back);
    }

    #[test]
    fn scene_modifier_v3_owner_stack_requires_instance_recipe() {
        let instance = graph_json(Some(recipe_json()), json!([]));
        let owner = graph_json(
            None,
            json!([
                {
                    "id": "modifier-a",
                    "scene": {"node": "scene"},
                    "targets": {"explicit": {"objects": [{"node": "object-a"}]}},
                    "graph": instance
                }
            ]),
        );
        let def: EffectGraphDef = serde_json::from_value(owner).expect("owner fixture parses");
        validate_scene_modifier_schema(&def).expect("owner and instance validate");
        assert_eq!(def.scene_modifiers[0].graph.scene_modifiers.len(), 0);

        let mut recursive = def.scene_modifiers[0].graph.as_ref().clone();
        recursive
            .scene_modifiers
            .push(def.scene_modifiers[0].clone());
        let err = validate_scene_modifier_schema(&recursive).expect_err("nested stack rejected");
        assert!(matches!(
            err,
            SceneModifierSchemaError::RecursiveModifier { .. }
        ));
    }

    #[test]
    fn scene_modifier_v3_frame_finiteness_and_identity_are_checked() {
        let mut def: EffectGraphDef = serde_json::from_value(graph_json(
            None,
            json!([{
                "id": "modifier-a",
                "scene": {"node": "scene"},
                "targets": "allObjects",
                "meshFrames": [{
                    "target": {"node": "object-a"},
                    "source": {"node": "source-a"},
                    "sourceDefinitionHash": "hash-a",
                    "sourceOffset": [0.0, 0.0, 0.0],
                    "sceneRadius": 1.0
                }],
                "graph": {
                    "version": 3,
                    "presetMetadata": metadata_json(Some(recipe_json())),
                    "nodes": [],
                    "wires": []
                }
            }]),
        ))
        .expect("frame fixture parses");
        validate_scene_modifier_schema(&def).expect("finite frame validates");
        let frame = &mut def.scene_modifiers[0].mesh_frames[0];
        frame.source_definition_hash.clear();
        frame.source_offset[0] = f64::MAX;
        let err = validate_scene_modifier_schema(&def).expect_err("invalid frame rejected");
        assert!(matches!(
            err,
            SceneModifierSchemaError::UnsupportedCoordinateFrame { .. }
        ));
    }

    #[test]
    fn scene_modifier_v3_rejects_duplicate_preparation_and_non_numeric_params() {
        let mut raw = graph_json(Some(recipe_json()), json!([]));
        raw["presetMetadata"]["sceneModifier"]["preparationParams"] = json!(["enabled", "enabled"]);
        let def: EffectGraphDef = serde_json::from_value(raw).expect("duplicate fixture parses");
        let err = validate_scene_modifier_schema(&def).expect_err("duplicate preparation rejected");
        assert!(matches!(
            err,
            SceneModifierSchemaError::DuplicateIdentity { .. }
        ));

        let mut raw = graph_json(Some(recipe_json()), json!([]));
        raw["presetMetadata"]["stringParams"] = json!([{"id": "label", "name": "Label"}]);
        raw["presetMetadata"]["sceneModifier"]["enabledParam"] = "label".into();
        let def: EffectGraphDef = serde_json::from_value(raw).expect("string fixture parses");
        let err = validate_scene_modifier_schema(&def).expect_err("string param rejected");
        assert!(matches!(
            err,
            SceneModifierSchemaError::InvalidBinding { .. }
        ));
    }

    #[test]
    fn scene_modifier_v3_checks_duplicate_ids_and_scoped_target_identity() {
        let mut raw = graph_json(
            None,
            json!([{
                "id": "modifier-a",
                "scene": {"node": "scene"},
                "targets": {"explicit": {"objects": [
                    {"scope": ["group-a"], "node": "object"},
                    {"scope": ["group-b"], "node": "object"}
                ]}},
                "graph": graph_json(Some(recipe_json()), json!([]))
            }]),
        );
        let def: EffectGraphDef =
            serde_json::from_value(raw.clone()).expect("scoped fixture parses");
        validate_scene_modifier_schema(&def).expect("scope is part of target identity");

        raw["sceneModifiers"][0]["targets"]["explicit"]["objects"] = json!([{"scope": ["group-a"], "node": "object"},
                   {"scope": ["group-a"], "node": "object"}]);
        let def: EffectGraphDef = serde_json::from_value(raw).expect("duplicate target parses");
        let err = validate_scene_modifier_schema(&def).expect_err("duplicate target rejected");
        assert!(matches!(
            err,
            SceneModifierSchemaError::DuplicateIdentity { .. }
        ));

        let raw = graph_json(
            None,
            json!([
                {"id": "modifier-a", "scene": {"node": "scene"},
                 "targets": "allObjects", "graph": graph_json(Some(recipe_json()), json!([]))},
                {"id": "modifier-a", "scene": {"node": "scene-2"},
                 "targets": "allObjects", "graph": graph_json(Some(recipe_json()), json!([]))}
            ]),
        );
        let def: EffectGraphDef = serde_json::from_value(raw).expect("duplicate ids parse");
        let err = validate_scene_modifier_schema(&def).expect_err("duplicate modifier id rejected");
        assert!(matches!(
            err,
            SceneModifierSchemaError::DuplicateIdentity { .. }
        ));
    }

    #[test]
    fn scene_modifier_v3_enforces_expression_depth_and_f32_calibration_bounds() {
        fn deep_add(depth: usize) -> SceneScalarExpr {
            if depth == 0 {
                SceneScalarExpr::Constant { value: 1.0 }
            } else {
                SceneScalarExpr::Add {
                    a: Box::new(deep_add(depth - 1)),
                    b: Box::new(SceneScalarExpr::Constant { value: 0.0 }),
                }
            }
        }

        let mut def: EffectGraphDef =
            serde_json::from_value(graph_json(Some(recipe_json()), json!([])))
                .expect("expression fixture parses");
        let recipe = def
            .preset_metadata
            .as_mut()
            .expect("metadata")
            .scene_modifier
            .as_mut()
            .expect("recipe");
        recipe.initializers.push(SceneNodeInitializer {
            target: SceneNodeRef {
                scope: Vec::new(),
                node: NodeId::new("local-node"),
            },
            param: "phase".into(),
            value: deep_add(16),
        });
        let err = validate_scene_modifier_schema(&def).expect_err("depth budget enforced");
        assert!(matches!(
            err,
            SceneModifierSchemaError::CapacityExceeded { .. }
        ));

        let mut def: EffectGraphDef =
            serde_json::from_value(graph_json(Some(recipe_json()), json!([])))
                .expect("calibration fixture parses");
        def.preset_metadata
            .as_mut()
            .expect("metadata")
            .scene_modifier
            .as_mut()
            .expect("recipe")
            .calibrations
            .push(SceneParamCalibration {
                param_id: "enabled".into(),
                min: SceneScalarExpr::Constant { value: f64::MAX },
                max: SceneScalarExpr::Constant { value: f64::MAX },
                default_value: SceneScalarExpr::Constant { value: f64::MAX },
            });
        let err = validate_scene_modifier_schema(&def)
            .expect_err("calibration outside f32 range rejected");
        assert!(matches!(
            err,
            SceneModifierSchemaError::InvalidRecipe { .. }
        ));
    }

    #[test]
    fn scene_modifier_v3_binding_and_preset_kind_keep_explicit_wire_names() {
        let target: BindingTarget = serde_json::from_value(json!({
            "kind": "sceneModifier",
            "modifierId": "modifier-a",
            "paramId": "enabled"
        }))
        .expect("scene modifier binding parses");
        assert!(matches!(target, BindingTarget::SceneModifier { .. }));
        let wire = serde_json::to_value(target).expect("scene modifier binding serializes");
        assert_eq!(wire["kind"], "sceneModifier");
    }

    #[test]
    fn scene_modifier_v3_metadata_promotion_never_downgrades() {
        let mut ordinary: EffectGraphDef =
            serde_json::from_value(graph_json(None, json!([]))).expect("ordinary graph parses");
        let ordinary_metadata = ordinary.preset_metadata.clone().expect("metadata present");
        ordinary.version = 1;
        assert_eq!(ordinary.with_preset_metadata(ordinary_metadata).version, 2);

        let mut scene: EffectGraphDef =
            serde_json::from_value(graph_json(Some(recipe_json()), json!([])))
                .expect("scene graph parses");
        let scene_metadata = scene.preset_metadata.clone().expect("metadata present");
        scene.version = 1;
        assert_eq!(
            scene.clone().with_preset_metadata(scene_metadata).version,
            3
        );

        let mut already_v3 = scene.clone();
        let metadata = already_v3
            .preset_metadata
            .clone()
            .expect("metadata present");
        already_v3.version = 3;
        assert_eq!(already_v3.with_preset_metadata(metadata).version, 3);
    }
}
