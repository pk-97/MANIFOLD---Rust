//! Small, deterministic `Project` fixtures driven through the REAL core→UI
//! translation path (`state_sync::sync_project_data`/`push_state`), so a
//! snapshot is what the app actually draws — not hand-built panel structs.
//! The `timeline` scene reproduces the redesign mockup: text / video /
//! generator / group+children / audio. See `docs/HEADLESS_UI_HARNESS.md` §2.

use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::{Beats, LayerId, Seconds};
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
        _ => None,
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

    let mut selection = UIState::default();
    selection.select_layer(lid("plasma"));

    SceneData { project, content, active: Some(2), selection }
}
