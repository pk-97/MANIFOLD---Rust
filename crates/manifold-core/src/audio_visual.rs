//! Bounded per-send waveform and spectrum histories for visual consumers.

use crate::AudioSendId;
use ahash::AHashMap;

const WAVE_MAX_MS: f32 = 250.0;

/// History for one audio send. Storage is allocated when the analyzer rate and
/// spectrum shape are known, then reused by every feed/read operation.
pub struct AudioVisualHistory {
    sample_rate: u32,
    waveform: Vec<f32>,
    waveform_head: usize,
    waveform_len: usize,
    spectrum_bins: usize,
    spectrum_hop: usize,
    spectrum_capacity: usize,
    spectrum: Vec<f32>,
    spectrum_head: usize,
    spectrum_len: usize,
}

impl AudioVisualHistory {
    pub fn new(sample_rate: u32, spectrum_bins: usize, spectrum_hop: usize) -> Self {
        let bins = spectrum_bins.max(1);
        let hop = spectrum_hop.max(1);
        let waveform_capacity =
            ((sample_rate as f32 * WAVE_MAX_MS / 1000.0).ceil() as usize).max(1);
        let spectrum_capacity = ((sample_rate as f32 * 2.5 / hop as f32).ceil() as usize).max(1);
        Self {
            sample_rate,
            waveform: vec![0.0; waveform_capacity],
            waveform_head: 0,
            waveform_len: 0,
            spectrum_bins: bins,
            spectrum_hop: hop,
            spectrum_capacity,
            spectrum: vec![0.0; spectrum_capacity * bins],
            spectrum_head: 0,
            spectrum_len: 0,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn spectrum_bins(&self) -> usize {
        self.spectrum_bins
    }
    pub fn spectrum_hop(&self) -> usize {
        self.spectrum_hop
    }

    pub fn clear(&mut self) {
        self.waveform_head = 0;
        self.waveform_len = 0;
        self.spectrum_head = 0;
        self.spectrum_len = 0;
    }

    pub fn push_waveform(&mut self, samples: &[f32]) {
        for &sample in samples {
            self.waveform[self.waveform_head] = sample;
            self.waveform_head = (self.waveform_head + 1) % self.waveform.len();
            self.waveform_len = (self.waveform_len + 1).min(self.waveform.len());
        }
    }

    pub fn push_spectrum(&mut self, column: &[f32]) {
        let base = self.spectrum_head * self.spectrum_bins;
        for i in 0..self.spectrum_bins {
            self.spectrum[base + i] = column.get(i).copied().unwrap_or(0.0);
        }
        self.spectrum_head = (self.spectrum_head + 1) % self.spectrum_capacity;
        self.spectrum_len = (self.spectrum_len + 1).min(self.spectrum_capacity);
    }

    /// Copy a chronological last 5–100 ms waveform. A triggered view starts at
    /// a recent rising zero crossing when one is available.
    pub fn waveform_into(&self, out: &mut [f32], window_ms: f32, trigger: bool) {
        if out.is_empty() {
            return;
        }
        let ms = window_ms.clamp(5.0, 100.0);
        let count = ((self.sample_rate as f32 * ms / 1000.0).round() as usize).max(1);
        let available = self.waveform_len;
        let ring_start = self.waveform_head + self.waveform.len() - self.waveform_len;
        let mut start = 0usize;
        let mut triggered = false;
        if trigger && available >= count && count > 1 {
            for rel in (1..=available - count).rev() {
                let prev = self.waveform[(ring_start + rel - 1) % self.waveform.len()];
                let curr = self.waveform[(ring_start + rel) % self.waveform.len()];
                if prev <= 0.0 && curr > 0.0 {
                    start = rel;
                    triggered = true;
                    break;
                }
            }
        }
        let source_start = if triggered {
            start
        } else {
            available.saturating_sub(count)
        };
        let missing = if triggered {
            0
        } else {
            count.saturating_sub(available)
        };
        let out_len = out.len();
        for (i, dst) in out.iter_mut().enumerate() {
            let pos = if out_len == 1 {
                0.0
            } else {
                i as f32 * (count - 1) as f32 / (out_len - 1) as f32
            };
            let conceptual = pos + source_start as f32 - missing as f32;
            if conceptual < 0.0 || conceptual >= available as f32 {
                *dst = 0.0;
            } else {
                let rel = conceptual.floor() as usize;
                let next = (rel + 1).min(available - 1);
                let a = self.waveform[(ring_start + rel) % self.waveform.len()];
                let b = self.waveform[(ring_start + next) % self.waveform.len()];
                *dst = a + (b - a) * conceptual.fract();
            }
        }
    }

    /// Copy a spectrum image in row-major order. Oldest time is left, newest
    /// right, and high frequency is top.
    pub fn spectrum_into(&self, out: &mut [f32], width: usize, height: usize, seconds: f32) {
        if width == 0 || height == 0 {
            return;
        }
        let hops = ((seconds.clamp(0.1, 2.5) * self.sample_rate as f32 / self.spectrum_hop as f32)
            .ceil() as usize)
            .max(1);
        let available = self.spectrum_len.min(hops);
        let missing = hops - available;
        out.fill(0.0);
        for y in 0..height {
            let bin = if height > 1 {
                ((height - 1 - y) * (self.spectrum_bins - 1) / (height - 1))
                    .min(self.spectrum_bins - 1)
            } else {
                self.spectrum_bins - 1
            };
            for x in 0..width {
                let dst = y * width + x;
                if dst >= out.len() {
                    continue;
                }
                let col_rel = if width == 1 {
                    hops - 1
                } else {
                    x * (hops - 1) / (width - 1)
                };
                if col_rel < missing {
                    continue;
                }
                let rel = self.spectrum_len - available + col_rel - missing;
                let col = (self.spectrum_head + self.spectrum_capacity - self.spectrum_len + rel)
                    % self.spectrum_capacity;
                out[dst] = self.spectrum[col * self.spectrum_bins + bin];
            }
        }
    }
}

/// Registry of visual histories keyed by stable audio send id.
#[derive(Default)]
pub struct AudioVisualRegistry {
    histories: AHashMap<AudioSendId, AudioVisualHistory>,
    first_send: Option<AudioSendId>,
}

impl AudioVisualRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// `None` selects the first configured history; an explicit missing id is
    /// intentionally `None` so callers can distinguish a bad binding.
    pub fn get(&self, send: Option<&AudioSendId>) -> Option<&AudioVisualHistory> {
        match send {
            Some(id) => self.histories.get(id),
            None => self
                .first_send
                .as_ref()
                .and_then(|id| self.histories.get(id)),
        }
    }

    pub fn get_mut(&mut self, send: Option<&AudioSendId>) -> Option<&mut AudioVisualHistory> {
        match send {
            Some(id) => self.histories.get_mut(id),
            None => self
                .first_send
                .as_ref()
                .and_then(|id| self.histories.get_mut(id)),
        }
    }

    pub fn set_first_send(&mut self, send: Option<&AudioSendId>) {
        if self.first_send.as_ref() != send {
            self.first_send = send.cloned();
        }
    }

    pub fn ensure(&mut self, send: &AudioSendId, sample_rate: u32, bins: usize, hop: usize) {
        let replace = self.histories.get(send).is_none_or(|h| {
            h.sample_rate() != sample_rate
                || h.spectrum_bins() != bins.max(1)
                || h.spectrum_hop() != hop.max(1)
        });
        if replace {
            self.histories.insert(
                send.clone(),
                AudioVisualHistory::new(sample_rate, bins, hop),
            );
        }
    }

    pub fn remove_unlisted(&mut self, sends: &[AudioSendId]) {
        self.histories
            .retain(|id, _| sends.iter().any(|candidate| candidate == id));
    }

    pub fn clear(&mut self) {
        for history in self.histories.values_mut() {
            history.clear();
        }
    }

    pub fn feed_waveform(&mut self, send: &AudioSendId, samples: &[f32]) {
        if let Some(history) = self.histories.get_mut(send) {
            history.push_waveform(samples);
        }
    }

    pub fn feed_spectrum(&mut self, send: &AudioSendId, column: &[f32]) {
        if let Some(history) = self.histories.get_mut(send) {
            history.push_spectrum(column);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_selects_a_crossing_with_a_complete_window() {
        let mut history = AudioVisualHistory::new(1_000, 2, 10);
        let mut samples = [1.0; 30];
        samples[17] = -1.0;
        samples[18] = 0.25;
        samples[27] = -1.0;
        samples[28] = 0.5;
        history.push_waveform(&samples);
        let mut out = [0.0; 10];
        history.waveform_into(&mut out, 10.0, true);
        assert_eq!(out, samples[18..28]);
        history.waveform_into(&mut out, 10.0, false);
        assert_eq!(out, samples[20..30]);
    }

    #[test]
    fn spectrum_startup_preserves_the_requested_time_window() {
        let mut history = AudioVisualHistory::new(1_000, 2, 100);
        history.push_spectrum(&[1.0, 10.0]);
        history.push_spectrum(&[2.0, 20.0]);
        let mut out = [99.0; 8];
        history.spectrum_into(&mut out, 4, 2, 0.4);
        assert_eq!(out, [0.0, 0.0, 10.0, 20.0, 0.0, 0.0, 1.0, 2.0]);
    }

    #[test]
    fn waveform_startup_is_right_aligned_and_ring_is_bounded() {
        let mut history = AudioVisualHistory::new(1_000, 2, 10);
        history.push_waveform(&[1.0, 2.0, 3.0]);
        let mut out = [9.0; 10];
        history.waveform_into(&mut out, 10.0, false);
        assert!(out[..7].iter().all(|sample| *sample == 0.0));
        assert_eq!(&out[7..], &[1.0, 2.0, 3.0]);
        history.push_waveform(&[4.0; 400]);
        assert_eq!(history.waveform_len, history.waveform.len());
    }

    #[test]
    fn spectrum_is_chronological_and_high_frequency_is_top() {
        let mut history = AudioVisualHistory::new(1_000, 2, 100);
        history.push_spectrum(&[1.0, 10.0]);
        history.push_spectrum(&[2.0, 20.0]);
        let mut out = [0.0; 4];
        history.spectrum_into(&mut out, 2, 2, 0.2);
        assert_eq!(out, [10.0, 20.0, 1.0, 2.0]);
    }

    #[test]
    fn missing_explicit_send_and_first_send_are_distinct() {
        let mut registry = AudioVisualRegistry::new();
        let first = AudioSendId::new("first");
        let second = AudioSendId::new("second");
        registry.set_first_send(Some(&first));
        registry.ensure(&second, 1_000, 2, 10);
        assert!(registry.get(None).is_none());
        assert!(registry.get(Some(&first)).is_none());
        assert!(registry.get(Some(&second)).is_some());
    }

    #[test]
    fn pruning_before_first_history_preserves_default_selection() {
        let mut registry = AudioVisualRegistry::new();
        let first = AudioSendId::new("first");
        registry.set_first_send(Some(&first));
        registry.remove_unlisted(&[]);
        assert!(registry.get(None).is_none());
        registry.ensure(&first, 1_000, 2, 10);
        assert!(registry.get(None).is_some());
    }
}
