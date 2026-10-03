//! Shared frame duration for stateless particle atoms.
//!
//! Particle atoms receive playback's frame delta directly. Live playback uses
//! that observed span; export keeps the supplied duration so its f32
//! `delta * 60` arithmetic remains unchanged.

use crate::Seconds;
use crate::stepping::{LiveStepError, LiveStepOutcome, StepInterval};

/// Resolve one stateless particle update to a half-open simulation interval.
/// Invalid live deltas produce a no-update interval and an advisory diagnostic.
pub fn interval(delta: Seconds, _offline: bool) -> LiveStepOutcome<StepInterval> {
    if !delta.0.is_finite() {
        return LiveStepOutcome::diagnostic(
            StepInterval::new(Seconds::ZERO, Seconds::ZERO),
            LiveStepError::NonFinite("particle frame delta"),
        );
    }
    if delta.0 < 0.0 {
        return LiveStepOutcome::diagnostic(
            StepInterval::new(Seconds::ZERO, Seconds::ZERO),
            LiveStepError::InvalidInput("particle frame delta must not be negative"),
        );
    }
    LiveStepOutcome::ok(StepInterval::new(Seconds::ZERO, delta))
}

/// Return the legacy frame-normalized duration used by particle shaders.
/// Keeping the cast before multiplication preserves export's existing f32
/// rounding exactly.
pub fn scaled(delta: Seconds, offline: bool) -> LiveStepOutcome<f32> {
    let resolved = interval(delta, offline);
    LiveStepOutcome {
        value: resolved.value.duration().0 as f32 * 60.0,
        diagnostic: resolved.diagnostic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_particle_duration_covers_20_24_30_60_fps() {
        for fps in [20.0, 24.0, 30.0, 60.0] {
            let duration = scaled(Seconds(1.0 / fps), false);
            assert_eq!(duration.diagnostic, None);
            assert!((f64::from(duration.value) - 60.0 / fps).abs() < 1.0e-6);
        }
    }

    #[test]
    fn offline_particle_duration_preserves_nominal_f32_arithmetic() {
        for delta in [0.0, 1.0 / 60.0, 1.0 / 24.0, 0.037] {
            let expected = (delta as f32) * 60.0;
            assert_eq!(scaled(Seconds(delta), true).value.to_bits(), expected.to_bits());
        }
        let invalid = scaled(Seconds(f64::NAN), true);
        assert_eq!(invalid.value, 0.0);
        assert!(invalid.diagnostic.is_some());
    }

    #[test]
    fn invalid_live_particle_duration_is_a_no_update() {
        for delta in [Seconds(f64::NAN), Seconds(f64::INFINITY), Seconds(-0.1)] {
            let result = interval(delta, false);
            assert_eq!(result.value.duration(), Seconds::ZERO);
            assert!(result.diagnostic.is_some());
            assert_eq!(scaled(delta, false).value, 0.0);
        }
    }
}
