//! In-place rewrite of the trigger lanes' target chips.

use super::*;

impl LayerHeaderPanel {
    /// Rewrite each trigger lane's target chip in place. `labels` walks the
    /// layers in this panel's order as `(layer id, label)`; a row whose id
    /// differs waits for the rebuild, which reads the snapshot written here.
    pub fn sync_trigger_labels(
        &mut self,
        tree: &mut UITree,
        labels: &mut dyn Iterator<Item = (&str, String)>,
    ) {
        for ((layer, row), (id, label)) in self.layers.iter_mut().zip(self.rows.iter()).zip(labels)
        {
            if layer.layer_id != id || layer.trigger_targets_label == label {
                continue;
            }
            if let Some(node) = row.id(LayerControl::TriggerTargets) {
                tree.set_text(node, &label);
            }
            layer.trigger_targets_label = label;
        }
    }
}
