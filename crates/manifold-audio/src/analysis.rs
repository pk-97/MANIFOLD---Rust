//! Audio feature analysis — the off-RT worker that turns captured samples
//! into per-send **feature frames** for audio modulation.
//!
//! See `docs/AUDIO_MODULATION_DESIGN.md`. This is step 1 of the build order:
//! the isolated, unit-testable analysis core. It depends on nothing in the
//! workspace except [`crate::capture`] — no `manifold-core`, no app, no GPU.
//!
//! # Shape
//!
//! ```text
//! capture ring (f32, interleaved) ─drain→ AudioFeatureWorker ─frames→ FeatureReader
//!   (cpal RT thread fills it)              (this worker thread)         (content thread)
//! ```
//!
//! The worker owns the capture ring's [`AudioConsumer`](crate::capture::AudioConsumer),
//! deinterleaves it, downmixes each configured **send** to mono, and runs that
//! send's feature extractors. Frames are published latest-wins through a second
//! SPSC `ringbuf` — no `Arc<Mutex>`, no locks on the read path.
//!
//! ## Send identity
//!
//! This module keys everything by **send index** (position in the `sends` slice
//! passed to [`AudioFeatureWorker::spawn`]), NOT by the project's `AudioSendId`.
//! The id↔index mapping is the wiring layer's job (`ContentPipeline`), which
//! keeps this crate free of `manifold-core` types and keeps the analysis core
//! testable in isolation.
//!
//! ## v1 features
//!
//! Only **band energy** (3 perceptual bands) today. The frame struct and worker
//! are built around a feature *seam*: adding onset/pitch later is a new field on
//! [`SendFeatures`] plus an extractor in the worker loop — no plumbing change.
//!
//! [`SendFeatures`] is defined in `manifold-core` (the modulation evaluator
//! reads it without depending on this audio/CoreAudio stack); the worker fills
//! it here.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use ringbuf::HeapRb;
use ringbuf::traits::{Consumer as ConsumerTrait, Observer as ObserverTrait, Producer as ProducerTrait, Split};
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

pub use manifold_core::audio_features::SendFeatures;

use crate::capture::AudioConsumer;

/// FFT window for band-energy analysis. Non-overlapping; at 48 kHz one window
/// is ~21 ms, comfortably finer than the 60 fps (~16 ms) content tick.
const FFT_SIZE: usize = 1024;

/// Upper edge of the low band / lower edge of the mid band (Hz).
const LOW_HZ: f32 = 250.0;
/// Upper edge of the mid band / lower edge of the high band (Hz).
const MID_HZ: f32 = 2000.0;

/// Maximum number of sends the worker tracks. Keeps [`FeatureFrame`] `Copy` and
/// alloc-free so it can ride the SPSC ring without per-frame heap churn.
pub const MAX_SENDS: usize = 16;

/// Output ring depth (feature frames). The worker produces ~1 frame / 21 ms and
/// the content thread drains every tick, so this is generous headroom; on the
/// rare full ring the newest frame is dropped (next arrives ~21 ms later).
const OUTPUT_RING_CAPACITY: usize = 16;

/// One send's routing + analysis config, as the worker needs it. The project's
/// `AudioSend` (in `manifold-core`) is resolved down to this at the wiring layer.
#[derive(Clone, Debug, Default)]
pub struct SendSpec {
    /// Device input channels to downmix to mono for this send. Out-of-range
    /// channels (≥ device channel count) are ignored at runtime.
    pub channels: Vec<u16>,
}

/// A snapshot of every send's features at one analysis instant. `Copy` and
/// fixed-size so it rides the SPSC ring with no allocation.
#[derive(Clone, Copy, Debug)]
pub struct FeatureFrame {
    sends: [SendFeatures; MAX_SENDS],
    count: usize,
    /// Monotonic frame counter — lets a reader tell a fresh frame from a repeat.
    pub seq: u64,
}

impl FeatureFrame {
    /// Features for send `index`, or `None` if out of range.
    pub fn send(&self, index: usize) -> Option<SendFeatures> {
        (index < self.count).then(|| self.sends[index])
    }

    /// Number of sends carried by this frame.
    pub fn count(&self) -> usize {
        self.count
    }
}

/// The content-thread end of the feature stream. Holds the SPSC consumer plus a
/// cache of the last frame seen, so a tick with no new frame still reports the
/// most recent value (modulation holds, it doesn't drop to zero).
pub struct FeatureReader {
    cons: ringbuf::HeapCons<FeatureFrame>,
    last: Option<FeatureFrame>,
}

impl FeatureReader {
    /// Drain all pending frames, keeping the newest, and return it. After the
    /// first frame ever seen, always returns `Some` (the cached latest).
    pub fn latest(&mut self) -> Option<FeatureFrame> {
        while let Some(frame) = self.cons.try_pop() {
            self.last = Some(frame);
        }
        self.last
    }
}

/// Spawns and owns the analysis worker thread. Stops the thread on `stop()` or
/// drop.
pub struct AudioFeatureWorker {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl AudioFeatureWorker {
    /// Spawn the worker. Takes ownership of the capture ring's `consumer`, the
    /// device `sample_rate` and `device_channels` (for deinterleaving), and the
    /// `sends` to analyze. Returns the worker handle and the [`FeatureReader`]
    /// for the content thread.
    ///
    /// Sends beyond [`MAX_SENDS`] are dropped with a warning.
    pub fn spawn(
        consumer: AudioConsumer,
        sample_rate: u32,
        device_channels: u16,
        mut sends: Vec<SendSpec>,
    ) -> (Self, FeatureReader) {
        if sends.len() > MAX_SENDS {
            log::warn!(
                "[AudioAnalysis] {} sends exceeds MAX_SENDS={MAX_SENDS}; extra dropped",
                sends.len(),
            );
            sends.truncate(MAX_SENDS);
        }

        let (prod, cons) = HeapRb::<FeatureFrame>::new(OUTPUT_RING_CAPACITY).split();
        let reader = FeatureReader { cons, last: None };

        let running = Arc::new(AtomicBool::new(true));
        let running_thread = running.clone();

        let handle = std::thread::Builder::new()
            .name("manifold-audio-analysis".into())
            .spawn(move || {
                let mut worker = WorkerLoop::new(
                    consumer,
                    prod,
                    sample_rate,
                    device_channels as usize,
                    sends,
                );
                worker.run(&running_thread);
            })
            .expect("spawn audio analysis thread");

        (Self { running, handle: Some(handle) }, reader)
    }

    /// Stop the worker thread and join it. Idempotent.
    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for AudioFeatureWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

// ── Worker loop internals ───────────────────────────────────────────────

/// Per-send running state on the worker thread.
struct SendState {
    channels: Vec<u16>,
    /// Samples accumulating toward the next FFT window.
    accum: Vec<f32>,
    features: SendFeatures,
}

struct WorkerLoop {
    consumer: AudioConsumer,
    producer: ringbuf::HeapProd<FeatureFrame>,
    device_channels: usize,
    sends: Vec<SendState>,
    analyzer: BandEnergyAnalyzer,
    /// Leftover interleaved samples that didn't complete a frame last drain.
    carry: Vec<f32>,
    /// Reusable drain buffer.
    drain_buf: Vec<f32>,
    seq: u64,
}

impl WorkerLoop {
    fn new(
        consumer: AudioConsumer,
        producer: ringbuf::HeapProd<FeatureFrame>,
        sample_rate: u32,
        device_channels: usize,
        specs: Vec<SendSpec>,
    ) -> Self {
        let sends = specs
            .into_iter()
            .map(|s| SendState {
                channels: s.channels,
                accum: Vec::with_capacity(FFT_SIZE),
                features: SendFeatures::default(),
            })
            .collect();

        Self {
            consumer,
            producer,
            device_channels: device_channels.max(1),
            sends,
            analyzer: BandEnergyAnalyzer::new(sample_rate),
            carry: Vec::with_capacity(FFT_SIZE),
            drain_buf: vec![0.0; 4096],
            seq: 0,
        }
    }

    fn run(&mut self, running: &AtomicBool) {
        while running.load(Ordering::Acquire) {
            let produced = self.drain_and_analyze();
            if !produced {
                // Nothing new; back off briefly so we don't spin a core.
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// Drain everything available, analyze complete windows, emit a frame if any
    /// send updated. Returns whether a frame was produced.
    fn drain_and_analyze(&mut self) -> bool {
        let available = self.consumer.occupied_len();
        if available == 0 && self.carry.is_empty() {
            return false;
        }

        // Pull the carry forward, then drain the ring onto it.
        let mut samples = std::mem::take(&mut self.carry);
        let mut remaining = available;
        while remaining > 0 {
            let n = remaining.min(self.drain_buf.len());
            let popped = self.consumer.pop_slice(&mut self.drain_buf[..n]);
            if popped == 0 {
                break;
            }
            samples.extend_from_slice(&self.drain_buf[..popped]);
            remaining -= popped;
        }

        let ch = self.device_channels;
        let usable = (samples.len() / ch) * ch;
        let mut updated = false;

        for frame in samples[..usable].chunks_exact(ch) {
            for send in &mut self.sends {
                let mono = downmix(frame, &send.channels);
                send.accum.push(mono);
                if send.accum.len() >= FFT_SIZE {
                    // Overall level: RMS of the raw (unwindowed) block, 0..1.
                    send.features.amplitude = block_rms(&send.accum[..FFT_SIZE]);
                    send.features.band_energy = self.analyzer.analyze(&send.accum[..FFT_SIZE]);
                    send.accum.clear();
                    updated = true;
                }
            }
        }

        // Stash the partial frame remainder for next drain.
        self.carry.clear();
        self.carry.extend_from_slice(&samples[usable..]);

        if updated {
            self.emit_frame();
        }
        updated
    }

    fn emit_frame(&mut self) {
        let mut sends = [SendFeatures::default(); MAX_SENDS];
        for (i, s) in self.sends.iter().enumerate() {
            sends[i] = s.features;
        }
        self.seq += 1;
        let frame = FeatureFrame { sends, count: self.sends.len(), seq: self.seq };
        // Latest-wins: on a full ring we drop this frame; the reader drains to
        // the newest each tick and another frame follows in ~21 ms.
        let _ = self.producer.try_push(frame);
    }
}

/// RMS level of one block of mono samples. For input in [-1, 1] this is in
/// 0..1 by construction — the natural "overall amplitude" of the block.
fn block_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|&s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

/// Downmix the channels of one interleaved frame to a single mono sample
/// (mean of the selected channels). Out-of-range channels are skipped.
fn downmix(frame: &[f32], channels: &[u16]) -> f32 {
    let mut acc = 0.0;
    let mut n = 0u32;
    for &c in channels {
        if let Some(&s) = frame.get(c as usize) {
            acc += s;
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { acc / n as f32 }
}

/// Windowed-FFT band energy: splits one FFT window into low/mid/high bands and
/// returns an amplitude-like scalar per band.
struct BandEnergyAnalyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buffer: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Inclusive bin ranges for [low, mid, high].
    band_bins: [(usize, usize); 3],
}

impl BandEnergyAnalyzer {
    fn new(sample_rate: u32) -> Self {
        let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let scratch_len = fft.get_inplace_scratch_len();

        // Hann window.
        let window = (0..FFT_SIZE)
            .map(|i| {
                let x = std::f32::consts::PI * 2.0 * i as f32 / FFT_SIZE as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();

        let nyquist_bin = FFT_SIZE / 2;
        let bin_of = |hz: f32| {
            ((hz * FFT_SIZE as f32 / sample_rate as f32).round() as usize).clamp(1, nyquist_bin)
        };
        let low_bin = bin_of(LOW_HZ);
        let mid_bin = bin_of(MID_HZ);
        // Skip the DC bin (0). Bands are contiguous, inclusive ranges.
        let band_bins = [
            (1, low_bin.saturating_sub(1).max(1)),
            (low_bin, mid_bin.saturating_sub(1).max(low_bin)),
            (mid_bin, nyquist_bin),
        ];

        Self {
            fft,
            window,
            buffer: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            scratch: vec![Complex::new(0.0, 0.0); scratch_len],
            band_bins,
        }
    }

    /// Analyze one full window of `FFT_SIZE` mono samples.
    fn analyze(&mut self, samples: &[f32]) -> [f32; 3] {
        debug_assert_eq!(samples.len(), FFT_SIZE);
        for (i, b) in self.buffer.iter_mut().enumerate() {
            *b = Complex::new(samples[i] * self.window[i], 0.0);
        }
        self.fft.process_with_scratch(&mut self.buffer, &mut self.scratch);

        let mut out = [0.0f32; 3];
        for (band, &(lo, hi)) in self.band_bins.iter().enumerate() {
            let mut sum = 0.0;
            for bin in lo..=hi {
                sum += self.buffer[bin].norm_sqr();
            }
            // Amplitude-like: RMS of the band magnitude over the window length.
            out[band] = (sum / FFT_SIZE as f32).sqrt();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48_000;

    fn sine(freq: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / SR as f32).sin())
            .collect()
    }

    #[test]
    fn block_rms_is_normalized_0_to_1() {
        assert_eq!(block_rms(&[0.0; 64]), 0.0);
        // Constant full-scale → RMS = 1.0 (the ceiling).
        assert!((block_rms(&[1.0; 64]) - 1.0).abs() < 1e-6);
        // Full-scale sine → RMS ≈ 1/√2 ≈ 0.707, comfortably inside 0..1.
        let r = block_rms(&sine(1000.0, FFT_SIZE));
        assert!((r - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.02, "got {r}");
    }

    #[test]
    fn downmix_averages_selected_channels() {
        // 2ch interleaved frame: ch0 = 1.0, ch1 = 0.0.
        let frame = [1.0, 0.0];
        assert_eq!(downmix(&frame, &[0]), 1.0);
        assert_eq!(downmix(&frame, &[1]), 0.0);
        assert_eq!(downmix(&frame, &[0, 1]), 0.5);
        // Out-of-range channel is skipped.
        assert_eq!(downmix(&frame, &[0, 7]), 1.0);
        // No valid channels → silence.
        assert_eq!(downmix(&frame, &[9]), 0.0);
    }

    #[test]
    fn band_energy_localizes_a_tone() {
        let mut a = BandEnergyAnalyzer::new(SR);

        let low = a.analyze(&sine(60.0, FFT_SIZE));
        assert!(low[0] > low[1] && low[0] > low[2], "60 Hz should dominate low band: {low:?}");

        let mid = a.analyze(&sine(1000.0, FFT_SIZE));
        assert!(mid[1] > mid[0] && mid[1] > mid[2], "1 kHz should dominate mid band: {mid:?}");

        let high = a.analyze(&sine(6000.0, FFT_SIZE));
        assert!(high[2] > high[0] && high[2] > high[1], "6 kHz should dominate high band: {high:?}");
    }

    #[test]
    fn silence_reads_near_zero() {
        let mut a = BandEnergyAnalyzer::new(SR);
        let e = a.analyze(&vec![0.0; FFT_SIZE]);
        assert!(e.iter().all(|&v| v < 1e-6), "silence should be ~0: {e:?}");
    }

    #[test]
    fn worker_end_to_end_produces_band_energy() {
        // Build a capture-style ring, fill it with a 1 kHz mono tone, run the
        // worker, and confirm a frame arrives with energy in the mid band.
        let cap = SR as usize; // 1 s headroom
        let (mut prod, cons) = HeapRb::<f32>::new(cap).split();
        let tone = sine(1000.0, FFT_SIZE * 4);
        let pushed = prod.push_slice(&tone);
        assert_eq!(pushed, tone.len());

        let sends = vec![SendSpec { channels: vec![0] }];
        let (mut worker, mut reader) =
            AudioFeatureWorker::spawn(cons, SR, /* device_channels */ 1, sends);

        // Poll up to ~500 ms for the first frame.
        let mut frame = None;
        for _ in 0..250 {
            if let Some(f) = reader.latest() {
                frame = Some(f);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        worker.stop();

        let frame = frame.expect("worker should produce a feature frame");
        let s = frame.send(0).expect("send 0 present");
        assert!(
            s.band_energy[1] > s.band_energy[0] && s.band_energy[1] > s.band_energy[2],
            "1 kHz tone should land in mid band: {:?}",
            s.band_energy,
        );
        // Overall amplitude is the normalized RMS — a full-scale tone reads
        // ≈0.707, always inside 0..1 (unlike the unnormalized band energy).
        assert!(
            s.amplitude > 0.5 && s.amplitude <= 1.0,
            "full-scale tone RMS in 0..1: {}",
            s.amplitude,
        );
    }
}
