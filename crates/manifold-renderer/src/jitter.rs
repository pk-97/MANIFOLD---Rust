//! Halton sequence jitter generator for temporal AA.
//!
//! Generates sub-pixel offsets in [−0.5, +0.5] using Halton bases 2 and 3.
//! Each frame gets a unique jitter offset so the TAA pass can accumulate
//! sub-pixel detail across multiple frames.

/// Sub-pixel jitter generator using Halton(2,3) quasi-random sequence.
#[derive(Clone, Copy, Debug)]
pub struct JitterSequence {
    frame_index: u32,
}

impl JitterSequence {
    pub fn new() -> Self {
        Self { frame_index: 0 }
    }

    /// Current sub-pixel jitter offset in [−0.5, +0.5] pixel range.
    pub fn current_offset(&self) -> (f32, f32) {
        let x = halton(self.frame_index, 2) - 0.5;
        let y = halton(self.frame_index, 3) - 0.5;
        (x, y)
    }

    /// Advance to the next frame in the sequence.
    pub fn advance(&mut self) {
        self.frame_index = self.frame_index.wrapping_add(1) & 0xFF;
    }

    /// Reset to the beginning (e.g., after seek).
    pub fn reset(&mut self) {
        self.frame_index = 0;
    }
}

impl Default for JitterSequence {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute the n-th element of the Halton sequence for the given base.
fn halton(mut index: u32, base: u32) -> f32 {
    let mut result = 0.0f32;
    let mut f = 1.0 / base as f32;
    index += 1; // skip index 0 (always 0)
    while index > 0 {
        result += f * (index % base) as f32;
        index /= base;
        f /= base as f32;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_range() {
        let mut seq = JitterSequence::new();
        for _ in 0..256 {
            let (x, y) = seq.current_offset();
            assert!((-0.5..0.5).contains(&x));
            assert!((-0.5..0.5).contains(&y));
            seq.advance();
        }
    }

    #[test]
    fn wraps_at_256() {
        let mut seq = JitterSequence::new();
        let first = seq.current_offset();
        for _ in 0..256 {
            seq.advance();
        }
        assert_eq!(first, seq.current_offset());
    }
}
