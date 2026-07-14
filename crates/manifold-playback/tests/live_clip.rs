use manifold_core::PresetTypeId;
use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::types::*;
use manifold_core::{Beats, Seconds};
use manifold_editing::command::Command;
use manifold_playback::live_clip_manager::*;
use manifold_playback::scheduler::ClipScheduler;

use std::collections::HashMap;

/// Mock host for tests.
struct MockHost {
    beat: Beats,
    time: Seconds,
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
            beat: Beats::ZERO,
            time: Seconds::ZERO,
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

}

impl LiveClipHost for MockHost {
    fn current_beat(&self) -> Beats {
        self.beat
    }
    fn current_time(&self) -> Seconds {
        self.time
    }
    fn is_recording(&self) -> bool {
        self.recording
    }
    fn is_playing(&self) -> bool {
        self.playing
    }
    fn show_debug_logs(&self) -> bool {
        false
    }
    fn get_bpm_at_beat(&self, _beat: Beats) -> f32 {
        self.bpm
    }
    fn get_tempo_source_at_beat(&self, _beat: Beats) -> manifold_core::types::TempoPointSource {
        manifold_core::types::TempoPointSource::Unknown
    }
    fn get_beat_snapped_beat(&self) -> Beats {
        self.beat
    }
    fn get_current_absolute_tick(&self) -> i32 {
        self.current_tick
    }
    fn stop_clip(&mut self, clip_id: &str) {
        self.stopped_clips.push(clip_id.to_string());
    }
    fn mark_sync_dirty(&mut self) {
        self.sync_dirty = true;
    }
    fn mark_compositor_dirty(&mut self) {
        self.compositor_dirty = true;
    }
    fn invalidate_lookahead_prewarm(&mut self) {}
    fn register_clip_lookup(&mut self, clip_id: &str, clip: &TimelineClip) {
        self.registered_clips
            .insert(clip_id.to_string(), clip.clone());
    }
    fn record_command(&mut self, cmd: Box<dyn Command>) {
        self.recorded_commands.push(cmd);
    }
    fn beat_to_timeline_time(&self, beat: Beats) -> Seconds {
        Seconds(beat.0 * 60.0 / self.bpm as f64)
    }
}

fn make_project() -> Project {
    let mut project = Project::default();
    project.settings.bpm = manifold_core::Bpm(120.0);
    project.settings.time_signature_numerator = 4;
    project.settings.quantize_mode = QuantizeMode::Beat;
    project
        .timeline
        .insert_layer(0, Layer::new("Layer 0".into(), LayerType::Video, 0));
    project
        .timeline
        .insert_layer(1, Layer::new("Layer 1".into(), LayerType::Video, 1));
    project
}

// ─── Trigger ───

#[test]
fn trigger_creates_phantom_clip() {
    let mut project = make_project();
    let host = MockHost::new();
    let mut mgr = LiveClipManager::new();

    let clip = mgr.trigger_live_clip(
        &mut project,
        &host,
        "video1".into(),
        0,
        2.0,
        0.0,
        None,
        -1,
        true,
        0.0,
        -1,
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
        &mut project,
        &host,
        PresetTypeId::PLASMA,
        0,
        4.0,
        None,
        -1,
        true,
        0.0,
        -1,
    );

    assert!(clip.is_some());
    let clip = clip.unwrap();
    assert!(clip.video_clip_id.is_empty());
    assert!(mgr.is_live_slot_clip(&clip.id));
}

#[test]
fn second_trigger_on_same_layer_replaces() {
    let mut project = make_project();
    let host = MockHost::new();
    let mut mgr = LiveClipManager::new();

    let clip1 = mgr
        .trigger_live_clip(
            &mut project,
            &host,
            "video1".into(),
            0,
            2.0,
            0.0,
            None,
            -1,
            true,
            0.0,
            -1,
        )
        .unwrap();

    let clip2 = mgr
        .trigger_live_clip(
            &mut project,
            &host,
            "video2".into(),
            0,
            3.0,
            0.0,
            None,
            -1,
            true,
            1.0,
            -1,
        )
        .unwrap();

    assert_eq!(mgr.live_slots().len(), 1);
    assert!(!mgr.is_live_slot_clip(&clip1.id));
    assert!(mgr.is_live_slot_clip(&clip2.id));
}

#[test]
fn multiple_layers_independent_slots() {
    let mut project = make_project();
    let host = MockHost::new();
    let mut mgr = LiveClipManager::new();

    mgr.trigger_live_clip(
        &mut project,
        &host,
        "v1".into(),
        0,
        2.0,
        0.0,
        None,
        -1,
        true,
        0.0,
        -1,
    );
    mgr.trigger_live_clip(
        &mut project,
        &host,
        "v2".into(),
        1,
        2.0,
        0.0,
        None,
        -1,
        true,
        0.0,
        -1,
    );

    assert_eq!(mgr.live_slots().len(), 2);
}

// ─── Commit ───

#[test]
fn commit_with_recording_adds_to_timeline() {
    let mut project = make_project();
    let mut host = MockHost::new().recording();
    let mut mgr = LiveClipManager::new();

    let clip = mgr
        .trigger_live_clip(
            &mut project,
            &host,
            "video1".into(),
            0,
            2.0,
            0.0,
            None,
            -1,
            true,
            0.0,
            -1,
        )
        .unwrap();
    let clip_id = clip.id.clone();

    host.beat = Beats(4.0); // held for 4 beats
    mgr.commit_live_clip(
        &mut project,
        &mut host,
        0,
        Some(&clip_id),
        Some(4.0),
        -1,
        1.0,
        -1,
    );

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

    let clip = mgr
        .trigger_live_clip(
            &mut project,
            &host,
            "video1".into(),
            0,
            2.0,
            0.0,
            None,
            -1,
            true,
            0.0,
            -1,
        )
        .unwrap();

    mgr.commit_live_clip(
        &mut project,
        &mut host,
        0,
        Some(&clip.id),
        Some(4.0),
        -1,
        1.0,
        -1,
    );

    // Clip should NOT be in timeline
    assert_eq!(project.timeline.layers[0].clips.len(), 0);
    assert!(host.stopped_clips.iter().any(|s| *s == clip.id));
}

// ─── Clear ───

#[test]
fn clear_on_seek_small_delta_no_clear() {
    let mut project = make_project();
    let host = MockHost::new();
    let mut mgr = LiveClipManager::new();

    mgr.trigger_live_clip(
        &mut project,
        &host,
        "v1".into(),
        0,
        2.0,
        0.0,
        None,
        -1,
        true,
        0.0,
        -1,
    );
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

    mgr.trigger_live_clip(
        &mut project,
        &host,
        "v1".into(),
        0,
        2.0,
        0.0,
        None,
        -1,
        true,
        0.0,
        -1,
    );
    mgr.trigger_live_clip(
        &mut project,
        &host,
        "v2".into(),
        1,
        2.0,
        0.0,
        None,
        -1,
        true,
        0.0,
        -1,
    );
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

    let clip = mgr
        .trigger_live_clip(
            &mut project,
            &host,
            "v1".into(),
            0,
            2.0,
            0.0,
            None,
            -1,
            true,
            0.0,
            -1,
        )
        .unwrap();

    mgr.notify_clip_stopped(&clip.id);
    // Unity behavior: only removes from liveSlotClipIds, NOT from liveSlots dict.
    // The slot persists so NoteOff can still commit the correct held duration.
    assert_eq!(mgr.live_slots().len(), 1); // slot still present
    assert!(!mgr.is_live_slot_clip(&clip.id)); // but clip ID removed from tracking set
}

// ─── Audio one-shot fire + expiry ───

#[test]
fn fire_oneshot_on_video_layer_creates_slot_and_tracks_expiry() {
    let mut project = make_project();
    project.timeline.layers[0].source_clip_ids = vec!["clipA".into()];
    let host = MockHost::new(); // beat 0, tick 0, 120 BPM, quantize Beat
    let mut mgr = LiveClipManager::new();

    let clip = mgr.fire_layer_oneshot(&mut project, &host, 0, Beats(1.0), 0.0);
    let clip = clip.expect("video layer fires");

    assert_eq!(mgr.live_slots().len(), 1);
    assert!(mgr.is_live_slot_clip(&clip.id));
    // 1 beat one-shot at 120 BPM, snapped to the beat grid → ends at beat 1.0.
    assert!(mgr.expire_due_oneshots(0.5).is_empty(), "not due yet");
    assert_eq!(mgr.live_slots().len(), 1);
}

#[test]
fn fire_oneshot_starts_at_playhead_when_abs_tick_is_frame_based() {
    // Regression: a live audio one-shot fires in real time at the playhead and
    // has NO musical event tick. With no external MIDI clock the host's
    // `get_current_absolute_tick()` returns a FRAME counter, which does not map
    // to `current_beat`. The one-shot must snap on the beat clock, so its slot
    // starts at the playhead — not at a tiny tick-derived beat the scheduler
    // would treat as long-expired and never start (black screen).
    let mut project = make_project();
    project.timeline.layers[0].source_clip_ids = vec!["clipA".into()];
    let mut host = MockHost::new();
    host.beat = Beats(100.0); // playhead deep into the arrangement
    host.current_tick = 5; // frame counter, nowhere near beat*960 — misaligned
    let mut mgr = LiveClipManager::new();

    let clip = mgr
        .fire_layer_oneshot(&mut project, &host, 0, Beats(1.0), 0.0)
        .expect("fires");

    // Slot starts at the playhead (~beat 100), NOT ~beat 0 from tick 5.
    assert!(
        (clip.start_beat.as_f32() - 100.0).abs() < 0.01,
        "start_beat should track the playhead, got {}",
        clip.start_beat.as_f32()
    );
    // Window is live at the playhead: not due at 100.5, due by 101.0.
    assert!(mgr.expire_due_oneshots(100.5).is_empty(), "not due mid-window");
    assert_eq!(mgr.expire_due_oneshots(101.0), vec![(0, clip.id.clone())]);
}

#[test]
fn expire_due_oneshot_ends_the_slot() {
    let mut project = make_project();
    project.timeline.layers[0].source_clip_ids = vec!["clipA".into()];
    let host = MockHost::new();
    let mut mgr = LiveClipManager::new();

    let clip = mgr
        .fire_layer_oneshot(&mut project, &host, 0, Beats(1.0), 0.0)
        .unwrap();

    let ended = mgr.expire_due_oneshots(1.0);
    assert_eq!(ended, vec![(0, clip.id.clone())]);
    assert_eq!(mgr.live_slots().len(), 0);
    assert!(!mgr.is_live_slot_clip(&clip.id));
}

#[test]
fn fire_oneshot_on_empty_layer_returns_none() {
    let mut project = make_project(); // layers have no source_clip_ids
    let host = MockHost::new();
    let mut mgr = LiveClipManager::new();
    assert!(
        mgr.fire_layer_oneshot(&mut project, &host, 0, Beats(1.0), 0.0)
            .is_none()
    );
    assert_eq!(mgr.live_slots().len(), 0);
}

#[test]
fn retrigger_drops_old_oneshot_expiry_so_it_cannot_end_new_slot() {
    let mut project = make_project();
    project.timeline.layers[0].source_clip_ids = vec!["clipA".into()];
    let host = MockHost::new();
    let mut mgr = LiveClipManager::new();

    let first = mgr
        .fire_layer_oneshot(&mut project, &host, 0, Beats(1.0), 0.0)
        .unwrap();
    // Retrigger the same layer before the first expires.
    let second = mgr
        .fire_layer_oneshot(&mut project, &host, 0, Beats(1.0), 0.1)
        .unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(mgr.live_slots().len(), 1);

    // The first one-shot's expiry must not end the (now-replaced) slot.
    let ended = mgr.expire_due_oneshots(1.0);
    assert_eq!(ended, vec![(0, second.id.clone())]);
}

// ─── Quantize math (pure functions) ───

#[test]
fn quantize_snap_beat_from_tick() {
    // Beat quantize: 24 ticks/beat, snap tick 25 → 1 beat (rounds to 24)
    let beat = LiveClipManager::compute_snap_beat_from_tick(25, QuantizeMode::Beat, 4, false);
    assert!((beat.0 - 1.0).abs() < 0.001);

    // Off: tick 25 → sixteenth compensation snaps to 24 → 1.0 beat
    let beat = LiveClipManager::compute_snap_beat_from_tick(25, QuantizeMode::Off, 4, false);
    assert!((beat.0 - 1.0).abs() < 0.001);

    // Off: tick 22 (>1 away from nearest 16th=24) → no compensation → 22/24
    let beat = LiveClipManager::compute_snap_beat_from_tick(22, QuantizeMode::Off, 4, false);
    assert!((beat.0 - 22.0 / 24.0).abs() < 0.01);

    // Bar quantize (4/4): snap tick 50 → round to 96 (4 beats)
    let beat = LiveClipManager::compute_snap_beat_from_tick(50, QuantizeMode::Bar, 4, true);
    // ceil: (50 + 96 - 1) / 96 * 96 = 96 → 4 beats
    assert!((beat.0 - 4.0).abs() < 0.001);
}

#[test]
fn quantize_duration_beats() {
    // 1 second at 120 BPM = 2 beats, with beat quantize → 2.0
    let dur = LiveClipManager::compute_duration_beats(1.0, 0.5, -1, QuantizeMode::Beat, 4);
    assert!((dur.0 - 2.0).abs() < 0.001);

    // 0.6 seconds at 120 BPM = 1.2 beats, with beat quantize → rounds to 1.0
    let dur = LiveClipManager::compute_duration_beats(0.6, 0.5, -1, QuantizeMode::Beat, 4);
    assert!((dur.0 - 1.0).abs() < 0.001);
}

#[test]
fn held_beats_from_ticks_with_quantize() {
    // Start tick 0, end tick 48 = 2 beats, beat quantize → 2.0
    let held = LiveClipManager::compute_held_beats_from_ticks(0, 48, QuantizeMode::Beat, 4);
    assert!((held.0 - 2.0).abs() < 0.001);

    // Start 0, end 30 ≈ 1.25 beats, beat quantize → rounds to 1 beat
    let held = LiveClipManager::compute_held_beats_from_ticks(0, 30, QuantizeMode::Beat, 4);
    assert!((held.0 - 1.0).abs() < 0.001);
}

#[test]
fn get_quantize_interval_ticks() {
    assert_eq!(
        LiveClipManager::get_quantize_interval_ticks(QuantizeMode::Off, 4),
        1
    );
    assert_eq!(
        LiveClipManager::get_quantize_interval_ticks(QuantizeMode::QuarterBeat, 4),
        6
    );
    assert_eq!(
        LiveClipManager::get_quantize_interval_ticks(QuantizeMode::Beat, 4),
        24
    );
    assert_eq!(
        LiveClipManager::get_quantize_interval_ticks(QuantizeMode::Bar, 4),
        96
    );
    assert_eq!(
        LiveClipManager::get_quantize_interval_ticks(QuantizeMode::Bar, 3),
        72
    );
}

// ─── F2: MIDI launch quantize (snap-forward on the midir fallback path) ───
//
// Root cause: midir events always carry absolute_tick = -1 (midi_input.rs),
// so beat_stamp is always None on the hardware path too. Before F2, that
// left `compute_trigger_snap_beat`'s fallback arm returning the raw,
// unsnapped current beat — QuantizeMode shaped clip *duration* but never
// launch *position*. These tests drive `LiveClipManager` (and the real
// `ClipScheduler::compute_sync` gate) exactly as the midir path does:
// beat_stamp = None, event_absolute_tick = -1.

#[test]
fn midi_launch_snaps_forward_to_bar_boundary_and_scheduler_gates_start() {
    let mut project = make_project();
    project.settings.quantize_mode = QuantizeMode::Bar; // 4/4 → 4-beat grid
    let mut host = MockHost::new();
    host.beat = Beats(1.37); // mid-bar, off grid
    let mut mgr = LiveClipManager::new();

    let clip = mgr
        .trigger_live_clip(
            &mut project,
            &host,
            "video1".into(),
            0,
            4.0,
            0.0,
            None, // no beat_stamp — the real midir fallback shape
            -1,   // no tick — midir always reports -1
            true, // MIDI note launch
            0.0,
            60,
        )
        .unwrap();

    assert!(
        (clip.start_beat.as_f32() - 4.0).abs() < 1e-4,
        "Bar quantize should snap-forward 1.37 to the next bar boundary (4.0), got {}",
        clip.start_beat.as_f32()
    );

    // Observe the REAL scheduler gate (scheduler.rs::compute_sync) — not a
    // fork of it: a live slot with a future start_beat must not appear in
    // `to_start` until the playhead reaches it, and must appear once it does.
    let mut refs = Vec::new();
    mgr.fill_live_slot_refs(&mut refs);
    assert_eq!(refs.len(), 1);

    let mut scheduler = ClipScheduler::new();
    let empty_active = ahash::AHashSet::default();
    let empty_looping = ahash::AHashSet::default();

    let before = scheduler.compute_sync(
        Seconds::ZERO,
        Beats(1.37), // playhead still before the boundary
        &[],
        &refs,
        &[],
        &empty_active,
        &empty_looping,
        Beats::ZERO,
    );
    assert!(
        before.to_start.is_empty(),
        "clip must not start before the playhead reaches its snapped start_beat"
    );
    scheduler.reclaim(before);

    let after = scheduler.compute_sync(
        Seconds::ZERO,
        Beats(4.0), // playhead has crossed the boundary
        &[],
        &refs,
        &[],
        &empty_active,
        &empty_looping,
        Beats::ZERO,
    );
    assert_eq!(
        after.to_start.len(),
        1,
        "clip must start once the playhead crosses the boundary"
    );
    assert_eq!(after.to_start[0].clip_id, clip.id);
}

#[test]
fn note_off_before_boundary_commits_cleanly_no_stuck_slot() {
    let mut project = make_project();
    project.settings.quantize_mode = QuantizeMode::Bar;
    let mut host = MockHost::new();
    host.beat = Beats(1.37);
    let mut mgr = LiveClipManager::new();

    let clip = mgr
        .trigger_live_clip(
            &mut project,
            &host,
            "video1".into(),
            0,
            4.0,
            0.0,
            None,
            -1,
            true,
            0.0,
            60,
        )
        .unwrap();
    assert!((clip.start_beat.as_f32() - 4.0).abs() < 1e-4);
    // The armed slot lands in live_slots immediately — the scheduler (above
    // test) is what gates rendering on start_beat, not slot activation.
    assert_eq!(mgr.live_slots().len(), 1);

    // Release the pad before the playhead reaches the snapped boundary.
    host.beat = Beats(2.0); // still short of 4.0
    mgr.commit_live_clip(
        &mut project,
        &mut host,
        0,
        Some(&clip.id),
        None, // real midir NoteOff also carries no beat_stamp
        -1,
        1.0, // past the 5ms NoteOff timing guard
        60,
    );

    assert_eq!(mgr.live_slots().len(), 0, "no stuck slot after early release");
    assert!(host.stopped_clips.iter().any(|s| *s == clip.id));
}

#[test]
fn midi_launch_with_quantize_off_uses_raw_beat() {
    let mut project = make_project();
    project.settings.quantize_mode = QuantizeMode::Off;
    let mut host = MockHost::new();
    host.beat = Beats(1.37);
    let mut mgr = LiveClipManager::new();

    let clip = mgr
        .trigger_live_clip(
            &mut project,
            &host,
            "video1".into(),
            0,
            2.0,
            0.0,
            None,
            -1,
            true,
            0.0,
            60,
        )
        .unwrap();

    assert!(
        (clip.start_beat.as_f32() - 1.37).abs() < 1e-4,
        "Off must preserve today's behavior — land exactly on the raw beat, got {}",
        clip.start_beat.as_f32()
    );
}

#[test]
fn midi_launch_snap_is_beat_domain_not_seconds_domain_at_90_bpm() {
    let mut project = make_project();
    project.settings.bpm = manifold_core::Bpm(90.0);
    project.settings.quantize_mode = QuantizeMode::Bar; // 4-beat grid
    let mut host = MockHost::new();
    host.bpm = 90.0;
    host.beat = Beats(1.37); // same off-grid beat as the 120 BPM case above

    let mut mgr = LiveClipManager::new();
    let clip = mgr
        .trigger_live_clip(
            &mut project,
            &host,
            "video1".into(),
            0,
            4.0,
            0.0,
            None,
            -1,
            true,
            0.0,
            60,
        )
        .unwrap();

    // The snap target is identical to the 120 BPM case (beat 4.0) because the
    // ceil happens purely in the beat domain. A seconds-domain implementation
    // (e.g. dividing by seconds-per-beat somewhere) would diverge here.
    assert!(
        (clip.start_beat.as_f32() - 4.0).abs() < 1e-4,
        "snap target must be BPM-independent (beat-domain ceil), got {}",
        clip.start_beat.as_f32()
    );
}

#[test]
fn audio_oneshot_ignores_launch_quantize_even_off_grid() {
    // Trap 1 regression: fire_layer_oneshot passes a synthetic
    // beat_stamp = Some(current_beat), which — before this fix — was itself
    // rounded to the nearest quantize grid line whenever quantize_mode was
    // on, contradicting "audio fires at the raw current beat".
    let mut project = make_project();
    project.timeline.layers[0].source_clip_ids = vec!["clipA".into()];
    project.settings.quantize_mode = QuantizeMode::Bar; // 4-beat grid
    let mut host = MockHost::new();
    host.beat = Beats(1.37); // off-grid — would snap to 4.0 if wrongly quantized
    let mut mgr = LiveClipManager::new();

    let clip = mgr
        .fire_layer_oneshot(&mut project, &host, 0, Beats(1.0), 0.0)
        .expect("fires");

    assert!(
        (clip.start_beat.as_f32() - 1.37).abs() < 1e-3,
        "audio one-shot must fire at the raw playhead beat, not the quantize grid — got {}",
        clip.start_beat.as_f32()
    );
}
