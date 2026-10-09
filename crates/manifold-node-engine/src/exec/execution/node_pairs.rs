//! Prepare the second participant before the first step of an ordered pair.

use super::*;
use crate::exec::node_pairs::NodePairSteps;

impl Executor {
    pub(super) fn prepare_node_pair(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        pair: NodePairSteps,
        time: FrameTime,
        sample: Option<CpuSample<'_>>,
    ) {
        let second_step = &plan.steps()[pair.second_step];
        self.input_scratch.clear();
        let mut complete = true;
        for &(port, resource) in &second_step.inputs {
            if let Some(slot) = self.backend.slot_for(resource) {
                self.input_scratch.push((port, slot));
            } else {
                // Historical passes may precede setup completion. An absent
                // resource is pending, never an unwired body with defaults.
                complete = false;
            }
        }
        let (behavior, first, second) = graph
            .pair_nodes_mut(pair.pair_index)
            .expect("compiled pair participants exist");
        if !complete {
            behavior.before_first(first.node.as_mut(), second.node.as_mut(), None);
            return;
        }

        self.scalar_write_scratch.clear();
        self.camera_write_scratch.clear();
        self.light_write_scratch.clear();
        self.material_write_scratch.clear();
        self.transform_write_scratch.clear();
        self.atmosphere_write_scratch.clear();
        self.render_mode_write_scratch.clear();
        self.object_write_scratch.clear();
        let backend: &dyn Backend = &*self.backend;
        let inputs = NodeInputs::new(&self.input_scratch, backend, &self.slot_generations)
            .with_pending(&self.slot_pending)
            .with_mesh_revisions(&self.slot_mesh_revisions)
            .with_content_versions(&self.slot_content_versions);
        // Input capture has no writable graph outputs, GPU or mutable state
        // store. Ordinary execution still owns all resource commits and frees.
        let outputs = NodeOutputs::new(
            &[],
            backend,
            &mut self.scalar_write_scratch,
            &mut self.camera_write_scratch,
            &mut self.light_write_scratch,
            &mut self.material_write_scratch,
            &mut self.transform_write_scratch,
            &mut self.atmosphere_write_scratch,
            &mut self.render_mode_write_scratch,
            &mut self.object_write_scratch,
        );
        let params = sample
            .map(|sample| {
                sample.params[pair.second_step]
                    .as_ref()
                    .expect("paired second step has retained parameters")
            })
            .unwrap_or(&second.params);
        let mut ctx = EffectNodeContext::new(time, params, inputs, outputs, None);
        behavior.before_first(first.node.as_mut(), second.node.as_mut(), Some(&mut ctx));
    }
}
