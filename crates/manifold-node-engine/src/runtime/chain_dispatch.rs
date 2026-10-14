//! Per-layer / per-group [`PresetRuntime`] caching + dispatch.
//!
//! Free functions over a [`ChainSlot`], which lives on the owner (e.g.,
//! [`LayerCompositor`]'s per-layer [`AHashMap`]); this module owns:
//!
//! - **Dispatch counters** — atomic stats fed by every chain run, read
//!   via [`take_chain_dispatch_stats`] for the periodic chain-rate log.
//! - **`dispatch_chain`** — the entry point the compositor calls per
//!   frame. Builds the graph on first call / topology change, runs it,
//!   returns the output texture. Returns `None` for empty chains or
//!   failed builds; a failed build stays remembered in the slot and is
//!   tried again only when one of its inputs moves.
//! - **`clear_chain_state`** — reset transient state (mip pyramids,
//!   feedback, `StateStore`) across the cached graph. Fired on seek /
//!   project load.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use manifold_foundation::cold_touch::{ColdTouchKind, record_cold_touch};

use crate::runtime::{ChainBuildInputs, PresetRuntime};
use crate::gpu::gpu_encoder::GpuEncoder;
use crate::persistence::PrimitiveRegistry;
use crate::runtime::preset_context::{PresetContext, ProjectTempo};
use manifold_core::EffectId;
use manifold_core::effects::{EffectGroup, PresetInstance};
use manifold_gpu::GpuTexture;

// ---------------------------------------------------------------------------
// Chain dispatch instrumentation
//
// Content thread is single-threaded so `Relaxed` ordering is sufficient;
// the atomics exist so any thread can read the counters without `unsafe`.
// `take_chain_dispatch_stats` resets and returns deltas.
// ---------------------------------------------------------------------------

static CHAIN_DISPATCH_COUNT: AtomicU64 = AtomicU64::new(0);
static CHAIN_REBUILD_COUNT: AtomicU64 = AtomicU64::new(0);
static CHAIN_GRAPH_RUN_COUNT: AtomicU64 = AtomicU64::new(0);
static CHAIN_EFFECT_COUNT: AtomicU64 = AtomicU64::new(0);
static CHAIN_DISPATCH_NS: AtomicU64 = AtomicU64::new(0);
static CHAIN_REBUILD_NS: AtomicU64 = AtomicU64::new(0);
static CHAIN_GRAPH_RUN_NS: AtomicU64 = AtomicU64::new(0);

/// Snapshot of chain-dispatch counters accumulated since the previous
/// call to [`take_chain_dispatch_stats`]. All counters reset on read.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChainDispatchStats {
    /// Number of [`dispatch_chain`] calls that did real work (non-empty
    /// chain with at least one enabled effect).
    pub dispatches: u64,
    /// Number of [`PresetRuntime::try_build`] invocations (topology rebuilds).
    pub rebuilds: u64,
    /// Number of [`PresetRuntime::run`] invocations (successful fast path).
    pub graph_runs: u64,
    /// Total enabled effects across all dispatched chains.
    pub effects: u64,
    /// Total wall time (ns) spent in [`dispatch_chain`] calls (CPU only —
    /// excludes GPU work the encoder commands).
    pub dispatch_ns: u64,
    /// Wall time (ns) spent in [`PresetRuntime::try_build`].
    pub rebuild_ns: u64,
    /// Wall time (ns) spent in [`PresetRuntime::run`] proper (param refresh +
    /// `execute_frame_with_gpu`).
    pub graph_run_ns: u64,
}

/// Read the chain-dispatch counters and reset them. Returns the deltas
/// since the previous call. Call once per logging interval.
pub fn take_chain_dispatch_stats() -> ChainDispatchStats {
    ChainDispatchStats {
        dispatches: CHAIN_DISPATCH_COUNT.swap(0, Ordering::Relaxed),
        rebuilds: CHAIN_REBUILD_COUNT.swap(0, Ordering::Relaxed),
        graph_runs: CHAIN_GRAPH_RUN_COUNT.swap(0, Ordering::Relaxed),
        effects: CHAIN_EFFECT_COUNT.swap(0, Ordering::Relaxed),
        dispatch_ns: CHAIN_DISPATCH_NS.swap(0, Ordering::Relaxed),
        rebuild_ns: CHAIN_REBUILD_NS.swap(0, Ordering::Relaxed),
        graph_run_ns: CHAIN_GRAPH_RUN_NS.swap(0, Ordering::Relaxed),
    }
}

/// Build a human-readable single-line dump of the topology that drives
/// `compute_topology_hash`. Used by the env-gated rebuild logger so
/// successive `[rebuild]` lines can be diffed to identify the flapping
/// field. Only allocates when the env var is set.
fn topology_dump(
    effects: &[PresetInstance],
    groups: &[EffectGroup],
    width: u32,
    height: u32,
) -> String {
    let summary: String = effects
        .iter()
        .map(|fx| {
            format!(
                "{}:{}/en={}/g={:?}",
                fx.id.as_str(),
                fx.effect_type().as_str(),
                fx.enabled,
                fx.group_id.as_ref().map(|g| g.as_str()),
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let group_summary: String = groups
        .iter()
        .map(|g| format!("{}:en={}", g.id.as_str(), g.enabled))
        .collect::<Vec<_>>()
        .join(", ");
    format!("dims={width}x{height} effects=[{summary}] groups=[{group_summary}]")
}

// ---------------------------------------------------------------------------
// Chain slot
// ---------------------------------------------------------------------------

/// Device memory that must come free before a build that failed with the same
/// inputs is tried again. A full rebuild costs thousands of allocations, so a
/// few MB of allocator jitter is not a reason to repeat it.
const RETRY_FREED_BYTES: u64 = 64 << 20;

/// One owner's chain: the built runtime, or the build that failed and the
/// inputs it failed with. A failed build is a result, not an empty slot, so the
/// dispatcher does not rebuild it every frame.
#[derive(Default)]
pub struct ChainSlot {
    runtime: Option<PresetRuntime>,
    failure: Option<ChainBuildFailure>,
}

/// A remembered failed build: what it was built from and why it failed.
pub struct ChainBuildFailure {
    inputs: u64,
    memory: Option<manifold_gpu::GpuMemorySnapshot>,
    message: std::sync::Arc<str>,
}

/// Everything a chain build reads apart from device memory: the topology
/// (graph structure, resolution, preview target), each card's graph version
/// (value-only node edits size buffers at build), the preset catalog and the
/// installed fused compiles. Computed only for chains whose build failed.
fn failed_build_inputs(
    effects: &[PresetInstance],
    groups: &[EffectGroup],
    width: u32,
    height: u32,
    preview_effect: Option<&EffectId>,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = ahash::AHasher::default();
    crate::runtime::chain_topology_hash(effects, groups, width, height, preview_effect).hash(&mut h);
    for fx in effects {
        fx.graph_version.hash(&mut h);
    }
    crate::load::preset_loader::catalog_generation().hash(&mut h);
    crate::freeze::install::segment_generation().hash(&mut h);
    crate::freeze::install::fused_effect_generation().hash(&mut h);
    h.finish()
}

impl ChainBuildFailure {
    fn new(
        inputs: u64,
        memory: Option<manifold_gpu::GpuMemorySnapshot>,
        errors: &[crate::runtime::ChainError],
    ) -> Self {
        let message = errors
            .iter()
            .rev()
            .find(|e| matches!(e, crate::runtime::ChainError::PreAllocationFailed { .. }))
            .map(ToString::to_string)
            .unwrap_or_else(|| "The effects could not be built, so they are bypassed. The log names the cause.".into());
        Self { inputs, memory, message: message.into() }
    }

    /// The same build would fail the same way: no build input moved and the
    /// device has not freed room since.
    fn still_holds(&self, inputs: u64, memory: Option<manifold_gpu::GpuMemorySnapshot>) -> bool {
        let room_freed = match (self.memory, memory) {
            (Some(then), Some(now)) => {
                now.recommended_max_working_set_bytes != then.recommended_max_working_set_bytes
                    || now.current_allocated_bytes + RETRY_FREED_BYTES <= then.current_allocated_bytes
            }
            (then, now) => then.is_some() != now.is_some(),
        };
        self.inputs == inputs && !room_freed
    }

    /// One line for the performer: why this chain is bypassed.
    pub fn message(&self) -> &std::sync::Arc<str> {
        &self.message
    }
}

impl ChainSlot {
    pub fn as_ref(&self) -> Option<&PresetRuntime> {
        self.runtime.as_ref()
    }

    pub fn as_mut(&mut self) -> Option<&mut PresetRuntime> {
        self.runtime.as_mut()
    }

    pub fn is_some(&self) -> bool {
        self.runtime.is_some()
    }

    pub fn is_none(&self) -> bool {
        self.runtime.is_none()
    }

    /// Drop the runtime and any remembered failure: the next dispatch builds.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The failed build this slot remembers, if its last build failed.
    pub fn failure(&self) -> Option<&ChainBuildFailure> {
        self.failure.as_ref()
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Apply `effects` against `input_texture`, returning the chain's final
/// output. `cache` is the per-owner [`PresetRuntime`] slot — `None` to
/// start, populated on first dispatch and reused across frames. The
/// caller owns the slot and is responsible for clearing it when the
/// owner is reset.
///
/// `scope` is this chain's instance identity for profiled GPU-attribution
/// tags (`fx:{layer_id}`, `master`, `led:{...}` — PERF_BUDGET_GATE_DESIGN P2 /
/// D6 correction) and `profiling` is whether attribution profiling is on this
/// frame. Both are (re-)applied on every call — including across a rebuild,
/// which replaces the cached [`PresetRuntime`] and its executor — so a scope
/// or profiling toggle can never go stale on a topology change. The cost when
/// `profiling` is `false` is a cheap `String`/`bool` set, not a GPU or
/// per-step cost (that stays gated inside the executor).
///
/// Returns `None` when:
///
/// - The chain has no enabled effects (caller should use the original
///   input).
/// - The build failed for these inputs ([`ChainSlot::failure`] names why).
///
/// The cache survives "no enabled effects" frames so its `RenderTarget`
/// pool and primitive state aren't thrown away on transient ducks.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_chain<'a>(
    cache: &'a mut ChainSlot,
    gpu: &mut GpuEncoder<'_>,
    input_texture: &'a GpuTexture,
    effects: &[PresetInstance],
    groups: &[EffectGroup],
    ctx: &PresetContext,
    preview_effect: Option<&EffectId>,
    scope: &str,
    profiling: bool,
    rt_quality: crate::exec::effect_node::RtQuality,
    sim_step: crate::exec::effect_node::SimStep,
    sim_metrics: &mut crate::exec::sim_metrics::SimMetrics,
    layer_sources: &crate::runtime::layer_skin::LayerSkinRegistry,
    project_tempo: Option<&ProjectTempo>,
) -> Option<&'a GpuTexture> {
    let mut viewport_error = None;
    dispatch_chain_with_scene_viewport(
        cache,
        gpu,
        input_texture,
        effects,
        groups,
        ctx,
        preview_effect,
        scope,
        profiling,
        rt_quality,
        sim_step,
        sim_metrics,
        layer_sources,
        None,
        &mut viewport_error,
        project_tempo,
    )
}

/// Dispatch a chain and apply a render-only scene viewport request after a
/// rebuild, before the first graph run. Existing runtimes keep their viewport
/// pass when the target is unchanged; [`PresetRuntime::set_scene_viewport`]
/// only invalidates the captured frame in that case.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_chain_with_scene_viewport<'a>(
    cache: &'a mut ChainSlot,
    gpu: &mut GpuEncoder<'_>,
    input_texture: &'a GpuTexture,
    effects: &[PresetInstance],
    groups: &[EffectGroup],
    ctx: &PresetContext,
    preview_effect: Option<&EffectId>,
    scope: &str,
    profiling: bool,
    rt_quality: crate::exec::effect_node::RtQuality,
    sim_step: crate::exec::effect_node::SimStep,
    sim_metrics: &mut crate::exec::sim_metrics::SimMetrics,
    layer_sources: &crate::runtime::layer_skin::LayerSkinRegistry,
    scene_viewport: Option<(
        &EffectId,
        &manifold_core::NodeId,
        crate::scene::scene_viewport::SceneViewportConfig,
    )>,
    scene_viewport_error: &mut Option<crate::scene::scene_viewport::SceneViewportHostError>,
    project_tempo: Option<&ProjectTempo>,
) -> Option<&'a GpuTexture> {
    if !effects.iter().any(|fx| fx.enabled) {
        // Nothing left to build, so nothing has failed.
        cache.failure = None;
        return None;
    }

    let dispatch_t0 = std::time::Instant::now();
    CHAIN_DISPATCH_COUNT.fetch_add(1, Ordering::Relaxed);
    let enabled_count = effects.iter().filter(|fx| fx.enabled).count() as u64;
    CHAIN_EFFECT_COUNT.fetch_add(enabled_count, Ordering::Relaxed);

    // Step 10 hot-reload: one relaxed atomic load. At rest the catalog
    // generation never moves, so a cached chain built against the current
    // generation passes through unchanged (byte-identical perform path).
    // When a preset `.json` is edited on disk the watcher bumps the
    // generation, and any chain built against the old generation is forced
    // to rebuild from the freshly-loaded defs on the next frame.
    // Chain fusion: land any finished background segment compiles before the
    // rebuild decision, then rebuild chains that were waiting on one (the
    // fused-segment swap-in — docs/CHAIN_FUSION_DESIGN.md section 5). At rest both
    // checks are a relaxed atomic load each.
    crate::freeze::install::pump_segment_results();

    let catalog_generation = crate::load::preset_loader::catalog_generation();
    if cache.runtime.is_none()
        && let Some(failure) = &cache.failure
    {
        let inputs = failed_build_inputs(effects, groups, ctx.width, ctx.height, preview_effect);
        if failure.still_holds(inputs, gpu.device.modifier_memory_snapshot()) {
            CHAIN_DISPATCH_NS.fetch_add(dispatch_t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
            return None;
        }
    }
    let (needs_rebuild, reason) = match cache.runtime.as_ref() {
        None => (true, "cache-miss"),
        Some(cg) => {
            if cg.built_generation() != catalog_generation {
                (true, "catalog-generation")
            } else if cg.awaiting_segment_swap() {
                (true, "segment-swap")
            } else if cg.awaiting_fused_swap() {
                // BUG-j8gy: per-card fused views land on the same worker —
                // same swap-in handshake as segments.
                (true, "fused-swap")
            } else if cg.awaiting_forced_outputs_rebuild() {
                // BUG-18l: a live forced-outputs change is a topology change
                // — same handshake as the segment swap-in above.
                (true, "forced-outputs")
            } else if !cg.is_compatible(effects, groups, ctx.width, ctx.height, preview_effect) {
                (true, "incompatible")
            } else {
                (false, "")
            }
        }
    };
    if needs_rebuild {
        if std::env::var("MANIFOLD_LOG_REBUILD_REASON").is_ok() {
            let curr = topology_dump(effects, groups, ctx.width, ctx.height);
            eprintln!("[rebuild] scope={scope} reason={reason} curr={curr}");
        }
        let t0 = std::time::Instant::now();
        // Hand the outgoing runtime to the build as the state-harvest donor:
        // unchanged cards carry their sims / trails / workers across the
        // rebuild instead of resetting (docs/CHAIN_FUSION_DESIGN.md section 5).
        let mut prior = cache.runtime.take();
        record_cold_touch(ColdTouchKind::ChainConstruction);
        let built = PresetRuntime::build(
            ChainBuildInputs {
                effects,
                groups,
                primitives: primitive_registry(),
                device: gpu.device,
                pool: gpu.pool,
                width: ctx.width,
                height: ctx.height,
                preview_effect,
            },
            prior.as_mut(),
        );
        drop(prior);
        match built {
            Ok(runtime) => {
                cache.runtime = runtime;
                cache.failure = None;
            }
            Err(errors) => {
                // Inputs and memory are read after the failed build released
                // what it took, so the next frame compares like with like.
                cache.failure = Some(ChainBuildFailure::new(
                    failed_build_inputs(effects, groups, ctx.width, ctx.height, preview_effect),
                    gpu.device.modifier_memory_snapshot(),
                    &errors,
                ));
            }
        }
        CHAIN_REBUILD_COUNT.fetch_add(1, Ordering::Relaxed);
        CHAIN_REBUILD_NS.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }

    let Some(cg) = cache.runtime.as_mut() else {
        CHAIN_DISPATCH_NS.fetch_add(dispatch_t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
        return None;
    };
    // D6 correction: re-applied every call (cheap) so a rebuild above (which
    // replaces `cg`'s executor) can never leave a stale scope/profiling flag.
    cg.set_profile_scope(scope);
    cg.set_profiling(profiling);
    // D6 correction for rt_quality: same rationale as profiling above —
    // rebuild swaps the executor, so per-call re-application keeps the value
    // from going stale.
    cg.set_rt_quality(rt_quality);
    cg.set_sim_step(sim_step);
    cg.set_layer_skin_registry(Some(layer_sources));
    cg.set_project_tempo(project_tempo);
    if let Some((effect_id, node_id, config)) = scene_viewport {
        match cg.set_scene_viewport(effect_id, node_id, config) {
            Ok(()) => *scene_viewport_error = None,
            Err(crate::scene::scene_viewport::SceneViewportError::TargetNotFound) => {}
            Err(error) => {
                *scene_viewport_error =
                    Some(crate::scene::scene_viewport::SceneViewportHostError::InvalidTarget(error));
            }
        }
    }
    let t0 = std::time::Instant::now();
    let ran = cg.run(gpu, input_texture, effects, groups, ctx).is_some();
    cg.drain_sim_metrics(sim_metrics);
    if ran {
        CHAIN_GRAPH_RUN_COUNT.fetch_add(1, Ordering::Relaxed);
        CHAIN_GRAPH_RUN_NS.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    CHAIN_DISPATCH_NS.fetch_add(dispatch_t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    if ran {
        cache.as_ref()?.output_texture()
    } else {
        None
    }
}

/// Reset per-effect transient state on the cached chain (mip pyramids,
/// feedback buffers, [`StateStore`](crate::node_graph::StateStore)
/// entries). No-op when the cache is empty.
pub fn clear_chain_state(cache: &mut ChainSlot) {
    if let Some(cg) = cache.as_mut() {
        cg.clear_state();
    }
}

/// Process-wide [`PrimitiveRegistry`] used by every chain dispatch.
/// Built lazily on first call so callers don't have to thread a
/// registry reference.
fn primitive_registry() -> &'static PrimitiveRegistry {
    static CELL: OnceLock<PrimitiveRegistry> = OnceLock::new();
    CELL.get_or_init(PrimitiveRegistry::with_builtin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ChainError;
    use manifold_gpu::GpuMemorySnapshot;

    fn in_use(bytes: u64) -> Option<GpuMemorySnapshot> {
        Some(GpuMemorySnapshot { current_allocated_bytes: bytes, recommended_max_working_set_bytes: 30 << 30 })
    }

    #[test]
    fn failed_build_holds_until_an_input_moves() {
        let mut effects = vec![PresetInstance::new(manifold_core::PresetTypeId::COLOR_GRADE)];
        let inputs = |fx: &[PresetInstance], w, h| failed_build_inputs(fx, &[], w, h, None);
        let full = 22 << 30;
        let errors = [ChainError::PreAllocationFailed { reason: "The scene needs 900 MB.".into() }];
        let failure = ChainBuildFailure::new(inputs(&effects, 1920, 1080), in_use(full), &errors);

        assert!(failure.still_holds(inputs(&effects, 1920, 1080), in_use(full)), "next frame, nothing moved");
        assert!(failure.still_holds(inputs(&effects, 1920, 1080), in_use(full - (1 << 20))), "allocator jitter");
        assert!(!failure.still_holds(inputs(&effects, 1280, 720), in_use(full)), "resolution changed");
        assert!(!failure.still_holds(inputs(&effects, 1920, 1080), in_use(full - RETRY_FREED_BYTES)), "device freed room");
        assert!(failure.message().contains("The scene needs 900 MB."));

        // A value-only node edit (a smaller sim count) sizes buffers at build.
        effects[0].graph_version += 1;
        assert_eq!(effects[0].graph_structure_version, 0, "structure unchanged");
        assert!(!failure.still_holds(inputs(&effects, 1920, 1080), in_use(full)), "graph value edit retries");
    }
}
