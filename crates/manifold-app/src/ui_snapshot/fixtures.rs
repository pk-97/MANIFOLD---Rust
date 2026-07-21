//! Small, deterministic `Project` fixtures driven through the REAL core→UI
//! translation path (`state_sync::sync_project_data`/`push_state`), so a
//! snapshot is what the app actually draws — not hand-built panel structs.
//! The `timeline` scene reproduces the redesign mockup: text / video /
//! generator / group+children / audio. See `docs/HEADLESS_UI_HARNESS.md` §2.

use manifold_core::clip::TimelineClip;
use manifold_core::effects::PresetInstance;
use manifold_core::layer::Layer;
use manifold_core::preset_def::PresetKind;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::{Beats, GraphTarget, LayerId, PresetTypeId, Seconds};
use manifold_ui::UIState;

use crate::content_state::ContentState;

/// Everything a scene needs to drive the real translation path.
pub struct SceneData {
    pub project: Project,
    pub content: ContentState,
    pub active: Option<usize>,
    pub selection: UIState,
}

/// Resolve a scene name to its fixture. Returns `None` for unknown names.
/// `project:<path>` is not a fixed name but a syntax — see [`project_scene`].
pub fn build(scene: &str) -> Option<SceneData> {
    if let Some(path) = scene.strip_prefix("project:") {
        return project_scene(path);
    }
    match scene {
        "timeline" => Some(timeline_scene()),
        "states" => Some(states_scene()),
        "inspector" => Some(inspector_scene()),
        "bug060" => Some(bug060_scene()),
        "bug060heavy" => Some(bug060heavy_scene()),
        "bug047" => Some(bug047_scene()),
        "paramsteps" => Some(param_steps_scene()),
        "scrollshrink" => Some(scroll_shrink_scene()),
        "hairlineclips" => Some(hairline_clips_scene()),
        "automation" => Some(automation_scene()),
        "automationplaceholder" => Some(automation_placeholder_scene()),
        "selectionclips" => Some(selection_clips_scene()),
        "audiosends" => Some(audio_sends_scene()),
        "gltfscene" => Some(gltf_scene()),
        "gltfanimscene" => Some(gltf_anim_scene()),
        "heldoutmerge" => Some(heldout_merge_scene()),
        "empty" => Some(empty_scene()),
        "envmod" => Some(envelope_modulation_scene()),
        _ => None,
    }
}

/// Zero-layer scene: what a user sees on File → New before doing anything.
/// Exists so the UX audit can look at the empty state itself (what, if any,
/// affordance points at creating the first layer).
fn empty_scene() -> SceneData {
    let project = Project::default();
    SceneData { project, content: ContentState::default(), active: None, selection: UIState::default() }
}

/// Real-project scene (`project:<abs-or-relative-path>`): loads an actual
/// `.manifold` file through the SAME path the live app uses
/// (`ProjectIOService::open_project_from_path`) — `load_project_with` plus the
/// embedded-preset install hook, so project-local forked presets resolve
/// their params exactly as the app would show them (BUG-036). Missing
/// media does not fail the load: `run_post_load_validation` only logs
/// (`manifold-io/src/loader.rs`'s step 6), so real projects with unreachable
/// source files still render — no special-casing needed here. Default
/// selection/active, same as a freshly opened project before the user clicks
/// anything.
fn project_scene(path: &str) -> Option<SceneData> {
    let load_result = manifold_io::loader::load_project_with(
        std::path::Path::new(path),
        crate::project_io::install_embedded_presets,
    );
    let project = match load_result {
        Ok(p) => p,
        Err(e) => {
            eprintln!("ui-snap: failed to load project '{path}': {e}");
            return None;
        }
    };
    Some(SceneData { project, content: ContentState::default(), active: None, selection: UIState::default() })
}

/// `gltfscene`: a fresh glTF import (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md
/// P3's demo vehicle), driven through the REAL production path — the same
/// `assemble_import_graph` + `ImportModelLayerCommand` sequence
/// `Application::import_model_file` runs on a real drop — so the resulting
/// card carries genuine importer-seeded `section`s (D9/D5), not a hand-built
/// stand-in. Selects the imported layer so its card renders immediately.
pub(super) fn gltf_scene() -> SceneData {
    use manifold_core::project::{EmbeddedOrigin, EmbeddedPreset};
    use manifold_editing::command::Command;
    use manifold_editing::commands::layer::ImportModelLayerCommand;

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb");
    let (def, report) = manifold_renderer::node_graph::gltf_import::assemble_import_graph(&path)
        .unwrap_or_else(|e| panic!("ui-snap gltfscene: assemble_import_graph({}) failed: {e}", path.display()));
    eprintln!("ui-snap gltfscene: import report: {report:?}");

    let display_name = def
        .preset_metadata
        .as_ref()
        .map(|m| m.display_name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Azalea".to_string());
    let embedded = EmbeddedPreset {
        kind: manifold_core::preset_def::PresetKind::Generator,
        def,
        origin: EmbeddedOrigin::Saved,
    };

    // Install the overlay BEFORE the layer is created — `Layer::new_generator`
    // → `init_defaults` reads the process-global preset-definition registry to
    // seed the curated card values, and the tracking layer's id only resolves
    // once the overlay knows it (mirrors `Application::import_model_file`'s
    // ordering, `app_lifecycle.rs`'s D9 deliverable-3 comment).
    crate::project_io::install_embedded_presets(std::slice::from_ref(&embedded));

    let mut project = Project::default();
    let mut cmd = ImportModelLayerCommand::new(display_name, embedded, 0, None);
    cmd.execute(&mut project);
    // Same post-install step the real loader/import path runs after
    // registering an embedded preset: resolve the tracking layer's manifest
    // against it (PARAM_STORAGE_BOUNDARIES_DESIGN.md D1).
    project.reconcile_param_manifests();

    let lid = cmd.inserted_layer_id().expect("layer inserted");
    let mut selection = UIState::default();
    selection.select_layer(lid);

    SceneData { project, content: ContentState::default(), active: Some(0), selection }
}

/// `heldoutmerge`: SCENE_OBJECT_AND_PANEL_V2_DESIGN.md P5's held-out
/// gate — the same real, gitignored assets and real production merge path
/// `scene_setup_p4_heldout_merge.rs`'s
/// `merges_skull_into_warehouse_held_out_real_assets` proves (skull merged
/// into the warehouse scene), wrapped as a live layer so the P5 outliner +
/// properties can be screenshotted against it — both objects, both lights,
/// and the eye/Duplicate/Remove affordances all have to read as real,
/// clickable chrome on a scene neither this wave's synthetic-def unit tests
/// nor the `gltfscene` two-object fixture exercise.
pub(super) fn heldout_merge_scene() -> SceneData {
    use manifold_core::effect_graph_def::SerializedParamValue;
    use manifold_core::project::{EmbeddedOrigin, EmbeddedPreset};
    use manifold_editing::command::Command;
    use manifold_editing::commands::layer::ImportModelLayerCommand;
    use manifold_renderer::node_graph::gltf_import::{assemble_import_graph, assemble_merge_plan};

    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf");
    let warehouse = fixtures.join("abandoned_warehouse_-_interior_scene.glb");
    let skull = fixtures.join("skull_salazar_downloadable.glb");

    let (mut merged_def, report) = assemble_import_graph(&warehouse)
        .unwrap_or_else(|e| panic!("ui-snap heldoutmerge: warehouse import failed: {e}"));
    eprintln!("ui-snap heldoutmerge: warehouse import report: {report:?}");

    let plan = assemble_merge_plan(&merged_def, &skull)
        .unwrap_or_else(|e| panic!("ui-snap heldoutmerge: merge plan failed: {e}"));
    eprintln!(
        "ui-snap heldoutmerge: merge plan: {} new nodes, {} new wires, new_objects_count={}, report_lines={:?}",
        plan.new_nodes.len(),
        plan.new_wires.len(),
        plan.new_objects_count,
        plan.report_lines
    );
    merged_def.nodes.extend(plan.new_nodes.clone());
    merged_def.wires.extend(plan.new_wires.clone());
    if let Some(render_scene_node) = merged_def.nodes.iter_mut().find(|n| n.id == plan.render_scene_node_id) {
        render_scene_node
            .params
            .insert("objects".to_string(), SerializedParamValue::Int { value: plan.new_objects_count as i32 });
    }

    let embedded = EmbeddedPreset {
        kind: manifold_core::preset_def::PresetKind::Generator,
        def: merged_def,
        origin: EmbeddedOrigin::Saved,
    };
    crate::project_io::install_embedded_presets(std::slice::from_ref(&embedded));

    let mut project = Project::default();
    let mut cmd = ImportModelLayerCommand::new("Warehouse + Skull".to_string(), embedded, 0, None);
    cmd.execute(&mut project);
    project.reconcile_param_manifests();

    let lid = cmd.inserted_layer_id().expect("layer inserted");
    let mut selection = UIState::default();
    selection.select_layer(lid);

    SceneData { project, content: ContentState::default(), active: Some(0), selection }
}

/// `gltfanimscene`: GLTF_ANIMATION_DESIGN.md A4's L3 fixture — imports
/// `BoxAnimated.glb` (A1's own gate fixture, already proven to animate
/// through this exact import path) through the SAME real production path
/// [`gltf_scene`] uses, so the resulting card carries the A4 Rate/Clip/Loop
/// Mode/Retrigger knobs `gltf_import.rs::animation_card_controls` actually
/// stamps — not a hand-built stand-in.
pub(super) fn gltf_anim_scene() -> SceneData {
    use manifold_core::project::{EmbeddedOrigin, EmbeddedPreset};
    use manifold_editing::command::Command;
    use manifold_editing::commands::layer::ImportModelLayerCommand;

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/khronos/BoxAnimated.glb");
    let (def, report) = manifold_renderer::node_graph::gltf_import::assemble_import_graph(&path)
        .unwrap_or_else(|e| panic!("ui-snap gltfanimscene: assemble_import_graph({}) failed: {e}", path.display()));
    eprintln!("ui-snap gltfanimscene: import report: {report:?}");

    let display_name = def
        .preset_metadata
        .as_ref()
        .map(|m| m.display_name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "BoxAnimated".to_string());
    let embedded = EmbeddedPreset {
        kind: manifold_core::preset_def::PresetKind::Generator,
        def,
        origin: EmbeddedOrigin::Saved,
    };

    crate::project_io::install_embedded_presets(std::slice::from_ref(&embedded));

    let mut project = Project::default();
    let mut cmd = ImportModelLayerCommand::new(display_name, embedded, 0, None);
    cmd.execute(&mut project);
    project.reconcile_param_manifests();

    let lid = cmd.inserted_layer_id().expect("layer inserted");
    let mut selection = UIState::default();
    selection.select_layer(lid);

    SceneData { project, content: ContentState::default(), active: Some(0), selection }
}

/// A fully-initialized effect-kind `PresetInstance` of `type_id`, params seeded
/// from the registry defaults (so the inspector card shows real rows, not an
/// empty effect). The renderer crate is linked into the snapshot binary, so its
/// `inventory::submit!` preset sources have populated the registry by here.
fn effect(type_id: &str) -> PresetInstance {
    let mut e = PresetInstance::new(PresetTypeId::from_string(type_id.to_string()));
    e.init_defaults();
    e
}

/// Arm a sine LFO (1/4 note) on the effect's first parameter, so the inspector
/// renders the source-tinted modulation drawer (LFO = teal) under that row —
/// the most involved inspector chrome. No-op if the effect has no params.
fn arm_lfo(fx: &mut PresetInstance) {
    use manifold_core::effects::ParameterDriver;
    use manifold_core::types::{BeatDivision, DriverWaveform};
    let param_id = manifold_core::preset_definition_registry::try_get(fx.effect_type())
        .and_then(|def| def.param_defs.first().map(|pd| pd.spec.id.clone()));
    if let Some(param_id) = param_id {
        fx.drivers = Some(vec![ParameterDriver::new(
            param_id,
            BeatDivision::Quarter,
            DriverWaveform::Sine,
        )]);
    }
}

/// Arm a decay envelope on the effect's first parameter, default target
/// (`target_normalized = 1.0`, i.e. the param's max) and default decay
/// (`DEFAULT_ENVELOPE_DECAY_BEATS`, 1 beat) — mirrors `arm_lfo` above. No-op
/// if the effect has no params. BUG-234: the harness's `--script` `Step`
/// action had no fixture exercising this modulation source (only
/// `arm_lfo`'s drivers existed) — see `envelope_modulation_scene`.
fn arm_envelope(fx: &mut PresetInstance) {
    let param_id = manifold_core::preset_definition_registry::try_get(fx.effect_type())
        .and_then(|def| def.param_defs.first().map(|pd| pd.spec.id.clone()));
    if let Some(param_id) = param_id {
        fx.envelopes = Some(vec![manifold_core::effects::ParamEnvelope::new(param_id)]);
    }
}

fn lid(s: &str) -> LayerId {
    LayerId::new(s)
}

/// The redesign-mockup scene: one of every layer type, a group with two
/// children, the generator layer selected.
fn timeline_scene() -> SceneData {
    let mut layers: Vec<Layer> = Vec::new();

    // 0: TEXT BOT L — text lane (modelled as a video lane carrying named clips).
    let mut text = Layer::new("TEXT BOT L".into(), LayerType::Video, 0);
    text.layer_id = lid("text-bot-l");
    text.clips.push(TimelineClip::new_video("EXILE".into(), Beats(0.0), Beats(16.0), Seconds::ZERO));
    text.clips.push(TimelineClip::new_video("RETURN".into(), Beats(16.0), Beats(12.0), Seconds::ZERO));
    text.clips.push(TimelineClip::new_video("HORIZON".into(), Beats(28.0), Beats(20.0), Seconds::ZERO));
    layers.push(text);

    // 1: FLOWERS — video, two clips.
    let mut flowers = Layer::new("FLOWERS".into(), LayerType::Video, 1);
    flowers.layer_id = lid("flowers");
    flowers.clips.push(TimelineClip::new_video("flowers_loop_A.mov".into(), Beats(0.0), Beats(28.0), Seconds::ZERO));
    flowers.clips.push(TimelineClip::new_video("flowers_loop_B.mov".into(), Beats(28.0), Beats(20.0), Seconds::ZERO));
    layers.push(flowers);

    // 2: PLASMA — generator, the SELECTED layer. `new_generator` (not `new`)
    // so `gen_params` is actually populated — P0.5 evidence needs a real
    // `generator_type` to show the label; a bare `LayerType::Generator` layer
    // with no gen_params is the same "renders black" trap `Layer::new_generator`'s
    // own doc comment warns about, just surfacing here as a missing label
    // instead (`docs/TIMELINE_LAYOUT_P0_SPEC.md` P0.5).
    let mut plasma = Layer::new_generator("PLASMA".into(), PresetTypeId::PLASMA, 2);
    plasma.layer_id = lid("plasma");
    plasma.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(48.0)));
    layers.push(plasma);

    // 3: BG STACK — group parent.
    let mut group = Layer::new("BG STACK".into(), LayerType::Group, 3);
    group.layer_id = lid("bg-stack");
    layers.push(group);

    // 4: CLOUDS — video child of BG STACK.
    let mut clouds = Layer::new("CLOUDS".into(), LayerType::Video, 4);
    clouds.layer_id = lid("clouds");
    clouds.parent_layer_id = Some(lid("bg-stack"));
    clouds.clips.push(TimelineClip::new_video("clouds_slow.mov".into(), Beats(0.0), Beats(24.0), Seconds::ZERO));
    clouds.clips.push(TimelineClip::new_video("clouds_fast.mov".into(), Beats(24.0), Beats(24.0), Seconds::ZERO));
    layers.push(clouds);

    // 5: NOISE FIELD — generator child of BG STACK.
    let mut noise = Layer::new("NOISE FIELD".into(), LayerType::Generator, 5);
    noise.layer_id = lid("noise-field");
    noise.parent_layer_id = Some(lid("bg-stack"));
    noise.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(48.0)));
    layers.push(noise);

    // 6: KICK — audio.
    let mut kick = Layer::new("KICK".into(), LayerType::Audio, 6);
    kick.layer_id = lid("kick");
    kick.clips.push(TimelineClip::new_audio(
        "kick_bus.wav".into(),
        Beats(0.0),
        Beats(48.0),
        Seconds::ZERO,
        Seconds(10.0),
    ));
    layers.push(kick);

    // Timeline has private lookup maps, so build the default and set the public
    // `layers` field (nested assign — not the flagged default-reassign pattern).
    let mut project = Project::default();
    project.timeline.layers = layers;

    let content = ContentState { current_beat: Beats(20.0), is_playing: false, ..Default::default() };

    // No selection by default — `--interact select:<layer>` makes the ring appear,
    // so base-vs-after renders/dumps differ measurably.
    SceneData { project, content, active: None, selection: UIState::default() }
}

/// P0.0 evidence scene for `docs/TIMELINE_LAYOUT_P0_SPEC.md`'s RC1 (dual scroll
/// state): 14 video layers at `TrackHeight::Normal` — well past the
/// `LOGICAL_H` viewport budget the `timeline` scene was sized to exactly fit
/// (7 lanes), so a vertical scrollbar exists and a `--scroll` seed is
/// meaningful. Used with `--scroll <px>` (seeds both `Viewport::scroll_y_px`
/// and `LayerHeaderPanel`'s `ScrollContainer` offset identically, mirroring
/// `ui_root.rs:512-517`'s settings-restore path) plus `--interact
/// collapse:<id>` on one of the layers, to capture the header/lane detach when
/// a scrolled, content-shrinking edit reclamps one column's scroll state but
/// not the other's.
fn scroll_shrink_scene() -> SceneData {
    let mut layers: Vec<Layer> = Vec::new();
    for i in 0..14 {
        let id = format!("stack-{i}");
        let mut l = Layer::new(format!("LAYER {i}"), LayerType::Video, i);
        l.layer_id = lid(&id);
        l.clips.push(TimelineClip::new_video(format!("{id}.mov"), Beats(0.0), Beats(48.0), Seconds::ZERO));
        layers.push(l);
    }

    let mut project = Project::default();
    project.timeline.layers = layers;

    let content = ContentState { current_beat: Beats(0.0), is_playing: false, ..Default::default() };

    SceneData { project, content, active: None, selection: UIState::default() }
}

/// P4a evidence scene (`docs/AUTOMATION_LANES_DESIGN.md` §7): the same layer
/// set as `timeline_scene`, but with the automation transport globals LIVE —
/// Automation Arm on, lane strips visible — so the transport bar's ARM/BACK/
/// LANES buttons render their lit state, AND the FLOWERS layer carries two
/// real automation lanes for the lane-strip renderer (P4 lane-strip
/// rendering, the read-only visual layer):
/// - Mirror's lane: three points, Linear then Curved segments, LIVE (red).
/// - Bloom's lane: two points, Hold then Linear, OVERRIDDEN (grayed) — its
///   `(EffectId, ParamId)` is in `automation_latched_params`, exercising the
///   override-graying path.
fn automation_scene() -> SceneData {
    use manifold_core::effects::{AutomationLane, AutomationPoint, SegmentShape};

    let mut data = timeline_scene();
    data.content.automation_armed = true;
    data.selection.automation_mode_visible = true;

    let flowers = data
        .project
        .timeline
        .layers
        .iter_mut()
        .find(|l| l.layer_id == lid("flowers"))
        .expect("timeline_scene always has a 'flowers' layer");

    let mut mirror = effect("Mirror");
    let mirror_param = manifold_core::preset_definition_registry::try_get(mirror.effect_type())
        .and_then(|def| def.param_defs.first().map(|pd| pd.spec.id.clone()))
        .expect("Mirror has at least one automatable param");
    mirror.automation_lanes = Some(vec![AutomationLane {
        param_id: mirror_param.into(),
        enabled: true,
        points: vec![
            AutomationPoint { beat: Beats(0.0), value: 0.1, shape: SegmentShape::Linear },
            AutomationPoint { beat: Beats(16.0), value: 0.9, shape: SegmentShape::Curved(0.6) },
            AutomationPoint { beat: Beats(32.0), value: 0.3, shape: SegmentShape::Linear },
        ],
    }]);

    let mut bloom = effect("Bloom");
    let bloom_param = manifold_core::preset_definition_registry::try_get(bloom.effect_type())
        .and_then(|def| def.param_defs.first().map(|pd| pd.spec.id.clone()))
        .expect("Bloom has at least one automatable param");
    // Bloom's `amount` registers 0..5 (not 0..1 like Mirror's) — pick values
    // far apart in that range (not just 0.2/0.8) so the Hold-then-jump reads
    // clearly once normalized, instead of both points sitting near the
    // bottom of the strip.
    bloom.automation_lanes = Some(vec![AutomationLane {
        param_id: bloom_param.clone().into(),
        enabled: true,
        points: vec![
            AutomationPoint { beat: Beats(0.0), value: 0.5, shape: SegmentShape::Hold },
            AutomationPoint { beat: Beats(20.0), value: 4.5, shape: SegmentShape::Linear },
        ],
    }]);
    let bloom_id = bloom.id.clone();

    flowers.effects = Some(vec![mirror, bloom]);

    data.content.automation_latched_params = vec![(bloom_id, bloom_param.into())];
    data
}

/// P5 (`docs/AUTOMATION_LANES_DESIGN.md` §7 addendum) evidence scene: proves
/// the "first-draw path" end to end — a param the user has chosen but never
/// automated renders as a flat line with NO dot (not `automation_scene`'s
/// already-real Mirror/Bloom lanes, which would make the two states
/// impossible to tell apart in a screenshot), and a real click on that line
/// creates the lane. Kept separate from `automation_scene` so this scene's
/// evidence never disturbs `toggle-lanes.json`/`drag-automation-point.json`'s
/// pixel-exact assertions against Mirror/Bloom's Y offsets.
fn automation_placeholder_scene() -> SceneData {
    let mut data = timeline_scene();
    data.selection.automation_mode_visible = true;

    let flowers = data
        .project
        .timeline
        .layers
        .iter_mut()
        .find(|l| l.layer_id == lid("flowers"))
        .expect("timeline_scene always has a 'flowers' layer");

    let mirror = effect("Mirror");
    let mirror_id = mirror.id.clone();
    let mirror_param = manifold_core::preset_definition_registry::try_get(mirror.effect_type())
        .and_then(|def| def.param_defs.first().map(|pd| pd.spec.id.clone()))
        .expect("Mirror has at least one automatable param");
    // No `automation_lanes` set — this param has never been automated. The
    // placeholder is what makes it choosable/drawable anyway.
    flowers.effects = Some(vec![mirror]);

    data.selection.chosen_automation_params.insert(
        lid("flowers"),
        (
            manifold_ui::view::UiGraphTarget::Effect(mirror_id),
            mirror_param.into(),
        ),
    );
    data
}

/// AUDIO_SENDS_UX_DESIGN Phase 2 gate scene: two sends, one fully wired (fed
/// by an audio layer, consumed by an enabled audio mod on a named layer's
/// effect param AND an enabled trigger route to a named layer) and one left
/// completely empty — so the Audio Setup panel's Inputs/Consumers sections
/// (`--interact "open:audio_setup"`) have real content on both sides. Send A
/// is pushed first so the panel's default-select picks it with no extra
/// interact step.
fn audio_sends_scene() -> SceneData {
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
    use manifold_core::audio_setup::AudioSend;
    use manifold_core::audio_trigger::TriggerRoute;

    // 0: KICK — audio layer, feeds Send A.
    let mut kick = Layer::new("KICK".into(), LayerType::Audio, 0);
    kick.layer_id = lid("kick");

    // 1: BLOOM LAYER — carries the Bloom effect whose first param has the
    // enabled audio mod that consumes Send A.
    let mut bloom_layer = Layer::new("BLOOM LAYER".into(), LayerType::Video, 1);
    bloom_layer.layer_id = lid("bloom-layer");
    bloom_layer.clips.push(TimelineClip::new_video(
        "bloom_loop.mov".into(),
        Beats(0.0),
        Beats(48.0),
        Seconds::ZERO,
    ));

    // 2: STROBE LAYER — the enabled Low trigger route's target.
    let mut strobe_layer = Layer::new("STROBE LAYER".into(), LayerType::Video, 2);
    strobe_layer.layer_id = lid("strobe-layer");
    strobe_layer.clips.push(TimelineClip::new_video(
        "strobe.mov".into(),
        Beats(0.0),
        Beats(48.0),
        Seconds::ZERO,
    ));

    let mut project = Project::default();

    let mut send_a = AudioSend::new("Kick");
    send_a.source.layers.push(lid("kick"));
    let mut low_route = TriggerRoute::new(AudioBand::Low);
    low_route.enabled = true;
    low_route.target_layer = Some(lid("strobe-layer"));
    send_a.triggers.push(low_route);
    let send_a_id = send_a.id.clone();

    // Send B: no capture, no layers, no mods, no triggers — fully empty.
    let send_b = AudioSend::new("Amb");

    project.audio_setup.sends.push(send_a);
    project.audio_setup.sends.push(send_b);

    let mut bloom = effect("Bloom");
    let bloom_param = manifold_core::preset_definition_registry::try_get(bloom.effect_type())
        .and_then(|def| def.param_defs.first().map(|pd| pd.spec.id.clone()))
        .expect("Bloom has at least one param");
    // `ParameterAudioMod::new` defaults `enabled: true` (audio_mod.rs) — no
    // separate enable step needed.
    bloom
        .audio_mods_mut()
        .push(ParameterAudioMod::new(
            bloom_param.clone().into(),
            send_a_id.clone(),
            AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full),
        ));
    bloom_layer.effects = Some(vec![bloom]);

    let mut layers = vec![kick, bloom_layer, strobe_layer];

    // BUG-199 (dock scroll): 24 extra Send-A consumer layers, each a distinct
    // video layer carrying its own Bloom-audio-mod consumer row in the
    // "Consumers — Kick" list. Pushes the Audio Setup dock's content well
    // past its viewport height (`audio-dock-scroll.json`'s acceptance flow),
    // without touching the sends themselves (Send A/B stay exactly as
    // above) — `audio-setup-hygiene.json`'s gain-reset assertions only key
    // on the two sends' gain labels and are unaffected by extra consumers.
    for i in 0..24 {
        let mut extra_layer = Layer::new(format!("EXTRA {i}"), LayerType::Video, 3 + i);
        extra_layer.layer_id = lid(&format!("extra-consumer-{i}"));
        extra_layer.clips.push(TimelineClip::new_video(
            format!("extra_{i}.mov"),
            Beats(0.0),
            Beats(48.0),
            Seconds::ZERO,
        ));
        let mut extra_bloom = effect("Bloom");
        extra_bloom.audio_mods_mut().push(ParameterAudioMod::new(
            bloom_param.clone().into(),
            send_a_id.clone(),
            AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full),
        ));
        extra_layer.effects = Some(vec![extra_bloom]);
        layers.push(extra_layer);
    }

    project.timeline.layers = layers;

    let content = ContentState { current_beat: Beats(0.0), is_playing: false, ..Default::default() };
    SceneData { project, content, active: Some(1), selection: UIState::default() }
}

/// P0.3 evidence scene for `docs/TIMELINE_LAYOUT_P0_SPEC.md`: one lane of many
/// short trigger clips — the MIDI-mockup workflow's bread and butter — spread
/// across a wide beat range. Rendered at the minimum zoom level
/// (`color::ZOOM_LEVELS[0]` = 1px/beat, wired in by `ui_snapshot::mod`'s
/// `zoom_ppb` override for this scene name) so each clip's on-screen width
/// (0.5 beats × 1px/beat = 0.5px) rounds below 1px. Proves `visible_clip_rects`
/// clamps sub-pixel clips to a 1px hairline instead of culling them — pre-fix
/// every one of these 200 clips renders as nothing.
fn hairline_clips_scene() -> SceneData {
    let mut trigs = Layer::new("TRIGGERS".into(), LayerType::Video, 0);
    trigs.layer_id = lid("triggers");
    for i in 0..200 {
        let start = Beats(i as f64 * 4.0);
        trigs
            .clips
            .push(TimelineClip::new_video(format!("trig_{i}"), start, Beats(0.5), Seconds::ZERO));
    }

    let mut project = Project::default();
    project.timeline.layers = vec![trigs];

    let content = ContentState { current_beat: Beats(0.0), is_playing: false, ..Default::default() };

    SceneData { project, content, active: None, selection: UIState::default() }
}

/// P1.0 evidence scene for `docs/TIMELINE_INTERACTION_P1_SPEC.md`'s S1/S3:
/// one layer with exactly 4 contiguous video clips at normal zoom, so
/// `--interact "click_clip:...;shift_click_clip:..."` / `cmd_click_clip` /
/// `cmd_d` chains have clean geometry to act on (no gaps to obscure whether a
/// chrome mismatch is the selection-authority bug or a fixture artifact). A
/// second layer with its own single clip sits alongside so the scene still
/// reads as a real multi-track session, not a single lane in isolation.
fn selection_clips_scene() -> SceneData {
    // `TimelineClip::new_video`'s first arg is the display name / video_clip_id,
    // not the `ClipId` — the id defaults to a fresh random one, so it's
    // overwritten explicitly below to fixed, predictable ids the interact
    // verbs (`click_clip:clip-a:...` etc.) can address.
    fn clip(id: &str, name: &str, start: f64, dur: f64) -> TimelineClip {
        let mut c = TimelineClip::new_video(name.into(), Beats(start), Beats(dur), Seconds::ZERO);
        c.id = manifold_core::ClipId::new(id);
        c
    }

    let mut clips_layer = Layer::new("SELECT ME".into(), LayerType::Video, 0);
    clips_layer.layer_id = lid("select-clips");
    clips_layer.clips.push(clip("clip-a", "clip_a", 0.0, 8.0));
    clips_layer.clips.push(clip("clip-b", "clip_b", 8.0, 8.0));
    clips_layer.clips.push(clip("clip-c", "clip_c", 16.0, 8.0));
    clips_layer.clips.push(clip("clip-d", "clip_d", 24.0, 8.0));

    let mut other = Layer::new("OTHER LANE".into(), LayerType::Video, 1);
    other.layer_id = lid("other-lane");
    other.clips.push(clip("other-clip", "other_clip", 0.0, 32.0));

    let mut project = Project::default();
    project.timeline.layers = vec![clips_layer, other];

    let content = ContentState { current_beat: Beats(0.0), is_playing: false, ..Default::default() };

    SceneData { project, content, active: None, selection: UIState::default() }
}

/// Inspector-focused scene: a selected video layer carrying a real effect chain
/// (Mirror → Bloom), so a single headless render shows the inspector's param
/// cards / sliders / chrome — the surface the `timeline` scene hides (it zeroes
/// the inspector width). A couple of context layers sit above/below so the
/// timeline still reads as a real session next to the inspector.
///
/// P4 §7 evidence (the param-card "automated" dot): Mirror carries an
/// enabled, non-empty automation lane with no latch — its card shows the red
/// dot. Bloom carries one too, but its `(EffectId, ParamId)` is latched in
/// `automation_latched_params` — its card shows the gray (overridden) dot.
/// Mirrors `automation_scene`'s Mirror-live / Bloom-overridden pairing.
///
/// §9 evidence (`LIVE_AUDIO_TRIGGERS_DESIGN.md` U-P2): Strobe carries an
/// armed trigger-gate audio mod (`clip_trigger`, mode Transient) and Plasma
/// carries one too (mode Both) — both reach the STANDARD audio-mod drawer
/// (Source/Feature/Band/Inv/Delta/Amount/Attack/Release) plus its trailing
/// Mode row, and both show the collapsed-row mode badge next to the toggle.
/// Bloom's plain "Radius"-style slider (no mod armed) sits in the same
/// screenshot as the regression look: a drawer-less row is unaffected by the
/// unification.
fn inspector_scene() -> SceneData {
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
    use manifold_core::audio_setup::AudioSend;
    use manifold_core::audio_trigger::TriggerFireMode;
    use manifold_core::effects::{AutomationLane, AutomationPoint, SegmentShape};

    let kick_send = AudioSend::new("Kick");
    let kick_send_id = kick_send.id.clone();

    // BUG-068: this scene forces a generous `inspector_width` (600px of the
    // 1536px canvas, set below in `ui_snapshot::mod`) so the selected layer's
    // param cards have room — at the fixed 24px/beat zoom that leaves only
    // ~29 beats of clip area before the inspector column starts. Every clip
    // below is kept at or under 20 beats (480px, ~226px of clearance) so it
    // renders and hit-tests entirely within the track area — none of them
    // bleed under the inspector the way the old 48-beat clips did.
    let mut text = Layer::new("TEXT BOT L".into(), LayerType::Video, 0);
    text.layer_id = lid("text-bot-l");
    text.clips
        .push(TimelineClip::new_video("EXILE".into(), Beats(0.0), Beats(10.0), Seconds::ZERO));
    text.clips
        .push(TimelineClip::new_video("RETURN".into(), Beats(10.0), Beats(10.0), Seconds::ZERO));

    // The subject: a video layer with a three-effect chain. Selected, so the
    // inspector shows its layer card + the Mirror, Bloom, and Strobe cards.
    let mut glow = Layer::new("GLOW".into(), LayerType::Video, 1);
    glow.layer_id = lid("glow");
    glow.clips
        .push(TimelineClip::new_video("glow_loop.mov".into(), Beats(0.0), Beats(20.0), Seconds::ZERO));
    let mut mirror = effect("Mirror");
    arm_lfo(&mut mirror); // arms the driver on Mirror's FIRST param.
    // The automation lane goes on a DIFFERENT param (second, if Mirror has
    // one) so the automated dot's row is distinct from the LFO-armed row —
    // clean evidence for each indicator instead of both stacked on one row.
    let mirror_param = manifold_core::preset_definition_registry::try_get(mirror.effect_type())
        .and_then(|def| {
            def.param_defs
                .get(1)
                .or_else(|| def.param_defs.first())
                .map(|pd| pd.spec.id.clone())
        })
        .expect("Mirror has at least one automatable param");
    mirror.automation_lanes = Some(vec![AutomationLane {
        param_id: mirror_param.into(),
        enabled: true,
        points: vec![
            AutomationPoint { beat: Beats(0.0), value: 0.1, shape: SegmentShape::Linear },
            AutomationPoint { beat: Beats(16.0), value: 0.9, shape: SegmentShape::Curved(0.6) },
            AutomationPoint { beat: Beats(32.0), value: 0.3, shape: SegmentShape::Linear },
        ],
    }]);

    let mut bloom = effect("Bloom");
    let bloom_param = manifold_core::preset_definition_registry::try_get(bloom.effect_type())
        .and_then(|def| def.param_defs.first().map(|pd| pd.spec.id.clone()))
        .expect("Bloom has at least one automatable param");
    bloom.automation_lanes = Some(vec![AutomationLane {
        param_id: bloom_param.clone().into(),
        enabled: true,
        points: vec![
            AutomationPoint { beat: Beats(0.0), value: 0.5, shape: SegmentShape::Hold },
            AutomationPoint { beat: Beats(20.0), value: 4.5, shape: SegmentShape::Linear },
        ],
    }]);
    let bloom_id = bloom.id.clone();
    // Regression look (§9 U2): a PLAIN slider's own armed audio mod reaches
    // the exact same drawer builder as Strobe's/Plasma's trigger-gate rows,
    // minus the Mode row — the direct side-by-side proof the unification
    // didn't disturb the non-gate path.
    let bloom_mod = ParameterAudioMod::new(
        bloom_param.clone().into(),
        kick_send_id.clone(),
        AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full),
    );
    bloom.audio_mods = Some(vec![bloom_mod]);

    // Strobe: the effect-side trigger-gate reachability proof (§8 P3a) — its
    // `clip_trigger` card gets an armed audio mod with `trigger_mode:
    // Some(Transient)`, so the card shows the standard drawer OPEN with the
    // Mode row highlighting "Audio", plus the collapsed-row badge.
    let mut strobe = effect("Strobe");
    let mut strobe_trigger = ParameterAudioMod::new(
        "clip_trigger".into(),
        kick_send_id.clone(),
        AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
    );
    strobe_trigger.trigger_mode = Some(TriggerFireMode::Transient);
    strobe.audio_mods = Some(vec![strobe_trigger]);

    glow.effects = Some(vec![mirror, bloom, strobe]);

    // P3b: one enabled `LayerClipTrigger` on GLOW itself — the layer this
    // scene selects — so the inspector's AUDIO TRIGGERS section (P3b,
    // AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md) has a real row to
    // show/expand. Kick + Low mirrors the design doc's own row-label example
    // verbatim ("Low → Kick").
    let mut glow_trigger = manifold_core::audio_trigger::LayerClipTrigger::new(
        manifold_core::audio_mod::AudioModSource {
            send_id: kick_send_id.clone(),
            feature: AudioFeature::new(AudioFeatureKind::Kick, AudioBand::Low),
        },
    );
    glow_trigger.enabled = true;
    glow.clip_triggers.push(glow_trigger);

    // Plasma: the generator-side proof, mode Both (the arm-time default,
    // §9 U3) — drawer open, badge reads "Both".
    let mut plasma = Layer::new_generator("PLASMA".into(), manifold_core::PresetTypeId::PLASMA, 2);
    plasma.layer_id = lid("plasma");
    plasma.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(20.0)));
    let mut plasma_trigger = ParameterAudioMod::new(
        "clip_trigger".into(),
        kick_send_id,
        AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Full),
    );
    plasma_trigger.trigger_mode = Some(TriggerFireMode::Both);
    plasma.gen_params_or_init().audio_mods = Some(vec![plasma_trigger]);

    let mut project = Project::default();
    project.audio_setup.sends.push(kick_send);
    project.timeline.layers = vec![text, glow, plasma];

    let mut content = ContentState { current_beat: Beats(8.0), is_playing: false, ..Default::default() };
    content.automation_latched_params = vec![(bloom_id, bloom_param.into())];

    let mut selection = UIState::default();
    selection.select_layer(lid("glow"));

    SceneData { project, content, active: Some(1), selection }
}

/// `UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` P1 gate scene: BUG-060 (inspector
/// painting over the footer). Reuses `inspector_scene`'s Mirror/Bloom/Strobe
/// chain (Strobe's trigger-gate drawer already open, per its own doc comment)
/// and stacks SEVEN more Bloom-shaped effects, each with its own armed audio
/// mod (drawer open), onto the same layer — enough card height that the
/// column genuinely overflows the inspector's visible rect in a normal
/// window, so `--interact`/a `--script` scroll-to-bottom actually moves
/// content instead of hitting an already-fully-visible stack (the failure
/// mode `inspector_scene` itself has too little content for). The scene name
/// mirrors the bug id so a reader doesn't have to open this function to know
/// what it's for — see `docs/BUG_BACKLOG.md` BUG-060.
fn bug060_scene() -> SceneData {
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
    use manifold_core::audio_setup::AudioSend;
    use manifold_core::audio_trigger::TriggerFireMode;

    let kick_send = AudioSend::new("Kick");
    let kick_send_id = kick_send.id.clone();

    let mut glow = Layer::new("GLOW".into(), LayerType::Video, 0);
    glow.layer_id = lid("glow");
    glow.clips
        .push(TimelineClip::new_video("glow_loop.mov".into(), Beats(0.0), Beats(48.0), Seconds::ZERO));

    let mut effects = Vec::new();

    let mut mirror = effect("Mirror");
    arm_lfo(&mut mirror);
    effects.push(mirror);

    // Strobe first (not last) so the trigger-gate drawer + its trailing Mode
    // row sit mid-stack, not conveniently at the visible top — a scroll to
    // the bottom must still carry it, same as any other card.
    let mut strobe = effect("Strobe");
    let mut strobe_trigger = ParameterAudioMod::new(
        "clip_trigger".into(),
        kick_send_id.clone(),
        AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
    );
    strobe_trigger.trigger_mode = Some(TriggerFireMode::Transient);
    strobe.audio_mods = Some(vec![strobe_trigger]);
    effects.push(strobe);

    // Seven more Bloom-shaped cards, each with an armed (drawer-open) audio
    // mod on its first param — enough total height to force scrolling.
    for i in 0..7 {
        let mut bloom = effect("Bloom");
        let bloom_param = manifold_core::preset_definition_registry::try_get(bloom.effect_type())
            .and_then(|def| def.param_defs.first().map(|pd| pd.spec.id.clone()))
            .expect("Bloom has at least one param");
        let bloom_mod = ParameterAudioMod::new(
            bloom_param.into(),
            kick_send_id.clone(),
            AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full),
        );
        bloom.audio_mods = Some(vec![bloom_mod]);
        bloom.id = manifold_core::EffectId::new(format!("bloom-{i}"));
        effects.push(bloom);
    }

    glow.effects = Some(effects);

    let mut project = Project::default();
    project.audio_setup.sends.push(kick_send);
    project.timeline.layers = vec![glow];

    let content = ContentState { current_beat: Beats(8.0), is_playing: false, ..Default::default() };

    let mut selection = UIState::default();
    selection.select_layer(lid("glow"));

    SceneData { project, content, active: Some(0), selection }
}

/// `UI_HARNESS_UNIFICATION_DESIGN.md` P0 (D7): Peter's stated BUG-060
/// worst-case — a Plasma GENERATOR layer (selected/active, so the inspector
/// shows its own card too), carrying a stacked Color Compass effect (3
/// instances — the reopen notes' "Plasma + stacked Color Compass" repro)
/// plus 6 additional DISTINCT effects chosen for high param counts / dense
/// modulation draws (`docs/NODE_CATALOG.md` §6.1 param counts):
/// ColorGrade(9), DepthOfField(8), WireframeDepth(8), ChromaticAberration(5),
/// Glitch(5), Strobe(4). 9 cards total. Several carry an armed (open)
/// audio-mod drawer — the heaviest per-frame modulation draw case, matching
/// Peter's "heavy modulation while scrolling" repro — and Strobe carries an
/// armed TRIGGER-GATE (`clip_trigger`) mod, the concrete BUG-060 escape
/// named in `docs/BUG_BACKLOG.md`. WireframeDepth and one Color Compass
/// instance are left plain (no mod) so the compact-toggle (§6b) gesture the
/// differential drives exercises a mix of armed/unarmed cards, not an
/// all-or-nothing set.
fn bug060heavy_scene() -> SceneData {
    use manifold_core::audio_mod::{AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod};
    use manifold_core::audio_setup::AudioSend;
    use manifold_core::audio_trigger::TriggerFireMode;

    let kick_send = AudioSend::new("Kick");
    let kick_send_id = kick_send.id.clone();

    // Arm an enabled ParameterAudioMod on `fx`'s first param — an open,
    // standard drawer at build time (mirrors `inspector_scene`'s Bloom).
    let arm_audio_mod = |fx: &mut PresetInstance, kick_send_id: &manifold_core::AudioSendId| {
        let param_id = manifold_core::preset_definition_registry::try_get(fx.effect_type())
            .and_then(|def| def.param_defs.first().map(|pd| pd.spec.id.clone()));
        if let Some(param_id) = param_id {
            let am = ParameterAudioMod::new(
                param_id.into(),
                kick_send_id.clone(),
                AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full),
            );
            fx.audio_mods = Some(vec![am]);
        }
    };

    let mut plasma = Layer::new_generator("PLASMA".into(), PresetTypeId::PLASMA, 0);
    plasma.layer_id = lid("plasma-heavy");
    plasma.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(48.0)));

    let mut effects = Vec::new();

    // Stacked Color Compass: LFO-armed, audio-mod-armed, plain.
    let mut cc0 = effect("ColorCompass");
    arm_lfo(&mut cc0);
    effects.push(cc0);
    let mut cc1 = effect("ColorCompass");
    arm_audio_mod(&mut cc1, &kick_send_id);
    effects.push(cc1);
    effects.push(effect("ColorCompass")); // plain — no drawer

    // 6 distinct, high-param-count effects.
    let mut color_grade = effect("ColorGrade");
    arm_audio_mod(&mut color_grade, &kick_send_id);
    effects.push(color_grade);

    let mut depth_of_field = effect("DepthOfField");
    arm_audio_mod(&mut depth_of_field, &kick_send_id);
    effects.push(depth_of_field);

    effects.push(effect("WireframeDepth")); // plain — no drawer

    let mut chromatic_aberration = effect("ChromaticAberration");
    arm_audio_mod(&mut chromatic_aberration, &kick_send_id);
    effects.push(chromatic_aberration);

    let mut glitch = effect("Glitch");
    arm_audio_mod(&mut glitch, &kick_send_id);
    effects.push(glitch);

    // Strobe: the trigger-gate escape (BUG-060's concrete named case).
    let mut strobe = effect("Strobe");
    let mut strobe_trigger = ParameterAudioMod::new(
        "clip_trigger".into(),
        kick_send_id,
        AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
    );
    strobe_trigger.trigger_mode = Some(TriggerFireMode::Transient);
    strobe.audio_mods = Some(vec![strobe_trigger]);
    effects.push(strobe);

    plasma.effects = Some(effects);

    let mut project = Project::default();
    project.audio_setup.sends.push(kick_send);
    project.timeline.layers = vec![plasma];

    let content = ContentState { current_beat: Beats(8.0), is_playing: false, ..Default::default() };

    let mut selection = UIState::default();
    selection.select_layer(lid("plasma-heavy"));

    SceneData { project, content, active: Some(0), selection }
}

/// `UI_CLIP_AND_Z_OWNERSHIP_DESIGN.md` P1 gate scene: BUG-047 (Audio Setup
/// panel content spilling past the panel edge with ≥18 rows). 20 sends, each
/// carrying a source layer and one enabled trigger route, so both the
/// Inputs and Consumers sections of the Audio Setup modal (opened via
/// `--interact "open:audio_setup"`) have real per-row content to overflow
/// with — `audio_sends_scene`'s 2 sends are nowhere near enough to exercise
/// this. See `docs/BUG_BACKLOG.md` BUG-047.
fn bug047_scene() -> SceneData {
    use manifold_core::audio_setup::AudioSend;
    use manifold_core::audio_trigger::TriggerRoute;

    let mut project = Project::default();
    let mut layers = Vec::new();

    for i in 0..20 {
        let send_layer_id = lid(&format!("send-src-{i}"));
        let mut send_layer = Layer::new(format!("SRC {i}"), LayerType::Audio, i);
        send_layer.layer_id = send_layer_id.clone();
        layers.push(send_layer);

        let target_layer_id = lid(&format!("send-tgt-{i}"));
        let mut target_layer = Layer::new(format!("TGT {i}"), LayerType::Video, 20 + i);
        target_layer.layer_id = target_layer_id.clone();
        target_layer.clips.push(TimelineClip::new_video(
            format!("tgt_{i}.mov"),
            Beats(0.0),
            Beats(48.0),
            Seconds::ZERO,
        ));
        // The first target layer carries an effect so the inspector shows real
        // param rows — the AUDIO_SETUP_DOCK D1 gate proves the inspector stays
        // usable (a param row is still clickable) while the dock is open.
        if i == 0 {
            target_layer.effects = Some(vec![effect("Bloom")]);
        }
        layers.push(target_layer);

        let mut send = AudioSend::new(format!("Send {i}"));
        send.source.layers.push(send_layer_id);
        let mut route = TriggerRoute::new(manifold_core::audio_mod::AudioBand::Full);
        route.enabled = true;
        route.target_layer = Some(target_layer_id);
        send.triggers.push(route);
        project.audio_setup.sends.push(send);
    }

    project.timeline.layers = layers;

    let content = ContentState { current_beat: Beats(0.0), is_playing: false, ..Default::default() };
    // Active = the first target layer (index 1: layers are [SRC0, TGT0, …]) so
    // the inspector renders TGT 0's Bloom params.
    SceneData { project, content, active: Some(1), selection: UIState::default() }
}

/// PARAM_STEP_ACTIONS P3 evidence scene: the drawer's D8 Action/Amount/Wrap
/// rows on both a whole-numbers param and a continuous one.
///
/// - PLASMA's `pattern` (whole_numbers, 0..7) carries a Step mod, pre-armed
///   directly (the same "construct the state for evidence" convention
///   `inspector_scene()` uses for its LFO/trigger-gate rows) — the drawer
///   renders open with Action=Step, the Amount slider snapped to whole
///   numbers, and the Wrap row.
/// - GLOW's Bloom `amount` (continuous, 0..5) starts ARMED but Continuous —
///   the `scripts/ui-flows/param-step-action.json` acceptance flow selects
///   GLOW (Bloom's card, and its drawer, builds fresh and SNAPS open at full
///   height — no reveal tween in flight, see the note below) then clicks
///   Action=Step through the REAL click path (AudioModSetActionKind), so the
///   vertical slice (model → command → UI → pixels) is exercised at least
///   once, not just constructed. Bloom does NOT start unarmed: the P1 drawer
///   reveal tween (`ParamCardPanel::tick_drawers`) only advances via a
///   per-frame wall-clock tick that no per-frame loop exists to drive in this
///   `--script` harness (a real gap, logged as BUG-073 — arming a mod live
///   inside a script would show a permanently zero-height clip region since
///   the tween never gets ticked past t=0). Building the card already-armed
///   sidesteps it entirely: a param count unchanged since the card's own
///   first `configure()` call snaps `drawer_height_anim` straight to its
///   target (`param_card.rs`'s "a *new* param... snaps so it never stalls
///   half-open") — the Action row still requires a REAL click to prove D8's
///   drawer wiring, it just isn't the FIRST arm of the mod itself.
fn param_steps_scene() -> SceneData {
    use manifold_core::audio_mod::{
        AudioBand, AudioFeature, AudioFeatureKind, ParameterAudioMod, TriggerAction, WrapMode,
    };
    use manifold_core::audio_setup::AudioSend;

    let kick_send = AudioSend::new("Kick");
    let kick_send_id = kick_send.id.clone();

    let mut glow = Layer::new("GLOW".into(), LayerType::Video, 0);
    glow.layer_id = lid("glow");
    glow.clips
        .push(TimelineClip::new_video("glow_loop.mov".into(), Beats(0.0), Beats(48.0), Seconds::ZERO));

    let mut bloom = effect("Bloom");
    let bloom_amount_mod = ParameterAudioMod::new(
        "amount".into(),
        kick_send_id.clone(),
        AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full),
    );
    bloom.audio_mods = Some(vec![bloom_amount_mod]);
    glow.effects = Some(vec![bloom]);

    let mut plasma = Layer::new_generator("PLASMA".into(), manifold_core::PresetTypeId::PLASMA, 1);
    plasma.layer_id = lid("plasma");
    plasma.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(48.0)));
    let mut pattern_step = ParameterAudioMod::new(
        "pattern".into(),
        kick_send_id.clone(),
        AudioFeature::new(AudioFeatureKind::Transients, AudioBand::Low),
    );
    pattern_step.action = TriggerAction::Step { amount: 1.0, wrap: WrapMode::Wrap };
    plasma.gen_params_or_init().audio_mods = Some(vec![pattern_step]);

    let mut project = Project::default();
    project.audio_setup.sends.push(kick_send);
    project.timeline.layers = vec![glow, plasma];

    let content = ContentState { current_beat: Beats(8.0), is_playing: false, ..Default::default() };

    let mut selection = UIState::default();
    selection.select_layer(lid("plasma"));

    SceneData { project, content, active: Some(1), selection }
}

/// BUG-234 acceptance scene (`scripts/ui-flows/envelope-modulation.json`):
/// one video layer, one long clip starting at the timeline origin so a
/// `--script` run's `Step`-driven clock walks straight into (and stays well
/// inside) its active-clip window, carrying one Bloom effect with
/// `arm_envelope` applied to its only param (`amount`, default 0.50,
/// range 0..5 — see `assets/effect-presets/Bloom.json`). Default envelope
/// target (`target_normalized = 1.0` ⇒ pulls toward 5.00) and decay
/// (`DEFAULT_ENVELOPE_DECAY_BEATS` = 1 beat) make the base (0.50) and the
/// near-start pulled-toward-target value visibly different, and the
/// post-decay value provably settle back to exactly the base (0.50) once
/// elapsed-into-clip clears 1 beat — a clean, computable curve endpoint
/// that only holds if the harness's `Step` action actually runs
/// `evaluate_modulation` each frame. A dedicated fixture (not an existing
/// inspector-bearing one) so this acceptance flow can't disturb
/// `inspector-card-drag-reorder.json`'s pixel assertions.
fn envelope_modulation_scene() -> SceneData {
    let mut glow = Layer::new("GLOW".into(), LayerType::Video, 0);
    glow.layer_id = lid("glow");
    glow.clips
        .push(TimelineClip::new_video("glow_loop.mov".into(), Beats(0.0), Beats(64.0), Seconds::ZERO));

    let mut bloom = effect("Bloom");
    arm_envelope(&mut bloom);
    glow.effects = Some(vec![bloom]);

    let mut project = Project::default();
    project.timeline.layers = vec![glow];

    let content = ContentState { current_beat: Beats(0.0), is_playing: false, ..Default::default() };

    let mut selection = UIState::default();
    selection.select_layer(lid("glow"));

    SceneData { project, content, active: Some(0), selection }
}

/// One layer per state, so a single real render shows the whole state matrix in
/// one image: normal / selected / muted / solo / collapsed / expanded.
fn states_scene() -> SceneData {
    fn vid(name: &str, id: &str, index: i32) -> Layer {
        let mut l = Layer::new(name.into(), LayerType::Video, index);
        l.layer_id = lid(id);
        l.clips
            .push(TimelineClip::new_video(format!("{id}.mov"), Beats(0.0), Beats(40.0), Seconds::ZERO));
        l
    }

    let normal = vid("NORMAL", "normal", 0);
    let selected = vid("SELECTED", "selected", 1);
    let mut muted = vid("MUTED", "muted", 2);
    muted.is_muted = true;
    let mut solo = vid("SOLO", "solo", 3);
    solo.is_solo = true;
    let mut collapsed = vid("COLLAPSED", "collapsed", 4);
    collapsed.is_collapsed = true;

    // A generator layer shows the expanded routing form.
    let mut expanded = Layer::new("EXPANDED".into(), LayerType::Generator, 5);
    expanded.layer_id = lid("expanded");
    expanded.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(40.0)));

    let mut project = Project::default();
    project.timeline.layers = vec![normal, selected, muted, solo, collapsed, expanded];

    let content = ContentState { current_beat: Beats(10.0), is_playing: false, ..Default::default() };

    let mut selection = UIState::default();
    selection.select_layer(lid("selected"));

    SceneData { project, content, active: Some(1), selection }
}

/// Fixture for the `editor` scene: a single generator layer carrying `preset`
/// at registry defaults, so `editor_card_config` resolves the real card — the
/// same `ParamSurface` the live editor's left lane builds from. `None` if
/// `preset` isn't a generator id (the scene only covers the `GraphTarget::Generator`
/// arm today; an effect needs a chain to live in, which this fixture doesn't build).
pub fn generator_editor_fixture(preset: &str) -> Option<(Project, GraphTarget, UIState)> {
    let pid = PresetTypeId::from_string(preset.to_string());
    let is_generator = manifold_renderer::node_graph::bundled_preset_type_ids(PresetKind::Generator)
        .any(|id| id == pid);
    if !is_generator {
        return None;
    }

    let mut layer = Layer::new(preset.into(), LayerType::Generator, 0);
    let layer_id = lid("editor-preview");
    layer.layer_id = layer_id.clone();
    layer.change_generator_type(pid);
    layer.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(48.0)));

    let mut project = Project::default();
    project.timeline.layers = vec![layer];

    Some((project, GraphTarget::Generator(layer_id), UIState::default()))
}
