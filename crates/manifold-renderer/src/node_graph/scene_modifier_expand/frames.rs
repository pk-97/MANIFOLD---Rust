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

use super::{SceneModifierExpandError, index::FlatSceneIndex};

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
            recipe
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
            let source_offset = static_offset(&index, &target)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::NodeId;
    use manifold_core::effect_graph_def::BindingTarget;
    use manifold_core::scene_modifier_preset::{
        SceneModifierRecipe, SceneModifierStageDef, SceneStageInput, SceneStageScope,
    };

    fn fixture() -> (EffectGraphDef, SceneModifierInstanceDef) {
        let mut owner: EffectGraphDef = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/scene-modifiers/nested_multimaterial_v2.json"
        )))
        .unwrap();
        for group in owner
            .nodes
            .iter_mut()
            .filter_map(|node| node.group.as_mut())
        {
            let mesh = group
                .nodes
                .iter_mut()
                .find(|node| node.type_id == "node.cube_mesh")
                .unwrap();
            mesh.type_id = "node.gltf_mesh_source".into();
            mesh.params.insert(
                "path".into(),
                SerializedParamValue::String {
                    value: "scan.glb".into(),
                },
            );
            mesh.params.insert(
                "source_bbox_radius".into(),
                SerializedParamValue::Float { value: 3.0 },
            );
            let transform = group
                .nodes
                .iter_mut()
                .find(|node| node.type_id == "node.transform_3d")
                .unwrap();
            transform.params.retain(|key, _| key.starts_with("pos_"));
        }
        let mut graph = owner.clone();
        graph.version = 3;
        graph.nodes.clear();
        graph.wires.clear();
        graph.preset_metadata.as_mut().unwrap().scene_modifier = Some(SceneModifierRecipe {
            schema_version: 1,
            singleton: false,
            enabled_param: "enabled".into(),
            preparation_params: vec![],
            initializers: vec![],
            calibrations: vec![],
            stages: vec![SceneModifierStageDef {
                group: NodeId::new("deform"),
                scope: SceneStageScope::EachObject,
                inputs: vec![SceneStageInput {
                    port: "radius".into(),
                    source: SceneStageSource::Context {
                        value: SceneContextValue::SceneRadius,
                    },
                }],
                outputs: vec![],
            }],
        });
        let instance = SceneModifierInstanceDef {
            id: NodeId::new("modifier"),
            scene: SceneNodeRef {
                scope: vec![],
                node: NodeId::new("scan_render"),
            },
            targets: SceneTargetSelection::AllObjects,
            mesh_frames: vec![],
            graph: Box::new(graph),
        };
        (owner, instance)
    }

    #[test]
    fn scene_modifier_coordinate_context_preserves_saved_frame_on_motion_rebuild_and_reopen() {
        let (mut owner, mut instance) = fixture();
        instance.mesh_frames = resolve_modifier_mesh_frames(&owner, &instance).unwrap();
        assert_eq!(instance.mesh_frames.len(), 2);
        assert_eq!(instance.mesh_frames[0].scene_radius, 41.0_f64.sqrt() / 2.0);
        assert_eq!(instance.mesh_frames[0].source_offset[0], -0.75);
        let saved = instance.mesh_frames.clone();
        let group = owner
            .nodes
            .iter_mut()
            .find_map(|node| node.group.as_mut())
            .unwrap();
        let transform = group
            .nodes
            .iter_mut()
            .find(|node| node.type_id == "node.transform_3d")
            .unwrap();
        transform
            .params
            .insert("pos_x".into(), SerializedParamValue::Float { value: 50.0 });
        transform
            .params
            .insert("rot_y".into(), SerializedParamValue::Float { value: 1.0 });
        owner.preset_metadata.as_mut().unwrap().scene_bounds = None;
        let instance: SceneModifierInstanceDef =
            serde_json::from_str(&serde_json::to_string(&instance).unwrap()).unwrap();
        validate_modifier_mesh_frames(&owner, &instance).unwrap();
        assert_eq!(
            resolve_modifier_mesh_frames(&owner, &instance).unwrap(),
            saved
        );
    }

    #[test]
    fn scene_modifier_coordinate_context_retarget_keeps_radius_and_survivors() {
        let (mut owner, mut instance) = fixture();
        let all = resolve_modifier_mesh_frames(&owner, &instance).unwrap();
        instance.targets = SceneTargetSelection::Explicit {
            objects: vec![all[0].target.clone()],
        };
        instance.mesh_frames = resolve_modifier_mesh_frames(&owner, &instance).unwrap();
        owner.preset_metadata.as_mut().unwrap().scene_bounds = Some(([-100.0; 3], [100.0; 3]));
        instance.targets = SceneTargetSelection::AllObjects;
        assert!(validate_modifier_mesh_frames(&owner, &instance).is_err());
        let result = resolve_modifier_mesh_frames(&owner, &instance).unwrap();
        assert_eq!(result, all);
        instance.mesh_frames = result;
        instance.targets = SceneTargetSelection::Explicit {
            objects: vec![all[1].target.clone()],
        };
        assert_eq!(
            resolve_modifier_mesh_frames(&owner, &instance).unwrap(),
            vec![all[1].clone()]
        );
    }

    #[test]
    fn scene_modifier_coordinate_context_rejects_source_change_without_mutation() {
        let (mut owner, mut instance) = fixture();
        instance.mesh_frames = resolve_modifier_mesh_frames(&owner, &instance).unwrap();
        let snapshot = instance.clone();
        let mesh = owner
            .nodes
            .iter_mut()
            .find_map(|node| node.group.as_mut())
            .unwrap()
            .nodes
            .iter_mut()
            .find(|node| node.type_id == "node.gltf_mesh_source")
            .unwrap();
        mesh.params.insert(
            "translate_x".into(),
            SerializedParamValue::Float { value: 1.0 },
        );
        assert!(
            resolve_modifier_mesh_frames(&owner, &instance)
                .unwrap_err()
                .to_string()
                .contains("source changed")
        );
        assert_eq!(instance, snapshot);
    }

    #[test]
    fn scene_modifier_coordinate_context_rejects_invalid_bounds_and_fresh_rotation() {
        let (mut owner, instance) = fixture();
        owner.preset_metadata.as_mut().unwrap().scene_bounds = Some(([1.0; 3], [-1.0; 3]));
        assert!(resolve_modifier_mesh_frames(&owner, &instance).is_err());
        owner.preset_metadata.as_mut().unwrap().scene_bounds = None;
        assert_eq!(
            resolve_modifier_mesh_frames(&owner, &instance).unwrap()[0].scene_radius,
            3.0
        );
        let transform = owner
            .nodes
            .iter_mut()
            .find_map(|node| node.group.as_mut())
            .unwrap()
            .nodes
            .iter_mut()
            .find(|node| node.type_id == "node.transform_3d")
            .unwrap();
        transform
            .params
            .insert("rot_y".into(), SerializedParamValue::Float { value: 0.1 });
        assert!(resolve_modifier_mesh_frames(&owner, &instance).is_err());
    }

    #[test]
    fn scene_modifier_coordinate_context_fingerprint_ignores_labels_but_tracks_bound_asset() {
        let (mut owner, mut instance) = fixture();
        instance.mesh_frames = resolve_modifier_mesh_frames(&owner, &instance).unwrap();
        owner.nodes[3].handle = Some("Renamed group".into());
        validate_modifier_mesh_frames(&owner, &instance).unwrap();
        owner
            .preset_metadata
            .as_mut()
            .unwrap()
            .string_bindings
            .push(manifold_core::effect_graph_def::StringBindingDef {
                id: "model_file".into(),
                label: "File".into(),
                default_value: "different.glb".into(),
                target: BindingTarget::Node {
                    node_id: NodeId::new("left_mesh"),
                    param: "path".into(),
                },
            });
        assert!(validate_modifier_mesh_frames(&owner, &instance).is_err());
    }
}
