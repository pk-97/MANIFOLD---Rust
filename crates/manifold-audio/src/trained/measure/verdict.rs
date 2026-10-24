//! The kick detector's verdict at a snare candidate (kick2): the most confident kick decision whose onset sits
//! near the snare's and that is out by the snare's emission, and that kick's onset offset. Bright kicks then
//! stop passing as snares, while a snare on top of a kick still has its own upper-band evidence.
//! Reference: `features` in `tools/audio_analysis/lab/kick_verdict.py`.

use super::dsp::History;
use crate::trained::container::{Container, ContainerError};

/// Kick decisions kept (several seconds of dense kicks).
const VERDICTS: usize = 256;

pub(super) struct KickVerdicts {
    before_s: f64,
    after_s: f64,
    none_dt_ms: f64,
    /// Per kick decision: (onset s, emission s, final probability), in emission order.
    ring: History<[f64; 3]>,
}

impl KickVerdicts {
    pub fn new(c: &Container) -> Result<Self, ContainerError> {
        let w = c.f64s("kick2.window_s", &[2])?;
        if !(w[0] >= 0.0 && w[1] >= 0.0) {
            return Err(ContainerError::WrongType {
                name: "kick2.window_s".into(),
                want: "two non-negative spans",
            });
        }
        Ok(Self {
            before_s: w[0],
            after_s: w[1],
            none_dt_ms: super::scalar(c, "kick2.none_dt_ms")?,
            ring: History::new(VERDICTS),
        })
    }

    pub fn push(&mut self, onset_s: f64, emit_s: f64, p: f64) {
        self.ring.push([onset_s, emit_s, p]);
    }

    /// (p, kick onset - snare onset in ms) of the most confident kick with onset in
    /// `[on - before, on + after]` and emission at or before `em`; `(0, none_dt_ms)` when there is none.
    /// Ties keep the earliest (`np.argmax`).
    pub fn features(&self, on: f64, em: f64, out: &mut [f64]) {
        let (lo, hi) = (on - self.before_s, on + self.after_s);
        let first = self.ring.len().saturating_sub(VERDICTS as u64);
        // KickFed keeps the kick a step ahead, so no verdict in the window has been evicted.
        debug_assert!(
            self.ring.len() <= VERDICTS as u64 || self.ring.get(first)[0] < lo,
            "kick verdicts evicted before they were read"
        );
        let mut best: Option<[f64; 3]> = None;
        for i in first..self.ring.len() {
            let v = *self.ring.get(i);
            if v[0] >= lo && v[0] <= hi && v[1] <= em + 1e-9 && best.is_none_or(|b| v[2] > b[2]) {
                best = Some(v);
            }
        }
        match best {
            Some([k_on, _, p]) => out.copy_from_slice(&[p, 1e3 * (k_on - on)]),
            None => out.copy_from_slice(&[0.0, self.none_dt_ms]),
        }
    }
}
