//! The snare CNN: the kick net family with a third input channel, the noisiness frames
//! (reference: `snare_release.py` `reference()`, `Song(..., extra=(flat,))`).

use crate::kick::container::{Container, ContainerError};
use crate::kick::net::{FrameSource, KickNet};

/// Push audio, then score each candidate once `ready_sample(emit_hop)` samples have arrived and the
/// noisiness source holds the slice's frames.
pub struct SnareNet {
    net: KickNet,
}

impl SnareNet {
    pub fn new(c: &Container) -> Result<Self, ContainerError> {
        let net = KickNet::new(c)?;
        if net.channels() != 3 {
            return Err(ContainerError::WrongShape { name: "net.conv.0.weight".into(), want: vec![3], got: vec![net.channels()] });
        }
        Ok(Self { net })
    }

    pub fn push(&mut self, samples: &[f64]) {
        self.net.push(samples);
    }

    pub fn ready_sample(&self, emit_hop: u64) -> u64 {
        self.net.ready_sample(emit_hop)
    }

    /// The logit for one candidate; `flat` holds the raw noisiness frames on the net's grid.
    /// NaN when a frame the slice reads is not held.
    pub fn logit(&mut self, cand_hop: u64, emit_hop: u64, flat: &dyn FrameSource) -> f64 {
        self.net.logit_with(cand_hop, emit_hop, &[flat]) as f64
    }

    /// `reference()`: the sigmoid of the f32 logit widened to f64.
    pub fn probability(&mut self, cand_hop: u64, emit_hop: u64, flat: &dyn FrameSource) -> f64 {
        1.0 / (1.0 + (-self.logit(cand_hop, emit_hop, flat)).exp())
    }

    #[cfg(test)]
    fn spectrum(&self) -> &crate::kick::net::Spectrum {
        self.net.spectrum()
    }
}

#[cfg(test)]
mod tests;
