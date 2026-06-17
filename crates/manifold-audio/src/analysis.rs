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

/// FFT window for band-energy analysis. At 48 kHz one window is ~21 ms; the
/// feature path slides it by [`HOP_SIZE`] (50% overlap) so a new analysis lands
/// every ~10.7 ms — finer than the 60 fps (~16 ms) content tick, and a transient
/// straddling a window boundary no longer splits its flux across two windows.
const FFT_SIZE: usize = 1024;

/// Hop between successive feature windows — 50% of [`FFT_SIZE`]. Hann satisfies
/// constant-overlap-add at this hop, so the signal is weighted uniformly across
/// frames (no per-hop analysis ripple).
const HOP_SIZE: usize = FFT_SIZE / 2;

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

/// Live Low/Mid/High crossover frequencies, shared content thread → worker.
/// Like [`GainBank`], these are a calibration the performer can ride while
/// watching the spectrogram, so they must apply without glitching the stream:
/// the worker re-splits its analysis bands in place, no capture restart. Global
/// to all sends — Low/Mid/High mean one consistent split everywhere.
pub struct CrossoverBank {
    low_hz: AtomicU32,
    mid_hz: AtomicU32,
}

impl CrossoverBank {
    /// Build from initial crossover frequencies (Hz).
    pub fn new(low_hz: f32, mid_hz: f32) -> Self {
        Self {
            low_hz: AtomicU32::new(low_hz.to_bits()),
            mid_hz: AtomicU32::new(mid_hz.to_bits()),
        }
    }

    /// Set both crossover frequencies (Hz).
    pub fn set(&self, low_hz: f32, mid_hz: f32) {
        self.low_hz.store(low_hz.to_bits(), Ordering::Relaxed);
        self.mid_hz.store(mid_hz.to_bits(), Ordering::Relaxed);
    }

    /// Current `(low_hz, mid_hz)`.
    pub fn get(&self) -> (f32, f32) {
        (
            f32::from_bits(self.low_hz.load(Ordering::Relaxed)),
            f32::from_bits(self.mid_hz.load(Ordering::Relaxed)),
        )
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

/// Read end of the worker's per-column overlay-scalar stream: 2 floats per
/// column, `[centroid_yfb, onset]`, produced in lockstep with [`ColumnReader`]
/// (same column count, same order).
pub struct ScalarReader {
    cons: ringbuf::HeapCons<f32>,
    scratch: [f32; 2],
}

impl ScalarReader {
    /// Pop every complete scalar pair available, calling `f(centroid_yfb, onset)`
    /// in arrival order (oldest → newest).
    pub fn drain(&mut self, mut f: impl FnMut(f32, f32)) {
        while self.cons.occupied_len() >= 2 {
            let got = self.cons.pop_slice(&mut self.scratch);
            if got < 2 {
                break;
            }
            f(self.scratch[0], self.scratch[1]);
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
        crossovers: Arc<CrossoverBank>,
        tap: Arc<SpectrogramTap>,
    ) -> (Self, FeatureReader, ColumnReader, ScalarReader) {
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

        // Overlay-scalar ring: 2 floats per column, sized to match the column
        // ring's column capacity so they never desync.
        let (scalar_prod, scalar_cons) = HeapRb::<f32>::new((2 * COLUMN_RING_COLS).max(1)).split();
        let scalar_reader = ScalarReader { cons: scalar_cons, scratch: [0.0; 2] };

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
                    crossovers,
                    tap,
                    col_prod,
                    scalar_prod,
                    spec_config,
                    num_bins,
                );
                worker.run(&running_thread);
            })
            .expect("spawn audio analysis thread");

        (Self { running, handle: Some(handle) }, reader, column_reader, scalar_reader)
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
    /// Previous window's tilted magnitude spectrum (`0..=nyquist`). One hop back,
    /// 50%-overlapping with the current window; used for the transient flux.
    prev_mags: Vec<f32>,
    /// The window *two* hops back — a full [`FFT_SIZE`] behind the current one, so
    /// disjoint from it. Liveliness diffs against this to keep its
    /// flux-over-a-full-window scaling identical to the pre-overlap path.
    prev2_mags: Vec<f32>,
    /// Per-band running average of flux — the adaptive onset threshold tracks it.
    /// Indexed in `AudioBand` order [Full, Low, Mid, High].
    flux_avg: [f32; 4],
    /// Windows analyzed so far. Flux features are held at 0 until a valid
    /// predecessor exists (≥2 for transients, ≥3 for liveliness's 2-hop diff), so
    /// arming audio-mod doesn't fire a spurious onset off the zero-init spectrum.
    windows_done: u32,
    features: SendFeatures,
}

struct WorkerLoop {
    consumer: AudioConsumer,
    producer: ringbuf::HeapProd<FeatureFrame>,
    device_channels: usize,
    sends: Vec<SendState>,
    /// Live per-send gain, written by the content thread, read here each drain.
    gains: Arc<GainBank>,
    /// Live Low/Mid/High crossovers, read each drain; a change re-splits the
    /// analyzer's bands (no capture restart).
    crossovers: Arc<CrossoverBank>,
    /// Last crossovers applied to the analyzer, to skip the recompute when
    /// unchanged.
    last_crossovers: (f32, f32),
    analyzer: SpectralAnalyzer,
    // ── Spectrogram column producer (one tapped send) ──
    sample_rate: f32,
    spec_config: SpectrogramConfig,
    spec_num_bins: usize,
    /// Which send to produce columns for, `-1` = none. Read from `tap` each drain.
    tap: Arc<SpectrogramTap>,
    column_producer: ringbuf::HeapProd<f32>,
    /// Per-column overlay scalars (2 per column: centroid-height, onset),
    /// produced in lockstep with `column_producer`. Drives the scope's scrolling
    /// centroid trace + transient ticks.
    scalar_producer: ringbuf::HeapProd<f32>,
    /// Previous scope column's magnitudes, for the per-column onset (flux).
    prev_spec_col: Vec<f32>,
    /// Whether `prev_spec_col` holds a real column yet (skips the startup spike).
    spec_has_prev: bool,
    /// Running average of the scope column flux — the onset threshold tracks it.
    spec_flux_avg: f32,
    /// Decaying onset impulse for the scope (per scope column).
    spec_onset: f32,
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
        crossovers: Arc<CrossoverBank>,
        tap: Arc<SpectrogramTap>,
        column_producer: ringbuf::HeapProd<f32>,
        scalar_producer: ringbuf::HeapProd<f32>,
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
                prev2_mags: vec![0.0; FFT_SIZE / 2 + 1],
                flux_avg: [0.0; 4],
                windows_done: 0,
                features: SendFeatures::default(),
            })
            .collect();

        // Start the analyzer at the bank's crossovers so the first window is
        // already split correctly (rather than at the defaults for one drain).
        let (init_low, init_mid) = crossovers.get();
        let mut analyzer = SpectralAnalyzer::new(sample_rate);
        analyzer.set_crossovers(init_low, init_mid);

        Self {
            consumer,
            producer,
            device_channels: device_channels.max(1),
            sends,
            gains,
            crossovers,
            last_crossovers: (init_low, init_mid),
            analyzer,
            sample_rate: sample_rate as f32,
            spec_config,
            spec_num_bins,
            tap,
            column_producer,
            scalar_producer,
            prev_spec_col: vec![0.0; spec_num_bins.max(1)],
            spec_has_prev: false,
            spec_flux_avg: 0.0,
            spec_onset: 0.0,
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

        // Re-split the analysis bands if the crossovers moved (a drag in the
        // Audio Setup scope). Cheap compare-and-skip — the recompute is a few
        // bin lookups, paid only on an actual change.
        let xover = self.crossovers.get();
        if xover != self.last_crossovers {
            self.analyzer.set_crossovers(xover.0, xover.1);
            self.last_crossovers = xover;
        }

        // Which send (if any) feeds the spectrogram this drain. A change resets
        // the rolling window so the scope doesn't splice two sources.
        let tapped = self.tap.selected();
        if tapped != self.spec_tapped {
            self.spec_window.clear();
            self.spec_since_hop = 0;
            self.spec_tapped = tapped;
            // Reset the scope's per-column onset state so a new source doesn't
            // inherit the old one's flux baseline / impulse.
            self.spec_has_prev = false;
            self.spec_flux_avg = 0.0;
            self.spec_onset = 0.0;
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
                    // One FFT, then the same five detectors run on every band.
                    // analyze() applies the perceptual tilt once and returns the
                    // stateless reductions (amplitude/brightness/noisiness) per
                    // band; liveliness/transients are stateful and read the tilted
                    // `mags` the analyzer just left, never a second transform.
                    let reductions = self.analyzer.analyze(&send.accum[..FFT_SIZE]);
                    let mags = &self.analyzer.mags;
                    send.windows_done = send.windows_done.saturating_add(1);
                    let wd = send.windows_done;
                    for (bi, &(lo, hi)) in self.analyzer.band_bins.iter().enumerate() {
                        let bf = &mut send.features.bands[bi];
                        bf.amplitude = reductions[bi].amplitude;
                        bf.brightness = reductions[bi].brightness;
                        bf.noisiness = reductions[bi].noisiness;

                        // Transients: a flux spike (vs the previous, 50%-overlapping
                        // window) above the band's running average fires a unit
                        // impulse; otherwise it decays. Needs one valid predecessor.
                        if wd >= 2 {
                            let (flux, _) = band_flux_energy(mags, &send.prev_mags, lo, hi);
                            let triggered =
                                flux > send.flux_avg[bi] * ONSET_RATIO && flux > ONSET_FLOOR;
                            bf.transients =
                                if triggered { 1.0 } else { bf.transients * ONSET_DECAY };
                            send.flux_avg[bi] += (flux - send.flux_avg[bi]) * FLUX_AVG_COEFF;
                        }

                        // Liveliness (relative flux) self-scales with density. It
                        // diffs the window two hops back — disjoint from the current
                        // one — so its scale matches the pre-overlap full-window flux.
                        if wd >= 3 {
                            let (flux2, energy2) =
                                band_flux_energy(mags, &send.prev2_mags, lo, hi);
                            bf.liveliness = relative_flux(flux2, energy2);
                        }
                    }
                    // Rotate history one hop: prev2 ← prev ← current.
                    send.prev2_mags.copy_from_slice(&send.prev_mags);
                    send.prev_mags.copy_from_slice(mags);

                    // Slide the window by one hop (50% overlap), keeping the tail.
                    send.accum.drain(0..HOP_SIZE);
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

        if self.spec_window.len() < n_fft {
            return;
        }

        // Columns owed since the last emit. Each is a window ending `hop` samples
        // later than the previous, so the oldest one we can form must still have
        // a full `n_fft` behind it — the retained buffer (capped at `n_fft + hop`)
        // bounds how many DISTINCT columns exist. A startup/stall backlog that
        // exceeds that is collapsed: emitting it would re-analyse the same static
        // window and paint a run of identical columns (the left-edge smear), so we
        // drop the columns we have no distinct data for instead of duplicating.
        let owed = self.spec_since_hop / hop;
        let avail = 1 + (self.spec_window.len() - n_fft) / hop;
        let emit = owed.min(avail);
        for j in (0..emit).rev() {
            let end = self.spec_window.len() - j * hop;
            cqt.process_magnitudes(&self.spec_window[end - n_fft..end], &mut self.spec_col);

            // Per-column overlay scalars. Centroid = the magnitude-weighted mean
            // bin, as height-from-bottom (0..1) — VQT bins are geometric, so this
            // is already the log-frequency centre the shader draws. Onset = a
            // flux impulse vs the previous column, thresholded on a running
            // average. Both advance every column (continuity) even if the ring is
            // full and the push below is skipped.
            let (mut num, mut den, mut flux) = (0.0f32, 0.0f32, 0.0f32);
            for (i, &m) in self.spec_col.iter().enumerate() {
                num += i as f32 * m;
                den += m;
                if self.spec_has_prev {
                    flux += (m - self.prev_spec_col[i]).max(0.0);
                }
            }
            let centroid_yfb = if den > 1e-9 && self.spec_num_bins > 1 {
                (num / den / (self.spec_num_bins - 1) as f32).clamp(0.0, 1.0)
            } else {
                -1.0
            };
            if self.spec_has_prev {
                let triggered =
                    flux > self.spec_flux_avg * ONSET_RATIO && flux > ONSET_FLOOR;
                self.spec_onset = if triggered { 1.0 } else { self.spec_onset * ONSET_DECAY };
                self.spec_flux_avg += (flux - self.spec_flux_avg) * FLUX_AVG_COEFF;
            }
            self.prev_spec_col.copy_from_slice(&self.spec_col);
            self.spec_has_prev = true;

            // Whole-column push only, in lockstep with its scalars — if either
            // ring can't fit, drop both (the scope skips a frame) rather than
            // desync the streams.
            if self.column_producer.vacant_len() >= self.spec_num_bins
                && self.scalar_producer.vacant_len() >= 2
            {
                self.column_producer.push_slice(&self.spec_col);
                self.scalar_producer.push_slice(&[centroid_yfb, self.spec_onset]);
            }
        }
        // Consume the whole backlog (including the collapsed remainder) so it
        // doesn't carry forward and re-smear on the next drain.
        self.spec_since_hop -= owed * hop;
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

/// Onset detection (derived from per-band flux). A window whose flux exceeds the
/// band's running average by `ONSET_RATIO` (and clears `ONSET_FLOOR`) fires a
/// unit impulse; between hits the impulse decays by `ONSET_DECAY` per hop, so a
/// transient reads as a snappy spike that settles in ~100 ms. `ONSET_RATIO` is a
/// self-ratio (flux vs its own running average), so it's invariant to the hop;
/// the decay and the averaging coefficient are per-hop, so each is the √ of its
/// old per-window value — at 2× the window rate (one hop ≈ 10.7 ms) that
/// preserves the old 0.6/window ~100 ms settle and 0.1/window ~210 ms averaging.
const ONSET_RATIO: f32 = 1.6;
const ONSET_FLOOR: f32 = 1e-3;
const ONSET_DECAY: f32 = 0.775; // √0.6 — per-hop; matches the old 0.6/window settle
/// Smoothing of the running flux average the onset threshold tracks (per hop).
const FLUX_AVG_COEFF: f32 = 0.05; // 1−√0.9 — per-hop; matches the old 0.1/window
/// Below this band energy, relative flux (liveliness) reads 0 — avoids the
/// flux ÷ energy ratio blowing up on near-silence.
const FLUX_ENERGY_GATE: f32 = 1e-4;

/// dB floor for band-amplitude normalization: this many dB below the reference
/// maps to 0.0, the reference itself to 1.0. Band amplitude is a raw magnitude
/// with no natural ceiling, so it's dB-mapped into 0..1. The per-send input gain
/// is the real calibration knob; this reference is a starting point to tune.
const FEATURE_FLOOR_DB: f32 = -60.0;
/// Band-amplitude RMS value treated as 0 dBFS. Per-bin full-scale magnitude is
/// ~FFT_SIZE/4, so a fully-saturated band sits near there; this is a generous
/// ceiling with headroom (band amplitude is the per-bin RMS, so it's band-size
/// independent — Full and Low read on the same scale).
const BAND_REF: f32 = 8.0;

/// Perceptual tilt applied once to the magnitude spectrum before every reduction
/// (the analysis counterpart of the spectrogram's pink-noise tilt). A `+3 dB/oct`
/// amplitude slope (≈ ×√(f/pivot)) flattens a 1/f spectrum, so highs aren't
/// buried: brightness/amplitude track perceived balance, and noisiness measures
/// flatness relative to pink (the right reference for music) rather than white.
const TILT_SLOPE_DB_PER_OCT: f32 = 3.0;
/// Pivot frequency where the tilt weight is unity.
const TILT_PIVOT_HZ: f32 = 1000.0;

/// One band's stateless reductions from the (tilted) spectrum. Liveliness and
/// transients are stateful and computed per-send by the caller from `mags`.
#[derive(Clone, Copy, Debug, Default)]
struct BandReduction {
    amplitude: f32,
    brightness: f32,
    noisiness: f32,
}

/// Windowed-FFT spectral analyzer. Shared across sends (stateless per call): it
/// computes the magnitude spectrum, applies the perceptual tilt once, then
/// reduces each band to amplitude/brightness/noisiness. The tilted spectrum is
/// left in `mags` so the caller can compute per-band flux against the send's
/// previous spectrum.
struct SpectralAnalyzer {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buffer: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// Inclusive bin ranges in `AudioBand` order: [Full, Low, Mid, High].
    band_bins: [(usize, usize); 4],
    /// Tilted magnitude per bin, `0..=nyquist`. Rewritten every `analyze` call;
    /// read by the caller for per-band flux.
    mags: Vec<f32>,
    /// Center frequency (Hz) per bin, precomputed for centroid + band edges.
    bin_hz: Vec<f32>,
    /// Perceptual tilt weight per bin (applied to `mags`).
    tilt: Vec<f32>,
    nyquist_bin: usize,
    /// Device sample rate (Hz) — needed to remap crossover frequencies to bins
    /// when [`Self::set_crossovers`] is called live.
    sample_rate: f32,
}

/// Inclusive bin ranges in `AudioBand` order [Full, Low, Mid, High] for the
/// given crossover frequencies. Full spans `1..=nyquist` (DC skipped); Low/Mid/
/// High are contiguous sub-ranges split at `low_hz` and `mid_hz`. Shared by
/// construction and live [`SpectralAnalyzer::set_crossovers`] so the two can't
/// drift.
fn band_bins_for(sample_rate: f32, low_hz: f32, mid_hz: f32) -> [(usize, usize); 4] {
    let nyquist_bin = FFT_SIZE / 2;
    let bin_of =
        |hz: f32| ((hz * FFT_SIZE as f32 / sample_rate).round() as usize).clamp(1, nyquist_bin);
    let low_bin = bin_of(low_hz);
    let mid_bin = bin_of(mid_hz).max(low_bin + 1).min(nyquist_bin);
    [
        (1, nyquist_bin),
        (1, low_bin.saturating_sub(1).max(1)),
        (low_bin, mid_bin.saturating_sub(1).max(low_bin)),
        (mid_bin, nyquist_bin),
    ]
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
        let band_bins = band_bins_for(sample_rate as f32, LOW_HZ, MID_HZ);

        let bin_hz: Vec<f32> = (0..=nyquist_bin)
            .map(|b| b as f32 * sample_rate as f32 / FFT_SIZE as f32)
            .collect();

        // Pink tilt: weight = (f/pivot)^k, where k turns the dB/oct slope into an
        // amplitude exponent (20·log10(weight) = slope·log2(f/pivot)). DC → 0.
        let k = TILT_SLOPE_DB_PER_OCT / (20.0 * std::f32::consts::LOG10_2);
        let tilt: Vec<f32> = bin_hz
            .iter()
            .map(|&f| if f > 0.0 { (f / TILT_PIVOT_HZ).powf(k) } else { 0.0 })
            .collect();

        Self {
            fft,
            window,
            buffer: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            scratch: vec![Complex::new(0.0, 0.0); scratch_len],
            band_bins,
            mags: vec![0.0; nyquist_bin + 1],
            bin_hz,
            tilt,
            nyquist_bin,
            sample_rate: sample_rate as f32,
        }
    }

    /// Re-split the Low/Mid/High bands at new crossover frequencies. Cheap (a
    /// few bin lookups); the worker calls it only when the project's crossovers
    /// change, so analysis retunes live with no capture restart. Full band is
    /// unaffected.
    fn set_crossovers(&mut self, low_hz: f32, mid_hz: f32) {
        self.band_bins = band_bins_for(self.sample_rate, low_hz, mid_hz);
    }

    /// Analyze one full window of `FFT_SIZE` mono samples. Fills `self.mags` with
    /// the tilted magnitude spectrum and returns the per-band stateless
    /// reductions; the caller derives liveliness/transients from `self.mags`.
    fn analyze(&mut self, samples: &[f32]) -> [BandReduction; 4] {
        debug_assert_eq!(samples.len(), FFT_SIZE);
        for (i, b) in self.buffer.iter_mut().enumerate() {
            *b = Complex::new(samples[i] * self.window[i], 0.0);
        }
        self.fft.process_with_scratch(&mut self.buffer, &mut self.scratch);

        // Tilted magnitude spectrum (perceptual weight applied once, here, so
        // every downstream reduction sees identical data).
        for bin in 0..=self.nyquist_bin {
            self.mags[bin] = self.buffer[bin].norm() * self.tilt[bin];
        }

        let mut out = [BandReduction::default(); 4];
        for (i, &(lo, hi)) in self.band_bins.iter().enumerate() {
            out[i] = self.reduce_band(lo, hi);
        }
        out
    }

    /// Amplitude / brightness / noisiness over one inclusive bin range of the
    /// tilted spectrum. Amplitude is the per-bin RMS (band-size independent),
    /// dB-normalized; brightness is the log-mapped centroid across the band's own
    /// frequency edges (so it uses the full 0..1 within the band); noisiness is
    /// the geometric÷arithmetic mean (flatness).
    fn reduce_band(&self, lo: usize, hi: usize) -> BandReduction {
        let mut sum_sq = 0.0;
        let mut num = 0.0;
        let mut den = 0.0;
        let mut log_sum = 0.0;
        let mut lin_sum = 0.0;
        for bin in lo..=hi {
            let m = self.mags[bin];
            sum_sq += m * m;
            num += self.bin_hz[bin] * m;
            den += m;
            log_sum += m.max(1e-9).ln();
            lin_sum += m;
        }
        let n = (hi - lo + 1) as f32;

        let amplitude = db_normalize((sum_sq / n).sqrt(), BAND_REF);

        let lo_hz = self.bin_hz[lo].max(1.0);
        let hi_hz = self.bin_hz[hi].max(lo_hz * 1.0001);
        let brightness = if den > 1e-9 {
            let c_hz = (num / den).clamp(lo_hz, hi_hz);
            ((c_hz.ln() - lo_hz.ln()) / (hi_hz.ln() - lo_hz.ln())).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let noisiness = if lin_sum > 1e-9 {
            let geo = (log_sum / n).exp();
            let arith = lin_sum / n;
            (geo / arith).clamp(0.0, 1.0)
        } else {
            0.0
        };

        BandReduction { amplitude, brightness, noisiness }
    }
}

/// Positive spectral flux and current energy over one inclusive bin range.
/// `flux` is the sum of positive bin-to-bin magnitude increases; `energy` is the
/// sum of current magnitudes. Liveliness is [`relative_flux`] of the two; onset
/// detection thresholds `flux` against its per-band running average. One pass
/// shared by the worker loop and the tests.
fn band_flux_energy(cur: &[f32], prev: &[f32], lo: usize, hi: usize) -> (f32, f32) {
    let mut flux = 0.0;
    let mut energy = 0.0;
    for bin in lo..=hi {
        let d = cur[bin] - prev[bin];
        if d > 0.0 {
            flux += d;
        }
        energy += cur[bin];
    }
    (flux, energy)
}

/// Relative flux = flux ÷ energy. Naturally 0..1 (each bin's positive change
/// can't exceed its current value when prev ≥ 0), gated to 0 on near-silence so
/// the ratio doesn't blow up.
fn relative_flux(flux: f32, energy: f32) -> f32 {
    if energy > FLUX_ENERGY_GATE { (flux / energy).clamp(0.0, 1.0) } else { 0.0 }
}

/// Map a raw, unbounded amplitude-like feature to 0..1 on a dB scale: `reference`
/// is treated as 0 dBFS (→ 1.0) and [`FEATURE_FLOOR_DB`] below it as 0.0. Used to
/// bring the band energies and flux into the same 0..1 range as the bounded
/// features.
fn db_normalize(raw: f32, reference: f32) -> f32 {
    if raw <= 1e-9 {
        return 0.0;
    }
    let db = 20.0 * (raw / reference).log10();
    ((db - FEATURE_FLOOR_DB) / -FEATURE_FLOOR_DB).clamp(0.0, 1.0)
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

    // Band index helpers (AudioBand order: Full, Low, Mid, High).
    fn bands() -> (usize, usize, usize, usize) {
        use manifold_core::AudioBand::*;
        (Full.index(), Low.index(), Mid.index(), High.index())
    }

    /// Deterministic pseudo-noise in −1..1 (no rng dependency).
    fn noise(n: usize) -> Vec<f32> {
        let mut state = 0x2545_F491u32;
        (0..n)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn band_amplitude_localizes_a_tone() {
        let (_full, low, mid, high) = bands();
        let mut a = SpectralAnalyzer::new(SR);

        let r = a.analyze(&sine(60.0, FFT_SIZE));
        assert!(
            r[low].amplitude > r[mid].amplitude && r[low].amplitude > r[high].amplitude,
            "60 Hz should dominate the low band"
        );
        let r = a.analyze(&sine(1000.0, FFT_SIZE));
        assert!(
            r[mid].amplitude > r[low].amplitude && r[mid].amplitude > r[high].amplitude,
            "1 kHz should dominate the mid band"
        );
        let r = a.analyze(&sine(6000.0, FFT_SIZE));
        assert!(
            r[high].amplitude > r[low].amplitude && r[high].amplitude > r[mid].amplitude,
            "6 kHz should dominate the high band"
        );
    }

    #[test]
    fn silence_reads_near_zero() {
        let mut a = SpectralAnalyzer::new(SR);
        let r = a.analyze(&vec![0.0; FFT_SIZE]);
        assert!(r.iter().all(|b| b.amplitude < 1e-6), "silence amplitude ~0");
    }

    #[test]
    fn brightness_rises_with_a_higher_tone() {
        let (full, ..) = bands();
        let mut a = SpectralAnalyzer::new(SR);
        let dark = a.analyze(&sine(100.0, FFT_SIZE))[full].brightness;
        let bright = a.analyze(&sine(5000.0, FFT_SIZE))[full].brightness;
        assert!(bright > dark, "5 kHz brighter than 100 Hz: {dark} vs {bright}");
        assert!((0.0..=1.0).contains(&dark) && (0.0..=1.0).contains(&bright), "0..1");
    }

    #[test]
    fn noisiness_separates_tone_from_noise() {
        let (full, ..) = bands();
        let mut a = SpectralAnalyzer::new(SR);
        let tone = a.analyze(&sine(1000.0, FFT_SIZE))[full].noisiness;
        let noisy = a.analyze(&noise(FFT_SIZE))[full].noisiness;
        assert!(noisy > tone, "noise flatter than a tone: {tone} vs {noisy}");
    }

    #[test]
    fn relative_flux_fires_on_change_not_steady_state() {
        let (full, ..) = bands();
        let mut a = SpectralAnalyzer::new(SR);
        let (lo, hi) = a.band_bins[full];
        let silence = vec![0.0; FFT_SIZE / 2 + 1];
        // Energy appearing against silence → near-max relative flux.
        a.analyze(&sine(1000.0, FFT_SIZE));
        let (flux, energy) = band_flux_energy(&a.mags, &silence, lo, hi);
        let onset = relative_flux(flux, energy);
        assert!(onset > 0.5, "energy from silence → high relative flux: {onset}");
        // The same spectrum twice → ~0 change.
        let prev = a.mags.clone();
        a.analyze(&sine(1000.0, FFT_SIZE));
        let (flux2, energy2) = band_flux_energy(&a.mags, &prev, lo, hi);
        let steady = relative_flux(flux2, energy2);
        assert!(steady < 0.1, "steady tone → low relative flux: {steady}");
    }

    #[test]
    fn set_crossovers_moves_band_edges() {
        let mut a = SpectralAnalyzer::new(SR);
        let (_f, _lo, _mid, high_default) = (
            a.band_bins[0],
            a.band_bins[1],
            a.band_bins[2],
            a.band_bins[3],
        );
        // Raise the mid/high split: the High band must start at a higher bin.
        a.set_crossovers(250.0, 6000.0);
        let high_raised = a.band_bins[3];
        assert!(
            high_raised.0 > high_default.0,
            "raising mid_hz should push the High band start up: {} -> {}",
            high_default.0,
            high_raised.0
        );
        // Full band is never touched by crossovers.
        assert_eq!(a.band_bins[0], (1, FFT_SIZE / 2));
        // Degenerate input (low ≥ mid) still yields ordered, non-empty bands.
        a.set_crossovers(5000.0, 1000.0);
        let (lo_a, lo_b) = a.band_bins[1];
        let (mi_a, mi_b) = a.band_bins[2];
        let (hi_a, hi_b) = a.band_bins[3];
        assert!(lo_a <= lo_b && mi_a <= mi_b && hi_a <= hi_b, "bands stay ordered");
    }

    #[test]
    fn transients_localize_to_their_band() {
        // The kick-detector premise: a low thump appearing produces far more flux
        // in the low band than the high band, so Transients@Low fires on it while
        // Transients@High stays quiet.
        let (_full, low, _mid, high) = bands();
        let mut a = SpectralAnalyzer::new(SR);
        a.analyze(&vec![0.0; FFT_SIZE]); // silence baseline
        let prev = a.mags.clone();
        a.analyze(&sine(60.0, FFT_SIZE)); // a 60 Hz thump appears
        let (lo_a, lo_b) = a.band_bins[low];
        let (hi_a, hi_b) = a.band_bins[high];
        let (low_flux, _) = band_flux_energy(&a.mags, &prev, lo_a, lo_b);
        let (high_flux, _) = band_flux_energy(&a.mags, &prev, hi_a, hi_b);
        assert!(
            low_flux > high_flux * 4.0,
            "60 Hz thump flux: low {low_flux} should ≫ high {high_flux}"
        );
    }

    #[test]
    fn worker_end_to_end_produces_band_features() {
        // Build a capture-style ring, fill it with a 1 kHz mono tone, run the
        // worker, and confirm a frame arrives with energy in the mid band.
        let cap = SR as usize; // 1 s headroom
        let (mut prod, cons) = HeapRb::<f32>::new(cap).split();
        let tone = sine(1000.0, FFT_SIZE * 4);
        let pushed = prod.push_slice(&tone);
        assert_eq!(pushed, tone.len());

        let sends = vec![SendSpec { channels: vec![0] }];
        let gains = Arc::new(GainBank::new(&[1.0]));
        let crossovers = Arc::new(CrossoverBank::new(250.0, 2000.0));
        let tap = Arc::new(SpectrogramTap::default());
        let (mut worker, mut reader, _columns, _scalars) = AudioFeatureWorker::spawn(
            cons,
            SR,
            /* device_channels */ 1,
            sends,
            gains,
            crossovers,
            tap,
        );

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
        let (full, low, mid, high) = bands();
        assert!(
            s.bands[mid].amplitude > s.bands[low].amplitude
                && s.bands[mid].amplitude > s.bands[high].amplitude,
            "1 kHz tone should land in the mid band: {:?}",
            s.bands.map(|b| b.amplitude),
        );
        // Full-band amplitude is present and inside 0..1.
        assert!(
            s.bands[full].amplitude > 0.0 && s.bands[full].amplitude <= 1.0,
            "full-band amplitude in 0..1: {}",
            s.bands[full].amplitude,
        );
    }
}
