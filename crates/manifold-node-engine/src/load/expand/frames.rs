//! Authored coordinate calibration for static imported meshes.

use std::collections::{BTreeMap, BTreeSet};

use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};
use manifold_core::scene_modifier_preset::{
    SceneContextValue, SceneMeshReferenceFrame, SceneModifierInstanceDef, SceneNodeRef,
    SceneStageSource, SceneTargetSelection,
};
use manifold_core::scene_source_identity::{
    SceneSourceIdentityError, scene_source_definition_hash,
};

use manifold_core::scene_index::FlatSceneIndex;

use super::SceneModifierExpandError;

fn frame_error(target: &SceneNodeRef, detail: impl Into<String>) -> SceneModifierExpandError {
    SceneModifierExpandError::UnsupportedCoordinateFrame {
        path: format!("{target:?}"),
        detail: detail.into(),
    }
}

pub(super) fn selected_objects(
    index: &FlatSceneIndex,
    instance: &SceneModifierInstanceDef,
) -> Result<Vec<SceneNodeRef>, SceneModifierExpandError> {
    let available = index.scene_objects(&instance.scene)?;
    let selected = match &instance.targets {
        SceneTargetSelection::AllObjects => available,
        SceneTargetSelection::Explicit { objects } => {
            let members: BTreeSet<_> = available.iter().collect();
            let mut selected = BTreeSet::new();
            for target in objects {
                if !members.contains(target) {
                    return Err(SceneModifierExpandError::MissingTarget {
                        path: format!("{target:?}"),
                        detail: "target is not an object in the selected scene".into(),
                    });
                }
                if !selected.insert(target.clone()) {
                    return Err(SceneModifierExpandError::DuplicateIdentity {
                        path: format!("{target:?}"),
                        detail: "target is selected more than once".into(),
                    });
                }
            }
            // A static import has one authored object with several material
            // draws. Its shared transform identifies the parts of that object.
            let roots = selected.clone();
            for target in &roots {
                if target.scope.is_empty() {
                    continue;
                }
                let transform = index.input(target, "parent_transform")?.or(index.input(target, "transform")?);
                for part in &available {
                    if part.scope == target.scope
                        && transform.is_some()
                        && index.input(part, "parent_transform")?.or(index.input(part, "transform")?).map(|w| (w.from_node, &w.from_port))
                            == transform.map(|w| (w.from_node, &w.from_port))
                    {
                        selected.insert(part.clone());
                    }
                }
            }
            selected.into_iter().collect()
        }
    };
    if selected.len() > 256 {
        return Err(SceneModifierExpandError::CapacityExceeded {
            path: instance.id.to_string(),
            detail: "a modifier supports at most 256 object targets".into(),
        });
    }
    Ok(selected)
}

pub(super) fn needs_mesh_frame(instance: &SceneModifierInstanceDef) -> bool {
    // The stage-less Math View modifier samples real faces, so it captures the
    // same frames a deformation recipe would.
    if manifold_core::scene_modifier_math_view::is_math_view_recipe(&instance.graph) {
        return true;
    }
    instance
        .graph
        .preset_metadata
        .as_ref()
        .and_then(|metadata| metadata.scene_modifier.as_ref())
        .is_some_and(|recipe| {
            recipe.shatter.is_some() || recipe
                .stages
                .iter()
                .flat_map(|stage| &stage.inputs)
                .any(|input| {
                    matches!(
                        input.source,
                        SceneStageSource::Context {
                            value: SceneContextValue::SourceOffsetX
                                | SceneContextValue::SourceOffsetY
                                | SceneContextValue::SourceOffsetZ
                                | SceneContextValue::SceneRadius
                        }
                    )
                })
        })
}

/// Capture only newly selected targets. Surviving frames retain their original
/// placement and radius even when the object's downstream transform has moved.
/// Call this during an undoable structural edit, never during live evaluation.
pub fn resolve_modifier_mesh_frames(
    owner: &EffectGraphDef,
    instance: &SceneModifierInstanceDef,
) -> Result<Vec<SceneMeshReferenceFrame>, SceneModifierExpandError> {
    let index = FlatSceneIndex::build(owner)?;
    let targets = selected_objects(&index, instance)?;
    if !needs_mesh_frame(instance) {
        return Ok(Vec::new());
    }
    if targets.is_empty() {
        return Err(frame_error(
            &instance.scene,
            "coordinate context requires at least one mesh target",
        ));
    }
    let mut saved = BTreeMap::new();
    let mut common_radius = None;
    for frame in &instance.mesh_frames {
        validate_numbers(frame)?;
        if saved.insert(&frame.target, frame).is_some() {
            return Err(frame_error(&frame.target, "duplicate saved mesh frame"));
        }
        if common_radius.is_some_and(|radius| radius != frame.scene_radius) {
            return Err(frame_error(
                &frame.target,
                "all material targets must share the saved scene radius",
            ));
        }
        common_radius = Some(frame.scene_radius);
    }
    let radius = match common_radius {
        Some(radius) => radius,
        None => scene_radius(owner, &index, &instance.scene)?,
    };
    let mut result = Vec::with_capacity(targets.len());
    for target in targets {
        let (source, source_node) = source_route(&index, &target)?;
        let fingerprint = source_fingerprint(owner, source_node)?;
        if let Some(frame) = saved.get(&target) {
            if frame.source != source || frame.source_definition_hash != fingerprint {
                return Err(frame_error(
                    &target,
                    "mesh source changed; remove and reapply this modifier to capture a new frame",
                ));
            }
            result.push((*frame).clone());
        } else {
            if source_node.type_id != "node.gltf_mesh_source" {
                return Err(frame_error(
                    &target,
                    "fresh coordinate capture requires a direct static glTF mesh source",
                ));
            }
            if !matches!(
                source_node.params.get("fit"),
                None | Some(SerializedParamValue::Enum { value: 0 })
            ) {
                return Err(frame_error(
                    &target,
                    "per-part fitted sources require a separately qualified coordinate frame",
                ));
            }
            let source_offset = if instance.graph.preset_metadata.as_ref()
                .and_then(|m| m.scene_modifier.as_ref()).is_some_and(|r| r.shatter.is_some()) {
                // Shatter follows the live rigid pose. Only its immutable mesh
                // selection needs capture; no world-space calibration is used.
                [scalar(source_node, "translate_x", 0.0)?,
                 scalar(source_node, "translate_y", 0.0)?,
                 scalar(source_node, "translate_z", 0.0)?]
            } else {
                static_offset(&index, &target)?
            };
            result.push(SceneMeshReferenceFrame {
                target,
                source,
                source_definition_hash: fingerprint,
                source_offset,
                scene_radius: radius,
            });
        }
    }
    Ok(result)
}

/// Validate a complete authored snapshot without capturing or changing frames.
pub fn validate_modifier_mesh_frames(
    owner: &EffectGraphDef,
    instance: &SceneModifierInstanceDef,
) -> Result<(), SceneModifierExpandError> {
    let index = FlatSceneIndex::build(owner)?;
    validate_saved_frames(owner, &index, instance)
}

pub(super) fn validate_saved_frames(
    owner: &EffectGraphDef,
    index: &FlatSceneIndex,
    instance: &SceneModifierInstanceDef,
) -> Result<(), SceneModifierExpandError> {
    if !needs_mesh_frame(instance) {
        if !instance.mesh_frames.is_empty() {
            return Err(frame_error(
                &instance.scene,
                "recipe does not consume mesh coordinate context",
            ));
        }
        return Ok(());
    }
    let targets = selected_objects(index, instance)?;
    let actual: BTreeSet<_> = instance
        .mesh_frames
        .iter()
        .map(|frame| &frame.target)
        .collect();
    let expected: BTreeSet<_> = targets.iter().collect();
    if actual != expected || actual.len() != instance.mesh_frames.len() || targets.is_empty() {
        return Err(frame_error(
            &instance.scene,
            "saved mesh frames must match every selected object; apply or retarget through an authoring edit",
        ));
    }
    let mut radius = None;
    for frame in &instance.mesh_frames {
        validate_numbers(frame)?;
        if radius.is_some_and(|value| value != frame.scene_radius) {
            return Err(frame_error(&frame.target, "saved target radii disagree"));
        }
        radius = Some(frame.scene_radius);
        let (source, node) = source_route(index, &frame.target)?;
        if source != frame.source
            || source_fingerprint(owner, node)? != frame.source_definition_hash
        {
            return Err(frame_error(
                &frame.target,
                "mesh source changed; remove and reapply this modifier",
            ));
        }
    }
    Ok(())
}

fn validate_numbers(frame: &SceneMeshReferenceFrame) -> Result<(), SceneModifierExpandError> {
    if !frame.scene_radius.is_finite()
        || frame.scene_radius <= 0.0
        || !(frame.scene_radius as f32).is_finite()
        || frame.scene_radius as f32 <= 0.0
        || frame
            .source_offset
            .iter()
            .any(|value| !value.is_finite() || !(*value as f32).is_finite())
        || frame.source_definition_hash.is_empty()
    {
        return Err(frame_error(
            &frame.target,
            "saved frame must have finite representable offsets, a positive radius, and a source fingerprint",
        ));
    }
    Ok(())
}

fn source_route<'a>(
    index: &'a FlatSceneIndex,
    target: &SceneNodeRef,
) -> Result<(SceneNodeRef, &'a EffectGraphNode), SceneModifierExpandError> {
    let wire = index
        .input(target, "vertices")?
        .ok_or_else(|| frame_error(target, "object has no vertices producer"))?;
    let source = index
        .by_id
        .get(&wire.from_node)
        .ok_or_else(|| frame_error(target, "vertices producer has no stable identity"))?;
    let node = index.node(source)?;
    if node.type_id == "node.gltf_skinned_mesh_source"
        || index.flat.wires.iter().any(|wire| wire.to_node == node.id)
    {
        return Err(frame_error(
            target,
            "animated or transformed mesh-source chains require a separately qualified coordinate frame",
        ));
    }
    Ok((source.clone(), node))
}

fn scalar(
    node: &EffectGraphNode,
    key: &str,
    default: f64,
) -> Result<f64, SceneModifierExpandError> {
    let value = match node.params.get(key) {
        None => default,
        Some(SerializedParamValue::Float { value }) => f64::from(*value),
        Some(SerializedParamValue::Int { value }) => f64::from(*value),
        _ => {
            return Err(SceneModifierExpandError::UnsupportedCoordinateFrame {
                path: format!("{}.{}", node.node_id, key),
                detail: "coordinate parameter is not a numeric scalar".into(),
            });
        }
    };
    if !value.is_finite() {
        return Err(SceneModifierExpandError::UnsupportedCoordinateFrame {
            path: format!("{}.{}", node.node_id, key),
            detail: "coordinate parameter is not finite".into(),
        });
    }
    Ok(value)
}

fn static_offset(
    index: &FlatSceneIndex,
    target: &SceneNodeRef,
) -> Result<[f64; 3], SceneModifierExpandError> {
    let wire = index.input(target, "transform")?.ok_or_else(|| {
        frame_error(
            target,
            "fresh capture requires the imported static transform",
        )
    })?;
    let reference = index
        .by_id
        .get(&wire.from_node)
        .ok_or_else(|| frame_error(target, "transform has no stable identity"))?;
    let node = index.node(reference)?;
    if node.type_id != "node.transform_3d"
        || index.flat.wires.iter().any(|wire| wire.to_node == node.id)
    {
        return Err(frame_error(
            target,
            "fresh capture requires an unwired static translation transform",
        ));
    }
    if !matches!(
        node.params.get("billboard"),
        None | Some(SerializedParamValue::Bool { value: false })
    ) {
        return Err(frame_error(
            target,
            "camera-facing transforms cannot define a static source frame",
        ));
    }
    for key in ["rot_x", "rot_y", "rot_z"] {
        if scalar(node, key, 0.0)? != 0.0 {
            return Err(frame_error(
                target,
                "rotated reference frames are not supported for fresh capture",
            ));
        }
    }
    for key in ["scale_x", "scale_y", "scale_z"] {
        if scalar(node, key, 1.0)? != 1.0 {
            return Err(frame_error(
                target,
                "scaled reference frames are not supported for fresh capture",
            ));
        }
    }
    Ok([
        scalar(node, "pos_x", 0.0)?,
        scalar(node, "pos_y", 0.0)?,
        scalar(node, "pos_z", 0.0)?,
    ])
}

fn scene_radius(
    owner: &EffectGraphDef,
    index: &FlatSceneIndex,
    scene: &SceneNodeRef,
) -> Result<f64, SceneModifierExpandError> {
    let radius = if let Some((min, max)) = owner
        .preset_metadata
        .as_ref()
        .and_then(|metadata| metadata.scene_bounds)
    {
        let mut squared = 0.0;
        for axis in 0..3 {
            if !min[axis].is_finite() || !max[axis].is_finite() || min[axis] > max[axis] {
                return Err(frame_error(
                    scene,
                    "imported scene bounds must be finite and ordered",
                ));
            }
            let half_extent = (f64::from(max[axis]) - f64::from(min[axis])) * 0.5;
            squared += half_extent * half_extent;
        }
        squared.sqrt()
    } else {
        let mut radius = 0.0_f64;
        for node in &index.flat.nodes {
            if node.type_id == "node.gltf_mesh_source" {
                radius = radius.max(scalar(node, "source_bbox_radius", 0.0)?);
            }
        }
        radius
    };
    if !radius.is_finite() || radius <= 0.0 || !(radius as f32).is_finite() || radius as f32 <= 0.0
    {
        return Err(frame_error(
            scene,
            "a finite positive imported source radius is required",
        ));
    }
    Ok(radius)
}

/// Hash source-defining data, excluding labels, document IDs, display
/// provenance, capacity and downstream object placement. No asset I/O occurs.
fn source_fingerprint(
    owner: &EffectGraphDef,
    node: &EffectGraphNode,
) -> Result<String, SceneModifierExpandError> {
    scene_source_definition_hash(owner, node).map_err(|error| match error {
        SceneSourceIdentityError::ConflictingAssetBindings { node_id } => {
            SceneModifierExpandError::ConflictingSource {
                path: node_id,
                detail: "mesh source has conflicting asset bindings".into(),
            }
        }
        SceneSourceIdentityError::Encoding { node_id, detail } => {
            SceneModifierExpandError::UnsupportedCoordinateFrame {
                path: node_id,
                detail: format!("source definition cannot be fingerprinted: {detail}"),
            }
        }
    })
}
