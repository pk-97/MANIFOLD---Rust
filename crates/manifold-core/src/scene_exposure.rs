//! Scene-panel exposure convergence — P1 stamping helpers.
//!
//! `docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md` P1: every scene-vocabulary
//! node (transform, material, light, camera, atmosphere, bake_environment,
//! modifiers, scene_object.visible) gets all of its params exposed as outer-card
//! sliders at creation time. This module is the crate-neutral stamping surface:
//! it knows the `EffectGraphDef` shape but not renderer `ParamDef`s, so the
//! metadata source is injected by callers.

use crate::NodeId;
use crate::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, ParamSpecDef, PresetMetadata,
    SerializedParamValue,
};
use crate::effects::ParamConvert;

/// Metadata for one inner-node parameter, produced from the primitive's own
/// `ParamDef` by the renderer-side provider.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneParamMetadata {
    pub name: String,
    pub label: String,
    pub min: f32,
    pub max: f32,
    pub default_value: SerializedParamValue,
    pub is_angle: bool,
    pub whole_numbers: bool,
    pub is_toggle: bool,
    pub is_trigger: bool,
    pub value_labels: Vec<String>,
    pub convert: ParamConvert,
}

/// Source of per-type param metadata used by the creation-site commands and the
/// load-time migration. Implemented by `manifold_renderer` using
/// `PrimitiveRegistry`.
pub trait SceneExposureMetadataProvider: Send + Sync {
    /// Return the full param manifest for `type_id`, in the order it should
    /// appear on the card. Empty when the type is unknown or has no exposed
    /// params.
    fn metadata_for_type(&self, type_id: &str) -> Vec<SceneParamMetadata>;
}

/// Stamp card exposures for every param in `params_metadata` onto the node with
/// document id `node_doc_id`, grouping them under `section`. Idempotent: a
/// binding already targeting `(node_id, param)` is left untouched.
///
/// Returns `true` iff any new exposure was added.
pub fn stamp_scene_node_exposures(
    def: &mut EffectGraphDef,
    node_doc_id: u32,
    section: &str,
    params_metadata: &[SceneParamMetadata],
) -> bool {
    let Some(node) = def.nodes.iter().find(|n| n.id == node_doc_id) else {
        return false;
    };
    let node_id = node.node_id.clone();

    let meta = def.preset_metadata.get_or_insert_with(|| PresetMetadata {
        id: crate::PresetTypeId::from_string("__scene_exposure__".to_string()),
        display_name: String::new(),
        category: String::new(),
        osc_prefix: String::new(),
        legacy_discriminant: None,
        available: true,
        is_line_based: false,
        params: Vec::new(),
        bindings: Vec::new(),
        skip_mode: crate::effect_graph_def::SkipModeDef::default(),
        param_aliases: Vec::new(),
        value_aliases: Vec::new(),
        string_params: Vec::new(),
        string_bindings: Vec::new(),
    });

    stamp_scene_node_exposures_into(
        &mut meta.params,
        &mut meta.bindings,
        node_doc_id,
        &node_id,
        section,
        params_metadata,
    )
}

/// Variant for callers that already own the `params`/`bindings` vectors (the
/// glTF importer builds its card surface before attaching it to the def).
pub fn stamp_scene_node_exposures_into(
    params: &mut Vec<ParamSpecDef>,
    bindings: &mut Vec<BindingDef>,
    node_doc_id: u32,
    node_id: &NodeId,
    section: &str,
    params_metadata: &[SceneParamMetadata],
) -> bool {
    if params_metadata.is_empty() {
        return false;
    }

    let existing_targets: std::collections::BTreeSet<(String, String)> = bindings
        .iter()
        .filter_map(|b| match &b.target {
            BindingTarget::Node { node_id: nid, param } => {
                Some((nid.as_str().to_string(), param.clone()))
            }
            _ => None,
        })
        .collect();

    let mut changed = false;
    for meta in params_metadata {
        if existing_targets.contains(&(node_id.as_str().to_string(), meta.name.clone())) {
            continue;
        }

        let base_id = format!("{}_{}", node_doc_id, meta.name);
        let id = unique_param_id(params, &base_id);

        let default_f32 = serialized_default_as_f32(&meta.default_value);

        params.push(ParamSpecDef {
            id: id.clone(),
            name: meta.label.clone(),
            min: meta.min,
            max: meta.max,
            default_value: default_f32,
            whole_numbers: meta.whole_numbers,
            is_toggle: meta.is_toggle,
            is_trigger: meta.is_trigger,
            value_labels: meta.value_labels.clone(),
            format_string: None,
            osc_suffix: String::new(),
            curve: crate::macro_bank::MacroCurve::default(),
            invert: false,
            is_angle: meta.is_angle,
            is_trigger_gate: false,
            wraps: false,
            section: Some(section.to_string()),
        });

        bindings.push(BindingDef {
            id,
            label: meta.label.clone(),
            default_value: default_f32,
            target: BindingTarget::Node {
                node_id: node_id.clone(),
                param: meta.name.clone(),
            },
            convert: meta.convert,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
        });

        changed = true;
    }

    changed
}

fn serialized_default_as_f32(value: &SerializedParamValue) -> f32 {
    match value {
        SerializedParamValue::Float { value } => *value,
        SerializedParamValue::Int { value } => *value as f32,
        SerializedParamValue::Bool { value } => if *value { 1.0 } else { 0.0 },
        SerializedParamValue::Enum { value } => *value as f32,
        _ => 0.0,
    }
}

fn unique_param_id(params: &[ParamSpecDef], base: &str) -> String {
    if !params.iter().any(|p| p.id == base) {
        return base.to_string();
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{}_{}", base, n);
        if !params.iter().any(|p| p.id == candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Walk every node in `def` and stamp exposures for any whose `type_id` is in
/// `vocabulary`. `section_name` is called for each stamped node. Returns `true`
/// iff anything changed.
///
/// The vocabulary and section naming live in the caller (`manifold_renderer`)
/// because this module intentionally has no primitive registry dependency.
pub fn migrate_scene_exposures<F>(
    def: &mut EffectGraphDef,
    vocabulary: &[&str],
    mut section_name: F,
    provider: &dyn SceneExposureMetadataProvider,
) -> bool
where
    F: FnMut(&EffectGraphNode) -> String,
{
    let node_ids: Vec<u32> = def
        .nodes
        .iter()
        .filter(|n| vocabulary.contains(&n.type_id.as_str()))
        .map(|n| n.id)
        .collect();

    let mut changed = false;
    for id in node_ids {
        // Re-find the node to satisfy the borrow checker: `section_name` may
        // need to inspect the node while we mutate `def`.
        let Some(node) = def.nodes.iter().find(|n| n.id == id) else {
            continue;
        };
        let section = section_name(node);
        let type_id = node.type_id.clone();
        let metadata = provider.metadata_for_type(&type_id);
        if stamp_scene_node_exposures(def, id, &section, &metadata) {
            changed = true;
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn make_node(id: u32, type_id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: NodeId::new(format!("n{id}")),
            type_id: type_id.to_string(),
            handle: Some(format!("node{id}")),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        }
    }

    fn float_meta(name: &str, label: &str) -> SceneParamMetadata {
        SceneParamMetadata {
            name: name.to_string(),
            label: label.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: SerializedParamValue::Float { value: 0.5 },
            is_angle: false,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            convert: ParamConvert::Float,
        }
    }

    #[test]
    fn stamps_exposures_and_creates_metadata() {
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![make_node(7, "node.light")],
            wires: vec![],
        };

        let changed = stamp_scene_node_exposures(
            &mut def,
            7,
            "Key Light",
            &[float_meta("intensity", "Intensity"), float_meta("pos_x", "X")],
        );

        assert!(changed);
        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(meta.params.len(), 2);
        assert_eq!(meta.bindings.len(), 2);
        assert_eq!(meta.params[0].section.as_deref(), Some("Key Light"));
        assert_eq!(meta.bindings[0].target, BindingTarget::Node {
            node_id: NodeId::new("n7"),
            param: "intensity".to_string(),
        });
    }

    #[test]
    fn idempotent_second_call_is_no_op() {
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![make_node(7, "node.light")],
            wires: vec![],
        };
        let metadata = vec![float_meta("intensity", "Intensity")];

        assert!(stamp_scene_node_exposures(&mut def, 7, "Key Light", &metadata));
        let after_first = def.clone();
        assert!(!stamp_scene_node_exposures(&mut def, 7, "Key Light", &metadata));
        assert_eq!(def, after_first);
    }

    #[test]
    fn migrate_skips_unknown_nodes_and_is_idempotent() {
        struct TestProvider;
        impl SceneExposureMetadataProvider for TestProvider {
            fn metadata_for_type(&self, type_id: &str) -> Vec<SceneParamMetadata> {
                if type_id == "node.light" {
                    vec![float_meta("intensity", "Intensity")]
                } else {
                    Vec::new()
                }
            }
        }

        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![make_node(5, "node.value"), make_node(7, "node.light")],
            wires: vec![],
        };

        let vocab = ["node.light"];
        let section = |_n: &EffectGraphNode| "Light".to_string();

        assert!(migrate_scene_exposures(&mut def, &vocab, section, &TestProvider));
        let after_first = def.clone();
        assert!(!migrate_scene_exposures(
            &mut def,
            &vocab,
            |_n| "Light".to_string(),
            &TestProvider
        ));
        assert_eq!(def, after_first);
    }
}
