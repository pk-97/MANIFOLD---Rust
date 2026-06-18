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

pub use manifold_core::audio_features::SendFeatures;
use manifold_spectral::{CqtTransform, SpectrogramConfig};

use crate::capture::AudioConsumer;

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

/// Overlay scalars per spectrogram column: four per-band centroid heights
/// `[centroid_full, centroid_low, centroid_mid, centroid_high]` followed by the
/// three per-band onset impulses `[onset_low, onset_mid, onset_high]`. The shader
/// reads the same stride.
const SCOPE_SCALAR_STRIDE: usize = 7;

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

/// Read end of the worker's per-column overlay-scalar stream:
/// [`SCOPE_SCALAR_STRIDE`] floats per column, `[centroid_full, centroid_low,
/// centroid_mid, centroid_high, onset_low, onset_mid, onset_high]`, produced in
/// lockstep with [`ColumnReader`] (same column count, same order).
pub struct ScalarReader {
    cons: ringbuf::HeapCons<f32>,
    scratch: [f32; SCOPE_SCALAR_STRIDE],
}

impl ScalarReader {
    /// Pop every complete scalar record available, calling
    /// `f([centroid_full, centroid_low, centroid_mid, centroid_high],
    /// [onset_low, onset_mid, onset_high])` in arrival order (oldest → newest).
    pub fn drain(&mut self, mut f: impl FnMut([f32; 4], [f32; 3])) {
        while self.cons.occupied_len() >= SCOPE_SCALAR_STRIDE {
            let got = self.cons.pop_slice(&mut self.scratch);
            if got < SCOPE_SCALAR_STRIDE {
                break;
            }
            f(
                [self.scratch[0], self.scratch[1], self.scratch[2], self.scratch[3]],
                [self.scratch[4], self.scratch[5], self.scratch[6]],
            );
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

        // Overlay-scalar ring: SCOPE_SCALAR_STRIDE floats per column, sized to
        // match the column ring's column capacity so they never desync.
        let (scalar_prod, scalar_cons) =
            HeapRb::<f32>::new((SCOPE_SCALAR_STRIDE * COLUMN_RING_COLS).max(1)).split();
        let scalar_reader = ScalarReader { cons: scalar_cons, scratch: [0.0; SCOPE_SCALAR_STRIDE] };

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

/// Per-send running state on the worker thread. Every send now runs the same
/// variable-Q transform the scope draws (one shared [`CqtTransform`]); features
/// and the spectrogram are the *same* analysis, so "what you see is what
/// modulates" holds per send.
struct SendState {
    channels: Vec<u16>,
    /// Linear input gain, refreshed from the [`GainBank`] once per drain.
    gain: f32,
    /// Rolling post-gain mono window, newest at the end. The VQT runs on its tail
    /// (zero-padded until it fills, so features fade in over one window rather
    /// than blacking out) every hop.
    window: Vec<f32>,
    /// Post-gain mono samples accumulated since the last VQT hop.
    since_hop: usize,
    /// Latest *tilted* VQT column (per-bin pink weight applied, matching the
    /// scope's colourmap), `num_bins` long. The domain every reduction reads.
    col: Vec<f32>,
    /// Previous column (one hop back) — the transient/liveliness flux baseline.
    prev_col: Vec<f32>,
    /// Hops remaining in each band's onset refractory window — after a transient
    /// fires, suppress re-fire until this elapses. Order [Full, Low, Mid, High].
    transient_refractory: [u8; 4],
    /// Per-band running mean of band energy — the onset detector's adaptive
    /// baseline. A transient is energy spiking above this by `ONSET_RATIO`, so the
    /// trigger self-calibrates to the music's level instead of a fixed threshold.
    energy_avg: [f32; 4],
    /// Whether `prev_col` holds a real column yet (skips the startup flux spike).
    has_prev: bool,
    /// Per-band spectral-centroid height-from-bottom (0..1) for the scope overlay,
    /// indexed [Full, Low, Mid, High]; `-1` when the band isn't loud enough to
    /// characterise. Mirrors the `brightness` feature's gating, but mapped to the
    /// global display y so each band's centroid draws within its own region.
    centroid_yfb: [f32; 4],
    features: SendFeatures,
}

struct WorkerLoop {
    consumer: AudioConsumer,
    producer: ringbuf::HeapProd<FeatureFrame>,
    device_channels: usize,
    sends: Vec<SendState>,
    /// Live per-send gain, written by the content thread, read here each drain.
    gains: Arc<GainBank>,
    /// Live Low/Mid/High crossovers, read each drain; a change re-splits every
    /// send's bands (no capture restart).
    crossovers: Arc<CrossoverBank>,
    /// Last crossovers applied, to skip the band-edge recompute when unchanged.
    last_crossovers: (f32, f32),
    /// The one variable-Q transform every send shares — the same transform the
    /// scope draws. Its kernels are read-only; the worker is single-threaded, so
    /// its FFT scratch is reused for each send in turn.
    cqt: CqtTransform,
    sample_rate: f32,
    spec_config: SpectrogramConfig,
    spec_num_bins: usize,
    /// VQT window length (`spec_config.n_fft`) and hop, cached.
    n_fft: usize,
    hop: usize,
    /// Per-bin pink-tilt weight (linear multiplier) matching the scope's
    /// colourmap. Applied to the raw VQT magnitudes so every reduction — and the
    /// presence gate — reads the exact tilted dB the user sees. `num_bins` long.
    tilt_w: Vec<f32>,
    /// Cached band edges (VQT bin indices) for the current crossovers: Low =
    /// `0..low_bin`, Mid = `low_bin..mid_bin`, High = `mid_bin..num_bins`.
    low_bin: usize,
    mid_bin: usize,
    // ── Spectrogram column producer (the tapped send drives the scope) ──
    /// Which send to produce columns for, `-1` = none. Read from `tap` each drain.
    tap: Arc<SpectrogramTap>,
    column_producer: ringbuf::HeapProd<f32>,
    /// Per-column overlay scalars ([`SCOPE_SCALAR_STRIDE`] per column: a centroid
    /// height per band [Full/Low/Mid/High] + a per-band onset for Low/Mid/High),
    /// in lockstep with `column_producer`. The onsets ARE the tapped send's
    /// Low/Mid/High transients, so the ticks and the modulation source are
    /// identical.
    scalar_producer: ringbuf::HeapProd<f32>,
    /// Last-seen tap, to detect a selection change (resets the tapped send).
    spec_tapped: i32,
    /// `n_fft` scratch for the VQT input (window tail, zero-padded at the front
    /// while the window fills, so features fade in over one window).
    vqt_in: Vec<f32>,
    /// `num_bins` scratch for the raw (untilted) VQT magnitudes — pushed to the
    /// scope ring as-is, since the shader applies the display tilt itself.
    vqt_raw: Vec<f32>,
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
        let nb = spec_num_bins.max(1);
        let n_fft = spec_config.n_fft;
        let hop = spec_config.hop.max(1);
        let sends = specs
            .into_iter()
            .map(|s| SendState {
                channels: s.channels,
                gain: 1.0,
                window: Vec::with_capacity(n_fft + WINDOW_BACKLOG_HOPS * hop),
                since_hop: 0,
                col: vec![0.0; nb],
                prev_col: vec![0.0; nb],
                transient_refractory: [0; 4],
                energy_avg: [0.0; 4],
                has_prev: false,
                centroid_yfb: [-1.0; 4],
                features: SendFeatures::default(),
            })
            .collect();

        let sr = sample_rate as f32;
        let cqt = spec_config.build_transform(sr);
        let tilt_w = tilt_weights(&spec_config, sr, nb);
        let (init_low, init_mid) = crossovers.get();
        let (low_bin, mid_bin) = band_edges(&spec_config, sr, nb, init_low, init_mid);

        Self {
            consumer,
            producer,
            device_channels: device_channels.max(1),
            sends,
            gains,
            crossovers,
            last_crossovers: (init_low, init_mid),
            cqt,
            sample_rate: sr,
            spec_config,
            spec_num_bins,
            n_fft,
            hop,
            tilt_w,
            low_bin,
            mid_bin,
            tap,
            column_producer,
            scalar_producer,
            spec_tapped: -1,
            vqt_in: vec![0.0; n_fft],
            vqt_raw: vec![0.0; nb],
            carry: Vec::with_capacity(4096),
            work: Vec::with_capacity(4096 + n_fft),
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
        // a gain edit lands here without a capture restart).
        for (i, send) in self.sends.iter_mut().enumerate() {
            send.gain = self.gains.get_linear(i);
        }

        // Re-split the bands if the crossovers moved (a drag in the Audio Setup
        // scope). Cheap compare-and-skip — a couple of bin lookups, paid only on
        // an actual change. The same edges feed every send and the scope.
        let xover = self.crossovers.get();
        if xover != self.last_crossovers {
            let (lo, mi) =
                band_edges(&self.spec_config, self.sample_rate, self.spec_num_bins.max(1), xover.0, xover.1);
            self.low_bin = lo;
            self.mid_bin = mi;
            self.last_crossovers = xover;
        }

        // Which send feeds the scope. Each send owns its own window + feature
        // state, so a tap change just redirects the column stream — no splice
        // reset needed (the new source's columns are already its own).
        let tapped = self.tap.selected();
        self.spec_tapped = tapped;

        // Accumulate post-gain mono into each send's rolling window.
        for frame in work[..usable].chunks_exact(ch) {
            for send in self.sends.iter_mut() {
                let mono = downmix(frame, &send.channels) * send.gain;
                send.window.push(mono);
                send.since_hop += 1;
            }
        }

        // Run the owed VQT hops per send. `sends` is taken out so the per-send
        // loop can borrow the shared transform, scratch, and scope producers on
        // `self` without aliasing.
        let mut sends = std::mem::take(&mut self.sends);
        for (i, send) in sends.iter_mut().enumerate() {
            if self.process_send_hops(send, i as i32 == tapped) {
                updated = true;
            }
        }
        self.sends = sends;

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

    /// Run any VQT hops one send has accumulated, updating its five per-band
    /// features from the same variable-Q column the scope draws. When `tapped`,
    /// also push the raw column + onset scalars to the scope rings — the scalars
    /// ARE this send's Low/Mid/High transients, so the red ticks and the
    /// modulation source are one and the same. Returns whether a hop was run.
    fn process_send_hops(&mut self, send: &mut SendState, tapped: bool) -> bool {
        let n_fft = self.n_fft;
        let hop = self.hop;
        let nb = self.spec_num_bins.max(1);

        // Bound the window to one full window plus a small backlog, so `drain` is
        // realloc-free and a brief stall doesn't lose distinct columns.
        let cap = n_fft + WINDOW_BACKLOG_HOPS * hop;
        if send.window.len() > cap {
            let excess = send.window.len() - cap;
            send.window.drain(0..excess);
        }

        let owed = send.since_hop / hop;
        if owed == 0 {
            return false;
        }
        // Distinct columns we can actually form. Before the window fills we still
        // emit one (zero-padded) so features fade in over a window rather than
        // blacking out; a stall backlog beyond what we retain is collapsed.
        let avail = if send.window.len() >= n_fft {
            1 + (send.window.len() - n_fft) / hop
        } else {
            1
        };
        let emit = owed.min(avail);
        let db_min = self.spec_config.db_min;
        let db_max = self.spec_config.db_max;

        for j in (0..emit).rev() {
            // Window slice ending `j` hops before the newest sample, zero-padded
            // at the front while the window is still filling.
            let end = send.window.len().saturating_sub(j * hop);
            let start = end.saturating_sub(n_fft);
            // Form the tilted VQT column (the exact column the scope draws) from
            // the window slice, then reduce all five features per band off it.
            // Shared with the offline analyzer so features are bit-for-bit the
            // same path. Disjoint borrows: `send.window` (read) vs `send.col`
            // (write) are distinct fields, so this single call type-checks.
            form_tilted_column(
                &send.window[start..end],
                &mut self.cqt,
                &self.tilt_w,
                &mut self.vqt_in,
                &mut self.vqt_raw,
                &mut send.col,
            );
            reduce_send(send, nb, self.low_bin, self.mid_bin, db_min, db_max);
            send.prev_col.copy_from_slice(&send.col);
            // Flux features only arm once the window has actually filled — a
            // zero-padded warm-up column differs from the next as content fills
            // in, and that ramp must not read as a transient.
            send.has_prev = send.window.len() >= n_fft;

            // The tapped send drives the scope: push the raw column (the shader
            // applies its own display tilt) and the overlay scalars — the four
            // per-band centroid traces (Full/Low/Mid/High, each drawn within its
            // own region) plus this send's Low/Mid/High transients as the ticks.
            if tapped
                && self.column_producer.vacant_len() >= self.spec_num_bins
                && self.scalar_producer.vacant_len() >= SCOPE_SCALAR_STRIDE
            {
                self.column_producer.push_slice(&self.vqt_raw);
                self.scalar_producer.push_slice(&[
                    send.centroid_yfb[0],
                    send.centroid_yfb[1],
                    send.centroid_yfb[2],
                    send.centroid_yfb[3],
                    send.features.bands[1].transients,
                    send.features.bands[2].transients,
                    send.features.bands[3].transients,
                ]);
            }
        }
        // Consume the whole backlog (including any collapsed remainder).
        send.since_hop -= owed * hop;
        true
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

/// Form the tilted variable-Q column for `seg` (a window slice no longer than
/// `vqt_in`, right-aligned and zero-padded at the front) into `out_col`, using
/// the shared transform + per-bin pink-tilt weights. This is the exact column
/// the live worker reduces and the scope draws; the offline analyzer
/// ([`OfflineSendAnalyzer`]) calls the same function so an audio layer's
/// precomputed feature curve is bit-for-bit identical to what the live path
/// would produce ("what you see is what modulates"). On return, `vqt_raw` holds
/// the untilted magnitudes (the scope pushes those as-is). `seg.len()` must be
/// `<= vqt_in.len()`.
fn form_tilted_column(
    seg: &[f32],
    cqt: &mut CqtTransform,
    tilt_w: &[f32],
    vqt_in: &mut [f32],
    vqt_raw: &mut [f32],
    out_col: &mut [f32],
) {
    let pad = vqt_in.len().saturating_sub(seg.len());
    for v in vqt_in[..pad].iter_mut() {
        *v = 0.0;
    }
    vqt_in[pad..].copy_from_slice(seg);
    cqt.process_magnitudes(vqt_in, vqt_raw);
    for (c, (&raw, &w)) in out_col.iter_mut().zip(vqt_raw.iter().zip(tilt_w.iter())) {
        *c = raw * w;
    }
}

// ── Onset (transient) detection ──────────────────────────────────────────
//
// Simple, deterministic, and self-calibrating: a transient is band energy
// spiking above its OWN running mean by `ONSET_RATIO`. The mean tracks the
// music's level, so the trigger adapts automatically instead of fighting a
// fixed threshold that's right for one song and wrong for the next — a kick
// jumps well above the recent mean whether the track is quiet or loud; a steady
// or held note sits at the mean (ratio ≈ 1). The refractory both debounces the
// multi-hop attack and outlasts the mean's catch-up, so a held note fires once
// (the ratio curve crosses back under `ONSET_RATIO` at a fixed hop count,
// independent of level). Energy is the SUM over the band, so a kick isn't
// diluted the way band-RMS amplitude was.

/// How far band energy must exceed its running mean to fire. THE sensitivity
/// knob — lower catches softer kicks, higher is stricter.
const ONSET_RATIO: f32 = 1.4;
/// Per-hop smoothing of the energy mean (~50 ms at hop ≈ 5.3 ms): slow enough
/// that a kick spikes above it, fast enough that a held note settles within the
/// refractory.
const ENERGY_AVG_COEFF: f32 = 0.1;
/// A transient only fires on a band loud enough to matter — `amplitude` is the
/// band's level on the colourmap's 0..1 dB scale (≈ −53 dBFS here).
const ONSET_AMP_GATE: f32 = 0.12;
/// Per-hop decay of the transient impulse (~100 ms settle at hop ≈ 5.3 ms).
const ONSET_DECAY: f32 = 0.85;
/// Below this band energy, relative flux reads 0 — avoids the flux ÷ energy ratio
/// blowing up on near-silence.
const FLUX_ENERGY_GATE: f32 = 1e-4;
/// Refractory after an onset (~74 ms at hop ≈ 5.3 ms). Debounces the attack AND
/// outlasts the energy mean's catch-up on a held note (the ratio drops back under
/// `ONSET_RATIO` by ~13 hops regardless of level), so a held note fires once.
/// Caps the rate at ~13/s — ample for any kick pattern.
const ONSET_REFRACTORY_HOPS: u8 = 14;
/// Pink-tilt slope the scope colourmap applies (dB/oct). Must stay equal to
/// `manifold_spectral`'s `PINK_SLOPE_DB_PER_OCT` so the worker's reductions and
/// presence gate evaluate the exact tilted dB the user sees painted.
const SCOPE_TILT_DB_PER_OCT: f32 = 3.0;
/// Hops of rolling-window backlog each send keeps beyond one full window, so a
/// brief drain stall doesn't drop distinct columns. ~85 ms at hop ≈ 5.3 ms.
const WINDOW_BACKLOG_HOPS: usize = 16;

/// VQT band edges (bin indices) for the Low/Mid/High split at the given
/// crossovers. VQT bins are geometric — `bin(f) = bpo·log2(f/fmin)` — so this is
/// the same mapping the scope draws its divider lines with, which is why the
/// bands the user sees and the bands that modulate are identical. Returns
/// `(low_bin, mid_bin)`: Low = `0..low_bin`, Mid = `low_bin..mid_bin`, High =
/// `mid_bin..num_bins`.
fn band_edges(
    cfg: &SpectrogramConfig,
    _sample_rate: f32,
    num_bins: usize,
    low_hz: f32,
    mid_hz: f32,
) -> (usize, usize) {
    let nb = num_bins.max(1);
    let fmin = cfg.fmin.max(1.0);
    let bpo = cfg.bpo as f32;
    let bin_of = |hz: f32| {
        ((bpo * (hz / fmin).max(1e-6).log2()).round() as i64).clamp(1, nb as i64 - 1) as usize
    };
    let low_bin = bin_of(low_hz).min(nb.saturating_sub(2).max(1));
    let mid_bin = bin_of(mid_hz).max(low_bin + 1).min(nb.saturating_sub(1));
    (low_bin, mid_bin)
}

/// Per-bin pink-tilt weight (a linear magnitude multiplier) matching the scope
/// colourmap. The shader adds `slope · log2(fmax/fmin) · (0.5 − uv.y)` dB,
/// centred over the displayed range; as a multiplier that's `10^(tilt_db/20)`.
/// Bin `k` sits at `uv.y = 1 − k/(nb−1)`, so `0.5 − uv.y = k/(nb−1) − 0.5`.
/// Applying it once to the raw magnitudes makes every reduction — and the
/// presence gate — read the same tilted dB the user sees.
fn tilt_weights(cfg: &SpectrogramConfig, sample_rate: f32, num_bins: usize) -> Vec<f32> {
    let nb = num_bins.max(1);
    let fmin = cfg.fmin.max(1.0);
    let flr = (cfg.effective_fmax(sample_rate) / fmin).log2();
    let inv = if nb > 1 { 1.0 / (nb - 1) as f32 } else { 0.0 };
    (0..nb)
        .map(|k| {
            let tilt_db = SCOPE_TILT_DB_PER_OCT * flr * (k as f32 * inv - 0.5);
            10.0f32.powf(tilt_db / 20.0)
        })
        .collect()
}

/// One band's reductions from a tilted VQT column plus the previous column.
struct BandReduce {
    amplitude: f32,
    brightness: f32,
    noisiness: f32,
    /// Positive spectral flux vs the previous column (liveliness input).
    flux: f32,
    /// Sum of current magnitudes (liveliness denominator).
    energy: f32,
}

/// Reduce one half-open bin range `[lo, hi)` of a tilted VQT column to a band's
/// amplitude / brightness / noisiness, plus the peak / flux / energy the stateful
/// features need. Amplitude maps the band's RMS through the colourmap's own dB
/// window (`db_min`…`db_max`), so a band's level reads on the same 0..1 scale it
/// is painted with.
fn band_reduce(col: &[f32], prev: &[f32], lo: usize, hi: usize, db_min: f32, db_max: f32) -> BandReduce {
    let hi = hi.min(col.len());
    if lo >= hi {
        return BandReduce {
            amplitude: 0.0,
            brightness: 0.0,
            noisiness: 0.0,
            flux: 0.0,
            energy: 0.0,
        };
    }
    let mut sum_sq = 0.0f32;
    let mut num = 0.0f32;
    let mut den = 0.0f32;
    let mut log_sum = 0.0f32;
    let mut lin_sum = 0.0f32;
    let mut flux = 0.0f32;
    let mut energy = 0.0f32;
    for k in lo..hi {
        let m = col[k];
        sum_sq += m * m;
        num += k as f32 * m;
        den += m;
        log_sum += m.max(1e-9).ln();
        lin_sum += m;
        let d = m - prev[k];
        if d > 0.0 {
            flux += d;
        }
        energy += m;
    }
    let n = (hi - lo) as f32;

    // Amplitude: band RMS, mapped through the colourmap's own dB window.
    let rms = (sum_sq / n).sqrt();
    let amplitude = if rms > 1e-9 {
        ((20.0 * rms.log10() - db_min) / (db_max - db_min)).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Brightness: log-frequency centroid within the band (geometric bins → the
    // bin-index centroid is already log-frequency), spread across the band 0..1.
    let brightness = if den > 1e-9 && hi > lo + 1 {
        let c = (num / den).clamp(lo as f32, (hi - 1) as f32);
        ((c - lo as f32) / (hi - 1 - lo) as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Noisiness: spectral flatness (geometric ÷ arithmetic mean).
    let noisiness = if lin_sum > 1e-9 {
        let geo = (log_sum / n).exp();
        let arith = lin_sum / n;
        (geo / arith).clamp(0.0, 1.0)
    } else {
        0.0
    };

    BandReduce { amplitude, brightness, noisiness, flux, energy }
}

/// Reduce one send's current tilted column into its five per-band features, using
/// the previous column for the flux-based ones. Band order is [Full, Low, Mid,
/// High]. Every feature that DESCRIBES the band — brightness / noisiness /
/// liveliness / transients — reads 0 unless the band is `loud` (level above
/// `ONSET_AMP_GATE`), so none of them characterises faint, near-noise-floor
/// content; amplitude is the honest level and is always reported. Flux/onset
/// features only run once a real predecessor exists ([`SendState::has_prev`], set
/// only after the window has filled), so neither arming nor warm-up fires a
/// spurious onset.
fn reduce_send(
    send: &mut SendState,
    num_bins: usize,
    low_bin: usize,
    mid_bin: usize,
    db_min: f32,
    db_max: f32,
) {
    let bands = [
        (0, num_bins),       // Full
        (0, low_bin),        // Low
        (low_bin, mid_bin),  // Mid
        (mid_bin, num_bins), // High
    ];
    let have_prev = send.has_prev;
    for (bi, &(lo, hi)) in bands.iter().enumerate() {
        let r = band_reduce(&send.col, &send.prev_col, lo, hi, db_min, db_max);
        // `loud` (band level above a musical floor) means "worth characterising."
        // Every feature that DESCRIBES the band gates on it, so none reports on
        // faint, near-noise-floor content (a noise floor reads as maximally
        // flat/noisy, a tiny energy wobble reads as high liveliness, etc.).
        let loud = r.amplitude > ONSET_AMP_GATE;
        let bf = &mut send.features.bands[bi];
        bf.amplitude = r.amplitude;
        bf.brightness = if loud { r.brightness } else { 0.0 };
        bf.noisiness = if loud { r.noisiness } else { 0.0 };

        // Scope overlay: the same centroid the `brightness` feature reads, but
        // mapped from band-local 0..1 onto the global display y so each band's
        // trace draws within its own region. Hidden (`-1`) on a non-loud band,
        // matching the feature reading 0 there.
        send.centroid_yfb[bi] = if loud && hi > lo + 1 {
            let c = lo as f32 + r.brightness * (hi - 1 - lo) as f32;
            (c / (num_bins.max(1) - 1) as f32).clamp(0.0, 1.0)
        } else {
            -1.0
        };

        // Energy mean tracks every hop — including warm-up — so firing arms with
        // a settled baseline (`avg` is the PAST mean; the current energy is
        // compared to it, then folded in).
        let avg = send.energy_avg[bi];
        send.energy_avg[bi] = avg + (r.energy - avg) * ENERGY_AVG_COEFF;

        if have_prev {
            // Liveliness self-scales with density (relative flux).
            bf.liveliness = if loud { relative_flux(r.flux, r.energy) } else { 0.0 };

            // Transient: band energy spiking above its running mean by ONSET_RATIO
            // on a loud band. A kick jumps well above the mean; a steady note sits
            // at it. The refractory debounces the attack and outlasts the mean's
            // catch-up, so a held note fires once.
            let refractory = &mut send.transient_refractory[bi];
            let triggered =
                loud && *refractory == 0 && avg > 1e-9 && r.energy > avg * ONSET_RATIO;
            if triggered {
                bf.transients = 1.0;
                *refractory = ONSET_REFRACTORY_HOPS;
            } else {
                bf.transients *= ONSET_DECAY;
                *refractory = refractory.saturating_sub(1);
            }
        }
    }
}

/// Relative flux = flux ÷ energy. Naturally 0..1 (each bin's positive change
/// can't exceed its current value when prev ≥ 0), gated to 0 on near-silence so
/// the ratio doesn't blow up.
fn relative_flux(flux: f32, energy: f32) -> f32 {
    if energy > FLUX_ENERGY_GATE { (flux / energy).clamp(0.0, 1.0) } else { 0.0 }
}

// ── Offline feature analysis (audio-layer modulation curve) ──────────────────
//
// An audio layer's modulation is precomputed, not analysed live: decode the file
// once, run the SAME variable-Q transform + band reductions the live worker uses
// over the whole buffer, and store one [`SendFeatures`] per hop as a
// [`FeatureCurve`]. At playback the content thread samples the curve at the
// playhead (docs/AUDIO_LAYER_DESIGN.md §3) — deterministic, look-ahead capable,
// and immune to content-thread hitches. The reductions are shared with the live
// worker ([`form_tilted_column`] + [`reduce_send`]), so the curve is bit-for-bit
// what the live path would produce.

/// A fresh per-send analysis state for the offline pass (no channels — the
/// offline analyzer feeds mono samples directly).
fn new_send_state(num_bins: usize) -> SendState {
    SendState {
        channels: Vec::new(),
        gain: 1.0,
        window: Vec::new(),
        since_hop: 0,
        col: vec![0.0; num_bins],
        prev_col: vec![0.0; num_bins],
        transient_refractory: [0; 4],
        energy_avg: [0.0; 4],
        has_prev: false,
        centroid_yfb: [-1.0; 4],
        features: SendFeatures::default(),
    }
}

/// A precomputed per-hop feature curve for one audio source. `features[i]`
/// describes the analysis window ending at sample `i * hop` (at `sample_rate`).
#[derive(Clone, Debug, Default)]
pub struct FeatureCurve {
    features: Vec<SendFeatures>,
    hop: usize,
    sample_rate: f32,
}

impl FeatureCurve {
    pub fn is_empty(&self) -> bool {
        self.features.is_empty()
    }

    pub fn len(&self) -> usize {
        self.features.len()
    }

    /// Hop in samples between successive feature frames (the live worker's rate).
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// Sample the curve at `seconds` into the source, clamped to the analysed
    /// range. `look_ahead_seconds` (≥ 0) reads ahead of the playhead so the
    /// modulation can anticipate a hit; pass 0 for the level-aligned reading.
    /// Returns silence (`SendFeatures::default`) for an empty curve.
    pub fn at_seconds(&self, seconds: f32, look_ahead_seconds: f32) -> SendFeatures {
        if self.features.is_empty() {
            return SendFeatures::default();
        }
        let t = (seconds + look_ahead_seconds.max(0.0)).max(0.0);
        let idx = (t * self.sample_rate / self.hop.max(1) as f32) as usize;
        self.features[idx.min(self.features.len() - 1)]
    }
}

/// Offline per-send analyzer: reduces a mono sample buffer into a [`FeatureCurve`]
/// using the same transform, tilt, band edges, and reductions as the live worker.
/// Build one per analysis pass (it owns the transform, scratch, and sequential
/// transient/flux state); cheap to construct, not a hot path.
pub struct OfflineSendAnalyzer {
    cqt: CqtTransform,
    spec_config: SpectrogramConfig,
    num_bins: usize,
    n_fft: usize,
    hop: usize,
    tilt_w: Vec<f32>,
    low_bin: usize,
    mid_bin: usize,
    sample_rate: f32,
    vqt_in: Vec<f32>,
    vqt_raw: Vec<f32>,
}

impl OfflineSendAnalyzer {
    /// Build for `sample_rate` and the project's Low/Mid/High crossovers (Hz) —
    /// the same crossovers a live send reads, so a layer's curve and a live send
    /// analyse identically.
    pub fn new(sample_rate: u32, low_hz: f32, mid_hz: f32) -> Self {
        let spec_config = SpectrogramConfig::default();
        let sr = sample_rate as f32;
        let num_bins = spec_config.num_bins(sr).max(1);
        let n_fft = spec_config.n_fft;
        let hop = spec_config.hop.max(1);
        let cqt = spec_config.build_transform(sr);
        let tilt_w = tilt_weights(&spec_config, sr, num_bins);
        let (low_bin, mid_bin) = band_edges(&spec_config, sr, num_bins, low_hz, mid_hz);
        Self {
            cqt,
            spec_config,
            num_bins,
            n_fft,
            hop,
            tilt_w,
            low_bin,
            mid_bin,
            sample_rate: sr,
            vqt_in: vec![0.0; n_fft],
            vqt_raw: vec![0.0; num_bins],
        }
    }

    /// Hop in samples between feature frames (matches the live worker's rate).
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// Reduce a mono sample buffer (already at unity or pre-multiplied by the
    /// send's gain) into a per-hop feature curve. One column per hop, ending at
    /// sample positions `hop, 2*hop, …` — the same accumulate-and-emit cadence
    /// as the live worker, zero-padded at the front while the window fills.
    pub fn analyze(&mut self, mono: &[f32]) -> FeatureCurve {
        let db_min = self.spec_config.db_min;
        let db_max = self.spec_config.db_max;
        let mut state = new_send_state(self.num_bins);
        let mut features = Vec::with_capacity(mono.len() / self.hop + 1);

        let mut end = self.hop;
        while end <= mono.len() {
            let start = end.saturating_sub(self.n_fft);
            form_tilted_column(
                &mono[start..end],
                &mut self.cqt,
                &self.tilt_w,
                &mut self.vqt_in,
                &mut self.vqt_raw,
                &mut state.col,
            );
            reduce_send(&mut state, self.num_bins, self.low_bin, self.mid_bin, db_min, db_max);
            state.prev_col.copy_from_slice(&state.col);
            // Arm flux/transients once the window has actually filled — matches
            // the live worker, so the warm-up ramp never reads as a transient.
            state.has_prev = end >= self.n_fft;
            features.push(state.features);
            end += self.hop;
        }

        FeatureCurve { features, hop: self.hop, sample_rate: self.sample_rate }
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

    // ── VQT feature-path helpers (the same transform + tilt the worker uses) ──
    fn cfg() -> SpectrogramConfig {
        SpectrogramConfig::default()
    }
    fn nbins() -> usize {
        cfg().num_bins(SR as f32)
    }
    fn nfft() -> usize {
        cfg().n_fft
    }
    /// Run the shared VQT on a window of samples (right-aligned, zero-padded at
    /// the front exactly as the worker does) and return the *tilted* column —
    /// the domain every band reduction reads.
    fn vqt_col(samples: &[f32]) -> Vec<f32> {
        let c = cfg();
        let nb = c.num_bins(SR as f32);
        let mut t = c.build_transform(SR as f32);
        let mut input = vec![0.0f32; c.n_fft];
        let seg = if samples.len() >= c.n_fft {
            &samples[samples.len() - c.n_fft..]
        } else {
            samples
        };
        let pad = c.n_fft - seg.len();
        input[pad..].copy_from_slice(seg);
        let mut raw = vec![0.0f32; nb];
        t.process_magnitudes(&input, &mut raw);
        let w = tilt_weights(&c, SR as f32, nb);
        raw.iter().zip(w.iter()).map(|(&m, &wt)| m * wt).collect()
    }
    /// Per-band amplitude of a tilted column at the default crossovers.
    fn band_amps(col: &[f32]) -> [f32; 4] {
        let c = cfg();
        let nb = c.num_bins(SR as f32);
        let (low_bin, mid_bin) = band_edges(&c, SR as f32, nb, 250.0, 2000.0);
        let prev = vec![0.0f32; nb];
        let ranges = [(0, nb), (0, low_bin), (low_bin, mid_bin), (mid_bin, nb)];
        let mut out = [0.0f32; 4];
        for (i, &(lo, hi)) in ranges.iter().enumerate() {
            out[i] = band_reduce(col, &prev, lo, hi, c.db_min, c.db_max).amplitude;
        }
        out
    }
    /// Drive `reduce_send` once on a tilted column (no predecessor → only the
    /// stateless features) and return the resulting per-band features.
    fn reduced(col: Vec<f32>) -> SendFeatures {
        let c = cfg();
        let nb = c.num_bins(SR as f32);
        let (low_bin, mid_bin) = band_edges(&c, SR as f32, nb, 250.0, 2000.0);
        let mut s = SendState {
            channels: vec![0],
            gain: 1.0,
            window: Vec::new(),
            since_hop: 0,
            col,
            prev_col: vec![0.0f32; nb],
            transient_refractory: [0; 4],
            energy_avg: [0.0; 4],
            has_prev: false,
            centroid_yfb: [-1.0; 4],
            features: SendFeatures::default(),
        };
        reduce_send(&mut s, nb, low_bin, mid_bin, c.db_min, c.db_max);
        s.features
    }

    #[test]
    fn band_amplitude_localizes_a_tone() {
        let (_full, low, mid, high) = bands();
        let n = nfft();

        let r = band_amps(&vqt_col(&sine(60.0, n)));
        assert!(
            r[low] > r[mid] && r[low] > r[high],
            "60 Hz should dominate the low band: {r:?}"
        );
        let r = band_amps(&vqt_col(&sine(1000.0, n)));
        assert!(
            r[mid] > r[low] && r[mid] > r[high],
            "1 kHz should dominate the mid band: {r:?}"
        );
        let r = band_amps(&vqt_col(&sine(6000.0, n)));
        assert!(
            r[high] > r[low] && r[high] > r[mid],
            "6 kHz should dominate the high band: {r:?}"
        );
    }

    #[test]
    fn silence_reads_near_zero() {
        let r = band_amps(&vqt_col(&vec![0.0; nfft()]));
        assert!(r.iter().all(|&a| a < 1e-6), "silence amplitude ~0: {r:?}");
    }

    #[test]
    fn brightness_rises_with_a_higher_tone() {
        let c = cfg();
        let nb = nbins();
        let prev = vec![0.0f32; nb];
        let n = nfft();
        let dark = band_reduce(&vqt_col(&sine(100.0, n)), &prev, 0, nb, c.db_min, c.db_max).brightness;
        let bright = band_reduce(&vqt_col(&sine(5000.0, n)), &prev, 0, nb, c.db_min, c.db_max).brightness;
        assert!(bright > dark, "5 kHz brighter than 100 Hz: {dark} vs {bright}");
        assert!((0.0..=1.0).contains(&dark) && (0.0..=1.0).contains(&bright), "0..1");
    }

    #[test]
    fn noisiness_separates_tone_from_noise() {
        let c = cfg();
        let nb = nbins();
        let prev = vec![0.0f32; nb];
        let n = nfft();
        let tone = band_reduce(&vqt_col(&sine(1000.0, n)), &prev, 0, nb, c.db_min, c.db_max).noisiness;
        let noisy = band_reduce(&vqt_col(&noise(n)), &prev, 0, nb, c.db_min, c.db_max).noisiness;
        assert!(noisy > tone, "noise flatter than a tone: {tone} vs {noisy}");
    }

    #[test]
    fn empty_bands_report_no_timbre() {
        // A pure 1 kHz tone fills the mid band; low and high hold only VQT
        // leakage — no real content. The presence gate must zero their
        // brightness/noisiness so a modulator mapped there reads silent rather
        // than describing the noise floor.
        let (_full, low, mid, high) = bands();
        let f = reduced(vqt_col(&sine(1000.0, nfft())));
        assert!(f.bands[mid].amplitude > 0.0, "mid band is present");
        for b in [low, high] {
            assert_eq!(f.bands[b].brightness, 0.0, "empty band brightness gated to 0");
            assert_eq!(f.bands[b].noisiness, 0.0, "empty band noisiness gated to 0");
        }
    }

    #[test]
    fn relative_flux_fires_on_change_not_steady_state() {
        let c = cfg();
        let nb = nbins();
        let col = vqt_col(&sine(1000.0, nfft()));
        // Energy appearing against silence → near-max relative flux.
        let zeros = vec![0.0f32; nb];
        let r = band_reduce(&col, &zeros, 0, nb, c.db_min, c.db_max);
        let onset = relative_flux(r.flux, r.energy);
        assert!(onset > 0.5, "energy from silence → high relative flux: {onset}");
        // The same spectrum twice → ~0 change.
        let r2 = band_reduce(&col, &col, 0, nb, c.db_min, c.db_max);
        let steady = relative_flux(r2.flux, r2.energy);
        assert!(steady < 0.1, "steady tone → low relative flux: {steady}");
    }

    #[test]
    fn transient_fires_once_per_note_not_per_hop() {
        // The reported bug: a HELD note machine-gunned onsets every refractory.
        // With the envelope + re-arm detector a held note must fire exactly once;
        // it can only fire again after the level dips/stops and the note restarts.
        let c = cfg();
        let nb = nbins();
        let (low_bin, mid_bin) = band_edges(&c, SR as f32, nb, 250.0, 2000.0);
        let mid = bands().2;
        let tone = vqt_col(&sine(1000.0, nfft()));
        let silence = vec![0.0f32; nb];

        let mut s = SendState {
            channels: vec![0],
            gain: 1.0,
            window: Vec::new(),
            since_hop: 0,
            col: tone.clone(),
            prev_col: vec![0.0f32; nb],
            transient_refractory: [0; 4],
            energy_avg: [0.0; 4],
            has_prev: true,
            centroid_yfb: [-1.0; 4],
            features: SendFeatures::default(),
        };

        // Hold the tone for many hops: exactly one onset at the start, then the
        // sustain must stay silent (no re-fire).
        let mut hold_fires = 0;
        for _ in 0..80 {
            s.col.copy_from_slice(&tone);
            reduce_send(&mut s, nb, low_bin, mid_bin, c.db_min, c.db_max);
            if s.features.bands[mid].transients > 0.99 {
                hold_fires += 1;
            }
            s.prev_col.copy_from_slice(&s.col);
        }
        assert_eq!(hold_fires, 1, "a held note fires one onset, not a burst");

        // A gap (re-arms the band), then the note again → a second onset.
        for _ in 0..40 {
            s.col.copy_from_slice(&silence);
            reduce_send(&mut s, nb, low_bin, mid_bin, c.db_min, c.db_max);
            s.prev_col.copy_from_slice(&s.col);
        }
        let mut reonset = false;
        for _ in 0..10 {
            s.col.copy_from_slice(&tone);
            reduce_send(&mut s, nb, low_bin, mid_bin, c.db_min, c.db_max);
            if s.features.bands[mid].transients > 0.99 {
                reonset = true;
            }
            s.prev_col.copy_from_slice(&s.col);
        }
        assert!(reonset, "the note returning after a gap must fire a fresh onset");
    }

    #[test]
    fn kick_on_sustained_bass_fires_each_hit() {
        // The other failure mode: kicks riding on a sustained bass floor (the low
        // band never goes quiet). Each kick's sharp attack must still fire even
        // though the band level never dips to silence between hits.
        let c = cfg();
        let nb = nbins();
        let (low_bin, mid_bin) = band_edges(&c, SR as f32, nb, 250.0, 2000.0);
        let low = bands().1;
        let kick = vqt_col(&sine(50.0, nfft())); // full-level low content
        let bass: Vec<f32> = kick.iter().map(|m| m * 0.3).collect(); // quieter sustain

        let mut s = SendState {
            channels: vec![0],
            gain: 1.0,
            window: Vec::new(),
            since_hop: 0,
            col: bass.clone(),
            prev_col: bass.clone(),
            transient_refractory: [0; 4],
            energy_avg: [0.0; 4],
            has_prev: true,
            centroid_yfb: [-1.0; 4],
            features: SendFeatures::default(),
        };
        // Settle the followers on the sustained bass floor.
        for _ in 0..40 {
            s.col.copy_from_slice(&bass);
            reduce_send(&mut s, nb, low_bin, mid_bin, c.db_min, c.db_max);
            s.prev_col.copy_from_slice(&s.col);
        }

        // Four kicks, each a short burst over the bass, with bass between them.
        let mut hits = 0;
        for _ in 0..4 {
            let mut fired = false;
            for h in 0..46 {
                let src = if h < 6 { &kick } else { &bass };
                s.col.copy_from_slice(src);
                reduce_send(&mut s, nb, low_bin, mid_bin, c.db_min, c.db_max);
                if s.features.bands[low].transients > 0.99 {
                    fired = true;
                }
                s.prev_col.copy_from_slice(&s.col);
            }
            if fired {
                hits += 1;
            }
        }
        assert_eq!(hits, 4, "every kick over sustained bass should fire an onset");
    }

    #[test]
    fn rapid_kicks_over_continuous_bass_dont_pin_out() {
        // Regression for the baseline-pinning miss: closely-spaced kicks over a
        // continuous loud bass (short gaps) must keep firing. If the baseline gets
        // ratcheted up by each hit, later kicks fall short and are missed.
        let c = cfg();
        let nb = nbins();
        let (low_bin, mid_bin) = band_edges(&c, SR as f32, nb, 250.0, 2000.0);
        let low = bands().1;
        let kick = vqt_col(&sine(50.0, nfft()));
        let bass: Vec<f32> = kick.iter().map(|m| m * 0.3).collect();

        let mut s = SendState {
            channels: vec![0],
            gain: 1.0,
            window: Vec::new(),
            since_hop: 0,
            col: bass.clone(),
            prev_col: bass.clone(),
            transient_refractory: [0; 4],
            energy_avg: [0.0; 4],
            has_prev: true,
            centroid_yfb: [-1.0; 4],
            features: SendFeatures::default(),
        };
        for _ in 0..40 {
            s.col.copy_from_slice(&bass);
            reduce_send(&mut s, nb, low_bin, mid_bin, c.db_min, c.db_max);
            s.prev_col.copy_from_slice(&s.col);
        }

        // Kicks every 24 hops (~127 ms, ~8/s) over unbroken bass — short gaps.
        let cycles = 8;
        let mut fires = 0;
        for _ in 0..cycles {
            let mut fired = false;
            for h in 0..24 {
                let src = if h < 6 { &kick } else { &bass };
                s.col.copy_from_slice(src);
                reduce_send(&mut s, nb, low_bin, mid_bin, c.db_min, c.db_max);
                if s.features.bands[low].transients > 0.99 {
                    fired = true;
                }
                s.prev_col.copy_from_slice(&s.col);
            }
            if fired {
                fires += 1;
            }
        }
        assert!(fires >= cycles - 1, "rapid kicks must keep firing, not pin out: {fires}/{cycles}");
    }

    #[test]
    fn band_edges_move_with_crossovers() {
        let c = cfg();
        let nb = nbins();
        let (_lo1, mid1) = band_edges(&c, SR as f32, nb, 250.0, 2000.0);
        // Raise the mid/high split: the High band must start at a higher bin.
        let (_lo2, mid2) = band_edges(&c, SR as f32, nb, 250.0, 6000.0);
        assert!(mid2 > mid1, "raising mid_hz pushes the High band start up: {mid1} -> {mid2}");
        // Degenerate input (low ≥ mid) still yields ordered, non-empty bands.
        let (lo3, mid3) = band_edges(&c, SR as f32, nb, 5000.0, 1000.0);
        assert!(mid3 > lo3 && mid3 < nb, "degenerate edges stay ordered: {lo3}..{mid3}/{nb}");
    }

    #[test]
    fn transients_localize_to_their_band() {
        // The kick-detector premise: a low thump appearing produces far more flux
        // in the low band than the high band, so Transients@Low fires on it while
        // Transients@High stays quiet.
        let c = cfg();
        let nb = nbins();
        let (low_bin, mid_bin) = band_edges(&c, SR as f32, nb, 250.0, 2000.0);
        let prev = vec![0.0f32; nb]; // silence baseline
        let col = vqt_col(&sine(60.0, nfft())); // a 60 Hz thump appears
        let low_flux = band_reduce(&col, &prev, 0, low_bin, c.db_min, c.db_max).flux;
        let high_flux = band_reduce(&col, &prev, mid_bin, nb, c.db_min, c.db_max).flux;
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
        let tone = sine(1000.0, nfft() * 4);
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

    // ── Offline analyzer (audio-layer feature curve) ──

    #[test]
    fn offline_analyzer_localizes_a_tone_in_the_curve() {
        // A 1 kHz tone, several windows long, must yield mid-band-dominant
        // amplitude in the curve — same result the live worker gives (it shares
        // the reduction path), proving the offline curve is the live analysis.
        let mono = sine(1000.0, nfft() * 4);
        let mut a = OfflineSendAnalyzer::new(SR, 250.0, 2000.0);
        let curve = a.analyze(&mono);
        assert!(!curve.is_empty());
        let (_full, low, mid, high) = bands();
        let secs = mono.len() as f32 / SR as f32;
        let f = curve.at_seconds(secs, 0.0);
        assert!(
            f.bands[mid].amplitude > f.bands[low].amplitude
                && f.bands[mid].amplitude > f.bands[high].amplitude,
            "1 kHz tone lands in mid band: {:?}",
            f.bands.map(|b| b.amplitude),
        );
    }

    #[test]
    fn offline_curve_matches_worker_hop_rate() {
        let a = OfflineSendAnalyzer::new(SR, 250.0, 2000.0);
        assert_eq!(a.hop(), SpectrogramConfig::default().hop.max(1));
    }

    #[test]
    fn feature_curve_sampling_clamps_and_looks_ahead() {
        let mono = sine(1000.0, nfft() * 4);
        let mut a = OfflineSendAnalyzer::new(SR, 250.0, 2000.0);
        let curve = a.analyze(&mono);
        // Out-of-range times clamp to the ends rather than panic.
        let _ = curve.at_seconds(-5.0, 0.0);
        let _ = curve.at_seconds(1e6, 0.0);
        // Look-ahead reads a later, window-full frame; a t=0 read with no
        // look-ahead lands on the zero-padded warm-up with less mid energy.
        let mid = bands().2;
        let early = curve.at_seconds(0.0, 0.0);
        let ahead = curve.at_seconds(0.0, mono.len() as f32 / SR as f32);
        assert!(ahead.bands[mid].amplitude >= early.bands[mid].amplitude);
    }

    #[test]
    fn empty_buffer_yields_empty_curve_and_silent_sample() {
        let mut a = OfflineSendAnalyzer::new(SR, 250.0, 2000.0);
        let curve = a.analyze(&[]);
        assert!(curve.is_empty());
        assert_eq!(curve.at_seconds(0.0, 0.0), SendFeatures::default());
    }
}
