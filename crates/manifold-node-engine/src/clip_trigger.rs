//! Clip-trigger output uniqueness — base behaviour for any
//! generator or primitive that cycles through `N` outputs on each
//! clip retrigger (`trigger_count % N` style).
//!
//! ## Invariant
//!
//! For two adjacent clip-trigger events, the emitted index is
//! never equal. With a strictly-monotonic `trigger_count`, this is
//! already automatic — `(c+1) % N` is never `c % N` for `N > 1`.
//! [`ClipTriggerCycle`] adds **defense in depth**: when the math
//! would repeat (e.g. `trigger_count` wraps cleanly through a
//! multiple of `N`, or a host-side counter glitches and the same
//! count arrives twice), the cycle catches the candidate matching
//! its last emission and advances by `+1`, keeping the output
//! strictly non-repeating.
//!
//! ## Idempotence
//!
//! Generators dispatch every frame regardless of whether a new
//! trigger event fired. [`ClipTriggerCycle::step`] is idempotent
//! within a single trigger event — repeated calls at the same
//! `trigger_count` return the cached emission rather than re-rolling.
//! Only a *new* `trigger_count` value advances the state.
//!
//! ## Lifecycle
//!
//! Each generator instance owns one cycle. When the generator is
//! recreated (override graph edit, type swap), the cycle resets to
//! its default; the paired fix to preserve
//! `LayerGeneratorState::trigger_count` across recreate keeps the
//! transport-relative counter monotonic, so the cycle's first
//! emission after recreate lands at the same modulo it would have
//! on the previous instance — guaranteeing visual continuity.

/// Per-call-site state for the [clip-trigger uniqueness
/// invariant](self). One cycle per `trigger_count % N` call site.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClipTriggerCycle {
    last_trigger_count: Option<u32>,
    last_emitted: u32,
}

impl ClipTriggerCycle {
    /// Default cycle: no prior state. Use `new()` or
    /// `..Default::default()` in struct literals.
    pub const fn new() -> Self {
        Self {
            last_trigger_count: None,
            last_emitted: 0,
        }
    }

    /// Compute the index to use this frame for a `% modulus`
    /// clip-trigger cycle. Returns a value in `[0, modulus)`.
    ///
    /// - **First call** (no prior state): emits `trigger_count % modulus`.
    /// - **Same trigger event** (`trigger_count` unchanged): returns the
    ///   cached emission. Idempotent — per-frame callers don't re-roll.
    /// - **New trigger event** (`trigger_count` advanced): emits
    ///   `trigger_count % modulus`, unless that would equal the previous
    ///   emission **and** `modulus > 1`, in which case advances by `+1`
    ///   modulo `modulus` to preserve the non-repeating invariant.
    ///
    /// `modulus == 0` collapses to `1` (always emit 0).
    pub fn step(&mut self, trigger_count: u32, modulus: u32) -> u32 {
        let n = modulus.max(1);
        if Some(trigger_count) == self.last_trigger_count {
            return self.last_emitted;
        }
        let candidate = trigger_count % n;
        let result = if self.last_trigger_count.is_some()
            && candidate == self.last_emitted
            && n > 1
        {
            (candidate + 1) % n
        } else {
            candidate
        };
        self.last_trigger_count = Some(trigger_count);
        self.last_emitted = result;
        result
    }

    /// Discard cached state. Called nowhere by default — the cycle
    /// resets naturally when its owning generator is dropped.
    /// Provided for tests and manual lifecycle control.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Read the last emitted index without advancing. Used for
    /// diagnostics + tests; not on the hot path.
    pub fn last_emitted(&self) -> Option<u32> {
        self.last_trigger_count.map(|_| self.last_emitted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sequential `trigger_count` with naive `% N` already never
    /// repeats consecutively. The cycle should pass each value
    /// through unchanged in that case.
    #[test]
    fn sequential_trigger_counts_pass_through_unchanged() {
        let mut cycle = ClipTriggerCycle::new();
        for i in 0..20 {
            let emitted = cycle.step(i, 8);
            assert_eq!(emitted, i % 8, "sequential count {i} should pass through");
        }
    }

    /// Idempotent within a single trigger event: repeated calls at
    /// the same `trigger_count` return the cached value, even if
    /// modulus changes between calls.
    #[test]
    fn repeated_calls_at_same_count_return_cached_emission() {
        let mut cycle = ClipTriggerCycle::new();
        let first = cycle.step(5, 8);
        for _ in 0..10 {
            assert_eq!(cycle.step(5, 8), first);
        }
    }

    /// If a host-side counter glitches and `trigger_count` jumps
    /// such that `% N` would repeat the last emission, the cycle
    /// advances by +1 modulo N.
    #[test]
    fn would_be_repeat_advances_by_one() {
        let mut cycle = ClipTriggerCycle::new();
        // Emit 5 from count=5, mod 8
        assert_eq!(cycle.step(5, 8), 5);
        // Now count jumps to 13 — 13 % 8 = 5, would repeat.
        // Cycle should advance to 6.
        assert_eq!(cycle.step(13, 8), 6);
        // Confirm state updated
        assert_eq!(cycle.last_emitted(), Some(6));
    }

    /// Wrap-around: when trigger_count crosses a clean multiple of
    /// N, the modulo wraps but doesn't repeat the previous emission.
    /// E.g. count 7 → 7 % 8 = 7; count 8 → 8 % 8 = 0. No collision.
    #[test]
    fn clean_wrap_around_does_not_trigger_advance() {
        let mut cycle = ClipTriggerCycle::new();
        assert_eq!(cycle.step(7, 8), 7);
        assert_eq!(cycle.step(8, 8), 0);
    }

    /// Modulus 1 has nowhere to advance to — the cycle just emits
    /// 0 every time without trying to "guarantee uniqueness" (which
    /// is impossible at N=1).
    #[test]
    fn modulus_one_emits_zero_always() {
        let mut cycle = ClipTriggerCycle::new();
        for i in 0..5 {
            assert_eq!(cycle.step(i, 1), 0);
        }
    }

    /// Modulus 0 is invalid input; treat as 1 (always emit 0) to
    /// avoid div-by-zero panics on accidental misuse.
    #[test]
    fn modulus_zero_collapses_to_one() {
        let mut cycle = ClipTriggerCycle::new();
        assert_eq!(cycle.step(7, 0), 0);
    }

    /// Worst-case stress: trigger_count always lands on the same
    /// modulo. Cycle should produce a strict alternation between
    /// the natural value and value+1, ensuring no back-to-back
    /// duplicates across an arbitrarily long sequence.
    #[test]
    fn pathological_stuck_modulo_produces_alternating_outputs() {
        let mut cycle = ClipTriggerCycle::new();
        // trigger_counts that all yield 3 mod 8: 3, 11, 19, 27, …
        let counts = [3, 11, 19, 27, 35, 43, 51];
        let mut last = None;
        for (i, &c) in counts.iter().enumerate() {
            let emitted = cycle.step(c, 8);
            if let Some(p) = last {
                assert_ne!(emitted, p, "back-to-back repeat at step {i}");
            }
            last = Some(emitted);
        }
    }

    /// `reset()` discards cached state so the next call starts
    /// fresh as if from `new()`.
    #[test]
    fn reset_discards_state() {
        let mut cycle = ClipTriggerCycle::new();
        cycle.step(5, 8);
        assert_eq!(cycle.last_emitted(), Some(5));
        cycle.reset();
        assert_eq!(cycle.last_emitted(), None);
    }
}
