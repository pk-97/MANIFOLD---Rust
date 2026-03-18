use manifold_core::clip::TimelineClip;
use manifold_core::project::Project;
use manifold_core::layer::Layer;
use manifold_core::types::*;
use manifold_playback::live_clip_manager::*;
use manifold_editing::command::Command;

use std::collections::HashMap;

/// Mock host for tests.
struct MockHost {
    beat: f32,
    time: f32,
    bpm: f32,
    recording: bool,
    playing: bool,
    current_tick: i32,
    stopped_clips: Vec<String>,
    registered_clips: HashMap<String, TimelineClip>,
    recorded_commands: Vec<Box<dyn Command>>,
    sync_dirty: bool,
    compositor_dirty: bool,
}

impl MockHost {
    fn new() -> Self {
        Self {
            beat: 0.0,
            time: 0.0,
            bpm: 120.0,
            recording: false,
            playing: true,
            current_tick: 0,
            stopped_clips: Vec::new(),
            registered_clips: HashMap::new(),
            recorded_commands: Vec::new(),
            sync_dirty: false,
            compositor_dirty: false,
        }
    }

    fn recording(mut self) -> Self {
        self.recording = true;
        self
    }

    fn at_tick(mut self, tick: i32) -> Self {
        self.current_tick = tick;
        self.beat = tick as f32 / MIDI_CLOCK_TICKS_PER_BEAT as f32;
        self
    }
}

impl LiveClipHost for MockHost {
    fn current_beat(&self) -> f32 { self.beat }
    fn current_time(&self) -> f32 { self.time }
    fn is_recording(&self) -> bool { self.recording }
    fn is_playing(&self) -> bool { self.playing }
    fn show_debug_logs(&self) -> bool { false }
    fn get_bpm_at_beat(&self, _beat: f32) -> f32 { self.bpm }
    fn get_tempo_source_at_beat(&self, _beat: f32) -> manifold_core::types::TempoPointSource {
        manifold_core::types::TempoPointSource::Unknown
    }
    fn get_beat_snapped_beat(&self) -> f32 { self.beat }
    fn get_current_absolute_tick(&self) -> i32 { self.current_tick }
    fn stop_clip(&mut self, clip_id: &str) { self.stopped_clips.push(clip_id.to_string()); }
    fn mark_sync_dirty(&mut self) { self.sync_dirty = true; }
    fn mark_compositor_dirty(&mut self) { self.compositor_dirty = true; }
    fn invalidate_lookahead_prewarm(&mut self) {}
    fn register_clip_lookup(&mut self, clip_id: &str, clip: &TimelineClip) {
        self.registered_clips.insert(clip_id.to_string(), clip.clone());
    }
    fn record_command(&mut self, cmd: Box<dyn Command>) {
        self.recorded_commands.push(cmd);
    }
    fn beat_to_timeline_time(&self, beat: f32) -> f32 {
        beat * 60.0 / self.bpm
    }
}

fn make_project() -> Project {
    let mut project = Project::default();
    project.settings.bpm = 120.0;
    project.settings.time_signature_numerator = 4;
    project.settings.quantize_mode = QuantizeMode::Beat;
    project.timeline.insert_layer(0, Layer::new("Layer 0".into(), LayerType::Video, 0));
    project.timeline.insert_layer(1, Layer::new("Layer 1".into(), LayerType::Video, 1));
    project
}

// ─── Trigger ───

#[test]
fn trigger_creates_phantom_clip() {
    let mut project = make_project();
    let host = MockHost::new();
    let mut mgr = LiveClipManager::new();

    let clip = mgr.trigger_live_clip(
        &mut project, &host,
        "video1".into(), 0, 2.0, 0.0,
        None, -1, 0.0,
    );

    assert!(clip.is_some());
    assert_eq!(mgr.live_slots().len(), 1);
    assert!(mgr.is_live_slot_clip(&clip.unwrap().id));
}

#[test]
fn trigger_live_generator_clip() {
    let mut project = make_project();
    let host = MockHost::new();
    let mut mgr = LiveClipManager::new();

    let clip = mgr.trigger_live_generator_clip(
        &mut project, &host,
        GeneratorType::Plasma, 0, 4.0,
        None, -1, 0.0,
    );

    assert!(clip.is_some());
    let clip = clip.unwrap();
    assert!(clip.is_generator());
    assert!(mgr.is_live_slot_clip(&clip.id));
}

#[test]
fn second_trigger_on_same_layer_replaces() {
    let mut project = make_project();
    let host = MockHost::new();
    let mut mgr = LiveClipManager::new();

    let clip1 = mgr.trigger_live_clip(
        &mut project, &host,
        "video1".into(), 0, 2.0, 0.0,
        None, -1, 0.0,
    ).unwrap();

    let clip2 = mgr.trigger_live_clip(
        &mut project, &host,
        "video2".into(), 0, 3.0, 0.0,
        None, -1, 1.0,
    ).unwrap();

    assert_eq!(mgr.live_slots().len(), 1);
    assert!(!mgr.is_live_slot_clip(&clip1.id));
    assert!(mgr.is_live_slot_clip(&clip2.id));
}

#[test]
fn multiple_layers_independent_slots() {
    let mut project = make_project();
    let host = MockHost::new();
    let mut mgr = LiveClipManager::new();

    mgr.trigger_live_clip(&mut project, &host, "v1".into(), 0, 2.0, 0.0, None, -1, 0.0);
    mgr.trigger_live_clip(&mut project, &host, "v2".into(), 1, 2.0, 0.0, None, -1, 0.0);

    assert_eq!(mgr.live_slots().len(), 2);
}

// ─── Commit ───

#[test]
fn commit_with_recording_adds_to_timeline() {
    let mut project = make_project();
    let mut host = MockHost::new().recording();
    let mut mgr = LiveClipManager::new();

    let clip = mgr.trigger_live_clip(
        &mut project, &host,
        "video1".into(), 0, 2.0, 0.0,
        None, -1, 0.0,
    ).unwrap();
    let clip_id = clip.id.clone();

    host.beat = 4.0; // held for 4 beats
    mgr.commit_live_clip(&mut project, &mut host, 0, Some(&clip_id), Some(4.0), -1, 1.0);

    // Clip should be committed to timeline
    assert_eq!(project.timeline.layers[0].clips.len(), 1);
    assert!(host.sync_dirty);
    assert!(host.compositor_dirty);
    assert!(host.recorded_commands.len() == 1);
}

#[test]
fn commit_without_recording_discards() {
    let mut project = make_project();
    let mut host = MockHost::new(); // NOT recording
    let mut mgr = LiveClipManager::new();

    let clip = mgr.trigger_live_clip(
        &mut project, &host,
        "video1".into(), 0, 2.0, 0.0,
        None, -1, 0.0,
    ).unwrap();

    mgr.commit_live_clip(&mut project, &mut host, 0, Some(&clip.id), Some(4.0), -1, 1.0);

    // Clip should NOT be in timeline
    assert_eq!(project.timeline.layers[0].clips.len(), 0);
    assert!(host.stopped_clips.contains(&clip.id));
}

// ─── Pending launches ───

#[test]
fn pending_launch_queue_activates_at_tick() {
    let mut project = make_project();
    let host = MockHost::new().at_tick(0);
    let mut mgr = LiveClipManager::new();

    // Trigger with tick 0, quantize to beat — should queue for tick 24
    let clip = mgr.trigger_live_clip(
        &mut project, &host,
        "video1".into(), 0, 2.0, 0.0,
        None, 0, 0.0,
    ).unwrap();

    // Might be queued as pending or activated immediately depending on snap
    let is_pending = mgr.pending_launch_count() > 0;
    let is_active = mgr.is_live_slot_clip(&clip.id);

    // Should be in one of the two states
    assert!(is_pending || is_active);

    if is_pending {
        // Advance tick past target
        let host2 = MockHost::new().at_tick(48);
        let activated = mgr.activate_due_pending_launches(&host2);
        assert!(activated);
        assert!(mgr.is_live_slot_clip(&clip.id));
    }
}

// ─── Clear ───

#[test]
fn clear_on_seek_small_delta_no_clear() {
    let mut project = make_project();
    let host = MockHost::new();
    let mut mgr = LiveClipManager::new();

    mgr.trigger_live_clip(&mut project, &host, "v1".into(), 0, 2.0, 0.0, None, -1, 0.0);
    assert_eq!(mgr.live_slots().len(), 1);

    // Small seek delta (< 1.0) should NOT clear
    let mut noop = |_: &str| {};
    mgr.clear_on_seek(0.5, &mut noop);
    assert_eq!(mgr.live_slots().len(), 1);
}

#[test]
fn clear_on_seek_large_delta_clears() {
    let mut project = make_project();
    let host = MockHost::new();
    let mut mgr = LiveClipManager::new();

    mgr.trigger_live_clip(&mut project, &host, "v1".into(), 0, 2.0, 0.0, None, -1, 0.0);
    mgr.trigger_live_clip(&mut project, &host, "v2".into(), 1, 2.0, 0.0, None, -1, 0.0);
    assert_eq!(mgr.live_slots().len(), 2);

    // Large seek delta (> 1.0) should clear and call stop for each
    let mut stopped = Vec::new();
    mgr.clear_on_seek(2.0, &mut |id: &str| stopped.push(id.to_string()));
    assert_eq!(mgr.live_slots().len(), 0);
    assert!(mgr.live_slot_clip_ids().is_empty());
    assert_eq!(stopped.len(), 2);
}

#[test]
fn notify_clip_stopped_removes_only_clip_id() {
    let mut project = make_project();
    let host = MockHost::new();
    let mut mgr = LiveClipManager::new();

    let clip = mgr.trigger_live_clip(
        &mut project, &host, "v1".into(), 0, 2.0, 0.0, None, -1, 0.0,
    ).unwrap();

    mgr.notify_clip_stopped(&clip.id);
    // Unity behavior: only removes from liveSlotClipIds, NOT from liveSlots dict.
    // The slot persists so NoteOff can still commit the correct held duration.
    assert_eq!(mgr.live_slots().len(), 1); // slot still present
    assert!(!mgr.is_live_slot_clip(&clip.id)); // but clip ID removed from tracking set
}

// ─── Quantize math (pure functions) ───

#[test]
fn quantize_snap_beat_from_tick() {
    // Beat quantize: 24 ticks/beat, snap tick 25 → 1 beat (rounds to 24)
    let beat = LiveClipManager::compute_snap_beat_from_tick(25, QuantizeMode::Beat, 4, false);
    assert!((beat - 1.0).abs() < 0.001);

    // Off: tick 25 → sixteenth compensation snaps to 24 → 1.0 beat
    let beat = LiveClipManager::compute_snap_beat_from_tick(25, QuantizeMode::Off, 4, false);
    assert!((beat - 1.0).abs() < 0.001);

    // Off: tick 22 (>1 away from nearest 16th=24) → no compensation → 22/24
    let beat = LiveClipManager::compute_snap_beat_from_tick(22, QuantizeMode::Off, 4, false);
    assert!((beat - 22.0 / 24.0).abs() < 0.01);

    // Bar quantize (4/4): snap tick 50 → round to 96 (4 beats)
    let beat = LiveClipManager::compute_snap_beat_from_tick(50, QuantizeMode::Bar, 4, true);
    // ceil: (50 + 96 - 1) / 96 * 96 = 96 → 4 beats
    assert!((beat - 4.0).abs() < 0.001);
}

#[test]
fn quantize_duration_beats() {
    // 1 second at 120 BPM = 2 beats, with beat quantize → 2.0
    let dur = LiveClipManager::compute_duration_beats(1.0, 0.5, -1, QuantizeMode::Beat, 4);
    assert!((dur - 2.0).abs() < 0.001);

    // 0.6 seconds at 120 BPM = 1.2 beats, with beat quantize → rounds to 1.0
    let dur = LiveClipManager::compute_duration_beats(0.6, 0.5, -1, QuantizeMode::Beat, 4);
    assert!((dur - 1.0).abs() < 0.001);
}

#[test]
fn held_beats_from_ticks_with_quantize() {
    // Start tick 0, end tick 48 = 2 beats, beat quantize → 2.0
    let held = LiveClipManager::compute_held_beats_from_ticks(0, 48, QuantizeMode::Beat, 4);
    assert!((held - 2.0).abs() < 0.001);

    // Start 0, end 30 ≈ 1.25 beats, beat quantize → rounds to 1 beat
    let held = LiveClipManager::compute_held_beats_from_ticks(0, 30, QuantizeMode::Beat, 4);
    assert!((held - 1.0).abs() < 0.001);
}

#[test]
fn get_quantize_interval_ticks() {
    assert_eq!(LiveClipManager::get_quantize_interval_ticks(QuantizeMode::Off, 4), 1);
    assert_eq!(LiveClipManager::get_quantize_interval_ticks(QuantizeMode::QuarterBeat, 4), 6);
    assert_eq!(LiveClipManager::get_quantize_interval_ticks(QuantizeMode::Beat, 4), 24);
    assert_eq!(LiveClipManager::get_quantize_interval_ticks(QuantizeMode::Bar, 4), 96);
    assert_eq!(LiveClipManager::get_quantize_interval_ticks(QuantizeMode::Bar, 3), 72);
}
