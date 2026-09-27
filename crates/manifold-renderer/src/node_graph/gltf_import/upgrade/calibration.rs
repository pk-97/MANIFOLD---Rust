use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};
use manifold_core::flatten::flatten_groups;
use manifold_core::scene_source_identity::{effective_source_params, scene_source_definition_hash};

use crate::node_graph::scene_modifier_expand::validate_modifier_mesh_frames;

/// Project traversal upgrades nested modifier definitions separately.
pub(super) fn has_saved_frames(def: &EffectGraphDef) -> bool {
    def.scene_modifiers
        .iter()
        .any(|modifier| !modifier.mesh_frames.is_empty())
}

/// Validate all saved frames before changing any source parameters.
pub(super) fn validate_saved_frames(def: &EffectGraphDef) -> Result<(), String> {
    for modifier in &def.scene_modifiers {
        if !modifier.mesh_frames.is_empty() {
            validate_modifier_mesh_frames(def, modifier).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// Refresh hashes only for the compatibility change represented by an absent
/// source `vertex_colors` parameter becoming a boolean. All other source
/// changes remain invalid and force the staged migration to roll back.
pub(super) fn refresh_hashes(
    before: &EffectGraphDef,
    candidate: &mut EffectGraphDef,
) -> Result<(), String> {
    if before.scene_modifiers.len() != candidate.scene_modifiers.len() {
        return Err("scene modifier stack changed during material upgrade".into());
    }
    for (before_modifier, candidate_modifier) in before
        .scene_modifiers
        .iter()
        .zip(candidate.scene_modifiers.iter_mut())
    {
        if before_modifier.id != candidate_modifier.id
            || before_modifier.scene != candidate_modifier.scene
            || before_modifier.targets != candidate_modifier.targets
            || before_modifier.legacy_math_view_carrier
                != candidate_modifier.legacy_math_view_carrier
        {
            return Err(format!(
                "scene modifier '{}' changed during material upgrade",
                before_modifier.id
            ));
        }
    }
    refresh_owner_hashes(before, candidate)
}

fn refresh_owner_hashes(
    before: &EffectGraphDef,
    candidate: &mut EffectGraphDef,
) -> Result<(), String> {
    if before
        .scene_modifiers
        .iter()
        .all(|modifier| modifier.mesh_frames.is_empty())
    {
        return Ok(());
    }

    let before_sources = FlattenedSources::build(before)?;
    let candidate_sources = FlattenedSources::build(candidate)?;
    let mut refreshed = Vec::new();
    for (modifier_index, before_modifier) in before.scene_modifiers.iter().enumerate() {
        let candidate_modifier = &candidate.scene_modifiers[modifier_index];
        if before_modifier.mesh_frames.len() != candidate_modifier.mesh_frames.len() {
            return Err(format!(
                "scene modifier '{}' frame count changed during material upgrade",
                before_modifier.id
            ));
        }
        for (frame_index, (before_frame, candidate_frame)) in before_modifier
            .mesh_frames
            .iter()
            .zip(candidate_modifier.mesh_frames.iter())
            .enumerate()
        {
            if before_frame.target != candidate_frame.target
                || before_frame.source != candidate_frame.source
                || before_frame.source_offset != candidate_frame.source_offset
                || before_frame.scene_radius != candidate_frame.scene_radius
            {
                return Err(format!(
                    "calibrated frame for '{}' changed during material upgrade",
                    before_frame.target.node
                ));
            }

            let before_node = before_sources.node(&before_frame.source.node)?;
            let candidate_node = candidate_sources.node(&candidate_frame.source.node)?;
            if before_node.type_id != candidate_node.type_id {
                return Err(format!(
                    "source '{}' changed type during material upgrade",
                    before_frame.source.node
                ));
            }
            let before_params =
                effective_source_params(before, before_node).map_err(|error| error.to_string())?;
            let candidate_params = effective_source_params(candidate, candidate_node)
                .map_err(|error| error.to_string())?;
            let before_hash = scene_source_definition_hash(before, before_node)
                .map_err(|error| error.to_string())?;
            if before_frame.source_definition_hash != before_hash {
                return Err(format!(
                    "calibrated frame for '{}' has a stale source fingerprint",
                    before_frame.target.node
                ));
            }

            if before_params == candidate_params {
                continue;
            }
            if !vertex_colors_only_change(&before_params, &candidate_params) {
                return Err(format!(
                    "source '{}' changed beyond legacy vertex_colors compatibility",
                    before_frame.source.node
                ));
            }
            refreshed.push((
                modifier_index,
                frame_index,
                scene_source_definition_hash(candidate, candidate_node)
                    .map_err(|error| error.to_string())?,
            ));
        }
    }
    for (modifier_index, frame_index, hash) in refreshed {
        candidate.scene_modifiers[modifier_index].mesh_frames[frame_index].source_definition_hash =
            hash;
    }
    Ok(())
}

struct FlattenedSources {
    def: EffectGraphDef,
    by_id: BTreeMap<String, usize>,
}

impl FlattenedSources {
    fn build(def: &EffectGraphDef) -> Result<Self, String> {
        let scratch = EffectGraphDef {
            version: def.version,
            name: None,
            description: None,
            // Flatten geometry only; authored modifier bindings are resolved
            // against the owning def when its source hashes are calculated.
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: def.nodes.clone(),
            wires: def.wires.clone(),
        };
        let flat = flatten_groups(&scratch).map_err(|error| error.to_string())?;
        let mut by_id = BTreeMap::new();
        for (index, node) in flat.nodes.iter().enumerate().filter(|(_, node)| {
            matches!(
                node.type_id.as_str(),
                "node.gltf_mesh_source" | "node.gltf_skinned_mesh_source"
            )
        }) {
            if by_id.insert(node.node_id.to_string(), index).is_some() {
                return Err(format!(
                    "source node '{}' is ambiguous after flattening",
                    node.node_id
                ));
            }
        }
        Ok(Self { def: flat, by_id })
    }

    fn node(&self, node_id: &str) -> Result<&EffectGraphNode, String> {
        self.by_id
            .get(node_id)
            .and_then(|index| self.def.nodes.get(*index))
            .ok_or_else(|| format!("source node '{node_id}' is missing after flattening"))
    }
}

fn vertex_colors_only_change(
    before: &BTreeMap<String, SerializedParamValue>,
    candidate: &BTreeMap<String, SerializedParamValue>,
) -> bool {
    if before.contains_key("vertex_colors") {
        return false;
    }
    let Some(SerializedParamValue::Bool { .. }) = candidate.get("vertex_colors") else {
        return false;
    };
    before
        .iter()
        .all(|(key, value)| candidate.get(key) == Some(value))
        && candidate.len() == before.len() + 1
}
