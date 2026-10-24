//! [`TrainedDetector`]: any contract-v2 model as one streaming detector (docs/DETECTOR_RUNTIME_DESIGN.md
//! section 5.3). The file names its parts; exactly those are built. Mono audio of any rate in, fires stamped
//! at the input sample where each one is decided out (D14).

use super::container::Container;
use super::finder::{Candidate, Finder};
use super::frames::FrameBank;
use super::history::History;
use super::input::ModelInput;
use super::measure::MeasureBank;
use super::net::{Net, average};
use super::spec::{Combine, MODEL_HOP, MODEL_RATE, Measure, Spec, SpecError};
use super::trees::{Forest, expit, logit};
use crate::detectors::EventKind;

/// Candidates waiting for their measures and the net (about 40 ms at most one per hop, so 64 leaves a wide
/// margin).
const PENDING: usize = 64;

/// One fire: the input-rate sample count at which it was decided, and its candidate's hops on the 48 kHz
/// model grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fire {
    pub sample: u64,
    pub cand_hop: u64,
    pub avail_hop: u64,
}

/// One candidate's decision, every intermediate exposed for parity tests and diagnostics.
pub struct Decision<'a> {
    pub cand_hop: u64,
    pub avail_hop: u64,
    /// `Spec::measures()` hstacked.
    pub measures: &'a [f64],
    /// One per net; empty when the model has no net.
    pub net_logits: &'a [f32],
    /// Averaged per D17; NaN when no net.
    pub p_net: f64,
    /// NaN when no trees.
    pub p_trees: f64,
    pub p_combined: f64,
    /// Empty when no stage.
    pub stage_x: &'a [f64],
    /// Ungated; NaN when no stage.
    pub p_stage: f64,
    pub p_final: f64,
    pub fire: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DetectorError {
    Spec(SpecError),
    BadRate(u32),
    /// The model reads a kick verdict, and the kick it runs with is not the one it was trained with.
    KickMismatch {
        want: String,
        got: String,
    },
}

impl std::fmt::Display for DetectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spec(e) => write!(f, "{e}"),
            Self::BadRate(r) => write!(f, "detector: input rate {r} Hz"),
            Self::KickMismatch { want, got } => write!(
                f,
                "detector: reads the kick {want}'s verdicts, the kick here is {got}"
            ),
        }
    }
}

impl std::error::Error for DetectorError {}

impl From<SpecError> for DetectorError {
    fn from(e: SpecError) -> Self {
        Self::Spec(e)
    }
}

impl From<super::container::ContainerError> for DetectorError {
    fn from(e: super::container::ContainerError) -> Self {
        Self::Spec(e.into())
    }
}

/// `detector_eval.fires`: p at or over the cutoff, and the emission at least the refractory after the last
/// fire's (in hops of the model grid). NaN never fires.
#[derive(Debug, Clone)]
pub(crate) struct FireRule {
    cutoff: f64,
    refractory_s: f64,
    last: Option<u64>,
}

impl FireRule {
    pub(crate) fn new(cutoff: f64, refractory_s: f64) -> Self {
        Self {
            cutoff,
            refractory_s,
            last: None,
        }
    }

    pub(crate) fn decide(&mut self, p: f64, avail_hop: u64) -> bool {
        let clear = self.last.is_none_or(|last| {
            let gap = (avail_hop as i64 - last as i64) * MODEL_HOP as i64;
            gap as f64 / MODEL_RATE as f64 >= self.refractory_s - 1e-12
        });
        let fire = p >= self.cutoff && clear;
        if fire {
            self.last = Some(avail_hop);
        }
        fire
    }
}

/// `models.combine` with the mean-logit rule (`expit(mean logit(clip(p)))` of net and trees).
pub(crate) fn combine(how: Combine, p_net: f64, p_trees: f64, clip: f64) -> f64 {
    match how {
        Combine::Net => p_net,
        Combine::Trees => p_trees,
        Combine::MeanLogit => {
            let lg = |p: f64| logit(p.clamp(clip, 1.0 - clip));
            expit((lg(p_net) + lg(p_trees)) / 2.0)
        }
    }
}

#[derive(Clone, Copy)]
struct Pending {
    cand: Candidate,
    ready: u64,
}

/// A model that reads the kick's verdicts (kick2): the kick it was trained with, how far after an emission
/// the kick decides at most (`None` until linked), and the latest emission hop through which every kick
/// candidate is decided.
struct KickLink {
    train_id: String,
    lag: Option<u64>,
    settled: Option<u64>,
}

/// Where each measure sits in the decision row.
#[derive(Clone, Copy)]
struct Column {
    measure: Measure,
    at: usize,
}

/// Streaming detector for any contract-v2 model. No allocation after construction when the caller reserves
/// [`Self::max_fires`] in the output first.
pub struct TrainedDetector {
    spec: Spec,
    input: ModelInput,
    finder: Finder,
    measures: MeasureBank,
    columns: Vec<Column>,
    trees: Option<(Forest, usize)>,
    net: Option<Net>,
    frames: FrameBank,
    history: Option<History>,
    /// Where the stage shape measures and the level column sit in the row.
    shape_cols: Vec<Column>,
    level_col: Option<usize>,
    rule: FireRule,
    kick: Option<KickLink>,
    cands: Vec<Candidate>,
    pending: [Option<Pending>; PENDING],
    head: usize,
    len: usize,
    samples48: u64,
    decided: u64,
    dropped: u64,
    last_avail: u64,
    row: Vec<f64>,
    logits: Vec<f32>,
    shape_row: Vec<f64>,
}

impl TrainedDetector {
    pub fn from_container(c: &Container, input_rate: u32) -> Result<Self, DetectorError> {
        let spec = Spec::parse(c)?;
        let input = ModelInput::new(input_rate).ok_or(DetectorError::BadRate(input_rate))?;
        let finder = Finder::new(spec.finder, c)?;
        let used = spec.measures();
        let mut columns = Vec::with_capacity(used.len());
        let mut at = 0;
        for &measure in &used {
            columns.push(Column { measure, at });
            at += measure.width();
        }
        let col = |m: Measure| {
            columns
                .iter()
                .find(|c| c.measure == m)
                .expect("Spec::measures holds every measure")
                .at
        };
        let trees = match &spec.trees {
            // Spec::measures starts with the tree measures in column order.
            Some(ms) => Some((
                Forest::from_container(c, "trees.")?,
                ms.iter().map(|m| m.width()).sum(),
            )),
            None => None,
        };
        let (net, frames) = match &spec.net {
            Some(n) => {
                let net = Net::new(c, n.count)?;
                if net.channels() != n.channels.len() + 2 {
                    return Err(SpecError::Mismatch {
                        what: "net channels",
                        detail: format!("weights take {} channels", net.channels()),
                    }
                    .into());
                }
                (Some(net), FrameBank::new(c, &n.channels)?)
            }
            None => (None, FrameBank::new(c, &[])?),
        };
        let (history, shape_cols, level_col) = match &spec.stage {
            Some(st) => (
                Some(History::new(c, st, MODEL_HOP, MODEL_RATE)?),
                st.shape
                    .iter()
                    .map(|&m| Column {
                        measure: m,
                        at: col(m),
                    })
                    .collect(),
                st.level.map(|(m, i)| col(m) + i),
            ),
            None => (None, Vec::new(), None),
        };
        let shape_width = spec.stage.as_ref().map_or(0, |s| s.shape_width());
        let kick = if used.contains(&Measure::Kick2) {
            Some(KickLink {
                train_id: c.text("kick2.kick_train_id")?.to_owned(),
                lag: None,
                settled: None,
            })
        } else {
            None
        };
        let count = spec.net.as_ref().map_or(0, |n| n.count);
        Ok(Self {
            measures: MeasureBank::new(c, &used)?,
            cands: Vec::with_capacity(finder.max_candidates(ModelInput::max_model(input_rate))),
            rule: FireRule::new(spec.cutoff, spec.refractory_s),
            kick,
            finder,
            columns,
            trees,
            net,
            frames,
            history,
            shape_cols,
            level_col,
            input,
            pending: [None; PENDING],
            head: 0,
            len: 0,
            samples48: 0,
            decided: 0,
            dropped: 0,
            last_avail: 0,
            row: vec![0.0; at],
            logits: vec![0.0; count],
            shape_row: vec![0.0; shape_width],
            spec,
        })
    }

    pub fn kind(&self) -> EventKind {
        self.spec.family
    }

    pub fn train_id(&self) -> &str {
        &self.spec.train_id
    }

    pub fn spec(&self) -> &Spec {
        &self.spec
    }

    #[cfg(test)]
    pub(crate) fn net(&self) -> Option<&Net> {
        self.net.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn frames(&self) -> &FrameBank {
        &self.frames
    }

    /// The first 48 kHz sample count at which every part can decide this candidate. A model reading kick
    /// verdicts also waits out the kick's own decision lag.
    fn ready(&self, cand: &Candidate) -> u64 {
        let mut ready = self
            .finder
            .ready_sample(cand.avail_hop)
            .max(self.measures.ready_sample(cand));
        if let Some(n) = &self.net {
            ready = ready.max(n.ready_sample(cand.avail_hop));
        }
        if let Some(k) = &self.kick {
            ready = ready.max(self.finder.ready_sample(cand.avail_hop) + k.lag.unwrap_or(0));
        }
        ready
    }

    /// The kick model this one reads verdicts from, if any.
    pub fn wants_kick(&self) -> Option<&str> {
        self.kick.as_ref().map(|k| k.train_id.as_str())
    }

    /// Pairs this model with the kick detector that will feed it, refusing any kick but the trained one.
    pub fn link_kick(&mut self, kick: &TrainedDetector) -> Result<(), DetectorError> {
        let lag = kick.decision_lag_samples();
        let link = self
            .kick
            .as_mut()
            .expect("link_kick on a model without kick2");
        if link.train_id != kick.train_id() {
            return Err(DetectorError::KickMismatch {
                want: link.train_id.clone(),
                got: kick.train_id().to_owned(),
            });
        }
        link.lag = Some(lag);
        Ok(())
    }

    /// One kick decision, in the kick's emission order.
    pub fn kick_verdict(&mut self, cand_hop: u64, avail_hop: u64, p_final: f64) {
        self.measures.push_verdict(cand_hop, avail_hop, p_final);
    }

    /// Every kick candidate with emission hop at or before `through` is decided and fed.
    pub fn kick_settled(&mut self, through: Option<u64>) {
        if let Some(k) = &mut self.kick {
            k.settled = through;
        }
    }

    /// The latest emission hop through which every candidate of this detector is decided: candidates are
    /// found at the end of their emission hop, and decided in emission order.
    pub fn settled_avail(&self) -> Option<u64> {
        let found = (self.samples48 / MODEL_HOP).checked_sub(1)?;
        match self.pending[self.head].filter(|_| self.len > 0) {
            Some(p) => p.cand.avail_hop.checked_sub(1).map(|a| a.min(found)),
            None => Some(found),
        }
    }

    /// Seconds from a candidate's emission to its decision, from the parts' ready rules: the most over every
    /// phase of the hop grid against the parts' own grids (the net's 2 ms frames, the 1 ms envelopes). Holds
    /// once the stream is longer than the net's slice; candidates in the first slice wait for it to fill.
    pub fn decision_lag_s(&self) -> f64 {
        let deadline = self.finder.deadline();
        let lag = (1000..1000 + 375u64)
            .map(|cand_hop| {
                let c = Candidate {
                    cand_hop,
                    avail_hop: cand_hop + deadline,
                    f15: [0.0; 15],
                };
                self.ready(&c) - self.finder.ready_sample(c.avail_hop)
            })
            .max()
            .unwrap_or(0);
        lag as f64 / MODEL_RATE as f64
    }

    /// [`Self::decision_lag_s`] in model samples.
    pub fn decision_lag_samples(&self) -> u64 {
        (self.decision_lag_s() * MODEL_RATE as f64).round() as u64
    }

    /// The most fires one push of `samples` input samples can append: one per candidate found in it, plus
    /// every candidate already waiting.
    pub fn max_fires(&self, samples: usize) -> usize {
        self.finder
            .max_candidates(self.input.model_samples(samples))
            + PENDING
    }

    /// Candidates decided so far (fired or not).
    pub fn decided(&self) -> u64 {
        self.decided
    }

    /// Candidates lost to a full pending queue or a full history; 0 on every golden.
    pub fn dropped(&self) -> u64 {
        self.dropped + self.history.as_ref().map_or(0, History::dropped)
    }

    /// Feeds mono input samples; hands each fire to `on_fire` as it is decided.
    pub fn push_with(&mut self, samples: &[f32], mut on_fire: impl FnMut(Fire)) {
        self.push_traced(samples, |_, fire| {
            if let Some(f) = fire {
                on_fire(f);
            }
        });
    }

    /// Feeds mono input samples; hands every decision (fired or not) to `on_decision` as it is made.
    pub fn push_traced(
        &mut self,
        samples: &[f32],
        mut on_decision: impl FnMut(&Decision<'_>, Option<Fire>),
    ) {
        // Unlinked, a kick2 model would wait for kick verdicts forever and never fire.
        debug_assert!(
            self.kick.as_ref().is_none_or(|k| k.lag.is_some()),
            "a kick2 model runs only fed by its kick (KickFed)"
        );
        for chunk in samples.chunks(ModelInput::STEP) {
            self.input.feed(chunk);
            self.step_block(&mut on_decision);
        }
    }

    fn step_block(&mut self, on_decision: &mut impl FnMut(&Decision<'_>, Option<Fire>)) {
        self.cands.clear();
        let block = &self.input.block;
        self.finder.push(block, &mut self.cands);
        self.measures.push(block);
        if let Some(n) = &mut self.net {
            n.push(block);
        }
        self.frames
            .push(block, self.net.as_ref().map(Net::spectrum));
        self.samples48 += block.len() as u64;
        for i in 0..self.cands.len() {
            let cand = self.cands[i];
            if self.len == PENDING {
                self.dropped += 1;
                continue;
            }
            let ready = self.ready(&cand);
            self.pending[(self.head + self.len) % PENDING] = Some(Pending { cand, ready });
            self.len += 1;
        }
        while self.len > 0 {
            let kick_ok = |p: &Pending| {
                self.kick
                    .as_ref()
                    .is_none_or(|k| k.settled.is_some_and(|s| s >= p.cand.avail_hop))
            };
            let Some(p) =
                self.pending[self.head].filter(|p| p.ready <= self.samples48 && kick_ok(p))
            else {
                break;
            };
            self.head = (self.head + 1) % PENDING;
            self.len -= 1;
            self.decide(&p, on_decision);
        }
    }

    /// Section 5.3, per decided candidate: measures, trees and nets, combine, history, gate, fire rule.
    fn decide(&mut self, p: &Pending, on_decision: &mut impl FnMut(&Decision<'_>, Option<Fire>)) {
        let c = &p.cand;
        debug_assert!(
            c.avail_hop >= self.last_avail,
            "decisions out of emission order"
        );
        self.last_avail = c.avail_hop;
        self.decided += 1;
        for col in &self.columns {
            self.measures.write(
                col.measure,
                c,
                &mut self.row[col.at..col.at + col.measure.width()],
            );
        }
        let p_trees = self
            .trees
            .as_ref()
            .map_or(f64::NAN, |(f, width)| f.predict_proba(&self.row[..*width]));
        let p_net = match &mut self.net {
            Some(net) => {
                net.logits(c.cand_hop, c.avail_hop, &self.frames, &mut self.logits);
                average(&self.logits, self.spec.combine_clip)
            }
            None => f64::NAN,
        };
        let p_combined = combine(self.spec.combine, p_net, p_trees, self.spec.combine_clip);
        let (stage_x, p_stage, p_final): (&[f64], f64, f64) = match &mut self.history {
            Some(h) => {
                let mut at = 0;
                for col in &self.shape_cols {
                    let w = col.measure.width();
                    self.shape_row[at..at + w].copy_from_slice(&self.row[col.at..col.at + w]);
                    at += w;
                }
                let level = self.level_col.map_or(0.0, |i| self.row[i]);
                h.decide(p_combined, &self.shape_row, level, c.avail_hop + 1)
            }
            None => (&[], f64::NAN, p_combined),
        };
        let fire = self.rule.decide(p_final, c.avail_hop);
        let d = Decision {
            cand_hop: c.cand_hop,
            avail_hop: c.avail_hop,
            measures: &self.row,
            net_logits: &self.logits,
            p_net,
            p_trees,
            p_combined,
            stage_x,
            p_stage,
            p_final,
            fire,
        };
        let f = fire.then(|| Fire {
            sample: self.input.input_count(p.ready),
            cand_hop: c.cand_hop,
            avail_hop: c.avail_hop,
        });
        on_decision(&d, f);
    }
}

impl crate::detectors::EventDetector for TrainedDetector {
    fn push(&mut self, samples: &[f32], events: &mut Vec<crate::detectors::DetectorEvent>) {
        let kind = self.kind();
        self.push_with(samples, |f| {
            events.push(crate::detectors::DetectorEvent {
                kind,
                sample: f.sample,
            })
        });
    }

    fn max_events(&self, samples: usize) -> usize {
        self.max_fires(samples)
    }
}

#[cfg(test)]
mod tests;
