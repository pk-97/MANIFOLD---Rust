//! Water-owned history and impulse state for one installed runtime graph.

use std::sync::Arc;

use crate::exec::{effect_node::FrameTime, execution_plan::ExecutionPlan};
use crate::graph::Graph;
use crate::runtime::preset_context::ProjectTempo;

use super::physics_sampling::{PhysicsInputSnapshot, physics_sample_steps};
use super::scene_impulses::SceneImpulses;

pub(crate) struct WaterRuntimeState {
    /// One source state per immutable effect slot, in construction order.
    #[cfg(feature = "gpu-proofs")]
    pub(crate) sources: Vec<super::physics_source_state::PhysicsSourceState>,
    pub(crate) impulse_identity: Arc<()>,
    pub(crate) scene_impulses: SceneImpulses,
    pub(crate) sample_steps: Option<Vec<bool>>,
    pub(crate) input_snapshot: Option<PhysicsInputSnapshot>,
    pub(crate) last_frame_time: Option<FrameTime>,
    pub(crate) project_tempo: Option<ProjectTempo>,
}

impl WaterRuntimeState {
    pub(crate) fn new(
        graph: &Graph,
        plan: &ExecutionPlan,
        #[cfg(feature = "gpu-proofs")] slot_count: usize,
    ) -> Result<Self, String> {
        let sample_steps = physics_sample_steps(graph, plan)?;
        let input_snapshot = sample_steps
            .as_ref()
            .map(|steps| PhysicsInputSnapshot::prepare(graph, plan, steps));
        Ok(Self {
            #[cfg(feature = "gpu-proofs")]
            sources: (0..slot_count).map(|_| Default::default()).collect(),
            impulse_identity: Arc::new(()),
            scene_impulses: SceneImpulses::default(),
            sample_steps,
            input_snapshot,
            last_frame_time: None,
            project_tempo: None,
        })
    }

    pub(crate) fn after_frame(&mut self, graph: &Graph, time: FrameTime) {
        self.last_frame_time = Some(time);
        self.observe_impulse_setup(graph);
    }

    pub(crate) fn reset(&mut self) {
        self.impulse_identity = Arc::new(());
        self.reset_modifier_impulses();
        self.last_frame_time = None;
    }
}
