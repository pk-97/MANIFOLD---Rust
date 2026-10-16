//! Effect-group helpers for [`PresetRuntime`].

use super::*;
use super::errors::record_chain_error;
use crate::scene::mesh_change::PreparedMeshRules;

/// State tracked for an open partial-wet-dry group during
/// `try_build`'s walk over active effects. Captures the pre-group
/// node + port so the Mix's `a` (dry) input wires from the same
/// source as the group's first effect, and the group's `wet_dry`
/// value so the Mix's `amount` param can be set at build time.
pub(super) struct OpenGroup {
    pub(super) group_id: EffectGroupId,
    pub(super) pre_node: NodeInstanceId,
    pub(super) pre_port: &'static str,
    pub(super) wet_dry: f32,
    pub(super) mask_expected: bool,
    pub(super) mask_output: Option<(NodeInstanceId, &'static str)>,
}

/// Emit the Mix sub-graph for a closing partial-wet-dry group:
/// `dry = pre_group_output`, `wet = last_effect_output`,
/// `out = lerp(dry, wet, wet_dry)`. Returns the Mix node id and
/// its output port (`"out"`).
pub(super) fn close_mix_group(
    graph: &mut Graph,
    closing: &OpenGroup,
    last_effect: (NodeInstanceId, &'static str),
) -> Option<(NodeInstanceId, &'static str)> {
    let mix_id = if closing.mask_expected {
        let mask = closing.mask_output?;
        let id = graph.add_node(Box::new(
            crate::primitives::masked_mix::MaskedMix::new(),
        ));
        graph.connect(mask, (id, "mask")).ok()?;
        id
    } else {
        let id = graph.add_node(Box::new(Mix::new()));
        graph.set_param(id, "mode", ParamValue::Enum(0)).ok()?;
        id
    };
    // Mode = Lerp (0) — matches legacy `WetDryLerpPipeline`'s
    // `lerp(dry, wet, wet_dry)`.
    graph
        .set_param(mix_id, "amount", ParamValue::Float(closing.wet_dry))
        .ok()?;
    // Mix.a = dry (pre-group input). Already wired into the
    // group's first effect via this same output port — output
    // ports can fan out to many input ports, so adding a second
    // consumer is legal.
    graph
        .connect((closing.pre_node, closing.pre_port), (mix_id, "a"))
        .ok()?;
    // Mix.b = wet (post-group result).
    graph.connect(last_effect, (mix_id, "b")).ok()?;
    Some((mix_id, "out"))
}

/// The active (enabled, group-enabled) effects of a chain, with their original
/// indices into `effects`. Shared between the chain build and the project-load
/// segment prewarm so both walk the identical card list.
pub(super) fn chain_active_effects<'a>(
    effects: &'a [PresetInstance],
    groups: &[EffectGroup],
) -> Vec<(usize, &'a PresetInstance)> {
    effects
        .iter()
        .enumerate()
        .filter(|(_, fx)| is_chain_active(fx, groups))
        .collect()
}

/// Whether [`chain_active_effects`] would be non-empty, without collecting.
pub(super) fn has_chain_active_effects(effects: &[PresetInstance], groups: &[EffectGroup]) -> bool {
    effects.iter().any(|fx| is_chain_active(fx, groups))
}

fn is_chain_active(fx: &PresetInstance, groups: &[EffectGroup]) -> bool {
    if !fx.enabled {
        return false;
    }
    !fx.group_id.as_deref().is_some_and(|gid| {
        groups.iter().find(|g| g.id.as_str() == gid).is_some_and(|g| !g.enabled)
    })
}

/// Validate mask membership and contiguity before assembling the chain graph.
pub(super) fn validate_mask_groups(
    effects: &[PresetInstance],
    groups: &[EffectGroup],
) -> Option<()> {
    for group in groups {
        if let Some(mask_id) = &group.mask_effect_id {
            let member = effects.iter().find(|fx| &fx.id == mask_id);
            if !member.is_some_and(|fx| fx.group_id.as_ref() == Some(&group.id)) {
                eprintln!(
                    "[chain-build-fail] group {} has missing mask member {}",
                    group.id, mask_id
                );
                return None;
            }
            let first = effects
                .iter()
                .position(|fx| fx.group_id.as_ref() == Some(&group.id))?;
            let last = effects
                .iter()
                .rposition(|fx| fx.group_id.as_ref() == Some(&group.id))?;
            if effects[first..=last]
                .iter()
                .any(|fx| fx.group_id.as_ref() != Some(&group.id))
            {
                eprintln!(
                    "[chain-build-fail] masked group {} is not contiguous",
                    group.id
                );
                return None;
            }
        }
    }
    Some(())
}

/// Splice one effect card's def into the chain graph, its mesh-rule
/// sidecar traveling with the def (SCENE_MODIFIER_RT_DESIGN.md section 3.3):
/// a fused card's sidecar is keyed by the fused def's generated node ids;
/// an unfused/edited def has none (empty map is correct only when fusion
/// did not occur). When the divergent (edited/fused) def fails to splice,
/// record the divergence and fall back to the canonical def + canonical
/// sidecar. Retired CPU FLIP authored graphs explicitly refuse that fallback
/// so their saved topology cannot be silently replaced. Returns None when the
/// canonical splice itself fails or a retired graph refuses fallback.
/// Which def [`splice_card_with_canonical_fallback`] spliced. The caller binds
/// the card through the view that matches it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SplicedDef {
    Primary,
    CanonicalFallback,
}

/// `canonical_relight` is the relight toggle for the canonical fallback: a
/// fused primary carries its relight template inside the fused def, the
/// canonical def does not.
#[allow(clippy::too_many_arguments)]
pub(crate) fn splice_card_with_canonical_fallback(
    graph: &mut Graph,
    card_input: (NodeInstanceId, &'static str),
    splice_def: &EffectGraphDef,
    mesh_rules: &PreparedMeshRules,
    canonical_def: &EffectGraphDef,
    canonical_mesh_rules: &PreparedMeshRules,
    primitives: &PrimitiveRegistry,
    relight_params: Option<&RelightParams>,
    canonical_relight: Option<&RelightParams>,
    divergent: Option<(EffectId, PresetTypeId)>,
    preserve_authored_cpu_flip_graph: bool,
    errors: &mut Vec<ChainError>,
) -> Option<(SpliceResult, SplicedDef)> {
    if let Some(r) = splice_def_into_chain(
        graph,
        card_input,
        splice_def,
        primitives,
        relight_params,
        mesh_rules,
    ) {
        return Some((r, SplicedDef::Primary));
    }
    if preserve_authored_cpu_flip_graph {
        eprintln!(
            "[chain-build-fail] retired CPU FLIP authored graph failed to splice; canonical fallback disabled"
        );
        return None;
    }
    if let Some((effect_id, effect_type)) = divergent {
        record_chain_error(
            errors,
            ChainError::DivergentGraphFellBack { effect_id, effect_type },
        );
    }
    match splice_def_into_chain(
        graph,
        card_input,
        canonical_def,
        primitives,
        canonical_relight,
        canonical_mesh_rules,
    ) {
        Some(r) => Some((r, SplicedDef::CanonicalFallback)),
        None => {
            eprintln!("[chain-build-fail] canonical splice failed after fallback");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(map_type: &str) -> EffectGraphDef {
        serde_json::from_value(serde_json::json!({
            "version": 3, "name": "fallback-probe",
            "nodes": [
                {"id": 0, "nodeId": "source", "typeId": "system.source"},
                {"id": 1, "nodeId": "map", "handle": "map", "typeId": map_type},
                {"id": 2, "nodeId": "final_output", "typeId": "system.final_output"}
            ],
            "wires": [
                {"fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in"},
                {"fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in"}
            ]
        }))
        .expect("probe def parses")
    }

    /// A fused splice that fails falls back to the canonical def and says so,
    /// so the chain build binds the card through the canonical view: the
    /// canonical binding reaches the spliced node, the fused one cannot.
    #[test]
    fn failed_fused_splice_reports_canonical_fallback() {
        let mut primitives = PrimitiveRegistry::with_builtin();
        crate::testkit::fusion_fixtures::register_fusion_test_nodes(&mut primitives);
        let fused = def("test.no_such_fused_kernel");
        let canonical = def("test.fusion_map");
        let mut graph = Graph::new();
        let source = graph.add_node(Box::new(crate::scene::boundary_nodes::Source::new()));
        let mut errors = Vec::new();
        let (splice, spliced) = splice_card_with_canonical_fallback(
            &mut graph,
            (source, "out"),
            &fused,
            &PreparedMeshRules::default(),
            &canonical,
            &PreparedMeshRules::default(),
            &primitives,
            None,
            None,
            Some((EffectId::new("fx"), PresetTypeId::new("Probe"))),
            false,
            &mut errors,
        )
        .expect("the canonical def splices");
        assert_eq!(spliced, SplicedDef::CanonicalFallback);
        assert!(errors.iter().any(|e| matches!(e, ChainError::DivergentGraphFellBack { .. })));

        let node_map: Vec<(NodeId, NodeInstanceId)> = splice
            .handles
            .iter()
            .filter_map(|(_, id)| graph.get_node(*id).map(|inst| (inst.node_id.clone(), *id)))
            .collect();
        assert!(
            node_map.iter().any(|(nid, _)| nid.as_str() == "map"),
            "the canonical binding's node is in the fallback graph",
        );
        assert!(
            !node_map.iter().any(|(nid, _)| nid.as_str().starts_with("fused_region")),
            "a fused binding target does not exist in the fallback graph",
        );
    }
}
