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
use manifold_core::effects::{ParamId, ParameterDriver};
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

/// Legacy projects embedded Math View controls in the Vortex recipe. Loading
/// moves them onto one appended standalone Math View modifier: host binding
/// ids (and therefore values, animation and modulation) survive, the retired
/// Scope control is dropped, and a second migration pass is a no-op.
#[test]
fn legacy_math_view_migration_journey() {
    let output_dir = PathBuf::from("target/journey-proofs/math-view-migration");
    std::fs::create_dir_all(&output_dir).expect("migration artifact directory");
    let layer_id = LayerId::new("math-grid");
    let carrier_id = NodeId::new("legacy_vortex");

    // Build the pre-standalone shape: the current (stripped) Vortex recipe
    // plus the legacy embedded control section, including retired Scope.
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
        // A non-default Mode proves value preservation; Scope proves removal.
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
    // Retired Scope is gone from metadata and manifest.
    let scope_macro = format!("sceneModifier:[\"{carrier_id}\",\"math_view_scope\"]");
    assert!(
        !graph
            .preset_metadata
            .as_ref()
            .unwrap()
            .bindings
            .iter()
            .any(|binding| binding.id == scope_macro),
        "scope binding dropped"
    );
    assert!(
        !owner.params.contains(scope_macro.as_str()),
        "scope host param pruned"
    );
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
