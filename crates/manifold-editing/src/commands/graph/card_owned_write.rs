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
    use super::*;
    use manifold_core::effect_graph_def::EffectGraphDef;
    use std::collections::BTreeMap;

    /// BUG-1l7f: the write-site warning fires exactly for a param a card
    /// binding owns with an AUTHORED default — not for mirrored defaults
    /// (BUG-ji6q: they don't plant, the node write stands), not for
    /// unbound params.
    #[test]
    fn card_owned_write_warning_scopes_to_authored_defaults() {
        use super::card_owned_write_warning;
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
