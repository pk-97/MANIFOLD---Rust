//! Per-clip audio-layer playback (Phase 3 of the Audio Layer feature — see
//! `docs/AUDIO_LAYER_DESIGN.md` §4) plus the realtime modulation tap (§3R).
//!
//! One **kira voice per active audio clip**, keyed by `ClipId`. Each tick the
//! content thread calls [`AudioLayerPlayback::update`]: every audio clip under
//! the playhead is played through kira (the existing output backend + mixer),
//! sample-accurately following the transport (seek-on-drift, replay-on-stop —
//! the same policy the imported-audio controller uses). Mute/solo/gain become a
//! per-voice volume tween, which also declicks start/stop/seek.
//!
//! Each audio **layer** owns a kira sub-track; its clip voices route to that
//! track, and a pass-through [`LayerTap`] effect on the track copies the
//! post-fader mono signal (warp + gain already applied by the mixer) into a
//! lock-free ring. The content thread drains that ring into a
//! [`StreamingSendAnalyzer`](manifold_audio::analysis::StreamingSendAnalyzer) to
//! drive a layer-fed send's modulation — what you hear is what modulates. This
//! replaces the old offline decode-the-whole-file approach (see §3R).
//!
//! Decoding reuses [`crate::audio_sync::preload_audio`] (symphonia + encoder-delay
//! probe), so there is no second decode path.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use ahash::AHashMap;
use parking_lot::Mutex;
use kira::clock::clock_info::ClockInfoProvider;
use kira::effect::{Effect, EffectBuilder};
use kira::modulator::value_provider::ModulatorValueProvider;
use kira::track::{TrackBuilder, TrackHandle};
use kira::{
    Frame,
    manager::{AudioManager, AudioManagerSettings, backend::DefaultBackend},
    sound::PlaybackState as KiraPlaybackState,
    sound::static_sound::{StaticSoundData, StaticSoundHandle},
    tween::Tween,
};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer, Split};

use manifold_core::id::{ClipId, LayerId};
use manifold_core::project::Project;
use manifold_core::types::PlaybackState;
use manifold_core::{Beats, Seconds};

use crate::audio_sync::preload_audio;
use crate::engine::PlaybackEngine;

/// Hard-resync threshold: reseek the voice if it drifts more than this from the
/// transport-expected position while playing. Matches the imported-audio path.
const HARD_RESYNC_SECONDS: f64 = 0.20;
/// Tolerance for nudging a *paused* voice to the scrub position.
const PAUSED_SEEK_TOLERANCE_SECONDS: f64 = 0.06;
/// Short fade for start/stop/volume changes so clip edges and mutes don't click.
const DECLICK_MS: u64 = 5;
/// Per-layer tap ring capacity (mono f32 samples). At 48 kHz this is ~0.34 s —
/// generous headroom over the ~800 samples a 60 Hz content tick consumes, so a
/// brief content-thread stall doesn't lose audio before the analyzer drains it.
/// On overflow the audio thread drops the newest sample (non-blocking) rather
/// than ever blocking the mixer.
const TAP_RING_CAPACITY: usize = 16_384;

/// A short volume/transport tween that declicks an edge (start, stop, seek-jump).
fn declick() -> Tween {
    Tween { duration: Duration::from_millis(DECLICK_MS), ..Default::default() }
}

/// Pass-through tap effect on a layer's sub-track: copies the post-fader mono
/// signal into the layer's lock-free ring and returns the frame untouched, so it
/// reads the same audio that reaches the speakers (warp + gain already applied).
/// Lives on the kira audio thread — never allocates.
///
/// kira requires `Effect: Send + Sync`, but the ring producer is `Send`-only (it
/// caches an index in a `Cell`). The producer is wrapped in a `Mutex` purely to
/// satisfy that bound: only the audio thread ever locks it (the content thread
/// drains the *consumer*, a separate ring end), so the lock is uncontended and
/// `try_lock` never blocks the mixer.
struct LayerTap {
    prod: Mutex<ringbuf::HeapProd<f32>>,
    /// Renderer sample rate, learned from [`Effect::init`] and read by the
    /// content thread to build the matching analyzer. 0 until the first init.
    sample_rate: Arc<AtomicU32>,
}

impl Effect for LayerTap {
    fn init(&mut self, sample_rate: u32) {
        self.sample_rate.store(sample_rate, Ordering::Relaxed);
    }

    fn on_change_sample_rate(&mut self, sample_rate: u32) {
        self.sample_rate.store(sample_rate, Ordering::Relaxed);
    }

    fn process(
        &mut self,
        input: Frame,
        _dt: f64,
        _clock: &ClockInfoProvider,
        _mods: &ModulatorValueProvider,
    ) -> Frame {
        // Mono downmix of the stereo bus. On a full ring drop the sample (the
        // analyzer fell behind); never block the audio thread.
        let mono = (input.left + input.right) * 0.5;
        if let Some(mut prod) = self.prod.try_lock() {
            let _ = prod.try_push(mono);
        }
        input
    }
}

/// Builds a [`LayerTap`] when the sub-track is created. The handle is unused —
/// the content thread reaches the tap through the ring + atomic it was built
/// with, not through a kira effect handle.
struct LayerTapBuilder {
    prod: ringbuf::HeapProd<f32>,
    sample_rate: Arc<AtomicU32>,
}

impl EffectBuilder for LayerTapBuilder {
    type Handle = ();
    fn build(self) -> (Box<dyn Effect>, Self::Handle) {
        (Box::new(LayerTap { prod: Mutex::new(self.prod), sample_rate: self.sample_rate }), ())
    }
}

/// One audio layer's kira sub-track plus the read end of its post-fader tap.
struct LayerTrack {
    /// Kept alive to keep the kira track alive (dropping the handle removes it).
    /// Clip voices route here via [`StaticSoundData::output_destination`].
    track: TrackHandle,
    /// Read end of the tap ring — drained on the content thread each tick and fed
    /// to the send's `StreamingSendAnalyzer` (the analysis runs inline, no worker
    /// thread; the kira audio thread is the only producer).
    tap: ringbuf::HeapCons<f32>,
    /// Renderer sample rate, written by the tap on init (0 until then).
    sample_rate: Arc<AtomicU32>,
}

/// One playing (or paused) clip voice.
struct Voice {
    handle: StaticSoundHandle,
    /// Kept so a voice that kira auto-stops at the natural end can be replayed.
    /// Carries the layer's track as its output destination, so a replay re-routes
    /// through the same tap.
    data: StaticSoundData,
    /// The clip's file path the voice was built from — a change rebuilds it.
    path: String,
    duration: Seconds,
    encoder_delay: Seconds,
}

/// Build a fresh voice for `path` routed to `track` (decode + start paused at 0).
/// `None` on a decode/play failure (logged) — a genuine "no audio," not a silent
/// stand-in.
fn make_voice(
    manager: &mut AudioManager<DefaultBackend>,
    track: &TrackHandle,
    path: &str,
) -> Option<Voice> {
    let pre = match preload_audio(path, Beats::ZERO) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("[AudioLayerPlayback] decode failed for '{path}': {e}");
            return None;
        }
    };
    // Route to the layer's sub-track so the tap sees this voice's output.
    let data = pre.sound_data.output_destination(track);
    let mut handle = match manager.play(data.clone()) {
        Ok(h) => h,
        Err(e) => {
            log::warn!("[AudioLayerPlayback] play failed for '{path}': {e}");
            return None;
        }
    };
    handle.pause(Tween::default());
    handle.seek_to(0.0);
    Some(Voice {
        handle,
        data,
        path: path.to_string(),
        duration: pre.clip_duration,
        encoder_delay: pre.encoder_delay,
    })
}

/// Owns the kira manager, one sub-track per audio layer, and one voice per active
/// audio clip. Lives on the content thread beside the imported-audio controller.
pub struct AudioLayerPlayback {
    manager: AudioManager<DefaultBackend>,
    /// One sub-track + tap per audio layer, keyed by `LayerId`.
    layer_tracks: AHashMap<LayerId, LayerTrack>,
    voices: AHashMap<ClipId, Voice>,
}

impl AudioLayerPlayback {
    /// Create the playback manager (opens kira's default-output backend).
    pub fn new() -> Result<Self, String> {
        let manager = AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())
            .map_err(|e| format!("Failed to create audio-layer manager: {e}"))?;
        Ok(Self {
            manager,
            layer_tracks: AHashMap::new(),
            voices: AHashMap::new(),
        })
    }

    /// Drive every audio clip under the playhead. Called each content tick.
    pub fn update(&mut self, project: &Project, engine: &PlaybackEngine) {
        let beat = engine.current_beat();
        let now = engine.current_time();
        let state = engine.current_state();
        // Audio layers have their own solo bus (design §5): a soloed audio layer
        // silences other audio layers, independent of the visual solo.
        let any_solo = project.timeline.layers.iter().any(|l| l.is_audio() && l.is_solo);

        let mut active: HashSet<ClipId> = HashSet::new();
        for layer in project.timeline.layers.iter().filter(|l| l.is_audio()) {
            // Every audio layer gets a sub-track + tap, so a layer-fed send sees a
            // silence-decaying stream even while the layer is paused or muted.
            self.ensure_layer_track(&layer.layer_id);

            // Output state (design §5): two gates from two flags.
            // - `tap_hot`: the layer feeds its post-fader send tap (drives visuals).
            //   Analysis-only is NOT muted, so it stays hot.
            // - `master_hot`: the layer reaches the speakers. Analysis-only cuts
            //   this while leaving the tap hot — the "silent but listening" state.
            //   Mute wins over analysis (muted → both off).
            let tap_hot = !layer.is_muted && (!any_solo || layer.is_solo);
            let master_hot = tap_hot && !layer.analysis_only;

            // Master gate: the sub-track's OUTPUT volume, applied after its effect
            // chain — the `LayerTap` reads the frame *before* this volume, so the
            // send still sees signal even when the sub-track is muted to master.
            if let Some(lt) = self.layer_tracks.get_mut(&layer.layer_id) {
                lt.track
                    .set_volume(if master_hot { 1.0_f64 } else { 0.0_f64 }, declick());
            }

            // Tap gate: per-voice volume. The tap sits in the sub-track chain after
            // the voices, so zeroing the voice silences the tap (full mute).
            let volume = if tap_hot { layer.audio_gain_linear() as f64 } else { 0.0 };
            let Some(clip) = layer.active_audio_clip_at(beat) else {
                continue;
            };
            active.insert(clip.id.clone());
            // Source position the playhead is over: wall-clock elapsed since the
            // clip start, scaled by the warp ratio (varispeed — the voice advances
            // `ratio` seconds of source per wall second), offset into the file by
            // the clip's in-point.
            let ratio = clip.warp_ratio(project.settings.bpm.0);
            let clip_start = engine.beat_to_timeline_time_immut(clip.start_beat);
            let expected = (now - clip_start) * ratio as f64 + clip.in_point;
            // Disjoint field borrows: the track (read) vs the manager + voices
            // (write) are distinct fields of `self`, so this type-checks without a
            // self method that would borrow all of `self`.
            let Some(lt) = self.layer_tracks.get(&layer.layer_id) else {
                continue;
            };
            let track = &lt.track;
            Self::sync_clip(
                &mut self.manager,
                &mut self.voices,
                track,
                &clip.id,
                &clip.audio_file_path,
                expected,
                state,
                volume,
                ratio,
            );
        }

        // Pause voices whose clip isn't active this tick (declicked).
        for (id, voice) in self.voices.iter_mut() {
            if !active.contains(id) && voice.handle.state() == KiraPlaybackState::Playing {
                voice.handle.pause(declick());
            }
        }

        self.evict_absent_clips(project);
        self.evict_absent_layer_tracks(project);
    }

    /// Ensure the layer has a sub-track carrying its post-fader tap. Cheap no-op
    /// when it already exists; on first sight it creates the kira sub-track, the
    /// tap effect, and the ring the content thread drains.
    fn ensure_layer_track(&mut self, layer_id: &LayerId) {
        if self.layer_tracks.contains_key(layer_id) {
            return;
        }
        let (prod, cons) = HeapRb::<f32>::new(TAP_RING_CAPACITY).split();
        let sample_rate = Arc::new(AtomicU32::new(0));
        let mut builder = TrackBuilder::new();
        builder.add_effect(LayerTapBuilder { prod, sample_rate: sample_rate.clone() });
        match self.manager.add_sub_track(builder) {
            Ok(track) => {
                self.layer_tracks
                    .insert(layer_id.clone(), LayerTrack { track, tap: cons, sample_rate });
            }
            Err(e) => {
                log::warn!("[AudioLayerPlayback] failed to create layer sub-track: {e}");
            }
        }
    }

    /// Sync a single clip's voice to the transport-expected position + volume,
    /// routed to its layer's `track`. The voice is removed from the map for the
    /// duration so the manager and the voice can be borrowed independently, then
    /// reinserted. Associated (not `&mut self`) so the caller can hold a borrow of
    /// the layer track concurrently with the manager + voice map.
    #[allow(clippy::too_many_arguments)]
    fn sync_clip(
        manager: &mut AudioManager<DefaultBackend>,
        voices: &mut AHashMap<ClipId, Voice>,
        track: &TrackHandle,
        id: &ClipId,
        path: &str,
        expected: Seconds,
        state: PlaybackState,
        volume: f64,
        ratio: f32,
    ) {
        // Take the existing voice if its file still matches; otherwise (re)build.
        let mut voice = match voices.remove(id) {
            Some(v) if v.path == path => v,
            stale => {
                if let Some(mut old) = stale {
                    old.handle.stop(Tween::default());
                }
                if path.is_empty() {
                    return;
                }
                match make_voice(manager, track, path) {
                    Some(v) => v,
                    None => return,
                }
            }
        };

        voice.handle.set_volume(volume, declick());
        // Varispeed warp: play the source faster/slower so its recorded tempo
        // locks to the project. Pitch moves with rate (Signalsmith replaces this
        // for pitch-preserving stretch in the next P4 step). Declicked so a
        // mid-clip BPM change glides instead of zippering.
        voice.handle.set_playback_rate(ratio as f64, declick());
        let duration = voice.duration;
        let in_range = expected >= Seconds::ZERO && expected < duration;
        let target = (expected + voice.encoder_delay)
            .clamp(Seconds::ZERO, (duration - Seconds(0.001)).max(Seconds::ZERO));
        let playing = voice.handle.state() == KiraPlaybackState::Playing;

        match state {
            PlaybackState::Playing => {
                if !in_range {
                    if playing {
                        voice.handle.pause(declick());
                    }
                } else if !playing {
                    if voice.handle.state() == KiraPlaybackState::Stopped {
                        // Kira stops a handle at the natural end; replay for a
                        // fresh one seeked to the expected position. `data` carries
                        // the layer track as its destination, so the replay still
                        // routes through the tap.
                        match manager.play(voice.data.clone()) {
                            Ok(mut h) => {
                                h.seek_to(target.0);
                                h.set_volume(volume, Tween::default());
                                h.set_playback_rate(ratio as f64, Tween::default());
                                voice.handle = h;
                            }
                            Err(e) => log::warn!("[AudioLayerPlayback] replay failed: {e}"),
                        }
                    } else {
                        voice.handle.seek_to(target.0);
                        voice.handle.resume(declick());
                    }
                } else {
                    let pos = Seconds(voice.handle.position());
                    if (pos - target).abs() > Seconds(HARD_RESYNC_SECONDS) {
                        voice.handle.seek_to(target.0);
                    }
                }
            }
            PlaybackState::Paused => {
                if playing {
                    voice.handle.pause(declick());
                }
                if in_range {
                    let pos = Seconds(voice.handle.position());
                    if (pos - target).abs() > Seconds(PAUSED_SEEK_TOLERANCE_SECONDS) {
                        voice.handle.seek_to(target.0);
                    }
                }
            }
            _ => {
                // Stopped: silence and rewind.
                if playing {
                    voice.handle.pause(declick());
                }
                if voice.handle.position() > 0.0 {
                    voice.handle.seek_to(0.0);
                }
            }
        }

        voices.insert(id.clone(), voice);
    }

    /// Drain the layer's post-fader tap, handing each chunk of mono samples to
    /// `f` (oldest → newest). No-op for a layer with no sub-track yet. Called once
    /// per tick by the audio-mod runtime to feed the send analyzer.
    pub fn drain_layer_tap(&mut self, layer_id: &LayerId, mut f: impl FnMut(&[f32])) {
        let Some(lt) = self.layer_tracks.get_mut(layer_id) else {
            return;
        };
        let mut buf = [0.0f32; 2048];
        loop {
            let n = lt.tap.pop_slice(&mut buf);
            if n == 0 {
                break;
            }
            f(&buf[..n]);
        }
    }

    /// The renderer sample rate of a layer's tap, or `None` until the tap's first
    /// `init` reports it (or if the layer has no sub-track). The analyzer is built
    /// for this rate, since the mixer resamples the source to the output rate.
    pub fn layer_tap_sample_rate(&self, layer_id: &LayerId) -> Option<u32> {
        self.layer_tracks.get(layer_id).and_then(|lt| {
            let sr = lt.sample_rate.load(Ordering::Relaxed);
            (sr > 0).then_some(sr)
        })
    }

    /// Drop voices whose clip is no longer present in the project (stopping the
    /// kira handle). Bounded scan, only when voices exist.
    fn evict_absent_clips(&mut self, project: &Project) {
        if self.voices.is_empty() {
            return;
        }
        let present: HashSet<&ClipId> = project
            .timeline
            .layers
            .iter()
            .filter(|l| l.is_audio())
            .flat_map(|l| l.clips.iter().filter(|c| c.is_audio()).map(|c| &c.id))
            .collect();
        self.voices.retain(|id, voice| {
            let keep = present.contains(id);
            if !keep {
                voice.handle.stop(Tween::default());
            }
            keep
        });
    }

    /// Drop sub-tracks for layers that are gone or no longer audio (dropping the
    /// `TrackHandle` removes the kira track + its tap). Bounded scan, only when
    /// tracks exist.
    fn evict_absent_layer_tracks(&mut self, project: &Project) {
        if self.layer_tracks.is_empty() {
            return;
        }
        let present: HashSet<&LayerId> = project
            .timeline
            .layers
            .iter()
            .filter(|l| l.is_audio())
            .map(|l| &l.layer_id)
            .collect();
        self.layer_tracks.retain(|id, _| present.contains(id));
    }

    /// Stop and drop every voice and layer track (e.g. on project close / reset).
    pub fn reset(&mut self) {
        for voice in self.voices.values_mut() {
            voice.handle.stop(Tween::default());
        }
        self.voices.clear();
        // Dropping the track handles removes the kira sub-tracks + their taps.
        self.layer_tracks.clear();
    }

    /// Number of live voices (test/diagnostic).
    #[cfg(test)]
    pub fn voice_count(&self) -> usize {
        self.voices.len()
    }
}

/// Realtime-tap path (steps 2–3 of the §3R plan): proves a clip routed to a
/// layer's sub-track is captured post-fader off the [`LayerTap`] and reaches the
/// content thread through [`AudioLayerPlayback::drain_layer_tap`] — the exact
/// signal the send analyzer consumes. Ignored because it opens the default
/// output device; run with:
///   cargo test -p manifold-playback layer_tap -- --ignored --nocapture
#[cfg(test)]
mod layer_tap_tests {
    use std::sync::Arc;

    use kira::Frame;
    use kira::sound::static_sound::{StaticSoundData, StaticSoundSettings};

    use super::*;

    #[test]
    #[ignore = "opens the default audio output device; run with --ignored"]
    fn layer_tap_streams_post_fader_samples() {
        let mut playback = AudioLayerPlayback::new().expect("open default output device");
        let layer_id = LayerId::new("test-layer");

        // Create the layer's sub-track + tap, then route a tone to it.
        playback.ensure_layer_track(&layer_id);
        let track = &playback.layer_tracks.get(&layer_id).expect("layer track").track;

        let sr = 48_000u32;
        let frames: Arc<[Frame]> = (0..sr / 20)
            .map(|i| {
                let s = (std::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin() * 0.2;
                Frame::new(s, s)
            })
            .collect();
        let data = StaticSoundData {
            sample_rate: sr,
            frames,
            settings: StaticSoundSettings::default(),
            slice: None,
        }
        .output_destination(track);
        let _handle = playback.manager.play(data).expect("play tone on layer track");

        std::thread::sleep(std::time::Duration::from_millis(150));

        let mut captured = Vec::<f32>::new();
        playback.drain_layer_tap(&layer_id, |chunk| captured.extend_from_slice(chunk));
        let n = captured.len();
        let peak = captured.iter().fold(0.0f32, |a, &x| a.max(x.abs()));
        println!("[layer_tap] drained {n} samples, peak {peak:.4}");
        assert!(n > 1000, "tap saw too few samples ({n}); effect not on the played path");
        assert!(peak > 0.05, "tap saw silence (peak {peak:.4}); routing/fader applied after the tap?");
        assert_eq!(
            playback.layer_tap_sample_rate(&layer_id),
            Some(sr),
            "tap should report the renderer sample rate via init"
        );
    }

    /// The analysis-only guarantee: with the sub-track's OUTPUT volume at 0 (silent
    /// to master), the `LayerTap` must STILL stream the tone — proving the tap reads
    /// the frame *before* the output volume. If this fails, kira applies the
    /// sub-track volume before the effect chain and analysis-only needs a different
    /// tap point (see AUDIO_LAYER_DESIGN §5).
    #[test]
    #[ignore = "opens the default audio output device; run with --ignored"]
    fn tap_stays_hot_when_subtrack_muted_to_master() {
        let mut playback = AudioLayerPlayback::new().expect("open default output device");
        let layer_id = LayerId::new("test-layer");

        playback.ensure_layer_track(&layer_id);
        // Silence the sub-track's output to master (the analysis-only routing).
        playback
            .layer_tracks
            .get_mut(&layer_id)
            .expect("layer track")
            .track
            .set_volume(0.0_f64, Tween::default());

        let track = &playback.layer_tracks.get(&layer_id).expect("layer track").track;
        let sr = 48_000u32;
        let frames: Arc<[Frame]> = (0..sr / 20)
            .map(|i| {
                let s = (std::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin() * 0.2;
                Frame::new(s, s)
            })
            .collect();
        let data = StaticSoundData {
            sample_rate: sr,
            frames,
            settings: StaticSoundSettings::default(),
            slice: None,
        }
        .output_destination(track);
        let _handle = playback.manager.play(data).expect("play tone on layer track");

        std::thread::sleep(std::time::Duration::from_millis(150));

        let mut captured = Vec::<f32>::new();
        playback.drain_layer_tap(&layer_id, |chunk| captured.extend_from_slice(chunk));
        let peak = captured.iter().fold(0.0f32, |a, &x| a.max(x.abs()));
        println!("[layer_tap] sub-track muted to master, tap peak {peak:.4}");
        assert!(
            peak > 0.05,
            "tap went silent (peak {peak:.4}) when sub-track muted to master — \
             output volume is applied before the tap; analysis-only needs a different tap point"
        );
    }
}
