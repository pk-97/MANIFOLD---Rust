//! Per-layer / per-group [`PresetRuntime`] caching + dispatch.
//!
//! Replaces the old [`EffectChain`](removed) shim with a free-function
//! interface that operates on an `Option<PresetRuntime>` slot. The slot
//! lives on the owner (e.g., [`LayerCompositor`]'s per-layer
//! [`AHashMap`]); this module owns:
//!
//! - **Dispatch counters** — atomic stats fed by every chain run, read
//!   via [`take_chain_dispatch_stats`] for the periodic chain-rate log.
//! - **`dispatch_chain`** — the entry point the compositor calls per
//!   frame. Builds the graph on first call / topology change, runs it,
//!   returns the output texture. Returns `None` for empty chains or
//!   build failures (which should be unreachable in production — see
//!   `docs/archive/AUDIT_NEXT_STEPS.md`).
//! - **`clear_chain_state`** — reset transient state (mip pyramids,
//!   feedback, `StateStore`) across the cached graph. Fired on seek /
//!   project load.
//!
//! No struct wrapper. The old `EffectChain` carried exactly one
//! `Option<PresetRuntime>` plus a debug-only topology dump field — moving
//! the dump into a local in [`dispatch_chain`] dropped the wrapper to
//! a no-op around `Option`. §6.6 #31.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::preset_runtime::{ChainBuildInputs, PresetRuntime};
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::PrimitiveRegistry;
use crate::preset_context::PresetContext;
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
/// - [`PresetRuntime::try_build`] failed for this topology — unreachable
///   in production with every shipped effect mapped to a primitive.
///
/// The cache survives "no enabled effects" frames so its `RenderTarget`
/// pool and primitive state aren't thrown away on transient ducks.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_chain<'a>(
    cache: &'a mut Option<PresetRuntime>,
    gpu: &mut GpuEncoder<'_>,
    input_texture: &'a GpuTexture,
    effects: &[PresetInstance],
    groups: &[EffectGroup],
    ctx: &PresetContext,
    preview_effect: Option<&EffectId>,
    scope: &str,
    profiling: bool,
) -> Option<&'a GpuTexture> {
    if !effects.iter().any(|fx| fx.enabled) {
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
    // fused-segment swap-in — docs/CHAIN_FUSION_DESIGN.md §5). At rest both
    // checks are a relaxed atomic load each.
    crate::node_graph::freeze::install::pump_segment_results();

    let catalog_generation = crate::preset_loader::catalog_generation();
    let needs_rebuild = match cache.as_ref() {
        None => true,
        Some(cg) => {
            cg.built_generation() != catalog_generation
                || cg.awaiting_segment_swap()
                || !cg.is_compatible(effects, groups, ctx.width, ctx.height, preview_effect)
        }
    };
    if needs_rebuild {
        if std::env::var("MANIFOLD_LOG_REBUILD_REASON").is_ok() {
            let curr = topology_dump(effects, groups, ctx.width, ctx.height);
            eprintln!("[rebuild] curr={curr}");
        }
        let t0 = std::time::Instant::now();
        // Hand the outgoing runtime to the build as the state-harvest donor:
        // unchanged cards carry their sims / trails / workers across the
        // rebuild instead of resetting (docs/CHAIN_FUSION_DESIGN.md §5).
        let mut prior = cache.take();
        *cache = PresetRuntime::try_build(
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
        CHAIN_REBUILD_COUNT.fetch_add(1, Ordering::Relaxed);
        CHAIN_REBUILD_NS.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }

    let Some(cg) = cache.as_mut() else {
        CHAIN_DISPATCH_NS.fetch_add(dispatch_t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
        return None;
    };
    // D6 correction: re-applied every call (cheap) so a rebuild above (which
    // replaces `cg`'s executor) can never leave a stale scope/profiling flag.
    cg.set_profile_scope(scope);
    cg.set_profiling(profiling);
    let t0 = std::time::Instant::now();
    let ran = cg.run(gpu, input_texture, effects, groups, ctx).is_some();
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
pub fn clear_chain_state(cache: &mut Option<PresetRuntime>) {
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
