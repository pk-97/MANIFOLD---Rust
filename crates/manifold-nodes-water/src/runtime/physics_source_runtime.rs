//! Host-control observation for the installed water runtime.

use manifold_core::Seconds;
use manifold_core::effects::PresetInstance;
use manifold_node_engine::graph::Graph;

impl super::WaterRuntimeState {
    /// Observe authored host controls without cloning per-frame runtime state.
    /// `tempo_shift` is this frame's: the seam where host hop times become
    /// played time.
    pub(super) fn set_source_instance(
        &mut self,
        _graph: &mut Graph,
        instance: Option<&PresetInstance>,
        tempo_shift: Seconds,
    ) {
        if let Some(inputs) = self.input_snapshot.as_mut() {
            inputs.set_hops(instance, tempo_shift);
            inputs.set_automation(instance);
        }
    }
}
