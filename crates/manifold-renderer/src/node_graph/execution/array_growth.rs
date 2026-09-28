//! Capacity propagation for growing CPU-origin geometry.
use super::*;
use crate::node_graph::ports::PortType;

impl Executor {
    pub(super) fn grow_step_arrays(
        &mut self,
        graph: &Graph,
        plan: &ExecutionPlan,
        step: &ExecutionStep,
        device: &manifold_gpu::GpuDevice,
    ) -> Result<(), String> {
        if !step.outputs.iter().any(|(_, id)| self.growing_arrays.get(id.0 as usize) == Some(&true)) {
            return Ok(());
        }
        let Some(node) = graph.get_node(step.node) else { return Ok(()); };
        self.array_capacity_scratch.clear();
        for &(port, id) in &step.inputs {
            if let Some(PortType::Array(layout)) = plan.resource_type(id)
                && let Some(slot) = self.backend.slot_for(id)
                && let Some(buffer) = self.backend.array_buffer(slot) {
                let count = buffer.size / u64::from(layout.item_size);
                let count = u32::try_from(count).map_err(|_| "Array exceeds 32-bit GPU indexing")?;
                self.array_capacity_scratch.push((port, count));
            }
        }
        for &(port, id) in &step.outputs {
            if self.growing_arrays.get(id.0 as usize) != Some(&true) { continue; }
            let Some(PortType::Array(layout)) = plan.resource_type(id) else { continue; };
            let Some(slot) = self.backend.slot_for(id) else { continue; };
            let Some(current) = self.backend.array_buffer(slot) else { continue; };
            // The source publishes storage after evaluating its CPU snapshot.
            if node.node.provides_array_output(port) { continue; }
            let Some(count) = node.node.array_output_capacity(port, &node.params, &self.array_capacity_scratch) else {
                return Err(format!("{}.{port} has no array capacity declaration", node.node.type_id().as_str()));
            };
            let bytes = u64::from(count) * u64::from(layout.item_size);
            if bytes <= current.size { continue; }
            super::super::scene_modifier_expand::admit_candidate_bytes(device.modifier_memory_snapshot(), bytes)
                .map_err(|error| error.to_string())?;
            let buffer = device.try_create_buffer_shared(bytes)?;
            buffer.zero_fill();
            if !self.backend.install_array_buffer(slot, buffer) {
                return Err("Backend cannot replace array storage".into());
            }
        }
        Ok(())
    }
}
