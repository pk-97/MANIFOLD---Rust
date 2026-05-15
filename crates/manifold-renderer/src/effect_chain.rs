use crate::effect::EffectContext;
use crate::effect_chain_graph::ChainGraph;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::PrimitiveRegistry;
use manifold_core::effects::{EffectGroup, EffectInstance};
use manifold_gpu::{GpuDevice, GpuTexture};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// Chain dispatch instrumentation.
//
// Lightweight counters incremented from the per-frame `apply_chain` /
// `try_run_chain_graph` hot path. Content thread is single-threaded so
// the `Relaxed` ordering is sufficient; the atomics exist so the
// counters can be read from anywhere (e.g., a periodic logger on the
// app thread) without `unsafe`.
//
// Read via `take_chain_dispatch_stats()` — that resets the counters,
// so each call returns the deltas since the last call.
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
    /// Number of `apply_chain` calls that did real work
    /// (non-empty chain that produced a result).
    pub dispatches: u64,
    /// Number of `ChainGraph::try_build` invocations (topology rebuilds).
    pub rebuilds: u64,
    /// Number of `ChainGraph::run` invocations (successful fast path).
    pub graph_runs: u64,
    /// Total enabled effects across all dispatched chains.
    pub effects: u64,
    /// Total wall time (ns) spent in `apply_chain` calls (CPU only —
    /// excludes GPU work the encoder commands).
    pub dispatch_ns: u64,
    /// Wall time (ns) spent in `ChainGraph::try_build`.
    pub rebuild_ns: u64,
    /// Wall time (ns) spent in `ChainGraph::run` proper (param
    /// refresh + execute_frame_with_gpu).
    pub graph_run_ns: u64,
}

/// Build a human-readable single-line dump of the topology that drives
/// `compute_topology_hash`. Used by the env-gated rebuild logger so
/// successive `[rebuild]` lines can be diffed to identify the flapping
/// field. Only allocates when the env var is set.
fn topology_dump(
    effects: &[EffectInstance],
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

/// Read the chain-dispatch counters and reset them. Returns the
/// deltas since the previous call. Call once per logging interval.
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

/// Wraps a cached [`ChainGraph`] — the single dispatch path for an
/// effect chain. The chain itself owns no GPU resources; the
/// underlying `ChainGraph` allocates its own slot-managed
/// `RenderTarget`s via the graph runtime's `MetalBackend`.
pub struct EffectChain {
    /// The compiled chain graph. `None` between construction and
    /// the first `apply_chain` call (or after a `resize` /
    /// topology-rebuild trigger). Lazy-built on first dispatch.
    chain_graph: Option<ChainGraph>,
    /// Diagnostic-only: human-readable dump of the topology that
    /// produced the current `chain_graph`. Populated on rebuild
    /// when `MANIFOLD_LOG_REBUILD_REASON` is set so successive
    /// rebuild logs can be diffed against the previous topology.
    /// `None` when the env var is unset (zero overhead in production).
    last_built_topology_dump: Option<String>,
}

impl Default for EffectChain {
    fn default() -> Self {
        Self::new()
    }
}

impl EffectChain {
    pub fn new() -> Self {
        Self {
            chain_graph: None,
            last_built_topology_dump: None,
        }
    }

    /// Build or reuse the cached `ChainGraph`. Returns `true` if a
    /// graph is available to dispatch the chain this frame; `false`
    /// if `ChainGraph::try_build` couldn't construct one for this
    /// topology. In production every shipped effect maps to either a
    /// primitive or a `LegacyPostProcessNode` adapter, so this should
    /// never return `false` for a real chain — see the audit notes
    /// in `docs/AUDIT_NEXT_STEPS.md`.
    fn try_run_chain_graph(
        &mut self,
        gpu: &mut GpuEncoder<'_>,
        input_texture: &GpuTexture,
        effects: &[EffectInstance],
        groups: &[EffectGroup],
        ctx: &EffectContext,
    ) -> bool {
        let needs_rebuild = match &self.chain_graph {
            None => true,
            Some(cg) => !cg.is_compatible(effects, groups, ctx.width, ctx.height),
        };
        if needs_rebuild {
            // Diagnostic: dump the topology fingerprint on rebuild so
            // we can see WHICH field flapped. Gated behind a separate
            // env var so it doesn't drown the chain-stats log on
            // rebuild-heavy frames. When enabled we also stash the
            // previous build's dump on `self` so each [rebuild] line
            // shows both prev and curr — diff-friendly.
            let log_enabled = std::env::var("MANIFOLD_LOG_REBUILD_REASON").is_ok();
            if log_enabled {
                let curr = topology_dump(effects, groups, ctx.width, ctx.height);
                let prev = self
                    .last_built_topology_dump
                    .as_deref()
                    .unwrap_or("<none — first build>");
                eprintln!("[rebuild] prev={prev}");
                eprintln!("[rebuild] curr={curr}");
                self.last_built_topology_dump = Some(curr);
            }
            let t0 = std::time::Instant::now();
            self.chain_graph = ChainGraph::try_build(
                effects,
                groups,
                primitive_registry(),
                gpu.device,
                gpu.pool,
                ctx.width,
                ctx.height,
            );
            CHAIN_REBUILD_COUNT.fetch_add(1, Ordering::Relaxed);
            CHAIN_REBUILD_NS.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
        let Some(cg) = self.chain_graph.as_mut() else {
            return false;
        };
        let t0 = std::time::Instant::now();
        let result = cg.run(gpu, input_texture, effects, groups, ctx).is_some();
        if result {
            CHAIN_GRAPH_RUN_COUNT.fetch_add(1, Ordering::Relaxed);
            CHAIN_GRAPH_RUN_NS.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
        }
        result
    }

    /// Read the chain output texture after a successful
    /// [`Self::try_run_chain_graph`]. Returns `None` if the chain
    /// graph isn't cached (preceding `try_run_chain_graph` either
    /// wasn't called or returned `false`).
    fn chain_graph_output(&self) -> Option<&GpuTexture> {
        self.chain_graph.as_ref()?.output_texture()
    }

    /// Apply a chain of effects. Returns the texture with the final result.
    ///
    /// If the chain is empty or has no enabled effects, returns `None` (caller
    /// should use the original input).
    pub fn apply_chain<'a>(
        &'a mut self,
        gpu: &mut GpuEncoder,
        input_texture: &'a GpuTexture,
        effects: &[EffectInstance],
        groups: &[EffectGroup],
        ctx: &EffectContext,
    ) -> Option<&'a GpuTexture> {
        // Quick scan: any enabled effects? (Skip registry lookup — the main loop
        // handles unregistered effects. This just avoids buffer/context setup.)
        if !effects.iter().any(|fx| fx.enabled) {
            return None;
        }

        let dispatch_t0 = std::time::Instant::now();
        CHAIN_DISPATCH_COUNT.fetch_add(1, Ordering::Relaxed);
        let enabled_count = effects.iter().filter(|fx| fx.enabled).count() as u64;
        CHAIN_EFFECT_COUNT.fetch_add(enabled_count, Ordering::Relaxed);

        // No cross-chain param scan — VoronoiPrism used to read
        // EdgeStretch's `width` here via `find_chain_param`, but the
        // splice migration replaced that hidden coupling with an
        // explicit `source_width` slider on VoronoiPrism's card.
        let chain_ctx = ctx;

        // Fast path: try to render the whole chain through one
        // cached `Graph`. Bails (returns `false`) for chains with
        // partial-wet-dry groups, unmapped effects, etc. — those
        // fall through to the per-effect dispatch below. (The
        // per-effect cache stays alive on fallback paths so its
        // runners survive across mode transitions.)
        // Single dispatch path: ChainGraph fast path. The previous
        // per-effect legacy fallback was deleted post-Phase-1
        // verification (Phase 0 multi-segment Mix support eliminated
        // the only topology that reached it; every shipped effect has
        // a primitive or LegacyPostProcessNode mapping).
        let ran = self.try_run_chain_graph(gpu, input_texture, effects, groups, chain_ctx);
        CHAIN_DISPATCH_NS.fetch_add(dispatch_t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
        if ran {
            self.chain_graph_output()
        } else {
            // ChainGraph couldn't build a runner for this chain shape.
            // Returning None tells the caller to use the original
            // input texture. In production this should be unreachable
            // — see the audit notes in `docs/AUDIT_NEXT_STEPS.md`.
            None
        }
    }

    /// Drop the cached `ChainGraph` so its pre-bound `RenderTarget`s
    /// return to their pool on next allocation. Used on render-
    /// resolution change — the underlying graph holds width/height-
    /// sized slots and a fresh build picks up the new dimensions.
    pub fn resize(&mut self, _device: &GpuDevice, _width: u32, _height: u32) {
        self.chain_graph = None;
    }

    /// Reset per-effect transient state on the cached chain graph
    /// (mip pyramids, feedback buffers, StateStore entries). Fired
    /// on seek and project load so trails / accumulators don't
    /// carry across discontinuities.
    pub fn clear_graph_runner_state(&mut self) {
        if let Some(cg) = self.chain_graph.as_mut() {
            cg.clear_state();
        }
    }
}

/// Process-wide [`PrimitiveRegistry`] used by every `EffectChain`'s
/// graph-runtime dispatch path. Built lazily on first call so the
/// renderer's effect-chain code doesn't have to thread a registry
/// reference through `apply_chain`'s already-wide signature.
fn primitive_registry() -> &'static PrimitiveRegistry {
    static CELL: OnceLock<PrimitiveRegistry> = OnceLock::new();
    CELL.get_or_init(PrimitiveRegistry::with_builtin)
}
