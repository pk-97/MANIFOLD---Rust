//! Load-time warmup state shared across the content thread and UI.
//!
//! Warmup runs inside the `LoadProject` content-command handler: every
//! generator layer is built and rendered offscreen until its async work
//! (GLB parses, RT accel builds, model loads) quiesces. Progress is published
//! on the existing `ContentState` snapshot so the UI can draw a load bar.

/// Progress of the load-time warmup pass, published per layer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WarmupProgress {
    pub done: u32,
    pub total: u32,
    pub label: String,
}

impl WarmupProgress {
    /// Fraction complete, clamped to 0.0..1.0.
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f32 / self.total as f32).clamp(0.0, 1.0)
        }
    }
}

/// Which cap ended warmup for a layer.
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum WarmupCap {
    /// Frame-count sanity bound tripped.
    PerLayerFrames,
    /// Per-layer wall-clock ceiling tripped.
    PerLayerWallClock,
    /// Whole-pass wall-clock ceiling tripped.
    TotalWallClock,
}

/// Per-layer and total budgets that bound warmup. Exhaustion logs and
/// continues — warmup never blocks a project from opening.
#[derive(Clone, Debug, Copy)]
pub struct WarmupBudget {
    /// Wall-clock ceiling for one layer before giving up.
    pub per_layer: std::time::Duration,
    /// Frame-count sanity bound for one layer (prevents a spin loop from
    /// outrunning the wall-clock cap indefinitely).
    pub per_layer_frames: u32,
    /// Wall-clock ceiling for the whole pass.
    pub total: std::time::Duration,
}

impl Default for WarmupBudget {
    fn default() -> Self {
        Self {
            per_layer: std::time::Duration::from_secs(10),
            per_layer_frames: 600,
            total: std::time::Duration::from_secs(60),
        }
    }
}

/// Result of warming one layer.
#[derive(Clone, Debug, Copy, PartialEq, Eq)]
pub enum WarmupOutcome {
    /// Async work quiesced within budget.
    Quiescent,
    /// A budget cap was exhausted; the `cap` and `elapsed` fields say which
    /// one and for how long the layer was pumped. The layer may first-touch
    /// once at play.
    BudgetExhausted {
        cap: WarmupCap,
        elapsed: std::time::Duration,
    },
}
