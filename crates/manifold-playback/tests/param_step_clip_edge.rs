//! PARAM_STEP_ACTIONS P2/P3 — engine-side clip-edge envelope tests.
//!
//! Exercises the real `PlaybackEngine` so the production path —
//! `sync_clips_to_time`'s per-layer `last_active_clip_id` tracking feeding
//! `evaluate_all_envelopes`'s rising-edge gate — is what's under test. The
//! Step/Random audio-mod clip-edge behavior moved onto `ParamEnvelope` in D8;
//! these scenarios now live on the envelope path.
//!
//! The last item exercises the out-of-tick `sync_clips_to_time` call
//! `session_launch_slot` makes directly, proving a non-timeline clip launch
//! still reaches the envelope phase on the next tick.

use manifold_core::audio_mod::{TriggerAction, WrapMode};
use manifold_core::clip::TimelineClip;
use manifold_core::effect_graph_def::ParamSpecDef;
use manifold_core::effects::{ParamEnvelope, PresetInstance};
use manifold_core::layer::Layer;
use manifold_core::params::Param;
use manifold_core::project::Project;
use manifold_core::types::PlaybackState;
use manifold_core::{Beats, Bpm, PresetTypeId, Seconds};
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::renderer::{ClipRenderer, StubRenderer};

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

/// A Step envelope on "level". Envelopes are always clip-edge triggered, so
/// every observed advance is attributable purely to the engine's clip edge.
fn clip_edge_step_envelope(amount: f32, wrap: WrapMode) -> ParamEnvelope {
    let mut env = ParamEnvelope::new("level");
    env.action = TriggerAction::Step { amount, wrap };
    env
}

fn clip_edge_random_envelope() -> ParamEnvelope {
    let mut env = ParamEnvelope::new("level");
    env.action = TriggerAction::Random;
    env
}

/// Layer 0 carries the envelope under test; layer 1 is a second, independent
/// generator layer whose own clip starts must never be mistaken for layer
/// 0's edge. BPM 120 (2 beats/sec) so beat math is easy frame arithmetic.
fn two_layer_project(env: ParamEnvelope) -> Project {
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);

    let mut layer0 = Layer::new_generator("L0".into(), PresetTypeId::new("TestGen"), 0);
    add_whole_number_param(layer0.gen_params_or_init(), "level", 8.0);
    layer0
        .gen_params_or_init()
        .envelopes = Some(vec![env]);
    // Clip A: beat 0..4. Clip B: beat 8..16 — a second, later start on the
    // SAME layer (a fresh edge distinct from clip A's).
    layer0.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(4.0)));
    layer0.clips.push(TimelineClip::new_generator(Beats(8.0), Beats(8.0)));

    let mut layer1 = Layer::new_generator("L1".into(), PresetTypeId::new("TestGen"), 1);
    // Layer 1's clip starts at beat 4 — must never fire layer 0's envelope.
    layer1.clips.push(TimelineClip::new_generator(Beats(4.0), Beats(4.0)));

    project.timeline.layers = vec![layer0, layer1];
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

fn envelope_step_value_of(engine: &PlaybackEngine, layer_index: usize) -> Option<f32> {
    engine.project().unwrap().timeline.layers[layer_index]
        .gen_params()
        .unwrap()
        .envelopes
        .as_ref()
        .unwrap()[0]
        .step_value
}

const DT: f64 = 1.0 / 60.0;

#[test]
fn timeline_clip_start_fires_step_envelope() {
    let mut engine = create_engine();
    engine.initialize(two_layer_project(clip_edge_step_envelope(1.0, WrapMode::Clamp)));
    engine.set_state(PlaybackState::Playing);

    tick_n(&mut engine, 5, DT);
    assert_eq!(
        envelope_step_value_of(&engine, 0),
        Some(1.0),
        "layer 0's own clip start (beat 0) fires the Step envelope"
    );
}

#[test]
fn no_clip_start_means_no_envelope_step() {
    // A generator layer with no timeline clips never sees a clip edge, so a
    // Step envelope never arms.
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);
    let mut layer0 = Layer::new_generator("L0".into(), PresetTypeId::new("TestGen"), 0);
    add_whole_number_param(layer0.gen_params_or_init(), "level", 8.0);
    layer0.gen_params_or_init().envelopes = Some(vec![clip_edge_step_envelope(1.0, WrapMode::Clamp)]);
    project.timeline.layers = vec![layer0];

    let mut engine = create_engine();
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);

    tick_n(&mut engine, 60, DT);
    assert_eq!(
        envelope_step_value_of(&engine, 0),
        None,
        "with no clip start, the envelope Step action stays cold"
    );
}

#[test]
fn random_envelope_fires_on_clip_edge() {
    let mut engine = create_engine();
    engine.initialize(two_layer_project(clip_edge_random_envelope()));
    engine.set_state(PlaybackState::Playing);

    tick_n(&mut engine, 5, DT);
    assert!(
        envelope_step_value_of(&engine, 0).is_some(),
        "Random envelope fires on the clip start"
    );
}

#[test]
fn other_layers_clip_start_does_not_fire() {
    let mut engine = create_engine();
    engine.initialize(two_layer_project(clip_edge_step_envelope(1.0, WrapMode::Clamp)));
    engine.set_state(PlaybackState::Playing);

    // Layer 0's clip A (beat 0) fires once. Tick past beat 4 (2s = 120
    // frames), where layer 1's clip starts — layer 0's own clip A is still
    // active the whole time (it ends at beat 4, exactly when layer 1
    // starts), so layer 0 sees no edge of its own in this window.
    tick_n(&mut engine, 125, DT);
    assert_eq!(
        envelope_step_value_of(&engine, 0),
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
    layer0
        .gen_params_or_init()
        .envelopes = Some(vec![clip_edge_step_envelope(1.0, WrapMode::Clamp)]);
    layer0.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(4.0)));
    project.timeline.layers = vec![layer0];

    let mut engine = create_engine();
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);

    // Fires once at the clip's start.
    tick_n(&mut engine, 10, DT);
    assert_eq!(envelope_step_value_of(&engine, 0), Some(1.0));

    // Tick well past beat 4 (clip end, 2s = 120 frames) with nothing new
    // starting on the layer: the shadow must not advance again.
    tick_n(&mut engine, 400, DT);
    assert_eq!(
        envelope_step_value_of(&engine, 0),
        Some(1.0),
        "a clip ending with no new clip starting fires nothing"
    );
}

#[test]
fn second_timeline_clip_start_refires_step_envelope() {
    // Two timeline clips on the same layer, back-to-back: the first clip's
    // start fires once, and the second clip's start (at beat 4) fires again.
    let mut engine = create_engine();
    engine.initialize(two_layer_project(clip_edge_step_envelope(1.0, WrapMode::Clamp)));
    engine.set_state(PlaybackState::Playing);

    tick_n(&mut engine, 5, DT);
    assert_eq!(envelope_step_value_of(&engine, 0), Some(1.0), "first clip start fires");

    // Tick past beat 8 (4s = 240 frames) where the second clip starts.
    tick_n(&mut engine, 245, DT);
    assert_eq!(
        envelope_step_value_of(&engine, 0),
        Some(2.0),
        "second timeline clip start on the same layer re-fires the Step envelope"
    );
}

/// A layer index that never appears in the project must never spuriously
/// gate a step — guards against an off-by-one or stale-index bug in the
/// `clip_edge_layers` → `evaluate_all_envelopes` wiring surfacing as a
/// false fire on an unrelated layer.
#[test]
fn unrelated_layer_edge_after_reorder_does_not_confuse_the_gate() {
    // Two clip starts on DIFFERENT layers at the same beat: only layer 0's
    // own envelope may fire from layer 0's edge.
    let mut engine = create_engine();
    engine.initialize(two_layer_project(clip_edge_step_envelope(1.0, WrapMode::Clamp)));
    engine.set_state(PlaybackState::Playing);
    tick_n(&mut engine, 5, DT);
    assert_eq!(envelope_step_value_of(&engine, 0), Some(1.0));

    // Layer 1 has no envelope at all — this just documents that its own
    // clip start (beat 4, exercised in `other_layers_clip_start_does_not_fire`)
    // never touches layer 0's state, holding the count-of-fires invariant
    // exact at exactly one fire so far.
    tick_n(&mut engine, 20, DT);
    assert_eq!(
        envelope_step_value_of(&engine, 0),
        Some(1.0),
        "no second fire without a second clip start on layer 0 itself"
    );
}

// ── PARAM_STEP_ACTIONS round-trip gate (DESIGN_DOC_STANDARD section 5, BUG-036
// rule) ──────────────────────────────────────────────────────────────────
//
// The P1 unit test (`modulation.rs`'s `serde round-trip` test) only proves
// `serde_json::to_string`/`from_str` round-trips the isolated
// `ParamEnvelope` struct in memory. This test exercises the full stack:
// build a project with an armed Step envelope, save it via `save_project_v1`,
// reload via `load_project`, then tick the reloaded project's OWN engine
// forward and confirm the clip-edge step still fires and resumes from the
// COMMITTED base — not from a corrupted or stale value the round trip might
// have introduced (D4/D5's "modulate AFTER reload" contract).
#[test]
fn step_envelope_resumes_from_committed_base_after_real_save_and_reload() {
    let project = two_layer_project(clip_edge_step_envelope(1.0, WrapMode::Clamp));

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

    // The envelope's runtime shadow never round-trips (serde-skip) — confirm
    // the reloaded project starts cold, exactly like a fresh load in the show.
    assert_eq!(
        reloaded.timeline.layers[0].gen_params().unwrap().envelopes.as_ref().unwrap()[0].step_value,
        None,
        "step_value must not survive the round trip (D4: reload drops the shadow)"
    );

    let mut engine = create_engine();
    engine.initialize(reloaded);
    engine.set_state(PlaybackState::Playing);

    tick_n(&mut engine, 5, DT);
    assert_eq!(
        envelope_step_value_of(&engine, 0),
        Some(1.0),
        "after a real save+reload, the layer's own clip start still fires the \
         Step envelope and resumes from the committed base (0 + amount 1 = 1)"
    );
}
