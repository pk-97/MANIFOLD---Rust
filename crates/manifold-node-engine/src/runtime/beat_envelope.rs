#[derive(Clone, Default)]
pub(crate) struct BeatEnvelopeState {
    last_count: Option<i32>,
    hit_beat: manifold_core::Beats,
    active: bool,
}

#[derive(Clone, Copy, Default)]
pub(crate) struct BeatEnvelopeDurations {
    pub(crate) window: f32,
    pub(crate) attack: f32,
    pub(crate) hold: f32,
    pub(crate) tail: f32,
}

impl BeatEnvelopeState {
    pub(crate) fn step(
        &mut self,
        trigger: f32,
        initial_count: Option<f32>,
        beat: manifold_core::Beats,
        durations: BeatEnvelopeDurations,
    ) -> Option<(f32, f32)> {
        if !trigger.is_finite() {
            return None;
        }
        let trigger = trigger.round() as i32;
        let initial_count = initial_count
            .filter(|value| value.is_finite())
            .map(|value| value.round() as i32);

        let event = match self.last_count {
            Some(last) => {
                let changed = trigger != last;
                self.last_count = Some(trigger);
                changed
            }
            None => {
                self.last_count = Some(trigger);
                initial_count.is_some_and(|baseline| baseline != trigger)
            }
        };
        if event {
            self.hit_beat = beat;
            self.active = true;
        }

        // Durations remain live for an active event. Invalid values settle it
        // immediately, matching the old window behavior for non-finite input.
        let durations_valid = durations.window.is_finite()
            && durations.attack.is_finite()
            && durations.hold.is_finite()
            && durations.tail.is_finite();
        let window = durations.window.max(0.0) as f64;
        let attack = durations.attack.max(0.0) as f64;
        let hold = durations.hold.max(0.0) as f64;
        let tail = durations.tail.max(0.0) as f64;
        let elapsed = (beat - self.hit_beat).0;
        if self.active && elapsed < 0.0 {
            // A backward seek during a live event must not resurrect it with
            // a negative phase. The next trigger edge can start a new event.
            self.active = false;
        }

        let output = if !self.active || !durations_valid {
            self.active = false;
            (0.0, -1.0)
        } else {
            let release_start = attack + hold;
            let tail_start = release_start + window;
            let complete_at = tail_start + tail;
            if elapsed >= complete_at {
                self.active = false;
                (0.0, -1.0)
            } else if elapsed < attack {
                let output = if attack <= 0.0 { 1.0 } else { elapsed / attack };
                (output.clamp(0.0, 1.0), elapsed)
            } else if elapsed < release_start {
                (1.0, elapsed)
            } else if elapsed < tail_start {
                let output = if window <= 0.0 {
                    0.0
                } else {
                    1.0 - ((elapsed - release_start) / window)
                };
                (output.clamp(0.0, 1.0), elapsed)
            } else {
                (0.0, elapsed)
            }
        };
        Some((output.0 as f32, output.1 as f32))
    }

    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }
}
