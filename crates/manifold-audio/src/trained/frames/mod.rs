//! Frame measures (`lab/components.py` `FRAME_MEASURES`): per-frame channels on the net's 2 ms grid, fed to
//! the net after its shape and new channels.

pub mod flat;
pub mod perc;

use super::container::Container;
use super::net::Spectrum;
use super::spec::{Channel, SpecError};

enum Frames {
    Flat(Box<flat::Flatness>),
    Perc(perc::Percussive),
}

impl Frames {
    fn frame(&self, f: u64) -> Option<&[f32]> {
        match self {
            Self::Flat(s) => s.frame(f),
            Self::Perc(s) => s.frame(f),
        }
    }
}

/// The net's extra input channels, in the model's order.
pub trait FrameSource {
    fn channels(&self) -> usize;
    /// Channel `ch`'s frame `f` (64 values); `None` when it is not held.
    fn frame(&self, ch: usize, f: u64) -> Option<&[f32]>;
}

/// Every extra channel a model's net reads, streamed together.
pub struct FrameBank {
    channels: Vec<Frames>,
}

impl FrameBank {
    pub fn new(c: &Container, channels: &[Channel]) -> Result<Self, SpecError> {
        let mut out = Vec::with_capacity(channels.len());
        for ch in channels {
            out.push(match ch {
                Channel::Flat => Frames::Flat(Box::new(flat::Flatness::new(c)?)),
                Channel::Perc => Frames::Perc(perc::Percussive::new(c)?),
            });
        }
        Ok(Self { channels: out })
    }

    /// Audio for the channels that read it, then the frames the net's spectrum has added for those that
    /// read the spectrum. Call after the net's own push.
    pub fn push(&mut self, samples: &[f64], spec: Option<&Spectrum>) {
        for ch in &mut self.channels {
            match ch {
                Frames::Flat(s) => s.push(samples),
                Frames::Perc(s) => s.update(spec.expect("perc is a net channel, so the net runs")),
            }
        }
    }
}

impl FrameSource for FrameBank {
    fn channels(&self) -> usize {
        self.channels.len()
    }

    fn frame(&self, ch: usize, f: u64) -> Option<&[f32]> {
        self.channels.get(ch)?.frame(f)
    }
}
