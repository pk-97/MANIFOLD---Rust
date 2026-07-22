//! Outer-card param / binding constructors for an imported model's curated
//! performance surface (camera / light / material knobs, animation clocks).

use manifold_core::NodeId;
use manifold_core::effect_graph_def::{BindingDef, BindingTarget, ParamSpecDef};

use crate::node_graph::gltf_load;
use crate::node_graph::primitives::gltf_anim_shared::LOOP_MODES;

/// One outer-card slider definition. Curated performance surface for an
/// imported model (camera framing, light, material) — see
/// [`assemble_import_graph`]'s metadata block. `default_value` must match
/// the wired node param's value (through `scale`) so the card reproduces the
/// assembler's look on first frame with no drift. `section` bundles this
/// knob under a collapsible card header (D5/D9,
/// SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2): per-object knobs get the
/// object's group name, shared knobs get `"Camera"`/`"Sun"`/`"Environment"`.
pub(super) fn card_param(
    id: &str,
    name: &str,
    min: f32,
    max: f32,
    default: f32,
    is_angle: bool,
    section: &str,
) -> ParamSpecDef {
    ParamSpecDef {
        id: id.to_string(),
        name: name.to_string(),
        min,
        max,
        default_value: default,
        whole_numbers: false,
        is_toggle: false,
        is_trigger: false,
        value_labels: Vec::new(),
        format_string: None,
        osc_suffix: String::new(),
        curve: manifold_core::macro_bank::MacroCurve::default(),
        invert: false,
        is_angle,
        is_trigger_gate: false,
        wraps: false,
        section: Some(section.to_string()),
    }
}

/// Route one card slider (`id`) to one inner node param. `scale` folds a
/// unit conversion into the write boundary; pass `1.0` for a pass-through.
/// `default_value` mirrors the matching [`card_param`]'s `default` so the
/// slider's fallback (when a project carries no `param_values` slot) still
/// reproduces the authored look.
pub(super) fn card_binding(
    id: &str,
    name: &str,
    default: f32,
    node_id: &str,
    param: &str,
    scale: f32,
) -> BindingDef {
    BindingDef {
        id: id.to_string(),
        label: name.to_string(),
        default_value: default,
        target: BindingTarget::Node {
            node_id: NodeId::new(node_id),
            param: param.to_string(),
        },
        convert: manifold_core::effects::ParamConvert::Float,
        user_added: false,
        scale,
        offset: 0.0,
    }
}

/// ONE set of
/// animation card knobs per glb, not one per animated object. Clips are
/// file-level in glTF — every `node.gltf_animation_source` /
/// `node.gltf_skeleton_pose` / `node.gltf_morph_weights` in one import
/// indexes the same clip list — so Rate / Clip / Loop Mode / Retrigger are
/// a single linked control that fans out to every animation clock in the
/// file (the binding apply loop resolves by `source_id`, so N bindings
/// sharing one card param is the same supported shape as Lissajous's
/// `clip_trigger`). This fn pushes only the four shared PARAMS;
/// [`animation_card_bindings`] pushes the four per-node bindings each
/// animated object contributes. `clip_count` sizes the Clip knob's range
/// (0 when unknown treated as 1, so a single-clip file still gets a valid,
/// if inert, 0..0 range rather than an empty one); `clip_labels` (clip
/// names from the file, padded/defaulted by the caller) label its detents.
pub(super) fn animation_card_params(
    card_params: &mut Vec<ParamSpecDef>,
    id_prefix: &str,
    section: &str,
    clip_count: u32,
    clip_labels: Vec<String>,
) {
    let base = ParamSpecDef {
        id: String::new(),
        name: String::new(),
        min: 0.0,
        max: 1.0,
        default_value: 0.0,
        whole_numbers: false,
        is_toggle: false,
        is_trigger: false,
        value_labels: Vec::new(),
        format_string: None,
        osc_suffix: String::new(),
        curve: manifold_core::macro_bank::MacroCurve::default(),
        invert: false,
        is_angle: false,
        is_trigger_gate: false,
        wraps: false,
        section: Some(section.to_string()),
    };

    card_params.push(ParamSpecDef {
        id: format!("{id_prefix}_rate"),
        name: "Rate".to_string(),
        min: 0.0625,
        max: 16.0,
        default_value: 1.0,
        ..base.clone()
    });

    card_params.push(ParamSpecDef {
        id: format!("{id_prefix}_clip"),
        name: "Clip".to_string(),
        min: 0.0,
        max: (clip_count.max(1) - 1) as f32,
        default_value: 0.0,
        whole_numbers: true,
        value_labels: clip_labels,
        ..base.clone()
    });

    card_params.push(ParamSpecDef {
        id: format!("{id_prefix}_loop_mode"),
        name: "Loop Mode".to_string(),
        min: 0.0,
        max: (LOOP_MODES.len() - 1) as f32,
        default_value: 0.0,
        whole_numbers: true,
        value_labels: LOOP_MODES.iter().map(|s| s.to_string()).collect(),
        ..base.clone()
    });

    card_params.push(ParamSpecDef {
        id: format!("{id_prefix}_retrigger"),
        name: "Retrigger".to_string(),
        min: 0.0,
        max: 1_000_000.0,
        default_value: 0.0,
        whole_numbers: true,
        is_trigger: true,
        ..base
    });
}

/// Detent labels for the per-glb Clip knob: the file's clip names where
/// present, `"Clip N"` otherwise. Empty when the file has no clips at all
/// (the knob's 0..0 range needs no labels).
pub(super) fn clip_labels(animations: &[gltf_load::GltfAnimationInfo]) -> Vec<String> {
    animations
        .iter()
        .enumerate()
        .map(|(i, a)| {
            a.name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("Clip {}", i + 1))
        })
        .collect()
}

/// The fan-out half of [`animation_card_params`]: the four bindings wiring
/// ONE animation-clock node (`node.gltf_animation_source` /
/// `node.gltf_skeleton_pose` / `node.gltf_morph_weights`) to the shared
/// per-glb card knobs. Every animated object in an import pushes one of
/// these sets against the SAME `{id_prefix}_*` source ids, so the whole
/// file's clocks move together from one card section.
pub(super) fn animation_card_bindings(card_bindings: &mut Vec<BindingDef>, id_prefix: &str, node_id: &str) {
    card_bindings.push(card_binding(&format!("{id_prefix}_rate"), "Rate", 1.0, node_id, "rate", 1.0));
    card_bindings.push(BindingDef {
        id: format!("{id_prefix}_clip"),
        label: "Clip".to_string(),
        default_value: 0.0,
        target: BindingTarget::Node { node_id: NodeId::new(node_id), param: "clip_index".to_string() },
        convert: manifold_core::effects::ParamConvert::IntRound,
        user_added: false,
        scale: 1.0,
        offset: 0.0,
    });
    card_bindings.push(BindingDef {
        id: format!("{id_prefix}_loop_mode"),
        label: "Loop Mode".to_string(),
        default_value: 0.0,
        target: BindingTarget::Node { node_id: NodeId::new(node_id), param: "loop_mode".to_string() },
        convert: manifold_core::effects::ParamConvert::EnumRound,
        user_added: false,
        scale: 1.0,
        offset: 0.0,
    });
    card_bindings.push(BindingDef {
        id: format!("{id_prefix}_retrigger"),
        label: "Retrigger".to_string(),
        default_value: 0.0,
        target: BindingTarget::Node { node_id: NodeId::new(node_id), param: "trigger_count".to_string() },
        convert: manifold_core::effects::ParamConvert::Trigger,
        user_added: false,
        scale: 1.0,
        offset: 0.0,
    });
}
