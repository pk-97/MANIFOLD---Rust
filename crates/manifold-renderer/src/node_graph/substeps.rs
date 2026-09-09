//! Bounded substep scheduling — types for the graph executor's repeat
//! regions. Contract: `docs/WATER_SIMULATION_DESIGN.md` section 4.
//!
//! A *substep boundary* node (e.g. `node.water_state`) declares its port
//! names via [`SubstepBoundaryPorts`]. At plan compile time the nodes between
//! its state outputs and its capture producers are contracted into a
//! [`SubstepRegion`]; the executor runs the boundary once, then repeats the
//! region body `step_count` times, capturing the candidate state into the
//! persistent accepted state after each iteration. Everything outside the
//! region — cameras, scene draws, post — runs once per output frame.

use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::execution_plan::ResourceId;

/// Port names a substep boundary node declares to the plan compiler and
/// executor. `seed` is the one-shot init source; `capture`/`state` are the
/// primary back-edge pair (candidate in, accepted out); `count`/`delta`/
/// `time`/`index` are the per-iteration scalar outputs the boundary sets
/// before each repeated body run. `results` names additional typed
/// back-edge pairs (collider transform, sticky status) whose final values
/// escape the region alongside `state`.
#[derive(Clone, Copy, Debug)]
pub struct SubstepBoundaryPorts {
    pub seed: &'static str,
    pub capture: &'static str,
    pub state: &'static str,
    pub count: &'static str,
    pub delta: &'static str,
    pub time: &'static str,
    pub index: &'static str,
    pub results: &'static [SubstepResultPorts],
}

/// One typed capture→output back-edge pair beyond the primary state buffer.
#[derive(Clone, Copy, Debug)]
pub struct SubstepResultPorts {
    pub capture: &'static str,
    pub output: &'static str,
}

/// One contracted repeat region in an [`crate::node_graph::execution_plan::ExecutionPlan`].
/// `steps` are indices into `ExecutionPlan.steps` (boundary first, then the
/// body in topological order), fixed at compile time. `held_resources` are
/// the region-internal wires: they are excluded from per-step `free_after`
/// for the whole repeat, so the pool never recycles a slot between
/// iterations.
#[derive(Debug, Clone)]
pub struct SubstepRegion {
    pub boundary: NodeInstanceId,
    pub steps: Vec<usize>,
    pub held_resources: Vec<ResourceId>,
}

/// The host-supplied simulation clock for one output frame. The boundary
/// accumulates `delta * time_scale` in f64 Seconds and consumes integer
/// ticks of its configured `step_hz`. `epoch` changes on seek / project
/// replacement and resets seed, clock and event latches; a duplicate
/// `frame_id` must not advance the clock twice. `advancing` false means
/// render current state with zero steps; `exporting` upgrades an
/// overloaded clock from dropped-ticks to a hard error.
#[derive(Clone, Copy, Debug)]
pub struct SimulationFrame {
    pub frame_id: u64,
    pub delta: manifold_core::Seconds,
    pub epoch: u64,
    pub advancing: bool,
    pub exporting: bool,
}
