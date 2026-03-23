use manifold_core::ClipId;
use std::collections::HashMap;

use manifold_core::midi::MidiNoteMapping;
use manifold_core::project::Project;
use manifold_core::tempo::TempoMapConverter;
use manifold_core::GeneratorTypeId;
use manifold_core::video::VideoClip;

use crate::live_clip_manager::{LiveClipHost, LiveClipManager};

/// Callback type for clip launch events.
/// Args: midi_note, video_clip_id, layer_index, start_time, in_point
type ClipLaunchedCallback = Option<Box<dyn FnMut(i32, String, i32, f32, f32) + Send>>;

/// Callback type for clip stop events.
/// Args: midi_note, layer_index, stop_time
type ClipStoppedCallback = Option<Box<dyn FnMut(i32, i32, f32) + Send>>;

/// Bridges MIDI input to LiveClipManager.
/// Handles random clip selection, random in-point generation,
/// and NoteOff tracking. All clips use hold-to-play (NoteOff) behaviour.
/// Port of C# ClipLauncher.cs.
pub struct ClipLauncher {
    randomize_in_point: bool,
    show_debug_logs: bool,

    // Track active NoteOff clips: (midi_note, device_id) → NoteOffTracking
    // Compound key prevents different MIDI devices from interfering with each other.
    active_note_off_clips: HashMap<(i32, i32), NoteOffTracking>,

    // Avoid triggering the same random clip twice in a row per note
    last_triggered_clip_id: HashMap<i32, String>,

    /// Fired when a clip is launched (for Phase 4 recording).
    pub on_clip_launched: ClipLaunchedCallback,

    /// Fired when a clip is stopped via NoteOff (for Phase 4 recording).
    pub on_clip_stopped: ClipStoppedCallback,
}

/// Tracks an active note for NoteOff matching.
/// Port of C# ClipLauncher.NoteOffTracking.
struct NoteOffTracking {
    layer_index: i32,
    clip_id: ClipId,
    source_channel: i32,
    #[allow(dead_code)]
    source_device_id: i32,
    /// Time.realtimeSinceStartup at NoteOn — guards against stale NoteOff.
    creation_time: f64,
    /// Native queue sequence for deterministic stale-event filtering.
    creation_sequence: u32,
    /// Native absolute tick captured at NoteOn.
    #[allow(dead_code)]
    creation_absolute_tick: i32,
}

impl ClipLauncher {
    pub fn new() -> Self {
        Self {
            randomize_in_point: false,
            show_debug_logs: false,
            active_note_off_clips: HashMap::new(),
            last_triggered_clip_id: HashMap::new(),
            on_clip_launched: None,
            on_clip_stopped: None,
        }
    }

    pub fn set_randomize_in_point(&mut self, value: bool) {
        self.randomize_in_point = value;
    }

    pub fn set_show_debug_logs(&mut self, value: bool) {
        self.show_debug_logs = value;
    }

    /// Called by MidiInputController when a note-on event occurs.
    /// Port of C# ClipLauncher.HandleNoteOn (lines 46–112).
    #[allow(clippy::too_many_arguments)]
    pub fn handle_note_on(
        &mut self,
        project: &mut Project,
        live_clip_manager: &mut LiveClipManager,
        host: &mut dyn LiveClipHost,
        midi_note: i32,
        velocity: f32,
        midi_channel: i32,
        device_id: i32,
        mapping: &MidiNoteMapping,
        beat_stamp: Option<f32>,
        event_sequence: u32,
        event_absolute_tick: i32,
        realtime_now: f64,
    ) {
        let key = (midi_note, device_id);

        // Auto-commit existing clip for this note+device before creating new one
        // (handles NoteOff + NoteOn in same frame when NoteOn processes first)
        if let Some(existing) = self.active_note_off_clips.remove(&key) {
            live_clip_manager.commit_live_clip(
                project,
                host,
                existing.layer_index,
                Some(&existing.clip_id),
                beat_stamp,
                event_absolute_tick,
                realtime_now,
                midi_note,
            );
        }

        // Select a random clip from the mapping
        let video_clip_id = match Self::select_random_clip(
            &mut self.last_triggered_clip_id,
            midi_note,
            &mapping.video_clip_ids,
            event_sequence,
        ) {
            Some(id) => id,
            None => return,
        };

        // Look up the video clip to get its duration
        let video_clip: VideoClip = match project.video_library.find_clip_by_id(&video_clip_id) {
            Some(c) => c.clone(),
            None => {
                log::warn!("[ClipLauncher] Video clip not found: {}", video_clip_id);
                return;
            }
        };

        // Fallback: if metadata hasn't been extracted yet, use a default duration
        let clip_duration = if video_clip.duration > 0.0 { video_clip.duration } else { 30.0 };

        // Calculate in-point
        let in_point = Self::compute_in_point(
            self.randomize_in_point,
            clip_duration,
            midi_note,
            event_sequence,
        );

        let remaining_duration = clip_duration - in_point;

        let layer_index = mapping.target_layer_index;

        // Always use live slot path — clip is committed to timeline on NoteOff
        let clip = live_clip_manager.trigger_live_clip(
            project,
            host,
            video_clip_id.clone(),
            layer_index,
            remaining_duration,
            in_point,
            beat_stamp,
            event_absolute_tick,
            realtime_now,
            midi_note,
        );

        let clip = match clip {
            Some(c) => c,
            None => return,
        };

        // Track for NoteOff
        self.active_note_off_clips.insert(
            key,
            NoteOffTracking {
                layer_index,
                clip_id: clip.id.clone(),
                source_channel: midi_channel,
                source_device_id: device_id,
                creation_time: realtime_now,
                creation_sequence: event_sequence,
                creation_absolute_tick: event_absolute_tick,
            },
        );

        let current_time = host.current_time();
        if let Some(cb) = &mut self.on_clip_launched {
            cb(midi_note, video_clip_id, layer_index, current_time, in_point);
        }

        if self.show_debug_logs {
            log::debug!(
                "[ClipLauncher] Note {} ch={} → {} on layer {} (inPoint={:.2}s, vel={:.2})",
                midi_note, midi_channel, video_clip.file_name, layer_index, in_point, velocity
            );
        }
    }

    /// Called by MidiInputController when a note-off event occurs.
    /// Truncates the clip to held duration and commits to timeline if recording.
    /// Port of C# ClipLauncher.HandleNoteOff (lines 118–156).
    #[allow(clippy::too_many_arguments)]
    pub fn handle_note_off(
        &mut self,
        project: &mut Project,
        live_clip_manager: &mut LiveClipManager,
        host: &mut dyn LiveClipHost,
        midi_note: i32,
        midi_channel: i32,
        device_id: i32,
        beat_stamp: Option<f32>,
        event_sequence: u32,
        event_absolute_tick: i32,
        realtime_now: f64,
    ) {
        let key = (midi_note, device_id);

        let tracking = match self.active_note_off_clips.get(&key) {
            Some(t) => t,
            None => return,
        };

        // Channel sanity check (compound key already isolates by device)
        if tracking.source_channel != midi_channel {
            return;
        }

        // Stale NoteOff guard for frame-timestamped Minis callbacks only.
        // Native clock-domain note events provide deterministic ordering and
        // explicit beat stamps, so they should not use this time-based filter.
        let has_beat_stamp = beat_stamp.is_some();
        if !has_beat_stamp && realtime_now - tracking.creation_time < 0.005 {
            return;
        }

        // Deterministic stale NoteOff guard for native-sequenced events.
        // If NoteOn and an old NoteOff share a frame, sequence order cleanly
        // disambiguates them without real-time thresholds.
        if event_sequence > 0 && tracking.creation_sequence > 0
            && event_sequence <= tracking.creation_sequence
        {
            return;
        }

        let layer_index = tracking.layer_index;
        let clip_id = tracking.clip_id.clone();

        self.active_note_off_clips.remove(&key);

        live_clip_manager.commit_live_clip(
            project,
            host,
            layer_index,
            Some(&clip_id),
            beat_stamp,
            event_absolute_tick,
            realtime_now,
            midi_note,
        );

        let current_time = host.current_time();
        if let Some(cb) = &mut self.on_clip_stopped {
            cb(midi_note, layer_index, current_time);
        }

        if self.show_debug_logs {
            log::debug!(
                "[ClipLauncher] NoteOff {} ch={} → committed layer {}",
                midi_note, midi_channel, layer_index
            );
        }
    }

    /// Called by MidiInputController when a note-on event occurs.
    /// Tries to find a layer that owns this MIDI note and triggers a random clip from its folder.
    /// Returns true if handled, false if no layer matched (caller should fall back to MidiMappingConfig).
    /// Port of C# ClipLauncher.HandleNoteOnFromLayer (lines 232–351).
    #[allow(clippy::too_many_arguments)]
    pub fn handle_note_on_from_layer(
        &mut self,
        project: &mut Project,
        live_clip_manager: &mut LiveClipManager,
        host: &mut dyn LiveClipHost,
        midi_note: i32,
        velocity: f32,
        midi_channel: i32,
        device_id: i32,
        beat_stamp: Option<f32>,
        event_sequence: u32,
        event_absolute_tick: i32,
        realtime_now: f64,
    ) -> bool {
        let key = (midi_note, device_id);

        // Auto-commit existing clip for this note+device before creating new one
        // (handles NoteOff + NoteOn in same frame when NoteOn processes first)
        if let Some(existing) = self.active_note_off_clips.remove(&key) {
            live_clip_manager.commit_live_clip(
                project,
                host,
                existing.layer_index,
                Some(&existing.clip_id),
                beat_stamp,
                event_absolute_tick,
                realtime_now,
                midi_note,
            );
        }

        // Find layer that owns this MIDI note and matches channel filter
        let target_layer_index = {
            let mut found = None;
            for layer in &project.timeline.layers {
                if layer.midi_note == midi_note
                    && (layer.midi_channel < 0 || layer.midi_channel == midi_channel)
                {
                    found = Some(layer.index);
                    break;
                }
            }
            found
        };

        let layer_index = match target_layer_index {
            Some(idx) => idx,
            None => return false,
        };

        // Check generator path
        let generator_type = project
            .timeline
            .layers
            .get(layer_index as usize)
            .map(|l| l.generator_type().clone())
            .unwrap_or(GeneratorTypeId::NONE);

        if generator_type != GeneratorTypeId::NONE {
            let bpm = project.settings.bpm;
            let spb = TempoMapConverter::seconds_per_beat_from_bpm(bpm);
            let generator_duration = spb * 4.0; // 1 bar at 4/4 default feel

            let gen_clip = live_clip_manager.trigger_live_generator_clip(
                project,
                host,
                generator_type.clone(),
                layer_index,
                generator_duration,
                beat_stamp,
                event_absolute_tick,
                realtime_now,
                midi_note,
            );

            let gen_clip = match gen_clip {
                Some(c) => c,
                None => return false,
            };

            self.active_note_off_clips.insert(
                key,
                NoteOffTracking {
                    layer_index,
                    clip_id: gen_clip.id.clone(),
                    source_channel: midi_channel,
                    source_device_id: device_id,
                    creation_time: realtime_now,
                    creation_sequence: event_sequence,
                    creation_absolute_tick: event_absolute_tick,
                },
            );

            let current_time = host.current_time();
            if let Some(cb) = &mut self.on_clip_launched {
                cb(
                    midi_note,
                    format!("generator:{:?}", generator_type),
                    layer_index,
                    current_time,
                    0.0,
                );
            }

            if self.show_debug_logs {
                let layer_name = project
                    .timeline
                    .layers
                    .get(layer_index as usize)
                    .map(|l| l.name.as_str())
                    .unwrap_or("");
                log::debug!(
                    "[ClipLauncher] Note {} ch={} → generator {:?} on layer {} \"{}\"",
                    midi_note, midi_channel, generator_type, layer_index, layer_name
                );
            }

            return true;
        }

        let source_clip_ids: Vec<String> = project
            .timeline
            .layers
            .get(layer_index as usize)
            .map(|l| l.source_clip_ids.clone())
            .unwrap_or_default();

        if source_clip_ids.is_empty() {
            return false;
        }

        // Select a random clip from the layer's folder
        let video_clip_id = match Self::select_random_clip(
            &mut self.last_triggered_clip_id,
            midi_note,
            &source_clip_ids,
            event_sequence,
        ) {
            Some(id) => id,
            None => return false,
        };

        let video_clip: VideoClip = match project.video_library.find_clip_by_id(&video_clip_id) {
            Some(c) => c.clone(),
            None => {
                log::warn!(
                    "[ClipLauncher] Video clip not found in library: {}",
                    video_clip_id
                );
                return false;
            }
        };

        // Fallback: if metadata hasn't been extracted yet, use a default duration
        let clip_duration = if video_clip.duration > 0.0 { video_clip.duration } else { 30.0 };

        // Calculate in-point
        let in_point = Self::compute_in_point(
            self.randomize_in_point,
            clip_duration,
            midi_note,
            event_sequence,
        );

        let remaining_duration = clip_duration - in_point;

        // Always use live slot path — clip is committed to timeline on NoteOff
        let clip = live_clip_manager.trigger_live_clip(
            project,
            host,
            video_clip_id.clone(),
            layer_index,
            remaining_duration,
            in_point,
            beat_stamp,
            event_absolute_tick,
            realtime_now,
            midi_note,
        );

        let clip = match clip {
            Some(c) => c,
            None => return false,
        };

        // Track for NoteOff
        self.active_note_off_clips.insert(
            key,
            NoteOffTracking {
                layer_index,
                clip_id: clip.id.clone(),
                source_channel: midi_channel,
                source_device_id: device_id,
                creation_time: realtime_now,
                creation_sequence: event_sequence,
                creation_absolute_tick: event_absolute_tick,
            },
        );

        let current_time = host.current_time();
        if let Some(cb) = &mut self.on_clip_launched {
            cb(midi_note, video_clip_id, layer_index, current_time, in_point);
        }

        if self.show_debug_logs {
            let layer_name = project
                .timeline
                .layers
                .get(layer_index as usize)
                .map(|l| l.name.as_str())
                .unwrap_or("");
            log::debug!(
                "[ClipLauncher] Note {} ch={} → {} on layer {} \"{}\" (inPoint={:.2}s)",
                midi_note, midi_channel, video_clip.file_name, layer_index, layer_name, in_point
            );
        }

        // suppress unused warning for velocity (present in C# signature)
        let _ = velocity;

        true
    }

    /// Clear all tracking state (call on stop/reset).
    /// Port of C# ClipLauncher.ClearAll (lines 356–360).
    pub fn clear_all(&mut self) {
        self.active_note_off_clips.clear();
        self.last_triggered_clip_id.clear();
    }

    // ─── Private helpers ───

    /// Select a random clip from the list, avoiding the same clip twice in a row.
    /// Port of C# ClipLauncher.SelectRandomClip (lines 161–189).
    fn select_random_clip(
        last_triggered_clip_id: &mut HashMap<i32, String>,
        midi_note: i32,
        clip_ids: &[String],
        event_sequence: u32,
    ) -> Option<String> {
        if clip_ids.is_empty() {
            return None;
        }
        if clip_ids.len() == 1 {
            return Some(clip_ids[0].clone());
        }

        let last_id = last_triggered_clip_id.get(&midi_note).cloned();

        let selected: String;
        if event_sequence > 0 {
            // Unity: unchecked(eventSequence ^ (uint)midiNote * 2654435761u)
            // C# operator precedence: * before ^, so: eventSequence ^ ((uint)midiNote * 2654435761u)
            let seed = event_sequence ^ (midi_note as u32).wrapping_mul(2654435761u32);
            let idx = Self::deterministic_index(clip_ids.len(), seed);
            let candidate = clip_ids[idx].clone();
            selected = if Some(&candidate) == last_id.as_ref() {
                clip_ids[(idx + 1) % clip_ids.len()].clone()
            } else {
                candidate
            };
        } else {
            let mut attempts = 0;
            let mut candidate;
            loop {
                let r = Self::pseudo_random_range(clip_ids.len());
                candidate = clip_ids[r].clone();
                attempts += 1;
                if Some(&candidate) != last_id.as_ref() || attempts >= 5 {
                    break;
                }
            }
            selected = candidate;
        }

        last_triggered_clip_id.insert(midi_note, selected.clone());
        Some(selected)
    }

    fn compute_in_point(
        randomize_in_point: bool,
        clip_duration: f32,
        midi_note: i32,
        event_sequence: u32,
    ) -> f32 {
        if !randomize_in_point || clip_duration <= 1.0 {
            return 0.0;
        }

        let max_in_point = clip_duration - 1.0;
        if event_sequence > 0 {
            let seed = event_sequence ^ (midi_note as u32).wrapping_mul(1597334677u32);
            Self::unit_from_seed(seed) * max_in_point
        } else {
            Self::pseudo_random_unit() * max_in_point
        }
    }

    fn deterministic_index(count: usize, seed: u32) -> usize {
        if count <= 1 {
            return 0;
        }
        (Self::mix_seed(seed) % count as u32) as usize
    }

    fn unit_from_seed(seed: u32) -> f32 {
        let mixed = Self::mix_seed(seed);
        (mixed & 0x00FF_FFFFu32) as f32 / 16_777_216.0
    }

    /// Port of C# ClipLauncher.MixSeed (lines 217–225).
    fn mix_seed(mut x: u32) -> u32 {
        x ^= x >> 16;
        x = x.wrapping_mul(0x7feb352du32);
        x ^= x >> 15;
        x = x.wrapping_mul(0x846ca68bu32);
        x ^= x >> 16;
        x
    }

    /// Non-deterministic uniform integer in [0, count). Replaces UnityEngine.Random.Range.
    fn pseudo_random_range(count: usize) -> usize {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        use std::time::SystemTime;

        let mut hasher = DefaultHasher::new();
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            .hash(&mut hasher);
        (hasher.finish() as usize) % count
    }

    /// Non-deterministic float in [0, 1). Replaces UnityEngine.Random.Range(0f, maxInPoint) / maxInPoint.
    fn pseudo_random_unit() -> f32 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        use std::time::SystemTime;

        let mut hasher = DefaultHasher::new();
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            .hash(&mut hasher);
        (hasher.finish() & 0x00FF_FFFFu64) as f32 / 16_777_216.0
    }
}

impl Default for ClipLauncher {
    fn default() -> Self {
        Self::new()
    }
}
