//! BUG-256 regression — a project swap must be a hard boundary for every
//! renderer's id-keyed state.
//!
//! The bug: `GeneratorRenderer` keeps generator instances in
//! `layer_generators` keyed by `LayerId` and gates rebuilds on the
//! project's serialized `graph_version` / `graph_structure_version`
//! counters. Both the ids AND the counters collide across two projects
//! derived from the same template (same layers, same edit depth, different
//! graph JSON), so after loading project B the renderer kept serving
//! project A's generator instances — the app looked "locked to the
//! first-loaded project". `ClipRenderer::release_all` existed for exactly
//! this but had no callers.
//!
//! The fix: `PlaybackEngine::initialize` stops all clips and calls
//! `release_all` on every renderer before installing the new project.
//! These tests pin the contract at the engine boundary using the real
//! `PlaybackEngine` + `StubRenderer`, with project B carrying the SAME
//! `LayerId`/`ClipId` as project A (the template-derived worst case).

use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::{Beats, Bpm, ClipId, PresetTypeId, Seconds};
use manifold_playback::engine::PlaybackEngine;
use manifold_playback::renderer::{ClipRenderer, StubRenderer};

fn create_engine() -> PlaybackEngine {
    let renderers: Vec<Box<dyn ClipRenderer>> = vec![Box::new(StubRenderer::new_generator())];
    PlaybackEngine::new(renderers)
}

/// One generator layer with one clip at beat 0..4.
fn template_project() -> Project {
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);
    let mut layer = Layer::new_generator("Gen".into(), PresetTypeId::new("TestGen"), 0);
    let mut clip = TimelineClip::new_generator(Beats(0.0), Beats(4.0));
    clip.layer_id = layer.layer_id.clone();
    layer.clips.push(clip);
    project.timeline.layers.push(layer);
    project
}

#[test]
fn initialize_releases_renderer_state_from_the_previous_project() {
    let project_a = template_project();
    let clip_id: ClipId = project_a.timeline.layers[0].clips[0].id.clone();

    let mut engine = create_engine();
    engine.initialize(project_a.clone());
    engine.start_clip(&project_a.timeline.layers[0].clips[0], Seconds(0.0), 0);
    assert_eq!(engine.active_clip_count(), 1);
    assert!(engine.renderers_mut()[0].is_active(clip_id.as_str()));

    // Project B: same template — SAME LayerId and ClipId, different content
    // (here: renamed layer + different tempo standing in for a different
    // embedded graph; the id collision is what matters).
    let mut project_b = project_a;
    project_b.timeline.layers[0].name = "Gen (edited)".into();
    project_b.settings.bpm = Bpm(97.0);
    engine.initialize(project_b);

    assert_eq!(
        engine.active_clip_count(),
        0,
        "engine-side id-keyed clip state must not survive a project swap"
    );
    assert!(
        !engine.renderers_mut()[0].is_active(clip_id.as_str()),
        "renderer caches keyed by project-local ids must be released at the \
         project boundary (BUG-256)"
    );
}

#[test]
fn initialize_releases_on_every_swap_not_just_the_first() {
    let project = template_project();
    let clip_id: ClipId = project.timeline.layers[0].clips[0].id.clone();

    let mut engine = create_engine();
    for _ in 0..3 {
        engine.initialize(project.clone());
        engine.start_clip(&project.timeline.layers[0].clips[0], Seconds(0.0), 0);
        assert_eq!(engine.active_clip_count(), 1);
        engine.initialize(project.clone());
        assert!(
            !engine.renderers_mut()[0].is_active(clip_id.as_str()),
            "every project boundary must release renderer state, not just the first"
        );
    }
}
