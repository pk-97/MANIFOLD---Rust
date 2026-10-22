//! Card exposure stamping for graph presets that still carry scene-vocabulary
//! nodes (the glTF importer and the GPU FLIP preset). Scene layers do not
//! stamp: their manifest comes from the (kind, property) table in
//! `scene_schema`. Obsolete when the importer builds documents and the water
//! preset is a scene item (lanes L3 and L7).

use std::collections::BTreeMap;

use crate::NodeId;
use crate::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, ParamSpecDef, PresetMetadata, SerializedParamValue,
};
use crate::param_lookup::{NodeParamMetadata, serialized_default_as_f32};


/// A stamped control is on the curated card only when the scene table gives
/// its param the performance role; a type outside the vocabulary stays off.
fn stamped_card_visible(meta: &NodeParamMetadata) -> bool {
    meta.scene
        .is_some_and(|s| s.role == manifold_foundation::scene_surface::SurfaceRole::Performance)
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
    params_metadata: &[NodeParamMetadata],
) -> bool {
    let Some(node) = def.nodes.iter().find(|n| n.id == node_doc_id) else {
        return false;
    };
    let node_id = node.node_id.clone();
    // Cloned rather than borrowed: `meta` below needs a mutable borrow of
    // `def.preset_metadata`, and the node's `params` map is small
    // (edit-time only, never on a hot path).
    let node_params = node.params.clone();

    let meta = def
        .preset_metadata
        .get_or_insert_with(empty_scene_preset_metadata);

    stamp_scene_node_exposures_into(
        &mut meta.params,
        &mut meta.bindings,
        node_doc_id,
        &node_id,
        section,
        params_metadata,
        &node_params,
    )
}

/// The empty `PresetMetadata` shell `stamp_scene_node_exposures` and
/// `migrate_scene_exposures` both lift a `None` `def.preset_metadata` into
/// before extending it. Not a real preset identity — every real generator's
/// catalog default already carries its own `preset_metadata`; this exists so
/// a hand-built def with none doesn't silently drop the new card entries.
pub(crate) fn empty_scene_preset_metadata() -> PresetMetadata {
    PresetMetadata {
        shape: None,
        id: crate::PresetTypeId::from_string("__scene_exposure__".to_string()),
        display_name: String::new(),
        category: String::new(),
        osc_prefix: String::new(),
        legacy_discriminant: None,
        available: true,
        is_line_based: false,
        layer_types: None,
        params: Vec::new(),
        bindings: Vec::new(),
        param_aliases: Vec::new(),
        value_aliases: Vec::new(),
        string_params: Vec::new(),
        string_bindings: Vec::new(),
        scene_modifier: None,
        scene_bounds: None,
    }
}

/// Variant for callers that already own the `params`/`bindings` vectors (the
/// glTF importer builds its card surface before attaching it to the def).
///
/// `node_params` is the target node's stamped param overrides
/// (`EffectGraphNode.params`) at stamping time. Each exposure's default is
/// seeded from the node's stamped value for that param when present, falling
/// back to the primitive manifest default (`meta.default_value`) otherwise —
/// this is what keeps an importer-placed object's position/material/framing
/// from being clobbered back to the generic primitive default at bind time
/// (`apply_binding_defaults`, BUG-303).
pub fn stamp_scene_node_exposures_into(
    params: &mut Vec<ParamSpecDef>,
    bindings: &mut Vec<BindingDef>,
    node_doc_id: u32,
    node_id: &NodeId,
    section: &str,
    params_metadata: &[NodeParamMetadata],
    node_params: &BTreeMap<String, SerializedParamValue>,
) -> bool {
    if params_metadata.is_empty() {
        return false;
    }

    let existing_targets: std::collections::BTreeSet<(String, String)> = bindings
        .iter()
        .filter_map(|b| match &b.target {
            BindingTarget::Node {
                node_id: nid,
                param,
            } => Some((nid.as_str().to_string(), param.clone())),
            _ => None,
        })
        .collect();

    let mut changed = false;
    for meta in params_metadata {
        if existing_targets.contains(&(node_id.as_str().to_string(), meta.name.clone())) {
            // Existing exposures are user state. Enrich their additive role
            // metadata in place so labels, ranges, bindings and automation
            // remain untouched while old projects gain the material surface.
            if let Some(binding_id) = bindings.iter().find_map(|binding| {
                matches!(
                    &binding.target,
                    BindingTarget::Node { node_id: nid, param }
                        if nid == node_id && param == &meta.name
                )
                .then_some(binding.id.clone())
            }) && let Some(spec) = params.iter_mut().find(|spec| spec.id == binding_id)
            {
                // A fan-out slot has multiple meanings. Keep its existing
                // scalar editor rather than claiming one compound owner.
                let role = if bindings
                    .iter()
                    .filter(|binding| binding.id == binding_id)
                    .count()
                    == 1
                {
                    meta.material_role
                } else {
                    None
                };
                if spec.material_role != role {
                    spec.material_role = role;
                    changed = true;
                }
            }
            continue;
        }

        let base_id = format!("{}_{}", node_doc_id, meta.name);
        let id = unique_param_id(params, &base_id);

        let default_f32 = match node_params.get(&meta.name) {
            Some(stamped) => serialized_default_as_f32(stamped),
            None => serialized_default_as_f32(&meta.default_value),
        };

        // Widen the range to contain a seeded default that falls outside the
        // manifest's generic min/max (e.g. a camera `distance` of 300 on a
        // large imported model) — but only for plain numeric params. Enum/
        // toggle/trigger ranges are index spaces, not physical quantities;
        // widening them would admit invalid indices.
        let widen = !meta.whole_numbers && !meta.is_toggle && !meta.is_trigger;
        let (min, max) = if widen {
            (meta.min.min(default_f32), meta.max.max(default_f32))
        } else {
            (meta.min, meta.max)
        };

        params.push(ParamSpecDef {
            tooltip: None,
            id: id.clone(),
            name: meta.label.clone(),
            min,
            max,
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
            wraps: meta.wraps,
            section: Some(section.to_string()),
            card_visible: stamped_card_visible(meta),
            material_role: meta.material_role,
            limit: Default::default(),
            unit: None,
            scene: None,
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
            // Every default this fn writes is a snapshot of the node's own
            // param (the stamped override, else the manifest default the node
            // is already sitting at) — never a value anyone chose against the
            // node. See `BindingDef::default_mirrors_node_param`.
            default_mirrors_node_param: true,
        });

        changed = true;
    }

    changed
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
