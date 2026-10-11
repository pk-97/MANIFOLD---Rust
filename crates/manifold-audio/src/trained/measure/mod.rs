//! Event measures (`lab/components.py` `MEASURES`): numbers about one candidate, for trees and the history stage.
//! [`MeasureBank`] runs only the streams the model's measures read (the band spectrum for tmpl3, prof32 and
//! fall16; the tail envelope; the low bank). Every coefficient comes from the `extra.*` and `fit.*` entries.

pub(crate) mod dsp;
mod lowbank;
mod profile;
mod spec;
mod tail;
mod templates;

use super::container::{Container, ContainerError};
use super::finder::Candidate;
use super::spec::Measure;

/// 1 ms grid history: covers the 60 ms pre-onset reach, the evidence span, and about
/// 3.5 s of lateness between `ready_sample` and the `write` call.
const HIST_MS: usize = 4096;
/// Spectral frame history (one per hop, about 5.4 s at 48 kHz / 256).
const HIST_FRAMES: usize = 1024;

/// Stream constants shared by the pieces.
struct Consts {
    sr: f64,
    hop: u64,
    ms: f64,
}

impl Consts {
    /// `(hop_index + 1) * hop / sr`, the reference's `onset_s` / `emit_s`.
    fn hop_end_s(&self, h: u64) -> f64 {
        ((h + 1) * self.hop) as f64 / self.sr
    }
}

fn scalar(c: &Container, name: &str) -> Result<f64, ContainerError> {
    Ok(c.f64s(name, &[])?[0])
}

fn positive(c: &Container, name: &str) -> Result<u64, ContainerError> {
    match c.i64s(name, &[])?[0] {
        v if v > 0 => Ok(v as u64),
        _ => Err(ContainerError::WrongType { name: name.to_owned(), want: "positive i64" }),
    }
}

/// Streams 48 kHz mono audio and writes any measure for a candidate whose data has arrived.
/// No allocation after `new`.
pub struct MeasureBank {
    k: Option<Consts>,
    samples: u64,
    tail: Option<tail::Tail>,
    spec: Option<spec::BandSpec>,
    templates: Option<templates::Templates>,
    low: Option<lowbank::LowBank>,
}

impl MeasureBank {
    /// Builds the streams `measures` read; f15 needs none (the kick finder computes it).
    pub fn new(c: &Container, measures: &[Measure]) -> Result<Self, ContainerError> {
        let uses = |m: Measure| measures.contains(&m);
        let k = if measures.iter().any(|&m| m != Measure::F15) {
            Some(Consts { sr: positive(c, "extra.sample_rate")? as f64, hop: positive(c, "extra.hop")?, ms: scalar(c, "extra.ms")? })
        } else {
            None
        };
        let band = uses(Measure::Tmpl3) || uses(Measure::Prof32) || uses(Measure::Fall16);
        Ok(Self {
            tail: if uses(Measure::Tail3) { Some(tail::Tail::new(c, k.as_ref().expect("set above"))?) } else { None },
            spec: if band { Some(spec::BandSpec::new(c, k.as_ref().expect("set above").hop)?) } else { None },
            templates: if uses(Measure::Tmpl3) { Some(templates::Templates::new(c)?) } else { None },
            low: if uses(Measure::Low16) { Some(lowbank::LowBank::new(c, k.as_ref().expect("set above"))?) } else { None },
            samples: 0,
            k,
        })
    }

    pub fn push(&mut self, samples: &[f64]) {
        if self.k.is_none() {
            return;
        }
        for &x in samples {
            let s = self.samples;
            if let Some(t) = &mut self.tail {
                t.push(s, x);
            }
            if let Some(b) = &mut self.spec {
                b.push(s, x);
            }
            if let Some(l) = &mut self.low {
                l.push(s, x);
            }
            self.samples += 1;
        }
    }

    /// Samples pushed so far (0 when no stream runs).
    pub fn samples(&self) -> u64 {
        self.samples
    }

    /// The first stream sample count at which every instantiated measure is valid for this candidate: the
    /// tail and low-bank envelopes at the deadline's 1 ms grid point, the spectrum's deadline frame, and the
    /// template patch's last frame (`cand + span - 1`, which can pass the deadline). 0 when no stream runs.
    pub fn ready_sample(&self, cand: &Candidate) -> u64 {
        let Some(k) = &self.k else { return 0 };
        let frame_end = |f: u64| (f + 1) * k.hop;
        let (c, a) = (cand.cand_hop, cand.avail_hop);
        let mut ready = 0;
        if let Some(t) = &self.tail {
            ready = ready.max(t.ready_sample(k, a));
        }
        if let Some(l) = &self.low {
            ready = ready.max(l.ready_sample(k, a));
        }
        if self.spec.is_some() {
            ready = ready.max(frame_end(a));
        }
        if let Some(t) = &self.templates {
            ready = ready.max(frame_end(t.last_frame(c)));
        }
        ready
    }

    /// Writes measure `m` (its width) for one candidate. Call between `ready_sample` and about 3.5 s later;
    /// outside that window, or for a measure the bank was not built with, it panics rather than read
    /// missing history.
    pub fn write(&mut self, m: Measure, cand: &Candidate, out: &mut [f64]) {
        debug_assert_eq!(out.len(), m.width());
        if m == Measure::F15 {
            out.copy_from_slice(&cand.f15);
            return;
        }
        assert!(self.samples >= self.ready_sample(cand), "measure {}: candidate {} asked before its data arrived", m.name(), cand.cand_hop);
        let k = self.k.as_ref().expect("built with this measure");
        let (c, a) = (cand.cand_hop, cand.avail_hop);
        let spec = || self.spec.as_ref().expect("built with this measure");
        match m {
            Measure::F15 => unreachable!("handled above"),
            Measure::Tail3 => self.tail.as_ref().expect("built with tail3").features(k, c, a, out),
            Measure::Tmpl3 => {
                let spec = self.spec.as_ref().expect("built with tmpl3");
                self.templates.as_mut().expect("built with tmpl3").features(spec, c, out)
            }
            Measure::Prof32 => profile::rise(spec(), c, a, out),
            Measure::Fall16 => profile::fall(spec(), c, a, out),
            Measure::Low16 => self.low.as_ref().expect("built with low16").features(k, c, a, out),
        }
    }
}

#[cfg(test)]
mod tests;
