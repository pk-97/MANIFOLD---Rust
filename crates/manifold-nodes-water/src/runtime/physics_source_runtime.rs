//! Host-control observation for the installed water runtime.

use manifold_node_engine::graph::Graph;
use manifold_core::effects::PresetInstance;

impl super::WaterRuntimeState {
    /// Observe authored host controls without cloning per-frame runtime state.
    pub(super) fn set_source_instance(&mut self, _graph: &mut Graph, instance: Option<&PresetInstance>) {
        if let Some(inputs) = self.input_snapshot.as_mut() { inputs.set_hops(instance); }
    }
}
