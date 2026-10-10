//! The kick CNN and its 64 causal band envelopes (reference: `tools/audio_analysis/eval/kick_goal_nn.py`,
//! run with `KICK_GOAL_NN_AHEAD_MS=40`, 64 bands, 150 ms, no extra channels).

mod cnn;
pub(crate) mod spectrum;

use crate::kick::container::{Container, ContainerError};
use cnn::Cnn;
pub(crate) use spectrum::{BANDS, HISTORY, Spectrum};

/// A per-frame source of one input channel on the net's 2 ms grid (BANDS values per frame), fed to the
/// net raw and divided by 20 like `Song.slices` extra channels. `None` when frame `f` is not held.
pub trait FrameSource {
    fn frame(&self, f: u64) -> Option<&[f32]>;
}

impl FrameSource for Spectrum {
    fn frame(&self, f: u64) -> Option<&[f32]> {
        Spectrum::frame(self, f)
    }
}

/// Baseline frames per candidate are sorted on the stack.
const MAX_SPAN: usize = 128;

/// Push audio, then score each candidate once `ready_sample(emit_hop)` samples have arrived.
pub struct KickNet {
    spec: Spectrum,
    cnn: Cnn,
    hop: i64,
    sample_rate: i64,
    frame_s: f64,
    pre_s: f64,
    ahead_s: f64,
    slice: usize,
    span: usize,
    window: Vec<f32>,
}

/// One candidate's frame anchors, as `Song.__init__` computes them for an unclipped candidate.
struct Anchors {
    end: u64,
    on: u64,
}

impl KickNet {
    pub fn new(c: &Container) -> Result<Self, ContainerError> {
        let scalar_i = |name: &str| -> Result<i64, ContainerError> {
            let v = c.i64s(name, &[])?[0];
            if v <= 0 {
                return Err(ContainerError::WrongType { name: name.into(), want: "positive i64" });
            }
            Ok(v)
        };
        let slice = scalar_i("net.slice")? as usize;
        let span = scalar_i("net.span")? as usize;
        if span > MAX_SPAN {
            return Err(ContainerError::WrongShape { name: "net.span".into(), want: vec![MAX_SPAN], got: vec![span] });
        }
        // The oldest frame a candidate reads must still be held, with room for emission lag.
        if slice + span > HISTORY / 2 {
            return Err(ContainerError::WrongShape { name: "net.slice".into(), want: vec![HISTORY / 2], got: vec![slice + span] });
        }
        Ok(Self {
            spec: Spectrum::new(c)?,
            cnn: Cnn::new(c, slice)?,
            hop: scalar_i("net.hop")?,
            sample_rate: scalar_i("net.sample_rate")?,
            frame_s: c.f64s("net.frame_s", &[])?[0],
            pre_s: c.f64s("net.pre_s", &[])?[0],
            ahead_s: c.f64s("net.ahead_s", &[])?[0],
            slice,
            span,
            window: vec![0.0; slice * BANDS],
        })
    }

    pub fn push(&mut self, samples: &[f64]) {
        self.spec.push(samples);
    }

    /// Seconds at the end of hop `h`, as the fusion features stamp onsets and emissions.
    fn hop_s(&self, h: u64) -> f64 {
        ((h as i64 + 1) * self.hop) as f64 / self.sample_rate as f64
    }

    /// `Song.__init__`: the slice ends at frame `int((emit_s + ahead) / FRAME_S)`; the onset frame is
    /// at least one past the pre-onset frame. Python clips both to the clip's last frames; a live
    /// stream has no last frame, and the slice end is only held up from the stream start.
    fn anchors(&self, cand_hop: u64, emit_hop: u64) -> Anchors {
        let onset_s = self.hop_s(cand_hop);
        let emit_s = self.hop_s(emit_hop) + self.ahead_s;
        let end = ((emit_s / self.frame_s) as i64).max(self.slice as i64 - 1) as u64;
        let pre = (((onset_s - self.pre_s) / self.frame_s) as i64).max(0);
        let on = ((onset_s / self.frame_s) as i64).max(pre + 1) as u64;
        Anchors { end, on }
    }

    /// The first absolute sample count at which `probability` is valid for a candidate emitted at
    /// `emit_hop`: one past the grid sample of the slice's last frame (emission + 40 ms).
    pub fn ready_sample(&self, emit_hop: u64) -> u64 {
        self.anchors(0, emit_hop).end * self.spec.frame_hop() + 1
    }

    /// The net's kick probability (`predict()`: sigmoid of the logit, f32, widened). NaN when the
    /// stream has not reached `ready_sample` or the candidate's baseline frames left the history.
    pub fn probability(&mut self, cand_hop: u64, emit_hop: u64) -> f64 {
        self.probability_with(cand_hop, emit_hop, &[])
    }

    /// `probability` for a net with extra input channels, one source per channel after shape and new.
    pub fn probability_with(&mut self, cand_hop: u64, emit_hop: u64, extra: &[&dyn FrameSource]) -> f64 {
        let z = self.logit_with(cand_hop, emit_hop, extra);
        (1.0 / (1.0 + (-z).exp())) as f64
    }

    #[cfg(test)]
    /// The net's own band envelopes.
    pub(crate) fn spectrum(&self) -> &Spectrum {
        &self.spec
    }

    /// Input channels the model expects: shape, new, then `channels() - 2` extra sources.
    pub fn channels(&self) -> usize {
        self.cnn.channels()
    }

    /// The net's logit. NaN when a spectrum or extra frame the slice reads is not held, or when
    /// `extra` does not supply exactly the model's extra channels.
    pub fn logit_with(&mut self, cand_hop: u64, emit_hop: u64, extra: &[&dyn FrameSource]) -> f32 {
        if extra.len() + 2 != self.cnn.channels() {
            return f32::NAN;
        }
        let Anchors { end, on } = self.anchors(cand_hop, emit_hop);
        let first = end + 1 - self.slice as u64;
        let oldest = on.saturating_sub(self.span as u64 - 1).min(first);
        if [end, on, oldest].iter().any(|&f| self.spec.frame(f).is_none()) {
            return f32::NAN;
        }
        if extra.iter().any(|e| e.frame(first).is_none() || e.frame(end).is_none()) {
            return f32::NAN;
        }
        // `slices()`: shape = x - the slice's max; new = x - each band's pre-onset median; extras raw; all / 20.
        let mut peak = f32::NEG_INFINITY;
        for t in 0..self.slice {
            let row = self.spec.frame(first + t as u64).expect("held: checked above");
            self.window[t * BANDS..][..BANDS].copy_from_slice(row);
            peak = row.iter().copied().fold(peak, f32::max);
        }
        let mut base = [0.0f32; BANDS];
        let mut scratch = [0.0f32; MAX_SPAN];
        let med = &mut scratch[..self.span];
        for (band, b) in base.iter_mut().enumerate() {
            for (j, m) in med.iter_mut().enumerate() {
                let f = on.saturating_sub(j as u64);
                *m = self.spec.frame(f).expect("held: checked above")[band];
            }
            // torch.median: the lower middle value for an even count.
            let mid = (self.span - 1) / 2;
            *b = *med.select_nth_unstable_by(mid, f32::total_cmp).1;
        }
        for (band, &b) in base.iter().enumerate() {
            for t in 0..self.slice {
                let x = self.window[t * BANDS + band];
                let i0 = self.cnn.input_at(0, band, t);
                let i1 = self.cnn.input_at(1, band, t);
                self.cnn.input[i0] = (x - peak) / 20.0;
                self.cnn.input[i1] = (x - b) / 20.0;
            }
        }
        for (ch, src) in extra.iter().enumerate() {
            for t in 0..self.slice {
                let Some(row) = src.frame(first + t as u64) else { return f32::NAN };
                for (band, &v) in row[..BANDS].iter().enumerate() {
                    let i = self.cnn.input_at(ch + 2, band, t);
                    self.cnn.input[i] = v / 20.0;
                }
            }
        }
        self.cnn.logit()
    }
}

#[cfg(test)]
mod tests;
