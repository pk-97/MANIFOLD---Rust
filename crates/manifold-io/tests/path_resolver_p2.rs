//! P2 gate — PathResolver extension (PROJECT_FOLDERS_DESIGN.md section 4 P2).
//!
//! A temp-dir project whose audio clip and flagged GLB string param reference
//! now-broken absolute paths must re-link after the files are moved into a
//! `Media/` folder sitting beside the project folder (the filename+size search
//! fallback in `PathResolver::resolve_all` finds them there). Runs through the
//! real save → load pipeline so the audio relative-sibling field and the
//! `TimelineClip.string_params` write-back both survive the full round trip.

use manifold_core::clip::TimelineClip;
use manifold_core::effect_graph_def::{EffectGraphDef, PresetMetadata, StringParamSpecDef};
use manifold_core::layer::Layer;
use manifold_core::preset_def::PresetKind;
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::project::{EmbeddedOrigin, EmbeddedPreset, Project};
use manifold_core::units::{Beats, Seconds};
use manifold_io::{loader, saver};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("manifold_p2_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A project with one audio clip and one generator layer carrying a flagged
/// `model_file` string param, both pointing at the (now-broken) original
/// absolute locations.
fn build_project(broken_audio: &Path, broken_glb: &Path) -> Project {
    let mut project = Project::default();
    project.project_name = "P2 Relink".to_string();

    // Audio layer — the clip's path is broken, no relative sibling yet.
    let mut audio_layer = Layer::new_audio("Audio".into(), 0);
    audio_layer.clips.push(TimelineClip::new_audio(
        broken_audio.to_string_lossy().to_string(),
        Beats::ZERO,
        Beats::from_f32(4.0),
        Seconds::ZERO,
        Seconds::ZERO,
    ));
    project.timeline.layers.push(audio_layer);

    // Generator layer tracking an embedded preset whose `model_file` param is
    // flagged `is_file_path` (D5). The per-clip override carries the broken GLB
    // path — the home `resolve_all` writes the re-linked path back into.
    let meta = PresetMetadata {
        id: PresetTypeId::new("p2_glb"),
        display_name: "P2 GLB".to_string(),
        category: "Geometry".to_string(),
        osc_prefix: "p2_glb".to_string(),
        legacy_discriminant: None,
        available: true,
        is_line_based: false,
        params: Vec::new(),
        bindings: Vec::new(),
        param_aliases: Vec::new(),
        value_aliases: Vec::new(),
        string_params: vec![StringParamSpecDef {
            id: "model_file".to_string(),
            name: "Model File".to_string(),
            default_value: broken_glb.to_string_lossy().to_string(),
            is_file_picker: false,
            use_dropdown: false,
            is_file_path: true,
        }],
        string_bindings: Vec::new(),
        scene_bounds: None,
    };
    let embedded = EmbeddedPreset {
        kind: PresetKind::Generator,
        def: EffectGraphDef {
            version: 1,
            name: Some("P2 GLB".to_string()),
            description: None,
            preset_metadata: Some(meta),
            nodes: Vec::new(),
            wires: Vec::new(),
        },
        origin: EmbeddedOrigin::Saved,
    };
    project.upsert_embedded_preset(embedded);

    let mut gen_layer = Layer::new_generator("GLB".into(), PresetTypeId::new("p2_glb"), 1);
    let mut gen_clip = TimelineClip::new_generator(Beats::ZERO, Beats::from_f32(8.0));
    let mut params = BTreeMap::new();
    params.insert("model_file".to_string(), broken_glb.to_string_lossy().to_string());
    gen_clip.string_params = Some(params);
    gen_layer.clips.push(gen_clip);
    project.timeline.layers.push(gen_layer);

    project
}

#[test]
fn moved_audio_and_glb_relink_through_media_sibling() {
    let root = temp_root("relink");
    let project_dir = root.join("show");
    let media_dir = root.join("Media"); // sibling of the project folder
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::create_dir_all(&media_dir).unwrap();

    // The "moved" files land in Media/.
    std::fs::write(media_dir.join("loop.wav"), b"RIFF\x00\x00\x00\x00WAVE").unwrap();
    std::fs::write(media_dir.join("model.glb"), b"glTF\x02\x00\x00\x00").unwrap();

    // The project still references the ORIGINAL locations, now broken.
    let broken_audio = PathBuf::from("/nonexistent/original/loop.wav");
    let broken_glb = PathBuf::from("/nonexistent/original/model.glb");
    let mut project = build_project(&broken_audio, &broken_glb);

    let project_path = project_dir.join("show.manifold");
    saver::save_project(&mut project, &project_path, None, false).unwrap();
    assert!(project_path.exists(), "save must write the project file");

    let loaded = loader::load_project(&project_path).expect("reload the saved project");

    // Audio re-linked into Media/ and its relative sibling filled.
    let audio_layer = loaded
        .timeline
        .layers
        .iter()
        .find(|l| l.name == "Audio")
        .expect("audio layer survives the round trip");
    let audio_clip = &audio_layer.clips[0];
    assert_ne!(audio_clip.audio_file_path, broken_audio.to_string_lossy());
    let audio_path = Path::new(&audio_clip.audio_file_path);
    assert!(audio_path.exists(), "audio re-linked to a live file: {audio_path:?}");
    assert_eq!(audio_path.file_name().unwrap(), "loop.wav");
    assert!(
        audio_clip.relative_audio_file_path.is_some(),
        "audio relative sibling must be filled on re-link"
    );

    // GLB string param re-linked into Media/ and written back into the clip's
    // string_params.
    let gen_layer = loaded
        .timeline
        .layers
        .iter()
        .find(|l| l.name == "GLB")
        .expect("generator layer survives the round trip");
    let gen_clip = &gen_layer.clips[0];
    let glb = gen_clip
        .string_params
        .as_ref()
        .and_then(|m| m.get("model_file"))
        .expect("model_file override survives the round trip");
    assert_ne!(glb, &broken_glb.to_string_lossy());
    let glb_path = Path::new(glb);
    assert!(glb_path.exists(), "GLB re-linked to a live file: {glb_path:?}");
    assert_eq!(glb_path.file_name().unwrap(), "model.glb");
}
