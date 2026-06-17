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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use ringbuf::HeapRb;
use ringbuf::traits::{Consumer as ConsumerTrait, Observer as ObserverTrait, Producer as ProducerTrait, Split};
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

pub use manifold_core::audio_features::SendFeatures;
use manifold_spectral::{CqtTransform, SpectrogramConfig};

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

/// Live per-send input gain (linear multiplier), shared content thread → worker.
///
/// Lock-free: the content thread writes a slot on a gain edit, the worker reads
/// it each drain. Sized to the send count at capture (re)build, so a *gain-only*
/// edit updates it in place with **no capture restart** — gain is a calibration
/// knob the performer rides while watching the meter, so it must not glitch the
/// stream. Structural changes (channels / device) rebuild capture and mint a
/// fresh bank (see the wiring layer's `CaptureSignature`).
pub struct GainBank {
    /// One linear gain per send, stored as `f32` bits in an atomic.
    linear: Vec<AtomicU32>,
}

impl GainBank {
    /// Build a bank from initial linear gains (one per send, in send order).
    pub fn new(linear_gains: &[f32]) -> Self {
        Self {
            linear: linear_gains.iter().map(|g| AtomicU32::new(g.to_bits())).collect(),
        }
    }

    /// Set send `index`'s linear gain. Out-of-range index is ignored.
    pub fn set_linear(&self, index: usize, gain: f32) {
        if let Some(slot) = self.linear.get(index) {
            slot.store(gain.to_bits(), Ordering::Relaxed);
        }
    }

    /// Send `index`'s current linear gain, or unity (1.0) if out of range.
    pub fn get_linear(&self, index: usize) -> f32 {
        self.linear
            .get(index)
            .map(|slot| f32::from_bits(slot.load(Ordering::Relaxed)))
            .unwrap_or(1.0)
    }

    /// Number of send slots.
    pub fn len(&self) -> usize {
        self.linear.len()
    }

    pub fn is_empty(&self) -> bool {
        self.linear.is_empty()
    }
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

/// How many columns of headroom the worker→UI column ring holds. The UI drains
/// every frame; this only matters if the UI stalls (then oldest columns drop).
const COLUMN_RING_COLS: usize = 128;

/// Live control for the spectrogram tap, shared content thread → worker. The
/// content thread sets which send (by index) the worker should produce VQT
/// columns for; `-1` = none (no spectrogram work). Lock-free, like [`GainBank`].
pub struct SpectrogramTap {
    selected: AtomicI32,
}

impl Default for SpectrogramTap {
    fn default() -> Self {
        Self { selected: AtomicI32::new(-1) }
    }
}

impl SpectrogramTap {
    /// Select the send (by worker index) to produce columns for, or `None` to
    /// stop. The worker resets its rolling window when this changes.
    pub fn set_selected(&self, send_index: Option<usize>) {
        self.selected
            .store(send_index.map(|i| i as i32).unwrap_or(-1), Ordering::Relaxed);
    }

    fn selected(&self) -> i32 {
        self.selected.load(Ordering::Relaxed)
    }
}

/// Read end of the worker's VQT column stream. Each column is `num_bins`
/// magnitudes (geometrically-spaced bins). The producer pushes whole columns
/// only, so the ring is always a whole number of columns.
pub struct ColumnReader {
    cons: ringbuf::HeapCons<f32>,
    num_bins: usize,
    scratch: Vec<f32>,
}

impl ColumnReader {
    /// Bin count per column (the spectrogram renderer must match it).
    pub fn num_bins(&self) -> usize {
        self.num_bins
    }

    /// Pop every complete column available, calling `f` with each in arrival
    /// order (oldest → newest). No-op if `num_bins == 0`.
    pub fn drain_columns(&mut self, mut f: impl FnMut(&[f32])) {
        if self.num_bins == 0 {
            return;
        }
        while self.cons.occupied_len() >= self.num_bins {
            let got = self.cons.pop_slice(&mut self.scratch);
            if got < self.num_bins {
                break; // shouldn't happen (whole-column pushes), but stay safe
            }
            f(&self.scratch[..self.num_bins]);
        }
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
        gains: Arc<GainBank>,
        tap: Arc<SpectrogramTap>,
    ) -> (Self, FeatureReader, ColumnReader) {
        if sends.len() > MAX_SENDS {
            log::warn!(
                "[AudioAnalysis] {} sends exceeds MAX_SENDS={MAX_SENDS}; extra dropped",
                sends.len(),
            );
            sends.truncate(MAX_SENDS);
        }

        let (prod, cons) = HeapRb::<FeatureFrame>::new(OUTPUT_RING_CAPACITY).split();
        let reader = FeatureReader { cons, last: None };

        // Spectrogram column ring. Bin count is fixed by config + sample rate,
        // so the ring and reader agree without a built transform.
        let spec_config = SpectrogramConfig::default();
        let num_bins = spec_config.num_bins(sample_rate as f32);
        let (col_prod, col_cons) =
            HeapRb::<f32>::new((num_bins * COLUMN_RING_COLS).max(1)).split();
        let column_reader = ColumnReader {
            cons: col_cons,
            num_bins,
            scratch: vec![0.0; num_bins.max(1)],
        };

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
                    gains,
                    tap,
                    col_prod,
                    spec_config,
                    num_bins,
                );
                worker.run(&running_thread);
            })
            .expect("spawn audio analysis thread");

        (Self { running, handle: Some(handle) }, reader, column_reader)
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
    /// Linear input gain, refreshed from the [`GainBank`] once per drain.
    gain: f32,
    /// Samples accumulating toward the next FFT window.
    accum: Vec<f32>,
    /// Previous window's magnitude spectrum (`0..=nyquist`), for spectral flux.
    prev_mags: Vec<f32>,
    /// Running average of spectral flux — the adaptive onset threshold tracks it.
    flux_avg: f32,
    features: SendFeatures,
}

struct WorkerLoop {
    consumer: AudioConsumer,
    producer: ringbuf::HeapProd<FeatureFrame>,
    device_channels: usize,
    sends: Vec<SendState>,
    /// Live per-send gain, written by the content thread, read here each drain.
    gains: Arc<GainBank>,
    analyzer: SpectralAnalyzer,
    // ── Spectrogram column producer (one tapped send) ──
    sample_rate: f32,
    spec_config: SpectrogramConfig,
    spec_num_bins: usize,
    /// Which send to produce columns for, `-1` = none. Read from `tap` each drain.
    tap: Arc<SpectrogramTap>,
    column_producer: ringbuf::HeapProd<f32>,
    /// Built lazily on the first active tap (kernel construction is one FFT per
    /// bin — paid once, only if the spectrogram is ever opened).
    cqt: Option<CqtTransform>,
    /// Rolling post-gain mono window of the tapped send (newest at the end).
    spec_window: Vec<f32>,
    /// Samples accumulated since the last column was emitted.
    spec_since_hop: usize,
    /// Last-seen tap, to detect a selection change (resets the window).
    spec_tapped: i32,
    /// Reusable per-column magnitude scratch (`spec_num_bins` long).
    spec_col: Vec<f32>,
    /// Leftover interleaved samples that didn't complete a frame last drain.
    /// Retains its allocation across drains (never moved out), so stashing the
    /// remainder each tick is realloc-free in steady state.
    carry: Vec<f32>,
    /// Persistent per-drain work buffer (carry-over + freshly drained samples).
    /// Swapped out and back so the analysis loop borrows a local, not `self`.
    work: Vec<f32>,
    /// Reusable drain buffer.
    drain_buf: Vec<f32>,
    seq: u64,
}

impl WorkerLoop {
    #[allow(clippy::too_many_arguments)]
    fn new(
        consumer: AudioConsumer,
        producer: ringbuf::HeapProd<FeatureFrame>,
        sample_rate: u32,
        device_channels: usize,
        specs: Vec<SendSpec>,
        gains: Arc<GainBank>,
        tap: Arc<SpectrogramTap>,
        column_producer: ringbuf::HeapProd<f32>,
        spec_config: SpectrogramConfig,
        spec_num_bins: usize,
    ) -> Self {
        let sends = specs
            .into_iter()
            .map(|s| SendState {
                channels: s.channels,
                gain: 1.0,
                accum: Vec::with_capacity(FFT_SIZE),
                prev_mags: vec![0.0; FFT_SIZE / 2 + 1],
                flux_avg: 0.0,
                features: SendFeatures::default(),
            })
            .collect();

        Self {
            consumer,
            producer,
            device_channels: device_channels.max(1),
            sends,
            gains,
            analyzer: SpectralAnalyzer::new(sample_rate),
            sample_rate: sample_rate as f32,
            spec_config,
            spec_num_bins,
            tap,
            column_producer,
            cqt: None,
            spec_window: Vec::new(),
            spec_since_hop: 0,
            spec_tapped: -1,
            spec_col: vec![0.0; spec_num_bins.max(1)],
            carry: Vec::with_capacity(FFT_SIZE),
            work: Vec::with_capacity(4096 + FFT_SIZE),
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

        // Move the carry-over into the persistent work buffer (`carry` keeps its
        // allocation), then drain the ring onto it. `work` is taken out so the
        // analysis loop below borrows a local, not `self`.
        let mut work = std::mem::take(&mut self.work);
        work.clear();
        work.extend_from_slice(&self.carry);
        self.carry.clear();

        let mut remaining = available;
        while remaining > 0 {
            let n = remaining.min(self.drain_buf.len());
            let popped = self.consumer.pop_slice(&mut self.drain_buf[..n]);
            if popped == 0 {
                break;
            }
            work.extend_from_slice(&self.drain_buf[..popped]);
            remaining -= popped;
        }

        let ch = self.device_channels;
        let usable = (work.len() / ch) * ch;
        let mut updated = false;

        // Refresh each send's live gain once per drain (the bank is lock-free;
        // a gain edit lands here without a capture restart). Disjoint fields, so
        // the mutable `sends` borrow and the `gains` read don't conflict.
        for (i, send) in self.sends.iter_mut().enumerate() {
            send.gain = self.gains.get_linear(i);
        }

        // Which send (if any) feeds the spectrogram this drain. A change resets
        // the rolling window so the scope doesn't splice two sources.
        let tapped = self.tap.selected();
        if tapped != self.spec_tapped {
            self.spec_window.clear();
            self.spec_since_hop = 0;
            self.spec_tapped = tapped;
        }

        for frame in work[..usable].chunks_exact(ch) {
            for (i, send) in self.sends.iter_mut().enumerate() {
                let mono = downmix(frame, &send.channels) * send.gain;
                if i as i32 == tapped {
                    self.spec_window.push(mono);
                    self.spec_since_hop += 1;
                }
                send.accum.push(mono);
                if send.accum.len() >= FFT_SIZE {
                    // Overall level: RMS of the raw (unwindowed) block, 0..1.
                    send.features.amplitude = block_rms(&send.accum[..FFT_SIZE]);
                    // One FFT, many reductions: bands/centroid/flatness come back
                    // directly; flux/onset are stateful and read the magnitude
                    // spectrum the analyzer just left in `mags`, never a second
                    // transform. See docs/AUDIO_INFRASTRUCTURE.md §8.
                    let sf = self.analyzer.analyze(&send.accum[..FFT_SIZE]);
                    send.features.band_energy = sf.band_energy;
                    send.features.centroid = sf.centroid;
                    send.features.flatness = sf.flatness;

                    let flux = spectral_flux(&self.analyzer.mags, &send.prev_mags);
                    send.prev_mags.copy_from_slice(&self.analyzer.mags);
                    send.features.flux = flux;

                    // Onset: a flux spike above the running average fires a unit
                    // impulse; otherwise the previous impulse decays.
                    let triggered = flux > send.flux_avg * ONSET_RATIO && flux > ONSET_FLOOR;
                    send.features.onset =
                        if triggered { 1.0 } else { send.features.onset * ONSET_DECAY };
                    send.flux_avg += (flux - send.flux_avg) * FLUX_AVG_COEFF;

                    send.accum.clear();
                    updated = true;
                }
            }
        }

        // Emit spectrogram columns for the tapped send (post-gain mono).
        if tapped >= 0 {
            self.produce_spectrogram_columns();
        }

        // Stash the partial frame remainder for next drain. `carry` kept its
        // allocation, so this is realloc-free once warmed.
        self.carry.extend_from_slice(&work[usable..]);
        // Return the work buffer for reuse next drain.
        self.work = work;

        if updated {
            self.emit_frame();
        }
        updated
    }

    /// Run the VQT over the rolling window every `hop` samples, pushing each
    /// resulting magnitude column to the worker→UI ring. Builds the transform
    /// lazily on first use (kernel construction is one FFT per bin).
    fn produce_spectrogram_columns(&mut self) {
        let cqt = self
            .cqt
            .get_or_insert_with(|| self.spec_config.build_transform(self.sample_rate));
        let n_fft = cqt.n_fft();
        let hop = self.spec_config.hop.max(1);

        // Bound the rolling window: keep at most one full window plus one hop of
        // lookahead, so `drain` is realloc-free in steady state.
        let cap = n_fft + hop;
        if self.spec_window.len() > cap {
            let excess = self.spec_window.len() - cap;
            self.spec_window.drain(0..excess);
        }

        while self.spec_since_hop >= hop && self.spec_window.len() >= n_fft {
            let start = self.spec_window.len() - n_fft;
            cqt.process_magnitudes(&self.spec_window[start..], &mut self.spec_col);
            // Whole-column push only — if the ring can't fit a full column, drop
            // it (the scope skips a frame) rather than desync the stream.
            if self.column_producer.vacant_len() >= self.spec_num_bins {
                self.column_producer.push_slice(&self.spec_col);
            }
            self.spec_since_hop -= hop;
        }
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

/// Spectral centroid normalization range (Hz) — centroid is mapped log onto
/// 0..1 across this span so "brightness" reads the same at any sample rate.
const CENTROID_LO_HZ: f32 = 50.0;
const CENTROID_HI_HZ: f32 = 8000.0;

/// Onset detection (derived from spectral flux). A window whose flux exceeds the
/// running average by `ONSET_RATIO` (and clears `ONSET_FLOOR`) fires a unit
/// impulse; between hits the impulse decays by `ONSET_DECAY` per window
/// (~21 ms), so a transient reads as a snappy spike that settles in ~100 ms.
const ONSET_RATIO: f32 = 1.6;
const ONSET_FLOOR: f32 = 1e-3;
const ONSET_DECAY: f32 = 0.6;
/// Smoothing of the running flux average the onset threshold tracks.
const FLUX_AVG_COEFF: f32 = 0.1;

/// One window's stateless spectral reductions, all from the single FFT the
/// analyzer runs. Flux/onset need the previous spectrum and are computed
/// per-send by the caller from [`SpectralAnalyzer::mags`].
#[derive(Clone, Copy, Debug, Default)]
struct SpectralFrame {
    band_energy: [f32; 3],
    centroid: f32,
    flatness: f32,
}

/// Windowed-FFT spectral analyzer. Shared across sends (stateless per call): it
/// computes the magnitude spectrum once and reduces it to band energy, centroid
/// and flatness. The magnitude spectrum is left in `mags` so the caller can
/// compute per-send spectral flux against that send's previous spectrum.
struct SpectralAnalyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buffer: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Inclusive bin ranges for [low, mid, high].
    band_bins: [(usize, usize); 3],
    /// Magnitude per bin, `0..=nyquist`. Rewritten every `analyze` call; read by
    /// the caller for spectral flux.
    mags: Vec<f32>,
    /// Center frequency (Hz) per bin, precomputed for the centroid sum.
    bin_hz: Vec<f32>,
    nyquist_bin: usize,
}

impl SpectralAnalyzer {
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

        let bin_hz = (0..=nyquist_bin)
            .map(|b| b as f32 * sample_rate as f32 / FFT_SIZE as f32)
            .collect();

        Self {
            fft,
            window,
            buffer: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            scratch: vec![Complex::new(0.0, 0.0); scratch_len],
            band_bins,
            mags: vec![0.0; nyquist_bin + 1],
            bin_hz,
            nyquist_bin,
        }
    }

    /// Analyze one full window of `FFT_SIZE` mono samples. Fills `self.mags` and
    /// returns the stateless reductions; the caller derives flux/onset from
    /// `self.mags`.
    fn analyze(&mut self, samples: &[f32]) -> SpectralFrame {
        debug_assert_eq!(samples.len(), FFT_SIZE);
        for (i, b) in self.buffer.iter_mut().enumerate() {
            *b = Complex::new(samples[i] * self.window[i], 0.0);
        }
        self.fft.process_with_scratch(&mut self.buffer, &mut self.scratch);

        // Magnitude spectrum, 0..=nyquist.
        for bin in 0..=self.nyquist_bin {
            self.mags[bin] = self.buffer[bin].norm();
        }

        // Band energy: RMS of the band magnitude over the window length.
        let mut band_energy = [0.0f32; 3];
        for (band, &(lo, hi)) in self.band_bins.iter().enumerate() {
            let mut sum = 0.0;
            for bin in lo..=hi {
                sum += self.mags[bin] * self.mags[bin];
            }
            band_energy[band] = (sum / FFT_SIZE as f32).sqrt();
        }

        // Centroid (magnitude-weighted mean frequency) and flatness (geometric /
        // arithmetic mean), both over the non-DC bins from one pass.
        let mut num = 0.0;
        let mut den = 0.0;
        let mut log_sum = 0.0;
        let mut lin_sum = 0.0;
        for bin in 1..=self.nyquist_bin {
            let m = self.mags[bin];
            num += self.bin_hz[bin] * m;
            den += m;
            log_sum += m.max(1e-9).ln();
            lin_sum += m;
        }
        let n = self.nyquist_bin as f32; // bins 1..=nyquist
        let centroid = if den > 1e-9 {
            let c_hz = (num / den).clamp(CENTROID_LO_HZ, CENTROID_HI_HZ);
            (c_hz.ln() - CENTROID_LO_HZ.ln()) / (CENTROID_HI_HZ.ln() - CENTROID_LO_HZ.ln())
        } else {
            0.0
        };
        let flatness = if lin_sum > 1e-9 {
            let geo = (log_sum / n).exp();
            let arith = lin_sum / n;
            (geo / arith).clamp(0.0, 1.0)
        } else {
            0.0
        };

        SpectralFrame { band_energy, centroid, flatness }
    }
}

/// Spectral flux: the sum of positive bin-to-bin magnitude increases between the
/// current and previous spectra. Unnormalized (like band energy); the shaper's
/// sensitivity scales it. On the first window prev is all-zero, so this returns
/// the rectified spectrum — which correctly reads as the initial onset.
fn spectral_flux(cur: &[f32], prev: &[f32]) -> f32 {
    cur.iter().zip(prev).map(|(&c, &p)| (c - p).max(0.0)).sum()
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
    fn gain_bank_set_get_and_out_of_range() {
        let bank = GainBank::new(&[1.0, 2.0]);
        assert_eq!(bank.len(), 2);
        assert_eq!(bank.get_linear(0), 1.0);
        assert_eq!(bank.get_linear(1), 2.0);
        bank.set_linear(0, 0.5);
        assert_eq!(bank.get_linear(0), 0.5);
        // Out-of-range read is unity; out-of-range write is ignored.
        assert_eq!(bank.get_linear(9), 1.0);
        bank.set_linear(9, 4.0);
        assert_eq!(bank.get_linear(9), 1.0);
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
        let mut a = SpectralAnalyzer::new(SR);

        let low = a.analyze(&sine(60.0, FFT_SIZE)).band_energy;
        assert!(low[0] > low[1] && low[0] > low[2], "60 Hz should dominate low band: {low:?}");

        let mid = a.analyze(&sine(1000.0, FFT_SIZE)).band_energy;
        assert!(mid[1] > mid[0] && mid[1] > mid[2], "1 kHz should dominate mid band: {mid:?}");

        let high = a.analyze(&sine(6000.0, FFT_SIZE)).band_energy;
        assert!(high[2] > high[0] && high[2] > high[1], "6 kHz should dominate high band: {high:?}");
    }

    #[test]
    fn silence_reads_near_zero() {
        let mut a = SpectralAnalyzer::new(SR);
        let e = a.analyze(&vec![0.0; FFT_SIZE]).band_energy;
        assert!(e.iter().all(|&v| v < 1e-6), "silence should be ~0: {e:?}");
    }

    #[test]
    fn centroid_rises_with_brightness() {
        let mut a = SpectralAnalyzer::new(SR);
        let dark = a.analyze(&sine(100.0, FFT_SIZE)).centroid;
        let bright = a.analyze(&sine(5000.0, FFT_SIZE)).centroid;
        assert!(bright > dark, "5 kHz should read brighter than 100 Hz: {dark} vs {bright}");
        assert!((0.0..=1.0).contains(&dark) && (0.0..=1.0).contains(&bright), "normalized 0..1");
    }

    #[test]
    fn flatness_separates_tone_from_noise() {
        let mut a = SpectralAnalyzer::new(SR);
        let tone = a.analyze(&sine(1000.0, FFT_SIZE)).flatness;
        // Deterministic pseudo-noise (no rng dep): an LCG mapped to -1..1.
        let mut state = 0x2545_F491u32;
        let noise: Vec<f32> = (0..FFT_SIZE)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
            })
            .collect();
        let noisy = a.analyze(&noise).flatness;
        assert!(noisy > tone, "noise should be flatter than a tone: {tone} vs {noisy}");
    }

    #[test]
    fn flux_fires_on_change_not_on_steady_state() {
        let mut a = SpectralAnalyzer::new(SR);
        let silence = vec![0.0; FFT_SIZE / 2 + 1];
        // Energy appearing against silence produces flux.
        let _ = a.analyze(&sine(1000.0, FFT_SIZE));
        let onset_flux = spectral_flux(&a.mags, &silence);
        assert!(onset_flux > 0.0, "energy appearing should produce flux: {onset_flux}");
        // The same spectrum twice → little positive change.
        let prev = a.mags.clone();
        let _ = a.analyze(&sine(1000.0, FFT_SIZE));
        let steady = spectral_flux(&a.mags, &prev);
        assert!(steady < onset_flux * 0.1, "steady tone should have little flux: {steady} vs {onset_flux}");
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
        let gains = Arc::new(GainBank::new(&[1.0]));
        let tap = Arc::new(SpectrogramTap::default());
        let (mut worker, mut reader, _columns) =
            AudioFeatureWorker::spawn(cons, SR, /* device_channels */ 1, sends, gains, tap);

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
