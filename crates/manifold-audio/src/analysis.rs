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
//! capture ring (f32, interleaved) ─drain→ AudioFeatureWorker ─mono→ MonoReader
//!   (cpal RT thread fills it)              (downmix worker thread)   (content thread)
//! ```
//!
//! The worker owns the capture ring's [`AudioConsumer`](crate::capture::AudioConsumer),
//! deinterleaves it, and downmixes each configured **send** to one post-gain mono
//! sample per device frame, published interleaved-by-send through a second SPSC
//! `ringbuf` — no `Arc<Mutex>`, no locks on the read path. Analysis itself (VQT,
//! bands, onsets, scope columns) runs on the **content thread** via
//! [`StreamingSendAnalyzer`], so a send can sum its capture mono with audio-layer
//! taps before a single analysis ("what you hear is what modulates").
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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use ringbuf::HeapRb;
use ringbuf::traits::{Consumer as ConsumerTrait, Observer as ObserverTrait, Producer as ProducerTrait, Split};

pub use manifold_core::audio_features::SendFeatures;
use manifold_spectral::{CqtTransform, SpectrogramConfig};

use crate::capture::AudioConsumer;

/// Maximum number of sends the worker downmixes. Caps the per-tick work and the
/// mono-handoff stride.
pub const MAX_SENDS: usize = 16;

/// Mono-handoff ring capacity (samples) for the worker→content-thread stream. At
/// 48 kHz a content tick spans ~800 samples per send; this holds ~170 ms of
/// generous headroom so jitter never starves a tick, and the content thread
/// drains every tick so it never overflows in steady state.
const MONO_RING_CAPACITY: usize = 16384;

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

/// Overlay scalars per spectrogram column: four per-band centroid heights
/// `[centroid_full, centroid_low, centroid_mid, centroid_high]` followed by the
/// three per-band onset impulses `[onset_low, onset_mid, onset_high]`. The shader
/// reads the same stride; a [`StreamingSendAnalyzer`] buffers this many floats
/// per scope column.
const SCOPE_SCALAR_STRIDE: usize = 7;

/// Read end of the capture worker's per-send **mono** stream.
///
/// The worker downmixes each send's device channels to one post-gain mono sample
/// per device frame and pushes them here, interleaved by send (stride = send
/// count). The content thread drains them and feeds each send's analyzer —
/// analysis lives on the content thread now, so a send can sum its capture mono
/// with audio-layer taps before a *single* analysis ("what you hear is what
/// modulates"). Lock-free SPSC, no `Arc<Mutex>` on the read path.
pub struct MonoReader {
    cons: ringbuf::HeapCons<f32>,
    send_count: usize,
    sample_rate: u32,
    /// Reusable drain scratch (a whole number of frames).
    scratch: Vec<f32>,
}

impl MonoReader {
    /// The capture device's sample rate — the rate the mono samples arrive at.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Number of sends (the interleave stride).
    pub fn send_count(&self) -> usize {
        self.send_count
    }

    /// Drain every complete per-send frame produced since the last call, appending
    /// each send's mono samples to `per_send[i]` (oldest → newest). `per_send`
    /// must have at least [`Self::send_count`] entries; callers clear them first.
    pub fn drain(&mut self, per_send: &mut [Vec<f32>]) {
        let stride = self.send_count.max(1);
        loop {
            let frames = self.cons.occupied_len() / stride;
            if frames == 0 {
                break;
            }
            let cap_frames = (self.scratch.len() / stride).max(1);
            let take = frames.min(cap_frames) * stride;
            let got = self.cons.pop_slice(&mut self.scratch[..take]);
            for frame in self.scratch[..got].chunks_exact(stride) {
                for (i, &s) in frame.iter().enumerate() {
                    if let Some(v) = per_send.get_mut(i) {
                        v.push(s);
                    }
                }
            }
            if got < take {
                break;
            }
        }
    }
}

/// Spawns and owns the capture downmix worker thread. Stops the thread on
/// `stop()` or drop.
pub struct AudioFeatureWorker {
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl AudioFeatureWorker {
    /// Spawn the downmix worker. Takes ownership of the capture ring's `consumer`,
    /// the device `sample_rate` and `device_channels` (for deinterleaving), the
    /// per-send `send_channels`, and the live `gains`. Returns the worker handle
    /// and a [`MonoReader`] the content thread drains each tick.
    ///
    /// The worker only downmixes — it produces one post-gain mono sample per send
    /// per device frame. Analysis (VQT, bands, onsets, scope columns) runs on the
    /// content thread via [`StreamingSendAnalyzer`], so a send can mix its capture
    /// mono with audio-layer taps before a single analysis.
    ///
    /// Sends beyond [`MAX_SENDS`] are dropped with a warning.
    pub fn spawn(
        consumer: AudioConsumer,
        sample_rate: u32,
        device_channels: u16,
        mut send_channels: Vec<Vec<u16>>,
        gains: Arc<GainBank>,
    ) -> (Self, MonoReader) {
        if send_channels.len() > MAX_SENDS {
            log::warn!(
                "[AudioAnalysis] {} sends exceeds MAX_SENDS={MAX_SENDS}; extra dropped",
                send_channels.len(),
            );
            send_channels.truncate(MAX_SENDS);
        }
        let send_count = send_channels.len();

        // Interleaved-by-send mono ring (stride = send count). Whole frames only,
        // so the stride never desyncs.
        let stride = send_count.max(1);
        let (prod, cons) = HeapRb::<f32>::new((MONO_RING_CAPACITY * stride).max(1)).split();
        let reader = MonoReader {
            cons,
            send_count,
            sample_rate,
            scratch: vec![0.0; (MONO_RING_CAPACITY * stride).max(stride)],
        };

        let running = Arc::new(AtomicBool::new(true));
        let running_thread = running.clone();

        let handle = std::thread::Builder::new()
            .name("manifold-audio-downmix".into())
            .spawn(move || {
                let mut worker = MonoWorkerLoop::new(
                    consumer,
                    prod,
                    device_channels as usize,
                    send_channels,
                    gains,
                );
                worker.run(&running_thread);
            })
            .expect("spawn audio downmix thread");

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

/// Per-send running state on the worker thread. Every send now runs the same
/// variable-Q transform the scope draws (one shared [`CqtTransform`]); features
/// and the spectrogram are the *same* analysis, so "what you see is what
/// modulates" holds per send.
struct SendState {
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
    /// Edge-trigger arm flag per band. A fire disarms the band; it re-arms only
    /// once the ODF falls back below `threshold·SUPERFLUX_REARM_RATIO`. This is
    /// what stops sustained/dense material (ODF parked above threshold) from
    /// re-firing every refractory — fire on the rising edge, not the level.
    transient_armed: [bool; 4],
    /// Per-band moving average of the SuperFlux ODF — the onset detector's
    /// adaptive threshold baseline. A transient fires when the ODF spikes above
    /// this by `SUPERFLUX_THRESH_FACTOR`, so detection self-calibrates to the
    /// track's onset density instead of a fixed threshold. Order [Full,Low,Mid,High].
    flux_avg: [f32; 4],
    /// Whether `prev_col` holds a real column yet (skips the startup flux spike).
    has_prev: bool,
    /// Per-band spectral-centroid height-from-bottom (0..1) for the scope overlay,
    /// indexed [Full, Low, Mid, High]; `-1` when the band isn't loud enough to
    /// characterise. Mirrors the `brightness` feature's gating, but mapped to the
    /// global display y so each band's centroid draws within its own region.
    centroid_yfb: [f32; 4],
    features: SendFeatures,
}

/// The capture downmix worker. Drains the device ring, deinterleaves it, and
/// downmixes each send's channels to one post-gain mono sample per device frame,
/// pushing the result interleaved-by-send to the content thread. No analysis —
/// that's the content thread's [`StreamingSendAnalyzer`], so a send can mix this
/// capture mono with audio-layer taps before a single analysis.
struct MonoWorkerLoop {
    consumer: AudioConsumer,
    /// Interleaved-by-send mono output (stride = send count). Whole frames only.
    producer: ringbuf::HeapProd<f32>,
    device_channels: usize,
    /// Per-send device channels to downmix to mono, in send order.
    send_channels: Vec<Vec<u16>>,
    /// Live per-send gain, written by the content thread, read here each drain.
    gains: Arc<GainBank>,
    /// Per-send linear gain snapshot, refreshed once per drain (avoids an atomic
    /// load per sample).
    gain_scratch: Vec<f32>,
    /// Leftover interleaved samples that didn't complete a device frame last drain.
    carry: Vec<f32>,
    /// Persistent per-drain work buffer (carry-over + freshly drained samples).
    work: Vec<f32>,
    /// Reusable device-ring drain buffer.
    drain_buf: Vec<f32>,
    /// Reusable interleaved-by-send mono output buffer.
    out: Vec<f32>,
}

impl MonoWorkerLoop {
    fn new(
        consumer: AudioConsumer,
        producer: ringbuf::HeapProd<f32>,
        device_channels: usize,
        send_channels: Vec<Vec<u16>>,
        gains: Arc<GainBank>,
    ) -> Self {
        let send_count = send_channels.len();
        Self {
            consumer,
            producer,
            device_channels: device_channels.max(1),
            send_channels,
            gains,
            gain_scratch: vec![1.0; send_count],
            carry: Vec::with_capacity(4096),
            work: Vec::with_capacity(4096),
            drain_buf: vec![0.0; 4096],
            out: Vec::with_capacity(4096),
        }
    }

    fn run(&mut self, running: &AtomicBool) {
        while running.load(Ordering::Acquire) {
            if !self.drain_and_downmix() {
                // Nothing new; back off briefly so we don't spin a core.
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// Drain the device ring, downmix each complete frame to per-send post-gain
    /// mono, and push the interleaved result. Returns whether anything was pushed.
    fn drain_and_downmix(&mut self) -> bool {
        let available = self.consumer.occupied_len();
        if available == 0 && self.carry.is_empty() {
            return false;
        }

        // carry-over + freshly drained samples → `work` (a borrowed local).
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

        // Refresh the per-send gain snapshot once per drain (lock-free; a gain
        // edit lands here without a capture restart).
        for (i, g) in self.gain_scratch.iter_mut().enumerate() {
            *g = self.gains.get_linear(i);
        }

        // Downmix each device frame → one post-gain mono sample per send,
        // interleaved by send (stride = send count).
        self.out.clear();
        for frame in work[..usable].chunks_exact(ch) {
            for (channels, &gain) in self.send_channels.iter().zip(self.gain_scratch.iter()) {
                self.out.push(downmix(frame, channels) * gain);
            }
        }

        // Stash the partial-frame remainder; return the work buffer for reuse.
        self.carry.extend_from_slice(&work[usable..]);
        self.work = work;

        if self.out.is_empty() {
            return false;
        }
        // Whole frames only so the stride never desyncs. On overflow (content
        // thread stalled) drop the OLDEST frames, keeping the newest.
        let stride = self.send_channels.len().max(1);
        let vacant_frames = self.producer.vacant_len() / stride;
        let want_frames = self.out.len() / stride;
        let push_frames = want_frames.min(vacant_frames);
        if push_frames == 0 {
            return false;
        }
        let start = (want_frames - push_frames) * stride;
        self.producer.push_slice(&self.out[start..]);
        true
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

/// Incremental linear resampler for one mono stream. Feed input-rate chunks, get
/// output-rate samples appended. Stateful across chunks (keeps the last input
/// sample + fractional read position), so a streaming source converts cleanly.
///
/// Used to align an audio-layer tap (kira's output rate) to the capture device's
/// rate before summing them into one send's analyzer. Linear interpolation is
/// ample here — the result feeds energy/transient analysis, not playback, so band
/// magnitudes are what matter, not reconstruction fidelity. When the rates match
/// the runtime skips this entirely (see [`Self::is_identity`]).
pub struct LinearResampler {
    /// Input samples consumed per output sample (`in_rate / out_rate`).
    step: f64,
    /// Read position of the next output, in input-sample units, where index 0 is
    /// `last` and index k≥1 is the current chunk's sample k−1.
    pos: f64,
    /// The previous chunk's final input sample (left edge across the boundary).
    last: f32,
}

impl LinearResampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        Self { step: in_rate as f64 / out_rate.max(1) as f64, pos: 0.0, last: 0.0 }
    }

    /// Whether input and output rates match (the runtime copies directly instead).
    pub fn is_identity(&self) -> bool {
        (self.step - 1.0).abs() < 1e-9
    }

    /// Resample `input` (at the input rate) into `out` (appended, at the output
    /// rate), carrying fractional state for the next call.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let n = input.len();
        if n == 0 {
            return;
        }
        // Extended sample s(k): s(0) = last, s(k) = input[k-1] for k in 1..=n.
        // Emit at t = pos, pos+step, … while both s(⌊t⌋) and s(⌊t⌋+1) exist.
        let mut t = self.pos;
        while t < n as f64 {
            let i = t.floor() as usize;
            let frac = (t - i as f64) as f32;
            let a = if i == 0 { self.last } else { input[i - 1] };
            let b = input[i]; // s(i+1) = input[i]; valid since i < n
            out.push(a + (b - a) * frac);
            t += self.step;
        }
        // Rebase: next chunk's index 0 becomes this chunk's final sample.
        self.last = input[n - 1];
        self.pos = t - n as f64;
    }
}

/// Form the tilted variable-Q column for `seg` (a window slice no longer than
/// `vqt_in`, right-aligned and zero-padded at the front) into `out_col`, using
/// the shared transform + per-bin pink-tilt weights. This is the exact column
/// the live worker reduces and the scope draws; the streaming analyzer
/// ([`StreamingSendAnalyzer`]) calls the same function so an audio layer's
/// tapped feature stream is bit-for-bit identical to what a captured send
/// produces ("what you see is what modulates"). On return, `vqt_raw` holds
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

// ── Onset (transient) detection — SuperFlux ──────────────────────────────
//
// SuperFlux (Böck & Widmer, DAFx 2013): a spectral-flux onset detector with a
// frequency MAXIMUM FILTER for vibrato/pitch-slide suppression. Per hop, the
// onset detection function (ODF) is the band sum of POSITIVE change vs the
// previous column after max-filtering that column across ±`MAXFILTER_RADIUS`
// bins (see `band_reduce`). Plain flux fires on any energy rise — a bending
// note moves energy to an adjacent bin and reads as an attack; the max-filter
// already "covers" that neighbour, so only genuinely NEW broadband energy (a
// real attack) survives. A band fires when its ODF spikes above a moving
// average of its own ODF by `SUPERFLUX_THRESH_FACTOR` (self-calibrating to the
// track's density), gated to loud bands and a short refractory so one attack's
// multi-hop rise fires once. This replaced an energy-over-mean detector that
// tripped on amplitude wobble in busy mixes. Shared by triggers, the
// `Transients` modulation feature, and the scope — one detector, three readers.

/// How far the ODF must exceed its moving average to fire. THE sensitivity knob
/// — lower catches softer onsets, higher is stricter.
const SUPERFLUX_THRESH_FACTOR: f32 = 2.0;
/// Small absolute floor added to the adaptive threshold so near-silent flux
/// jitter (average ≈ 0) can't satisfy the multiplicative test on its own.
const SUPERFLUX_DELTA: f32 = 1e-3;
/// Edge-trigger re-arm: after firing, a band can't fire again until its ODF
/// falls back below `threshold · this`. Stops sustained/dense material (ODF
/// parked above threshold) from re-firing every refractory — one fire per
/// rising edge. Real onsets dip between hits and re-arm; a sustain does not.
const SUPERFLUX_REARM_RATIO: f32 = 0.7;
/// Frequency max-filter radius (bins) for vibrato suppression. The SuperFlux
/// paper uses ±1 bin at 24 bins/octave — wide enough to cover a semitone wobble.
const MAXFILTER_RADIUS: usize = 1;
/// Per-hop smoothing of the ODF moving-average threshold (~50 ms at hop ≈ 5.3 ms):
/// slow enough that an attack spikes above it, fast enough to track a build.
const FLUX_AVG_COEFF: f32 = 0.1;
/// A transient only fires on a band loud enough to matter — `amplitude` is the
/// band's level on the colourmap's 0..1 dB scale (≈ −53 dBFS here).
const ONSET_AMP_GATE: f32 = 0.12;
/// Per-hop decay of the transient impulse (~100 ms settle at hop ≈ 5.3 ms).
const ONSET_DECAY: f32 = 0.85;
/// Below this band energy, relative flux reads 0 — avoids the flux ÷ energy ratio
/// blowing up on near-silence.
const FLUX_ENERGY_GATE: f32 = 1e-4;
/// Refractory after an onset (~32 ms at hop ≈ 5.3 ms) — SuperFlux's built-in
/// minimum inter-onset interval. Debounces one attack's multi-hop rise while
/// still allowing fast hat runs (≈1/32 at 160 BPM). Caps the rate at ~30/s.
const ONSET_REFRACTORY_HOPS: u8 = 6;
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
    /// SuperFlux onset detection function: positive flux vs the previous column
    /// after a frequency **maximum filter** — the vibrato/pitch-slide suppression
    /// that makes this fire on attacks, not amplitude wobble. Onset input.
    superflux: f32,
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
            superflux: 0.0,
            energy: 0.0,
        };
    }
    let n_bins = prev.len();
    let mut sum_sq = 0.0f32;
    let mut num = 0.0f32;
    let mut den = 0.0f32;
    let mut log_sum = 0.0f32;
    let mut lin_sum = 0.0f32;
    let mut flux = 0.0f32;
    let mut superflux = 0.0f32;
    let mut energy = 0.0f32;
    for k in lo..hi {
        let m = col[k];
        sum_sq += m * m;
        num += k as f32 * m;
        den += m;
        log_sum += m.max(1e-9).ln();
        lin_sum += m;
        // Plain flux (liveliness): rise vs the same bin one hop back.
        let d = m - prev[k];
        if d > 0.0 {
            flux += d;
        }
        // SuperFlux ODF (onsets): rise vs the MAX of the previous column over a
        // ±`MAXFILTER_RADIUS`-bin neighbourhood. A small pitch slide just shifts
        // energy to an adjacent bin, which the max-filter already covers — so it
        // contributes ~0, while a real attack (new broadband energy) still does.
        let klo = k.saturating_sub(MAXFILTER_RADIUS);
        let khi = (k + MAXFILTER_RADIUS + 1).min(n_bins);
        let mut prev_max = 0.0f32;
        for &p in &prev[klo..khi] {
            if p > prev_max {
                prev_max = p;
            }
        }
        let ds = m - prev_max;
        if ds > 0.0 {
            superflux += ds;
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

    BandReduce { amplitude, brightness, noisiness, flux, superflux, energy }
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

        // SuperFlux ODF moving average — the adaptive threshold baseline. `avg`
        // is the PAST average; the current ODF is tested against it, then folded
        // in (slow, so an attack spikes above it rather than raising it).
        let avg = send.flux_avg[bi];

        if have_prev {
            // Liveliness self-scales with density (relative plain flux).
            bf.liveliness = if loud { relative_flux(r.flux, r.energy) } else { 0.0 };

            // Transient (SuperFlux peak-pick, edge-triggered): the band's
            // max-filtered flux ODF rising above its own moving-average threshold,
            // on a loud band, ARMED, outside the short refractory. The max-filter
            // (band_reduce) rejects vibrato/pitch slides; the arm flag makes this
            // fire ONCE per rising edge, so sustained/dense content (ODF parked
            // above threshold) fires once, not every refractory.
            let threshold = avg * SUPERFLUX_THRESH_FACTOR + SUPERFLUX_DELTA;
            let refractory = &mut send.transient_refractory[bi];
            let armed = &mut send.transient_armed[bi];
            let triggered = loud && *armed && *refractory == 0 && r.superflux > threshold;
            if triggered {
                bf.transients = 1.0;
                *refractory = ONSET_REFRACTORY_HOPS;
                *armed = false;
            } else {
                bf.transients *= ONSET_DECAY;
                *refractory = refractory.saturating_sub(1);
                // Re-arm once the ODF falls back below the re-arm floor — a real
                // onset's flux dips between hits; a sustain's does not.
                if r.superflux < threshold * SUPERFLUX_REARM_RATIO {
                    *armed = true;
                }
            }
        }

        // Fold the current ODF into the moving-average threshold (after the test).
        send.flux_avg[bi] = avg + (r.superflux - avg) * FLUX_AVG_COEFF;
    }
}

/// Relative flux = flux ÷ energy. Naturally 0..1 (each bin's positive change
/// can't exceed its current value when prev ≥ 0), gated to 0 on near-silence so
/// the ratio doesn't blow up.
fn relative_flux(flux: f32, energy: f32) -> f32 {
    if energy > FLUX_ENERGY_GATE { (flux / energy).clamp(0.0, 1.0) } else { 0.0 }
}

// ── Streaming feature analysis (audio-layer modulation) ──────────────────────
//
// An audio layer's modulation is analysed live off the kira mixer: a pass-through
// tap on the layer's sub-track copies the post-fader mono signal into a ring, and
// the content thread feeds it to a [`StreamingSendAnalyzer`] each tick. That runs
// the SAME variable-Q transform + band reductions the live capture worker uses
// ([`form_tilted_column`] + [`reduce_send`]), so a layer-fed send analyses
// bit-for-bit like a captured one — warp, gain, and mute are already baked into
// the tapped samples (docs/AUDIO_LAYER_DESIGN.md §3R).

/// A fresh per-send analysis state for an analyzer that feeds mono samples
/// directly (no channel downmix) — the offline-free streaming + test paths.
fn new_send_state(num_bins: usize) -> SendState {
    SendState {
        window: Vec::new(),
        since_hop: 0,
        col: vec![0.0; num_bins],
        prev_col: vec![0.0; num_bins],
        transient_refractory: [0; 4],
        transient_armed: [true; 4],
        flux_avg: [0.0; 4],
        has_prev: false,
        centroid_yfb: [-1.0; 4],
        features: SendFeatures::default(),
    }
}

/// Streaming per-send analyzer for audio-layer modulation.
/// Push mono samples as they arrive — e.g. tapped off a kira audio-layer track,
/// already post-fader (the mixer applied warp + gain) — and read the
/// [`latest`](Self::latest) per-band features. Uses the exact same transform,
/// tilt, band edges, and reductions as the live capture worker, so a layer-fed
/// send modulates identically to a captured one: "what you see is what
/// modulates" holds whether the audio is live-captured or streamed through the
/// mixer.
///
/// Build one per layer-fed send. Construction owns a transform + scratch (not a
/// per-tick cost); `push` runs whole VQT hops as the window fills and refreshes
/// `latest`, and is nearly free between hops. Silence in → features decay to
/// silence, so a muted or paused layer reads as no modulation with no special
/// casing.
pub struct StreamingSendAnalyzer {
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
    state: SendState,
    latest: SendFeatures,
    /// When set, `push` buffers the raw column + overlay scalars per hop so the
    /// Audio Setup scope can draw this send (the runtime turns it on only for the
    /// send the scope shows, and drains every tick). Same data the capture worker
    /// pushes to its scope rings — so a layer-fed send draws identically.
    scope: bool,
    scope_cols: Vec<f32>,
    scope_scalars: Vec<f32>,
    /// Pre-analysis noise floor (dB). Bins quieter than this are zeroed in the
    /// raw + tilted column before scope display and feature reduction, so the
    /// squelch is identical in what you see and what you detect. `FLOOR_DB_OFF`
    /// = no gate (the default).
    floor_db: f32,
}

impl StreamingSendAnalyzer {
    /// Build for `sample_rate` (the rate samples are pushed at — the mixer's
    /// output rate, not the source file's) and the project's Low/Mid/High
    /// crossovers (Hz). Same crossovers a live send reads, so the analyses match.
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
            state: new_send_state(num_bins),
            latest: SendFeatures::default(),
            scope: false,
            scope_cols: Vec::new(),
            scope_scalars: Vec::new(),
            floor_db: manifold_core::audio_setup::FLOOR_DB_OFF,
        }
    }

    /// Set the pre-analysis noise floor (dB). Applied live every hop; no rebuild.
    /// `FLOOR_DB_OFF` (or anything at/below it) disables the gate.
    pub fn set_floor_db(&mut self, floor_db: f32) {
        self.floor_db = floor_db;
    }

    /// The sample rate this analyzer was built for — the caller rebuilds it if
    /// the mixer's output rate ever changes under it.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate as u32
    }

    /// Number of spectrogram bins per column (the scope's vertical resolution).
    pub fn num_bins(&self) -> usize {
        self.num_bins
    }

    /// Analysed frequency range `(fmin, fmax)` Hz, for the scope's frequency axis.
    pub fn freq_range(&self) -> (f32, f32) {
        (self.spec_config.fmin, self.spec_config.effective_fmax(self.sample_rate))
    }

    /// Turn scope-column capture on/off. On only for the send the Audio Setup
    /// scope shows; off clears any buffered columns so they don't pile up while
    /// undrained.
    pub fn set_scope(&mut self, on: bool) {
        if on != self.scope {
            self.scope = on;
            if !on {
                self.scope_cols.clear();
                self.scope_scalars.clear();
            }
        }
    }

    /// Drain buffered raw spectrogram columns (oldest → newest), one `num_bins`
    /// slice per call, then clear. No-op when scope capture is off / nothing new.
    pub fn drain_scope_columns(&mut self, mut f: impl FnMut(&[f32])) {
        let nb = self.num_bins.max(1);
        for col in self.scope_cols.chunks_exact(nb) {
            f(col);
        }
        self.scope_cols.clear();
    }

    /// Drain buffered overlay scalars in lockstep with the columns: four per-band
    /// centroid heights `[full, low, mid, high]` + three per-band onset impulses
    /// `[low, mid, high]`. Same stride the scope shader reads.
    pub fn drain_scope_scalars(&mut self, mut f: impl FnMut([f32; 4], [f32; 3])) {
        for s in self.scope_scalars.chunks_exact(SCOPE_SCALAR_STRIDE) {
            f([s[0], s[1], s[2], s[3]], [s[4], s[5], s[6]]);
        }
        self.scope_scalars.clear();
    }

    /// Retune the analysis band edges to new Low/Mid crossovers (cheap; no
    /// transform rebuild). Mirrors the live worker's live-crossover retune.
    pub fn set_crossovers(&mut self, low_hz: f32, mid_hz: f32) {
        let (low_bin, mid_bin) =
            band_edges(&self.spec_config, self.sample_rate, self.num_bins, low_hz, mid_hz);
        self.low_bin = low_bin;
        self.mid_bin = mid_bin;
    }

    /// Push freshly produced mono samples and run any whole VQT hops the window
    /// now owes, refreshing [`latest`](Self::latest). Same accumulate-and-emit
    /// cadence as the live worker's per-send loop.
    pub fn push(&mut self, mono: &[f32]) {
        if mono.is_empty() {
            return;
        }
        let floor_db = self.floor_db;
        let Self {
            cqt,
            spec_config,
            num_bins,
            n_fft,
            hop,
            tilt_w,
            low_bin,
            mid_bin,
            vqt_in,
            vqt_raw,
            state,
            latest,
            scope,
            scope_cols,
            scope_scalars,
            ..
        } = self;
        let (n_fft, hop, nb) = (*n_fft, *hop, *num_bins);

        for &s in mono {
            state.window.push(s);
            state.since_hop += 1;
        }
        // Bound the window to one window plus a small backlog — realloc-free, and
        // a brief drain stall doesn't lose distinct columns.
        let cap = n_fft + WINDOW_BACKLOG_HOPS * hop;
        if state.window.len() > cap {
            let excess = state.window.len() - cap;
            state.window.drain(0..excess);
        }

        let owed = state.since_hop / hop;
        if owed == 0 {
            return;
        }
        // Distinct columns we can actually form; before the window fills we still
        // emit one (zero-padded) so features fade in rather than blacking out.
        let avail = if state.window.len() >= n_fft {
            1 + (state.window.len() - n_fft) / hop
        } else {
            1
        };
        let emit = owed.min(avail);
        let db_min = spec_config.db_min;
        let db_max = spec_config.db_max;

        for j in (0..emit).rev() {
            let end = state.window.len().saturating_sub(j * hop);
            let start = end.saturating_sub(n_fft);
            form_tilted_column(
                &state.window[start..end],
                cqt,
                tilt_w,
                vqt_in,
                vqt_raw,
                &mut state.col,
            );
            // Pre-analysis floor: zero every bin whose raw magnitude is below the
            // dB floor, in BOTH the scope (`vqt_raw`) and feature (`state.col`)
            // column — so the squelch the user sees on the spectrogram is exactly
            // the signal the bands + transients detect on. Off (sentinel) skips.
            if floor_db > manifold_core::audio_setup::FLOOR_DB_OFF {
                let lin_floor = 10f32.powf(floor_db / 20.0);
                for (raw, c) in vqt_raw.iter_mut().zip(state.col.iter_mut()) {
                    if *raw < lin_floor {
                        *raw = 0.0;
                        *c = 0.0;
                    }
                }
            }
            reduce_send(state, nb, *low_bin, *mid_bin, db_min, db_max);
            state.prev_col.copy_from_slice(&state.col);
            // Flux/transients arm only once the window has filled, so the warm-up
            // ramp never reads as a transient (matches the live worker).
            state.has_prev = state.window.len() >= n_fft;

            // Scope capture: buffer the raw (untilted) column + overlay scalars,
            // exactly what the live worker pushes to its scope rings — the shader
            // applies its own display tilt. Drained by the runtime each tick.
            if *scope {
                scope_cols.extend_from_slice(vqt_raw);
                let b = &state.features.bands;
                scope_scalars.extend_from_slice(&[
                    state.centroid_yfb[0],
                    state.centroid_yfb[1],
                    state.centroid_yfb[2],
                    state.centroid_yfb[3],
                    b[1].transients,
                    b[2].transients,
                    b[3].transients,
                ]);
            }
        }
        state.since_hop -= owed * hop;
        *latest = state.features;
    }

    /// Latest per-band features (silence until the first hop completes).
    pub fn latest(&self) -> SendFeatures {
        self.latest
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

    #[test]
    fn resampler_identity_passes_rate_through() {
        let mut r = LinearResampler::new(48_000, 48_000);
        assert!(r.is_identity());
        let mut out = Vec::new();
        // At a 1:1 step the output count tracks the input across chunks (within
        // one sample of startup); content is what matters downstream, not phase.
        for chunk in sine(440.0, 1000).chunks(133) {
            r.process(chunk, &mut out);
        }
        assert!(
            (out.len() as i64 - 1000).abs() <= 1,
            "identity resample preserves count: {}",
            out.len()
        );
    }

    #[test]
    fn resampler_halves_count_when_downsampling_2to1() {
        // 48k → 24k: ~half the samples out, streamed in odd chunks (state carries).
        let mut r = LinearResampler::new(48_000, 24_000);
        assert!(!r.is_identity());
        let mut out = Vec::new();
        let input = sine(440.0, 4096);
        for chunk in input.chunks(101) {
            r.process(chunk, &mut out);
        }
        let expected = 4096 / 2;
        assert!(
            (out.len() as i64 - expected as i64).abs() <= 4,
            "2:1 downsample yields ~half: got {} want ~{expected}",
            out.len()
        );
    }

    #[test]
    fn resampler_doubles_count_when_upsampling_1to2() {
        // 24k → 48k: ~double the samples out.
        let mut r = LinearResampler::new(24_000, 48_000);
        let mut out = Vec::new();
        let input = sine(440.0, 2048);
        for chunk in input.chunks(97) {
            r.process(chunk, &mut out);
        }
        let expected = 2048 * 2;
        assert!(
            (out.len() as i64 - expected as i64).abs() <= 4,
            "1:2 upsample yields ~double: got {} want ~{expected}",
            out.len()
        );
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
            window: Vec::new(),
            since_hop: 0,
            col,
            prev_col: vec![0.0f32; nb],
            transient_refractory: [0; 4],
            transient_armed: [true; 4],
            flux_avg: [0.0; 4],
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
    fn superflux_ignores_a_pitch_slide_but_keeps_real_attacks() {
        // The SuperFlux headline: a one-bin pitch slide (energy moving to a
        // neighbour — vibrato/bass wobble) trips PLAIN flux but not the
        // max-filtered ODF, while a genuine attack from silence fires both.
        let c = cfg();
        let nb = nbins();
        let b = nb / 2;
        let mut prev_shifted = vec![0.0f32; nb];
        prev_shifted[b - 1] = 1.0; // energy was one bin lower last hop
        let mut col = vec![0.0f32; nb];
        col[b] = 1.0; // now one bin higher — a slide, not a new onset

        let slide = band_reduce(&col, &prev_shifted, 0, nb, c.db_min, c.db_max);
        assert!(slide.flux > 0.5, "plain flux trips on the bin shift: {}", slide.flux);
        assert!(
            slide.superflux < 1e-6,
            "SuperFlux's max-filter covers the neighbour, so a 1-bin slide reads ~0: {}",
            slide.superflux,
        );

        // A real attack from silence still produces strong SuperFlux.
        let silence = vec![0.0f32; nb];
        let attack = band_reduce(&col, &silence, 0, nb, c.db_min, c.db_max);
        assert!(
            attack.superflux > 0.5,
            "new broadband energy still fires SuperFlux: {}",
            attack.superflux,
        );
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
            window: Vec::new(),
            since_hop: 0,
            col: tone.clone(),
            prev_col: vec![0.0f32; nb],
            transient_refractory: [0; 4],
            transient_armed: [true; 4],
            flux_avg: [0.0; 4],
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
            window: Vec::new(),
            since_hop: 0,
            col: bass.clone(),
            prev_col: bass.clone(),
            transient_refractory: [0; 4],
            transient_armed: [true; 4],
            flux_avg: [0.0; 4],
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
            window: Vec::new(),
            since_hop: 0,
            col: bass.clone(),
            prev_col: bass.clone(),
            transient_refractory: [0; 4],
            transient_armed: [true; 4],
            flux_avg: [0.0; 4],
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
    fn downmix_worker_produces_per_send_mono() {
        // Two device channels, two sends (one channel each). Fill the ring with a
        // distinguishable interleaved signal and confirm the worker downmixes each
        // send to mono, interleaved by send, post-gain.
        let frames = 4000;
        let (mut prod, cons) = HeapRb::<f32>::new(frames * 2 + 8).split();
        let mut interleaved = Vec::with_capacity(frames * 2);
        for _ in 0..frames {
            interleaved.push(0.5); // channel 0
            interleaved.push(-0.25); // channel 1
        }
        let pushed = prod.push_slice(&interleaved);
        assert_eq!(pushed, interleaved.len());

        let gains = Arc::new(GainBank::new(&[1.0, 2.0]));
        let (mut worker, mut reader) = AudioFeatureWorker::spawn(
            cons,
            SR,
            /* device_channels */ 2,
            vec![vec![0], vec![1]],
            gains,
        );
        assert_eq!(reader.send_count(), 2);
        assert_eq!(reader.sample_rate(), SR);

        let mut per_send = vec![Vec::new(), Vec::new()];
        for _ in 0..250 {
            reader.drain(&mut per_send);
            if per_send[0].len() >= frames {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        worker.stop();
        reader.drain(&mut per_send);

        assert!(
            per_send[0].len() >= frames - 64,
            "send 0 mono count {} (want ~{frames})",
            per_send[0].len()
        );
        assert_eq!(per_send[0].len(), per_send[1].len(), "sends stay in lockstep");
        // Send 0 = channel 0 (0.5) at unity; send 1 = channel 1 (-0.25) × 2 gain.
        assert!((per_send[0][100] - 0.5).abs() < 1e-6, "send0={}", per_send[0][100]);
        assert!((per_send[1][100] + 0.5).abs() < 1e-6, "send1={}", per_send[1][100]);
    }

    // ── Streaming analyzer (audio-layer realtime tap) ──

    #[test]
    fn streaming_analyzer_localizes_a_tone() {
        // A 1 kHz tone, several windows long, pushed in small chunks (as the tap
        // delivers it) must yield mid-band-dominant amplitude — the same result
        // the live worker gives (shared reduction path), proving the streamed
        // analysis IS the live analysis.
        let mono = sine(1000.0, nfft() * 4);
        let mut a = StreamingSendAnalyzer::new(SR, 250.0, 2000.0);
        for chunk in mono.chunks(257) {
            a.push(chunk);
        }
        let (_full, low, mid, high) = bands();
        let f = a.latest();
        assert!(
            f.bands[mid].amplitude > f.bands[low].amplitude
                && f.bands[mid].amplitude > f.bands[high].amplitude,
            "1 kHz tone lands in mid band: {:?}",
            f.bands.map(|b| b.amplitude),
        );
    }

    #[test]
    fn floor_gate_squelches_below_threshold() {
        // The same tone, analyzed with the floor off vs. a floor above any bin's
        // magnitude. Off → mid band has energy; a high floor zeroes every bin
        // before reduction, so the detected amplitude collapses to silence.
        let mono = sine(1000.0, nfft() * 4);
        let (_full, _low, mid, _high) = bands();

        let mut open = StreamingSendAnalyzer::new(SR, 250.0, 2000.0);
        for chunk in mono.chunks(257) {
            open.push(chunk);
        }
        let amp_open = open.latest().bands[mid].amplitude;
        assert!(amp_open > 0.0, "floor off: tone is detected ({amp_open})");

        let mut gated = StreamingSendAnalyzer::new(SR, 250.0, 2000.0);
        gated.set_floor_db(20.0); // lin floor = 10, above every normalized bin
        for chunk in mono.chunks(257) {
            gated.push(chunk);
        }
        assert_eq!(
            gated.latest().bands[mid].amplitude,
            0.0,
            "a floor above every bin squelches the whole column",
        );
    }

    #[test]
    fn streaming_analyzer_silent_until_first_hop() {
        let mut a = StreamingSendAnalyzer::new(SR, 250.0, 2000.0);
        // Fresh: no audio seen yet.
        assert_eq!(a.latest(), SendFeatures::default());
        // An empty push and a sub-hop push run no column, so still silent.
        a.push(&[]);
        a.push(&sine(1000.0, SpectrogramConfig::default().hop.max(1) / 2));
        assert_eq!(a.latest(), SendFeatures::default());
    }

    #[test]
    fn streaming_analyzer_decays_to_silence() {
        // Energy from a tone, then a long run of silence (a muted/paused layer
        // taps zeros): the mid-band amplitude must fall back toward zero, so the
        // modulation stops when the audio does.
        let mut a = StreamingSendAnalyzer::new(SR, 250.0, 2000.0);
        for chunk in sine(1000.0, nfft() * 4).chunks(257) {
            a.push(chunk);
        }
        let mid = bands().2;
        let loud = a.latest().bands[mid].amplitude;
        for chunk in vec![0.0f32; nfft() * 4].chunks(257) {
            a.push(chunk);
        }
        let quiet = a.latest().bands[mid].amplitude;
        assert!(quiet < loud, "silence should decay mid energy: {loud} -> {quiet}");
    }

    #[test]
    fn streaming_analyzer_sample_rate_round_trips() {
        let a = StreamingSendAnalyzer::new(SR, 250.0, 2000.0);
        assert_eq!(a.sample_rate(), SR);
    }

    #[test]
    fn streaming_analyzer_scope_emits_columns_in_lockstep() {
        let mut a = StreamingSendAnalyzer::new(SR, 250.0, 2000.0);
        let nb = a.num_bins();
        // Off by default → no columns buffered.
        for chunk in sine(1000.0, nfft() * 2).chunks(257) {
            a.push(chunk);
        }
        let mut none = 0;
        a.drain_scope_columns(|_| none += 1);
        assert_eq!(none, 0, "no columns buffered while scope is off");

        // On → columns + scalars buffer, one scalar set per num_bins column.
        a.set_scope(true);
        for chunk in sine(1000.0, nfft() * 4).chunks(257) {
            a.push(chunk);
        }
        let mut scal = 0;
        a.drain_scope_scalars(|_, _| scal += 1);
        let mut cols = 0;
        a.drain_scope_columns(|c| {
            cols += 1;
            assert_eq!(c.len(), nb, "each column is num_bins wide");
        });
        assert!(cols > 0, "scope buffers columns when enabled");
        assert_eq!(cols, scal, "one overlay-scalar set per column");

        // Drained → empty; disabling clears any residue.
        a.push(&sine(1000.0, nfft() * 2));
        a.set_scope(false);
        let mut after = 0;
        a.drain_scope_columns(|_| after += 1);
        assert_eq!(after, 0, "disabling scope clears buffered columns");
    }
}
