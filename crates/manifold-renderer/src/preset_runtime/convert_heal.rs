//! Load-time binding heals for [`PresetRuntime`] construction — extracted
//! from build.rs (the from_* constructors) to keep that file under its
//! god-file ceiling.

use super::*;

/// Load-time heal: upgrade a Float/IntRound-convert binding whose target
/// param is declared Bool to BoolThreshold. A Float write into a Bool slot
/// never lands (readers match `ParamValue::Bool` exactly), so the poisoned
/// form is strictly dead — thresholding is the semantics the stamp always
/// meant. Returns the number of bindings healed.
pub(super) fn heal_bool_convert_bindings(doc: &mut EffectGraphDef, registry: &PrimitiveRegistry) -> usize {
    use crate::node_graph::ParamType;
    use manifold_core::effect_graph_def::BindingTarget;
    use manifold_core::effects::ParamConvert;

    fn collect<'a>(
        nodes: &'a [manifold_core::effect_graph_def::EffectGraphNode],
        out: &mut ahash::AHashMap<&'a str, &'a str>,
    ) {
        for n in nodes {
            out.insert(n.node_id.as_str(), n.type_id.as_str());
            if let Some(group) = n.group.as_deref() {
                collect(&group.nodes, out);
            }
        }
    }
    let mut node_types = ahash::AHashMap::new();
    collect(&doc.nodes, &mut node_types);

    let Some(meta) = doc.preset_metadata.as_mut() else {
        return 0;
    };
    let mut healed = 0usize;
    for binding in &mut meta.bindings {
        let BindingTarget::Node { node_id, param } = &binding.target else {
            continue;
        };
        if !matches!(binding.convert, ParamConvert::Float | ParamConvert::IntRound) {
            continue;
        }
        let Some(type_id) = node_types.get(node_id.as_str()) else {
            continue;
        };
        let is_bool_target = registry
            .construct(type_id)
            .and_then(|node| {
                node.parameters()
                    .iter()
                    .find(|p| p.name == param.as_str())
                    .map(|p| p.ty)
            })
            .is_some_and(|ty| ty == ParamType::Bool);
        if is_bool_target {
            binding.convert = ParamConvert::BoolThreshold;
            healed += 1;
        }
    }
    if healed > 0 {
        log::info!(
            "[preset] healed {healed} Float-convert binding(s) into Bool targets → BoolThreshold \
             (legacy tail stamps)"
        );
    }
    healed
}
