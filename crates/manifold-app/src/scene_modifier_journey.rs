//! Native F8 release journey for playing photoscan modifiers.
//!
//! This deliberately stays a single, bounded observation: one real imported
//! photoscan, two Surface Peel instances, one live LFO, structural
//! gestures, save/reopen, and a pair of post-reopen output captures. The test
//! uses the same headless `ContentThread` and native Metal output as the app.

use std::path::{Path, PathBuf};

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::effects::{ParamId, ParameterDriver};
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_core::{
    Beats, Bpm, LayerId, NodeId, PresetTypeId,
    cold_touch::{ColdTouchKind, cold_touch_count, reset_cold_touch_counts},
};
use manifold_renderer::headless_readback::{encode_rgba8_png, linear_to_srgb8};
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::headless_harness::headless_content_thread;
use crate::scene_modifier_edit::SceneModifierAction;

const MUSHROOM_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/cc0___mushroom.glb"
);
const STATIC_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/cc0__japanese_thistle_cirsium_japonicum.glb"
);

fn imported_layer(name: &str, layer_id: &str, index: i32, path: &Path) -> Layer {
    let (graph, report) = assemble_import_graph(path)
        .unwrap_or_else(|error| panic!("{} import failed: {error}", path.display()));
    assert!(
        report.object_count > 0,
        "{} has no imported objects",
        path.display()
    );
    let mut layer =
        Layer::new_generator(name.into(), PresetTypeId::new("PhotoscanBaseline"), index);
    layer.layer_id = LayerId::new(layer_id);
    layer.gen_params_or_init().graph = Some(graph);
    layer.gen_params_or_init().refresh_manifest_from_graph();
    let mut clip = manifold_core::clip::TimelineClip::new_generator(Beats(0.0), Beats(16.0));
    clip.layer_id = layer.layer_id.clone();
    layer.clips.push(clip);
    layer
}

fn journey_project() -> Project {
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);
    project.settings.output_width = 320;
    project.settings.output_height = 180;
    project.timeline.layers.push(imported_layer(
        "Mushroom",
        "journey-mushroom",
        0,
        Path::new(MUSHROOM_FIXTURE),
    ));
    project
}

fn generator_graph<'a>(project: &'a Project, layer_id: &LayerId) -> &'a EffectGraphDef {
    project
        .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
        .and_then(|owner| owner.graph_def().as_ref())
        .expect("journey generator graph")
}

fn modifier_ids(project: &Project, layer_id: &LayerId) -> Vec<NodeId> {
    generator_graph(project, layer_id)
        .scene_modifiers
        .iter()
        .map(|modifier| modifier.id.clone())
        .collect()
}

fn host_binding(
    project: &Project,
    layer_id: &LayerId,
    modifier_id: &NodeId,
    local_param: &str,
) -> String {
    generator_graph(project, layer_id)
        .preset_metadata
        .as_ref()
        .expect("journey host metadata")
        .bindings
        .iter()
        .find_map(|binding| {
            matches!(
                &binding.target,
                BindingTarget::SceneModifier { modifier_id: id, param_id }
                    if id == modifier_id && param_id == local_param
            )
            .then(|| binding.id.clone())
        })
        .unwrap_or_else(|| panic!("missing binding for {modifier_id}/{local_param}"))
}

fn live_param(
    ct: &crate::content_thread::ContentThread,
    modifier: &NodeId,
    node_id: &str,
    param_id: &str,
) -> Vec<f32> {
    let owner = generator_graph(
        ct.engine.project().unwrap(),
        &LayerId::new("journey-mushroom"),
    );
    let prepared = manifold_renderer::node_graph::scene_modifier_expand::prepare_scene_modifiers(
        owner,
        &manifold_renderer::node_graph::PrimitiveRegistry::with_builtin(),
    )
    .unwrap();
    let routes: Vec<_> = prepared
        .routes
        .iter()
        .filter(|route| &route.modifier_id == modifier && route.local.node.as_str() == node_id)
        .collect();
    assert_eq!(routes.len(), 1, "exact modifier-local route");
    let live = ct.content_pipeline.live_node_params();
    routes[0]
        .copies
        .iter()
        .map(|copy| {
            live.iter()
                .find(|(id, _)| id == &copy.node_id)
                .and_then(|(_, params)| params.iter().find(|(id, _)| *id == param_id))
                .map(|(_, value)| *value)
                .unwrap_or_else(|| panic!("missing live value for {}.{param_id}", copy.node_id))
        })
        .collect()
}

fn send_modifier(ct: &mut crate::content_thread::ContentThread, action: SceneModifierAction) {
    assert!(!ct.handle_command(ContentCommand::SceneModifier(action)));
    assert!(
        ct.graph_edit_diagnostic.is_none(),
        "scene modifier action rejected"
    );
}

/// Mirror the production command loop, which follows LoadProject with a
/// bounded warmup. Calling handle_command alone skips asynchronous GLB decode.
pub(super) fn warm_project(
    ct: &mut crate::content_thread::ContentThread,
    state_tx: &crossbeam_channel::Sender<ContentState>,
) {
    let (command_tx, command_rx) = crossbeam_channel::unbounded();
    ct.run_warmup(&command_rx, &command_tx, state_tx);
}

fn capture_output(ct: &crate::content_thread::ContentThread, path: &Path) -> (Vec<u8>, usize) {
    let device = ct
        .content_pipeline
        .native_gpu_for_tests()
        .expect("native journey device");
    let texture = ct.content_pipeline.export_output_texture();
    assert_eq!(texture.format, manifold_gpu::GpuTextureFormat::Rgba16Float);
    let row_bytes = texture.width * texture.format.bytes_per_pixel();
    let size = row_bytes as usize * texture.height as usize;
    let buffer = device.create_buffer_shared(size as u64);
    let mut encoder = device.create_encoder("f8-journey-readback");
    encoder.copy_texture_to_buffer(texture, &buffer, texture.width, texture.height, row_bytes);
    encoder.commit_and_wait_completed();
    let ptr = buffer.mapped_ptr().expect("journey readback mapping");
    let bytes = unsafe { std::slice::from_raw_parts(ptr, size) };
    let mut rgba = Vec::with_capacity((texture.width * texture.height * 4) as usize);
    for pixel in bytes.chunks_exact(8) {
        for channel in 0..4 {
            let bits = u16::from_le_bytes([pixel[channel * 2], pixel[channel * 2 + 1]]);
            let value = half::f16::from_bits(bits).to_f32();
            assert!(value.is_finite(), "nonfinite native output");
            rgba.push(if channel == 3 {
                (value.clamp(0.0, 1.0) * 255.0).round() as u8
            } else {
                linear_to_srgb8(value)
            });
        }
    }
    let nonzero = rgba
        .chunks_exact(4)
        .filter(|pixel| pixel[3] != 0 && pixel[..3].iter().any(|&value| value != 0))
        .count();
    std::fs::write(path, encode_rgba8_png(&rgba, texture.width, texture.height))
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    assert!(
        rgba.chunks_exact(4).any(|pixel| pixel != &rgba[..4]),
        "uniform output {:?} is not geometry; capture {}",
        &rgba[..4],
        path.display()
    );
    (rgba, nonzero)
}

#[test]
fn f8_playing_photoscan_lfo_reorder_save_reopen_journey() {
    env_logger::Builder::new()
        .is_test(true)
        .filter_level(log::LevelFilter::Warn)
        .try_init()
        .expect("journey logger");
    let output_dir = PathBuf::from("target/journey-proofs/f8_playing_photoscan");
    std::fs::create_dir_all(&output_dir).expect("journey artifact directory");
    let (_, held_out_report) =
        assemble_import_graph(Path::new(STATIC_FIXTURE)).expect("held-out static scan import");
    assert!(
        held_out_report.object_count > 0,
        "held-out static scan has no imported objects"
    );
    eprintln!(
        "[f8 journey] held-out static scan import-qualified ({} objects); rendering remains open",
        held_out_report.object_count
    );
    reset_cold_touch_counts();
    let mut ct = headless_content_thread(journey_project(), 320, 180);
    let (state_tx, _state_rx) = crossbeam_channel::unbounded::<ContentState>();
    let mushroom_id = LayerId::new("journey-mushroom");
    ct.watched_graph_target = Some(manifold_core::GraphTarget::Generator(mushroom_id.clone()));

    send_modifier(
        &mut ct,
        SceneModifierAction::Add(mushroom_id.clone(), "SurfacePeel".into()),
    );
    send_modifier(
        &mut ct,
        SceneModifierAction::Add(mushroom_id.clone(), "SurfacePeel".into()),
    );
    let ids = modifier_ids(ct.engine.project().expect("journey project"), &mushroom_id);
    assert_eq!(ids.len(), 2, "two Surface Peel instances are playing");
    let lift_id = host_binding(
        ct.engine.project().expect("journey project"),
        &mushroom_id,
        &ids[0],
        "lift",
    );

    let driver_id = lift_id.clone();
    let driver_layer_id = mushroom_id.clone();
    ct.handle_command(ContentCommand::MutateProject(Box::new(move |project| {
        let owner = project
            .graph_target_owner_mut(&manifold_core::GraphTarget::Generator(driver_layer_id))
            .expect("mushroom owner");
        let driver = ParameterDriver::new(
            ParamId::from(driver_id.clone()),
            BeatDivision::Quarter,
            DriverWaveform::Sine,
        );
        owner.drivers_mut().push(driver);
    })));
    ct.timer.set_frame_clocked(true);
    warm_project(&mut ct, &state_tx);
    ct.handle_command(ContentCommand::SeekToBeat(Beats(0.0)));
    ct.handle_command(ContentCommand::Play);
    for _ in 0..4 {
        ct.tick_frame(&state_tx);
    }
    assert!(ct.engine.is_playing(), "native transport must be playing");
    assert_eq!(
        ct.engine.active_clip_count(),
        1,
        "one real clip must be active"
    );
    eprintln!(
        "[f8 journey] beat={} clips={} ready={} live_nodes={}",
        ct.engine.current_beat_f64(),
        ct.engine.active_clip_count(),
        ct.engine.all_active_clips_ready(),
        ct.content_pipeline.live_node_params().len()
    );
    let (first_frame, first_nonzero) = capture_output(&ct, &output_dir.join("playing-0004.png"));
    assert!(
        first_nonzero > 0,
        "playing journey output contains geometry"
    );
    let prepared_compiles = cold_touch_count(ColdTouchKind::PipelineCompile);
    for _ in 0..20 {
        ct.tick_frame(&state_tx);
    }
    assert_eq!(
        cold_touch_count(ColdTouchKind::PipelineCompile),
        prepared_compiles,
        "live LFO must not compile pipelines"
    );
    let (later_frame, later_nonzero) = capture_output(&ct, &output_dir.join("playing-0024.png"));
    assert!(later_nonzero > 0, "later playing output contains geometry");
    assert_ne!(
        first_frame, later_frame,
        "the live LFO changes the observed output"
    );
    eprintln!(
        "[f8 journey] pipeline compile cold touches during journey: {} (graph rebuild counter unavailable)",
        cold_touch_count(ColdTouchKind::PipelineCompile)
    );

    let first = ids[0].clone();
    send_modifier(
        &mut ct,
        SceneModifierAction::Move(mushroom_id.clone(), first.clone(), 1),
    );
    assert_eq!(
        modifier_ids(ct.engine.project().expect("journey project"), &mushroom_id),
        vec![ids[1].clone(), first.clone()]
    );
    send_modifier(
        &mut ct,
        SceneModifierAction::Remove(mushroom_id.clone(), first.clone()),
    );
    assert_eq!(
        modifier_ids(ct.engine.project().expect("journey project"), &mushroom_id).len(),
        1
    );
    ct.handle_command(ContentCommand::Undo);
    assert_eq!(
        modifier_ids(ct.engine.project().expect("journey project"), &mushroom_id).len(),
        2,
        "undo restores both instances"
    );

    let saved_path = output_dir.join("playing-photoscan.manifold");
    manifold_io::saver::save_project_v1(ct.engine.project().expect("journey project"), &saved_path)
        .expect("save playing photoscan journey");
    let reopened =
        manifold_io::loader::load_project(&saved_path).expect("reopen playing photoscan journey");
    assert_eq!(
        modifier_ids(&reopened, &mushroom_id).len(),
        2,
        "save/reopen preserves both modifiers"
    );
    assert_eq!(
        reopened
            .graph_target_owner(&manifold_core::GraphTarget::Generator(mushroom_id.clone()))
            .expect("reopened owner")
            .get_drivers_list()
            .map_or(0, Vec::len),
        1,
        "save/reopen preserves the LFO"
    );

    ct.handle_command(ContentCommand::LoadProject(Box::new(reopened)));
    warm_project(&mut ct, &state_tx);
    ct.timer.set_frame_clocked(true);
    ct.handle_command(ContentCommand::Pause);
    ct.handle_command(ContentCommand::SeekToBeat(Beats(4.25)));
    ct.tick_frame(&state_tx);
    let (before_mapping, before_mapping_nonzero) =
        capture_output(&ct, &output_dir.join("reopen-before-mapping.png"));
    assert!(
        before_mapping_nonzero > 0,
        "reopened output contains geometry"
    );
    let before_effective = live_param(&ct, &ids[0], "patch", "separation");
    let reopened_lift = host_binding(
        ct.engine.project().expect("reopened project"),
        &mushroom_id,
        &ids[0],
        "lift",
    );
    let mapping_id = reopened_lift.clone();
    let mapping_layer_id = mushroom_id.clone();
    ct.handle_command(ContentCommand::MutateProject(Box::new(move |project| {
        let owner = project
            .graph_target_owner_mut(&manifold_core::GraphTarget::Generator(mapping_layer_id))
            .expect("reopened mushroom owner");
        let binding = owner
            .graph_def_mut()
            .as_mut()
            .and_then(|graph| graph.preset_metadata.as_mut())
            .and_then(|metadata| {
                metadata
                    .bindings
                    .iter_mut()
                    .find(|binding| binding.id == mapping_id)
            })
            .expect("reopened lift mapping");
        binding.scale = 0.5;
    })));
    let remapped_scale =
        generator_graph(ct.engine.project().expect("reopened project"), &mushroom_id)
            .preset_metadata
            .as_ref()
            .and_then(|metadata| {
                metadata
                    .bindings
                    .iter()
                    .find(|binding| binding.id == reopened_lift)
            })
            .map(|binding| binding.scale)
            .expect("reopened lift mapping after edit");
    assert_eq!(remapped_scale, 0.5, "the post-reopen mapping edit is live");
    ct.handle_command(ContentCommand::Pause);
    ct.handle_command(ContentCommand::SeekToBeat(Beats(4.25)));
    ct.tick_frame(&state_tx);
    let (after_mapping, after_mapping_nonzero) =
        capture_output(&ct, &output_dir.join("reopen-after-mapping.png"));
    assert!(
        after_mapping_nonzero > 0,
        "remapped output contains geometry"
    );
    let after_effective = live_param(&ct, &ids[0], "patch", "separation");
    assert_ne!(
        before_effective, after_effective,
        "changed mapping changes the effective driver value at the same beat"
    );
    assert_ne!(
        before_mapping, after_mapping,
        "changed mapping changes the observed output"
    );
}
