//! Offline audio-reactive export driver — P2 of
//! `docs/OFFLINE_AUDIO_REACTIVE_EXPORT_DESIGN.md`.
//!
//! [`OfflineAudioModDriver`] is a sibling consumer of
//! [`manifold_audio::analysis::StreamingSendAnalyzer`], NOT a mode of
//! [`crate::audio_mod_runtime::AudioModRuntime`] (design: "`AudioModRuntime`
//! itself is NOT reused offline — it drags CoreAudio directory subscriptions,
//! hot-plug listeners, and capture lifecycle"). It feeds the export-rendered
//! mono audio ([`ExportAudio`], from the P1 mixdown seam) through one analyzer
//! per analyzed send, per frame, before the engine ticks — so audio-bound
//! parameters, param triggers, and live clip triggers all move in rendered
//! video exactly as the design's D1 intends.
//!
//! ## Mirrors the live path (never reuses it)
//!
//! Everything here is a deliberate, cited mirror of
//! `audio_mod_runtime.rs::AudioModRuntime::update` (audio_mod_runtime.rs
//! ~201-432):
//! - **Which sends are analyzed** — `Project::analysis_consumed_sends()`
//!   (audio_mod_runtime.rs:240-241), not `sends.len()`. A send with an enabled
//!   audio mod OR an enabled trigger route qualifies.
//! - **Per-send analyzer config** — `set_crossovers`/`set_scope`/
//!   `set_pitch_tracking`/`set_floor_db` (audio_mod_runtime.rs:342-346), same
//!   values (`Project::sends_with_pitch_mods()` for the pitch gate). D5: scope
//!   is always off offline (meters are a live-UX concern).
//! - **The snapshot write** — `snap.sends.clear()`,
//!   `resize(send_count, SendFeatures::default())`, then one write per
//!   analyzed send by its position in `AudioSetup::sends`
//!   (audio_mod_runtime.rs:421-431). `send_count` is every send in the
//!   project, not just the analyzed ones — matching the live snapshot shape.
//!
//! ## What's different (by design, not by shortcut)
//!
//! The live runtime rebuilds its per-tick mono mix from a draining capture
//! ring buffer and a set of currently-playing layer taps — inherently
//! streaming, stateful state carried tick to tick. Offline has the entire
//! rendered range as one static buffer up front ([`ExportAudio`]), so D2's
//! source mapping (capture vs layers vs both) is resolved ONCE at
//! construction into a fixed per-send buffer (or a shared reference to the
//! master mix), and each frame is a pure slice-by-index into it (D6: no
//! per-frame allocation, D1: no cumulative cursor — see
//! [`frame_sample_bounds`]).

use manifold_core::audio_setup::AudioSend;
use manifold_core::id::LayerId;
use manifold_core::project::Project;
use manifold_core::SendFeatures;
use manifold_audio::analysis::StreamingSendAnalyzer;
use manifold_playback::audio_mixdown::ExportAudio;
use manifold_playback::engine::PlaybackEngine;

/// Where one analyzed send's samples come from, resolved once at
/// construction (design D2).
enum SendSource {
    /// Capture-fed, layer-free: read straight from
    /// [`OfflineAudioModDriver::master_mono`] every frame — avoids cloning the
    /// (possibly large) master mix once per capture-fed send.
    Master,
    /// Layer-fed, or capture-fed-and-layer-fed (summed once here): the send's
    /// own buffer, built once at init (D6).
    Own(Vec<f32>),
}

/// One send this driver actually analyzes: its snapshot slot, its source, and
/// its analyzer.
struct AnalyzedSend {
    /// Index into `AudioSetup::sends` — also the index into
    /// `AudioFeatureSnapshot::sends`, since the snapshot is positional
    /// (mirrors the live write at audio_mod_runtime.rs:427-430).
    snapshot_index: usize,
    source: SendSource,
    analyzer: StreamingSendAnalyzer,
}

/// Sample-index bounds `[start, end)` for frame `frame_idx`, at `rate` Hz,
/// `fps` frames/sec. Computed directly from the frame index every call — NOT
/// from a running cursor — so per-frame rounding can never compound into
/// drift over a long export (design D1). `frame_idx as f64 * rate` stays
/// exact in f64 for any export this app will render (frame_idx and rate are
/// both far under 2^26, so the product is far under 2^53).
fn frame_sample_bounds(frame_idx: u32, rate: u32, fps: f64) -> (usize, usize) {
    let rate = rate as f64;
    let start = (frame_idx as f64 * rate / fps).floor() as usize;
    let end = ((frame_idx as f64 + 1.0) * rate / fps).floor() as usize;
    (start, end)
}

/// Sum every present, non-empty layer tap referenced by `layer_ids` into one
/// buffer. Layers with no entry in `per_layer_mono` (or an empty one) are
/// skipped, not treated as an error — the honest-silence rule (D2) applies at
/// the per-layer granularity, not just the per-send one: a send with three
/// layers where one was never tapped still analyzes the other two. Returns
/// `None` (not an empty `Vec`) when nothing contributed, so the caller can
/// distinguish "no layer input" from "silent layer input".
fn sum_layer_taps(
    layer_ids: &[LayerId],
    per_layer_mono: &ahash::AHashMap<LayerId, Vec<f32>>,
) -> Option<Vec<f32>> {
    let mut sum: Option<Vec<f32>> = None;
    for lid in layer_ids {
        let Some(buf) = per_layer_mono.get(lid) else { continue };
        if buf.is_empty() {
            continue;
        }
        match sum.as_mut() {
            None => sum = Some(buf.clone()),
            Some(acc) => {
                if buf.len() > acc.len() {
                    acc.resize(buf.len(), 0.0);
                }
                for (a, b) in acc.iter_mut().zip(buf.iter()) {
                    *a += b;
                }
            }
        }
    }
    sum
}

/// Add `add` into `base` in place, extending `base` with zeros if `add` is
/// longer (element-wise sum, zero-padding the shorter side).
fn add_in_place(base: &mut Vec<f32>, add: &[f32]) {
    if add.len() > base.len() {
        base.resize(add.len(), 0.0);
    }
    for (a, b) in base.iter_mut().zip(add.iter()) {
        *a += b;
    }
}

/// Feeds export-rendered audio ([`ExportAudio`], from the P1 mixdown seam)
/// through one [`StreamingSendAnalyzer`] per analyzed send, per export frame —
/// the offline counterpart to `AudioModRuntime::update`. See the module docs
/// for what's mirrored and what's deliberately different.
pub struct OfflineAudioModDriver<'a> {
    /// The full export mix, mono — read directly by every send whose source
    /// is [`SendSource::Master`] (see that variant's docs).
    master_mono: &'a [f32],
    sends: Vec<AnalyzedSend>,
    /// Total sends in the project (analyzed or not) — the snapshot's length,
    /// matching the live write's `send_count` (audio_mod_runtime.rs:267).
    send_count: usize,
    sample_rate: u32,
    pre_roll_samples: usize,
    fps: f64,
}

impl<'a> OfflineAudioModDriver<'a> {
    /// Build the driver for one export: resolve every consumed send's D2
    /// source mapping against `audio`, construct + pre-roll its analyzer, and
    /// log the mapping (D2: "This substitution is LOGGED per send ... never
    /// silent"). Returns `None` when `Project::analysis_consumed_sends()` is
    /// empty — nothing in the project reads audio, so there's nothing for the
    /// export loop to drive.
    pub fn new(project: &Project, audio: &'a ExportAudio, fps: f64) -> Option<Self> {
        let consumed = project.analysis_consumed_sends();
        if consumed.is_empty() {
            log::info!(
                "[OfflineAudioMod] no send has an enabled audio mod or trigger route — \
                 offline audio-mod is inactive for this export"
            );
            return None;
        }

        let pitch_sends = project.sends_with_pitch_mods();
        let (low_hz, mid_hz) = (project.audio_setup.low_hz, project.audio_setup.mid_hz);

        let mut analyzed = Vec::with_capacity(consumed.len());
        for (i, send) in project.audio_setup.sends.iter().enumerate() {
            if !consumed.contains(&send.id) {
                continue;
            }

            let has_cap = send.has_capture();
            let layer_sum = sum_layer_taps(send.layers(), &audio.per_layer_mono);

            let source = match (has_cap, layer_sum) {
                (true, Some(sum)) => {
                    log::info!(
                        "[OfflineAudioMod] send '{}' ({}): capture -> full export mix + \
                         layers {:?}",
                        send.label,
                        send.id,
                        send.layers().iter().map(LayerId::to_string).collect::<Vec<_>>(),
                    );
                    let mut combined = audio.master_mono.clone();
                    add_in_place(&mut combined, &sum);
                    SendSource::Own(combined)
                }
                (true, None) => {
                    log::info!(
                        "[OfflineAudioMod] send '{}' ({}): capture -> full export mix",
                        send.label,
                        send.id,
                    );
                    SendSource::Master
                }
                (false, Some(sum)) => {
                    log::info!(
                        "[OfflineAudioMod] send '{}' ({}): layers {:?}",
                        send.label,
                        send.id,
                        send.layers().iter().map(LayerId::to_string).collect::<Vec<_>>(),
                    );
                    SendSource::Own(sum)
                }
                (false, None) => {
                    log::info!(
                        "[OfflineAudioMod] send '{}' ({}): no audio in range \
                         (no capture, no reachable layer tap) — features stay default",
                        send.label,
                        send.id,
                    );
                    continue;
                }
            };

            analyzed.push(build_analyzed_send(i, source, send, audio, low_hz, mid_hz, &pitch_sends));
        }

        Some(Self {
            master_mono: &audio.master_mono,
            sends: analyzed,
            send_count: project.audio_setup.sends.len(),
            sample_rate: audio.sample_rate,
            pre_roll_samples: audio.pre_roll_samples,
            fps,
        })
    }

    /// Push frame `frame_idx`'s sample window into every analyzed send and
    /// write the resulting `SendFeatures` into `engine`'s audio snapshot —
    /// call this immediately before `engine.tick(..)` for that frame (mirrors
    /// `AudioModRuntime::update`'s write, audio_mod_runtime.rs:421-431).
    ///
    /// The window is `[floor(f*rate/fps), floor((f+1)*rate/fps))`, offset by
    /// the pre-roll and clamped to the buffer length — see
    /// [`frame_sample_bounds`] for why this can't drift.
    pub fn feed_frame(&mut self, frame_idx: u32, engine: &mut PlaybackEngine) {
        let (start, end) = frame_sample_bounds(frame_idx, self.sample_rate, self.fps);
        let pre = self.pre_roll_samples;
        let master = self.master_mono;

        let snap = engine.audio_snapshot_mut();
        snap.sends.clear();
        snap.sends.resize(self.send_count, SendFeatures::default());

        for entry in self.sends.iter_mut() {
            let buf: &[f32] = match &entry.source {
                SendSource::Master => master,
                SendSource::Own(v) => v.as_slice(),
            };
            let lo = (pre + start).min(buf.len());
            let hi = (pre + end).min(buf.len());
            entry.analyzer.push(&buf[lo..hi]);
            if let Some(slot) = snap.sends.get_mut(entry.snapshot_index) {
                *slot = entry.analyzer.latest();
            }
        }
    }
}

/// Construct one send's analyzer, configure it identically to the live path
/// (audio_mod_runtime.rs:342-346), and push its pre-roll (design D3).
fn build_analyzed_send(
    snapshot_index: usize,
    source: SendSource,
    send: &AudioSend,
    audio: &ExportAudio,
    low_hz: f32,
    mid_hz: f32,
    pitch_sends: &ahash::AHashSet<manifold_core::id::AudioSendId>,
) -> AnalyzedSend {
    let mut analyzer = StreamingSendAnalyzer::new(audio.sample_rate, low_hz, mid_hz);
    // audio_mod_runtime.rs:342-346 — set_crossovers is redundant with `new`'s
    // own crossover args here (nothing retunes them offline mid-export), kept
    // for parity with the live call sequence and so a future per-frame
    // crossover feature (none exists today) finds the call already in place.
    analyzer.set_crossovers(low_hz, mid_hz);
    // D5: scope/spectrogram is never driven offline.
    analyzer.set_scope(false);
    analyzer.set_pitch_tracking(pitch_sends.contains(&send.id));
    analyzer.set_floor_db(send.floor_db);

    let buf: &[f32] = match &source {
        SendSource::Master => &audio.master_mono,
        SendSource::Own(v) => v.as_slice(),
    };
    // D3: settle envelopes/decays before frame 0 with up to 1s of pre-roll.
    let preroll_end = audio.pre_roll_samples.min(buf.len());
    analyzer.push(&buf[..preroll_end]);

    AnalyzedSend { snapshot_index, source, analyzer }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ahash::AHashMap;
    use manifold_core::audio_mod::AudioBand;
    use manifold_core::{AudioSend, TriggerRoute};

    /// A send that qualifies for `analysis_consumed_sends()` via an enabled
    /// trigger route — the simplest construction that makes the send
    /// "consumed" without needing a `PresetInstance`/audio-mod fixture (per
    /// the P2 brief: "a send with an active trigger route also qualifies if
    /// that's easier to construct").
    fn consumed_send(label: &str) -> AudioSend {
        let mut send = AudioSend::new(label);
        let mut route = TriggerRoute::new(AudioBand::Low);
        route.enabled = true;
        send.triggers.push(route);
        send
    }

    fn empty_export_audio(sample_rate: u32, master_mono: Vec<f32>, pre_roll_samples: usize) -> ExportAudio {
        ExportAudio {
            sample_rate,
            left: Vec::new(),
            right: Vec::new(),
            master_mono,
            per_layer_mono: AHashMap::new(),
            pre_roll_samples,
            audible_in_range: true,
        }
    }

    // ─── frame_sample_bounds — D1 no-drift property ───

    #[test]
    fn frame_bounds_are_contiguous_and_exact_for_integer_ratio() {
        // 48000/60 == 800 exactly.
        let mut prev_end = 0usize;
        for f in 0..10_000u32 {
            let (s, e) = frame_sample_bounds(f, 48_000, 60.0);
            assert_eq!(s, prev_end, "frame {f} start must equal the previous frame's end");
            assert_eq!(e - s, 800, "frame {f} length must be exactly 800 at 48kHz/60fps");
            prev_end = e;
        }
        assert_eq!(prev_end, 10_000 * 800);
    }

    #[test]
    fn frame_bounds_are_contiguous_and_bounded_for_fractional_ratio() {
        // 44100/24 == 1837.5 -- a genuinely fractional per-frame boundary.
        let (rate, fps) = (44_100u32, 24.0);
        let mut prev_end = 0usize;
        for f in 0..10_000u32 {
            let (s, e) = frame_sample_bounds(f, rate, fps);
            assert_eq!(s, prev_end, "frame {f} start must equal the previous frame's end (no gap/overlap => no drift)");
            let len = e - s;
            assert!(len == 1837 || len == 1838, "frame {f} length {len} not in {{1837,1838}}");
            prev_end = e;
        }
        let expected_final = ((10_000u64 * rate as u64) as f64 / fps).floor() as usize;
        assert_eq!(prev_end, expected_final, "final boundary must equal floor(N*rate/fps) exactly");
    }

    // ─── inactive project ───

    #[test]
    fn new_returns_none_when_no_send_is_consumed() {
        let project = Project::default();
        let audio = empty_export_audio(48_000, vec![0.0; 48_000], 0);
        assert!(OfflineAudioModDriver::new(&project, &audio, 60.0).is_none());
    }

    // ─── sine fixture: silence before onset, clear signal after ───

    fn sine_master_mono(rate: u32, pre_roll_samples: usize, silent_seconds_in_range: f32, total_seconds: f32) -> Vec<f32> {
        let total_len = (rate as f32 * total_seconds) as usize;
        let onset_at = pre_roll_samples + (rate as f32 * silent_seconds_in_range) as usize;
        let mut buf = vec![0.0f32; total_len];
        for (i, s) in buf.iter_mut().enumerate().skip(onset_at) {
            let t = (i - onset_at) as f32 / rate as f32;
            *s = (2.0 * std::f32::consts::PI * 220.0 * t).sin();
        }
        buf
    }

    #[test]
    fn sine_fixture_full_band_amplitude_silent_before_onset_and_nonzero_after() {
        let rate = 48_000u32;
        let pre_roll = rate as usize; // 1s
        // Silence continues 1s into the main range too, so frame 0 (range
        // start) is still silent; sine starts 1s into the range. Total
        // buffer is 5s (pre-roll 1s + 4s range) so a 170-frame walk at
        // 60fps (< 240 frames == the full 4s range) stays well inside the
        // buffer, never touching the clamped-to-empty tail.
        let master = sine_master_mono(rate, pre_roll, 1.0, 5.0);

        let audio = empty_export_audio(rate, master, pre_roll);
        let mut project = Project::default();
        let mut send = consumed_send("Kick");
        send.channels = vec![0, 1]; // capture-fed -> Master source
        project.audio_setup.sends.push(send);

        let fps = 60.0;
        let mut driver = OfflineAudioModDriver::new(&project, &audio, fps)
            .expect("a send with an enabled trigger route must be consumed");
        let mut engine = PlaybackEngine::new(Vec::new());

        // Frame 0 == range start == still inside the silent second.
        driver.feed_frame(0, &mut engine);
        let silent = engine.audio_snapshot().sends[0].bands[AudioBand::Full.index()].amplitude;

        // Drive forward well past the onset (range frame 60 == t=2.0s ==
        // sine start; frame 170 == t=3.83s, comfortably into steady signal)
        // so the analyzer's window is full of signal, not straddling the
        // transition.
        let mut loud = silent;
        for f in 1..=170u32 {
            driver.feed_frame(f, &mut engine);
            loud = engine.audio_snapshot().sends[0].bands[AudioBand::Full.index()].amplitude;
        }

        assert!(silent < 0.05, "expected near-zero amplitude before onset, got {silent}");
        assert!(loud > silent + 0.2, "expected clearly higher amplitude after onset ({loud} vs {silent})");
    }

    // ─── determinism (D4) ───

    #[test]
    fn two_runs_over_the_same_inputs_are_bit_identical() {
        let rate = 48_000u32;
        let pre_roll = rate as usize;
        let master = sine_master_mono(rate, pre_roll, 0.5, 3.0);
        let audio = empty_export_audio(rate, master, pre_roll);

        let mut project = Project::default();
        project.audio_setup.sends.push(consumed_send("Kick"));
        let fps = 30.0;

        let run = || {
            let mut driver = OfflineAudioModDriver::new(&project, &audio, fps).unwrap();
            let mut engine = PlaybackEngine::new(Vec::new());
            let mut out = Vec::new();
            for f in 0..120u32 {
                driver.feed_frame(f, &mut engine);
                out.push(engine.audio_snapshot().sends[0]);
            }
            out
        };

        let a = run();
        let b = run();
        assert_eq!(a.len(), b.len());
        for (i, (fa, fb)) in a.iter().zip(b.iter()).enumerate() {
            assert_eq!(fa, fb, "frame {i} diverged between two identical runs");
        }
    }

    // ─── D2 source mapping ───

    #[test]
    fn capture_only_send_reads_master_mono() {
        let rate = 48_000u32;
        let master = sine_master_mono(rate, 0, 0.0, 2.0);
        let audio = empty_export_audio(rate, master, 0);

        let mut project = Project::default();
        let mut send = consumed_send("Master tap");
        send.channels = vec![0, 1]; // has_capture() == true, no layers
        project.audio_setup.sends.push(send);

        let mut driver = OfflineAudioModDriver::new(&project, &audio, 60.0).unwrap();
        let mut engine = PlaybackEngine::new(Vec::new());
        for f in 0..30u32 {
            driver.feed_frame(f, &mut engine);
        }
        let amp = engine.audio_snapshot().sends[0].bands[AudioBand::Full.index()].amplitude;
        assert!(amp > 0.1, "capture-fed send should read the master mix's signal, got {amp}");
    }

    #[test]
    fn mixed_capture_and_layer_send_sums_both_sources() {
        let rate = 48_000u32;
        // Master mix carries the signal; the layer tap is silence. If the
        // "both" branch ignores the master and reads only the layer, this
        // send would read as silent — it must not.
        let master = sine_master_mono(rate, 0, 0.0, 2.0);
        let layer_id = LayerId::new("layer-silent");
        let mut per_layer = AHashMap::new();
        per_layer.insert(layer_id.clone(), vec![0.0f32; master.len()]);
        let mut audio = empty_export_audio(rate, master, 0);
        audio.per_layer_mono = per_layer;

        let mut project = Project::default();
        let mut send = consumed_send("Both A");
        send.channels = vec![0, 1];
        send.source.layers.push(layer_id);
        project.audio_setup.sends.push(send);

        let mut driver = OfflineAudioModDriver::new(&project, &audio, 60.0).unwrap();
        let mut engine = PlaybackEngine::new(Vec::new());
        for f in 0..30u32 {
            driver.feed_frame(f, &mut engine);
        }
        let amp = engine.audio_snapshot().sends[0].bands[AudioBand::Full.index()].amplitude;
        assert!(amp > 0.1, "master's signal must still contribute when a silent layer is also routed, got {amp}");

        // And the reverse: silent master, signal-carrying layer.
        let rate2 = 48_000u32;
        let silent_master = vec![0.0f32; (rate2 as f32 * 2.0) as usize];
        let signal_layer = sine_master_mono(rate2, 0, 0.0, 2.0);
        let layer_id2 = LayerId::new("layer-loud");
        let mut per_layer2 = AHashMap::new();
        per_layer2.insert(layer_id2.clone(), signal_layer);
        let mut audio2 = empty_export_audio(rate2, silent_master, 0);
        audio2.per_layer_mono = per_layer2;

        let mut project2 = Project::default();
        let mut send2 = consumed_send("Both B");
        send2.channels = vec![0, 1];
        send2.source.layers.push(layer_id2);
        project2.audio_setup.sends.push(send2);

        let mut driver2 = OfflineAudioModDriver::new(&project2, &audio2, 60.0).unwrap();
        let mut engine2 = PlaybackEngine::new(Vec::new());
        for f in 0..30u32 {
            driver2.feed_frame(f, &mut engine2);
        }
        let amp2 = engine2.audio_snapshot().sends[0].bands[AudioBand::Full.index()].amplitude;
        assert!(amp2 > 0.1, "layer's signal must still contribute when the master is silent, got {amp2}");
    }

    #[test]
    fn unrouted_consumed_send_stays_at_default_features() {
        // Consumed (via trigger route) but neither capture nor layers are
        // wired — honest silence (D2), and the send is simply not analyzed.
        let rate = 48_000u32;
        let master = sine_master_mono(rate, 0, 0.0, 2.0);
        let audio = empty_export_audio(rate, master, 0);

        let mut project = Project::default();
        project.audio_setup.sends.push(consumed_send("Unrouted"));

        let mut driver = OfflineAudioModDriver::new(&project, &audio, 60.0)
            .expect("driver still builds - the send IS consumed, it just has no source");
        let mut engine = PlaybackEngine::new(Vec::new());
        driver.feed_frame(0, &mut engine);
        assert_eq!(
            engine.audio_snapshot().sends[0],
            SendFeatures::default(),
            "an unrouted consumed send must stay at default features, never read the master mix"
        );
    }
}
