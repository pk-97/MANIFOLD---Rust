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
use crate::layer_skin::LayerSkinRegistry;
use crate::node_graph::backend::{Backend, MockBackend};
use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
use crate::node_graph::content_revision::{ContentVersion, StorageRevision};
use crate::node_graph::effect_node::{EffectNodeContext, FrameTime, NodeInstanceId};
use crate::node_graph::execution_plan::{CompiledMeshRevisionRule, ExecutionPlan, ExecutionStep, ResourceId};
use crate::node_graph::mesh_change::{MeshAspect, MeshRevision};
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
/// SCENE_MODIFIER_RT_DESIGN.md §3.2 — one recorded dependency snapshot
/// entry: `(input resource, watched aspect, value seen at the output's
/// last commit)`.
type MeshDepSnapshot = (ResourceId, crate::node_graph::mesh_change::MeshAspect, u64);

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
    /// Sibling scratch for [`PortType::RenderMode`] writes — same drain pattern.
    rigid_body_write_scratch: Vec<(Slot, crate::node_graph::physics::RigidBody)>,
    render_mode_write_scratch: Vec<(Slot, crate::node_graph::render_mode::RenderMode)>,
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
    /// RT_QUALITY_SETTINGS_DESIGN.md D5 — resolved per-frame values from
    /// the active quality column (realtime vs export). Default = live constants
    /// so tests and non-RT graphs run unchanged. Set per frame via
    /// [`set_rt_quality`]; consumed by `render_scene` through the context.
    rt_quality: crate::node_graph::RtQuality,
    /// SCENE_FX P4a — borrowed pointer to the compositor's layer-skin registry,
    /// set each frame before the executor runs. A raw pointer is used because
    /// the executor's lifetime is independent of the registry; the content
    /// thread guarantees the pointer is valid for the frame. `None` when no
    /// registry is available (mock-backend tests, standalone validation).
    layer_skin_registry: Option<crate::layer_skin::LayerSkinPtr>,
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
    /// side: [`crate::node_graph::bindings::NodeInputs::storage_revision`].
    /// Monotonic within one rebuild epoch. Reset renews the epoch, so a
    /// physical cache must compare both storage identity and lifetime.
    slot_generations: Vec<u64>,
    /// Per-physical-slot content-availability flag, indexed by `Slot.0`:
    /// `true` = the producing step declared its outputs pending this frame
    /// (`ctx.mark_outputs_pending()` — async content in flight, bytes are
    /// allocation not content). Rewritten from the step's latest evaluate
    /// each time it runs (a skipped step keeps its last declaration), so
    /// stopping the declaration returns the slot to ready. Read side:
    /// [`crate::node_graph::bindings::NodeInputs::slot_content_ready`].
    slot_pending: Vec<bool>,
    /// SCENE_MODIFIER_RT_DESIGN.md §3.2 — per-LOGICAL-resource mesh
    /// revisions, indexed by `ResourceId`, sized to the plan's resource
    /// count at the plan-shape reset below. This is the authority; the
    /// per-slot `slot_mesh_revisions` is only its published snapshot, so
    /// pool reuse can never hand one logical resource another's revision
    /// (a recycled slot gets whatever its NEW resource publishes).
    mesh_revisions: Vec<crate::node_graph::mesh_change::MeshRevision>,
    /// SCENE_MODIFIER_RT_DESIGN.md §3.2 — per-resource mesh pending
    /// flags, indexed by `ResourceId`: the producing step's declared
    /// pending OR any wired input's pending, so a pending source remains
    /// pending through deformers, fusion, and scene bundles and no AS
    /// work may consume it.
    mesh_pending: Vec<bool>,
    /// SCENE_MODIFIER_RT_DESIGN.md §3.2 — per-resource dependency
    /// snapshots for `Dependencies` rules, indexed by `ResourceId`:
    /// the `(input resource, watched aspect, last-seen value)` triples
    /// recorded at the output's last commit. A missing snapshot (first
    /// commit) counts as changed — a fresh token is issued.
    mesh_dep_snapshots: Vec<Option<Box<[MeshDepSnapshot]>>>,
    /// SCENE_MODIFIER_RT_DESIGN.md §3.2 — the executor's single
    /// monotonically increasing mesh revision counter. Tokens are unique
    /// within this executor's `rebuild_epoch`; a new epoch (new
    /// `Executor`) starts its own counter, and consumers already fold
    /// the epoch into any cross-executor comparison (the
    /// `slot_generations` precedent).
    mesh_revision_counter: u64,
    /// SCENE_MODIFIER_RT_DESIGN.md §3.2 — per-physical-slot published
    /// mesh revision snapshot, indexed by `Slot.0`. Written at the same
    /// choke point as `slot_generations` from the logical
    /// `mesh_revisions`. Read side:
    /// [`crate::node_graph::bindings::NodeInputs::mesh_revision`].
    slot_mesh_revisions: Vec<crate::node_graph::mesh_change::MeshRevision>,
    /// Per-physical-slot logical content snapshots, published at the output
    /// commit point. A recycled slot is overwritten with its new logical
    /// resource's version before any downstream step reads it.
    slot_content_versions: Vec<Option<ContentVersion>>,
    /// Last observed concrete shape for each logical resource. Shape changes
    /// force a fresh logical publication even when a producer declares its
    /// bytes unchanged.
    content_shapes: Vec<Option<ContentShape>>,
    /// Last committed physical `(slot, storage revision)` for each logical
    /// resource. A same-numbered slot that was recycled through another
    /// tenant is not a safe no-write destination.
    resource_storage_state: Vec<Option<StorageSnapshot>>,
    /// Selected source, destination storage and logical content observed at
    /// the previous alias or passthrough copy. Logical equality preserves
    /// content across relocation; storage equality only controls physical
    /// freshness. Unknown source content never establishes semantic reuse.
    alias_propagation_state: Vec<Option<AliasPropagationState>>,
    /// Whether a step declared logical output content unchanged this frame.
    node_content_unchanged: Vec<bool>,
    /// Resources whose logical content revision changed at the current
    /// commit. Reused to drive memo epochs without allocating per frame.
    content_changed_resources: Vec<ResourceId>,
    /// RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md D6 — this executor
    /// instance's rebuild epoch, renewed at construction and reset from
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContentShape {
    Texture2D(u32, u32, manifold_gpu::GpuTextureFormat),
    Array(u64),
    DeclaredTexture(u32, u32, Option<manifold_gpu::GpuTextureFormat>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StorageSnapshot {
    slot: Slot,
    revision: StorageRevision,
    identity: Option<usize>,
}

#[derive(Clone, Copy)]
struct AliasPropagationState {
    source: ResourceId,
    destination: Slot,
    source_storage: StorageRevision,
    source_content: Option<ContentVersion>,
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
    /// (docs/CHAIN_FUSION_DESIGN.md section 5) after installing a carried-over
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
            render_mode_write_scratch: Vec::new(),
            rigid_body_write_scratch: Vec::new(),
            object_write_scratch: Vec::new(),
            error_scratch: Vec::new(),
            initialized_persistent: ahash::AHashSet::default(),
            live_steps: Vec::new(),
            wired_scratch: Vec::new(),
            preview_target: None,
            rt_quality: crate::node_graph::RtQuality::default(),
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
            slot_pending: Vec::new(),
            mesh_revisions: Vec::new(),
            mesh_pending: Vec::new(),
            mesh_dep_snapshots: Vec::new(),
            mesh_revision_counter: 0,
            slot_mesh_revisions: Vec::new(),
            slot_content_versions: Vec::new(),
            content_shapes: Vec::new(),
            resource_storage_state: Vec::new(),
            slot_generations: Vec::new(),
            alias_propagation_state: Vec::new(),
            node_content_unchanged: Vec::new(),
            content_changed_resources: Vec::new(),
            rebuild_epoch: NEXT_REBUILD_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            memo_steps_len: None,
            empty_resources: ahash::AHashSet::default(),
            empty_resources_prev: ahash::AHashSet::default(),
            live_scalar_inputs: Vec::new(),
            layer_skin_registry: None,
        }
    }

    /// BUG-318: drop all memoized-dataflow state so the next frame
    /// re-executes every step. MUST be called whenever the host swaps
    /// `ExecutionPlan` under a live executor (BUG-317's forced-outputs
    /// recompile): a memo-CLEAN step serves consumers from *held* backend
    /// slots recorded under the old plan — after a swap those handles can
    /// dangle (observed: a static gltf mesh subgraph stayed "clean" across
    /// an `rt_enabled` toggle, its held `vertices` array slot went stale,
    /// and every object rendered magenta-clear). Setting `memo_steps_len`
    /// to `None` routes the next `execute_frame_*` through the existing
    /// rebuild branch, which clears `step_memo` / `resource_epoch` /
    /// `alias_propagation_state` together. Persistent resources
    /// (`initialized_persistent`) are deliberately kept: their ResourceIds
    /// are topo-stable across a same-structure recompile and their contents
    /// (temporal history) remain valid — a toggle must not cause a history
    /// reset that D15 didn't decide.
    pub fn invalidate_memoized_dataflow(&mut self) {
        self.memo_steps_len = None;
    }

    /// Reset readiness and dataflow metadata after a backend resource swap.
    /// ResourceIds and slots remain valid, but every physical target is fresh;
    /// no prior memoized write or persistent-clear decision may skip its first
    /// producer evaluation.
    pub fn reset_after_resource_replacement(&mut self) {
        self.memo_steps_len = None;
        self.step_memo.clear();
        self.resource_epoch.clear();
        self.alias_propagation_state.clear();
        self.node_content_unchanged.clear();
        self.content_changed_resources.clear();
        self.initialized_persistent.clear();
        self.slot_pending.fill(false);
        self.mesh_pending.fill(false);
        self.mesh_revisions
            .fill(crate::node_graph::mesh_change::MeshRevision::default());
        self.slot_mesh_revisions
            .fill(crate::node_graph::mesh_change::MeshRevision::default());
        self.slot_content_versions.fill(None);
        self.content_shapes.fill(None);
        self.resource_storage_state.fill(None);
        self.mesh_dep_snapshots.iter_mut().for_each(|snapshot| *snapshot = None);
        self.slot_generations.clear();
        self.mesh_revision_counter = 0;
        self.rebuild_epoch = NEXT_REBUILD_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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

    /// RT_QUALITY_SETTINGS_DESIGN.md D5 — set the per-frame RT quality values
    /// (samples per pixel for each RT term and ray resolution). Call once per
    /// frame before `execute_frame_*` with the resolved values from the active
    /// project column (realtime vs export). Cheap — stores by value, no allocation.
    pub fn set_rt_quality(&mut self, q: crate::node_graph::RtQuality) {
        self.rt_quality = q;
    }

    /// SCENE_FX P4a — set the borrowed layer-skin registry for the next frame.
    /// Call once per frame before `execute_frame_*`. The registry must outlive
    /// the `execute_frame_*` call (the content thread guarantees this). `None`
    /// clears the pointer.
    pub fn set_layer_skin_registry(&mut self, registry: Option<&LayerSkinRegistry>) {
        self.layer_skin_registry = registry.map(crate::layer_skin::LayerSkinPtr::new);
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

    /// Whether an exported array contains producer content rather than only
    /// allocated storage. Render views borrow these arrays across executors.
    pub(crate) fn resource_content_ready(&self, resource: ResourceId) -> bool {
        self.backend.slot_for(resource).is_some_and(|slot|
            !self.slot_pending.get(slot.0 as usize).copied().unwrap_or(false))
    }

    /// Test-only read of the logical per-resource mesh revision — the
    /// §3.2 authority state the slot snapshot is published from.
    #[cfg(test)]
    pub(crate) fn mesh_revision_of_res(
        &self,
        res: ResourceId,
    ) -> crate::node_graph::mesh_change::MeshRevision {
        self.mesh_revisions.get(res.0 as usize).copied().unwrap_or_default()
    }

    /// Test-only read of the logical per-resource mesh pending flag —
    /// the producer's declaration OR any wired input's pending.
    #[cfg(test)]
    pub(crate) fn mesh_pending_of(&self, res: ResourceId) -> bool {
        self.mesh_pending.get(res.0 as usize).copied().unwrap_or(false)
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
    /// Publish logical content and mesh aspects at the shared output commit.
    /// Identical recopies retain content; first publication, shape changes and
    /// pending-to-ready transitions revise it. Mesh topology/positions follow
    /// their compiled rules when content changes. Pending follows the actual
    /// selected input for aliases/muxes and all inputs for ordinary transforms.
    /// Logical ResourceId state is authoritative; slot snapshots are refreshed
    /// even on skips so recycled storage cannot inherit another tenant's stamp.
    fn commit_mesh_revisions(
        &mut self,
        plan: &ExecutionPlan,
        step: &ExecutionStep,
        content_unchanged: bool,
        selected_source: Option<ResourceId>,
    ) {
        self.content_changed_resources.clear();
        for &(_, res) in &step.outputs {
            let idx = res.0 as usize;
            if idx >= self.mesh_revisions.len() {
                continue; // defensive: sized at the plan-shape reset
            }
            let previous_pending = self.mesh_pending[idx];
            let declared = self
                .backend
                .slot_for(res)
                .and_then(|s| self.slot_pending.get(s.0 as usize).copied())
                .unwrap_or(false);
            let input_pending = step.inputs.iter().any(|&(_, r)| {
                if selected_source.is_some_and(|selected| r != selected) {
                    return false;
                }
                self.mesh_pending.get(r.0 as usize).copied().unwrap_or(false)
                    || self
                        .backend
                        .slot_for(r)
                        .and_then(|s| self.slot_pending.get(s.0 as usize).copied())
                        .unwrap_or(false)
            });
            self.mesh_pending[idx] = declared || input_pending;

            let shape = self
                .backend
                .slot_for(res)
                .and_then(|slot| self.backend.texture_2d(slot))
                .map(|texture| ContentShape::Texture2D(texture.width, texture.height, texture.format))
                .or_else(|| self.backend.slot_for(res)
                    .and_then(|slot| self.backend.array_buffer(slot))
                    .map(|buffer| ContentShape::Array(buffer.size)))
                .or_else(|| {
                    plan.resource_dims(res)
                        .map(|(width, height)| ContentShape::DeclaredTexture(width, height, plan.resource_format(res)))
                });
            let shape_changed = self.content_shapes[idx] != shape;
            self.content_shapes[idx] = shape;
            let ready_transition = previous_pending && !self.mesh_pending[idx];
            let content_changed = !content_unchanged
                || self.mesh_revisions[idx].content == 0
                || shape_changed
                || ready_transition;

            if content_changed {
                self.mesh_revision_counter += 1;
                let token = self.mesh_revision_counter;
                if let Some(rule) = plan.mesh_rule(res) {
                    let old = self.mesh_revisions[idx];
                    let topology = match &rule.topology {
                        CompiledMeshRevisionRule::Written => token,
                        CompiledMeshRevisionRule::Fixed => old.topology,
                        CompiledMeshRevisionRule::Dependencies(deps) => {
                            if self.mesh_deps_changed(idx, deps) { token } else { old.topology }
                        }
                    };
                    let positions = match &rule.positions {
                        CompiledMeshRevisionRule::Written => token,
                        CompiledMeshRevisionRule::Fixed => old.positions,
                        CompiledMeshRevisionRule::Dependencies(deps) => {
                            if self.mesh_deps_changed(idx, deps) { token } else { old.positions }
                        }
                    };
                    self.mesh_revisions[idx] =
                        MeshRevision { topology, positions, content: token };
                    self.record_mesh_dep_snapshot(idx, rule);
                } else {
                    // Non-mesh resources still use the existing logical
                    // content counter as their authority.
                    self.mesh_revisions[idx].content = token;
                }
                self.content_changed_resources.push(res);
            }

            // Publish every output, including unchanged and pending outputs,
            // so a recycled slot never retains the previous occupant's
            // logical metadata.
            if let Some(slot) = self.backend.slot_for(res) {
                let s = slot.0 as usize;
                if self.slot_pending.len() <= s { self.slot_pending.resize(s + 1, false); }
                self.slot_pending[s] = self.mesh_pending[idx];
                if self.slot_mesh_revisions.len() <= s {
                    self.slot_mesh_revisions.resize(s + 1, MeshRevision::default());
                }
                self.slot_mesh_revisions[s] = self.mesh_revisions[idx];
                if self.slot_content_versions.len() <= s {
                    self.slot_content_versions.resize(s + 1, None);
                }
                self.slot_content_versions[s] = if self.mesh_pending[idx]
                    || self.mesh_revisions[idx].content == 0
                {
                    None
                } else {
                    Some(ContentVersion::new(
                        self.rebuild_epoch,
                        res,
                        self.mesh_revisions[idx].content,
                    ))
                };
            }
        }
    }

    /// Prove that every output still owns the same physical slot at the same
    /// storage revision as its last commit. Pool recycling can reuse a slot
    /// number for another logical resource, so checking the slot alone is
    /// insufficient. A producer receives this proof before it chooses a
    /// no-write fast path.
    fn outputs_retained(&self, step: &ExecutionStep) -> bool {
        step.outputs.iter().all(|&(_, res)| {
            let current = self.storage_snapshot(res);
            current.is_some()
                && self.resource_storage_state.get(res.0 as usize).copied().flatten() == current
        })
    }

    fn storage_snapshot(&self, resource: ResourceId) -> Option<StorageSnapshot> {
        let slot = self.backend.slot_for(resource)?;
        let revision = StorageRevision(*self.slot_generations.get(slot.0 as usize)?);
        let identity = self.backend.texture_2d(slot).map(|texture| texture.identity_key())
            .or_else(|| self.backend.array_buffer(slot).map(|buffer| buffer.identity_key()));
        Some(StorageSnapshot { slot, revision, identity })
    }

    /// §3.2 dependency comparison: true when any watched `(resource,
    /// aspect)` value differs from the snapshot recorded at the output's
    /// last commit. A missing snapshot (first commit) is changed.
    fn mesh_deps_changed(
        &self,
        out_idx: usize,
        deps: &[(ResourceId, MeshAspect)],
    ) -> bool {
        let Some(stored) = &self.mesh_dep_snapshots[out_idx] else { return true };
        deps.iter().any(|&(r, a)| {
            let current = self.mesh_revisions[r.0 as usize].aspect(a);
            stored
                .iter()
                .find(|&&(sr, sa, _)| sr == r && sa == a)
                .is_none_or(|&(_, _, sv)| sv != current)
        })
    }

    /// §3.2: record the union of both rules' dependencies with their
    /// current values. Reuses the existing allocation once it matches
    /// the required length — the dependency set is plan-fixed, so only
    /// the first commit allocates.
    fn record_mesh_dep_snapshot(
        &mut self,
        out_idx: usize,
        rule: &crate::node_graph::execution_plan::CompiledMeshOutputRule,
    ) {
        let top_deps: &[(ResourceId, MeshAspect)] = match &rule.topology {
            CompiledMeshRevisionRule::Dependencies(d) => d,
            _ => &[],
        };
        let pos_deps: &[(ResourceId, MeshAspect)] = match &rule.positions {
            CompiledMeshRevisionRule::Dependencies(d) => d,
            _ => &[],
        };
        let mut needed = top_deps.len();
        for &(r, a) in pos_deps {
            if !top_deps.contains(&(r, a)) {
                needed += 1;
            }
        }
        if needed == 0 {
            self.mesh_dep_snapshots[out_idx] = None;
            return;
        }
        // Disjoint field borrows: read current values from
        // `mesh_revisions` while writing the snapshot box.
        let revisions = &self.mesh_revisions;
        let slot = &mut self.mesh_dep_snapshots[out_idx];
        if slot.as_ref().is_none_or(|s| s.len() != needed) {
            *slot = Some(vec![(ResourceId(0), MeshAspect::Content, 0); needed].into_boxed_slice());
        }
        let buf = slot.as_mut().expect("just ensured");
        let mut n = 0;
        for &(r, a) in top_deps.iter().chain(pos_deps.iter()) {
            if buf[..n].iter().any(|&(sr, sa, _)| sr == r && sa == a) {
                continue;
            }
            buf[n] = (r, a, revisions[r.0 as usize].aspect(a));
            n += 1;
        }
        debug_assert_eq!(n, needed, "snapshot union size must match the pre-count");
    }

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
            self.rebuild_epoch = NEXT_REBUILD_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.step_memo.clear();
            self.step_memo.resize_with(plan.steps().len(), || None);
            self.resource_epoch.clear();
            self.slot_pending.clear();
            self.slot_generations.clear();
            self.slot_mesh_revisions.clear();
            self.slot_content_versions.clear();
            self.mesh_revisions.clear();
            self.mesh_pending.clear();
            self.mesh_dep_snapshots.clear();
            self.content_shapes.clear();
            self.resource_storage_state.clear();
            self.mesh_revision_counter = 0;
            self.node_declared_unchanged.resize(plan.steps().len(), false);
            self.node_content_unchanged.resize(plan.steps().len(), false);
            self.alias_propagation_state.clear();
            self.alias_propagation_state.resize_with(plan.steps().len(), || None);
            // SCENE_MODIFIER_RT_DESIGN.md §3.2: (re)size mesh revision
            // state to the plan's resources. Zeroed revisions are
            // conservative: no stored comparison can match, so the first
            // commit after a reshape always issues fresh tokens.
            self.mesh_revisions
                .resize(plan.resource_count(), crate::node_graph::mesh_change::MeshRevision::default());
            self.mesh_pending.resize(plan.resource_count(), false);
            self.mesh_dep_snapshots.resize_with(plan.resource_count(), || None);
            self.content_shapes.resize(plan.resource_count(), None);
            self.resource_storage_state.resize(plan.resource_count(), None);
        }
        // D5: reset every frame (not sticky like `step_memo`) — a node
        // must re-declare on every frame it wants to skip; the executor
        // never carries last frame's declaration forward.
        self.node_declared_unchanged.iter_mut().for_each(|v| *v = false);
        self.node_content_unchanged.iter_mut().for_each(|v| *v = false);

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

        // SCENE_FX P4a: dereference the raw pointer the host set this frame.
        // The content thread guarantees the registry outlives this call.
        let layer_skin_registry: Option<&LayerSkinRegistry> =
            self.layer_skin_registry.map(|ptr| unsafe { ptr.get() });

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

        let mut evaluated_steps = 0u32;
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
                && step.inputs.iter().all(|&(_, resource)| {
                    self.backend.slot_for(resource)
                        .and_then(|slot| self.slot_content_versions.get(slot.0 as usize))
                        .is_some_and(Option::is_some)
                        && !self.mesh_pending.get(resource.0 as usize).copied().unwrap_or(true)
                })
                && self.outputs_retained(step)
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
                // Re-publish the held logical metadata to the slots that
                // consumers will read. The authority remains unchanged, but
                // pool rebinding must never expose a stale occupant token.
                self.commit_mesh_revisions(plan, step, true, None);
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
                        self.commit_mesh_revisions(plan, step, true, None);
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
                let slot = if plan.is_provided_texture(res_id) {
                    self.backend.acquire_provided_texture(res_id, ty, fmt, dims)
                } else {
                    self.backend.acquire(res_id, ty, fmt, dims)
                };
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
            let mut selected_input_resource: Option<ResourceId> = None;
            if let Some(inst) = graph.get_node_mut(step.node) {
                if inst.node.is_pure() {
                    executed_pure_epoch = Some(inst.param_epoch);
                    // Pure/fused transforms preserve semantic content when
                    // their complete input set and parameters are unchanged,
                    // even when transient output storage still needs a write.
                    // This does not expand the plan's held-resource set.
                    self.node_content_unchanged[idx] = self.step_memo[idx].as_ref().is_some_and(|memo| {
                        memo.param_epoch == inst.param_epoch
                            && memo.input_epochs.len() == step.inputs.len()
                            && step.inputs.iter().zip(&memo.input_epochs).all(|(&(_, resource), &epoch)| {
                                self.resource_epoch.get(&resource).copied() == Some(epoch)
                                    && self.backend.slot_for(resource)
                                        .and_then(|slot| self.slot_content_versions.get(slot.0 as usize))
                                        .is_some_and(Option::is_some)
                                    && !self.mesh_pending.get(resource.0 as usize).copied().unwrap_or(true)
                            })
                    });
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
                    selected_input_resource = inst
                        .node
                        .selected_input_branch(&inst.params, &self.wired_scratch)
                        .and_then(|port| {
                            step.inputs
                                .iter()
                                .find(|&&(name, _)| name == port)
                                .map(|&(_, resource)| resource)
                        });
                    inst.node.skip_passthrough(&inst.params, &self.wired_scratch)
                };
                let mut performed_alias = false;
                let mut copied_passthrough = false;
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
                    let compatible = |i: Slot, o: Slot| {
                        if data_skip {
                            return true;
                        }
                        let res_of = |list: &[(&'static str, ResourceId)], port: &str| {
                            list.iter().find(|&&(n, _)| n == port).map(|&(_, r)| r)
                        };
                        let (Some(r_in), Some(r_out)) = (
                            res_of(&step.inputs, in_port),
                            res_of(&step.outputs, out_port),
                        ) else {
                            return false;
                        };
                        let plan_compatible = resolve_dims(plan, r_in, canvas_dims)
                            == resolve_dims(plan, r_out, canvas_dims)
                            && plan.resource_format(r_in) == plan.resource_format(r_out);
                        // The plan's format declaration can be absent on an
                        // inherited/default edge while the allocated textures
                        // still have the same concrete format. Prefer the
                        // bound textures when both are exposed by the backend.
                        match (self.backend.texture_2d(i), self.backend.texture_2d(o)) {
                            (Some(src), Some(dst)) => {
                                src.width == dst.width
                                    && src.height == dst.height
                                    && src.format == dst.format
                            }
                            _ => plan_compatible,
                        }
                    };
                    let compatible = match (in_slot, out_slot) {
                        (Some(i), Some(o)) => compatible(i, o),
                        _ => false,
                    };
                    if let (Some(i), Some(o)) = (in_slot, out_slot)
                        && compatible
                        && self.backend.alias_2d(i, o)
                    {
                        performed_alias = true;
                        // Propagate the empty mark through a data-skip alias
                        // so a chain of declaring draw nodes each skips.
                        if data_skip {
                            for &(_, res) in &step.outputs {
                                self.empty_resources.insert(res);
                            }
                        }
                        let r_in = step
                            .inputs
                            .iter()
                            .find(|&&(n, _)| n == in_port)
                            .map(|&(_, r)| r);
                        if let Some(r) = r_in {
                            let source_storage = self
                                .slot_generations
                                .get(i.0 as usize)
                                .copied()
                                .map(StorageRevision)
                                .unwrap_or(StorageRevision(0));
                            let source_content = self
                                .slot_content_versions
                                .get(i.0 as usize)
                                .copied()
                                .flatten();
                            let prev = self.alias_propagation_state[idx];
                            self.alias_propagation_state[idx] = Some(AliasPropagationState {
                                source: r,
                                destination: o,
                                source_storage,
                                source_content,
                            });
                            // A missing content token is deliberately not
                            // stable. It represents pending or externally
                            // prebound bytes, never a synthesized zero.
                            if prev.is_some_and(|state| {
                                state.source == r
                                    && state.source_content.is_some()
                                    && state.source_content == source_content
                            }) {
                                self.node_content_unchanged[idx] = true;
                            }
                            // Physical no-write propagation retains its
                            // established destination and storage guards.
                            if !data_skip
                                && source_content.is_some()
                                && self.outputs_retained(step)
                                && prev.is_some_and(|state| {
                                    state.source == r
                                        && state.destination == o
                                        && state.source_storage == source_storage
                                        && state.source_content == source_content
                                })
                            {
                                self.node_declared_unchanged[idx] = true;
                            }
                        }
                    }
                    if !performed_alias
                        && !data_skip
                        && let (Some(i), Some(o), Some(g)) = (in_slot, out_slot, gpu.as_deref_mut())
                        && compatible
                        && let (Some(src), Some(dst)) =
                            (self.backend.texture_2d(i), self.backend.texture_2d(o))
                    {
                        // A real backend can refuse aliasing when the
                        // destination is borrowed by the host. Preserve the
                        // no-op contract with a same-format blit, while
                        // retaining evaluation for genuine resampling cases.
                        g.copy_texture_to_texture(src, dst, dst.width, dst.height);
                        copied_passthrough = true;
                    }
                    if copied_passthrough {
                        // A backend may refuse aliasing when the destination is
                        // host-borrowed. The copy still carries the selected
                        // input's logical identity and participates in the same
                        // next-frame content comparison.
                        if let (Some(i), Some(o), Some(&(_, r))) = (
                            in_slot,
                            out_slot,
                            step.inputs.iter().find(|&&(n, _)| n == in_port),
                        ) {
                            let source_storage = self
                                .slot_generations
                                .get(i.0 as usize)
                                .copied()
                                .map(StorageRevision)
                                .unwrap_or(StorageRevision(0));
                            let source_content = self
                                .slot_content_versions
                                .get(i.0 as usize)
                                .copied()
                                .flatten();
                            let prev = self.alias_propagation_state[idx];
                            self.alias_propagation_state[idx] = Some(AliasPropagationState {
                                source: r,
                                destination: o,
                                source_storage,
                                source_content,
                            });
                            if prev.is_some_and(|state| {
                                state.source == r
                                    && state.source_content.is_some()
                                    && state.source_content == source_content
                            }) {
                                self.node_content_unchanged[idx] = true;
                            }
                        }
                    }
                }
                if performed_alias || copied_passthrough {
                    // The alias/copy has no independent pending declaration.
                    // Its selected logical input supplies readiness at commit.
                    for &(_, resource) in &step.outputs {
                        if let Some(slot) = self.backend.slot_for(resource) {
                            let index = slot.0 as usize;
                            if self.slot_pending.len() <= index { self.slot_pending.resize(index + 1, false); }
                            self.slot_pending[index] = false;
                        }
                    }
                }
                let outputs_retained = self.outputs_retained(step);
                if !performed_alias && !copied_passthrough {
                    self.alias_propagation_state[idx] = None;
                }

                if !performed_alias && !copied_passthrough {
                    self.scalar_write_scratch.clear();
                    self.camera_write_scratch.clear();
                    self.light_write_scratch.clear();
                    self.material_write_scratch.clear();
                    self.transform_write_scratch.clear();
                    self.atmosphere_write_scratch.clear();
                    self.render_mode_write_scratch.clear();
                    self.rigid_body_write_scratch.clear();
                    self.object_write_scratch.clear();
                    self.error_scratch.clear();
                    {
                        let backend_ref: &dyn Backend = &*self.backend;
                        let inputs = NodeInputs::new(&self.input_scratch, backend_ref, &self.slot_generations)
                            .with_pending(&self.slot_pending)
                            .with_mesh_revisions(&self.slot_mesh_revisions)
                            .with_content_versions(&self.slot_content_versions);
                        let outputs = NodeOutputs::new(
                            &self.output_scratch,
                            backend_ref,
                            &mut self.scalar_write_scratch,
                            &mut self.camera_write_scratch,
                            &mut self.light_write_scratch,
                            &mut self.material_write_scratch,
                            &mut self.transform_write_scratch,
                            &mut self.atmosphere_write_scratch,
                            &mut self.render_mode_write_scratch,
                            &mut self.object_write_scratch,
                        ).with_rigid_body_writes(&mut self.rigid_body_write_scratch);
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
                            self.rt_quality,
                            layer_skin_registry,
                        )
                        .with_errors(&mut self.error_scratch)
                        .with_outputs_retained(outputs_retained);
                        let has_gpu_binding = ctx.gpu.is_some();
                        inst.node.evaluate(&mut ctx);
                        debug_assert!(
                            !has_gpu_binding
                                || !ctx.outputs_unchanged
                                || ctx.outputs_retained(),
                            "node `{}` declared physical outputs unchanged without retained output storage",
                            inst.node.type_id().as_str(),
                        );
                        evaluated_steps += 1;
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
                        self.node_content_unchanged[idx] |= ctx.output_content_unchanged;
                        // Content availability: rewrite this step's output
                        // slots from its latest declaration (default ready).
                        // A slot's producer is the single writer of its
                        // flag, so a stale `true` can only survive while
                        // the producer itself is skipped.
                        let declared_pending = ctx.outputs_pending || inst.node.io_pending();
                        for &(_, res) in &step.outputs {
                            if let Some(slot) = self.backend.slot_for(res) {
                                let slot_idx = slot.0 as usize;
                                if self.slot_pending.len() <= slot_idx {
                                    self.slot_pending.resize(slot_idx + 1, false);
                                }
                                self.slot_pending[slot_idx] = declared_pending;
                            }
                        }
                    }
                    // Publish before revision commit and downstream reads.
                    for &(port, slot) in &self.output_scratch {
                        if self.backend.provided_texture_descriptor(slot).is_some() {
                            if let Some(texture) = inst.node.provided_texture_output(port) {
                                self.backend.install_provided_texture(slot, texture);
                            } else {
                                assert!(gpu.is_none(), "node-owned texture output was not provided");
                            }
                        }
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
                    // RenderMode writes use the same drain shape.
                    for (slot, value) in self.render_mode_write_scratch.drain(..) {
                        self.backend.set_render_mode(slot, value);
                    }
                    for (slot, value) in self.rigid_body_write_scratch.drain(..) {
                        self.backend.set_rigid_body(slot, value);
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

            // Storage freshness advances independently of semantic content:
            // a physical recopy must remain visible to binding/copy guards.
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
            for &(_, res) in &step.outputs {
                let state = self.storage_snapshot(res);
                if let Some(previous) = self.resource_storage_state.get_mut(res.0 as usize) {
                    *previous = state;
                }
            }

            // SCENE_MODIFIER_RT_DESIGN.md §3.2: commit mesh revisions at
            // the same choke point. Token issuance honors the same skip
            // condition (`node_declared_unchanged` retains revisions);
            // the slot snapshot publish runs either way so pool rebinds
            // never leave a slot reading another resource's revision.
            self.commit_mesh_revisions(
                plan,
                step,
                self.node_content_unchanged[idx] || self.node_declared_unchanged[idx],
                self.alias_propagation_state[idx]
                    .map(|state| state.source)
                    .or(selected_input_resource),
            );

            // Memo dependencies advance only when logical content changes.
            // Pure steps then snapshot the input epochs they
            // ran with (the clean-skip compares against this next frame);
            // non-pure steps clear any stale memo. Reuse the input-epoch
            // storage on dirty frames as well as on steady unchanged frames.
            for &res in &self.content_changed_resources {
                *self.resource_epoch.entry(res).or_insert(0) += 1;
            }
            if let Some(param_epoch) = executed_pure_epoch {
                let memo = self.step_memo[idx].get_or_insert_with(|| StepMemo {
                    param_epoch,
                    input_epochs: Vec::with_capacity(step.inputs.len()),
                });
                memo.param_epoch = param_epoch;
                memo.input_epochs.clear();
                memo.input_epochs.extend(step.inputs.iter().map(|&(_, res)| {
                    self.resource_epoch.get(&res).copied().unwrap_or(0)
                }));
            } else {
                self.step_memo[idx] = None;
            }

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

        if evaluated_steps == 0
            && gpu.is_some()
            && std::env::var("MANIFOLD_LOG_REBUILD_REASON").is_ok()
        {
            eprintln!(
                "[rebuild] scope=executor reason=zero-steps-evaluated steps={}",
                plan.steps().len(),
            );
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

            let capture_outputs_retained = self.outputs_retained(step);
            if let Some(inst) = graph.get_node_mut(step.node) {
                self.scalar_write_scratch.clear();
                self.camera_write_scratch.clear();
                self.light_write_scratch.clear();
                self.material_write_scratch.clear();
                self.transform_write_scratch.clear();
                self.atmosphere_write_scratch.clear();
                self.render_mode_write_scratch.clear();
                self.object_write_scratch.clear();
                self.error_scratch.clear();
                let backend_ref: &dyn Backend = &*self.backend;
                let inputs = NodeInputs::new(&self.input_scratch, backend_ref, &self.slot_generations)
                    .with_pending(&self.slot_pending)
                    .with_mesh_revisions(&self.slot_mesh_revisions)
                    .with_content_versions(&self.slot_content_versions);
                let outputs = NodeOutputs::new(
                    &self.output_scratch,
                    backend_ref,
                    &mut self.scalar_write_scratch,
                    &mut self.camera_write_scratch,
                    &mut self.light_write_scratch,
                    &mut self.material_write_scratch,
                    &mut self.transform_write_scratch,
                    &mut self.atmosphere_write_scratch,
                    &mut self.render_mode_write_scratch,
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
                    self.rt_quality,
                    layer_skin_registry,
                )
                .with_errors(&mut self.error_scratch)
                .with_outputs_retained(capture_outputs_retained);
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
                    if swapped && let Some(slot) = in_slot {
                        if let Some(snapshot) = self.slot_content_versions.get_mut(slot.0 as usize) {
                            *snapshot = None;
                        }
                        if let Some(revision) = self.slot_generations.get_mut(slot.0 as usize) {
                            *revision += 1;
                        }
                    }
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
                // late_capture may swap or copy bytes into a persistent
                // state output after the normal commit point. Its logical
                // publication is therefore unknown until the producer runs
                // again; clear the physical snapshots so consumers cannot
                // cache against stale content metadata.
                for &(_, res) in &step.outputs {
                    if let Some(slot) = self.backend.slot_for(res)
                        && let Some(snapshot) = self.slot_content_versions.get_mut(slot.0 as usize)
                    {
                        *snapshot = None;
                        if let Some(revision) = self.slot_generations.get_mut(slot.0 as usize) {
                            *revision += 1;
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

    /// A Texture2D producer whose pending declaration is driven by a
    /// shared flag — stands in for `gltf_mesh_source` mid-parse.
    struct PendingSourceNode {
        type_id: EffectNodeType,
        declare_pending: Arc<Mutex<bool>>,
    }

    impl EffectNode for PendingSourceNode {
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
            if *self.declare_pending.lock().unwrap() {
                ctx.mark_outputs_pending();
            }
        }
    }

    /// Records `slot_content_ready` of its "in" port on every evaluate.
    struct ReadinessObservingNode {
        type_id: EffectNodeType,
        log: Arc<Mutex<Vec<bool>>>,
    }

    impl EffectNode for ReadinessObservingNode {
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
            let ready = ctx
                .inputs
                .slot("in")
                .map(|s| ctx.inputs.slot_content_ready(s))
                .expect("wired input must resolve to a slot");
            self.log.lock().unwrap().push(ready);
        }
    }

    /// A producer's pending declaration must reach the consumer in the
    /// SAME frame (topological order), persist across frames while the
    /// producer keeps declaring, and reset to ready on the first evaluate
    /// that stops declaring — the contract render_scene's not-ready
    /// object gate relies on.
    #[test]
    fn pending_declaration_reaches_consumers_and_resets() {
        let declare_pending = Arc::new(Mutex::new(true));
        let log = Arc::new(Mutex::new(Vec::new()));

        let mut g = Graph::new();
        let src = g.add_node(Box::new(PendingSourceNode {
            type_id: EffectNodeType::new("test.pending_source"),
            declare_pending: declare_pending.clone(),
        }));
        let observer = g.add_node(Box::new(ReadinessObservingNode {
            type_id: EffectNodeType::new("test.readiness_observer"),
            log: log.clone(),
        }));
        g.connect((src, "out"), (observer, "in")).unwrap();
        let plan = compile(&g).unwrap();
        let mut exec = Executor::new(Box::new(crate::node_graph::MockBackend::new()));

        exec.execute_frame(&mut g, &plan, frame_time());
        exec.execute_frame(&mut g, &plan, frame_time());
        *declare_pending.lock().unwrap() = false;
        exec.execute_frame(&mut g, &plan, frame_time());

        let log = log.lock().unwrap();
        assert_eq!(log.as_slice(), &[false, false, true]);
    }

    /// SCENE_MODIFIER_RT_DESIGN.md §3.2 — executor mesh revision and
    /// pending semantics over `Array(MeshVertex)` resources, proven
    /// without a GPU. The fixture drives per-frame declarations through
    /// shared flags, the same shape the production gated sources
    /// (gltf_mesh_source and friends) use.
    mod mesh_revision_tests {
        use super::*;
        use crate::generators::mesh_common::MeshVertex;
        use crate::node_graph::mesh_change::{MeshOutputRule, MeshRevisionRule};
        use crate::node_graph::ports::ArrayType;

        fn mesh_ty() -> PortType {
            PortType::Array(ArrayType::of_known::<MeshVertex>())
        }

        /// `MeshVertex`-layout producer/consumer with scripted
        /// write/unchanged/pending declarations and an optional
        /// `mesh_output_rule` override (None = trait default, the
        /// conservative `Written`/`Written`). The rule lives behind a
        /// shared handle because the compiled rule is a plan-compile-time
        /// snapshot — a test that flips the source's topology mid-run
        /// recompiles the plan with the handle changed.
        struct MeshNode {
            type_id: EffectNodeType,
            inputs: Vec<NodeInput>,
            outputs: Vec<NodeOutput>,
            declare_unchanged: Arc<Mutex<bool>>,
            declare_pending: Arc<Mutex<bool>>,
            rule: Arc<Mutex<Option<MeshOutputRule<'static>>>>,
        }

        fn shared_rule(rule: Option<MeshOutputRule<'static>>) -> Arc<Mutex<Option<MeshOutputRule<'static>>>> {
            Arc::new(Mutex::new(rule))
        }

        /// Handles a producer hands back: unchanged/pending declaration
        /// flags plus the shared rule handle (see [`MeshNode`]).
        type ProducerHandles = (
            Arc<Mutex<bool>>,
            Arc<Mutex<bool>>,
            Arc<Mutex<Option<MeshOutputRule<'static>>>>,
        );

        impl MeshNode {
            fn producer(rule: Option<MeshOutputRule<'static>>) -> (Self, ProducerHandles) {
                let declare_unchanged = Arc::new(Mutex::new(false));
                let declare_pending = Arc::new(Mutex::new(false));
                let rule = shared_rule(rule);
                (
                    Self {
                        type_id: EffectNodeType::new("test.mesh_node"),
                        inputs: vec![],
                        outputs: vec![output("out", mesh_ty())],
                        declare_unchanged: declare_unchanged.clone(),
                        declare_pending: declare_pending.clone(),
                        rule: rule.clone(),
                    },
                    (declare_unchanged, declare_pending, rule),
                )
            }

            fn consumer(rule: Option<MeshOutputRule<'static>>) -> (Self, Arc<Mutex<bool>>, Arc<Mutex<bool>>) {
                let declare_unchanged = Arc::new(Mutex::new(false));
                let declare_pending = Arc::new(Mutex::new(false));
                (
                    Self {
                        type_id: EffectNodeType::new("test.mesh_consumer"),
                        inputs: vec![input("in", mesh_ty(), true)],
                        outputs: vec![output("out", mesh_ty())],
                        declare_unchanged: declare_unchanged.clone(),
                        declare_pending: declare_pending.clone(),
                        rule: shared_rule(rule),
                    },
                    declare_unchanged,
                    declare_pending,
                )
            }

            /// Terminal mesh consumer: an input and no outputs. Plan
            /// compile prunes UNCONSUMED outputs from a step, so every
            /// producer under test needs its mesh output wired somewhere
            /// to keep its resource in the plan.
            fn sink() -> Self {
                Self {
                    type_id: EffectNodeType::new("test.mesh_sink"),
                    inputs: vec![input("in", mesh_ty(), true)],
                    outputs: vec![],
                    declare_unchanged: Arc::new(Mutex::new(false)),
                    declare_pending: Arc::new(Mutex::new(false)),
                    rule: shared_rule(None),
                }
            }
        }

        impl EffectNode for MeshNode {
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
            fn mesh_output_rule(&self, _port: &str) -> MeshOutputRule<'_> {
                self.rule.lock().unwrap().unwrap_or(MeshOutputRule {
                    topology: MeshRevisionRule::Written,
                    positions: MeshRevisionRule::Written,
                })
            }
            fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
                if *self.declare_unchanged.lock().unwrap() {
                    ctx.mark_outputs_unchanged();
                }
                if *self.declare_pending.lock().unwrap() {
                    ctx.mark_outputs_pending();
                }
            }
        }

        /// The single output resource of `node`, the way production
        /// callers address plan resources.
        fn out_res(plan: &ExecutionPlan, node: NodeInstanceId) -> ResourceId {
            plan.steps()
                .iter()
                .find(|s| s.node == node)
                .and_then(|s| s.outputs.first())
                .map(|&(_, res)| res)
                .expect("node must have one output resource")
        }

        fn fixed_rule() -> MeshOutputRule<'static> {
            MeshOutputRule {
                topology: MeshRevisionRule::Fixed,
                positions: MeshRevisionRule::Fixed,
            }
        }

        /// A1: an undeclared mesh writer compiles to the conservative
        /// `Written`/`Written` rule and every actual write issues fresh
        /// topology/positions/content tokens.
        #[test]
        fn mesh_change_default_is_conservative() {
            let (node, (_unchanged, _pending, _rule)) = MeshNode::producer(None);
            let mut g = Graph::new();
            let n = g.add_node(Box::new(node));
            let sink = g.add_node(Box::new(MeshNode::sink()));
            g.connect((n, "out"), (sink, "in")).unwrap();
            let plan = compile(&g).unwrap();
            let res = out_res(&plan, n);

            let compiled = plan.mesh_rule(res).expect("MeshVertex output must compile a mesh rule");
            assert!(
                matches!(compiled.topology, CompiledMeshRevisionRule::Written)
                    && matches!(compiled.positions, CompiledMeshRevisionRule::Written),
                "undeclared writer must compile to Written/Written, got {compiled:?}"
            );

            let mut exec = Executor::with_mock();
            exec.execute_frame(&mut g, &plan, frame_time());
            let r1 = exec.mesh_revision_of_res(res);
            assert!(r1.topology > 0 && r1.positions > 0 && r1.content > 0);
            assert_eq!(
                (r1.topology, r1.positions, r1.content),
                (r1.content, r1.content, r1.content),
                "a default-rule write issues one shared token for all three aspects"
            );

            exec.execute_frame(&mut g, &plan, frame_time());
            let r2 = exec.mesh_revision_of_res(res);
            assert!(
                r2.topology > r1.topology
                    && r2.positions > r1.positions
                    && r2.content > r1.content,
                "every actual write must revise all three aspects, got {r1:?} then {r2:?}"
            );
        }

        /// A1: a truthful `mark_outputs_unchanged` retains all three
        /// revisions on skipped frames; the next actual write issues
        /// fresh tokens again.
        #[test]
        fn mesh_change_written_honors_unchanged_declaration() {
            let (node, (unchanged, _pending, _rule)) = MeshNode::producer(None);
            let mut g = Graph::new();
            let n = g.add_node(Box::new(node));
            let sink = g.add_node(Box::new(MeshNode::sink()));
            g.connect((n, "out"), (sink, "in")).unwrap();
            let plan = compile(&g).unwrap();
            let res = out_res(&plan, n);

            let mut exec = Executor::with_mock();
            exec.execute_frame(&mut g, &plan, frame_time());
            let written = exec.mesh_revision_of_res(res);
            assert!(written.topology > 0, "first write must issue a token");

            *unchanged.lock().unwrap() = true;
            exec.execute_frame(&mut g, &plan, frame_time());
            let skipped = exec.mesh_revision_of_res(res);
            assert_eq!(
                skipped, written,
                "truthful unchanged declaration must retain all three revisions"
            );

            *unchanged.lock().unwrap() = false;
            exec.execute_frame(&mut g, &plan, frame_time());
            let rewritten = exec.mesh_revision_of_res(res);
            assert!(
                rewritten.topology > written.topology
                    && rewritten.positions > written.positions
                    && rewritten.content > written.content,
                "the write after a skip must issue fresh tokens, got {written:?} then {rewritten:?}"
            );
        }

        /// A1: a producer's pending declaration reaches every downstream
        /// mesh consumer's logical resource and persists while declared;
        /// pending is independent of revision tokens — revisions keep
        /// advancing while the resource stays pending.
        #[test]
        fn mesh_change_pending_propagates_through_mesh_lineage() {
            let (src, (_src_unchanged, src_pending, _rule)) = MeshNode::producer(None);
            let (consumer, _c_unchanged, _c_pending) = MeshNode::consumer(None);
            let mut g = Graph::new();
            let a = g.add_node(Box::new(src));
            let b = g.add_node(Box::new(consumer));
            let sink = g.add_node(Box::new(MeshNode::sink()));
            g.connect((a, "out"), (b, "in")).unwrap();
            g.connect((b, "out"), (sink, "in")).unwrap();
            let plan = compile(&g).unwrap();
            let (res_a, res_b) = (out_res(&plan, a), out_res(&plan, b));

            let mut exec = Executor::with_mock();
            *src_pending.lock().unwrap() = true;
            exec.execute_frame(&mut g, &plan, frame_time());
            assert!(exec.mesh_pending_of(res_a), "producer's own declaration must set its pending");
            assert!(
                exec.mesh_pending_of(res_b),
                "pending must propagate to the downstream mesh consumer"
            );
            let rev_frame1 = exec.mesh_revision_of_res(res_a);

            exec.execute_frame(&mut g, &plan, frame_time());
            assert!(
                exec.mesh_pending_of(res_a) && exec.mesh_pending_of(res_b),
                "pending must persist across frames while the producer keeps declaring"
            );
            assert!(
                exec.mesh_revision_of_res(res_a).topology > rev_frame1.topology,
                "pending is independent of revision tokens: actual writes still advance revisions"
            );

            *src_pending.lock().unwrap() = false;
            exec.execute_frame(&mut g, &plan, frame_time());
            assert!(
                !exec.mesh_pending_of(res_a) && !exec.mesh_pending_of(res_b),
                "stopping the declaration must return the whole lineage to ready"
            );
        }

        /// A1: a `Fixed`/`Fixed` rule keeps topology and position
        /// revisions across actual writes while content — which always
        /// revises on a write — still advances.
        #[test]
        fn mesh_change_fixed_rule_retains_revisions() {
            let (node, (_unchanged, _pending, _rule)) = MeshNode::producer(Some(fixed_rule()));
            let mut g = Graph::new();
            let n = g.add_node(Box::new(node));
            let sink = g.add_node(Box::new(MeshNode::sink()));
            g.connect((n, "out"), (sink, "in")).unwrap();
            let plan = compile(&g).unwrap();
            let res = out_res(&plan, n);

            let compiled = plan.mesh_rule(res).expect("MeshVertex output must compile a mesh rule");
            assert!(
                matches!(compiled.topology, CompiledMeshRevisionRule::Fixed)
                    && matches!(compiled.positions, CompiledMeshRevisionRule::Fixed),
                "override must compile to Fixed/Fixed, got {compiled:?}"
            );

            let mut exec = Executor::with_mock();
            exec.execute_frame(&mut g, &plan, frame_time());
            let r1 = exec.mesh_revision_of_res(res);

            exec.execute_frame(&mut g, &plan, frame_time());
            let r2 = exec.mesh_revision_of_res(res);
            assert_eq!(
                (r2.topology, r2.positions),
                (r1.topology, r1.positions),
                "Fixed aspects must retain their revisions across writes, got {r1:?} then {r2:?}"
            );
            assert!(
                r2.content > r1.content,
                "content always revises on an actual write, got {r1:?} then {r2:?}"
            );

            exec.execute_frame(&mut g, &plan, frame_time());
            let r3 = exec.mesh_revision_of_res(res);
            assert_eq!(
                (r3.topology, r3.positions),
                (r1.topology, r1.positions),
                "Fixed aspects must stay retained over repeated writes, got {r1:?} then {r3:?}"
            );
            assert!(r3.content > r2.content);
        }

        #[test]
        fn mesh_change_non_mesh_map_content_revises_topology() {
            use crate::node_graph::mesh_change::MeshDependency;
            static MAP_DEPENDENCY: [MeshDependency; 1] = [MeshDependency {
                input: std::borrow::Cow::Borrowed("in"),
                aspect: MeshAspect::Content,
            }];
            let map_ty = PortType::Array(ArrayType::of_known::<crate::generators::mesh_common::Vec4Vertex>());
            let (mut source, (unchanged, _, _)) = MeshNode::producer(None);
            source.outputs = vec![output("out", map_ty)];
            let (mut remap, _, _) = MeshNode::consumer(Some(MeshOutputRule {
                topology: MeshRevisionRule::Dependencies(&MAP_DEPENDENCY),
                positions: MeshRevisionRule::Written,
            }));
            remap.inputs = vec![input("in", map_ty, true)];
            let mut graph = Graph::new();
            let source = graph.add_node(Box::new(source));
            let remap = graph.add_node(Box::new(remap));
            let sink = graph.add_node(Box::new(MeshNode::sink()));
            graph.connect((source, "out"), (remap, "in")).unwrap();
            graph.connect((remap, "out"), (sink, "in")).unwrap();
            let plan = compile(&graph).unwrap();
            assert!(plan.mesh_rule(out_res(&plan, source)).is_none());
            let result = out_res(&plan, remap);
            let mut executor = Executor::with_mock();
            executor.execute_frame(&mut graph, &plan, frame_time());
            let first = executor.mesh_revision_of_res(result);
            *unchanged.lock().unwrap() = true;
            executor.execute_frame(&mut graph, &plan, frame_time());
            let reused = executor.mesh_revision_of_res(result);
            assert_eq!(first.topology, reused.topology);
            assert!(reused.positions > first.positions);
            *unchanged.lock().unwrap() = false;
            executor.execute_frame(&mut graph, &plan, frame_time());
            assert!(executor.mesh_revision_of_res(result).topology > reused.topology);
        }

        /// Wraps a real stock primitive so its DECLARED
        /// `mesh_output_rule` is compiled into the plan and driven
        /// through the executor on `MockBackend`. `evaluate` is a
        /// deliberate no-op write: the mock binds no GPU encoder, so the
        /// primitive's real `run()` (a compute dispatch) cannot execute
        /// here — the contract under test is the executor's revision
        /// commit, which the compiled rule drives, not the kernel.
        struct DeclaredPrimitiveProbe {
            inner: Box<dyn EffectNode>,
        }

        impl EffectNode for DeclaredPrimitiveProbe {
            fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
                self.inner.depth_rule()
            }
            fn type_id(&self) -> &EffectNodeType {
                self.inner.type_id()
            }
            fn inputs(&self) -> &[NodeInput] {
                self.inner.inputs()
            }
            fn outputs(&self) -> &[NodeOutput] {
                self.inner.outputs()
            }
            fn parameters(&self) -> &[ParamDef] {
                self.inner.parameters()
            }
            fn mesh_output_rule(&self, port: &str) -> MeshOutputRule<'_> {
                self.inner.mesh_output_rule(port)
            }
            fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {
                // No-op actual write — see the struct doc.
            }
        }

        /// P2c: the stock deformer declaration on `node.normal_wave_mesh`
        /// (topology = `Dependencies([in.Topology])`, positions =
        /// `Written`) makes the deformer's output topology revision
        /// follow the INPUT's topology revision — held while the source
        /// topology is stable, and revising again the moment the source
        /// topology starts changing — while positions and content
        /// advance on every write. The real `NormalWaveMesh` declaration
        /// is exercised through [`DeclaredPrimitiveProbe`] because the
        /// mock backend cannot run its compute dispatch (see the probe's
        /// doc).
        #[test]
        fn mesh_change_declared_deformer_tracks_input_topology() {
            use crate::node_graph::mesh_change::MeshAspect;
            use crate::node_graph::primitives::NormalWaveMesh;

            let (src, (_unchanged, _pending, src_rule)) = MeshNode::producer(Some(fixed_rule()));
            let mut g = Graph::new();
            let a = g.add_node(Box::new(src));
            let probe = g.add_node(Box::new(DeclaredPrimitiveProbe {
                inner: Box::new(NormalWaveMesh::new()),
            }));
            let sink = g.add_node(Box::new(MeshNode::sink()));
            g.connect((a, "out"), (probe, "in")).unwrap();
            g.connect((probe, "out"), (sink, "in")).unwrap();
            let plan = compile(&g).unwrap();
            let (res_src, res_out) = (out_res(&plan, a), out_res(&plan, probe));

            // The plan must have compiled the REAL declaration off the
            // stock primitive: topology depends on the wired input's
            // Topology aspect, positions are Written.
            let compiled = plan
                .mesh_rule(res_out)
                .expect("MeshVertex output must compile a mesh rule");
            match &compiled.topology {
                CompiledMeshRevisionRule::Dependencies(deps) => {
                    assert_eq!(
                        deps,
                        &[(res_src, MeshAspect::Topology)],
                        "declared deformer topology must watch the wired input's Topology"
                    );
                }
                other => panic!(
                    "declared deformer topology must be Dependencies([in.Topology]), got {other:?}"
                ),
            }
            assert!(
                matches!(compiled.positions, CompiledMeshRevisionRule::Written),
                "declared deformer positions must be Written, got {:?}",
                compiled.positions
            );

            let mut exec = Executor::with_mock();
            exec.execute_frame(&mut g, &plan, frame_time());
            let src_rev = exec.mesh_revision_of_res(res_src);
            let out_rev = exec.mesh_revision_of_res(res_out);
            // The source topology rule is Fixed, so its topology revision
            // retains 0 — content still advances on the write.
            assert!(src_rev.content > 0, "source write must issue a content token");
            assert!(out_rev.topology > 0, "deformer write must issue a topology token");

            // Phase 1: the source writes every frame (content advances)
            // with a Fixed topology rule — the declared deformer must
            // hold its topology revision while positions/content advance.
            exec.execute_frame(&mut g, &plan, frame_time());
            let src2 = exec.mesh_revision_of_res(res_src);
            let out2 = exec.mesh_revision_of_res(res_out);
            assert_eq!(src2.topology, src_rev.topology, "source topology is Fixed");
            assert_eq!(
                out2.topology, out_rev.topology,
                "declared deformer topology must track the input: unchanged while input topology is unchanged"
            );
            assert!(
                out2.positions > out_rev.positions && out2.content > out_rev.content,
                "positions/content advance on every write, got {out_rev:?} then {out2:?}"
            );

            exec.execute_frame(&mut g, &plan, frame_time());
            let out3 = exec.mesh_revision_of_res(res_out);
            assert_eq!(
                out3.topology, out_rev.topology,
                "declared deformer topology must keep tracking the still-stable input"
            );
            assert!(out3.positions > out2.positions && out3.content > out2.content);

            // Phase 2: the source topology starts changing (rule flips to
            // Written at plan recompile; revision state persists because
            // the plan shape is unchanged). The dependency must follow.
            *src_rule.lock().unwrap() = Some(MeshOutputRule {
                topology: MeshRevisionRule::Written,
                positions: MeshRevisionRule::Fixed,
            });
            let plan = compile(&g).unwrap();
            let (res_src, res_out) = (out_res(&plan, a), out_res(&plan, probe));
            let before = exec.mesh_revision_of_res(res_out);

            exec.execute_frame(&mut g, &plan, frame_time());
            let after1 = exec.mesh_revision_of_res(res_out);
            assert!(
                exec.mesh_revision_of_res(res_src).topology > src_rev.topology,
                "flipped source rule must revise its own topology"
            );
            assert!(
                after1.topology > before.topology,
                "input topology now changes every write — the declared dependency must follow, got {before:?} then {after1:?}"
            );
            assert!(after1.positions > before.positions);

            exec.execute_frame(&mut g, &plan, frame_time());
            let after2 = exec.mesh_revision_of_res(res_out);
            assert!(
                after2.topology > after1.topology,
                "tracking must persist frame over frame, got {after1:?} then {after2:?}"
            );
        }

        /// P2 (BUG-e3p6.4, design §3.3) — fused/unfused parity. The chain is
        /// P2 (BUG-e3p6.4, design §3.3) — fused/unfused parity on a REAL
        /// fused mesh kernel. The chain is two coincident `ripple_mesh`
        /// deformers — the mesh-deformer shape that fuses today (every
        /// stock deformer declaring `Dependencies` rules, wave/morph
        /// included, also declares a `weights_len` derived uniform with no
        /// registered recompute, so the fail-closed gate in
        /// `fuse_canonical_def_masked` keeps those regions unfused; see the
        /// report and the composition unit proof in freeze/install.rs).
        /// Ripple carries no mesh-rule declaration, so both sides compile
        /// the conservative Written/Written class: the fused node's
        /// installed sidecar must select exactly that class, and driving
        /// both graphs through the executor must show the class's behavior
        /// on both paths — every actual write revises all three aspects.
        #[test]
        fn mesh_change_fused_rules_match_unfused() {
            use crate::node_graph::freeze::install::{FusedDef, fuse_canonical_def};
            use crate::node_graph::mesh_change::{PreparedMeshRevisionRule, PreparedMeshRules};
            use crate::node_graph::persistence::EffectGraphDefExt;
            use crate::node_graph::PrimitiveRegistry;
            use manifold_core::NodeId;
            use manifold_core::effect_graph_def::EffectGraphDef;

            let json = r#"{
                "version": 1, "name": "p2_fused_parity",
                "nodes": [
                    { "id": 0, "typeId": "system.mesh_input", "nodeId": "mesh_in" },
                    { "id": 1, "typeId": "node.ripple_mesh", "nodeId": "r1" },
                    { "id": 2, "typeId": "node.ripple_mesh", "nodeId": "r2" },
                    { "id": 3, "typeId": "node.free_camera", "nodeId": "cam" },
                    { "id": 4, "typeId": "node.unlit_material", "nodeId": "mat" },
                    { "id": 5, "typeId": "node.render_mesh", "nodeId": "render" },
                    { "id": 6, "typeId": "system.final_output", "nodeId": "final" }
                ],
                "wires": [
                    { "fromNode": 0, "fromPort": "vertices", "toNode": 1, "toPort": "in" },
                    { "fromNode": 0, "fromPort": "weights", "toNode": 1, "toPort": "weights" },
                    { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                    { "fromNode": 0, "fromPort": "weights", "toNode": 2, "toPort": "weights" },
                    { "fromNode": 3, "fromPort": "out", "toNode": 5, "toPort": "camera" },
                    { "fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "material" },
                    { "fromNode": 2, "fromPort": "out", "toNode": 5, "toPort": "vertices" },
                    { "fromNode": 5, "fromPort": "color", "toNode": 6, "toPort": "in" }
                ]
            }"#;
            let def: EffectGraphDef = serde_json::from_str(json).unwrap();
            let registry = PrimitiveRegistry::with_builtin();

            // The scripted source replaces `system.mesh_input` (identical
            // ports) because MockBackend cannot run the real producers; its
            // no-op evaluate is an actual write every frame, matching the
            // MeshNode fixture contract.
            let swap_source =
                |graph: &mut Graph, rule: &Arc<Mutex<Option<MeshOutputRule<'static>>>>| {
                    let id = graph
                        .instance_by_node_id(&NodeId::new("mesh_in"))
                        .expect("mesh_input must instantiate");
                    graph.get_node_mut(id).unwrap().node =
                        Box::new(ScriptedMeshSource::new(Arc::clone(rule)));
                    id
                };

            // ── Path A: canonical def, empty sidecar (the unfused chain) ──
            let mut graph_a =
                def.clone().into_graph(&registry, &PreparedMeshRules::default()).unwrap();
            let _src_a = swap_source(&mut graph_a, &shared_rule(None));
            let r1_a = graph_a.instance_by_node_id(&NodeId::new("r1")).unwrap();
            let r2_a = graph_a.instance_by_node_id(&NodeId::new("r2")).unwrap();
            for id in [r1_a, r2_a] {
                let inner = std::mem::replace(
                    &mut graph_a.get_node_mut(id).unwrap().node,
                    Box::new(MeshNode::sink()),
                );
                graph_a.get_node_mut(id).unwrap().node = Box::new(DeclaredPrimitiveProbe { inner });
            }
            let plan_a = compile(&graph_a).unwrap();
            let res_r2 = out_res(&plan_a, r2_a);
            // No declaration on ripple: the conservative Written/Written class.
            let unfused_rule = plan_a.mesh_rule(res_r2).expect("r2 output compiles a mesh rule");
            assert!(
                matches!(unfused_rule.topology, CompiledMeshRevisionRule::Written)
                    && matches!(unfused_rule.positions, CompiledMeshRevisionRule::Written),
                "unfused ripple must compile to the conservative class, got {unfused_rule:?}"
            );

            // ── Path B: fuse the same def and install the sidecar ──
            let fused = fuse_canonical_def(&def, &registry)
                .expect("the ripple+ripple region must fuse");
            let fused_key = {
                let doc = fused
                    .def
                    .nodes
                    .iter()
                    .find(|n| n.type_id == "node.wgsl_compute")
                    .expect("the fused def carries the fused kernel node");
                if doc.node_id.is_empty() {
                    doc.handle.clone().expect("fused node carries an id or handle")
                } else {
                    doc.node_id.as_str().to_string()
                }
            };
            // The composed sidecar: Written/Written, matching the unfused
            // declarations — no silent class change from fusion.
            {
                let mut entries = fused.mesh_rules.values().flatten();
                let rule = entries.next().expect("the fused node carries a mesh-rule sidecar");
                assert!(
                    entries.next().is_none(),
                    "exactly one fused node carries mesh rules, got {:?}",
                    fused.mesh_rules
                );
                assert_eq!(rule.output, "dst", "single-output region emits dst, got {:?}", rule);
                assert!(
                    matches!(rule.topology, PreparedMeshRevisionRule::Written)
                        && matches!(rule.positions, PreparedMeshRevisionRule::Written),
                    "fused sidecar must compose to Written/Written, got {rule:?}"
                );
            }
            let FusedDef { def: fused_def, mesh_rules, .. } = fused;
            let mut graph_b = fused_def.into_graph(&registry, &mesh_rules).unwrap();
            let _src_b = swap_source(&mut graph_b, &shared_rule(None));
            let fused_rt = graph_b
                .instance_by_node_id(&NodeId::new(&fused_key))
                .expect("fused node must instantiate");
            {
                let inner = std::mem::replace(
                    &mut graph_b.get_node_mut(fused_rt).unwrap().node,
                    Box::new(MeshNode::sink()),
                );
                graph_b.get_node_mut(fused_rt).unwrap().node =
                    Box::new(DeclaredPrimitiveProbe { inner });
            }
            let plan_b = compile(&graph_b).unwrap();
            let res_fused = out_res(&plan_b, fused_rt);
            let fused_rule = plan_b
                .mesh_rule(res_fused)
                .expect("fused MeshVertex output compiles a mesh rule");
            assert!(
                matches!(fused_rule.topology, CompiledMeshRevisionRule::Written)
                    && matches!(fused_rule.positions, CompiledMeshRevisionRule::Written),
                "fused rule must match the unfused class, got {fused_rule:?}"
            );

            // ── Drive both graphs: same-class revision behavior every frame ──
            // Revision tokens come from a per-executor global counter, so
            // absolute values are not comparable across two executors (the
            // unfused graph has more mesh writers). The parity invariant is
            // behavioral: both sides advance ALL THREE aspects on EVERY
            // write — the conservative class's signature.
            fn run_frame_pair(
                graph_a: &mut Graph,
                plan_a: &ExecutionPlan,
                exec_a: &mut Executor,
                graph_b: &mut Graph,
                plan_b: &ExecutionPlan,
                exec_b: &mut Executor,
                res_unfused: ResourceId,
                res_fused: ResourceId,
            ) -> (MeshRevision, MeshRevision) {
                exec_a.execute_frame(graph_a, plan_a, frame_time());
                exec_b.execute_frame(graph_b, plan_b, frame_time());
                let a = exec_a.mesh_revision_of_res(res_unfused);
                let b = exec_b.mesh_revision_of_res(res_fused);
                (a, b)
            }
            let mut exec_a = Executor::with_mock();
            let mut exec_b = Executor::with_mock();
            let (first_a, first_b) = run_frame_pair(
                &mut graph_a, &plan_a, &mut exec_a,
                &mut graph_b, &plan_b, &mut exec_b,
                res_r2, res_fused,
            );
            assert!(
                first_a.topology > 0 && first_b.topology > 0,
                "the first write must issue a topology token on both paths, got {first_a:?} / {first_b:?}"
            );
            let (mut prev_a, mut prev_b) = (first_a, first_b);
            for _ in 1..4 {
                let (next_a, next_b) = run_frame_pair(
                    &mut graph_a, &plan_a, &mut exec_a,
                    &mut graph_b, &plan_b, &mut exec_b,
                    res_r2, res_fused,
                );
                for (side, next, prev) in [
                    ("unfused", next_a, prev_a),
                    ("fused", next_b, prev_b),
                ] {
                    assert!(
                        next.topology > prev.topology
                            && next.positions > prev.positions
                            && next.content > prev.content,
                        "{side}: the conservative class must revise all aspects on every \
                         write, got {prev:?} then {next:?}"
                    );
                }
                (prev_a, prev_b) = (next_a, next_b);
            }
        }
        /// P2 acceptance (BUG-e3p6.4, design §7): the stock Surface Waves
        /// modifiers must select the refit-eligible update class — topology
        /// driven by Topology-only input dependencies, positions Written —
        /// so a fused path (where it exists) can never degrade below the
        /// unfused class. Two parts:
        ///
        /// 1. The unfused oracle: the bundled preset's own member atoms
        ///    (`normal_wave_mesh`, `morph_mesh`) declare the class plan
        ///    compilation reads straight off the node.
        /// 2. The real preset graph, embedded VERBATIM (bundled group JSON)
        ///    in a production-shaped host def (mesh inputs + scalar values +
        ///    render tail — the shape a scene render view gives it). The
        ///    host MUST fuse now: every weights-carrying deformer has a
        ///    registered `weights_len` recompute whose marker carries the
        ///    member→fused-port mapping, and buffer regions admit the mask's
        ///    unwired optional coincident `weights` (BUG-7wwy + BUG-jwyh).
        ///    The composed §3.3 sidecar must keep the refit-eligible class.
        ///
        /// The fused-path executor parity on the fusing chain is proven on
        /// GPU in `tests/gpu_proofs/rt_dynamic_fusion.rs`.
        #[test]
        fn mesh_change_surface_waves_fused_sidecar_is_refit_eligible() {
            use crate::node_graph::bundled_presets::bundled_preset_json;
            use crate::node_graph::freeze::install::fuse_canonical_def;
            use crate::node_graph::mesh_change::{
                PreparedMeshOutputRule, PreparedMeshRevisionRule,
            };
            use crate::node_graph::persistence::EffectGraphDefExt;
            use crate::node_graph::primitive::Primitive;
            use crate::node_graph::PrimitiveRegistry;
            use manifold_core::PresetTypeId;
            use manifold_core::effect_graph_def::EffectGraphDef;

            let json = bundled_preset_json(&PresetTypeId::new("SurfaceWaves"))
                .expect("SurfaceWaves is a bundled scene-modifier preset");
            let registry = PrimitiveRegistry::with_builtin();

            // Part 1 — the unfused class, straight off the stock declarations
            // the preset's graph compiles today.
            let wave_node = crate::node_graph::primitives::NormalWaveMesh::new();
            let wave = Primitive::mesh_output_rule(&wave_node, "out");
            match wave.topology {
                MeshRevisionRule::Dependencies(deps) => {
                    assert_eq!(deps.len(), 1);
                    assert_eq!(deps[0].aspect, MeshAspect::Topology);
                }
                other => panic!("wave topology must be Dependencies([in.Topology]), got {other:?}"),
            }
            assert!(matches!(wave.positions, MeshRevisionRule::Written));
            let morph_node = crate::node_graph::primitives::MorphMesh::new();
            let morph = Primitive::mesh_output_rule(&morph_node, "out");
            match morph.topology {
                MeshRevisionRule::Dependencies(deps) => {
                    assert_eq!(deps.len(), 2);
                    assert!(deps.iter().all(|d| d.aspect == MeshAspect::Topology));
                }
                other => panic!(
                    "morph topology must be Dependencies([in.Topology, b.Topology]), got {other:?}"
                ),
            }
            assert!(matches!(morph.positions, MeshRevisionRule::Written));

            // Part 2 — the verbatim bundled group in a production-shaped
            // host. (Standalone the bundled JSON cannot fuse at all: fusion
            // liveness seeds from system.final_output, which only a render
            // host provides.)
            let preset: serde_json::Value = serde_json::from_str(&json).unwrap();
            let mut group = preset["nodes"][0].clone();
            group["id"] = serde_json::json!(1);
            let host = serde_json::json!({
                "version": 1,
                "name": "surface_waves_host",
                "nodes": [
                    { "id": 0, "typeId": "system.mesh_input", "nodeId": "mesh_in" },
                    group,
                    { "id": 2, "typeId": "system.mesh_input", "nodeId": "mesh_ref" },
                    { "id": 3, "typeId": "node.value", "nodeId": "radius",
                      "params": { "value": { "type": "Float", "value": 1.0 } } },
                    { "id": 4, "typeId": "node.value", "nodeId": "off_x",
                      "params": { "value": { "type": "Float", "value": 0.0 } } },
                    { "id": 5, "typeId": "node.value", "nodeId": "off_y",
                      "params": { "value": { "type": "Float", "value": 0.0 } } },
                    { "id": 6, "typeId": "node.value", "nodeId": "off_z",
                      "params": { "value": { "type": "Float", "value": 0.0 } } },
                    { "id": 9, "typeId": "node.free_camera", "nodeId": "cam" },
                    { "id": 10, "typeId": "node.unlit_material", "nodeId": "mat" },
                    { "id": 11, "typeId": "node.render_mesh", "nodeId": "render" },
                    { "id": 12, "typeId": "system.final_output", "nodeId": "final" }
                ],
                "wires": [
                    { "fromNode": 0, "fromPort": "vertices", "toNode": 1, "toPort": "current" },
                    { "fromNode": 2, "fromPort": "vertices", "toNode": 1, "toPort": "reference" },
                    { "fromNode": 3, "fromPort": "out", "toNode": 1, "toPort": "sourceRadius" },
                    { "fromNode": 4, "fromPort": "out", "toNode": 1, "toPort": "sourceOffsetX" },
                    { "fromNode": 5, "fromPort": "out", "toNode": 1, "toPort": "sourceOffsetY" },
                    { "fromNode": 6, "fromPort": "out", "toNode": 1, "toPort": "sourceOffsetZ" },
                    { "fromNode": 9, "fromPort": "out", "toNode": 11, "toPort": "camera" },
                    { "fromNode": 10, "fromPort": "out", "toNode": 11, "toPort": "material" },
                    { "fromNode": 1, "fromPort": "vertices", "toNode": 11, "toPort": "vertices" },
                    { "fromNode": 11, "fromPort": "color", "toNode": 12, "toPort": "in" }
                ]
            });
            let host_def: EffectGraphDef = serde_json::from_value(host).unwrap();

            // The mask fusion gap is closed (BUG-7wwy + BUG-jwyh): the host
            // must fuse, and the composed sidecar must keep the
            // refit-eligible class (Topology-only Dependencies, Written
            // positions), same as the unfused declarations above.
            let fused = fuse_canonical_def(&host_def, &registry).expect(
                "the production-shaped Surface Waves host must fuse: every \
                 weights-carrying deformer has a registered weights_len \
                 recompute and buffer regions admit the mask's unwired \
                 optional coincident weights (BUG-7wwy, BUG-jwyh)",
            );
            {
                let rules: Vec<&PreparedMeshOutputRule> =
                    fused.mesh_rules.values().flatten().collect();
                assert!(
                    !rules.is_empty(),
                    "fused Surface Waves must carry a mesh-rule sidecar for its mesh output"
                );
                for rule in &rules {
                    match &rule.topology {
                        PreparedMeshRevisionRule::Dependencies(deps) => {
                            assert!(
                                !deps.is_empty()
                                    && deps.iter().all(|d| d.aspect == MeshAspect::Topology),
                                "every composed leaf must be a Topology aspect, got {deps:?}"
                            );
                        }
                        other => panic!(
                            "the fused mesh output must stay refit-eligible (Dependencies), got {other:?}"
                        ),
                    }
                    assert!(
                        matches!(rule.positions, PreparedMeshRevisionRule::Written),
                        "morph positions stay Written under fusion, got {:?}",
                        rule.positions
                    );
                }
                let graph = fused.def.into_graph(&registry, &fused.mesh_rules).unwrap();
                let plan = compile(&graph).unwrap();
                let compiled: Vec<&crate::node_graph::execution_plan::CompiledMeshOutputRule> = plan
                    .steps()
                    .iter()
                    .flat_map(|s| s.outputs.iter())
                    .filter_map(|&(_, res)| plan.mesh_rule(res))
                    .collect();
                assert!(
                    compiled.iter().any(|r| matches!(
                        r.topology,
                        CompiledMeshRevisionRule::Dependencies(_)
                    )),
                    "the compiled fused plan must carry a Dependencies mesh rule, got {compiled:?}"
                );
            }
        }

        /// Scripted stand-in for `system.mesh_input` (same output ports, so
        /// the loader's wiring stays valid) with the topology rule behind a
        /// shared handle — see `mesh_change_fused_rules_match_unfused`.
        struct ScriptedMeshSource {
            type_id: EffectNodeType,
            rule: Arc<Mutex<Option<MeshOutputRule<'static>>>>,
        }

        impl ScriptedMeshSource {
            fn new(rule: Arc<Mutex<Option<MeshOutputRule<'static>>>>) -> Self {
                Self {
                    type_id: EffectNodeType::new("test.scripted_mesh_source"),
                    rule,
                }
            }
        }

        impl EffectNode for ScriptedMeshSource {
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
                static OUTPUTS: [NodeOutput; 2] = [
                    NodePort {
                        name: std::borrow::Cow::Borrowed("vertices"),
                        ty: PortType::Array(ArrayType::of_known::<MeshVertex>()),
                        kind: PortKind::Output,
                        required: false,
                    },
                    NodePort {
                        name: std::borrow::Cow::Borrowed("weights"),
                        ty: PortType::Array(ArrayType::of_known::<f32>()),
                        kind: PortKind::Output,
                        required: false,
                    },
                ];
                &OUTPUTS
            }
            fn parameters(&self) -> &[ParamDef] {
                &[]
            }
            fn array_output_capacity(
                &self,
                port: &str,
                _: &crate::node_graph::ParamValues,
                _: &[(&str, u32)],
            ) -> Option<u32> {
                // Mirror `MeshInput`'s standalone minima.
                match port {
                    "vertices" => Some(1536),
                    "weights" => Some(1),
                    _ => None,
                }
            }
            fn mesh_output_rule(&self, _port: &str) -> MeshOutputRule<'_> {
                self.rule.lock().unwrap().unwrap_or(MeshOutputRule {
                    topology: MeshRevisionRule::Written,
                    positions: MeshRevisionRule::Written,
                })
            }
            fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {
                // No-op actual write — MockBackend cannot run GPU dispatches.
            }
        }
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

    // A disabled effect whose evaluate would visibly replace its input.
    struct BypassProbe {
        source: bool,
        evals: Arc<std::sync::atomic::AtomicUsize>,
        type_id: EffectNodeType,
    }

    impl EffectNode for BypassProbe {
        fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
            crate::node_graph::depth_rule::DepthRule::Terminal
        }
        fn type_id(&self) -> &EffectNodeType { &self.type_id }
        fn inputs(&self) -> &[NodeInput] {
            static INPUTS: [NodeInput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("in"), ty: PortType::Texture2D,
                kind: PortKind::Input, required: true,
            }];
            if self.source { &[] } else { &INPUTS }
        }
        fn outputs(&self) -> &[NodeOutput] {
            static OUTPUTS: [NodeOutput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("out"), ty: PortType::Texture2D,
                kind: PortKind::Output, required: false,
            }];
            &OUTPUTS
        }
        fn parameters(&self) -> &[ParamDef] { &[] }
        fn output_format(&self, _: &str) -> Option<GpuTextureFormat> {
            self.source.then_some(GpuTextureFormat::Rgba16Float)
        }
        fn skip_passthrough_ports(&self) -> Option<(&'static str, &'static str)> {
            (!self.source).then_some(("in", "out"))
        }
        fn skip_passthrough(
            &self, _: &crate::node_graph::ParamValues, _: &[&str],
        ) -> Option<(&'static str, &'static str)> { self.skip_passthrough_ports() }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            self.evals.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let out = ctx.outputs.texture_2d("out").unwrap().clone();
            ctx.gpu_encoder().clear_texture(&out, if self.source { 0.25 } else { 0.75 }, 0.0, 0.0, 1.0);
        }
    }

    fn check_skip_passthrough(borrowed: bool, size: u32, format: GpuTextureFormat) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let device = crate::test_device();
        let evals = Arc::new(AtomicUsize::new(0));
        let mut graph = Graph::new();
        let src = graph.add_node(Box::new(BypassProbe {
            source: true, evals: Arc::new(AtomicUsize::new(0)),
            type_id: EffectNodeType::new("test.bypass_source"),
        }));
        let effect = graph.add_node(Box::new(BypassProbe {
            source: false, evals: evals.clone(),
            type_id: EffectNodeType::new("test.bypass_effect"),
        }));
        let out = graph.add_node(Box::new(crate::node_graph::FinalOutput::new()));
        graph.connect((src, "out"), (effect, "in")).unwrap();
        graph.connect((effect, "out"), (out, "in")).unwrap();
        let plan = compile(&graph).unwrap();
        let resource = |node| plan.steps().iter().find(|s| s.node == node).unwrap().outputs[0].1;
        let (src_res, dst_res) = (resource(src), resource(effect));
        assert_eq!(plan.resource_format(src_res), Some(GpuTextureFormat::Rgba16Float));
        assert_eq!(plan.resource_format(dst_res), None);
        let mut backend = MetalBackend::new(device.arc(), 4, 4, GpuTextureFormat::Rgba16Float);
        backend.pre_bind_texture_2d(src_res, RenderTarget::new(&device, 4, 4, GpuTextureFormat::Rgba16Float, "bypass-src"));
        backend.pre_bind_texture_2d(dst_res, RenderTarget::new(&device, size, size, format, "bypass-dst"));
        let dst_slot = backend.slot_for(dst_res).unwrap();
        let host = RenderTarget::new(&device, size, size, format, "bypass-host");
        if borrowed { assert!(backend.replace_texture_2d(dst_slot, host.texture.clone())); }
        let mut exec = Executor::new(Box::new(backend));
        // Two frames also exercise alias generation bookkeeping and copy freshness.
        for _ in 0..2 {
            let mut enc = device.create_encoder("bypass-proof");
            let mut gpu = GpuEncoder::new(&mut enc, &device);
            exec.execute_frame_with_gpu(&mut graph, &plan, frame_time(), &mut gpu);
            enc.commit_and_wait_completed();
        }
        let compatible = size == 4 && format == GpuTextureFormat::Rgba16Float;
        assert_eq!(evals.load(Ordering::Relaxed), if compatible { 0 } else { 2 });
        if borrowed && compatible {
            assert_eq!(exec.backend().texture_2d(dst_slot).unwrap().raw_ptr(), host.texture.raw_ptr());
            let buffer = device.create_buffer_shared(4 * 4 * 8);
            let mut enc = device.create_encoder("bypass-readback");
            enc.copy_texture_to_buffer(&host.texture, &buffer, 4, 4, 32);
            enc.commit_and_wait_completed();
            let bits = unsafe { *buffer.mapped_ptr().unwrap().cast::<u16>() };
            assert_eq!(half::f16::from_bits(bits).to_f32(), 0.25);
            assert!(exec.alias_propagation_state.iter().flatten().any(|state| {
                state.source == src_res && state.source_content.is_some()
            }), "a physical passthrough copy must retain its selected logical dependency");
        }
    }

    #[test]
    fn skip_passthrough_matches_concrete_default_format() {
        check_skip_passthrough(false, 4, GpuTextureFormat::Rgba16Float);
    }

    #[test]
    fn skip_passthrough_copies_to_borrowed_destination() {
        check_skip_passthrough(true, 4, GpuTextureFormat::Rgba16Float);
    }

    #[test]
    fn skip_passthrough_evaluates_real_mismatches() {
        check_skip_passthrough(false, 2, GpuTextureFormat::Rgba16Float);
        check_skip_passthrough(false, 4, GpuTextureFormat::Rgba8Unorm);
    }

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
        log: Arc<Mutex<Vec<Option<StorageRevision>>>>,
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
            self.log.lock().unwrap().push(ctx.inputs.storage_revision("in"));
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

#[cfg(test)]
mod content_revision_tests {
    use super::*;
    use crate::node_graph::compile;
    use crate::node_graph::effect_node::{EffectNode, EffectNodeType, ParamValues};
    use crate::node_graph::parameters::ParamDef;
    use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
    use manifold_core::{Beats, Seconds};
    use std::sync::{Arc, Mutex};

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    struct ContentProbeSource {
        type_id: EffectNodeType,
        unchanged: Arc<Mutex<bool>>,
        pending: Arc<Mutex<bool>>,
        retained_log: Option<Arc<Mutex<Vec<bool>>>>,
        force_pure_recopy: bool,
    }

    impl EffectNode for ContentProbeSource {
        fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
            crate::node_graph::depth_rule::DepthRule::Terminal
        }
        fn type_id(&self) -> &EffectNodeType { &self.type_id }
        fn inputs(&self) -> &[NodeInput] { &[] }
        fn outputs(&self) -> &[NodeOutput] {
            static OUTPUTS: [NodeOutput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("out"),
                ty: PortType::Texture2D,
                kind: PortKind::Output,
                required: false,
            }];
            &OUTPUTS
        }
        fn parameters(&self) -> &[ParamDef] { &[] }
        fn is_pure(&self) -> bool { self.force_pure_recopy }
        fn skip_passthrough(
            &self,
            _params: &ParamValues,
            _wired_inputs: &[&str],
        ) -> Option<(&'static str, &'static str)> {
            self.force_pure_recopy.then_some(("missing", "out"))
        }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            if let Some(log) = &self.retained_log {
                log.lock().unwrap().push(ctx.outputs_retained());
            }
            if *self.unchanged.lock().unwrap() {
                ctx.mark_output_content_unchanged();
            }
            if *self.pending.lock().unwrap() {
                ctx.mark_outputs_pending();
            }
        }
    }

    type ContentObservation = (
        Option<ContentVersion>,
        Option<StorageRevision>,
        Option<bool>,
    );

    struct ContentProbeSink {
        type_id: EffectNodeType,
        log: Arc<Mutex<Vec<ContentObservation>>>,
    }

    impl EffectNode for ContentProbeSink {
        fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
            crate::node_graph::depth_rule::DepthRule::Terminal
        }
        fn type_id(&self) -> &EffectNodeType { &self.type_id }
        fn inputs(&self) -> &[NodeInput] {
            static INPUTS: [NodeInput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("in"),
                ty: PortType::Texture2D,
                kind: PortKind::Input,
                required: true,
            }];
            &INPUTS
        }
        fn outputs(&self) -> &[NodeOutput] { &[] }
        fn parameters(&self) -> &[ParamDef] { &[] }
        fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
            self.log.lock().unwrap().push((
                ctx.inputs.content_version("in"),
                ctx.inputs.storage_revision("in"),
                ctx.inputs
                    .slot("in")
                    .map(|slot| ctx.inputs.slot_content_ready(slot)),
            ));
        }
    }

    #[test]
    fn content_versions_separate_semantic_copy_pending_and_executor_epoch() {
        let unchanged = Arc::new(Mutex::new(false));
        let pending = Arc::new(Mutex::new(false));
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut graph = Graph::new();
        let source = graph.add_node(Box::new(ContentProbeSource {
            type_id: EffectNodeType::new("test.content_source"),
            unchanged: unchanged.clone(),
            pending: pending.clone(),
            retained_log: None,
            force_pure_recopy: false,
        }));
        let sink = graph.add_node(Box::new(ContentProbeSink {
            type_id: EffectNodeType::new("test.content_sink"),
            log: log.clone(),
        }));
        graph.connect((source, "out"), (sink, "in")).unwrap();
        let plan = compile(&graph).unwrap();
        let mut executor = Executor::with_mock();

        executor.execute_frame(&mut graph, &plan, frame_time());
        *unchanged.lock().unwrap() = true;
        executor.execute_frame(&mut graph, &plan, frame_time());
        let first_two = log.lock().unwrap().clone();
        assert!(first_two[0].0.is_some());
        assert_eq!(first_two[1].0, first_two[0].0, "semantic copy retains content");
        assert_ne!(first_two[1].1, first_two[0].1, "physical copy advances storage");

        *unchanged.lock().unwrap() = false;
        executor.execute_frame(&mut graph, &plan, frame_time());
        let changed = log.lock().unwrap().last().unwrap().0;
        assert_ne!(changed, first_two[1].0, "a true write gets a fresh content version");

        *pending.lock().unwrap() = true;
        executor.execute_frame(&mut graph, &plan, frame_time());
        assert!(log.lock().unwrap().last().unwrap().0.is_none());
        *pending.lock().unwrap() = false;
        *unchanged.lock().unwrap() = true;
        executor.execute_frame(&mut graph, &plan, frame_time());
        let ready = log.lock().unwrap().last().unwrap().0;
        assert!(ready.is_some());
        assert_ne!(ready, changed, "pending to ready forces a fresh publication");
        executor.reset_after_resource_replacement();
        executor.execute_frame(&mut graph, &plan, frame_time());
        let after_reset = log.lock().unwrap().last().unwrap().0;
        assert_ne!(after_reset, ready, "resource reset must issue a new epoch identity");

        let epoch_log = Arc::new(Mutex::new(Vec::new()));
        let mut epoch_graph = Graph::new();
        let epoch_source = epoch_graph.add_node(Box::new(ContentProbeSource {
            type_id: EffectNodeType::new("test.content_source_epoch"),
            unchanged: Arc::new(Mutex::new(false)),
            pending: Arc::new(Mutex::new(false)),
            retained_log: None,
            force_pure_recopy: false,
        }));
        let epoch_sink = epoch_graph.add_node(Box::new(ContentProbeSink {
            type_id: EffectNodeType::new("test.content_sink_epoch"),
            log: epoch_log.clone(),
        }));
        epoch_graph.connect((epoch_source, "out"), (epoch_sink, "in")).unwrap();
        let epoch_plan = compile(&epoch_graph).unwrap();
        let mut second = Executor::with_mock();
        second.execute_frame(&mut epoch_graph, &epoch_plan, frame_time());
        assert_ne!(
            epoch_log.lock().unwrap().last().unwrap().0,
            log.lock().unwrap().last().unwrap().0,
            "different executor lifetimes must not collide in content identity"
        );
    }

    #[test]
    fn pending_publication_does_not_invalidate_physical_retention() {
        let pending = Arc::new(Mutex::new(true));
        let retained = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut graph = Graph::new();
        let source = graph.add_node(Box::new(ContentProbeSource {
            type_id: EffectNodeType::new("test.pending_retention"),
            unchanged: Arc::new(Mutex::new(true)),
            pending: pending.clone(),
            retained_log: Some(retained.clone()),
            force_pure_recopy: false,
        }));
        let sink = graph.add_node(Box::new(ContentProbeSink {
            type_id: EffectNodeType::new("test.pending_retention_sink"),
            log: log.clone(),
        }));
        graph.connect((source, "out"), (sink, "in")).unwrap();
        let plan = compile(&graph).unwrap();
        let mut executor = Executor::with_mock();
        executor.execute_frame(&mut graph, &plan, frame_time());
        *pending.lock().unwrap() = false;
        executor.execute_frame(&mut graph, &plan, frame_time());
        assert_eq!(*retained.lock().unwrap(), [false, true],
            "completed physical copy remains retained while publication was pending");
        let log = log.lock().unwrap();
        assert!(log[0].0.is_none());
        assert!(log[1].0.is_some(), "pending publication must be able to become ready");
    }

    #[test]
    fn content_version_identity_rejects_equal_counters_from_other_resources() {
        let left = ContentVersion::new(7, ResourceId(1), 3);
        let right = ContentVersion::new(7, ResourceId(2), 3);
        let rebuilt = ContentVersion::new(8, ResourceId(1), 3);
        assert_ne!(left, right, "resource identity is part of logical content");
        assert_ne!(left, rebuilt, "executor epoch is part of logical content");
    }

    #[test]
    fn recycled_slot_invalidates_output_retention_proof() {
        let retained_a = Arc::new(Mutex::new(Vec::new()));
        let retained_b = Arc::new(Mutex::new(Vec::new()));
        let mut graph = Graph::new();
        let source_a = graph.add_node(Box::new(ContentProbeSource {
            type_id: EffectNodeType::new("test.recycle_source_a"),
            unchanged: Arc::new(Mutex::new(true)),
            pending: Arc::new(Mutex::new(false)),
            retained_log: Some(retained_a.clone()),
            force_pure_recopy: false,
        }));
        let sink_a = graph.add_node(Box::new(ContentProbeSink {
            type_id: EffectNodeType::new("test.recycle_sink_a"),
            log: Arc::new(Mutex::new(Vec::new())),
        }));
        let source_b = graph.add_node(Box::new(ContentProbeSource {
            type_id: EffectNodeType::new("test.recycle_source_b"),
            unchanged: Arc::new(Mutex::new(true)),
            pending: Arc::new(Mutex::new(false)),
            retained_log: Some(retained_b.clone()),
            force_pure_recopy: false,
        }));
        let sink_b = graph.add_node(Box::new(ContentProbeSink {
            type_id: EffectNodeType::new("test.recycle_sink_b"),
            log: Arc::new(Mutex::new(Vec::new())),
        }));
        graph.connect((source_a, "out"), (sink_a, "in")).unwrap();
        graph.connect((source_b, "out"), (sink_b, "in")).unwrap();
        let plan = compile(&graph).unwrap();
        let mut executor = Executor::with_mock();
        executor.execute_frame(&mut graph, &plan, frame_time());
        executor.execute_frame(&mut graph, &plan, frame_time());

        let a = retained_a.lock().unwrap().clone();
        let b = retained_b.lock().unwrap().clone();
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 2);
        assert!(!a[0] && !b[0], "first publication has no retained proof");
        assert!(
            !a[1] || !b[1],
            "at least one source must observe another logical resource overwriting its slot"
        );
    }

    #[test]
    fn selected_ready_alias_ignores_unselected_pending_and_switches_identity() {
        let pending_b = Arc::new(Mutex::new(true));
        let mut graph = Graph::new();
        let source_a = graph.add_node(Box::new(ContentProbeSource {
            type_id: EffectNodeType::new("test.alias_source_a"),
            unchanged: Arc::new(Mutex::new(false)),
            pending: Arc::new(Mutex::new(false)),
            retained_log: None,
            force_pure_recopy: false,
        }));
        let source_b = graph.add_node(Box::new(ContentProbeSource {
            type_id: EffectNodeType::new("test.alias_source_b"),
            unchanged: Arc::new(Mutex::new(false)),
            pending: pending_b.clone(),
            retained_log: None,
            force_pure_recopy: false,
        }));
        let mux = graph.add_node(Box::new(crate::node_graph::primitives::MuxTexture::new()));
        let selected_log = Arc::new(Mutex::new(Vec::new()));
        let selected_sink = graph.add_node(Box::new(ContentProbeSink {
            type_id: EffectNodeType::new("test.alias_selected_sink"),
            log: selected_log.clone(),
        }));
        let pending_sink = graph.add_node(Box::new(ContentProbeSink {
            type_id: EffectNodeType::new("test.alias_pending_sink"),
            log: Arc::new(Mutex::new(Vec::new())),
        }));
        graph.connect((source_a, "out"), (mux, "in_0")).unwrap();
        graph.connect((source_b, "out"), (mux, "in_1")).unwrap();
        graph.connect((mux, "out"), (selected_sink, "in")).unwrap();
        graph.connect((source_b, "out"), (pending_sink, "in")).unwrap();
        graph.set_param(
            mux,
            "selector",
            crate::node_graph::parameters::ParamValue::Float(0.0),
        ).unwrap();
        let plan = compile(&graph).unwrap();
        let mut executor = Executor::with_mock();
        executor.execute_frame(&mut graph, &plan, frame_time());
        let first = selected_log.lock().unwrap().last().copied().unwrap();
        assert!(first.0.is_some(), "selected ready source publishes content");
        assert_eq!(first.2, Some(true), "unselected pending input must not poison alias");

        *pending_b.lock().unwrap() = false;
        graph.set_param(
            mux,
            "selector",
            crate::node_graph::parameters::ParamValue::Float(1.0),
        ).unwrap();
        executor.execute_frame(&mut graph, &plan, frame_time());
        let second = selected_log.lock().unwrap().last().copied().unwrap();
        assert!(second.0.is_some());
        assert_ne!(first.0, second.0, "switching selected source changes output identity");
    }

    struct PureProbe {
        type_id: EffectNodeType,
        evaluations: Arc<Mutex<u32>>,
    }

    impl EffectNode for PureProbe {
        fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
            crate::node_graph::depth_rule::DepthRule::Terminal
        }
        fn type_id(&self) -> &EffectNodeType { &self.type_id }
        fn inputs(&self) -> &[NodeInput] {
            static INPUTS: [NodeInput; 1] = [NodePort {
                name: std::borrow::Cow::Borrowed("in"),
                ty: PortType::Texture2D,
                kind: PortKind::Input,
                required: true,
            }];
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
        fn parameters(&self) -> &[ParamDef] { &[] }
        fn is_pure(&self) -> bool { true }
        fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {
            *self.evaluations.lock().unwrap() += 1;
        }
    }

    #[test]
    fn pure_consumer_skips_identical_logical_recopy() {
        let unchanged = Arc::new(Mutex::new(false));
        let evaluations = Arc::new(Mutex::new(0));
        let mut graph = Graph::new();
        let source = graph.add_node(Box::new(ContentProbeSource {
            type_id: EffectNodeType::new("test.pure_source"),
            unchanged: unchanged.clone(),
            pending: Arc::new(Mutex::new(false)),
            retained_log: None,
            force_pure_recopy: true,
        }));
        let pure = graph.add_node(Box::new(PureProbe {
            type_id: EffectNodeType::new("test.pure_consumer"),
            evaluations: evaluations.clone(),
        }));
        let sink = graph.add_node(Box::new(ContentProbeSink {
            type_id: EffectNodeType::new("test.pure_sink"),
            log: Arc::new(Mutex::new(Vec::new())),
        }));
        graph.connect((source, "out"), (pure, "in")).unwrap();
        graph.connect((pure, "out"), (sink, "in")).unwrap();
        let plan = compile(&graph).unwrap();
        let mut executor = Executor::with_mock();
        executor.execute_frame(&mut graph, &plan, frame_time());
        *unchanged.lock().unwrap() = true;
        executor.execute_frame(&mut graph, &plan, frame_time());
        executor.execute_frame(&mut graph, &plan, frame_time());
        assert_eq!(*evaluations.lock().unwrap(), 1);
    }

    #[test]
    fn pure_consumer_preserves_content_across_transient_io_recopy() {
        let unchanged = Arc::new(Mutex::new(false));
        let evaluations = Arc::new(Mutex::new(0));
        let mut graph = Graph::new();
        let source = graph.add_node(Box::new(ContentProbeSource {
            type_id: EffectNodeType::new("test.pure_source"),
            unchanged: unchanged.clone(),
            pending: Arc::new(Mutex::new(false)),
            retained_log: None,
            force_pure_recopy: false,
        }));
        let pure = graph.add_node(Box::new(PureProbe {
            type_id: EffectNodeType::new("test.pure_consumer"),
            evaluations: evaluations.clone(),
        }));
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = graph.add_node(Box::new(ContentProbeSink {
            type_id: EffectNodeType::new("test.pure_sink"),
            log: log.clone(),
        }));
        graph.connect((source, "out"), (pure, "in")).unwrap();
        graph.connect((pure, "out"), (sink, "in")).unwrap();
        let plan = compile(&graph).unwrap();
        let mut executor = Executor::with_mock();
        executor.execute_frame(&mut graph, &plan, frame_time());
        *unchanged.lock().unwrap() = true;
        executor.execute_frame(&mut graph, &plan, frame_time());
        executor.execute_frame(&mut graph, &plan, frame_time());
        assert_eq!(*evaluations.lock().unwrap(), 3, "transient outputs still execute to populate storage");
        let observations = log.lock().unwrap();
        assert!(observations[0].0.is_some());
        assert!(observations.iter().all(|entry| entry.0 == observations[0].0),
            "pure/fused output content remains stable across identical IO-source recopies");
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
