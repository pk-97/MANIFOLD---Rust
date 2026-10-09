//! Read accepted water snapshots through the existing runtime scopes.
use manifold_core::id::EffectId;

impl super::WaterRuntimeRef<'_> {
    /// Append accepted fluid domains from this effect's running nodes. The
    /// caller owns and reuses the output buffer; reading never polls a worker.
    pub fn write_fluid_domains(
        &self,
        effect_id: &EffectId,
        output: &mut Vec<(
            manifold_core::NodeId,
            crate::water::fluid::FluidDomainSnapshot,
        )>,
    ) {
        let Some(slot) = self
            .effect_nodes
            .iter()
            .find(|slot| slot.effect_id == effect_id)
        else {
            return;
        };
        for (node_id, instance) in slot.node_map {
            if let Some(snapshot) = self
                .graph
                .get_node(*instance)
                .and_then(|node| crate::water::node::get(node.node.as_ref()))
                .and_then(|node| node.fluid_domain_snapshot())
            {
                output.push((node_id.clone(), snapshot));
            }
        }
    }

    /// Generator convenience for the single effect owned by this runtime.
    pub fn write_fluid_domains_watched(
        &self,
        output: &mut Vec<(
            manifold_core::NodeId,
            crate::water::fluid::FluidDomainSnapshot,
        )>,
    ) {
        if let Some(slot) = self.effect_nodes.first() {
            self.write_fluid_domains(slot.effect_id, output);
        }
    }
}
