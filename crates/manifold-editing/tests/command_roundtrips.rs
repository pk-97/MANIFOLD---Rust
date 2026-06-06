use manifold_core::LayerId;
use manifold_core::clip::TimelineClip;
use manifold_core::effects::*;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::types::*;
use manifold_core::units::Bpm;
use manifold_core::{Beats, PresetTypeId, Seconds};
use manifold_editing::command::Command;
use manifold_editing::commands::clip::*;
use manifold_editing::commands::drivers::*;
use manifold_editing::commands::effect_groups::*;
use manifold_editing::commands::effect_target::DriverTarget;
use manifold_editing::commands::effect_target::EffectTarget;
use manifold_editing::commands::effects::*;
use manifold_editing::commands::envelopes::*;
use manifold_editing::commands::layer::*;
use manifold_editing::commands::settings::*;

// Test-only inventory submissions — manifold-renderer isn't linked in editing tests.
use manifold_core::effect_registration::EffectMetadata;
use manifold_core::generator_registration::{GeneratorMetadata, ParamSpec};

// Step 16: ChangeEffectParamCommand resolves param_id → index via the
// effect registry on each execute/undo. Tests must register at least
// the effects they reference (Bloom is the only one used).
inventory::submit! {
    EffectMetadata {
        id: PresetTypeId::BLOOM,
        display_name: "Bloom",
        category: "Post-Process",
        available: true,
        osc_prefix: "bloom",
        legacy_discriminant: Some(12),
        params: &[
            ParamSpec::continuous("amount", "Amount", 0.0, 5.0, 0.187, "F2", ""),
            ParamSpec::continuous("threshold", "Threshold", 0.0, 5.0, 1.0, "F2", ""),
        ],
    }
}

inventory::submit! {
    GeneratorMetadata {
        id: PresetTypeId::PLASMA,
        display_name: "Plasma",
        is_line_based: false,
        available: true,
        osc_prefix: "plasma",
        legacy_discriminant: Some(6),
        params: &[
            ParamSpec::whole_labels("pattern", "Pattern", 0.0, 7.0, 0.0, &["Classic", "Rings", "Diamond", "Warp", "Cells", "Noise", "Fractal", "Lattice"], "pattern"),
            ParamSpec::continuous("complexity", "Complexity", 0.0, 1.0, 0.5, "F2", "complexity"),
            ParamSpec::continuous("contrast", "Contrast", 0.0, 1.0, 0.63, "F2", "contrast"),
            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
            ParamSpec::toggle("clip_trigger", "Clip Trigger", 0.0, 1.0, 1.0, "clipTrigger"),
        ],
        string_params: &[],
    }
}
inventory::submit! {
    GeneratorMetadata {
        id: PresetTypeId::TESSERACT,
        display_name: "Tesseract",
        is_line_based: true,
        available: true,
        osc_prefix: "tesseract",
        legacy_discriminant: Some(4),
        params: &[
            ParamSpec::continuous("xy", "XY", 0.0, 2.0, 0.6, "F2", "rotXY"),
            ParamSpec::continuous("zw", "ZW", 0.0, 2.0, 0.4, "F2", "rotZW"),
            ParamSpec::continuous("xw", "XW", 0.0, 2.0, 0.25, "F2", "rotXW"),
            ParamSpec::continuous("line", "Line", 0.0005, 0.03, 0.002, "F4", "line"),
            ParamSpec::continuous("dist", "Dist", 1.0, 6.0, 3.0, "F1", "dist"),
            ParamSpec::toggle("verts", "Verts", 0.0, 1.0, 1.0, "verts"),
            ParamSpec::continuous("v_size", "VSize", 0.1, 4.0, 1.0, "F1", "vsize"),
            ParamSpec::toggle("anim", "Anim", 0.0, 1.0, 0.0, "anim"),
            ParamSpec::continuous("speed", "Speed", 0.1, 5.0, 1.0, "F1", "speed"),
            ParamSpec::continuous("window", "Window", 0.01, 1.0, 0.1, "F2", "window"),
            ParamSpec::continuous("scale", "Scale", 0.25, 3.0, 1.0, "F2", "scale"),
        ],
        string_params: &[],
    }
}

fn fixture_path(name: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../tests/fixtures");
    p.push(name);
    p
}

fn load_project(name: &str) -> Project {
    let path = fixture_path(name);
    manifold_io::loader::load_project(&path)
        .unwrap_or_else(|e| panic!("Failed to load {name}: {e}"))
}

fn make_test_project() -> Project {
    let mut project = Project::default();
    project.settings.bpm = manifold_core::units::Bpm(120.0);
    project.settings.time_signature_numerator = 4;

    // Add 2 layers
    project
        .timeline
        .insert_layer(0, Layer::new("Layer 1".into(), LayerType::Video, 0));
    project
        .timeline
        .insert_layer(1, Layer::new("Layer 2".into(), LayerType::Generator, 1));

    // Add clips to layer 0
    let clip1 = TimelineClip {
        start_beat: Beats(0.0),
        duration_beats: Beats(4.0),
        ..Default::default()
    };
    let clip2 = TimelineClip {
        start_beat: Beats(4.0),
        duration_beats: Beats(4.0),
        ..Default::default()
    };
    project.timeline.layers[0].restore_clip(clip1);
    project.timeline.layers[0].restore_clip(clip2);

    project.timeline.rebuild_clip_lookup();
    project
}

// ─── Clip Commands ───

#[test]
fn swap_video_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let mut cmd = SwapVideoCommand::new(
        clip_id.clone(),
        "old_video".into(),
        "new_video".into(),
        Seconds(0.0),
        Seconds(1.5),
        Beats(4.0),
        Beats(8.0),
    );

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert_eq!(clip.video_clip_id, "new_video");
    assert!((clip.in_point - Seconds(1.5)).abs() < Seconds(0.001));
    assert!((clip.duration_beats - Beats(8.0)).abs() < Beats(0.001));

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert_eq!(clip.video_clip_id, "old_video");
    assert!((clip.in_point - Seconds(0.0)).abs() < Seconds(0.001));
    assert!((clip.duration_beats - Beats(4.0)).abs() < Beats(0.001));
}

#[test]
fn slip_clip_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let mut cmd = SlipClipCommand::new(clip_id.clone(), Seconds(0.0), Seconds(2.5));

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.in_point - Seconds(2.5)).abs() < Seconds(0.001));

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.in_point - Seconds(0.0)).abs() < Seconds(0.001));
}

#[test]
fn clip_effects_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let old = ClipEffectsSnapshot {
        is_looping: false,
        loop_duration_beats: Beats(0.0),
        translate_x: 0.0,
        translate_y: 0.0,
        scale: 1.0,
        rotation: 0.0,
    };
    let new = ClipEffectsSnapshot {
        is_looping: true,
        loop_duration_beats: Beats(2.0),
        translate_x: 0.5,
        translate_y: -0.3,
        scale: 2.0,
        rotation: 45.0,
    };

    let mut cmd = ClipEffectsCommand::new(clip_id.clone(), old, new);

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!(clip.is_looping);
    assert!((clip.scale - 2.0).abs() < 0.001);

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!(!clip.is_looping);
    assert!((clip.scale - 1.0).abs() < 0.001);
}

#[test]
fn change_clip_loop_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let mut cmd = ChangeClipLoopCommand::new(clip_id.clone(), false, true, Beats(0.0), Beats(2.0));

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!(clip.is_looping);
    assert!((clip.loop_duration_beats - Beats(2.0)).abs() < Beats(0.001));

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!(!clip.is_looping);
}

#[test]
fn change_clip_recorded_bpm_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();

    let mut cmd = ChangeClipRecordedBpmCommand::new(clip_id.clone(), 0.0, 130.0);

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.recorded_bpm - 130.0).abs() < 0.01);

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.recorded_bpm - 0.0).abs() < 0.01);
}

#[test]
fn split_clip_undo_roundtrip() {
    let mut project = make_test_project();
    let clip_id = project.timeline.layers[0].clips[0].id.clone();
    let initial_count = project.timeline.layers[0].clips.len();

    let mut tail = project.timeline.layers[0].clips[0].clone_with_new_id();
    tail.start_beat = Beats(2.0);
    tail.duration_beats = Beats(2.0);

    let layer_id = project.timeline.layers[0].layer_id.clone();
    let mut cmd = SplitClipCommand::new(clip_id.clone(), layer_id, Beats(4.0), Beats(2.0), tail);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers[0].clips.len(), initial_count + 1);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.duration_beats - Beats(2.0)).abs() < Beats(0.001));

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers[0].clips.len(), initial_count);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.duration_beats - Beats(4.0)).abs() < Beats(0.001));
}

// ─── Layer Commands ───

#[test]
fn add_layer_undo_roundtrip() {
    let mut project = make_test_project();
    let initial_count = project.timeline.layers.len();

    let mut cmd = AddLayerCommand::new(
        "New Layer".into(),
        LayerType::Video,
        PresetTypeId::NONE,
        0,
        None,
    );

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers.len(), initial_count + 1);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers.len(), initial_count);
}

#[test]
fn delete_layer_undo_roundtrip() {
    let mut project = make_test_project();
    let initial_count = project.timeline.layers.len();
    let layer = project.timeline.layers[0].clone();

    let mut cmd = DeleteLayerCommand::new(layer);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers.len(), initial_count - 1);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers.len(), initial_count);
}

#[test]
fn delete_group_clears_children_parent_ids() {
    let mut project = make_test_project();
    // Create a group with children
    let group = Layer::new("Group".into(), LayerType::Group, 0);
    let group_id = group.layer_id.clone();
    project.timeline.insert_layer(0, group);

    // Set children's parent
    for i in 1..project.timeline.layers.len() {
        project.timeline.layers[i].parent_layer_id = Some(group_id.clone());
    }
    let child_count = project.timeline.layers.len() - 1;

    let group_layer = project.timeline.layers[0].clone();
    let mut cmd = DeleteLayerCommand::new(group_layer);

    cmd.execute(&mut project);
    // Children should have parent cleared
    for layer in &project.timeline.layers {
        assert!(
            layer.parent_layer_id.is_none(),
            "child {} still has parent",
            layer.name
        );
    }

    cmd.undo(&mut project);
    // Group restored, children re-parented
    assert!(project.timeline.layers[0].is_group());
    let reparented = project
        .timeline
        .layers
        .iter()
        .filter(|l| l.parent_layer_id.as_ref() == Some(&group_id))
        .count();
    assert_eq!(reparented, child_count);
}

#[test]
fn reorder_layer_undo_roundtrip() {
    let mut project = make_test_project();
    let old_order = project.timeline.layers.clone();
    let old_names: Vec<String> = old_order.iter().map(|l| l.name.clone()).collect();

    let mut new_order = old_order.clone();
    new_order.reverse();

    let empty_map = std::collections::HashMap::new();
    let mut cmd = ReorderLayerCommand::new(old_order, new_order, empty_map.clone(), empty_map);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers[0].name, old_names[1]);
    assert_eq!(project.timeline.layers[1].name, old_names[0]);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers[0].name, old_names[0]);
    assert_eq!(project.timeline.layers[1].name, old_names[1]);
}

#[test]
fn group_layers_undo_roundtrip() {
    let mut project = make_test_project();
    let initial_count = project.timeline.layers.len();
    let layer_ids: Vec<LayerId> = project
        .timeline
        .layers
        .iter()
        .map(|l| l.layer_id.clone())
        .collect();
    let original_order = project.timeline.layers.clone();

    let mut cmd = GroupLayersCommand::new(layer_ids, original_order);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers.len(), initial_count + 1); // group added
    assert!(project.timeline.layers.iter().any(|l| l.is_group()));

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers.len(), initial_count);
    assert!(!project.timeline.layers.iter().any(|l| l.is_group()));
}

// ─── Settings Commands ───

#[test]
fn change_quantize_mode_undo_roundtrip() {
    let mut project = make_test_project();

    let mut cmd = ChangeQuantizeModeCommand::new(QuantizeMode::Off, QuantizeMode::Beat);

    cmd.execute(&mut project);
    assert_eq!(project.settings.quantize_mode, QuantizeMode::Beat);

    cmd.undo(&mut project);
    assert_eq!(project.settings.quantize_mode, QuantizeMode::Off);
}

#[test]
fn change_frame_rate_undo_roundtrip() {
    let mut project = make_test_project();

    let mut cmd = ChangeFrameRateCommand::new(60.0, 30.0);

    cmd.execute(&mut project);
    assert!((project.settings.frame_rate - 30.0).abs() < 0.01);

    cmd.undo(&mut project);
    assert!((project.settings.frame_rate - 60.0).abs() < 0.01);
}

#[test]
fn change_layer_midi_note_undo_roundtrip() {
    let mut project = make_test_project();

    let layer_id = project.timeline.layers[0].layer_id.clone();
    let mut cmd = ChangeLayerMidiNoteCommand::new(layer_id, -1, 60);

    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers[0].midi_note, 60);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers[0].midi_note, -1);
}

#[test]
fn change_layer_blend_mode_undo_roundtrip() {
    let mut project = make_test_project();

    let layer_id = project.timeline.layers[0].layer_id.clone();
    let mut cmd =
        ChangeLayerBlendModeCommand::new(layer_id, BlendMode::Normal, BlendMode::Additive);

    cmd.execute(&mut project);
    assert_eq!(
        project.timeline.layers[0].default_blend_mode,
        BlendMode::Additive
    );

    cmd.undo(&mut project);
    assert_eq!(
        project.timeline.layers[0].default_blend_mode,
        BlendMode::Normal
    );
}

#[test]
fn change_layer_opacity_undo_roundtrip() {
    let mut project = make_test_project();

    let layer_id = project.timeline.layers[0].layer_id.clone();
    let mut cmd = ChangeLayerOpacityCommand::new(layer_id, 1.0, 0.5);

    cmd.execute(&mut project);
    assert!((project.timeline.layers[0].opacity - 0.5).abs() < 0.001);

    cmd.undo(&mut project);
    assert!((project.timeline.layers[0].opacity - 1.0).abs() < 0.001);
}

#[test]
fn change_generator_type_undo_roundtrip() {
    let mut project = make_test_project();
    // Layer 1 is a generator layer
    let gp = project.timeline.layers[1].gen_params_or_init();
    gp.restore(PresetTypeId::PLASMA, vec![0.5, 0.8, 1.0], None, None);

    let old_params = project.timeline.layers[1].snapshot_gen_params();
    let old_drivers = project.timeline.layers[1].snapshot_gen_drivers();
    let old_envelopes = project.timeline.layers[1].snapshot_gen_envelopes();

    let layer_id = project.timeline.layers[1].layer_id.clone();
    let mut cmd = ChangeGeneratorTypeCommand::new(
        layer_id,
        PresetTypeId::PLASMA,
        PresetTypeId::TESSERACT,
        old_params,
        old_drivers,
        old_envelopes,
    );

    cmd.execute(&mut project);
    assert_eq!(
        *project.timeline.layers[1].generator_type(),
        PresetTypeId::TESSERACT
    );
    // After type change, params are filled with Tesseract's definition defaults (11 params)
    assert_eq!(
        project.timeline.layers[1].snapshot_gen_params().len(),
        manifold_core::preset_definition_registry::generator::get(&PresetTypeId::TESSERACT).param_count
    );

    cmd.undo(&mut project);
    assert_eq!(
        *project.timeline.layers[1].generator_type(),
        PresetTypeId::PLASMA
    );
    assert_eq!(project.timeline.layers[1].snapshot_gen_params().len(), 3);
}

/// Regression for the "Lissajous renders as wireframe / BasicShapes
/// shows a huge white blob after switching from another generator"
/// bug class. The renderer's per-frame override-version sweep was
/// rebuilding the new-type generator from the previous type's stale
/// `Layer::generator_graph` — so a Plasma → Tesseract type swap kept
/// rendering the Plasma graph, and a Wireframe → BasicShapes swap
/// rendered wireframe polyhedra with BasicShapes' four outer-card
/// values jammed into the wireframe bindings (rotate_x_speed=0.015,
/// line=1.0 → a massive white shape covering the screen).
///
/// `Layer::change_generator_type` now clears the per-layer graph
/// override and bumps `generator_graph_version`, and
/// `ChangeGeneratorTypeCommand` snapshots the cleared graph so undo
/// reinstates whatever the user had edited before the type swap.
#[test]
fn change_generator_type_clears_and_restores_graph_override() {
    use manifold_core::effect_graph_def::EffectGraphDef;

    let mut project = make_test_project();
    // Layer 1 is a generator layer in make_test_project.
    let gp = project.timeline.layers[1].gen_params_or_init();
    gp.restore(PresetTypeId::PLASMA, vec![0.5, 0.8, 1.0], None, None);

    // Plant a non-empty per-layer graph override + version, simulating
    // a user who edited the Plasma graph through the editor.
    let stale_graph = EffectGraphDef {
        version: 2,
        name: Some("stale-plasma".into()),
        description: None,
        preset_metadata: None,
        nodes: Vec::new(),
        wires: Vec::new(),
    };
    project.timeline.layers[1].generator_graph = Some(stale_graph.clone());
    project.timeline.layers[1].generator_graph_version = 7;

    let old_params = project.timeline.layers[1].snapshot_gen_params();
    let old_drivers = project.timeline.layers[1].snapshot_gen_drivers();
    let old_envelopes = project.timeline.layers[1].snapshot_gen_envelopes();
    let layer_id = project.timeline.layers[1].layer_id.clone();

    let mut cmd = ChangeGeneratorTypeCommand::new(
        layer_id,
        PresetTypeId::PLASMA,
        PresetTypeId::TESSERACT,
        old_params,
        old_drivers,
        old_envelopes,
    );

    cmd.execute(&mut project);

    // After execute: type swapped + graph override cleared + version
    // bumped so the renderer's per-frame sweep notices and rebuilds
    // against the new type's bundled JSON instead of the stale graph.
    assert_eq!(
        *project.timeline.layers[1].generator_type(),
        PresetTypeId::TESSERACT
    );
    assert!(
        project.timeline.layers[1].generator_graph.is_none(),
        "stale per-layer graph override must be cleared on type change \
         (otherwise the renderer keeps drawing the previous generator)",
    );
    assert_ne!(
        project.timeline.layers[1].generator_graph_version, 7,
        "generator_graph_version must bump on clear so the renderer's \
         per-frame override-version sweep rebuilds the generator",
    );

    cmd.undo(&mut project);

    // After undo: original graph + version-bump restored. The exact
    // version value isn't load-bearing (the renderer compares against
    // its own cached snapshot), but it must be different from the
    // post-execute value so the sweep detects the change.
    assert_eq!(
        *project.timeline.layers[1].generator_type(),
        PresetTypeId::PLASMA
    );
    assert_eq!(
        project.timeline.layers[1].generator_graph.as_ref().and_then(|g| g.name.as_deref()),
        Some("stale-plasma"),
        "undo must restore the pre-change graph override verbatim",
    );
}

#[test]
fn change_master_opacity_undo_roundtrip() {
    let mut project = make_test_project();

    let mut cmd = ChangeMasterOpacityCommand::new(1.0, 0.7);

    cmd.execute(&mut project);
    assert!((project.settings.master_opacity - 0.7).abs() < 0.001);

    cmd.undo(&mut project);
    assert!((project.settings.master_opacity - 1.0).abs() < 0.001);
}

#[test]
fn clear_tempo_map_undo_roundtrip() {
    let mut project = make_test_project();
    project
        .tempo_map
        .add_or_replace_point(Beats(0.0), Bpm(120.0), TempoPointSource::Manual, 0.001);
    project
        .tempo_map
        .add_or_replace_point(Beats(4.0), Bpm(140.0), TempoPointSource::Manual, 0.001);

    let old_points = project.tempo_map.clone_points();
    assert_eq!(old_points.len(), 2);

    let mut cmd = ClearTempoMapCommand::new(old_points, Bpm(120.0));

    cmd.execute(&mut project);
    assert_eq!(project.tempo_map.point_count(), 1); // just the beat-zero point

    cmd.undo(&mut project);
    assert_eq!(project.tempo_map.point_count(), 2);
}

#[test]
fn restore_tempo_lane_undo_roundtrip() {
    let mut project = make_test_project();
    project
        .tempo_map
        .add_or_replace_point(Beats(0.0), Bpm(120.0), TempoPointSource::Manual, 0.001);
    let old_points = project.tempo_map.clone_points();

    let new_points = vec![
        manifold_core::tempo::TempoPoint {
            beat: Beats(0.0),
            bpm: Bpm(130.0),
            source: TempoPointSource::Recorded,
            recorded_at_seconds: Seconds(0.0),
        },
        manifold_core::tempo::TempoPoint {
            beat: Beats(4.0),
            bpm: Bpm(140.0),
            source: TempoPointSource::Recorded,
            recorded_at_seconds: Seconds(2.0),
        },
    ];

    let mut cmd = RestoreRecordedTempoLaneCommand::new(Bpm(120.0), old_points, new_points);

    cmd.execute(&mut project);
    assert_eq!(project.tempo_map.point_count(), 2);
    assert!((project.settings.bpm.0 - 130.0).abs() < 0.01);

    cmd.undo(&mut project);
    assert_eq!(project.tempo_map.point_count(), 1);
    assert!((project.settings.bpm.0 - 120.0).abs() < 0.01);
}

// ─── Effect Commands ───

#[test]
fn add_effect_undo_roundtrip() {
    let mut project = make_test_project();
    let target = EffectTarget::Master;

    let mut effect = EffectInstance::new(PresetTypeId::BLOOM);
    effect.param_values = vec![ParamSlot::exposed(0.5)];

    let mut cmd = AddEffectCommand::new(target, effect, 0);

    cmd.execute(&mut project);
    assert_eq!(project.settings.master_effects.len(), 1);

    cmd.undo(&mut project);
    assert_eq!(project.settings.master_effects.len(), 0);
}

#[test]
fn remove_effect_undo_roundtrip() {
    let mut project = make_test_project();
    {
        let mut fx = EffectInstance::new(PresetTypeId::BLOOM);
        fx.param_values = vec![ParamSlot::exposed(0.5)];
        project.settings.master_effects.push(fx);
    }

    let effect = project.settings.master_effects[0].clone();
    let target = EffectTarget::Master;
    let mut cmd = RemoveEffectCommand::new(target, effect, 0);

    cmd.execute(&mut project);
    assert_eq!(project.settings.master_effects.len(), 0);

    cmd.undo(&mut project);
    assert_eq!(project.settings.master_effects.len(), 1);
}

#[test]
fn toggle_effect_undo_roundtrip() {
    let mut project = make_test_project();
    project
        .settings
        .master_effects
        .push(EffectInstance::new(PresetTypeId::BLOOM));

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ToggleEffectCommand::new(effect_id, true, false);

    cmd.execute(&mut project);
    assert!(!project.settings.master_effects[0].enabled);

    cmd.undo(&mut project);
    assert!(project.settings.master_effects[0].enabled);
}

#[test]
fn change_effect_param_undo_roundtrip() {
    let mut project = make_test_project();
    {
        let mut fx = EffectInstance::new(PresetTypeId::BLOOM);
        fx.param_values = vec![ParamSlot::exposed(0.5), ParamSlot::exposed(0.3)];
        fx.base_param_values = Some(vec![0.5, 0.3]);
        project.settings.master_effects.push(fx);
    }

    // Step 16: id-keyed addressing (was positional `0`).
    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ChangeGraphParamCommand::new(
        manifold_core::GraphTarget::Effect(effect_id.clone()),
        "amount",
        0.5,
        0.9,
    );

    cmd.execute(&mut project);
    assert!((project.settings.master_effects[0].param_values[0].value - 0.9).abs() < 0.001);

    cmd.undo(&mut project);
    assert!((project.settings.master_effects[0].param_values[0].value - 0.5).abs() < 0.001);

    // Targets `threshold` (index 1) — confirm id-based addressing
    // routes to the right slot, not just index 0.
    let mut cmd2 = ChangeGraphParamCommand::new(
        manifold_core::GraphTarget::Effect(effect_id),
        "threshold",
        0.3,
        0.7,
    );
    cmd2.execute(&mut project);
    assert!((project.settings.master_effects[0].param_values[1].value - 0.7).abs() < 0.001);
    cmd2.undo(&mut project);
    assert!((project.settings.master_effects[0].param_values[1].value - 0.3).abs() < 0.001);
}

#[test]
fn change_effect_param_unknown_id_is_no_op() {
    // An undo entry that targets a param id which has been dropped
    // from the schema since the entry was recorded must NOT panic
    // and must NOT scribble random indices. It silently no-ops.
    let mut project = make_test_project();
    let mut fx = EffectInstance::new(PresetTypeId::BLOOM);
    fx.param_values = vec![ParamSlot::exposed(0.5), ParamSlot::exposed(0.3)];
    fx.base_param_values = Some(vec![0.5, 0.3]);
    project.settings.master_effects.push(fx);

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ChangeGraphParamCommand::new(
        manifold_core::GraphTarget::Effect(effect_id),
        "phantom_param",
        0.5,
        0.9,
    );

    cmd.execute(&mut project);
    // Unchanged — no slot was matched.
    assert_eq!(
        project.settings.master_effects[0].param_values,
        vec![ParamSlot::exposed(0.5), ParamSlot::exposed(0.3)]
    );
    cmd.undo(&mut project);
    assert_eq!(
        project.settings.master_effects[0].param_values,
        vec![ParamSlot::exposed(0.5), ParamSlot::exposed(0.3)]
    );
}

#[test]
fn change_effect_param_undo_roundtrip_on_user_tail_binding() {
    // Regression: a user-exposed inner-node param lives at the tail of
    // `param_values` (past the static prefix) and is addressed by an
    // id like `user.<handle>.<param>.<n>`. The command must resolve
    // that through the *instance*'s `param_id_to_value_index`, which
    // consults both the static registry and the per-instance user
    // bindings. The earlier registry-only lookup returned `None` for
    // user-tail ids, making undo/redo a silent no-op for any slider
    // exposed from an inner node.
    let mut project = make_test_project();
    let mut fx = EffectInstance::new(PresetTypeId::BLOOM);
    fx.param_values = vec![ParamSlot::exposed(0.5), ParamSlot::exposed(0.3)];
    fx.base_param_values = Some(vec![0.5, 0.3]);
    fx.append_user_binding(UserParamBinding {
        id: "user.uv.translate.1".to_string(),
        label: "Translate".to_string(),
        node_id: manifold_core::NodeId::new("uv"), legacy_node_handle: None,
        inner_param: "translate".to_string(),
        min: -1.0,
        max: 1.0,
        default_value: 0.0,
        convert: ParamConvert::Float,
        is_angle: false,
        invert: false,
        curve: Default::default(),
        scale: 1.0,
        offset: 0.0,
    });
    project.settings.master_effects.push(fx);

    let user_id = "user.uv.translate.1";
    let tail_idx = project.settings.master_effects[0]
        .param_id_to_value_index(user_id)
        .expect("user binding resolves to a slot");
    assert_eq!(tail_idx, 2, "user binding lands past the 2-param static prefix");

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ChangeGraphParamCommand::new(
        manifold_core::GraphTarget::Effect(effect_id),
        user_id,
        0.0,
        0.42,
    );
    cmd.execute(&mut project);
    let v = project.settings.master_effects[0].param_values[tail_idx].value;
    assert!((v - 0.42).abs() < 0.001, "execute writes the user-tail slot");

    cmd.undo(&mut project);
    let v = project.settings.master_effects[0].param_values[tail_idx].value;
    assert!((v - 0.0).abs() < 0.001, "undo restores the user-tail slot");
}

#[test]
fn reorder_effect_undo_roundtrip() {
    let mut project = make_test_project();
    project
        .settings
        .master_effects
        .push(make_effect(&PresetTypeId::BLOOM));
    project
        .settings
        .master_effects
        .push(make_effect(&PresetTypeId::GLITCH));

    let target = EffectTarget::Master;
    // to_index uses pre-removal indexing: to=2 means "insert after the last element"
    // Unity: remove at 0 -> [Feedback], insertAt = 2-1 = 1, insert Bloom at 1 -> [Feedback, Bloom]
    let mut cmd = ReorderEffectCommand::new(target, 0, 2);

    cmd.execute(&mut project);
    assert_eq!(
        *project.settings.master_effects[0].effect_type(),
        PresetTypeId::GLITCH
    );
    assert_eq!(
        *project.settings.master_effects[1].effect_type(),
        PresetTypeId::BLOOM
    );

    cmd.undo(&mut project);
    assert_eq!(
        *project.settings.master_effects[0].effect_type(),
        PresetTypeId::BLOOM
    );
    assert_eq!(
        *project.settings.master_effects[1].effect_type(),
        PresetTypeId::GLITCH
    );
}

#[test]
fn effect_on_layer_undo_roundtrip() {
    let mut project = make_test_project();

    let effect = EffectInstance::new(PresetTypeId::MIRROR);
    let target = EffectTarget::Layer {
        layer_id: project.timeline.layers[0].layer_id.clone(),
    };
    let mut cmd = AddEffectCommand::new(target, effect, 0);

    cmd.execute(&mut project);
    assert_eq!(
        project.timeline.layers[0].effects.as_ref().unwrap().len(),
        1
    );

    cmd.undo(&mut project);
    assert_eq!(
        project.timeline.layers[0].effects.as_ref().unwrap().len(),
        0
    );
}

// ─── Effect Group Commands ───

#[test]
fn group_effects_undo_roundtrip() {
    let mut project = make_test_project();
    project
        .settings
        .master_effects
        .push(make_effect(&PresetTypeId::BLOOM));
    project
        .settings
        .master_effects
        .push(make_effect(&PresetTypeId::GLITCH));

    let target = EffectTarget::Master;
    let mut cmd = GroupEffectsCommand::new(target, vec![0, 1], "My Group".into());

    cmd.execute(&mut project);
    assert!(project.settings.master_effects[0].group_id.is_some());
    assert_eq!(
        project.settings.master_effects[0].group_id,
        project.settings.master_effects[1].group_id
    );
    assert_eq!(
        project
            .settings
            .master_effect_groups
            .as_ref()
            .unwrap()
            .len(),
        1
    );

    cmd.undo(&mut project);
    assert!(project.settings.master_effects[0].group_id.is_none());
    assert!(project.settings.master_effects[1].group_id.is_none());
    assert!(
        project
            .settings
            .master_effect_groups
            .as_ref()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ungroup_effects_undo_roundtrip() {
    let mut project = make_test_project();
    let group = EffectGroup::new("Test".into());
    let gid = group.id.clone();
    {
        let mut fx = EffectInstance::new(PresetTypeId::BLOOM);
        fx.group_id = Some(gid.clone());
        project.settings.master_effects.push(fx);
    }
    project.settings.master_effect_groups = Some(vec![group]);

    let target = EffectTarget::Master;
    let mut cmd = UngroupEffectsCommand::new(target, gid);

    cmd.execute(&mut project);
    assert!(project.settings.master_effects[0].group_id.is_none());
    assert!(
        project
            .settings
            .master_effect_groups
            .as_ref()
            .unwrap()
            .is_empty()
    );

    cmd.undo(&mut project);
    assert!(project.settings.master_effects[0].group_id.is_some());
    assert_eq!(
        project
            .settings
            .master_effect_groups
            .as_ref()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn toggle_group_undo_roundtrip() {
    let mut project = make_test_project();
    let group = EffectGroup::new("Test".into());
    let gid = group.id.clone();
    project.settings.master_effect_groups = Some(vec![group]);

    let target = EffectTarget::Master;
    let mut cmd = ToggleGroupCommand::new(target, gid, true, false);

    cmd.execute(&mut project);
    assert!(!project.settings.master_effect_groups.as_ref().unwrap()[0].enabled);

    cmd.undo(&mut project);
    assert!(project.settings.master_effect_groups.as_ref().unwrap()[0].enabled);
}

#[test]
fn rename_group_undo_roundtrip() {
    let mut project = make_test_project();
    let group = EffectGroup::new("Old Name".into());
    let gid = group.id.clone();
    project.settings.master_effect_groups = Some(vec![group]);

    let target = EffectTarget::Master;
    let mut cmd = RenameGroupCommand::new(target, gid, "Old Name".into(), "New Name".into());

    cmd.execute(&mut project);
    assert_eq!(
        project.settings.master_effect_groups.as_ref().unwrap()[0].name,
        "New Name"
    );

    cmd.undo(&mut project);
    assert_eq!(
        project.settings.master_effect_groups.as_ref().unwrap()[0].name,
        "Old Name"
    );
}

#[test]
fn change_group_wet_dry_undo_roundtrip() {
    let mut project = make_test_project();
    let group = EffectGroup::new("Test".into());
    let gid = group.id.clone();
    project.settings.master_effect_groups = Some(vec![group]);

    let target = EffectTarget::Master;
    let mut cmd = ChangeGroupWetDryCommand::new(target, gid, 1.0, 0.5);

    cmd.execute(&mut project);
    assert!(
        (project.settings.master_effect_groups.as_ref().unwrap()[0].wet_dry - 0.5).abs() < 0.001
    );

    cmd.undo(&mut project);
    assert!(
        (project.settings.master_effect_groups.as_ref().unwrap()[0].wet_dry - 1.0).abs() < 0.001
    );
}

// ─── Driver Commands ───

#[test]
fn add_driver_effect_undo_roundtrip() {
    let mut project = make_test_project();
    project
        .settings
        .master_effects
        .push(make_effect(&PresetTypeId::BLOOM));

    let effect_id = project.settings.master_effects[0].id.clone();
    let target = DriverTarget::Effect { effect_id };
    let driver = ParameterDriver {
        param_id: std::borrow::Cow::Borrowed("amount"),
        beat_division: BeatDivision::Quarter,
        waveform: DriverWaveform::Sine,
        enabled: true,
        phase: 0.0,
        base_value: 0.0,
        trim_min: 0.0,
        trim_max: 1.0,
        reversed: false,
        legacy_param_index: None,
        is_paused_by_user: false,
    };

    let mut cmd = AddDriverCommand::new(target, driver);

    cmd.execute(&mut project);
    assert_eq!(
        project.settings.master_effects[0]
            .drivers
            .as_ref()
            .unwrap()
            .len(),
        1
    );

    cmd.undo(&mut project);
    assert!(
        project.settings.master_effects[0]
            .drivers
            .as_ref()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn toggle_driver_enabled_undo_roundtrip() {
    let mut project = make_test_project();
    {
        let mut fx = EffectInstance::new(PresetTypeId::BLOOM);
        fx.drivers = Some(vec![ParameterDriver {
            param_id: std::borrow::Cow::Borrowed("amount"),
            enabled: true,
            ..make_driver()
        }]);
        project.settings.master_effects.push(fx);
    }

    let effect_id = project.settings.master_effects[0].id.clone();
    let target = DriverTarget::Effect { effect_id };
    let mut cmd = ToggleDriverEnabledCommand::new(target, 0, true, false);

    cmd.execute(&mut project);
    assert!(!project.settings.master_effects[0].drivers.as_ref().unwrap()[0].enabled);

    cmd.undo(&mut project);
    assert!(project.settings.master_effects[0].drivers.as_ref().unwrap()[0].enabled);
}

#[test]
fn change_driver_waveform_undo_roundtrip() {
    let mut project = make_test_project();
    {
        let mut fx = EffectInstance::new(PresetTypeId::BLOOM);
        fx.drivers = Some(vec![ParameterDriver {
            param_id: std::borrow::Cow::Borrowed("amount"),
            waveform: DriverWaveform::Sine,
            ..make_driver()
        }]);
        project.settings.master_effects.push(fx);
    }

    let effect_id = project.settings.master_effects[0].id.clone();
    let target = DriverTarget::Effect { effect_id };
    let mut cmd =
        ChangeDriverWaveformCommand::new(target, 0, DriverWaveform::Sine, DriverWaveform::Square);

    cmd.execute(&mut project);
    assert_eq!(
        project.settings.master_effects[0].drivers.as_ref().unwrap()[0].waveform,
        DriverWaveform::Square
    );

    cmd.undo(&mut project);
    assert_eq!(
        project.settings.master_effects[0].drivers.as_ref().unwrap()[0].waveform,
        DriverWaveform::Sine
    );
}

#[test]
fn change_trim_undo_roundtrip() {
    let mut project = make_test_project();
    {
        let mut fx = EffectInstance::new(PresetTypeId::BLOOM);
        fx.drivers = Some(vec![ParameterDriver {
            param_id: std::borrow::Cow::Borrowed("amount"),
            trim_min: 0.0,
            trim_max: 1.0,
            ..make_driver()
        }]);
        project.settings.master_effects.push(fx);
    }

    let effect_id = project.settings.master_effects[0].id.clone();
    let target = DriverTarget::Effect { effect_id };
    let mut cmd = ChangeTrimCommand::new(target, 0, 0.0, 1.0, 0.2, 0.8);

    cmd.execute(&mut project);
    let d = &project.settings.master_effects[0].drivers.as_ref().unwrap()[0];
    assert!((d.trim_min - 0.2).abs() < 0.001);
    assert!((d.trim_max - 0.8).abs() < 0.001);

    cmd.undo(&mut project);
    let d = &project.settings.master_effects[0].drivers.as_ref().unwrap()[0];
    assert!((d.trim_min - 0.0).abs() < 0.001);
    assert!((d.trim_max - 1.0).abs() < 0.001);
}

// ─── Envelope Commands ───

#[test]
fn add_layer_envelope_undo_roundtrip() {
    let mut project = make_test_project();

    let envelope = ParamEnvelope {
        target_effect_type: PresetTypeId::TRANSFORM,
        param_id: std::borrow::Cow::Borrowed("x"),
        enabled: true,
        ..make_envelope()
    };

    let layer_id = project.timeline.layers[0].layer_id.clone();
    let mut cmd = AddLayerEnvelopeCommand::new(layer_id, envelope);

    cmd.execute(&mut project);
    assert_eq!(
        project.timeline.layers[0].envelopes.as_ref().unwrap().len(),
        1
    );

    cmd.undo(&mut project);
    assert!(
        project.timeline.layers[0]
            .envelopes
            .as_ref()
            .unwrap()
            .is_empty()
    );
}

// ─── Test with real project fixtures ───

#[test]
fn commands_work_on_loaded_project() {
    let mut project = load_project("Burn V5.manifold");
    let clip_id = project.timeline.layers[0].clips[0].id.clone();
    let original_beat = project.timeline.layers[0].clips[0].start_beat;

    // Chain several commands
    let mut cmd1 = SlipClipCommand::new(clip_id.clone(), Seconds(0.0), Seconds(1.0));
    cmd1.execute(&mut project);
    assert!(
        (project.timeline.find_clip_by_id(&clip_id).unwrap().in_point - Seconds(1.0)).abs()
            < Seconds(0.001)
    );

    let mut cmd2 = ChangeClipLoopCommand::new(clip_id.clone(), false, true, Beats(0.0), Beats(2.0));
    cmd2.execute(&mut project);
    assert!(
        project
            .timeline
            .find_clip_by_id(&clip_id)
            .unwrap()
            .is_looping
    );

    // Undo both
    cmd2.undo(&mut project);
    assert!(
        !project
            .timeline
            .find_clip_by_id(&clip_id)
            .unwrap()
            .is_looping
    );

    cmd1.undo(&mut project);
    assert!(
        (project.timeline.find_clip_by_id(&clip_id).unwrap().in_point - Seconds(0.0)).abs()
            < Seconds(0.001)
    );
    assert!(
        (project
            .timeline
            .find_clip_by_id(&clip_id)
            .unwrap()
            .start_beat
            - original_beat)
            .abs()
            < Beats(0.001)
    );
}

fn make_effect(effect_type: &PresetTypeId) -> EffectInstance {
    EffectInstance::new(effect_type.clone())
}

fn make_driver() -> ParameterDriver {
    ParameterDriver {
        param_id: std::borrow::Cow::Borrowed("amount"),
        beat_division: BeatDivision::Quarter,
        waveform: DriverWaveform::Sine,
        enabled: true,
        phase: 0.0,
        base_value: 0.0,
        trim_min: 0.0,
        trim_max: 1.0,
        reversed: false,
        legacy_param_index: None,
        is_paused_by_user: false,
    }
}

fn make_envelope() -> ParamEnvelope {
    let mut env = ParamEnvelope::new_for_gen("x");
    env.attack_beats = 0.25;
    env.decay_beats = 0.25;
    env.sustain_level = 1.0;
    env.release_beats = 0.25;
    env
}

// ─── Undo-blind fix commands (invariant audit 2026-03-23) ───

#[test]
fn toggle_export_hdr_undo_roundtrip() {
    let mut project = make_test_project();
    assert!(!project.settings.export_hdr);

    let mut cmd = ToggleExportHdrCommand::new(false);
    cmd.execute(&mut project);
    assert!(project.settings.export_hdr);

    cmd.undo(&mut project);
    assert!(!project.settings.export_hdr);
}

#[test]
fn change_midi_channel_undo_roundtrip() {
    let mut project = make_test_project();
    let layer_id = project.timeline.layers[0].layer_id.clone();
    let old_channel = project.timeline.layers[0].midi_channel;

    let mut cmd = ChangeLayerMidiChannelCommand::new(layer_id, old_channel, 5);
    cmd.execute(&mut project);
    assert_eq!(project.timeline.layers[0].midi_channel, 5);

    cmd.undo(&mut project);
    assert_eq!(project.timeline.layers[0].midi_channel, old_channel);
}

#[test]
fn set_display_dimensions_undo_roundtrip() {
    let mut project = make_test_project();
    let old_w = project.settings.output_width;
    let old_h = project.settings.output_height;

    let mut cmd = SetDisplayDimensionsCommand::new(old_w, old_h, 3840, 2160);
    cmd.execute(&mut project);
    assert_eq!(project.settings.output_width, 3840);
    assert_eq!(project.settings.output_height, 2160);

    cmd.undo(&mut project);
    assert_eq!(project.settings.output_width, old_w);
    assert_eq!(project.settings.output_height, old_h);
}

#[test]
fn clear_percussion_undo_roundtrip() {
    let mut project = make_test_project();
    // Set up percussion state
    project.percussion_import = Some(manifold_core::percussion::PercussionImportState {
        audio_start_beat: Beats(4.0),
        audio_path: Some("/test/audio.wav".into()),
        ..Default::default()
    });

    let mut cmd = ClearPercussionCommand::new(None);
    cmd.execute(&mut project);
    assert!(project.percussion_import.is_none());

    cmd.undo(&mut project);
    assert!(project.percussion_import.is_some());
    assert_eq!(
        project.percussion_import.as_ref().unwrap().audio_start_beat,
        Beats(4.0)
    );
    assert_eq!(
        project
            .percussion_import
            .as_ref()
            .unwrap()
            .audio_path
            .as_deref(),
        Some("/test/audio.wav"),
    );
}

#[test]
fn reorder_effect_group_undo_roundtrip() {
    let mut project = make_test_project();
    // Add 3 master effects
    let fx_a = EffectInstance::new(PresetTypeId::BLOOM);
    let fx_b = EffectInstance::new(PresetTypeId::HALATION);
    let fx_c = EffectInstance::new(PresetTypeId::GLITCH);
    project.settings.master_effects = vec![fx_a.clone(), fx_b.clone(), fx_c.clone()];

    let old_effects = project.settings.master_effects.clone();
    // Reorder: move [Bloom, CRT, Glitch] → [CRT, Glitch, Bloom]
    let new_effects = vec![fx_b.clone(), fx_c.clone(), fx_a.clone()];

    let mut cmd =
        ReorderEffectGroupCommand::new(EffectTarget::Master, old_effects.clone(), new_effects);
    cmd.execute(&mut project);
    assert_eq!(
        *project.settings.master_effects[0].effect_type(),
        PresetTypeId::HALATION
    );
    assert_eq!(
        *project.settings.master_effects[1].effect_type(),
        PresetTypeId::GLITCH
    );
    assert_eq!(
        *project.settings.master_effects[2].effect_type(),
        PresetTypeId::BLOOM
    );

    cmd.undo(&mut project);
    assert_eq!(
        *project.settings.master_effects[0].effect_type(),
        PresetTypeId::BLOOM
    );
    assert_eq!(
        *project.settings.master_effects[1].effect_type(),
        PresetTypeId::HALATION
    );
    assert_eq!(
        *project.settings.master_effects[2].effect_type(),
        PresetTypeId::GLITCH
    );
}

// ─── Project load → cache verification ───

#[test]
fn project_load_verifies_caches() {
    // Default project with layers and clips
    let mut project = make_test_project();

    // Simulate deserialization: call on_after_deserialize
    project.on_after_deserialize();

    // Verify clip lookup cache is populated
    let clip_id = project.timeline.layers[0].clips[0].id.clone();
    assert!(
        project.timeline.find_clip_by_id(&clip_id).is_some(),
        "clip_lookup cache must be populated after on_after_deserialize",
    );

    // Verify layer index cache is populated
    let layer_id = project.timeline.layers[0].layer_id.clone();
    let layer_id = &layer_id;
    assert!(
        project.timeline.find_layer_index_by_id(layer_id).is_some(),
        "layer_id_to_index cache must be populated after on_after_deserialize",
    );

    // Verify layer indices are synced
    for (i, layer) in project.timeline.layers.iter().enumerate() {
        assert_eq!(
            layer.index, i as i32,
            "layer.index must match position after reindex"
        );
    }

    // clip.layer_id is now a legacy deserialization-only field — no longer synced
}

#[test]
fn project_load_strips_unknown_effects() {
    let mut project = make_test_project();
    // Add a valid and an unknown effect to master
    project
        .settings
        .master_effects
        .push(EffectInstance::new(PresetTypeId::BLOOM));
    project
        .settings
        .master_effects
        .push(EffectInstance::new(PresetTypeId::UNKNOWN));
    assert_eq!(project.settings.master_effects.len(), 2);

    project.strip_unknown_effects();
    assert_eq!(project.settings.master_effects.len(), 1);
    assert_eq!(
        *project.settings.master_effects[0].effect_type(),
        PresetTypeId::BLOOM
    );
}

// ─── ToggleEffectParamExposeCommand (Phase 3 Commit 3) ────────────

fn meta_default() -> InnerParamMeta {
    InnerParamMeta {
        label: "Translate".to_string(),
        min: -1.0,
        max: 1.0,
        default_value: 0.0,
        convert: ParamConvert::Float,
        is_angle: false,
    }
}

#[test]
fn expose_effect_param_command_undo_roundtrip() {
    // Build a project with a Bloom master effect that has 2 static
    // params (amount + threshold per the test inventory at the top
    // of this file). Expose UVTransform.translate as a user binding;
    // assert state, undo, assert state.
    let mut project = make_test_project();
    let mut fx = EffectInstance::new(PresetTypeId::BLOOM);
    fx.param_values = vec![ParamSlot::exposed(0.5), ParamSlot::exposed(1.0)];
    fx.base_param_values = Some(vec![0.5, 1.0]);
    project.settings.master_effects.push(fx);

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ToggleEffectParamExposeCommand::new(
        effect_id,
        manifold_core::NodeId::new("uv_transform"),
        "uv_transform".to_string(),
        "translate".to_string(),
        true, // expose
        meta_default(),
    );

    cmd.execute(&mut project);
    let fx = &project.settings.master_effects[0];
    let ub = fx.user_param_bindings();
    assert_eq!(ub.len(), 1);
    let binding = &ub[0];
    assert_eq!(binding.id, "user.uv_transform.translate.1");
    assert_eq!(binding.node_id, "uv_transform");
    assert_eq!(binding.inner_param, "translate");
    assert_eq!(binding.label, "Translate");
    // param_values: [0.5 (amount), 1.0 (threshold), 0.0 (user binding default)].
    assert_eq!(
        fx.param_values,
        vec![
            ParamSlot::exposed(0.5),
            ParamSlot::exposed(1.0),
            ParamSlot::exposed(0.0)
        ]
    );
    assert_eq!(fx.base_param_values.as_ref().unwrap(), &vec![0.5, 1.0, 0.0]);

    cmd.undo(&mut project);
    let fx = &project.settings.master_effects[0];
    assert!(fx.user_param_bindings().is_empty());
    assert_eq!(
        fx.param_values,
        vec![ParamSlot::exposed(0.5), ParamSlot::exposed(1.0)]
    );
    assert_eq!(fx.base_param_values.as_ref().unwrap(), &vec![0.5, 1.0]);

    // Re-execute (redo) yields the same binding id (deterministic
    // generator: binding list is empty after undo, so we land on
    // `.1` again).
    cmd.execute(&mut project);
    let fx = &project.settings.master_effects[0];
    let ub = fx.user_param_bindings();
    assert_eq!(ub.len(), 1);
    assert_eq!(ub[0].id, "user.uv_transform.translate.1");
}

#[test]
fn expose_already_exposed_is_idempotent_noop() {
    // Ticking an already-on checkbox is a no-op. The command must
    // not panic, and undo must also be a no-op (not remove the
    // pre-existing binding).
    let mut project = make_test_project();
    let mut fx = EffectInstance::new(PresetTypeId::BLOOM);
    fx.param_values = vec![ParamSlot::exposed(0.5), ParamSlot::exposed(1.0)];
    fx.base_param_values = Some(vec![0.5, 1.0]);
    fx.append_user_binding(UserParamBinding {
        id: "user.uv_transform.translate.1".to_string(),
        label: "Translate".to_string(),
        node_id: manifold_core::NodeId::new("uv_transform"), legacy_node_handle: None,
        inner_param: "translate".to_string(),
        min: -1.0,
        max: 1.0,
        default_value: 0.0,
        convert: ParamConvert::Float,
        is_angle: false,
        invert: false,
        curve: Default::default(),
        scale: 1.0,
        offset: 0.0,
    });
    project.settings.master_effects.push(fx);

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ToggleEffectParamExposeCommand::new(
        effect_id,
        manifold_core::NodeId::new("uv_transform"),
        "uv_transform".to_string(),
        "translate".to_string(),
        true,
        meta_default(),
    );
    cmd.execute(&mut project);
    // Still 1 binding (not 2 — execute is idempotent).
    assert_eq!(
        project.settings.master_effects[0].user_param_bindings().len(),
        1
    );
    cmd.undo(&mut project);
    // Pre-existing binding is preserved (the no-op execute recorded
    // ReverseState::None, so undo did nothing).
    assert_eq!(
        project.settings.master_effects[0].user_param_bindings().len(),
        1
    );
}

#[test]
fn unexpose_effect_param_command_undo_roundtrip() {
    let mut project = make_test_project();
    let mut fx = EffectInstance::new(PresetTypeId::BLOOM);
    fx.param_values = vec![ParamSlot::exposed(0.5), ParamSlot::exposed(1.0)];
    fx.base_param_values = Some(vec![0.5, 1.0]);
    fx.append_user_binding(UserParamBinding {
        id: "user.uv_transform.translate.1".to_string(),
        label: "Translate".to_string(),
        node_id: manifold_core::NodeId::new("uv_transform"), legacy_node_handle: None,
        inner_param: "translate".to_string(),
        min: -1.0,
        max: 1.0,
        default_value: 0.0,
        convert: ParamConvert::Float,
        is_angle: false,
        invert: false,
        curve: Default::default(),
        scale: 1.0,
        offset: 0.0,
    });
    // Drag the slider — user-tail at index 2 (n_static=2 + j=0) changed.
    fx.param_values[2].value = 0.42;
    fx.base_param_values.as_mut().unwrap()[2] = 0.42;
    project.settings.master_effects.push(fx);

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ToggleEffectParamExposeCommand::new(
        effect_id,
        manifold_core::NodeId::new("uv_transform"),
        "uv_transform".to_string(),
        "translate".to_string(),
        false, // unexpose
        meta_default(),
    );

    cmd.execute(&mut project);
    let fx = &project.settings.master_effects[0];
    assert!(fx.user_param_bindings().is_empty());
    assert_eq!(
        fx.param_values,
        vec![ParamSlot::exposed(0.5), ParamSlot::exposed(1.0)]
    );

    cmd.undo(&mut project);
    let fx = &project.settings.master_effects[0];
    let ub = fx.user_param_bindings();
    assert_eq!(ub.len(), 1);
    assert_eq!(ub[0].id, "user.uv_transform.translate.1");
    // Slot value restored — including the dragged 0.42, NOT the binding default.
    assert!((fx.param_values[2].value - 0.42).abs() < f32::EPSILON);
    assert!(
        (fx.base_param_values.as_ref().unwrap()[2] - 0.42).abs() < f32::EPSILON,
        "base value also restored"
    );
}

#[test]
fn unexpose_when_not_exposed_is_noop() {
    let mut project = make_test_project();
    let mut fx = EffectInstance::new(PresetTypeId::BLOOM);
    fx.param_values = vec![ParamSlot::exposed(0.5), ParamSlot::exposed(1.0)];
    project.settings.master_effects.push(fx);

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ToggleEffectParamExposeCommand::new(
        effect_id,
        manifold_core::NodeId::new("uv_transform"),
        "uv_transform".to_string(),
        "translate".to_string(),
        false,
        meta_default(),
    );
    cmd.execute(&mut project);
    cmd.undo(&mut project);
    assert!(
        project.settings.master_effects[0]
            .user_param_bindings()
            .is_empty()
    );
    assert_eq!(
        project.settings.master_effects[0].param_values,
        vec![ParamSlot::exposed(0.5), ParamSlot::exposed(1.0)]
    );
}

#[test]
fn generate_user_param_id_collision_probe() {
    // Linear probe lands on the smallest free .n suffix.
    let existing = [
        UserParamBinding {
            id: "user.uv_transform.translate.1".to_string(),
            label: String::new(),
            node_id: manifold_core::NodeId::new("uv_transform"), legacy_node_handle: None,
            inner_param: "translate".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            convert: ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
        },
        UserParamBinding {
            id: "user.uv_transform.translate.2".to_string(),
            label: String::new(),
            node_id: manifold_core::NodeId::new("uv_transform"), legacy_node_handle: None,
            inner_param: "translate".to_string(),
            min: 0.0,
            max: 1.0,
            default_value: 0.0,
            convert: ParamConvert::Float,
            is_angle: false,
            invert: false,
            curve: Default::default(),
            scale: 1.0,
            offset: 0.0,
        },
    ];
    let existing_ids: Vec<String> = existing.iter().map(|b| b.id.clone()).collect();
    let id = generate_user_param_id("uv_transform", "translate", &existing_ids);
    assert_eq!(id, "user.uv_transform.translate.3");
    // Different inner param under same handle gets a fresh prefix.
    let id2 = generate_user_param_id("uv_transform", "scale", &existing_ids);
    assert_eq!(id2, "user.uv_transform.scale.1");
}

/// Regression for the post-Phase-5 follow-up: un-exposing a
/// user-bound param must prune any drivers / Ableton mappings /
/// layer envelopes that referenced that binding's `param_id`. The
/// old behaviour left them sitting on the data model forever — never
/// matched by `find_driver` / the modulation evaluators / the Ableton
/// router, never applied, never editable. The fix captures them on
/// the reverse state so re-exposing restores every modulation
/// surface verbatim.
#[test]
fn unexpose_prunes_orphan_drivers_and_undo_restores_them() {
    let mut project = make_test_project();
    let mut fx = EffectInstance::new(PresetTypeId::BLOOM);
    fx.param_values = vec![ParamSlot::exposed(0.5), ParamSlot::exposed(1.0)];
    fx.append_user_binding(UserParamBinding {
        id: "user.uv_transform.translate.1".to_string(),
        label: "Translate".to_string(),
        node_id: manifold_core::NodeId::new("uv_transform"), legacy_node_handle: None,
        inner_param: "translate".to_string(),
        min: -1.0,
        max: 1.0,
        default_value: 0.0,
        convert: ParamConvert::Float,
        is_angle: false,
        invert: false,
        curve: Default::default(),
        scale: 1.0,
        offset: 0.0,
    });
    // Attach a driver keyed to the user binding's id. Plus a driver
    // for the static `amount` param — that one must survive the
    // un-expose untouched.
    fx.drivers = Some(vec![
        ParameterDriver {
            param_id: std::borrow::Cow::Owned("user.uv_transform.translate.1".to_string()),
            beat_division: BeatDivision::Quarter,
            waveform: DriverWaveform::Sine,
            enabled: true,
            phase: 0.0,
            base_value: 0.0,
            trim_min: 0.0,
            trim_max: 1.0,
            reversed: false,
            legacy_param_index: None,
            is_paused_by_user: false,
        },
        ParameterDriver {
            param_id: std::borrow::Cow::Borrowed("amount"),
            beat_division: BeatDivision::Eighth,
            waveform: DriverWaveform::Triangle,
            enabled: true,
            phase: 0.0,
            base_value: 0.0,
            trim_min: 0.0,
            trim_max: 1.0,
            reversed: false,
            legacy_param_index: None,
            is_paused_by_user: false,
        },
    ]);
    project.settings.master_effects.push(fx);

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ToggleEffectParamExposeCommand::new(
        effect_id,
        manifold_core::NodeId::new("uv_transform"),
        "uv_transform".to_string(),
        "translate".to_string(),
        false, // unexpose
        meta_default(),
    );
    cmd.execute(&mut project);

    let fx = &project.settings.master_effects[0];
    let drivers = fx.drivers.as_ref().expect("drivers still present");
    assert_eq!(
        drivers.len(),
        1,
        "the orphan driver targeting the unexposed binding must be pruned",
    );
    assert_eq!(
        drivers[0].param_id, "amount",
        "the static-param driver must survive untouched",
    );

    cmd.undo(&mut project);
    let fx = &project.settings.master_effects[0];
    let drivers = fx.drivers.as_ref().expect("drivers vec restored");
    assert_eq!(drivers.len(), 2, "undo must restore the pruned driver");
    assert!(
        drivers
            .iter()
            .any(|d| d.param_id == "user.uv_transform.translate.1"),
        "the previously pruned driver must come back with its original param_id",
    );
    // Re-execute: prune + restore is idempotent across redo.
    cmd.execute(&mut project);
    let drivers = &project.settings.master_effects[0]
        .drivers
        .as_ref()
        .expect("drivers vec retained for static row");
    assert_eq!(drivers.len(), 1);
}

#[test]
fn unexpose_prunes_orphan_ableton_mappings_and_undo_restores_them() {
    use manifold_core::ableton_mapping::{
        AbletonDeviceIdentity, AbletonMacroAddress, AbletonMappingStatus, AbletonParamMapping,
    };
    let mut project = make_test_project();
    let mut fx = EffectInstance::new(PresetTypeId::BLOOM);
    fx.param_values = vec![ParamSlot::exposed(0.5), ParamSlot::exposed(1.0)];
    fx.append_user_binding(UserParamBinding {
        id: "user.uv_transform.translate.1".to_string(),
        label: "Translate".to_string(),
        node_id: manifold_core::NodeId::new("uv_transform"), legacy_node_handle: None,
        inner_param: "translate".to_string(),
        min: -1.0,
        max: 1.0,
        default_value: 0.0,
        convert: ParamConvert::Float,
        is_angle: false,
        invert: false,
        curve: Default::default(),
        scale: 1.0,
        offset: 0.0,
    });
    let address = AbletonMacroAddress {
        track_id: 0,
        device_id: 0,
        param_id: 0,
        device_identity: AbletonDeviceIdentity {
            device_class_name: "InstrumentGroupDevice".to_string(),
        },
        track_name: "Bass".into(),
        device_name: "Rack".into(),
        macro_name: "Macro 1".into(),
    };
    fx.ableton_mappings = Some(vec![AbletonParamMapping {
        param_id: std::borrow::Cow::Owned("user.uv_transform.translate.1".to_string()),
        address: address.clone(),
        range_min: 0.0,
        range_max: 1.0,
        inverted: false,
        legacy_param_index: None,
        last_value: 0.0,
        status: AbletonMappingStatus::Active,
    }]);
    project.settings.master_effects.push(fx);

    let effect_id = project.settings.master_effects[0].id.clone();
    let mut cmd = ToggleEffectParamExposeCommand::new(
        effect_id,
        manifold_core::NodeId::new("uv_transform"),
        "uv_transform".to_string(),
        "translate".to_string(),
        false,
        meta_default(),
    );
    cmd.execute(&mut project);
    let fx = &project.settings.master_effects[0];
    assert!(
        fx.ableton_mappings.is_none(),
        "the only mapping targeted the unexposed binding — vec should collapse to None",
    );

    cmd.undo(&mut project);
    let mappings = project.settings.master_effects[0]
        .ableton_mappings
        .as_ref()
        .expect("ableton mappings restored");
    assert_eq!(mappings.len(), 1);
    assert_eq!(
        mappings[0].param_id,
        "user.uv_transform.translate.1",
        "undo must reinstate the pruned mapping with its original param_id",
    );
}

#[test]
fn unexpose_prunes_orphan_layer_envelopes_and_undo_restores_them() {
    let mut project = make_test_project();
    let mut fx = EffectInstance::new(PresetTypeId::BLOOM);
    fx.param_values = vec![ParamSlot::exposed(0.5), ParamSlot::exposed(1.0)];
    fx.append_user_binding(UserParamBinding {
        id: "user.uv_transform.translate.1".to_string(),
        label: "Translate".to_string(),
        node_id: manifold_core::NodeId::new("uv_transform"), legacy_node_handle: None,
        inner_param: "translate".to_string(),
        min: -1.0,
        max: 1.0,
        default_value: 0.0,
        convert: ParamConvert::Float,
        is_angle: false,
        invert: false,
        curve: Default::default(),
        scale: 1.0,
        offset: 0.0,
    });
    // Layer envelopes are keyed by (target_effect_type, param_id).
    // Plant one targeting our binding and one targeting an unrelated
    // param on a different effect type so the second survives.
    let envs = project.timeline.layers[0].envelopes_mut();
    envs.push(ParamEnvelope::new_for_effect(
        PresetTypeId::BLOOM,
        std::borrow::Cow::Owned("user.uv_transform.translate.1".to_string()),
    ));
    envs.push(ParamEnvelope::new_for_effect(
        PresetTypeId::MIRROR,
        std::borrow::Cow::Borrowed("amount"),
    ));
    project.timeline.layers[0].effects = Some(vec![fx]);

    let effect_id = project.timeline.layers[0].effects.as_ref().unwrap()[0]
        .id
        .clone();
    let mut cmd = ToggleEffectParamExposeCommand::new(
        effect_id,
        manifold_core::NodeId::new("uv_transform"),
        "uv_transform".to_string(),
        "translate".to_string(),
        false,
        meta_default(),
    );
    cmd.execute(&mut project);
    let envs = project.timeline.layers[0]
        .envelopes
        .as_ref()
        .expect("layer envelopes vec retained");
    assert_eq!(
        envs.len(),
        1,
        "the orphan envelope must be pruned; the unrelated one survives",
    );
    assert_eq!(envs[0].param_id, "amount");

    cmd.undo(&mut project);
    let envs = project.timeline.layers[0]
        .envelopes
        .as_ref()
        .expect("layer envelopes vec restored");
    assert_eq!(envs.len(), 2);
    assert!(
        envs.iter().any(|e| e.target_effect_type == PresetTypeId::BLOOM
            && e.param_id == "user.uv_transform.translate.1"),
        "undo must reinstate the pruned envelope",
    );
}
