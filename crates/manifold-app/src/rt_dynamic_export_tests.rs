//! P7b production-export acceptance probes for dynamic scene modifiers.
//!
//! These tests deliberately drive [`ContentThread::run_export`] through the
//! existing headless construction. They assert encoder-visible invariants and
//! consume the production pre-encode observer without introducing a second
//! renderer or an export loop. The fixture proves a real RT Surface-Waves
//! deformation and a real cut/remap topology transition at export frame 6.
#![cfg(all(test, feature = "journey-proofs", target_os = "macos"))]

use std::path::{Path, PathBuf};

use crossbeam_channel::unbounded;
use manifold_core::clip::TimelineClip;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::effects::{ParamId, ParameterDriver};
use manifold_core::project::Project;
use manifold_core::{BeatDivision, Beats, Bpm, DriverWaveform, PresetTypeId};
use manifold_media::export_config::ExportConfig;

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::headless_harness::headless_content_thread;

/// Tiny generated scene copied from the renderer's RT current-frame proof.
/// The normal-wave stage is the same primitive used by SurfaceWaves; the
/// render_scene node keeps the production RT path active for export.
const RT_SCENE_JSON: &str = r#"{"version":3,"name":"RtDynamicExport","presetMetadata":{
"id":"RtDynamicExport","displayName":"RT Dynamic Export","category":"Geometry",
"oscPrefix":"rt_dynamic_export","params":[
{"id":"phase","name":"Phase","min":-100.0,"max":100.0,"defaultValue":0.0},
{"id":"cell_size","name":"Cell Size","min":0.000001,"max":1000.0,"defaultValue":0.1}],
"bindings":[
{"id":"phase","label":"Phase","defaultValue":0.0,"target":{"kind":"node","nodeId":"wave","param":"phase"}},
{"id":"cell_size","label":"Cell Size","defaultValue":0.1,"target":{"kind":"node","nodeId":"cut","param":"cell_size"}}]},"nodes":[
{"id":0,"typeId":"system.generator_input","nodeId":"input"},
{"id":1,"typeId":"node.grid_mesh","nodeId":"grid","params":{
"max_capacity":{"type":"Int","value":16},"resolution_x":{"type":"Int","value":2},
"resolution_y":{"type":"Int","value":2},"size_x":{"type":"Float","value":2.0},
"size_y":{"type":"Float","value":2.0}}},
{"id":2,"typeId":"node.make_triangles","nodeId":"triangles","params":{
"src_cols":{"type":"Int","value":2},"src_rows":{"type":"Int","value":2}}},
{"id":3,"typeId":"node.normal_wave_mesh","nodeId":"wave","params":{
"amplitude":{"type":"Float","value":0.2},"frequency":{"type":"Float","value":1.5},
"phase":{"type":"Float","value":0.0},"pitch":{"type":"Float","value":1.0}},
"exposedParams":["phase"]},
{"id":8,"typeId":"node.cut_mesh_cells","nodeId":"cut","params":{
"cell_size":{"type":"Float","value":0.1},"scale":{"type":"Float","value":1.0}},
"exposedParams":["cell_size"]},
{"id":9,"typeId":"node.remap_mesh_cut","nodeId":"remap"},
{"id":4,"typeId":"node.phong_material","nodeId":"material","params":{
"color_r":{"type":"Float","value":1.0},"color_g":{"type":"Float","value":1.0},
"color_b":{"type":"Float","value":1.0},"ambient":{"type":"Float","value":0.05}}},
{"id":5,"typeId":"node.scene_object","nodeId":"object"},
{"id":6,"typeId":"node.orbit_camera","nodeId":"camera","params":{
"orbit":{"type":"Float","value":0.7},"tilt":{"type":"Float","value":0.95},
"distance":{"type":"Float","value":6.0},"fov_y":{"type":"Float","value":0.8}}},
{"id":7,"typeId":"node.light","nodeId":"sun","params":{
"mode":{"type":"Enum","value":0},"pos_y":{"type":"Float","value":10.0},
"aim_y":{"type":"Float","value":0.0},"color_r":{"type":"Float","value":1.0},
"color_g":{"type":"Float","value":1.0},"color_b":{"type":"Float","value":1.0},
"intensity":{"type":"Float","value":1.0},"cast_shadows":{"type":"Float","value":1.0}}},
{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
"objects":{"type":"Int","value":1},"lights":{"type":"Int","value":1},
"rt_enabled":{"type":"Bool","value":true}}},
{"id":99,"typeId":"system.final_output","nodeId":"out"}],"wires":[
{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
{"fromNode":2,"fromPort":"out","toNode":3,"toPort":"in"},
{"fromNode":2,"fromPort":"out","toNode":8,"toPort":"reference"},
{"fromNode":3,"fromPort":"out","toNode":9,"toPort":"in"},
{"fromNode":8,"fromPort":"map","toNode":9,"toPort":"map"},
{"fromNode":9,"fromPort":"out","toNode":5,"toPort":"vertices"},
{"fromNode":4,"fromPort":"out","toNode":5,"toPort":"material"},
{"fromNode":5,"fromPort":"object","toNode":20,"toPort":"object_0"},
{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"camera"},
{"fromNode":7,"fromPort":"out","toNode":20,"toPort":"light_0"},
{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}]}"#;

const BPM: f32 = 120.0;
const FPS: f32 = 12.0;
const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
const TWO_BEATS: f64 = 2.0;

struct ExportObservation {
    output: Option<PathBuf>,
    states: Vec<ContentState>,
    frames: Vec<crate::content_export::ExportFrameObservation>,
    gpu_abort_requested: bool,
}

fn output_dir(name: &str) -> PathBuf {
    let path = PathBuf::from("target/journey-proofs").join(name);
    std::fs::create_dir_all(&path).expect("create RT export acceptance directory");
    path
}

fn fixture_project() -> Project {
    let mut project = Project::default();
    project.settings.bpm = Bpm(BPM);
    project.settings.output_width = WIDTH as i32;
    project.settings.output_height = HEIGHT as i32;

    let graph: EffectGraphDef = serde_json::from_str(RT_SCENE_JSON).expect("RT fixture parses");
    let mut layer = manifold_core::layer::Layer::new_generator(
        "RT Export Surface Waves".into(),
        PresetTypeId::new("PhotoscanBaseline"),
        0,
    );
    layer.gen_params_or_init().graph = Some(graph);
    layer.gen_params_or_init().refresh_manifest_from_graph();
    let metadata = layer
        .gen_params_or_init()
        .graph_def()
        .as_ref()
        .and_then(|graph| graph.preset_metadata.as_ref())
        .expect("RT fixture exposes graph metadata");
    assert!(
        metadata
            .bindings
            .iter()
            .any(|binding| binding.id == "phase"),
        "RT phase must be an exposed production binding"
    );
    assert!(
        metadata
            .bindings
            .iter()
            .any(|binding| binding.id == "cell_size"),
        "RT cut size must be an exposed production binding"
    );
    layer
        .gen_params_or_init()
        .drivers_mut()
        .push(ParameterDriver::new(
            ParamId::from("phase"),
            BeatDivision::Quarter,
            DriverWaveform::Sine,
        ));
    let mut topology_driver = ParameterDriver::new(
        ParamId::from("cell_size"),
        BeatDivision::Whole,
        DriverWaveform::Square,
    );
    // One whole period is two beats = twelve 12-fps frames at 120 BPM;
    // Square therefore changes the cut map exactly at export frame 6.
    topology_driver.free_period_beats = Some(2.0);
    topology_driver.trim_min = 0.0001;
    topology_driver.trim_max = 0.0008;
    layer
        .gen_params_or_init()
        .drivers_mut()
        .push(topology_driver);
    layer
        .clips
        .push(TimelineClip::new_generator(Beats::ZERO, Beats(TWO_BEATS)));
    project.timeline.layers.push(layer);
    project
}

fn section_fixture() -> Project {
    let mut project = fixture_project();
    project
        .timeline
        .add_marker(manifold_core::marker::TimelineMarker::new(Beats(1.0)).with_name("Middle"));
    project
}

fn config(path: &Path, width: u32, height: u32, hdr: bool, split: bool) -> ExportConfig {
    ExportConfig {
        output_path: path.to_string_lossy().into_owned(),
        width,
        height,
        fps: FPS,
        hdr,
        start_beat: 0.0,
        end_beat: TWO_BEATS,
        audio_path: None,
        audio_start_beat: 0.0,
        audio_encoder_delay: 0.0,
        split_at_markers: split,
    }
}

fn run_export(
    project: Project,
    cfg: ExportConfig,
    cancel_before_frame: bool,
    fail_before_encode_frame: Option<(u32, crate::content_export::ExportTestFault)>,
) -> ExportObservation {
    let faults_before = manifold_gpu::gpu_fault::fault_count();
    let mut content = headless_content_thread(project, cfg.width, cfg.height);
    let (cmd_tx, cmd_rx) = unbounded();
    let (state_tx, state_rx) = unbounded();
    // Match app/project-load and export-repro preparation. Async imported
    // sources must be ready before entering the one-evaluation-per-frame
    // export loop; the observer deliberately excludes this preparation.
    crate::scene_modifier_journey::warm_project(&mut content, &state_tx);
    let (observation_tx, observation_rx) = unbounded();
    let _observer =
        crate::content_export::install_export_observer(observation_tx, fail_before_encode_frame);
    if cancel_before_frame {
        cmd_tx
            .send(ContentCommand::CancelExport)
            .expect("cancel command channel remains connected");
    }

    content.run_export(cfg, &cmd_rx, &state_tx);
    assert_eq!(
        manifold_gpu::gpu_fault::fault_count(),
        faults_before,
        "synthetic export failures must never fault hardware"
    );
    drop(cmd_tx);
    drop(state_tx);
    let states: Vec<ContentState> = state_rx.try_iter().collect();
    let output = states
        .iter()
        .find_map(|state| state.export_finished.as_ref())
        .filter(|event| event.success)
        .map(|event| PathBuf::from(&event.output_path));
    let frames = observation_rx.try_iter().collect();
    ExportObservation {
        output,
        states,
        frames,
        gpu_abort_requested: crate::content_export::export_test_gpu_abort_requested(),
    }
}

fn ffprobe() -> String {
    std::env::var("FFPROBE_PATH").unwrap_or_else(|_| {
        [
            "/opt/homebrew/bin/ffprobe",
            "/usr/local/bin/ffprobe",
            "/usr/bin/ffprobe",
        ]
        .iter()
        .find(|candidate| Path::new(candidate).exists())
        .unwrap_or_else(|| panic!("ffprobe is required for RT export acceptance"))
        .to_string()
    })
}

fn ffmpeg() -> String {
    std::env::var("FFMPEG_PATH").unwrap_or_else(|_| {
        [
            "/opt/homebrew/bin/ffmpeg",
            "/usr/local/bin/ffmpeg",
            "/usr/bin/ffmpeg",
        ]
        .iter()
        .find(|candidate| Path::new(candidate).exists())
        .unwrap_or_else(|| panic!("ffmpeg is required for RT export parity checks"))
        .to_string()
    })
}

fn video_shape(path: &Path) -> (u32, u32, usize) {
    let output = std::process::Command::new(ffprobe())
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_frames",
            "-show_entries",
            "stream=width,height,nb_read_frames",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("spawn ffprobe");
    assert!(
        output.status.success(),
        "ffprobe failed for {}",
        path.display()
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let fields: Vec<&str> = stdout.trim().split(',').collect();
    assert_eq!(fields.len(), 3, "ffprobe shape output: {:?}", fields);
    (
        fields[0].parse().expect("ffprobe width"),
        fields[1].parse().expect("ffprobe height"),
        fields[2].parse().expect("ffprobe frame count"),
    )
}

fn frame_timestamps(path: &Path) -> Vec<f64> {
    let output = std::process::Command::new(ffprobe())
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "frame=best_effort_timestamp_time",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .expect("spawn ffprobe timestamp probe");
    assert!(
        output.status.success(),
        "ffprobe timestamps failed for {}",
        path.display()
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("ffprobe JSON timestamps");
    value["frames"]
        .as_array()
        .expect("ffprobe frames")
        .iter()
        .map(|frame| {
            frame["best_effort_timestamp_time"]
                .as_str()
                .expect("every frame has a timestamp")
                .parse()
                .expect("numeric frame timestamp")
        })
        .collect()
}

fn decoded_frame_hashes(path: &Path) -> Vec<String> {
    let output = std::process::Command::new(ffmpeg())
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-map", "0:v:0", "-f", "framemd5", "-"])
        .output()
        .expect("spawn ffmpeg frame hash probe");
    assert!(
        output.status.success(),
        "ffmpeg frame hash probe failed for {}",
        path.display()
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            line.rsplit(',')
                .next()
                .expect("framemd5 checksum")
                .trim()
                .to_owned()
        })
        .collect()
}

fn assert_video(path: &Path, width: u32, height: u32, frames: usize) {
    assert!(path.exists(), "export did not produce {}", path.display());
    assert_eq!(video_shape(path), (width, height, frames));
    let timestamps = frame_timestamps(path);
    assert_eq!(
        timestamps.len(),
        frames,
        "every encoded frame needs a timestamp"
    );
    assert!(timestamps[0].abs() < 0.01, "frame 0 must start at t=0");
    for (index, pair) in timestamps.windows(2).enumerate() {
        assert!(
            pair[1] > pair[0],
            "timestamps must increase at pair {index}"
        );
        assert!((pair[1] - pair[0] - (1.0 / FPS as f64)).abs() < 0.01);
    }
}

#[test]
fn rt_dynamic_export_first_frame_and_state_steps() {
    let dir = output_dir("rt_dynamic_export_first_frame_and_state_steps");
    let path = dir.join("first-frame.mp4");
    let observation = run_export(
        fixture_project(),
        config(&path, WIDTH, HEIGHT, false, false),
        false,
        None,
    );
    let produced = observation
        .output
        .expect("production export should succeed");
    assert_eq!(produced, path);
    assert_video(&path, WIDTH, HEIGHT, 12);
    assert!(
        observation
            .states
            .iter()
            .any(|state| state.export_finished.is_some())
    );
    assert!(observation.states.iter().any(|state| state.is_exporting));
    assert_eq!(observation.frames.len(), 12);
    for (index, frame) in observation.frames.iter().enumerate() {
        assert_eq!(frame.frame_idx as usize, index);
        assert!(
            (frame.beat - index as f64 / 6.0).abs() < 1e-5,
            "frame {index}: {frame:?}"
        );
        assert!(
            frame
                .generator_values
                .iter()
                .any(|(id, _)| id == "cell_size")
        );
        assert!((frame.time_seconds - index as f64 / FPS as f64).abs() < 1e-9);
        let expected_dt = if index == 0 { 0.0 } else { 1.0 / FPS as f64 };
        assert!((frame.dt_seconds - expected_dt).abs() < 1e-9);
        assert_eq!(
            frame.status,
            manifold_renderer::frame_status::FrameRenderStatus::Complete
        );
    }
    assert!(
        observation.frames[0].rt_dispatches > 0,
        "frame 0 must dispatch RT"
    );
    assert!(
        observation
            .frames
            .iter()
            .skip(1)
            .any(|frame| { frame.rt_updates.blas_refits > 0 || frame.rt_updates.blas_builds > 0 }),
        "automated Surface-Waves phase must move geometry after frame 0"
    );
    assert_eq!(
        observation.frames[5].rt_updates.blas_builds, 0,
        "stable cut map refits deformation"
    );
    assert!(
        observation.frames[1..6]
            .iter()
            .any(|frame| frame.rt_updates.blas_refits > 0),
        "phase changes refit before the topology transition; equal adjacent sine samples may reuse"
    );
    assert!(
        observation.frames[6].rt_updates.blas_builds > 0,
        "frame 6 must rebuild RT BLAS after the automated cut-map topology change: {:#?}",
        observation.frames
    );
    let hashes = decoded_frame_hashes(&path);
    assert_eq!(hashes.len(), 12, "one decoded hash per export frame");
    assert!(
        hashes.windows(2).any(|pair| pair[0] != pair[1]),
        "automated phase must change encoded frame pixels"
    );
}

#[test]
fn rt_dynamic_export_repeat_and_sections() {
    let dir = output_dir("rt_dynamic_export_repeat_and_sections");
    let source = fixture_project();
    let saved = dir.join("fixture.manifold");
    manifold_io::saver::save_project_v1(&source, &saved).expect("save fixture project");
    let reloaded = manifold_io::loader::load_project(&saved).expect("reload fixture project");

    let first = dir.join("repeat-a.mp4");
    let second = dir.join("repeat-b.mp4");
    let first_observation = run_export(
        reloaded.clone(),
        config(&first, WIDTH, HEIGHT, false, false),
        false,
        None,
    );
    first_observation
        .output
        .as_ref()
        .expect("first repeat export");
    let second_observation = run_export(
        reloaded,
        config(&second, WIDTH, HEIGHT, false, false),
        false,
        None,
    );
    second_observation
        .output
        .as_ref()
        .expect("second repeat export");
    assert_video(&first, WIDTH, HEIGHT, 12);
    assert_video(&second, WIDTH, HEIGHT, 12);
    assert_eq!(frame_timestamps(&first), frame_timestamps(&second));
    assert_eq!(
        decoded_frame_hashes(&first),
        decoded_frame_hashes(&second),
        "repeat exports must decode to identical frame pixels"
    );
    let frame_witness = |frame: &crate::content_export::ExportFrameObservation| {
        (
            frame.frame_idx,
            frame.time_seconds.to_bits(),
            frame.dt_seconds.to_bits(),
            frame.status,
            frame.rt_updates,
            frame.rt_dispatches,
            frame.history_resets,
        )
    };
    assert_eq!(
        first_observation
            .frames
            .iter()
            .map(frame_witness)
            .collect::<Vec<_>>(),
        second_observation
            .frames
            .iter()
            .map(frame_witness)
            .collect::<Vec<_>>(),
        "repeat exports must have identical numerical frame witnesses"
    );

    let base = dir.join("sections.mp4");
    let section_observation = run_export(
        section_fixture(),
        config(&base, WIDTH, HEIGHT, false, true),
        false,
        None,
    );
    section_observation.output.as_ref().expect("section export");
    let first_section = dir.join("sections--section-1.mp4");
    let named_section = dir.join("sections--Middle.mp4");
    assert_video(&first_section, WIDTH, HEIGHT, 6);
    assert_video(&named_section, WIDTH, HEIGHT, 6);
    assert_eq!(section_observation.frames.len(), 12);
    assert_eq!(section_observation.frames[0].frame_idx, 0);
    assert_eq!(section_observation.frames[6].frame_idx, 0);
    assert_eq!(section_observation.frames[0].dt_seconds, 0.0);
    assert_eq!(section_observation.frames[6].dt_seconds, 0.0);
    assert!(
        !base.exists(),
        "split export must not write the unsuffixed file"
    );
}

#[test]
fn rt_dynamic_export_fault_before_encode() {
    use crate::content_export::ExportTestFault;
    let dir = output_dir("rt_dynamic_export_fault_before_encode");
    for fault in [
        ExportTestFault::BeforeEncode,
        ExportTestFault::PendingGeometry,
        ExportTestFault::Preparation,
        ExportTestFault::Encode,
        ExportTestFault::GpuFault,
        ExportTestFault::IgnoredSubmission,
        ExportTestFault::CompletionTimeout,
    ] {
        let path = dir.join(format!("fault-{fault:?}.mp4"));
        let cfg = config(&path, WIDTH, HEIGHT, false, false);
        let observation = run_export(fixture_project(), cfg, false, Some((0, fault)));
        assert!(observation.output.is_none());
        assert_eq!(
            observation.gpu_abort_requested,
            matches!(
                fault,
                ExportTestFault::GpuFault
                    | ExportTestFault::IgnoredSubmission
                    | ExportTestFault::CompletionTimeout
            )
        );
        assert!(observation.states.iter().any(|state| {
            state
                .export_finished
                .as_ref()
                .is_some_and(|event| !event.success)
        }));
        assert!(
            !path.exists(),
            "a pre-encode failure must not create an output file"
        );
        assert_eq!(
            observation.frames.len(),
            usize::from(fault == ExportTestFault::BeforeEncode),
            "{fault:?}: injection must stop before frame 1 and before native encoding"
        );
    }
}

fn imported_modifiers_content(recipes: &[&str]) -> crate::content_thread::ContentThread {
    let source = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/gltf/cc0___mushroom.glb"
    ));
    let (graph, _) = manifold_renderer::node_graph::gltf_import::assemble_import_graph(source)
        .expect("import SceneLoop host");
    let mut project = Project::default();
    project.settings.bpm = Bpm(BPM);
    project.settings.output_width = WIDTH as i32;
    project.settings.output_height = HEIGHT as i32;
    let mut layer = manifold_core::layer::Layer::new_generator(
        "RT SceneLoop export".into(),
        PresetTypeId::new("PhotoscanBaseline"),
        0,
    );
    layer.gen_params_or_init().graph = Some(graph);
    layer.gen_params_or_init().refresh_manifest_from_graph();
    let id = layer.layer_id.clone();
    let mut clip = TimelineClip::new_generator(Beats::ZERO, Beats(TWO_BEATS));
    clip.layer_id = id.clone();
    layer.clips.push(clip);
    project.timeline.layers.push(layer);
    let mut content = headless_content_thread(project, WIDTH, HEIGHT);
    for recipe in recipes {
        content.handle_command(ContentCommand::SceneModifier(
            crate::scene_modifier_edit::SceneModifierAction::Add(id.clone(), (*recipe).into()),
        ));
        assert!(
            content.graph_edit_diagnostic.is_none(),
            "{recipe} attachment rejected"
        );
    }
    let target = manifold_core::GraphTarget::Generator(id);
    let (param, old) = {
        let instance = content
            .engine
            .project()
            .unwrap()
            .preset_instance(&target)
            .unwrap();
        let param = instance
            .params
            .iter()
            .find(|param| param.id().ends_with("rt_enabled"))
            .expect("imported RT binding")
            .id()
            .to_owned();
        let old = instance.get_base_param(&param);
        (param, old)
    };
    content.handle_command(ContentCommand::Execute(Box::new(
        manifold_editing::commands::effects::ChangeGraphParamCommand::new(target, param, old, 1.0),
    )));
    assert!(content.graph_edit_diagnostic.is_none());
    content
}

#[test]
fn rt_dynamic_export_saved_modifier_stack() {
    let dir = output_dir("rt_dynamic_export_saved_modifier_stack");
    let mut content =
        imported_modifiers_content(&["SurfaceWaves", "OrderedRecon", "SpatialEchoes"]);
    let id = content.engine.project().unwrap().timeline.layers[0]
        .layer_id
        .clone();
    let ids = |project: &Project| {
        project.timeline.layers[0]
            .gen_params()
            .unwrap()
            .graph_def()
            .as_ref()
            .unwrap()
            .scene_modifiers
            .iter()
            .map(|modifier| modifier.id.clone())
            .collect::<Vec<_>>()
    };
    let original = ids(content.engine.project().unwrap());
    content.handle_command(ContentCommand::SceneModifier(
        crate::scene_modifier_edit::SceneModifierAction::Move(id, original[0].clone(), 1),
    ));
    assert!(content.graph_edit_diagnostic.is_none());
    let reordered = vec![
        original[1].clone(),
        original[0].clone(),
        original[2].clone(),
    ];
    assert_eq!(ids(content.engine.project().unwrap()), reordered);
    content.handle_command(ContentCommand::Undo);
    assert_eq!(ids(content.engine.project().unwrap()), original);
    content.handle_command(ContentCommand::Redo);
    assert_eq!(ids(content.engine.project().unwrap()), reordered);
    let saved = dir.join("stack.manifold");
    manifold_io::saver::save_project_v1(content.engine.project().unwrap(), &saved).unwrap();
    let reopened = manifold_io::loader::load_project(&saved).unwrap();
    assert_eq!(ids(&reopened), reordered);
    drop(content);
    let output = dir.join("stack.mp4");
    let observation = run_export(
        reopened,
        config(&output, WIDTH, HEIGHT, false, false),
        false,
        None,
    );
    assert!(observation.output.is_some());
    assert_video(&output, WIDTH, HEIGHT, 12);
    assert!(
        observation
            .frames
            .iter()
            .all(|frame| frame.rt_dispatches > 0)
    );
}

#[test]
fn rt_dynamic_export_stateful_scene_loop() {
    let dir = output_dir("rt_dynamic_export_stateful_scene_loop");
    let mut content = imported_modifiers_content(&["SceneLoop"]);
    let (target, param, old) =
        {
            let project = content.engine.project().unwrap();
            let target =
                manifold_core::GraphTarget::Generator(project.timeline.layers[0].layer_id.clone());
            let instance = project.preset_instance(&target).unwrap();
            let param = instance.graph_def().as_ref().unwrap().preset_metadata.as_ref().unwrap()
            .bindings.iter().find(|binding| matches!(&binding.target,
                manifold_core::effect_graph_def::BindingTarget::SceneModifier { param_id, .. }
                    if param_id.as_str() == "bars"
            )).expect("promoted SceneLoop bars binding").id.clone();
            let old = instance.get_base_param(&param);
            (target, param, old)
        };
    // Cross cell boundaries within the two-beat fixture; the default eight
    // bars can move the camera without changing its resident instance window.
    content.handle_command(ContentCommand::Execute(Box::new(
        manifold_editing::commands::effects::ChangeGraphParamCommand::new(target, param, old, 0.25),
    )));
    let project = content.engine.project().unwrap().clone();
    drop(content);
    let mut witnesses = Vec::new();
    for index in 0..2 {
        let path = dir.join(format!("stateful-{index}.mp4"));
        let observation = run_export(
            project.clone(),
            config(&path, WIDTH, HEIGHT, false, false),
            false,
            None,
        );
        assert!(
            observation.output.is_some(),
            "stateful export {index} failed: {:?}",
            observation
                .states
                .iter()
                .filter_map(|state| state.export_finished.as_ref())
                .collect::<Vec<_>>()
        );
        assert_video(&path, WIDTH, HEIGHT, 12);
        assert_eq!(observation.frames.len(), 12);
        assert_eq!(observation.frames[0].dt_seconds, 0.0);
        assert!(
            observation
                .frames
                .iter()
                .all(|frame| frame.rt_dispatches > 0)
        );
        assert!(
            observation.frames[1..]
                .iter()
                .all(|frame| (frame.dt_seconds - 1.0 / FPS as f64).abs() < 1e-9)
        );
        assert!(
            observation.frames[1..]
                .iter()
                .any(|frame| frame.rt_updates.tlas_refits > 0),
            "SceneLoop must move instance bounds"
        );
        witnesses.push(decoded_frame_hashes(&path));
    }
    assert_eq!(
        witnesses[0], witnesses[1],
        "stateful repeat must restart from the same initial state"
    );
}

#[test]
fn rt_dynamic_export_cancel_resize_hdr() {
    let dir = output_dir("rt_dynamic_export_cancel_resize_hdr");
    let cancelled = dir.join("cancelled.mp4");
    let observation = run_export(
        fixture_project(),
        config(&cancelled, WIDTH, HEIGHT, false, false),
        true,
        None,
    );
    assert!(observation.output.is_none());
    assert!(
        !cancelled.exists(),
        "cancelled export must clean its partial output"
    );

    let resized = dir.join("resized.mp4");
    run_export(
        fixture_project(),
        config(&resized, 640, 360, false, false),
        false,
        None,
    )
    .output
    .expect("resized export");
    assert_video(&resized, 640, 360, 12);

    let hdr = dir.join("hdr.mp4");
    let mut hdr_cfg = config(&hdr, WIDTH, HEIGHT, true, false);
    hdr_cfg.start_beat = 0.0;
    hdr_cfg.end_beat = 1.0 / 3.0;
    run_export(fixture_project(), hdr_cfg, false, None)
        .output
        .expect("HDR export");
    assert_eq!(video_shape(&hdr), (WIDTH, HEIGHT, 2));
}
