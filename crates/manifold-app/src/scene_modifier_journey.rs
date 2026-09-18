//! Native F8 release journey for playing photoscan modifiers.
//!
//! This deliberately stays a single, bounded observation: one real imported
//! photoscan, two Surface Peel instances, one live LFO, structural
//! gestures, save/reopen, and a pair of post-reopen output captures. The test
//! uses the same headless `ContentThread` and native Metal output as the app.

mod periodic;
mod angular;
mod consolidation;
mod resize;

use std::path::{Path, PathBuf};

use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphDef, EffectGraphNode, SerializedParamValue,
};
use manifold_core::effects::{ParamEnvelope, ParamId, ParameterDriver};
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::scene_modifier_preset::{
    SceneMeshReferenceFrame, SceneModifierInstanceDef, SceneNodeRef, SceneTargetSelection,
};
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
const NESTED_MULTIMATERIAL_V2: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../manifold-renderer/tests/fixtures/scene-modifiers/nested_multimaterial_v2.json"
));

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

fn math_view_project() -> Project {
    let mut graph: EffectGraphDef =
        serde_json::from_str(NESTED_MULTIMATERIAL_V2).expect("nested math fixture parses");
    fn bake_left_material(nodes: &mut [EffectGraphNode]) {
        for node in nodes {
            if node.node_id == "left_pbr" {
                node.params.insert(
                    "baked_look".into(),
                    SerializedParamValue::Bool { value: true },
                );
            }
            if let Some(group) = node.group.as_mut() {
                bake_left_material(&mut group.nodes);
            }
        }
    }
    bake_left_material(&mut graph.nodes);
    graph.version = 3;
    let recipe: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../manifold-renderer/assets/scene-modifier-presets/VortexFragments.json"
    )))
    .expect("Vortex Fragments recipe parses");
    let view_recipe: EffectGraphDef = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../manifold-renderer/assets/scene-modifier-presets/MathView.json"
    )))
    .expect("Math View recipe parses");
    let mut frames = Vec::new();
    for container in &graph.nodes {
        let Some(group) = &container.group else {
            continue;
        };
        let source = group
            .nodes
            .iter()
            .find(|node| node.type_id == "node.cube_mesh")
            .expect("cube mesh source");
        let object = group
            .nodes
            .iter()
            .find(|node| node.type_id == "node.scene_object")
            .expect("scene object target");
        let transform = group
            .nodes
            .iter()
            .find(|node| node.type_id == "node.transform_3d")
            .expect("transform source");
        let scope = vec![container.node_id.clone()];
        frames.push(SceneMeshReferenceFrame {
            target: SceneNodeRef {
                scope: scope.clone(),
                node: object.node_id.clone(),
            },
            source: SceneNodeRef {
                scope,
                node: source.node_id.clone(),
            },
            source_definition_hash:
                manifold_core::scene_source_identity::scene_source_definition_hash(&graph, source)
                    .expect("cube source hash"),
            source_offset: ["pos_x", "pos_y", "pos_z"].map(|param| {
                match transform.params.get(param) {
                    Some(SerializedParamValue::Float { value }) => f64::from(*value),
                    _ => 0.0,
                }
            }),
            scene_radius: 3.0,
        });
    }
    let modifier_id = NodeId::new("vortex_a");
    let view_id = NodeId::new("math_view");
    graph.scene_modifiers.push(SceneModifierInstanceDef {
        id: modifier_id.clone(),
        scene: SceneNodeRef {
            scope: Vec::new(),
            node: NodeId::new("scan_render"),
        },
        targets: SceneTargetSelection::AllObjects,
        mesh_frames: frames.clone(),
        legacy_math_view_carrier: None,
        graph: Box::new(recipe),
    });
    graph.scene_modifiers.push(SceneModifierInstanceDef {
        id: view_id.clone(),
        scene: SceneNodeRef {
            scope: vec![],
            node: NodeId::new("scan_render"),
        },
        targets: SceneTargetSelection::AllObjects,
        mesh_frames: frames,
        legacy_math_view_carrier: None,
        graph: Box::new(view_recipe),
    });
    graph = manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(
        &graph,
        &modifier_id,
    )
    .expect("reconcile Vortex Fragments controls")
    .graph;
    graph = manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(
        &graph,
        &view_id,
    )
    .expect("reconcile Math View controls")
    .graph;
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);
    project.settings.output_width = 320;
    project.settings.output_height = 180;
    let mut layer = Layer::new_generator(
        "Math Grid".into(),
        PresetTypeId::new("PhotoscanBaseline"),
        0,
    );
    layer.layer_id = LayerId::new("math-grid");
    layer.gen_params_or_init().graph = Some(graph);
    layer.gen_params_or_init().refresh_manifest_from_graph();
    let mut clip = manifold_core::clip::TimelineClip::new_generator(Beats(0.0), Beats(16.0));
    clip.layer_id = layer.layer_id.clone();
    layer.clips.push(clip);
    project.timeline.layers.push(layer);
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

fn capture_output_impl(
    ct: &crate::content_thread::ContentThread,
    path: &Path,
    require_geometry: bool,
) -> (Vec<u8>, usize) {
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
    if require_geometry {
        assert!(
            rgba.chunks_exact(4).any(|pixel| pixel != &rgba[..4]),
            "uniform output {:?} is not geometry; capture {}",
            &rgba[..4],
            path.display()
        );
    }
    (rgba, nonzero)
}

fn capture_output(ct: &crate::content_thread::ContentThread, path: &Path) -> (Vec<u8>, usize) {
    capture_output_impl(ct, path, true)
}

fn capture_output_allow_uniform(
    ct: &crate::content_thread::ContentThread,
    path: &Path,
) -> (Vec<u8>, usize) {
    capture_output_impl(ct, path, false)
}

fn set_generator_param(
    ct: &mut crate::content_thread::ContentThread,
    layer_id: &LayerId,
    param_id: &str,
    value: f32,
) {
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());
    let old = ct
        .engine
        .project_mut()
        .expect("math journey project")
        .with_preset_graph_mut(&target, |instance| {
            instance
                .params
                .contains(param_id)
                .then(|| instance.get_base_param(param_id))
        })
        .flatten()
        .unwrap_or_else(|| panic!("missing generator parameter {param_id}"));
    assert!(!ct.handle_command(ContentCommand::Execute(Box::new(
        manifold_editing::commands::effects::ChangeGraphParamCommand::new(
            target,
            param_id.to_string(),
            old,
            value,
        ),
    ))));
    assert!(
        ct.graph_edit_diagnostic.is_none(),
        "parameter edit rejected"
    );
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

#[test]
fn math_view_grid_app_control_journey() {
    let output_dir = PathBuf::from("target/journey-proofs/math-grid");
    std::fs::create_dir_all(&output_dir).expect("math grid artifact directory");
    let layer_id = LayerId::new("math-grid");
    let mut ct = headless_content_thread(Project::default(), 320, 180);
    let (state_tx, _state_rx) = crossbeam_channel::unbounded::<ContentState>();
    ct.handle_command(ContentCommand::LoadProject(Box::new(math_view_project())));
    assert!(
        ct.graph_edit_diagnostic.is_none(),
        "saved Math View project rejected: {:?}",
        ct.graph_edit_diagnostic
    );
    let ids = modifier_ids(
        ct.engine.project().expect("math journey project"),
        &layer_id,
    );
    assert_eq!(ids.len(), 2, "Vortex plus the standalone Math View are playing");
    let modifier_id = NodeId::new("math_view");
    let modifier = generator_graph(
        ct.engine.project().expect("math journey project"),
        &layer_id,
    )
    .scene_modifiers
    .iter()
    .find(|modifier| modifier.id == modifier_id)
    .expect("Math View modifier");
    assert_eq!(
        modifier.mesh_frames.len(),
        2,
        "fixture captures both mesh objects"
    );
    let control_ids: Vec<(String, String)> =
        ["fragments", "ghosts", "vectors", "trails", "mode", "grid"]
            .iter()
            .map(|local| {
                (
                    (*local).into(),
                    host_binding(
                        ct.engine.project().expect("math journey project"),
                        &layer_id,
                        &modifier_id,
                        &format!("math_view_{local}"),
                    ),
                )
            })
            .collect();
    let control_id = |local: &str| {
        control_ids
            .iter()
            .find(|(name, _)| name == local)
            .map(|(_, id)| id.as_str())
            .expect("resolved Math View host binding")
    };
    ct.timer.set_frame_clocked(true);
    warm_project(&mut ct, &state_tx);
    ct.handle_command(ContentCommand::SeekToBeat(Beats(0.0)));
    ct.tick_frame(&state_tx);
    assert!(
        ct.graph_edit_diagnostic.is_none(),
        "Math View runtime graph invalid: {:?}",
        ct.graph_edit_diagnostic
    );

    for control in ["fragments", "ghosts", "vectors", "trails"] {
        set_generator_param(&mut ct, &layer_id, control_id(control), 0.0);
    }
    set_generator_param(&mut ct, &layer_id, control_id("mode"), 1.0);
    set_generator_param(&mut ct, &layer_id, control_id("grid"), 0.0);
    ct.tick_frame(&state_tx);
    let (_, math_off_nonzero) =
        capture_output_allow_uniform(&ct, &output_dir.join("math-mode-grid-off.png"));
    assert_eq!(
        math_off_nonzero, 0,
        "Math mode with all diagrams off is black"
    );

    set_generator_param(&mut ct, &layer_id, control_id("grid"), 1.0);
    ct.tick_frame(&state_tx);
    let (_, grid_on_nonzero) =
        capture_output_allow_uniform(&ct, &output_dir.join("math-mode-grid-on.png"));
    assert!(
        grid_on_nonzero > 0,
        "Grid on produces visible diagram output"
    );

    set_generator_param(&mut ct, &layer_id, control_id("grid"), 0.0);
    ct.tick_frame(&state_tx);
    let (_, grid_off_nonzero) =
        capture_output_allow_uniform(&ct, &output_dir.join("math-mode-grid-off-after-edit.png"));
    assert_eq!(grid_off_nonzero, 0, "Grid off returns Math mode to black");

    ct.handle_command(ContentCommand::Undo);
    ct.tick_frame(&state_tx);
    let (_, undo_nonzero) =
        capture_output_allow_uniform(&ct, &output_dir.join("math-mode-grid-undo.png"));
    assert!(undo_nonzero > 0, "undo restores Grid output");
    ct.handle_command(ContentCommand::Redo);
    ct.tick_frame(&state_tx);
    let (_, redo_nonzero) =
        capture_output_allow_uniform(&ct, &output_dir.join("math-mode-grid-redo.png"));
    assert_eq!(redo_nonzero, 0, "redo restores Grid off");

    // The view reflects the combined preceding chain: editing the Vortex's
    // Orbit changes the diagram without touching any Math View control.
    set_generator_param(&mut ct, &layer_id, control_id("fragments"), 1.0);
    ct.tick_frame(&state_tx);
    let (before_pixels, before_nonzero) =
        capture_output_allow_uniform(&ct, &output_dir.join("math-chain-before.png"));
    assert!(before_nonzero > 0, "fragments render the deformed samples");
    let orbit_id = host_binding(
        ct.engine.project().expect("math journey project"),
        &layer_id,
        &NodeId::new("vortex_a"),
        "orbit",
    );
    set_generator_param(&mut ct, &layer_id, &orbit_id, 4.4);
    ct.tick_frame(&state_tx);
    let (after_pixels, _) =
        capture_output_allow_uniform(&ct, &output_dir.join("math-chain-after.png"));
    assert_ne!(
        before_pixels, after_pixels,
        "changing the preceding modifier changes the combined Math View output"
    );
    set_generator_param(&mut ct, &layer_id, &orbit_id, 2.2);
    set_generator_param(&mut ct, &layer_id, control_id("fragments"), 0.0);
    ct.tick_frame(&state_tx);
    let (_, restored_nonzero) =
        capture_output_allow_uniform(&ct, &output_dir.join("math-chain-restored.png"));
    assert_eq!(restored_nonzero, 0, "restored defaults return to black");

    let saved_path = output_dir.join("math-grid.manifold");
    manifold_io::saver::save_project_v1(
        ct.engine.project().expect("math journey project"),
        &saved_path,
    )
    .expect("save math grid journey");
    let reopened =
        manifold_io::loader::load_project(&saved_path).expect("reopen math grid journey");
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());
    let reopened_grid = reopened
        .graph_target_owner(&target)
        .expect("reopened math generator")
        .params
        .get(control_id("grid"))
        .expect("reopened grid parameter")
        .base;
    assert_eq!(reopened_grid, 0.0, "save/load preserves Grid off");

    ct.handle_command(ContentCommand::LoadProject(Box::new(reopened)));
    warm_project(&mut ct, &state_tx);
    ct.timer.set_frame_clocked(true);
    ct.handle_command(ContentCommand::SeekToBeat(Beats(0.0)));
    ct.tick_frame(&state_tx);
    let (_, loaded_nonzero) =
        capture_output_allow_uniform(&ct, &output_dir.join("math-mode-grid-off-after-load.png"));
    assert_eq!(loaded_nonzero, 0, "loaded Grid off output remains black");
}

/// The pre-standalone shape of the Vortex recipe: the current (stripped)
/// bundled recipe plus the legacy embedded Math View control section,
/// including the retired Scope control. Detection and migration both run
/// against this on load.
fn legacy_vortex_carrier_graph() -> EffectGraphDef {
    let mut carrier = serde_json::from_str::<serde_json::Value>(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../manifold-renderer/assets/scene-modifier-presets/VortexFragments.json"
    )))
    .expect("Vortex recipe parses");
    let mut next_node_id = 100u32;
    let mut controls: Vec<(String, f64, f64, f64)> =
        manifold_core::scene_modifier_math_view::CONTROLS
            .iter()
            .map(|(suffix, _, default, min, max)| {
                ((*suffix).to_string(), *default as f64, *min as f64, *max as f64)
            })
            .collect();
    controls.push(("scope".into(), 1.0, 0.0, 1.0));
    for (suffix, default, min, max) in &controls {
        let local = format!("math_view_{suffix}");
        carrier["presetMetadata"]["params"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": local, "name": suffix, "min": min, "max": max,
                "defaultValue": default, "section": "Math View", "cardVisible": true,
            }));
        carrier["presetMetadata"]["bindings"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "id": local, "label": suffix, "defaultValue": default,
                "target": {"kind": "node", "nodeId": format!("__math_view_{suffix}"), "param": "value"},
            }));
        carrier["nodes"].as_array_mut().unwrap().push(serde_json::json!({
            "id": next_node_id, "nodeId": format!("__math_view_{suffix}"),
            "typeId": "node.value", "handle": local,
            "params": {"value": {"type": "Float", "value": default}},
        }));
        next_node_id += 1;
    }
    let carrier_graph: EffectGraphDef =
        serde_json::from_value(carrier).expect("legacy carrier graph parses");
    assert!(
        manifold_core::scene_modifier_math_view::has_legacy_math_view_controls(&carrier_graph),
        "fixture reproduces the legacy embedded section"
    );
    carrier_graph
}

/// Legacy projects embedded Math View controls in the Vortex recipe. Loading
/// moves them onto one appended standalone Math View modifier: host binding
/// ids (and therefore values, animation and modulation) survive, the retired
/// Scope remains hidden and functional, and a second migration pass is a no-op.
#[test]
fn legacy_math_view_migration_journey() {
    let output_dir = PathBuf::from("target/journey-proofs/math-view-migration");
    std::fs::create_dir_all(&output_dir).expect("migration artifact directory");
    let layer_id = LayerId::new("math-grid");
    let carrier_id = NodeId::new("legacy_vortex");

    let carrier_graph = legacy_vortex_carrier_graph();

    let mut project = math_view_project();
    {
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let owner = project.graph_target_owner_mut(&target).unwrap();
        let graph = owner.graph.as_mut().unwrap();
        // Replace the post-migration pair with the single legacy carrier.
        let frames = graph.scene_modifiers[0].mesh_frames.clone();
        graph.scene_modifiers.clear();
        graph.scene_modifiers.push(SceneModifierInstanceDef {
            id: carrier_id.clone(),
            scene: SceneNodeRef {
                scope: vec![],
                node: NodeId::new("scan_render"),
            },
            targets: SceneTargetSelection::AllObjects,
            mesh_frames: frames,
            legacy_math_view_carrier: None,
            graph: Box::new(carrier_graph),
        });
        // Drop the now-dangling host bindings of the old fixture pair, then
        // mint the carrier's (including every math_view_* control).
        let metadata = graph.preset_metadata.as_mut().unwrap();
        metadata.bindings.retain(|binding| !matches!(
            &binding.target,
            BindingTarget::SceneModifier { .. }
        ));
        metadata.params.retain(|param| !param.id.starts_with("sceneModifier:"));
        *graph = manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(
            graph,
            &carrier_id,
        )
        .expect("reconcile legacy carrier")
        .graph;
        owner.refresh_manifest_from_graph();
        // Non-default Mode and Scope both survive migration.
        let mode_macro = format!("sceneModifier:[\"{carrier_id}\",\"math_view_mode\"]");
        owner.set_base_param(mode_macro.as_str(), 1.0);
        let scope_macro = format!("sceneModifier:[\"{carrier_id}\",\"math_view_scope\"]");
        owner.set_base_param(scope_macro.as_str(), 0.0);
        owner.refresh_manifest_from_graph();
    }
    let pre_save = output_dir.join("legacy-math-view.manifold");
    manifold_io::saver::save_project_v1(&project, &pre_save).expect("save legacy project");
    let mut reopened = manifold_io::loader::load_project(&pre_save).expect("reopen legacy project");

    let notices = crate::project_io::migrate_project_scene_graphs(&mut reopened);
    assert!(notices.is_empty(), "clean migration, got: {notices:?}");

    let graph = generator_graph(&reopened, &layer_id);
    let ids: Vec<_> = graph.scene_modifiers.iter().map(|m| m.id.clone()).collect();
    assert_eq!(ids.len(), 2, "carrier plus one appended Math View: {ids:?}");
    assert_eq!(ids[0], carrier_id);
    let carrier = &graph.scene_modifiers[0];
    assert!(
        !manifold_core::scene_modifier_math_view::has_legacy_math_view_controls(&carrier.graph),
        "carrier section stripped"
    );
    assert!(
        carrier
            .graph
            .preset_metadata
            .as_ref()
            .unwrap()
            .bindings
            .iter()
            .any(|binding| binding.id == "orbit"),
        "authored deformation survives the strip"
    );
    let view = &graph.scene_modifiers[1];
    assert!(
        manifold_core::scene_modifier_math_view::is_math_view_recipe(&view.graph),
        "appended instance is the standalone Math View"
    );
    assert_eq!(view.mesh_frames.len(), 2, "view captures both mesh objects");

    // The donor's host binding keeps its id and value, retargeted to the view.
    let mode_macro = format!("sceneModifier:[\"{carrier_id}\",\"math_view_mode\"]");
    let binding = graph
        .preset_metadata
        .as_ref()
        .unwrap()
        .bindings
        .iter()
        .find(|binding| binding.id == mode_macro)
        .expect("mode binding survives");
    assert!(
        matches!(&binding.target, BindingTarget::SceneModifier { modifier_id, param_id }
            if modifier_id == &view.id && param_id == "math_view_mode"),
        "mode binding retargeted to the standalone view"
    );
    let owner = reopened
        .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
        .unwrap();
    assert_eq!(
        owner.get_base_param(mode_macro.as_str()),
        1.0,
        "saved Mode value survives migration"
    );
    // Scope keeps its stable macro id but leaves the authoring card.
    let scope_macro = format!("sceneModifier:[\"{carrier_id}\",\"math_view_scope\"]");
    assert!(
        graph
            .preset_metadata
            .as_ref()
            .unwrap()
            .bindings
            .iter()
            .any(|binding| binding.id == scope_macro),
        "scope binding survives"
    );
    assert!(
        owner.params.contains(scope_macro.as_str()),
        "scope host param survives"
    );
    assert_eq!(owner.get_base_param(&scope_macro), 0.0);
    assert!(!graph.preset_metadata.as_ref().unwrap().params.iter()
        .find(|param| param.id == scope_macro).unwrap().card_visible);
    // The view's enabled param got a fresh host binding.
    let _ = host_binding(&reopened, &layer_id, &view.id, "enabled");

    // Idempotent: a second pass changes nothing.
    let once = serde_json::to_string(
        &reopened
            .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
            .unwrap()
            .graph,
    )
    .unwrap();
    let mut twice = reopened.clone();
    let notices = crate::project_io::migrate_project_scene_graphs(&mut twice);
    assert!(notices.is_empty(), "second pass adds no notices: {notices:?}");
    let twice_graph = serde_json::to_string(
        &twice
            .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
            .unwrap()
            .graph,
    )
    .unwrap();
    assert_eq!(once, twice_graph, "migration is idempotent");

    // The migrated project drives the runtime: Math mode renders the diagram.
    let mut ct = headless_content_thread(Project::default(), 320, 180);
    let (state_tx, _state_rx) = crossbeam_channel::unbounded::<ContentState>();
    ct.handle_command(ContentCommand::LoadProject(Box::new(reopened)));
    assert!(
        ct.graph_edit_diagnostic.is_none(),
        "migrated project rejected: {:?}",
        ct.graph_edit_diagnostic
    );
    ct.timer.set_frame_clocked(true);
    warm_project(&mut ct, &state_tx);
    ct.handle_command(ContentCommand::SeekToBeat(Beats(0.0)));
    // Two ticks: the first Math-mode frame can precede the derived runtime's
    // borrowed resources going ready (the grid journey shows the same shape).
    ct.tick_frame(&state_tx);
    ct.tick_frame(&state_tx);
    let (_, migrated_nonzero) =
        capture_output_allow_uniform(&ct, &output_dir.join("migrated-math-mode.png"));
    assert!(
        migrated_nonzero > 0,
        "migrated Math View renders in Math mode (Mode value survived)"
    );
}

/// Multi-carrier legacy migration (BUG-ngdf): four Vortex carriers, one per
/// authoredness class. A is disabled with default content; D is enabled but
/// equally untouched — neither is authored, and both strip cleanly without a
/// view. B is enabled, in Math mode, with a beat driver on its pulse trigger
/// and an explicit one-object selection. C is enabled, in Overlay mode, with
/// an envelope on scan amount and all objects. B and C each gain their own
/// view immediately after themselves; binding ids are unchanged so values
/// and modulation survive save/reload, and a second migration pass plus a
/// save/reload round trip change nothing.
#[test]
fn legacy_math_view_multi_carrier_migration_journey() {
    use manifold_core::scene_modifier_math_view as mv;
    let output_dir = PathBuf::from("target/journey-proofs/math-view-migration-multi");
    std::fs::create_dir_all(&output_dir).expect("migration artifact directory");
    let layer_id = LayerId::new("math-grid");
    let carrier_a = NodeId::new("legacy_vortex_a");
    let carrier_b = NodeId::new("legacy_vortex_b");
    let carrier_c = NodeId::new("legacy_vortex_c");
    let carrier_d = NodeId::new("legacy_vortex_d");

    let mut project = math_view_project();
    let frames = {
        let graph = generator_graph(&project, &layer_id);
        graph.scene_modifiers[0].mesh_frames.clone()
    };
    assert_eq!(frames.len(), 2, "fixture captures both mesh objects");
    let scene = SceneNodeRef {
        scope: vec![],
        node: NodeId::new("scan_render"),
    };
    let frame_b = frames[1].clone();
    let selection_b = SceneTargetSelection::Explicit {
        objects: vec![frame_b.target.clone()],
    };
    let mode_macro = |carrier: &NodeId, suffix: &str| {
        format!("sceneModifier:[\"{carrier}\",\"math_view_{suffix}\"]")
    };
    let pulse_trigger = mode_macro(&carrier_b, "pulse_trigger");
    let scan_amount = mode_macro(&carrier_c, "scan_amount");
    let mode_macro_b = mode_macro(&carrier_b, "mode");
    let mode_macro_c = mode_macro(&carrier_c, "mode");
    {
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let owner = project.graph_target_owner_mut(&target).unwrap();
        let graph = owner.graph.as_mut().unwrap();
        graph.scene_modifiers.clear();
        graph.scene_modifiers.push(SceneModifierInstanceDef {
            id: carrier_a.clone(),
            scene: scene.clone(),
            targets: SceneTargetSelection::AllObjects,
            mesh_frames: frames.clone(),
            legacy_math_view_carrier: None,
            graph: Box::new(legacy_vortex_carrier_graph()),
        });
        graph.scene_modifiers.push(SceneModifierInstanceDef {
            id: carrier_b.clone(),
            scene: scene.clone(),
            targets: selection_b.clone(),
            mesh_frames: vec![frame_b.clone()],
            legacy_math_view_carrier: None,
            graph: Box::new(legacy_vortex_carrier_graph()),
        });
        graph.scene_modifiers.push(SceneModifierInstanceDef {
            id: carrier_c.clone(),
            scene: scene.clone(),
            targets: SceneTargetSelection::AllObjects,
            mesh_frames: frames.clone(),
            legacy_math_view_carrier: None,
            graph: Box::new(legacy_vortex_carrier_graph()),
        });
        graph.scene_modifiers.push(SceneModifierInstanceDef {
            id: carrier_d.clone(),
            scene: scene.clone(),
            targets: SceneTargetSelection::AllObjects,
            mesh_frames: frames.clone(),
            legacy_math_view_carrier: None,
            graph: Box::new(legacy_vortex_carrier_graph()),
        });
        // Drop the old fixture pair's bindings, then mint each carrier's
        // (including every math_view_* control and enabled).
        let metadata = graph.preset_metadata.as_mut().unwrap();
        metadata.bindings.retain(|binding| {
            !matches!(&binding.target, BindingTarget::SceneModifier { .. })
        });
        metadata
            .params
            .retain(|param| !param.id.starts_with("sceneModifier:"));
        for id in [&carrier_a, &carrier_b, &carrier_c, &carrier_d] {
            *graph = manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(
                graph, id,
            )
            .expect("reconcile legacy carrier")
            .graph;
        }
        owner.refresh_manifest_from_graph();
        // A: disabled, defaults; D: enabled but equally untouched — neither
        // is authored. B: Math mode + driver. C: Overlay mode + envelope.
        let enabled_a = format!("sceneModifier:[\"{carrier_a}\",\"enabled\"]");
        owner.set_base_param(&enabled_a, 0.0);
        // D is deliberately left exactly as reconciled: enabled at its
        // default, every Math View control at its default.
        owner.set_base_param(&mode_macro_b, 1.0);
        owner.drivers_mut().push(ParameterDriver::new(
            ParamId::from(pulse_trigger.clone()),
            BeatDivision::Quarter,
            DriverWaveform::Sine,
        ));
        owner.set_base_param(&mode_macro_c, 2.0);
        owner
            .envelopes_mut()
            .push(ParamEnvelope::new(ParamId::from(scan_amount.clone())));
        owner.refresh_manifest_from_graph();
    }

    let pre_save = output_dir.join("legacy-multi-carrier.manifold");
    manifold_io::saver::save_project_v1(&project, &pre_save).expect("save legacy project");
    let mut reopened =
        manifold_io::loader::load_project(&pre_save).expect("reopen legacy project");
    let notices = crate::project_io::migrate_project_scene_graphs(&mut reopened);
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains("became 2 standalone Math View modifiers")),
        "per-carrier migration summary expected, got: {notices:?}"
    );

    let graph = generator_graph(&reopened, &layer_id);
    let kinds: Vec<(NodeId, bool)> = graph
        .scene_modifiers
        .iter()
        .map(|modifier| {
            (
                modifier.id.clone(),
                mv::is_math_view_recipe(&modifier.graph),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        vec![
            (carrier_a.clone(), false),
            (carrier_b.clone(), false),
            (kinds[2].0.clone(), true),
            (carrier_c.clone(), false),
            (kinds[4].0.clone(), true),
            (carrier_d.clone(), false),
        ],
        "chain order preserved, one view after each authored carrier: {kinds:?}"
    );
    let view_b = &graph.scene_modifiers[2];
    let view_c = &graph.scene_modifiers[4];
    assert_eq!(view_b.targets, selection_b, "B's view keeps B's selection");
    assert_eq!(
        view_b.mesh_frames,
        vec![frame_b.clone()],
        "B's view keeps B's mesh frames"
    );
    assert_eq!(
        view_c.targets,
        SceneTargetSelection::AllObjects,
        "C's view keeps C's selection"
    );
    assert_eq!(view_c.mesh_frames, frames, "C's view keeps C's mesh frames");

    // Every carrier is stripped; the authored deformation survives.
    for (carrier, index) in [
        (&carrier_a, 0),
        (&carrier_b, 1),
        (&carrier_c, 3),
        (&carrier_d, 5),
    ] {
        let modifier = &graph.scene_modifiers[index];
        assert_eq!(&modifier.id, carrier);
        assert!(
            !mv::has_legacy_math_view_controls(&modifier.graph),
            "{carrier} section stripped"
        );
        assert!(
            modifier
                .graph
                .preset_metadata
                .as_ref()
                .unwrap()
                .bindings
                .iter()
                .any(|binding| binding.id == "orbit"),
            "{carrier} deformation survives the strip"
        );
    }

    // Binding ids are unchanged, so host values and modulation survive; only
    // the retarget differs.
    let metadata = graph.preset_metadata.as_ref().unwrap();
    let mode_binding_b = metadata
        .bindings
        .iter()
        .find(|binding| binding.id == mode_macro_b)
        .expect("B's mode binding survives");
    assert!(
        matches!(
            &mode_binding_b.target,
            BindingTarget::SceneModifier { modifier_id, param_id }
                if modifier_id == &view_b.id && param_id == "math_view_mode"
        ),
        "B's mode binding retargeted to its own view"
    );
    let owner = reopened
        .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
        .unwrap();
    assert_eq!(owner.get_base_param(&mode_macro_b), 1.0, "B's Math mode value");
    assert_eq!(owner.get_base_param(&mode_macro_c), 2.0, "C's Overlay mode value");
    assert!(
        owner
            .drivers
            .as_ref()
            .expect("drivers survive")
            .iter()
            .any(|driver| driver.param_id.as_ref() == pulse_trigger.as_str()),
        "B's pulse driver still keyed by the unchanged binding id"
    );
    assert!(
        owner
            .envelopes
            .as_ref()
            .expect("envelopes survive")
            .iter()
            .any(|envelope| envelope.param_id.as_ref() == scan_amount.as_str()),
        "C's scan envelope still keyed by the unchanged binding id"
    );

    // A and D carried no authored content: their minted math_view bindings
    // and params were pruned with the strip, and neither gained a view.
    for untouched in [&carrier_a, &carrier_d] {
        assert!(
            !metadata.bindings.iter().any(|binding| {
                binding.id.contains(&format!("\"{untouched}\",\"math_view"))
            }),
            "{untouched}'s math_view bindings pruned"
        );
        assert!(
            !owner.params.contains(&format!(
                "sceneModifier:[\"{untouched}\",\"math_view_mode\"]"
            )),
            "{untouched}'s math_view host params pruned"
        );
    }

    // Both views expose the full current control surface: every control is
    // declared by the view recipe and host-bound.
    for view in [view_b, view_c] {
        let declared: std::collections::HashSet<&str> = view
            .graph
            .preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .map(|param| param.id.as_str())
            .collect();
        for (suffix, ..) in mv::CONTROLS {
            let local = format!("math_view_{suffix}");
            assert!(declared.contains(local.as_str()), "view declares {local}");
            assert!(
                metadata.bindings.iter().any(|binding| matches!(
                    &binding.target,
                    BindingTarget::SceneModifier { modifier_id, param_id }
                        if modifier_id == &view.id && param_id == &local
                )),
                "view is host-bound for {local}"
            );
        }
    }

    // Idempotence: a second pass is a no-op, and save/reload of the migrated
    // project is byte-stable at the graph level.
    let once = serde_json::to_string(
        &reopened
            .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
            .unwrap()
            .graph,
    )
    .unwrap();
    let mut twice = reopened.clone();
    let notices = crate::project_io::migrate_project_scene_graphs(&mut twice);
    assert!(notices.is_empty(), "second pass adds no notices: {notices:?}");
    let twice_graph = serde_json::to_string(
        &twice
            .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
            .unwrap()
            .graph,
    )
    .unwrap();
    assert_eq!(once, twice_graph, "migration is idempotent");
    let post_save = output_dir.join("migrated-multi-carrier.manifold");
    manifold_io::saver::save_project_v1(&reopened, &post_save).expect("save migrated project");
    let mut reloaded =
        manifold_io::loader::load_project(&post_save).expect("reload migrated project");
    let notices = crate::project_io::migrate_project_scene_graphs(&mut reloaded);
    assert!(
        notices.is_empty(),
        "migrated project reloads without new migration work: {notices:?}"
    );
    let reloaded_graph = serde_json::to_string(
        &reloaded
            .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
            .unwrap()
            .graph,
    )
    .unwrap();
    assert_eq!(once, reloaded_graph, "save/reload preserves the migrated graph");

    // The migrated project drives the runtime: both views render.
    let mut ct = headless_content_thread(Project::default(), 320, 180);
    let (state_tx, _state_rx) = crossbeam_channel::unbounded::<ContentState>();
    ct.handle_command(ContentCommand::LoadProject(Box::new(reopened)));
    assert!(
        ct.graph_edit_diagnostic.is_none(),
        "migrated multi-view project rejected: {:?}",
        ct.graph_edit_diagnostic
    );
    ct.timer.set_frame_clocked(true);
    warm_project(&mut ct, &state_tx);
    ct.handle_command(ContentCommand::SeekToBeat(Beats(0.0)));
    ct.tick_frame(&state_tx);
    ct.tick_frame(&state_tx);
    let (_, migrated_nonzero) =
        capture_output_allow_uniform(&ct, &output_dir.join("migrated-multi-view.png"));
    assert!(
        migrated_nonzero > 0,
        "migrated per-carrier Math Views render (mode values survived)"
    );
}

fn carrier_macro(carrier: &NodeId, suffix: &str) -> String {
    format!("sceneModifier:[\"{carrier}\",\"math_view_{suffix}\"]")
}

/// A math-view project with the fixture modifier pair removed, the old host
/// modifier bindings dropped, and the fixture frames returned for reuse.
fn legacy_carrier_project() -> (
    Project,
    LayerId,
    Vec<SceneMeshReferenceFrame>,
    SceneNodeRef,
) {
    let layer_id = LayerId::new("math-grid");
    let mut project = math_view_project();
    let frames =
        generator_graph(&project, &layer_id).scene_modifiers[0].mesh_frames.clone();
    let scene = SceneNodeRef {
        scope: vec![],
        node: NodeId::new("scan_render"),
    };
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());
    let owner = project.graph_target_owner_mut(&target).unwrap();
    let graph = owner.graph.as_mut().unwrap();
    graph.scene_modifiers.clear();
    let metadata = graph.preset_metadata.as_mut().unwrap();
    metadata.bindings.retain(|binding| {
        !matches!(&binding.target, BindingTarget::SceneModifier { .. })
    });
    metadata
        .params
        .retain(|param| !param.id.starts_with("sceneModifier:"));
    owner.refresh_manifest_from_graph();
    (project, layer_id, frames, scene)
}

/// Push one legacy-format carrier (or any modifier) onto the journey layer.
fn push_modifier(
    project: &mut Project,
    layer_id: &LayerId,
    id: &NodeId,
    scene: &SceneNodeRef,
    frames: Vec<SceneMeshReferenceFrame>,
    graph: EffectGraphDef,
    reconcile: bool,
) {
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());
    let owner = project.graph_target_owner_mut(&target).unwrap();
    {
        let owner_graph = owner.graph.as_mut().unwrap();
        owner_graph.scene_modifiers.push(SceneModifierInstanceDef {
            id: id.clone(),
            scene: scene.clone(),
            targets: SceneTargetSelection::AllObjects,
            mesh_frames: frames,
            legacy_math_view_carrier: None,
            graph: Box::new(graph),
        });
        if reconcile {
            *owner_graph =
                manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(
                    owner_graph, id,
                )
                .expect("reconcile modifier")
                .graph;
        }
    }
    owner.refresh_manifest_from_graph();
}

fn math_view_recipe() -> EffectGraphDef {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../manifold-renderer/assets/scene-modifier-presets/MathView.json"
    )))
    .expect("Math View recipe parses")
}

fn vortex_recipe() -> EffectGraphDef {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../manifold-renderer/assets/scene-modifier-presets/VortexFragments.json"
    )))
    .expect("Vortex Fragments recipe parses")
}

/// Set the patch transform's cell_size inside a (possibly grouped) recipe.
fn set_patch_cell_size(graph: &mut EffectGraphDef, value: f32) {
    fn walk(nodes: &mut [EffectGraphNode], value: f32) -> bool {
        for node in nodes {
            if node.node_id == "patch" && node.type_id == "node.transform_mesh_patches" {
                node.params.insert(
                    "cell_size".into(),
                    SerializedParamValue::Float { value },
                );
                return true;
            }
            if let Some(group) = node.group.as_mut() && walk(&mut group.nodes, value) {
                return true;
            }
        }
        false
    }
    assert!(walk(&mut graph.nodes, value), "patch node found");
}

/// Two authored legacy carriers on the same objects, both with Connect to
/// Mesh on (BUG-ngdf): before this fix the second view saw two preceding
/// patch sources, `math_view_connect_support` reported ambiguous and the
/// authored connection silently died. Migration now records the carrier on
/// each view; each view's mask must borrow its own carrier's patch params
/// (cell_size/source_offset) and both masks must compose into the scene
/// object's weights. Save/reload preserves the association.
#[test]
fn legacy_math_view_connected_multi_carrier_migration_journey() {
    use manifold_core::scene_modifier_math_view as mv;
    let output_dir = PathBuf::from("target/journey-proofs/math-view-connect-parity");
    std::fs::create_dir_all(&output_dir).expect("migration artifact directory");
    let (mut project, layer_id, frames, scene) = legacy_carrier_project();
    let carrier_a = NodeId::new("legacy_vortex_a");
    let carrier_b = NodeId::new("legacy_vortex_b");
    let mut carrier_b_graph = legacy_vortex_carrier_graph();
    set_patch_cell_size(&mut carrier_b_graph, 0.3);
    push_modifier(
        &mut project,
        &layer_id,
        &carrier_a,
        &scene,
        frames.clone(),
        legacy_vortex_carrier_graph(),
        true,
    );
    push_modifier(
        &mut project,
        &layer_id,
        &carrier_b,
        &scene,
        frames.clone(),
        carrier_b_graph,
        true,
    );
    {
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let owner = project.graph_target_owner_mut(&target).unwrap();
        for carrier in [&carrier_a, &carrier_b] {
            owner.set_base_param(&carrier_macro(carrier, "connect_mesh"), 1.0);
            owner.set_base_param(&carrier_macro(carrier, "mode"), 1.0);
        }
        owner.set_base_param(&carrier_macro(&carrier_a, "pulse_strength"), 2.0);
        owner.set_base_param(&carrier_macro(&carrier_b, "pulse_strength"), 0.5);
        owner.refresh_manifest_from_graph();
    }

    let pre_save = output_dir.join("legacy-connected-multi-carrier.manifold");
    manifold_io::saver::save_project_v1(&project, &pre_save).expect("save legacy project");
    let mut reopened =
        manifold_io::loader::load_project(&pre_save).expect("reopen legacy project");
    let notices = crate::project_io::migrate_project_scene_graphs(&mut reopened);
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains("became 2 standalone Math View modifiers")),
        "per-carrier migration summary expected, got: {notices:?}"
    );

    let graph = generator_graph(&reopened, &layer_id);
    assert_eq!(
        graph.scene_modifiers.len(),
        4,
        "two carriers plus one view each: {:#?}",
        graph
            .scene_modifiers
            .iter()
            .map(|m| m.id.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(graph.scene_modifiers[0].id, carrier_a);
    assert_eq!(graph.scene_modifiers[2].id, carrier_b);
    let view_a = &graph.scene_modifiers[1];
    let view_b = &graph.scene_modifiers[3];
    for view in [view_a, view_b] {
        assert!(
            mv::is_math_view_recipe(&view.graph),
            "appended instance is the standalone Math View"
        );
        assert!(
            mv::math_view_connect_support(graph, &view.id).is_ok(),
            "migrated view keeps its authored Connect to Mesh: {:?}",
            mv::math_view_connect_support(graph, &view.id)
        );
    }
    // The migration association survives on both views.
    assert_eq!(view_a.legacy_math_view_carrier, Some(carrier_a.clone()));
    assert_eq!(view_b.legacy_math_view_carrier, Some(carrier_b.clone()));
    // Distinct authored settings moved with each carrier's bindings (binding
    // ids are unchanged by retargeting, so the carrier-keyed macros resolve).
    let owner = reopened
        .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
        .unwrap();
    assert_eq!(
        owner.get_base_param(&carrier_macro(&carrier_a, "pulse_strength")),
        2.0,
        "A's pulse strength lands on A's view"
    );
    assert_eq!(
        owner.get_base_param(&carrier_macro(&carrier_b, "pulse_strength")),
        0.5,
        "B's pulse strength lands on B's view"
    );

    // Preparation: each view's mask borrows its own carrier's patch params
    // and the masks compose into the scene objects' weights.
    let registry = manifold_renderer::node_graph::PrimitiveRegistry::with_builtin();
    let prepared =
        manifold_renderer::node_graph::scene_modifier_expand::prepare_scene_modifiers(
            graph, &registry,
        )
        .expect("connected migrated views prepare");
    let node_by_id = |id: u32| {
        prepared
            .def
            .nodes
            .iter()
            .find(|node| node.id == id)
            .expect("prepared node")
    };
    // Namespaced node ids are length-prefixed UTF-8 parts; decoding exposes
    // the provenance baked into them (e.g. the per-carrier coordinate
    // context that feeds each mask's source_offset).
    let decode_namespace = |node_id: &str| -> Vec<String> {
        let hex = node_id
            .strip_prefix("__scene_modifier_namespace_v1")
            .unwrap_or(node_id);
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("hex pair"))
            .collect();
        let mut parts = Vec::new();
        let mut pos = 0;
        while pos + 4 <= bytes.len() {
            let len = u32::from_be_bytes([
                bytes[pos],
                bytes[pos + 1],
                bytes[pos + 2],
                bytes[pos + 3],
            ]) as usize;
            pos += 4;
            if pos + len > bytes.len() {
                break;
            }
            parts.push(String::from_utf8_lossy(&bytes[pos..pos + len]).into_owned());
            pos += len;
        }
        parts
    };
    // View-resource masks live in the "math_view" namespace (observed first
    // namespace part); the carriers' internal recipe masks live in their own.
    let view_masks: Vec<&EffectGraphNode> = prepared
        .def
        .nodes
        .iter()
        .filter(|node| node.type_id == "node.mesh_spatial_mask")
        .filter(|node| {
            node.node_id
                .as_str()
                .starts_with("__scene_modifier_namespace_v1000000096d6174685f76696577")
        })
        .collect();
    assert_eq!(view_masks.len(), 4, "two views times two sampled objects");
    let mut from_a = 0;
    let mut from_b = 0;
    for mask in &view_masks {
        let cell_size = match mask.params.get("cell_size") {
            Some(SerializedParamValue::Float { value }) => *value,
            other => panic!("view mask borrows cell_size as a param, got {other:?}"),
        };
        let offset_wire = prepared
            .def
            .wires
            .iter()
            .find(|wire| wire.to_node == mask.id && wire.to_port == "source_offset_x")
            .expect("view mask borrows source_offset_x");
        // The offset source is the coordinate-context node of the mask's own
        // carrier; its name carries the carrier id.
        let offset_parts = decode_namespace(&node_by_id(offset_wire.from_node).node_id);
        assert_eq!(
            offset_parts.first().map(String::as_str),
            Some("context"),
            "offset provenance is a coordinate context: {offset_parts:?}"
        );
        let provenance = offset_parts.get(1).expect("context name");
        match cell_size {
            0.15 if provenance.contains(carrier_a.as_str()) => from_a += 1,
            0.3 if provenance.contains(carrier_b.as_str()) => from_b += 1,
            cell_size => panic!(
                "mask cell_size {cell_size} borrows offset from {provenance}, not its own carrier"
            ),
        }
    }
    assert_eq!(from_a, 2, "both A masks borrow A's patch params");
    assert_eq!(from_b, 2, "both B masks borrow B's patch params");
    // The connected masks compose: the first view's mask feeds the second's
    // weights input before the chain reaches the scene.
    let composes = view_masks.iter().any(|mask| {
        prepared.def.wires.iter().any(|wire| {
            wire.from_node == mask.id
                && wire.from_port == "weights"
                && view_masks.iter().any(|other| other.id == wire.to_node)
        })
    });
    assert!(composes, "connected view masks multiply into one chain");
    // Every view mask reaches both scene objects' weights inputs through the
    // prepared graph (via the carriers' fragment-cut remap chain).
    let mut reachable: std::collections::BTreeSet<u32> = Default::default();
    let mut stack: Vec<u32> = view_masks.iter().map(|mask| mask.id).collect();
    while let Some(id) = stack.pop() {
        if reachable.insert(id) {
            for wire in prepared.def.wires.iter().filter(|wire| wire.from_node == id) {
                stack.push(wire.to_node);
            }
        }
    }
    for frame in &frames {
        let object = prepared
            .def
            .nodes
            .iter()
            .find(|node| node.node_id == frame.target.node)
            .expect("sampled scene object");
        let weights_from = prepared
            .def
            .wires
            .iter()
            .find(|wire| wire.to_node == object.id && wire.to_port == "weights")
            .map(|wire| wire.from_node)
            .expect("object weights input");
        assert!(
            reachable.contains(&weights_from),
            "view masks feed the weights of {}",
            frame.target.node
        );
    }

    // Idempotence and save/reload stability carry the association.
    let once = serde_json::to_string(
        &reopened
            .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
            .unwrap()
            .graph,
    )
    .unwrap();
    let mut twice = reopened.clone();
    let notices = crate::project_io::migrate_project_scene_graphs(&mut twice);
    assert!(notices.is_empty(), "second pass adds no notices: {notices:?}");
    let twice_graph = serde_json::to_string(
        &twice
            .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
            .unwrap()
            .graph,
    )
    .unwrap();
    assert_eq!(once, twice_graph, "migration is idempotent");
    let post_save = output_dir.join("migrated-connected-multi-carrier.manifold");
    manifold_io::saver::save_project_v1(&reopened, &post_save).expect("save migrated project");
    let mut reloaded =
        manifold_io::loader::load_project(&post_save).expect("reload migrated project");
    let notices = crate::project_io::migrate_project_scene_graphs(&mut reloaded);
    assert!(
        notices.is_empty(),
        "migrated project reloads without new migration work: {notices:?}"
    );
    let reloaded_graph = generator_graph(&reloaded, &layer_id);
    assert_eq!(
        reloaded_graph.scene_modifiers[1].legacy_math_view_carrier,
        Some(carrier_a.clone()),
        "save/reload preserves A's carrier association"
    );
    assert_eq!(
        reloaded_graph.scene_modifiers[3].legacy_math_view_carrier,
        Some(carrier_b.clone()),
        "save/reload preserves B's carrier association"
    );

    // The migrated project drives the runtime: both views render.
    let mut ct = headless_content_thread(Project::default(), 320, 180);
    let (state_tx, _state_rx) = crossbeam_channel::unbounded::<ContentState>();
    ct.handle_command(ContentCommand::LoadProject(Box::new(reopened)));
    assert!(
        ct.graph_edit_diagnostic.is_none(),
        "migrated connected project rejected: {:?}",
        ct.graph_edit_diagnostic
    );
    ct.timer.set_frame_clocked(true);
    warm_project(&mut ct, &state_tx);
    ct.handle_command(ContentCommand::SeekToBeat(Beats(0.0)));
    ct.tick_frame(&state_tx);
    ct.tick_frame(&state_tx);
    let (_, migrated_nonzero) =
        capture_output_allow_uniform(&ct, &output_dir.join("migrated-connected-views.png"));
    assert!(migrated_nonzero > 0, "migrated connected Math Views render");
}

/// Modifier-limit overflow (BUG-ty86): nine authored legacy carriers become
/// 18 modifiers and must prepare, round-trip, and keep every carrier's
/// authored value on its own view.
#[test]
fn legacy_math_view_nine_carrier_capacity_journey() {
    use manifold_core::scene_modifier_math_view as mv;
    let output_dir = PathBuf::from("target/journey-proofs/math-view-nine-carrier");
    std::fs::create_dir_all(&output_dir).expect("migration artifact directory");
    let (mut project, layer_id, frames, scene) = legacy_carrier_project();
    let carriers: Vec<NodeId> = (0..9)
        .map(|index| NodeId::new(format!("legacy_vortex_{index}")))
        .collect();
    for (index, carrier) in carriers.iter().enumerate() {
        push_modifier(
            &mut project,
            &layer_id,
            carrier,
            &scene,
            frames.clone(),
            legacy_vortex_carrier_graph(),
            true,
        );
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let owner = project.graph_target_owner_mut(&target).unwrap();
        owner.set_base_param(&carrier_macro(carrier, "mode"), 1.0);
        owner.set_base_param(&carrier_macro(carrier, "scan_amount"), index as f32 / 10.0);
        owner.refresh_manifest_from_graph();
    }

    let pre_save = output_dir.join("legacy-nine-carrier.manifold");
    manifold_io::saver::save_project_v1(&project, &pre_save).expect("save legacy project");
    let mut reopened =
        manifold_io::loader::load_project(&pre_save).expect("reopen legacy project");
    let notices = crate::project_io::migrate_project_scene_graphs(&mut reopened);
    assert!(
        notices.iter().any(|notice| {
            notice.contains("9 legacy Math View sections became 9 standalone Math View modifiers")
        }),
        "per-carrier migration summary expected, got: {notices:?}"
    );

    let graph = generator_graph(&reopened, &layer_id);
    assert_eq!(
        graph.scene_modifiers.len(),
        18,
        "nine stripped carriers plus nine views"
    );
    let owner = reopened
        .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
        .unwrap();
    for (index, carrier) in carriers.iter().enumerate() {
        assert_eq!(graph.scene_modifiers[index * 2].id, *carrier);
        let view = &graph.scene_modifiers[index * 2 + 1];
        assert!(
            mv::is_math_view_recipe(&view.graph),
            "view after {carrier} is the standalone Math View"
        );
        assert_eq!(
            view.legacy_math_view_carrier,
            Some(carrier.clone()),
            "view records its carrier"
        );
        assert_eq!(
            owner.get_base_param(&carrier_macro(carrier, "scan_amount")),
            index as f32 / 10.0,
            "{carrier}'s authored scan amount lands on its own view"
        );
    }
    // The 18-modifier graph prepares: 9 stage carriers + 9 views, each under
    // its own cap.
    let registry = manifold_renderer::node_graph::PrimitiveRegistry::with_builtin();
    manifold_renderer::node_graph::scene_modifier_expand::prepare_scene_modifiers(
        graph,
        &registry,
    )
    .expect("nine migrated views plus nine carriers prepare");

    // Save/reload round trip at 18 modifiers.
    let post_save = output_dir.join("migrated-nine-carrier.manifold");
    manifold_io::saver::save_project_v1(&reopened, &post_save).expect("save migrated project");
    let mut reloaded =
        manifold_io::loader::load_project(&post_save).expect("reload migrated project");
    let notices = crate::project_io::migrate_project_scene_graphs(&mut reloaded);
    assert!(
        notices.is_empty(),
        "migrated project reloads without new migration work: {notices:?}"
    );
    let once = serde_json::to_string(
        &reopened
            .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
            .unwrap()
            .graph,
    )
    .unwrap();
    let reloaded_graph = serde_json::to_string(
        &reloaded
            .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
            .unwrap()
            .graph,
    )
    .unwrap();
    assert_eq!(once, reloaded_graph, "save/reload preserves the 18-modifier graph");
}

/// View-cap fallback: when appending another view would exceed the Math View
/// cap, migration preserves the carrier's embedded controls instead of
/// dropping them, and the graph stays executable.
#[test]
fn legacy_math_view_view_cap_preservation_journey() {
    use manifold_core::scene_modifier_math_view as mv;
    let output_dir = PathBuf::from("target/journey-proofs/math-view-cap-preservation");
    std::fs::create_dir_all(&output_dir).expect("migration artifact directory");
    let (mut project, layer_id, frames, scene) = legacy_carrier_project();
    // 14 pre-existing standalone views, then 3 authored carriers: only two
    // views fit under the 16-view cap.
    for index in 0..14 {
        push_modifier(
            &mut project,
            &layer_id,
            &NodeId::new(format!("existing_view_{index}")),
            &scene,
            frames.clone(),
            math_view_recipe(),
            false,
        );
    }
    let carriers: Vec<NodeId> = (0..3)
        .map(|index| NodeId::new(format!("legacy_vortex_{index}")))
        .collect();
    for carrier in &carriers {
        push_modifier(
            &mut project,
            &layer_id,
            carrier,
            &scene,
            frames.clone(),
            legacy_vortex_carrier_graph(),
            true,
        );
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let owner = project.graph_target_owner_mut(&target).unwrap();
        owner.set_base_param(&carrier_macro(carrier, "mode"), 1.0);
        owner.refresh_manifest_from_graph();
    }

    let mut migrated = project.clone();
    let notices = crate::project_io::migrate_project_scene_graphs(&mut migrated);
    assert!(
        notices.iter().any(|notice| notice.contains("Math View limit is reached")),
        "capacity fallback names the limit, got: {notices:?}"
    );
    assert!(
        notices.iter().any(|notice| {
            notice.contains("preserved embedded controls on 1 modifier(s)")
        }),
        "preservation summary expected, got: {notices:?}"
    );

    let graph = generator_graph(&migrated, &layer_id);
    let view_count = graph
        .scene_modifiers
        .iter()
        .filter(|m| mv::is_math_view_recipe(&m.graph))
        .count();
    assert_eq!(view_count, mv::MAX_MATH_VIEW_MODIFIERS, "view cap is filled");
    let last_carrier = graph
        .scene_modifiers
        .iter()
        .find(|m| m.id == carriers[2])
        .expect("preserved carrier stays in the chain");
    assert!(
        mv::has_legacy_math_view_controls(&last_carrier.graph),
        "the carrier that hit the cap keeps its embedded controls"
    );
    // The mixed graph prepares: 3 stage carriers under the stage cap, 16
    // views at the view cap.
    let registry = manifold_renderer::node_graph::PrimitiveRegistry::with_builtin();
    manifold_renderer::node_graph::scene_modifier_expand::prepare_scene_modifiers(
        graph,
        &registry,
    )
    .expect("capped graph prepares");

    // A second pass is graph-stable: the preserved carrier retries, fails the
    // same way, and adds no views.
    let once = serde_json::to_string(
        &migrated
            .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
            .unwrap()
            .graph,
    )
    .unwrap();
    let mut twice = migrated.clone();
    let _ = crate::project_io::migrate_project_scene_graphs(&mut twice);
    let twice_graph = serde_json::to_string(
        &twice
            .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
            .unwrap()
            .graph,
    )
    .unwrap();
    assert_eq!(once, twice_graph, "retrying migration changes nothing");
}

/// A failed migration keeps the original graph executable (BUG-ty86
/// acceptance): an authored carrier whose mesh frames are gone cannot gain a
/// view, so its embedded controls are preserved and the single-modifier
/// graph still prepares.
#[test]
fn legacy_math_view_failed_migration_stays_executable_journey() {
    use manifold_core::scene_modifier_math_view as mv;
    let output_dir = PathBuf::from("target/journey-proofs/math-view-failed-migration");
    std::fs::create_dir_all(&output_dir).expect("migration artifact directory");
    let (mut project, layer_id, frames, scene) = legacy_carrier_project();
    let carrier = NodeId::new("legacy_vortex_frames_lost");
    // Authored content but no mesh frames: the view cannot be created.
    push_modifier(
        &mut project,
        &layer_id,
        &carrier,
        &scene,
        Vec::new(),
        legacy_vortex_carrier_graph(),
        true,
    );
    {
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let owner = project.graph_target_owner_mut(&target).unwrap();
        owner.set_base_param(&carrier_macro(&carrier, "mode"), 1.0);
        owner.refresh_manifest_from_graph();
    }

    let mut migrated = project.clone();
    let notices = crate::project_io::migrate_project_scene_graphs(&mut migrated);
    assert!(
        notices.iter().any(|notice| notice.contains("no mesh frames")),
        "skip reason named, got: {notices:?}"
    );
    let graph = generator_graph(&migrated, &layer_id);
    assert_eq!(graph.scene_modifiers.len(), 1, "no view appended");
    assert!(
        mv::has_legacy_math_view_controls(&graph.scene_modifiers[0].graph),
        "embedded controls preserved"
    );
    // The deformation recipe is intact. A frame-less Vortex was never
    // preparable (frames must match the selection), so the honest
    // executability claim is that migration adds no NEW failure: the prepare
    // error is the same frames error before and after.
    assert!(
        graph.scene_modifiers[0]
            .graph
            .preset_metadata
            .as_ref()
            .unwrap()
            .bindings
            .iter()
            .any(|binding| binding.id == "orbit"),
        "authored deformation survives"
    );
    let registry = manifold_renderer::node_graph::PrimitiveRegistry::with_builtin();
    let prepare_error = |project: &Project| {
        let graph = generator_graph(project, &layer_id);
        manifold_renderer::node_graph::scene_modifier_expand::prepare_scene_modifiers(
            graph,
            &registry,
        )
        .map(|_| String::new())
        .map_err(|error| format!("{error:?}"))
    };
    let before = prepare_error(&project).expect_err("frame-less carrier does not prepare");
    let after = prepare_error(&migrated).expect_err("frame-less carrier still does not prepare");
    assert_eq!(
        before, after,
        "migration adds no new executability failure: {after}"
    );
    let _ = frames;
}

/// Embedded control values without host bindings (compatibility audit b): a
/// carrier saved before a control's binding was minted carries the value on
/// the node alone. Migration must copy it onto the new view; where a host
/// binding exists, the host value still wins.
#[test]
fn legacy_math_view_embedded_value_migration_journey() {
    use manifold_core::scene_modifier_math_view as mv;
    let output_dir = PathBuf::from("target/journey-proofs/math-view-embedded-value");
    std::fs::create_dir_all(&output_dir).expect("migration artifact directory");
    let (mut project, layer_id, frames, scene) = legacy_carrier_project();
    let carrier = NodeId::new("legacy_vortex_embedded");
    push_modifier(
        &mut project,
        &layer_id,
        &carrier,
        &scene,
        frames,
        legacy_vortex_carrier_graph(),
        true,
    );
    let density_macro = carrier_macro(&carrier, "density");
    {
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let owner = project.graph_target_owner_mut(&target).unwrap();
        // Density: authored on the embedded node, binding removed — the save
        // predates the minted binding.
        let graph = owner.graph.as_mut().unwrap();
        let instance = graph
            .scene_modifiers
            .iter_mut()
            .find(|m| m.id == carrier)
            .expect("carrier");
        let density = instance
            .graph
            .nodes
            .iter_mut()
            .find(|node| node.node_id == "__math_view_density")
            .expect("density control node");
        density.params.insert(
            "value".into(),
            SerializedParamValue::Float { value: 5.0 },
        );
        let metadata = graph.preset_metadata.as_mut().unwrap();
        metadata.bindings.retain(|binding| binding.id != density_macro);
        metadata.params.retain(|param| param.id != density_macro);
        // Mode: authored through its host binding, which must win.
        owner.set_base_param(&carrier_macro(&carrier, "mode"), 1.0);
        owner.params.remove(density_macro.as_str());
        owner.refresh_manifest_from_graph();
    }

    let mut migrated = project.clone();
    let notices = crate::project_io::migrate_project_scene_graphs(&mut migrated);
    assert!(
        notices.is_empty(),
        "clean migration expected, got: {notices:?}"
    );
    let graph = generator_graph(&migrated, &layer_id);
    assert_eq!(graph.scene_modifiers.len(), 2, "carrier plus one view");
    let view = &graph.scene_modifiers[1];
    assert!(mv::is_math_view_recipe(&view.graph));
    let density_node = view
        .graph
        .nodes
        .iter()
        .find(|node| node.node_id == "__math_view_density")
        .expect("view density control");
    assert_eq!(
        density_node.params.get("value"),
        Some(&SerializedParamValue::Float { value: 5.0 }),
        "embedded density copied onto the view's control node"
    );
    let owner = migrated
        .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
        .unwrap();
    assert_eq!(
        owner.get_base_param(&carrier_macro(&view.id, "density")),
        5.0,
        "embedded density becomes the view's host base value"
    );
    assert_eq!(
        owner.get_base_param(&carrier_macro(&carrier, "mode")),
        1.0,
        "host-bound mode still wins over the embedded default"
    );

    // Save/reload keeps the carried value.
    let post_save = output_dir.join("migrated-embedded-value.manifold");
    manifold_io::saver::save_project_v1(&migrated, &post_save).expect("save migrated project");
    let mut reloaded =
        manifold_io::loader::load_project(&post_save).expect("reload migrated project");
    let notices = crate::project_io::migrate_project_scene_graphs(&mut reloaded);
    assert!(notices.is_empty(), "reload adds no notices: {notices:?}");
    let reloaded_owner = reloaded
        .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
        .unwrap();
    let reloaded_view = &generator_graph(&reloaded, &layer_id).scene_modifiers[1];
    assert_eq!(
        reloaded_owner.get_base_param(&carrier_macro(&reloaded_view.id, "density")),
        5.0,
        "save/reload keeps the carried embedded value"
    );
}

/// Reusable-view guards (compatibility audit c): retargeting a carrier's
/// bindings onto an EXISTING standalone view is allowed only when the view is
/// default, immediately follows the carrier, samples the same objects
/// (targets and mesh frames) and holds no host Math View bindings of its
/// own; a reused view inherits the carrier association. Otherwise a fresh
/// view is appended and the existing view, the carrier's selection and its
/// chain position are untouched.
#[test]
fn legacy_math_view_reuse_guards_migration_journey() {
    use manifold_core::scene_modifier_math_view as mv;
    let layer_id = LayerId::new("math-grid");
    let scene = SceneNodeRef {
        scope: vec![],
        node: NodeId::new("scan_render"),
    };

    // Allowed: a default, unreconciled view immediately after the carrier.
    let (mut project, project_layer, frames, project_scene) = legacy_carrier_project();
    assert_eq!(project_layer, layer_id);
    let carrier = NodeId::new("legacy_vortex_reuse_ok");
    push_modifier(
        &mut project,
        &layer_id,
        &carrier,
        &project_scene,
        frames.clone(),
        legacy_vortex_carrier_graph(),
        true,
    );
    {
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let owner = project.graph_target_owner_mut(&target).unwrap();
        owner.set_base_param(&carrier_macro(&carrier, "mode"), 1.0);
        owner.refresh_manifest_from_graph();
    }
    let existing_view = NodeId::new("existing_view_ok");
    push_modifier(
        &mut project,
        &layer_id,
        &existing_view,
        &scene,
        frames.clone(),
        math_view_recipe(),
        false,
    );
    let mut migrated = project.clone();
    let notices = crate::project_io::migrate_project_scene_graphs(&mut migrated);
    assert!(notices.is_empty(), "clean reuse, got: {notices:?}");
    let graph = generator_graph(&migrated, &layer_id);
    assert_eq!(graph.scene_modifiers.len(), 2, "existing view absorbs the section, no append");
    assert_eq!(graph.scene_modifiers[0].id, carrier, "carrier keeps its chain position");
    assert_eq!(graph.scene_modifiers[1].id, existing_view);
    let metadata = graph.preset_metadata.as_ref().unwrap();
    let mode_binding = metadata
        .bindings
        .iter()
        .find(|binding| binding.id == carrier_macro(&carrier, "mode"))
        .expect("mode binding survives");
    assert!(
        matches!(
            &mode_binding.target,
            BindingTarget::SceneModifier { modifier_id, param_id }
                if modifier_id == &existing_view && param_id == "math_view_mode"
        ),
        "carrier binding retargeted onto the existing view"
    );
    assert_eq!(
        graph.scene_modifiers[1].legacy_math_view_carrier,
        Some(carrier.clone()),
        "the reused view inherits the carrier association for Connect to Mesh"
    );

    // Rejected: the existing view is authored (non-default embedded value).
    let (mut project, project_layer, frames, project_scene) = legacy_carrier_project();
    assert_eq!(project_layer, layer_id);
    let carrier = NodeId::new("legacy_vortex_reuse_authored");
    push_modifier(
        &mut project,
        &layer_id,
        &carrier,
        &project_scene,
        frames.clone(),
        legacy_vortex_carrier_graph(),
        true,
    );
    {
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let owner = project.graph_target_owner_mut(&target).unwrap();
        owner.set_base_param(&carrier_macro(&carrier, "mode"), 1.0);
        owner.refresh_manifest_from_graph();
    }
    let authored_view = NodeId::new("existing_view_authored");
    let mut authored_view_graph = math_view_recipe();
    authored_view_graph.nodes.iter_mut().find(|node| {
        node.node_id == "__math_view_density"
    }).expect("density control").params.insert(
        "value".into(),
        SerializedParamValue::Float { value: 5.0 },
    );
    push_modifier(
        &mut project,
        &layer_id,
        &authored_view,
        &scene,
        frames.clone(),
        authored_view_graph,
        false,
    );
    let mut migrated = project.clone();
    let _ = crate::project_io::migrate_project_scene_graphs(&mut migrated);
    let graph = generator_graph(&migrated, &layer_id);
    assert_eq!(
        graph.scene_modifiers.len(),
        3,
        "fresh view appended, authored view kept"
    );
    assert_eq!(graph.scene_modifiers[0].id, carrier, "carrier keeps its chain position");
    assert!(
        mv::is_math_view_recipe(&graph.scene_modifiers[1].graph),
        "fresh view appended immediately after the carrier"
    );
    assert_eq!(graph.scene_modifiers[2].id, authored_view);
    let kept = &graph.scene_modifiers[2];
    assert_eq!(
        kept.graph
            .nodes
            .iter()
            .find(|node| node.node_id == "__math_view_density")
            .and_then(|node| node.params.get("value")),
        Some(&SerializedParamValue::Float { value: 5.0 }),
        "the existing view's authored settings are not overwritten"
    );

    // Rejected: the existing view does not immediately follow the carrier.
    let (mut project, project_layer, frames, project_scene) = legacy_carrier_project();
    assert_eq!(project_layer, layer_id);
    let carrier = NodeId::new("legacy_vortex_reuse_distant");
    push_modifier(
        &mut project,
        &layer_id,
        &carrier,
        &project_scene,
        frames.clone(),
        legacy_vortex_carrier_graph(),
        true,
    );
    {
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let owner = project.graph_target_owner_mut(&target).unwrap();
        owner.set_base_param(&carrier_macro(&carrier, "mode"), 1.0);
        owner.refresh_manifest_from_graph();
    }
    let other = NodeId::new("unrelated_modifier");
    push_modifier(
        &mut project,
        &layer_id,
        &other,
        &scene,
        frames.clone(),
        vortex_recipe(),
        true,
    );
    let distant_view = NodeId::new("existing_view_distant");
    push_modifier(
        &mut project,
        &layer_id,
        &distant_view,
        &scene,
        frames.clone(),
        math_view_recipe(),
        false,
    );
    let mut migrated = project.clone();
    let _ = crate::project_io::migrate_project_scene_graphs(&mut migrated);
    let graph = generator_graph(&migrated, &layer_id);
    assert_eq!(graph.scene_modifiers.len(), 4, "fresh view appended after the carrier");
    let ids: Vec<NodeId> = graph.scene_modifiers.iter().map(|m| m.id.clone()).collect();
    assert_eq!(ids, vec![carrier.clone(), ids[1].clone(), other, distant_view]);
    assert!(
        mv::is_math_view_recipe(&graph.scene_modifiers[1].graph),
        "the fresh view sits immediately after the carrier"
    );

    // Rejected: the existing view holds its own host Math View bindings —
    // retargeted carrier bindings would target the same params twice.
    let (mut project, project_layer, frames, project_scene) = legacy_carrier_project();
    assert_eq!(project_layer, layer_id);
    let carrier = NodeId::new("legacy_vortex_reuse_bound");
    push_modifier(
        &mut project,
        &layer_id,
        &carrier,
        &project_scene,
        frames.clone(),
        legacy_vortex_carrier_graph(),
        true,
    );
    {
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let owner = project.graph_target_owner_mut(&target).unwrap();
        owner.set_base_param(&carrier_macro(&carrier, "mode"), 1.0);
        owner.refresh_manifest_from_graph();
    }
    let bound_view = NodeId::new("existing_view_bound");
    push_modifier(
        &mut project,
        &layer_id,
        &bound_view,
        &scene,
        frames.clone(),
        math_view_recipe(),
        true,
    );
    let mut migrated = project.clone();
    let _ = crate::project_io::migrate_project_scene_graphs(&mut migrated);
    let graph = generator_graph(&migrated, &layer_id);
    assert_eq!(
        graph.scene_modifiers.len(),
        3,
        "a bound existing view is not reused; a fresh view is appended"
    );
    assert_eq!(graph.scene_modifiers[0].id, carrier, "carrier keeps its place");
    assert!(
        mv::is_math_view_recipe(&graph.scene_modifiers[1].graph)
            && graph.scene_modifiers[1].id != bound_view,
        "fresh view appended immediately after the carrier"
    );
    assert_eq!(
        graph.scene_modifiers[2].id, bound_view,
        "the bound existing view is pushed behind the fresh one, bindings intact"
    );
    let metadata = graph.preset_metadata.as_ref().unwrap();
    let targets_for = |view: &NodeId| {
        metadata
            .bindings
            .iter()
            .filter(|binding| {
                matches!(
                    &binding.target,
                    BindingTarget::SceneModifier { modifier_id, param_id }
                        if modifier_id == view && param_id.starts_with("math_view_")
                )
            })
            .count()
    };
    assert_eq!(
        targets_for(&bound_view),
        mv::CONTROLS.len(),
        "the existing view keeps exactly its own bindings"
    );
    let fresh_view = graph.scene_modifiers[1].id.clone();
    assert_eq!(
        targets_for(&fresh_view),
        mv::CONTROLS.len() + 1,
        "the fresh view carries the carrier's bindings, including hidden Scope"
    );
    // Rejected: the existing view samples different objects than the carrier
    // — reusing it would move the carrier's bindings onto the wrong geometry
    // and the authored connection would degrade to presentation-only.
    let (mut project, project_layer, frames, project_scene) = legacy_carrier_project();
    assert_eq!(project_layer, layer_id);
    let carrier = NodeId::new("legacy_vortex_reuse_frames_mismatch");
    push_modifier(
        &mut project,
        &layer_id,
        &carrier,
        &project_scene,
        frames.clone(),
        legacy_vortex_carrier_graph(),
        true,
    );
    {
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let owner = project.graph_target_owner_mut(&target).unwrap();
        owner.set_base_param(&carrier_macro(&carrier, "mode"), 1.0);
        owner.refresh_manifest_from_graph();
    }
    let mismatched_view = NodeId::new("existing_view_frames_mismatch");
    push_modifier(
        &mut project,
        &layer_id,
        &mismatched_view,
        &scene,
        vec![frames[0].clone()],
        math_view_recipe(),
        false,
    );
    let mut migrated = project.clone();
    let _ = crate::project_io::migrate_project_scene_graphs(&mut migrated);
    let graph = generator_graph(&migrated, &layer_id);
    assert_eq!(
        graph.scene_modifiers.len(),
        3,
        "a view sampling different objects is not reused; a fresh view is appended"
    );
    assert_eq!(graph.scene_modifiers[0].id, carrier, "carrier keeps its chain position");
    assert!(
        mv::is_math_view_recipe(&graph.scene_modifiers[1].graph)
            && graph.scene_modifiers[1].id != mismatched_view,
        "fresh view appended immediately after the carrier"
    );
    assert_eq!(
        graph.scene_modifiers[1].mesh_frames, frames,
        "the fresh view samples the carrier's objects"
    );
    assert_eq!(graph.scene_modifiers[2].id, mismatched_view);
    assert_eq!(
        graph.scene_modifiers[2].mesh_frames,
        vec![frames[0].clone()],
        "the existing view's own sampling is untouched"
    );
}

/// Scope remains an animated hidden binding after migration and save/reload.
#[test]
fn legacy_math_view_isolated_scope_preservation_journey() {
    use manifold_core::scene_modifier_math_view as mv;
    let (mut project, layer_id, frames, project_scene) = legacy_carrier_project();
    // A plain modifier precedes the carrier in the same scene.
    push_modifier(
        &mut project,
        &layer_id,
        &NodeId::new("preceding_modifier"),
        &project_scene,
        frames.clone(),
        vortex_recipe(),
        true,
    );
    let carrier = NodeId::new("legacy_vortex_isolated_scope");
    push_modifier(
        &mut project,
        &layer_id,
        &carrier,
        &project_scene,
        frames,
        legacy_vortex_carrier_graph(),
        true,
    );
    {
        let target = manifold_core::GraphTarget::Generator(layer_id.clone());
        let owner = project.graph_target_owner_mut(&target).unwrap();
        owner.set_base_param(&carrier_macro(&carrier, "mode"), 1.0);
        owner.set_base_param(&carrier_macro(&carrier, "scope"), 0.0);
        owner.drivers_mut().push(ParameterDriver::new(
            ParamId::from(carrier_macro(&carrier, "scope")),
            BeatDivision::Whole,
            DriverWaveform::Sine,
        ));
        owner.refresh_manifest_from_graph();
    }
    let mut migrated = project.clone();
    let notices = crate::project_io::migrate_project_scene_graphs(&mut migrated);
    assert!(notices.is_empty(), "Scope is preserved: {notices:?}");
    let graph = generator_graph(&migrated, &layer_id);
    assert_eq!(graph.scene_modifiers.len(), 3, "preceding modifier, carrier, view");
    assert!(
        !mv::has_legacy_math_view_controls(&graph.scene_modifiers[1].graph),
        "carrier stripped"
    );
    let scope_macro = carrier_macro(&carrier, "scope");
    assert!(
        graph
            .preset_metadata
            .as_ref()
            .unwrap()
            .bindings
            .iter()
            .any(|binding| binding.id == scope_macro && matches!(&binding.target,
                BindingTarget::SceneModifier { modifier_id, param_id }
                    if modifier_id == &graph.scene_modifiers[2].id && param_id == "math_view_scope")),
        "scope binding retargets to the view"
    );
    let view = &graph.scene_modifiers[2];
    assert_eq!(view.legacy_math_view_carrier.as_ref(), Some(&carrier));
    assert!(mv::has_legacy_scope_control(&view.graph));
    assert!(!view.graph.preset_metadata.as_ref().unwrap().params.iter()
        .find(|param| param.id == "math_view_scope").unwrap().card_visible);
    let target = manifold_core::GraphTarget::Generator(layer_id.clone());
    let owner = migrated.graph_target_owner(&target).unwrap();
    assert_eq!(owner.get_base_param(&scope_macro), 0.0);
    assert!(owner.drivers.as_ref().unwrap().iter().any(|driver| driver.param_id.as_ref() == scope_macro));
    let dir = PathBuf::from("target/journey-proofs/math-view-scope");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("migrated-scope.manifold");
    manifold_io::saver::save_project_v1(&migrated, &path).unwrap();
    let mut reloaded = manifold_io::loader::load_project(&path).unwrap();
    assert!(crate::project_io::migrate_project_scene_graphs(&mut reloaded).is_empty());
    let reloaded_owner = reloaded.graph_target_owner(&target).unwrap();
    assert_eq!(serde_json::to_value(owner).unwrap(), serde_json::to_value(reloaded_owner).unwrap());
}
/// Historical partial control sets (BUG-t8at): the real bundled Vortex
/// snapshots from each legacy era detect as carriers, migrate to a standalone
/// view with every current control declared and host-bound, carry their saved
/// values, fill absent controls with the current defaults, and stay stable
/// across a second migration pass and a save/reload round trip.
#[test]
fn legacy_math_view_historical_snapshots_migration_journey() {
    use manifold_core::scene_modifier_math_view as mv;
    let output_dir = PathBuf::from("target/journey-proofs/math-view-migration-historical");
    std::fs::create_dir_all(&output_dir).expect("migration artifact directory");
    let layer_id = LayerId::new("math-grid");
    let carrier_id = NodeId::new("legacy_vortex");

    let fixture_dir = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../manifold-core/tests/fixtures/math-view-legacy/"
    );
    // (fixture, era, configure carrier host state)
    let cases: &[(&str, &str)] = &[
        ("vortex-fragments-initial-ce78a59d0.json", "initial"),
        ("vortex-fragments-events-96c78f522.json", "events"),
        ("vortex-fragments-occlusion-9f453beb0.json", "occlusion"),
    ];
    for (file, era) in cases {
        let carrier_graph: EffectGraphDef =
            serde_json::from_str(&std::fs::read_to_string(format!("{fixture_dir}{file}")).expect(
                "historical fixture readable",
            ))
            .expect("historical fixture parses");
        assert!(
            mv::has_legacy_math_view_controls(&carrier_graph),
            "{era} fixture detects as a legacy carrier"
        );

        let mut project = math_view_project();
        {
            let target = manifold_core::GraphTarget::Generator(layer_id.clone());
            let owner = project.graph_target_owner_mut(&target).unwrap();
            let graph = owner.graph.as_mut().unwrap();
            let frames = graph.scene_modifiers[0].mesh_frames.clone();
            let scene = SceneNodeRef {
                scope: vec![],
                node: NodeId::new("scan_render"),
            };
            graph.scene_modifiers.clear();
            graph.scene_modifiers.push(SceneModifierInstanceDef {
                id: carrier_id.clone(),
                scene,
                targets: SceneTargetSelection::AllObjects,
                mesh_frames: frames,
                legacy_math_view_carrier: None,
                graph: Box::new(carrier_graph),
            });
            let metadata = graph.preset_metadata.as_mut().unwrap();
            metadata.bindings.retain(|binding| {
                !matches!(&binding.target, BindingTarget::SceneModifier { .. })
            });
            metadata
                .params
                .retain(|param| !param.id.starts_with("sceneModifier:"));
            *graph = manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(
                graph,
                &carrier_id,
            )
            .expect("reconcile historical carrier")
            .graph;
            owner.refresh_manifest_from_graph();
            // Non-default values prove carriage; the events era also connects
            // to its patch carrier and animates the pulse trigger.
            let mode_macro = format!("sceneModifier:[\"{carrier_id}\",\"math_view_mode\"]");
            owner.set_base_param(&mode_macro, 1.0);
            if *era == "initial" {
                let density =
                    format!("sceneModifier:[\"{carrier_id}\",\"math_view_density\"]");
                owner.set_base_param(&density, 5.0);
            }
            if *era == "events" {
                let connect =
                    format!("sceneModifier:[\"{carrier_id}\",\"math_view_connect_mesh\"]");
                owner.set_base_param(&connect, 1.0);
                let pulse_trigger =
                    format!("sceneModifier:[\"{carrier_id}\",\"math_view_pulse_trigger\"]");
                owner.drivers_mut().push(ParameterDriver::new(
                    ParamId::from(pulse_trigger.clone()),
                    BeatDivision::Quarter,
                    DriverWaveform::Sine,
                ));
            }
            owner.refresh_manifest_from_graph();
        }

        let pre_save = output_dir.join(format!("legacy-{era}.manifold"));
        manifold_io::saver::save_project_v1(&project, &pre_save).expect("save legacy project");
        let mut reopened =
            manifold_io::loader::load_project(&pre_save).expect("reopen legacy project");
        let notices = crate::project_io::migrate_project_scene_graphs(&mut reopened);
        assert!(
            notices.is_empty(),
            "{era}: clean single-carrier migration, got: {notices:?}"
        );

        let graph = generator_graph(&reopened, &layer_id);
        assert_eq!(
            graph.scene_modifiers.len(),
            2,
            "{era}: carrier plus one appended view"
        );
        assert!(
            !mv::has_legacy_math_view_controls(&graph.scene_modifiers[0].graph),
            "{era}: carrier stripped"
        );
        let view = &graph.scene_modifiers[1];
        assert!(
            mv::is_math_view_recipe(&view.graph),
            "{era}: appended instance is the standalone Math View"
        );
        let owner = reopened
            .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
            .unwrap();
        let metadata = graph.preset_metadata.as_ref().unwrap();
        let declared: std::collections::HashSet<&str> = view
            .graph
            .preset_metadata
            .as_ref()
            .unwrap()
            .params
            .iter()
            .map(|param| param.id.as_str())
            .collect();
        for (suffix, _, default, ..) in mv::CONTROLS {
            let local = format!("math_view_{suffix}");
            assert!(
                declared.contains(local.as_str()),
                "{era}: view declares {local}"
            );
            let binding = metadata
                .bindings
                .iter()
                .find(|binding| {
                    matches!(
                        &binding.target,
                        BindingTarget::SceneModifier { modifier_id, param_id }
                            if modifier_id == &view.id && param_id == &local
                    )
                })
                .unwrap_or_else(|| panic!("{era}: view is host-bound for {local}"));
            assert_eq!(
                binding.default_value,
                *default,
                "{era}: absent controls fill with the current default ({local})"
            );
        }
        let mode_macro = format!("sceneModifier:[\"{carrier_id}\",\"math_view_mode\"]");
        let mode_binding = metadata
            .bindings
            .iter()
            .find(|binding| binding.id == mode_macro)
            .expect("mode binding survives");
        assert!(
            matches!(
                &mode_binding.target,
                BindingTarget::SceneModifier { modifier_id, param_id }
                    if modifier_id == &view.id && param_id == "math_view_mode"
            ),
            "{era}: mode binding retargeted"
        );
        assert_eq!(
            owner.get_base_param(&mode_macro),
            1.0,
            "{era}: saved Mode value survives"
        );
        if *era == "initial" {
            let density = format!("sceneModifier:[\"{carrier_id}\",\"math_view_density\"]");
            assert_eq!(
                owner.get_base_param(&density),
                5.0,
                "{era}: saved Density value survives"
            );
        }
        if *era == "events" {
            // The connected mask contract holds on the migrated view: exactly
            // one preceding patch carrier covers every sampled object, and
            // the pulse driver is still keyed by its unchanged binding id.
            assert!(
                mv::math_view_connect_support(graph, &view.id).is_ok(),
                "{era}: migrated connected mask contract holds"
            );
            let pulse_trigger =
                format!("sceneModifier:[\"{carrier_id}\",\"math_view_pulse_trigger\"]");
            assert!(
                owner
                    .drivers
                    .as_ref()
                    .expect("drivers survive")
                    .iter()
                    .any(|driver| driver.param_id.as_ref() == pulse_trigger.as_str()),
                "{era}: pulse driver still keyed by the unchanged binding id"
            );
        }

        // Idempotence and save/reload stability.
        let once = serde_json::to_string(
            &reopened
                .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
                .unwrap()
            .graph,
        )
        .unwrap();
        let mut twice = reopened.clone();
        let notices = crate::project_io::migrate_project_scene_graphs(&mut twice);
        assert!(
            notices.is_empty(),
            "{era}: second pass adds no notices: {notices:?}"
        );
        assert_eq!(
            once,
            serde_json::to_string(
                &twice
                    .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
                    .unwrap()
            .graph,
            )
            .unwrap(),
            "{era}: migration is idempotent"
        );
        let post_save = output_dir.join(format!("migrated-{era}.manifold"));
        manifold_io::saver::save_project_v1(&reopened, &post_save)
            .expect("save migrated project");
        let mut reloaded =
            manifold_io::loader::load_project(&post_save).expect("reload migrated project");
        let notices = crate::project_io::migrate_project_scene_graphs(&mut reloaded);
        assert!(
            notices.is_empty(),
            "{era}: migrated project reloads without new migration work: {notices:?}"
        );
        assert_eq!(
            once,
            serde_json::to_string(
                &reloaded
                    .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
                    .unwrap()
            .graph,
            )
            .unwrap(),
            "{era}: save/reload preserves the migrated graph"
        );
    }
}
