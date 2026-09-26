//! Per-clip audio-layer playback (Phase 3 of the Audio Layer feature — see
//! `docs/AUDIO_LAYER_DESIGN.md` section 4) plus the realtime modulation tap (section 3R).
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
//! replaces the old offline decode-the-whole-file approach (see section 3R).
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
use manifold_core::audio_stream::{
    audio_stream, AudioStreamConsumer, AudioStreamProducer, AudioStreamRead,
};
use manifold_core::id::{ClipId, LayerId};
use manifold_core::project::Project;
use manifold_core::tempo::TempoMapConverter;
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
/// Per-layer tap ring capacity (mono f32 frames). At 48 kHz this is ~0.34 s —
/// generous headroom over the ~800 samples a 60 Hz content tick consumes, so a
/// brief content-thread stall doesn't lose audio before the analyzer drains it.
const TAP_RING_FRAME_CAPACITY: usize = 16_384;
/// Descriptor capacity for the bounded stream. The tap normally publishes one
/// descriptor per 64-frame staging flush, so this leaves ample headroom over the
/// frame ring and keeps descriptor publication allocation-free.
const TAP_BLOCK_CAPACITY: usize = 512;
/// Small fixed staging buffer used to amortize descriptor publication on the
/// realtime thread. A partial buffer is flushed before a sample-rate change.
const TAP_STAGING_CAPACITY: usize = 64;

/// A short volume/transport tween that declicks an edge (start, stop, seek-jump).
fn declick() -> Tween {
    Tween { duration: Duration::from_millis(DECLICK_MS), ..Default::default() }
}

/// Pure function: compute the warped source file position (seconds) for a given beat.
/// Beat-anchored warp — exact under any tempo map by construction.
///
/// # Arguments
/// * `beat` - The transport beat to compute position for
/// * `clip` - The audio clip (must be an audio clip with recorded_bpm resolved)
/// * `project` - The project (for tempo map)
///
/// # Returns
/// The source file position in seconds that should be playing at the given beat.
///
/// # Semantics
/// - **Warped clip** (`recorded_bpm > 0`): source position is a pure function of beat:
///   `pos(beat) = in_point + (beat − clip.start_beat) × (60.0 / recorded_bpm)`
///   Voice playback rate at beat b = `local_bpm(b) / recorded_bpm`, where local_bpm uses
///   the same priority as `PlaybackEngine::get_seconds_per_beat_at_beat`:
///   live external tempo (Link/MIDI clock) first, then tempo map at that beat,
///   then `settings.bpm` fallback.
/// - **Unwarped clip** (`recorded_bpm == 0`): seconds-based position via tempo map:
///   `pos(beat) = in_point + secs(beat) − secs(start_beat)`, rate 1.0.
fn warped_source_position_at_beat(
    beat: Beats,
    clip: &manifold_core::clip::TimelineClip,
    project: &Project,
) -> f64 {
    let clip_bpm = clip.recorded_bpm_resolved();
    let in_point = clip.in_point.0;
    let start_beat = clip.start_beat;

    if clip_bpm <= 0.0 {
        // Unwarped: seconds-based via tempo map
        let start_secs = TempoMapConverter::beat_to_seconds_immut(
            &project.tempo_map, start_beat, project.settings.bpm,
        ).0;
        let beat_secs = TempoMapConverter::beat_to_seconds_immut(
            &project.tempo_map, beat, project.settings.bpm,
        ).0;
        in_point + (beat_secs - start_secs)
    } else {
        // Warped: beat-linear, source advances at 60/recorded_bpm seconds per beat
        let beats_since_start = (beat - start_beat).0;
        let source_secs_per_beat = 60.0 / clip_bpm as f64;
        in_point + beats_since_start * source_secs_per_beat
    }
}

/// Compute the local BPM at a given beat for voice playback rate.
/// Uses the same priority as `PlaybackEngine::get_seconds_per_beat_at_beat`.
fn local_bpm_at_beat(beat: Beats, project: &Project, engine: &PlaybackEngine) -> f32 {
    // Priority 1: live external tempo (Link/MIDI Clock)
    if let Some((live_bpm, _)) = engine.try_get_live_external_tempo() {
        return live_bpm;
    }
    // Priority 2: tempo map (allocation-free immutable scan)
    project.tempo_map.get_bpm_at_beat_immut(beat, project.settings.bpm).0
}

/// Pass-through tap effect on a layer's sub-track: copies the post-fader mono
/// signal into the layer's lock-free ring and returns the frame untouched, so it
/// reads the same audio that reaches the speakers (warp + gain already applied).
/// Lives on the kira audio thread — never allocates.
///
/// kira requires `Effect: Send + Sync`, but the ring producer is `Send`-only (it
/// caches an index in a `Cell`). The producer is wrapped in a `Mutex` purely to
/// satisfy that bound. Effect callbacks own `&mut self`, so `get_mut` accesses
/// the producer exclusively without acquiring the mutex on the audio thread.
struct LayerTap {
    prod: Mutex<AudioStreamProducer>,
    /// Renderer sample rate, learned from [`Effect::init`] and read by the
    /// content thread to build the matching analyzer. 0 until the first init.
    sample_rate: Arc<AtomicU32>,
    staging: [f32; TAP_STAGING_CAPACITY],
    staging_len: usize,

}

impl Effect for LayerTap {
    fn init(&mut self, sample_rate: u32) {
        self.request_sample_rate(sample_rate);
    }

    fn on_change_sample_rate(&mut self, sample_rate: u32) {
        self.request_sample_rate(sample_rate);
    }

    fn process(
        &mut self,
        input: Frame,
        _dt: f64,
        _clock: &ClockInfoProvider,
        _mods: &ModulatorValueProvider,
    ) -> Frame {
        // Mono downmix of the stereo bus. The frame is returned untouched so
        // this effect remains a true pass-through tap.
        self.enqueue_frame(input)
    }
}

impl LayerTap {
    /// Request a producer-rate change. A partial old-rate block is published
    /// before changing the producer so every descriptor has one rate stamp.
    fn request_sample_rate(&mut self, sample_rate: u32) {
        self.flush_staging();
        self.prod.get_mut().set_sample_rate(sample_rate);
        self.sample_rate.store(sample_rate, Ordering::Relaxed);
    }

    /// Add one mono source frame to the fixed staging buffer. The buffer is
    /// flushed as a whole so the stream publishes one bounded descriptor rather
    /// than one descriptor per audio frame.
    fn enqueue_frame(&mut self, input: Frame) -> Frame {
        self.enqueue_mono((input.left + input.right) * 0.5);
        input
    }

    fn enqueue_mono(&mut self, mono: f32) {
        self.staging[self.staging_len] = mono;
        self.staging_len += 1;
        if self.staging_len == TAP_STAGING_CAPACITY {
            self.flush_staging();
        }
    }

    fn flush_staging(&mut self) {
        if self.staging_len == 0 { return; }
        // Capacity loss is reported by the stream at the original source frame
        // positions. The existing mutex holder is accessed without locking.
        self.prod.get_mut().push_interleaved(&self.staging[..self.staging_len]);
        self.staging_len = 0;
    }

}

/// Builds a [`LayerTap`] when the sub-track is created. The handle is unused —
/// the content thread reaches the tap through the ring + atomic it was built
/// with, not through a kira effect handle.
struct LayerTapBuilder {
    prod: AudioStreamProducer,
    sample_rate: Arc<AtomicU32>,
}

impl EffectBuilder for LayerTapBuilder {
    type Handle = ();
    fn build(self) -> (Box<dyn Effect>, Self::Handle) {
        (
            Box::new(LayerTap {
                prod: Mutex::new(self.prod),
                sample_rate: self.sample_rate,
                staging: [0.0; TAP_STAGING_CAPACITY],
                staging_len: 0,
            }),
            (),
        )
    }
}

/// One audio layer's kira sub-track plus the read end of its post-fader tap.
struct LayerTrack {
    /// Kept alive to keep the kira track alive (dropping the handle removes it).
    /// Clip voices route here via [`StaticSoundData::output_destination`].
    track: TrackHandle,
    /// Read end of the tap stream — drained on the content thread each tick and fed
    /// to the send's `StreamingSendAnalyzer` (the analysis runs inline, no worker
    /// thread; the kira audio thread is the only producer).
    tap: AudioStreamConsumer,
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
    // Route to the layer's sub-track so the tap sees this voice's output. Built silent
    // (rather than played-then-paused) so kira's default 10ms pause fade-out never renders
    // the file's first samples audibly — see BUG-081.
    let data = pre.sound_data.output_destination(track).volume(0.0_f64);
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
        let state = engine.current_state();
        // Audio layers have their own solo bus (design section 5): a soloed audio layer
        // silences other audio layers, independent of the visual solo.
        let any_solo = project.timeline.layers.iter().any(|l| l.is_audio() && l.is_solo);

        let mut active: HashSet<ClipId> = HashSet::new();
        for layer in project.timeline.layers.iter().filter(|l| l.is_audio()) {
            // Every audio layer gets a sub-track + tap, so a layer-fed send sees a
            // silence-decaying stream even while the layer is paused or muted.
            self.ensure_layer_track(&layer.layer_id);

            // Output state (design section 5): two gates from two flags.
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
            // Source position the playhead is over: beat-anchored warp.
            // For warped clips: pos(beat) = in_point + (beat − start_beat) × (60 / recorded_bpm).
            // For unwarped clips: pos(beat) = in_point + secs(beat) − secs(start_beat).
            let expected = Seconds(warped_source_position_at_beat(beat, clip, project));

            // Voice playback rate: local_bpm / recorded_bpm for warped clips, 1.0 for unwarped.
            let clip_bpm = clip.recorded_bpm_resolved();
            let ratio = if clip_bpm > 0.0 {
                local_bpm_at_beat(beat, project, engine) / clip_bpm
            } else {
                1.0
            };
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
        let (prod, cons) = audio_stream(1, TAP_RING_FRAME_CAPACITY, TAP_BLOCK_CAPACITY, 0);
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

    /// Drain the layer's post-fader tap, handing each stamped event and its mono
    /// samples to `f` (oldest → newest). Gap events carry an empty sample slice;
    /// invalid streams are reported and stop the drain. No-op for a layer with no
    /// sub-track yet. Called once per tick by the audio-mod runtime to feed the
    /// send analyzer.
    pub fn drain_layer_tap_stamped(
        &mut self,
        layer_id: &LayerId,
        mut f: impl FnMut(AudioStreamRead, &[f32]),
    ) {
        let Some(lt) = self.layer_tracks.get_mut(layer_id) else {
            return;
        };
        let mut buf = [0.0f32; 2048];
        loop {
            let Some(read) = lt.tap.read(&mut buf) else {
                break;
            };
            match read {
                AudioStreamRead::Samples { samples, .. } => f(read, &buf[..samples]),
                AudioStreamRead::Gap { .. } => f(read, &[]),
                AudioStreamRead::InvalidInput => {
                    log::warn!(
                        "[AudioLayerPlayback] layer tap stream is invalid; replacing the layer stream is required"
                    );
                    f(read, &[]);
                    break;
                }
            }
        }
    }

    /// Drain the layer's post-fader tap, handing each contiguous sample chunk to
    /// `f` (oldest → newest). This legacy API omits stream gaps and warns when it
    /// encounters one; stamped callers should use [`Self::drain_layer_tap_stamped`].
    pub fn drain_layer_tap(&mut self, layer_id: &LayerId, mut f: impl FnMut(&[f32])) {
        self.drain_layer_tap_stamped(layer_id, |read, samples| match read {
            AudioStreamRead::Samples { .. } => f(samples),
            AudioStreamRead::Gap { first_frame, end_frame } => log::warn!(
                "[AudioLayerPlayback] legacy layer tap drain omitted source gap [{first_frame}, {end_frame})"
            ),
            AudioStreamRead::InvalidInput => log::warn!(
                "[AudioLayerPlayback] legacy layer tap drain stopped on invalid stream"
            ),
        });
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

/// Realtime-tap path (steps 2–3 of the section 3R plan): proves a clip routed to a
/// layer's sub-track is captured post-fader off the [`LayerTap`] and reaches the
/// content thread through [`AudioLayerPlayback::drain_layer_tap`] — the exact
/// signal the send analyzer consumes. Ignored because it opens the default
/// output device; run with:
///   cargo test -p manifold-playback layer_tap -- --ignored --nocapture
#[cfg(test)]
mod layer_tap_tests {
    use std::sync::Arc;

    use kira::sound::static_sound::{StaticSoundData, StaticSoundSettings};
    use kira::Frame;

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
    /// tap point (see AUDIO_LAYER_DESIGN section 5).
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

#[cfg(test)]
mod layer_tap_stream_tests {
    use super::*;

    fn test_tap(
        frame_capacity: usize,
        block_capacity: usize,
        sample_rate: u32,
    ) -> (LayerTap, AudioStreamConsumer) {
        let (prod, cons) = audio_stream(1, frame_capacity, block_capacity, sample_rate);
        let sample_rate = Arc::new(AtomicU32::new(sample_rate));
        (
            LayerTap {
                prod: Mutex::new(prod),
                sample_rate,
                staging: [0.0; TAP_STAGING_CAPACITY],
                staging_len: 0,
            },
            cons,
        )
    }

    #[test]
    fn staging_downmixes_mean_and_preserves_input_frame() {
        let (mut tap, mut reader) = test_tap(128, 4, 48_000);
        let input = Frame::new(0.2, 0.6);
        let returned = tap.enqueue_frame(input);
        assert_eq!(returned.left, input.left);
        assert_eq!(returned.right, input.right);

        tap.flush_staging();
        let mut output = [0.0; 4];
        assert_eq!(
            reader.read(&mut output),
            Some(AudioStreamRead::Samples {
                stamp: manifold_core::audio_stream::AudioBlockStamp {
                    first_frame: 0,
                    sample_rate: 48_000,
                    generation: 0,
                },
                samples: 1,
            })
        );
        assert_eq!(output[0], 0.4);
    }

    #[test]
    fn overflow_reports_terminal_gap_after_accepted_prefix() {
        let (mut tap, mut reader) = test_tap(8, 2, 48_000);
        for value in 0..128 {
            tap.enqueue_mono(value as f32);
        }

        let mut output = [0.0; 128];
        assert!(matches!(
            reader.read(&mut output),
            Some(AudioStreamRead::Samples { samples: 8, .. })
        ));
        assert_eq!(
            reader.read(&mut output),
            Some(AudioStreamRead::Gap {
                first_frame: 8,
                end_frame: 128,
            })
        );
        assert_eq!(reader.read(&mut output), None);
    }

    #[test]
    fn rate_change_flushes_partial_block_before_new_stamp() {
        let (mut tap, mut reader) = test_tap(128, 4, 44_100);
        tap.enqueue_mono(1.0);
        tap.enqueue_mono(2.0);
        tap.request_sample_rate(48_000);
        tap.enqueue_mono(3.0);
        tap.flush_staging();

        let mut output = [0.0; 8];
        assert_eq!(
            reader.read(&mut output),
            Some(AudioStreamRead::Samples {
                stamp: manifold_core::audio_stream::AudioBlockStamp {
                    first_frame: 0,
                    sample_rate: 44_100,
                    generation: 0,
                },
                samples: 2,
            })
        );
        assert_eq!(&output[..2], &[1.0, 2.0]);
        assert_eq!(
            reader.read(&mut output),
            Some(AudioStreamRead::Samples {
                stamp: manifold_core::audio_stream::AudioBlockStamp {
                    first_frame: 2,
                    sample_rate: 48_000,
                    generation: 1,
                },
                samples: 1,
            })
        );
        assert_eq!(output[0], 3.0);
    }
}

#[cfg(test)]
mod warped_position_tests {
    use super::*;
    use manifold_core::clip::TimelineClip;
    use manifold_core::project::Project;
    use manifold_core::types::TempoPointSource;
    use manifold_core::{Beats, Bpm, Seconds};

    /// Helper: create a minimal engine for testing (no audio backend).
    fn test_engine(project: Project) -> PlaybackEngine {
        let mut engine = PlaybackEngine::new(Vec::new());
        engine.initialize(project);
        engine
    }

    /// (a) Warped clip at constant tempo: pos(beat) = in_point + beats × (60/recorded_bpm)
    #[test]
    fn warped_position_constant_tempo() {
        let mut project = Project::default();
        project.settings.bpm = Bpm(140.0);
        let mut clip = TimelineClip::new_audio(
            "test.wav".to_string(),
            Beats::from_f32(4.0),
            Beats::from_f32(8.0),
            Seconds(2.0),
            Seconds(120.0),
        );
        clip.set_recorded_bpm(120.0); // Warped

        let engine = test_engine(project);
        let project_binding = engine.project();
        let project_ref = project_binding.as_ref().unwrap();
        let pos_at_4 = warped_source_position_at_beat(Beats::from_f32(4.0), &clip, project_ref);
        let pos_at_8 = warped_source_position_at_beat(Beats::from_f32(8.0), &clip, project_ref);
        let pos_at_12 = warped_source_position_at_beat(Beats::from_f32(12.0), &clip, project_ref);

        // pos(4) = 2.0 + (4-4) × (60/120) = 2.0
        assert!((pos_at_4 - 2.0).abs() < 1e-6, "at start beat, position = in_point");
        // pos(8) = 2.0 + 4 × 0.5 = 4.0
        assert!((pos_at_8 - 4.0).abs() < 1e-6, "mid-clip position");
        // pos(12) = 2.0 + 8 × 0.5 = 6.0
        assert!((pos_at_12 - 6.0).abs() < 1e-6, "end-clip position");
    }

    /// (b) Warped clip with tempo map step: still beat-linear, step has no effect.
    #[test]
    fn warped_position_tempo_map_step() {
        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);
        project.tempo_map.add_or_replace_point(Beats::ZERO, Bpm(120.0), TempoPointSource::Manual, 0.001);
        // Tempo step at beat 6 (inside the clip)
        project.tempo_map.add_or_replace_point(Beats::from_f32(6.0), Bpm(60.0), TempoPointSource::Manual, 0.001);

        let mut clip = TimelineClip::new_audio(
            "test.wav".to_string(),
            Beats::from_f32(4.0),
            Beats::from_f32(6.0), // 4..10
            Seconds(1.0),
            Seconds(120.0),
        );
        clip.set_recorded_bpm(100.0); // Warped

        let engine = test_engine(project);
        let project_binding = engine.project();
        let project_ref = project_binding.as_ref().unwrap();

        // At beat 4 (start): pos = 1.0 + (4-4) × (60/100) = 1.0
        let pos_4 = warped_source_position_at_beat(Beats::from_f32(4.0), &clip, project_ref);
        assert!((pos_4 - 1.0).abs() < 1e-6);

        // At beat 6 (tempo step): pos = 1.0 + 2 × 0.6 = 2.2
        let pos_6 = warped_source_position_at_beat(Beats::from_f32(6.0), &clip, project_ref);
        assert!((pos_6 - 2.2).abs() < 1e-6);

        // At beat 10 (end): pos = 1.0 + 6 × 0.6 = 4.6
        let pos_10 = warped_source_position_at_beat(Beats::from_f32(10.0), &clip, project_ref);
        assert!((pos_10 - 4.6).abs() < 1e-6);
    }

    /// (c) Unwarped clip: follows tempo map (piecewise).
    #[test]
    fn unwarped_position_tempo_map() {
        let mut project = Project::default();
        project.settings.bpm = Bpm(120.0);
        project.tempo_map.add_or_replace_point(Beats::ZERO, Bpm(120.0), TempoPointSource::Manual, 0.001);
        // Tempo halves at beat 6
        project.tempo_map.add_or_replace_point(Beats::from_f32(6.0), Bpm(60.0), TempoPointSource::Manual, 0.001);

        let clip = TimelineClip::new_audio(
            "test.wav".to_string(),
            Beats::from_f32(4.0),
            Beats::from_f32(6.0), // 4..10
            Seconds(1.5),
            Seconds(120.0),
        );
        // No recorded_bpm → unwarped

        let engine = test_engine(project);
        let project_binding = engine.project();
        let project_ref = project_binding.as_ref().unwrap();

        // Unwarped: pos(beat) = in_point + secs(beat) - secs(start_beat)
        let pos_4 = warped_source_position_at_beat(Beats::from_f32(4.0), &clip, project_ref);
        assert!((pos_4 - 1.5).abs() < 1e-6, "unwarped at start: in_point only");

        // At beat 6: secs(6) = 6 × (60/120) = 3.0, secs(4) = 2.0, so pos = 1.5 + 3.0 - 2.0 = 2.5
        let pos_6 = warped_source_position_at_beat(Beats::from_f32(6.0), &clip, project_ref);
        assert!((pos_6 - 2.5).abs() < 1e-4, "unwarped at tempo step");

        // At beat 10: secs(10) = secs(6) + 4 × (60/60) = 3.0 + 4.0 = 7.0, secs(4) = 2.0
        // pos = 1.5 + 7.0 - 2.0 = 6.5
        let pos_10 = warped_source_position_at_beat(Beats::from_f32(10.0), &clip, project_ref);
        assert!((pos_10 - 6.5).abs() < 1e-4, "unwarped at end");
    }
}
