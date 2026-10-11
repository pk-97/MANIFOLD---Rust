//! Candidate finders (`lab/components.py` `FINDERS`): where a detector looks. Each emits candidates in
//! candidate order, one per hop at most, at the end of the hop that makes them known.

pub mod kick;
pub mod snare;

use super::container::Container;
use super::spec::{FinderKind, Measure, SpecError};

/// The kick finder's own measure width.
pub const F15: usize = Measure::F15.width();

/// One candidate: its onset hop, its emission hop (`cand_hop + deadline`: when the lab decides it), and the
/// kick finder's 15 features (zeros from any other finder; only the kick finder may name `f15`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub cand_hop: u64,
    pub avail_hop: u64,
    pub f15: [f64; F15],
}

pub enum Finder {
    Kick(Box<kick::BaseDetector>),
    Snare(snare::SnareCands),
}

impl Finder {
    pub fn new(kind: FinderKind, c: &Container) -> Result<Self, SpecError> {
        Ok(match kind {
            FinderKind::Kick => Self::Kick(Box::new(kick::BaseDetector::new(c)?)),
            FinderKind::Snare => Self::Snare(snare::SnareCands::new(c)?),
        })
    }

    /// The most candidates one push of `samples` 48 kHz samples can append.
    pub fn max_candidates(&self, samples: usize) -> usize {
        match self {
            Self::Kick(f) => f.max_candidates(samples),
            Self::Snare(f) => f.max_candidates(samples),
        }
    }

    /// Hops from a candidate to its emission.
    pub fn deadline(&self) -> u64 {
        match self {
            Self::Kick(f) => f.deadline(),
            Self::Snare(f) => f.deadline(),
        }
    }

    /// The emission: the end of the candidate's availability hop. No decision comes before it.
    pub fn ready_sample(&self, avail_hop: u64) -> u64 {
        (avail_hop + 1) * super::spec::MODEL_HOP
    }

    /// Appends each candidate found in `samples`; the caller reserves [`Self::max_candidates`] first.
    pub fn push(&mut self, samples: &[f64], out: &mut Vec<Candidate>) {
        match self {
            Self::Kick(f) => f.push(samples, out),
            Self::Snare(f) => f.push(samples, out),
        }
    }
}
