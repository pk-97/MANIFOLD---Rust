//! Step 4 — install fused regions into the live render path (design §12.2/§12.3).
//!
//! [`super::region::partition_regions`] is the finder: it splits a flattened
//! [`EffectGraphDef`] into its maximal pointwise/coincident regions, cutting at
//! every boundary. This module turns that partition into a *rendered* def — one
//! fused [`node.wgsl_compute`] kernel per region, wired back into the surviving
//! boundary nodes — and retargets the outer-card bindings onto the fused nodes.
//!
//! ## What it does
//!
//! Given an effect's canonical [`EffectGraphDef`], it:
//!
//! 1. **Partitions** it into regions ([`super::region`]). A region is a maximal
//!    run of register-threadable atoms; everything else (blur, warp/gather,
//!    feedback, DNN, resolution change, generators, control-wired params) is a
//!    boundary that bounds the regions around it. ColorGrade is the degenerate
//!    case — the whole card is one region — but an effect with a blur in the
//!    middle now fuses the pure runs on *both* sides of it.
//! 2. **Rewrites the def** (DD-A1 — a *definition* rewrite, not a `Graph`
//!    clone): every region's worker nodes + internal wires are deleted and
//!    replaced by ONE `node.wgsl_compute` node carrying the generated fused
//!    WGSL. Surviving boundary nodes carry over unchanged; each region's
//!    external producers are re-anchored onto the fused node's `src_<n>` inputs
//!    (read once) and its output onto the consumers the region used to feed.
//!    Because distinct regions are never directly texture-wired (such a wire
//!    would have merged them), every external/consumer is a surviving node, so
//!    the rewrite is local and the graph stays valid.
//! 3. **Retargets the bindings** (DD-A5): each outer-card slider that drove an
//!    inner node param (`gain.gain`, `colorize.focus`, …) is repointed at *its*
//!    region's fused node + namespaced uniform field (`n0_gain`, `n4_focus`, …);
//!    a slider driving a surviving boundary (a blur radius) is left untouched.
//!    The fused [`WgslCompute`] derives those as port-shadowed params from the
//!    uniform struct, so drivers / Ableton / LFOs keep writing them every frame
//!    (DD-A4: `var<uniform>`, never std430).
//!
//! The fused [`LoadedPresetView`] is cached `&'static` (built once per effect
//! type, exactly like [`crate::node_graph::loaded_preset_view_by_id`]), so the
//! per-frame chain rebuilds on resize don't leak.
//!
//! ## What it deliberately does NOT touch (DD-A6)
//!
//! - The **unfused** canonical view ([`crate::node_graph::loaded_preset_view_by_id`])
//!   stays the authoring + fallback surface. The graph editor reads it, so
//!   drilling into a fused effect still shows the original atoms. Only the chain
//!   *render* path swaps in the fused view, and only for the un-edited canonical
//!   preset — an effect with a per-instance graph override
//!   (`PresetInstance.graph = Some`) is rendered from the user's wiring,
//!   unfused, so editing stays live.
//! - This is "freeze = render-only binary, graph = source" (the §12 framing).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::OnceLock;

use ahash::{AHashMap, AHashSet};
use manifold_core::PresetTypeId;
use manifold_core::NodeId;
use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EFFECT_GRAPH_VERSION_WITH_METADATA, EffectGraphDef, EffectGraphNode,
    EffectGraphWire, SerializedParamValue,
};

use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::freeze::codegen::{self, FusionRegion, InputSource, RegionNode};
use crate::node_graph::freeze::markers::Marker;
use crate::node_graph::freeze::region::{Region, RegionInput, partition_regions};
use crate::node_graph::freeze::space::{ElementSpace, resolve_output_spaces, space_of};
use crate::node_graph::ports::PortType;
use crate::node_graph::parameters::{ParamType, ParamValue};
use crate::node_graph::param_binding::ParamConvert;
use crate::node_graph::{LoadedPresetView, ParamBinding, ParamTarget, PrimitiveRegistry};

/// Whether fusion is enabled this process. Default ON — the freeze compiler is
/// the main render path (Peter's request). The `MANIFOLD_FREEZE` env var is the
/// v1 kill-switch: set it to `0` / `false` / `off` and relaunch to render every
/// effect unfused (the §12.3 step 7 "never fuse tonight" switch, restart-scoped
/// for now; a live hot-toggle is the step-7 follow-up). Read once and cached so
/// it's a process constant — no per-frame env lookup, no topology-hash churn.
pub fn freeze_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| match std::env::var("MANIFOLD_FREEZE") {
        Ok(v) => !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no"),
        Err(_) => true,
    })
}

/// The single home for the "should we attempt to fuse this card?" decision,
/// shared by the effect chain build ([`crate::effect_chain_graph`]) and the
/// generator registry ([`crate::generators::registry`]). Folding it here keeps
/// the watched-target override from drifting into a third copy.
///
/// The decision is structural, not measured: fuse whenever fusion is enabled
/// this process and the card isn't the *watched* target (open in the graph
/// editor — kept unfused so per-node output preview can sample inner-node
/// textures and edits render live). *Which* atoms actually fuse is then decided
/// entirely by the region partition ([`super::region::partition_regions`], gated
/// on `MIN_REGION_LEN`): a card with no fusable region simply gets `None` back
/// from [`fused_view_for`] and renders unfused. There is no per-device GPU
/// timing — the partition's structure (chains of ≥2 pointwise atoms collapse,
/// gathers/boundaries cut) is the cost model, so the decision is instant,
/// deterministic, and resolution-independent, and works identically for shipped,
/// edited, and brand-new graphs.
///
/// Note there is no `has_override` veto: an edited/created graph is no longer
/// special-cased to unfused. Its *content* is fused on demand via
/// [`fused_view_for`] / [`fused_generator_def_for`], keyed by the def itself —
/// so it fuses on editor-close exactly like a shipped shape. While the editor is
/// open it's the watched target, which `is_watched` already keeps unfused.
pub fn should_render_fused(is_watched: bool) -> bool {
    freeze_enabled() && !is_watched
}

/// Fused [`LoadedPresetView`] for an effect *type* — the canonical (shipped)
/// shape. Now a thin wrapper over the content-keyed [`fused_view_for`]: the
/// canonical lookup is just that cache keyed by the type's canonical def, the
/// same door edited shapes use. `None` for any effect whose canonical graph has
/// no fusable region. Startup `tune_all` calls this, warming the canonical
/// entries; the live gate fills edited entries on demand.
pub fn fused_view_by_id(id: &PresetTypeId) -> Option<Arc<LoadedPresetView>> {
    let base = crate::node_graph::loaded_preset_view_by_id(id)?;
    fused_view_for(&base.canonical_def, base)
}

/// Fuse an arbitrary `canonical_def` (shipped, edited, or created) carrying the
/// given outer-card `bindings` + `skip_mode`, or `None` if the def has no fusable
/// region (or a binding would strand). The fused view keeps the same outer-card
/// params + skip mode (so the chain builder's `outer_param_index` /
/// `n_static_slots` / skip logic are byte-identical) and swaps in the fused def +
/// retargeted bindings. Takes the def by reference (not `&'static`) so an edited
/// `PresetInstance::graph` can be fused in place — only the freshly-built fused
/// def is leaked. Pure codegen, no device: the GPU pipeline compile happens
/// downstream when the fused def is spliced into the chain (the executor compiles
/// whatever it's handed, fused or not), so this is the unit that relocates to a
/// background worker later.
fn fuse_view_parts(
    canonical_def: &EffectGraphDef,
    bindings: &[ParamBinding],
    type_id: &PresetTypeId,
    skip_mode: crate::node_graph::SkipMode,
    registry: &PrimitiveRegistry,
    region_mask: Option<&[bool]>,
) -> Option<LoadedPresetView> {
    let fused =
        fuse_canonical_def_masked(canonical_def, registry, region_mask, &binding_targets(bindings))?;
    // Node ids that survive the rewrite (boundaries + the fused nodes): a binding
    // targeting one of these is left as-is; one targeting a fused-away member is
    // retargeted; anything else strands a slider, so refuse to fuse.
    let surviving: AHashSet<String> = fused
        .def
        .nodes
        .iter()
        .map(|n| resolve_node_id(n).as_str().to_string())
        .collect();
    let bindings = retarget_bindings(bindings, &fused.retarget, &surviving)?;
    // The fused def must actually build (not just parse) AND preserve every
    // region output's element space — fall back to unfused otherwise.
    if !fused_def_builds(&fused.def, registry, &fused.expected_spaces) {
        return None;
    }
    let FusedDef {
        def: fused_def,
        retarget,
        ..
    } = fused;
    Some(LoadedPresetView {
        type_id: type_id.clone(),
        canonical_def: Arc::new(fused_def),
        bindings,
        skip_mode,
        // Carry the full retarget map so the chain builder can repoint a
        // per-instance user binding (off-def, invisible to the content-keyed
        // fuse) onto the fused node — the same retarget the static bindings
        // above already went through.
        fused_retarget: retarget,
    })
}

// ===========================================================================
// On-demand, content-keyed fusion cache — the single door (design: step 2).
//
// Fusion is no longer a startup-only, per-*type* artifact. Any graph — shipped,
// edited in the node editor, or created from scratch — fuses through one cache
// keyed by the def's structural *content*, not its type id. Startup `tune_all`
// merely pre-warms the canonical entries (via `fused_view_by_id` below, now a
// thin content-keyed wrapper); the live gate fills edited entries on demand.
//
// Blocking now, background-swappable later: the miss path compiles synchronously
// (standard memoization). To move compile to a worker, the miss path returns
// `None` and spawns instead of blocking — selection stays `fused_view_for`, so
// that's the whole change. Keeping selection cache-only *now* is what makes that
// a localized swap. The cache is owned by the content thread (the only thread
// that builds chains / generators / runs `tune_all`), so it's lock-free; the
// future shared-cache upgrade rides along with the background-worker step.
// ===========================================================================

/// Structural content key for a def: topology + node configs + baked (non-
/// exposed) param values. Deterministic because every map in `EffectGraphDef` is
/// a `BTreeMap` and every list a `Vec`, so `serde_json` is a stable total
/// encoding (and handles `f32` params, which aren't `Hash`).
///
/// Live *exposed* params are NOT in the def — they flow through
/// `PresetInstance.param_values` as runtime uniforms — so two instances that
/// differ only in live modulation share one key, and the fused kernel keeps
/// exposed params as uniforms (never baked). Computed on cache miss / chain
/// rebuild, an editing-time event, never per frame.
pub(crate) fn def_content_key(def: &EffectGraphDef) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = ahash::AHasher::default();
    // Normalize away the purely-cosmetic fields (editor canvas position, node
    // title) before hashing: neither affects the compiled kernel, so a node
    // drag or rename must not perturb the key and force a spurious re-fuse.
    // Same "serialize the whole thing and hash the bytes" mechanism as
    // before — this just feeds it a cleared clone instead of `def` directly.
    let mut normalized = def.clone();
    clear_cosmetic_fields(&mut normalized.nodes);
    match serde_json::to_vec(&normalized) {
        Ok(bytes) => bytes.hash(&mut h),
        // Unserializable def → distinguished key; `compile_fused_view` will also
        // fail it to `None`, so it renders unfused (always correct).
        Err(_) => return u64::MAX,
    }
    h.finish()
}

/// Recursively clears `editor_pos` and `title` on every node, including nodes
/// nested inside group bodies (`EffectGraphNode::group`), which are
/// themselves `EffectGraphNode`s — a drag or rename anywhere in the tree
/// must not perturb `def_content_key`.
fn clear_cosmetic_fields(nodes: &mut [EffectGraphNode]) {
    for node in nodes {
        node.editor_pos = None;
        node.title = None;
        if let Some(group) = node.group.as_mut() {
            clear_cosmetic_fields(&mut group.nodes);
        }
    }
}

/// Cap on each content cache (effect view / generator def / segment view). The
/// cached values are CPU codegen artifacts (a `LoadedPresetView` / fused
/// `EffectGraphDef` — WGSL text + def structure, a few KB), NOT GPU pipelines:
/// the pipeline lives in the chain executor / generator, bounded by the
/// recycled per-layer chain pool. So the only thing bounded here is small CPU
/// memory, one entry per *distinct* edited shape.
///
/// FUSION_SOTA_DESIGN D5: values are `Arc`-owned now (not a leaked
/// `&'static`), so past the cap the cache evicts the least-recently-hit entry
/// instead of refusing to insert — an evicted `Arc`'s memory genuinely frees
/// once every clone (the render path never holds one past a single chain
/// build/rebuild) drops. Raising this cap doesn't remove the leak class it
/// used to bound (rejected in the design) — it isn't a leak class anymore.
const FUSED_CACHE_CAP: usize = 512;

/// Bounded, LRU-evicting content-keyed cache — the shape every fused-artifact
/// cache in this module shares (effect view / generator def / segment view).
/// Same precedent as the chain pool's `last_used_frame` + eviction
/// (`docs/EFFECT_CHAIN_LIFECYCLE.md`): a monotonic tick recorded per access
/// instead of a frame counter (this cache is hit at chain-rebuild time, an
/// editing-time event, not every frame — there's no frame counter to reuse
/// here), least-recently-*hit* eviction instead of a time-based idle grace (a
/// codegen cache has no "the operator muted this and may come back" story —
/// the only failure mode being bounded is pathological edit-spam, so recency
/// alone is the right signal).
struct LruCache<V> {
    entries: AHashMap<u64, V>,
    last_hit: AHashMap<u64, u64>,
    tick: u64,
    cap: usize,
}

impl<V: Clone> LruCache<V> {
    fn new(cap: usize) -> Self {
        Self {
            entries: AHashMap::default(),
            last_hit: AHashMap::default(),
            tick: 0,
            cap,
        }
    }

    /// Look up `key`, bumping its recency on a hit so a hot entry survives
    /// eviction even under edit-spam on other content.
    fn get(&mut self, key: u64) -> Option<V> {
        let v = self.entries.get(&key)?.clone();
        self.tick += 1;
        self.last_hit.insert(key, self.tick);
        Some(v)
    }

    /// Insert (or refresh) `value` at `key`. A refresh of an existing key
    /// never evicts (P2's at-cap refresh fix, still correct under LRU — this
    /// supersedes it: a key already present just gets a new value + a bumped
    /// tick). A genuinely new key at cap evicts the single least-recently-hit
    /// entry first — D5's replacement for "stop inserting past the cap".
    fn insert(&mut self, key: u64, value: V) {
        if !self.entries.contains_key(&key)
            && self.entries.len() >= self.cap
            && let Some(evict_key) = self.last_hit.iter().min_by_key(|(_, t)| **t).map(|(k, _)| *k)
        {
            self.entries.remove(&evict_key);
            self.last_hit.remove(&evict_key);
        }
        self.tick += 1;
        self.last_hit.insert(key, self.tick);
        self.entries.insert(key, value);
    }

    /// Drop `key` outright. Test-only: used by the segment-pending-expiry
    /// test to reset state between test runs sharing this thread_local.
    #[cfg(test)]
    fn remove(&mut self, key: u64) {
        self.entries.remove(&key);
        self.last_hit.remove(&key);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    fn contains_key(&self, key: u64) -> bool {
        self.entries.contains_key(&key)
    }
}

thread_local! {
    /// Content-keyed fused-effect-view cache. `None` is cached too (negative
    /// cache) so a non-fusable shape isn't recompiled every rebuild.
    static FUSED_EFFECT_CACHE: std::cell::RefCell<LruCache<Option<Arc<LoadedPresetView>>>> =
        std::cell::RefCell::new(LruCache::new(FUSED_CACHE_CAP));
    /// Generator twin — values are fused defs (generators compile via `from_def`).
    static FUSED_GENERATOR_CACHE: std::cell::RefCell<LruCache<Option<Arc<EffectGraphDef>>>> =
        std::cell::RefCell::new(LruCache::new(FUSED_CACHE_CAP));
}

/// Test-only cache size observation for D8/P7 knob-invariance proofs.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) fn fused_effect_cache_len_for_test() -> usize {
    FUSED_EFFECT_CACHE.with(|c| c.borrow().len())
}

/// On-demand fused view for ANY effect shape, keyed by the def's structural
/// content. Cache hit → return; miss → compile (blocking, content thread),
/// cache, return. `base` supplies the canonical outer bindings + skip mode —
/// for an edited def these still address inner nodes by stable NodeId, and the
/// fuse retargets them onto the fused nodes (refusing, → `None` → unfused, if one
/// would strand). Selection reads only this; the blocking is just memoize-on-miss.
pub fn fused_view_for(def: &EffectGraphDef, base: &LoadedPresetView) -> Option<Arc<LoadedPresetView>> {
    let key = def_content_key(def);
    if let Some(cached) = FUSED_EFFECT_CACHE.with(|c| c.borrow_mut().get(key)) {
        return cached;
    }
    let compiled = compile_fused_view(def, base);
    FUSED_EFFECT_CACHE.with(|c| c.borrow_mut().insert(key, compiled.clone()));
    compiled
}

/// Pure codegen of a fused view from an arbitrary def + the canonical bindings /
/// skip mode to carry. No device, no UI state, no thread assumption — the unit
/// that relocates to a background worker later. `None` when the def has no
/// fusable region or a binding would strand.
fn compile_fused_view(def: &EffectGraphDef, base: &LoadedPresetView) -> Option<Arc<LoadedPresetView>> {
    let registry = PrimitiveRegistry::with_builtin();
    let fused =
        fuse_view_parts(def, &base.bindings, &base.type_id, base.skip_mode, &registry, None)?;
    Some(Arc::new(fused))
}

/// Generator twin of [`fused_view_for`]: a generator carries its modulation
/// bindings inside `def.preset_metadata.bindings`, so fusing is self-contained
/// (no separate `base`). Content-keyed, compile-on-miss, negative-cached.
pub fn fused_generator_def_for(def: &EffectGraphDef) -> Option<Arc<EffectGraphDef>> {
    let key = def_content_key(def);
    if let Some(cached) = FUSED_GENERATOR_CACHE.with(|c| c.borrow_mut().get(key)) {
        return cached;
    }
    let registry = PrimitiveRegistry::with_builtin();
    let compiled = fuse_generator_def(def, &registry).map(Arc::new);
    FUSED_GENERATOR_CACHE.with(|c| c.borrow_mut().insert(key, compiled.clone()));
    compiled
}

// ===========================================================================
// Cross-card chain fusion (docs/CHAIN_FUSION_DESIGN.md).
//
// A chain is already ONE runtime graph; fusion just ran per card def, so every
// card seam paid a full-canvas round-trip. A SEGMENT is a maximal run of ≥2
// adjacent eligible cards; its defs concatenate (freeze::segment) into one
// namespaced def that the EXISTING pipeline fuses — a region spanning a seam
// is just a region. Compilation + gate measurement run on a background worker:
// the chain renders per-card (today's path, byte-identical) the moment a
// segment is requested, and swaps to the fused segment on a later rebuild once
// the worker delivers a measured win. Fail-closed everywhere: any refusal —
// malformed card, stranded binding, no seam-spanning region, gate loss, build
// failure — negative-caches and the chain stays per-card forever (never-worse).
// ===========================================================================

/// Fused view of one chain segment. `Arc`-owned like every fused artifact
/// (bounded + LRU-evicted by [`FUSED_CACHE_CAP`] / [`LruCache`], D5).
pub struct SegmentView {
    /// The fused concatenated def — an ordinary Source→…→FinalOutput effect
    /// def, spliced into the chain ONCE in place of the member cards' splices.
    pub def: EffectGraphDef,
    /// Per-card static bindings (same order as the segment's cards), already
    /// namespaced (`c{i}.`) and retargeted onto the fused nodes. Each card's
    /// `EffectSlot` resolves its own slice against the segment splice, so
    /// every slider / MIDI map / envelope keeps its own card's
    /// `param_values` lane.
    pub card_bindings: Vec<Vec<ParamBinding>>,
    /// `(prefixed node_id, param) → (fused node id, uniform field)` — the
    /// user-binding repoint map. Chain build prefixes a user binding's target
    /// with its card's namespace before the lookup.
    pub retarget: AHashMap<(String, String), (NodeId, String)>,
}

/// Lookup outcome for a segment this frame.
pub enum SegmentLookup {
    /// Compiled and spliceable — render through the fused segment.
    Ready(Arc<SegmentView>),
    /// Codegen in flight on the worker — render per-card, a later rebuild
    /// swaps in.
    Pending,
    /// Refused (no seam-spanning region, stranded binding, or build failure) —
    /// render per-card, permanently for this content.
    Refused,
}

/// Chain-fusion kill switch, layered on the master [`freeze_enabled`] switch.
/// `MANIFOLD_CHAIN_FUSION=0` renders every chain per-card while leaving
/// per-card fusion intact.
pub fn chain_fusion_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    freeze_enabled()
        && *ENABLED.get_or_init(|| match std::env::var("MANIFOLD_CHAIN_FUSION") {
            Ok(v) => {
                !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no")
            }
            Err(_) => true,
        })
}

/// Segment identity: positional hash of the member cards' def content keys.
/// Equivalent discriminative power to hashing the concatenated def (the
/// namespacing is positional), without building the concat on every lookup.
pub fn segment_key(cards: &[(&EffectGraphDef, &'static LoadedPresetView)]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = ahash::AHasher::default();
    cards.len().hash(&mut h);
    for (def, _) in cards {
        def_content_key(def).hash(&mut h);
    }
    h.finish()
}

thread_local! {
    /// Content-keyed segment cache (content thread). `Some` = ready winner,
    /// `None` = refused (negative cache).
    static SEGMENT_CACHE: std::cell::RefCell<LruCache<Option<Arc<SegmentView>>>> =
        std::cell::RefCell::new(LruCache::new(FUSED_CACHE_CAP));
    /// Keys currently in flight on the worker — dedupes enqueues across the
    /// rebuilds that happen while a compile is pending. Value is the enqueue
    /// time, so [`pump_segment_results`] can expire a wedged key (D2,
    /// FUSION_SOTA_DESIGN §2): a panic already survives via `catch_unwind` in
    /// the worker, but a truly hung compile (infinite loop, not a panic) would
    /// otherwise leave the key `Pending` forever.
    static SEGMENT_PENDING: std::cell::RefCell<AHashMap<u64, std::time::Instant>> =
        std::cell::RefCell::new(AHashMap::default());
}

/// How long a segment compile may sit `Pending` before the pump gives up
/// waiting and negative-caches the key as `Refused`. Segment codegen is pure
/// CPU and measured in milliseconds (§8 of the map), so 60s crossing means the
/// worker is genuinely wedged, not just busy. A late result landing after
/// expiry still overwrites the negative-cache entry (`pump_segment_results`'s
/// insert path), so a slow-but-alive worker self-heals back to fused on its
/// next successful compile.
const SEGMENT_COMPILE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// Bumped once per worker result landed by [`pump_segment_results`]. A runtime
/// built while any of its segments were `Pending` records the generation it
/// saw; the dispatcher rebuilds it when this advances (the swap-in trigger).
static SEGMENT_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn segment_generation() -> u64 {
    SEGMENT_GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

struct SegmentJob {
    key: u64,
    /// Owned def clones — the live defs can mutate under editing while the
    /// worker runs.
    cards: Vec<(EffectGraphDef, &'static LoadedPresetView)>,
}

struct SegmentResult {
    key: u64,
    view: Option<Arc<SegmentView>>,
}

struct SegmentWorker {
    // Only sent on from the production (`not(test)`) path; in test builds the
    // worker is never fed, so the field reads as dead there.
    #[cfg_attr(test, allow(dead_code))]
    tx: std::sync::mpsc::Sender<SegmentJob>,
    /// Drained only by the content thread ([`pump_segment_results`]); the
    /// Mutex exists solely because `OnceLock` requires `Sync` — it is never
    /// contended.
    rx: std::sync::Mutex<std::sync::mpsc::Receiver<SegmentResult>>,
}

/// Set once the worker thread exists, so [`pump_segment_results`] (called
/// every chain dispatch) is a single relaxed load until the first segment is
/// actually enqueued.
static WORKER_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn segment_worker() -> &'static SegmentWorker {
    static WORKER: OnceLock<SegmentWorker> = OnceLock::new();
    WORKER.get_or_init(|| {
        WORKER_STARTED.store(true, std::sync::atomic::Ordering::Release);
        let (tx_job, rx_job) = std::sync::mpsc::channel::<SegmentJob>();
        let (tx_res, rx_res) = std::sync::mpsc::channel::<SegmentResult>();
        std::thread::Builder::new()
            .name("chain-fusion-worker".into())
            .spawn(move || {
                // Segment codegen is pure CPU (the fuse decision is structural —
                // the region partition — so there's no measurement). It runs off
                // the content thread only to keep a big concat's partition + WGSL
                // build off the live frame; no GPU device is needed.
                let registry = PrimitiveRegistry::with_builtin();
                while let Ok(job) = rx_job.recv() {
                    let card_refs: Vec<(&EffectGraphDef, &'static LoadedPresetView)> =
                        job.cards.iter().map(|(d, v)| (d, *v)).collect();
                    let view = compile_segment_view_panic_safe(&card_refs, &registry);
                    if tx_res.send(SegmentResult { key: job.key, view }).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn chain-fusion worker");
        SegmentWorker {
            tx: tx_job,
            rx: std::sync::Mutex::new(rx_res),
        }
    })
}

/// Drain finished segment compiles into the content-thread cache. Call at
/// chain-dispatch entry, before any rebuild decision. Each landed result bumps
/// the generation so runtimes holding per-card fallbacks rebuild and pick the
/// winner up.
pub fn pump_segment_results() {
    if !WORKER_STARTED.load(std::sync::atomic::Ordering::Acquire) {
        return;
    }
    let worker = segment_worker();
    let rx = worker.rx.lock().expect("segment worker rx poisoned");
    while let Ok(res) = rx.try_recv() {
        if let Some(v) = &res.view {
            eprintln!(
                "[freeze] chain segment ready: {} cards fused into {} nodes",
                v.card_bindings.len(),
                v.def.nodes.len(),
            );
        }
        SEGMENT_CACHE.with(|c| c.borrow_mut().insert(res.key, res.view));
        SEGMENT_PENDING.with(|p| {
            p.borrow_mut().remove(&res.key);
        });
        SEGMENT_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    expire_stale_segment_pending(std::time::Instant::now());
}

/// Expire `Pending` segment keys that have outlived [`SEGMENT_COMPILE_DEADLINE`]
/// into the negative cache (D2): the chain stops waiting on a wedged worker and
/// renders per-card, visibly (one log line per expired key, not per-frame spam —
/// the key is removed from `SEGMENT_PENDING` so it can't re-fire). Only walks
/// the pending map when it's non-empty, so this is zero-cost in the steady
/// state. `now` is injected so the unit test doesn't need a real 60s sleep.
fn expire_stale_segment_pending(now: std::time::Instant) {
    let expired: Vec<u64> = SEGMENT_PENDING.with(|p| {
        let pending = p.borrow();
        if pending.is_empty() {
            return Vec::new();
        }
        pending
            .iter()
            .filter(|(_, enqueued)| now.saturating_duration_since(**enqueued) >= SEGMENT_COMPILE_DEADLINE)
            .map(|(k, _)| *k)
            .collect()
    });
    for key in expired {
        SEGMENT_PENDING.with(|p| {
            p.borrow_mut().remove(&key);
        });
        SEGMENT_CACHE.with(|c| c.borrow_mut().insert(key, None));
        eprintln!("[freeze] chain segment compile timed out — rendering per-card …");
    }
}

/// Segment lookup for the chain builder. Hit → `Ready`/`Refused`; miss →
/// enqueue on the worker and report `Pending` (the chain splices per-card this
/// build). Content thread only. `cards` carries owned defs so relight-on
/// members can be augmented before fusion while keeping the view references
/// for bindings.
pub fn fused_segment_view_for(
    cards: &[(EffectGraphDef, &'static LoadedPresetView)],
) -> SegmentLookup {
    let card_refs: Vec<(&EffectGraphDef, &'static LoadedPresetView)> =
        cards.iter().map(|(d, v)| (d, *v)).collect();
    let key = segment_key(&card_refs);
    if let Some(cached) = SEGMENT_CACHE.with(|c| c.borrow_mut().get(key)) {
        return match cached {
            Some(view) => SegmentLookup::Ready(view),
            None => SegmentLookup::Refused,
        };
    }
    let newly_queued = SEGMENT_PENDING
        .with(|p| p.borrow_mut().insert(key, std::time::Instant::now()))
        .is_none();
    // Tests must stay deterministic: no worker thread, no 4K gate measurement
    // contending with the GPU-bound suite. Un-seeded segments stay Pending
    // forever (per-card render — today's path); fused paths are exercised via
    // `seed_segment_cache_for_test`.
    #[cfg(test)]
    {
        let _ = newly_queued;
        SegmentLookup::Pending
    }
    // Each cfg branch is a self-contained tail expression, so neither build
    // sees the other's `Pending` as unreachable code.
    #[cfg(not(test))]
    {
        if newly_queued {
            let job = SegmentJob {
                key,
                cards: cards.iter().map(|(d, v)| (d.clone(), *v)).collect(),
            };
            if segment_worker().tx.send(job).is_err() {
                // Worker died (startup panic) — refuse rather than wedge Pending.
                SEGMENT_PENDING.with(|p| {
                    p.borrow_mut().remove(&key);
                });
                SEGMENT_CACHE.with(|c| {
                    c.borrow_mut().insert(key, None);
                });
                return SegmentLookup::Refused;
            }
        }
        SegmentLookup::Pending
    }
}

// Test-only panic injection for `compile_segment_view` (D2, FUSION_SOTA_DESIGN
// §2): armed by `segment_worker_panic_refuses_key` to prove the worker survives
// a panicking compile. Compiled only under `#[cfg(test)]` — no production code
// path can reach it.
#[cfg(test)]
thread_local! {
    static PANIC_HOOK_ARMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn arm_segment_compile_panic_hook_for_test(armed: bool) {
    PANIC_HOOK_ARMED.with(|c| c.set(armed));
}

/// Panic-contained wrapper around [`compile_segment_view`] (D2): a malformed
/// segment definition or a codegen bug that panics mid-compile must refuse the
/// in-flight key (negative-caches as `Refused` via the `None` result) rather
/// than kill the `chain-fusion-worker` thread — every other pending/future
/// segment this session still gets serviced. Not a substitute for a genuine
/// hang (Rust can't kill a wedged thread); the pump-side deadline in
/// `pump_segment_results` handles that case.
fn compile_segment_view_panic_safe(
    cards: &[(&EffectGraphDef, &'static LoadedPresetView)],
    registry: &PrimitiveRegistry,
) -> Option<Arc<SegmentView>> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        compile_segment_view(cards, registry)
    })) {
        Ok(view) => view,
        Err(_) => {
            eprintln!("[freeze] chain segment compile panicked — refusing key, worker continues");
            None
        }
    }
}

/// Compile one segment: concat the member cards, fuse the seam-spanning regions
/// the partition finds, retarget bindings. Pure CPU codegen — the fuse decision
/// is structural (the region partition), so there is no GPU measurement. Runs on
/// the codegen worker in production and synchronously in tests.
pub(crate) fn compile_segment_view(
    cards: &[(&EffectGraphDef, &'static LoadedPresetView)],
    registry: &PrimitiveRegistry,
) -> Option<Arc<SegmentView>> {
    #[cfg(test)]
    if PANIC_HOOK_ARMED.with(|c| c.get()) {
        panic!("compile_segment_view: test panic hook armed (segment_worker_panic_refuses_key)");
    }
    let defs: Vec<&EffectGraphDef> = cards.iter().map(|(d, _)| *d).collect();
    let concat = super::segment::concat_defs(&defs)?;

    // Require at least one region that actually spans a seam. Without one the
    // segment buys nothing over the per-card fused views (and would discard
    // their per-card tuned region masks).
    let spans_seam = {
        let regions = partition_regions(&concat, registry);
        let prefix_of = |doc_id: u32| -> Option<&str> {
            let nid = concat.nodes.iter().find(|n| n.id == doc_id)?.node_id.as_str();
            nid.split_once('.').map(|(p, _)| p)
        };
        regions.iter().any(|r| {
            let mut prefixes = AHashSet::default();
            for m in &r.members {
                if let Some(p) = prefix_of(m.doc_id) {
                    prefixes.insert(p);
                }
            }
            prefixes.len() >= 2
        })
    };
    if !spans_seam {
        return None;
    }

    // Binding targets across the segment, namespaced to match `concat`'s
    // `c{i}.`-prefixed node ids. Each card's outer-card bindings are the live
    // performance surface, so the static-param baker must exclude them here too.
    let bound_targets: AHashSet<(String, String)> = cards
        .iter()
        .enumerate()
        .flat_map(|(ci, (_, view))| {
            let prefix = super::segment::card_prefix(ci);
            view.bindings.iter().filter_map(move |b| match &b.target {
                crate::node_graph::param_binding::ParamTarget::Node { node_id, param } => {
                    Some((format!("{prefix}{}", node_id.as_str()), param.to_string()))
                }
                _ => None,
            })
        })
        .collect();

    let fused = fuse_canonical_def_masked(&concat, registry, None, &bound_targets)?;
    if !fused_def_builds(&fused.def, registry, &fused.expected_spaces) {
        return None;
    }

    let surviving: AHashSet<String> = fused
        .def
        .nodes
        .iter()
        .map(|n| resolve_node_id(n).as_str().to_string())
        .collect();

    // Per-card binding retarget: prefix each card's spec-binding targets into
    // the segment namespace, then run the standard retarget (strand → refuse).
    let mut card_bindings: Vec<Vec<ParamBinding>> = Vec::with_capacity(cards.len());
    for (i, (_, view)) in cards.iter().enumerate() {
        let prefix = super::segment::card_prefix(i);
        let prefixed: Vec<ParamBinding> = view
            .bindings
            .iter()
            .map(|b| {
                let mut nb = b.clone();
                if let ParamTarget::Node { node_id, param } = &b.target {
                    nb.target = ParamTarget::Node {
                        node_id: NodeId::new(format!("{prefix}{}", node_id.as_str())),
                        param: param.clone(),
                    };
                }
                nb
            })
            .collect();
        // Composite / Custom targets resolve through per-splice handle maps
        // that don't exist for a segment splice — refuse, render per-card.
        if prefixed
            .iter()
            .any(|b| !matches!(b.target, ParamTarget::Node { .. }))
        {
            return None;
        }
        card_bindings.push(retarget_bindings(&prefixed, &fused.retarget, &surviving)?);
    }

    let FusedDef { def, retarget, .. } = fused;
    Some(Arc::new(SegmentView {
        def,
        card_bindings,
        retarget,
    }))
}

/// The production-fallback chain as one def: each card swapped for its
/// Test/tooling hook: compile a segment synchronously and seed the
/// content-thread cache, so integration tests exercise the Ready path without
/// the worker's asynchrony.
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) fn seed_segment_cache_for_test(
    cards: &[(&EffectGraphDef, &'static LoadedPresetView)],
    registry: &PrimitiveRegistry,
) -> Option<Arc<SegmentView>> {
    let view = compile_segment_view(cards, registry);
    let key = segment_key(cards);
    SEGMENT_CACHE.with(|c| {
        c.borrow_mut().insert(key, view.clone());
    });
    view
}

// ===========================================================================
// Generator fusion. A generator preset is the SAME `EffectGraphDef` as an
// effect, but its live render path ([`JsonGraphGenerator::from_def`]) reads its
// modulation bindings straight from the def's `preset_metadata.bindings`
// (`BindingDef`s) rather than from a separate `LoadedPresetView.bindings` list.
// So fusing a generator means rewriting the def with fused kernels (the shared
// `fuse_canonical_def`) AND retargeting those `BindingDef`s onto the fused node —
// the generator analog of `retarget_bindings`. The fused generator def then loads
// through the unchanged `from_def` path, so a wired generator param keeps
// modulating after its atom folds into a kernel.
// ===========================================================================

/// Fused generator def for a generator *type* — the canonical (shipped) shape.
/// Thin wrapper over the content-keyed [`fused_generator_def_for`]: parse the
/// bundled preset and route it through the same cache edited generator defs use.
/// `None` for any generator whose canonical graph has no fusable region, or whose
/// modulation bindings can't be retargeted (stranded) — either way it renders
/// unfused, always correct. Mirrors [`fused_view_by_id`].
pub fn fused_generator_def_by_id(id: &PresetTypeId) -> Option<Arc<EffectGraphDef>> {
    let json = crate::node_graph::bundled_presets::bundled_preset_json(id)?;
    let def: EffectGraphDef = serde_json::from_str(&json).ok()?;
    fused_generator_def_for(&def)
}

/// Fuse a generator's canonical def + retarget its `preset_metadata.bindings`
/// onto the fused nodes. `None` if nothing fuses or a binding strands. The result
/// loads through the same `from_def` path as the unfused preset — only the def
/// changed.
pub fn fuse_generator_def(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Option<EffectGraphDef> {
    fuse_generator_def_masked(def, registry, None)
}

/// [`fuse_generator_def`] with the perf gate's region mask — see
/// [`fuse_canonical_def_masked`].
pub(crate) fn fuse_generator_def_masked(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    region_mask: Option<&[bool]>,
) -> Option<EffectGraphDef> {
    let fused =
        fuse_canonical_def_masked(def, registry, region_mask, &def_binding_targets(def))?;
    // Node ids that survive (boundaries + fused nodes) — a binding targeting one
    // is left as-is; one targeting a fused-away member is retargeted; anything
    // else strands, so refuse to fuse (render unfused).
    let surviving: AHashSet<String> = fused
        .def
        .nodes
        .iter()
        .map(|n| resolve_node_id(n).as_str().to_string())
        .collect();
    let expected_spaces = fused.expected_spaces;
    let mut out_def = fused.def;
    if let Some(meta) = out_def.preset_metadata.as_mut() {
        meta.bindings = retarget_binding_defs(&meta.bindings, &fused.retarget, &surviving)?;
    }
    // The fused def must actually build (not just parse) AND preserve every
    // region output's element space — fall back to unfused otherwise.
    if !fused_def_builds(&out_def, registry, &expected_spaces) {
        return None;
    }
    Some(out_def)
}

/// Rewrite each `preset_metadata` `BindingDef` so it lands right after fusion: a
/// binding that drove a fused-away inner node is repointed at that node's fused
/// uniform field (`n{idx}_<param>`); one driving a surviving boundary is left
/// alone; one that hits neither strands modulation, so `None` (unfused fallback).
/// The generator twin of [`retarget_bindings`] — same routing, `BindingDef`
/// instead of `ParamBinding`.
fn retarget_binding_defs(
    bindings: &[BindingDef],
    retarget: &AHashMap<(String, String), (NodeId, String)>,
    surviving: &AHashSet<String>,
) -> Option<Vec<BindingDef>> {
    let mut out = Vec::with_capacity(bindings.len());
    for b in bindings {
        let mut nb = b.clone();
        if let BindingTarget::Node { node_id, param } = &b.target {
            let key = (node_id.as_str().to_string(), param.clone());
            if let Some((fused_id, field)) = retarget.get(&key) {
                nb.target = BindingTarget::Node {
                    node_id: fused_id.clone(),
                    param: field.clone(),
                };
                nb.convert = convert_for_fused_field(nb.convert);
            } else if !surviving.contains(node_id.as_str()) {
                return None; // stranded binding — refuse to fuse this generator
            }
            // else: drives a surviving boundary node — leave it exactly as-is.
        }
        // Composite targets route by outer name, never by a fused-away id.
        out.push(nb);
    }
    Some(out)
}

/// Rewrite each outer-card binding so it lands on the right place after fusion.
/// A binding that drove a fused-away inner node is repointed at that node's
/// region's fused uniform field; a binding driving a surviving boundary node is
/// left untouched; a binding that hits neither (a stranded slider) makes the
/// whole fusion unsafe — return `None` so the card renders unfused rather than
/// silently dropping live control.
fn retarget_bindings(
    base: &[ParamBinding],
    retarget: &AHashMap<(String, String), (NodeId, String)>,
    surviving: &AHashSet<String>,
) -> Option<Vec<ParamBinding>> {
    let mut out = Vec::with_capacity(base.len());
    for b in base {
        let mut nb = b.clone();
        if let ParamTarget::Node { node_id, param } = &b.target {
            let key = (node_id.as_str().to_string(), (*param).to_string());
            if let Some((fused_id, field)) = retarget.get(&key) {
                nb.target = ParamTarget::Node {
                    node_id: fused_id.clone(),
                    // Owned, not leaked (D5): `ParamTarget::Node::param` is
                    // `Cow` — a per-fuse field name owns its `String` instead
                    // of leaking one per fuse-build.
                    param: std::borrow::Cow::Owned(field.clone()),
                };
                nb.convert = convert_for_fused_field(nb.convert);
            } else if !surviving.contains(node_id.as_str()) {
                // Neither retargeted nor surviving — a stranded slider. Refuse.
                return None;
            }
            // else: drives a surviving boundary node — leave it exactly as-is.
        }
        // Composite / Custom targets route by outer-name / fn pointer, never by a
        // fused-away inner node id, so they pass through unchanged.
        out.push(nb);
    }
    Some(out)
}

/// The convert a binding needs once it's repointed onto a fused uniform field.
/// An `EnumRound` convert targeted the inner atom's Enum param; the fused field
/// for that param is a plain u32 uniform member, whose writer
/// (`UniformMemberType::write_to`) consumes `ParamValue::Float` and casts
/// `f.max(0.0) as u32` at the write boundary — `ParamValue::Enum` would land in
/// its silent-zeros mismatch arm, and the loader's convert check rejects it
/// outright (`BindingConvertTypeMismatch`). `IntRound` produces
/// `Float(v.round())`, so the value the field receives is identical to
/// `EnumRound`'s `round().max(0)` for every input. Everything else already
/// writes a variant the scalar field accepts and passes through unchanged.
/// This rewrite is what lets binding-targeted Enum atoms fuse (FluidSim3D's
/// `container` → container_repel_force_3d) instead of being classify gated.
pub(crate) fn convert_for_fused_field(convert: ParamConvert) -> ParamConvert {
    match convert {
        ParamConvert::EnumRound => ParamConvert::IntRound,
        other => other,
    }
}

/// A canonical def rewritten with one fused node per region, plus the routing the
/// binding retarget needs. `pub(crate)` so the end-to-end oracle test can drive
/// both the unfused and fused graphs from one fixture (set inner params by stable
/// node id on the unfused side, by the `retarget`ed `(fused id, field)` on the
/// fused side).
pub(crate) struct FusedDef {
    pub def: EffectGraphDef,
    /// `(original stable node_id, original param) → (fused node id, fused uniform
    /// field)`. The field is `"n{idx}_{param}"` (`idx` = the member's topo index
    /// within its region — the codegen convention); the node id is that region's
    /// `fused_region_{i}`.
    pub retarget: AHashMap<(String, String), (NodeId, String)>,
    /// Tier 6: `(fused doc id, output port, space)` per texture-region output —
    /// the element space the replaced member's output resolved to in the
    /// UNFUSED plan. [`fused_def_builds`] verifies the fused def resolves each
    /// of these ports to the SAME space; any drift rejects the fusion (renders
    /// unfused), so element-space preservation is an installed invariant.
    pub expected_spaces: Vec<(u32, String, ElementSpace)>,
}

/// `node.array_feedback`'s stable type id — the head of a buffer in-place
/// feedback loop. The in-place fusion gate keys on it (see
/// [`external_is_inplace_loop`]).
const ARRAY_FEEDBACK_TYPE_ID: &str = "node.array_feedback";

/// The WGSL storage-format token the unfused executor allocates a TEXTURE
/// member's output at — its `outputFormats` fp32 override (`"rgba32float"`) else
/// the `"rgba16float"` working default. When this member is a region OUTPUT, the
/// fused codegen declares its `dst` at this format, so a fused region honours an
/// fp32 output the same way the unfused chain does — the dst half of
/// full-precision in-loop fusion. Buffer members have no texture output → default.
fn resolve_output_storage(
    doc_node: &EffectGraphNode,
    node: &dyn crate::node_graph::effect_node::EffectNode,
) -> &'static str {
    let tex_out = node
        .outputs()
        .iter()
        .find(|o| matches!(o.ty, PortType::Texture2D))
        .map(|o| o.name.clone());
    match tex_out.as_ref().and_then(|name| doc_node.output_formats.get(name.as_ref())) {
        Some(s) if s.contains("32float") => "rgba32float",
        _ => "rgba16float",
    }
}

/// The address-mode token (`"clamp"` / `"repeat"` / `"mirror"`) the fused
/// region's single shared gather `samp` sampler must bind, or `None` when the
/// region's `Gather` members disagree on a mode — one sampler can't serve two,
/// so the caller leaves the card unfused (safe, just no speedup; conflicts don't
/// occur in any shipped preset). A region with no gather members, or whose
/// gathers all want the default clamp, returns `"clamp"` — no marker, the
/// historical byte-identical sampler. Mirrors each gather atom's standalone
/// sampler choice via [`EffectNode::fused_gather_sampler_mode`], fed the member's
/// effective params (def override else atom default), so a `wrap_mode = Repeat`
/// gradient resolves to a repeat sampler the toroidal flow field needs.
fn resolve_gather_sampler_mode(
    region: &Region,
    registry: &PrimitiveRegistry,
    def: &EffectGraphDef,
) -> Option<&'static str> {
    let mut mode: Option<manifold_gpu::GpuAddressMode> = None;
    for member in &region.members {
        if !member.input_access.iter().any(|a| a.is_gather()) {
            continue; // no gather input — doesn't touch the sampler
        }
        let doc_node = def.nodes.iter().find(|n| n.id == member.doc_id)?;
        let node = crate::node_graph::freeze::region::configured_construct(registry, doc_node)?;
        // Effective params as f32 (the gather-mode override reads its enum from a
        // Float or Enum value); covers every scalar param the finder admits.
        let mut params: crate::node_graph::effect_node::ParamValues = AHashMap::default();
        for p in node.parameters() {
            if let Some(v) = effective_param_f32(doc_node.params.get(p.name.as_ref()), &p.default) {
                params.insert(p.name.clone(), ParamValue::Float(v));
            }
        }
        let m = node.fused_gather_sampler_mode(&params);
        match mode {
            Some(existing) if existing != m => return None, // two gathers disagree
            _ => mode = Some(m),
        }
    }
    Some(match mode {
        Some(manifold_gpu::GpuAddressMode::Repeat) => "repeat",
        Some(manifold_gpu::GpuAddressMode::MirrorRepeat) => "mirror",
        _ => "clamp",
    })
}

/// Trace a single-output BUFFER region's output back through its members'
/// `aliased_array_io` chain to the external input it ultimately writes IN PLACE.
/// Returns that external index, or `None` if the region isn't a clean in-place
/// chain: multi-output, a member that isn't `aliased_array_io` (so it can't be
/// threading one buffer in place), or the chain doesn't bottom out at an external
/// input. Pure structural analysis — the caller still gates on whether that
/// external is an actual feedback-loop buffer ([`external_is_inplace_loop`]).
fn region_output_aliases_external(
    region: &Region,
    registry: &PrimitiveRegistry,
    def: &EffectGraphDef,
) -> Option<usize> {
    if region.outputs.len() != 1 {
        return None;
    }
    let mut cur_doc = region.outputs[0].0;
    // Bounded walk: a region has finitely many members; the cap is a cycle guard.
    for _ in 0..64 {
        let member = region.members.iter().find(|m| m.doc_id == cur_doc)?;
        let doc_node = def.nodes.iter().find(|n| n.id == cur_doc)?;
        let node = crate::node_graph::freeze::region::configured_construct(registry, doc_node)?;
        // v1: a single aliased in/out pair — the member mutates its input buffer.
        let (in_port, _out_port) = *node.aliased_array_io().first()?;
        // Index of the aliased input among the member's ARRAY input ports — the
        // same order `RegionMember.inputs` (and the codegen's `InputSource`) use.
        let in_idx = node
            .inputs()
            .iter()
            .filter(|p| matches!(p.ty, PortType::Array(_)))
            .position(|p| p.name == in_port)?;
        match member.inputs.get(in_idx)? {
            RegionInput::External(e) => return Some(*e),
            RegionInput::Member(prev) => cur_doc = *prev,
            // BUFFER regions never carry a multi-output (texture-domain) producer —
            // buffer atoms with texture outputs are boundaries — so this never
            // actually fires; kept for match exhaustiveness only.
            RegionInput::MemberPort(prev, _) => cur_doc = *prev,
            RegionInput::Unwired => return None, // no buffer threads through an unwired port
            RegionInput::Virtual(_) => return None, // stencil chains are texture-domain only
        }
    }
    None
}

/// Is `region.externals[ext_idx]` the buffer of a `node.array_feedback` IN-PLACE
/// loop? Walks the producer chain backward through `aliased_array_io` nodes: an
/// `array_feedback` head ⇒ yes (a true loop buffer, safe to alias as the fused
/// output); a forward (non-aliased) producer ⇒ no (aliasing it would reintroduce
/// the ordering bug the fresh-`dst` model avoids — the input has a producer that
/// must run first). A producer already fused away ⇒ conservatively no.
fn external_is_inplace_loop(
    region: &Region,
    ext_idx: usize,
    registry: &PrimitiveRegistry,
    def: &EffectGraphDef,
    member_region: &AHashMap<u32, usize>,
) -> bool {
    let Some(ext) = region.externals.get(ext_idx) else {
        return false;
    };
    let mut node = ext.from_node;
    let mut port = ext.from_port.clone();
    for _ in 0..128 {
        if member_region.contains_key(&node) {
            return false; // producer fused away — can't verify the loop
        }
        let Some(n) = def.nodes.iter().find(|x| x.id == node) else {
            return false;
        };
        if n.type_id == ARRAY_FEEDBACK_TYPE_ID {
            return true;
        }
        let Some(prim) = registry.construct(&n.type_id) else {
            return false;
        };
        // Follow the aliased pair whose OUTPUT is the port we arrived on, back to
        // its INPUT's producer. A non-aliased producer ends the walk (not a loop).
        let aliasing = prim.aliased_array_io();
        let Some((in_port, _)) = aliasing.iter().copied().find(|(_, op)| *op == port) else {
            return false;
        };
        let Some(w) = def
            .wires
            .iter()
            .find(|w| w.to_node == node && w.to_port == in_port)
        else {
            return false;
        };
        node = w.from_node;
        port = w.from_port.clone();
    }
    false
}

/// Resolve the live-count dispatch cap for an IN-PLACE buffer region: every
/// member must declare a [`fused_dispatch_count_param`] (the particle
/// integrators' `active_count`) AND every one of those params must be driven
/// by the SAME producer wire — then one uniform field is authoritative for the
/// whole region and the fused kernel can dispatch (and guard) at the live
/// count instead of the pool capacity, exactly like the standalone dispatches.
/// Any member without the hint, an unwired count param, or disagreeing
/// producers ⇒ `None` (capacity dispatch — always correct, just wider).
///
/// Returns `(member index, param name)` of the first member; the codegen
/// formats the field as `n{i}_<param>`.
///
/// [`fused_dispatch_count_param`]: crate::node_graph::effect_node::EffectNode::fused_dispatch_count_param
fn resolve_dispatch_count_field(
    region: &Region,
    registry: &PrimitiveRegistry,
    def: &EffectGraphDef,
) -> Option<(usize, &'static str)> {
    let mut field: Option<(usize, &'static str)> = None;
    let mut source: Option<(u32, String)> = None;
    for (i, member) in region.members.iter().enumerate() {
        let doc_node = def.nodes.iter().find(|n| n.id == member.doc_id)?;
        let node = crate::node_graph::freeze::region::configured_construct(registry, doc_node)?;
        let param = node.fused_dispatch_count_param()?;
        let wire = def
            .wires
            .iter()
            .find(|w| w.to_node == member.doc_id && w.to_port == param)?;
        let src = (wire.from_node, wire.from_port.clone());
        match &source {
            None => {
                source = Some(src);
                field = Some((i, param));
            }
            Some(s) if *s == src => {}
            Some(_) => return None, // members disagree — capacity dispatch
        }
    }
    field
}

/// `(node_id, param)` targets of every outer-card binding in a renderer-side
/// `ParamBinding` slice. These are the LIVE performance surface — sliders,
/// drivers, Ableton, LFOs, envelopes all write them via `param_values` every
/// frame — so the fuse must keep them uniform, never bake them static.
fn binding_targets(bindings: &[ParamBinding]) -> AHashSet<(String, String)> {
    bindings
        .iter()
        .filter_map(|b| match &b.target {
            crate::node_graph::param_binding::ParamTarget::Node { node_id, param } => {
                Some((node_id.as_str().to_string(), param.to_string()))
            }
            _ => None,
        })
        .collect()
}

/// `(node_id, param)` targets carried in a def's own `preset_metadata.bindings`
/// — how generators (and shipped/edited single-card effect defs) declare their
/// outer-card bindings. Same live-surface meaning as [`binding_targets`].
fn def_binding_targets(def: &EffectGraphDef) -> AHashSet<(String, String)> {
    def.preset_metadata
        .as_ref()
        .map(|m| {
            m.bindings
                .iter()
                .filter_map(|b| match &b.target {
                    manifold_core::effect_graph_def::BindingTarget::Node { node_id, param } => {
                        Some((node_id.as_str().to_string(), param.clone()))
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Partition `def` into its fusable regions and rewrite it with one fused
/// `node.wgsl_compute` per region. Returns `None` (leave the card entirely
/// unfused) when nothing fuses. Conservative throughout: any inability to
/// express a region's params, body, or wiring aborts the whole rewrite.
// Live callers all pass a mask now; the unmasked form remains the proof/test
// surface (every oracle drives fusion through it).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn fuse_canonical_def(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> Option<FusedDef> {
    fuse_canonical_def_masked(def, registry, None, &def_binding_targets(def))
}

/// How many fusable regions the canonical def partitions into (after the same
/// flatten the fuse performs). 0 ⇒ nothing fuses. Test-only since the structural
/// fuse path consumes the partition directly rather than its count.
#[cfg(test)]
pub(crate) fn canonical_region_count(def: &EffectGraphDef, registry: &PrimitiveRegistry) -> usize {
    let Ok(flattened) = manifold_core::flatten::flatten_groups(def) else {
        return 0;
    };
    partition_regions(&flattened, registry).len()
}

/// [`fuse_canonical_def`] with a REGION MASK: `mask[i] = false` leaves the
/// partition's i-th region (deterministic order) unfused — its members (and
/// any absorbed virtual-chain members) survive as ordinary nodes. The perf
/// gate explores masks per card so one slow region can't drag the rest back
/// to fully-unfused; `None` (or all-true) fuses everything, byte-identical to
/// the unmasked path. All-false (or all regions masked off) returns `None`.
pub(crate) fn fuse_canonical_def_masked(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    region_mask: Option<&[bool]>,
    // Outer-card binding targets `(node_id, param)` — the live performance
    // surface. A field they drive must never be tagged `@static_param` (baked),
    // or every dragged/modulated value recompiles the fused kernel.
    bound_targets: &AHashSet<(String, String)>,
) -> Option<FusedDef> {
    // The finder operates on a FLATTENED graph: `partition_regions` refuses any
    // def still carrying a group node (group boundary nodes would fragment every
    // region), and the live loader (`graph_loader`) flattens before building. So
    // flatten here too — otherwise a grouped preset (Glitch, FluidSim2D)
    // silently never fuses even though its flattened form has regions. Flatten
    // PRESERVES each node's stable `node_id` (only the debug handle is prefixed),
    // so the binding retarget downstream — which keys on `node_id` via
    // `resolve_node_id` — still lands correctly. An ungrouped def is returned
    // clone-equal (ids byte-identical), making this a no-op for the common case;
    // a malformed group def errors out to "render unfused", always safe.
    let flattened = manifold_core::flatten::flatten_groups(def).ok()?;
    let def = &flattened;
    let regions = partition_regions(def, registry);
    let regions: Vec<_> = match region_mask {
        None => regions,
        Some(mask) => regions
            .into_iter()
            .enumerate()
            .filter(|(i, _)| mask.get(*i).copied().unwrap_or(true))
            .map(|(_, r)| r)
            .collect(),
    };
    if regions.is_empty() {
        return None;
    }

    // Which region (if any) each fused-away node belongs to — main members AND
    // virtual chain members (both are deleted from the installed def; a chain's
    // work lives inside its consumer's fetch).
    let mut member_region: AHashMap<u32, usize> = AHashMap::default();
    for (i, r) in regions.iter().enumerate() {
        for m in &r.members {
            member_region.insert(m.doc_id, i);
        }
        for c in &r.virtual_chains {
            for m in &c.members {
                member_region.insert(m.doc_id, i);
            }
        }
    }

    let max_id = def.nodes.iter().map(|n| n.id).max().unwrap_or(0);
    let mut new_nodes: Vec<EffectGraphNode> = Vec::new();
    let mut retarget: AHashMap<(String, String), (NodeId, String)> = AHashMap::default();
    let mut fused_docs: Vec<u32> = Vec::with_capacity(regions.len());
    // Tier 6: per texture-region output, the element space the unfused member
    // resolved to — verified against the fused def by `fused_def_builds`.
    let mut expected_spaces: Vec<(u32, String, ElementSpace)> = Vec::new();
    // Per region (parallel to `fused_docs`): `Some(k)` if its output is written in
    // place to external `src_k` (an aliased feedback-loop buffer), so the output
    // rewrite below routes consumers off the `src_k` port instead of `dst`.
    let mut region_in_place: Vec<Option<usize>> = Vec::with_capacity(regions.len());
    // Control wires re-anchored onto a fused node's port-shadow: (fused_doc,
    // producer node, producer port, `n{idx}_<param>` field). Emitted after the
    // texture rewrite so the producer (a surviving boundary) is already in place.
    let mut control_wires: Vec<(u32, u32, String, String)> = Vec::new();

    for (i, region) in regions.iter().enumerate() {
        // ── Build the codegen region from this component's members (topo order)
        // PLUS its virtual chain members (appended after, in chain order — their
        // params join the merged uniform under the same `n{idx}` numbering, but
        // cs_main skips them; the consumer's fetch evaluates them per corner),
        // resolving each member's inputs to an external slot, an earlier
        // member's register, or a virtual chain. ──
        let all_members: Vec<&crate::node_graph::freeze::region::RegionMember> = region
            .members
            .iter()
            .chain(region.virtual_chains.iter().flat_map(|c| c.members.iter()))
            .collect();
        let node_index_of =
            |doc: u32| -> Option<usize> { all_members.iter().position(|m| m.doc_id == doc) };
        let mut region_nodes: Vec<RegionNode<'_>> = Vec::with_capacity(all_members.len());
        // D7/P0 (`docs/CINEMATIC_POST_DESIGN.md`): distinct Camera-producing
        // externals this region's derived-uniform members need, deduped by
        // (producer doc id, producer output port). Assigned `camera_ext_{index}`
        // names below; wired onto the fused node once `fused_doc` is known, via
        // the SAME `control_wires` mechanism ordinary control wires use — a
        // Camera-typed wire into a Camera-typed port is an unremarkable, plain
        // typed wire once `node.wgsl_compute` declares that port (see
        // `emit_derived_uniform_markers` in codegen.rs / `introspect` in
        // `primitives/wgsl_compute.rs`).
        let mut camera_ext_producers: Vec<(u32, String)> = Vec::new();
        // FUSION_SOTA_DESIGN P7 (D5): `RegionNode<'a>` is already
        // lifetime-generic, so the params/inputs/outputs it borrows off each
        // constructed `node` don't need `'static` — they only need to outlive
        // this region's `codegen::generate_fused` call below. The old code
        // leaked (`leak_params`/`leak_ports`/the substituted body) because
        // `node: Box<dyn EffectNode>` would otherwise drop at the end of each
        // loop iteration while `region_nodes` (built across iterations) still
        // borrowed from it. Fix: keep every member's `node` alive in
        // `node_keepalive` for the region's whole codegen call instead of
        // leaking. Pass 1 does every "may bail to unfused" check (order is
        // irrelevant — a bail aborts the whole function regardless of which
        // member triggered it) and stashes the per-member owned side-data;
        // pass 2 (below, once `node_keepalive` has no more pushes coming)
        // builds `region_nodes` borrowing straight off it.
        struct BuiltMember {
            body: std::borrow::Cow<'static, str>,
            derived_camera_ext: Option<usize>,
        }
        let mut node_keepalive: Vec<Box<dyn crate::node_graph::effect_node::EffectNode>> =
            Vec::with_capacity(all_members.len());
        let mut built: Vec<BuiltMember> = Vec::with_capacity(all_members.len());
        for member in &all_members {
            let doc_node = def.nodes.iter().find(|n| n.id == member.doc_id)?;
            let node = crate::node_graph::freeze::region::configured_construct(registry, doc_node)?;
            // Specialization tokens baked from the def's static params (classify
            // already gated binding-targeted / control-wired ones).
            // `substituted_body` already returns `Cow<'static, str>` (the
            // `Borrowed` arm is a compile-time WGSL const; the `Owned` arm is
            // a per-fuse-formatted `String`) — own it, no leak needed.
            let body = crate::node_graph::freeze::region::substituted_body(node.as_ref(), doc_node)?;
            let derived = node.derived_uniforms();
            // D7/P0: a member with declared derived uniforms must have a
            // registered recompute or the whole region bails to unfused — the
            // fail-closed contract the deleted install-time name whitelist had
            // (an unrecognized name used to `return None`; an unregistered
            // type_id does the same now, data-driven instead of name-matched).
            if !derived.is_empty()
                && !crate::node_graph::freeze::derived_uniform_registry::has_recompute(
                    &doc_node.type_id,
                )
            {
                return None;
            }
            // If this member has a wired Camera input port, route that producer
            // to the fused node as a distinct `camera_ext_N` external (deduped
            // across members feeding the same producer wire). A member with
            // derived_uniforms but no wired Camera port (the whole time-family)
            // needs no camera routing — `derived_camera_ext` stays `None`.
            let derived_camera_ext = if derived.is_empty() {
                None
            } else {
                let camera_wire = node
                    .inputs()
                    .iter()
                    .find(|i| i.ty == PortType::Camera)
                    .and_then(|inp| {
                        def.wires
                            .iter()
                            .find(|w| w.to_node == member.doc_id && w.to_port == inp.name.as_ref())
                    });
                match camera_wire {
                    Some(cw) => {
                        if member_region.contains_key(&cw.from_node) {
                            return None; // camera producer fused away — can't route it
                        }
                        let key = (cw.from_node, cw.from_port.clone());
                        let idx = camera_ext_producers.iter().position(|e| *e == key).unwrap_or_else(|| {
                            camera_ext_producers.push(key.clone());
                            camera_ext_producers.len() - 1
                        });
                        Some(idx)
                    }
                    None => None,
                }
            };
            built.push(BuiltMember { body, derived_camera_ext });
            node_keepalive.push(node);
        }
        // Pass 2: `node_keepalive` has every member's node, fully populated —
        // no more pushes, so borrowing off it (and off `built`) for the rest
        // of this region's codegen is sound.
        for (idx, member) in all_members.iter().enumerate() {
            let doc_node = def.nodes.iter().find(|n| n.id == member.doc_id)?;
            let node = &node_keepalive[idx];
            let inputs: Vec<InputSource> = member
                .inputs
                .iter()
                .map(|ri| match ri {
                    RegionInput::External(e) => InputSource::External(*e),
                    RegionInput::Member(doc) => InputSource::Node(NodeInstanceId(*doc)),
                    RegionInput::MemberPort(doc, port) => {
                        InputSource::NodeOutput(NodeInstanceId(*doc), port.clone())
                    }
                    RegionInput::Unwired => InputSource::Unwired,
                    RegionInput::Virtual(ci) => InputSource::Virtual(*ci),
                })
                .collect();
            region_nodes.push(RegionNode {
                node_id: NodeInstanceId(member.doc_id),
                fusion_kind: node.fusion_kind(),
                body: built[idx].body.as_ref(),
                params: node.parameters(),
                inputs,
                input_access: member.input_access.clone(),
                node_inputs: node.inputs(),
                node_outputs: node.outputs(),
                node_includes: node.wgsl_includes(),
                derived_uniforms: node.derived_uniforms(),
                type_id: doc_node.type_id.clone(),
                derived_camera_ext: built[idx].derived_camera_ext,
                output_storage: resolve_output_storage(doc_node, node.as_ref()),
                stencil_fetch: node.stencil_fetch(),
                quantize_f16: member.quantize_f16,
            });
        }
        // In-place gate: if the region's output threads (through aliased members)
        // back to an external that is an `array_feedback` loop buffer, the fused
        // kernel must write back into THAT buffer in place — else the loop's
        // `in==out` aliasing breaks and array_feedback drops to a one-frame copy
        // delay (the FluidSim divergence). Forward-produced regions (DigitalPlants)
        // trace to a non-loop external → None → keep the fresh-`dst` model.
        let in_place_alias = region_output_aliases_external(region, registry, def)
            .filter(|&e| external_is_inplace_loop(region, e, registry, def, &member_region));

        // The region's shared gather sampler mode (clamp default; repeat for a
        // toroidal gradient). `None` ⇒ two gather members want different modes ⇒
        // leave the whole card unfused (a single `samp` can't serve both).
        let sampler_address_mode = resolve_gather_sampler_mode(region, registry, def)?;

        // Live-count dispatch cap: only meaningful for an in-place loop region
        // (the pool tail must stay untouched either way; a fresh-dst region's
        // unwritten tail would be garbage, so it keeps the capacity dispatch).
        let dispatch_count_field = if in_place_alias.is_some() {
            resolve_dispatch_count_field(region, registry, def)
        } else {
            None
        };

        // Virtual chains resolved to region-node indices (the finder's doc-id
        // form → the codegen's `n{idx}` numbering over `all_members`).
        let mut virtual_chains = Vec::with_capacity(region.virtual_chains.len());
        for c in &region.virtual_chains {
            let members: Option<Vec<usize>> =
                c.members.iter().map(|m| node_index_of(m.doc_id)).collect();
            virtual_chains.push(codegen::FusedVirtualChain {
                consumer: node_index_of(c.consumer)?,
                input_index: c.input_index,
                members: members?,
                output: node_index_of(c.output)?,
            });
        }

        let fusion_region = FusionRegion {
            nodes: region_nodes,
            num_external_inputs: region.externals.len(),
            outputs: region
                .outputs
                .iter()
                .map(|(d, p)| (NodeInstanceId(*d), p.clone()))
                .collect(),
            in_place_alias,
            sampler_address_mode,
            dispatch_count_field,
            virtual_chains,
            sampled_externals: region.sampled_externals.clone(),
            camera_externals: camera_ext_producers.len(),
        };
        let generated = codegen::generate_fused(&fusion_region).ok()?;
        // Defense in depth: the fused kernel must parse through the plain pipeline
        // compiler — the same `naga` front-end the live `WgslCompute` node uses. The
        // classify gate already keeps specialization / free-identifier atoms out of
        // regions, but two bodies could still collide at module scope (e.g. two
        // same-named consts with different values, which dedup can't merge). If the
        // kernel doesn't parse, leave the whole card unfused rather than ship a
        // node whose introspection silently fails back to its default shape.
        if let Err(e) = naga::front::wgsl::parse_str(&generated.wgsl) {
            // Falls back to unfused (always correct) — but a region the codegen
            // emitted and naga rejected is a codegen bug worth surfacing.
            log::warn!("[freeze] fused region {i} failed to parse, card renders unfused: {e:?}");
            return None;
        }

        // ── Seed the fused node's params (def override else atom default) + the
        // retarget map. The field `n{idx}_{param}` matches the codegen's
        // region-topo-index convention. ──
        let fused_doc = max_id + 1 + i as u32;
        let fused_id = NodeId::new(format!("fused_region_{i}").as_str());
        let mut fused_params: BTreeMap<String, SerializedParamValue> = BTreeMap::new();
        // Fields this region exposes to an outer-card binding (live performance
        // surface). Excluded from `@static_param` below so a dragged/modulated
        // slider never recompiles the fused kernel.
        let mut bound_fields: AHashSet<String> = AHashSet::default();
        for (idx, member) in all_members.iter().enumerate() {
            let doc_node = def.nodes.iter().find(|n| n.id == member.doc_id)?;
            let node = crate::node_graph::freeze::region::configured_construct(registry, doc_node)?;
            let stable = resolve_node_id(doc_node);
            for p in node.parameters() {
                let field = format!("n{idx}_{}", p.name);

                // Vec3/Vec4/Color (P5/D4 lift): the codegen field is split into
                // 3/4 namespaced scalar sub-fields (`field_x`/`_y`/`_z`[`_w`]) —
                // see `codegen.rs`'s matching struct-emission blocks. Seed each
                // sub-field's initial value here; there is no single `field`
                // uniform member to seed directly for these types.
                //
                // `bound_targets`/`retarget` are skipped for these types:
                // `ParamConvert` (param_binding.rs) has no Vec3/Vec4/Color
                // variant, so no outer-card binding can ever target one —
                // `bound_targets.contains(&(stable, p.name))` is always false
                // for a non-scalar param, and there is no single field to point
                // a retarget entry at anyway (the codegen split it into parts).
                if matches!(p.ty, ParamType::Vec3 | ParamType::Vec4 | ParamType::Color) {
                    // A wire into a Vec3/Vec4/Color param is not representable
                    // by this seeding (a control wire carries one scalar value,
                    // and there's no single field left to re-anchor it onto) —
                    // refuse the whole region rather than silently drop it or
                    // mis-wire a component. No shipped preset wires a control
                    // producer into one of these param types today (they're
                    // static colour/vector knobs), so this is not expected to
                    // ever fire; it exists as a fail-closed guard.
                    if def.wires.iter().any(|w| w.to_node == member.doc_id && w.to_port == p.name)
                    {
                        return None;
                    }
                    let comps: Vec<f32> = match p.ty {
                        ParamType::Vec3 => {
                            effective_param_vec3(doc_node.params.get(p.name.as_ref()), &p.default)?
                                .to_vec()
                        }
                        _ => effective_param_vec4(doc_node.params.get(p.name.as_ref()), &p.default)?
                            .to_vec(),
                    };
                    for (comp, suffix) in comps.iter().zip(["_x", "_y", "_z", "_w"]) {
                        fused_params.insert(
                            format!("{field}{suffix}"),
                            SerializedParamValue::Float { value: *comp },
                        );
                    }
                    continue;
                }

                if bound_targets.contains(&(stable.as_str().to_string(), p.name.to_string())) {
                    bound_fields.insert(field.clone());
                }
                // D8/P7: relight template params are live uniforms written per-frame;
                // they must never be `@static_param`-specialized, or the fused kernel
                // would bake the default seed value and ignore knob drags.
                if crate::node_graph::relight::is_relight_node_id(stable.as_str()) {
                    bound_fields.insert(field.clone());
                }
                retarget.insert(
                    (stable.as_str().to_string(), p.name.to_string()),
                    (fused_id.clone(), field.clone()),
                );
                let value = effective_param_f32(doc_node.params.get(p.name.as_ref()), &p.default)?;
                fused_params.insert(field.clone(), SerializedParamValue::Float { value });

                // A control wire driving this param (LFO → gain.gain) is re-anchored
                // onto the fused node's port-shadow `n{idx}_<param>`, so the producer
                // keeps driving it every frame (DD-A5). The seeded value above is the
                // fallback the shadow port overrides. The producer is a control
                // producer and so a boundary (survives) — guard defensively.
                if let Some(cw) = def
                    .wires
                    .iter()
                    .find(|w| w.to_node == member.doc_id && w.to_port == p.name)
                {
                    if member_region.contains_key(&cw.from_node) {
                        return None; // producer fused away — can't route its scalar
                    }
                    control_wires.push((fused_doc, cw.from_node, cw.from_port.clone(), field));
                }
            }

            // Frame-derived uniforms (dt_scaled, frame_count, a camera's
            // cam_fwd_x/_y/_z, …): D7/P0 deleted the install-time name whitelist
            // that used to wire each from `system.generator_input` here. There is
            // nothing left to DO at install time — `node.wgsl_compute` recomputes
            // every derived-uniform field itself, every frame, from the
            // `// @derived_uniform_member:` marker `emit_derived_uniform_markers`
            // wrote into this member's slice of the fused kernel (registry lookup
            // by type_id — `derived_uniform_registry::recompute`, gated at
            // fuse-build time above by `has_recompute`). See
            // `docs/CINEMATIC_POST_DESIGN.md` D7 and
            // `crates/manifold-renderer/src/node_graph/freeze/derived_uniform_registry.rs`.
        }
        // D7/P0: wire each distinct Camera external this region's derived-uniform
        // members need onto the fused node's synthesized `camera_ext_N` port —
        // the SAME `control_wires` mechanism ordinary param control wires use
        // (a plain node→port wire; `camera_ext_N` is Camera-typed on both ends).
        for (n, (from_node, from_port)) in camera_ext_producers.iter().enumerate() {
            control_wires.push((
                fused_doc,
                *from_node,
                from_port.clone(),
                format!("camera_ext_{n}"),
            ));
        }

        // Tier 6: stamp the region's element space onto the fused node so the
        // executor sizes its output exactly like the member output it
        // replaced. A `Scaled` space becomes a def-level `output_canvas_scales`
        // entry (honoured by `node.wgsl_compute`); `Canvas` needs no stamp;
        // `Concrete` has no def-level override and relies on the fused node's
        // input propagation — all three are verified by `fused_def_builds`
        // against `expected_spaces`, so a resolution that drifts rejects the
        // fusion instead of shipping a wrong-grid kernel. In-place regions
        // (their output rides an aliased input port) are skipped: buffer loops
        // have no texture grid, and the verify below would mis-read the port.
        let mut output_canvas_scales: std::collections::BTreeMap<String, [u32; 2]> =
            Default::default();
        if let Some(space) = region.space
            && in_place_alias.is_none()
        {
            let multi = region.outputs.len() > 1;
            for (k, _) in region.outputs.iter().enumerate() {
                let port = if multi { format!("dst_{k}") } else { "dst".to_string() };
                if let ElementSpace::Scaled(num, den) = space {
                    output_canvas_scales.insert(port.clone(), [num, den]);
                }
                expected_spaces.push((fused_doc, port, space));
            }
        }

        // STATIC-PARAM SPECIALIZATION (roadmap 4). A texture fused kernel carries
        // every param as a uniform field, so the compiled kernel keeps a runtime
        // branch for every mode/quality/count even though most params never move
        // during a show. Tag each param field that has NO control wire driving it
        // (graph-internal LFOs + frame-derived uniforms are the only in-graph
        // dynamic sources, and both land in `control_wires`) with a
        // `// @static_param:` marker. `node.wgsl_compute` reads the markers and, at
        // dispatch, bakes those fields' LIVE values into a module-scope `const`
        // variant so spirv-opt's CCP + DCE strip the dead branches — value-keyed,
        // with the generic kernel as the permanent fallback (correctness is the
        // runtime value-key compare, NOT this classification, so a binding written
        // after the build, or a knob tweak, is always served correctly). Buffer /
        // particle kernels are out of v1 scope (their uniform also carries derived
        // counts) — detect them by the `var<storage` they always declare and the
        // pure-texture kernels never do, and emit no markers there.
        let fused_wgsl = if generated.wgsl.contains("var<storage") {
            generated.wgsl
        } else {
            let controlled: std::collections::HashSet<&str> = control_wires
                .iter()
                .filter(|(doc, ..)| *doc == fused_doc)
                .map(|(.., field)| field.as_str())
                .collect();
            let mut markers = String::new();
            for field in fused_params.keys() {
                // Skip in-graph control-wired fields AND outer-card binding
                // targets: both are dynamic, so baking either thrashes the
                // pipeline cache on every value change (the slider-drag stutter).
                if !controlled.contains(field.as_str()) && !bound_fields.contains(field.as_str()) {
                    markers.push_str(&Marker::StaticParam { field: field.clone() }.emit());
                    markers.push('\n');
                }
            }
            // P7/D8 precision propagation: the fused node replaces its members
            // as the CONSUMER of every external texture, so it must report the
            // access and precision-criticality those members declared — else
            // the executor's fp32 promotion (D6(a), `wants_fp32_intermediate`)
            // sees a default filtering consumer and vetoes the upstream fp32
            // the unfused chain would have allocated (the fp16-height relight
            // divergence). Per slot: filtering wins across members (fp32 is
            // non-filterable, so one true sampler read makes the veto correct);
            // precision_critical is emitted only for texel-exact reads.
            let mut slot_access: Vec<Option<crate::node_graph::freeze::classify::InputAccess>> =
                vec![None; region.externals.len()];
            let mut slot_pc: Vec<bool> = vec![false; region.externals.len()];
            for (m_idx, member) in all_members.iter().enumerate() {
                let node = &node_keepalive[m_idx];
                let tex_ports: Vec<&str> = node
                    .inputs()
                    .iter()
                    .filter(|i| {
                        matches!(
                            i.ty,
                            PortType::Texture2D | PortType::Texture2DTyped(_) | PortType::Texture3D
                        )
                    })
                    .map(|i| i.name.as_ref())
                    .collect();
                for (idx, ri) in member.inputs.iter().enumerate() {
                    let RegionInput::External(e) = ri else { continue };
                    let Some(port_name) = tex_ports.get(idx).copied() else { continue };
                    let access = member.input_access.get(idx).copied().unwrap_or_default();
                    slot_access[*e] = Some(match slot_access[*e] {
                        Some(prev) if prev.is_filtering_sampler() => prev,
                        _ => access,
                    });
                    if access.is_texel_exact()
                        && node.precision_critical_inputs().contains(&port_name)
                    {
                        slot_pc[*e] = true;
                    }
                }
            }
            for (e, access) in slot_access.iter().enumerate() {
                let Some(access) = access else { continue };
                use crate::node_graph::freeze::classify::InputAccess as IA;
                let token = match access {
                    IA::Coincident => "coincident",
                    IA::CoincidentTexel => "coincident_texel",
                    IA::Gather => "gather",
                    IA::GatherTexel => "gather_texel",
                    _ => continue, // buffer-domain accesses never name a src texture
                };
                markers.push_str(
                    &Marker::InputAccess { port: format!("src_{e}"), token: token.to_string() }
                        .emit(),
                );
                markers.push('\n');
                if slot_pc[e] {
                    markers.push_str(
                        &Marker::PrecisionCritical { port: format!("src_{e}") }.emit(),
                    );
                    markers.push('\n');
                }
            }
            if markers.is_empty() {
                generated.wgsl
            } else {
                format!("{markers}{}", generated.wgsl)
            }
        };

        new_nodes.push(EffectGraphNode {
            id: fused_doc,
            node_id: fused_id,
            // The dynamic-WGSL escape-hatch primitive — same stable type id the
            // preset JSON uses; it derives its ports/params from the source.
            type_id: "node.wgsl_compute".to_string(),
            handle: Some(format!("fused_region_{i}")),
            params: fused_params,
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: Some(fused_wgsl),
            title: Some(format!("Fused Region {i}")),
            output_formats: Default::default(),
            output_canvas_scales,
            group: None,
        });
        fused_docs.push(fused_doc);
        region_in_place.push(in_place_alias);
    }

    // ── Surviving (non-member) nodes carry over unchanged. ──
    for n in &def.nodes {
        if !member_region.contains_key(&n.id) {
            new_nodes.push(n.clone());
        }
    }

    // ── Rewire. Two distinct regions can only be directly texture-wired through a
    // GATHER (a coincident eligible→eligible wire would have merged them into one
    // region) — region A's output member is region B's gathered external. So an
    // external producer may itself be a fused-away member; `resolve_producer`
    // repoints it onto its region's fused `dst_<k>`. Output consumers are always
    // surviving nodes (a member consumer is the OTHER region's external, handled
    // from that side), so the output rewrite stays local. ──
    let mut new_wires: Vec<EffectGraphWire> = Vec::new();
    // Where a texture producer lands in the rewritten def: itself if it survived,
    // else its region's fused node at the dst slot for its output index. A
    // fused-away producer is always one of its region's outputs (it escaped the
    // region to be read here), so the slot lookup resolves — `?` bails to unfused
    // if that invariant is ever violated.
    let resolve_producer = |from_node: u32, from_port: &str| -> Option<(u32, String)> {
        match member_region.get(&from_node) {
            None => Some((from_node, from_port.to_string())),
            Some(&r) => {
                let producer = &regions[r];
                let k = producer
                    .outputs
                    .iter()
                    .position(|(o, p)| *o == from_node && p == from_port)?;
                // Match the output port the codegen emitted: an in-place region's
                // output is its aliased `src_<k>` buffer; otherwise `dst` (single)
                // or `dst_<k>` (fan-out).
                let port = match region_in_place[r] {
                    Some(src_k) => format!("src_{src_k}"),
                    None if producer.outputs.len() > 1 => format!("dst_{k}"),
                    None => "dst".to_string(),
                };
                Some((fused_docs[r], port))
            }
        }
    };
    // (a) surviving → surviving wires pass through.
    for w in &def.wires {
        if !member_region.contains_key(&w.from_node) && !member_region.contains_key(&w.to_node) {
            new_wires.push(w.clone());
        }
    }
    for (i, region) in regions.iter().enumerate() {
        let fused_doc = fused_docs[i];
        // (b) each external producer → the fused node's `src_<slot>` (read once,
        // even if several members read the same external — the finder deduped). A
        // producer that was itself fused away (cross-region gather) is repointed
        // onto its region's fused dst.
        for (e, ext) in region.externals.iter().enumerate() {
            let (from_node, from_port) = resolve_producer(ext.from_node, &ext.from_port)?;
            new_wires.push(EffectGraphWire {
                from_node,
                from_port,
                to_node: fused_doc,
                to_port: format!("src_{e}"),
            });
        }
        // (c) each region output → every consumer it fed. Output port depends on
        // the region's output model:
        //   - IN-PLACE (region_in_place[i] = Some(k)): the output IS the aliased
        //     loop buffer `src_k` (read_write, no separate dst), so consumers route
        //     off `src_k`. Only single-output regions ever get here.
        //   - FRESH single-output: the `dst` port (byte-identical to v1).
        //   - FRESH fan-out: each escaping member through its own `dst_<k>` (k = its
        //     index in `region.outputs`, matching the codegen's binding order).
        // The finder guaranteed every consumer is a live surviving node, so each
        // output lands on a resource the executor allocates.
        let multi = region.outputs.len() > 1;
        for (k, (out_doc, out_port)) in region.outputs.iter().enumerate() {
            let from_port = match region_in_place[i] {
                Some(src_k) => format!("src_{src_k}"),
                None if multi => format!("dst_{k}"),
                None => "dst".to_string(),
            };
            // D4/P6: match the ORIGINAL escaping port too — a multi-output
            // member (voronoi_2d) can appear here TWICE, once per distinct
            // port, and each entry must only claim the wires that actually
            // left THAT port (not every wire off the node, which would
            // duplicate onto both `dst_<k>` slots).
            for w in &def.wires {
                if w.from_node == *out_doc
                    && w.from_port == *out_port
                    && !member_region.contains_key(&w.to_node)
                {
                    new_wires.push(EffectGraphWire {
                        from_node: fused_doc,
                        from_port: from_port.clone(),
                        to_node: w.to_node,
                        to_port: w.to_port.clone(),
                    });
                }
            }
        }
    }
    // (d) control wires: the surviving producer drives the fused node's port-shadow
    // `n{idx}_<param>`, so a graph-wired param (LFO → gain.gain) keeps modulating
    // after the atom folds into the kernel. WgslCompute shadows every uniform field
    // as an optional ScalarF32 input, and reads the wire when present (else the
    // seeded fallback), so this is a plain control wire onto the fused node.
    for (fused_doc, from_node, from_port, field) in control_wires {
        new_wires.push(EffectGraphWire {
            from_node,
            from_port,
            to_node: fused_doc,
            to_port: field,
        });
    }

    let fused_def = EffectGraphDef {
        version: EFFECT_GRAPH_VERSION_WITH_METADATA,
        name: def.name.clone(),
        description: def.description.clone(),
        // Keep the outer-card surface (params / skip / aliases) byte-identical so
        // the chain builder's outer_param_index + skip logic are unchanged.
        preset_metadata: def.preset_metadata.clone(),
        nodes: new_nodes,
        wires: new_wires,
    };

    Some(FusedDef { def: fused_def, retarget, expected_spaces })
}

/// Defense in depth: a fused def must BUILD, not just contain valid WGSL. The
/// per-region naga-parse in [`fuse_canonical_def`] catches malformed shader text,
/// but a fused node can still be a well-formed shader the GRAPH compiler rejects
/// — e.g. a buffer region whose `var<storage, read_write>` output introspects as
/// a required-but-unwired aliased input port. The real entry points
/// ([`fuse_view_parts`] / [`fuse_generator_def`]) run this on their final def and fall
/// back to unfused on any failure, so a def that can't build never installs.
/// (Not called from [`fuse_canonical_def`] itself — the install unit tests drive
/// it with synthetic fixtures that intentionally don't fully compile.) Runs once
/// at fuse-build (cached), so the cost is negligible.
fn fused_def_builds(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    expected_spaces: &[(u32, String, ElementSpace)],
) -> bool {
    // `resolve_output_spaces` builds the graph AND compiles the plan, so a
    // `Some` already proves the def builds (the old check). On top of that,
    // tier 6: every fused texture output must resolve to the SAME element
    // space the member output it replaced had in the unfused plan — a drift
    // (the mixed-input canvas fallback, a stamp the loader didn't honour)
    // means the fused kernel would iterate a different grid than the atoms
    // did, the ParticleText fp32 divergence class. Reject → render unfused.
    let Some(spaces) = resolve_output_spaces(def, registry) else {
        return false;
    };
    expected_spaces
        .iter()
        .all(|(doc, port, want)| space_of(Some(&spaces), *doc, port) == *want)
}

/// A node's stable id defaults to its handle when the document carries none —
/// the same convention `instantiate_def` / the preset stamp use.
fn resolve_node_id(n: &EffectGraphNode) -> NodeId {
    if n.node_id.is_empty() {
        n.handle.as_deref().map(NodeId::new).unwrap_or_default()
    } else {
        n.node_id.clone()
    }
}

/// Effective scalar value for a region param: the def override if present, else
/// the atom's declared default. Every fused uniform field is f32 / i32 / u32
/// (the codegen maps Bool/Enum → u32 too), so all seed as a single f32 the
/// `WgslCompute` casts at the uniform-write boundary. `None` for a non-scalar
/// value (which the finder already rejected upstream — defensive).
fn effective_param_f32(
    override_val: Option<&SerializedParamValue>,
    default: &ParamValue,
) -> Option<f32> {
    if let Some(v) = override_val {
        return serialized_to_f32(v);
    }
    param_value_to_f32(default)
}

fn serialized_to_f32(v: &SerializedParamValue) -> Option<f32> {
    match v {
        SerializedParamValue::Float { value } => Some(*value),
        SerializedParamValue::Int { value } => Some(*value as f32),
        SerializedParamValue::Enum { value } => Some(*value as f32),
        SerializedParamValue::Bool { value } => Some(if *value { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn param_value_to_f32(v: &ParamValue) -> Option<f32> {
    match v {
        ParamValue::Float(f) => Some(*f),
        ParamValue::Enum(u) => Some(*u as f32),
        ParamValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Vec3 analogue of [`effective_param_f32`] — P5/D4 lift. Component order
/// matches the codegen's `_x`/`_y`/`_z` field emission.
fn effective_param_vec3(
    override_val: Option<&SerializedParamValue>,
    default: &ParamValue,
) -> Option<[f32; 3]> {
    if let Some(SerializedParamValue::Vec3 { value }) = override_val {
        return Some(*value);
    }
    match default {
        ParamValue::Vec3(v) => Some(*v),
        _ => None,
    }
}

/// Vec4/Color analogue of [`effective_param_f32`] — P5/D4 lift. Color and
/// Vec4 share the same `[f32; 4]` shape and the same `_x`/`_y`/`_z`/`_w`
/// codegen field emission, so one helper covers both.
fn effective_param_vec4(
    override_val: Option<&SerializedParamValue>,
    default: &ParamValue,
) -> Option<[f32; 4]> {
    match override_val {
        Some(SerializedParamValue::Vec4 { value }) => return Some(*value),
        Some(SerializedParamValue::Color { value }) => return Some(*value),
        _ => {}
    }
    match default {
        ParamValue::Vec4(v) => Some(*v),
        ParamValue::Color(v) => Some(*v),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ParamTarget;
    use manifold_core::effect_graph_def::EffectGraphDef;

    fn registry() -> PrimitiveRegistry {
        PrimitiveRegistry::with_builtin()
    }

    /// A region mask leaves the disabled region's nodes as ordinary surviving
    /// nodes: source → gain → contrast → threshold(boundary) → saturation →
    /// clamp → final partitions into two regions; masking the second fuses only
    /// {gain, contrast}, and saturation/clamp survive verbatim. All-false ⇒
    /// `None` (fully unfused).
    #[test]
    fn region_mask_fuses_only_enabled_regions() {
        let json = r#"{
            "version": 1, "name": "split", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 2, "typeId": "node.contrast", "nodeId": "contrast" },
                { "id": 3, "typeId": "node.threshold", "nodeId": "thresh" },
                { "id": 4, "typeId": "node.saturation", "nodeId": "sat" },
                { "id": 5, "typeId": "node.clamp", "nodeId": "clamp" },
                { "id": 6, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" },
                { "fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "in" },
                { "fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let reg = registry();
        assert_eq!(canonical_region_count(&def, &reg), 2);

        let full = fuse_canonical_def(&def, &reg).expect("both regions fuse unmasked");
        assert_eq!(
            full.def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute").count(),
            2
        );

        let half = fuse_canonical_def_masked(&def, &reg, Some(&[true, false]), &def_binding_targets(&def))
            .expect("one enabled region still fuses");
        assert_eq!(
            half.def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute").count(),
            1,
            "only the enabled region becomes a fused node"
        );
        for survivor in ["node.saturation", "node.clamp"] {
            assert!(
                half.def.nodes.iter().any(|n| n.type_id == survivor),
                "{survivor} must survive the masked fuse"
            );
        }
        assert!(
            !half.def.nodes.iter().any(|n| n.type_id == "node.exposure"),
            "the enabled region's members are fused away"
        );

        assert!(
            fuse_canonical_def_masked(&def, &reg, Some(&[false, false]), &def_binding_targets(&def)).is_none(),
            "all regions masked off = fully unfused = None"
        );
    }

    /// FUSION_SOTA_DESIGN D5: at cap, `LruCache` evicts the single
    /// least-recently-*hit* entry rather than refusing the insert — the
    /// replacement for the old "stop inserting past `FUSED_CACHE_CAP`"
    /// behavior, now safe because values are `Arc`-owned (an eviction's
    /// `Arc` frees for real, unlike the old leaked `&'static` statics).
    #[test]
    fn lru_cache_evicts_least_recently_hit_at_cap() {
        let mut cache: LruCache<u32> = LruCache::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        assert_eq!(cache.len(), 2);
        // Touch key 1 so it's more recently hit than key 2.
        assert_eq!(cache.get(1), Some(10));
        // Cache is at cap; inserting a third, genuinely new key must evict
        // the least-recently-hit entry — key 2, never touched since insert.
        cache.insert(3, 30);
        assert_eq!(cache.len(), 2, "cap must not be exceeded");
        assert!(cache.contains_key(1), "recently-hit key survives eviction");
        assert!(!cache.contains_key(2), "least-recently-hit key is evicted");
        assert!(cache.contains_key(3), "the new insert is present");
    }

    /// A refresh of an already-present key must never evict — only a
    /// genuinely new key at cap triggers eviction. This is the LRU
    /// replacement for P2's at-cap refresh fix
    /// (`m.len() < CAP || m.contains_key(&key)`): the old insert-skip logic
    /// is gone, but a key's own refresh still always lands.
    #[test]
    fn lru_cache_refresh_of_existing_key_never_evicts() {
        let mut cache: LruCache<u32> = LruCache::new(2);
        cache.insert(1, 10);
        cache.insert(2, 20);
        // Refresh key 1 with a new value — key 1 was already present, so
        // this must not evict key 2 even though the cache is at cap.
        cache.insert(1, 11);
        assert_eq!(cache.len(), 2, "refreshing a present key must not grow past cap");
        assert!(cache.contains_key(2), "refresh must not evict the other entry");
        assert_eq!(cache.get(1), Some(11), "refresh must update the value");
    }

    #[test]
    fn shared_gate_keeps_watched_target_unfused_both_arms() {
        // The single home for the fuse-or-not decision. The watched
        // (open-in-editor) target must force unfused so per-node preview can
        // sample inner-node textures and edits render live; otherwise fusion is
        // on (freeze enabled this test binary) and *which* atoms fuse is left to
        // the region partition. (There is intentionally no `has_override` veto —
        // an edited graph fuses by content via `fused_view_for`; while open it's
        // the watched target.)
        // Not watched → attempt fusion.
        assert!(should_render_fused(false));
        // Watched → never fused.
        assert!(!should_render_fused(true));
    }

    #[test]
    fn content_keyed_cache_separates_edited_from_canonical_and_negative_caches() {
        // An edited shape (different topology) must get its own fused entry by its
        // own content key and never clobber the canonical one; a non-fusable def
        // must cache `None` rather than recompile each call. Uses ColorGrade,
        // whose canonical shape fuses.
        let base = crate::node_graph::loaded_preset_view_by_id(&PresetTypeId::new("ColorGrade"))
            .expect("ColorGrade canonical view");

        // Canonical content key fuses and is stable across calls (cache hit).
        let canon_a = fused_view_for(&base.canonical_def, base);
        let canon_b = fused_view_for(&base.canonical_def, base);
        assert!(canon_a.is_some(), "canonical ColorGrade must fuse");
        assert!(
            Arc::ptr_eq(canon_a.as_ref().unwrap(), canon_b.as_ref().unwrap()),
            "same def content must return the same cached Arc view",
        );

        // Mutating the def's content (duplicate a node → a structurally distinct
        // def) must route to a *different* cache entry, proving keying is by
        // content, not type id. We don't assert it fuses (the malformed dup may
        // strand → None); we assert the canonical entry is untouched afterward.
        let mut edited = (*base.canonical_def).clone();
        edited.nodes.push(edited.nodes[0].clone());
        assert_ne!(
            def_content_key(&base.canonical_def),
            def_content_key(&edited),
            "a structural edit must change the content key",
        );
        let _ = fused_view_for(&edited, base);
        let canon_c = fused_view_for(&base.canonical_def, base);
        assert!(
            Arc::ptr_eq(canon_a.as_ref().unwrap(), canon_c.as_ref().unwrap()),
            "an edited def's entry must not clobber the canonical entry",
        );
    }

    /// A minimal three-node def (source → gain → final_output) used by the
    /// content-key cosmetic-field tests below.
    fn minimal_def() -> EffectGraphDef {
        let json = r#"{
            "version": 1, "name": "minimal", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain", "editorPos": [10.0, 20.0], "title": "Gain" },
                { "id": 2, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn content_key_ignores_editor_pos_drag() {
        // FREEZE_COMPILER_MAP.md §11 honest-edge #5: a node drag (editor_pos
        // change only) must not perturb the content key, or a watched graph
        // re-fuses on every mouse-up.
        let def = minimal_def();
        let key_before = def_content_key(&def);

        let mut dragged = def.clone();
        for node in &mut dragged.nodes {
            node.editor_pos = Some((999.0, -123.0));
        }
        let key_after = def_content_key(&dragged);

        assert_eq!(
            key_before, key_after,
            "editor_pos-only change (node drag) must not change the content key",
        );
    }

    #[test]
    fn content_key_ignores_title_rename() {
        // Same cosmetic-field contract as editor_pos: renaming a node's
        // display title must not force a re-fuse.
        let def = minimal_def();
        let key_before = def_content_key(&def);

        let mut renamed = def.clone();
        renamed.nodes[1].title = Some("Renamed Gain".to_string());
        let key_after = def_content_key(&renamed);

        assert_eq!(
            key_before, key_after,
            "title-only change (node rename) must not change the content key",
        );
    }

    #[test]
    fn content_key_changes_on_wire_param_or_topology_edit() {
        // Any *structural* edit — a wire, a baked param value, or node
        // topology — must change the content key, and each edit must land on
        // a key distinct from the original AND from the other edits (not all
        // collapsing to one "something changed" bucket).
        let def = minimal_def();
        let key_original = def_content_key(&def);

        // (a) wire edit: retarget the gain node's wire source directly to
        // final_output, dropping the source → gain hop.
        let mut wire_edited = def.clone();
        wire_edited.wires[0].to_node = 2;
        let key_wire = def_content_key(&wire_edited);

        // (b) baked param edit: set a param value on the gain node.
        let mut param_edited = def.clone();
        param_edited.nodes[1].params.insert(
            "amount".to_string(),
            SerializedParamValue::Float { value: 2.5 },
        );
        let key_param = def_content_key(&param_edited);

        // (c) topology edit: drop the gain node and its wires entirely.
        let mut topology_edited = def.clone();
        topology_edited.nodes.retain(|n| n.id != 1);
        topology_edited.wires.clear();
        let key_topology = def_content_key(&topology_edited);

        assert_ne!(key_original, key_wire, "a wire edit must change the content key");
        assert_ne!(key_original, key_param, "a baked param edit must change the content key");
        assert_ne!(key_original, key_topology, "a topology edit must change the content key");
        assert_ne!(key_wire, key_param, "distinct edits must not collide on the same key");
        assert_ne!(key_wire, key_topology, "distinct edits must not collide on the same key");
        assert_ne!(key_param, key_topology, "distinct edits must not collide on the same key");
    }

    /// The fused view must carry the full binding-retarget map so the chain
    /// builder can repoint a per-instance USER binding (which lives off the def,
    /// on `PresetInstance.user_param_bindings`, and so is invisible to the
    /// content-keyed fuse) onto the fused node — exactly as the static card
    /// bindings are. Without this the map was discarded after retargeting the
    /// statics, and a user-exposed slider went inert the moment the effect
    /// re-fused on editor close (the effect/generator divergence: generators
    /// keep bindings in the def, so they retargeted; effects didn't).
    #[test]
    fn fused_view_carries_retarget_map_for_user_bindings() {
        let base = crate::node_graph::loaded_preset_view_by_id(&PresetTypeId::new("ColorGrade"))
            .expect("ColorGrade canonical view");
        // Plain JSON-loaded view: no fusion, so nothing to retarget.
        assert!(
            base.fused_retarget.is_empty(),
            "unfused view must carry an empty retarget map",
        );

        let fused = fused_view_for(&base.canonical_def, base).expect("ColorGrade fuses");
        // Same routing the standalone `fuse_canonical_def` retarget asserts —
        // proving the map survived onto the cached view rather than being
        // dropped after the static-binding rewrite.
        assert_eq!(
            fused
                .fused_retarget
                .get(&("gain".to_string(), "gain".to_string()))
                .map(|(id, f)| (id.as_str(), f.as_str())),
            Some(("fused_region_0", "n0_gain")),
            "an inner (node_id, param) the fuse collapsed must resolve to its \
             fused uniform field so a user binding can be repointed onto it",
        );
        // The map is total over fused-away inner params (every param of every
        // collapsed node), so a user binding can never strand under fusion.
        assert_eq!(fused.fused_retarget.len(), 14, "all 7 ColorGrade atoms' params");
    }

    fn colorgrade_def() -> EffectGraphDef {
        let json = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/effect-presets/ColorGrade.json"
        ))
        .expect("read ColorGrade.json");
        serde_json::from_str(&json).expect("parse ColorGrade.json")
    }

    /// The whole ColorGrade card (7 atoms, one region) collapses to ONE
    /// `node.wgsl_compute` node between the retained boundaries, wired
    /// source → fused.src_0 → final_output. The retarget maps each inner
    /// (node_id, param) to its region's fused node + `n{i}_{param}` field — the
    /// load-bearing routing for the binding rewrite.
    #[test]
    fn colorgrade_fuses_to_single_wgsl_node() {
        let def = colorgrade_def();
        let fused = fuse_canonical_def(&def, &registry()).expect("ColorGrade fuses");

        // 3 nodes: source, fused, final_output. 2 wires.
        assert_eq!(fused.def.nodes.len(), 3, "boundaries + one fused node");
        let wgsl_nodes: Vec<_> = fused
            .def
            .nodes
            .iter()
            .filter(|n| n.type_id == "node.wgsl_compute")
            .collect();
        assert_eq!(wgsl_nodes.len(), 1, "exactly one fused node");
        assert!(wgsl_nodes[0].wgsl_source.is_some(), "fused node carries WGSL");
        assert_eq!(fused.def.wires.len(), 2, "source→fused, fused→final_output");
        assert!(
            fused.def.wires.iter().any(|w| w.to_port == "src_0"),
            "an input wire targets the fused src_0 port"
        );
        assert!(
            fused.def.wires.iter().any(|w| w.from_port == "dst"),
            "the fused output wire leaves the dst port"
        );

        // Region topo order: gain(0) sat(1) hue(2) contrast(3) colorize(4)
        // mix(5) clamp(6). Spot-check the routing the binding rewrite depends on.
        let field_of = |nid: &str, p: &str| {
            fused
                .retarget
                .get(&(nid.into(), p.into()))
                .map(|(_, f)| f.clone())
        };
        assert_eq!(field_of("gain", "gain").as_deref(), Some("n0_gain"));
        assert_eq!(field_of("saturation", "saturation").as_deref(), Some("n1_saturation"));
        assert_eq!(field_of("hue", "hue").as_deref(), Some("n2_hue"));
        assert_eq!(field_of("contrast", "contrast").as_deref(), Some("n3_contrast"));
        assert_eq!(field_of("colorize", "focus").as_deref(), Some("n4_focus"));
        assert_eq!(field_of("grade_mix", "amount").as_deref(), Some("n5_amount"));
        assert_eq!(field_of("clamp", "max").as_deref(), Some("n6_max"));
        // 14 inner params across the 7 atoms (1+1+3+1+4+2+2).
        assert_eq!(fused.retarget.len(), 14);
        // All routed onto the single region's fused node.
        for (fused_id, _) in fused.retarget.values() {
            assert_eq!(fused_id.as_str(), "fused_region_0");
        }
    }

    /// A true boundary in the middle splits the card into TWO fused nodes — the
    /// headline generalisation past whole-card fusion. source → gain → contrast
    /// → threshold(boundary) → saturation → clamp → final rewrites to
    /// source → fused_region_0 → threshold → fused_region_1 → final_output. (A
    /// gather like gaussian_blur would instead fold IN — see the region tests.)
    #[test]
    fn boundary_splits_into_two_fused_nodes() {
        let json = r#"{
            "version": 1, "name": "split", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 2, "typeId": "node.contrast", "nodeId": "contrast" },
                { "id": 3, "typeId": "node.threshold", "nodeId": "thresh" },
                { "id": 4, "typeId": "node.saturation", "nodeId": "sat" },
                { "id": 5, "typeId": "node.clamp", "nodeId": "clamp" },
                { "id": 6, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" },
                { "fromNode": 4, "fromPort": "out", "toNode": 5, "toPort": "in" },
                { "fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let fused = fuse_canonical_def(&def, &registry()).expect("two regions fuse");

        // Nodes: source, fused_region_0, threshold, fused_region_1, final_output.
        let wgsl_nodes: Vec<_> = fused
            .def
            .nodes
            .iter()
            .filter(|n| n.type_id == "node.wgsl_compute")
            .collect();
        assert_eq!(wgsl_nodes.len(), 2, "two fused regions");
        assert!(
            fused.def.nodes.iter().any(|n| n.type_id == "node.threshold"),
            "the threshold boundary survives between the two fused nodes"
        );

        // Routing: gain/contrast → fused_region_0; sat/clamp → fused_region_1.
        let region_of = |nid: &str, p: &str| {
            fused
                .retarget
                .get(&(nid.into(), p.into()))
                .map(|(id, _)| id.as_str().to_string())
        };
        assert_eq!(region_of("gain", "gain").as_deref(), Some("fused_region_0"));
        assert_eq!(region_of("contrast", "contrast").as_deref(), Some("fused_region_0"));
        assert_eq!(region_of("sat", "saturation").as_deref(), Some("fused_region_1"));
        assert_eq!(region_of("clamp", "max").as_deref(), Some("fused_region_1"));

        // The chain reconnects: source → r0, r0 → threshold, threshold → r1 → final.
        let id_of =
            |nid: &str| fused.def.nodes.iter().find(|n| n.node_id.as_str() == nid).map(|n| n.id);
        let thresh = id_of("thresh").unwrap();
        let r0 = id_of("fused_region_0").unwrap();
        let r1 = id_of("fused_region_1").unwrap();
        let has_wire =
            |from: u32, to: u32| fused.def.wires.iter().any(|w| w.from_node == from && w.to_node == to);
        assert!(has_wire(r0, thresh), "fused_region_0 feeds the threshold");
        assert!(has_wire(thresh, r1), "the threshold feeds fused_region_1");
    }

    /// Every seeded field name + every retarget target exists as a real param on
    /// the `WgslCompute` node once it reparses the generated source. The drift
    /// guard: if the codegen's `n{i}_{param}` field-naming convention diverges
    /// from the install-side reconstruction, the seeded params would land on
    /// non-existent fields and silently no-op — this catches it without a GPU.
    #[test]
    fn seeded_fields_match_wgsl_compute_params() {
        use crate::node_graph::effect_node::EffectNode;
        use crate::node_graph::primitives::WgslCompute;
        let def = colorgrade_def();
        let fused = fuse_canonical_def(&def, &registry()).expect("ColorGrade fuses");
        let node = fused
            .def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.wgsl_compute")
            .unwrap();

        let mut wc = WgslCompute::new();
        wc.set_wgsl_source(node.wgsl_source.as_deref().unwrap());
        let param_names: AHashSet<&str> =
            wc.parameters().iter().map(|p| p.name.as_ref()).collect();

        for field in node.params.keys() {
            assert!(
                param_names.contains(field.as_str()),
                "seeded field `{field}` is not a derived WgslCompute param — codegen drift"
            );
        }
        for (_, field) in fused.retarget.values() {
            assert!(
                param_names.contains(field.as_str()),
                "retarget field `{field}` is not a derived WgslCompute param — codegen drift"
            );
        }
    }

    /// P5/D4: a Vec3 param (`node.brightness`'s `weights`) and a Vec4 param
    /// (`node.channel_mixer`'s `row0..row3`) both actually SEED correct
    /// per-component values into the fused node's `params` map — not just
    /// "the codegen text compiles" (the GPU parity tests already prove that
    /// downstream), but that install-time seeding (`effective_param_vec3`/
    /// `effective_param_vec4`) reconstructs the right `n{i}_<name>_x/_y/_z
    /// [_w]` fields from the atom's declared default, matching the exact
    /// component order `codegen.rs`'s struct/arg emission uses. Also reruns
    /// the `seeded_fields_match_wgsl_compute_params` drift guard on a region
    /// containing a non-scalar param, which the ColorGrade-only original
    /// never covered.
    #[test]
    fn vec3_and_vec4_params_seed_correct_component_values() {
        use crate::node_graph::effect_node::EffectNode;
        use crate::node_graph::primitives::WgslCompute;
        let json = r#"{
            "version": 1, "name": "vec-params", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.contrast", "nodeId": "contrast" },
                { "id": 2, "typeId": "node.brightness", "nodeId": "bright" },
                { "id": 3, "typeId": "node.channel_mixer", "nodeId": "mixer" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "source" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "source" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let fused = fuse_canonical_def(&def, &registry())
            .expect("contrast+brightness+channel_mixer all fuse (P5 lifts Vec3/Vec4)");
        let node = fused
            .def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.wgsl_compute")
            .expect("one fused region");

        // node.brightness is region index 1 (contrast=0, bright=1, mixer=2) —
        // the Vec3 `weights` default is BT.709 luma [0.2126, 0.7152, 0.0722].
        let get = |field: &str| match node.params.get(field) {
            Some(SerializedParamValue::Float { value }) => *value,
            other => panic!("expected a seeded Float at `{field}`, got {other:?}"),
        };
        assert_eq!(get("n1_weights_x"), 0.2126);
        assert_eq!(get("n1_weights_y"), 0.7152);
        assert_eq!(get("n1_weights_z"), 0.0722);

        // node.channel_mixer is region index 2 — row0's Vec4 default is the
        // identity matrix's first row [1.0, 0.0, 0.0, 0.0].
        assert_eq!(get("n2_row0_x"), 1.0);
        assert_eq!(get("n2_row0_y"), 0.0);
        assert_eq!(get("n2_row0_z"), 0.0);
        assert_eq!(get("n2_row0_w"), 0.0);
        // row1's default is [0.0, 1.0, 0.0, 0.0].
        assert_eq!(get("n2_row1_x"), 0.0);
        assert_eq!(get("n2_row1_y"), 1.0);

        // Drift guard (same pattern as `seeded_fields_match_wgsl_compute_
        // params`, on a region a Vec3/Vec4 param actually reaches): every
        // seeded field name must be a real reparsed WgslCompute param.
        let mut wc = WgslCompute::new();
        wc.set_wgsl_source(node.wgsl_source.as_deref().unwrap());
        let param_names: AHashSet<&str> =
            wc.parameters().iter().map(|p| p.name.as_ref()).collect();
        for field in node.params.keys() {
            assert!(
                param_names.contains(field.as_str()),
                "seeded field `{field}` is not a derived WgslCompute param — codegen drift"
            );
        }
    }

    /// The cached fused view retargets every outer-card binding onto its region's
    /// fused node, preserving the card surface: 9 bindings, all pointing at the
    /// fused node, at the matching `n{i}_{param}` field.
    #[test]
    fn fused_view_retargets_every_binding() {
        let view = fused_view_by_id(&PresetTypeId::new("ColorGrade"))
            .expect("ColorGrade has a fused view");
        assert_eq!(view.bindings.len(), 9, "all outer-card sliders survive");
        for b in &view.bindings {
            match &b.target {
                ParamTarget::Node { node_id, param } => {
                    assert_eq!(node_id.as_str(), "fused_region_0");
                    assert!(param.starts_with('n'), "retargeted to a fused field");
                }
                other => panic!("binding {:?} not retargeted to a node: {other:?}", b.id),
            }
        }
        // Spot-check two specific routings end-to-end through the cache.
        let field_for = |id: &str| {
            view.bindings
                .iter()
                .find(|b| AsRef::<str>::as_ref(&b.id) == id)
                .and_then(|b| match &b.target {
                    ParamTarget::Node { param, .. } => Some(param.clone()),
                    _ => None,
                })
        };
        assert_eq!(field_for("amount").as_deref(), Some("n5_amount"));
        assert_eq!(field_for("gain").as_deref(), Some("n0_gain"));
        assert_eq!(field_for("tint_focus").as_deref(), Some("n4_focus"));
    }

    /// An effect with no fusable node has no region — left entirely unfused, safe
    /// by construction. `node.threshold` is a Boundary, so a single-threshold
    /// card returns `None`.
    #[test]
    fn boundary_only_card_does_not_fuse() {
        let json = r#"{
            "version": 1,
            "name": "t",
            "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.threshold", "handle": "t", "nodeId": "t" },
                { "id": 2, "typeId": "system.final_output", "nodeId": "final_output" }
            ],
            "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        assert!(
            fuse_canonical_def(&def, &registry()).is_none(),
            "a card with no fusable region must not fuse"
        );
    }

    /// Fan-out — a region with two escaping members fuses to ONE node exposing
    /// two output ports (`dst_0`, `dst_1`), each wired to the boundary its member
    /// fed. gain forks into invert and contrast; each runs into its own
    /// multi_blend (a permanent by-design boundary — a self-synthesizing router
    /// that never gains a `wgsl_body`), which re-merge at a mix. The rewrite
    /// keeps both boundaries + the mix as surviving nodes and routes
    /// `dst_0 → thr_a`, `dst_1 → thr_b`.
    #[test]
    fn fanout_region_wires_two_dst_ports() {
        let json = r#"{
            "version": 1, "name": "fanout", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 2, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 3, "typeId": "node.contrast", "nodeId": "contrast" },
                { "id": 4, "typeId": "node.multi_blend", "nodeId": "thr_a" },
                { "id": 5, "typeId": "node.multi_blend", "nodeId": "thr_b" },
                { "id": 6, "typeId": "node.mix", "nodeId": "mix" },
                { "id": 7, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 4, "toPort": "in_0" },
                { "fromNode": 3, "fromPort": "out", "toNode": 5, "toPort": "in_0" },
                { "fromNode": 4, "fromPort": "out", "toNode": 6, "toPort": "a" },
                { "fromNode": 5, "fromPort": "out", "toNode": 6, "toPort": "b" },
                { "fromNode": 6, "fromPort": "out", "toNode": 7, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let fused = fuse_canonical_def(&def, &registry()).expect("the fan-out region fuses");

        // Exactly one fused node (gain+invert+contrast), both thresholds + the mix
        // survive.
        let wgsl_nodes: Vec<_> =
            fused.def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute").collect();
        assert_eq!(wgsl_nodes.len(), 1, "the fork is one fused node");
        let fused_doc = wgsl_nodes[0].id;
        assert_eq!(
            fused.def.nodes.iter().filter(|n| n.type_id == "node.multi_blend").count(),
            2,
            "both router boundaries survive"
        );

        // The fused node exposes two outputs, each routed to its member's boundary.
        let id_of =
            |nid: &str| fused.def.nodes.iter().find(|n| n.node_id.as_str() == nid).map(|n| n.id);
        let thr_a = id_of("thr_a").unwrap();
        let thr_b = id_of("thr_b").unwrap();
        let port_into = |to: u32| -> Option<String> {
            fused
                .def
                .wires
                .iter()
                .find(|w| w.from_node == fused_doc && w.to_node == to)
                .map(|w| w.from_port.clone())
        };
        // invert(2) < contrast(3) by doc-id, so invert → dst_0, contrast → dst_1.
        assert_eq!(port_into(thr_a).as_deref(), Some("dst_0"), "invert's output drives thr_a via dst_0");
        assert_eq!(port_into(thr_b).as_deref(), Some("dst_1"), "contrast's output drives thr_b via dst_1");

        // Retarget still routes both members' params onto the one fused node.
        let region_of = |nid: &str, p: &str| {
            fused.retarget.get(&(nid.into(), p.into())).map(|(id, _)| id.as_str().to_string())
        };
        assert_eq!(region_of("gain", "gain").as_deref(), Some("fused_region_0"));
        assert_eq!(region_of("invert", "intensity").as_deref(), Some("fused_region_0"));
        assert_eq!(region_of("contrast", "contrast").as_deref(), Some("fused_region_0"));
    }

    /// A control wire driving a fused-away atom's param is re-anchored onto the
    /// fused node's port-shadow `n{idx}_<param>`. texture_dimensions.aspect drives
    /// gain.gain; gain is member 0 of its region, so after fusion the wire runs
    /// texture_dimensions → fused.n0_gain — keeping the modulation live.
    #[test]
    fn control_wire_reanchors_onto_fused_shadow_port() {
        let json = r#"{
            "version": 1, "name": "ctrl", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.texture_size", "nodeId": "dims" },
                { "id": 2, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 3, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 1, "toPort": "in" },
                { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 1, "fromPort": "aspect", "toNode": 2, "toPort": "gain" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let fused = fuse_canonical_def(&def, &registry()).expect("the control-wired region fuses");
        let fused_doc =
            fused.def.nodes.iter().find(|n| n.type_id == "node.wgsl_compute").unwrap().id;
        let dims_doc =
            fused.def.nodes.iter().find(|n| n.type_id == "node.texture_size").unwrap().id;
        let cw = fused
            .def
            .wires
            .iter()
            .find(|w| w.from_node == dims_doc && w.to_node == fused_doc)
            .expect("texture_dimensions still drives the fused node");
        assert_eq!(cw.from_port, "aspect", "the producer's aspect output");
        assert_eq!(cw.to_port, "n0_gain", "re-anchored onto gain's shadow field (member 0)");
        // The fused WgslCompute must actually expose that shadow port.
        use crate::node_graph::effect_node::EffectNode;
        use crate::node_graph::primitives::WgslCompute;
        let src = fused
            .def
            .nodes
            .iter()
            .find(|n| n.id == fused_doc)
            .and_then(|n| n.wgsl_source.as_deref())
            .unwrap();
        let mut wc = WgslCompute::new();
        wc.set_wgsl_source(src);
        assert!(
            wc.inputs().iter().any(|i| i.name == "n0_gain"),
            "the fused node exposes n0_gain as a control input"
        );
    }

    /// A generator's `preset_metadata` binding is retargeted onto the fused node.
    /// checkerboard (Source) → gain → invert fuse into one region; the binding that
    /// drove `gain.gain` is repointed at the fused node's `n1_gain` field (gain is
    /// member 1), so the generator's modulation surface keeps driving the kernel.
    #[test]
    fn generator_binding_def_retargets_onto_fused() {
        use manifold_core::effect_graph_def::BindingTarget;
        let json = r#"{
            "version": 1, "name": "FuseGen",
            "presetMetadata": {
                "id": "FuseGen", "displayName": "Fuse Gen", "category": "Diagnostic",
                "oscPrefix": "fuse_gen",
                "params": [{ "id": "g", "name": "Gain", "min": 0.0, "max": 4.0, "defaultValue": 2.0 }],
                "bindings": [{ "id": "g", "label": "Gain", "defaultValue": 2.0,
                    "target": { "kind": "node", "nodeId": "gain", "param": "gain" } }]
            },
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "nodeId": "gen_in" },
                { "id": 1, "typeId": "node.checkerboard", "nodeId": "checker" },
                { "id": 2, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 3, "typeId": "node.invert", "nodeId": "invert" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let fused = fuse_generator_def(&def, &registry()).expect("the generator fuses");
        let meta = fused.preset_metadata.as_ref().expect("metadata preserved");
        assert_eq!(meta.bindings.len(), 1);
        match &meta.bindings[0].target {
            BindingTarget::Node { node_id, param } => {
                assert_eq!(node_id.as_str(), "fused_region_0", "binding re-anchored to the fused node");
                assert_eq!(param, "n1_gain", "gain is member 1, so its field is n1_gain");
            }
            other => panic!("binding not retargeted to a node: {other:?}"),
        }
    }

    /// A param an outer-card binding drives (the live performance surface — a
    /// slider/driver/Ableton/LFO writes it every frame) must NOT be tagged
    /// `// @static_param` in the fused kernel: baking it bakes the value into a
    /// `const` variant, so each dragged/modulated value recompiles the kernel
    /// (the slider-drag render stutter). An unbound, unwired constant param in
    /// the same region still bakes — that perf win is the whole point of the
    /// specialization, and a true constant never thrashes.
    #[test]
    fn bound_param_rides_uniform_unbound_constant_bakes() {
        let json = r#"{
            "version": 1, "name": "FuseGenStatic",
            "presetMetadata": {
                "id": "FuseGenStatic", "displayName": "Fuse Gen Static", "category": "Diagnostic",
                "oscPrefix": "fuse_gen_static",
                "params": [{ "id": "g", "name": "Gain", "min": 0.0, "max": 4.0, "defaultValue": 2.0 }],
                "bindings": [{ "id": "g", "label": "Gain", "defaultValue": 2.0,
                    "target": { "kind": "node", "nodeId": "gain_a", "param": "gain" } }]
            },
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "nodeId": "gen_in" },
                { "id": 1, "typeId": "node.checkerboard", "nodeId": "checker" },
                { "id": 2, "typeId": "node.exposure", "nodeId": "gain_a" },
                { "id": 3, "typeId": "node.exposure", "nodeId": "gain_b" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let fused = fuse_generator_def(&def, &registry()).expect("the generator fuses");
        let node = fused
            .nodes
            .iter()
            .find(|n| n.type_id == "node.wgsl_compute")
            .expect("a fused wgsl_compute node");
        let wgsl = node.wgsl_source.as_deref().expect("fused source");
        // gain_a is member 1 (field n1_gain) and is the binding target → uniform.
        assert!(
            !wgsl.contains(&Marker::StaticParam { field: "n1_gain".to_string() }.emit()),
            "a bound (live) param must never be baked static:\n{wgsl}"
        );
        // gain_b is member 2 (field n2_gain), unbound + unwired → still bakes.
        assert!(
            wgsl.contains(&Marker::StaticParam { field: "n2_gain".to_string() }.emit()),
            "an unbound constant param should still bake (perf win preserved):\n{wgsl}"
        );
    }

    /// The enum mirror of [`generator_binding_def_retargets_onto_fused`] +
    /// [`fused_view_retargets_every_binding`]: a binding with an `EnumRound`
    /// convert onto a member's Enum param (mix.mode — the FluidSim3D
    /// `container` shape) retargets onto the fused uniform field with its
    /// convert rewritten to `IntRound`, and the fused def passes the loader's
    /// convert check (`BindingConvertTypeMismatch` is exactly what stranded
    /// this before — the fused field introspects as Int, which rejects an
    /// Enum-producing convert). The atom fuses instead of being classify
    /// gated (59b3cf25 removed).
    #[test]
    fn enum_converted_binding_retargets_with_int_round_and_loads() {
        use manifold_core::effect_graph_def::BindingTarget;
        let json = r#"{
            "version": 1, "name": "EnumFuseGen",
            "presetMetadata": {
                "id": "EnumFuseGen", "displayName": "Enum Fuse Gen", "category": "Diagnostic",
                "oscPrefix": "enum_fuse_gen",
                "params": [{ "id": "m", "name": "Mode", "min": 0.0, "max": 4.0, "defaultValue": 3.0, "wholeNumbers": true }],
                "bindings": [{ "id": "m", "label": "Mode", "defaultValue": 3.0,
                    "target": { "kind": "node", "nodeId": "mix", "param": "mode" },
                    "convert": { "type": "EnumRound" } }]
            },
            "nodes": [
                { "id": 0, "typeId": "system.generator_input", "nodeId": "gen_in" },
                { "id": 1, "typeId": "node.checkerboard", "nodeId": "checker" },
                { "id": 2, "typeId": "node.exposure", "nodeId": "gain" },
                { "id": 3, "typeId": "node.mix", "nodeId": "mix" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "a" },
                { "fromNode": 1, "fromPort": "out", "toNode": 3, "toPort": "b" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).unwrap();
        let reg = registry();
        let fused = fuse_generator_def(&def, &reg)
            .expect("an enum-binding-targeted member must fuse, not classify gate");
        let meta = fused.preset_metadata.as_ref().expect("metadata preserved");
        assert_eq!(meta.bindings.len(), 1);
        match &meta.bindings[0].target {
            BindingTarget::Node { node_id, param } => {
                assert_eq!(node_id.as_str(), "fused_region_0");
                assert_eq!(param, "n2_mode", "mix is member 2, so its field is n2_mode");
            }
            other => panic!("binding not retargeted to a node: {other:?}"),
        }
        assert_eq!(
            meta.bindings[0].convert,
            manifold_core::effects::ParamConvert::IntRound,
            "EnumRound must rewrite to IntRound on retarget — the fused u32 \
             field consumes Float and casts at the uniform-write boundary"
        );
        // The fused def must clear the loader's binding-convert validation —
        // the check that originally rejected EnumRound against the fused Int
        // field. A load error here means the rewrite regressed.
        use crate::node_graph::persistence::EffectGraphDefExt;
        fused
            .clone()
            .into_graph(&reg)
            .expect("fused def with retargeted enum binding must load");
    }

    /// The effect-side (`ParamBinding`) twin of the rewrite: retargeting maps
    /// `EnumRound → IntRound`, while a binding left on a surviving boundary
    /// node keeps its `EnumRound` untouched (the real node still declares a
    /// real Enum param).
    #[test]
    fn retarget_bindings_rewrites_enum_round_only_when_repointed() {
        use crate::node_graph::param_binding::ParamId;
        let mk = |node: &str, param: &'static str| ParamBinding {
            id: ParamId::from("m"),
            label: "Mode",
            default_value: 0.0,
            target: ParamTarget::Node { node_id: NodeId::new(node), param: std::borrow::Cow::Borrowed(param) },
            convert: ParamConvert::EnumRound,
            scale: 1.0,
            offset: 0.0,
            min: 0.0,
            max: 3.0,
            curve: manifold_core::macro_bank::MacroCurve::Linear,
            invert: false,
        };
        let mut retarget: AHashMap<(String, String), (NodeId, String)> = AHashMap::default();
        retarget
            .insert(("mix".into(), "mode".into()), (NodeId::new("fused_region_0"), "n2_mode".into()));
        let surviving: AHashSet<String> = ["boundary".to_string()].into_iter().collect();

        let out = retarget_bindings(&[mk("mix", "mode"), mk("boundary", "mode")], &retarget, &surviving)
            .expect("both bindings route");
        assert_eq!(out[0].convert, ParamConvert::IntRound, "repointed → rewritten");
        match &out[0].target {
            ParamTarget::Node { node_id, param } => {
                assert_eq!(node_id.as_str(), "fused_region_0");
                assert_eq!(param.as_ref(), "n2_mode");
            }
            other => panic!("not retargeted: {other:?}"),
        }
        assert_eq!(
            out[1].convert,
            ParamConvert::EnumRound,
            "surviving-node binding keeps its enum convert — the real node still \
             declares a real Enum param"
        );
    }

    /// D2: a `Pending` segment key that outlives `SEGMENT_COMPILE_DEADLINE`
    /// expires into the negative cache (`Refused`) instead of waiting forever
    /// for a wedged worker. Injects `now` past the deadline rather than
    /// sleeping 60s for real.
    #[test]
    fn segment_pending_expires_to_refused() {
        // A key unlikely to collide with any other test's segment content key
        // sharing this thread-local (thread_local state persists across tests
        // run on the same libtest worker thread).
        const TEST_KEY: u64 = 0xF00D_BEEF_DEAD_0001;
        let enqueued_at = std::time::Instant::now() - std::time::Duration::from_secs(120);
        SEGMENT_PENDING.with(|p| {
            p.borrow_mut().insert(TEST_KEY, enqueued_at);
        });
        SEGMENT_CACHE.with(|c| {
            c.borrow_mut().remove(TEST_KEY);
        });

        expire_stale_segment_pending(std::time::Instant::now());

        SEGMENT_PENDING.with(|p| {
            assert!(
                !p.borrow().contains_key(&TEST_KEY),
                "expired key must leave the pending map"
            );
        });
        SEGMENT_CACHE.with(|c| {
            assert!(
                matches!(c.borrow_mut().get(TEST_KEY), Some(None)),
                "expired key must negative-cache as Refused"
            );
        });

        // Cleanup — don't leak test state into whatever test runs next on
        // this thread.
        SEGMENT_CACHE.with(|c| {
            c.borrow_mut().remove(TEST_KEY);
        });
    }

    /// D2: a panic mid-compile must refuse the in-flight key (return `None`,
    /// same as any other refusal) rather than propagate and kill the
    /// `chain-fusion-worker` thread. Exercises the panic-contained wrapper
    /// directly (tests never feed the real worker thread, per the segment
    /// cache's test convention) via the `#[cfg(test)]`-only injection hook.
    #[test]
    fn segment_worker_panic_refuses_key() {
        arm_segment_compile_panic_hook_for_test(true);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            compile_segment_view_panic_safe(&[], &registry())
        }));
        arm_segment_compile_panic_hook_for_test(false);

        let view = result.expect(
            "compile_segment_view_panic_safe must itself never unwind — it is the containment",
        );
        assert!(view.is_none(), "a panicking compile refuses (None), it doesn't propagate");
    }

    /// Recursively collect every `.rs` file under `dir` (std::fs only — no
    /// `walkdir`, no shelling out to `rg`/`find`; same convention as
    /// `freeze/markers.rs`'s `marker_literals_live_in_one_module`).
    fn rust_files_under(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_files_under(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// FUSION_SOTA_DESIGN P7 (D5) negative gate: the fused-cache leak model is
    /// gone. Every fused artifact (effect view / generator def / segment
    /// view) is `Arc`-owned now, and every leaked interior was replaced by
    /// owned data borrowed for exactly the fuse-build call that needs it —
    /// so the process-leak primitive this module used to lean on should not
    /// remain anywhere under `node_graph/freeze/`. The exact byte pattern
    /// this scans for is deliberately never written contiguously in this
    /// file's own source (built from separately-literal fragments below) so
    /// the gate can scan every file under `freeze/`, itself included, with
    /// no self-exclusion — the authoritative version of this check is
    /// `rg` over the same directory (see the phase brief), which this test
    /// mirrors in-process so it runs under `cargo test` too.
    #[test]
    fn freeze_has_no_leaks() {
        // CARGO_MANIFEST_DIR = <repo>/crates/manifold-renderer
        let freeze_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/node_graph/freeze");
        let mut files = Vec::new();
        rust_files_under(&freeze_dir, &mut files);
        assert!(!files.is_empty(), "freeze/ file walk found nothing — path broke");

        // Assembled from fragments, none of which is the leak-call name
        // itself, so this file's own source never contains the searched-for
        // byte sequence contiguously — the reason no file (including this
        // one) needs to be excluded from the walk below.
        let leak_type = ["B", "o", "x"].concat();
        let leak_fn = ["l", "e", "a"].concat() + "k";
        let needle = format!("{leak_type}::{leak_fn}");
        let mut violations = Vec::new();
        for path in &files {
            let Ok(text) = std::fs::read_to_string(path) else { continue };
            for (i, line) in text.lines().enumerate() {
                if line.contains(&needle) {
                    violations.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
                }
            }
        }
        assert!(
            violations.is_empty(),
            "a process-leak call was found under node_graph/freeze/ — the fused-cache \
             leak model (FUSION_SOTA_DESIGN D5) must stay fully Arc/owned, not leaked:\n{}",
            violations.join("\n")
        );
    }
}
