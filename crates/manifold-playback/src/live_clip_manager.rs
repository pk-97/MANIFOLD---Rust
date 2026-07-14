use manifold_core::PresetTypeId;
use manifold_core::clip::TimelineClip;
use manifold_core::math::BeatQuantizer;
use manifold_core::project::Project;
use manifold_core::recording::RecordedClipProvenance;
use manifold_core::types::{QuantizeMode, TempoPointSource};
use manifold_core::{Beats, Bpm, ClipId, Seconds};
use manifold_editing::command::Command;
use manifold_editing::commands::clip::AddClipCommand;

use std::collections::{HashMap, HashSet};

/// MIDI clock ticks per beat (standard).
pub const MIDI_CLOCK_TICKS_PER_BEAT: i32 = 24;
/// Ticks per sixteenth note.
const TICKS_PER_SIXTEENTH: i32 = MIDI_CLOCK_TICKS_PER_BEAT / 4; // 6

/// Host interface for LiveClipManager.
/// Port of C# ILiveClipHost.cs.
pub trait LiveClipHost {
    fn current_beat(&self) -> Beats;
    fn current_time(&self) -> Seconds;
    fn is_recording(&self) -> bool;
    fn is_playing(&self) -> bool;
    fn show_debug_logs(&self) -> bool;
    fn get_bpm_at_beat(&self, beat: Beats) -> f32;
    fn get_tempo_source_at_beat(&self, beat: Beats) -> TempoPointSource;
    fn get_beat_snapped_beat(&self) -> Beats;
    fn get_current_absolute_tick(&self) -> i32;
    fn stop_clip(&mut self, clip_id: &str);
    fn mark_sync_dirty(&mut self);
    fn mark_compositor_dirty(&mut self);
    fn invalidate_lookahead_prewarm(&mut self);
    fn register_clip_lookup(&mut self, clip_id: &str, clip: &TimelineClip);
    fn record_command(&mut self, cmd: Box<dyn Command>);
    fn beat_to_timeline_time(&self, beat: Beats) -> Seconds;
}

/// 5ms timing guard threshold (seconds). Reject NoteOff within this window of NoteOn.
/// Port of C# ClipLauncher STALE_NOTE_OFF_THRESHOLD (lines 129-143).
const NOTE_OFF_TIMING_GUARD: f64 = 0.005;

/// Synthetic MIDI note used by audio-triggered one-shots. They have no real note
/// and never receive a NoteOff (the one-shot ends by its own duration), so a
/// sentinel keeps them out of the note-keyed MIDI tracking maps.
const AUDIO_TRIGGER_NOTE: i32 = -1;

/// What a layer plays when triggered live: a generator, a video clip from its
/// folder, or nothing. Resolved from the layer's authoring state and shared by
/// the MIDI from-layer path and the audio one-shot path so the "what does this
/// layer fire" rule lives in exactly one place.
pub(crate) enum LayerLiveContent {
    Generator(PresetTypeId),
    /// The layer's `source_clip_ids` (non-empty), newest-folder order.
    Video(Vec<String>),
    Empty,
}

/// Classify what `layer_index` fires when triggered live. A generator layer
/// fires its generator; otherwise its video folder; otherwise nothing.
pub(crate) fn resolve_layer_live_content(project: &Project, layer_index: i32) -> LayerLiveContent {
    let Some(layer) = project.timeline.layers.get(layer_index as usize) else {
        return LayerLiveContent::Empty;
    };
    let generator = layer.generator_type().clone();
    if generator != PresetTypeId::NONE {
        return LayerLiveContent::Generator(generator);
    }
    let ids = layer.source_clip_ids.clone();
    if ids.is_empty() {
        LayerLiveContent::Empty
    } else {
        LayerLiveContent::Video(ids)
    }
}

/// Start-of-clip recording provenance snapshot.
/// Port of C# TempoRecorder.RecordingClipStartInfo (lines 254-265).
struct RecordingClipStartInfo {
    midi_note: i32,
    start_time_seconds: Seconds,
    start_beat: Beats,
    start_absolute_tick: i32,
    start_bpm: Bpm,
    start_tempo_source: TempoPointSource,
}

/// Manages phantom (live-triggered) clips for MIDI performance.
/// Port of C# LiveClipManager.
pub struct LiveClipManager {
    // Active live slots: layer_index → phantom clip
    live_slots: HashMap<i32, TimelineClip>,
    live_slots_list: Vec<(i32, TimelineClip)>,
    live_slot_clip_ids: HashSet<ClipId>,

    // Tracking
    last_live_trigger_at: f64,

    // Per-slot creation timestamps for 5ms NoteOff timing guard.
    // Port of C# ClipLauncher.TrackedNote.CreationTime / CreationSequence.
    slot_creation_times: HashMap<i32, f64>,
    slot_creation_sequences: HashMap<i32, i32>,

    // Recording provenance: pending clip start snapshots.
    // Port of C# TempoRecorder.clipStarts (line 22-23).
    clip_starts: HashMap<ClipId, RecordingClipStartInfo>,

    // Audio-trigger one-shots: clip_id → (layer_index, end_beat). A live audio
    // trigger has no NoteOff, so the slot is ended here when the playhead passes
    // `end_beat` (see `expire_due_oneshots`). MIDI slots never appear here.
    oneshot_ends: HashMap<ClipId, (i32, f32)>,
}

impl LiveClipManager {
    pub fn new() -> Self {
        Self {
            live_slots: HashMap::with_capacity(8),
            live_slots_list: Vec::with_capacity(8),
            live_slot_clip_ids: HashSet::with_capacity(8),
            last_live_trigger_at: 0.0,
            slot_creation_times: HashMap::with_capacity(8),
            slot_creation_sequences: HashMap::with_capacity(8),
            clip_starts: HashMap::with_capacity(8),
            oneshot_ends: HashMap::with_capacity(8),
        }
    }

    // ─── Accessors ───

    pub fn live_slots(&self) -> &HashMap<i32, TimelineClip> {
        &self.live_slots
    }
    pub fn live_slots_list(&self) -> &[(i32, TimelineClip)] {
        &self.live_slots_list
    }
    /// Build lightweight ActiveClipRef entries for all live slots into caller's buffer.
    pub fn fill_live_slot_refs(&self, out: &mut Vec<crate::scheduler::ActiveClipRef>) {
        for (li, clip) in &self.live_slots_list {
            out.push(crate::scheduler::ActiveClipRef {
                clip_id: clip.id.clone(),
                layer_index: *li,
                clip_index: crate::scheduler::ActiveClipRef::LIVE_SLOT,
                start_beat: clip.start_beat,
                duration_beats: clip.duration_beats,
                is_looping: clip.is_looping,
                is_video: !clip.video_clip_id.is_empty(),
            });
        }
    }
    /// Look up a live slot clip by clip ID (for start_clip resolution).
    pub fn find_live_slot_clip(&self, clip_id: &str) -> Option<&TimelineClip> {
        self.live_slots_list
            .iter()
            .find(|(_, c)| c.id == clip_id)
            .map(|(_, c)| c)
    }
    pub fn live_slot_clip_ids(&self) -> &HashSet<ClipId> {
        &self.live_slot_clip_ids
    }
    pub fn last_live_trigger_at(&self) -> f64 {
        self.last_live_trigger_at
    }

    // ─── Lifecycle ───

    pub fn clear_all(&mut self) {
        self.live_slots.clear();
        self.live_slots_list.clear();
        self.live_slot_clip_ids.clear();
        self.slot_creation_times.clear();
        self.slot_creation_sequences.clear();
        self.clip_starts.clear();
        self.oneshot_ends.clear();
    }

    /// Clear live slots on large seek. Only clears when seek_delta > 1.0.
    /// Port of C# LiveClipManager.ClearOnSeek (lines 92-106).
    /// `stop_clip_fn` is called for each live slot clip before clearing (avoids
    /// self-referential borrow with LiveClipHost trait).
    pub fn clear_on_seek(&mut self, seek_delta: f32, stop_clip_fn: &mut dyn FnMut(&str)) {
        if seek_delta > 1.0 && !self.live_slots.is_empty() {
            for (_, clip) in &self.live_slots_list {
                stop_clip_fn(&clip.id);
            }
            self.live_slots.clear();
            self.live_slots_list.clear();
            self.live_slot_clip_ids.clear();
            self.oneshot_ends.clear();
        }
    }

    /// Notify that a clip was stopped by the engine.
    /// Port of C# LiveClipManager.NotifyClipStopped (lines 108-111).
    /// Unity only removes from liveSlotClipIds — the slot entry persists
    /// so NoteOff can still commit the correct held duration.
    pub fn notify_clip_stopped(&mut self, clip_id: &str) {
        self.live_slot_clip_ids.remove(clip_id);
    }

    pub fn is_live_slot_clip(&self, clip_id: &str) -> bool {
        self.live_slot_clip_ids.contains(clip_id)
    }

    pub fn find_live_clip(&self, clip_id: &str) -> Option<&TimelineClip> {
        self.live_slots.values().find(|c| c.id == clip_id)
    }

    // ─── Quantize math (pure functions) ───

    /// Get quantize interval in MIDI clock ticks.
    pub fn get_quantize_interval_ticks(
        quantize_mode: QuantizeMode,
        time_sig_numerator: i32,
    ) -> i32 {
        match quantize_mode {
            QuantizeMode::Off => 1,
            QuantizeMode::QuarterBeat => TICKS_PER_SIXTEENTH, // 6
            QuantizeMode::Beat => MIDI_CLOCK_TICKS_PER_BEAT,  // 24
            QuantizeMode::Bar => time_sig_numerator * MIDI_CLOCK_TICKS_PER_BEAT,
        }
    }

    /// Compute duration in beats from seconds-domain duration.
    pub fn compute_duration_beats(
        duration_seconds: f32,
        spb: f32,
        event_absolute_tick: i32,
        quantize_mode: QuantizeMode,
        time_sig_numerator: i32,
    ) -> Beats {
        let min_beats = 1.0 / MIDI_CLOCK_TICKS_PER_BEAT as f64;
        if event_absolute_tick >= 0 && spb > 0.0 {
            // Tick-based path
            let raw_ticks = ((duration_seconds / spb) * MIDI_CLOCK_TICKS_PER_BEAT as f32) as i32;
            let interval = Self::get_quantize_interval_ticks(quantize_mode, time_sig_numerator);
            let snapped_ticks = if quantize_mode != QuantizeMode::Off && interval > 0 {
                ((raw_ticks as f32 / interval as f32).round() as i32).max(1) * interval
            } else {
                raw_ticks.max(1)
            };
            Beats(snapped_ticks as f64 / MIDI_CLOCK_TICKS_PER_BEAT as f64)
        } else if spb > 0.0 {
            // Beat-based path
            let raw_beats = duration_seconds / spb;
            if quantize_mode != QuantizeMode::Off {
                let interval = match quantize_mode {
                    QuantizeMode::QuarterBeat => 0.25,
                    QuantizeMode::Beat => 1.0,
                    QuantizeMode::Bar => time_sig_numerator as f32,
                    QuantizeMode::Off => 1.0,
                };
                let rounded = (raw_beats / interval).round() * interval;
                Beats(rounded.max(min_beats as f32) as f64)
            } else {
                Beats(raw_beats.max(min_beats as f32) as f64)
            }
        } else {
            Beats(1.0)
        }
    }

    /// Snap a tick position to the quantize grid, returning beats.
    pub fn compute_snap_beat_from_tick(
        tick: i32,
        quantize_mode: QuantizeMode,
        time_sig_numerator: i32,
        ceil_to_next_grid: bool,
    ) -> Beats {
        let mut snapped_tick = tick;

        // Sixteenth-note compensation: snap to nearest 16th if within ±1 tick
        let nearest_16th =
            ((tick as f32 / TICKS_PER_SIXTEENTH as f32).round() as i32) * TICKS_PER_SIXTEENTH;
        if (tick - nearest_16th).abs() <= 1 {
            snapped_tick = nearest_16th;
        }

        // Quantize snap
        if quantize_mode != QuantizeMode::Off {
            let interval = Self::get_quantize_interval_ticks(quantize_mode, time_sig_numerator);
            if interval > 0 {
                snapped_tick = if ceil_to_next_grid {
                    ((snapped_tick + interval - 1) / interval) * interval
                } else {
                    ((snapped_tick as f32 / interval as f32).round() as i32) * interval
                };
            }
        }

        Beats(snapped_tick as f64 / MIDI_CLOCK_TICKS_PER_BEAT as f64)
    }

    /// Compute held beats from start and end ticks.
    pub fn compute_held_beats_from_ticks(
        start_tick: i32,
        end_tick: i32,
        quantize_mode: QuantizeMode,
        time_sig_numerator: i32,
    ) -> Beats {
        let held_ticks = (end_tick - start_tick).max(1);
        let interval = Self::get_quantize_interval_ticks(quantize_mode, time_sig_numerator);

        let snapped = if quantize_mode != QuantizeMode::Off && interval > 0 {
            ((held_ticks as f32 / interval as f32).round() as i32).max(1) * interval
        } else {
            held_ticks
        };

        Beats(snapped as f64 / MIDI_CLOCK_TICKS_PER_BEAT as f64)
    }

    // ─── Activation ───

    /// Activate a live slot, stopping any existing slot on the same layer.
    /// Port of C# LiveClipManager.ActivateLiveSlotNow (lines 315-351).
    /// `stop_clip_fn` stops the old clip's renderer if replacing with a different ID.
    fn activate_live_slot_now_with_stop(
        &mut self,
        layer_index: i32,
        clip: TimelineClip,
        stop_clip_fn: &mut dyn FnMut(&str),
    ) {
        // Remove existing live slot on this layer — stop its renderer if different clip
        if let Some(old_clip) = self.live_slots.remove(&layer_index) {
            if old_clip.id != clip.id {
                stop_clip_fn(&old_clip.id);
            }
            self.live_slot_clip_ids.remove(&old_clip.id);
            self.live_slots_list.retain(|(l, _)| *l != layer_index);
            // A retriggered layer drops the old one-shot's expiry so it can't
            // later end the *new* slot on this layer.
            self.oneshot_ends.remove(&old_clip.id);
            // Port of C# ActivateLiveSlotNow line 337.
            self.remove_recording_clip_start(&old_clip.id);
        }

        self.live_slot_clip_ids.insert(clip.id.clone());
        self.live_slots_list.push((layer_index, clip.clone()));
        self.live_slots.insert(layer_index, clip);
    }

    /// Internal activation without host stop callback (used by trigger methods
    /// that manage their own host interaction).
    fn activate_live_slot_now(&mut self, layer_index: i32, clip: TimelineClip) {
        let mut noop = |_: &str| {};
        self.activate_live_slot_now_with_stop(layer_index, clip, &mut noop);
    }

    // ─── Trigger ───

    /// Trigger a live video clip (NoteOn).
    #[allow(clippy::too_many_arguments)]
    pub fn trigger_live_clip(
        &mut self,
        project: &mut Project,
        host: &dyn LiveClipHost,
        video_clip_id: String,
        layer_index: i32,
        duration_seconds: f32,
        in_point: f32,
        beat_stamp: Option<f32>,
        event_absolute_tick: i32,
        apply_launch_quantize: bool,
        realtime_now: f64,
        midi_note: i32,
    ) -> Option<TimelineClip> {
        // Ensure enough layers exist
        project
            .timeline
            .ensure_layer_count((layer_index + 1) as usize);

        let spb = 60.0 / host.get_bpm_at_beat(host.current_beat());

        // Compute snap beat
        let snap_beat = self.compute_trigger_snap_beat(
            host,
            beat_stamp,
            event_absolute_tick,
            project.settings.quantize_mode,
            project.settings.time_signature_numerator,
            apply_launch_quantize,
        );

        // Compute duration
        let duration_beats = Self::compute_duration_beats(
            duration_seconds,
            spb,
            event_absolute_tick,
            project.settings.quantize_mode,
            project.settings.time_signature_numerator,
        );

        // Create phantom clip
        let mut clip = TimelineClip::new_video(
            video_clip_id,
            snap_beat,
            duration_beats,
            Seconds::from_f32(in_point),
        );
        clip.recorded_bpm = host.get_bpm_at_beat(snap_beat);

        if event_absolute_tick >= 0 {
            clip.start_absolute_tick =
                (snap_beat.as_f32() * MIDI_CLOCK_TICKS_PER_BEAT as f32) as i32;
            clip.has_start_absolute_tick = true;
        }

        self.activate_live_slot_now(layer_index, clip.clone());

        // Track recording provenance. Port of C# ActivateLiveSlotNow line 350.
        self.track_recording_clip_start(host, project, &clip, midi_note);

        // Record creation timestamps for 5ms NoteOff timing guard
        self.slot_creation_times.insert(layer_index, realtime_now);
        if event_absolute_tick >= 0 {
            self.slot_creation_sequences
                .insert(layer_index, event_absolute_tick);
        }

        self.last_live_trigger_at = realtime_now;
        Some(clip)
    }

    /// Trigger a live generator clip (NoteOn).
    #[allow(clippy::too_many_arguments)]
    pub fn trigger_live_generator_clip(
        &mut self,
        project: &mut Project,
        host: &dyn LiveClipHost,
        _generator_type: PresetTypeId,
        layer_index: i32,
        duration_seconds: f32,
        beat_stamp: Option<f32>,
        event_absolute_tick: i32,
        apply_launch_quantize: bool,
        realtime_now: f64,
        midi_note: i32,
    ) -> Option<TimelineClip> {
        project
            .timeline
            .ensure_layer_count((layer_index + 1) as usize);

        let spb = 60.0 / host.get_bpm_at_beat(host.current_beat());

        let snap_beat = self.compute_trigger_snap_beat(
            host,
            beat_stamp,
            event_absolute_tick,
            project.settings.quantize_mode,
            project.settings.time_signature_numerator,
            apply_launch_quantize,
        );

        let duration_beats = Self::compute_duration_beats(
            duration_seconds,
            spb,
            event_absolute_tick,
            project.settings.quantize_mode,
            project.settings.time_signature_numerator,
        );

        let mut clip = TimelineClip::new_generator(snap_beat, duration_beats);
        clip.recorded_bpm = host.get_bpm_at_beat(snap_beat);

        if event_absolute_tick >= 0 {
            clip.start_absolute_tick =
                (snap_beat.as_f32() * MIDI_CLOCK_TICKS_PER_BEAT as f32) as i32;
            clip.has_start_absolute_tick = true;
        }

        self.activate_live_slot_now(layer_index, clip.clone());

        // Track recording provenance. Port of C# ActivateLiveSlotNow line 350.
        self.track_recording_clip_start(host, project, &clip, midi_note);

        // Record creation timestamps for 5ms NoteOff timing guard
        self.slot_creation_times.insert(layer_index, realtime_now);
        if event_absolute_tick >= 0 {
            self.slot_creation_sequences
                .insert(layer_index, event_absolute_tick);
        }

        self.last_live_trigger_at = realtime_now;
        Some(clip)
    }

    /// Fire a fixed-length one-shot on `layer_index` from a live audio trigger.
    ///
    /// Resolves the layer's content ([`resolve_layer_live_content`]) and reuses
    /// the MIDI trigger primitives ([`Self::trigger_live_clip`] /
    /// [`Self::trigger_live_generator_clip`]) — no duplicated clip creation. The
    /// fire snaps to the project quantize grid exactly as a MIDI launch does
    /// (`event_absolute_tick` = the host's current tick, no `beat_stamp`), so
    /// there is no audio-specific timing math. Records the slot's end beat so
    /// [`Self::expire_due_oneshots`] can close it (a transient has no NoteOff).
    /// Returns `None` if the layer has no content to fire.
    pub fn fire_layer_oneshot(
        &mut self,
        project: &mut Project,
        host: &dyn LiveClipHost,
        layer_index: i32,
        one_shot_beats: Beats,
        realtime_now: f64,
    ) -> Option<TimelineClip> {
        let bpm = host.get_bpm_at_beat(host.current_beat());
        let spb = 60.0 / bpm.max(1.0);
        let duration_seconds = (one_shot_beats.0 as f32 * spb).max(0.05);
        // A live audio one-shot fires in REAL TIME at the playhead — it has no
        // musical event tick. Snap on the beat clock (`current_beat`), NOT
        // `get_current_absolute_tick()`: that returns a frame counter when no
        // external MIDI clock is connected, which yields a start_beat unrelated
        // to the playhead and a window the scheduler treats as long-expired
        // (so `start_clip` never runs and nothing renders). Passing
        // `event_absolute_tick = -1` + `beat_stamp = current_beat` routes
        // through the beat-domain snap and activates the slot immediately.
        let beat_stamp = Some(host.current_beat().as_f32());
        let tick = -1;

        let clip = match resolve_layer_live_content(project, layer_index) {
            LayerLiveContent::Generator(generator) => self.trigger_live_generator_clip(
                project,
                host,
                generator,
                layer_index,
                duration_seconds,
                beat_stamp,
                tick,
                false, // audio trigger — never launch-quantized (trap 1)
                realtime_now,
                AUDIO_TRIGGER_NOTE,
            )?,
            LayerLiveContent::Video(ids) => {
                let video_clip_id = ids.into_iter().next()?;
                // Cap the one-shot to the source clip's own length when known so
                // a long one-shot can't run past the media.
                let clip_len = project
                    .video_library
                    .find_clip_by_id(&video_clip_id)
                    .map(|c| c.duration)
                    .unwrap_or(0.0);
                let dur = if clip_len > 0.0 {
                    duration_seconds.min(clip_len)
                } else {
                    duration_seconds
                };
                self.trigger_live_clip(
                    project,
                    host,
                    video_clip_id,
                    layer_index,
                    dur,
                    0.0,
                    beat_stamp,
                    tick,
                    false, // audio trigger — never launch-quantized (trap 1)
                    realtime_now,
                    AUDIO_TRIGGER_NOTE,
                )?
            }
            LayerLiveContent::Empty => return None,
        };

        let end_beat = (clip.start_beat + clip.duration_beats).as_f32();
        self.oneshot_ends
            .insert(clip.id.clone(), (layer_index, end_beat));
        Some(clip)
    }

    /// End every audio one-shot whose `end_beat` the playhead has passed.
    /// Removes the live slot and returns `(layer_index, clip_id)` for each so the
    /// engine can stop the renderer. MIDI slots are untouched (they never carry
    /// an entry here).
    pub fn expire_due_oneshots(&mut self, current_beat: f32) -> Vec<(i32, ClipId)> {
        if self.oneshot_ends.is_empty() {
            return Vec::new();
        }
        let due: Vec<ClipId> = self
            .oneshot_ends
            .iter()
            .filter(|(_, (_, end_beat))| current_beat >= *end_beat)
            .map(|(id, _)| id.clone())
            .collect();

        let mut ended = Vec::with_capacity(due.len());
        for clip_id in due {
            let Some((layer_index, _)) = self.oneshot_ends.remove(&clip_id) else {
                continue;
            };
            // Only end it if this clip still owns the layer's slot — a retrigger
            // may already have replaced it (and dropped this entry, but guard
            // anyway).
            if self.live_slots.get(&layer_index).map(|c| &c.id) == Some(&clip_id) {
                self.live_slots.remove(&layer_index);
                self.live_slots_list.retain(|(l, _)| *l != layer_index);
                self.live_slot_clip_ids.remove(&clip_id);
                self.slot_creation_times.remove(&layer_index);
                self.slot_creation_sequences.remove(&layer_index);
                ended.push((layer_index, clip_id));
            }
        }
        ended
    }

    /// `apply_launch_quantize` distinguishes a MIDI note launch (quantize
    /// launch *position* to the grid, F2) from an audio-transient one-shot
    /// (fire immediately at the playhead — the music's own timing; quantizing
    /// it would fire a kick beats late). Both `trigger_live_clip` and
    /// `trigger_live_generator_clip` are shared by the MIDI NoteOn path
    /// (`clip_launcher.rs`) and the audio one-shot path (`fire_layer_oneshot`
    /// below), so this can't be decided from which of the two functions
    /// called in — it must be threaded down from the actual caller.
    fn compute_trigger_snap_beat(
        &self,
        host: &dyn LiveClipHost,
        beat_stamp: Option<f32>,
        event_absolute_tick: i32,
        quantize_mode: QuantizeMode,
        time_sig_numerator: i32,
        apply_launch_quantize: bool,
    ) -> Beats {
        if event_absolute_tick >= 0 {
            Self::compute_snap_beat_from_tick(
                event_absolute_tick,
                quantize_mode,
                time_sig_numerator,
                true, // ceil to next grid for NoteOn
            )
        } else if let Some(stamp) = beat_stamp {
            // `apply_launch_quantize` gates this arm too: today only
            // `fire_layer_oneshot` ever supplies a synthetic `beat_stamp`
            // here (real midir events never carry one — see the fallback arm
            // below), so unguarded this branch was rounding audio one-shots
            // to the nearest grid line whenever the playhead fell off-grid,
            // silently contradicting "audio fires at the raw current beat".
            if apply_launch_quantize && quantize_mode != QuantizeMode::Off {
                let interval = match quantize_mode {
                    QuantizeMode::QuarterBeat => 0.25,
                    QuantizeMode::Beat => 1.0,
                    QuantizeMode::Bar => time_sig_numerator as f32,
                    QuantizeMode::Off => 1.0,
                };
                Beats::from_f32((stamp / interval).round() * interval)
            } else {
                Beats::from_f32(stamp)
            }
        } else {
            // The live midir path: no clock-domain tick (midi_input.rs sets
            // absolute_tick = -1 always) and therefore no beat_stamp either
            // (midi_input.rs only derives one from a real tick). F2 root
            // cause: without this arm, a MIDI launch here landed exactly
            // where the finger hit, ignoring QuantizeMode entirely. Snap
            // FORWARD (ceil) to the next grid boundary, matching Ableton —
            // the live-slot scheduler (`scheduler.rs::compute_sync`) already
            // gates a slot's activation on `current_beat >= start_beat`, so
            // handing back a future beat here simply arms the clip; it does
            // not delay this function's own bookkeeping.
            let raw = host.get_beat_snapped_beat();
            if apply_launch_quantize && quantize_mode != QuantizeMode::Off {
                let interval = match quantize_mode {
                    QuantizeMode::QuarterBeat => 0.25,
                    QuantizeMode::Beat => 1.0,
                    QuantizeMode::Bar => time_sig_numerator as f64,
                    QuantizeMode::Off => unreachable!("guarded above"),
                };
                Beats(crate::session_state::SessionRuntime::ceil_to_boundary(
                    raw.0, interval,
                ))
            } else {
                raw
            }
        }
    }

    // ─── Commit ───

    /// Commit a live clip (NoteOff). If recording, adds to timeline.
    /// Port of C# ClipLauncher.HandleNoteOff with 5ms timing guard.
    #[allow(clippy::too_many_arguments)]
    pub fn commit_live_clip(
        &mut self,
        project: &mut Project,
        host: &mut dyn LiveClipHost,
        layer_index: i32,
        clip_id: Option<&str>,
        beat_stamp: Option<f32>,
        event_absolute_tick: i32,
        realtime_now: f64,
        midi_note: i32,
    ) {
        // 5ms timing guard: reject NoteOff that arrives within 5ms of NoteOn.
        // Port of C# ClipLauncher.cs lines 129-143.
        // Some MIDI controllers (Minis) send NoteOff very quickly after NoteOn.
        if let Some(&creation_time) = self.slot_creation_times.get(&layer_index) {
            // Time-based guard: reject if no native tick and within 5ms window
            if event_absolute_tick < 0 && (realtime_now - creation_time) < NOTE_OFF_TIMING_GUARD {
                return;
            }
            // Sequence-based guard: reject if NoteOff tick <= NoteOn tick (out of order)
            if event_absolute_tick > 0
                && let Some(&creation_seq) = self.slot_creation_sequences.get(&layer_index)
                && creation_seq > 0
                && event_absolute_tick <= creation_seq
            {
                return;
            }
        }

        if !self.live_slots.contains_key(&layer_index) {
            return;
        }

        let live_clip = match self.live_slots.get(&layer_index) {
            Some(c) => c.clone(),
            None => return,
        };

        // If a specific clip_id was given but doesn't match, skip
        if let Some(id) = clip_id
            && id != live_clip.id
        {
            return;
        }

        let start_beat = live_clip.start_beat;

        // Compute held duration
        let held_beats = if event_absolute_tick >= 0 {
            let start_tick = if live_clip.has_start_absolute_tick {
                live_clip.start_absolute_tick
            } else {
                (start_beat.as_f32() * MIDI_CLOCK_TICKS_PER_BEAT as f32) as i32
            };
            Self::compute_held_beats_from_ticks(
                start_tick,
                event_absolute_tick,
                project.settings.quantize_mode,
                project.settings.time_signature_numerator,
            )
        } else {
            let beat_now = beat_stamp
                .map(Beats::from_f32)
                .unwrap_or_else(|| host.get_beat_snapped_beat());
            let raw = (beat_now - start_beat).as_f32();
            let min_beats = 1.0 / MIDI_CLOCK_TICKS_PER_BEAT as f32;
            if project.settings.quantize_mode != QuantizeMode::Off {
                let interval = project.settings.get_quantize_interval_beats();
                if interval > 0.0 {
                    Beats::from_f32(((raw / interval).round() * interval).max(interval))
                } else {
                    Beats::from_f32(raw.max(min_beats))
                }
            } else {
                Beats::from_f32(raw.max(min_beats))
            }
        };

        // Remove live slot and creation tracking
        host.stop_clip(&live_clip.id);
        self.live_slots.remove(&layer_index);
        self.live_slots_list.retain(|(l, _)| *l != layer_index);
        self.live_slot_clip_ids.remove(&live_clip.id);
        self.slot_creation_times.remove(&layer_index);
        self.slot_creation_sequences.remove(&layer_index);

        // Compute end beat for provenance.
        // Port of C# LiveClipManager.CommitLiveClip lines 698-707.
        let beat_now = if event_absolute_tick >= 0 {
            let raw_snap = Self::compute_snap_beat_from_tick(
                event_absolute_tick,
                project.settings.quantize_mode,
                project.settings.time_signature_numerator,
                false,
            );
            if project.settings.quantize_mode != QuantizeMode::Off {
                start_beat + held_beats
            } else {
                raw_snap
            }
        } else {
            beat_stamp
                .map(Beats::from_f32)
                .unwrap_or_else(|| host.get_beat_snapped_beat())
        };

        let live_clip_id = live_clip.id.clone();

        // If recording, commit to timeline
        let mut committed_clip: Option<TimelineClip> = None;
        if host.is_recording() {
            let original_duration = live_clip.duration_beats;

            let mut committed = live_clip;
            committed.duration_beats = held_beats;

            // If held longer than original + epsilon, enable looping.
            // Port of C# LiveClipManager commit: heldBeats > liveClip.DurationBeats + 0.001f
            if held_beats > original_duration + Beats(0.001) {
                committed.is_looping = true;
                committed.loop_duration_beats = original_duration;
            }

            let layer_lid = project
                .timeline
                .layers
                .get(layer_index as usize)
                .map(|l| l.layer_id.clone())
                .unwrap_or_default();
            committed.layer_id = layer_lid.clone();

            // AddClipCommand enforces non-overlap internally (trims/deletes
            // existing clips that collide with the committed clip).
            let spb = 60.0 / host.get_bpm_at_beat(committed.start_beat).max(1.0);
            let mut add_cmd = AddClipCommand::new(committed.clone(), layer_lid, spb);
            add_cmd.execute(project);

            host.register_clip_lookup(&committed.id, &committed);
            committed_clip = Some(committed);
            host.record_command(Box::new(add_cmd));
        }

        // Recording provenance finalization.
        // Port of C# LiveClipManager.CommitLiveClip lines 803-810.
        let resolved_end_tick = if event_absolute_tick >= 0 {
            event_absolute_tick
        } else {
            (beat_now.0 * MIDI_CLOCK_TICKS_PER_BEAT as f64).round() as i32
        };

        if let Some(ref recorded) = committed_clip {
            self.finalize_recording_clip(
                host,
                project,
                &live_clip_id,
                recorded,
                beat_now,
                resolved_end_tick,
                midi_note,
            );
        } else {
            self.remove_recording_clip_start(&live_clip_id);
        }

        host.mark_sync_dirty();
        host.mark_compositor_dirty();
    }

    // ─── Recording provenance (Phase 7C) ───

    /// Track recording clip start for tempo/provenance metadata.
    /// Port of C# LiveClipManager.TrackRecordingClipStart (lines 820-834)
    /// + TempoRecorder.TrackClipStart (lines 173-196).
    pub fn track_recording_clip_start(
        &mut self,
        host: &dyn LiveClipHost,
        project: &mut Project,
        clip: &TimelineClip,
        midi_note: i32,
    ) {
        if !host.is_recording() || !host.is_playing() {
            return;
        }

        let start_beat = clip.start_beat;
        let start_bpm = Bpm(host.get_bpm_at_beat(start_beat));
        let start_source = host.get_tempo_source_at_beat(start_beat);

        // Port of C# TempoRecorder.CaptureProjectBpm (line 179).
        project
            .recording_provenance
            .set_recorded_project_bpm(start_bpm, start_source, false);

        // Resolve start tick. Port of C# TempoRecorder.TrackClipStart lines 181-183.
        let resolved_start_tick = if clip.start_absolute_tick >= 0 {
            clip.start_absolute_tick
        } else {
            (start_beat.as_f32() * MIDI_CLOCK_TICKS_PER_BEAT as f32).round() as i32
        };

        self.clip_starts.insert(
            clip.id.clone(),
            RecordingClipStartInfo {
                midi_note,
                start_time_seconds: host.beat_to_timeline_time(start_beat),
                start_beat,
                start_absolute_tick: resolved_start_tick,
                start_bpm,
                start_tempo_source: start_source,
            },
        );
    }

    /// Finalize recording clip provenance metadata.
    /// Port of C# LiveClipManager.FinalizeRecordingClip (lines 836-849)
    /// + TempoRecorder.FinalizeClip (lines 202-241).
    pub fn finalize_recording_clip(
        &mut self,
        host: &dyn LiveClipHost,
        project: &mut Project,
        live_clip_id: &str,
        recorded_clip: &TimelineClip,
        end_beat: Beats,
        end_absolute_tick: i32,
        midi_note: i32,
    ) {
        let start = match self.clip_starts.remove(live_clip_id) {
            Some(s) => s,
            None => return,
        };

        // Resolve end tick. Port of C# TempoRecorder.FinalizeClip lines 214-216.
        let resolved_end_tick = if end_absolute_tick >= 0 {
            end_absolute_tick
        } else {
            (end_beat.0 * MIDI_CLOCK_TICKS_PER_BEAT as f64).round() as i32
        };

        // Resolve MIDI note. Port of C# TempoRecorder.FinalizeClip line 217.
        let resolved_midi_note = if midi_note >= 0 {
            midi_note
        } else {
            start.midi_note
        };

        // Use recorded clip's identity/layer if available.
        // Port of C# TempoRecorder.FinalizeClip lines 219-221.
        let saved_clip_id = recorded_clip.id.clone();
        let saved_video_id = recorded_clip.video_clip_id.clone();
        let saved_layer = project
            .timeline
            .layer_index_for_id(&recorded_clip.layer_id)
            .unwrap_or(0) as i32;

        let end_bpm = host.get_bpm_at_beat(end_beat);
        let end_source = host.get_tempo_source_at_beat(end_beat);
        let end_time = host.beat_to_timeline_time(end_beat);

        let entry = RecordedClipProvenance {
            clip_id: saved_clip_id,
            video_clip_id: saved_video_id,
            layer_index: saved_layer.max(0),
            layer_id: None,
            midi_note: resolved_midi_note,
            start_time_seconds: BeatQuantizer::quantize_time_seconds(start.start_time_seconds)
                .as_f32(),
            end_time_seconds: BeatQuantizer::quantize_time_seconds(end_time).as_f32(),
            start_beat: BeatQuantizer::quantize_beat(start.start_beat),
            end_beat: BeatQuantizer::quantize_beat(end_beat),
            start_absolute_tick: start.start_absolute_tick,
            end_absolute_tick: resolved_end_tick,
            start_bpm: Bpm(BeatQuantizer::quantize_bpm(start.start_bpm.0)),
            end_bpm: Bpm(BeatQuantizer::quantize_bpm(end_bpm)),
            start_tempo_source: start.start_tempo_source,
            end_tempo_source: end_source,
        };

        project.recording_provenance.add_recorded_clip(entry);
    }

    /// Remove a recording clip start tracking entry.
    /// Port of C# LiveClipManager.RemoveRecordingClipStart (lines 851-858)
    /// + TempoRecorder.RemoveClipStart (lines 243-248).
    pub fn remove_recording_clip_start(&mut self, clip_id: &str) {
        if clip_id.is_empty() {
            return;
        }
        self.clip_starts.remove(clip_id);
    }

    // ─── Prewarm candidates (Phase 7D) ───

    /// Append live clip prewarm candidates for video decoding.
    /// Port of C# LiveClipManager.AppendLivePrewarmCandidates (lines 860-965).
    ///
    /// This populates prewarmed clip candidates based on:
    /// - Currently active live slots
    /// - Recently triggered clips (recency priority)
    /// - MIDI-mapped layer source clips
    ///
    /// Requires MidiMapping and VideoLibrary infrastructure (not yet ported) — stubbed.
    pub fn append_live_prewarm_candidates(
        &self,
        _candidates: &mut Vec<TimelineClip>,
        _max_unique: usize,
        _existing_ids: &HashSet<ClipId>,
    ) {
        // TODO: Port full prewarm logic when MidiMapping and VideoLibrary are ported.
        // For now, add currently active live slot clips as prewarm candidates.
        for clip in self.live_slots.values() {
            if _candidates.len() >= _max_unique {
                break;
            }
            if !_existing_ids.contains(&clip.id) {
                _candidates.push(clip.clone());
            }
        }
    }
}

impl Default for LiveClipManager {
    fn default() -> Self {
        Self::new()
    }
}
