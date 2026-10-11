//! Input of any rate in, 48 kHz model blocks out (D13), plus the map from model sample counts back to the
//! input clock that fires are stamped on (D14).

use super::spec::MODEL_RATE;
use crate::analysis::LinearResampler;

/// Input samples handled per internal step; bounds every scratch buffer.
const IN_STEP: usize = 512;

pub(crate) struct ModelInput {
    /// `None` at 48 kHz. Linear interpolation without an anti-alias filter: downsampling folds
    /// content above 24 kHz back into the band, which reaches only the upper-band features and
    /// is small next to music's level up there. Upsampling adds no content above the source's
    /// Nyquist, so the model sees a band-limited 48 kHz stream.
    resampler: Option<LinearResampler>,
    /// Input samples per model sample.
    step: f64,
    resampled: Vec<f32>,
    /// The latest model block; valid after [`Self::feed`].
    pub(crate) block: Vec<f64>,
}

impl ModelInput {
    /// Input samples handled per [`Self::feed`]; bounds every scratch buffer.
    pub(crate) const STEP: usize = IN_STEP;

    /// `None` for a zero rate.
    pub(crate) fn new(input_rate: u32) -> Option<Self> {
        if input_rate == 0 {
            return None;
        }
        let max = Self::max_model(input_rate);
        Some(Self {
            resampler: (input_rate != MODEL_RATE).then(|| LinearResampler::new(input_rate, MODEL_RATE)),
            step: input_rate as f64 / MODEL_RATE as f64,
            resampled: Vec::with_capacity(max),
            block: Vec::with_capacity(max),
        })
    }

    /// Model samples one [`Self::STEP`] of input can produce, plus the resampler's carried edge.
    pub(crate) fn max_model(input_rate: u32) -> usize {
        (IN_STEP as u64 * MODEL_RATE as u64).div_ceil(input_rate as u64) as usize + 2
    }

    /// Model samples a push of `samples` input samples can produce.
    pub(crate) fn model_samples(&self, samples: usize) -> usize {
        (samples as f64 / self.step).ceil() as usize + 2
    }

    /// Fills [`Self::block`] from at most [`Self::STEP`] input samples.
    pub(crate) fn feed(&mut self, chunk: &[f32]) {
        self.block.clear();
        match self.resampler.as_mut() {
            Some(r) => {
                self.resampled.clear();
                r.process(chunk, &mut self.resampled);
                self.block.extend(self.resampled.iter().map(|&x| x as f64));
            }
            None => self.block.extend(chunk.iter().map(|&x| x as f64)),
        }
    }

    /// The input sample count by which `model_count` model samples exist.
    pub(crate) fn input_count(&self, model_count: u64) -> u64 {
        self.clock().input_count(model_count)
    }

    pub(crate) fn clock(&self) -> InputClock {
        InputClock { step: self.resampler.is_some().then_some(self.step) }
    }
}

/// The map from model sample counts back to the input clock.
#[derive(Debug, Clone, Copy)]
pub(crate) struct InputClock {
    /// Input samples per model sample; `None` at 48 kHz.
    step: Option<f64>,
}

impl InputClock {
    /// The input sample count by which `model_count` model samples exist. The resampler emits model sample
    /// `j` once input index `floor(j * step)` has arrived.
    pub(crate) fn input_count(self, model_count: u64) -> u64 {
        match self.step {
            None => model_count,
            Some(step) => ((model_count.saturating_sub(1)) as f64 * step).floor() as u64 + 1,
        }
    }
}
