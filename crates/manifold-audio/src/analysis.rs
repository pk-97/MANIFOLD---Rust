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
use manifold_spectral::{CqtTransform, ScopeColumn, ScopeOnsets, SpectrogramConfig};

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
    /// Hops remaining in the Kick detector's own onset refractory (Low band
    /// only). Independent of `transient_refractory` so a general onset and a
    /// kick never debounce each other.
    kick_refractory: u8,
    /// Rolling history of the last `ODF_MEDIAN_HOPS` SuperFlux ODF values per band,
    /// newest last. Serves the peak-pick twice over: its MEDIAN is the adaptive
    /// threshold baseline (robust to the onset spikes it's compared against, where a
    /// moving average was inflated by them and lagged), and its tail is the past
    /// window the candidate must be a local maximum over. The candidate is the most
    /// recent entry (one hop back); the current hop is tested before it's pushed.
    /// Order [Full, Low, Mid, High].
    odf_hist: [[f32; ODF_MEDIAN_HOPS]; 4],
    /// Whether `prev_col` holds a real column yet (skips the startup flux spike).
    has_prev: bool,
    /// Per-band spectral-centroid height-from-bottom (0..1) for the scope overlay,
    /// indexed [Full, Low, Mid, High]; `-1` when the band isn't loud enough to
    /// characterise. Mirrors the `brightness` feature's gating, but mapped to the
    /// global display y so each band's centroid draws within its own region.
    centroid_yfb: [f32; 4],
    features: SendFeatures,
    /// D1 harmonic-sum salience scratch, `num_bins` long — computed ONCE per
    /// hop from the untilted, floored column, shared read-only by all four
    /// window trackers below (D4, `docs/AUDIO_OBJECT_TRACKING_DESIGN.md`).
    salience: Vec<f32>,
    /// Apex-masked column scratch for `salience_into` (BUG-043), `num_bins`
    /// long — pre-allocated here so the hot path stays allocation-free.
    salience_peaks: Vec<f32>,
    /// Per-window D5 ridge trackers, order [Full, Low, Mid, High] (D4).
    trackers: [RidgeTracker; 4],
    /// Low-band kick sweep-event detector (BUG-046 successor). Replaces the
    /// masked-novelty criterion; fires on a coherent descending ridge.
    kick_ridges: KickRidges,
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
// onset detection function (ODF) is the band sum of POSITIVE change in the LOG
// (dB) magnitude vs the previous column after max-filtering that column across
// ±`MAXFILTER_RADIUS` bins (see `band_reduce`). The dB domain is essential — it
// is the same dB the spectrogram paints, so a sustained band (a flat horizontal
// line) reads zero change and a loud sustained note can't out-fire a quiet real
// attack the way it did when flux was measured on linear magnitude. Plain flux
// fires on any energy rise — a bending
// note moves energy to an adjacent bin and reads as an attack; the max-filter
// already "covers" that neighbour, so only genuinely NEW broadband energy (a
// real attack) survives. The ODF is then PEAK-PICKED: a band fires only at a
// local maximum of its ODF — the candidate (one hop back) is the max over the last
// `ODF_PEAK_LOOKBACK` hops and the current hop has turned over — that clears a
// rolling MEDIAN of its own ODF by `SUPERFLUX_THRESH_FACTOR` plus a `SUPERFLUX_DELTA`
// floor (self-calibrating to the track's density, robust to its own spikes), past a
// short refractory. Picking the peak — not every hop the ODF sits above threshold —
// is what stops one attack from
// firing many times: a kick is a downward pitch sweep whose energy keeps moving
// into new bins, so its ODF parks high for the whole ~100 ms body; a crossing
// detector fired all the way down it, the peak-picker fires once at the attack.
// This replaced an energy-over-mean detector that tripped on amplitude wobble in
// busy mixes. Shared by triggers, the `Transients` modulation feature, and the
// scope — one detector, three readers.

/// How far the ODF must exceed its adaptive baseline (the rolling MEDIAN) to fire.
/// THE sensitivity knob — lower catches softer onsets, higher is stricter.
/// Raised 2.0 → 7.0 by the P3 sweep (2026-07-06,
/// `docs/AUDIO_OBJECT_TRACKING_DESIGN.md` P3, BUG-041): at 2.0 the dive/riser/
/// growl scenarios false-fired continuously (their timbre/tilt motion reads as
/// broadband dB flux even after the frequency max-filter); 7.0 is the minimal
/// value (paired with `SUPERFLUX_DELTA` 48.0) that silences all three while
/// leaving every real kick in `kicks`/`busymix` untouched — see the phase
/// report's sweep table for the full grid.
const SUPERFLUX_THRESH_FACTOR: f32 = 7.0;
/// Absolute floor on the adaptive threshold, in the ODF's units (the band sum of
/// positive dB-rise — a real attack is tens to hundreds; faint quiet-passage
/// flicker is a few units). The median × factor does the per-band self-scaling;
/// this δ is just the floor that stops near-silent flux from firing on the
/// multiplicative term when the median baseline is ≈ 0 (the old 1e-3 was dead in
/// these units). Active only in quiet passages, where band width barely differs,
/// so one value works across bands.
/// Raised 3.0 → 48.0 by the same P3 sweep — the plateau that clears the false
/// fires runs from ~30 to ~300 (real kicks start dropping out above ~400), so
/// 48.0 sits with margin on both sides against denser real material.
/// Raised 48.0 → 80.0 by the BUG-243 sweep (2026-07-18): sustained-pad flux
/// flickers ~80 ODF units against a ≈0 median baseline, clearing the old
/// 48.0 floor and firing 6+29 median-criterion transients (full+low bands)
/// on 20 s of pad-only material. Swept {80, 100, 125, 150} on the pad fixture
/// (`self_render/sustained_pad_100bpm`) via `causal_events` — 80 already
/// collapses full/low to 3 fires each (matching mid/high's untouched 3,
/// which are genuine swell attacks passing the NOVELTY criterion, out of
/// this knob's scope); 100/125/150 measured identical, so 80 is the minimal
/// value in the sweep set. Verified lossless on recall fixtures (edm_kit
/// 0.714 R / 1.000 P unchanged, kick_hat 0.785/1.000/0.646 unchanged, arp_16th
/// 252 unchanged) and on the P3 false-fire guards (dive/riser/growl fires
/// held at 0 or dropped; kicks/busymix/densemix fire counts unchanged at
/// 8/7/7 low-band). The remaining pad fires are kick-ridge false positives
/// (BUG-243 part B, unfixed — see `KICK_ABS_FLOOR`/`KICK_MIN_PEAK` below) and
/// the 3+3+3+3 novelty-admitted swell attacks, neither owned by this delta.
const SUPERFLUX_DELTA: f32 = 80.0;
/// BUG-044 novelty criterion (see the second-criterion comment in
/// [`reduce_send`]): how far the ODF candidate must exceed the recent-window
/// MAX to fire on dense material where the median test has self-raised past
/// real onsets. Swept 2026-07-06 on the ODF dumps of all 9 selftest scenarios
/// plus the 10 mix/drums fixtures (full fire-logic replay, exact match with
/// the live detector on all 10 entry counts): factor 1.5 leaks dive (3) and riser
/// (1); 1.75 leaks dive (1) at delta ≤ 100; 2.0 holds every false-fire guard
/// at 0 across delta 48–300; 2.25–3.0 also hold but bleed the recovered mixes
/// back toward deafness (feel 12→8→3 at delta 100). 2.0 sits on the guard
/// plateau's edge-with-margin while keeping recovery — the same "minimal value
/// that silences the guards" logic that picked THRESH_FACTOR 7.
const SUPERFLUX_NOVELTY_FACTOR: f32 = 2.0;
/// Absolute floor of the novelty criterion, in ODF units. Where the median
/// test's delta (48.0) guards near-silence, this guards SPARSE material: on a
/// quiet stem the recent max is also ~0, and factor × ~0 would re-admit the
/// small ghost bumps the P3 raise deliberately silenced. Swept 48–300 at
/// factor 2.0: 48–100 inflates already-healthy stems past their retention
/// bounds (busymix low 9, apricots drums +14); 150+ starts dropping densemix
/// kicks (7→6) and recovered mix fires (feel 10→8, tears 60→53); 125 is the
/// plateau point that keeps every guard and every recovery gate
/// simultaneously (kicks == 8, densemix 7/7, all three dead mixes recovered).
const SUPERFLUX_NOVELTY_DELTA: f32 = 125.0;
/// Novelty reference window bounds within the ODF history ring (newest entry
/// `ODF_MEDIAN_HOPS - 1` is the candidate itself): `hist[1..10]` = 7..15 hops
/// before the candidate. Upper bound excludes the candidate's own VQT-smeared
/// rise (~6 hops); lower bound excludes hist[0], where a previous REAL hit
/// sits at fast-16ths spacing (~85 ms) and would suppress legitimate runs.
/// Rationale detail in [`reduce_send`]'s second-criterion comment.
const ODF_NOVELTY_LO: usize = 1;
const ODF_NOVELTY_HI: usize = 10;
// ── Kick sweep-event detector (BUG-046 successor, docs/KICK_SWEEP_EVENT_DESIGN.md) ──
//
// A kick in a bass-occupied Low band survives only as its descending FM sweep
// (~120→45 Hz over ~90 ms ≈ 2 bins/hop at bpo=24), which SuperFlux's max-filter
// nulls BY DESIGN. So the detector is motion, not flux: it peak-picks the Low
// band, follows every local maximum as a ridge, and fires on a coherent descent.
// It REPLACES the masked-novelty criterion — proven to ~2x its kick recall on the
// 73-label corpus at equal bass-false-fire cost (P1 spike, hpss_proto.rs). Low
// band only (a Full-band tracker would fire on a spectrum-wide dive).
//
// The fire lands when the confirmation window FILLS, so `KICK_WIN` is the
// detector's structural latency (~5.3 ms/hop). The 2026-07-07 latency retune
// (`--family ridge-latency` + signed-offset grading vs the 73 attack labels)
// moved d14/w10 → d10/w6: median fire offset +31 → +9 ms (p90 +60 → +39),
// mix recall@±35ms 37→49/73, drums 38→53, all synth guards green (d14/w10
// failed kicks 7/8). The cost — mix false fires 58→115, concentrated on the
// label-README's "ambiguous 808/bass" tracks — is the evidence/latency trade:
// a shorter window sees less of the descent, and no cheaper discriminator
// exists (a birth-attack ODF gate was swept and falsified: bass masking hides
// kick attacks exactly where the ridge is needed). drop 10 in 6 hops =
// 1.67 bins/hop, still cleanly between the kick's ~2 and portamento's <1.
// These constants are the spike's `--family ridge-one --drop 10 --win 6
// --absfloor 0.005 --ridge-only` config; the runtime must reproduce its
// per-band fire counts exactly on the 48 kHz fixtures (the 44.1 kHz stems
// near-match: same BUG-052 grid, but the offline replay's window placement
// differs sub-hop from the streaming fade-in, flipping a few borderline
// events — see KICK_SWEEP_EVENT_DESIGN §retune).
const KICK_WIN: usize = 6; // descent-confirmation window (hops) = fire latency
const KICK_DROP_BINS: f32 = 10.0; // net descent required across the window (bins)
const KICK_STEP_MAX: f32 = 4.0; // max down-step per hop (2 bins/hop + slop)
/// BUG-243 part B (2026-07-18): swept 0.15/0.2/0.25/0.3 against the
/// `sustained_pad_100bpm` fixture's 30 kick-ridge false fires — ZERO effect at
/// any tested value (pad `kick_low` held at 30 all the way to 0.3, 2.5x the
/// default), while 0.25+ started costing `densemix`'s guard (kick_fires
/// 8→7→6, right at its `>= 6` floor). Root cause traced with
/// `MANIFOLD_KICK_DEBUG`: the pad's false-firing ridges have apex peaks
/// (~86–109 raw column units) squarely inside the same range as `edm_kit`'s
/// real kick ridges (~62–104) — this knob filters CANDIDATE peaks by
/// fraction-of-band-max, and the pad's ridges are not weak relative to their
/// own band, so no relative floor separates them from real kicks. Left at
/// its P3 (2026-07-06) value; not the fix for BUG-243B — see `KICK_ABS_FLOOR`.
const KICK_MIN_PEAK: f32 = 0.12; // ridge floor as a fraction of the band max
/// Absolute ridge-peak floor (tilted-column units), paired with the relative
/// `KICK_MIN_PEAK`. The relative floor scales down in quiet passages, so
/// near-silent filter-skirt ripple (a riser after its band ascends out of Low,
/// snare tails) still yields local maxima — and a 6-hop track can random-walk
/// down through that noise field to a false fire (riser guard 2→13 when the
/// window shortened). A kick apex is loud in absolute terms; skirt ripple is
/// not. Swept 2026-07-07 at d10/w6: 0.001 no effect, 0.005 riser 13→0 at ZERO
/// recall/latency cost, 0.02 kills a real synth kick (guard 8→7), 0.08
/// collapses recall — 0.005 is the plateau point with 4x margin to the cliff.
///
/// BUG-243 part B (2026-07-18) re-swept this same knob against the pad's 30
/// kick-ridge false fires and found NO safe value: `MANIFOLD_KICK_DEBUG`
/// showed the pad's false-firing ridge apexes (~86–109 raw units) and
/// `edm_kit`'s real kick apexes (~62–104) occupy the SAME magnitude range —
/// confirmed by sweeping 40/60/65/70/75/80 (all comfortably above the 2026-07
/// cliff, since this knob and that sweep's 0.001–0.08 units evidently predate
/// a column-scale change): every value from 40 up killed `edm_kit`'s
/// `kick_low` (15→0) and the `kicks`/`busymix`/`densemix` selftest guards
/// (8/7/8 → 0/0/0) in the same step that first touched the pad. There is no
/// floor that separates real kicks from this pad's false ridges — BUG-243
/// part B is UNFIXED via either documented knob; left at 0.005. See
/// `docs/BUG_BACKLOG.md` BUG-243 for the honest partial-fix status.
const KICK_ABS_FLOOR: f32 = 0.005;
const KICK_AGE_CAP: usize = KICK_WIN + 6; // reject long-lived (portamento) ridges
const KICK_MAX_GAP: u8 = 1; // a ridge may skip one hop before it dies
const KICK_MAX_TRACKS: usize = 12; // per-send bound on followed ridges

/// One followed ridge: its last `KICK_WIN` apex bins (newest at `len-1`), hops
/// since last extended, a once-per-descent latch, and its birth hop.
#[derive(Clone)]
struct KickTrack {
    bins: [f32; KICK_WIN],
    len: usize,
    gap: u8,
    fired: bool,
    birth: usize,
}

impl KickTrack {
    /// Append one apex bin, sliding the window once full (same shape as the ODF
    /// history ring's `copy_within`).
    fn extend(&mut self, bin: f32) {
        if self.len < KICK_WIN {
            self.bins[self.len] = bin;
            self.len += 1;
        } else {
            self.bins.copy_within(1.., 0);
            self.bins[KICK_WIN - 1] = bin;
        }
        self.gap = 0;
    }
}

/// Per-send kick-sweep state (Low band only). All scratch is pre-allocated —
/// the hot path (content thread via `StreamingSendAnalyzer`) never allocates
/// per hop.
struct KickRidges {
    tracks: Vec<KickTrack>,
    peaks: Vec<usize>,
    consumed: Vec<bool>,
    hop: usize,
}

impl KickRidges {
    fn new(num_bins: usize) -> Self {
        Self {
            tracks: Vec::with_capacity(KICK_MAX_TRACKS),
            peaks: Vec::with_capacity(num_bins),
            consumed: Vec::with_capacity(num_bins),
            hop: 0,
        }
    }

    /// Advance one Low-band hop. `col` is the full tilted column, `[lo,hi)` the
    /// Low band. Returns whether a kick ridge coherently descended this hop (the
    /// raw event); the caller gates it with the Kick refractory. This is the
    /// no-fallback Kick detector — no flux dedup, since Kick is now its own
    /// feature independent of `Transients` (the hybrid flux-OR-ridge path was
    /// retired). Called once per hop (Low band only), so `self.hop` is a faithful
    /// per-hop clock; birth/age are relative, so its origin is free.
    fn update(&mut self, col: &[f32], lo: usize, hi: usize) -> bool {
        let hop = self.hop;
        self.hop += 1;
        let hi = hi.min(col.len());

        // Peak-pick: Low-band local maxima above a fraction of the band max.
        let mut band_max = 0.0f32;
        for &v in &col[lo..hi] {
            if v > band_max {
                band_max = v;
            }
        }
        let floor = (band_max * KICK_MIN_PEAK).max(KICK_ABS_FLOOR);
        self.peaks.clear();
        for k in lo.max(1)..hi.saturating_sub(1) {
            let v = col[k];
            if v >= floor && v > col[k - 1] && v >= col[k + 1] {
                self.peaks.push(k);
            }
        }
        self.consumed.clear();
        self.consumed.resize(self.peaks.len(), false);

        // Extend each track with the nearest unconsumed peak in the descent
        // gate [last - step_max, last + 1].
        for tk in self.tracks.iter_mut() {
            let last = tk.bins[tk.len - 1];
            let mut best_j: Option<usize> = None;
            let mut best_d = f32::INFINITY;
            for (j, &pk) in self.peaks.iter().enumerate() {
                if self.consumed[j] {
                    continue;
                }
                let d = pk as f32 - last;
                if (-KICK_STEP_MAX..=1.0).contains(&d) && d.abs() < best_d {
                    best_d = d.abs();
                    best_j = Some(j);
                }
            }
            if let Some(j) = best_j {
                self.consumed[j] = true;
                tk.extend(self.peaks[j] as f32);
            } else {
                tk.gap += 1;
            }
        }

        // Fire: a full window that descended >= drop_bins coherently, once per
        // descent, born recently (the age cap rejects a late-bending portamento).
        let mut ridge_fire = false;
        for tk in self.tracks.iter_mut() {
            if tk.fired || tk.gap != 0 || tk.len < KICK_WIN || hop - tk.birth > KICK_AGE_CAP {
                continue;
            }
            let front = tk.bins[0];
            let back = tk.bins[KICK_WIN - 1];
            if front - back < KICK_DROP_BINS {
                continue;
            }
            let coherent = (1..KICK_WIN)
                .all(|w| (-KICK_STEP_MAX..=1.0).contains(&(tk.bins[w] - tk.bins[w - 1])));
            if coherent {
                tk.fired = true;
                ridge_fire = true;
                if std::env::var_os("MANIFOLD_KICK_DEBUG").is_some() {
                    let birth = tk.birth;
                    let len = tk.len;
                    let drop = front - back;
                    let peak = front;
                    let bins = &tk.bins[..len];
                    eprintln!(
                        "KICKDBG hop={hop} birth={birth} len={len} drop={drop:.3} peak={peak:.3} bins={bins:?}"
                    );
                }
            }
        }

        // Cull broken ridges; birth new ones from stray peaks.
        self.tracks.retain(|tk| tk.gap <= KICK_MAX_GAP);
        for j in 0..self.peaks.len() {
            if !self.consumed[j] {
                let mut tk =
                    KickTrack { bins: [0.0; KICK_WIN], len: 0, gap: 0, fired: false, birth: hop };
                tk.extend(self.peaks[j] as f32);
                self.tracks.push(tk);
            }
        }
        if self.tracks.len() > KICK_MAX_TRACKS {
            let n = self.tracks.len() - KICK_MAX_TRACKS;
            self.tracks.drain(0..n);
        }

        // The raw coherent-descent event. The per-track `fired` latch already
        // gives one fire per descent; the caller's Kick refractory debounces the
        // multi-hop confirmation. No flux dedup: Kick is a standalone detector.
        ridge_fire
    }
}
/// Frequency max-filter radius (bins) for vibrato suppression. The SuperFlux
/// paper uses ±1 bin at 24 bins/octave — wide enough to cover a semitone wobble.
/// P3 sweep (2026-07-06) tried 1/2/3: radius 1 always matched or beat wider
/// radii at the same threshold/delta, so it is unchanged.
const MAXFILTER_RADIUS: usize = 1;
/// Length of the rolling ODF history per band (~85 ms at hop ≈ 5.3 ms). Its MEDIAN
/// is the adaptive threshold baseline — robust to the onset spikes it is measured
/// against (an EMA was inflated by them and lagged), and causal so it adds no
/// latency. Also the buffer the peak-pick's past-window max reads from.
const ODF_MEDIAN_HOPS: usize = 16;
/// How many past hops the peak candidate must be the maximum over (~21 ms). A
/// 1-hop turnover fired on every small 2-hop bump a noisy ODF throws on a busy mix;
/// requiring the candidate to top the last few hops rejects those. Past-only data,
/// so it costs no latency — the fire still lands one hop after the true peak.
/// P3 sweep (2026-07-06) tried 4/8: no measurable effect at the chosen
/// threshold/delta, so left unchanged.
const ODF_PEAK_LOOKBACK: usize = 4;
/// Per-hop decay of the transient impulse (~100 ms settle at hop ≈ 5.3 ms).
const ONSET_DECAY: f32 = 0.85;
/// Below this band energy, relative flux reads 0 — avoids the flux ÷ energy ratio
/// blowing up on near-silence.
const FLUX_ENERGY_GATE: f32 = 1e-4;
/// Refractory after an onset (~32 ms at hop ≈ 5.3 ms) — SuperFlux's built-in
/// minimum inter-onset interval. Debounces one attack's multi-hop rise while
/// still allowing fast hat runs (≈1/32 at 160 BPM). Caps the rate at ~30/s.
const ONSET_REFRACTORY_HOPS: u8 = 6;
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
/// Slope is the single [`SpectrogramConfig::tilt_slope`] the display shader also
/// reads, so applying it once to the raw magnitudes makes every reduction — and
/// the floor — read the exact tilted dB the user sees painted.
///
/// Public so `examples/mod_harness.rs` can form the exact tilted column from
/// the (floored, untilted) scope column it drains, without duplicating this
/// formula — see the harness's per-hop salience overlay
/// (`docs/AUDIO_OBJECT_TRACKING_DESIGN.md` P1).
pub fn tilt_weights(cfg: &SpectrogramConfig, sample_rate: f32, num_bins: usize) -> Vec<f32> {
    let nb = num_bins.max(1);
    let fmin = cfg.fmin.max(1.0);
    let flr = (cfg.effective_fmax(sample_rate) / fmin).log2();
    let inv = if nb > 1 { 1.0 / (nb - 1) as f32 } else { 0.0 };
    (0..nb)
        .map(|k| {
            let tilt_db = cfg.tilt_slope * flr * (k as f32 * inv - 0.5);
            10.0f32.powf(tilt_db / 20.0)
        })
        .collect()
}

// ── Salience — D1, docs/AUDIO_OBJECT_TRACKING_DESIGN.md ─────────────────────
//
// "Which peak is the perceptual fundamental" over the exact tilted, floored
// VQT column the scope draws and the bands reduce (P1's whole point: no
// second transform, no divergence between what's seen and what modulates).
// A wide supersaw or a growl smears energy around each harmonic; summing
// across the harmonic stack makes the fundamental dominant even when no
// single bin is the loudest.

/// D1 harmonic weights `w_h` for `h = 1..=5` (fundamental through 5th
/// harmonic) — starting values, tuned against the eval set in P1.
const SALIENCE_WEIGHTS: [f32; 5] = [1.0, 0.8, 0.6, 0.45, 0.35];

/// Apex-mask radius (bins) for [`salience_into`]'s peak pass (BUG-043): a
/// bin survives into the comb's input only if it is within one bin of a
/// local maximum over ±this radius. 4 = half the comb's minimum tooth
/// spacing at bpo 24 (`off_5 − off_4` = 8 bins) — the smallest radius that
/// still guarantees one smeared mound can't hand two adjacent teeth two
/// "independent" readings — while leaving simultaneous objects a minor
/// third apart (6 bins) both standing.
const PEAK_MASK_RADIUS: usize = 4;

/// Harmonic-sum salience (D1): `S[k] = Σ_{h=1..5} w_h · P[k + off_h]`, where
/// `off_h = round(bpo · log2(h))` — bins are geometric, so harmonic `h` above
/// bin `k` is a *fixed* bin offset regardless of `k` (0, 24, 38, 48, 56 at
/// `bpo` 24). `col` must be the untilted, floored column — the same data the
/// bands read. Bins beyond `col`'s end contribute 0 (saturating access, no
/// wraparound). `peaks` is caller-provided scratch; both it and `out` must
/// equal `col.len()`.
///
/// `P` is `col` **apex-masked** (BUG-043, 2026-07-06): `P[j] = col[j]` where
/// `j` lies within one bin of a strict local maximum over ±[`PEAK_MASK_RADIUS`],
/// else 0 — the comb reads spectral PEAKS, never skirt. The sum's founding
/// assumption is that each tooth samples an independent spectral object; at
/// the transform's bottom octaves the 4096-sample kernels are far under-Q
/// (a 45 Hz fundamental smears over ~40 bins at >50% magnitude), so a
/// subharmonic candidate's teeth — spaced only 8–14 bins — could all land
/// inside the ONE smeared mound and out-sum the true bin (measured: S[15 Hz
/// ghost] 0.70 vs S[45 Hz true] 0.52, `sub_45hz` test). Masking to apexes
/// restores the property that makes the harmonic sum correct at all: a
/// sub-octave ghost collects each true harmonic at strictly LOWER weight
/// than the true fundamental collects it (`w_{2h} < w_h`), so the true bin
/// always wins. The ±1 dilation keeps the apex's own two neighbours so
/// [`refine_peak`]'s parabolic fit still sees the true peak shape, and
/// covers the ≤0.5-bin rounding of `off_h`.
///
/// Deliberately **not normalized** per hop — the absolute scale is what lets
/// the presence feature read tracked-bin salience against its neighbourhood
/// (D6); normalizing here would erase that signal before it exists.
pub fn salience_into(col: &[f32], bpo: usize, peaks: &mut [f32], out: &mut [f32]) {
    debug_assert_eq!(col.len(), out.len(), "salience_into: out must match col in length");
    debug_assert_eq!(col.len(), peaks.len(), "salience_into: peaks scratch must match col in length");
    let n = col.len();
    peaks.fill(0.0);
    for k in 0..n {
        let v = col[k];
        if v <= 0.0 {
            continue;
        }
        let lo = k.saturating_sub(PEAK_MASK_RADIUS);
        let hi = (k + PEAK_MASK_RADIUS + 1).min(n);
        let is_apex = (lo..hi).all(|j| j == k || col[j] <= v);
        if is_apex {
            if k > 0 {
                peaks[k - 1] = col[k - 1];
            }
            peaks[k] = v;
            if k + 1 < n {
                peaks[k + 1] = col[k + 1];
            }
        }
    }
    let bpof = bpo as f32;
    for (k, s) in out.iter_mut().enumerate() {
        let mut sum = 0.0f32;
        for (i, &w) in SALIENCE_WEIGHTS.iter().enumerate() {
            let h = (i + 1) as f32;
            let off = (bpof * h.log2()).round() as usize;
            if let Some(&v) = peaks.get(k + off) {
                sum += w * v;
            }
        }
        *s = sum;
    }
}

/// Parabolic-refine bin `k` against its immediate neighbours (clamped at the
/// array edges), returning `(fractional_bin, value)`. Shared by
/// [`salience_peak`] (the global peak) and [`local_peaks`] (the tracker's
/// per-window peak-pick, D5 step 1) — one refine formula, two callers.
fn refine_peak(salience: &[f32], k: usize) -> (f32, f32) {
    let n = salience.len();
    let v = salience[k];
    let km1 = k.saturating_sub(1);
    let kp1 = (k + 1).min(n - 1);
    let delta = if km1 != k && kp1 != k {
        let (y0, y1, y2) = (salience[km1], salience[k], salience[kp1]);
        let denom = y0 - 2.0 * y1 + y2;
        if denom.abs() > 1e-12 { (0.5 * (y0 - y2) / denom).clamp(-1.0, 1.0) } else { 0.0 }
    } else {
        0.0
    };
    (k as f32 + delta, v)
}

/// The dominant salience peak: the global maximum, refined to a fractional
/// bin by parabolic interpolation over its two neighbours (clamped to the
/// valid range at the array edges). Returns `(fractional_bin, peak_value)`,
/// or `None` when the column is fully floored (all-zero salience — every
/// magnitude non-negative, so the max is exactly 0 only in that case).
pub fn salience_peak(salience: &[f32]) -> Option<(f32, f32)> {
    if salience.is_empty() {
        return None;
    }
    let (mut best_k, mut best_v) = (0usize, salience[0]);
    for (k, &v) in salience.iter().enumerate().skip(1) {
        if v > best_v {
            best_k = k;
            best_v = v;
        }
    }
    if best_v <= 0.0 {
        return None;
    }
    Some(refine_peak(salience, best_k))
}

/// Local maxima of `salience` within the half-open range `[lo, hi)`, refined
/// to fractional bins (D5 step 1 — a tracker's window peak-pick). A bin
/// qualifies if it is strictly positive (a floored column reads 0 — a local
/// max of 0 isn't a peak) and strictly greater than each in-window neighbour;
/// at a window edge the missing outer neighbour doesn't disqualify it, so a
/// genuine peak riding the window boundary is still found. The parabolic
/// refine itself may reach one bin outside `[lo, hi)` (the true neighbour),
/// which only sharpens the estimate — it never changes which bins qualify.
fn local_peaks(salience: &[f32], lo: usize, hi: usize) -> Vec<(f32, f32)> {
    let hi = hi.min(salience.len());
    let mut out = Vec::new();
    if lo >= hi {
        return out;
    }
    for k in lo..hi {
        let v = salience[k];
        if v <= 0.0 {
            continue;
        }
        let is_left_peak = k == lo || salience[k - 1] < v;
        let is_right_peak = k + 1 >= hi || salience[k + 1] < v;
        if is_left_peak && is_right_peak {
            out.push(refine_peak(salience, k));
        }
    }
    out
}

/// The peak with the greatest value in a `local_peaks` result, or `None` if
/// empty. Ties keep the first (lowest-bin) candidate.
fn strongest_peak(peaks: &[(f32, f32)]) -> Option<(f32, f32)> {
    peaks.iter().copied().fold(None, |best, cand| match best {
        None => Some(cand),
        Some((_, bv)) if cand.1 > bv => Some(cand),
        _ => best,
    })
}

// ── Tracker — D5, docs/AUDIO_OBJECT_TRACKING_DESIGN.md ──────────────────────
//
// Bounded slew, challenger hysteresis, hold-then-release. One `RidgeTracker`
// per window (Full/Low/Mid/High, D4) runs over the ONE shared salience column
// computed above — the tracker never re-derives salience, and never reads its
// own past *output* features, only this hop's salience + this hop's transient
// fire (docs §6: "no second transform", "not reading latest()").

/// Continuation radius (bins): a peak within this of `pos` is "the same
/// object moving", eligible for slewed continuation (D5 step 2) rather than
/// challenger/takeover treatment (D5 step 3).
const SLEW_RADIUS: f32 = 6.0;
/// Max bins a single hop's continuation snap may move `pos` (D5 step 2) — a
/// 2-octave/s glide at a ~5.3 ms hop is ~0.5 bins/hop, so this bounds
/// teleports, not real motion.
const MAX_SLEW: f32 = 3.0;
/// A challenger elsewhere must out-salience the continuation peak by this
/// ratio to start the takeover clock (D5 step 3).
const CHALLENGE_RATIO: f32 = 1.5;
/// Consecutive hops a challenger must hold `CHALLENGE_RATIO` before the
/// tracker jumps to it (~64 ms at hop ≈ 5.3 ms) — kills one-hop flicker to a
/// passing element without adding lag to a real takeover.
const CHALLENGE_HOPS: u8 = 12;
/// Hops of dropout (no acceptable peak, no completed takeover) tolerated
/// before the tracker goes inactive (~200 ms at hop ≈ 5.3 ms, D5 step 5).
const HOLD_HOPS: u8 = 38;
/// Consecutive apex-consistent hops the post-onset re-acquire window waits
/// for before jumping (BUG-042 third design, 2026-07-06): the transform's
/// peak POSITION parks within ±0.3 bin ~3 hops after an attack even though
/// its relative STRENGTH needs ~12 (the measured fact both rejected fix
/// shapes ignored). The window itself stays open CHALLENGE_HOPS (~12) — 4×
/// slack over this streak, the exact slack whose absence broke rejected
/// shape 2 (window == streak froze pos permanently when settle noise ate a
/// hop).
const SETTLE_STREAK: u8 = 3;
/// Presence one-pole attack time constant, seconds (D6 recalibration
/// 2026-07-06): trust is earned over ~a tenth of a second of sustained
/// evidence. Noise rejection is NOT this constant's job — the stability
/// weight on the target (see `RidgeTracker::stability`) crushes wandering
/// (noise-following) trackers structurally, so the attack only needs to be
/// slow enough that a single spiky hop doesn't register.
const PRESENCE_ATTACK_S: f32 = 0.100;
/// Presence one-pole release time constant, seconds — slightly slower than
/// attack so a masked beat dips rather than strobes (D5's "lost slowly").
const PRESENCE_RELEASE_S: f32 = 0.150;

/// Per-window tracker state (D3 data model), owned by [`SendState`]. `pos` is
/// a fractional bin index into the SAME array `salience_into` fills — the
/// window's `[lo, hi)` bounds only scope which peaks this tracker may
/// continue/takeover to; the position itself lives in the shared column's
/// coordinate space so the per-send Full-tracker Hz conversion
/// (`fmin · 2^(pos/bpo)`) needs no window offset.
#[derive(Clone, Copy, Debug)]
struct RidgeTracker {
    /// Fractional bin, HOLDS its last value on dropout — pitch is a position,
    /// never a null (D5/D6).
    pos: f32,
    /// Tracker confidence 0..1, one-pole smoothed (D6).
    presence: f32,
    /// Hops since the last accepted peak (continuation, onset re-acquire, or
    /// a completed takeover). `active` clears once this exceeds `HOLD_HOPS`.
    hold: u8,
    /// Bin of the current takeover challenger, carried across hops so
    /// `challenger_hops` counts CONSECUTIVE hops from (about) the same
    /// challenger, not any peak that momentarily clears the ratio.
    challenger_bin: f32,
    /// Consecutive hops the challenger has out-scored the continuation peak
    /// by `CHALLENGE_RATIO`. Resets on takeover, on the challenger moving more
    /// than `SLEW_RADIUS` bins from `challenger_bin`, or when nothing clears
    /// the ratio this hop.
    challenger_hops: u8,
    /// False until the first acquisition (D5 step 6) — no continuation or
    /// takeover runs before then, only the acquisition test.
    active: bool,
    /// Position stability 0..1 from the last matched hop: unified distance
    /// law `1 − |Δpos|/MAX_SLEW` (clamped 0..1), `Δpos` = new pos − previous
    /// pos, applied on EVERY hop that moves `pos` — continuation, takeover,
    /// AND onset re-acquire alike (unified 2026-07-06, real-clip finding,
    /// `docs/AUDIO_OBJECT_TRACKING_DESIGN.md` D6/P2c: a bassline re-attacking
    /// the SAME note on a fresh onset is the SAME tracked object, not a new
    /// one, so it must keep trust, not reset it — the old unconditional 0 on
    /// onset re-acquire meant presence never accumulated on real note
    /// material, only on the synthetics' continuous tones). Acquisition from
    /// `!active` is the one exception, unconditionally 0 — there is no prior
    /// `pos` to be near. The unified law still crushes takeover trust in
    /// practice (a takeover challenger is by construction > `SLEW_RADIUS`,
    /// hence > `MAX_SLEW`, bins away, so it still reads 0) and still crushes
    /// noise-following jitter at the slew limit; only a genuine re-attack at
    /// (about) the same pitch newly keeps trust across a jump. Multiplies the
    /// presence target (D6): temporal continuity is the discriminator the
    /// per-hop salience ratio structurally can't provide (a swept-noise
    /// column shows genuine octave-contrast spikes on ~35% of hops at any
    /// radius).
    stability: f32,
    /// The window's memoryless salience argmax last hop (fractional bin) —
    /// the apex-consistency factor's sole state (see `update`). Unlike `pos`
    /// it follows nothing and holds nothing: it is wherever the window's
    /// strongest peak actually was.
    last_apex: f32,
    /// Hops the post-onset re-acquire window stays open (BUG-042); 0 =
    /// closed. Opened to CHALLENGE_HOPS by a transient fire.
    reacquire_hops: u8,
    /// Consecutive in-window hops the apex has stayed within MAX_SLEW of
    /// `settle_anchor`; the jump happens at SETTLE_STREAK (strength-agnostic
    /// — position evidence only).
    settle_streak: u8,
    /// Last hop's continuation candidate bin (strongest in-radius peak,
    /// accepted or not); `NEG_INFINITY` when last hop had none. The
    /// super-slew static-snap below compares against it.
    last_cont_bin: f32,
    /// The bin the current settle streak STARTED at. Each streak hop must
    /// stay within MAX_SLEW of this anchor, not of the previous hop — the
    /// post-attack spectral splash DRIFTS slowly (~1-3 bins/hop, measured on
    /// `notes`: 125→100 Hz over ~6 hops before the fundamental resolves),
    /// so hop-to-hop consistency reads true on garbage; cumulative
    /// displacement from the anchor is what breaks it. A genuinely parked
    /// apex completes the streak; anything still moving re-anchors.
    settle_anchor: f32,
}

impl RidgeTracker {
    fn new() -> Self {
        Self {
            pos: 0.0,
            presence: 0.0,
            hold: 0,
            // Guarantees the FIRST real challenger is always treated as new
            // (an unconditional distance-reset), rather than accidentally
            // matching the zero-valued default bin.
            challenger_bin: f32::NEG_INFINITY,
            challenger_hops: 0,
            active: false,
            stability: 0.0,
            // Same trick as `challenger_bin`: the first observed apex always
            // reads inconsistent (one hop of zero presence target), never
            // accidentally consistent with bin 0.
            last_apex: f32::NEG_INFINITY,
            last_cont_bin: f32::NEG_INFINITY,
            reacquire_hops: 0,
            settle_streak: 0,
            settle_anchor: f32::NEG_INFINITY,
        }
    }

    /// One hop's D5 update for this window. `salience` is the shared column;
    /// `col` is the untilted, floored VQT column `salience` was computed from
    /// (D6 recalibration's presence ratio reads the tracked bin's own raw
    /// magnitude from it directly — see `presence_target`); `lo`/`hi` this
    /// window's bin range; `transient_fired` whether THIS band's onset fired
    /// THIS hop (D5 step 4, read from `reduce_send`'s output, never from the
    /// tracker's own prior output); `bpo` bins-per-octave (D6's presence
    /// neighbourhood radius — see `presence_target`); `dt` the hop period in
    /// seconds (presence one-pole time base).
    fn update(&mut self, salience: &[f32], col: &[f32], lo: usize, hi: usize, transient_fired: bool, bpo: usize, dt: f32) {
        let peaks = local_peaks(salience, lo, hi);

        // Apex position-consistency (BUG-043 riser follow-up, 2026-07-06):
        // is the window's memoryless argmax still where it was last hop
        // (within the existing MAX_SLEW motion bound)? A real object's apex
        // is self-consistent hop to hop (measured: sub/growl < 0.3 bins/hop,
        // a gliding dive ~0.2); band-noise's apex is not, at ANY frequency
        // (measured on the riser: 10-20 bins/hop, every window of the clip).
        // This is the discriminator the tracked `pos` structurally cannot
        // provide: continuation parks `pos` on nearby residue (small delta,
        // HIGH stability) while the window's real maximum jumps around
        // elsewhere. Binary, judged fresh each hop with an apex - a genuine
        // note-jump reads inconsistent for exactly one hop (a ~3% one-pole
        // dip), noise reads inconsistent nearly every hop and presence never
        // accumulates. Silent hops pass no judgment: the dropout path below
        // already decays presence on its own.
        let apex = strongest_peak(&peaks);
        let consistency = match apex {
            Some((bin, _)) => {
                let ok = (bin - self.last_apex).abs() <= MAX_SLEW;
                self.last_apex = bin;
                if ok { 1.0 } else { 0.0 }
            }
            None => 1.0,
        };

        // Step 6: acquisition (inactive → active). Requires salience > 0,
        // already guaranteed by `local_peaks`.
        if !self.active {
            if let Some((bin, _)) = apex {
                self.pos = bin;
                self.active = true;
                self.hold = 0;
                self.challenger_hops = 0;
                // The one exception to the unified distance law below: no
                // prior `pos` exists yet to measure Δ against.
                self.stability = 0.0;
            }
            let target = if self.active {
                self.stability * dominance(salience, self.pos, lo, hi) * consistency * presence_target(salience, col, self.pos, bpo)
            } else {
                0.0
            };
            self.step_presence(target, dt);
            return;
        }

        // Step 4 as amended (BUG-042 third design, 2026-07-06): an onset
        // OPENS a re-acquire window instead of teleporting. The fire-hop
        // apex is garbage — the transform needs ~1-3 hops for the new
        // peak's POSITION to park (and ~12 for its strength, which is why
        // strength-based challenge is too slow for 8th notes). While the
        // window is open `pos` HOLDS (a bassline re-striking the same note
        // — the overwhelmingly common case — therefore reads correctly
        // through the attack), continuation and takeover below keep running
        // (nothing freezes: rejected shape 2's fatal flaw), and the jump
        // happens on position evidence alone: SETTLE_STREAK consecutive
        // apex-consistent hops, however weak the apex still is relative to
        // the old peak. Both rejected shapes are recorded in BUG_BACKLOG
        // (now Fixed) / the design doc P2c record — do not resurrect them.
        if transient_fired && apex.is_some() {
            self.reacquire_hops = CHALLENGE_HOPS;
            self.settle_streak = 0;
            self.settle_anchor = f32::NEG_INFINITY;
        }
        if self.reacquire_hops > 0 {
            self.reacquire_hops -= 1;
            if let Some((bin, apex_val)) = apex {
                if (bin - self.settle_anchor).abs() > MAX_SLEW {
                    self.settle_anchor = bin;
                    self.settle_streak = 1;
                } else {
                    self.settle_streak = self.settle_streak.saturating_add(1);
                }
                // The jump needs BOTH: a parked apex (position evidence, the
                // streak) AND decisiveness against what `pos` still holds —
                // the SAME CHALLENGE_RATIO bar the takeover path uses, so
                // the window is an accelerated takeover clock (3 parked hops
                // instead of 12) and never a lowered strength bar. On a real
                // re-attack the held bin is dead residue (S[pos] ≈ 0) and
                // any real note passes immediately; on a still-sounding
                // object (the dive's fundamental during a warm-up-artifact
                // fire) a briefly-parked harmonic is refused — measured:
                // without this clause one hop-18 fire teleported the dive
                // tracker 19 st onto a fade-in harmonic.
                let held_bin = (self.pos.round().max(0.0) as usize).min(salience.len().saturating_sub(1));
                let held_val = salience[held_bin];
                if self.settle_streak >= SETTLE_STREAK && apex_val > held_val * CHALLENGE_RATIO {
                    let delta = bin - self.pos;
                    self.pos = bin;
                    self.hold = 0;
                    self.challenger_hops = 0;
                    self.reacquire_hops = 0;
                    // Unified distance law (D6/P2c): a re-attack near the
                    // held position is the SAME object and keeps trust; a
                    // genuine jump (the octave note) re-earns it.
                    self.stability = (1.0 - (delta.abs() / MAX_SLEW)).clamp(0.0, 1.0);
                    let target = self.stability * dominance(salience, self.pos, lo, hi) * consistency * presence_target(salience, col, self.pos, bpo);
                    self.step_presence(target, dt);
                    return;
                }
            }
            // Window open, streak not complete: fall through — continuation,
            // takeover, and dropout behave exactly as on any other hop.
        }

        // Step 2: continuation — the strongest peak within `SLEW_RADIUS`.
        let continuation = strongest_peak(
            &peaks.iter().copied().filter(|&(bin, _)| (bin - self.pos).abs() <= SLEW_RADIUS).collect::<Vec<_>>(),
        );

        let this_cont_bin = continuation.map(|(b, _)| b).unwrap_or(f32::NEG_INFINITY);
        if let Some((cbin, cont_val)) = continuation {
            // Step 3: takeover — a stronger peak OUTSIDE the radius must
            // out-salience THIS hop's real continuation value (never a
            // trivial pass against nothing — see the note below) for
            // `CHALLENGE_HOPS` consecutive hops.
            let challenger = strongest_peak(
                &peaks.iter().copied().filter(|&(bin, _)| (bin - self.pos).abs() > SLEW_RADIUS).collect::<Vec<_>>(),
            );
            let mut took_over = false;
            if let Some((xbin, xval)) = challenger {
                if xval > cont_val * CHALLENGE_RATIO {
                    if (xbin - self.challenger_bin).abs() > SLEW_RADIUS {
                        self.challenger_hops = 1;
                    } else {
                        self.challenger_hops = self.challenger_hops.saturating_add(1);
                    }
                    self.challenger_bin = xbin;
                    if self.challenger_hops >= CHALLENGE_HOPS {
                        let delta = xbin - self.pos;
                        self.pos = xbin; // no slew clamp on the takeover hop itself
                        self.hold = 0;
                        self.challenger_hops = 0;
                        // Same unified distance law as onset re-acquire — a
                        // takeover challenger is by construction outside
                        // SLEW_RADIUS (> MAX_SLEW), so this reads 0 in
                        // practice; it's one law, not a special case.
                        self.stability = (1.0 - (delta.abs() / MAX_SLEW)).clamp(0.0, 1.0);
                        took_over = true;
                    }
                } else {
                    self.challenger_hops = 0;
                }
            } else {
                self.challenger_hops = 0;
            }
            if !took_over {
                let delta = cbin - self.pos;
                if delta.abs() <= MAX_SLEW {
                    self.pos += delta;
                    self.hold = 0;
                    // Continuity IS the confidence signal: a still or slowly
                    // gliding object reads ~1; noise-following jitter at the slew
                    // limit reads ~0. Uses the existing MAX_SLEW — no new constant.
                    self.stability = 1.0 - (delta.abs() / MAX_SLEW).clamp(0.0, 1.0);
                } else if (cbin - self.last_cont_bin).abs() <= MAX_SLEW {
                    // Super-slew but STATIC: a real peak sitting in the dead
                    // zone between MAX_SLEW and SLEW_RADIUS (e.g. the
                    // fundamental returning ~4 bins away after a tremolo
                    // trough) — adopt it directly. A moving super-slew
                    // candidate never qualifies: it was somewhere else last
                    // hop.
                    let delta = cbin - self.pos;
                    self.pos = cbin;
                    self.hold = 0;
                    self.stability = (1.0 - (delta.abs() / MAX_SLEW)).clamp(0.0, 1.0);
                } else {
                    // Super-slew AND moving: NOT the same object gliding —
                    // MAX_SLEW is ~12 oct/s, no real glide approaches it —
                    // so HOLD instead of chasing it at the clamp (BUG-042
                    // follow-up, 2026-07-06). The measured failure this
                    // deletes: after every note release the transform's
                    // kernel ring-down presents an accelerating downward
                    // artifact (0.1 → 1 → 3 → 4+ bins/hop); the old
                    // clamp-and-follow dragged pos ~7 st below the note in
                    // the gap, so every next attack started badly wrong.
                    // Holding parks near the note instead, and the attack's
                    // own continuation/window recovers immediately.
                    self.hold = self.hold.saturating_add(1);
                    if self.hold > HOLD_HOPS {
                        self.active = false;
                    }
                }
            }
        } else {
            // Step 5: dropout — no peak within SLEW_RADIUS of `pos` this hop.
            // `pos` HOLDS; presence decays below. Deliberately does NOT touch
            // `challenger_hops`: with no live continuation value there is
            // nothing real to out-salience, so a challenger cannot accrue
            // credit from beating "nothing" (that was the bug this comment
            // replaces — an empty continuation used to make ANY peak,
            // however faint, an unconditional challenger). A single flickered
            // hop mid-takeover doesn't lose progress either; a genuinely
            // vanished object still surfaces via HOLD_HOPS → inactive →
            // acquisition, or via onset re-acquire (step 4), never via this
            // path.
            self.hold = self.hold.saturating_add(1);
            if self.hold > HOLD_HOPS {
                self.active = false;
            }
        }

        self.last_cont_bin = this_cont_bin;

        let target = if self.active {
            self.stability * dominance(salience, self.pos, lo, hi) * consistency * presence_target(salience, col, self.pos, bpo)
        } else {
            0.0
        };
        self.step_presence(target, dt);
    }

    /// One-pole toward `target`: attack tau while rising, release tau while
    /// falling (D5/D6) — trust is earned fast, lost slowly.
    fn step_presence(&mut self, target: f32, dt: f32) {
        let tau = if target > self.presence { PRESENCE_ATTACK_S } else { PRESENCE_RELEASE_S };
        let alpha = if tau > 0.0 { 1.0 - (-dt / tau).exp() } else { 1.0 };
        self.presence += (target - self.presence) * alpha;
    }
}

/// D6 presence recalibration (docs/AUDIO_OBJECT_TRACKING_DESIGN.md D6,
/// "finding 2" of the P2 shipped-status paragraph, closed by this task).
/// `window_energy` (raw magnitude energy, summed over the whole window) was
/// the wrong denominator on two counts, both measured against
/// `selftest_dive.png` 2026-07-06: (1) it mixes scales — the numerator is a
/// harmonic-SUM salience value (~5 weighted terms) while the denominator was
/// a wide window's total RAW energy (dozens of real harmonic bins for a
/// buzzy source), so even a flawlessly tracked object reads a tiny ratio
/// (growl measured 0.02–0.08); (2) it has no opinion on WHERE a peak's
/// salience came from, so a subharmonic ghost — a bin whose comb offsets
/// (`salience_into`'s `off_h`) land on REAL energy that actually belongs to a
/// higher, out-of-window fundamental — reads high confidence off a
/// near-silent window (the dive's Low-band phantom below ~3 s, ground truth
/// still up around 1200 Hz: h=5's offset lands its weight on the real
/// fundamental bin while the ghost's own bin carries none of it).
///
/// **Rejected candidate 1** (kept here so it isn't retried): a
/// "concentration" ratio `S[pos] / Σ S[window]` (tracked peak's share of the
/// window's total salience MASS), optionally gated by an in-window
/// comb-support fraction. Measured on a synthetic single dominant, fully
/// comb-supported object (`dominant_fully_supported_object_reads_high_presence`):
/// presence 0.246, well under the 0.5 display bar. Real-signal diagnostics
/// against the selftest columns confirmed it fails for the wrong reason:
/// `salience_into` is a harmonic-SUM over EVERY bin, so ANY window — tonal or
/// not — ends up with most of its 266 bins carrying some nonzero salience
/// (measured 145–220 "significant" bins per hop, every scenario alike), and
/// `concentration` came out equally tiny for growl (mean 0.0202) and riser
/// (mean 0.0102) — it does not discriminate coherent from broadband content
/// at all, let alone clear the 0.5 bar.
///
/// **Rejected candidate 2:** a purely local ratio `S[pos] / (col[pos] · Σw_h)`
/// — the tracked peak's salience against a theoretical ceiling if every
/// harmonic matched its own raw magnitude (`Σw_h`, the D1 weight vocabulary's
/// own sum ≈ 3.2). Scale-consistent and correctly gates the ghost
/// (`col[pos] ≈ 0` collapses it), but measured against the real harness
/// (`cargo run --example mod_harness -- --selftest`): riser's Full presence
/// was ≤0.15 on 0% of hops (need ≥90%) and kicks' Low presence exceeded 0.5
/// on 51.7% of hops (need ≤20%) — riser and kicks scored HIGHER than growl
/// (mean ratio 1.56 and 1.07 vs growl's 0.53). Diagnosis: a swept bandpass
/// (riser) or a VQT-smeared transient (kicks) both leave comparable raw
/// magnitude at the bins the D1 comb offsets land on relative to `col[pos]`
/// itself — "the harmonics are about as loud as the fundamental" is true for
/// both a genuine harmonic stack AND a locally-flat noise/transient region,
/// so a same-bin ceiling can't tell them apart.
///
/// **Shipped (candidate 3):** what candidates 1–2 were both missing is a
/// reference to the CANDIDATE'S OWN NEIGHBOURHOOD rather than the whole
/// window or the same bin: does this bin's salience stand out from the
/// typical salience one octave around it? A genuine harmonic lock is sharp in
/// salience-space even where the raw column is locally smooth (measured:
/// raw-column peakedness never separated growl from riser at any radius 1–6;
/// salience-space peakedness does, cleanly, from radius ≈ 1 octave up).
/// `presence = clamp((S[pos] − mean(S[pos±bpo], excluding pos)) / S[pos], 0, 1)`,
/// gated to 0 when `col[pos]` itself is negligible (the design brief's named
/// ghost check: a bin with no real energy of its own can't be "present" no
/// matter how its neighbourhood compares). `bpo` (bins-per-octave) is not a
/// new tuned constant — it is the transform's own existing parameter, reused
/// as "compare against one octave of context," and the choice sits on a
/// measured plateau: radii of 16, 20, and 24 bins (bpo is 24 in this
/// transform) all separate growl/dive/busymix (94–100% of post-acquisition
/// hops ≥ 0.5) from kicks (90–100% of hops ≤ 0.15) equally well — it is not a
/// knife-edge single value.
/// - **Problem 1 (scale):** both terms are salience-space, and the
///   neighbourhood is sized to the object itself (one octave), so it never
///   dilutes against unrelated content far away in a wide window (unlike
///   candidate 1) and never depends on how "rich" the object's own harmonic
///   series looks (unlike candidate 2).
/// - **Problem 2 (ghost):** `col[pos]` gate, same mechanism as candidate 2 —
///   a subharmonic ghost's own bin carries no real energy, so it can never
///   read present regardless of how its neighbourhood computes.
///
/// Known tension (measured, not fixed by this formula — see the task
/// report): riser's raw per-hop ratio clears 0.15 on a genuine, roughly
/// radius-INDEPENDENT ~32–39% of hops (band-limited noise really does
/// produce locally-peaky salience some of the time; the swept passband is
/// wide enough that "one octave of context" sometimes sits entirely inside
/// it). The existing D5 one-pole (attack 30 ms / release 250 ms, untouched)
/// absorbs isolated spikes; whether it absorbs ENOUGH of them to clear the
/// riser gate is a real measurement, reported with the other P2/P2b numbers,
/// not asserted here.
/// Window-dominance factor on the presence target (BUG-043 follow-up,
/// 2026-07-06): `S[pos] / max(S over [lo, hi))`, clamped 0..1 — presence
/// requires the tracked peak to BE the window's dominant salience object.
/// No new tuned constant: it is a pure ratio against the window's own max.
///
/// Why it exists: apex-masking the comb input (see [`salience_into`]) fixed
/// the sub-octave ghost but made salience sparse for EVERY input, so the D6
/// neighbourhood-contrast ratio alone stopped discriminating noise — a
/// broadband mound (riser) also yields one sharp salience spike now.
/// Measured on the riser: the memoryless window apex wanders ~20 bins/hop
/// (vs < 0.3 for sub/growl), but the tracker doesn't follow it — continuation
/// keeps it parked on small noise residue near its old pos (small Δpos, so
/// the stability term stays HIGH — stability measures motion of the tracked
/// pos, not whether that pos matters). The wandering true apex is exactly
/// what this ratio sees: a tracker sitting on residue while the window's
/// real maximum jumps around elsewhere reads ~0 most hops, and the presence
/// one-pole never accumulates. A genuine lock (sub, growl, notes, dive) IS
/// the window max, ratio ≈ 1, unchanged. A brief out-dominance (a kick hop
/// inside busymix's Full window) dips the target for a hop or two and the
/// one-pole absorbs it — the same masking-tolerance argument as D5's
/// hold-then-release.
fn dominance(salience: &[f32], pos: f32, lo: usize, hi: usize) -> f32 {
    let k = (pos.round().max(0.0) as usize).min(salience.len().saturating_sub(1));
    let s_pos = salience.get(k).copied().unwrap_or(0.0);
    if s_pos <= 0.0 {
        return 0.0;
    }
    let hi = hi.min(salience.len());
    let win_max = salience[lo.min(hi)..hi].iter().copied().fold(0.0f32, f32::max);
    if win_max <= 0.0 { 0.0 } else { (s_pos / win_max).clamp(0.0, 1.0) }
}

fn presence_target(salience: &[f32], col: &[f32], pos: f32, bpo: usize) -> f32 {
    let k = (pos.round().max(0.0) as usize).min(salience.len().saturating_sub(1));
    let peak_col = col.get(k).copied().unwrap_or(0.0);
    if peak_col <= FLUX_ENERGY_GATE {
        return 0.0;
    }
    let peak_s = salience.get(k).copied().unwrap_or(0.0);
    if peak_s <= 0.0 {
        return 0.0;
    }
    let r = bpo; // one octave of context — see doc comment
    let nb_lo = k.saturating_sub(r);
    let nb_hi = (k + r + 1).min(salience.len());
    let (mut nb_sum, mut nb_n) = (0.0f32, 0usize);
    for (b, &v) in salience[nb_lo..nb_hi].iter().enumerate() {
        if nb_lo + b != k {
            nb_sum += v;
            nb_n += 1;
        }
    }
    let nb_mean = if nb_n > 0 { nb_sum / nb_n as f32 } else { 0.0 };
    ((peak_s - nb_mean) / peak_s).clamp(0.0, 1.0)
}

/// The four tracker windows in band order [Full, Low, Mid, High] — the SAME
/// bin ranges `reduce_send`'s band split uses (D4: "the band cell scopes the
/// tracker's search window").
fn tracker_windows(num_bins: usize, low_bin: usize, mid_bin: usize) -> [(usize, usize); 4] {
    [(0, num_bins), (0, low_bin), (low_bin, mid_bin), (mid_bin, num_bins)]
}

/// Run all four windows' D5 update for one hop and fill `pitch`/`presence` on
/// each band plus the per-send reserved pitch fields from the Full tracker
/// (D4). `vqt_raw` is the untilted, floored column salience was computed from
/// (also D6's presence ghost-gate source, `presence_target`); `bpo`/`fmin`
/// come from the shared `SpectrogramConfig` (the same formula family
/// `band_edges` uses); `dt` is the hop period in seconds.
fn update_trackers(
    send: &mut SendState,
    vqt_raw: &[f32],
    num_bins: usize,
    low_bin: usize,
    mid_bin: usize,
    bpo: usize,
    fmin: f32,
    dt: f32,
) {
    let windows = tracker_windows(num_bins, low_bin, mid_bin);
    for (wi, &(lo, hi)) in windows.iter().enumerate() {
        let hi = hi.min(vqt_raw.len());
        let fired = send.features.bands[wi].transients > 0.999;
        let prev_pos = send.trackers[wi].pos;
        send.trackers[wi].update(&send.salience, vqt_raw, lo, hi, fired, bpo, dt);

        let bf = &mut send.features.bands[wi];
        if hi > lo + 1 {
            bf.pitch = ((send.trackers[wi].pos - lo as f32) / (hi - 1 - lo) as f32).clamp(0.0, 1.0);
        }
        bf.presence = send.trackers[wi].presence;

        if wi == 0 {
            // Full tracker fills the per-send reserved fields (D4).
            let bpof = bpo as f32;
            send.features.pitch_hz = fmin * 2f32.powf(send.trackers[wi].pos / bpof);
            let delta_bins = send.trackers[wi].pos - prev_pos;
            let hops_per_sec = if dt > 0.0 { 1.0 / dt } else { 0.0 };
            let bins_per_semitone = bpof / 12.0;
            send.features.pitch_delta_st =
                if bins_per_semitone > 0.0 { delta_bins * hops_per_sec / bins_per_semitone } else { 0.0 };
            send.features.pitch_confidence = send.trackers[wi].presence;
        }
    }
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
fn band_reduce(
    col: &[f32],
    prev: &[f32],
    lo: usize,
    hi: usize,
    db_min: f32,
    db_max: f32,
) -> BandReduce {
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
        //
        // The difference is taken in the LOG (dB) domain — the SAME dB the
        // spectrogram paints — not in linear magnitude. This is the load-bearing
        // detail: a sustained band is a FLAT horizontal line on the dB scope, so it
        // must read zero change. In linear magnitude a loud band's natural wobble
        // scales with its absolute level (a 1 dB ripple at −10 dB is a far bigger
        // number than the same ripple at −40 dB), so loud sustained notes false-fired
        // while quiet real attacks barely registered. In dB the ODF is loudness-
        // invariant: it measures RELATIVE change, so a flat line reads ~0 regardless
        // of how loud it is, and only genuine attacks (big dB jumps) clear threshold.
        let klo = k.saturating_sub(MAXFILTER_RADIUS);
        let khi = (k + MAXFILTER_RADIUS + 1).min(n_bins);
        let mut prev_max = 0.0f32;
        for &p in &prev[klo..khi] {
            if p > prev_max {
                prev_max = p;
            }
        }
        let m_db = (20.0 * m.max(1e-9).log10()).clamp(db_min, db_max);
        let prev_db = (20.0 * prev_max.max(1e-9).log10()).clamp(db_min, db_max);
        let ds = m_db - prev_db;
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
/// High]. The column is already floored upstream (sub-floor bins zeroed), so a
/// band below the floor reads zero energy and every feature returns 0 for it
/// naturally — the single floor is the only gate, there is no separate presence
/// test. Flux/onset features only run once a real predecessor exists
/// ([`SendState::has_prev`], set only after the window has filled), so warm-up
/// never fires a spurious onset.
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
        // The column is already floored (sub-floor bins zeroed upstream), so every
        // reduction reads only above-floor energy — the exact content the user sees.
        // No hidden gate: a band below the floor has zero energy, and the feature
        // math returns 0 for it naturally. The single floor is the only gate.
        let bf = &mut send.features.bands[bi];
        bf.amplitude = r.amplitude;
        bf.brightness = r.brightness;
        bf.noisiness = r.noisiness;

        // Scope overlay: the centroid the `brightness` feature reads, mapped from
        // band-local 0..1 onto the global display y. Hidden (`-1`) when the band has
        // no above-floor energy, so the trace is drawn only where the user sees
        // colour — never over a black (floored) band.
        send.centroid_yfb[bi] = if r.energy > 0.0 && hi > lo + 1 {
            let c = lo as f32 + r.brightness * (hi - 1 - lo) as f32;
            (c / (num_bins.max(1) - 1) as f32).clamp(0.0, 1.0)
        } else {
            -1.0
        };

        if have_prev {
            // Liveliness self-scales with density (relative plain flux).
            bf.liveliness = relative_flux(r.flux, r.energy);

            // Transient = SuperFlux PEAK-PICK. The ODF (max-filtered positive dB
            // flux) is a curve over time; a real onset is a PEAK on it — the ODF
            // rises into the attack, then falls. The candidate is last hop's ODF
            // (`hist`'s newest entry); we fire it when:
            //   • it is a LOCAL MAXIMUM over the past `ODF_PEAK_LOOKBACK` hops AND no
            //     lower than the current hop (it has turned over) — a wider test than
            //     a 1-hop turnover, which rejects the small 2-hop bumps a noisy ODF
            //     throws on a busy mix, at no latency cost (all past data);
            //   • it clears the adaptive threshold — the rolling MEDIAN of the ODF
            //     history times `SUPERFLUX_THRESH_FACTOR`, plus the `SUPERFLUX_DELTA`
            //     floor. The median (not a mean) ignores the onset spikes it's
            //     compared against, so a run of hits doesn't inflate the baseline and
            //     mask the softer ones;
            //   • we're past the refractory.
            // Firing only at the local maximum — not on every hop the ODF sits above
            // threshold — is what stops one attack (a kick's whole sweeping body
            // parks the ODF high for ~100 ms) from firing many times. A floored band
            // reads superflux 0, so the floor — not a separate gate — keeps silent
            // bands silent. Fires one hop (~5 ms) after the true peak; imperceptible.
            let odf = r.superflux;
            let hist = &send.odf_hist[bi];
            let candidate = hist[ODF_MEDIAN_HOPS - 1];

            let mut sorted = *hist;
            sorted.sort_unstable_by(f32::total_cmp);
            let median = sorted[ODF_MEDIAN_HOPS / 2];
            let threshold = median * SUPERFLUX_THRESH_FACTOR + SUPERFLUX_DELTA;

            let lookback_lo = ODF_MEDIAN_HOPS - 1 - ODF_PEAK_LOOKBACK;
            let past_max = hist[lookback_lo..ODF_MEDIAN_HOPS - 1]
                .iter()
                .copied()
                .fold(0.0f32, f32::max);
            let is_peak = candidate >= past_max && odf <= candidate;

            // BUG-044 second criterion — NOVELTY vs the recent ODF max. The median
            // test alone goes deaf on dense mixes: continuous broadband change (a
            // full production's every envelope moving at once) parks the ODF median
            // high, and median × 7 self-raises past real kick rises (feel mix fired
            // 1× in 11 s while its drums stem fired 32×). The rescue cannot be a
            // median walk-back (BUG-041), so a genuine attack is admitted by a
            // second, OR'd test: the candidate must dwarf the LARGEST ODF seen in
            // the recent window — dense-but-steady material cannot inflate that
            // reference to kick size, while every continuous false-firer can.
            // Measured on the 2026-07-06 ODF dumps: growl's tilt wobble spikes to
            // 1259 every ~5 hops, so its recent max ≈ its peaks and novelty never
            // fires it; same for dive's beating (peaks 687, neighbours 598) and
            // riser's noise (481 vs 413). A real kick over the densemix bed reads
            // 300–530 against a recent max of ~100, and feel/apricots/tears mix
            // kicks read 500–1600 against floors of 250–400.
            //
            // Window geometry (indices into `hist`, newest at ODF_MEDIAN_HOPS-1 =
            // the candidate): the reference max is taken over hist[1..10], i.e.
            // 7..15 hops before the candidate. The 6 hops just before the candidate
            // are EXCLUDED — the attack's own VQT-kernel-smeared rise lives there
            // and must not become its own suppressor. hist[0] (16 hops ≈ 85 ms
            // back) is also excluded: at 174 BPM a 16th-note grid is ~86 ms, so a
            // previous real hit sits right at that edge and would suppress legit
            // hits in fast drum runs (measured: including it cost tears mix 3
            // fires with no guard improvement).
            let novelty_ref = hist[ODF_NOVELTY_LO..ODF_NOVELTY_HI]
                .iter()
                .copied()
                .fold(0.0f32, f32::max);
            let novel =
                candidate > novelty_ref * SUPERFLUX_NOVELTY_FACTOR + SUPERFLUX_NOVELTY_DELTA;

            // Transients: a plain SuperFlux onset on every band, Low included.
            // The kick sweep is NOT folded in here anymore — it is its own
            // `Kick` feature (below), so a general onset and a kick can be bound
            // to different targets and never block each other's refractory.
            let refr = send.transient_refractory[bi];
            let fired = is_peak && refr == 0 && (candidate > threshold || novel);
            if std::env::var_os("MANIFOLD_ODF_DEBUG").is_some() {
                eprintln!(
                    "ODFDBG band={bi} candidate={candidate:.1} median={median:.1} threshold={threshold:.1} novelty_ref={novelty_ref:.1} novel={novel} is_peak={is_peak} refr={refr} fired={fired}"
                );
            }
            if fired {
                bf.transients = 1.0;
                send.transient_refractory[bi] = ONSET_REFRACTORY_HOPS;
            } else {
                bf.transients *= ONSET_DECAY;
                send.transient_refractory[bi] = refr.saturating_sub(1);
            }

            // Kick — the descending-FM-ridge detector, Low band only
            // (docs/KICK_SWEEP_EVENT_DESIGN.md). On a bass-heavy Low band the ODF
            // median and recent max are owned by the bassline, so a kick clears
            // neither flux test above; its one distinguishing trace is the
            // coherent pitch descent, which SuperFlux nulls by design. Ridge-only,
            // no fallback (a bass note's fixed-pitch attack can't fake a descent),
            // with its own refractory so it's independent of `Transients`. The
            // tracker advances every Low hop; the fire is gated by the Kick
            // refractory — this reproduces the prototype `--ridge-only` reference.
            if bi == 1 {
                let ridge_fire = send.kick_ridges.update(&send.col, lo, hi);
                let kick_refr = send.kick_refractory;
                if ridge_fire && kick_refr == 0 {
                    bf.kick = 1.0;
                    send.kick_refractory = ONSET_REFRACTORY_HOPS;
                } else {
                    bf.kick *= ONSET_DECAY;
                    send.kick_refractory = kick_refr.saturating_sub(1);
                }
            }

            // Push the current ODF into the history ring (newest last).
            let h = &mut send.odf_hist[bi];
            h.copy_within(1.., 0);
            h[ODF_MEDIAN_HOPS - 1] = odf;
        }
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
        kick_refractory: 0,
        odf_hist: [[0.0; ODF_MEDIAN_HOPS]; 4],
        has_prev: false,
        centroid_yfb: [-1.0; 4],
        features: SendFeatures::default(),
        salience: vec![0.0; num_bins],
        salience_peaks: vec![0.0; num_bins],
        trackers: [RidgeTracker::new(); 4],
        kick_ridges: KickRidges::new(num_bins),
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
    scope_scalars: Vec<ScopeColumn>,
    /// D7 (simplified for P2): gates the D5 tracker on/off. Default OFF so an
    /// untouched project's analysis is byte-identical to the pre-tracker path
    /// (checked by `pitch_tracking_disabled_matches_untouched_path` below);
    /// the harness turns it on. The in-app activation OR-gate is P4's job.
    pitch_tracking: bool,
    /// The single audio floor (dB). Bins whose TILTED magnitude is below this are
    /// zeroed in BOTH the scope and feature column before display and reduction, so
    /// what you see (black) is exactly what every algorithm reads (silence). It is a
    /// GATE only — it never moves the colour ramp (`db_min`/`db_max` are fixed
    /// contrast), and is clamped to `db_min` so it can't black out content the
    /// detector still sees. `FLOOR_DB_OFF` resolves to `db_min` (no cut).
    floor_db: f32,
}

impl StreamingSendAnalyzer {
    /// Build for `sample_rate` (the rate samples are pushed at — the mixer's
    /// output rate, not the source file's) and the project's Low/Mid/High
    /// crossovers (Hz). Same crossovers a live send reads, so the analyses match.
    pub fn new(sample_rate: u32, low_hz: f32, mid_hz: f32) -> Self {
        let sr = sample_rate as f32;
        // BUG-052: derive hop/n_fft from the device rate so a hop is always
        // ~5.33 ms and the window ~85 ms — every hop-count tuning constant below
        // (kick descent, ODF median, refractories, tracker slew) is then valid at
        // any sample rate without resampling. No-op at 48 kHz.
        let spec_config = SpectrogramConfig::default().with_time_grid_for(sr);
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
            pitch_tracking: false,
            floor_db: manifold_core::audio_setup::FLOOR_DB_OFF,
        }
    }

    /// Set the single audio floor (dB). Applied live every hop; no rebuild.
    /// `FLOOR_DB_OFF` (or anything at/below it) resolves to the config `db_min`.
    pub fn set_floor_db(&mut self, floor_db: f32) {
        self.floor_db = floor_db;
    }

    /// Turn the D5 pitch/presence tracker on/off (D7, simplified for P2).
    /// Default OFF: an untouched project's analysis stays byte-identical to
    /// the pre-tracker path. When off, salience and all four trackers are
    /// skipped entirely each hop — `pitch`/`presence` stay 0.
    pub fn set_pitch_tracking(&mut self, on: bool) {
        self.pitch_tracking = on;
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

    /// Hop size in SAMPLES at this analyzer's rate — the BUG-052 rate-scaled
    /// value, NOT `SpectrogramConfig::default().hop`. Consumers deriving a
    /// per-hop time base (CSV time axis, bar grids) must use this: at 44.1 kHz
    /// the default's 256 is 8.8% wrong (the real hop is 235 ≈ 5.33 ms).
    pub fn hop(&self) -> usize {
        self.hop
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

    /// Drain buffered overlay records in lockstep with the columns — one
    /// [`ScopeColumn`] (centroid traces + onset tick lanes) per scope column.
    pub fn drain_scope_scalars(&mut self, mut f: impl FnMut(ScopeColumn)) {
        for s in self.scope_scalars.drain(..) {
            f(s);
        }
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
        let sample_rate = self.sample_rate;
        let pitch_tracking = self.pitch_tracking;
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
        // Hop period in seconds — the D5 presence one-pole's time base.
        let dt = hop as f32 / sample_rate.max(1.0);

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
        // `db_min`/`db_max` are the FIXED colour-ramp + amplitude contrast — NOT the
        // floor. The floor is a separate gate that only ZEROS the column below it; it
        // never rescales the colourmap (coupling them made the floor act as a
        // contrast knob — lowering it blew every colour out hot). The floor is
        // clamped to `db_min`: below the ramp bottom it would black out content the
        // detector still sees (mismatch) and reveal nothing. Off → db_min (no cut).
        let db_min = spec_config.db_min;
        let db_max = spec_config.db_max;
        let floor_db = if floor_db > manifold_core::audio_setup::FLOOR_DB_OFF {
            floor_db.max(db_min)
        } else {
            db_min
        };
        let lin_floor = 10f32.powf(floor_db / 20.0);

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
            // The single floor: zero every bin whose TILTED magnitude is below the
            // floor, in BOTH the scope (`vqt_raw`) and feature (`state.col`) column,
            // so the black the user sees on the spectrogram is exactly the silence
            // the bands + transients detect. Decided in the tilted domain (`*c`) —
            // the same tilted dB the shader paints — so the floor line matches the
            // picture. A zeroed bin paints black (mag 0 → below the fixed ramp), so
            // black = zeroed = silent. The floor only zeros; it does NOT move the
            // colour ramp (`db_min`), so raising it cuts from the bottom without
            // recolouring what's above.
            for (raw, c) in vqt_raw.iter_mut().zip(state.col.iter_mut()) {
                if *c < lin_floor {
                    *raw = 0.0;
                    *c = 0.0;
                }
            }
            reduce_send(state, nb, *low_bin, *mid_bin, db_min, db_max);
            // Same guard `reduce_send` used internally for flux/transients
            // (captured before the has_prev update just below) — the D5
            // tracker's warm-up gate (D4: "so the zero-padded fade-in never
            // acquires a ghost").
            let have_prev = state.has_prev;
            state.prev_col.copy_from_slice(&state.col);
            // Flux/transients arm only once the window has filled, so the warm-up
            // ramp never reads as a transient (matches the live worker).
            state.has_prev = state.window.len() >= n_fft;

            // D5 tracker (docs/AUDIO_OBJECT_TRACKING_DESIGN.md P2): salience is
            // computed ONCE per hop from the untilted, floored column (D1 as
            // amended), then each of the four windows' trackers update from
            // that one shared array (D4). Gated on `pitch_tracking` (D7,
            // simplified for P2 — off by default, the harness turns it on) and
            // `have_prev` — an untouched/disabled project's other five
            // features are unaffected either way (this never touches
            // `state.col`/`prev_col`/the existing band fields).
            if pitch_tracking && have_prev {
                salience_into(vqt_raw, spec_config.bpo, &mut state.salience_peaks, &mut state.salience);
                update_trackers(state, vqt_raw, nb, *low_bin, *mid_bin, spec_config.bpo, spec_config.fmin, dt);
            }

            // Scope capture: buffer the raw (untilted) column + overlay scalars,
            // exactly what the live worker pushes to its scope rings — the shader
            // applies its own display tilt. Drained by the runtime each tick.
            if *scope {
                scope_cols.extend_from_slice(vqt_raw);
                let b = &state.features.bands;
                // Scope onset ticks: mark only the hop a transient actually FIRED
                // (impulse == 1.0), NOT its ~5-hop decay tail. The decaying impulse
                // feeds modulation (smooth); on the scope it smeared each hit across
                // ~5 columns into a solid carpet that read as far busier than the
                // real fire rate. One column per fire = the true rate, visible.
                let fired = |t: f32| if t > 0.999 { 1.0 } else { 0.0 };
                scope_scalars.push(ScopeColumn {
                    centroids: state.centroid_yfb,
                    onsets: ScopeOnsets {
                        low: fired(b[1].transients),
                        mid: fired(b[2].transients),
                        high: fired(b[3].transients),
                    },
                });
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
    fn onset_odf_is_loudness_invariant() {
        // The fix for "loud sustained notes fire as transients": the SuperFlux ODF is
        // computed in the dB domain, so a given FRACTIONAL level step produces the same
        // ODF whether the band is loud or quiet. That is exactly why a loud sustained
        // band (small fractional wobble) can no longer out-fire a quiet real attack. The
        // old linear-domain ODF scaled with absolute level — the loud step would read
        // ~10x the quiet one and false-fire. Same +3.5 dB step (×1.5) at two levels:
        let c = cfg();
        let nb = nbins();
        let loud = band_reduce(&vec![0.45f32; nb], &vec![0.30f32; nb], 0, nb, c.db_min, c.db_max).superflux;
        let quiet = band_reduce(&vec![0.045f32; nb], &vec![0.030f32; nb], 0, nb, c.db_min, c.db_max).superflux;
        assert!(loud > 0.0 && quiet > 0.0, "a +3.5 dB step is a positive ODF: loud {loud}, quiet {quiet}");
        assert!(
            (loud - quiet).abs() / loud < 0.02,
            "same fractional step → same dB ODF regardless of loudness: loud {loud}, quiet {quiet}"
        );
    }

    #[test]
    fn floored_band_reports_no_timbre() {
        // Single-floor contract: a band reads 0 brightness/noisiness because the
        // FLOOR zeroed its bins, not because of a hidden presence gate (deleted with
        // ONSET_AMP_GATE). A floor above every bin blacks out the whole column, so
        // every band's timbre collapses to silence — what you see (black) is exactly
        // what a modulator mapped there reads (nothing).
        let mono = sine(1000.0, nfft() * 4);
        let (_full, low, mid, high) = bands();
        let mut a = StreamingSendAnalyzer::new(SR, 250.0, 2000.0);
        a.set_floor_db(20.0); // lin floor 10, above every (tilted) bin
        for chunk in mono.chunks(257) {
            a.push(chunk);
        }
        let f = a.latest();
        for b in [low, mid, high] {
            assert_eq!(f.bands[b].brightness, 0.0, "floored band brightness = 0");
            assert_eq!(f.bands[b].noisiness, 0.0, "floored band noisiness = 0");
        }
    }

    #[test]
    fn floored_band_fires_no_transient() {
        // The reported bug, pinned: a band whose energy is below the floor must fire
        // NO transient — the floor is the only gate. Floor off → the onset fires;
        // floor above the tone → the column is zeroed, no peak, no fire (no ticks on
        // a band that reads black).
        let mut attack = vec![0.0f32; nfft() * 2];
        attack.extend(sine(1000.0, nfft() * 2)); // silence, then a 1 kHz onset
        let (_full, _low, mid, _high) = bands();

        let mut open = StreamingSendAnalyzer::new(SR, 250.0, 2000.0);
        let mut fired_open = false;
        for chunk in attack.chunks(257) {
            open.push(chunk);
            if open.latest().bands[mid].transients > 0.5 {
                fired_open = true;
            }
        }
        assert!(fired_open, "floor off: the onset fires a transient");

        let mut gated = StreamingSendAnalyzer::new(SR, 250.0, 2000.0);
        gated.set_floor_db(20.0);
        let mut fired_gated = false;
        for chunk in attack.chunks(257) {
            gated.push(chunk);
            if gated.latest().bands[mid].transients > 0.5 {
                fired_gated = true;
            }
        }
        assert!(!fired_gated, "floored onset must not fire — the floor is the only gate");
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

        let silence = vec![0.0f32; nb];
        let slide = band_reduce(&col, &prev_shifted, 0, nb, c.db_min, c.db_max);
        assert!(slide.flux > 0.5, "plain flux trips on the bin shift: {}", slide.flux);
        assert!(
            slide.superflux < 1e-6,
            "SuperFlux's max-filter covers the neighbour, so a 1-bin slide reads ~0: {}",
            slide.superflux,
        );

        // A real attack from silence still produces strong SuperFlux.
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
            kick_refractory: 0,
            odf_hist: [[0.0; ODF_MEDIAN_HOPS]; 4],
            has_prev: true,
            centroid_yfb: [-1.0; 4],
            features: SendFeatures::default(),
            salience: vec![0.0; nb],
            salience_peaks: vec![0.0; nb],
            trackers: [RidgeTracker::new(); 4],
            kick_ridges: KickRidges::new(nb),
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
            kick_refractory: 0,
            odf_hist: [[0.0; ODF_MEDIAN_HOPS]; 4],
            has_prev: true,
            centroid_yfb: [-1.0; 4],
            features: SendFeatures::default(),
            salience: vec![0.0; nb],
            salience_peaks: vec![0.0; nb],
            trackers: [RidgeTracker::new(); 4],
            kick_ridges: KickRidges::new(nb),
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
            kick_refractory: 0,
            odf_hist: [[0.0; ODF_MEDIAN_HOPS]; 4],
            has_prev: true,
            centroid_yfb: [-1.0; 4],
            features: SendFeatures::default(),
            salience: vec![0.0; nb],
            salience_peaks: vec![0.0; nb],
            trackers: [RidgeTracker::new(); 4],
            kick_ridges: KickRidges::new(nb),
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
    fn downward_pitch_sweep_fires_once_not_per_hop() {
        // The reported over-fire, faithfully: a kick is a downward PITCH SWEEP. Its
        // energy moves into new (lower) bins every hop — faster than the max-filter's
        // ±1-bin reach — so the SuperFlux ODF stays high for the whole sweep. A
        // crossing detector fired all the way down it (the tick-cluster-per-kick on a
        // clean kick); the peak-picker must fire ONCE at the attack, not per hop.
        let c = cfg();
        let nb = nbins();
        let (low_bin, mid_bin) = band_edges(&c, SR as f32, nb, 250.0, 2000.0);
        let full = bands().0;
        let n = nfft();

        let mut s = new_send_state(nb);
        s.has_prev = true;

        // Settle the baseline on silence, then a 28-hop glide 350 Hz -> 45 Hz (a
        // long 808, ~150 ms) at full level. Count Full-band fires across the sweep.
        let silence = vec![0.0f32; nb];
        for _ in 0..8 {
            s.col.copy_from_slice(&silence);
            reduce_send(&mut s, nb, low_bin, mid_bin, c.db_min, c.db_max);
            s.prev_col.copy_from_slice(&s.col);
        }

        let sweep_hops = 28;
        let mut fires = 0;
        for h in 0..sweep_hops {
            let t = h as f32 / (sweep_hops - 1) as f32;
            let freq = 350.0 * (45.0f32 / 350.0).powf(t); // geometric glide
            s.col.copy_from_slice(&vqt_col(&sine(freq, n)));
            reduce_send(&mut s, nb, low_bin, mid_bin, c.db_min, c.db_max);
            if s.features.bands[full].transients > 0.99 {
                fires += 1;
            }
            s.prev_col.copy_from_slice(&s.col);
        }
        assert!(fires >= 1, "the sweep's attack must fire: {fires}");
        assert!(
            fires <= 2,
            "a single downward sweep must fire once, not per hop: {fires} over {sweep_hops} hops",
        );
    }

    // ── Kick sweep-event detector (BUG-046 successor) ─────────────────────────
    // A single-bin column with the ridge at `bin` (value 1.0, rest 0) — the
    // cleanest stimulus for the descent discriminators.
    fn ridge_col(nb: usize, bin: usize) -> Vec<f32> {
        let mut c = vec![0.0f32; nb];
        c[bin] = 1.0;
        c
    }

    #[test]
    fn kick_ridges_fires_on_coherent_descent() {
        // A ridge falling ~2 bins/hop (the kick sweep rate) clears KICK_DROP_BINS
        // within KICK_WIN and fires exactly once (the per-track latch).
        let nb = 120;
        let mut kr = KickRidges::new(nb);
        let mut fires = 0;
        for h in 0..20i32 {
            let bin = (90 - 2 * h).max(1) as usize;
            if kr.update(&ridge_col(nb, bin), 0, nb) {
                fires += 1;
            }
        }
        assert_eq!(fires, 1, "a coherent descending ridge fires exactly once: {fires}");
    }

    #[test]
    fn kick_ridges_ignores_static_slow_and_late_bends() {
        let nb = 120;
        // Static ridge (a held bass note): zero descent, never fires.
        let mut kr = KickRidges::new(nb);
        let mut any = false;
        for _ in 0..24 {
            any |= kr.update(&ridge_col(nb, 80), 0, nb);
        }
        assert!(!any, "a static ridge must not fire");

        // Slow descent ~0.5 bin/hop (bass portamento): can't reach KICK_DROP_BINS
        // inside KICK_WIN — rate/extent rejects it.
        let mut kr = KickRidges::new(nb);
        let mut any = false;
        for h in 0..24i32 {
            let bin = (90 - h / 2).max(1) as usize;
            any |= kr.update(&ridge_col(nb, bin), 0, nb);
        }
        assert!(!any, "a slow portamento descent must not fire");

        // Late bend: a ridge held static for 15 hops, THEN a fast descent. Its
        // age at the descent far exceeds KICK_AGE_CAP — the age cap rejects it
        // even though the descent itself has kick rate/extent.
        let mut kr = KickRidges::new(nb);
        let mut any = false;
        for _ in 0..15 {
            any |= kr.update(&ridge_col(nb, 90), 0, nb);
        }
        for h in 0..15i32 {
            let bin = (90 - 2 * h).max(1) as usize;
            any |= kr.update(&ridge_col(nb, bin), 0, nb);
        }
        assert!(!any, "a late-bending (long-lived) ridge must not fire — age cap");
    }

    #[test]
    fn kick_ridge_rearms_for_a_second_descent() {
        // Two separate kicks: a coherent descent, a gap of static/silence long
        // enough to retire the first ridge, then a second descent. Each fires its
        // own raw event — the detector re-arms per descent (new track born at the
        // second attack), it doesn't latch shut after the first.
        let nb = 120;
        let mut kr = KickRidges::new(nb);
        let mut fires = 0;
        let descent = |kr: &mut KickRidges, fires: &mut u32| {
            for h in 0..12i32 {
                let bin = (90 - 2 * h).max(1) as usize;
                if kr.update(&ridge_col(nb, bin), 0, nb) {
                    *fires += 1;
                }
            }
        };
        descent(&mut kr, &mut fires);
        // Silence gap: the first ridge's tracks die (gap > KICK_MAX_GAP).
        for _ in 0..8 {
            kr.update(&vec![0.0f32; nb], 0, nb);
        }
        descent(&mut kr, &mut fires);
        assert_eq!(fires, 2, "two distinct descents each fire once: {fires}");
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
        a.drain_scope_scalars(|_| scal += 1);
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

    // `streaming_analyzer_scope_reports_kick_fires` removed
    // (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` §7.2 item 1, P8,
    // 2026-07-11): its whole premise — the scope's kick lane firing end to
    // end — no longer exists (`ScopeOnsets` dropped the `kick` field
    // outright). The detector itself is untouched and still covered by
    // `kick_ridges_fires_on_coherent_descent` and its siblings above.

    // ── Salience (D1) — synthetic columns, no FFT ────────────────────────

    /// bpo 24 harmonic offsets, matching the D1 worked example in the design
    /// doc: fundamental + 2nd..5th harmonic land at +0, +24, +38, +48, +56.
    const SAL_BPO: usize = 24;
    const SAL_OFFS: [usize; 5] = [0, 24, 38, 48, 56];

    #[test]
    fn salience_argmax_lands_on_fundamental_not_a_harmonic() {
        // A full harmonic series (fundamental at B, decaying harmonics at
        // B + off_h) — salience must peak at B, not at any harmonic bin.
        let b = 40usize;
        let n = b + SAL_OFFS[4] + 20;
        let mut col = vec![0.0f32; n];
        for (off, &w) in SAL_OFFS.iter().zip(SALIENCE_WEIGHTS.iter()) {
            col[b + off] = w;
        }
        let mut sal = vec![0.0f32; n];
        let mut pk = vec![0.0f32; sal.len()];
        salience_into(&col, SAL_BPO, &mut pk, &mut sal);
        let (peak_bin, peak_val) = salience_peak(&sal).expect("harmonic series is not all-zero");
        assert!(
            (peak_bin - b as f32).abs() < 1.0,
            "salience argmax should land on the fundamental bin {b}, got {peak_bin}"
        );
        // The fundamental sums every harmonic; no other bin can do that, so
        // its raw (un-refined) value must strictly beat every other bin.
        let (argmax_k, _) =
            sal.iter().enumerate().fold((0usize, f32::MIN), |(bk, bv), (k, &v)| if v > bv { (k, v) } else { (bk, bv) });
        assert_eq!(argmax_k, b, "argmax bin");
        assert!(peak_val > 0.0);
    }

    #[test]
    fn salience_survives_a_missing_fundamental() {
        // Energy ONLY at the 2nd..5th harmonic bins (nothing at B itself) —
        // salience must still argmax at B, because summing the harmonics
        // that ARE present still out-scores treating any single harmonic as
        // its own fundamental.
        let b = 60usize;
        let n = b + SAL_OFFS[4] + 20;
        let mut col = vec![0.0f32; n];
        for (off, &w) in SAL_OFFS.iter().zip(SALIENCE_WEIGHTS.iter()).skip(1) {
            col[b + off] = w;
        }
        assert_eq!(col[b], 0.0, "fundamental bin itself carries no energy");
        let mut sal = vec![0.0f32; n];
        let mut pk = vec![0.0f32; sal.len()];
        salience_into(&col, SAL_BPO, &mut pk, &mut sal);
        let (argmax_k, _) =
            sal.iter().enumerate().fold((0usize, f32::MIN), |(bk, bv), (k, &v)| if v > bv { (k, v) } else { (bk, bv) });
        assert_eq!(argmax_k, b, "missing-fundamental argmax should still land on B");
    }

    #[test]
    fn salience_peak_parabolic_refine_leans_toward_taller_neighbour() {
        // Asymmetric 3-point peak: y0=1.0, y1=3.0 (argmax), y2=2.0 — the
        // taller neighbour is to the right, so the refined bin must sit
        // strictly between k and k+1 (biased right of the integer peak).
        let k = 10usize;
        let mut sal = vec![0.0f32; 21];
        sal[k - 1] = 1.0;
        sal[k] = 3.0;
        sal[k + 1] = 2.0;
        let (refined, val) = salience_peak(&sal).expect("has a positive peak");
        assert_eq!(val, 3.0);
        assert!(
            refined > k as f32 && refined < (k + 1) as f32,
            "refined bin {refined} should lie strictly between {k} and {}",
            k + 1
        );
    }

    #[test]
    fn salience_peak_none_on_all_zero_column() {
        let sal = vec![0.0f32; 32];
        assert_eq!(salience_peak(&sal), None, "fully floored column has no peak");
    }

    // ── Tracker (D5) — synthetic salience columns, no FFT ────────────────

    /// A hop period matching the real transform's (~5.3 ms at hop 256 / 48 kHz)
    /// — only the numeric scale of `dt` matters here, not the exact transform
    /// config (these tests drive `RidgeTracker::update` directly).
    const TRACKER_DT: f32 = 256.0 / 48_000.0;

    /// An isolated impulse of `val` at `bin` in an `n`-bin column, zero
    /// elsewhere — the simplest possible "one clear peak" salience column.
    fn impulse(n: usize, bin: usize, val: f32) -> Vec<f32> {
        let mut v = vec![0.0f32; n];
        if bin < n {
            v[bin] = val;
        }
        v
    }

    /// Two isolated impulses — "the tracked object plus a competitor".
    fn two_impulses(n: usize, bin_a: usize, val_a: f32, bin_b: usize, val_b: f32) -> Vec<f32> {
        let mut v = impulse(n, bin_a, val_a);
        if bin_b < n {
            v[bin_b] += val_b;
        }
        v
    }

    #[test]
    fn tracker_acquires_a_stable_peak_and_presence_rises() {
        let n = 80;
        let mut t = RidgeTracker::new();
        let mut last_presence = 0.0f32;
        // 120 hops ≈ 640 ms — enough for the 100 ms attack tau to close to
        // within 1% of the asymptote (the acquisition hop itself contributes
        // nothing: stability is 0 until the first continuation hop).
        for hop in 0..120 {
            // `col` == `sal` here: a single isolated impulse with NOTHING
            // else nonzero anywhere in the array means its D6 octave
            // neighbourhood (`salience[pos ± bpo]`) is entirely zero — the
            // uncontested ceiling case, presence should approach 1.0.
            let sal = impulse(n, 40, 10.0);
            t.update(&sal, &sal, 0, n, false, SAL_BPO, TRACKER_DT);
            assert!(t.active, "acquires on the first hop with a peak");
            assert_eq!(t.pos, 40.0, "an isolated peak has no fractional refine, hop {hop}");
            assert!(
                t.presence >= last_presence - 1e-6,
                "presence must not fall while tracking a stable peak: hop {hop} {last_presence} -> {}",
                t.presence
            );
            last_presence = t.presence;
        }
        let expected = 1.0f32;
        assert!(
            (last_presence - expected).abs() < 0.01,
            "an uncontested peak (nothing in its octave neighbourhood) should approach presence {expected:.4} (D6): got {last_presence}"
        );
    }

    #[test]
    fn tracker_follows_a_glide_within_slew_no_discontinuity() {
        let n = 120;
        let mut t = RidgeTracker::new();
        let mut prev_pos: Option<f32> = None;
        for hop in 0..40usize {
            let bin = 20 + hop / 2; // +0.5 bin/hop on average
            let sal = impulse(n, bin, 10.0);
            t.update(&sal, &sal, 0, n, false, SAL_BPO, TRACKER_DT);
            if let Some(p) = prev_pos {
                let step = t.pos - p;
                assert!(step.abs() <= MAX_SLEW, "hop {hop}: step {step} exceeds MAX_SLEW");
            }
            prev_pos = Some(t.pos);
        }
        assert_eq!(t.pos, 39.0, "tracker should have followed the glide to its final bin");
    }

    #[test]
    fn tracker_holds_through_a_dropout_and_resumes_without_a_jump() {
        let n = 80;
        let mut t = RidgeTracker::new();
        for _ in 0..5 {
            let sal = impulse(n, 40, 10.0);
            t.update(&sal, &sal, 0, n, false, SAL_BPO, TRACKER_DT);
        }
        assert_eq!(t.pos, 40.0);
        let presence_before_dropout = t.presence;
        let silent = vec![0.0f32; n];
        for hop in 0..20 {
            t.update(&silent, &silent, 0, n, false, SAL_BPO, TRACKER_DT);
            assert_eq!(t.pos, 40.0, "pos must hold through the dropout, hop {hop}");
            assert!(t.active, "20 hops is well within HOLD_HOPS ({HOLD_HOPS}), hop {hop}");
        }
        assert!(
            t.presence < presence_before_dropout,
            "presence should have dipped during the dropout: {presence_before_dropout} -> {}",
            t.presence
        );
        let sal = impulse(n, 40, 10.0);
        t.update(&sal, &sal, 0, n, false, SAL_BPO, TRACKER_DT);
        assert_eq!(t.pos, 40.0, "resuming at the same bin must not jump");
    }

    #[test]
    fn tracker_takeover_needs_challenge_hops_consecutive_hops() {
        let n = 120;
        let mut t = RidgeTracker::new();
        for _ in 0..5 {
            let sal = impulse(n, 40, 10.0);
            t.update(&sal, &sal, 0, n, false, SAL_BPO, TRACKER_DT);
        }
        assert_eq!(t.pos, 40.0);
        // A competitor 30 bins away, well out-salient (> CHALLENGE_RATIO), on
        // every subsequent hop.
        let sal = two_impulses(n, 40, 10.0, 70, 20.0);
        for hop in 1..CHALLENGE_HOPS {
            t.update(&sal, &sal, 0, n, false, SAL_BPO, TRACKER_DT);
            assert_eq!(t.pos, 40.0, "must not jump before CHALLENGE_HOPS consecutive hops (hop {hop})");
        }
        t.update(&sal, &sal, 0, n, false, SAL_BPO, TRACKER_DT);
        assert_eq!(t.pos, 70.0, "must jump exactly at CHALLENGE_HOPS consecutive hops");
    }

    /// BUG-043 mechanism regression (2026-07-06): a 45 Hz deep sub through
    /// the real analyzer path — the salience argmax must land ON the
    /// fundamental bin, not on the sub-octave ghosts (~11-15 Hz) the
    /// pre-apex-mask comb manufactured. The pinned failure: at the bottom
    /// octaves the under-Q kernels smear one peak over ~40 bins, so a
    /// ghost's comb teeth (spaced 8-14 bins) all landed inside the ONE
    /// mound and out-summed the true bin (S[15 Hz] 0.70 vs S[45 Hz] 0.52).
    /// The apex mask in `salience_into` is the fix; the `sub` harness
    /// scenario is the end-to-end gate. Prints the contribution breakdown
    /// with --nocapture for column-level archaeology.
    #[test]
    fn sub_45hz_salience_argmax_on_fundamental_not_subharmonic_ghost() {
        let n = SR as usize * 2;
        let mono: Vec<f32> = (0..n)
            .map(|i| {
                let ph = std::f32::consts::TAU * 45.0 * i as f32 / SR as f32;
                (0.5 * ph.sin() + 0.06 * (2.0 * ph).sin() + 0.03 * (3.0 * ph).sin()).tanh()
            })
            .collect();
        let mut an = StreamingSendAnalyzer::new(SR, 250.0, 2000.0);
        an.set_scope(true);
        an.set_pitch_tracking(true);
        let mut cols: Vec<Vec<f32>> = Vec::new();
        for chunk in mono.chunks(256) {
            an.push(chunk);
            an.drain_scope_columns(|c| cols.push(c.to_vec()));
        }
        let col = &cols[cols.len() - 10]; // deep steady state
        let bpo = an.spec_config.bpo;
        let fmin = an.spec_config.fmin;
        let mut sal = vec![0.0f32; col.len()];
        let mut pk = vec![0.0f32; sal.len()];
        salience_into(col, bpo, &mut pk, &mut sal);

        let hz_of = |bin: f32| fmin * 2f32.powf(bin / bpo as f32);
        let breakdown = |k: usize| {
            let bpof = bpo as f32;
            print!("  bin {k:3} ({:6.2} Hz): S={:9.4}  own col[k]={:9.4}  terms:", hz_of(k as f32), sal[k], col[k]);
            for (i, &w) in SALIENCE_WEIGHTS.iter().enumerate() {
                let h = (i + 1) as f32;
                let off = (bpof * h.log2()).round() as usize;
                let v = col.get(k + off).copied().unwrap_or(0.0);
                print!("  h{}[bin {}, {:5.1} Hz]: {:.4}*{:.4}={:.4}", i + 1, k + off, hz_of((k + off) as f32), w, v, w * v);
            }
            println!();
        };

        // Top 5 salience bins + the true bin's breakdown.
        let mut idx: Vec<usize> = (0..sal.len()).collect();
        idx.sort_by(|&a, &b| sal[b].partial_cmp(&sal[a]).unwrap());
        println!("== top-5 salience bins ==");
        for &k in idx.iter().take(5) {
            breakdown(k);
        }
        let true_bin = (bpo as f32 * (45.0f32 / fmin).log2()).round() as usize;
        println!("== true fundamental bin ==");
        breakdown(true_bin);
        println!("== raw col around bottom two octaves (bins 0..60, magnitude) ==");
        for k in 0..60 {
            println!("  col[{k:2}] {:6.2} Hz = {:.5}", hz_of(k as f32), col[k]);
        }

        let (argmax_bin, _) = salience_peak(&sal).expect("a loud sub is not an all-floored column");
        assert!(
            (argmax_bin - true_bin as f32).abs() <= 1.0,
            "salience argmax must sit on the 45 Hz fundamental (bin {true_bin}), got bin {argmax_bin} ({:.1} Hz)",
            hz_of(argmax_bin)
        );
    }

    /// BUG-043 riser follow-up (2026-07-06): a window whose strongest peak
    /// JUMPS around hop-to-hop (band-noise: measured 10-20 bins/hop on the
    /// riser, vs < 0.3 for any real object) must never accumulate presence,
    /// even though continuation keeps the tracker itself moving smoothly on
    /// nearby residue (small Δpos = HIGH stability - stability alone cannot
    /// catch this). The apex-consistency factor is what kills it.
    #[test]
    fn tracker_wandering_apex_reads_no_presence() {
        let n = 120;
        let mut t = RidgeTracker::new();
        // Strong apex alternating between bins 40 and 70 (30 bins apart)
        // every hop, plus a faint static residue peak at bin 55 the tracker
        // can park on. 200 hops ≈ 1 s.
        for hop in 0..200 {
            let apex_bin = if hop % 2 == 0 { 40 } else { 70 };
            let mut sal = vec![0.0f32; n];
            sal[apex_bin] = 10.0;
            sal[55] = 2.0;
            let col = sal.clone();
            t.update(&sal, &col, 0, n, false, SAL_BPO, TRACKER_DT);
        }
        assert!(
            t.presence < 0.1,
            "a hop-to-hop wandering apex is noise, not an object: presence {}",
            t.presence
        );
    }

    #[test]
    fn tracker_onset_reacquires_after_position_settles_strength_agnostic() {
        let n = 120;
        let mut t = RidgeTracker::new();
        for _ in 0..5 {
            let sal = impulse(n, 40, 10.0);
            t.update(&sal, &sal, 0, n, false, SAL_BPO, TRACKER_DT);
        }
        assert_eq!(t.pos, 40.0);
        // BUG-042 third design (as amended): a new note fires an onset and
        // the OLD peak collapses to residue (1.0) — the real re-attack
        // signature. The new peak is decisive against the held residue
        // (CHALLENGE_RATIO clears trivially) but the fire hop must still
        // NOT teleport (the fire-hop estimate is garbage on real material);
        // the jump happens once the apex position has PARKED for
        // SETTLE_STREAK hops — far sooner than the 12-hop strength-based
        // takeover clock, which is the acceleration this window exists for.
        let sal = two_impulses(n, 40, 1.0, 95, 12.0);
        t.update(&sal, &sal, 0, n, true, SAL_BPO, TRACKER_DT);
        assert_eq!(t.pos, 40.0, "the fire hop itself must hold, never teleport");
        // The fire hop anchored the streak at 95 (streak 1); it completes
        // after SETTLE_STREAK - 1 further parked hops.
        for hop in 0..(SETTLE_STREAK - 2) {
            t.update(&sal, &sal, 0, n, false, SAL_BPO, TRACKER_DT);
            assert_eq!(t.pos, 40.0, "must hold until the streak completes (hop {hop})");
        }
        t.update(&sal, &sal, 0, n, false, SAL_BPO, TRACKER_DT);
        assert_eq!(t.pos, 95.0, "must jump to the settled apex after SETTLE_STREAK parked hops, not after the takeover clock");
    }

    /// BUG-042 guard: an onset whose window never sees a position-consistent
    /// apex (band-noise attack: the argmax jumps around) must expire with
    /// pos unmoved — the re-acquire window is position-evidence-only.
    #[test]
    fn tracker_onset_window_expires_on_wandering_apex() {
        let n = 120;
        let mut t = RidgeTracker::new();
        for _ in 0..5 {
            let sal = impulse(n, 40, 10.0);
            t.update(&sal, &sal, 0, n, false, SAL_BPO, TRACKER_DT);
        }
        assert_eq!(t.pos, 40.0);
        let mut fired = true;
        for hop in 0..(CHALLENGE_HOPS + 4) {
            // Apex alternates 70/100 every hop (30 bins apart) with the old
            // peak still present at 40 — no SETTLE_STREAK run can form.
            let apex_bin = if hop % 2 == 0 { 70 } else { 100 };
            let sal = two_impulses(n, 40, 10.0, apex_bin, 50.0);
            t.update(&sal, &sal, 0, n, fired, SAL_BPO, TRACKER_DT);
            fired = false;
            assert_eq!(t.pos, 40.0, "a wandering apex must never win the re-acquire window (hop {hop})");
        }
        assert_eq!(t.reacquire_hops, 0, "window must have expired");
    }

    /// D6/P2c unification (2026-07-06, real-clip finding): the onset
    /// re-acquire path must distinguish a re-attack NEAR the current position
    /// (a bassline re-striking the SAME note — same object, keep trust) from
    /// one FAR from it (a genuinely new note — new object, reset trust). The
    /// old unconditional `stability = 0.0` on every onset fire treated both
    /// identically, which meant presence never accumulated on note-based
    /// material: every attack, however close in pitch, reset trust to 0.
    #[test]
    fn tracker_onset_reacquire_near_pos_keeps_stability_far_resets_it() {
        let n = 120;
        let mut t = RidgeTracker::new();
        for _ in 0..5 {
            let sal = impulse(n, 40, 10.0);
            t.update(&sal, &sal, 0, n, false, SAL_BPO, TRACKER_DT);
        }
        assert_eq!(t.pos, 40.0);
        assert!(
            t.stability > 0.99,
            "continuation on a static peak should already read ~full stability: {}",
            t.stability
        );

        // (i) Re-attack at the SAME bin (the common real-bassline case): the
        // onset opens the window, the apex is already position-consistent at
        // pos, the streak-complete jump is a no-op with Δ=0 — stability must
        // be preserved and presence must keep rising across the attack.
        let presence_before_near = t.presence;
        let sal = impulse(n, 40, 50.0);
        t.update(&sal, &sal, 0, n, true, SAL_BPO, TRACKER_DT);
        for hop in 0..10 {
            t.update(&sal, &sal, 0, n, false, SAL_BPO, TRACKER_DT);
            assert_eq!(t.pos, 40.0, "same-pitch re-attack must never move pos (hop {hop})");
            assert!(
                t.presence >= presence_before_near - 1e-6,
                "presence must not dip below its pre-re-attack value while re-earning nothing (same object), hop {hop}"
            );
        }
        assert_eq!(t.stability, 1.0, "re-attack at Δpos=0 must read full stability (same object)");
        assert!(
            t.presence > presence_before_near,
            "presence must have kept rising through the same-pitch re-attack: {presence_before_near} -> {}",
            t.presence
        );

        // (ii) A genuinely NEW note far outside SLEW_RADIUS: after the
        // settle streak completes the jump, stability must read 0 (new
        // object, trust re-earned) and presence must fall from its
        // pre-attack level.
        let presence_before_far = t.presence;
        let sal_far = impulse(n, 95, 50.0);
        t.update(&sal_far, &sal_far, 0, n, true, SAL_BPO, TRACKER_DT);
        // Drive hops until the settle-streak jump lands, then check the
        // jump hop's own stability (a later static continuation hop would
        // legitimately read 1.0 again).
        let mut jumped_at = None;
        for hop in 0..CHALLENGE_HOPS {
            if t.pos == 95.0 {
                jumped_at = Some(hop);
                break;
            }
            t.update(&sal_far, &sal_far, 0, n, false, SAL_BPO, TRACKER_DT);
        }
        assert!(jumped_at.is_some(), "the settled far apex must have won the window");
        assert_eq!(t.stability, 0.0, "a far jump must read 0 stability (new object)");
        assert!(
            t.presence < presence_before_far,
            "presence must fall on a far re-acquire before it can re-earn trust: {presence_before_far} -> {}",
            t.presence
        );
    }

    /// D6 recalibration — the ghost case named in the task brief: a peak
    /// whose entire salience total is BORROWED from a harmonic partner that
    /// lies outside the window being asked about (the dive's Low-band
    /// subharmonic phantom of an out-of-band fundamental). The peak's own
    /// bin carries none of that energy (`col[pos] ≈ 0`), which
    /// `presence_target`'s first gate reads directly — presence must stay
    /// near 0 regardless of how dominant the peak looks within the window
    /// (it is, in fact, the sole nonzero bin in its neighbourhood too, so
    /// the octave-neighbourhood term alone would NOT have caught this case —
    /// the `col[pos]` gate is load-bearing here, not redundant).
    #[test]
    fn ghost_peak_with_out_of_window_comb_support_reads_low_presence() {
        let n = 140;
        // Real energy lives ONLY at bin 100 (the true, out-of-window
        // fundamental). `salience_into` deposits h=5's weighted copy of it
        // at bin 100 - 56 = 44 (off_5 = round(24*log2(5)) = 56) — a "ghost"
        // fundamental candidate at 44 with no real energy of its own.
        let mut col = vec![0.0f32; n];
        col[100] = 10.0;
        let mut sal = vec![0.0f32; n];
        let mut pk = vec![0.0f32; sal.len()];
        salience_into(&col, SAL_BPO, &mut pk, &mut sal);
        assert!(sal[44] > 0.0, "the ghost bin must show nonzero borrowed salience (test setup check)");
        assert_eq!(col[44], 0.0, "the ghost bin itself carries no real energy (test setup check)");

        // Window = [0, 50): the ghost bin (44) is inside it, its real
        // support (bin 100) is not. (Narrower than "everything below 100"
        // deliberately — bin 52 also receives a borrowed copy via h=4's
        // offset 48 (100-48=52) and would out-salience bin 44 if left in
        // range, which would acquire the wrong bin for this test's purpose.)
        let mut t = RidgeTracker::new();
        let mut last = 0.0f32;
        for _ in 0..60 {
            t.update(&sal, &col, 0, 50, false, SAL_BPO, TRACKER_DT);
            last = t.presence;
        }
        assert!(t.active, "the ghost bin is still a real local maximum within the window, so it acquires");
        assert_eq!(t.pos, 44.0, "test setup: the tracker must have acquired the ghost bin, not bin 100 (outside the window)");
        assert!(last < 0.1, "a peak with no in-window comb support must not read as present: got {last}");
    }

    /// D6 recalibration — the mirror case: a single, real, fully-supported
    /// harmonic comb (matching the D1 worked example's weights exactly,
    /// `col[pos]` genuinely nonzero) with nothing else in the array must
    /// read HIGH presence — its octave neighbourhood (`salience[pos ± bpo]`)
    /// is entirely empty, so the fundamental's own salience stands out
    /// completely and the ratio approaches its ceiling of 1.0.
    #[test]
    fn dominant_fully_supported_object_reads_high_presence() {
        let n = 140;
        let b = 40usize;
        let mut col = vec![0.0f32; n];
        for (off, &w) in SAL_OFFS.iter().zip(SALIENCE_WEIGHTS.iter()) {
            col[b + off] = w;
        }
        let mut sal = vec![0.0f32; n];
        let mut pk = vec![0.0f32; sal.len()];
        salience_into(&col, SAL_BPO, &mut pk, &mut sal);

        let mut t = RidgeTracker::new();
        let mut last = 0.0f32;
        for _ in 0..60 {
            t.update(&sal, &col, 0, n, false, SAL_BPO, TRACKER_DT);
            last = t.presence;
        }
        assert!(t.active);
        assert_eq!(t.pos, b as f32, "test setup: must have acquired the fundamental");
        assert!(last >= 0.5, "a single dominant, fully-supported object must clear the D6 display bar: got {last}");
    }

    #[test]
    fn pitch_tracking_disabled_matches_untouched_path() {
        // The tracker must be pure plumbing when off: the pre-existing five
        // features must be bit-identical whether the tracker ran or not, and
        // pitch/presence must stay exactly 0 when disabled (D7, simplified).
        let mono = sine(1000.0, nfft() * 6);
        let mut off = StreamingSendAnalyzer::new(SR, 250.0, 2000.0);
        let mut on = StreamingSendAnalyzer::new(SR, 250.0, 2000.0);
        on.set_pitch_tracking(true);
        for chunk in mono.chunks(257) {
            off.push(chunk);
            on.push(chunk);
        }
        let (fo, fon) = (off.latest(), on.latest());
        for b in 0..4 {
            assert_eq!(fo.bands[b].amplitude, fon.bands[b].amplitude, "band {b} amplitude diverged");
            assert_eq!(fo.bands[b].brightness, fon.bands[b].brightness, "band {b} brightness diverged");
            assert_eq!(fo.bands[b].noisiness, fon.bands[b].noisiness, "band {b} noisiness diverged");
            assert_eq!(fo.bands[b].liveliness, fon.bands[b].liveliness, "band {b} liveliness diverged");
            assert_eq!(fo.bands[b].transients, fon.bands[b].transients, "band {b} transients diverged");
            assert_eq!(fo.bands[b].pitch, 0.0, "disabled tracker must read pitch 0, band {b}");
            assert_eq!(fo.bands[b].presence, 0.0, "disabled tracker must read presence 0, band {b}");
        }
        assert_eq!(fo.pitch_hz, 0.0);
        assert_eq!(fo.pitch_confidence, 0.0);
        // Sanity: tracking ON over several hundred ms of a loud steady tone
        // should actually have acquired something on some band.
        assert!(
            fon.bands.iter().any(|b| b.presence > 0.0),
            "tracking on: some band should show nonzero presence on a loud tone"
        );
    }
}
