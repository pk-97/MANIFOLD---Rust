//! Configured array-cycle classification.
use crate::persistence::PrimitiveRegistry;
use manifold_core::effect_graph_def::EffectGraphDef;
pub fn cycle_contains_array(
    start: u32,
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> bool {
    super::cycle_contains_array(start, def, registry)
}
