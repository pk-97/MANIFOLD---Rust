//! Native trigger-lane routing proof for two independent UniformForce inputs.
//!
//! This is intentionally a child of `content_pipeline`: it exercises the
//! same retained pulse handoff that the native content path uses, while the
//! render loop below keeps the fixture small enough for a focused GPU proof.

use super::ContentPipeline;
use super::trigger_targets::TriggerTargets;

use manifold_compositor::generator_renderer::GeneratorRenderer;
use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::layer::Layer;
use manifold_core::params::ClipTriggerSource;
use manifold_core::project::Project;
use manifold_core::scene_modifier_preset::{SceneNodeRef, SceneTargetSelection};
use manifold_core::{
    Beats, Bpm, GraphTarget, LayerId, NodeId, PlaybackState, PresetTypeId, Seconds,
};
use manifold_editing::command::Command;
use manifold_editing::commands::trigger_source::SetParamClipTriggerSourceCommand;
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::runtime::preset_context::ProjectTempo;
use manifold_playback::engine::{PlaybackEngine, TickContext};
use manifold_playback::modulation::{TriggerPulseKind, TriggerSourceStamp};
use std::sync::Arc;

const DT: f64 = 1.0 / 60.0;
const WIDTH: u32 = 32;
const HEIGHT: u32 = 32;
const DATA_VERSION: u64 = 1;

fn reference(id: &str) -> SceneNodeRef {
    SceneNodeRef {
        scope: Vec::new(),
        node: NodeId::new(id),
    }
}

fn scene_fixture() -> EffectGraphDef {
    let mut def: EffectGraphDef = serde_json::from_value(serde_json::json!({
        "version": 3,
        "name": "Trigger lane force isolation",
        "nodes": [
            {"id":0,"nodeId":"clock","typeId":"system.generator_input"},
            {"id":1,"nodeId":"pose_a","typeId":"node.transform_3d","params":{"pos_x":{"type":"Float","value":-1.0},"pos_y":{"type":"Float","value":10.0}}},
            {"id":2,"nodeId":"pose_b","typeId":"node.transform_3d","params":{"pos_x":{"type":"Float","value":1.0},"pos_y":{"type":"Float","value":10.0}}},
            {"id":3,"nodeId":"body_a","typeId":"node.rigid_body"},
            {"id":4,"nodeId":"body_b","typeId":"node.rigid_body"},
            {"id":5,"nodeId":"world","typeId":"node.physics_world","params":{"gravity_y":{"type":"Float","value":0.0}}},
            {"id":6,"nodeId":"part_a","typeId":"node.scene_object"},
            {"id":7,"nodeId":"part_b","typeId":"node.scene_object"},
            {"id":8,"nodeId":"scene","typeId":"node.render_scene","params":{"objects":{"type":"Int","value":2}}},
            {"id":9,"nodeId":"output","typeId":"system.final_output"},
            {"id":10,"nodeId":"camera","typeId":"node.look_at_camera","params":{"pos_y":{"type":"Float","value":10.0},"pos_z":{"type":"Float","value":-8.0},"target_y":{"type":"Float","value":10.0}}},
            {"id":11,"nodeId":"mesh","typeId":"node.cube_mesh"},
            {"id":12,"nodeId":"material","typeId":"node.unlit_material"}
        ],
        "wires": [
            {"fromNode":10,"fromPort":"out","toNode":8,"toPort":"camera"},
            {"fromNode":1,"fromPort":"transform","toNode":3,"toPort":"transform"},
            {"fromNode":2,"fromPort":"transform","toNode":4,"toPort":"transform"},
            {"fromNode":3,"fromPort":"body","toNode":5,"toPort":"body_0"},
            {"fromNode":4,"fromPort":"body","toNode":5,"toPort":"body_1"},
            {"fromNode":5,"fromPort":"pose_0","toNode":6,"toPort":"transform"},
            {"fromNode":5,"fromPort":"pose_1","toNode":7,"toPort":"transform"},
            {"fromNode":11,"fromPort":"vertices","toNode":6,"toPort":"vertices"},
            {"fromNode":11,"fromPort":"vertices","toNode":7,"toPort":"vertices"},
            {"fromNode":12,"fromPort":"out","toNode":6,"toPort":"material"},
            {"fromNode":12,"fromPort":"out","toNode":7,"toPort":"material"},
            {"fromNode":6,"fromPort":"object","toNode":8,"toPort":"object_0"},
            {"fromNode":7,"fromPort":"object","toNode":8,"toPort":"object_1"},
            {"fromNode":8,"fromPort":"color","toNode":9,"toPort":"in"}
        ]
    }))
    .expect("valid scene graph");
    let mut metadata =
        manifold_nodes::bundled_presets::bundled_preset_def(&PresetTypeId::new("Scene"))
            .and_then(|scene| scene.preset_metadata.clone())
            .expect("Scene metadata");
    metadata.params.clear();
    metadata.bindings.clear();
    metadata.string_params.clear();
    metadata.string_bindings.clear();
    metadata.scene_modifier = None;
    def.preset_metadata = Some(metadata);
    def
}

fn modifier_alias(def: &EffectGraphDef, modifier: &str, param_id: &str) -> String {
    def.preset_metadata
        .as_ref()
        .expect("Scene metadata")
        .bindings
        .iter()
        .find_map(|binding| {
            matches!(
                &binding.target,
                BindingTarget::SceneModifier {
                    modifier_id,
                    param_id: binding_param_id,
                }
                    if modifier_id.as_str() == modifier && binding_param_id == param_id
            )
            .then_some(binding.id.clone())
        })
        .expect("scene modifier exposes public parameter")
}

fn force_alias(def: &EffectGraphDef, modifier: &str) -> String {
    modifier_alias(def, modifier, "fire")
}

fn insert_uniform_force(mut owner: EffectGraphDef, id: &str, target: &str) -> EffectGraphDef {
    let recipe: EffectGraphDef = serde_json::from_str(
        manifold_nodes::testkit::assets::ASSETS_SCENE_MODIFIER_PRESETS_UNIFORMFORCE_JSON,
    )
    .expect("bundled UniformForce recipe");
    let instance =
        manifold_nodes_scene::node_graph::scene_modifier_authoring::prepare_new_scene_modifier(
            &owner,
            &recipe,
            NodeId::new(id),
            reference("scene"),
            SceneTargetSelection::Explicit {
                objects: vec![reference(target)],
            },
        )
        .expect("prepare UniformForce through production authoring seam");
    owner = manifold_core::scene_modifier_edit::insert_scene_modifier(
        &owner,
        owner.scene_modifiers.len(),
        instance,
    )
    .expect("insert UniformForce through production graph edit")
    .graph;
    owner
}

fn project_fixture() -> (Project, LayerId, LayerId, LayerId, String, String) {
    let mut project = Project::default();
    project.settings.bpm = Bpm(240.0);

    let mut parent = Layer::new_generator("Force scene".into(), PresetTypeId::new("Scene"), 0);
    let parent_id = parent.layer_id.clone();
    let mut graph = scene_fixture();
    graph = insert_uniform_force(graph, "force_a", "part_a");
    graph = insert_uniform_force(graph, "force_b", "part_b");
    parent.gen_params_or_init().graph = Some(graph.clone());
    parent.gen_params_or_init().params = Default::default();
    parent.gen_params_or_init().refresh_manifest_from_graph();
    let strength_a_alias = modifier_alias(&graph, "force_a", "strength");
    let strength_b_alias = modifier_alias(&graph, "force_b", "strength");
    let parent_params = parent.gen_params_or_init();
    assert!(parent_params.set_base_param(&strength_a_alias, 0.0));
    assert!(parent_params.set_base_param(&strength_b_alias, 0.0));
    parent
        .clips
        .push(manifold_core::clip::TimelineClip::new_generator(
            Beats::ZERO,
            Beats(16.0),
        ));

    let mut lane_a = Layer::new_trigger("Pattern A".into(), parent_id.clone(), 1);
    let lane_a_id = lane_a.layer_id.clone();
    for beat in [0.4, 0.8, 1.2] {
        lane_a
            .clips
            .push(manifold_core::clip::TimelineClip::new_trigger(
                Beats(beat),
                Beats(0.1),
            ));
    }
    let mut lane_b = Layer::new_trigger("Pattern B".into(), parent_id.clone(), 2);
    let lane_b_id = lane_b.layer_id.clone();
    for beat in [0.6, 1.0] {
        lane_b
            .clips
            .push(manifold_core::clip::TimelineClip::new_trigger(
                Beats(beat),
                Beats(0.1),
            ));
    }

    project.timeline.layers = vec![parent, lane_a, lane_b];
    let owner = GraphTarget::Generator(parent_id.clone());
    for alias in [
        force_alias(&graph, "force_a"),
        force_alias(&graph, "force_b"),
    ] {
        let param = project
            .graph_target_owner(&owner)
            .expect("generator owner")
            .params
            .get(&alias)
            .expect("public UniformForce Fire parameter");
        assert!(param.spec.is_trigger, "UniformForce Fire must be a trigger");
        assert_eq!(
            param.spec.name, "Fire",
            "force trigger keeps its exposed name"
        );
    }
    for (lane, alias) in [
        (lane_a_id.clone(), force_alias(&graph, "force_a")),
        (lane_b_id.clone(), force_alias(&graph, "force_b")),
    ] {
        let source = ClipTriggerSource::Lane {
            layer_id: lane.clone(),
        };
        let param_alias = alias.clone();
        let mut command =
            SetParamClipTriggerSourceCommand::for_assignment(owner.clone(), alias, source.clone());
        command.execute(&mut project);
        assert_eq!(
            project
                .graph_target_owner(&owner)
                .expect("generator owner")
                .params
                .get(&param_alias)
                .expect("assigned Fire parameter")
                .clip_trigger_source,
            source,
            "Fire assignment must use the existing command",
        );
    }

    (
        project,
        parent_id,
        lane_a_id,
        lane_b_id,
        force_alias(&graph, "force_a"),
        force_alias(&graph, "force_b"),
    )
}

fn generator(engine: &mut PlaybackEngine) -> &mut GeneratorRenderer {
    engine
        .renderers_mut()
        .iter_mut()
        .find_map(|renderer| renderer.as_any_mut().downcast_mut::<GeneratorRenderer>())
        .expect("real GeneratorRenderer")
}

#[test]
fn trigger_lane_force_isolation() {
    let (project, parent_id, lane_a_id, lane_b_id, force_a_alias, force_b_alias) =
        project_fixture();
    let device = Arc::new(GpuDevice::new_queued("trigger lane force proof"));
    let mut engine = PlaybackEngine::new(vec![Box::new(GeneratorRenderer::new_unwarmed(
        device.clone(),
        WIDTH,
        HEIGHT,
        GpuTextureFormat::Rgba16Float,
        0,
    ))]);
    engine.initialize(project);
    engine.set_state(PlaybackState::Playing);

    let mut targets = TriggerTargets::default();
    let mut master_trigger_count = 0;
    let mut expected_a = 0_u64;
    let mut expected_b = 0_u64;
    let mut baseline_generator_count = None;
    let mut runtime_identity = None;
    let mut world_epoch = None;
    let mut pending_force = None;
    let mut received = [0_u64; 2];

    for frame in 0..36_u64 {
        let started_before = generator(&mut engine).scene_impulse_diagnostics().started;
        let result = engine.tick(TickContext {
            dt_seconds: Seconds(DT),
            realtime_now: Seconds(frame as f64 * DT),
            pre_render_dt: Seconds(DT),
            frame_count: frame,
            ..TickContext::default()
        });

        engine
            .with_trigger_pulses(|pulses, renderers, project| {
                let project = project.expect("project installed");
                targets.refresh(
                    Some(project),
                    DATA_VERSION,
                    pulses.first().map_or(0, |pulse| pulse.epoch),
                );
                for captured in pulses {
                    assert_eq!(captured.pulse.kind, TriggerPulseKind::Parameter);
                    let (destination_layer, destination_param) = targets
                        .scene_impulse(&captured.pulse)
                        .expect("captured Fire must resolve to a scene impulse");
                    assert_eq!(destination_layer, &parent_id);
                    let TriggerSourceStamp::Clip { layer_id, .. } = &captured.pulse.source_stamp
                    else {
                        panic!("trigger-lane proof requires clip provenance")
                    };
                    assert!(
                        pending_force.is_none(),
                        "previous force must apply before the next source window"
                    );
                    match layer_id {
                        id if id == &lane_a_id => {
                            expected_a += 1;
                            pending_force = Some(0);
                            assert_eq!(destination_param, &force_a_alias);
                        }
                        id if id == &lane_b_id => {
                            expected_b += 1;
                            pending_force = Some(1);
                            assert_eq!(destination_param, &force_b_alias);
                        }
                        other => panic!("unexpected trigger source lane {other}"),
                    }
                    assert_eq!(
                        captured.pulse.owner_id,
                        project
                            .timeline
                            .layers
                            .iter()
                            .find(|layer| layer.layer_id == parent_id)
                            .and_then(Layer::gen_params)
                            .expect("parent generator")
                            .id
                    );
                }
                ContentPipeline::apply_trigger_pulses(
                    &mut master_trigger_count,
                    &targets,
                    pulses,
                    renderers,
                    Some(project),
                );
            })
            .expect("trigger delivery available");

        let (layers, tempo) = {
            let project = engine.project().expect("project installed");
            (
                project.timeline.layers.clone(),
                ProjectTempo::new(&project.tempo_map, project.settings.bpm),
            )
        };
        let mut command_buffer = device.create_encoder("trigger lane force proof");
        let mut gpu = GpuEncoder::new(&mut command_buffer, &device);
        let render_time = engine.current_time_double();
        let render_beat = engine.current_beat_f64();
        generator(&mut engine).render_all(
            &mut gpu,
            render_time,
            render_beat,
            DT as f32,
            &layers,
            DATA_VERSION,
            &[],
            Some(&tempo),
        );
        command_buffer.commit_and_wait_completed();

        let renderer = generator(&mut engine);
        let state = renderer
            .layer_generators
            .get(&parent_id)
            .expect("parent runtime installed");
        assert!(
            state.generator.errors().is_empty(),
            "runtime errors: {:?}",
            state.generator.errors()
        );
        let identity = state.generator.as_ref() as *const _ as usize;
        if let Some(previous) = runtime_identity {
            assert_eq!(identity, previous, "live scene runtime was reset mid-run");
        } else {
            runtime_identity = Some(identity);
        }
        let epoch = state
            .generator
            .graph
            .instance_by_node_id(&NodeId::new("world"))
            .and_then(|instance| state.generator.graph.get_node(instance))
            .and_then(|node| manifold_water_rigid::node::get(node.node.as_ref()))
            .and_then(|node| node.physics_impulse_epoch());
        let epoch = epoch.expect("native physics world must expose a live epoch");
        assert!(epoch > 0, "native physics world must expose a live epoch");
        if let Some(previous) = world_epoch {
            assert_eq!(epoch, previous, "native physics epoch changed mid-run");
        }
        world_epoch = Some(epoch);
        let effective_count = renderer.effective_trigger_count_for_layer(&parent_id);
        baseline_generator_count.get_or_insert(effective_count);
        assert_eq!(
            effective_count,
            baseline_generator_count.expect("baseline count"),
            "named Fire pulses must not change compatibility gate counters",
        );
        let diagnostics = renderer.scene_impulse_diagnostics();
        let applied = diagnostics.started - started_before;
        assert!(applied <= 1, "source windows contain one native event");
        if applied == 1 {
            received[pending_force
                .take()
                .expect("native receipt must have a routed Fire")] += 1;
        }
        assert_eq!(diagnostics.discarded, 0);
        engine.reclaim_tick_result(result);
    }

    let renderer = generator(&mut engine);
    let diagnostics = renderer.scene_impulse_diagnostics();
    assert_eq!(expected_a, 3, "pattern A must produce three isolated fires");
    assert_eq!(expected_b, 2, "pattern B must produce two isolated fires");
    assert!(
        pending_force.is_none(),
        "last force must reach a native tick"
    );
    assert_eq!(
        received,
        [3, 2],
        "native applied receipts stay isolated by force"
    );
    assert_eq!(
        diagnostics.started, 5,
        "each captured Fire must reach native physics once"
    );
    assert_eq!(diagnostics.discarded, 0);
    assert_eq!(
        master_trigger_count, 0,
        "named Fire pulses must not broadcast a Gate"
    );
    assert!(
        world_epoch.is_some(),
        "native physics epoch must remain observable"
    );
    assert!(
        runtime_identity.is_some(),
        "scene runtime must remain installed"
    );
}

#[cfg(feature = "journey-proofs")]
#[test]
fn trigger_lane_force_mixed_audio_timing_acceptance() {
    use crate::headless_harness::headless_content_thread;
    use crate::offline_audio_mod::OfflineAudioModDriver;
    use manifold_core::audio_mod::ParameterAudioMod;
    use manifold_core::effects::ParamId;
    use manifold_core::{AudioBand, AudioFeature, AudioFeatureKind, AudioSend};
    use manifold_playback::audio_mixdown::ExportAudio;
    use std::time::Instant;

    const WARM_FRAMES: u32 = 30;
    const MEASURED_FRAMES: u32 = 36;
    const SAMPLE_RATE: u32 = 44_100;
    const AUDIO_SECONDS: usize = 2;
    const FPS: f64 = 60.0;

    let (mut project, parent_id, lane_a_id, lane_b_id, _, _) = project_fixture();
    for layer in &mut project.timeline.layers {
        if layer.layer_id == lane_a_id || layer.layer_id == lane_b_id {
            for clip in &mut layer.clips {
                // Thirty warm frames at 240 BPM span two beats.
                clip.start_beat += Beats(2.0);
            }
        }
    }

    let mut send = AudioSend::new("Trigger timing send");
    send.channels = vec![0];
    let send_id = send.id.clone();
    project.audio_setup.sends.push(send);
    let parent = project
        .timeline
        .layers
        .iter_mut()
        .find(|layer| layer.layer_id == parent_id)
        .expect("force parent layer");
    let strength_alias = modifier_alias(parent.generator_graph().unwrap(), "force_a", "strength");
    let audio_mod = ParameterAudioMod::new(
        ParamId::from(strength_alias.clone()),
        send_id,
        AudioFeature::new(AudioFeatureKind::Amplitude, AudioBand::Full),
    );
    parent.gen_params_or_init().audio_mods_mut().push(audio_mod);

    let sample_count = SAMPLE_RATE as usize * AUDIO_SECONDS;
    let master_mono = (0..sample_count)
        .map(|sample| {
            let time = sample as f32 / SAMPLE_RATE as f32;
            (std::f32::consts::TAU * 220.0 * time).sin() * 0.7
        })
        .collect();
    let audio = ExportAudio {
        sample_rate: SAMPLE_RATE,
        left: Vec::new(),
        right: Vec::new(),
        master_mono,
        per_layer_mono: Default::default(),
        pre_roll_samples: 0,
        audible_in_range: true,
    };
    let mut audio_driver = OfflineAudioModDriver::new(&project, &audio, FPS, Seconds::ZERO)
        .expect("audio mod must consume the synthetic send");
    let mut content = headless_content_thread(project, WIDTH, HEIGHT);
    content.engine.set_state(PlaybackState::Playing);
    let initial_faults = manifold_gpu::gpu_fault::fault_count();

    let mut retained_audio_hops = 0usize;
    let mut max_audio_ms = 0.0f64;
    let mut max_engine_ms = 0.0f64;
    let mut max_render_ms = 0.0f64;
    let mut max_content_ms = 0.0f64;
    let total_frames = WARM_FRAMES + MEASURED_FRAMES;
    for frame in 0..total_frames {
        let audio_start = Instant::now();
        audio_driver
            .feed_frame(frame, &mut content.engine)
            .expect("synthetic audio feed");
        let audio_ms = audio_start.elapsed().as_secs_f64() * 1000.0;

        let engine_start = Instant::now();
        let tick_result = content.engine.tick(TickContext {
            dt_seconds: Seconds(DT),
            realtime_now: Seconds(frame as f64 * DT),
            pre_render_dt: Seconds(DT),
            frame_count: frame as u64,
            ..TickContext::default()
        });
        let engine_ms = engine_start.elapsed().as_secs_f64() * 1000.0;

        let observations = content
            .engine
            .project()
            .expect("project installed")
            .timeline
            .layers
            .iter()
            .find(|layer| layer.layer_id == parent_id)
            .and_then(|layer| layer.gen_params())
            .and_then(|params| params.find_audio_mod(&strength_alias))
            .expect("force strength audio mod retained");
        retained_audio_hops += observations.control_observations.observations().len();
        assert!(
            observations.control_observations.failure().is_none(),
            "audio control observations failed: {:?}",
            observations.control_observations.failure()
        );
        assert!(
            content.engine.trigger_delivery_failure().is_none(),
            "trigger delivery failed"
        );

        let render_start = Instant::now();
        content.content_pipeline.render_content(
            &content.gpu,
            &mut content.engine,
            &tick_result,
            DT,
            frame as u64,
            true,
            DATA_VERSION,
            Some(audio_driver.visuals()),
        );
        let render_ms = render_start.elapsed().as_secs_f64() * 1000.0;
        content
            .content_pipeline
            .wait_for_export_complete(initial_faults)
            .expect("native frame must complete without GPU faults");
        content.engine.reclaim_tick_result(tick_result);

        if frame >= WARM_FRAMES {
            max_audio_ms = max_audio_ms.max(audio_ms);
            max_engine_ms = max_engine_ms.max(engine_ms);
            max_render_ms = max_render_ms.max(render_ms);
            let content_ms = audio_ms + engine_ms + render_ms;
            max_content_ms = max_content_ms.max(content_ms);
            assert!(
                content_ms <= 20.0,
                "content frame {frame} took {content_ms:.3}ms"
            );
        }
    }

    let generator = generator(&mut content.engine);
    let diagnostics = generator.scene_impulse_diagnostics();
    assert!(
        retained_audio_hops > total_frames as usize,
        "audio hops must exceed video frames"
    );
    assert_eq!(
        diagnostics.started, 5,
        "all child Fire pulses must reach native physics"
    );
    assert_eq!(
        diagnostics.discarded, 0,
        "native physics must discard no Fire pulses"
    );
    assert!(
        manifold_playback::modulation::control_capture_error(
            content.engine.project().expect("project installed")
        )
        .is_none(),
        "audio control capture failed"
    );
    eprintln!(
        "trigger mixed timing max ms: content={max_content_ms:.3}, audio={max_audio_ms:.3}, engine={max_engine_ms:.3}, render={max_render_ms:.3}"
    );
}
