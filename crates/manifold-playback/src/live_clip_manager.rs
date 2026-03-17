use manifold_core::clip::TimelineClip;
use manifold_core::project::Project;
use manifold_core::types::{GeneratorType, QuantizeMode};
use manifold_editing::command::Command;
use manifold_editing::commands::clip::AddClipCommand;

use std::collections::{BTreeMap, HashMap, HashSet};

/// MIDI clock ticks per beat (standard).
pub const MIDI_CLOCK_TICKS_PER_BEAT: i32 = 24;
/// Ticks per sixteenth note.
const TICKS_PER_SIXTEENTH: i32 = MIDI_CLOCK_TICKS_PER_BEAT / 4; // 6

/// Host interface for LiveClipManager.
pub trait LiveClipHost {
    fn current_beat(&self) -> f32;
    fn current_time(&self) -> f32;
    fn is_recording(&self) -> bool;
    fn is_playing(&self) -> bool;
    fn get_bpm_at_beat(&self, beat: f32) -> f32;
    fn get_beat_snapped_beat(&self) -> f32;
    fn get_current_absolute_tick(&self) -> i32;
    fn stop_clip(&mut self, clip_id: &str);
    fn mark_sync_dirty(&mut self);
    fn mark_compositor_dirty(&mut self);
    fn register_clip_lookup(&mut self, clip_id: &str, clip: &TimelineClip);
    fn record_command(&mut self, cmd: Box<dyn Command>);
    fn beat_to_timeline_time(&self, beat: f32) -> f32;
}

/// Queued launch waiting for a target tick.
#[derive(Debug, Clone)]
struct PendingLiveLaunch {
    clip: TimelineClip,
    layer_index: i32,
    target_tick: i32,
    #[allow(dead_code)]
    midi_note: i32,
}

/// 5ms timing guard threshold (seconds). Reject NoteOff within this window of NoteOn.
/// Port of C# ClipLauncher STALE_NOTE_OFF_THRESHOLD (lines 129-143).
const NOTE_OFF_TIMING_GUARD: f64 = 0.005;

/// Manages phantom (live-triggered) clips for MIDI performance.
/// Port of C# LiveClipManager.
pub struct LiveClipManager {
    // Active live slots: layer_index → phantom clip
    live_slots: HashMap<i32, TimelineClip>,
    live_slots_list: Vec<(i32, TimelineClip)>,
    live_slot_clip_ids: HashSet<String>,

    // Pending launches (queued for future ticks)
    pending_by_clip_id: HashMap<String, PendingLiveLaunch>,
    pending_by_layer: HashMap<i32, String>,
    pending_by_tick: BTreeMap<i32, Vec<String>>,

    // Tracking
    last_live_trigger_at: f64,

    // Per-slot creation timestamps for 5ms NoteOff timing guard.
    // Port of C# ClipLauncher.TrackedNote.CreationTime / CreationSequence.
    slot_creation_times: HashMap<i32, f64>,
    slot_creation_sequences: HashMap<i32, i32>,
}

impl LiveClipManager {
    pub fn new() -> Self {
        Self {
            live_slots: HashMap::with_capacity(8),
            live_slots_list: Vec::with_capacity(8),
            live_slot_clip_ids: HashSet::with_capacity(8),
            pending_by_clip_id: HashMap::with_capacity(4),
            pending_by_layer: HashMap::with_capacity(4),
            pending_by_tick: BTreeMap::new(),
            last_live_trigger_at: 0.0,
            slot_creation_times: HashMap::with_capacity(8),
            slot_creation_sequences: HashMap::with_capacity(8),
        }
    }

    // ─── Accessors ───

    pub fn live_slots(&self) -> &HashMap<i32, TimelineClip> { &self.live_slots }
    pub fn live_slots_list(&self) -> &[(i32, TimelineClip)] { &self.live_slots_list }
    pub fn live_slot_clip_ids(&self) -> &HashSet<String> { &self.live_slot_clip_ids }
    pub fn pending_launch_count(&self) -> usize { self.pending_by_clip_id.len() }
    pub fn last_live_trigger_at(&self) -> f64 { self.last_live_trigger_at }

    // ─── Lifecycle ───

    pub fn clear_all(&mut self) {
        self.live_slots.clear();
        self.live_slots_list.clear();
        self.live_slot_clip_ids.clear();
        self.pending_by_clip_id.clear();
        self.pending_by_layer.clear();
        self.pending_by_tick.clear();
        self.slot_creation_times.clear();
        self.slot_creation_sequences.clear();
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
        }
        if seek_delta > 1.0 {
            self.pending_by_clip_id.clear();
            self.pending_by_layer.clear();
            self.pending_by_tick.clear();
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
    pub fn get_quantize_interval_ticks(quantize_mode: QuantizeMode, time_sig_numerator: i32) -> i32 {
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
    ) -> f32 {
        if event_absolute_tick >= 0 && spb > 0.0 {
            // Tick-based path
            let raw_ticks = ((duration_seconds / spb) * MIDI_CLOCK_TICKS_PER_BEAT as f32) as i32;
            let interval = Self::get_quantize_interval_ticks(quantize_mode, time_sig_numerator);
            let snapped_ticks = if quantize_mode != QuantizeMode::Off && interval > 0 {
                ((raw_ticks as f32 / interval as f32).round() as i32).max(1) * interval
            } else {
                raw_ticks.max(1)
            };
            snapped_ticks as f32 / MIDI_CLOCK_TICKS_PER_BEAT as f32
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
                rounded.max(1.0 / MIDI_CLOCK_TICKS_PER_BEAT as f32)
            } else {
                raw_beats.max(1.0 / MIDI_CLOCK_TICKS_PER_BEAT as f32)
            }
        } else {
            1.0
        }
    }

    /// Snap a tick position to the quantize grid, returning beats.
    pub fn compute_snap_beat_from_tick(
        tick: i32,
        quantize_mode: QuantizeMode,
        time_sig_numerator: i32,
        ceil_to_next_grid: bool,
    ) -> f32 {
        let mut snapped_tick = tick;

        // Sixteenth-note compensation: snap to nearest 16th if within ±1 tick
        let nearest_16th = ((tick as f32 / TICKS_PER_SIXTEENTH as f32).round() as i32) * TICKS_PER_SIXTEENTH;
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

        snapped_tick as f32 / MIDI_CLOCK_TICKS_PER_BEAT as f32
    }

    /// Compute held beats from start and end ticks.
    pub fn compute_held_beats_from_ticks(
        start_tick: i32,
        end_tick: i32,
        quantize_mode: QuantizeMode,
        time_sig_numerator: i32,
    ) -> f32 {
        let held_ticks = (end_tick - start_tick).max(1);
        let interval = Self::get_quantize_interval_ticks(quantize_mode, time_sig_numerator);

        let snapped = if quantize_mode != QuantizeMode::Off && interval > 0 {
            ((held_ticks as f32 / interval as f32).round() as i32).max(1) * interval
        } else {
            held_ticks
        };

        snapped as f32 / MIDI_CLOCK_TICKS_PER_BEAT as f32
    }

    // ─── Pending launch queue ───

    fn queue_pending(&mut self, clip_id: String, launch: PendingLiveLaunch) {
        let tick = launch.target_tick;
        let layer = launch.layer_index;

        // Remove any existing pending for this layer
        if let Some(old_id) = self.pending_by_layer.remove(&layer) {
            self.remove_pending_by_clip_id(&old_id);
        }

        self.pending_by_clip_id.insert(clip_id.clone(), launch);
        self.pending_by_layer.insert(layer, clip_id.clone());
        self.pending_by_tick.entry(tick).or_default().push(clip_id);
    }

    fn remove_pending_by_clip_id(&mut self, clip_id: &str) {
        if let Some(launch) = self.pending_by_clip_id.remove(clip_id) {
            self.pending_by_layer.remove(&launch.layer_index);
            if let Some(ids) = self.pending_by_tick.get_mut(&launch.target_tick) {
                ids.retain(|id| id != clip_id);
                if ids.is_empty() {
                    self.pending_by_tick.remove(&launch.target_tick);
                }
            }
        }
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

    /// Process pending launches that have reached their target tick.
    /// Returns true if any launches were activated.
    pub fn activate_due_pending_launches(&mut self, host: &dyn LiveClipHost) -> bool {
        if self.pending_by_tick.is_empty() {
            return false;
        }

        let now_tick = host.get_current_absolute_tick();
        let mut any_activated = false;

        // Collect all due launches first to avoid borrow conflicts
        let mut due_launches: Vec<PendingLiveLaunch> = Vec::new();

        while let Some(&earliest_tick) = self.pending_by_tick.keys().next() {
            if earliest_tick > now_tick {
                break;
            }

            let clip_ids = match self.pending_by_tick.remove(&earliest_tick) {
                Some(ids) => ids,
                None => break,
            };

            for clip_id in clip_ids {
                if let Some(launch) = self.pending_by_clip_id.remove(&clip_id) {
                    self.pending_by_layer.remove(&launch.layer_index);
                    due_launches.push(launch);
                }
            }
        }

        // Now activate all collected launches
        for launch in due_launches {
            self.activate_live_slot_now(launch.layer_index, launch.clip);
            any_activated = true;
        }

        any_activated
    }

    /// Check if any pending launches activated (for the engine to call mark_dirty).
    pub fn has_pending_activations(&self, host: &dyn LiveClipHost) -> bool {
        if let Some(&earliest) = self.pending_by_tick.keys().next() {
            earliest <= host.get_current_absolute_tick()
        } else {
            false
        }
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
        realtime_now: f64,
    ) -> Option<TimelineClip> {
        // Ensure enough layers exist
        project.timeline.ensure_layer_count((layer_index + 1) as usize);

        let spb = 60.0 / host.get_bpm_at_beat(host.current_beat());

        // Compute snap beat
        let snap_beat = self.compute_trigger_snap_beat(
            host, beat_stamp, event_absolute_tick,
            project.settings.quantize_mode,
            project.settings.time_signature_numerator,
        );

        // Compute duration
        let duration_beats = Self::compute_duration_beats(
            duration_seconds, spb, event_absolute_tick,
            project.settings.quantize_mode,
            project.settings.time_signature_numerator,
        );

        // Create phantom clip
        let mut clip = TimelineClip::new_video(
            video_clip_id,
            layer_index,
            snap_beat,
            duration_beats,
            in_point,
        );
        clip.recorded_bpm = host.get_bpm_at_beat(snap_beat);

        if event_absolute_tick >= 0 {
            clip.start_absolute_tick = (snap_beat * MIDI_CLOCK_TICKS_PER_BEAT as f32) as i32;
            clip.has_start_absolute_tick = true;
        }

        // Queue or activate immediately
        let target_tick = (snap_beat * MIDI_CLOCK_TICKS_PER_BEAT as f32) as i32;
        if event_absolute_tick >= 0 && target_tick > event_absolute_tick {
            // Queue for future activation
            let launch = PendingLiveLaunch {
                clip: clip.clone(),
                layer_index,
                target_tick,
                midi_note: -1,
            };
            self.queue_pending(clip.id.clone(), launch);
        } else {
            self.activate_live_slot_now(layer_index, clip.clone());
        }

        // Record creation timestamps for 5ms NoteOff timing guard
        self.slot_creation_times.insert(layer_index, realtime_now);
        if event_absolute_tick >= 0 {
            self.slot_creation_sequences.insert(layer_index, event_absolute_tick);
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
        generator_type: GeneratorType,
        layer_index: i32,
        duration_seconds: f32,
        beat_stamp: Option<f32>,
        event_absolute_tick: i32,
        realtime_now: f64,
    ) -> Option<TimelineClip> {
        project.timeline.ensure_layer_count((layer_index + 1) as usize);

        let spb = 60.0 / host.get_bpm_at_beat(host.current_beat());

        let snap_beat = self.compute_trigger_snap_beat(
            host, beat_stamp, event_absolute_tick,
            project.settings.quantize_mode,
            project.settings.time_signature_numerator,
        );

        let duration_beats = Self::compute_duration_beats(
            duration_seconds, spb, event_absolute_tick,
            project.settings.quantize_mode,
            project.settings.time_signature_numerator,
        );

        let mut clip = TimelineClip::new_generator(
            generator_type, layer_index, snap_beat, duration_beats,
        );
        clip.recorded_bpm = host.get_bpm_at_beat(snap_beat);

        if event_absolute_tick >= 0 {
            clip.start_absolute_tick = (snap_beat * MIDI_CLOCK_TICKS_PER_BEAT as f32) as i32;
            clip.has_start_absolute_tick = true;
        }

        let target_tick = (snap_beat * MIDI_CLOCK_TICKS_PER_BEAT as f32) as i32;
        if event_absolute_tick >= 0 && target_tick > event_absolute_tick {
            let launch = PendingLiveLaunch {
                clip: clip.clone(),
                layer_index,
                target_tick,
                midi_note: -1,
            };
            self.queue_pending(clip.id.clone(), launch);
        } else {
            self.activate_live_slot_now(layer_index, clip.clone());
        }

        // Record creation timestamps for 5ms NoteOff timing guard
        self.slot_creation_times.insert(layer_index, realtime_now);
        if event_absolute_tick >= 0 {
            self.slot_creation_sequences.insert(layer_index, event_absolute_tick);
        }

        self.last_live_trigger_at = realtime_now;
        Some(clip)
    }

    fn compute_trigger_snap_beat(
        &self,
        host: &dyn LiveClipHost,
        beat_stamp: Option<f32>,
        event_absolute_tick: i32,
        quantize_mode: QuantizeMode,
        time_sig_numerator: i32,
    ) -> f32 {
        if event_absolute_tick >= 0 {
            Self::compute_snap_beat_from_tick(
                event_absolute_tick,
                quantize_mode,
                time_sig_numerator,
                true, // ceil to next grid for NoteOn
            )
        } else if let Some(stamp) = beat_stamp {
            if quantize_mode != QuantizeMode::Off {
                let interval = match quantize_mode {
                    QuantizeMode::QuarterBeat => 0.25,
                    QuantizeMode::Beat => 1.0,
                    QuantizeMode::Bar => time_sig_numerator as f32,
                    QuantizeMode::Off => 1.0,
                };
                (stamp / interval).round() * interval
            } else {
                stamp
            }
        } else {
            host.get_beat_snapped_beat()
        }
    }

    // ─── Commit ───

    /// Commit a live clip (NoteOff). If recording, adds to timeline.
    /// Port of C# ClipLauncher.HandleNoteOff with 5ms timing guard.
    pub fn commit_live_clip(
        &mut self,
        project: &mut Project,
        host: &mut dyn LiveClipHost,
        layer_index: i32,
        clip_id: Option<&str>,
        beat_stamp: Option<f32>,
        event_absolute_tick: i32,
        realtime_now: f64,
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
            if event_absolute_tick > 0 {
                if let Some(&creation_seq) = self.slot_creation_sequences.get(&layer_index) {
                    if creation_seq > 0 && event_absolute_tick <= creation_seq {
                        return;
                    }
                }
            }
        }

        // Check for pending launch cancellation
        if !self.live_slots.contains_key(&layer_index) {
            if let Some(pending_id) = self.pending_by_layer.get(&layer_index).cloned() {
                if clip_id.is_none_or(|id| id == pending_id) {
                    self.remove_pending_by_clip_id(&pending_id);
                    return;
                }
            }
            return;
        }

        let live_clip = match self.live_slots.get(&layer_index) {
            Some(c) => c.clone(),
            None => return,
        };

        // If a specific clip_id was given but doesn't match, skip
        if let Some(id) = clip_id {
            if id != live_clip.id {
                return;
            }
        }

        let start_beat = live_clip.start_beat;

        // Compute held duration
        let held_beats = if event_absolute_tick >= 0 {
            let start_tick = if live_clip.has_start_absolute_tick {
                live_clip.start_absolute_tick
            } else {
                (start_beat * MIDI_CLOCK_TICKS_PER_BEAT as f32) as i32
            };
            Self::compute_held_beats_from_ticks(
                start_tick,
                event_absolute_tick,
                project.settings.quantize_mode,
                project.settings.time_signature_numerator,
            )
        } else {
            let beat_now = beat_stamp.unwrap_or_else(|| host.get_beat_snapped_beat());
            let raw = beat_now - start_beat;
            if project.settings.quantize_mode != QuantizeMode::Off {
                let interval = project.settings.get_quantize_interval_beats();
                if interval > 0.0 {
                    ((raw / interval).round() * interval).max(interval)
                } else {
                    raw.max(1.0 / MIDI_CLOCK_TICKS_PER_BEAT as f32)
                }
            } else {
                raw.max(1.0 / MIDI_CLOCK_TICKS_PER_BEAT as f32)
            }
        };

        // Remove live slot and creation tracking
        host.stop_clip(&live_clip.id);
        self.live_slots.remove(&layer_index);
        self.live_slots_list.retain(|(l, _)| *l != layer_index);
        self.live_slot_clip_ids.remove(&live_clip.id);
        self.slot_creation_times.remove(&layer_index);
        self.slot_creation_sequences.remove(&layer_index);

        // If recording, commit to timeline
        if host.is_recording() {
            let original_duration = live_clip.duration_beats;

            let mut committed = live_clip;
            committed.duration_beats = held_beats;

            // If held longer than original, enable looping
            if held_beats > original_duration {
                committed.is_looping = true;
                committed.loop_duration_beats = original_duration;
            }

            committed.layer_index = layer_index;

            if let Some(layer) = project.timeline.layers.get_mut(layer_index as usize) {
                layer.add_clip(committed.clone());
            }
            project.timeline.mark_clip_lookup_dirty();

            host.register_clip_lookup(&committed.id, &committed);
            host.record_command(Box::new(AddClipCommand::new(committed, layer_index)));
        }

        host.mark_sync_dirty();
        host.mark_compositor_dirty();
    }
}

impl Default for LiveClipManager {
    fn default() -> Self {
        Self::new()
    }
}
