//! BUG-1l7f: write-site warning for card-owned def node params.
//!
//! Setting a param on a def node whose card binding carries an AUTHORED
//! default is a silent no-op — `apply_binding_defaults` replants the
//! default at the next runtime rebuild and the write reverts. The runtime
//! reports the shadow at BUILD time (`ChainError::CardBindingShadowsDefParam`);
//! this module names it at WRITE time, where the author actually is.
//! Extracted from `node_edit.rs` to keep that file under its godfile
//! ceiling (the godfile campaign owns the ceilings; new behavior lands in
//! new modules).

/// BUG-1l7f: a write to a def node param that a card binding owns with an
/// AUTHORED default is dead on arrival — `apply_binding_defaults` replants
/// the default at the next runtime rebuild and the write silently reverts.
/// The runtime already reports the shadow at BUILD time
/// (`ChainError::CardBindingShadowsDefParam`); this names it at WRITE time,
/// where the author actually is. Mirrored defaults are excluded (they no
/// longer plant — BUG-ji6q — so the node write stands). Root-level edits
/// only: binding metadata addresses root node ids, and a scoped
/// (group-internal) edit's numeric id doesn't resolve against them.
/// Returns the loud message; the caller decides the channel.
pub(crate) fn card_owned_write_warning(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    node_id_num: u32,
    param: &str,
) -> Option<String> {
    use manifold_core::effect_graph_def::BindingTarget;
    let meta = def.preset_metadata.as_ref()?;
    let node_id = &def.nodes.iter().find(|n| n.id == node_id_num)?.node_id;
    let binding = meta.bindings.iter().find(|b| {
        !b.default_mirrors_node_param
            && matches!(&b.target, BindingTarget::Node { node_id: bid, param: bp }
                if bid == node_id && bp == param)
    })?;
    Some(format!(
        "[manifold-editing] param write to `{}`.{} is card-owned by binding `{}` \
         (authored default {}) — the binding replants its default at the next runtime \
         rebuild and this write reverts (BUG-1l7f). Drive it through the card slot instead.",
        node_id.as_str(),
        param,
        binding.id,
        binding.default_value,
    ))
}

#[cfg(test)]
mod tests {
    use super::card_owned_write_warning;
    use manifold_core::effect_graph_def::EffectGraphDef;
    use std::collections::BTreeMap;
    use crate::command::Command;
    use super::super::SetGraphNodeParamCommand;
    use super::super::test_support::*;
    use manifold_core::effect_graph_def::SerializedParamValue;
    use manifold_core::effects::PresetInstance;
    use manifold_core::project::Project;
    use manifold_core::{EffectId, GraphTarget, PresetTypeId};

// ── BUG-1l7f redirect fixtures ──────────────────────────────────────

/// One node (`id 1`, `node_id "n_a"`) whose `amount` param is owned by
/// an authored-default card binding `amount` with a ×2 affine fold, and
/// a matching slider spec. `mirror` flips the binding to a stamp-time
/// mirror (BUG-ji6q: never plants, node writes stand — no redirect).
fn card_owned_def(mirror: bool) -> EffectGraphDef {
    use manifold_core::effect_graph_def::{
        BindingDef, BindingTarget, EFFECT_GRAPH_VERSION, EffectGraphNode, ParamSpecDef,
        PresetMetadata, SkipModeDef,
    };
    use manifold_core::effects::ParamConvert;
    use manifold_core::macro_bank::MacroCurve;
    use manifold_core::{NodeId, PresetTypeId};
    let spec = ParamSpecDef {
        id: "amount".to_string(),
        name: "Amount".to_string(),
        min: 0.0,
        max: 1.0,
        default_value: 0.0,
        whole_numbers: false,
        is_toggle: false,
        is_trigger: false,
        value_labels: Vec::new(),
        format_string: None,
        osc_suffix: String::new(),
        curve: MacroCurve::Linear,
        invert: false,
        is_angle: false,
        is_trigger_gate: false,
        wraps: false,
        section: None,
        card_visible: true,
    };
    EffectGraphDef {
        version: EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: Some(PresetMetadata {
            id: PresetTypeId::new("CardOwned"),
            display_name: "Card Owned".to_string(),
            category: "Stylize".to_string(),
            osc_prefix: "card_owned".to_string(),
            legacy_discriminant: None,
            scene_bounds: None,
            available: true,
            is_line_based: false,
            params: vec![spec],
            bindings: vec![BindingDef {
                id: "amount".to_string(),
                label: "Amount".to_string(),
                default_value: 0.25,
                target: BindingTarget::Node {
                    node_id: NodeId::new("n_a"),
                    param: "amount".to_string(),
                },
                convert: ParamConvert::Float,
                user_added: false,
                scale: 2.0,
                offset: 0.0,
                default_mirrors_node_param: mirror,
            }],
            skip_mode: SkipModeDef::Never,
            param_aliases: Vec::new(),
            value_aliases: Vec::new(),
            string_params: Vec::new(),
            string_bindings: Vec::new(),
        }),
        nodes: vec![EffectGraphNode {
            id: 1,
            node_id: NodeId::new("n_a"),
            type_id: "node.x".to_string(),
            handle: None,
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        }],
        wires: Vec::new(),
    }
}

/// Project + effect whose graph carries the card-owned def and whose
/// manifest holds the `amount` card slot (base 0.0). `with_manifest`
/// false simulates a pruned exposure — the slot is gone.
fn project_with_card_owned(mirror: bool, with_manifest: bool) -> (Project, EffectId) {
    let mut project = Project::default();
    let effect_id = EffectId::new("test-card-fx");
    let mut fx = PresetInstance::new(PresetTypeId::new("test.fx"));
    fx.id = effect_id.clone();
    fx.graph = Some(card_owned_def(mirror));
    if with_manifest {
        let spec = card_owned_def(mirror)
            .preset_metadata
            .unwrap()
            .params
            .pop()
            .unwrap();
        fx.params
            .push(manifold_core::params::Param::bundled(spec));
    }
    project.settings.master_effects.push(fx);
    (project, effect_id)
}

fn card_value_of(project: &Project, id: &EffectId) -> f32 {
    project.find_effect_by_id(id).unwrap().get_param("amount")
}

/// BUG-1l7f: a def-node write to a card-owned param reroutes through the
/// card slot — manifest moves to the inverse-reshaped value, the def
/// node is never written, undo restores the card.
#[test]
fn card_owned_write_reroutes_through_card_slot() {
    let (mut project, fx) = project_with_card_owned(false, true);
    let mut cmd = SetGraphNodeParamCommand::new(
        GraphTarget::Effect(fx.clone()),
        1,
        "amount".to_string(),
        SerializedParamValue::Float { value: 0.5 },
        card_owned_def(false),
    );
    cmd.execute(&mut project);
    // fold is out = v*2 + 0 → inverse of 0.5 is 0.25 on the card.
    assert!(
        (card_value_of(&project, &fx) - 0.25).abs() < 1e-6,
        "card slot carries the inverse-reshaped value: {}",
        card_value_of(&project, &fx)
    );
    assert!(
        graph_of(&project, &fx).nodes[0].params.is_empty(),
        "def node never written — the card is the sole authority"
    );
    cmd.undo(&mut project);
    assert!(
        (card_value_of(&project, &fx) - 0.0).abs() < 1e-6,
        "undo restores the pre-edit card value: {}",
        card_value_of(&project, &fx)
    );
}

/// BUG-1l7f: with no per-instance override the lookup reads the catalog
/// default WITHOUT lifting it into an override — a card write never
/// dirties the def.
#[test]
fn card_owned_write_reads_catalog_without_lifting() {
    let mut project = Project::default();
    let effect_id = EffectId::new("test-card-fx");
    let mut fx = PresetInstance::new(PresetTypeId::new("test.fx"));
    fx.id = effect_id.clone();
    let spec = card_owned_def(false)
        .preset_metadata
        .unwrap()
        .params
        .pop()
        .unwrap();
    fx.params.push(manifold_core::params::Param::bundled(spec));
    project.settings.master_effects.push(fx);

    let mut cmd = SetGraphNodeParamCommand::new(
        GraphTarget::Effect(effect_id.clone()),
        1,
        "amount".to_string(),
        SerializedParamValue::Float { value: 0.5 },
        card_owned_def(false),
    );
    cmd.execute(&mut project);
    assert!(
        (card_value_of(&project, &effect_id) - 0.25).abs() < 1e-6,
        "redirect resolves against the catalog default"
    );
    assert!(
        project.find_effect_by_id(&effect_id).unwrap().graph.is_none(),
        "no override lifted for a card write"
    );
}

/// Mirrored defaults never plant (BUG-ji6q), so the node write stands:
/// no redirect, the def takes the value.
#[test]
fn mirrored_default_stays_on_def_write_path() {
    let (mut project, fx) = project_with_card_owned(true, true);
    let mut cmd = SetGraphNodeParamCommand::new(
        GraphTarget::Effect(fx.clone()),
        1,
        "amount".to_string(),
        SerializedParamValue::Float { value: 0.5 },
        card_owned_def(true),
    );
    cmd.execute(&mut project);
    assert_eq!(
        graph_of(&project, &fx).nodes[0].params.get("amount"),
        Some(&SerializedParamValue::Float { value: 0.5 }),
        "mirrored binding: def write stands"
    );
    assert!(
        (card_value_of(&project, &fx) - 0.0).abs() < 1e-6,
        "card slot untouched"
    );
}

/// A non-scalar value has no card reading — the write stays on the def
/// path (with the loud warning) rather than dropping data.
#[test]
fn non_scalar_value_stays_on_def_write_path() {
    let (mut project, fx) = project_with_card_owned(false, true);
    let mut cmd = SetGraphNodeParamCommand::new(
        GraphTarget::Effect(fx.clone()),
        1,
        "amount".to_string(),
        SerializedParamValue::Vec2 { value: [1.0, 2.0] },
        card_owned_def(false),
    );
    cmd.execute(&mut project);
    assert_eq!(
        graph_of(&project, &fx).nodes[0].params.get("amount"),
        Some(&SerializedParamValue::Vec2 { value: [1.0, 2.0] }),
        "non-scalar falls through to the def write"
    );
    assert!((card_value_of(&project, &fx) - 0.0).abs() < 1e-6);
}

/// A pruned exposure leaves no manifest slot to write — the redirect
/// refuses (rather than silently no-oping) and the def path takes it.
#[test]
fn pruned_card_slot_refuses_redirect() {
    let (mut project, fx) = project_with_card_owned(false, false);
    let mut cmd = SetGraphNodeParamCommand::new(
        GraphTarget::Effect(fx.clone()),
        1,
        "amount".to_string(),
        SerializedParamValue::Float { value: 0.5 },
        card_owned_def(false),
    );
    cmd.execute(&mut project);
    assert_eq!(
        graph_of(&project, &fx).nodes[0].params.get("amount"),
        Some(&SerializedParamValue::Float { value: 0.5 }),
        "no manifest slot → def write, never a silent drop"
    );
}


    /// BUG-1l7f: the write-site warning fires exactly for a param a card
    /// binding owns with an AUTHORED default — not for mirrored defaults
    /// (BUG-ji6q: they don't plant, the node write stands), not for
    /// unbound params.
    #[test]
    fn card_owned_write_warning_scopes_to_authored_defaults() {
        use manifold_core::effect_graph_def::{
            BindingDef, BindingTarget, EFFECT_GRAPH_VERSION, EffectGraphNode, PresetMetadata,
            SkipModeDef,
        };
        use manifold_core::effects::ParamConvert;
        use manifold_core::{NodeId, PresetTypeId};

        let binding = |id: &str, node: &str, param: &str, mirror: bool| BindingDef {
            id: id.to_string(),
            label: id.to_string(),
            default_value: 1.0,
            target: BindingTarget::Node {
                node_id: NodeId::new(node),
                param: param.to_string(),
            },
            convert: ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
            default_mirrors_node_param: mirror,
        };
        let def = EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: Some(PresetMetadata {
                id: PresetTypeId::new("WarnTest"),
                display_name: "Warn Test".to_string(),
                category: "Stylize".to_string(),
                osc_prefix: "warn_test".to_string(),
                legacy_discriminant: None,
                scene_bounds: None,
                available: true,
                is_line_based: false,
                params: Vec::new(),
                bindings: vec![
                    binding("authored", "n_a", "amount", false),
                    binding("mirrored", "n_b", "radius", true),
                ],
                skip_mode: SkipModeDef::Never,
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
            }),
            nodes: vec![
                EffectGraphNode {
                    id: 1,
                    node_id: NodeId::new("n_a"),
                    type_id: "node.x".to_string(),
                    handle: None,
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: BTreeMap::new(),
                    output_canvas_scales: BTreeMap::new(),
                    group: None,
                },
                EffectGraphNode {
                    id: 2,
                    node_id: NodeId::new("n_b"),
                    type_id: "node.y".to_string(),
                    handle: None,
                    params: BTreeMap::new(),
                    exposed_params: Default::default(),
                    editor_pos: None,
                    wgsl_source: None,
                    title: None,
                    output_formats: BTreeMap::new(),
                    output_canvas_scales: BTreeMap::new(),
                    group: None,
                },
            ],
            wires: Vec::new(),
        };

        let authored = card_owned_write_warning(&def, 1, "amount");
        assert!(
            authored.as_ref().is_some_and(|w| w.contains("authored") && w.contains("n_a")),
            "authored-default binding must produce the loud write-site warning: {authored:?}"
        );
        assert_eq!(
            card_owned_write_warning(&def, 2, "radius"),
            None,
            "mirrored defaults don't plant (BUG-ji6q) — no warning"
        );
        assert_eq!(
            card_owned_write_warning(&def, 1, "unbound_param"),
            None,
            "unbound params are ordinary writes — no warning"
        );
        assert_eq!(
            card_owned_write_warning(&def, 99, "amount"),
            None,
            "unknown node id resolves to nothing — no warning"
        );
    }
}
