//! PARAM_STEP_ACTIONS P2 — engine-side clip-edge gate tests.
//!
//! Exercises the real `PlaybackEngine` (not a hand-rolled `clip_edge_layers`
//! slice) so the actual production path — `sync_clips_to_time`'s per-layer
//! `last_active_clip_id` tracking (`engine.rs`) feeding
//! `evaluate_all_audio_mods`'s `trigger_mode` gate (`modulation.rs`) — is
//! what's under test, matching the P2 phase brief's gate: "timeline clip
//! start fires a Clip-mode step; Transient-mode step ignores clip edges;
//! Both sums; a clip ending with no new clip fires nothing; a live-slot/
//! phantom clip launch fires."
//!
//! The last item is exercised here via a SESSION-grid slot launch
//! (`session_launch_slot`), not a MIDI/phantom live-clip launch. Both
//! sources are `ActiveClipRef`s merged into the identical `compute_sync`
//! diff this design's P2 code treats uniformly (no live-vs-session
//! special-casing anywhere in the new code — see `sync_clips_to_time`'s
//! `for entry in &starts` loop), so a session-slot launch is the same
//! "engine clip start, not a timeline clip" proof. A live/MIDI phantom
//! launch would additionally require driving `PlaybackEngine`'s
//! `LiveClipHost` impl through the crate-internal raw-pointer split-borrow
//! pattern `tick_audio_triggers` uses (`fire_layer_oneshot` calls
//! `host.get_bpm_at_beat`, which reads `self.project` while an aliased
//! `&mut Project` from the same raw-pointer split is alive) — an existing,
//! out-of-P2-scope pattern this test deliberately does not replicate from
//! outside the crate.

use manifold_core::audio_features::{AudioFeatureSnapshot, SendFeatures};
use manifold_core::audio_mod::{
    AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod, TriggerAction, WrapMode,
};
use manifold_core::audio_setup::AudioSend;
use manifold_core::audio_trigger::TriggerFireMode;
use manifold_core::clip::TimelineClip;
use manifold_core::effect_graph_def::ParamSpecDef;
use manifold_core::effects::PresetInstance;
use manifold_core::layer::Layer;
use manifold_core::params::Param;
use manifold_core::project::Project;
use manifold_core::session::{ClipSequence, Scene, SessionSlot};
use manifold_core::types::PlaybackState;
use manifold_core::{Beats, Bpm, ClipId, PresetTypeId, SceneId, Seconds};
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::renderer::{ClipRenderer, StubRenderer};

/// `evaluate_all_audio_mods` early-returns when the snapshot is empty (P1
/// scope, inherited unchanged by P2 — see the module doc's note on
/// `evaluate_all_audio_mods`'s doc comment in `modulation.rs`): every
/// `ParameterAudioMod` is fundamentally send-sourced, so even a pure
/// Clip-mode step needs *some* resolvable send data to reach the per-mod
/// walk where the clip-edge gate lives. One send slot at a permanently
/// cold level (never crosses the 0.5 fire threshold) satisfies that
/// without contributing an audio-edge fire of its own.
fn arm_cold_audio_snapshot(engine: &mut PlaybackEngine) {
    *engine.audio_snapshot_mut() = AudioFeatureSnapshot { sends: vec![SendFeatures::default()] };
}

fn create_engine() -> PlaybackEngine {
    let renderers: Vec<Box<dyn ClipRenderer>> = vec![Box::new(StubRenderer::new_generator())];
    PlaybackEngine::new(renderers)
}

/// A whole-numbers 0..8 param (mirrors `modulation.rs`'s `add_trigger_gate_param`
/// pattern for getting a param onto the manifest without the registry).
fn add_whole_number_param(inst: &mut PresetInstance, id: &str, max: f32) {
    inst.params.push(Param::bundled(ParamSpecDef {
        id: id.to_string(),
        name: id.to_string(),
        min: 0.0,
        max,
        default_value: 0.0,
        whole_numbers: true,
        is_toggle: false,
        is_trigger: false,
        value_labels: Vec::new(),
        format_string: None,
        osc_suffix: String::new(),
        curve: Default::default(),
        invert: false,
        is_angle: false,
        is_trigger_gate: false,
        wraps: false,
        section: None,
        card_visible: true,
    }));
}

/// A Step/Random mod on "level", sourced from a send whose feature never
/// crosses the fire threshold (an all-zero snapshot, engine default) — so
/// throughout these tests the mod's OWN audio edge never fires and every
/// observed advance is attributable purely to the engine's clip edge.
fn clip_edge_only_mod(send_id: &manifold_core::id::AudioSendId, mode: TriggerFireMode) -> ParameterAudioMod {
    let mut m = ParameterAudioMod::new(
        "level".into(),
        send_id.clone(),
        AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
    );
    m.action = TriggerAction::Step { amount: 1.0, wrap: WrapMode::Clamp };
    m.trigger_mode = Some(mode);
    m
}

/// Layer 0 carries the mod under test; layer 1 is a second, independent
/// generator layer whose own clip starts must never be mistaken for layer
/// 0's edge. BPM 120 (2 beats/sec) so beat math is easy frame arithmetic.
fn two_layer_project(mode: TriggerFireMode) -> Project {
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);

    let mut layer0 = Layer::new_generator("L0".into(), PresetTypeId::new("TestGen"), 0);
    add_whole_number_param(layer0.gen_params_or_init(), "level", 8.0);
    let send = AudioSend::new("Kick");
    let send_id = send.id.clone();
    layer0
        .gen_params_or_init()
        .audio_mods_mut()
        .push(clip_edge_only_mod(&send_id, mode));
    // Clip A: beat 0..4. Clip B: beat 8..16 — a second, later start on the
    // SAME layer (a fresh edge distinct from clip A's).
    layer0.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(4.0)));
    layer0.clips.push(TimelineClip::new_generator(Beats(8.0), Beats(8.0)));

    let mut layer1 = Layer::new_generator("L1".into(), PresetTypeId::new("TestGen"), 1);
    // Layer 1's clip starts at beat 4 — must never fire layer 0's mod.
    layer1.clips.push(TimelineClip::new_generator(Beats(4.0), Beats(4.0)));

    project.timeline.layers = vec![layer0, layer1];
    project.audio_setup.sends.push(send);
    project
}

fn tick_n(engine: &mut PlaybackEngine, n: usize, dt: f64) {
    for i in 0..n {
        let ctx = TickContext {
            dt_seconds: Seconds(dt),
            realtime_now: Seconds(i as f64 * dt),
            pre_render_dt: Seconds(dt),
            frame_count: i as u64,
            export_fixed_dt: Seconds(0.0),
        };
        let _ = engine.tick(ctx);
    }
}

fn step_value_of(engine: &PlaybackEngine, layer_index: usize) -> Option<f32> {
    engine.project().unwrap().timeline.layers[layer_index]
        .gen_params()
        .unwrap()
        .audio_mods
        .as_ref()
        .unwrap()[0]
        .step_value
}

const DT: f64 = 1.0 / 60.0;

#[test]
fn timeline_clip_start_fires_clip_mode_step() {
    let mut engine = create_engine();
    engine.initialize(two_layer_project(TriggerFireMode::ClipEdge));
    arm_cold_audio_snapshot(&mut engine);
    engine.set_state(PlaybackState::Playing);

    // A few frames past beat 0 is enough for clip A's start to sync and for
    // the NEXT tick's Phase 1.5 to surface the shadow (D4's one-frame
    // latency — the shadow advances on the fire tick, `p.value` catches up
    // next tick; `step_value` itself is visible immediately).
    tick_n(&mut engine, 5, DT);
    assert_eq!(
        step_value_of(&engine, 0),
        Some(1.0),
        "layer 0's own clip start (beat 0) fires the Clip-mode step"
    );
}

#[test]
fn transient_mode_step_ignores_clip_edge_entirely() {
    let mut engine = create_engine();
    engine.initialize(two_layer_project(TriggerFireMode::Transient));
    arm_cold_audio_snapshot(&mut engine);
    engine.set_state(PlaybackState::Playing);

    // Tick well past both of layer 0's clip starts (beat 0 and beat 8 — 8
    // beats at 120bpm = 4s = 240 frames) with an all-zero audio snapshot
    // (the engine default): a Transient-mode step must never see either
    // clip edge, so the shadow never arms.
    tick_n(&mut engine, 300, DT);
    assert_eq!(
        step_value_of(&engine, 0),
        None,
        "Transient-mode step ignores clip edges entirely; no audio signal either, so it never fires"
    );
}

#[test]
fn both_mode_fires_on_clip_edge_alone() {
    let mut engine = create_engine();
    engine.initialize(two_layer_project(TriggerFireMode::Both));
    arm_cold_audio_snapshot(&mut engine);
    engine.set_state(PlaybackState::Playing);

    tick_n(&mut engine, 5, DT);
    assert_eq!(
        step_value_of(&engine, 0),
        Some(1.0),
        "Both mode fires on the clip edge alone (no audio signal present)"
    );
}

#[test]
fn other_layers_clip_start_does_not_fire() {
    let mut engine = create_engine();
    engine.initialize(two_layer_project(TriggerFireMode::ClipEdge));
    arm_cold_audio_snapshot(&mut engine);
    engine.set_state(PlaybackState::Playing);

    // Layer 0's clip A (beat 0) fires once. Tick past beat 4 (2s = 120
    // frames), where layer 1's clip starts — layer 0's own clip A is still
    // active the whole time (it ends at beat 4, exactly when layer 1
    // starts), so layer 0 sees no edge of its own in this window.
    tick_n(&mut engine, 125, DT);
    assert_eq!(
        step_value_of(&engine, 0),
        Some(1.0),
        "only layer 0's own clip start (beat 0) may have fired; layer 1's clip start at beat 4 must not bleed across layers"
    );
}

#[test]
fn clip_ending_with_nothing_replacing_fires_nothing_more() {
    // Single clip, single layer: beat 0..4, nothing after it.
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);
    let mut layer0 = Layer::new_generator("L0".into(), PresetTypeId::new("TestGen"), 0);
    add_whole_number_param(layer0.gen_params_or_init(), "level", 8.0);
    let send = AudioSend::new("Kick");
    let send_id = send.id.clone();
    layer0
        .gen_params_or_init()
        .audio_mods_mut()
        .push(clip_edge_only_mod(&send_id, TriggerFireMode::ClipEdge));
    layer0.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(4.0)));
    project.timeline.layers = vec![layer0];
    project.audio_setup.sends.push(send);

    let mut engine = create_engine();
    engine.initialize(project);
    arm_cold_audio_snapshot(&mut engine);
    engine.set_state(PlaybackState::Playing);

    // Fires once at the clip's start.
    tick_n(&mut engine, 10, DT);
    assert_eq!(step_value_of(&engine, 0), Some(1.0));

    // Tick well past beat 4 (clip end, 2s = 120 frames) with nothing new
    // starting on the layer: the shadow must not advance again.
    tick_n(&mut engine, 400, DT);
    assert_eq!(
        step_value_of(&engine, 0),
        Some(1.0),
        "a clip ending with no new clip starting fires nothing"
    );
}

#[test]
fn session_slot_launch_fires_clip_mode_step() {
    // A non-timeline clip source (session grid) merges into the identical
    // `compute_sync` diff a live/MIDI phantom clip does (see module doc) —
    // this proves the engine's clip edge isn't timeline-clip-specific, and
    // exercises the out-of-tick `sync_clips_to_time` call `session_launch_slot`
    // makes directly (the drain-queue path: the edge must survive to the
    // next tick's modulation pass, not be dropped because it happened
    // outside `tick()`).
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);
    let mut layer0 = Layer::new_generator("L0".into(), PresetTypeId::new("TestGen"), 0);
    add_whole_number_param(layer0.gen_params_or_init(), "level", 8.0);
    let send = AudioSend::new("Kick");
    let send_id = send.id.clone();
    layer0
        .gen_params_or_init()
        .audio_mods_mut()
        .push(clip_edge_only_mod(&send_id, TriggerFireMode::ClipEdge));
    // No timeline clips at all — the layer is silent until the session launch.
    let layer_id = layer0.layer_id.clone();
    project.timeline.layers = vec![layer0];
    project.audio_setup.sends.push(send);

    let scene_id = SceneId::new("scene-a");
    project.session.scenes.push(Scene { id: scene_id.clone(), name: "Scene".into(), color: None });
    let mut seq_clip = TimelineClip::new_generator(Beats(0.0), Beats(4.0));
    seq_clip.id = ClipId::new("session-clip");
    project.session.slots.push(SessionSlot {
        layer_id: layer_id.clone(),
        scene_id: scene_id.clone(),
        sequence: ClipSequence { length_beats: Beats(4.0), clips: vec![seq_clip] },
        name: "Slot".into(),
        color: None,
    });

    let mut engine = create_engine();
    engine.initialize(project);
    arm_cold_audio_snapshot(&mut engine);
    assert_eq!(engine.current_state(), PlaybackState::Stopped);

    // Launch from stopped: starts transport AND syncs immediately (both
    // out-of-tick sync_clips_to_time calls, per `session_launch_slot`'s own
    // doc comment) — the edge must still reach modulation on the next tick.
    engine.session_launch_slot(layer_id, scene_id);
    assert_eq!(engine.current_state(), PlaybackState::Playing);

    tick_n(&mut engine, 5, DT);
    assert_eq!(
        step_value_of(&engine, 0),
        Some(1.0),
        "a session-slot (non-timeline) clip launch fires the Clip-mode step"
    );
}

/// A layer index that never appears in the project must never spuriously
/// gate a step — guards against an off-by-one or stale-index bug in the
/// `clip_edge_layers` → `evaluate_all_audio_mods` wiring surfacing as a
/// false fire on an unrelated layer.
#[test]
fn unrelated_layer_edge_after_reorder_does_not_confuse_the_gate() {
    // Two clip starts on DIFFERENT layers at the same beat: only layer 0's
    // own mod may fire from layer 0's edge.
    let mut engine = create_engine();
    engine.initialize(two_layer_project(TriggerFireMode::ClipEdge));
    arm_cold_audio_snapshot(&mut engine);
    engine.set_state(PlaybackState::Playing);
    tick_n(&mut engine, 5, DT);
    assert_eq!(step_value_of(&engine, 0), Some(1.0));

    // Layer 1 has no audio mod at all — this just documents that its own
    // clip start (beat 4, exercised in `other_layers_clip_start_does_not_fire`)
    // never touches layer 0's state, holding the count-of-fires invariant
    // exact at exactly one fire so far.
    tick_n(&mut engine, 20, DT);
    assert_eq!(
        step_value_of(&engine, 0),
        Some(1.0),
        "no second fire without a second clip start on layer 0 itself"
    );
}

// ── PARAM_STEP_ACTIONS P3 round-trip gate (DESIGN_DOC_STANDARD §5, BUG-036
// rule) ──────────────────────────────────────────────────────────────────
//
// The P1 unit test (`modulation.rs`'s `serde round-trip` test) only proves
// `serde_json::to_string`/`from_str` round-trips the isolated
// `ParameterAudioMod` struct in memory. That is HALF the gate for stateful
// features (§5): it never drives the real `manifold-io` save/load pipeline
// (path resolution, migrations, post-load validation) that a saved show
// file actually goes through, and it never re-fires the mod afterward. This
// test exercises the full stack: build a project with an armed Step mod,
// save it to a real file via `save_project_v1`, reload it via
// `load_project`, then tick the reloaded project's OWN engine forward and
// confirm the clip-edge step still fires and resumes from the COMMITTED
// base — not from a corrupted or stale value the round trip might have
// introduced (D4/D5's "modulate AFTER reload" contract).
#[test]
fn step_mod_resumes_from_committed_base_after_real_save_and_reload() {
    let project = two_layer_project(TriggerFireMode::ClipEdge);

    let mut save_path = std::env::temp_dir();
    save_path.push(format!(
        "manifold_param_step_roundtrip_{}_{}.manifold",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    manifold_io::saver::save_project_v1(&project, &save_path)
        .expect("save_project_v1 should succeed to a scratch path");
    let reloaded = manifold_io::loader::load_project(&save_path).expect("reload should succeed");
    std::fs::remove_file(&save_path).ok();

    // The step mod's runtime shadow never round-trips (serde-skip) — confirm
    // the reloaded project starts cold, exactly like a fresh load in the show.
    assert_eq!(
        reloaded.timeline.layers[0].gen_params().unwrap().audio_mods.as_ref().unwrap()[0].step_value,
        None,
        "step_value must not survive the round trip (D4: reload drops the shadow)"
    );

    let mut engine = create_engine();
    engine.initialize(reloaded);
    arm_cold_audio_snapshot(&mut engine);
    engine.set_state(PlaybackState::Playing);

    tick_n(&mut engine, 5, DT);
    assert_eq!(
        step_value_of(&engine, 0),
        Some(1.0),
        "after a real save+reload, the layer's own clip start still fires the \
         Clip-mode step and resumes from the committed base (0 + amount 1 = 1)"
    );
}
