//! Capture both participants before their single shared worker step.

use super::*;
use crate::node_graph::physics_scene::CoupledSceneSteps;

impl Executor {
    pub(super) fn capture_coupled_scene(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        pair: CoupledSceneSteps,
        time: FrameTime,
        sample: Option<PhysicsSample<'_>>,
    ) {
        let rigid_step = &plan.steps()[pair.rigid_step];
        self.input_scratch.clear();
        let mut complete = true;
        for &(port, resource) in &rigid_step.inputs {
            if let Some(slot) = self.backend.slot_for(resource) {
                self.input_scratch.push((port, slot));
            } else {
                // Historical passes may precede setup completion. An absent
                // resource is pending, never an unwired body with defaults.
                complete = false;
            }
        }
        let (fluid, rigid) = graph
            .node_pair_mut(plan.steps()[pair.fluid_step].node, rigid_step.node)
            .expect("compiled coupled participants exist");
        if !complete {
            fluid
                .node
                .set_coupled_rigid_inputs(None, pair.colliders, None);
            rigid.node.accept_coupled_rigid_frame(None);
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
                sample.params[pair.rigid_step]
                    .as_ref()
                    .expect("coupled rigid sample has retained parameters")
            })
            .unwrap_or(&rigid.params);
        let mut ctx = EffectNodeContext::new(time, params, inputs, outputs, None);
        let result = rigid.node.capture_coupled_rigid(&mut ctx);
        fluid.node.set_coupled_rigid_inputs(
            rigid.node.rigid_scene_observation(),
            pair.colliders,
            result.as_ref().err().map(String::as_str),
        );
    }
}
