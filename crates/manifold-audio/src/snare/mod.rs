//! The snare (docs/SNARE_REALTIME_DESIGN.md): its v1 model file (`assets/snare_model.mkick`) run by the generic
//! runtime ([`crate::trained`]). A thin constructor until P3 of docs/DETECTOR_RUNTIME_DESIGN.md replaces the
//! file with its converted v2 release and deletes this module.

use std::sync::OnceLock;

use crate::trained::container::{Container, ContainerError};
use crate::trained::v1::{self, V1Error};
use crate::trained::{DetectorError, TrainedDetector};

static MODEL: OnceLock<Result<Container, ContainerError>> = OnceLock::new();

/// The embedded model, parsed once per process.
fn model() -> Result<&'static Container, ContainerError> {
    MODEL.get_or_init(|| Container::parse(include_bytes!("../../assets/snare_model.mkick"))).as_ref().map_err(Clone::clone)
}

#[derive(Debug, Clone, PartialEq)]
pub enum SnareError {
    V1(V1Error),
    Detector(DetectorError),
}

impl std::fmt::Display for SnareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::V1(e) => write!(f, "snare: {e}"),
            Self::Detector(e) => write!(f, "snare: {e}"),
        }
    }
}

impl std::error::Error for SnareError {}

impl From<ContainerError> for SnareError {
    fn from(e: ContainerError) -> Self {
        Self::V1(V1Error::Model(e))
    }
}

/// One fire: the input sample count at which it was decided (the stamp, as for kicks, so a fire lands on the
/// same hop whatever the chunking), its emission on the input clock, and its candidate's hops on the 48 kHz
/// model grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnareFire {
    pub sample: u64,
    pub emission: u64,
    pub cand_hop: u64,
    pub avail_hop: u64,
}

/// The shipped snare: [`TrainedDetector`] on the embedded v1 file read as contract v2.
pub struct SnareDetector(TrainedDetector);

impl SnareDetector {
    /// Builds the detector for mono input at `input_rate` Hz from the embedded model.
    pub fn new(input_rate: u32) -> Result<Self, SnareError> {
        Self::from_container(model()?, input_rate)
    }

    pub fn from_container(c: &Container, input_rate: u32) -> Result<Self, SnareError> {
        let v2 = v1::snare(c).map_err(SnareError::V1)?;
        Ok(Self(TrainedDetector::from_container(&v2, input_rate).map_err(SnareError::Detector)?))
    }

    pub fn max_fires(&self, samples: usize) -> usize {
        self.0.max_fires(samples)
    }

    pub fn decided(&self) -> u64 {
        self.0.decided()
    }

    pub fn dropped(&self) -> u64 {
        self.0.dropped()
    }

    /// Feeds mono input samples; appends each fire as it is decided.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<SnareFire>) {
        self.push_with(samples, |f| out.push(f));
    }

    /// Feeds mono input samples; hands each fire to `on_fire` as it is decided.
    pub fn push_with(&mut self, samples: &[f32], mut on_fire: impl FnMut(SnareFire)) {
        let clock = self.0.input_clock();
        self.0.push_with(samples, |f| {
            let emission = clock.input_count((f.avail_hop + 1) * crate::trained::spec::MODEL_HOP);
            on_fire(SnareFire { sample: f.sample, emission, cand_hop: f.cand_hop, avail_hop: f.avail_hop });
        });
    }
}

#[cfg(test)]
mod tests;
