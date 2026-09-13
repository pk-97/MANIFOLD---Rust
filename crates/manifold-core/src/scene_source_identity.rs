//! Pure identity hashing for authored scene mesh sources.
//!
//! The identity covers only the source definition: its primitive type, its
//! serialized parameters, and the effective bound asset path. Graph layout,
//! node identity, display labels, provenance, and calibration capacity do not
//! participate in the digest.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use sha2::{Digest, Sha256};

use crate::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, GroupDef, SerializedParamValue,
};
use crate::scene_modifier_preset::SceneNodeRef;

/// Failure while deriving a source definition identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneSourceIdentityError {
    /// More than one binding supplies a different effective path for the
    /// source node.
    ConflictingAssetBindings { node_id: String },
    /// The source definition could not be serialized for hashing.
    Encoding { node_id: String, detail: String },
}

impl fmt::Display for SceneSourceIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConflictingAssetBindings { node_id } => {
                write!(f, "{node_id}: mesh source has conflicting asset bindings")
            }
            Self::Encoding { node_id, detail } => {
                write!(
                    f,
                    "{node_id}: source definition cannot be fingerprinted: {detail}"
                )
            }
        }
    }
}

impl std::error::Error for SceneSourceIdentityError {}

/// Failure while applying an explicit same-asset relocation to calibrated
/// scene sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneSourceRelocationError {
    /// The saved graph cannot be traversed or its source references are not
    /// uniquely resolvable.
    InvalidGraph { detail: String },
    /// A saved frame's source definition no longer matches its calibrated
    /// identity, so relocation must refuse to launder stale calibration.
    StaleSourceHash {
        reference: String,
        expected: String,
        actual: String,
    },
    /// Source identity hashing failed while validating the transaction.
    Identity { detail: String },
}

impl fmt::Display for SceneSourceRelocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGraph { detail } => write!(f, "invalid scene source graph: {detail}"),
            Self::StaleSourceHash {
                reference,
                expected,
                actual,
            } => write!(
                f,
                "{reference}: saved source identity {expected} does not match current {actual}"
            ),
            Self::Identity { detail } => write!(f, "scene source identity failed: {detail}"),
        }
    }
}

impl std::error::Error for SceneSourceRelocationError {}

/// Hash source-defining data with the stable scene-frame identity semantics.
///
/// Parameters are serialized in sorted key order, as provided by the
/// [`BTreeMap`], after removing runtime calibration/capacity fields. A bound
/// string parameter's effective spec default takes precedence over its
/// binding default. Distinct effective bindings for the same `path` target
/// are rejected rather than choosing an arbitrary asset.
pub fn scene_source_definition_hash(
    owner: &EffectGraphDef,
    node: &EffectGraphNode,
) -> Result<String, SceneSourceIdentityError> {
    let params = effective_source_params(owner, node)?;
    let bytes = serde_json::to_vec(&(node.type_id.as_str(), params)).map_err(|error| {
        SceneSourceIdentityError::Encoding {
            node_id: node.node_id.to_string(),
            detail: error.to_string(),
        }
    })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// Authored source selectors used by the calibration fingerprint, including
/// the effective asset path. Runtime statistics and buffer capacity are omitted.
pub fn effective_source_params(
    owner: &EffectGraphDef,
    node: &EffectGraphNode,
) -> Result<BTreeMap<String, SerializedParamValue>, SceneSourceIdentityError> {
    let mut params: BTreeMap<String, SerializedParamValue> = node
        .params
        .iter()
        .filter(|(key, _)| {
            !matches!(
                key.as_str(),
                "source_vertex_count" | "source_bbox_radius" | "max_capacity"
            )
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();

    if let Some(metadata) = &owner.preset_metadata {
        let mut path: Option<&str> = None;
        for binding in &metadata.string_bindings {
            if matches!(
                &binding.target,
                BindingTarget::Node { node_id, param }
                    if *node_id == node.node_id && param == "path"
            ) {
                let value = metadata
                    .string_params
                    .iter()
                    .find(|param| param.id == binding.id)
                    .map_or(binding.default_value.as_str(), |param| {
                        param.default_value.as_str()
                    });
                if path.is_some_and(|previous| previous != value) {
                    return Err(SceneSourceIdentityError::ConflictingAssetBindings {
                        node_id: node.node_id.to_string(),
                    });
                }
                path = Some(value);
            }
        }
        if let Some(path) = path {
            params.insert(
                "path".into(),
                SerializedParamValue::String { value: path.into() },
            );
        }
    }

    Ok(params)
}

const MAX_RELOCATION_DEPTH: usize = 64;
const MAX_RELOCATION_NODES: usize = 65_536;

#[derive(Debug, Clone)]
struct RelocationSource {
    reference: SceneNodeRef,
    leaf_node_id: crate::NodeId,
    before_params: BTreeMap<String, SerializedParamValue>,
}

/// Relocate a saved calibrated source from `old` to `new` without recalibrating.
///
/// The operation is transactional: all source references, hashes, and graph
/// routes are validated before the cloned result is returned. A stale saved
/// hash is an error, while a graph with no calibrated source whose effective
/// path is `old` returns `Ok(None)`.
pub fn relocate_scene_source_asset(
    owner: &EffectGraphDef,
    old: &str,
    new: &str,
) -> Result<Option<EffectGraphDef>, SceneSourceRelocationError> {
    if old == new {
        return Ok(None);
    }

    let mut frame_refs: BTreeMap<SceneNodeRef, Vec<(usize, usize)>> = BTreeMap::new();
    for (modifier_index, modifier) in owner.scene_modifiers.iter().enumerate() {
        for (frame_index, frame) in modifier.mesh_frames.iter().enumerate() {
            frame_refs
                .entry(frame.source.clone())
                .or_default()
                .push((modifier_index, frame_index));
        }
    }
    if frame_refs.is_empty() {
        return Ok(None);
    }

    let mut authored_nodes = 0;
    let mut leaf_ids = BTreeMap::new();
    collect_relocation_nodes(&owner.nodes, &[], 0, &mut authored_nodes, &mut leaf_ids)?;
    let flat = flatten_relocation_graph(owner)?;
    let mut flat_by_id = BTreeMap::<String, &EffectGraphNode>::new();
    for node in &flat.nodes {
        if node.node_id.is_empty() {
            continue;
        }
        if flat_by_id.insert(node.node_id.to_string(), node).is_some() {
            return Err(SceneSourceRelocationError::InvalidGraph {
                detail: format!(
                    "flattened leaf NodeId '{}' resolves more than once",
                    node.node_id
                ),
            });
        }
    }

    let mut sources = Vec::new();
    for reference in frame_refs.keys() {
        let leaf = resolve_relocation_reference(&owner.nodes, reference)?;
        let Some(flat_node) = flat_by_id.get(leaf.node_id.as_str()) else {
            return Err(SceneSourceRelocationError::InvalidGraph {
                detail: format!(
                    "source reference '{}' has no flattened leaf",
                    reference_path(reference)
                ),
            });
        };
        let before_params = effective_source_params(owner, flat_node)
            .map_err(scene_source_relocation_identity_error)?;
        if source_path(&before_params) != Some(old) {
            continue;
        }
        let actual = scene_source_definition_hash(owner, flat_node)
            .map_err(scene_source_relocation_identity_error)?;
        for (modifier_index, frame_index) in &frame_refs[reference] {
            let frame = &owner.scene_modifiers[*modifier_index].mesh_frames[*frame_index];
            if frame.source_definition_hash != actual {
                return Err(SceneSourceRelocationError::StaleSourceHash {
                    reference: reference_path(reference),
                    expected: frame.source_definition_hash.clone(),
                    actual,
                });
            }
        }
        sources.push(RelocationSource {
            reference: reference.clone(),
            leaf_node_id: leaf.node_id.clone(),
            before_params,
        });
    }
    if sources.is_empty() {
        return Ok(None);
    }

    let mut relocated = owner.clone();
    let mut affected_ids = BTreeSet::new();
    for source in &sources {
        affected_ids.insert(source.leaf_node_id.to_string());
        relocate_source_path(
            &mut relocated.nodes,
            &source.reference.scope,
            &source.leaf_node_id,
            old,
            new,
        )?;
    }
    relocate_metadata_paths(&mut relocated, &affected_ids, old, new);

    let relocated_flat = flatten_relocation_graph(&relocated)?;
    let mut relocated_by_id = BTreeMap::<String, &EffectGraphNode>::new();
    for node in &relocated_flat.nodes {
        if node.node_id.is_empty() {
            continue;
        }
        if relocated_by_id
            .insert(node.node_id.to_string(), node)
            .is_some()
        {
            return Err(SceneSourceRelocationError::InvalidGraph {
                detail: format!(
                    "relocated leaf NodeId '{}' resolves more than once",
                    node.node_id
                ),
            });
        }
    }
    let mut refreshed_hashes = BTreeMap::new();
    for source in &sources {
        let Some(after_node) = relocated_by_id.get(source.leaf_node_id.as_str()) else {
            return Err(SceneSourceRelocationError::InvalidGraph {
                detail: format!(
                    "source reference '{}' disappeared after relocation",
                    reference_path(&source.reference)
                ),
            });
        };
        let after_params = effective_source_params(&relocated, after_node)
            .map_err(scene_source_relocation_identity_error)?;
        let mut expected_params = source.before_params.clone();
        expected_params.insert(
            "path".into(),
            SerializedParamValue::String {
                value: new.to_string(),
            },
        );
        if after_params != expected_params {
            return Err(SceneSourceRelocationError::InvalidGraph {
                detail: format!(
                    "relocation changed source parameters beyond path for '{}'",
                    reference_path(&source.reference)
                ),
            });
        }
        let hash = scene_source_definition_hash(&relocated, after_node)
            .map_err(scene_source_relocation_identity_error)?;
        refreshed_hashes.insert(source.reference.clone(), hash);
    }

    for modifier in &mut relocated.scene_modifiers {
        for frame in &mut modifier.mesh_frames {
            if let Some(hash) = refreshed_hashes.get(&frame.source) {
                frame.source_definition_hash = hash.clone();
            }
        }
    }
    Ok(Some(relocated))
}

fn scene_source_relocation_identity_error(
    error: SceneSourceIdentityError,
) -> SceneSourceRelocationError {
    SceneSourceRelocationError::Identity {
        detail: error.to_string(),
    }
}

fn source_path(params: &BTreeMap<String, SerializedParamValue>) -> Option<&str> {
    match params.get("path") {
        Some(SerializedParamValue::String { value }) => Some(value),
        _ => None,
    }
}

fn reference_path(reference: &SceneNodeRef) -> String {
    reference
        .scope
        .iter()
        .map(crate::NodeId::as_str)
        .chain(std::iter::once(reference.node.as_str()))
        .collect::<Vec<_>>()
        .join("/")
}

fn collect_relocation_nodes(
    nodes: &[EffectGraphNode],
    scope: &[crate::NodeId],
    depth: usize,
    count: &mut usize,
    leaf_ids: &mut BTreeMap<String, SceneNodeRef>,
) -> Result<(), SceneSourceRelocationError> {
    if depth > MAX_RELOCATION_DEPTH {
        return Err(SceneSourceRelocationError::InvalidGraph {
            detail: format!("group nesting exceeds {MAX_RELOCATION_DEPTH} levels"),
        });
    }
    *count = count.saturating_add(nodes.len());
    if *count > MAX_RELOCATION_NODES {
        return Err(SceneSourceRelocationError::InvalidGraph {
            detail: format!("authored graph exceeds {MAX_RELOCATION_NODES} nodes"),
        });
    }
    for node in nodes {
        if let Some(group) = node.group.as_deref() {
            let mut child_scope = scope.to_vec();
            child_scope.push(node.node_id.clone());
            collect_relocation_nodes(&group.nodes, &child_scope, depth + 1, count, leaf_ids)?;
        } else if !node.node_id.is_empty() {
            let reference = SceneNodeRef {
                scope: scope.to_vec(),
                node: node.node_id.clone(),
            };
            if leaf_ids
                .insert(node.node_id.to_string(), reference.clone())
                .is_some()
            {
                return Err(SceneSourceRelocationError::InvalidGraph {
                    detail: format!("leaf NodeId '{}' resolves more than once", node.node_id),
                });
            }
        }
    }
    Ok(())
}

fn resolve_relocation_reference<'a>(
    nodes: &'a [EffectGraphNode],
    reference: &SceneNodeRef,
) -> Result<&'a EffectGraphNode, SceneSourceRelocationError> {
    let mut current = nodes;
    for scope_id in &reference.scope {
        let matches: Vec<_> = current
            .iter()
            .filter(|node| node.node_id == *scope_id && node.group.is_some())
            .collect();
        if matches.len() != 1 {
            return Err(SceneSourceRelocationError::InvalidGraph {
                detail: format!(
                    "source reference '{}' has {} matching group scopes",
                    reference_path(reference),
                    matches.len()
                ),
            });
        }
        current = &matches[0].group.as_deref().expect("group checked").nodes;
    }
    let matches: Vec<_> = current
        .iter()
        .filter(|node| node.node_id == reference.node && node.group.is_none())
        .collect();
    if matches.len() != 1 {
        return Err(SceneSourceRelocationError::InvalidGraph {
            detail: format!(
                "source reference '{}' has {} matching leaves",
                reference_path(reference),
                matches.len()
            ),
        });
    }
    Ok(matches[0])
}

fn flatten_relocation_graph(
    owner: &EffectGraphDef,
) -> Result<EffectGraphDef, SceneSourceRelocationError> {
    let scratch = EffectGraphDef {
        version: owner.version,
        name: None,
        description: None,
        preset_metadata: None,
        scene_modifiers: Vec::new(),
        nodes: owner.nodes.clone(),
        wires: owner.wires.clone(),
    };
    crate::flatten::flatten_groups(&scratch).map_err(|error| {
        SceneSourceRelocationError::InvalidGraph {
            detail: error.to_string(),
        }
    })
}

fn relocate_source_path(
    nodes: &mut [EffectGraphNode],
    scope: &[crate::NodeId],
    leaf_node_id: &crate::NodeId,
    old: &str,
    new: &str,
) -> Result<(), SceneSourceRelocationError> {
    let mut current = nodes;
    for (depth, scope_id) in scope.iter().enumerate() {
        let matches: Vec<_> = current
            .iter_mut()
            .filter(|node| node.node_id == *scope_id && node.group.is_some())
            .collect();
        if matches.len() != 1 {
            return Err(SceneSourceRelocationError::InvalidGraph {
                detail: format!("source scope '{}' is no longer unique", scope_id),
            });
        }
        let group_node = matches.into_iter().next().expect("unique group checked");
        let group = group_node.group.as_deref_mut().expect("group checked");
        update_group_routes(
            &mut group_node.params,
            group,
            &scope[depth + 1..],
            leaf_node_id,
            old,
            new,
        )?;
        current = &mut group.nodes;
    }
    let matches: Vec<_> = current
        .iter_mut()
        .filter(|node| node.node_id == *leaf_node_id && node.group.is_none())
        .collect();
    if matches.len() != 1 {
        return Err(SceneSourceRelocationError::InvalidGraph {
            detail: format!("source leaf '{}' is no longer unique", leaf_node_id),
        });
    }
    let leaf = matches.into_iter().next().expect("unique leaf checked");
    if let Some(SerializedParamValue::String { value }) = leaf.params.get_mut("path")
        && value == old {
        *value = new.to_string();
    }
    Ok(())
}

fn update_group_routes(
    group_params: &mut BTreeMap<String, SerializedParamValue>,
    group: &mut GroupDef,
    remaining_scope: &[crate::NodeId],
    leaf_node_id: &crate::NodeId,
    old: &str,
    new: &str,
) -> Result<(), SceneSourceRelocationError> {
    let routes: Vec<(String, String)> = group
        .interface
        .params
        .iter()
        .filter(|param| {
            group_param_reaches_source(group, &param.name, remaining_scope, leaf_node_id)
        })
        .map(|param| (param.name.clone(), param.target_param.clone()))
        .collect();
    for (name, _) in routes {
        if let Some(SerializedParamValue::String { value }) = group_params.get_mut(&name)
            && value == old {
            *value = new.to_string();
        }
        if let Some(param) = group
            .interface
            .params
            .iter_mut()
            .find(|param| param.name == name)
            && let Some(SerializedParamValue::String { value }) = param.default.as_mut()
            && value == old {
            *value = new.to_string();
        }
    }
    Ok(())
}

fn group_param_reaches_source(
    group: &GroupDef,
    param_name: &str,
    remaining_scope: &[crate::NodeId],
    leaf_node_id: &crate::NodeId,
) -> bool {
    let Some(param) = group
        .interface
        .params
        .iter()
        .find(|param| param.name == param_name)
    else {
        return false;
    };
    if param.target_param != "path" {
        return false;
    }
    let mut nodes = group.nodes.as_slice();
    let mut handles = Vec::with_capacity(remaining_scope.len() + 1);
    for id in remaining_scope {
        let Some(node) = nodes.iter().find(|node| node.node_id == *id) else {
            return false;
        };
        let Some(handle) = node.handle.as_ref() else {
            return false;
        };
        let Some(body) = node.group.as_deref() else {
            return false;
        };
        handles.push(handle.as_str());
        nodes = &body.nodes;
    }
    let Some(leaf) = nodes.iter().find(|node| node.node_id == *leaf_node_id) else {
        return false;
    };
    let Some(handle) = leaf.handle.as_ref() else {
        return false;
    };
    handles.push(handle);
    param.target_handle == handles.join("/")
}

fn relocate_metadata_paths(
    owner: &mut EffectGraphDef,
    leaf_ids: &BTreeSet<String>,
    old: &str,
    new: &str,
) {
    let Some(metadata) = owner.preset_metadata.as_mut() else {
        return;
    };
    let matching_ids: BTreeSet<String> = metadata
        .string_bindings
        .iter()
        .filter_map(|binding| match &binding.target {
            BindingTarget::Node { node_id, param }
                if param == "path" && leaf_ids.contains(node_id.as_str()) =>
            {
                Some(binding.id.clone())
            }
            _ => None,
        })
        .collect();
    for binding in &mut metadata.string_bindings {
        if matching_ids.contains(&binding.id) && binding.default_value == old {
            binding.default_value = new.to_string();
        }
    }
    for param in &mut metadata.string_params {
        if matching_ids.contains(&param.id) && param.default_value == old {
            param.default_value = new.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_graph_def::{
        BindingTarget, EffectGraphDef, EffectGraphNode, GroupDef, GroupInterface, GroupParamDef,
        PresetMetadata, StringBindingDef, StringParamSpecDef,
    };
    use crate::id::NodeId;
    use crate::scene_modifier_preset::{
        SceneMeshReferenceFrame, SceneModifierInstanceDef, SceneTargetSelection,
    };

    fn node() -> EffectGraphNode {
        EffectGraphNode {
            id: 7,
            node_id: NodeId::new("mesh-source"),
            type_id: "node.gltf_mesh_source".into(),
            handle: Some("Source label".into()),
            params: BTreeMap::from([
                (
                    "path".into(),
                    SerializedParamValue::String {
                        value: "authored.glb".into(),
                    },
                ),
                ("fit".into(), SerializedParamValue::Enum { value: 0 }),
                (
                    "source_vertex_count".into(),
                    SerializedParamValue::Int { value: 12 },
                ),
                (
                    "source_bbox_radius".into(),
                    SerializedParamValue::Float { value: 3.0 },
                ),
                (
                    "max_capacity".into(),
                    SerializedParamValue::Int { value: 64 },
                ),
            ]),
            exposed_params: Default::default(),
            editor_pos: Some((100.0, 200.0)),
            wgsl_source: None,
            title: Some("Provenance title".into()),
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        }
    }

    fn owner() -> EffectGraphDef {
        EffectGraphDef {
            version: 2,
            name: Some("owner".into()),
            description: Some("description".into()),
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![node()],
            wires: Vec::new(),
        }
    }

    fn metadata() -> PresetMetadata {
        PresetMetadata {
            id: crate::preset_type_id::PresetTypeId::from_string("identity-test".into()),
            display_name: "Identity test".into(),
            category: "Diagnostic".into(),
            osc_prefix: "identity_test".into(),
            legacy_discriminant: None,
            available: true,
            is_line_based: false,
            layer_types: None,
            params: Vec::new(),
            bindings: Vec::new(),
            string_params: vec![StringParamSpecDef {
                id: "asset".into(),
                name: "Asset".into(),
                default_value: "spec.glb".into(),
                is_file_picker: true,
                use_dropdown: false,
                is_file_path: true,
            }],
            string_bindings: vec![StringBindingDef {
                id: "asset".into(),
                label: "Asset".into(),
                default_value: "binding.glb".into(),
                target: BindingTarget::Node {
                    node_id: NodeId::new("mesh-source"),
                    param: "path".into(),
                },
            }],
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            scene_bounds: None,
            scene_modifier: None,
        }
    }

    fn frame(source: &[&str], source_node: &str, hash: String) -> SceneMeshReferenceFrame {
        SceneMeshReferenceFrame {
            target: SceneNodeRef {
                scope: Vec::new(),
                node: NodeId::new("target"),
            },
            source: SceneNodeRef {
                scope: source.iter().map(|id| NodeId::new(*id)).collect(),
                node: NodeId::new(source_node),
            },
            source_definition_hash: hash,
            source_offset: [1.0, 2.0, 3.0],
            scene_radius: 4.0,
        }
    }

    fn add_frames(owner: &mut EffectGraphDef, frames: Vec<SceneMeshReferenceFrame>) {
        owner.scene_modifiers.push(SceneModifierInstanceDef {
            id: NodeId::new("modifier"),
            scene: SceneNodeRef {
                scope: Vec::new(),
                node: NodeId::new("scene"),
            },
            targets: SceneTargetSelection::AllObjects,
            mesh_frames: frames,
            graph: Box::new(EffectGraphDef {
                version: 3,
                name: None,
                description: None,
                preset_metadata: None,
                scene_modifiers: Vec::new(),
                nodes: Vec::new(),
                wires: Vec::new(),
            }),
        });
    }

    #[test]
    fn identity_ignores_labels_provenance_and_capacity_fields() {
        let owner = owner();
        let source = node();
        let mut changed = source.clone();
        changed.id = 999;
        changed.node_id = NodeId::new("another-id");
        changed.handle = Some("Another label".into());
        changed.title = Some("Another provenance".into());
        changed.editor_pos = Some((-20.0, 4.0));
        changed.params.insert(
            "source_vertex_count".into(),
            SerializedParamValue::Int { value: 9999 },
        );
        changed.params.insert(
            "source_bbox_radius".into(),
            SerializedParamValue::Float { value: 900.0 },
        );
        changed.params.insert(
            "max_capacity".into(),
            SerializedParamValue::Int { value: 9999 },
        );
        assert_eq!(
            scene_source_definition_hash(&owner, &source).unwrap(),
            scene_source_definition_hash(&owner, &changed).unwrap()
        );
    }

    #[test]
    fn asset_and_static_selector_changes_alter_identity() {
        let owner = owner();
        let source = node();
        let original = scene_source_definition_hash(&owner, &source).unwrap();
        let mut asset = source.clone();
        asset.params.insert(
            "path".into(),
            SerializedParamValue::String {
                value: "other.glb".into(),
            },
        );
        assert_ne!(
            original,
            scene_source_definition_hash(&owner, &asset).unwrap()
        );
        let mut selector = source;
        selector
            .params
            .insert("fit".into(), SerializedParamValue::Enum { value: 1 });
        assert_ne!(
            original,
            scene_source_definition_hash(&owner, &selector).unwrap()
        );
    }

    #[test]
    fn string_spec_default_takes_precedence_over_binding_default() {
        let mut owner = owner();
        owner.preset_metadata = Some(metadata());
        let mut source = node();
        source.params.insert(
            "path".into(),
            SerializedParamValue::String {
                value: "node.glb".into(),
            },
        );
        let mut spec_default = source.clone();
        spec_default.params.insert(
            "path".into(),
            SerializedParamValue::String {
                value: "spec.glb".into(),
            },
        );
        assert_eq!(
            scene_source_definition_hash(&owner, &source).unwrap(),
            scene_source_definition_hash(&owner, &spec_default).unwrap()
        );
        let mut binding_default = owner.clone();
        binding_default
            .preset_metadata
            .as_mut()
            .unwrap()
            .string_params
            .clear();
        let binding_hash = scene_source_definition_hash(&binding_default, &source).unwrap();
        assert_ne!(
            scene_source_definition_hash(&owner, &source).unwrap(),
            binding_hash
        );
    }

    #[test]
    fn conflicting_asset_bindings_fail_with_typed_error() {
        let mut owner = owner();
        let mut metadata = metadata();
        metadata.string_bindings.push(StringBindingDef {
            id: "other".into(),
            label: "Other".into(),
            default_value: "other.glb".into(),
            target: BindingTarget::Node {
                node_id: NodeId::new("mesh-source"),
                param: "path".into(),
            },
        });
        owner.preset_metadata = Some(metadata);
        assert!(matches!(
            scene_source_definition_hash(&owner, &node()),
            Err(SceneSourceIdentityError::ConflictingAssetBindings { .. })
        ));
    }

    #[test]
    fn scene_source_relocation_updates_shared_frames_atomically_and_preserves_calibration() {
        let mut owner = owner();
        let hash = scene_source_definition_hash(&owner, &owner.nodes[0]).unwrap();
        add_frames(
            &mut owner,
            vec![
                frame(&[], "mesh-source", hash.clone()),
                frame(&[], "mesh-source", hash),
            ],
        );
        let before = owner.clone();
        let relocated = relocate_scene_source_asset(&owner, "authored.glb", "new.glb")
            .unwrap()
            .expect("matching calibrated source");
        assert_eq!(owner, before);
        assert_eq!(
            relocated.nodes[0].params["path"],
            SerializedParamValue::String {
                value: "new.glb".into()
            }
        );
        let refreshed = scene_source_definition_hash(&relocated, &relocated.nodes[0]).unwrap();
        assert!(
            relocated.scene_modifiers[0]
                .mesh_frames
                .iter()
                .all(|frame| frame.source_definition_hash == refreshed)
        );
        assert_eq!(
            relocated.scene_modifiers[0].mesh_frames[0].source_offset,
            [1.0, 2.0, 3.0]
        );
        assert_eq!(
            relocated.scene_modifiers[0].mesh_frames[0].scene_radius,
            4.0
        );
    }

    #[test]
    fn scene_source_relocation_updates_group_interface_default_and_override() {
        let mut owner = owner();
        let mut group = EffectGraphNode {
            id: 10,
            node_id: NodeId::new("group"),
            type_id: "group".into(),
            handle: Some("Group".into()),
            params: BTreeMap::from([(
                "asset".into(),
                SerializedParamValue::String {
                    value: "old.glb".into(),
                },
            )]),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: Some(Box::new(GroupDef {
                interface: GroupInterface {
                    inputs: Vec::new(),
                    outputs: Vec::new(),
                    params: vec![GroupParamDef {
                        name: "asset".into(),
                        target_handle: "mesh".into(),
                        target_param: "path".into(),
                        default: Some(SerializedParamValue::String {
                            value: "old.glb".into(),
                        }),
                    }],
                },
                nodes: vec![node()],
                wires: Vec::new(),
                tint: None,
            })),
        };
        group.group.as_mut().unwrap().nodes[0].handle = Some("mesh".into());
        owner.nodes = vec![group];
        let flat = flatten_relocation_graph(&owner).unwrap();
        let hash = scene_source_definition_hash(&owner, &flat.nodes[0]).unwrap();
        add_frames(&mut owner, vec![frame(&["group"], "mesh-source", hash)]);
        let relocated = relocate_scene_source_asset(&owner, "old.glb", "new.glb")
            .unwrap()
            .unwrap();
        let group = &relocated.nodes[0];
        assert_eq!(
            group.params["asset"],
            SerializedParamValue::String {
                value: "new.glb".into()
            }
        );
        assert_eq!(
            group.group.as_ref().unwrap().interface.params[0].default,
            Some(SerializedParamValue::String {
                value: "new.glb".into()
            })
        );
    }

    #[test]
    fn scene_source_relocation_updates_metadata_aliases_and_leaves_unrelated_asset() {
        let mut owner = owner();
        owner.preset_metadata = Some(metadata());
        let hash = scene_source_definition_hash(&owner, &owner.nodes[0]).unwrap();
        add_frames(&mut owner, vec![frame(&[], "mesh-source", hash)]);
        let mut unrelated = node();
        unrelated.node_id = NodeId::new("other-source");
        unrelated.params.insert(
            "path".into(),
            SerializedParamValue::String {
                value: "other.glb".into(),
            },
        );
        owner.nodes.push(unrelated);
        let relocated = relocate_scene_source_asset(&owner, "spec.glb", "new.glb")
            .unwrap()
            .unwrap();
        let metadata = relocated.preset_metadata.unwrap();
        assert_eq!(metadata.string_params[0].default_value, "new.glb");
        assert_eq!(
            metadata.string_bindings[0].default_value, "binding.glb",
            "a shadowed different path is not the explicitly relocated asset"
        );
        assert_eq!(
            relocated.nodes[1].params["path"],
            SerializedParamValue::String {
                value: "other.glb".into()
            }
        );
    }

    #[test]
    fn scene_source_relocation_rejects_stale_hash_and_selector_change_without_mutation() {
        let mut owner = owner();
        let hash = scene_source_definition_hash(&owner, &owner.nodes[0]).unwrap();
        add_frames(&mut owner, vec![frame(&[], "mesh-source", hash)]);
        owner.nodes[0]
            .params
            .insert("fit".into(), SerializedParamValue::Enum { value: 1 });
        let before = owner.clone();
        assert!(matches!(
            relocate_scene_source_asset(&owner, "authored.glb", "new.glb"),
            Err(SceneSourceRelocationError::StaleSourceHash { .. })
        ));
        assert_eq!(owner, before);
    }
}
