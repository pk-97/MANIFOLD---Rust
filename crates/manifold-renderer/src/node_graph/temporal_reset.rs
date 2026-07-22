//! Node-local temporal-history reset detection.
//!
//! RAYTRACING_DESIGN.md §5.2 P4 + D3; ruling RT-D2
//! (`.claude/orchestration/decisions.md`) — read before touching this file.
//!
//! D3: scene cuts (clip triggers) reset denoiser/upscaler history
//! explicitly; strobes (light-intensity flips) must NOT. RT-D2 answers
//! *how* this wave detects a cut: NODE-LOCAL state, no engine-side
//! `cut_generation` plumbing (that stays deferred — a named revival
//! trigger in RT-D2, not built here). A reset trips when EITHER:
//!
//!   (a) the node's stored [`OwnerKey`] differs from the current one — a
//!       different clip is now driving this node (owner_key is
//!       `hash(clip_id)` for a clip, per `effect_node.rs`'s doc comment),
//!       OR
//!   (b) frame-time discontinuity: `|actual_delta - expected_delta| > 1.5 *
//!       expected_delta`, in EITHER direction — a seek/scrub/pause within
//!       the SAME clip, which owner_key alone can't see.
//!
//! A strobe (same clip → same owner_key; steady cadence → no time jump)
//! trips NEITHER — history holds, exactly D3's requirement.
//!
//! **THE SHARED PATH.** P4's temporal-upscaler history reset
//! (`metalfx_temporal_upscaler.rs`) and P2's temporal accumulation reset
//! (soft-shadow/AO/GI, when P2 lands) both wire onto this SAME
//! [`TemporalResetDetector`] — do not build a second reset-detection path.
//! The negative-`rg` gate in both P4's and P2's briefs enforces exactly one.

use crate::node_graph::effect_node::FrameTime;
use crate::node_graph::state_store::OwnerKey;

/// Per-node persistent state backing reset detection. Plain data — the
/// owning node holds this as a struct field directly (the same shape
/// `RenderScene::prev_view_proj` already uses for its own per-frame
/// history), or a caller may key it into
/// [`crate::node_graph::state_store::StateStore`] under its own node's
/// `(NodeInstanceId, OwnerKey)` bucket. Either storage shape works —
/// `detect_reset` only needs `&mut self` access, once per node per frame.
#[derive(Debug, Clone, Copy, Default)]
pub struct TemporalResetDetector {
    last_owner_key: Option<OwnerKey>,
    last_seconds: Option<manifold_core::Seconds>,
}

impl TemporalResetDetector {
    pub const fn new() -> Self {
        Self {
            last_owner_key: None,
            last_seconds: None,
        }
    }

    /// Returns `true` if temporal history should be discarded this frame.
    ///
    /// A node's very first call (no prior state) always resets — the
    /// same "no history yet" case a freshly-created node starts in, and
    /// numerically identical to a post-cut frame (both are "cold start").
    ///
    /// Updates internal bookkeeping unconditionally on every call — call
    /// exactly once per node per frame, matching `evaluate()`'s
    /// once-per-frame contract. Calling it more than once per frame (or
    /// skipping a frame) desyncs the discontinuity check.
    pub fn detect_reset(&mut self, owner_key: OwnerKey, frame: &FrameTime) -> bool {
        let owner_changed = self.last_owner_key != Some(owner_key);
        let discontinuous = match self.last_seconds {
            Some(prev) => {
                let actual_delta = (frame.seconds.0 - prev.0).abs();
                let expected_delta = frame.delta.0.abs().max(1e-9);
                (actual_delta - expected_delta).abs() > 1.5 * expected_delta
            }
            None => false,
        };
        self.last_owner_key = Some(owner_key);
        self.last_seconds = Some(frame.seconds);
        owner_changed || discontinuous
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::{Beats, Seconds};

    fn frame_at(seconds: f64, delta: f64) -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(seconds),
            delta: Seconds(delta),
            frame_count: 0,
        }
    }

    #[test]
    fn first_frame_always_resets() {
        let mut d = TemporalResetDetector::new();
        assert!(d.detect_reset(1, &frame_at(0.0, 1.0 / 60.0)));
    }

    #[test]
    fn steady_cadence_same_owner_does_not_reset() {
        let mut d = TemporalResetDetector::new();
        assert!(d.detect_reset(1, &frame_at(0.0, 1.0 / 60.0)));
        for i in 1..30 {
            let t = i as f64 / 60.0;
            assert!(
                !d.detect_reset(1, &frame_at(t, 1.0 / 60.0)),
                "steady 60fps cadence, same owner_key, frame {i} must not reset"
            );
        }
    }

    #[test]
    fn owner_change_resets_even_at_steady_cadence() {
        let mut d = TemporalResetDetector::new();
        assert!(d.detect_reset(1, &frame_at(0.0, 1.0 / 60.0)));
        assert!(!d.detect_reset(1, &frame_at(1.0 / 60.0, 1.0 / 60.0)));
        // Cut: a different clip (owner_key 2) starts driving this node,
        // time keeps advancing normally.
        assert!(
            d.detect_reset(2, &frame_at(2.0 / 60.0, 1.0 / 60.0)),
            "owner_key change must reset even with a steady time delta"
        );
    }

    #[test]
    fn strobe_same_owner_steady_cadence_does_not_reset() {
        // A light-intensity flip changes what the scene LOOKS like but not
        // its owner_key or its per-frame timing — D3's explicit "strobes
        // are not cuts" case.
        let mut d = TemporalResetDetector::new();
        assert!(d.detect_reset(7, &frame_at(0.0, 1.0 / 60.0)));
        for i in 1..10 {
            let t = i as f64 / 60.0;
            assert!(!d.detect_reset(7, &frame_at(t, 1.0 / 60.0)));
        }
    }

    #[test]
    fn time_discontinuity_same_owner_resets() {
        // A seek/scrub within the SAME clip: owner_key is unchanged but
        // the wall-clock jump is far outside the expected per-frame delta.
        let mut d = TemporalResetDetector::new();
        assert!(d.detect_reset(3, &frame_at(0.0, 1.0 / 60.0)));
        assert!(!d.detect_reset(3, &frame_at(1.0 / 60.0, 1.0 / 60.0)));
        // Jump forward 5 seconds in one step — a scrub, not a normal tick.
        assert!(
            d.detect_reset(3, &frame_at(5.0, 1.0 / 60.0)),
            "a large same-owner time jump must reset"
        );
    }

    #[test]
    fn small_jitter_under_1_5x_does_not_reset() {
        // Frame-pacing jitter well under the 1.5x threshold must not
        // false-positive a reset every frame.
        let mut d = TemporalResetDetector::new();
        let dt = 1.0 / 60.0;
        assert!(d.detect_reset(9, &frame_at(0.0, dt)));
        // 1.4x the expected delta — under threshold, must not reset.
        assert!(!d.detect_reset(9, &frame_at(1.4 * dt, dt)));
    }

    #[test]
    fn backwards_time_jump_resets_too() {
        // Discontinuity is checked in EITHER direction (a rewind/loop
        // restart within the same clip is also not "steady cadence").
        let mut d = TemporalResetDetector::new();
        let dt = 1.0 / 60.0;
        assert!(d.detect_reset(4, &frame_at(1.0, dt)));
        assert!(
            d.detect_reset(4, &frame_at(0.0, dt)),
            "a backwards time jump must reset too"
        );
    }
}
