//! Cross-cutting cold-touch detector.
//!
//! A "cold touch" is a first-time initialization cost that the warmup pass is
//! supposed to front-load before the transport reaches the audience: pipeline
//! shader compiles, GLB parses, HDRI decodes, DNN model loads, and effect
//! chain construction. Each site calls [`record_cold_touch`]; the counter is
//! machine-readable so tests can assert zero during playback.
//!
//! This lives in `manifold-foundation` so `manifold-gpu` (pipeline compile)
//! and `manifold-core`/`manifold-app` (test assertions) can both reach it
//! without introducing a crate-cycle.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Initialization-cost category tracked by the cold-touch detector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColdTouchKind {
    /// GPU shader/pipeline compile (compute or render).
    PipelineCompile,
    /// GLB mesh scene parse on a worker thread.
    GlbParse,
    /// HDRI / EXR decode on a worker thread.
    HdriDecode,
    /// DNN model load (MiDaS, person segmentation, optical flow, …).
    ModelLoad,
    /// Effect-chain runtime construction.
    ChainConstruction,
}

impl ColdTouchKind {
    const fn index(self) -> usize {
        match self {
            ColdTouchKind::PipelineCompile => 0,
            ColdTouchKind::GlbParse => 1,
            ColdTouchKind::HdriDecode => 2,
            ColdTouchKind::ModelLoad => 3,
            ColdTouchKind::ChainConstruction => 4,
        }
    }

    fn label(self) -> &'static str {
        match self {
            ColdTouchKind::PipelineCompile => "pipeline compile",
            ColdTouchKind::GlbParse => "GLB parse",
            ColdTouchKind::HdriDecode => "HDRI decode",
            ColdTouchKind::ModelLoad => "DNN model load",
            ColdTouchKind::ChainConstruction => "effect-chain construction",
        }
    }
}

static COUNTERS: [AtomicU64; 5] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

/// Best-effort transport flag. Set by the content thread each tick; worker
/// threads read it when reporting cold touches so warnings only fire when the
/// audience could actually see the hitch.
static TRANSPORT_PLAYING: AtomicBool = AtomicBool::new(false);

/// Tell the detector whether the transport is currently playing. The content
/// thread should call this every tick before any work that might log a touch.
pub fn set_transport_playing(playing: bool) {
    TRANSPORT_PLAYING.store(playing, Ordering::Relaxed);
}

/// Record one cold touch of `kind`. Safe from any thread; increments the
/// counter and emits a loud warning if the transport is currently playing.
pub fn record_cold_touch(kind: ColdTouchKind) {
    COUNTERS[kind.index()].fetch_add(1, Ordering::Relaxed);
    if TRANSPORT_PLAYING.load(Ordering::Relaxed) {
        log::warn!(
            "[cold-touch] {} while transport playing — warmup missed this",
            kind.label()
        );
    }
}

/// Read the counter for one kind.
pub fn cold_touch_count(kind: ColdTouchKind) -> u64 {
    COUNTERS[kind.index()].load(Ordering::Relaxed)
}

/// Sum across all cold-touch kinds.
pub fn total_cold_touches() -> u64 {
    COUNTERS
        .iter()
        .map(|c| c.load(Ordering::Relaxed))
        .sum()
}

/// Reset every counter to zero. Tests call this after warmup and before the
/// playback sample window.
pub fn reset_cold_touch_counts() {
    for c in &COUNTERS {
        c.store(0, Ordering::Relaxed);
    }
}
