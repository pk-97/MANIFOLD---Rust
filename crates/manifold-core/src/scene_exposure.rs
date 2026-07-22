//! Scene-panel exposure convergence — P1 stamping helpers.
//!
//! `docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md` P1: every scene-vocabulary
//! node (transform, material, light, camera, atmosphere, bake_environment,
//! modifiers, scene_object.visible) gets all of its params exposed as outer-card
//! sliders at creation time. This module is the crate-neutral stamping surface:
//! it knows the `EffectGraphDef` shape but not renderer `ParamDef`s, so the
//! metadata source is injected by callers.

use std::collections::BTreeMap;

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

/// Card-visibility curation for the P1 scene-vocabulary auto-stamping path
/// (`docs/SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md` — card-visibility
/// convergence follow-up). P1 stamps EVERY param on a scene-vocabulary node
/// as a scene-panel exposure — the scene panel keeps all of them (its own
/// section query never filters, `sections_for_doc_ids` in
/// `manifold_app::ui_bridge::projection::scene`), but the generator's outer
/// CARD only wants a curated performance subset. This is a pure lookup: the
/// param stays a real, fully addressable exposure either way (OSC, Ableton,
/// macros, drivers) — `card_visible` only gates the CARD row builder.
///
/// Visible iff explicitly listed here; every other vocab type/param is
/// hidden from the card. Hand-curated explicit card params stamped OUTSIDE
/// this path (env_mode, scene_ambient, the Animation section, a user-added
/// graph-editor expose, …) never call this function, so they default `true`
/// unaffected.
pub fn card_visible_for(type_id: &str, param: &str) -> bool {
    let visible: &[&str] = match type_id {
        // `fov_y` lives on `node.orbit_camera` (not `node.camera_lens` —
        // that primitive's real params are focus_distance/f_stop/
        // shutter_angle/exposure_ev, all correctly hidden by the
        // default-deny below).
        "node.orbit_camera" => &["orbit", "tilt", "distance", "look_y", "fov_y"],
        "node.light" => &[
            "pos_x",
            "pos_y",
            "pos_z",
            "intensity",
            "color_r",
            "color_g",
            "color_b",
            "cast_shadows",
            "shadow_softness",
        ],
        "node.transform_3d" => &["pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z"],
        "node.bake_environment" => &["intensity", "fill"],
        _ => &[],
    };
    visible.contains(&param)
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
    let type_id = node.type_id.clone();
    // Cloned rather than borrowed: `meta` below needs a mutable borrow of
    // `def.preset_metadata`, and the node's `params` map is small
    // (edit-time only, never on a hot path).
    let node_params = node.params.clone();

    let meta = def.preset_metadata.get_or_insert_with(empty_scene_preset_metadata);

    stamp_scene_node_exposures_into(
        &mut meta.params,
        &mut meta.bindings,
        node_doc_id,
        &node_id,
        &type_id,
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
fn empty_scene_preset_metadata() -> PresetMetadata {
    PresetMetadata {
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
///
/// `type_id` selects the stamped spec's `card_visible` flag via
/// [`card_visible_for`] — the CARD-curation table, orthogonal to `node_id`
/// (the exposure's addressing target) and `section` (its card grouping).
pub fn stamp_scene_node_exposures_into(
    params: &mut Vec<ParamSpecDef>,
    bindings: &mut Vec<BindingDef>,
    node_doc_id: u32,
    node_id: &NodeId,
    type_id: &str,
    section: &str,
    params_metadata: &[SceneParamMetadata],
    node_params: &BTreeMap<String, SerializedParamValue>,
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
            wraps: false,
            section: Some(section.to_string()),
            card_visible: card_visible_for(type_id, &meta.name),
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

/// `(doc_id, node_id, type_id, section, params)` for one vocab-matched node,
/// collected by `collect_vocab_nodes` and consumed by `migrate_scene_exposures`.
type VocabNodeEntry = (u32, NodeId, String, String, BTreeMap<String, SerializedParamValue>);

/// Walk every node in `def` — INCLUDING every `node.group`'s inner body,
/// recursively at any depth — and stamp exposures for any whose `type_id` is
/// in `vocabulary`. `section_name` is called for each stamped node. Returns
/// `true` iff anything changed.
///
/// A grouped node (e.g. an imported/added object's `mat_k`/`transform_k`/
/// `scene_object`) still stamps into the def's TOP-LEVEL `preset_metadata`,
/// targeting the inner node's bare `NodeId` — the same convention the glTF
/// importer and the creation commands use (`docs/
/// SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md` P1). Nested node ids are
/// unique across the def by construction, so this never collides with a
/// top-level exposure.
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
    let mut found: Vec<VocabNodeEntry> = Vec::new();
    collect_vocab_nodes(&def.nodes, vocabulary, &mut section_name, &mut found);
    if found.is_empty() {
        return false;
    }

    let meta = def.preset_metadata.get_or_insert_with(empty_scene_preset_metadata);

    let mut changed = false;
    for (node_doc_id, node_id, type_id, section, node_params) in &found {
        let metadata = provider.metadata_for_type(type_id);
        if stamp_scene_node_exposures_into(
            &mut meta.params,
            &mut meta.bindings,
            *node_doc_id,
            node_id,
            type_id,
            section,
            &metadata,
            node_params,
        ) {
            changed = true;
        }
    }

    // Repair pass: a def stamped before this fix carries auto exposures whose
    // `default_value` came from the primitive manifest instead of the node's
    // stamped value (BUG-303). The idempotence guard above skips them forever
    // because a binding already targets `(node_id, param)` — re-seed those in
    // place whenever the node still has a stamped value that disagrees.
    for (_, node_id, type_id, _, node_params) in &found {
        let metadata = provider.metadata_for_type(type_id);
        for meta_entry in &metadata {
            let Some(stamped) = node_params.get(&meta_entry.name) else {
                // No stamped value for this param — nothing to repair against.
                // Protects curated fan-outs (e.g. sun -> envmap.sun_x) whose
                // target node never carries this param in its own overrides.
                continue;
            };
            let correct_default = serialized_default_as_f32(stamped);

            let Some(binding_idx) = meta.bindings.iter().position(|b| {
                !b.user_added
                    && matches!(
                        &b.target,
                        BindingTarget::Node { node_id: nid, param }
                            if nid == node_id && param == &meta_entry.name
                    )
            }) else {
                continue;
            };
            if meta.bindings[binding_idx].default_value == correct_default {
                continue;
            }

            let binding_id = meta.bindings[binding_idx].id.clone();
            meta.bindings[binding_idx].default_value = correct_default;
            if let Some(spec) = meta.params.iter_mut().find(|p| p.id == binding_id) {
                spec.default_value = correct_default;
                let widen = !spec.whole_numbers && !spec.is_toggle && !spec.is_trigger;
                if widen {
                    spec.min = spec.min.min(correct_default);
                    spec.max = spec.max.max(correct_default);
                }
            }
            changed = true;
        }
    }

    // Repair pass 2: a def stamped before the card-visibility curation
    // (`card_visible_for`) landed carries auto exposures whose spec
    // `card_visible` is stale — every param defaulted `true` regardless of
    // vocabulary, so a param the curated table now hides (e.g. a transform's
    // `scale_x`, any material param) still shows on the card. Unlike the
    // default-value repair above, this doesn't need a stamped override on
    // the node — `card_visible_for` is a pure function of `(type_id, param)`
    // — so it applies to every auto exposure the vocab node has, whether or
    // not its value diverges from the manifest default. Idempotent: a
    // second run re-derives the same flag and writes nothing.
    for (_, node_id, type_id, _, _) in &found {
        let metadata = provider.metadata_for_type(type_id);
        for meta_entry in &metadata {
            let Some(binding) = meta.bindings.iter().find(|b| {
                !b.user_added
                    && matches!(
                        &b.target,
                        BindingTarget::Node { node_id: nid, param }
                            if nid == node_id && param == &meta_entry.name
                    )
            }) else {
                continue;
            };
            let binding_id = binding.id.clone();
            let correct_visible = card_visible_for(type_id, &meta_entry.name);
            if let Some(spec) = meta.params.iter_mut().find(|p| p.id == binding_id)
                && spec.card_visible != correct_visible
            {
                spec.card_visible = correct_visible;
                changed = true;
            }
        }
    }

    changed
}

/// Recursively collect `(doc_id, node_id, type_id, section, params)` for
/// every node in `nodes` (and every `node.group`'s inner body, at any depth)
/// whose `type_id` is in `vocabulary`. `section_name` is invoked once per
/// matched node during this read-only walk, before any mutation of the
/// owning def. `params` is a clone of the node's stamped overrides — small,
/// edit-time-only maps — carried through so stamping (and the repair pass)
/// can seed defaults from them without re-walking the tree under a mutable
/// borrow of `def.preset_metadata`.
fn collect_vocab_nodes<F>(
    nodes: &[EffectGraphNode],
    vocabulary: &[&str],
    section_name: &mut F,
    out: &mut Vec<VocabNodeEntry>,
) where
    F: FnMut(&EffectGraphNode) -> String,
{
    for node in nodes {
        if vocabulary.contains(&node.type_id.as_str()) {
            let section = section_name(node);
            out.push((
                node.id,
                node.node_id.clone(),
                node.type_id.clone(),
                section,
                node.params.clone(),
            ));
        }
        if let Some(body) = node.group.as_deref() {
            collect_vocab_nodes(&body.nodes, vocabulary, section_name, out);
        }
    }
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

    /// P1 Task D: a grouped scene-vocab node (e.g. an added object's own
    /// `node.transform_3d`, living inside a `node.group` body) must still get
    /// its exposure stamped — into the def's TOP-LEVEL `preset_metadata`,
    /// targeting the inner node's bare `NodeId` — not just top-level nodes.
    /// Idempotent on a second run.
    #[test]
    fn migrate_exposes_grouped_node_param_targeting_inner_node_id() {
        use crate::effect_graph_def::{GroupDef, GroupInterface, GROUP_TYPE_ID};

        struct TestProvider;
        impl SceneExposureMetadataProvider for TestProvider {
            fn metadata_for_type(&self, type_id: &str) -> Vec<SceneParamMetadata> {
                if type_id == "node.transform_3d" {
                    vec![float_meta("pos_x", "X")]
                } else {
                    Vec::new()
                }
            }
        }

        let inner_node_id = NodeId::new("transform_0");
        let mut inner = make_node(10, "node.transform_3d");
        inner.node_id = inner_node_id.clone();

        let mut group_node = make_node(1, GROUP_TYPE_ID);
        group_node.group = Some(Box::new(GroupDef {
            interface: GroupInterface { inputs: Vec::new(), outputs: Vec::new(), params: Vec::new() },
            nodes: vec![inner],
            wires: Vec::new(),
            tint: None,
        }));

        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![group_node],
            wires: vec![],
        };

        let vocab = ["node.transform_3d"];
        assert!(migrate_scene_exposures(
            &mut def,
            &vocab,
            |_n| "Object 1 — Transform".to_string(),
            &TestProvider
        ));

        let meta = def.preset_metadata.as_ref().expect("stamped into top-level preset_metadata");
        assert_eq!(meta.params.len(), 1);
        assert_eq!(meta.params[0].section.as_deref(), Some("Object 1 — Transform"));
        assert!(
            meta.bindings.iter().any(|b| matches!(
                &b.target,
                BindingTarget::Node { node_id, param } if *node_id == inner_node_id && param == "pos_x"
            )),
            "binding targets the grouped node's bare NodeId, not the group's"
        );

        let after_first = def.clone();
        assert!(!migrate_scene_exposures(
            &mut def,
            &vocab,
            |_n| "Object 1 — Transform".to_string(),
            &TestProvider
        ));
        assert_eq!(def, after_first, "second run is idempotent");
    }

    /// BUG-303: stamping a node whose `params` carry a non-manifest value
    /// (`pos_x = 7.5`) must seed BOTH the `ParamSpecDef` and the `BindingDef`
    /// default from that stamped value — not the primitive manifest's
    /// generic default (0.5, per `float_meta`). A param the node does NOT
    /// stamp (`pos_y`) still falls back to the manifest default.
    #[test]
    fn stamped_node_value_seeds_both_spec_and_binding_defaults() {
        let mut node = make_node(7, "node.transform_3d");
        node.params.insert("pos_x".to_string(), SerializedParamValue::Float { value: 7.5 });
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![node],
            wires: vec![],
        };

        assert!(stamp_scene_node_exposures(
            &mut def,
            7,
            "Transform",
            &[float_meta("pos_x", "X"), float_meta("pos_y", "Y")],
        ));

        let meta = def.preset_metadata.as_ref().unwrap();
        let pos_x_spec = meta.params.iter().find(|p| p.name == "X").unwrap();
        let pos_x_binding = meta.bindings.iter().find(|b| b.label == "X").unwrap();
        assert_eq!(pos_x_spec.default_value, 7.5, "spec default seeded from the node's stamped value");
        assert_eq!(pos_x_binding.default_value, 7.5, "binding default seeded from the node's stamped value");

        let pos_y_spec = meta.params.iter().find(|p| p.name == "Y").unwrap();
        assert_eq!(pos_y_spec.default_value, 0.5, "unstamped param falls back to the manifest default");
    }

    /// BUG-303: a seeded default outside the manifest's declared min/max
    /// (e.g. a camera `distance` of 300 on a large imported model, against a
    /// manifest range of 0..1) must widen the spec's range to contain it —
    /// a slider whose default sits outside its own min/max is unusable.
    #[test]
    fn seeded_default_outside_manifest_range_widens_min_max() {
        let mut node = make_node(7, "node.orbit_camera");
        node.params.insert("distance".to_string(), SerializedParamValue::Float { value: 300.0 });
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![node],
            wires: vec![],
        };

        assert!(stamp_scene_node_exposures(
            &mut def,
            7,
            "Camera",
            &[float_meta("distance", "Distance")],
        ));

        let meta = def.preset_metadata.as_ref().unwrap();
        let spec = meta.params.iter().find(|p| p.name == "Distance").unwrap();
        assert_eq!(spec.default_value, 300.0);
        assert!(spec.min <= 300.0 && spec.max >= 300.0, "range widened to contain the seeded default");
    }

    /// BUG-303 migration repair: a def stamped BEFORE this fix carries an
    /// auto exposure whose default came from the manifest (0.0) even though
    /// the target node itself carries a stamped `pos_x = 7.5`. Because a
    /// binding already targets `(node_id, pos_x)`, the ordinary idempotence
    /// guard would skip it forever — `migrate_scene_exposures` must detect
    /// the mismatch and re-seed both the binding and the matching
    /// `ParamSpecDef` from the node's stamped value. A `user_added: true`
    /// binding is left untouched even if it disagrees. The whole function
    /// stays idempotent: a second run is a no-op.
    #[test]
    fn migrate_repairs_pre_fix_binding_default_from_stamped_node_value() {
        struct TestProvider;
        impl SceneExposureMetadataProvider for TestProvider {
            fn metadata_for_type(&self, type_id: &str) -> Vec<SceneParamMetadata> {
                if type_id == "node.transform_3d" {
                    vec![float_meta("pos_x", "X")]
                } else {
                    Vec::new()
                }
            }
        }

        let mut node = make_node(7, "node.transform_3d");
        node.params.insert("pos_x".to_string(), SerializedParamValue::Float { value: 7.5 });
        let node_id = node.node_id.clone();

        // Simulate a pre-fix def: a spec + binding already stamped at the
        // manifest default (0.5), even though the node itself carries 7.5.
        let stale_spec = ParamSpecDef {
            id: "7_pos_x".to_string(),
            name: "X".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.5,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: crate::macro_bank::MacroCurve::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: Some("Transform".to_string()),
            card_visible: true,
        };
        let stale_binding = BindingDef {
            id: "7_pos_x".to_string(),
            label: "X".to_string(),
            default_value: 0.5,
            target: BindingTarget::Node { node_id: node_id.clone(), param: "pos_x".to_string() },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
        };
        // A user-added binding on the SAME param must survive untouched.
        let user_binding = BindingDef {
            id: "user_pos_x".to_string(),
            label: "X (user)".to_string(),
            default_value: 0.5,
            target: BindingTarget::Node { node_id: node_id.clone(), param: "pos_x".to_string() },
            convert: ParamConvert::Float,
            user_added: true,
            scale: 1.0,
            offset: 0.0,
        };

        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: Some(PresetMetadata {
                params: vec![stale_spec],
                bindings: vec![stale_binding, user_binding.clone()],
                ..empty_scene_preset_metadata()
            }),
            nodes: vec![node],
            wires: vec![],
        };

        let vocab = ["node.transform_3d"];
        assert!(migrate_scene_exposures(&mut def, &vocab, |_n| "Transform".to_string(), &TestProvider));

        let meta = def.preset_metadata.as_ref().unwrap();
        let repaired_spec = meta.params.iter().find(|p| p.id == "7_pos_x").unwrap();
        assert_eq!(repaired_spec.default_value, 7.5, "spec re-seeded from the node's stamped value");
        let repaired_binding = meta.bindings.iter().find(|b| b.id == "7_pos_x").unwrap();
        assert_eq!(repaired_binding.default_value, 7.5, "binding re-seeded from the node's stamped value");
        let untouched_user_binding = meta.bindings.iter().find(|b| b.id == "user_pos_x").unwrap();
        assert_eq!(
            untouched_user_binding.default_value, 0.5,
            "user_added binding is never touched by the repair pass"
        );

        let after_repair = def.clone();
        assert!(
            !migrate_scene_exposures(&mut def, &vocab, |_n| "Transform".to_string(), &TestProvider),
            "second migration run is a no-op once repaired"
        );
        assert_eq!(def, after_repair);
    }

    // ── Card-visibility curation (card_visible_for) ────────────────────

    #[test]
    fn card_visible_for_transform_shows_pos_and_rot_hides_scale() {
        assert!(card_visible_for("node.transform_3d", "pos_x"));
        assert!(card_visible_for("node.transform_3d", "pos_y"));
        assert!(card_visible_for("node.transform_3d", "pos_z"));
        assert!(card_visible_for("node.transform_3d", "rot_x"));
        assert!(card_visible_for("node.transform_3d", "rot_y"));
        assert!(card_visible_for("node.transform_3d", "rot_z"));
        assert!(!card_visible_for("node.transform_3d", "scale_x"));
        assert!(!card_visible_for("node.transform_3d", "scale_y"));
        assert!(!card_visible_for("node.transform_3d", "scale_z"));
    }

    #[test]
    fn card_visible_for_material_hides_everything() {
        assert!(!card_visible_for("node.pbr_material", "color_r"));
        assert!(!card_visible_for("node.pbr_material", "roughness"));
        assert!(!card_visible_for("node.phong_material", "color_r"));
    }

    /// Stamping a transform node exposes `pos_x` (card-visible) and
    /// `scale_x` (card-hidden) — both are real, fully addressable
    /// exposures (the binding + spec both exist), `card_visible` only gates
    /// which one becomes a CARD row.
    #[test]
    fn stamp_sets_card_visible_true_for_pos_false_for_scale() {
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![make_node(7, "node.transform_3d")],
            wires: vec![],
        };

        assert!(stamp_scene_node_exposures(
            &mut def,
            7,
            "Transform",
            &[float_meta("pos_x", "X"), float_meta("scale_x", "Scale X")],
        ));

        let meta = def.preset_metadata.as_ref().unwrap();
        let pos_spec = meta.params.iter().find(|p| p.name == "X").unwrap();
        assert!(pos_spec.card_visible, "pos_x stays visible on the card");
        let scale_spec = meta.params.iter().find(|p| p.name == "Scale X").unwrap();
        assert!(!scale_spec.card_visible, "scale_x is hidden from the card");

        // Both are still real bindings — card_visible never affects addressing.
        assert_eq!(meta.bindings.len(), 2);
    }

    /// Stamping a material node hides every param — `card_visible_for`
    /// returns `false` for the entire vocab type.
    #[test]
    fn stamp_material_node_hides_all_params_from_card() {
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![make_node(3, "node.pbr_material")],
            wires: vec![],
        };

        assert!(stamp_scene_node_exposures(
            &mut def,
            3,
            "Material",
            &[float_meta("color_r", "Colour R"), float_meta("roughness", "Roughness")],
        ));

        let meta = def.preset_metadata.as_ref().unwrap();
        assert!(meta.params.iter().all(|p| !p.card_visible), "no material param shows on the card");
    }

    /// Migration repair for `card_visible`: a def stamped before the
    /// curation table landed carries every auto exposure at `card_visible:
    /// true` (the universal pre-fix default) — including `scale_x`, which
    /// the curated table now hides. `migrate_scene_exposures` must correct
    /// it in place, independent of whether the node's stamped value
    /// disagrees with the manifest default (unlike the BUG-303 default-
    /// value repair, this one doesn't need a `node_params` entry). Second
    /// run is a no-op.
    #[test]
    fn migrate_repairs_stale_card_visible_flag_and_is_idempotent() {
        struct TestProvider;
        impl SceneExposureMetadataProvider for TestProvider {
            fn metadata_for_type(&self, type_id: &str) -> Vec<SceneParamMetadata> {
                if type_id == "node.transform_3d" {
                    vec![float_meta("pos_x", "X"), float_meta("scale_x", "Scale X")]
                } else {
                    Vec::new()
                }
            }
        }

        let node = make_node(7, "node.transform_3d");
        let node_id = node.node_id.clone();

        // Pre-fix def: both specs/bindings stamped `card_visible: true`
        // (the only value that existed before this fix), even though
        // `scale_x` should now read `false`.
        let mut stale_pos = float_spec_default("7_pos_x", "X", "Transform");
        stale_pos.card_visible = true;
        let mut stale_scale = float_spec_default("7_scale_x", "Scale X", "Transform");
        stale_scale.card_visible = true;

        let stale_pos_binding = BindingDef {
            id: "7_pos_x".to_string(),
            label: "X".to_string(),
            default_value: 0.5,
            target: BindingTarget::Node { node_id: node_id.clone(), param: "pos_x".to_string() },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
        };
        let stale_scale_binding = BindingDef {
            id: "7_scale_x".to_string(),
            label: "Scale X".to_string(),
            default_value: 0.5,
            target: BindingTarget::Node { node_id: node_id.clone(), param: "scale_x".to_string() },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
        };

        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: Some(PresetMetadata {
                params: vec![stale_pos, stale_scale],
                bindings: vec![stale_pos_binding, stale_scale_binding],
                ..empty_scene_preset_metadata()
            }),
            nodes: vec![node],
            wires: vec![],
        };

        let vocab = ["node.transform_3d"];
        assert!(migrate_scene_exposures(&mut def, &vocab, |_n| "Transform".to_string(), &TestProvider));

        let meta = def.preset_metadata.as_ref().unwrap();
        let pos_spec = meta.params.iter().find(|p| p.id == "7_pos_x").unwrap();
        assert!(pos_spec.card_visible, "pos_x was already correct (true) and stays true");
        let scale_spec = meta.params.iter().find(|p| p.id == "7_scale_x").unwrap();
        assert!(!scale_spec.card_visible, "scale_x repaired from stale true to correct false");

        let after_repair = def.clone();
        assert!(
            !migrate_scene_exposures(&mut def, &vocab, |_n| "Transform".to_string(), &TestProvider),
            "second migration run is a no-op once repaired"
        );
        assert_eq!(def, after_repair);
    }

    /// Minimal `ParamSpecDef` builder for the card-visible repair test —
    /// mirrors the shape of `stale_spec` above without repeating every field.
    fn float_spec_default(id: &str, name: &str, section: &str) -> ParamSpecDef {
        ParamSpecDef {
            id: id.to_string(),
            name: name.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.5,
            section: Some(section.to_string()),
            ..Default::default()
        }
    }
}
