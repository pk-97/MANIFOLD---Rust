//! The net: 64 causal band envelopes on the 2 ms grid, one input window per candidate (shape, new, then the
//! model's extra channels), and N nets on that one window (D17). Reference: `eval/kick_goal_nn.py`
//! (`spectrum`, `Song.slices`, `Net`, `predict`), run with `KICK_GOAL_NN_AHEAD_MS=40` and 64 bands.

mod cnn;
pub(crate) mod spectrum;

use super::container::{Container, ContainerError};
use super::frames::FrameSource;
use super::trees::{expit, logit};
use cnn::Cnn;
pub(crate) use spectrum::{BANDS, HISTORY, Spectrum};

/// Baseline frames per candidate are sorted on the stack.
const MAX_SPAN: usize = 128;

/// Push audio, then score each candidate once `ready_sample(emit_hop)` samples have arrived.
pub struct Net {
    spec: Spectrum,
    cnns: Vec<Cnn>,
    channels: usize,
    hop: i64,
    sample_rate: i64,
    frame_s: f64,
    pre_s: f64,
    ahead_s: f64,
    slice: usize,
    span: usize,
    window: Vec<f32>,
    input: Vec<f32>,
}

/// One candidate's frame anchors, as `Song.__init__` computes them for an unclipped candidate.
struct Anchors {
    end: u64,
    on: u64,
}

/// D9: each net's probability is the sigmoid of its f32 logit, in f32, widened.
pub fn probability(z: f32) -> f64 {
    (1.0 / (1.0 + (-z).exp())) as f64
}

/// D17: one net's probability as is; several nets as `expit(mean logit(clip(p_k)))`, summed in net order
/// like `np.mean`.
pub fn average(logits: &[f32], clip: f64) -> f64 {
    if let [z] = logits {
        return probability(*z);
    }
    let sum: f64 = logits.iter().map(|&z| logit(probability(z).clamp(clip, 1.0 - clip))).sum();
    expit(sum / logits.len() as f64)
}

impl Net {
    /// `count` nets, `net.0.*` .. `net.<count - 1>.*`, sharing the `net.*` input entries.
    pub fn new(c: &Container, count: usize) -> Result<Self, ContainerError> {
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
        let cnns = (0..count).map(|k| Cnn::new(c, &format!("net.{k}."), slice)).collect::<Result<Vec<_>, _>>()?;
        let channels = cnns.first().map_or(0, Cnn::channels);
        Ok(Self {
            spec: Spectrum::new(c)?,
            cnns,
            channels,
            hop: scalar_i("net.hop")?,
            sample_rate: scalar_i("net.sample_rate")?,
            frame_s: c.f64s("net.frame_s", &[])?[0],
            pre_s: c.f64s("net.pre_s", &[])?[0],
            ahead_s: c.f64s("net.ahead_s", &[])?[0],
            slice,
            span,
            window: vec![0.0; slice * BANDS],
            input: vec![0.0; cnn::input_len(channels, slice)],
        })
    }

    pub fn push(&mut self, samples: &[f64]) {
        self.spec.push(samples);
    }

    /// Nets averaged.
    pub fn count(&self) -> usize {
        self.cnns.len()
    }

    /// Input channels: shape, new, then `channels() - 2` extra channels.
    pub fn channels(&self) -> usize {
        self.channels
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

    /// The first absolute sample count at which `logits` is valid for a candidate emitted at
    /// `emit_hop`: one past the grid sample of the slice's last frame (emission + 40 ms). Extra
    /// channels hold that frame by then too.
    pub fn ready_sample(&self, emit_hop: u64) -> u64 {
        self.anchors(0, emit_hop).end * self.spec.frame_hop() + 1
    }

    #[cfg(test)]
    /// The net's own band envelopes.
    pub(crate) fn spectrum(&self) -> &Spectrum {
        &self.spec
    }

    /// Every net's logit into `out` (one per net). NaN when a spectrum or extra frame the slice reads is
    /// not held, or when `extra` does not supply exactly the model's extra channels.
    pub fn logits(&mut self, cand_hop: u64, emit_hop: u64, extra: &dyn FrameSource, out: &mut [f32]) {
        debug_assert_eq!(out.len(), self.cnns.len());
        if self.fill(cand_hop, emit_hop, extra) {
            for (o, cnn) in out.iter_mut().zip(&mut self.cnns) {
                *o = cnn.logit(&self.input);
            }
        } else {
            out.fill(f32::NAN);
        }
    }

    /// Builds the shared input window (`Song.slices`): shape = x - the slice's max; new = x - each band's
    /// pre-onset median; extras raw; all / 20. False when a frame it needs is not held.
    fn fill(&mut self, cand_hop: u64, emit_hop: u64, extra: &dyn FrameSource) -> bool {
        if extra.channels() + 2 != self.channels {
            return false;
        }
        let Anchors { end, on } = self.anchors(cand_hop, emit_hop);
        let first = end + 1 - self.slice as u64;
        let oldest = on.saturating_sub(self.span as u64 - 1).min(first);
        if [end, on, oldest].iter().any(|&f| self.spec.frame(f).is_none()) {
            return false;
        }
        if (0..extra.channels()).any(|ch| extra.frame(ch, first).is_none() || extra.frame(ch, end).is_none()) {
            return false;
        }
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
        let frames = self.slice;
        for (band, &b) in base.iter().enumerate() {
            for t in 0..frames {
                let x = self.window[t * BANDS + band];
                self.input[cnn::input_at(frames, 0, band, t)] = (x - peak) / 20.0;
                self.input[cnn::input_at(frames, 1, band, t)] = (x - b) / 20.0;
            }
        }
        for ch in 0..extra.channels() {
            for t in 0..frames {
                let Some(row) = extra.frame(ch, first + t as u64) else { return false };
                for (band, &v) in row[..BANDS].iter().enumerate() {
                    self.input[cnn::input_at(frames, ch + 2, band, t)] = v / 20.0;
                }
            }
        }
        true
    }
}

/// No extra channels.
#[cfg(test)]
pub struct NoFrames;

#[cfg(test)]
impl FrameSource for NoFrames {
    fn channels(&self) -> usize {
        0
    }

    fn frame(&self, _: usize, _: u64) -> Option<&[f32]> {
        None
    }
}

#[cfg(test)]
mod snare_tests;
#[cfg(test)]
mod tests;
