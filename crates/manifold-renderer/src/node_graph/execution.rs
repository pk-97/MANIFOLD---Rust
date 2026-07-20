//! Per-frame graph execution.
//!
//! The [`Executor`] takes a [`Graph`] plus a precompiled [`ExecutionPlan`]
//! and runs one frame, delegating physical resource allocation to a
//! [`Backend`].
//!
//! ## Mock vs real GPU
//!
//! [`execute_frame`](Executor::execute_frame) runs without a `GpuEncoder` —
//! suitable for [`MockBackend`] tests that exercise resource lifetime
//! logic without touching Metal. [`execute_frame_with_gpu`](Executor::execute_frame_with_gpu)
//! threads a real encoder through to nodes that issue compute / render
//! passes, and is the production entry point alongside [`MetalBackend`].
//!
//! [`MetalBackend`]: crate::node_graph::MetalBackend

use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::backend::{Backend, MockBackend};
use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
use crate::node_graph::effect_node::{EffectNodeContext, FrameTime, NodeInstanceId};
use crate::node_graph::execution_plan::{ExecutionPlan, ExecutionStep, ResourceId};
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::state_store::{OwnerKey, StateStore};

/// Resolve a resource's slot dims for `Backend::acquire` / `release`.
///
/// Resolution order (matches the planner's compile-time decision):
///   1. `plan.resource_dims(res_id)` — concrete `(w, h)` resolved at
///      compile time from a known input chain.
///   2. `plan.resource_canvas_scale(res_id)` — a canvas-relative
///      `(num, den)` hint declared by the producer's
///      `EffectNode::output_canvas_scale`. Resolved here to
///      `(canvas_w * num / den, canvas_h * num / den)`, with `max(1)`
///      so a too-small canvas can't produce a zero-sized allocation.
///   3. Full canvas fallback.
///
/// Used by every site that allocates / releases a slot so the
/// resolution policy lives in one place — the `acquire` / `release`
/// pair MUST agree on dims (the backend's pool keys on dims), so a
/// single helper here prevents the two sites from drifting.
pub(crate) fn resolve_dims(
    plan: &ExecutionPlan,
    res_id: ResourceId,
    canvas_dims: (u32, u32),
) -> (u32, u32) {
    if let Some(dims) = plan.resource_dims(res_id) {
        return dims;
    }
    if let Some((num, den)) = plan.resource_canvas_scale(res_id)
        && den != 0
    {
        let w = (canvas_dims.0 as u64 * num as u64 / den as u64).max(1) as u32;
        let h = (canvas_dims.1 as u64 * num as u64 / den as u64).max(1) as u32;
        return (w, h);
    }
    canvas_dims
}

/// Runs a graph against a precompiled plan, one frame per call.
///
/// The executor owns its [`Backend`] across frames so the high-water mark
/// stabilises after the first frame: slots allocated for frame 0's peak
/// intermediates are reused for every subsequent frame at the same graph
/// topology.
pub struct Executor {
    backend: Box<dyn Backend>,
    /// Scratch buffer reused across steps to avoid per-step allocation.
    /// (Per-frame allocation in tight loops is forbidden by CLAUDE.md.)
    input_scratch: Vec<(&'static str, Slot)>,
    output_scratch: Vec<(&'static str, Slot)>,
    /// Per-step scratch the executor hands to [`NodeOutputs`] so control-rate
    /// nodes can queue scalar writes. Drained back into the backend after
    /// each node's `evaluate` returns.
    scalar_write_scratch: Vec<(Slot, ParamValue)>,
    /// Sibling scratch for [`PortType::Camera`] writes — same drain pattern.
    camera_write_scratch: Vec<(Slot, crate::node_graph::camera::Camera)>,
    /// Sibling scratch for [`PortType::Light`] writes — same drain pattern.
    light_write_scratch: Vec<(Slot, crate::node_graph::light::Light)>,
    /// Sibling scratch for [`PortType::Material`] writes — same drain pattern.
    material_write_scratch: Vec<(Slot, crate::node_graph::material::Material)>,
    /// Sibling scratch for [`PortType::Transform`] writes — same drain pattern.
    transform_write_scratch: Vec<(Slot, crate::node_graph::transform::Transform)>,
    /// Sibling scratch for [`PortType::Atmosphere`] writes — same drain pattern.
    atmosphere_write_scratch: Vec<(Slot, crate::node_graph::atmosphere::Atmosphere)>,
    /// Sibling scratch for [`PortType::Object`] writes — same drain pattern.
    object_write_scratch: Vec<(Slot, crate::node_graph::scene_object::SceneObject)>,
    /// Per-step scratch for structured errors pushed via
    /// [`EffectNodeContext::error`]. Drained + logged after each
    /// `evaluate` / `late_capture` returns. Errors don't halt the frame
    /// — the producing primitive is expected to emit a deterministic
    /// fallback (e.g. magenta clear) alongside the error report.
    error_scratch: Vec<String>,
    /// Persistent resources whose first acquisition has been cleared to
    /// opaque black. Subsequent frames find them in this set and skip
    /// the clear — the buffer's contents are now valid producer writes
    /// carrying state across the frame boundary.
    initialized_persistent: ahash::AHashSet<ResourceId>,
    /// Per-frame "this step is reachable from a final output via at
    /// least one live mux branch" bitset, reused across frames to
    /// avoid per-frame allocation. Populated by [`compute_live_steps`]
    /// at the top of each frame; consumed by the step loop to skip
    /// dispatches for pruned branches. Cleared (`.fill(false)`) before
    /// each rebuild; capacity grows on demand.
    live_steps: Vec<bool>,
    /// Per-frame scratch for `selected_input_branch`'s `wired_inputs`
    /// argument. Reused across nodes; cleared before each call.
    wired_scratch: Vec<&'static str>,
    /// Authoring-time output preview: when set, the executor preserves this
    /// node's first Texture2D output past the frame (skips its `free_after`
    /// release) so the graph editor can sample it. `None` disables capture —
    /// zero cost on the live path. Set per frame via [`set_preview_target`].
    preview_target: Option<NodeInstanceId>,
    /// The Texture2D output resource of `preview_target`, recorded during the
    /// step loop. After `execute_frame_*`, the integration layer reads its
    /// texture via [`Backend::slot_for`] + [`Backend::texture_2d`] and
    /// downscales it into the preview surface. `None` if the target didn't run
    /// (pruned) or has no texture output (a scalar/array-only node).
    preview_resource: Option<ResourceId>,
    /// Live scalar values on the previewed node's input ports this frame
    /// (`port_name`, value). Captured when the target node has no texture
    /// output — drives the editor's value-inspector panel for control / math /
    /// envelope nodes that currently show a black pane. Cleared each frame.
    preview_scalar_inputs: Vec<(String, f32)>,
    /// Same for the previewed node's scalar OUTPUT ports — the live signal the
    /// node is producing (an LFO's current value, a math result).
    preview_scalar_outputs: Vec<(String, f32)>,
    /// Authoring-time "dump EVERY output" mode (the Cmd+D one-shot disk dump).
    /// When set, every node's outputs are recorded in [`dump_resources`] so the
    /// host can read them all from one frame and write them to disk. One-shot,
    /// off by default — costs nothing on the live path. For the continuous
    /// editor thumbnail atlas, prefer [`dump_set`] (records only the nodes the
    /// canvas can show) instead of dumping the whole flattened graph.
    dump_all: bool,
    /// Continuous "dump only THESE nodes" mode — the editor thumbnail atlas.
    /// `Some(set)` records only the listed nodes (the canvas's currently-visible
    /// scope), so a collapsed group or an off-scope subgraph costs nothing:
    /// hidden nodes keep their memoization and their textures recycle through
    /// the pool. `None` = atlas off. Coexists with [`dump_all`] via
    /// [`should_dump`](Self::should_dump) (Cmd+D still dumps everything).
    dump_set: Option<ahash::AHashSet<NodeInstanceId>>,
    /// Resources recorded into the dump this frame, so the release loop can pin
    /// exactly those past the frame (their slots must not be reacquired and
    /// overwritten before the host reads them) and recycle everything else.
    /// Populated by [`record_dump_outputs`](Self::record_dump_outputs), cleared
    /// at frame start. Replaces the old "skip every free_after while dumping"
    /// blanket pin — under [`dump_all`] this still pins every recorded output,
    /// but under [`dump_set`] only the visible nodes' outputs are held.
    dump_pinned_resources: ahash::AHashSet<ResourceId>,
    /// `(node, output_port, resource, texture)` for every Texture2D output
    /// recorded this frame by a node in the dump scope (see
    /// [`should_dump`](Self::should_dump): all under `dump_all`, or only the
    /// visible nodes under `dump_set`). Cleared and repopulated
    /// each frame. Read after `execute_frame_*` via [`dump_resources`].
    ///
    /// The texture is a clone (retain bump) captured at the moment the producer
    /// step records its output — *before* the end-of-frame feedback swap
    /// ([`MetalBackend::swap_texture_2d`]) physically swaps render targets
    /// between persistent slots. Re-resolving `slot_for(res)` after the frame
    /// (the old approach) returns the swapped, about-to-be-overwritten buffer on
    /// alternate frames, which strobed the editor's per-node thumbnails between
    /// the real output and black. Pinning the identity here reads the buffer the
    /// step actually wrote, regardless of any later swap. `None` only when the
    /// resource has no backing texture (e.g. the mock backend in tests); real
    /// GPU runs always resolve it.
    dump_resources:
        Vec<(NodeInstanceId, &'static str, ResourceId, Option<manifold_gpu::GpuTexture>)>,
    /// Same, for `Array` (storage-buffer) outputs — particle/instance/edge
    /// buffers. Read via [`dump_array_resources`] and decoded against the
    /// resource's `ArrayType` channel layout.
    dump_array_resources: Vec<(NodeInstanceId, &'static str, ResourceId)>,
    /// Dedup key for the node-output-preview diagnostic log:
    /// `(target, matched_a_live_step, texture_2d_output_count,
    /// captured_resource)`. Logged (grep `[preview]`) only when it changes
    /// while a preview is active, so the terminal shows one line per retarget
    /// instead of per-frame spam. Diagnostic only — the live render path never
    /// sets a preview target, so this stays `None` there.
    preview_debug_last: Option<(Option<NodeInstanceId>, bool, usize, Option<ResourceId>)>,
    /// Profiling-only: force every step live, bypassing the
    /// [`compute_live_steps`] mux/liveness pruning. Lets the per-dispatch
    /// profiler run an arbitrary plan *prefix* (which has no `FinalOutput` to
    /// seed liveness) so it executes exactly steps `[0..k]` and the marginal
    /// `time[k]-time[k-1]` attributes to step `k`. Off by default — the live
    /// render path never sets it, so pruning behaves exactly as before.
    profile_force_all_live: bool,
    /// Per-step attribution profiling: when on, the executor stamps a
    /// `s{step_idx}` tag onto the GPU encoder before each node evaluates (so
    /// counter-sampled GPU spans join back to steps) and records each step's
    /// CPU encode cost in [`step_profiles`]. Off by default — one branch per
    /// step on the live path.
    profiling: bool,
    /// This executor's instance identity (`fx:{layer_id}`, `gen:{layer_id}`,
    /// `master`, `led:{...}`) — set by the owning compositor/generator-
    /// renderer at chain-insertion time (D6 correction, PERF_BUDGET_GATE_DESIGN
    /// P2). Stamped as the `"{scope}:s{idx}"` prefix on every profiled tag so
    /// GPU spans from a multi-executor, multi-command-buffer frame join back
    /// to the right instance instead of colliding on a bare `s{idx}`. Empty
    /// string is a valid (unscoped) default — the tag format always includes
    /// it so the join key shape never depends on whether a scope was set.
    profile_scope: String,
    /// `(step_idx, node, type_id, cpu_nanos)` per live step of the last
    /// profiled frame. Cleared at frame start while [`profiling`] is on.
    step_profiles: Vec<StepProfile>,
    /// Memoized-dataflow state (constant-subgraph hoisting). `step_memo[idx]`
    /// records the epochs a PURE step ([`EffectNode::is_pure`]) last executed
    /// with. A pure step whose node `param_epoch` and input resource epochs
    /// are unchanged is CLEAN: skipped exactly like a pruned mux branch — its
    /// held output slots serve consumers. A static gradient LUT renders once,
    /// not per frame; a palette tweak bumps the param epoch and re-renders it
    /// once. Sized to the plan's step count on first frame (plans never swap
    /// under a live executor — topology changes rebuild the whole runtime).
    step_memo: Vec<Option<StepMemo>>,
    /// Producer-execution counter per resource: bumped every time the step
    /// producing the resource actually evaluates. The memo compares these to
    /// detect upstream changes. Non-pure producers bump every frame they run,
    /// which conservatively keeps their consumers dirty.
    resource_epoch: ahash::AHashMap<ResourceId, u64>,
    /// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md D5 — per-step "this node
    /// declared its outputs unchanged this frame" flag
    /// (`ctx.mark_outputs_unchanged()`). Reset to `false` for every step
    /// EVERY frame (unlike `step_memo`, which persists across frames) —
    /// sized to the plan's step count alongside it. Populated after each
    /// step's `evaluate` returns; READ BY NOTHING yet (P1 stub only — P2
    /// consumes this to gate dirty-caching decisions elsewhere).
    node_declared_unchanged: Vec<bool>,
    /// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md D5 — per-physical-slot write
    /// generation, indexed by `Slot.0`. Bumped at the single choke point
    /// where a step's outputs are committed (the same site `resource_epoch`
    /// bumps, immediately below it), UNLESS `node_declared_unchanged[idx]`
    /// is `true` for this step. Grows on demand as new physical slots are
    /// allocated (same pattern as `live_steps`'s per-frame resize). Read
    /// side: [`crate::node_graph::bindings::NodeInputs::slot_generation`].
    /// Never reset within an executor's lifetime — only ever grows or
    /// increments, so two frames of the SAME executor comparing generation
    /// numbers is always sound. See `rebuild_epoch` for the cross-executor-
    /// lifetime hazard this alone does not cover.
    slot_generations: Vec<u64>,
    /// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3b/BUG-197 — per-step
    /// "last frame's param-driven alias" state: `(aliased-from resource,
    /// destination slot, in-resource's write generation at alias time)`,
    /// indexed like `node_declared_unchanged`/`step_memo`. Populated ONLY
    /// on the `performed_alias && !data_skip` path (a node's
    /// `skip_passthrough` declaration, e.g. `mux_texture`'s
    /// inline-selector fast path) — `None` for every other step, and reset
    /// to `None` whenever that step does not take that exact path this
    /// frame (an alias that fails `compatible()`, a step whose alias
    /// source flips to the data-skip contract, or a step that stops
    /// aliasing altogether must not let a stale match fire later). The
    /// resource (not the physical destination-input slot) is the identity
    /// term for the ALIASED-FROM side deliberately: the compiled edge a
    /// `skip_passthrough` declaration selects is stable frame to frame
    /// unless the node's own param-driven branch choice changes, whereas
    /// pool recycling can legitimately hand the SAME resource a different
    /// physical slot between frames (the `last_mip_identity` precedent);
    /// keying the input side on physical slot would treat that ordinary
    /// recycling as "a different source" and never stabilize. The
    /// destination side stays a physical `Slot` on purpose: the generation
    /// bookkeeping this state guards is itself slot-indexed
    /// (`slot_generations`), so a destination slot reassignment
    /// invalidates any generation comparison and must fall through to a
    /// conservative bump. This is the trust prerequisite for declaring
    /// `node_declared_unchanged[idx]` on an alias step: same aliased-from
    /// resource AND same destination slot as last frame AND the resource's
    /// generation hasn't moved since ⇒ the aliased output is provably the
    /// same content as last frame's, safe to skip downstream. The empty-
    /// propagation data-skip alias path is explicitly excluded (never
    /// populates or reads this) — its identity can flip between different
    /// pruned producers frame to frame with no generation signal backing
    /// it, so it keeps the conservative bump. Cleared (all `None`) on
    /// rebuild alongside `step_memo`/`node_declared_unchanged`.
    alias_propagation_state: Vec<Option<(ResourceId, Slot, u64)>>,
    /// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md D6 — this executor
    /// instance's rebuild epoch, assigned once at construction from
    /// [`NEXT_REBUILD_EPOCH`] (a process-global monotonic counter; precedent:
    /// `chain_dispatch.rs`'s `CHAIN_REBUILD_COUNT`, `bundled_presets.rs`'s
    /// `generation: AtomicU64`). `PresetRuntime::harvest_state_from`
    /// (preset_runtime.rs) can swap a matching node's own `Box<dyn
    /// EffectNode>` across a topology rebuild into a BRAND NEW `Executor`
    /// (fresh `resource_epoch`/`slot_generations`, both starting over) — so
    /// a harvested node's cached dirty-check key, computed under the PRIOR
    /// executor's generation numbers, could otherwise coincidentally collide
    /// with the new executor's low counts. Folding this epoch into any such
    /// key guarantees a stale key can never match: every `Executor::new()`
    /// call gets a strictly higher epoch than the last one issued.
    rebuild_epoch: u64,
    /// Per-step HOISTABLE bit: the step's node is pure AND every input is
    /// produced by a hoistable step. The closure itself is classified at
    /// plan compile time ([`ExecutionPlan::step_hoistable`] /
    /// [`ExecutionPlan::held_resources`]) — lifetimes are decided once, in
    /// the plan, so every consumer (this executor's pool release, the chain
    /// runtime's slot planner) agrees. Held resources never appear in
    /// `free_after`, so no runtime exemption exists here.
    /// Step count the memo structures were built for; rebuilt when it
    /// differs (defensive — a live executor's plan does not change shape).
    memo_steps_len: Option<usize>,

    /// Data-driven skip (the third skip reason, after the mux short-circuit
    /// and the memoized-dataflow clean skip). Resources whose producer
    /// reported EMPTY output this frame ([`EffectNode::reports_empty_output`]
    /// — zero blobs, zero spawned particles), plus the outputs of every step
    /// skipped through [`EffectNode::empty_skip_input_ports`] (transitive).
    /// Rebuilt every frame in step order — producers always precede
    /// consumers, so a consumer's check sees its producers' marks.
    empty_resources: ahash::AHashSet<ResourceId>,
    /// Last frame's [`Self::empty_resources`] (swapped at frame top). The
    /// consumer skip requires empty-last-frame AND empty-this-frame, so a
    /// declaring node always EXECUTES the first empty frame — writing out its
    /// empty state — before its held outputs are served to consumers. Without
    /// the guard, a skip on the first empty frame would serve the last
    /// NON-empty frame's content (ghost blobs).
    empty_resources_prev: ahash::AHashSet<ResourceId>,
    /// Live wire-resolved scalar value for every node's wired scalar INPUT
    /// port, snapshotted at the top of each step's turn in the per-frame
    /// loop — the point at which the step's own declared inputs are
    /// guaranteed bound (produced by an earlier step, not yet released:
    /// release only happens at the LAST reader's own turn, which is this
    /// step or later). Captured unconditionally, before the mux / memo /
    /// data-driven skip branches, so a value stays fresh even on frames
    /// where the node's step is skip-continued (its held resource is
    /// still the last real write). Entries are `(node, port name,
    /// value)`; port names are the same `&'static str`-interned strings
    /// as [`execution_plan::ExecutionStep::inputs`], so capture is a
    /// plain push — no string allocation. Small (bounded by the graph's
    /// wired-scalar-input count) and read via a linear scan in
    /// [`live_scalar_input`](Self::live_scalar_input) — avoids a hash
    /// key whose lifetime would need to match a caller's borrowed `&str`
    /// param name against this map's `&'static str` port name.
    ///
    /// Feeds [`crate::preset_runtime::PresetRuntime::live_node_params`]:
    /// a param whose same-named input port carries a scalar wire should
    /// report the wire's value here, not the frozen `NodeInstance::params`
    /// entry (which a wired scalar input never writes) — the same
    /// resolution order as
    /// [`EffectNodeContext::scalar_or_param`](crate::node_graph::effect_node::EffectNodeContext::scalar_or_param)
    /// (wire first, param second). Cleared and rebuilt every frame.
    live_scalar_inputs: Vec<(NodeInstanceId, &'static str, f32)>,
}

/// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md D6 — process-global source for
/// [`Executor::rebuild_epoch`]. Starts at 1 (0 is never issued, left free
/// as an obviously-invalid sentinel for any future test/default construction
/// that doesn't go through `Executor::new`).
static NEXT_REBUILD_EPOCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Epoch snapshot a pure step last executed with — see [`Executor::step_memo`].
struct StepMemo {
    param_epoch: u64,
    /// Aligned with the step's `inputs` order.
    input_epochs: Vec<u64>,
}

/// One step's CPU-side cost from a profiled frame: acquire + evaluate
/// (= GPU command encoding) + scalar drains. GPU time lives in the
/// command buffer's [`manifold_gpu::GpuFrameProfile`], joined by `tag`
/// (the same `"{scope}:s{step_idx}"` string [`GpuEncoder::set_profile_tag`]
/// stamped on the encoder for this step — D6 correction).
#[derive(Clone, Debug)]
pub struct StepProfile {
    pub step_idx: usize,
    pub node: NodeInstanceId,
    pub type_id: String,
    pub cpu_nanos: u64,
    /// The scoped join key: `"{scope}:s{step_idx}"`, byte-identical to the
    /// tag stamped on the GPU encoder for this step.
    pub tag: String,
}

impl Executor {
    /// Mark a persistent resource as already initialized, so the first-frame
    /// clear-to-black at acquisition is skipped. Called by the state harvest
    /// (docs/CHAIN_FUSION_DESIGN.md §5) after installing a carried-over
    /// texture into the resource's slot — without this, the rebuilt
    /// executor's fresh `initialized_persistent` set would wipe the migrated
    /// trail on its first frame.
    pub fn mark_persistent_initialized(&mut self, res_id: ResourceId) {
        self.initialized_persistent.insert(res_id);
    }

    /// Construct an executor with the given backend.
    pub fn new(backend: Box<dyn Backend>) -> Self {
        Self {
            backend,
            input_scratch: Vec::new(),
            output_scratch: Vec::new(),
            scalar_write_scratch: Vec::new(),
            camera_write_scratch: Vec::new(),
            light_write_scratch: Vec::new(),
            material_write_scratch: Vec::new(),
            transform_write_scratch: Vec::new(),
            atmosphere_write_scratch: Vec::new(),
            object_write_scratch: Vec::new(),
            error_scratch: Vec::new(),
            initialized_persistent: ahash::AHashSet::default(),
            live_steps: Vec::new(),
            wired_scratch: Vec::new(),
            preview_target: None,
            preview_resource: None,
            preview_scalar_inputs: Vec::new(),
            preview_scalar_outputs: Vec::new(),
            dump_all: false,
            dump_set: None,
            dump_pinned_resources: ahash::AHashSet::new(),
            dump_resources: Vec::new(),
            dump_array_resources: Vec::new(),
            preview_debug_last: None,
            profile_force_all_live: false,
            profiling: false,
            profile_scope: String::new(),
            step_profiles: Vec::new(),
            step_memo: Vec::new(),
            resource_epoch: ahash::AHashMap::default(),
            node_declared_unchanged: Vec::new(),
            slot_generations: Vec::new(),
            alias_propagation_state: Vec::new(),
            rebuild_epoch: NEXT_REBUILD_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            memo_steps_len: None,
            empty_resources: ahash::AHashSet::default(),
            empty_resources_prev: ahash::AHashSet::default(),
            live_scalar_inputs: Vec::new(),
        }
    }

    /// Enable per-step attribution profiling (CPU encode cost + GPU span
    /// tags). Pair with [`manifold_gpu::GpuEncoder::enable_dispatch_profiling`]
    /// on the frame's encoder; read results via [`Self::take_step_profiles`].
    pub fn set_profiling(&mut self, on: bool) {
        self.profiling = on;
    }

    /// Set this executor's instance identity for profiled tags (D6
    /// correction): `fx:{layer_id}`, `gen:{layer_id}`, `master`, `led:{...}`.
    /// Cheap (a `String` assign) — call at chain-insertion time from the
    /// owning compositor/generator-renderer, not gated on [`Self::profiling`]
    /// so the scope is always current the moment profiling IS turned on.
    pub fn set_profile_scope(&mut self, scope: &str) {
        self.profile_scope.clear();
        self.profile_scope.push_str(scope);
    }

    /// Drain the per-step CPU profiles recorded on the last profiled frame.
    pub fn take_step_profiles(&mut self) -> Vec<StepProfile> {
        std::mem::take(&mut self.step_profiles)
    }

    /// This executor's rebuild epoch — see [`Self::rebuild_epoch`]'s field
    /// doc. Stable for the executor's whole lifetime; a fresh `Executor`
    /// always gets a strictly higher value than any issued before it.
    pub fn rebuild_epoch(&self) -> u64 {
        self.rebuild_epoch
    }

    /// Profiling-only: when on, [`compute_live_steps`] marks every step live
    /// (no mux/liveness pruning), so an arbitrary plan prefix runs all of its
    /// steps. Used by the per-dispatch profiler; never set on the live path.
    pub fn set_profile_force_all_live(&mut self, on: bool) {
        self.profile_force_all_live = on;
    }

    /// Enable/disable "dump EVERY output" mode for the NEXT frame (the Cmd+D
    /// disk dump). When on, every node's Texture2D/Array outputs are recorded in
    /// [`dump_resources`](Self::dump_resources) and each recorded resource is
    /// held past the frame (pinned via [`dump_pinned_resources`], so its slot
    /// isn't reacquired and overwritten before the host reads it). One-shot: the
    /// host turns it on, runs a frame, reads the textures, turns it off. For the
    /// continuous editor atlas use [`set_dump_set`](Self::set_dump_set) instead.
    pub fn set_dump_all(&mut self, on: bool) {
        self.dump_all = on;
    }

    /// Set (or clear) the continuous thumbnail-atlas dump set — the nodes the
    /// editor canvas can currently show. `Some(set)` records only those nodes;
    /// `None` turns the atlas dump off. Coexists with [`set_dump_all`]; the
    /// Cmd+D one-shot still dumps everything. Call per frame on the watched
    /// chain (see `PresetRuntime::set_dump_visible`).
    pub fn set_dump_set(&mut self, set: Option<ahash::AHashSet<NodeInstanceId>>) {
        self.dump_set = set;
    }

    /// Whether `node`'s outputs should be recorded into the dump this frame:
    /// everything under the Cmd+D `dump_all`, or only the listed nodes under
    /// the atlas `dump_set`. False on the live path (both off).
    fn should_dump(&self, node: NodeInstanceId) -> bool {
        self.dump_all || self.dump_set.as_ref().is_some_and(|s| s.contains(&node))
    }

    /// Record every Texture2D / Array output of `step` into the dump buffers,
    /// pinning each texture's identity NOW (before the end-of-frame feedback
    /// swap rebinds slots — see [`dump_resources`](Self::dump_resources)) and
    /// marking each recorded resource in [`dump_pinned_resources`] so the
    /// release loop holds it past the frame. Called both for steps that
    /// executed this frame and for steps that skipped (memoized / data-skipped)
    /// but whose held output slots still carry valid content, so a static
    /// subgraph keeps its zero-cost skip yet still shows a current thumbnail.
    /// Caller gates on [`should_dump`](Self::should_dump).
    fn record_dump_outputs(&mut self, plan: &ExecutionPlan, step: &ExecutionStep) {
        for &(port, res) in &step.outputs {
            match plan.resource_type(res) {
                Some(t) if t.is_texture_2d() => {
                    let tex = self
                        .backend
                        .slot_for(res)
                        .and_then(|s| self.backend.texture_2d(s))
                        .cloned();
                    self.dump_resources.push((step.node, port, res, tex));
                    self.dump_pinned_resources.insert(res);
                }
                Some(crate::node_graph::ports::PortType::Array(_)) => {
                    self.dump_array_resources.push((step.node, port, res));
                    self.dump_pinned_resources.insert(res);
                }
                _ => {}
            }
        }
    }

    /// `(node, output_port, resource, texture)` for every Texture2D output
    /// captured on the last frame while dump mode was on. The texture is pinned
    /// to the buffer the producer step wrote, before any end-of-frame swap — use
    /// it directly rather than re-resolving `slot_for(res)`, which would read the
    /// swapped buffer on alternate frames.
    pub fn dump_resources(
        &self,
    ) -> &[(NodeInstanceId, &'static str, ResourceId, Option<manifold_gpu::GpuTexture>)] {
        &self.dump_resources
    }

    /// `(node, output_port, resource)` for every `Array` output captured on
    /// the last frame while dump mode was on. Resolve to a buffer via
    /// [`Backend::array_buffer`] and decode against the resource's `ArrayType`.
    pub fn dump_array_resources(&self) -> &[(NodeInstanceId, &'static str, ResourceId)] {
        &self.dump_array_resources
    }

    /// Set the node whose output texture should be preserved for an
    /// authoring-time preview, or `None` to disable. Cheap; call per frame
    /// before `execute_frame_*`. When set, the named node's first Texture2D
    /// output survives the frame so [`preview_resource`](Self::preview_resource)
    /// can hand it to the integration layer for downscaling.
    pub fn set_preview_target(&mut self, node: Option<NodeInstanceId>) {
        self.preview_target = node;
    }

    /// The preview target's Texture2D output resource from the last frame, if
    /// the target ran and produced one. Resolve to a texture via
    /// [`Backend::slot_for`] + [`Backend::texture_2d`] on [`backend`](Self::backend).
    pub fn preview_resource(&self) -> Option<ResourceId> {
        self.preview_resource
    }

    /// Live scalar input values on the previewed node this frame
    /// (`port_name`, value). Non-empty only when the target ran and had no
    /// texture output (a control / math / scalar node). See
    /// [`preview_scalar_outputs`](Self::preview_scalar_outputs).
    pub fn preview_scalar_inputs(&self) -> &[(String, f32)] {
        &self.preview_scalar_inputs
    }

    /// Live scalar OUTPUT values on the previewed node — the signal it's
    /// producing this frame.
    pub fn preview_scalar_outputs(&self) -> &[(String, f32)] {
        &self.preview_scalar_outputs
    }

    /// Live wire-resolved value of `node`'s scalar INPUT port `port`, if a
    /// scalar wire is connected to it this frame. `None` when the port is
    /// unwired (or wired to a non-scalar/absent resource) — the caller
    /// should fall back to the node's param-map value, exactly the
    /// `scalar_or_param` port-shadows-param order. See
    /// [`live_scalar_inputs`](Self::live_scalar_inputs) for how this is
    /// captured.
    pub fn live_scalar_input(&self, node: NodeInstanceId, port: &str) -> Option<f32> {
        self.live_scalar_inputs
            .iter()
            .find(|&&(n, p, _)| n == node && p == port)
            .map(|&(_, _, v)| v)
    }

    /// Read a scalar resource's current value off the backend as `f32`, or
    /// `None` if it isn't a `Scalar` port or has no bound slot. `Bool`/`Enum`
    /// collapse to a number for display.
    fn read_scalar_resource(&self, plan: &ExecutionPlan, res: ResourceId) -> Option<f32> {
        if !matches!(plan.resource_type(res), Some(crate::node_graph::ports::PortType::Scalar(_))) {
            return None;
        }
        let slot = self.backend.slot_for(res)?;
        match self.backend.scalar(slot)? {
            ParamValue::Float(f) => Some(f),
            ParamValue::Bool(b) => Some(if b { 1.0 } else { 0.0 }),
            ParamValue::Enum(e) => Some(e as f32),
            _ => None,
        }
    }

    /// Convenience constructor with a fresh [`MockBackend`]. Used by tests
    /// and any code that doesn't need real GPU resources.
    pub fn with_mock() -> Self {
        Self::new(Box::new(MockBackend::new()))
    }

    pub fn backend(&self) -> &dyn Backend {
        &*self.backend
    }

    pub fn backend_mut(&mut self) -> &mut dyn Backend {
        &mut *self.backend
    }

    /// Run one frame of the graph without a GPU encoder.
    ///
    /// Convenience entry point for tests against [`MockBackend`] and any
    /// scenario where the graph contains only nodes that don't issue real
    /// GPU work (boundary nodes, stub primitives).
    ///
    /// Panics with a clean diagnostic *at entry* if the compiled plan
    /// contains any node that declares it [`requires`](crate::node_graph::EffectNode::requires)
    /// a `GpuEncoder` or a `StateStore` — that's a programmer error
    /// (wrong entry point for the graph), not a per-node `.expect()`.
    pub fn execute_frame(&mut self, graph: &mut Graph, plan: &ExecutionPlan, time: FrameTime) {
        let r = plan.requires();
        assert!(
            !r.gpu_encoder,
            "Executor::execute_frame called with a plan containing node(s) that require a GpuEncoder \
             — dispatch through `execute_frame_with_gpu` instead.",
        );
        assert!(
            !r.state_store,
            "Executor::execute_frame called with a plan containing node(s) that require a StateStore \
             — dispatch through `execute_frame_with_state` instead.",
        );
        self.execute_frame_inner(graph, plan, time, None, None, 0);
    }

    /// Run one frame of the graph with a real `GpuEncoder` available to
    /// every node. Used by the production renderer integration; pairs with
    /// [`MetalBackend`](crate::node_graph::MetalBackend) for real
    /// `GpuTexture` allocation.
    ///
    /// Panics with a clean diagnostic *at entry* if the plan contains
    /// any node that declares it requires a `StateStore` — those
    /// graphs must dispatch through `execute_frame_with_state`.
    pub fn execute_frame_with_gpu(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        time: FrameTime,
        gpu: &mut GpuEncoder<'_>,
    ) {
        assert!(
            !plan.requires().state_store,
            "Executor::execute_frame_with_gpu called with a plan containing node(s) that require \
             a StateStore — dispatch through `execute_frame_with_state` instead. \
             (Common cause: a chain containing `temporal::Feedback` dispatched via a code path \
             that hasn't been ported to the StateStore-aware execute method.)",
        );
        self.execute_frame_inner(graph, plan, time, Some(gpu), None, 0);
    }

    /// Run one frame of the graph with a real `GpuEncoder` plus a
    /// `StateStore` for stateful nodes (Bloom mip chains, Feedback prev-
    /// frame buffers, etc.). The `owner_key` is forwarded to every node
    /// via `EffectNodeContext::owner_key` and keys per-clip / per-layer
    /// state in the store.
    ///
    /// This entry point provides every runtime service today's nodes
    /// can declare, so there's no entry-side panic for plan-vs-services
    /// mismatch.
    pub fn execute_frame_with_state(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        time: FrameTime,
        gpu: &mut GpuEncoder<'_>,
        state: &mut StateStore,
        owner_key: OwnerKey,
    ) {
        self.execute_frame_inner(graph, plan, time, Some(gpu), Some(state), owner_key);
    }

    /// Build the per-frame live-step bitset that drives mux short-
    /// circuit. Walks `plan.steps()` in reverse: every `FinalOutput`
    /// step seeds the live set, and each live step propagates
    /// liveness backwards to its inputs' producers — with one
    /// twist for branch-selector nodes (see
    /// [`EffectNode::selected_input_branch`]). When a live step is a
    /// selector with an unwired selector port, only the chosen input
    /// port's producer is marked live; the other inputs' producers
    /// stay unmarked unless some OTHER live path also depends on
    /// them. Equivalent to "every node reachable from a FinalOutput
    /// via at least one live mux branch."
    ///
    /// Worklist propagation: push every newly-live step and process
    /// it once. The reason a single reverse-only sweep is wrong: a
    /// state-capture wire from a `breaks_dependency_cycle` node (e.g.
    /// `node.feedback`'s `in` port) connects a LOW-topo-idx consumer
    /// to a HIGH-topo-idx producer — `feedback`'s `in` reads from
    /// `color_combine`, which runs LATER in the plan because the
    /// state-capture exemption removes that wire from in-degree. A
    /// reverse sweep marks `color_combine` live when it visits
    /// `feedback`, but it has already passed `color_combine`'s index,
    /// so `color_combine`'s OWN inputs (and their producers) never
    /// propagate. Result: the feedback-write subgraph runs with
    /// unbound inputs, the persistent slot never updates, state
    /// stays at the first-frame clear. Worklist processes a step
    /// the moment it's marked, so back-edges across topo order are
    /// handled without iteration to convergence.
    ///
    /// `wired_scratch` is reused across nodes to avoid per-frame
    /// allocation in the inner loop.
    fn compute_live_steps(&mut self, graph: &Graph, plan: &ExecutionPlan) {
        let steps = plan.steps();
        self.live_steps.clear();
        self.live_steps.resize(steps.len(), false);

        // Profiling override: run every step, skipping liveness/mux pruning.
        // A plan prefix has no FinalOutput to seed liveness, so without this it
        // would prune to whatever the prefix's stateful roots happen to reach.
        if self.profile_force_all_live {
            self.live_steps.fill(true);
            return;
        }

        // Build producer map: ResourceId → step index that produces
        // it. Walked once; reused for every input-port propagation.
        // Per-frame allocation is a deliberate tradeoff against
        // carrying a parallel structure on ExecutionPlan — this
        // table's size is bounded by `plan.resource_count()` which
        // is small (tens to low hundreds even for the densest
        // generators), and rebuilding it keeps the executor's
        // per-frame state self-contained.
        let mut producer: ahash::AHashMap<ResourceId, usize> =
            ahash::AHashMap::with_capacity(plan.resource_count());
        for (idx, step) in steps.iter().enumerate() {
            for &(_, res_id) in &step.outputs {
                producer.insert(res_id, idx);
            }
        }

        // Seed every node that's a liveness root — `system.final_output`,
        // primitives with `aliased_array_io`, primitives with
        // `state_capture_input_ports`, and any future cross-frame
        // mechanism. See `EffectNode::is_liveness_root` for the concept
        // and the default impl. Roots run regardless of downstream
        // consumers; everything else is reachable from a root via
        // per-frame wires or gets pruned.
        let mut worklist: Vec<usize> = Vec::new();
        for (idx, step) in steps.iter().enumerate() {
            if let Some(inst) = graph.get_node(step.node)
                && inst.node.is_liveness_root()
            {
                self.live_steps[idx] = true;
                worklist.push(idx);
            }
        }

        // Drain the worklist. Each pop processes a live step's inputs,
        // marking their producers live and pushing them on for their
        // own propagation. Mux short-circuit applies as before:
        // selector-equipped nodes restrict propagation to the chosen
        // branch's input port.
        while let Some(idx) = worklist.pop() {
            let step = &steps[idx];
            let Some(inst) = graph.get_node(step.node) else {
                continue;
            };

            // Resolve the optional selected-input-branch hint. The
            // node sees the list of port names that have wires
            // connected — used by mux to detect a wired selector and
            // bail out of the optimisation.
            self.wired_scratch.clear();
            for &(port_name, _) in &step.inputs {
                self.wired_scratch.push(port_name);
            }
            let selected =
                inst.node.selected_input_branch(&inst.params, &self.wired_scratch);
            // Branch pruning applies only to the chosen port's SIBLINGS — the
            // ports of the same type (the mux's other `in_N` branches). A
            // control input of a different type (a WIRED selector, whose
            // latched value produced this hint) must stay live or the
            // selector chain itself would be pruned and the latch frozen.
            let chosen_ty = selected.and_then(|chosen| {
                inst.node.inputs().iter().find(|p| p.name == chosen).map(|p| p.ty)
            });

            for &(port_name, res_id) in &step.inputs {
                if let Some(chosen) = selected
                    && port_name != chosen
                    && chosen_ty.as_ref().is_some_and(|ct| {
                        inst.node
                            .inputs()
                            .iter()
                            .find(|p| p.name == port_name)
                            .map(|p| &p.ty)
                            == Some(ct)
                    })
                {
                    continue;
                }
                if let Some(&prod_step) = producer.get(&res_id)
                    && !self.live_steps[prod_step]
                {
                    self.live_steps[prod_step] = true;
                    worklist.push(prod_step);
                }
            }
        }

        // Graphs without any FinalOutput (test fixtures, in-flight
        // editor graphs) get NO live seeds → every step skipped →
        // executor is a no-op for that frame. That matches the
        // pre-existing behaviour of `compile` filtering to
        // FinalOutput-reachable nodes only when a FinalOutput is
        // present (see execution_plan.rs `has_final_output` branch).
        // For the no-FinalOutput fallback path we want every step
        // live, otherwise tests like
        // `value::tests::value_runs_without_final_output` would
        // regress. Detect by checking whether anything got seeded.
        if !self.live_steps.iter().any(|&b| b) {
            self.live_steps.fill(true);
        }
    }

    /// Shared implementation. For each step in plan order:
    ///   1. Acquire a slot for every output port (so distinct slots from inputs).
    ///   2. Look up slots for every wired input port.
    ///   3. Call `EffectNode::evaluate` with the assembled context.
    ///   4. Release slots for resources whose last reader is this step.
    ///
    /// The acquire-then-release order is correct because evaluate writes to
    /// outputs while reading from inputs; freeing inputs before allocating
    /// outputs would let the new acquire reuse the still-being-read slot.
    ///
    /// Mux short-circuit: [`compute_live_steps`] runs first, marking
    /// every step reachable from a FinalOutput via at least one live
    /// mux branch. Non-live steps are skipped entirely (no acquire,
    /// no evaluate, no `free_after`). The resources they would have
    /// freed remain bound to their slots — that's correct, the
    /// backend's idempotent `acquire` will hand the same slot back
    /// next frame if the consumer becomes live again. Worst-case
    /// slot count grows to "max over all branches ever selected"
    /// rather than "max over currently-selected branches," which is
    /// the right tradeoff for live-perform mode switches.
    fn execute_frame_inner(
        &mut self,
        graph: &mut Graph,
        plan: &ExecutionPlan,
        time: FrameTime,
        mut gpu: Option<&mut GpuEncoder<'_>>,
        mut state: Option<&mut StateStore>,
        owner_key: OwnerKey,
    ) {
        self.compute_live_steps(graph, plan);

        // Build the memoized-dataflow structures on first frame (or if the
        // plan shape ever changed — defensive; live executors keep one plan).
        // The hoistable closure and the held (sticky) resource set are
        // classified at plan compile time — see ExecutionPlan::held_resources.
        if self.memo_steps_len != Some(plan.steps().len()) {
            self.memo_steps_len = Some(plan.steps().len());
            self.step_memo.clear();
            self.step_memo.resize_with(plan.steps().len(), || None);
            self.resource_epoch.clear();
            self.node_declared_unchanged.resize(plan.steps().len(), false);
            self.alias_propagation_state.clear();
            self.alias_propagation_state.resize_with(plan.steps().len(), || None);
        }
        // D5: reset every frame (not sticky like `step_memo`) — a node
        // must re-declare on every frame it wants to skip; the executor
        // never carries last frame's declaration forward.
        self.node_declared_unchanged.iter_mut().for_each(|v| *v = false);

        // Reset preview capture for this frame. Re-resolved below if the
        // target node is live and produces a texture.
        self.preview_resource = None;
        self.preview_scalar_inputs.clear();
        self.preview_scalar_outputs.clear();
        self.live_scalar_inputs.clear();
        self.dump_resources.clear();
        self.dump_array_resources.clear();
        self.dump_pinned_resources.clear();
        if self.profiling {
            self.step_profiles.clear();
        }

        // Data-driven skip: roll this frame's empty-resource marks into
        // "previous" and start the current set fresh — reporters re-mark on
        // every evaluate, so emptiness never persists past the frame that
        // observed it.
        std::mem::swap(&mut self.empty_resources, &mut self.empty_resources_prev);
        self.empty_resources.clear();

        // Wipe any skip-passthrough aliases installed during the previous
        // frame. Without this, a slot that was aliased-on-skip last frame
        // would shadow its real write this frame and downstream reads
        // would still see the old upstream texture. Host-installed
        // borrows (e.g. the chain source slot's per-frame
        // `replace_texture_2d`) are untouched.
        self.backend.clear_skip_aliases();

        // Pre-acquire persistent resources before the step loop.
        // These are wires that close a per-frame feedback loop through
        // the StateStore (their consumer node declared
        // `breaks_dependency_cycle`). The consumer runs at step 0 — its
        // `slot_for(res_id)` would panic if the resource hadn't been
        // acquired yet, because the producer that writes the resource
        // runs LATER in the same frame's step order. Acquiring here is
        // idempotent on existing bindings, so the first frame allocates
        // a slot; subsequent frames find the slot already bound from
        // last frame and carry the producer's prior-frame write into
        // the consumer's read.
        //
        // On a resource's FIRST-EVER acquisition by this executor we
        // also clear the underlying texture to opaque black, so
        // first-frame consumers don't read uninitialised pixels. Only
        // applies when a `GpuEncoder` is available — mock-backend code
        // paths (used by logic tests) skip this and rely on the test
        // primitive's tolerance for the mock's zero slots.
        // Canvas dims (resolved once per frame) used to concretize
        // `ExecutionPlan::resource_dims = None` (the "use canvas"
        // sentinel) before calling into the backend. Pulling it once
        // here keeps the per-step loop free of repeated trait calls.
        let canvas_dims = self.backend.canvas_dims();

        // Install the plan's mip-chained resource set BEFORE any acquire —
        // acquire/release consult it for pool keying (IMPORT_FIDELITY F-P6).
        self.backend.declare_mipmapped(plan.mipmapped_resources());

        for &res_id in plan.persistent_resources() {
            let ty = plan
                .resource_type(res_id)
                .expect("persistent resource type known from compile()");
            let fmt = plan.resource_format(res_id);
            let dims = resolve_dims(plan, res_id, canvas_dims);
            let slot = self.backend.acquire(res_id, ty, fmt, dims);
            if self.initialized_persistent.insert(res_id)
                && let Some(gpu) = gpu.as_deref_mut()
                && let Some(tex) = self.backend.texture_2d(slot)
            {
                gpu.clear_texture(tex, 0.0, 0.0, 0.0, 0.0);
            }
        }

        // Node-output-preview diagnostic accumulators. `matched` flips true if
        // the preview target named a live step this frame; `tex_count` records
        // how many Texture2D outputs that step had. Distinguishes the two
        // preview-black failure modes (no step matched = identity problem;
        // matched but black = resource recycled) in the post-loop log below.
        let mut preview_matched = false;
        let mut preview_tex_count = 0usize;

        for (idx, step) in plan.steps().iter().enumerate() {
            // Live wire-value tap (see `live_scalar_inputs` field doc):
            // snapshot this step's wired scalar inputs before any skip
            // branch below can `continue` past it. The step's own
            // declared inputs are always bound at this point, live-step,
            // memo-skipped, or mux-pruned alike.
            for &(port, res) in &step.inputs {
                if let Some(v) = self.read_scalar_resource(plan, res) {
                    self.live_scalar_inputs.push((step.node, port, v));
                }
            }

            if !self.live_steps[idx] {
                // Mux short-circuit: producer subgraph of an
                // unselected branch. Skip acquire / evaluate /
                // free_after entirely — slots stay bound from last
                // frame so re-selection picks up the prior state.
                continue;
            }

            // Memoized-dataflow skip (constant-subgraph hoisting): a PURE
            // step whose params and input resources are unchanged since its
            // last execute re-emits its held output slots without running.
            // Skipped exactly like the mux short-circuit above — no acquire,
            // no evaluate, no free_after — so consumers read the prior write.
            // Diagnostic modes force-dirty: attribution profiling wants real
            // per-step cost, and the preview path resolves its capture inside
            // the execute body. Dump mode does NOT force-dirty — a memoized
            // step's held output slot still holds the valid texture, so the
            // skip records it from that slot (below) instead of paying a
            // re-execute just to capture an unchanged thumbnail.
            let force_dirty = self.profile_force_all_live
                || self.profiling
                || self.preview_target == Some(step.node);
            self.wired_scratch.clear();
            for &(port_name, _) in &step.inputs {
                self.wired_scratch.push(port_name);
            }
            if !force_dirty
                && plan.step_hoistable(idx)
                && let Some(inst) = graph.get_node(step.node)
                && inst.node.skip_passthrough(&inst.params, &self.wired_scratch).is_none()
                && let Some(memo) = &self.step_memo[idx]
                && memo.param_epoch == inst.param_epoch
                && memo.input_epochs.len() == step.inputs.len()
                && step
                    .inputs
                    .iter()
                    .zip(&memo.input_epochs)
                    .all(|(&(_, res), &epoch)| {
                        self.resource_epoch.get(&res).copied().unwrap_or(0) == epoch
                    })
                && step
                    .outputs
                    .iter()
                    .all(|&(_, res)| self.backend.slot_for(res).is_some())
            {
                // The held output is unchanged but still valid — capture it for
                // the dump so a static subgraph keeps its zero-cost skip yet
                // shows a current thumbnail. Slots are guaranteed bound here:
                // the memo guard above required slot_for(res).is_some(). Safe
                // against the feedback-swap hazard because only PURE nodes reach
                // this skip (step_hoistable → is_pure), so the held slot is this
                // frame's content — a stateful/feedback node, whose held slot can
                // be the pre-swap buffer, never memo-skips.
                if self.should_dump(step.node) {
                    self.record_dump_outputs(plan, step);
                }
                continue;
            }

            // Data-driven skip (zero blobs / zero spawned particles): a step
            // that declared its data input ports skips when EVERY declared
            // port is wired and its resource was marked empty BOTH last frame
            // and this frame (the one-frame guard — the node executed the
            // first empty frame and wrote out its empty state, so the held
            // outputs consumers read are the empty content, never the last
            // non-empty frame's). Its outputs are marked empty too, so the
            // skip propagates through a declaring chain. Diagnostic modes
            // force-dirty, same as the memo skip above.
            let mut data_skip = false;
            if !force_dirty
                && let Some(inst) = graph.get_node(step.node)
            {
                let empty_ports = inst.node.empty_skip_input_ports();
                if !empty_ports.is_empty()
                    && empty_ports.iter().all(|p| {
                        step.inputs.iter().any(|&(name, res)| {
                            name == *p
                                && self.empty_resources.contains(&res)
                                && self.empty_resources_prev.contains(&res)
                        })
                    })
                {
                    // A node that composites onto a source texture can't
                    // just be skipped — its held output would be a STALE
                    // copy of the source, freezing the video underneath.
                    // When it declares `skip_passthrough_ports`, fall
                    // through to the evaluate section, which aliases the
                    // live input texture onto the output slot (zero GPU
                    // work) instead of evaluating. Pure data-shapers
                    // (no passthrough declaration) keep the zero-cost
                    // early skip: their held outputs already carry the
                    // empty content from the first empty frame.
                    if inst.node.skip_passthrough_ports().is_some() {
                        data_skip = true;
                    } else {
                        for &(_, res) in &step.outputs {
                            self.empty_resources.insert(res);
                        }
                        // Held outputs carry this node's empty state — still
                        // record them so the dump stays complete across the
                        // data-driven skip (matches the memo-skip above). Slots
                        // are bound here too: the two-frame empty guard (empty
                        // this frame AND last) means the node executed and wrote
                        // its outputs on the first empty frame before it could
                        // start skipping. No explicit slot-bound check is needed
                        // — if one were somehow unbound, record_dump_outputs
                        // reads None (a blank cell), never a panic.
                        if self.should_dump(step.node) {
                            self.record_dump_outputs(plan, step);
                        }
                        continue;
                    }
                }
            }

            // Attribution profiling: stamp the step tag onto the GPU encoder
            // so counter-sampled spans join back to this step, and start the
            // CPU encode clock. Both gated on `profiling` (off on the live
            // path).
            let prof_start = self.profiling.then(std::time::Instant::now);
            if self.profiling
                && let Some(g) = gpu.as_deref_mut()
            {
                g.native_enc
                    .set_profile_tag(&format!("{}:s{idx}", self.profile_scope));
            }

            // 1. Acquire output slots.
            self.output_scratch.clear();
            for &(port_name, res_id) in &step.outputs {
                let ty = plan
                    .resource_type(res_id)
                    .expect("resource type known from compile()");
                let fmt = plan.resource_format(res_id);
                let dims = resolve_dims(plan, res_id, canvas_dims);
                let slot = self.backend.acquire(res_id, ty, fmt, dims);
                self.output_scratch.push((port_name, slot));
            }

            // 2. Look up input slots. A wired input whose producer
            // step was pruned (mux short-circuit) has no slot bound
            // this frame — drop it from the input scratch so the
            // node's `NodeInputs` accessor returns `None`. Mux
            // primitives tolerate this via their port-shadows-param
            // fallback (selector resolves to a port whose `in_N` IS
            // bound); other nodes wouldn't legitimately end up with
            // a pruned input because the live-set walk only prunes
            // mux branches (the unselected `in_K`s on the mux's own
            // input list).
            self.input_scratch.clear();
            for &(port_name, res_id) in &step.inputs {
                if let Some(slot) = self.backend.slot_for(res_id) {
                    self.input_scratch.push((port_name, slot));
                }
            }

            // 3. Evaluate (or skip-passthrough alias). The context holds
            // an immutable backend ref for typed accessor resolution and
            // (optionally) a per-step mutable reborrow of the host's
            // GpuEncoder + StateStore. Scoped tightly so the borrows end
            // before the release loop's mutable borrow below.
            // Set when this step is hoistable and it executed (evaluate or
            // skip-alias) — the memo snapshot is recorded after the node
            // borrow ends. `None` leaves any prior memo cleared (non-
            // hoistable or missing node).
            let mut executed_pure_epoch: Option<u64> = None;
            if let Some(inst) = graph.get_node_mut(step.node) {
                if plan.step_hoistable(idx) {
                    executed_pure_epoch = Some(inst.param_epoch);
                }
                // Query skip-passthrough BEFORE building the full context.
                // If the node declares itself a no-op, alias the input
                // slot's texture onto the output slot — zero GPU work
                // — and skip evaluate. Matches the legacy chain
                // dispatch's "skip + don't swap" semantic without the
                // per-skip blit a naive fix would require.
                // A data-skipped draw node aliases unconditionally via its
                // STATIC port declaration (the live source flows through at
                // zero cost); otherwise the node's per-frame param-driven
                // declaration decides.
                let skip_alias = if data_skip {
                    inst.node.skip_passthrough_ports()
                } else {
                    self.wired_scratch.clear();
                    for &(port_name, _) in &step.inputs {
                        self.wired_scratch.push(port_name);
                    }
                    inst.node.skip_passthrough(&inst.params, &self.wired_scratch)
                };
                let mut performed_alias = false;
                if let Some((in_port, out_port)) = skip_alias {
                    let in_slot = self
                        .input_scratch
                        .iter()
                        .find(|(name, _)| *name == in_port)
                        .map(|(_, s)| *s);
                    let out_slot = self
                        .output_scratch
                        .iter()
                        .find(|(name, _)| *name == out_port)
                        .map(|(_, s)| *s);
                    // The alias makes downstream readers see the INPUT texture
                    // verbatim, so the dynamic (param-driven) path is only
                    // legal when the output slot would have matched it exactly
                    // — same dims, same format. A mismatch (mux resampling a
                    // 256×1 LUT up to canvas) falls through to evaluate, which
                    // performs the real resample. The data-skip path keeps its
                    // established declaration-only contract (draw atoms
                    // composite onto their source at identical shape).
                    let compatible = || {
                        if data_skip {
                            return true;
                        }
                        let res_of = |list: &[(&'static str, ResourceId)], port: &str| {
                            list.iter().find(|&&(n, _)| n == port).map(|&(_, r)| r)
                        };
                        let (Some(r_in), Some(r_out)) =
                            (res_of(&step.inputs, in_port), res_of(&step.outputs, out_port))
                        else {
                            return false;
                        };
                        resolve_dims(plan, r_in, canvas_dims) == resolve_dims(plan, r_out, canvas_dims)
                            && plan.resource_format(r_in) == plan.resource_format(r_out)
                    };
                    if let (Some(i), Some(o)) = (in_slot, out_slot)
                        && compatible()
                        && self.backend.alias_2d(i, o)
                    {
                        performed_alias = true;
                        // Propagate the empty mark through a data-skip alias
                        // so a chain of declaring draw nodes each skips.
                        if data_skip {
                            for &(_, res) in &step.outputs {
                                self.empty_resources.insert(res);
                            }
                        } else {
                            // RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3b/
                            // BUG-197: a param-driven (skip_passthrough)
                            // alias is a per-pixel identity onto a STABLE
                            // choice of input WIRE — when this frame's
                            // aliased-from RESOURCE (the compiled edge
                            // `skip_passthrough` selected — stable across
                            // frames unless the node's param-driven branch
                            // choice itself changes, e.g. a mux selector
                            // flip) matches last frame's, the destination
                            // SLOT matches last frame's (pool recycling can
                            // legitimately hand the same resource a
                            // DIFFERENT physical slot between frames — the
                            // `last_mip_identity` precedent this file's own
                            // comments cite elsewhere; the generation
                            // bookkeeping below is slot-indexed, so a slot
                            // reassignment invalidates it and must fall
                            // through to a conservative bump), AND the
                            // in-resource's write generation hasn't moved
                            // since, this step's output is provably
                            // unchanged, so declare it (propagating the
                            // input's generation through the alias instead
                            // of conservatively bumping). Fenced to
                            // `!data_skip` — the data-skip alias above keeps
                            // its established conservative bump.
                            let r_in = step
                                .inputs
                                .iter()
                                .find(|&&(n, _)| n == in_port)
                                .map(|&(_, r)| r);
                            let in_generation =
                                self.slot_generations.get(i.0 as usize).copied().unwrap_or(0);
                            let prev = self.alias_propagation_state[idx];
                            self.alias_propagation_state[idx] =
                                r_in.map(|r| (r, o, in_generation));
                            if let Some(r) = r_in
                                && prev == Some((r, o, in_generation))
                            {
                                self.node_declared_unchanged[idx] = true;
                            }
                        }
                    }
                }
                if !performed_alias || data_skip {
                    // Any step that didn't take the param-driven alias path
                    // this frame must not carry a stale prior-frame match
                    // forward into some future frame that does.
                    self.alias_propagation_state[idx] = None;
                }

                if !performed_alias {
                    self.scalar_write_scratch.clear();
                    self.camera_write_scratch.clear();
                    self.light_write_scratch.clear();
                    self.material_write_scratch.clear();
                    self.transform_write_scratch.clear();
                    self.atmosphere_write_scratch.clear();
                    self.object_write_scratch.clear();
                    self.error_scratch.clear();
                    {
                        let backend_ref: &dyn Backend = &*self.backend;
                        let inputs = NodeInputs::new(&self.input_scratch, backend_ref, &self.slot_generations);
                        let outputs = NodeOutputs::new(
                            &self.output_scratch,
                            backend_ref,
                            &mut self.scalar_write_scratch,
                            &mut self.camera_write_scratch,
                            &mut self.light_write_scratch,
                            &mut self.material_write_scratch,
                            &mut self.transform_write_scratch,
                            &mut self.atmosphere_write_scratch,
                            &mut self.object_write_scratch,
                        );
                        // Canvas dims are no longer hung off the
                        // context as a side-channel. Primitives that
                        // need them (`scatter_particles` and friends)
                        // declare `width`/`height` as required scalar
                        // input ports and the JSON preset wires them
                        // from `system.generator_input.output_width /
                        // output_height` — the value is visible in the
                        // graph editor and the chain validator catches
                        // missing wires at preset-load instead of at
                        // runtime via a sub-rect render bug.
                        let mut ctx = EffectNodeContext::with_state(
                            time,
                            &inst.params,
                            inputs,
                            outputs,
                            gpu.as_deref_mut(),
                            state.as_deref_mut(),
                            step.node,
                            owner_key,
                            self.rebuild_epoch,
                        )
                        .with_errors(&mut self.error_scratch);
                        let has_gpu_binding = ctx.gpu.is_some();
                        inst.node.evaluate(&mut ctx);
                        // Aliased-output contract: a primitive that
                        // declares `aliased_array_io = [(in, out)]`
                        // promises its dispatch writes to the aliased
                        // buffer. If it returned without touching the
                        // GPU at all (early-return path skipped the
                        // dispatch), downstream consumers of `out`
                        // read whatever was in the buffer last frame —
                        // stale data with no error signal. Debug
                        // builds panic loudly; release builds skip
                        // the check (per-frame cost stays off the hot
                        // path). The primitive surface uses either
                        // `ctx.gpu_encoder()` or
                        // `ctx.mark_gpu_accessed()` to flip the flag.
                        debug_assert!(
                            !(has_gpu_binding
                                && !ctx.gpu_accessed
                                && !inst.node.aliased_array_io().is_empty()),
                            "primitive `{}` declared aliased_array_io {:?} \
                             but its `evaluate` returned without accessing \
                             the GPU. Downstream consumers of the aliased \
                             output will read stale data. Fix: either drop \
                             the aliased_array_io declaration (the primitive \
                             isn't actually in-place mutating), or call \
                             `ctx.gpu_encoder()` / `ctx.mark_gpu_accessed()` \
                             on every code path through `evaluate` and \
                             ensure each one dispatches at least one \
                             compute pass through the encoder.",
                            inst.node.type_id().as_str(),
                            inst.node.aliased_array_io(),
                        );
                        // D5: record this step's declaration for the
                        // frame. `idx` indexes `plan.steps()`, which
                        // `node_declared_unchanged` is sized to match.
                        self.node_declared_unchanged[idx] = ctx.outputs_unchanged;
                    }
                    // Drain scalar writes back into the backend so
                    // downstream readers in the same frame see them via
                    // `NodeInputs::scalar`. Synchronous — control wires
                    // evaluate in topological order, so producers always
                    // precede consumers.
                    for (slot, value) in self.scalar_write_scratch.drain(..) {
                        self.backend.set_scalar(slot, value);
                    }
                    // Camera writes use the same drain shape.
                    for (slot, value) in self.camera_write_scratch.drain(..) {
                        self.backend.set_camera(slot, value);
                    }
                    // Light writes use the same drain shape.
                    for (slot, value) in self.light_write_scratch.drain(..) {
                        self.backend.set_light(slot, value);
                    }
                    // Material writes use the same drain shape.
                    for (slot, value) in self.material_write_scratch.drain(..) {
                        self.backend.set_material(slot, value);
                    }
                    // Transform writes use the same drain shape.
                    for (slot, value) in self.transform_write_scratch.drain(..) {
                        self.backend.set_transform(slot, value);
                    }
                    // Atmosphere writes use the same drain shape.
                    for (slot, value) in self.atmosphere_write_scratch.drain(..) {
                        self.backend.set_atmosphere(slot, value);
                    }
                    // Object writes use the same drain shape.
                    for (slot, value) in self.object_write_scratch.drain(..) {
                        self.backend.set_object(slot, value);
                    }
                    // Structured errors reported via `ctx.error(...)` —
                    // log once per occurrence. Primitives are expected
                    // to ALSO emit a deterministic fallback (e.g. magenta
                    // clear) alongside the error report, so downstream
                    // consumers don't read garbage.
                    for msg in self.error_scratch.drain(..) {
                        eprintln!(
                            "[graph error] node {:?} ({}): {msg}",
                            step.node,
                            inst.node.type_id().as_str(),
                        );
                    }
                    // Data-driven skip, reporter side: an evaluate that
                    // produced EMPTY output (zero blobs, zero spawned
                    // particles) marks its output resources so downstream
                    // `empty_skip_input_ports` declarers can skip. Queried
                    // only on real evaluates — an aliased passthrough never
                    // reports.
                    if inst.node.reports_empty_output() {
                        for &(_, res) in &step.outputs {
                            self.empty_resources.insert(res);
                        }
                    }
                }
            }

            // RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md D5: bump every output
            // slot's write generation — the SINGLE choke point for this
            // signal (same site `resource_epoch` bumps at, immediately
            // below) — UNLESS this step declared its outputs unchanged this
            // frame. A step that never calls `mark_outputs_unchanged` (every
            // node today except R1's gated sources) always lands in this
            // branch, so its consumers' cached generations always change —
            // provably never-stale by construction (I3's contract is the
            // node's side of this; a false declaration is the only way this
            // could go wrong, and that's per-node-tested, not this site's
            // job). Now a param-driven alias
            // (`performed_alias && !data_skip`, e.g. `mux_texture`'s
            // inline-selector fast path) CAN set it — fenced to that exact
            // case just above, where `alias_propagation_state[idx]` proves
            // this frame's (in_slot, out_slot) pair and the in_slot's write
            // generation both match last frame's. The data-driven
            // (`data_skip`) passthrough alias is unchanged and still always
            // conservatively bumps — its aliased identity can flip between
            // different pruned producers frame to frame with no generation
            // signal to trust.
            if !self.node_declared_unchanged[idx] {
                for &(_, res) in &step.outputs {
                    if let Some(slot) = self.backend.slot_for(res) {
                        let slot_idx = slot.0 as usize;
                        if self.slot_generations.len() <= slot_idx {
                            self.slot_generations.resize(slot_idx + 1, 0);
                        }
                        self.slot_generations[slot_idx] += 1;
                    }
                }
            }

            // Memoized-dataflow bookkeeping: this step executed, so every
            // output resource is new content — bump its epoch so consumers'
            // memos see the change. Pure steps then snapshot the epochs they
            // ran with (the clean-skip compares against this next frame);
            // non-pure steps clear any stale memo. The input-epoch Vec only
            // allocates on dirty executes of pure steps — never on the
            // steady-state (clean) path.
            for &(_, res) in &step.outputs {
                *self.resource_epoch.entry(res).or_insert(0) += 1;
            }
            self.step_memo[idx] = executed_pure_epoch.map(|param_epoch| StepMemo {
                param_epoch,
                input_epochs: step
                    .inputs
                    .iter()
                    .map(|&(_, res)| self.resource_epoch.get(&res).copied().unwrap_or(0))
                    .collect(),
            });

            // Attribution profiling: close the step's CPU encode clock.
            if let Some(t0) = prof_start {
                let type_id = graph
                    .get_node(step.node)
                    .map(|i| i.node.type_id().as_str().to_string())
                    .unwrap_or_default();
                self.step_profiles.push(StepProfile {
                    step_idx: idx,
                    node: step.node,
                    type_id,
                    cpu_nanos: u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    tag: format!("{}:s{idx}", self.profile_scope),
                });
            }

            // Preview capture: if this is the node being previewed, remember
            // its first Texture2D output so the release loop below keeps that
            // slot bound past the frame. The integration layer reads it after
            // `execute_frame_*` and downscales it into the preview surface.
            if self.preview_target == Some(step.node) {
                preview_matched = true;
                preview_tex_count = 0;
                let mut first_texture: Option<ResourceId> = None;
                for &(_, res) in &step.outputs {
                    if plan.resource_type(res).is_some_and(|t| t.is_texture_2d()) {
                        if first_texture.is_none() {
                            first_texture = Some(res);
                        }
                        preview_tex_count += 1;
                    }
                }
                self.preview_resource = first_texture;
                // When the node has no image, capture its live scalar I/O so the
                // editor can show a value inspector instead of a black pane.
                // Skip the scalar read on image nodes — that pane shows the
                // texture, not numbers.
                if first_texture.is_none() {
                    for &(port, res) in &step.inputs {
                        if let Some(v) = self.read_scalar_resource(plan, res) {
                            self.preview_scalar_inputs.push((port.to_string(), v));
                        }
                    }
                    for &(port, res) in &step.outputs {
                        if let Some(v) = self.read_scalar_resource(plan, res) {
                            self.preview_scalar_outputs.push((port.to_string(), v));
                        }
                    }
                }
            }

            // Dump capture: record this step's Texture2D/Array outputs if it's
            // in the dump scope (Cmd+D everything, or the atlas's visible set).
            // The identity is pinned NOW, before the end-of-frame feedback swap
            // rebinds slots — see record_dump_outputs / dump_resources.
            if self.should_dump(step.node) {
                self.record_dump_outputs(plan, step);
            }

            // 4. Release dead resources. `dims` must match the
            // acquire-time value so the slot returns to the correct
            // (PortType, format, dims) bucket. The preview-captured resource
            // is held back so its texture survives for a post-frame read; it
            // returns to the pool next frame (re-resolved at the top).
            for &res_id in &step.free_after {
                // A recorded dump output is held past the frame so the host can
                // read it before its slot is reacquired and overwritten; the
                // preview-captured resource the same. Everything else — hidden
                // nodes' outputs under the atlas, and all non-dumped resources —
                // recycles through the pool as normal. This is sub-change B: the
                // atlas pins only what it shows, not the whole graph.
                if self.dump_pinned_resources.contains(&res_id)
                    || self.preview_resource == Some(res_id)
                {
                    continue;
                }
                // Held (memo-latched) resources never appear in `free_after`
                // — excluded at plan compile time, see
                // ExecutionPlan::held_resources — so no exemption is needed.
                let ty = plan
                    .resource_type(res_id)
                    .expect("resource type known from compile()");
                let fmt = plan.resource_format(res_id);
                let dims = resolve_dims(plan, res_id, canvas_dims);
                self.backend.release(res_id, ty, fmt, dims);
            }
        }

        // Node-output-preview diagnostic. Fires once per retarget (deduped) when
        // a preview is active, so the terminal reveals which failure mode a
        // black preview is: `matched=false` means the target id named no live
        // step (an identity problem — the node is a group container or a
        // pruned/multi-pass node whose previewable id differs); `matched=true`
        // with `resource=None` means the step ran but had no Texture2D output;
        // `matched=true` with a resource that still reads black points at
        // resource recycling. Grep `[preview]`.
        if self.preview_target.is_some() {
            let key = (
                self.preview_target,
                preview_matched,
                preview_tex_count,
                self.preview_resource,
            );
            if self.preview_debug_last != Some(key) {
                self.preview_debug_last = Some(key);
                eprintln!(
                    "[preview] target={:?} matched_live_step={} texture2d_outputs={} \
                     captured_resource={:?}",
                    self.preview_target, preview_matched, preview_tex_count, self.preview_resource,
                );
            }
        } else if self.preview_debug_last.is_some() {
            self.preview_debug_last = None;
        }

        // ===== Late-capture pass =====
        //
        // Runs AFTER every node's `evaluate` for the frame has been
        // encoded. At this point the producer feeding any state-capture
        // input port has already written THIS frame's output into the
        // persistent back-edge slot — `late_capture` reads that fresh
        // value and snapshots it into the node's StateStore entry, so
        // next frame's `evaluate` emits a true 1-frame-delayed value
        // (matching ping-pong + end-of-frame swap).
        //
        // Doing the capture here instead of inside `evaluate` is the
        // structural fix for the 2-frame-delay bug class that produced
        // the OilyFluid per-frame flicker: state-capture nodes run
        // FIRST in topo, so an in-`evaluate` capture would read the
        // PREVIOUS frame's producer output, decoupling the simulation
        // into independent even/odd streams driven by per-frame noise.
        // No new primitive that declares `state_capture_input_ports`
        // can recreate that bug as long as it uses `late_capture` for
        // its snapshot.
        //
        // Output slots may have been freed by `step.free_after` above —
        // we deliberately build the context with an EMPTY output
        // scratch. `late_capture` implementations must read only inputs
        // and write to state, never to outputs.
        for &step_idx in plan.late_capture_step_indices() {
            if !self.live_steps[step_idx] {
                continue;
            }
            let step = &plan.steps()[step_idx];
            // Attribution profiling: late-capture GPU work (a feedback node's
            // state-snapshot blit) belongs to ITS node's row, not whichever
            // step happened to set the tag last (final_output — the
            // "final_output burns 2-3 dispatches" red herring).
            if self.profiling
                && let Some(g) = gpu.as_deref_mut()
            {
                g.native_enc
                    .set_profile_tag(&format!("{}:s{step_idx}", self.profile_scope));
            }
            // Re-resolve input slot bindings. State-capture inputs are
            // backed by persistent resources whose slots stay bound
            // across the frame, so the same slot the main pass saw is
            // still live and now holds the producer's frame-N write.
            self.input_scratch.clear();
            for &(port_name, res_id) in &step.inputs {
                if let Some(slot) = self.backend.slot_for(res_id) {
                    self.input_scratch.push((port_name, slot));
                }
            }
            // Output scratch carries ONLY this node's PERSISTENT outputs —
            // those slots are never pool-released, so a late_capture write
            // (feedback's direct state landing: swap for same-format,
            // cross-format bridge otherwise) can't corrupt a recycled
            // slot. Pooled outputs stay unbound: any erroneous write
            // attempt resolves to `None` exactly as before.
            self.output_scratch.clear();
            for &(port_name, res_id) in &step.outputs {
                if plan.persistent_resources().contains(&res_id)
                    && let Some(slot) = self.backend.slot_for(res_id)
                {
                    self.output_scratch.push((port_name, slot));
                }
            }

            if let Some(inst) = graph.get_node_mut(step.node) {
                self.scalar_write_scratch.clear();
                self.camera_write_scratch.clear();
                self.light_write_scratch.clear();
                self.material_write_scratch.clear();
                self.transform_write_scratch.clear();
                self.atmosphere_write_scratch.clear();
                self.object_write_scratch.clear();
                self.error_scratch.clear();
                let backend_ref: &dyn Backend = &*self.backend;
                let inputs = NodeInputs::new(&self.input_scratch, backend_ref, &self.slot_generations);
                let outputs = NodeOutputs::new(
                    &self.output_scratch,
                    backend_ref,
                    &mut self.scalar_write_scratch,
                    &mut self.camera_write_scratch,
                    &mut self.light_write_scratch,
                    &mut self.material_write_scratch,
                    &mut self.transform_write_scratch,
                    &mut self.atmosphere_write_scratch,
                    &mut self.object_write_scratch,
                );
                let mut ctx = EffectNodeContext::with_state(
                    time,
                    &inst.params,
                    inputs,
                    outputs,
                    gpu.as_deref_mut(),
                    state.as_deref_mut(),
                    step.node,
                    owner_key,
                    self.rebuild_epoch,
                )
                .with_errors(&mut self.error_scratch);
                inst.node.late_capture(&mut ctx);
                let swap_request = ctx.texture_swap_request.take();
                for msg in self.error_scratch.drain(..) {
                    eprintln!(
                        "[graph error] node {:?} ({}) late_capture: {msg}",
                        step.node,
                        inst.node.type_id().as_str(),
                    );
                }
                // Zero-copy feedback ping-pong: perform a requested
                // texture swap between one of this node's output slots
                // and one of its input slots (both persistent). The
                // node verified eligibility (matching dims + format)
                // before requesting; a failed swap here (slot missing /
                // borrowed shadow) is loud because silently dropping it
                // would freeze the feedback loop on one frame.
                if let Some((out_port, in_port)) = swap_request {
                    let out_slot = step
                        .outputs
                        .iter()
                        .find(|(p, _)| *p == out_port)
                        .and_then(|&(_, res)| self.backend.slot_for(res));
                    let in_slot = step
                        .inputs
                        .iter()
                        .find(|(p, _)| *p == in_port)
                        .and_then(|&(_, res)| self.backend.slot_for(res));
                    let swapped = match (out_slot, in_slot) {
                        (Some(a), Some(b)) => self.backend.swap_texture_2d(a, b),
                        _ => false,
                    };
                    // BUG-216: the swap refuses whenever `in_slot` (or
                    // `out_slot`) carries a borrowed shadow — the common
                    // shape is a boundary output (`system.final_output`)
                    // pre-binding the SAME resource a feedback loop wires
                    // into its `in` port (mix → final_output AND mix →
                    // feedback.in share one ResourceId/slot). Swapping
                    // there would change final_output's physical texture
                    // identity mid-frame, which is exactly what the
                    // refusal protects against — but the loop's state
                    // still needs to land somewhere. Fall back to a
                    // format-bridge COPY (`node.feedback`'s own
                    // `Feedback::copy_with_format_bridge`, `temporal.rs`,
                    // is the same blit-or-resize contract): copy `in`'s
                    // CONTENT (this frame's fresh producer write) into
                    // `out`'s persistent texture — `in`'s physical
                    // identity is untouched (final_output keeps pointing
                    // at the same texture), but next frame's `run()`
                    // reads `out` and now sees this frame's trail. One
                    // dispatch, same as the dims-mismatch mode already
                    // proven there.
                    if !swapped {
                        let landed = match (out_slot, in_slot, gpu.as_deref_mut()) {
                            (Some(out_s), Some(in_s), Some(g)) => {
                                match (self.backend.texture_2d(in_s), self.backend.texture_2d(out_s)) {
                                    (Some(src), Some(dst)) if src.format == dst.format => {
                                        if src.width == dst.width && src.height == dst.height {
                                            g.copy_texture_to_texture(src, dst, dst.width, dst.height);
                                        } else {
                                            g.resize_sample(src, dst);
                                        }
                                        true
                                    }
                                    _ => false,
                                }
                            }
                            _ => false,
                        };
                        if !landed {
                            eprintln!(
                                "[graph error] node {:?} ({}): texture swap \
                                 {out_port}<->{in_port} failed AND no copy \
                                 fallback was possible (missing texture or \
                                 format mismatch) — feedback state did NOT \
                                 advance this frame",
                                step.node,
                                inst.node.type_id().as_str(),
                            );
                        }
                    }
                }
            }
        }
    }
}

impl Default for Executor {
    fn default() -> Self {
        Self::with_mock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use manifold_core::{Beats, Seconds};

    use crate::node_graph::EffectNode;
    use crate::node_graph::compile;
    use crate::node_graph::effect_node::EffectNodeType;
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{
        NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType,
    };

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn input(name: &'static str, ty: PortType, required: bool) -> NodeInput {
        NodePort {
            name: std::borrow::Cow::Borrowed(name),
            ty,
            kind: PortKind::Input,
            required,
        }
    }

    fn output(name: &'static str, ty: PortType) -> NodeOutput {
        NodePort {
            name: std::borrow::Cow::Borrowed(name),
            ty,
            kind: PortKind::Output,
            required: false,
        }
    }

    /// Misbehaving test node: declares `aliased_array_io` claiming
    /// in-place mutation but its `evaluate` returns without touching
    /// the GPU. Exercises the debug-build aliased-output assertion
    /// in the executor — without it, downstream consumers of the
    /// aliased output would silently read stale data.
    struct SilentAliasedNode {
        type_id: EffectNodeType,
        outputs: Vec<NodeOutput>,
    }

    impl SilentAliasedNode {
        fn new(particle_layout: crate::node_graph::ports::ArrayType) -> Self {
            Self {
                type_id: EffectNodeType::new("test.silent_aliased"),
                outputs: vec![output("out", PortType::Array(particle_layout))],
            }
        }
    }

    impl EffectNode for SilentAliasedNode {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &[]
        }
        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn aliased_array_io(&self) -> &'static [(&'static str, &'static str)] {
            // Asserts a self-loop alias even though `in` isn't an
            // input port. The runtime check fires on the contract
            // ("if you declare aliased_array_io, you must dispatch"),
            // not on whether the declared ports exist.
            &[("in", "out")]
        }
        fn array_output_capacity(
            &self,
            _port: &str,
            _params: &crate::node_graph::effect_node::ParamValues,
            _input_capacities: &[(&str, u32)],
        ) -> Option<u32> {
            Some(16)
        }
        fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {
            // Deliberately silent — no `gpu_encoder()` call, no
            // `mark_gpu_accessed()`, no dispatch. The debug_assert
            // should fire.
        }
    }

    /// Debug-build aliased-output contract: a primitive that declares
    /// `aliased_array_io` MUST access the GPU during `evaluate`,
    /// otherwise the aliased output never gets written and downstream
    /// reads stale data. Release builds skip the check; debug catches
    /// the contract violation.
    #[test]
    #[should_panic(expected = "aliased_array_io")]
    #[cfg(debug_assertions)]
    fn aliased_output_assertion_fires_on_silent_primitive() {
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::node_graph::MetalBackend;
        use crate::node_graph::ports::ArrayType;
        use manifold_gpu::{GpuDevice, GpuTextureFormat};

        let device = std::sync::Arc::new(GpuDevice::new());
        let particle_layout = ArrayType::of_known::<crate::generators::compute_common::Particle>();

        let mut g = Graph::new();
        g.add_node(Box::new(SilentAliasedNode::new(particle_layout)));
        let plan = compile(&g).expect("trivial graph compiles");

        let backend = MetalBackend::new(std::sync::Arc::clone(&device), 256, 256, GpuTextureFormat::Rgba16Float);
        let mut exec = Executor::new(Box::new(backend));
        let mut native_enc = device.create_encoder("aliased-contract-test");
        let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
        // Should panic inside the executor's debug_assert! after the
        // node's `evaluate` returns without touching the GPU.
        exec.execute_frame_with_gpu(&mut g, &plan, frame_time(), &mut gpu);
    }

    /// Test EffectNode that records each evaluation's bindings into a shared log.
    struct RecordingNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        log: Arc<Mutex<Vec<EvaluationRecord>>>,
        /// Optional branch-selector hint — when set, the node returns
        /// it from `selected_input_branch` so the executor's live-set
        /// walk treats only that input port as live. Interior-mutable
        /// (Arc<Mutex<…>>) so frame-to-frame selector-flip tests can
        /// mutate the hint without going through `get_node_mut` and
        /// downcast gymnastics — mirrors the production path where
        /// `mux_texture`'s selected_input_branch reads from
        /// `inst.params` (which IS mutable through the graph's
        /// `set_param`, but RecordingNode doesn't have params so we
        /// model the same write-then-rebuild behaviour via a shared
        /// handle the test holds onto).
        selected_branch: Arc<Mutex<Option<&'static str>>>,
        /// Optional list of state-capture input port names. Mirrors
        /// the `EffectNode::state_capture_input_ports` declaration on
        /// real stateful primitives (`node.feedback`, `node.array_feedback`).
        /// `&'static [&'static str]` so the trait can return it
        /// directly; tests pass leaked slices.
        state_capture_ports: &'static [&'static str],
    }

    #[derive(Debug, Clone, PartialEq)]
    struct EvaluationRecord {
        type_name: String,
        inputs: Vec<(&'static str, Slot)>,
        outputs: Vec<(&'static str, Slot)>,
    }

    impl RecordingNode {
        fn new(
            name: &'static str,
            inputs: Vec<NodeInput>,
            outputs: Vec<NodeOutput>,
            log: Arc<Mutex<Vec<EvaluationRecord>>>,
        ) -> Self {
            Self {
                type_id: EffectNodeType::new(name),
                inputs,
                outputs,
                log,
                selected_branch: Arc::new(Mutex::new(None)),
                state_capture_ports: &[],
            }
        }

        /// Mark a port as state-capture for executor tests that need
        /// to exercise the back-edge propagation path. Mirrors what
        /// `node.feedback` declares for its `in` port.
        fn with_state_capture_ports(mut self, ports: &'static [&'static str]) -> Self {
            self.state_capture_ports = ports;
            self
        }

        /// Make this node act as a branch-selector for executor
        /// live-set tests. Returns the shared `Arc<Mutex<Option<&str>>>`
        /// handle so the test can later flip the selection between
        /// frames to exercise the per-frame live-set rebuild.
        fn with_selected_branch(
            mut self,
            port: Option<&'static str>,
        ) -> (Self, Arc<Mutex<Option<&'static str>>>) {
            let handle = Arc::new(Mutex::new(port));
            self.selected_branch = handle.clone();
            (self, handle)
        }
    }

    impl EffectNode for RecordingNode {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &self.inputs
        }
        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            let inputs: Vec<_> = ctx.inputs.iter().collect();
            let outputs: Vec<_> = ctx.outputs.iter().collect();
            self.log.lock().unwrap().push(EvaluationRecord {
                type_name: self.type_id.as_str().to_string(),
                inputs,
                outputs,
            });
        }
        fn selected_input_branch(
            &self,
            _params: &crate::node_graph::effect_node::ParamValues,
            _wired_inputs: &[&str],
        ) -> Option<&'static str> {
            *self.selected_branch.lock().unwrap()
        }
        fn state_capture_input_ports(&self) -> &'static [&'static str] {
            self.state_capture_ports
        }
    }

    #[test]
    fn linear_chain_uses_only_two_slots_via_ping_pong() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let a = g.add_node(Box::new(RecordingNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let b = g.add_node(Box::new(RecordingNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let c = g.add_node(Box::new(RecordingNode::new(
            "c",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let d = g.add_node(Box::new(RecordingNode::new(
            "d",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
            log.clone(),
        )));
        g.connect((a, "out"), (b, "in")).unwrap();
        g.connect((b, "out"), (c, "in")).unwrap();
        g.connect((c, "out"), (d, "in")).unwrap();

        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());

        assert_eq!(
            exec.backend().slot_count(),
            2,
            "linear chain should ping-pong between 2 physical slots"
        );

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 4);
        let names: Vec<_> = log.iter().map(|r| r.type_name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn evaluate_sees_correct_input_and_output_bindings() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let a = g.add_node(Box::new(RecordingNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let b = g.add_node(Box::new(RecordingNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        g.connect((a, "out"), (b, "in")).unwrap();

        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());

        let log = log.lock().unwrap();
        let a_eval = &log[0];
        let b_eval = &log[1];
        let a_out_slot = a_eval.outputs[0].1;
        let b_in_slot = b_eval.inputs[0].1;
        assert_eq!(a_out_slot, b_in_slot);
    }

    #[test]
    fn preview_target_records_upstream_texture_output() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let a = g.add_node(Box::new(RecordingNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let b = g.add_node(Box::new(RecordingNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        g.connect((a, "out"), (b, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();

        // No target → nothing captured.
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(exec.preview_resource(), None);

        // Target the upstream node: its Texture2D output is recorded (and
        // held back from recycling) even though `b` is its last reader.
        exec.set_preview_target(Some(a));
        exec.execute_frame(&mut g, &plan, frame_time());
        assert!(
            exec.preview_resource().is_some(),
            "upstream texture output should be captured for preview"
        );

        // Clearing the target stops capture next frame.
        exec.set_preview_target(None);
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(exec.preview_resource(), None);
    }

    #[test]
    fn dump_all_records_every_texture_output() {
        // a → b → c. `a` and `b` have downstream consumers (so their outputs
        // get resources); `c` is the dangling sink (no resource, like a graph
        // with no final_output). Dump should record `a` and `b`.
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let a = g.add_node(Box::new(RecordingNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let b = g.add_node(Box::new(RecordingNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let c = g.add_node(Box::new(RecordingNode::new(
            "c",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        g.connect((a, "out"), (b, "in")).unwrap();
        g.connect((b, "out"), (c, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();

        // Off by default.
        exec.execute_frame(&mut g, &plan, frame_time());
        assert!(exec.dump_resources().is_empty());

        // On: every consumed Texture2D output is recorded.
        exec.set_dump_all(true);
        exec.execute_frame(&mut g, &plan, frame_time());
        let nodes: Vec<_> = exec.dump_resources().iter().map(|(n, _, _, _)| *n).collect();
        assert!(nodes.contains(&a), "a's output recorded");
        assert!(nodes.contains(&b), "b's output recorded");

        // Off again clears it next frame.
        exec.set_dump_all(false);
        exec.execute_frame(&mut g, &plan, frame_time());
        assert!(exec.dump_resources().is_empty());
    }

    /// Dump mode records a memoized node's output WITHOUT re-running it: a pure
    /// producer executes once, then on the next dump frame its held texture is
    /// captured from the slot rather than recomputed. This is the editor-atlas
    /// win — opening the graph editor must not force every static node to
    /// re-render 60×/s just to fill a thumbnail.
    #[test]
    fn dump_records_memoized_node_without_reexecuting() {
        let evals = Arc::new(Mutex::new(0));
        let mut g = Graph::new();
        // Pure producer → consumer, so the producer's output gets a resource
        // (a dangling output gets none and never enters the dump).
        let producer = g.add_node(Box::new(PureCountingNode::new(true, evals.clone())));
        let consumer =
            g.add_node(Box::new(PureCountingNode::with_input(true, Arc::new(Mutex::new(0)))));
        g.connect((producer, "out"), (consumer, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();

        exec.set_dump_all(true);

        // Frame 1: producer executes and is recorded.
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(*evals.lock().unwrap(), 1);
        assert!(
            exec.dump_resources().iter().any(|(n, _, _, _)| *n == producer),
            "producer recorded on its executing frame"
        );

        // Frame 2: producer is clean → memo-skips (no re-execute) but is STILL
        // recorded from its held output slot.
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(
            *evals.lock().unwrap(),
            1,
            "memoized node must not re-execute just to fill the dump"
        );
        assert!(
            exec.dump_resources().iter().any(|(n, _, _, _)| *n == producer),
            "memoized producer still recorded from its held slot"
        );
    }

    /// The atlas dump_set records ONLY the listed nodes. A hidden / off-scope
    /// node (here `b`) is skipped entirely — the editor captures only what the
    /// canvas can show. This is sub-change A: visible-set scoping.
    #[test]
    fn dump_set_records_only_listed_nodes() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let a = g.add_node(Box::new(RecordingNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let b = g.add_node(Box::new(RecordingNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let c = g.add_node(Box::new(RecordingNode::new(
            "c",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        g.connect((a, "out"), (b, "in")).unwrap();
        g.connect((b, "out"), (c, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();

        // Only `a` is "visible" on the canvas.
        exec.set_dump_set(Some([a].into_iter().collect()));
        exec.execute_frame(&mut g, &plan, frame_time());
        let nodes: Vec<_> = exec.dump_resources().iter().map(|(n, _, _, _)| *n).collect();
        assert!(nodes.contains(&a), "listed node recorded");
        assert!(!nodes.contains(&b), "unlisted (hidden) node NOT recorded");

        // Clearing the set turns the atlas dump off entirely.
        exec.set_dump_set(None);
        exec.execute_frame(&mut g, &plan, frame_time());
        assert!(exec.dump_resources().is_empty(), "no dump set, no records");
    }

    #[test]
    fn preview_target_with_no_texture_output_captures_nothing() {
        use crate::node_graph::ports::ScalarType;
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let s = g.add_node(Box::new(RecordingNode::new(
            "scalar_src",
            vec![],
            vec![output("v", PortType::Scalar(ScalarType::F32))],
            log.clone(),
        )));
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.set_preview_target(Some(s));
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(
            exec.preview_resource(),
            None,
            "a node with only a scalar output is not previewable"
        );
    }

    #[test]
    fn diamond_uses_three_slots() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let a = g.add_node(Box::new(RecordingNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let b = g.add_node(Box::new(RecordingNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let c = g.add_node(Box::new(RecordingNode::new(
            "c",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let d = g.add_node(Box::new(RecordingNode::new(
            "d",
            vec![
                input("a", PortType::Texture2D, true),
                input("b", PortType::Texture2D, true),
            ],
            vec![],
            log.clone(),
        )));
        g.connect((a, "out"), (b, "in")).unwrap();
        g.connect((a, "out"), (c, "in")).unwrap();
        g.connect((b, "out"), (d, "a")).unwrap();
        g.connect((c, "out"), (d, "b")).unwrap();

        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(exec.backend().slot_count(), 3);
    }

    #[test]
    fn slot_count_is_stable_across_frames() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let a = g.add_node(Box::new(RecordingNode::new(
            "a",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let b = g.add_node(Box::new(RecordingNode::new(
            "b",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
            log.clone(),
        )));
        g.connect((a, "out"), (b, "in")).unwrap();

        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        for _ in 0..10 {
            exec.execute_frame(&mut g, &plan, frame_time());
        }
        assert_eq!(exec.backend().slot_count(), 1);
    }

    #[test]
    fn texture_2d_and_texture_3d_use_separate_slot_pools() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let mixed = g.add_node(Box::new(RecordingNode::new(
            "mixed",
            vec![],
            vec![
                output("color", PortType::Texture2D),
                output("volume", PortType::Texture3D),
            ],
            log.clone(),
        )));
        // Sinks: per d84ae560, an output without a downstream consumer
        // is never allocated, so each output needs at least one reader
        // to force slot allocation.
        let sink2d = g.add_node(Box::new(RecordingNode::new(
            "sink2d",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
            log.clone(),
        )));
        let sink3d = g.add_node(Box::new(RecordingNode::new(
            "sink3d",
            vec![input("in", PortType::Texture3D, true)],
            vec![],
            log.clone(),
        )));
        g.connect((mixed, "color"), (sink2d, "in")).unwrap();
        g.connect((mixed, "volume"), (sink3d, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(exec.backend().slot_count(), 2);
    }

    #[test]
    fn scalar_inputs_and_textures_are_pooled_separately() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let mix = g.add_node(Box::new(RecordingNode::new(
            "mix",
            vec![],
            vec![
                output("tex", PortType::Texture2D),
                output("k", PortType::Scalar(ScalarType::F32)),
            ],
            log.clone(),
        )));
        // Sinks force slot allocation for each output (see above).
        let sink_tex = g.add_node(Box::new(RecordingNode::new(
            "sink_tex",
            vec![input("in", PortType::Texture2D, true)],
            vec![],
            log.clone(),
        )));
        let sink_scalar = g.add_node(Box::new(RecordingNode::new(
            "sink_scalar",
            vec![input("in", PortType::Scalar(ScalarType::F32), true)],
            vec![],
            log.clone(),
        )));
        g.connect((mix, "tex"), (sink_tex, "in")).unwrap();
        g.connect((mix, "k"), (sink_scalar, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(exec.backend().slot_count(), 2);
    }

    // --- NodeRequires entry-point validation -----------------------

    /// Test node that declares a `state_store` requirement.
    struct NeedsStateNode {
        type_id: EffectNodeType,
        outputs: Vec<NodeOutput>,
    }

    impl NeedsStateNode {
        fn new() -> Self {
            Self {
                type_id: EffectNodeType::new("needs_state"),
                outputs: vec![output("out", PortType::Texture2D)],
            }
        }
    }

    impl EffectNode for NeedsStateNode {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &[]
        }
        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
        fn requires(&self) -> crate::node_graph::effect_node::NodeRequires {
            crate::node_graph::effect_node::NodeRequires {
                state_store: true,
                gpu_encoder: false,
            }
        }
    }

    #[test]
    #[should_panic(expected = "require a StateStore")]
    fn execute_frame_panics_on_state_requiring_node() {
        let mut g = Graph::new();
        g.add_node(Box::new(NeedsStateNode::new()));
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
    }

    #[test]
    fn plan_requires_reflects_node_declaration() {
        let mut g = Graph::new();
        g.add_node(Box::new(NeedsStateNode::new()));
        let plan = compile(&g).unwrap();
        assert!(plan.requires().state_store);
        assert!(!plan.requires().gpu_encoder);
    }

    #[test]
    fn plan_requires_default_for_stateless_graph() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        g.add_node(Box::new(RecordingNode::new(
            "stateless",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log,
        )));
        let plan = compile(&g).unwrap();
        assert!(!plan.requires().state_store);
        assert!(!plan.requires().gpu_encoder);
    }

    // --- Mux short-circuit / live-set propagation ------------------
    //
    // Switch-statement semantics for `EffectNode::selected_input_branch`:
    // only the chosen branch's producer chain evaluates each frame.
    // These tests use FinalOutput as the live-set seed (the real
    // production trigger) and a `selected_branch`-configured
    // RecordingNode as a stand-in for `node.switch_texture`, so the
    // tests stay isolated from the mux's WGSL dispatch path. The
    // mux's own selector → port-name resolution is covered in
    // primitives/mux_texture.rs.

    use crate::node_graph::FinalOutput;

    /// Build `[prod_A → mux.in_0, prod_B → mux.in_1, prod_C → mux.in_2]
    /// → FinalOutput`, mark mux as selecting `selected`, and return
    /// the graph plus the shared selector handle (for tests that
    /// flip the selection between frames) and the evaluation log.
    #[allow(clippy::type_complexity)]
    fn build_three_branch_mux_graph(
        selected: Option<&'static str>,
    ) -> (
        Graph,
        Arc<Mutex<Option<&'static str>>>,
        Arc<Mutex<Vec<EvaluationRecord>>>,
    ) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();

        let prod_a = g.add_node(Box::new(RecordingNode::new(
            "prod_a",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let prod_b = g.add_node(Box::new(RecordingNode::new(
            "prod_b",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let prod_c = g.add_node(Box::new(RecordingNode::new(
            "prod_c",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let (mux_node, selector_handle) = RecordingNode::new(
            "mux",
            vec![
                input("in_0", PortType::Texture2D, false),
                input("in_1", PortType::Texture2D, false),
                input("in_2", PortType::Texture2D, false),
            ],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )
        .with_selected_branch(selected);
        let mux = g.add_node(Box::new(mux_node));
        let fout = g.add_node(Box::new(FinalOutput::new()));

        g.connect((prod_a, "out"), (mux, "in_0")).unwrap();
        g.connect((prod_b, "out"), (mux, "in_1")).unwrap();
        g.connect((prod_c, "out"), (mux, "in_2")).unwrap();
        g.connect((mux, "out"), (fout, "in")).unwrap();

        (g, selector_handle, log)
    }

    #[test]
    fn selected_branch_prunes_unselected_producers() {
        let (mut g, _sel, log) = build_three_branch_mux_graph(Some("in_1"));
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());

        let names: Vec<String> = log
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.type_name.clone())
            .collect();
        assert!(
            names.contains(&"prod_b".to_string()),
            "selected branch's producer must run, got: {names:?}",
        );
        assert!(
            names.contains(&"mux".to_string()),
            "mux itself must run, got: {names:?}",
        );
        assert!(
            !names.contains(&"prod_a".to_string()),
            "unselected branch (in_0) must NOT run, got: {names:?}",
        );
        assert!(
            !names.contains(&"prod_c".to_string()),
            "unselected branch (in_2) must NOT run, got: {names:?}",
        );
    }

    #[test]
    fn selected_branch_none_keeps_all_producers_live() {
        // `selected_branch: None` mirrors the production fallback —
        // mux returns None from `selected_input_branch` (e.g. when
        // its selector port is wired to a runtime-computed value).
        // Every input's producer must run since we can't predict
        // which one the selector will resolve to.
        let (mut g, _sel, log) = build_three_branch_mux_graph(None);
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());

        let names: Vec<String> = log
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.type_name.clone())
            .collect();
        for required in ["prod_a", "prod_b", "prod_c", "mux"] {
            assert!(
                names.contains(&required.to_string()),
                "fallback path must run every branch; missing `{required}` in {names:?}",
            );
        }
    }

    #[test]
    fn switching_selected_branch_across_frames_flips_live_set() {
        // Wire perform-mode flow: a mux's selector slides between
        // values across frames. Each frame's live set must reflect
        // THAT frame's selection — the previous frame's selection
        // shouldn't leak into the next.
        //
        // We mutate the shared selector handle directly (interior
        // mutability via Arc<Mutex>). In production the equivalent
        // path is `set_param` writing into `inst.params`, which the
        // mux's `selected_input_branch` reads on the next frame's
        // live-set rebuild.
        let (mut g, selector, log) = build_three_branch_mux_graph(Some("in_0"));
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();

        // Frame 0: in_0 selected → prod_a runs.
        exec.execute_frame(&mut g, &plan, frame_time());

        // Flip the selection and drain frame 0's log so frame 1's
        // assertions only see frame 1's evaluations.
        *selector.lock().unwrap() = Some("in_2");
        log.lock().unwrap().clear();

        // Frame 1: in_2 selected → prod_c runs, prod_a no longer.
        exec.execute_frame(&mut g, &plan, frame_time());

        let names: Vec<String> = log
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.type_name.clone())
            .collect();
        assert!(
            names.contains(&"prod_c".to_string()),
            "frame 1 should run the newly-selected branch (prod_c), got: {names:?}",
        );
        assert!(
            !names.contains(&"prod_a".to_string()),
            "frame 1 should NOT run the previously-selected branch (prod_a) — \
             live set must be rebuilt per frame, got: {names:?}",
        );
    }

    /// Regression: live-set propagation must traverse state-capture
    /// back-edges. OilyFluid hit this — `node.feedback` (low topo idx)
    /// reads its `in` port from `color_combine` (high topo idx, because
    /// the state-capture exemption removes the back-wire from in-degree).
    /// A pure reverse single-pass walk marks `color_combine` live when
    /// it reaches `feedback`, but its iteration has already passed
    /// `color_combine`'s slot — so `color_combine`'s OWN inputs never
    /// propagate. The noise/advect subgraph stays dark, the persistent
    /// resource never gets written, state stays at the first-frame
    /// clear, the visible output is static.
    ///
    /// Shape mirrors OilyFluid (mode = 0 = "Oil Slick"): only `in_0`
    /// of the mux is live → `consumer → feedback.out → mux.in_0 → final`.
    /// `feedback.in` is fed by `writer`, which combines `noise` and
    /// `feedback.out`. `noise` exists only to feed `writer`; if the
    /// propagation skips `writer`'s producers, `noise` is dead — which
    /// is the exact bug.
    #[test]
    fn live_set_propagates_through_state_capture_back_edge() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();

        // noise: only consumed by writer (whose only consumer is the
        // feedback's state-capture `in` port).
        let noise = g.add_node(Box::new(RecordingNode::new(
            "noise",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        // feedback: state-capture on `in`. Topo order places feedback
        // EARLIER than writer because the `in`-port wire from writer
        // skips in-degree counting.
        let feedback = g.add_node(Box::new(
            RecordingNode::new(
                "feedback",
                vec![input("in", PortType::Texture2D, true)],
                vec![output("out", PortType::Texture2D)],
                log.clone(),
            )
            .with_state_capture_ports(&["in"]),
        ));
        // writer: combines noise + feedback.out into the resource
        // feedback's `in` reads next frame. Sits HIGHER in topo than
        // feedback (this is what trips the single-pass walk).
        let writer = g.add_node(Box::new(RecordingNode::new(
            "writer",
            vec![
                input("a", PortType::Texture2D, true),
                input("b", PortType::Texture2D, true),
            ],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        // consumer: reads feedback.out — the path that pulls feedback
        // into the live set in the first place.
        let consumer = g.add_node(Box::new(RecordingNode::new(
            "consumer",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        // mux: in_0 selected. consumer feeds in_0; an unused producer
        // feeds in_1 to make the short-circuit do real work.
        let unused = g.add_node(Box::new(RecordingNode::new(
            "unused",
            vec![],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        let (mux_node, _sel) = RecordingNode::new(
            "mux",
            vec![
                input("in_0", PortType::Texture2D, false),
                input("in_1", PortType::Texture2D, false),
            ],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )
        .with_selected_branch(Some("in_0"));
        let mux = g.add_node(Box::new(mux_node));
        let fout = g.add_node(Box::new(FinalOutput::new()));

        g.connect((noise, "out"), (writer, "a")).unwrap();
        g.connect((feedback, "out"), (writer, "b")).unwrap();
        g.connect((writer, "out"), (feedback, "in")).unwrap();
        g.connect((feedback, "out"), (consumer, "in")).unwrap();
        g.connect((consumer, "out"), (mux, "in_0")).unwrap();
        g.connect((unused, "out"), (mux, "in_1")).unwrap();
        g.connect((mux, "out"), (fout, "in")).unwrap();

        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());

        let names: Vec<String> = log
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.type_name.clone())
            .collect();
        for required in ["noise", "writer", "feedback", "consumer", "mux"] {
            assert!(
                names.contains(&required.to_string()),
                "state-capture back-edge propagation must keep the feedback-write \
                 chain live; missing `{required}` in {names:?}",
            );
        }
        // Mux short-circuit still works: the in_1 producer is dead.
        assert!(
            !names.contains(&"unused".to_string()),
            "mux short-circuit must still prune the unselected branch; got {names:?}",
        );
    }

    #[test]
    fn unselected_branch_resources_dont_grow_slot_count_per_frame() {
        // Verifies the comment in `execute_frame_inner`: skipping
        // free_after on non-live steps doesn't leak slots within a
        // single frame. Slot count after a frame with one selected
        // branch is strictly less than the count with all branches
        // live — confirms the optimization actually reduces work.
        let (mut g_all, _sel_all, _log_all) = build_three_branch_mux_graph(None);
        let plan_all = compile(&g_all).unwrap();
        let mut exec_all = Executor::with_mock();
        exec_all.execute_frame(&mut g_all, &plan_all, frame_time());
        let slots_all = exec_all.backend().slot_count();

        let (mut g_one, _sel_one, _log_one) = build_three_branch_mux_graph(Some("in_1"));
        let plan_one = compile(&g_one).unwrap();
        let mut exec_one = Executor::with_mock();
        exec_one.execute_frame(&mut g_one, &plan_one, frame_time());
        let slots_one = exec_one.backend().slot_count();

        assert!(
            slots_one < slots_all,
            "single-branch selection must allocate fewer slots than full eager evaluation; \
             eager={slots_all}, pruned={slots_one}",
        );
    }

    // ─── Memoized-dataflow (constant-subgraph hoisting) ───

    /// Pure test node: one float param, one texture output, counts evaluates.
    /// Optionally takes a texture input (for transitive-closure tests).
    struct PureCountingNode {
        type_id: EffectNodeType,
        pure: bool,
        with_input: bool,
        evals: Arc<Mutex<u32>>,
    }

    impl PureCountingNode {
        fn new(pure: bool, evals: Arc<Mutex<u32>>) -> Self {
            Self {
                type_id: EffectNodeType::new("test.pure_counting"),
                pure,
                with_input: false,
                evals,
            }
        }

        fn with_input(pure: bool, evals: Arc<Mutex<u32>>) -> Self {
            Self {
                type_id: EffectNodeType::new("test.pure_counting_consumer"),
                pure,
                with_input: true,
                evals,
            }
        }
    }

    impl EffectNode for PureCountingNode {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            static INPUTS: [NodeInput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("in"),
                ty: PortType::Texture2D,
                kind: PortKind::Input,
                required: false,
            }];
            if self.with_input { &INPUTS } else { &[] }
        }
        fn outputs(&self) -> &[NodeOutput] {
            static OUTPUTS: [NodeOutput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("out"),
                ty: PortType::Texture2D,
                kind: PortKind::Output,
                required: false,
            }];
            &OUTPUTS
        }
        fn parameters(&self) -> &[ParamDef] {
            static PARAMS: [ParamDef; 1] = [ParamDef {
                name: std::borrow::Cow::Borrowed("k"),
                label: "K",
                ty: crate::node_graph::parameters::ParamType::Float,
                default: crate::node_graph::parameters::ParamValue::Float(1.0),
                range: None,
                enum_values: &[],
            }];
            &PARAMS
        }
        fn is_pure(&self) -> bool {
            self.pure
        }
        fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {
            *self.evals.lock().unwrap() += 1;
        }
    }

    /// A pure step with unchanged params executes exactly once — frames 2..n
    /// serve its held output slot without re-running (the Infrared static-LUT
    /// shape: a constant ramp must not re-render 60×/s).
    #[test]
    fn pure_step_executes_once_while_clean() {
        let evals = Arc::new(Mutex::new(0));
        let mut g = Graph::new();
        g.add_node(Box::new(PureCountingNode::new(true, evals.clone())));
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        for _ in 0..3 {
            exec.execute_frame(&mut g, &plan, frame_time());
        }
        assert_eq!(*evals.lock().unwrap(), 1, "clean pure step must skip");
    }

    /// The default (non-pure) node never memoizes — identical setup, three
    /// executes. Guards against accidentally memoizing un-opted-in nodes.
    #[test]
    fn impure_step_executes_every_frame() {
        let evals = Arc::new(Mutex::new(0));
        let mut g = Graph::new();
        g.add_node(Box::new(PureCountingNode::new(false, evals.clone())));
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        for _ in 0..3 {
            exec.execute_frame(&mut g, &plan, frame_time());
        }
        assert_eq!(*evals.lock().unwrap(), 3);
    }

    /// A REAL param change re-executes the pure step exactly once; re-writing
    /// the SAME value (what binding applies do every frame) does not. This is
    /// the live-perform contract: twist the palette knob → one re-render.
    #[test]
    fn param_change_reexecutes_pure_step_once() {
        use crate::node_graph::parameters::ParamValue;

        let evals = Arc::new(Mutex::new(0));
        let mut g = Graph::new();
        let n = g.add_node(Box::new(PureCountingNode::new(true, evals.clone())));
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();

        exec.execute_frame(&mut g, &plan, frame_time());
        // Same-value writes: no epoch bump, still clean.
        g.set_param(n, "k", ParamValue::Float(1.0)).unwrap();
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(*evals.lock().unwrap(), 1, "same-value write must stay clean");

        // Real change: one re-execute, then clean again.
        g.set_param(n, "k", ParamValue::Float(2.0)).unwrap();
        exec.execute_frame(&mut g, &plan, frame_time());
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(*evals.lock().unwrap(), 2, "changed param re-executes exactly once");
    }

    /// A pure chain goes transitively quiet: pure producer → pure consumer,
    /// both execute exactly once (the hoistable closure extends through the
    /// wire — Infrared's ramp-bank → mux shape).
    #[test]
    fn pure_chain_goes_transitively_quiet() {
        let p_evals = Arc::new(Mutex::new(0));
        let c_evals = Arc::new(Mutex::new(0));
        let mut g = Graph::new();
        let producer = g.add_node(Box::new(PureCountingNode::new(true, p_evals.clone())));
        let consumer = g.add_node(Box::new(PureCountingNode::with_input(true, c_evals.clone())));
        g.connect((producer, "out"), (consumer, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        for _ in 0..3 {
            exec.execute_frame(&mut g, &plan, frame_time());
        }
        assert_eq!(*p_evals.lock().unwrap(), 1);
        assert_eq!(*c_evals.lock().unwrap(), 1, "pure consumer of a pure producer must skip");
    }

    /// A pure node fed by an IMPURE producer is NOT hoistable (the closure
    /// rule): the producer re-executes every frame, so the consumer must too
    /// — and its output is never held out of the texture pool.
    #[test]
    fn pure_node_fed_by_impure_producer_runs_every_frame() {
        let p_evals = Arc::new(Mutex::new(0));
        let c_evals = Arc::new(Mutex::new(0));
        let mut g = Graph::new();
        let producer = g.add_node(Box::new(PureCountingNode::new(false, p_evals.clone())));
        let consumer = g.add_node(Box::new(PureCountingNode::with_input(true, c_evals.clone())));
        g.connect((producer, "out"), (consumer, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        for _ in 0..3 {
            exec.execute_frame(&mut g, &plan, frame_time());
        }
        assert_eq!(*p_evals.lock().unwrap(), 3);
        assert_eq!(*c_evals.lock().unwrap(), 3, "dynamic upstream must keep the pure node live");
    }

    /// A non-pure consumer keeps running every frame and reads the SAME slot
    /// the pure producer wrote on frame 1 — the held slot serves consumers
    /// (this is what `sticky_resources` protects from `free_after`).
    #[test]
    fn consumer_reads_held_slot_of_clean_pure_producer() {
        let evals = Arc::new(Mutex::new(0));
        let log: Arc<Mutex<Vec<EvaluationRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let mut g = Graph::new();
        let producer = g.add_node(Box::new(PureCountingNode::new(true, evals.clone())));
        let consumer = g.add_node(Box::new(RecordingNode::new(
            "test.consumer",
            vec![input("in", PortType::Texture2D, true)],
            vec![output("out", PortType::Texture2D)],
            log.clone(),
        )));
        g.connect((producer, "out"), (consumer, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        for _ in 0..3 {
            exec.execute_frame(&mut g, &plan, frame_time());
        }
        assert_eq!(*evals.lock().unwrap(), 1, "producer runs once");
        let records = log.lock().unwrap();
        assert_eq!(records.len(), 3, "consumer runs every frame");
        let first_in = records[0].inputs.clone();
        assert!(
            records.iter().all(|r| r.inputs == first_in),
            "consumer must read the producer's held slot on every frame"
        );
    }

    /// Data-driven skip fixture (the third skip reason). `reporter()` flips
    /// [`EffectNode::reports_empty_output`] from a shared flag — a stand-in
    /// for blob_detect_ffi's zero-track frames. `consumer()` declares its
    /// `in` port via [`EffectNode::empty_skip_input_ports`]. Both count
    /// evaluates.
    struct EmptyDrivenNode {
        type_id: EffectNodeType,
        with_input: bool,
        declares_skip: bool,
        empty: Arc<Mutex<bool>>,
        evals: Arc<Mutex<u32>>,
    }

    impl EmptyDrivenNode {
        fn reporter(empty: Arc<Mutex<bool>>, evals: Arc<Mutex<u32>>) -> Self {
            Self {
                type_id: EffectNodeType::new("test.empty_reporter"),
                with_input: false,
                declares_skip: false,
                empty,
                evals,
            }
        }

        fn consumer(declares_skip: bool, evals: Arc<Mutex<u32>>) -> Self {
            Self {
                type_id: EffectNodeType::new("test.empty_consumer"),
                with_input: true,
                declares_skip,
                empty: Arc::new(Mutex::new(false)),
                evals,
            }
        }
    }

    impl EffectNode for EmptyDrivenNode {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            static INPUTS: [NodeInput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("in"),
                ty: PortType::Texture2D,
                kind: PortKind::Input,
                required: false,
            }];
            if self.with_input { &INPUTS } else { &[] }
        }
        fn outputs(&self) -> &[NodeOutput] {
            static OUTPUTS: [NodeOutput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("out"),
                ty: PortType::Texture2D,
                kind: PortKind::Output,
                required: false,
            }];
            &OUTPUTS
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn reports_empty_output(&self) -> bool {
            !self.with_input && *self.empty.lock().unwrap()
        }
        fn empty_skip_input_ports(&self) -> &'static [&'static str] {
            if self.declares_skip { &["in"] } else { &[] }
        }
        fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {
            *self.evals.lock().unwrap() += 1;
        }
    }

    fn empty_skip_graph(
        empty: Arc<Mutex<bool>>,
        declares: bool,
    ) -> (Graph, Arc<Mutex<u32>>, Arc<Mutex<u32>>) {
        let r_evals = Arc::new(Mutex::new(0));
        let c_evals = Arc::new(Mutex::new(0));
        let mut g = Graph::new();
        let reporter = g.add_node(Box::new(EmptyDrivenNode::reporter(empty, r_evals.clone())));
        let consumer = g.add_node(Box::new(EmptyDrivenNode::consumer(declares, c_evals.clone())));
        g.connect((reporter, "out"), (consumer, "in")).unwrap();
        (g, r_evals, c_evals)
    }

    /// Steady empty data: the declaring consumer executes the FIRST empty
    /// frame (writing out its empty state — the one-frame guard) and skips
    /// every frame after. The reporter itself runs every frame — it is the
    /// detector and must keep detecting.
    #[test]
    fn empty_consumer_skips_after_one_empty_frame() {
        let empty = Arc::new(Mutex::new(true));
        let (mut g, r_evals, c_evals) = empty_skip_graph(empty, true);
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        for _ in 0..4 {
            exec.execute_frame(&mut g, &plan, frame_time());
        }
        assert_eq!(*r_evals.lock().unwrap(), 4, "the reporter keeps detecting every frame");
        assert_eq!(
            *c_evals.lock().unwrap(),
            1,
            "declaring consumer runs the first empty frame, then skips"
        );
    }

    /// Data returning un-skips immediately: the frame the reporter stops
    /// reporting empty, its output is no longer marked and the consumer
    /// evaluates that same frame (no one-frame lag on the way BACK — a blob
    /// appearing must draw on the frame it appears).
    #[test]
    fn empty_skip_unskips_the_frame_data_returns() {
        let empty = Arc::new(Mutex::new(true));
        let (mut g, _r_evals, c_evals) = empty_skip_graph(empty.clone(), true);
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        for _ in 0..3 {
            exec.execute_frame(&mut g, &plan, frame_time());
        }
        assert_eq!(*c_evals.lock().unwrap(), 1, "steady empty: one execute");
        *empty.lock().unwrap() = false;
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(
            *c_evals.lock().unwrap(),
            2,
            "consumer must evaluate on the frame data returns"
        );
    }

    /// A node that does NOT declare `empty_skip_input_ports` never skips on
    /// empty input — the skip is strictly opt-in (a track-ager or trail decay
    /// must keep evolving while its input is empty).
    #[test]
    fn undeclared_consumer_never_skips_on_empty_input() {
        let empty = Arc::new(Mutex::new(true));
        let (mut g, _r_evals, c_evals) = empty_skip_graph(empty, false);
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        for _ in 0..4 {
            exec.execute_frame(&mut g, &plan, frame_time());
        }
        assert_eq!(*c_evals.lock().unwrap(), 4, "no declaration → no skip");
    }

    /// Emptiness propagates through a DECLARING chain, one frame per stage:
    /// a skipped consumer's outputs count as empty, so the next declarer
    /// downstream skips a frame later (after IT has written its own empty
    /// state once).
    #[test]
    fn empty_skip_propagates_through_declaring_chain() {
        let empty = Arc::new(Mutex::new(true));
        let a_evals = Arc::new(Mutex::new(0));
        let b_evals = Arc::new(Mutex::new(0));
        let mut g = Graph::new();
        let reporter = g.add_node(Box::new(EmptyDrivenNode::reporter(
            empty,
            Arc::new(Mutex::new(0)),
        )));
        let a = g.add_node(Box::new(EmptyDrivenNode::consumer(true, a_evals.clone())));
        let b = g.add_node(Box::new(EmptyDrivenNode::consumer(true, b_evals.clone())));
        g.connect((reporter, "out"), (a, "in")).unwrap();
        g.connect((a, "out"), (b, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();
        for _ in 0..5 {
            exec.execute_frame(&mut g, &plan, frame_time());
        }
        assert_eq!(*a_evals.lock().unwrap(), 1, "first declarer skips from frame 2");
        assert_eq!(
            *b_evals.lock().unwrap(),
            2,
            "second declarer sees emptiness one frame later (a's frame-2 skip marks it)"
        );
    }

    /// Draw-shaped fixture for the data-skip PASSTHROUGH path: declares
    /// `empty_skip_input_ports` AND `skip_passthrough_ports`, plus a
    /// separate texture `src` it composites over. A plain data-skip would
    /// freeze a stale copy of `src`; the executor must alias `src` → `out`
    /// instead (zero work, live video flows through).
    struct DrawShapedNode {
        type_id: EffectNodeType,
        evals: Arc<Mutex<u32>>,
    }

    impl EffectNode for DrawShapedNode {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            static INPUTS: [NodeInput; 2] = [
                NodePort {
                    name: std::borrow::Cow::Borrowed("src"),
                    ty: PortType::Texture2D,
                    kind: PortKind::Input,
                    required: false,
                },
                NodePort {
                    name: std::borrow::Cow::Borrowed("detections"),
                    ty: PortType::Texture2D,
                    kind: PortKind::Input,
                    required: false,
                },
            ];
            &INPUTS
        }
        fn outputs(&self) -> &[NodeOutput] {
            static OUTPUTS: [NodeOutput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("out"),
                ty: PortType::Texture2D,
                kind: PortKind::Output,
                required: false,
            }];
            &OUTPUTS
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn empty_skip_input_ports(&self) -> &'static [&'static str] {
            &["detections"]
        }
        fn skip_passthrough_ports(&self) -> Option<(&'static str, &'static str)> {
            Some(("src", "out"))
        }
        fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {
            *self.evals.lock().unwrap() += 1;
        }
    }

    /// Data-skip on a passthrough-declaring node ALIASES instead of
    /// holding: the node evaluates the first empty frame (one-frame
    /// guard), then every later empty frame installs the src → out alias
    /// (observed on the mock backend) without evaluating — and the frame
    /// data returns it evaluates again immediately.
    #[test]
    fn data_skip_aliases_passthrough_declaring_draw_node() {
        let empty = Arc::new(Mutex::new(true));
        let r_evals = Arc::new(Mutex::new(0));
        let d_evals = Arc::new(Mutex::new(0));
        let mut g = Graph::new();
        let reporter = g.add_node(Box::new(EmptyDrivenNode::reporter(
            empty.clone(),
            r_evals,
        )));
        // A second source standing in for the live video the draw node
        // composites over (the reporter plays the detections producer).
        let video = g.add_node(Box::new(EmptyDrivenNode::reporter(
            Arc::new(Mutex::new(false)),
            Arc::new(Mutex::new(0)),
        )));
        let draw = g.add_node(Box::new(DrawShapedNode {
            type_id: EffectNodeType::new("test.draw_shaped"),
            evals: d_evals.clone(),
        }));
        g.connect((video, "out"), (draw, "src")).unwrap();
        g.connect((reporter, "out"), (draw, "detections")).unwrap();
        // A downstream reader keeps draw's output allocated — without a
        // consumer the planner drops the dead output slot and the alias
        // has nothing to install onto (production draw stacks always
        // feed the next layer / final_output).
        let sink = g.add_node(Box::new(EmptyDrivenNode::consumer(
            false,
            Arc::new(Mutex::new(0)),
        )));
        g.connect((draw, "out"), (sink, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::with_mock();

        for _ in 0..4 {
            exec.execute_frame(&mut g, &plan, frame_time());
        }
        // Eval count 1 PROVES the alias path: with the mock's alias_2d
        // returning true, a passthrough-declaring node only avoids
        // evaluate via the installed alias (an alias failure falls
        // through to evaluate, which would count 4 here).
        assert_eq!(
            *d_evals.lock().unwrap(),
            1,
            "draw node evaluates the first empty frame, then alias-skips"
        );

        *empty.lock().unwrap() = false;
        exec.execute_frame(&mut g, &plan, frame_time());
        assert_eq!(
            *d_evals.lock().unwrap(),
            2,
            "draw node evaluates again the frame detections return"
        );
    }

}

/// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3b/BUG-197 — the alias-path
/// generation-propagation gate needs a real backend: pool recycling can
/// legitimately hand the SAME `ResourceId` a DIFFERENT physical `Slot`
/// across frames whenever a resource is released and reacquired every
/// frame (see this file's own `alias_propagation_state` doc comment) —
/// under `MockBackend`/`MetalBackend`'s shared free-list mechanics, a
/// minimal chain with no other same-shaped resource competing for the pool
/// bucket can even oscillate between exactly two slots forever, which
/// would make a raw physical-slot comparison meaningless noise rather
/// than a signal. Pinning both resources under test via
/// `MetalBackend::pre_bind_texture_2d` sidesteps that entirely (same
/// technique the primitive-level P1/P3 gpu_tests use for their own output
/// slots) so this test observes the propagation LOGIC in isolation from
/// ordinary pool churn — exactly what a real `render_scene` envmap slot
/// gets in production (its host pre-binds/reuses long-lived resources,
/// not a two-texture pool that flips every frame).
#[cfg(all(test, feature = "gpu-proofs"))]
mod alias_gpu_tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::MetalBackend;
    use crate::node_graph::compile;
    use crate::node_graph::effect_node::EffectNodeType;
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
    use crate::render_target::RenderTarget;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;
    use std::sync::{Arc, Mutex};

    fn frame_time() -> FrameTime {
        FrameTime { beats: Beats(0.0), seconds: Seconds(0.0), delta: Seconds(1.0 / 60.0), frame_count: 0 }
    }

    /// A Texture2D producer whose declared-unchanged behavior is driven by
    /// a shared flag the test flips per frame — stands in for a real gated
    /// source (e.g. `gltf_texture_source`'s R1 gate) feeding a
    /// `mux_texture`-shaped alias consumer.
    struct GatedSourceNode {
        type_id: EffectNodeType,
        declare_unchanged: Arc<Mutex<bool>>,
    }

    impl EffectNode for GatedSourceNode {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            &[]
        }
        fn outputs(&self) -> &[NodeOutput] {
            static OUTPUTS: [NodeOutput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("out"),
                ty: PortType::Texture2D,
                kind: PortKind::Output,
                required: false,
            }];
            &OUTPUTS
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            if *self.declare_unchanged.lock().unwrap() {
                ctx.mark_outputs_unchanged();
            }
        }
    }

    /// Records the write generation of its "in" port on every evaluate —
    /// the probe for the alias-path propagation test below.
    struct GenObservingNode {
        type_id: EffectNodeType,
        log: Arc<Mutex<Vec<Option<u64>>>>,
    }

    impl EffectNode for GenObservingNode {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            static INPUTS: [NodeInput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("in"),
                ty: PortType::Texture2D,
                kind: PortKind::Input,
                required: false,
            }];
            &INPUTS
        }
        fn outputs(&self) -> &[NodeOutput] {
            &[]
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            self.log.lock().unwrap().push(ctx.inputs.slot_generation("in"));
        }
    }

    /// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P3b/BUG-197 gate: a
    /// param-driven (`skip_passthrough`) alias — `mux_texture`'s
    /// inline-selector fast path is the production case that motivated
    /// this — propagates its aliased input's write generation through to
    /// its own output's generation instead of always conservatively
    /// bumping, so a downstream consumer (standing in for `render_scene`'s
    /// IBL cache key) sees a STABLE generation across static frames and a
    /// real bump the frame the source actually re-emits, then
    /// re-stabilizes — proving this isn't a one-shot fluke.
    #[test]
    fn alias_path_propagates_generation_through_mux_fast_path() {
        let device = crate::test_device();
        let (w, h) = (16u32, 16u32);
        let format = GpuTextureFormat::Rgba16Float;

        let declare_unchanged = Arc::new(Mutex::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));

        let mut g = Graph::new();
        let src = g.add_node(Box::new(GatedSourceNode {
            type_id: EffectNodeType::new("test.gated_source"),
            declare_unchanged: declare_unchanged.clone(),
        }));
        let mux = g.add_node(Box::new(crate::node_graph::primitives::MuxTexture::new()));
        let observer = g.add_node(Box::new(GenObservingNode {
            type_id: EffectNodeType::new("test.gen_observer"),
            log: log.clone(),
        }));
        g.connect((src, "out"), (mux, "in_0")).unwrap();
        g.connect((mux, "out"), (observer, "in")).unwrap();
        let plan = compile(&g).unwrap();

        let r_src_out = plan
            .steps()
            .iter()
            .find(|s| s.node == src)
            .and_then(|s| s.outputs.iter().find(|(n, _)| *n == "out"))
            .map(|&(_, r)| r)
            .expect("src's out resource is bound (observer reads it transitively)");
        let r_mux_out = plan
            .steps()
            .iter()
            .find(|s| s.node == mux)
            .and_then(|s| s.outputs.iter().find(|(n, _)| *n == "out"))
            .map(|&(_, r)| r)
            .expect("mux's out resource is bound (observer wires it)");

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        // Pin BOTH resources to fixed physical slots so the propagation
        // logic is observed in isolation from ordinary pool recycling —
        // see this module's doc comment.
        backend.pre_bind_texture_2d(r_src_out, RenderTarget::new(&device, w, h, format, "p3b-src-out"));
        backend.pre_bind_texture_2d(r_mux_out, RenderTarget::new(&device, w, h, format, "p3b-mux-out"));

        let mut exec = Executor::new(Box::new(backend));

        // Frame 1: no prior alias state exists either way — the mux's own
        // generation always bumps on the first frame.
        exec.execute_frame(&mut g, &plan, frame_time());
        // Frame 2: source declares unchanged — same alias pair as frame 1,
        // same source generation ⇒ the mux alias propagates "unchanged".
        *declare_unchanged.lock().unwrap() = true;
        exec.execute_frame(&mut g, &plan, frame_time());
        // Frame 3: source re-emits again — generation must move.
        *declare_unchanged.lock().unwrap() = false;
        exec.execute_frame(&mut g, &plan, frame_time());
        // Frame 4: source declares unchanged again — proves
        // re-stabilization, not a one-shot fluke.
        *declare_unchanged.lock().unwrap() = true;
        exec.execute_frame(&mut g, &plan, frame_time());

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 4, "observer must evaluate every frame (never pruned)");
        let (g1, g2, g3, g4) = (log[0], log[1], log[2], log[3]);
        assert!(g1.is_some(), "aliased input must resolve to a bound slot");
        assert_eq!(g2, g1, "static input ⇒ mux alias propagates unchanged, generation stable");
        assert_ne!(g3, g2, "source re-emitting must bump the generation downstream sees");
        assert_eq!(g4, g3, "re-stabilization after a change must also propagate as unchanged");
    }
}

/// BUG-216 (`docs/BUG_BACKLOG.md`, D6(b) of `docs/DEPTH_RELIGHT_DESIGN.md`):
/// a `node.feedback` loop whose blend output feeds `system.final_output`
/// DIRECTLY (the natural authoring wiring) used to freeze at one frame of
/// history — the boundary output's resource is pre-bound as a borrowed
/// target, `node.feedback`'s ping-pong swap refuses under that shadow, and
/// the executor's `late_capture` had no fallback, silently dropping the
/// frame's capture forever. Real-GPU regression: builds exactly that shape
/// (`node.mix` Add-blending a constant source against its own delayed
/// output, wired straight to `FinalOutput`) and proves the readback value
/// keeps compounding across frames instead of freezing after frame 1.
#[cfg(all(test, feature = "gpu-proofs"))]
mod bug_216_gpu_tests {
    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::GpuTextureFormat;

    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::node_graph::{
        ExecutionPlan, Executor, FinalOutput, FrameTime, Graph, MetalBackend, NodeInstanceId,
        PrimitiveRegistry, ResourceId, Source, StateStore, compile,
    };
    use crate::render_target::RenderTarget;

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    fn output_resource(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
        for step in plan.steps() {
            if step.node == node {
                for &(name, id) in &step.outputs {
                    if name == port {
                        return id;
                    }
                }
            }
        }
        panic!("no output `{port}` on node {node:?}");
    }

    /// Reads back pixel (0,0) of `res`'s CURRENT texture as rgba16float.
    fn readback_pixel(
        device: &manifold_gpu::GpuDevice,
        exec: &Executor,
        res: ResourceId,
        w: u32,
        h: u32,
    ) -> [f32; 4] {
        let slot = exec
            .backend()
            .slot_for(res)
            .expect("resource must be bound to a slot");
        let tex = exec
            .backend()
            .texture_2d(slot)
            .expect("resource's texture must be retained");
        let bytes_per_row = w * 8; // rgba16float = 8 bytes/pixel
        let total_bytes = u64::from(h * bytes_per_row);
        let readback_buf = device.create_buffer_shared(total_bytes);
        let mut readback_enc = device.create_encoder("bug216-readback");
        readback_enc.copy_texture_to_buffer(tex, &readback_buf, w, h, bytes_per_row);
        readback_enc.commit_and_wait_completed();
        let ptr = readback_buf.mapped_ptr().expect("shared buffer pointer");
        let halves: &[u16] =
            unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
        [
            f16::from_bits(halves[0]).to_f32(),
            f16::from_bits(halves[1]).to_f32(),
            f16::from_bits(halves[2]).to_f32(),
            f16::from_bits(halves[3]).to_f32(),
        ]
    }

    #[test]
    fn feedback_direct_to_final_output_accumulates_trails() {
        use crate::node_graph::parameters::ParamValue;

        let device = crate::test_device();
        let (w, h) = (4u32, 4u32);
        let format = GpuTextureFormat::Rgba16Float;
        let registry = PrimitiveRegistry::with_builtin();

        // BUG-216 shape: mix(source, feedback.out) → feedback.in AND
        // mix.out → final_output DIRECTLY (no node sitting between the
        // blend and the boundary — the wiring the backlog entry calls
        // "the natural wiring", and the one that used to freeze).
        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let mix = g.add_node(registry.construct("node.mix").expect("node.mix registered"));
        let fb = g
            .add_node(registry.construct("node.feedback").expect("node.feedback registered"));
        let out = g.add_node(Box::new(FinalOutput::new()));

        g.connect((src, "out"), (mix, "a")).unwrap();
        g.connect((fb, "out"), (mix, "b")).unwrap();
        g.connect((mix, "out"), (fb, "in")).unwrap();
        g.connect((mix, "out"), (out, "in")).unwrap();

        // Add mode, amount=1.0 (full blend, no crossfade) — every frame's
        // output is `source + previous frame's delayed output`, so a
        // WORKING loop compounds monotonically; a FROZEN loop (BUG-216)
        // reads the SAME delayed value forever and every frame after the
        // first renders identically to frame 1.
        {
            let inst = g.get_node_mut(mix).expect("mix node exists");
            inst.params
                .insert(std::borrow::Cow::Borrowed("mode"), ParamValue::Enum(2)); // Add
            inst.params
                .insert(std::borrow::Cow::Borrowed("amount"), ParamValue::Float(1.0));
        }

        let plan = compile(&g).unwrap();
        let source_res = output_resource(&plan, src, "out");
        let mix_out_res = output_resource(&plan, mix, "out");

        let source_target = RenderTarget::new(&device, w, h, format, "bug216-source");
        let canvas_target = RenderTarget::new(&device, w, h, format, "bug216-canvas");
        let mut native_enc = device.create_encoder("bug216-setup");
        {
            let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
            gpu.clear_texture(&source_target.texture, 0.05, 0.05, 0.05, 1.0);
        }
        native_enc.commit_and_wait_completed();

        let mut backend = MetalBackend::new(device.arc(), w, h, format);
        backend.pre_bind_texture_2d(source_res, source_target);
        // The exact BUG-216 condition: `mix.out` (== `feedback.in` ==
        // `final_output.in`, one shared ResourceId) carries a BORROWED
        // shadow via `replace_texture_2d` — the real mechanism
        // `PresetRuntime::install_target` uses to install the host's
        // canvas texture over `final_output.in` each frame
        // (`preset_runtime.rs:3091`), NOT `pre_bind_texture_2d` (which
        // installs an OWNED slot with no shadow, and would let the swap
        // succeed every frame — a plain `pre_bind` does not reproduce
        // this bug). `replace_texture_2d` requires the slot to already
        // own a `RenderTarget`, so allocate one first and bind it to
        // `mix_out_res`.
        let placeholder = RenderTarget::new(&device, w, h, format, "bug216-mix-out-placeholder");
        let mix_out_slot = backend.allocate_slot(placeholder);
        backend.bind_resource_to_slot(mix_out_res, mix_out_slot);
        assert!(
            backend.replace_texture_2d(mix_out_slot, canvas_target.texture.clone()),
            "replace_texture_2d requires an owned RenderTarget already at the slot"
        );

        let mut exec = Executor::new(Box::new(backend));
        let mut store = StateStore::new();
        let owner_key = 216;

        let mut pixels: Vec<[f32; 4]> = Vec::new();
        for _ in 0..4 {
            let mut native_enc = device.create_encoder("bug216-frame");
            {
                let mut gpu = RendererGpuEncoder::new(&mut native_enc, &device);
                exec.execute_frame_with_state(
                    &mut g,
                    &plan,
                    frame_time(),
                    &mut gpu,
                    &mut store,
                    owner_key,
                );
            }
            native_enc.commit_and_wait_completed();
            pixels.push(readback_pixel(&device, &exec, mix_out_res, w, h));
        }

        assert_ne!(
            pixels[3][0], pixels[0][0],
            "BUG-216: frame 4's output must differ from frame 1's — a frozen \
             loop (the swap-refused-and-dropped bug) reproduces the SAME \
             value every frame after the first. Frames: {pixels:?}",
        );
        // Frame 1 == frame 2 is EXPECTED, not the bug under test:
        // `node.feedback`'s allocation frame seeds its state from `in` and
        // deliberately skips ITS OWN late_capture that same frame (else the
        // seed would be immediately clobbered — `temporal.rs`'s
        // `just_allocated` guard), so the delayed value first advances
        // starting frame 3. From frame 2 onward the loop is in steady
        // state; a frozen loop (BUG-216) would hold frame 2's value
        // forever, so frames 2→4 must strictly increase.
        assert!(
            pixels[2][0] > pixels[1][0] && pixels[3][0] > pixels[2][0],
            "trails must compound monotonically frame over frame under \
             Add-mode feedback once past the alloc-frame plateau — got {pixels:?}",
        );
    }
}
