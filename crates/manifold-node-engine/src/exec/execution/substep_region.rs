//! The executor side of substep regions:
//! run the boundary once, then its body as many times as the boundary asks,
//! writing the per-iteration scalars before each run and capturing after it.
//! Every step goes through the one step evaluator the frame pass uses.
//! A boundary that names a clock owner may have the executor
//! commit, wait for the GPU and run the owner's host step between two
//! iterations; nothing else in a region ever commits or waits. After that
//! host step the owner's outputs are published again and the boundary may
//! refresh its scalars, so the next iteration reads the finished one.
//!
//! A region the plan marks `overlaps` runs its iterations on the encoder's
//! async lane, and its tick publishers follow it there. The main lane carries
//! on with the rest of the graph and waits for the async lane once, when the
//! graph pass ends. A frame whose region runs no iteration never opens the
//! lane, and an encoder that refuses it (a profiled frame) runs in order.

use super::{Executor, FrameTally, StepEnv, StepFlow, StepPass, resolve_dims};
use crate::exec::effect_node::NodeInstanceId;
use crate::exec::execution_plan::ExecutionPlan;
use crate::exec::substeps::{SubstepClockOutput, SubstepRegion};
use crate::gpu::gpu_encoder::GpuEncoder;
use crate::graph::Graph;
use crate::parameters::ParamValue;
use crate::state_store::StateStore;

/// Iterations one region may run in one frame. The boundary owns the count
/// (the MPM rule caps it at 128 substeps per tick); this only stops a
/// boundary that never ends from hanging the content thread, loudly.
pub(crate) const MAX_REGION_ITERATIONS: u32 = 4096;

/// One running region's iteration and per-iteration scalars: the boundary's
/// scalar output slots and the values it serves the next iteration. Kept on
/// the executor and reused, one per member of the widest group.
#[derive(Default)]
pub(super) struct RegionRun {
    /// The next iteration, or `None` once the region ran its last.
    iteration: Option<u32>,
    slots: Vec<Option<crate::bindings::Slot>>,
    values: Vec<f32>,
}

/// What a region's boundary decided on its frame run.
enum Opened {
    /// The body iterates.
    Iterates,
    /// Nothing to iterate: a skipped, restarting or pending boundary.
    Ran,
    Abort,
}

fn too_many_iterations(region: &SubstepRegion) {
    eprintln!(
        "[graph error] substep boundary {:?} asked for more than \
         {MAX_REGION_ITERATIONS} iterations this frame; the region stopped there",
        region.boundary,
    );
}

/// One graph pass's use of the encoder's async lane by overlapping regions.
/// Reset at the start of every pass; its vector is reused, never reallocated
/// once it has the plan's region count.
#[derive(Default)]
pub(super) struct TickLane {
    /// A region ticked on the async lane and the lane is not finished yet.
    open: bool,
    /// The encoder is encoding on the async lane right now.
    on_async: bool,
    /// A lane finished this pass, so the main lane still owes it a wait.
    finished: bool,
    /// The lane finishes once this step has run.
    finish_after: usize,
    /// Per region of the plan: it ticked on the async lane this pass.
    ticked: Vec<bool>,
}

impl TickLane {
    pub(super) fn begin_pass(&mut self, regions: usize) {
        self.open = false;
        self.on_async = false;
        self.finished = false;
        self.finish_after = 0;
        self.ticked.clear();
        self.ticked.resize(regions, false);
    }

    /// Region `region` began ticking on the async lane; the lane must stay
    /// open through step `finish_after`.
    fn opened(&mut self, region: usize, finish_after: usize) {
        self.ticked[region] = true;
        self.finish_after = if self.open {
            self.finish_after.max(finish_after)
        } else {
            finish_after
        };
        self.open = true;
        self.on_async = true;
    }
}

impl Executor {
    /// Run one contracted region. Region resources are held for the whole
    /// repeat — no body step carries a `free_after` for them — and released
    /// here when it ends.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_substep_region(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        region_idx: usize,
        region: &SubstepRegion,
        env: StepEnv<'_>,
        tally: &mut FrameTally,
        gpu: &mut Option<&mut GpuEncoder<'_>>,
        state: &mut Option<&mut StateStore>,
    ) -> StepFlow {
        match self.open_region(graph, plan, region, env, tally, gpu, state) {
            Opened::Iterates => {}
            Opened::Ran => return StepFlow::Next,
            Opened::Abort => return StepFlow::Abort,
        }
        let mut runs = std::mem::take(&mut self.region_runs);
        if runs.is_empty() {
            runs.push(RegionRun::default());
        }
        self.bind_iteration_scalars(graph, plan, region, &mut runs[0]);

        // The body replays its recorded dispatches. A dump copies arrays on
        // the main lane mid-frame, so a dumping frame runs in order.
        let replay = self.encode_replay && !self.dumps_arrays();
        let overlaps = region.overlaps && !self.dumps_arrays();
        let mut replaying = false;
        let flow = 'iterations: {
            let mut iteration = 0u32;
            loop {
                if !self.begin_iteration(graph, plan, region, iteration, &mut runs[0]) {
                    break StepFlow::Next;
                }
                if iteration == 0 {
                    // The first tick moves the region onto the async lane,
                    // then the replay span opens on it.
                    if overlaps && let Some(gpu) = gpu.as_deref_mut() {
                        let event = self
                            .tick_event
                            .get_or_insert_with(|| gpu.device.create_event());
                        if gpu.native_enc.use_async_lane(gpu.device, event) {
                            let region_end = region.steps[region.steps.len() - 1];
                            let finish_after = region
                                .publishers
                                .last()
                                .map_or(region_end, |&last| last.max(region_end));
                            self.lane.opened(region_idx, finish_after);
                        }
                    }
                    if replay && let Some(gpu) = gpu.as_deref_mut() {
                        let cache =
                            std::mem::take(self.replay_caches.entry(region.boundary).or_default());
                        gpu.native_enc.begin_replay(gpu.device, cache);
                        replaying = true;
                    }
                }
                if iteration == MAX_REGION_ITERATIONS {
                    too_many_iterations(region);
                    break StepFlow::Next;
                }
                let run = &mut runs[0];
                if self.finish_iteration(graph, plan, region, iteration, run, env, tally, gpu, state)
                    == StepFlow::Abort
                {
                    break 'iterations StepFlow::Abort;
                }
                iteration += 1;
            }
        };
        self.region_runs = runs;
        if replaying && let Some(gpu) = gpu.as_deref_mut() {
            let cache = gpu.native_enc.end_replay();
            *self
                .replay_caches
                .get_mut(&region.boundary)
                .expect("taken above") = cache;
        }
        if flow == StepFlow::Abort {
            return StepFlow::Abort;
        }
        self.release_region_held(plan, region, env);
        StepFlow::Next
    }

    /// Run a co-iterated group: open every member, then step the World tick
    /// by tick, each tick running every member's iterations of that tick in
    /// group order (a member reads the ones before it within the tick), then
    /// release every member's held resources together. Which tick an
    /// iteration belongs to is its clock owner's interval ordinal. The whole
    /// interleaved run is one replay span, keyed by the first member's
    /// boundary; the async lane stays off.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn run_region_group(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        group: usize,
        env: StepEnv<'_>,
        tally: &mut FrameTally,
        gpu: &mut Option<&mut GpuEncoder<'_>>,
        state: &mut Option<&mut StateStore>,
    ) -> StepFlow {
        let members = &plan.region_groups()[group];
        let mut runs = std::mem::take(&mut self.region_runs);
        if runs.len() < members.len() {
            runs.resize_with(members.len(), RegionRun::default);
        }
        let flow = self.step_region_group(graph, plan, members, &mut runs, env, tally, gpu, state);
        self.region_runs = runs;
        if flow == StepFlow::Abort {
            return StepFlow::Abort;
        }
        for &member in members {
            self.release_region_held(plan, &plan.substep_regions()[member], env);
        }
        StepFlow::Next
    }

    #[allow(clippy::too_many_arguments)]
    fn step_region_group(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        members: &[usize],
        runs: &mut [RegionRun],
        env: StepEnv<'_>,
        tally: &mut FrameTally,
        gpu: &mut Option<&mut GpuEncoder<'_>>,
        state: &mut Option<&mut StateStore>,
    ) -> StepFlow {
        for (run, &member) in runs.iter_mut().zip(members) {
            let region = &plan.substep_regions()[member];
            run.iteration = match self.open_region(graph, plan, region, env, tally, gpu, state) {
                Opened::Iterates => {
                    self.bind_iteration_scalars(graph, plan, region, run);
                    Some(0)
                }
                Opened::Ran => None,
                Opened::Abort => return StepFlow::Abort,
            };
        }
        let runs = &mut runs[..members.len()];
        // A dump copies arrays on the main lane mid-frame, so a dumping frame
        // runs in order.
        let key = plan.substep_regions()[members[0]].boundary;
        let replaying = self.encode_replay
            && !self.dumps_arrays()
            && runs.iter().any(|run| run.iteration.is_some())
            && gpu.as_deref_mut().is_some_and(|gpu| {
                let cache = std::mem::take(self.replay_caches.entry(key).or_default());
                gpu.native_enc.begin_replay(gpu.device, cache);
                true
            });
        let mut flow = StepFlow::Next;
        let mut tick = 0u32;
        'ticks: while runs.iter().any(|run| run.iteration.is_some()) {
            let mut next_tick = u32::MAX;
            for (run, &member) in runs.iter_mut().zip(members) {
                let region = &plan.substep_regions()[member];
                while let Some(iteration) = run.iteration {
                    let ordinal = region
                        .clock
                        .and_then(|clock| graph.get_node(clock))
                        .and_then(|owner| owner.node.substep_clock_interval(iteration))
                        .map(|output| output.timing.ordinal);
                    if let Some(ordinal) = ordinal
                        && ordinal > tick
                    {
                        next_tick = next_tick.min(ordinal);
                        break;
                    }
                    if !self.begin_iteration(graph, plan, region, iteration, run) {
                        run.iteration = None;
                        break;
                    }
                    if iteration == MAX_REGION_ITERATIONS {
                        too_many_iterations(region);
                        run.iteration = None;
                        break;
                    }
                    if self.finish_iteration(graph, plan, region, iteration, run, env, tally, gpu, state)
                        == StepFlow::Abort
                    {
                        flow = StepFlow::Abort;
                        break 'ticks;
                    }
                    run.iteration = Some(iteration + 1);
                }
            }
            tick = next_tick;
        }
        if replaying && let Some(gpu) = gpu.as_deref_mut() {
            let cache = gpu.native_enc.end_replay();
            *self.replay_caches.get_mut(&key).expect("taken above") = cache;
        }
        flow
    }

    /// The boundary's frame run and what it decides: a restart or a pending
    /// boundary runs no iteration (its held resources are released here),
    /// otherwise the body iterates.
    #[allow(clippy::too_many_arguments)]
    fn open_region(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        region: &SubstepRegion,
        env: StepEnv<'_>,
        tally: &mut FrameTally,
        gpu: &mut Option<&mut GpuEncoder<'_>>,
        state: &mut Option<&mut StateStore>,
    ) -> Opened {
        let boundary_idx = region.steps[0];
        if !self.live_steps[boundary_idx] {
            return Opened::Ran;
        }
        if self.run_step(
            graph,
            plan,
            boundary_idx,
            StepPass::Frame,
            env,
            tally,
            gpu,
            state,
        ) == StepFlow::Abort
        {
            return Opened::Abort;
        }
        let restart = graph
            .get_node_mut(region.boundary)
            .is_some_and(|boundary| boundary.node.take_substep_restart_request());
        if restart {
            if let Some(clock) = region.clock.and_then(|id| graph.get_node_mut(id)) {
                // The next clock visit produces a fresh domain epoch, including
                // a coupled rigid owner. No ID is reused in the old epoch.
                clock.node.clear_state();
            } else {
                eprintln!(
                    "[graph error] substep boundary {:?} requested an identity restart without a clock owner",
                    region.boundary
                );
            }
            self.release_region_held(plan, region, env);
            return Opened::Ran;
        }
        if self.pending_skipped[boundary_idx] {
            // A pending boundary has no state to iterate: the body visits
            // once, so each step publishes its outputs pending.
            for &step_idx in &region.steps[1..] {
                let pass = StepPass::Repeat { first: true };
                if self.run_step(graph, plan, step_idx, pass, env, tally, gpu, state)
                    == StepFlow::Abort
                {
                    return Opened::Abort;
                }
            }
            self.release_region_held(plan, region, env);
            return Opened::Ran;
        }
        Opened::Iterates
    }

    /// Find the slots of the boundary's per-iteration scalar outputs.
    fn bind_iteration_scalars(
        &mut self,
        graph: &Graph,
        plan: &ExecutionPlan,
        region: &SubstepRegion,
        run: &mut RegionRun,
    ) {
        let ports = graph
            .get_node(region.boundary)
            .and_then(|inst| inst.node.substep_boundary())
            .expect("a compiled region's boundary declares its ports");
        let boundary_step = &plan.steps()[region.steps[0]];
        run.slots.clear();
        for name in ports.iteration_scalars {
            let slot = boundary_step
                .outputs
                .iter()
                .find(|(port, _)| port == name)
                .and_then(|&(_, resource)| self.backend.slot_for(resource));
            run.slots.push(slot);
        }
        run.values.clear();
        run.values.resize(ports.iteration_scalars.len(), 0.0);
    }

    /// Publish the clock owner's interval for `iteration` and ask the
    /// boundary whether it runs; false ends the region for this frame.
    fn begin_iteration(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        region: &SubstepRegion,
        iteration: u32,
        run: &mut RegionRun,
    ) -> bool {
        if let Some(clock) = region.clock
            && let Some(output) = graph
                .get_node(clock)
                .and_then(|owner| owner.node.substep_clock_interval(iteration))
        {
            let timing = output.timing;
            self.publish_clock_output(plan, clock, &output);
            graph
                .get_node_mut(region.boundary)
                .expect("boundary exists")
                .node
                .set_substep_interval(timing);
        }
        graph
            .get_node_mut(region.boundary)
            .expect("boundary exists")
            .node
            .substep_iteration(iteration, &mut run.values)
    }

    /// One iteration's work after the boundary agreed to it: the clock
    /// owner's host step when it asks for one, the scalars, the body, and the
    /// capture.
    #[allow(clippy::too_many_arguments)]
    fn finish_iteration(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        region: &SubstepRegion,
        iteration: u32,
        run: &mut RegionRun,
        env: StepEnv<'_>,
        tally: &mut FrameTally,
        gpu: &mut Option<&mut GpuEncoder<'_>>,
        state: &mut Option<&mut StateStore>,
    ) -> StepFlow {
        // Only opted-in clock owners may wait. Coupled liquids ask
        // once between ticks, bounded live by MAX_LIVE_TICKS - 1.
        if let Some(clock) = region.clock
            && iteration > 0
            && graph
                .get_node(clock)
                .is_some_and(|inst| inst.node.substep_host_sync(iteration))
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
                return StepFlow::Abort;
            }
            // The finished iteration refreshes what the next one reads: the
            // clock owner's outputs, then the boundary's scalars.
            if let Some(output) = graph
                .get_node(clock)
                .and_then(|owner| owner.node.substep_clock_interval(iteration))
            {
                self.publish_clock_output(plan, clock, &output);
            }
            graph
                .get_node_mut(region.boundary)
                .expect("boundary exists")
                .node
                .substep_host_synced(iteration, &mut run.values);
        }
        for (slot, &value) in run.slots.iter().zip(&run.values) {
            if let Some(slot) = *slot {
                self.backend.set_scalar(slot, ParamValue::Float(value));
            }
        }
        let body_pass = StepPass::Repeat {
            first: iteration == 0,
        };
        for &step_idx in &region.steps[1..] {
            if self.run_step(graph, plan, step_idx, body_pass, env, tally, gpu, state)
                == StepFlow::Abort
            {
                return StepFlow::Abort;
            }
        }
        self.capture_step(graph, plan, region.steps[0], env, gpu, state);
        StepFlow::Next
    }

    /// Pick the lane the next non-region step encodes on. A publisher of a
    /// region that ticked this pass follows the tick on the async lane; every
    /// other step runs on the main lane. Costs nothing while no lane is open.
    pub(super) fn route_tick_lane(
        &mut self,
        plan: &ExecutionPlan,
        idx: usize,
        gpu: &mut Option<&mut GpuEncoder<'_>>,
    ) {
        if !self.lane.open {
            return;
        }
        let publishes = self.live_steps[idx]
            && plan
                .substep_regions()
                .iter()
                .zip(&self.lane.ticked)
                .any(|(region, &ticked)| ticked && region.publishers.contains(&idx));
        if publishes {
            self.enter_tick_lane(gpu);
        } else {
            self.leave_tick_lane(gpu);
        }
    }

    /// Encode on the async lane, which a region already opened.
    fn enter_tick_lane(&mut self, gpu: &mut Option<&mut GpuEncoder<'_>>) {
        if self.lane.on_async {
            return;
        }
        if let Some(gpu) = gpu.as_deref_mut() {
            let event = self
                .tick_event
                .as_ref()
                .expect("an open lane has its event");
            let entered = gpu.native_enc.use_async_lane(gpu.device, event);
            debug_assert!(entered, "an open lane stays available to its publishers");
            self.lane.on_async = entered;
        }
    }

    /// Encode on the main lane again; the async lane stays open.
    pub(super) fn leave_tick_lane(&mut self, gpu: &mut Option<&mut GpuEncoder<'_>>) {
        if self.lane.on_async
            && let Some(gpu) = gpu.as_deref_mut()
        {
            gpu.native_enc.use_main_lane();
            self.lane.on_async = false;
        }
    }

    /// Finish the async lane once step `last_step` has run, if that was the
    /// last step it carries.
    pub(super) fn finish_tick_lane_after(
        &mut self,
        last_step: usize,
        gpu: &mut Option<&mut GpuEncoder<'_>>,
    ) {
        if self.lane.open && last_step >= self.lane.finish_after {
            self.finish_tick_lane(gpu);
        }
    }

    /// Commit the async lane and hand control back to the main lane, which
    /// does not wait for it yet.
    fn finish_tick_lane(&mut self, gpu: &mut Option<&mut GpuEncoder<'_>>) {
        if !self.lane.open {
            return;
        }
        if let Some(gpu) = gpu.as_deref_mut() {
            let event = self
                .tick_event
                .as_ref()
                .expect("an open lane has its event");
            gpu.native_enc.finish_async_lane(event);
        }
        self.lane.open = false;
        self.lane.on_async = false;
        self.lane.finished = true;
    }

    /// End of the graph pass, normal or aborted: finish a lane still open and
    /// make the main lane wait for the async lane's last value. Everything
    /// the pass's caller encodes afterwards runs after the tick.
    pub(super) fn end_tick_lane(&mut self, gpu: &mut Option<&mut GpuEncoder<'_>>) {
        self.finish_tick_lane(gpu);
        if self.lane.finished
            && let Some(gpu) = gpu.as_deref_mut()
        {
            let event = self
                .tick_event
                .as_ref()
                .expect("a finished lane has its event");
            gpu.native_enc.wait_async_lane(event);
            self.lane.finished = false;
        }
    }

    /// Write a clock owner's per-iteration outputs: the interval duration,
    /// then its extra scalars, each into the clock step's output slot.
    fn publish_clock_output(
        &mut self,
        plan: &ExecutionPlan,
        clock: NodeInstanceId,
        output: &SubstepClockOutput<'_>,
    ) {
        let clock_step = plan.steps().iter().find(|step| step.node == clock);
        let duration = (output.duration_port, output.timing.duration().0 as f32);
        for &(port, value) in std::iter::once(&duration).chain(output.scalars) {
            if let Some(slot) = clock_step
                .and_then(|step| step.outputs.iter().find(|(name, _)| *name == port))
                .and_then(|(_, resource)| self.backend.slot_for(*resource))
            {
                self.backend.set_scalar(slot, ParamValue::Float(value));
            }
        }
    }

    fn release_region_held(
        &mut self,
        plan: &ExecutionPlan,
        region: &SubstepRegion,
        env: StepEnv<'_>,
    ) {
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
    }
}
