//! Serializable scene-modifier recipes and their schema-level validation.
//!
//! This module deliberately contains no renderer or GPU types.  Expansion is
//! owned by the renderer; these values are the authored snapshot and recipe
//! contract that expansion consumes.

use std::collections::{BTreeSet, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, ParamSpecDef, SerializedParamValue,
};
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

/// Return the admission diagnostic for an authored parameter that is part of
/// a calibrated scene-modifier contract.  Stable node identities are the
/// source of truth; document ids and display handles can change when a graph
/// is grouped or flattened.  Calibration statistics are deliberately left
/// writable because they are import provenance rather than selectors.
pub fn scene_modifier_parameter_lock_reason(
    owner: &EffectGraphDef,
    node: &NodeId,
    param: &str,
) -> Option<&'static str> {
    if !matches!(param, "source_vertex_count" | "source_bbox_radius")
        && owner
            .scene_modifiers
            .iter()
            .any(|modifier| modifier.mesh_frames.iter().any(|frame| &frame.source.node == node))
    {
        return Some("Source settings are locked by a calibrated modifier; remove it before changing the source.");
    }

    None
}

/// A host control is locked if any binding addresses captured source data.
/// Modifier controls use a distinct namespace.
pub fn scene_modifier_macro_lock_reason(owner: &EffectGraphDef, param: &str) -> Option<&'static str> {
    owner.preset_metadata.as_ref()?.bindings.iter().filter(|binding| binding.id == param)
        .find_map(|binding| match &binding.target {
            BindingTarget::Node { node_id, param } => scene_modifier_parameter_lock_reason(owner, node_id, param),
            _ => None,
        })
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
    RenderMode,
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
    /// Counter before the first real event pending for this evaluation;
    /// equals TriggerCount when no event is pending.
    TriggerBaseline,
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

/// Evaluate one bounded recipe expression against imported scene bounds.
///
/// Evaluation stays in `f64`; callers that write a serialized `f32` value
/// remain responsible for checking that final conversion. Intermediate
/// results must still be finite so authored overflow cannot be hidden by a
/// later operation.
pub fn evaluate_scene_scalar_expr(
    expr: &SceneScalarExpr,
    bounds: ([f64; 3], [f64; 3]),
) -> Result<f64, SceneModifierSchemaError> {
    validate_expr("expression", expr)?;
    for axis in 0..3 {
        let min = bounds.0[axis];
        let max = bounds.1[axis];
        if !min.is_finite() || !max.is_finite() || min > max {
            return Err(SceneModifierSchemaError::InvalidRecipe {
                path: format!("bounds[{axis}]"),
                detail: "bounds must be finite and min must not exceed max".into(),
            });
        }
    }

    fn evaluate(
        expr: &SceneScalarExpr,
        bounds: &([f64; 3], [f64; 3]),
    ) -> Result<f64, SceneModifierSchemaError> {
        let value = match expr {
            SceneScalarExpr::Constant { value } => *value,
            SceneScalarExpr::BoundsMin { axis } => bounds.0[axis_index(*axis)],
            SceneScalarExpr::BoundsMax { axis } => bounds.1[axis_index(*axis)],
            SceneScalarExpr::Add { a, b } => evaluate(a, bounds)? + evaluate(b, bounds)?,
            SceneScalarExpr::Subtract { a, b } => evaluate(a, bounds)? - evaluate(b, bounds)?,
            SceneScalarExpr::Multiply { a, b } => evaluate(a, bounds)? * evaluate(b, bounds)?,
            SceneScalarExpr::Max { a, b } => evaluate(a, bounds)?.max(evaluate(b, bounds)?),
        };
        if value.is_finite() {
            Ok(value)
        } else {
            Err(SceneModifierSchemaError::InvalidRecipe {
                path: "expression".into(),
                detail: "expression evaluation produced a non-finite value".into(),
            })
        }
    }

    fn axis_index(axis: SceneAxis) -> usize {
        match axis {
            SceneAxis::X => 0,
            SceneAxis::Y => 1,
            SceneAxis::Z => 2,
        }
    }

    evaluate(expr, &bounds)
}

/// Apply a recipe's initializers and manifest calibrations to a fresh local
/// snapshot. The input graph is never mutated; all validation and evaluation
/// happen before the completed clone is returned.
pub fn initialize_scene_modifier_snapshot(
    recipe_graph: &EffectGraphDef,
    bounds: ([f64; 3], [f64; 3]),
) -> Result<EffectGraphDef, SceneModifierSchemaError> {
    validate_scene_modifier_schema(recipe_graph)?;
    let recipe = recipe_graph
        .preset_metadata
        .as_ref()
        .and_then(|metadata| metadata.scene_modifier.as_ref())
        .cloned()
        .ok_or_else(|| SceneModifierSchemaError::InvalidRecipe {
            path: "graph.presetMetadata.sceneModifier".into(),
            detail: "fresh scene-modifier application requires recipe metadata".into(),
        })?;

    let mut snapshot = recipe_graph.clone();
    let mut initializer_targets = HashSet::new();
    for (index, initializer) in recipe.initializers.iter().enumerate() {
        let path = format!("graph.presetMetadata.sceneModifier.initializers[{index}]");
        let identity = (initializer.target.clone(), initializer.param.clone());
        if !initializer_targets.insert(identity) {
            return Err(SceneModifierSchemaError::DuplicateIdentity {
                path: format!("{path}.target"),
                detail: "initializer target and parameter must be unique".into(),
            });
        }
        let value = evaluate_scene_scalar_expr(&initializer.value, bounds)?;
        let node = resolve_leaf_node_mut(
            &mut snapshot,
            &initializer.target,
            &format!("{path}.target"),
        )?;
        let Some(serialized) = node.params.get_mut(&initializer.param) else {
            return Err(SceneModifierSchemaError::InvalidBinding {
                path: format!("{path}.param"),
                detail: format!("serialized parameter '{}' was not found", initializer.param),
            });
        };
        apply_scalar_value(serialized, value, &format!("{path}.value"))?;
    }

    let mut calibration_ids = HashSet::new();
    for (index, calibration) in recipe.calibrations.iter().enumerate() {
        let path = format!("graph.presetMetadata.sceneModifier.calibrations[{index}]");
        if !calibration_ids.insert(calibration.param_id.clone()) {
            return Err(SceneModifierSchemaError::DuplicateIdentity {
                path: format!("{path}.paramId"),
                detail: "calibration parameter ids must be unique".into(),
            });
        }
        let min = evaluate_scene_scalar_expr(&calibration.min, bounds)?;
        let max = evaluate_scene_scalar_expr(&calibration.max, bounds)?;
        let default_value = evaluate_scene_scalar_expr(&calibration.default_value, bounds)?;
        if min > max || !(min..=max).contains(&default_value) {
            return Err(SceneModifierSchemaError::InvalidRecipe {
                path,
                detail: "calibration min/max/default must be ordered and in range before conversion".into(),
            });
        }
        let min = finite_f32(min, &format!("{path}.min"))?;
        let max = finite_f32(max, &format!("{path}.max"))?;
        let default_value = finite_f32(default_value, &format!("{path}.defaultValue"))?;
        if min > max || !(min..=max).contains(&default_value) {
            return Err(SceneModifierSchemaError::InvalidRecipe {
                path,
                detail: "calibration min/max/default must be ordered and in range".into(),
            });
        }
        let Some(metadata) = snapshot.preset_metadata.as_mut() else {
            unreachable!("recipe metadata was present after cloning");
        };
        let Some(param) = metadata
            .params
            .iter_mut()
            .find(|param| param.id == calibration.param_id)
        else {
            return Err(SceneModifierSchemaError::InvalidBinding {
                path: format!("{path}.paramId"),
                detail: format!(
                    "calibration parameter '{}' was not found",
                    calibration.param_id
                ),
            });
        };
        param.min = min;
        param.max = max;
        param.default_value = default_value;
        for binding in &mut metadata.bindings {
            if binding.id == calibration.param_id {
                binding.default_value = default_value;
            }
        }
    }

    Ok(snapshot)
}

fn resolve_leaf_node_mut<'a>(
    graph: &'a mut EffectGraphDef,
    target: &SceneNodeRef,
    path: &str,
) -> Result<&'a mut EffectGraphNode, SceneModifierSchemaError> {
    resolve_leaf_in_nodes(&mut graph.nodes, &target.scope, &target.node, path)
}

fn resolve_leaf_in_nodes<'a>(
    nodes: &'a mut [EffectGraphNode],
    scope: &[NodeId],
    leaf: &NodeId,
    path: &str,
) -> Result<&'a mut EffectGraphNode, SceneModifierSchemaError> {
    if scope.is_empty() {
        let matches: Vec<usize> = nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| (node.node_id == *leaf).then_some(index))
            .collect();
        let Some(&index) = matches.first() else {
            return Err(SceneModifierSchemaError::MissingTarget {
                path: path.into(),
                detail: format!("leaf node '{leaf}' was not found"),
            });
        };
        if matches.len() > 1 {
            return Err(SceneModifierSchemaError::DuplicateIdentity {
                path: path.into(),
                detail: format!("leaf node '{leaf}' is ambiguous"),
            });
        }
        let node = &mut nodes[index];
        if node.group.is_some() {
            return Err(SceneModifierSchemaError::MissingTarget {
                path: path.into(),
                detail: format!("target node '{leaf}' is not a leaf"),
            });
        }
        return Ok(node);
    }

    let group_id = &scope[0];
    let matches: Vec<usize> = nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| (node.node_id == *group_id).then_some(index))
        .collect();
    let Some(&index) = matches.first() else {
        return Err(SceneModifierSchemaError::MissingTarget {
            path: path.into(),
            detail: format!("scope group '{group_id}' was not found"),
        });
    };
    if matches.len() > 1 {
        return Err(SceneModifierSchemaError::DuplicateIdentity {
            path: path.into(),
            detail: format!("scope group '{group_id}' is ambiguous"),
        });
    }
    let node = &mut nodes[index];
    let Some(group) = node.group.as_mut() else {
        return Err(SceneModifierSchemaError::MissingTarget {
            path: path.into(),
            detail: format!("scope node '{group_id}' is not a group"),
        });
    };
    resolve_leaf_in_nodes(&mut group.nodes, &scope[1..], leaf, path)
}

fn finite_f32(value: f64, path: &str) -> Result<f32, SceneModifierSchemaError> {
    let converted = value as f32;
    if converted.is_finite() {
        Ok(converted)
    } else {
        Err(SceneModifierSchemaError::InvalidRecipe {
            path: path.into(),
            detail: "value must remain finite after f32 conversion".into(),
        })
    }
}

fn apply_scalar_value(
    target: &mut SerializedParamValue,
    value: f64,
    path: &str,
) -> Result<(), SceneModifierSchemaError> {
    match target {
        SerializedParamValue::Float { value: target } => {
            *target = finite_f32(value, path)?;
        }
        SerializedParamValue::Int { value: target } => {
            if !value.is_finite()
                || value.fract() != 0.0
                || value < f64::from(i32::MIN)
                || value > f64::from(i32::MAX)
            {
                return Err(SceneModifierSchemaError::InvalidRecipe {
                    path: path.into(),
                    detail: "integer initializer must be an exact i32 value".into(),
                });
            }
            *target = value as i32;
        }
        SerializedParamValue::Enum { value: target } => {
            if !value.is_finite()
                || value.fract() != 0.0
                || value < 0.0
                || value > f64::from(u32::MAX)
            {
                return Err(SceneModifierSchemaError::InvalidRecipe {
                    path: path.into(),
                    detail: "enum initializer must be an exact u32 value".into(),
                });
            }
            *target = value as u32;
        }
        _ => {
            return Err(SceneModifierSchemaError::InvalidBinding {
                path: path.into(),
                detail: "initializer target must be a serialized numeric scalar".into(),
            });
        }
    }
    Ok(())
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
    use crate::effect_graph_def::{BindingDef, BindingTarget, EffectGraphDef};
    use crate::effects::ParamConvert;
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

    fn initializer_fixture() -> EffectGraphDef {
        let mut raw = graph_json(Some(recipe_json()), json!([]));
        raw["presetMetadata"]["params"] = json!([
            {"id": "enabled", "name": "Enabled", "min": 0.0,
             "max": 1.0, "defaultValue": 1.0},
            {"id": "amount", "name": "Amount", "min": -2.0,
             "max": 2.0, "defaultValue": 0.0}
        ]);
        raw["presetMetadata"]["sceneModifier"]["initializers"] = json!([
            {
                "target": {"scope": ["group-id"], "node": "leaf-id"},
                "param": "amount",
                "value": {"constant": {"value": 0.0}}
            }
        ]);
        raw["nodes"] = json!([
            {
                "id": 1,
                "nodeId": "group-id",
                "typeId": "group",
                "group": {
                    "interface": {"inputs": [], "outputs": [], "params": []},
                    "nodes": [{
                        "id": 1,
                        "nodeId": "leaf-id",
                        "typeId": "node.test",
                        "params": {
                            "amount": {"type": "Float", "value": 0.0},
                            "count": {"type": "Int", "value": 0},
                            "mode": {"type": "Enum", "value": 0},
                            "toggle": {"type": "Bool", "value": false}
                        }
                    }],
                    "wires": []
                }
            }
        ]);
        serde_json::from_value(raw).expect("initializer fixture parses")
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
    fn scene_modifier_expand_scalar_expr_loop_cell_size() {
        let expr = SceneScalarExpr::Multiply {
            a: Box::new(SceneScalarExpr::Constant { value: 2.0 }),
            b: Box::new(SceneScalarExpr::Subtract {
                a: Box::new(SceneScalarExpr::BoundsMax { axis: SceneAxis::Z }),
                b: Box::new(SceneScalarExpr::BoundsMin { axis: SceneAxis::Z }),
            }),
        };
        let value = evaluate_scene_scalar_expr(&expr, ([-1.0, 2.0, -3.5], [4.0, 8.0, 2.5]))
            .expect("Loop cell size evaluates");
        assert_eq!(value, 12.0);
    }

    #[test]
    fn scene_modifier_expand_scalar_expr_axes_and_large_finite_intermediate() {
        let axes = [SceneAxis::X, SceneAxis::Y, SceneAxis::Z];
        let bounds = ([-2.0, -4.0, -8.0], [3.0, 6.0, 10.0]);
        for (index, axis) in axes.into_iter().enumerate() {
            let min = evaluate_scene_scalar_expr(&SceneScalarExpr::BoundsMin { axis }, bounds)
                .expect("minimum evaluates");
            let max = evaluate_scene_scalar_expr(&SceneScalarExpr::BoundsMax { axis }, bounds)
                .expect("maximum evaluates");
            assert_eq!(min, bounds.0[index]);
            assert_eq!(max, bounds.1[index]);
        }

        let cancellation = SceneScalarExpr::Subtract {
            a: Box::new(SceneScalarExpr::Constant { value: 1.0e200 }),
            b: Box::new(SceneScalarExpr::Constant { value: 1.0e200 }),
        };
        assert_eq!(
            evaluate_scene_scalar_expr(&cancellation, ([-1.0; 3], [1.0; 3]))
                .expect("large finite intermediates remain legal"),
            0.0
        );
    }

    #[test]
    fn scene_modifier_expand_scalar_expr_rejects_overflow_and_invalid_bounds() {
        let overflow = SceneScalarExpr::Multiply {
            a: Box::new(SceneScalarExpr::Constant { value: f64::MAX }),
            b: Box::new(SceneScalarExpr::Constant { value: 2.0 }),
        };
        assert!(matches!(
            evaluate_scene_scalar_expr(&overflow, ([-1.0; 3], [1.0; 3])),
            Err(SceneModifierSchemaError::InvalidRecipe { .. })
        ));

        let reversed = evaluate_scene_scalar_expr(
            &SceneScalarExpr::Constant { value: 1.0 },
            ([2.0, 0.0, 0.0], [1.0, 1.0, 1.0]),
        );
        assert!(matches!(
            reversed,
            Err(SceneModifierSchemaError::InvalidRecipe { .. })
        ));
        let nonfinite = evaluate_scene_scalar_expr(
            &SceneScalarExpr::Constant { value: 1.0 },
            ([f64::NAN; 3], [1.0; 3]),
        );
        assert!(matches!(
            nonfinite,
            Err(SceneModifierSchemaError::InvalidRecipe { .. })
        ));
    }

    #[test]
    fn scene_modifier_expand_scalar_expr_reuses_depth_and_node_limits() {
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
        fn wide_add(depth: usize) -> SceneScalarExpr {
            if depth == 0 {
                SceneScalarExpr::Constant { value: 1.0 }
            } else {
                SceneScalarExpr::Add {
                    a: Box::new(wide_add(depth - 1)),
                    b: Box::new(wide_add(depth - 1)),
                }
            }
        }

        assert!(matches!(
            evaluate_scene_scalar_expr(&deep_add(16), ([-1.0; 3], [1.0; 3])),
            Err(SceneModifierSchemaError::CapacityExceeded { .. })
        ));
        assert!(matches!(
            evaluate_scene_scalar_expr(&wide_add(6), ([-1.0; 3], [1.0; 3])),
            Err(SceneModifierSchemaError::CapacityExceeded { .. })
        ));
    }

    #[test]
    fn scene_modifier_expand_initializers_nested_loop_and_calibration() {
        let mut input = initializer_fixture();
        input
            .preset_metadata
            .as_mut()
            .expect("metadata")
            .bindings
            .push(BindingDef {
                id: "amount".into(),
                label: "Amount".into(),
                default_value: -0.25,
                target: BindingTarget::Node {
                    node_id: NodeId::new("leaf-id"),
                    param: "amount".into(),
                },
                convert: ParamConvert::Float,
                user_added: false,
                scale: 1.0,
                offset: 0.0,
                default_mirrors_node_param: false,
            });
        let recipe = input
            .preset_metadata
            .as_mut()
            .expect("metadata")
            .scene_modifier
            .as_mut()
            .expect("recipe");
        recipe.initializers[0].value = SceneScalarExpr::Multiply {
            a: Box::new(SceneScalarExpr::Constant { value: 2.0 }),
            b: Box::new(SceneScalarExpr::Subtract {
                a: Box::new(SceneScalarExpr::BoundsMax { axis: SceneAxis::Z }),
                b: Box::new(SceneScalarExpr::BoundsMin { axis: SceneAxis::Z }),
            }),
        };
        recipe.calibrations.push(SceneParamCalibration {
            param_id: "amount".into(),
            min: SceneScalarExpr::Constant { value: -1.0 },
            max: SceneScalarExpr::Constant { value: 2.0 },
            default_value: SceneScalarExpr::Constant { value: 0.5 },
        });
        let snapshot =
            initialize_scene_modifier_snapshot(&input, ([0.0, 0.0, 0.0], [1.0, 2.0, 0.75]))
                .expect("fresh initializer applies");
        let leaf = &snapshot.nodes[0].group.as_ref().expect("group").nodes[0];
        assert_eq!(
            leaf.params["amount"],
            SerializedParamValue::Float { value: 1.5 }
        );
        let amount = &snapshot.preset_metadata.as_ref().expect("metadata").params[1];
        assert_eq!(
            (amount.min, amount.max, amount.default_value),
            (-1.0, 2.0, 0.5)
        );
        assert_eq!(
            snapshot.preset_metadata.as_ref().expect("metadata").bindings[0].default_value,
            0.5
        );
        assert_eq!(
            input.nodes[0].group.as_ref().expect("group").nodes[0].params["amount"],
            SerializedParamValue::Float { value: 0.0 }
        );
    }

    #[test]
    fn scene_modifier_expand_initializers_is_transactional_on_success_and_failure() {
        let mut input = initializer_fixture();
        input
            .preset_metadata
            .as_mut()
            .expect("metadata")
            .scene_modifier
            .as_mut()
            .expect("recipe")
            .initializers[0]
            .value = SceneScalarExpr::Constant { value: 1.0 };
        let original = input.clone();
        let snapshot = initialize_scene_modifier_snapshot(&input, ([-1.0; 3], [1.0; 3]))
            .expect("valid initializer applies");
        assert_ne!(snapshot, input);
        assert_eq!(input, original);

        let mut invalid = input.clone();
        invalid
            .preset_metadata
            .as_mut()
            .expect("metadata")
            .scene_modifier
            .as_mut()
            .expect("recipe")
            .initializers
            .push(SceneNodeInitializer {
                target: SceneNodeRef {
                    scope: vec![NodeId::new("group-id")],
                    node: NodeId::new("leaf-id"),
                },
                param: "missing".into(),
                value: SceneScalarExpr::Constant { value: 1.0 },
            });
        let invalid_original = invalid.clone();
        assert!(initialize_scene_modifier_snapshot(&invalid, ([-1.0; 3], [1.0; 3])).is_err());
        assert_eq!(invalid, invalid_original);
    }

    #[test]
    fn scene_modifier_expand_initializers_rejects_duplicate_and_missing_targets() {
        let mut duplicate = initializer_fixture();
        let recipe = duplicate
            .preset_metadata
            .as_mut()
            .expect("metadata")
            .scene_modifier
            .as_mut()
            .expect("recipe");
        let initializer = recipe.initializers[0].clone();
        recipe.initializers.push(initializer);
        assert!(matches!(
            initialize_scene_modifier_snapshot(&duplicate, ([-1.0; 3], [1.0; 3])),
            Err(SceneModifierSchemaError::DuplicateIdentity { .. })
        ));

        let mut missing = initializer_fixture();
        missing
            .preset_metadata
            .as_mut()
            .expect("metadata")
            .scene_modifier
            .as_mut()
            .expect("recipe")
            .initializers[0]
            .target
            .node = NodeId::new("missing-leaf");
        assert!(matches!(
            initialize_scene_modifier_snapshot(&missing, ([-1.0; 3], [1.0; 3])),
            Err(SceneModifierSchemaError::MissingTarget { .. })
        ));
    }

    #[test]
    fn scene_modifier_expand_initializers_rejects_integer_fraction_and_overflow() {
        for value in [1.5, f64::from(i32::MAX) + 1.0] {
            let mut input = initializer_fixture();
            let leaf = &mut input.nodes[0].group.as_mut().expect("group").nodes[0];
            leaf.params
                .insert("amount".into(), SerializedParamValue::Int { value: 0 });
            input
                .preset_metadata
                .as_mut()
                .expect("metadata")
                .scene_modifier
                .as_mut()
                .expect("recipe")
                .initializers[0]
                .value = SceneScalarExpr::Constant { value };
            assert!(matches!(
                initialize_scene_modifier_snapshot(&input, ([-1.0; 3], [1.0; 3])),
                Err(SceneModifierSchemaError::InvalidRecipe { .. })
            ));
        }
    }

    #[test]
    fn scene_modifier_expand_initializers_allows_large_finite_cancellation() {
        let mut input = initializer_fixture();
        input
            .preset_metadata
            .as_mut()
            .expect("metadata")
            .scene_modifier
            .as_mut()
            .expect("recipe")
            .initializers[0]
            .value = SceneScalarExpr::Subtract {
            a: Box::new(SceneScalarExpr::Constant { value: 1.0e200 }),
            b: Box::new(SceneScalarExpr::Constant { value: 1.0e200 }),
        };
        let snapshot = initialize_scene_modifier_snapshot(&input, ([-1.0; 3], [1.0; 3]))
            .expect("large finite intermediate remains valid");
        assert_eq!(
            snapshot.nodes[0].group.as_ref().expect("group").nodes[0].params["amount"],
            SerializedParamValue::Float { value: 0.0 }
        );
    }

    #[test]
    fn scene_modifier_expand_initializers_rejects_invalid_calibration_ranges() {
        for (min, max, default_value) in [(2.0, 1.0, 1.0), (0.0, 1.0, 2.0)] {
            let mut input = initializer_fixture();
            input
                .preset_metadata
                .as_mut()
                .expect("metadata")
                .scene_modifier
                .as_mut()
                .expect("recipe")
                .calibrations
                .push(SceneParamCalibration {
                    param_id: "amount".into(),
                    min: SceneScalarExpr::Constant { value: min },
                    max: SceneScalarExpr::Constant { value: max },
                    default_value: SceneScalarExpr::Constant {
                        value: default_value,
                    },
                });
            assert!(matches!(
                initialize_scene_modifier_snapshot(&input, ([-1.0; 3], [1.0; 3])),
                Err(SceneModifierSchemaError::InvalidRecipe { .. })
            ));
        }
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

    fn owner_with_calibrated_source(source: &str) -> EffectGraphDef {
        let local = graph_json(None, json!([]));
        let raw = json!({
            "version": 3,
            "nodes": [{
                "id": 1, "nodeId": source, "typeId": "node.gltf_mesh_source"
            }],
            "wires": [],
            "sceneModifiers": [{
                "id": "modifier",
                "scene": {"scope": [], "node": "scene"},
                "targets": "allObjects",
                "meshFrames": [{
                    "target": {"scope": ["nested-group"], "node": "object"},
                    "source": {"scope": ["nested-group"], "node": source},
                    "sourceDefinitionHash": "hash",
                    "sourceOffset": [0.0, 0.0, 0.0],
                    "sceneRadius": 1.0
                }],
                "graph": local
            }]
        });
        serde_json::from_value(raw).expect("calibrated source fixture parses")
    }

    #[test]
    fn calibrated_source_lock_uses_stable_nested_identity_and_exempts_provenance() {
        let owner = owner_with_calibrated_source("nested-source");
        assert!(scene_modifier_parameter_lock_reason(
            &owner,
            &NodeId::new("nested-source"),
            "path",
        )
        .is_some());
        assert!(scene_modifier_parameter_lock_reason(
            &owner,
            &NodeId::new("nested-source"),
            "max_capacity",
        )
        .is_some());
        assert!(scene_modifier_parameter_lock_reason(
            &owner,
            &NodeId::new("nested-source"),
            "source_vertex_count",
        )
        .is_none());
        assert!(scene_modifier_parameter_lock_reason(
            &owner,
            &NodeId::new("same-text-different-node"),
            "path",
        )
        .is_none());
    }

    #[test]
    fn vertices_recipe_allows_its_host_scene_rt_enabled() {
        let mut owner: EffectGraphDef = serde_json::from_value(json!({
            "version": 3,
            "nodes": [{
                "id": 1, "nodeId": "scene", "typeId": "node.render_scene"
            }],
            "wires": [],
            "sceneModifiers": [{
                "id": "modifier",
                "scene": {"scope": [], "node": "scene"},
                "targets": "allObjects",
                "graph": {
                    "version": 3,
                    "presetMetadata": {
                        "id": "vertices", "displayName": "Vertices", "category": "Geometry",
                        "oscPrefix": "vertices", "params": [], "bindings": [],
                        "sceneModifier": {
                            "schemaVersion": 1, "singleton": false, "enabledParam": "enabled",
                            "stages": [{"group": "deform", "scope": "scene", "outputs":
                                [{"port": "vertices", "endpoint": "vertices"}]}]
                        }
                    },
                    "nodes": [], "wires": []
                }
            }]
        }))
        .expect("vertices recipe fixture parses");
        assert!(scene_modifier_parameter_lock_reason(
            &owner,
            &NodeId::new("scene"),
            "rt_enabled",
        )
        .is_none());
        assert!(scene_modifier_parameter_lock_reason(
            &owner,
            &NodeId::new("other-scene"),
            "rt_enabled",
        )
        .is_none());
        // Keep the fixture mutable so this test also guards that the helper is
        // read-only and does not consume the authored owner graph.
        owner.nodes[0].params.insert(
            "rt_enabled".into(),
            SerializedParamValue::Bool { value: true },
        );
        assert!(owner.nodes[0].params.contains_key("rt_enabled"));
    }
}
