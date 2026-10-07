//! Configured array-cycle classification.
use manifold_core::effect_graph_def::EffectGraphDef;
use crate::node_graph::PrimitiveRegistry;
pub(crate) fn cycle_contains_array(start: u32, def: &EffectGraphDef, registry: &PrimitiveRegistry) -> bool {
    super::cycle_contains_array(start, def, registry)
}
