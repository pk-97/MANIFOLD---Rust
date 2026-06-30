//! Small, deterministic `Project` fixtures driven through the REAL core→UI
//! translation path (`state_sync::sync_project_data`/`push_state`), so a
//! snapshot is what the app actually draws — not hand-built panel structs.
//! The `timeline` scene reproduces the redesign mockup: text / video /
//! generator / group+children / audio. See `docs/HEADLESS_UI_HARNESS.md` §2.

use manifold_core::clip::TimelineClip;
use manifold_core::effects::PresetInstance;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::{Beats, LayerId, PresetTypeId, Seconds};
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

    // 2: PLASMA — generator, the SELECTED layer.
    let mut plasma = Layer::new("PLASMA".into(), LayerType::Generator, 2);
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
