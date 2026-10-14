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
pub struct NodeParamMetadata {
    pub name: String,
    pub label: String,
    pub min: f32,
    pub max: f32,
    pub default_value: SerializedParamValue,
    pub is_angle: bool,
    /// Whether values outside the declared range wrap back into it.
    pub wraps: bool,
    pub whole_numbers: bool,
    pub is_toggle: bool,
    pub is_trigger: bool,
    pub value_labels: Vec<String>,
    pub convert: ParamConvert,
    /// Optional material-inspector semantic role supplied by the renderer's
    /// classifier and copied into the persisted manifest descriptor.
    pub material_role: Option<crate::material_inspector::MaterialParamRole>,
    /// Where the param lives on the scene surfaces, from the scene table
    /// (SCENE_SURFACES_DESIGN.md section 3.3 (the scene table)). `None` for a
    /// type outside the scene vocabulary.
    pub scene: Option<manifold_foundation::scene_surface::SceneParamDesc>,
    pub unit: Option<manifold_foundation::scene_surface::Unit>,
    pub limit: manifold_foundation::param_range::HardLimit,
}

/// Source of per-type param metadata used by the creation-site commands and the
/// load-time migration. Implemented by `manifold_nodes_scene` using
/// `PrimitiveRegistry`.
pub trait NodeParamProvider: Send + Sync {
    /// The params `type_id` stamps, in card order: every param except those
    /// the scene table marks Hidden. Empty when the type is unknown.
    fn params_for_type(&self, type_id: &str) -> Vec<NodeParamMetadata>;
}

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
fn empty_scene_preset_metadata() -> PresetMetadata {
    PresetMetadata {
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

pub(crate) fn serialized_default_as_f32(value: &SerializedParamValue) -> f32 {
    match value {
        SerializedParamValue::Float { value } => *value,
        SerializedParamValue::Int { value } => *value as f32,
        SerializedParamValue::Bool { value } => {
            if *value {
                1.0
            } else {
                0.0
            }
        }
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

// ── Scene-scale range table (single source of truth) ─────────────────

/// Compute the bounding-sphere radius from a `(min, max)` bbox. Same math as
/// `SceneScale::from_bbox` — half the diagonal, floored at 0.01 so a
/// degenerate/empty bbox still yields nonzero slider widths.
pub fn scene_radius_from_bounds(bounds: ([f32; 3], [f32; 3])) -> f32 {
    let (min, max) = bounds;
    let dims = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    ((dims[0] * dims[0] + dims[1] * dims[1] + dims[2] * dims[2]).sqrt() * 0.5).max(0.01)
}

/// The single scene-scaled slider range table. Radius in world units; every
/// band is a multiple of it so a scene twice as big gets sliders twice as wide.
/// Returns `None` for params not in the table (the generic refresh path).
///
/// Multiplier rationale (compressed from `scene_scale.rs` doc comments):
/// - Position (`pos_*`, `aim_*`): ±2·radius — one radius covers the model,
///   two gives the model plus neighbourhood, enough to dock at the edge of
///   the shot without squeezing the useful band into the first few percent.
/// - Light range: 0.01..4·radius — the soft falloff half-distance fills the
///   whole set while keeping fine control in the near half.
/// - Focus distance: 0..4·radius — 0 is the hyperfocal pin (focus off),
///   far end reaches two scene-widths out (import camera sits at ~2.2·radius).
/// - F-stop: 0.5..max(32, 64·radius) — 0.5 is the photographic floor; 64·R
///   keeps migrated f_stop×R values on-slider; 32 keeps unit-scale default.
/// - Orbit distance: 0.01..6·radius — ~3× headroom past the 2.2·radius
///   framing distance so the full orbit range is on-slider.
/// - Orbit near: 0.001..2·radius — import stamps near at ~0.6·radius, so
///   2·radius contains it with room.
/// - Orbit far: 1.0..min(20·radius, 10_000) — 20× covers the import-stamped
///   far with headroom; 10_000 is the primitive's declared range max.
/// - Fog density: 0.0..2/radius — fog is per-world-unit (1 - exp(-d·dist)),
///   so the useful slider range scales inversely with scene size.
pub fn scene_scaled_range(type_id: &str, param: &str, radius: f32) -> Option<(f32, f32)> {
    match type_id {
        "node.transform_3d" if matches!(param, "pos_x" | "pos_y" | "pos_z") => {
            Some((-2.0 * radius, 2.0 * radius))
        }
        "node.light"
            if matches!(
                param,
                "pos_x" | "pos_y" | "pos_z" | "aim_x" | "aim_y" | "aim_z"
            ) =>
        {
            Some((-2.0 * radius, 2.0 * radius))
        }
        "node.light" if param == "range" => Some((0.01, 4.0 * radius)),
        "node.camera_lens" if param == "focus_distance" => Some((0.0, 4.0 * radius)),
        "node.camera_lens" if param == "f_stop" => Some((0.5, (32.0_f32).max(64.0 * radius))),
        "node.orbit_camera" if param == "distance" => Some((0.01, 6.0 * radius)),
        "node.orbit_camera" if param == "near" => Some((0.001, 2.0 * radius)),
        "node.orbit_camera" if param == "far" => Some((1.0, (20.0 * radius).min(10_000.0))),
        "node.atmosphere" if param == "fog_density" => Some((0.0, 2.0 / radius)),
        _ => None,
    }
}

/// `(doc_id, node_id, type_id, section, params, wired_inputs)` for one
/// vocab-matched node, collected by collect_vocab_nodes and consumed by
/// migrate_scene_exposures.
type VocabNodeEntry = (
    u32,
    NodeId,
    String,
    String,
    BTreeMap<String, SerializedParamValue>,
    std::collections::BTreeSet<String>,
);

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
/// The vocabulary and section naming live in the caller (`manifold_nodes`)
/// because this module intentionally has no primitive registry dependency.
pub fn migrate_scene_exposures<F>(
    def: &mut EffectGraphDef,
    vocabulary: &[&str],
    mut section_name: F,
    provider: &dyn NodeParamProvider,
) -> bool
where
    F: FnMut(&EffectGraphNode) -> String,
{
    let mut found: Vec<VocabNodeEntry> = Vec::new();
    collect_vocab_nodes(
        &def.nodes,
        &def.wires,
        vocabulary,
        &mut section_name,
        &mut found,
    );
    if found.is_empty() {
        return false;
    }

    let meta = def
        .preset_metadata
        .get_or_insert_with(empty_scene_preset_metadata);

    let mut changed = false;
    for (node_doc_id, node_id, type_id, section, node_params, wired_inputs) in &found {
        let metadata = provider.params_for_type(type_id);
        let fresh_metadata: Vec<_> = metadata
            .into_iter()
            .filter(|entry| {
                !wired_inputs.contains(&entry.name)
                    || meta.bindings.iter().any(|binding| matches!(
                        &binding.target,
                        BindingTarget::Node { node_id: target, param }
                            if target == node_id && param == &entry.name
                    ))
            })
            .collect();
        if stamp_scene_node_exposures_into(
            &mut meta.params,
            &mut meta.bindings,
            *node_doc_id,
            node_id,
            section,
            &fresh_metadata,
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
    for (_, node_id, type_id, _, node_params, _) in &found {
        let metadata = provider.params_for_type(type_id);
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

    // Repair pass 3: a def stamped before `default_mirrors_node_param`
    // existed carries auto exposures whose defaults are stamp-time snapshots
    // of the node param but which deserialize as authored (the field is
    // absent, so `false`). `apply_binding_defaults` therefore keeps
    // replanting the snapshot over whatever the node param now holds, and the
    // BUG-ji6q clobber survives in every already-imported project. Re-derive
    // the flag from what the stamp itself would produce: every auto exposure
    // on a vocab node is a mirror by construction.
    //
    // No stamped override needed: the
    // not-user-added predicate is the guard that keeps a hand-authored
    // exposure on the same node authored. Idempotent: a second run
    // re-derives `true` and writes nothing.
    for (_, node_id, type_id, _, _, _) in &found {
        let metadata = provider.params_for_type(type_id);
        for meta_entry in &metadata {
            let Some(binding) = meta.bindings.iter_mut().find(|b| {
                !b.user_added
                    && matches!(
                        &b.target,
                        BindingTarget::Node { node_id: nid, param }
                            if nid == node_id && param == &meta_entry.name
                    )
            }) else {
                continue;
            };
            if !binding.default_mirrors_node_param {
                binding.default_mirrors_node_param = true;
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
    wires: &[crate::effect_graph_def::EffectGraphWire],
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
                wires
                    .iter()
                    .filter(|wire| wire.to_node == node.id)
                    .map(|wire| wire.to_port.clone())
                    .collect(),
            ));
        }
        if let Some(body) = node.group.as_deref() {
            collect_vocab_nodes(&body.nodes, &body.wires, vocabulary, section_name, out);
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

    fn float_meta(name: &str, label: &str) -> NodeParamMetadata {
        NodeParamMetadata {
            name: name.to_string(),
            label: label.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: SerializedParamValue::Float { value: 0.5 },
            is_angle: false,
            wraps: false,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            convert: ParamConvert::Float,
            material_role: None,
            scene: None,
            unit: None,
            limit: Default::default(),
        }
    }

    #[test]
    fn stamps_exposures_and_creates_metadata() {
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![make_node(7, "node.light")],
            wires: vec![],
        };

        let changed = stamp_scene_node_exposures(
            &mut def,
            7,
            "Key Light",
            &[
                float_meta("intensity", "Intensity"),
                float_meta("pos_x", "X"),
            ],
        );

        assert!(changed);
        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(meta.params.len(), 2);
        assert_eq!(meta.bindings.len(), 2);
        assert_eq!(meta.params[0].section.as_deref(), Some("Key Light"));
        assert_eq!(
            meta.bindings[0].target,
            BindingTarget::Node {
                node_id: NodeId::new("n7"),
                param: "intensity".to_string(),
            }
        );
    }

    #[test]
    fn idempotent_second_call_is_no_op() {
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![make_node(7, "node.light")],
            wires: vec![],
        };
        let metadata = vec![float_meta("intensity", "Intensity")];

        assert!(stamp_scene_node_exposures(
            &mut def,
            7,
            "Key Light",
            &metadata
        ));
        let after_first = def.clone();
        assert!(!stamp_scene_node_exposures(
            &mut def,
            7,
            "Key Light",
            &metadata
        ));
        assert_eq!(def, after_first);
    }

    #[test]
    fn material_inspector_existing_exposure_gains_role_without_resetting_authored_fields() {
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![make_node(7, "node.pbr_material")],
            wires: Vec::new(),
        };
        let mut metadata = float_meta("metallic", "Metallic");
        metadata.material_role = Some(crate::material_inspector::MaterialParamRole::Scalar(
            crate::material_inspector::MaterialGroup::Surface,
        ));
        assert!(stamp_scene_node_exposures(
            &mut def,
            7,
            "Material",
            std::slice::from_ref(&metadata),
        ));
        let meta = def.preset_metadata.as_mut().unwrap();
        let original_id = meta.params[0].id.clone();
        meta.params[0].name = "Authored metallic label".to_string();
        meta.params[0].min = -0.25;
        meta.params[0].material_role = None;
        meta.bindings[0].label = "Authored binding label".to_string();

        assert!(stamp_scene_node_exposures(
            &mut def,
            7,
            "Material",
            std::slice::from_ref(&metadata),
        ));
        let meta = def.preset_metadata.as_ref().unwrap();
        let spec = meta
            .params
            .iter()
            .find(|spec| spec.id == original_id)
            .unwrap();
        assert_eq!(spec.name, "Authored metallic label");
        assert_eq!(spec.min, -0.25);
        assert_eq!(spec.material_role, metadata.material_role);
        assert_eq!(meta.bindings[0].label, "Authored binding label");
        let meta = def.preset_metadata.as_mut().unwrap();
        let mut fanout = meta.bindings[0].clone();
        let BindingTarget::Node { param, .. } = &mut fanout.target else {
            unreachable!()
        };
        *param = "roughness".into();
        meta.bindings.push(fanout);
        assert!(stamp_scene_node_exposures(
            &mut def,
            7,
            "Material",
            std::slice::from_ref(&metadata)
        ));
        assert_eq!(
            def.preset_metadata.as_ref().unwrap().params[0].material_role,
            None
        );
    }

    #[test]
    fn migrate_skips_unknown_nodes_and_is_idempotent() {
        struct TestProvider;
        impl NodeParamProvider for TestProvider {
            fn params_for_type(&self, type_id: &str) -> Vec<NodeParamMetadata> {
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
            scene_modifiers: Vec::new(),
            nodes: vec![make_node(5, "node.value"), make_node(7, "node.light")],
            wires: vec![],
        };

        let vocab = ["node.light"];
        let section = |_n: &EffectGraphNode| "Light".to_string();

        assert!(migrate_scene_exposures(
            &mut def,
            &vocab,
            section,
            &TestProvider
        ));
        let after_first = def.clone();
        assert!(!migrate_scene_exposures(
            &mut def,
            &vocab,
            |_n| "Light".to_string(),
            &TestProvider
        ));
        assert_eq!(def, after_first);
    }

    /// One auto exposure exactly as a def stamped BEFORE
    /// `default_mirrors_node_param` existed carries it: the field is absent
    /// on disk, so it deserializes `false` (authored) even though the stamp
    /// produced it. `user_added` says whether the graph editor's expose
    /// checkbox minted it instead.
    fn pre_fix_exposure(
        node_id: &NodeId,
        param: &str,
        user_added: bool,
    ) -> (ParamSpecDef, BindingDef) {
        let id = format!(
            "{}_{}_{}",
            node_id.as_str(),
            param,
            if user_added { "user" } else { "auto" }
        );
        let spec = ParamSpecDef {
            tooltip: None,
            id: id.clone(),
            name: param.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.5,
            whole_numbers: false,
            is_toggle: false,
            is_trigger: false,
            value_labels: Vec::new(),
            format_string: None,
            osc_suffix: String::new(),
            curve: Default::default(),
            invert: false,
            is_angle: false,
            is_trigger_gate: false,
            wraps: false,
            section: Some("Light".to_string()),
            card_visible: true,
            material_role: None,
            limit: Default::default(),
            unit: None,
            scene: None,
        };
        let binding = BindingDef {
            id,
            label: param.to_string(),
            default_value: 0.5,
            target: BindingTarget::Node {
                node_id: node_id.clone(),
                param: param.to_string(),
            },
            convert: ParamConvert::Float,
            user_added,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        };
        (spec, binding)
    }

    struct LightProvider;
    impl NodeParamProvider for LightProvider {
        fn params_for_type(&self, type_id: &str) -> Vec<NodeParamMetadata> {
            if type_id == "node.light" {
                vec![float_meta("intensity", "Intensity")]
            } else {
                Vec::new()
            }
        }
    }

    fn def_with_exposures(exposures: Vec<(ParamSpecDef, BindingDef)>) -> EffectGraphDef {
        let (params, bindings) = exposures.into_iter().unzip();
        let mut meta = empty_scene_preset_metadata();
        meta.params = params;
        meta.bindings = bindings;
        EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: Some(meta),
            scene_modifiers: Vec::new(),
            nodes: vec![make_node(7, "node.light")],
            wires: vec![],
        }
    }

    /// BUG-ji6q repair pass 3: a project stamped before the provenance flag
    /// existed carries auto exposures that deserialize as AUTHORED, so
    /// `apply_binding_defaults` keeps replanting the stamp-time snapshot and
    /// the clobber survives every rebuild. The migration re-derives the flag.
    /// Second run must change nothing — this pass rewrites saved user work,
    /// so idempotence is asserted, not argued.
    #[test]
    fn migrate_marks_pre_fix_auto_exposures_as_mirrors_and_is_idempotent() {
        let node_id = NodeId::new("n7");
        let mut def = def_with_exposures(vec![pre_fix_exposure(&node_id, "intensity", false)]);
        let vocab = ["node.light"];

        assert!(
            migrate_scene_exposures(&mut def, &vocab, |_n| "Light".to_string(), &LightProvider),
            "a pre-fix auto exposure must be repaired"
        );
        let binding = &def.preset_metadata.as_ref().unwrap().bindings[0];
        assert!(
            binding.default_mirrors_node_param,
            "the stamped exposure's default is a snapshot of the node param, so the \
             migration must mark it as a mirror — otherwise apply_binding_defaults \
             keeps replanting it and BUG-ji6q survives in every already-imported project"
        );

        let after_first = def.clone();
        assert!(
            !migrate_scene_exposures(&mut def, &vocab, |_n| "Light".to_string(), &LightProvider),
            "second run must report no change"
        );
        assert_eq!(def, after_first, "second run must not touch a single byte");
    }

    /// The `!b.user_added` guard is the whole line between repairing a stamp
    /// and silently discarding what someone chose by hand. A user-authored
    /// exposure on a stamped card keeps its authored default — which still
    /// plants — no matter that it targets the same node param as the auto
    /// one. The user binding is listed FIRST so a loosened predicate would
    /// reach it before the auto binding and fail here.
    #[test]
    fn migrate_leaves_a_user_authored_exposure_authored() {
        let node_id = NodeId::new("n7");
        let mut def = def_with_exposures(vec![
            pre_fix_exposure(&node_id, "intensity", true),
            pre_fix_exposure(&node_id, "intensity", false),
        ]);

        assert!(migrate_scene_exposures(
            &mut def,
            &["node.light"],
            |_n| "Light".to_string(),
            &LightProvider
        ));

        let meta = def.preset_metadata.as_ref().unwrap();
        let user = meta
            .bindings
            .iter()
            .find(|b| b.user_added)
            .expect("user binding kept");
        let auto = meta
            .bindings
            .iter()
            .find(|b| !b.user_added)
            .expect("auto binding kept");
        assert!(
            !user.default_mirrors_node_param,
            "a hand-authored exposure is a value someone chose — the migration must \
             leave it authored so its default still plants"
        );
        assert!(
            auto.default_mirrors_node_param,
            "the auto exposure is still repaired"
        );
    }

    /// P1 Task D: a grouped scene-vocab node (e.g. an added object's own
    /// `node.transform_3d`, living inside a `node.group` body) must still get
    /// its exposure stamped — into the def's TOP-LEVEL `preset_metadata`,
    /// targeting the inner node's bare `NodeId` — not just top-level nodes.
    /// Idempotent on a second run.
    #[test]
    fn migrate_exposes_grouped_node_param_targeting_inner_node_id() {
        use crate::effect_graph_def::{GROUP_TYPE_ID, GroupDef, GroupInterface};

        struct TestProvider;
        impl NodeParamProvider for TestProvider {
            fn params_for_type(&self, type_id: &str) -> Vec<NodeParamMetadata> {
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
            interface: GroupInterface {
                inputs: Vec::new(),
                outputs: Vec::new(),
                params: Vec::new(),
            },
            nodes: vec![inner],
            wires: Vec::new(),
            tint: None,
        }));

        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
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

        let meta = def
            .preset_metadata
            .as_ref()
            .expect("stamped into top-level preset_metadata");
        assert_eq!(meta.params.len(), 1);
        assert_eq!(
            meta.params[0].section.as_deref(),
            Some("Object 1 — Transform")
        );
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

    #[test]
    fn migration_skips_wired_fallbacks_and_scopes_group_wires_locally() {
        use crate::effect_graph_def::{EffectGraphWire, GroupDef, GroupInterface};

        struct TestProvider;
        impl NodeParamProvider for TestProvider {
            fn params_for_type(&self, type_id: &str) -> Vec<NodeParamMetadata> {
                if type_id == "node.light" {
                    vec![float_meta("intensity", "Intensity")]
                } else {
                    Vec::new()
                }
            }
        }

        let mut driven = make_node(7, "node.light");
        driven.node_id = NodeId::new("driven");
        driven.params.insert(
            "intensity".to_string(),
            SerializedParamValue::Float { value: 0.8 },
        );
        let mut source = make_node(8, "node.value");
        source.node_id = NodeId::new("source");
        let mut fresh_driven = make_node(11, "node.light");
        fresh_driven.node_id = NodeId::new("fresh_driven");

        let mut unwired = make_node(9, "node.light");
        unwired.node_id = NodeId::new("unwired");
        let mut outer = make_node(1, "node.light");
        outer.node_id = NodeId::new("outer");

        let mut inner = make_node(1, "node.light");
        inner.node_id = NodeId::new("inner");
        let mut inner_source = make_node(2, "node.value");
        inner_source.node_id = NodeId::new("inner_source");
        let mut group = make_node(10, "node.group");
        group.group = Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: Vec::new(),
                outputs: Vec::new(),
                params: Vec::new(),
            },
            nodes: vec![inner, inner_source],
            wires: vec![EffectGraphWire {
                from_node: 2,
                from_port: "value".to_string(),
                to_node: 1,
                to_port: "intensity".to_string(),
            }],
            tint: None,
        }));

        let existing_spec = float_spec_default("driven_intensity", "Driven", "Light");
        let existing_binding = BindingDef {
            id: "driven_intensity".to_string(),
            label: "Driven".to_string(),
            default_value: 0.25,
            target: BindingTarget::Node {
                node_id: NodeId::new("driven"),
                param: "intensity".to_string(),
            },
            convert: ParamConvert::Float,
            user_added: true,
            scale: 2.0,
            offset: -0.5,
            default_mirrors_node_param: false,
        };
        let expected_existing = existing_binding.clone();

        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: Some(PresetMetadata {
                params: vec![existing_spec],
                bindings: vec![existing_binding],
                ..empty_scene_preset_metadata()
            }),
            scene_modifiers: Vec::new(),
            nodes: vec![driven, source, fresh_driven, unwired, outer, group],
            wires: vec![
                EffectGraphWire {
                    from_node: 8,
                    from_port: "value".to_string(),
                    to_node: 7,
                    to_port: "intensity".to_string(),
                },
                EffectGraphWire {
                    from_node: 8,
                    from_port: "value".to_string(),
                    to_node: 11,
                    to_port: "intensity".to_string(),
                },
            ],
        };
        let vocabulary = ["node.light"];

        assert!(migrate_scene_exposures(
            &mut def,
            &vocabulary,
            |_node| "Light".to_string(),
            &TestProvider,
        ));

        let meta = def.preset_metadata.as_ref().unwrap();
        assert_eq!(
            meta.bindings
                .iter()
                .filter(|binding| matches!(
                    &binding.target,
                    BindingTarget::Node { node_id, param }
                        if node_id == &NodeId::new("driven") && param == "intensity"
                ))
                .count(),
            1,
            "wired fallback does not create a duplicate auto exposure"
        );
        assert_eq!(
            meta.bindings
                .iter()
                .find(|binding| binding.id == expected_existing.id)
                .unwrap(),
            &expected_existing,
            "existing explicit binding remains intact"
        );
        assert!(
            !meta.bindings.iter().any(|binding| matches!(
                &binding.target,
                BindingTarget::Node { node_id, param }
                    if node_id == &NodeId::new("fresh_driven") && param == "intensity"
            )),
            "wired fallback without an existing binding gets no new exposure"
        );
        assert!(
            meta.bindings.iter().any(|binding| matches!(
                &binding.target,
                BindingTarget::Node { node_id, param }
                    if node_id == &NodeId::new("unwired") && param == "intensity"
            )),
            "unwired fallback still receives an auto exposure"
        );
        assert!(
            meta.bindings.iter().any(|binding| matches!(
                &binding.target,
                BindingTarget::Node { node_id, param }
                    if node_id == &NodeId::new("outer") && param == "intensity"
            )),
            "root-scope node with the same numeric id is stamped"
        );
        assert!(
            !meta.bindings.iter().any(|binding| matches!(
                &binding.target,
                BindingTarget::Node { node_id, param }
                    if node_id == &NodeId::new("inner") && param == "intensity"
            )),
            "inner wired fallback uses its own group-scope wires"
        );

        let after_first = def.clone();
        assert!(!migrate_scene_exposures(
            &mut def,
            &vocabulary,
            |_node| "Light".to_string(),
            &TestProvider,
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
        node.params.insert(
            "pos_x".to_string(),
            SerializedParamValue::Float { value: 7.5 },
        );
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
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
        assert_eq!(
            pos_x_spec.default_value, 7.5,
            "spec default seeded from the node's stamped value"
        );
        assert_eq!(
            pos_x_binding.default_value, 7.5,
            "binding default seeded from the node's stamped value"
        );

        let pos_y_spec = meta.params.iter().find(|p| p.name == "Y").unwrap();
        assert_eq!(
            pos_y_spec.default_value, 0.5,
            "unstamped param falls back to the manifest default"
        );
    }

    /// BUG-303: a seeded default outside the manifest's declared min/max
    /// (e.g. a camera `distance` of 300 on a large imported model, against a
    /// manifest range of 0..1) must widen the spec's range to contain it —
    /// a slider whose default sits outside its own min/max is unusable.
    #[test]
    fn seeded_default_outside_manifest_range_widens_min_max() {
        let mut node = make_node(7, "node.orbit_camera");
        node.params.insert(
            "distance".to_string(),
            SerializedParamValue::Float { value: 300.0 },
        );
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
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
        assert!(
            spec.min <= 300.0 && spec.max >= 300.0,
            "range widened to contain the seeded default"
        );
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
        impl NodeParamProvider for TestProvider {
            fn params_for_type(&self, type_id: &str) -> Vec<NodeParamMetadata> {
                if type_id == "node.transform_3d" {
                    vec![float_meta("pos_x", "X")]
                } else {
                    Vec::new()
                }
            }
        }

        let mut node = make_node(7, "node.transform_3d");
        node.params.insert(
            "pos_x".to_string(),
            SerializedParamValue::Float { value: 7.5 },
        );
        let node_id = node.node_id.clone();

        // Simulate a pre-fix def: a spec + binding already stamped at the
        // manifest default (0.5), even though the node itself carries 7.5.
        let stale_spec = ParamSpecDef {
            tooltip: None,
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
            material_role: None,
            limit: Default::default(),
            unit: None,
            scene: None,
        };
        let stale_binding = BindingDef {
            id: "7_pos_x".to_string(),
            label: "X".to_string(),
            default_value: 0.5,
            target: BindingTarget::Node {
                node_id: node_id.clone(),
                param: "pos_x".to_string(),
            },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        };
        // A user-added binding on the SAME param must survive untouched.
        let user_binding = BindingDef {
            id: "user_pos_x".to_string(),
            label: "X (user)".to_string(),
            default_value: 0.5,
            target: BindingTarget::Node {
                node_id: node_id.clone(),
                param: "pos_x".to_string(),
            },
            convert: ParamConvert::Float,
            user_added: true,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: false,
        };

        let mut def = EffectGraphDef {
            scene_modifiers: Vec::new(),
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
        assert!(migrate_scene_exposures(
            &mut def,
            &vocab,
            |_n| "Transform".to_string(),
            &TestProvider
        ));

        let meta = def.preset_metadata.as_ref().unwrap();
        let repaired_spec = meta.params.iter().find(|p| p.id == "7_pos_x").unwrap();
        assert_eq!(
            repaired_spec.default_value, 7.5,
            "spec re-seeded from the node's stamped value"
        );
        let repaired_binding = meta.bindings.iter().find(|b| b.id == "7_pos_x").unwrap();
        assert_eq!(
            repaired_binding.default_value, 7.5,
            "binding re-seeded from the node's stamped value"
        );
        let untouched_user_binding = meta.bindings.iter().find(|b| b.id == "user_pos_x").unwrap();
        assert_eq!(
            untouched_user_binding.default_value, 0.5,
            "user_added binding is never touched by the repair pass"
        );

        let after_repair = def.clone();
        assert!(
            !migrate_scene_exposures(
                &mut def,
                &vocab,
                |_n| "Transform".to_string(),
                &TestProvider
            ),
            "second migration run is a no-op once repaired"
        );
        assert_eq!(def, after_repair);
    }

    // ── Card visibility follows the scene table's role ──────────────────

    fn with_role(mut meta: NodeParamMetadata, role: manifold_foundation::scene_surface::SurfaceRole) -> NodeParamMetadata {
        use manifold_foundation::scene_surface::{CardBand, CardLabel, ControlGroup, SceneParamDesc, SurfaceRole};
        meta.scene = Some(SceneParamDesc {
            role,
            group: ControlGroup::Transform,
            band: (role == SurfaceRole::Performance).then_some(CardBand::Scene),
            card_order: 0,
            card_label: CardLabel::Param,
            gate: None, switch: None, colour: None,
        });
        meta
    }

    /// A stamped control is card-visible exactly when its param has the
    /// performance role; Setup and unclassified params stay off the card.
    /// Every stamp is a real binding either way.
    #[test]
    fn stamp_card_visible_follows_the_role() {
        use manifold_foundation::scene_surface::SurfaceRole;
        let mut def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![make_node(7, "node.transform_3d")],
            wires: vec![],
        };

        assert!(stamp_scene_node_exposures(
            &mut def,
            7,
            "Transform",
            &[
                with_role(float_meta("pos_x", "X"), SurfaceRole::Performance),
                with_role(float_meta("scale_x", "Scale X"), SurfaceRole::Setup),
                float_meta("loose", "Loose"),
            ],
        ));

        let meta = def.preset_metadata.as_ref().unwrap();
        let visible = |name: &str| meta.params.iter().find(|p| p.name == name).unwrap().card_visible;
        assert!(visible("X"));
        assert!(!visible("Scale X"));
        assert!(!visible("Loose"), "a param outside the table stays off the card");
        assert_eq!(meta.bindings.len(), 3);
    }

    fn float_spec_default(id: &str, name: &str, section: &str) -> ParamSpecDef {
        ParamSpecDef {
            tooltip: None,
            id: id.to_string(),
            name: name.to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.5,
            section: Some(section.to_string()),
            ..Default::default()
        }
    }

    // ── scene_scaled_range table tests ────────────────────────────────

    #[test]
    fn scene_radius_from_bounds_zero_is_floor() {
        let r = scene_radius_from_bounds(([0.0; 3], [0.0; 3]));
        assert!(
            (r - 0.01).abs() < 1e-6,
            "degenerate bbox floors at 0.01, got {r}"
        );
    }

    #[test]
    fn scene_radius_from_bounds_symmetric() {
        // [-10, -10, -10]..[10, 10, 10]: diagonal = sqrt(600), radius = sqrt(600)/2
        let r = scene_radius_from_bounds(([-10.0; 3], [10.0; 3]));
        let expected = (300.0_f32).sqrt(); // sqrt(1200)/2 = sqrt(300)
        assert!(
            (r - expected).abs() < 1e-3,
            "symmetric bbox radius, got {r} expected {expected}"
        );
    }

    #[test]
    fn scene_scaled_range_returns_none_for_unknown() {
        assert!(scene_scaled_range("node.value", "value", 1.0).is_none());
        assert!(scene_scaled_range("node.transform_3d", "scale_x", 1.0).is_none());
    }

    #[test]
    fn scene_scaled_range_position_is_symmetric() {
        let (min, max) = scene_scaled_range("node.transform_3d", "pos_x", 5.0).unwrap();
        assert!((min - -10.0).abs() < 1e-6);
        assert!((max - 10.0).abs() < 1e-6);
    }

    #[test]
    fn scene_scaled_range_orbit_far_capped_at_10000() {
        let (_, max) = scene_scaled_range("node.orbit_camera", "far", 1000.0).unwrap();
        assert!(
            (max - 10_000.0).abs() < 1e-6,
            "far capped at 10_000, got {max}"
        );
    }

    #[test]
    fn scene_scaled_range_fog_density_inversely_scales() {
        let (_, max_small) = scene_scaled_range("node.atmosphere", "fog_density", 1.0).unwrap();
        let (_, max_large) = scene_scaled_range("node.atmosphere", "fog_density", 10.0).unwrap();
        assert!((max_small - 2.0).abs() < 1e-6);
        assert!((max_large - 0.2).abs() < 1e-6);
    }
}
