//! Effect-group helpers for [`PresetRuntime`].

use super::*;
use super::errors::record_chain_error;
use crate::node_graph::mesh_change::PreparedMeshRules;

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
            crate::node_graph::primitives::MaskedMix::new(),
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
        .filter(|(_, fx)| {
            if !fx.enabled {
                return false;
            }
            if let Some(gid) = fx.group_id.as_deref()
                && let Some(group) = groups.iter().find(|g| g.id.as_str() == gid)
                && !group.enabled
            {
                return false;
            }
            true
        })
        .collect()
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
/// sidecar. Returns None only when the canonical splice itself fails.
#[allow(clippy::too_many_arguments)]
pub(super) fn splice_card_with_canonical_fallback(
    graph: &mut Graph,
    card_input: (NodeInstanceId, &'static str),
    splice_def: &EffectGraphDef,
    mesh_rules: &PreparedMeshRules,
    canonical_def: &EffectGraphDef,
    canonical_mesh_rules: &PreparedMeshRules,
    primitives: &PrimitiveRegistry,
    relight_params: Option<&RelightParams>,
    divergent: Option<(EffectId, PresetTypeId)>,
    errors: &mut Vec<ChainError>,
) -> Option<SpliceResult> {
    if let Some(r) = splice_def_into_chain(
        graph,
        card_input,
        splice_def,
        primitives,
        relight_params,
        mesh_rules,
    ) {
        return Some(r);
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
        relight_params,
        canonical_mesh_rules,
    ) {
        Some(r) => Some(r),
        None => {
            eprintln!("[chain-build-fail] canonical splice failed after fallback");
            None
        }
    }
}
