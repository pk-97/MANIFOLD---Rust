//! The song-history stage:
//! each candidate against the song's own recent confident hits over up to four memory windows, re-scored by
//! the stage forest, then the gate. References: `lab/stage.py` (`inputs`, `gated`) over
//! `eval/kick_goal_selfsim.py` (`self_features`, `descriptor`).

use super::container::{Container, ContainerError};
use super::spec::StageSpec;
use super::trees::{Forest, expit, logit};

/// Candidates per second the history holds without dropping any. Candidates are at most one per hop
/// (187.5 hops/s at 48 kHz, 256-sample hop); the margin covers candidates whose emission lags their onset.
pub const MAX_CANDIDATE_RATE: f64 = 192.0;

/// The causal decision stage. Call [`History::decide`] once per candidate in emission order.
pub struct History {
    spec: StageSpec,
    forest: Forest,
    /// `(logit(cut_detector), logit(cut_stage))` when the gate is on.
    cut_logits: Option<(f64, f64)>,
    width: usize,
    sample_rate: f64,
    hop: i64,
    // The ring, oldest first from `head`; descriptors are `width` values per slot.
    emit_s: Vec<f64>,
    w: Vec<f64>,
    lp: Vec<f64>,
    level: Vec<f64>,
    desc: Vec<f64>,
    head: usize,
    len: usize,
    dropped: u64,
    // Per-call scratch.
    d: Vec<f64>,
    tmpl: Vec<f64>,
    x: Vec<f64>,
}

impl History {
    /// The stage forest from `stage.*`; `hop` and `sample_rate` give emission times.
    pub fn new(
        c: &Container,
        spec: &StageSpec,
        hop: u64,
        sample_rate: u32,
    ) -> Result<Self, ContainerError> {
        let forest = Forest::from_container(c, "stage.")?;
        if forest.n_features() != spec.inputs() {
            return Err(ContainerError::WrongShape {
                name: "stage.n_features".into(),
                want: vec![spec.inputs()],
                got: vec![forest.n_features()],
            });
        }
        let max_w = spec.windows_s.iter().copied().fold(0.0, f64::max);
        let capacity = (max_w * MAX_CANDIDATE_RATE).ceil() as usize + 1;
        let width = spec.shape_width();
        Ok(Self {
            cut_logits: spec
                .cuts
                .filter(|_| spec.gate > 0.0)
                .map(|(d, s)| (logit(d), logit(s))),
            spec: spec.clone(),
            forest,
            width,
            sample_rate: sample_rate as f64,
            hop: hop as i64,
            emit_s: vec![0.0; capacity],
            w: vec![0.0; capacity],
            lp: vec![0.0; capacity],
            level: vec![0.0; capacity],
            desc: vec![0.0; capacity * width],
            head: 0,
            len: 0,
            dropped: 0,
            d: vec![0.0; width],
            tmpl: vec![0.0; width],
            x: vec![0.0; spec.inputs()],
        })
    }

    /// Candidates dropped from a full history. Zero unless candidates arrive faster than
    /// [`MAX_CANDIDATE_RATE`] over a whole window.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Stage inputs, the stage probability (ungated) and the final probability for one candidate.
    /// `shape_row` is the hstacked raw shape measures (clipped and normalised here), `level` the named level
    /// column (ignored without one), `emit_hop` the emission hop (`avail_hop + 1`). Calls come in
    /// non-decreasing emission order; the candidate then joins the history.
    pub fn decide(
        &mut self,
        p_combined: f64,
        shape_row: &[f64],
        level: f64,
        emit_hop: u64,
    ) -> (&[f64], f64, f64) {
        debug_assert_eq!(shape_row.len(), self.width);
        let st = &self.spec;
        let emit_s = (emit_hop as i64 * self.hop) as f64 / self.sample_rate;
        let lp = logit(p_combined.clamp(st.lp_clip, 1.0 - st.lp_clip));
        unit_descriptor(shape_row, &mut self.d);
        let has_level = st.level.is_some();
        let level = if has_level { level } else { 0.0 };

        let cap = self.emit_s.len();
        let max_w = st.windows_s.iter().copied().fold(0.0, f64::max);
        while self.len > 0 && self.emit_s[self.head] < emit_s - max_w {
            self.head = (self.head + 1) % cap;
            self.len -= 1;
        }
        self.x[0] = lp;
        let per = 3 + has_level as usize;
        for (wi, &window) in st.windows_s.iter().enumerate() {
            // self_features: every earlier candidate with emit >= now - window, summed oldest first over the
            // ring each call; no running sums, so no drift over a long set.
            let from = emit_s - window;
            let first = (0..self.len)
                .find(|&k| self.emit_s[(self.head + k) % cap] >= from)
                .unwrap_or(self.len);
            let (mut sw, mut sl, mut sv) = (0.0, 0.0, 0.0);
            self.tmpl.fill(0.0);
            for k in first..self.len {
                let i = (self.head + k) % cap;
                let w = self.w[i];
                sw += w;
                for (t, v) in self
                    .tmpl
                    .iter_mut()
                    .zip(&self.desc[i * self.width..][..self.width])
                {
                    *t += w * v;
                }
                sl += w * self.lp[i];
                sv += w * self.level[i];
            }
            let out = &mut self.x[1 + wi * per..][..per];
            out.fill(0.0);
            out[per - 1] = sw.ln_1p();
            if sw > 1e-9 {
                let safe = sw.max(1e-12);
                let dot: f64 = self.d.iter().zip(&self.tmpl).map(|(a, b)| a * b).sum();
                let norm = self.tmpl.iter().map(|t| t * t).sum::<f64>().sqrt();
                out[0] = dot / (norm + 1e-12);
                out[1] = lp - sl / safe;
                if has_level {
                    out[2] = level - sv / safe;
                }
            }
        }
        let p_stage = self.forest.predict_proba(&self.x);
        let p_final = match self.cut_logits {
            // lab/stage.py gated: the detector's distance from its cutoff plus history's change in that
            // distance, faded in by window 0's evidence over the gate.
            Some((cut_d, cut_s)) => {
                let g = (self.x[st.evidence0()].exp_m1() / st.gate).min(1.0);
                let det = self.x[0] - cut_d;
                let stg = logit(p_stage.clamp(st.q_clip, 1.0 - st.q_clip)) - cut_s;
                expit(cut_d + det + g * (stg - det))
            }
            None => p_stage,
        };

        if self.len == cap {
            self.head = (self.head + 1) % cap;
            self.len -= 1;
            self.dropped += 1;
        }
        let i = (self.head + self.len) % cap;
        self.emit_s[i] = emit_s;
        self.w[i] = p_combined.clamp(0.0, 1.0).powf(st.power);
        self.lp[i] = lp;
        self.level[i] = level;
        self.desc[i * self.width..][..self.width].copy_from_slice(&self.d);
        self.len += 1;
        (&self.x, p_stage, p_final)
    }
}

/// `kick_goal_selfsim.descriptor`: clipped at zero, then L2-normalised (NaN passes through like numpy).
fn unit_descriptor(raw: &[f64], d: &mut [f64]) {
    for (o, &v) in d.iter_mut().zip(raw) {
        *o = if v < 0.0 { 0.0 } else { v };
    }
    let norm = d.iter().map(|v| v * v).sum::<f64>().sqrt() + 1e-9;
    for v in d.iter_mut() {
        *v /= norm;
    }
}

#[cfg(test)]
mod tests;
