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
pub fn build(scene: &str) -> Option<SceneData> {
    match scene {
        "timeline" => Some(timeline_scene()),
        "states" => Some(states_scene()),
        "inspector" => Some(inspector_scene()),
        "scrollshrink" => Some(scroll_shrink_scene()),
        "hairlineclips" => Some(hairline_clips_scene()),
        "automation" => Some(automation_scene()),
        _ => None,
    }
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
        .and_then(|def| def.param_ids.first().cloned());
    if let Some(param_id) = param_id {
        fx.drivers = Some(vec![ParameterDriver::new(
            param_id,
            BeatDivision::Quarter,
            DriverWaveform::Sine,
        )]);
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
        .and_then(|def| def.param_ids.first().cloned())
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
        .and_then(|def| def.param_ids.first().cloned())
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

/// Inspector-focused scene: a selected video layer carrying a real effect chain
/// (Mirror → Bloom), so a single headless render shows the inspector's param
/// cards / sliders / chrome — the surface the `timeline` scene hides (it zeroes
/// the inspector width). A couple of context layers sit above/below so the
/// timeline still reads as a real session next to the inspector.
fn inspector_scene() -> SceneData {
    let mut text = Layer::new("TEXT BOT L".into(), LayerType::Video, 0);
    text.layer_id = lid("text-bot-l");
    text.clips
        .push(TimelineClip::new_video("EXILE".into(), Beats(0.0), Beats(24.0), Seconds::ZERO));
    text.clips
        .push(TimelineClip::new_video("RETURN".into(), Beats(24.0), Beats(24.0), Seconds::ZERO));

    // The subject: a video layer with a two-effect chain. Selected, so the
    // inspector shows its layer card + the Mirror and Bloom effect cards.
    let mut glow = Layer::new("GLOW".into(), LayerType::Video, 1);
    glow.layer_id = lid("glow");
    glow.clips
        .push(TimelineClip::new_video("glow_loop.mov".into(), Beats(0.0), Beats(48.0), Seconds::ZERO));
    let mut mirror = effect("Mirror");
    arm_lfo(&mut mirror);
    glow.effects = Some(vec![mirror, effect("Bloom")]);

    let mut plasma = Layer::new("PLASMA".into(), LayerType::Generator, 2);
    plasma.layer_id = lid("plasma");
    plasma.clips.push(TimelineClip::new_generator(Beats(0.0), Beats(48.0)));

    let mut project = Project::default();
    project.timeline.layers = vec![text, glow, plasma];

    let content = ContentState { current_beat: Beats(8.0), is_playing: false, ..Default::default() };

    let mut selection = UIState::default();
    selection.select_layer(lid("glow"));

    SceneData { project, content, active: Some(1), selection }
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
/// same `ParamCardConfig` the live editor's left lane builds from. `None` if
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
