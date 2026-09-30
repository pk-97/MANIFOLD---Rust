//! The executor side of substep regions (`docs/GPU_MPM_SOLVER_DESIGN.md` D7):
//! run the boundary once, then its body as many times as the boundary asks,
//! writing the per-iteration scalars before each run and capturing after it.
//! Every step goes through the one step evaluator the frame pass uses.
//! A boundary that names a clock owner may, offline, have the executor
//! commit, wait for the GPU and run the owner's host step between two
//! iterations; nothing else in a region ever commits or waits.

use super::{Executor, FrameTally, StepEnv, StepFlow, StepPass, resolve_dims};
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::execution_plan::ExecutionPlan;
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::physics::offline_simulation;
use crate::node_graph::state_store::StateStore;
use crate::node_graph::substeps::SubstepRegion;

/// Iterations one region may run in one frame. The boundary owns the count
/// (the MPM rule caps it at 128 substeps per tick); this only stops a
/// boundary that never ends from hanging the content thread, loudly.
pub(crate) const MAX_REGION_ITERATIONS: u32 = 4096;

impl Executor {
    /// Run one contracted region. Region resources are held for the whole
    /// repeat — no body step carries a `free_after` for them — and released
    /// here when it ends.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_substep_region(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        region: &SubstepRegion,
        env: StepEnv<'_>,
        tally: &mut FrameTally,
        gpu: &mut Option<&mut GpuEncoder<'_>>,
        state: &mut Option<&mut StateStore>,
    ) -> StepFlow {
        let boundary_idx = region.steps[0];
        if !self.live_steps[boundary_idx] {
            return StepFlow::Next;
        }
        if self.run_step(graph, plan, boundary_idx, StepPass::Frame, env, tally, gpu, state)
            == StepFlow::Abort
        {
            return StepFlow::Abort;
        }
        let ports = graph
            .get_node(region.boundary)
            .and_then(|inst| inst.node.substep_boundary())
            .expect("a compiled region's boundary declares its ports");
        let boundary_step = &plan.steps()[boundary_idx];
        self.substep_scalar_slots.clear();
        for name in ports.iteration_scalars {
            let slot = boundary_step
                .outputs
                .iter()
                .find(|(port, _)| port == name)
                .and_then(|&(_, resource)| self.backend.slot_for(resource));
            self.substep_scalar_slots.push(slot);
        }
        self.substep_scalar_values.clear();
        self.substep_scalar_values.resize(ports.iteration_scalars.len(), 0.0);

        let mut iteration = 0u32;
        loop {
            let more = graph
                .get_node_mut(region.boundary)
                .expect("boundary exists")
                .node
                .substep_iteration(iteration, &mut self.substep_scalar_values);
            if !more {
                break;
            }
            if iteration == MAX_REGION_ITERATIONS {
                eprintln!(
                    "[graph error] substep boundary {:?} asked for more than \
                     {MAX_REGION_ITERATIONS} iterations this frame; the region stopped there",
                    region.boundary,
                );
                break;
            }
            // Host syncs are opt-in per boundary and offline only: a region
            // without a clock owner, or any live frame, never commits or
            // waits mid-region.
            if let Some(clock) = region.clock
                && iteration > 0
                && offline_simulation()
                && graph.get_node(clock).is_some_and(|inst| inst.node.substep_host_sync(iteration))
            {
                if let Some(gpu) = gpu.as_deref_mut() {
                    gpu.native_enc.commit_wait_and_continue(gpu.device);
                }
                self.substep_host_syncs += 1;
                let owner = graph.get_node_mut(clock).expect("clock owner exists");
                if let Err(error) = owner.node.substep_host_step(iteration, gpu.as_deref_mut()) {
                    eprintln!(
                        "[graph error] node {clock:?} ({}): substep host step before iteration {iteration}: {error}",
                        owner.node.type_id().as_str(),
                    );
                }
            }
            for (slot, &value) in self.substep_scalar_slots.iter().zip(&self.substep_scalar_values) {
                if let Some(slot) = *slot {
                    self.backend.set_scalar(slot, ParamValue::Float(value));
                }
            }
            for &body_idx in &region.steps[1..] {
                if self.run_step(
                    graph,
                    plan,
                    body_idx,
                    StepPass::Iteration(iteration),
                    env,
                    tally,
                    gpu,
                    state,
                ) == StepFlow::Abort
                {
                    return StepFlow::Abort;
                }
            }
            self.capture_step(graph, plan, boundary_idx, env, gpu, state);
            iteration += 1;
        }

        for &resource in &region.held_resources {
            if self.backend.slot_for(resource).is_none()
                || self.dump_pinned_resources.contains(&resource)
                || self.preview_resource == Some(resource)
            {
                continue;
            }
            let ty = plan
                .resource_type(resource)
                .expect("resource type known from compile()");
            let fmt = plan.resource_format(resource);
            let dims = resolve_dims(plan, resource, env.canvas_dims);
            self.backend.release(resource, ty, fmt, dims);
        }
        StepFlow::Next
    }
}
