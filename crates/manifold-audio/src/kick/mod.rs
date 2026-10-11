//! The shipped kick (docs/KICK_REALTIME_DESIGN.md): its v1 model file run by the generic runtime
//! ([`crate::trained`]). A thin constructor until P3 of docs/DETECTOR_RUNTIME_DESIGN.md replaces the file with
//! its converted v2 release and deletes this module.

use std::sync::OnceLock;

pub use crate::trained::container;
use crate::trained::container::{Container, ContainerError};
use crate::trained::v1::{self, V1Error};
use crate::trained::{DetectorError, Fire, TrainedDetector};

pub type KickFire = Fire;

static MODEL: OnceLock<Result<Container, ContainerError>> = OnceLock::new();

/// The embedded model, parsed once per process.
fn model() -> Result<&'static Container, ContainerError> {
    MODEL.get_or_init(|| Container::parse(include_bytes!("../../assets/kick_model.mkick"))).as_ref().map_err(Clone::clone)
}

#[derive(Debug, Clone, PartialEq)]
pub enum KickError {
    V1(V1Error),
    Detector(DetectorError),
}

impl std::fmt::Display for KickError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::V1(e) => write!(f, "kick: {e}"),
            Self::Detector(e) => write!(f, "kick: {e}"),
        }
    }
}

impl std::error::Error for KickError {}

impl From<ContainerError> for KickError {
    fn from(e: ContainerError) -> Self {
        Self::V1(V1Error::Model(e))
    }
}

/// The shipped kick: [`TrainedDetector`] on the embedded v1 file read as contract v2.
pub struct KickDetector(TrainedDetector);

impl KickDetector {
    /// Builds the detector for mono input at `input_rate` Hz from the embedded model.
    pub fn new(input_rate: u32) -> Result<Self, KickError> {
        Self::from_container(model()?, input_rate)
    }

    pub fn from_container(c: &Container, input_rate: u32) -> Result<Self, KickError> {
        let v2 = v1::kick(c).map_err(KickError::V1)?;
        Ok(Self(TrainedDetector::from_container(&v2, input_rate).map_err(KickError::Detector)?))
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
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<KickFire>) {
        self.0.push_with(samples, |f| out.push(f));
    }

    /// Feeds mono input samples; hands each fire to `on_fire` as it is decided.
    pub fn push_with(&mut self, samples: &[f32], on_fire: impl FnMut(KickFire)) {
        self.0.push_with(samples, on_fire);
    }
}

#[cfg(test)]
mod tests;
