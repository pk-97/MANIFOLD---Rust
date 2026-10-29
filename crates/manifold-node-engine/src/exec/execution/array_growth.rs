//! Capacity propagation for growing CPU-origin geometry.
use super::*;
use crate::ports::PortType;
use manifold_gpu::BufferUse;

impl Executor {
    pub(super) fn grow_step_arrays(
        &mut self,
        graph: &Graph,
        plan: &ExecutionPlan,
        step: &ExecutionStep,
        gpu: &mut GpuEncoder<'_>,
    ) -> Result<(), String> {
        let device = gpu.device;
        if !step
            .outputs
            .iter()
            .any(|(_, id)| self.growing_arrays.get(id.0 as usize) == Some(&true))
        {
            return Ok(());
        }
        let Some(node) = graph.get_node(step.node) else {
            return Ok(());
        };
        self.array_capacity_scratch.clear();
        for &(port, id) in &step.inputs {
            if let Some(PortType::Array(layout)) = plan.resource_type(id)
                && let Some(slot) = self.backend.slot_for(id)
            {
                // A passthrough switch output that did not pass through this
                // frame (held) has no storage to size from; its readers grow
                // on the frame it is bound again.
                let Some(buffer) = self.backend.array_buffer(slot) else {
                    return Ok(());
                };
                let count = buffer.size / u64::from(layout.item_size);
                let count =
                    u32::try_from(count).map_err(|_| "Array exceeds 32-bit GPU indexing")?;
                self.array_capacity_scratch.push((port, count));
            }
        }
        for &(port, id) in &step.outputs {
            if self.growing_arrays.get(id.0 as usize) != Some(&true) {
                continue;
            }
            let Some(PortType::Array(layout)) = plan.resource_type(id) else {
                continue;
            };
            let Some(slot) = self.backend.slot_for(id) else {
                continue;
            };
            let Some(current) = self.backend.array_buffer(slot) else {
                continue;
            };
            // The source publishes storage after evaluating its CPU snapshot.
            if node.node.provides_array_output(port) {
                continue;
            }
            let Some(count) =
                node.node
                    .array_output_capacity(port, &node.params, &self.array_capacity_scratch)
            else {
                return Err(format!(
                    "{}.{port} has no array capacity declaration",
                    node.node.type_id().as_str()
                ));
            };
            let bytes = u64::from(count) * u64::from(layout.item_size);
            if bytes <= current.size {
                continue;
            }
            crate::load::expand::admit_candidate_bytes(device.modifier_memory_snapshot(), bytes)
                .map_err(|error| error.to_string())?;
            let buffer = device.try_create_buffer(bytes, BufferUse::Upload)?;
            // Zeroed on the GPU in encoder order: a CPU memset of a large
            // array stalls the content thread mid-show.
            gpu.native_enc.clear_buffer(&buffer);
            if !self.backend.install_array_buffer(slot, buffer) {
                return Err("Backend cannot replace array storage".into());
            }
        }
        Ok(())
    }
}
