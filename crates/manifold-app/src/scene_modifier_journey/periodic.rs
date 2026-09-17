use std::path::PathBuf;

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef};
use manifold_core::effects::{ParamId, ParameterDriver};
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_core::{Beats, LayerId, NodeId};
use manifold_core::project::Project;

use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::headless_harness::headless_content_thread;

use super::{
    capture_output, generator_graph, host_binding, math_view_project, modifier_ids,
    set_generator_param, warm_project,
};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;
const PERIODIC_OUTPUT_DIR: &str = "target/journey-proofs/scene-modifier-periodic";

fn mark_stale_periodic_specs(graph: &mut EffectGraphDef) {
    let metadata = graph
        .preset_metadata
        .as_mut()
        .expect("periodic host metadata");
    let host_ids: Vec<_> = metadata
        .bindings
        .iter()
        .filter_map(|binding| {
            matches!(&binding.target, BindingTarget::SceneModifier { param_id, .. }
            if matches!(param_id.as_str(), "phase" | "mask_yaw"))
            .then_some(binding.id.clone())
        })
        .collect();
    for param in &mut metadata.params {
        if host_ids.contains(&param.id) {
            param.wraps = false;
        }
    }
    for modifier in &mut graph.scene_modifiers {
        let local = modifier
            .graph
            .preset_metadata
            .as_mut()
            .expect("periodic local metadata");
        for param in &mut local.params {
            if matches!(param.id.as_str(), "phase" | "mask_yaw") {
                param.wraps = false;
            }
        }
    }
}

fn live_param_for_layer(
    ct: &crate::content_thread::ContentThread,
    layer_id: &LayerId,
    modifier_id: &NodeId,
    node_id: &str,
    param_id: &str,
) -> Vec<f32> {
    let owner = generator_graph(ct.engine.project().expect("periodic project"), layer_id);
    let prepared = manifold_renderer::node_graph::scene_modifier_expand::prepare_scene_modifiers(
        owner,
        &manifold_renderer::node_graph::PrimitiveRegistry::with_builtin(),
    )
    .expect("periodic modifier prepares");
    let routes: Vec<_> = prepared
        .routes
        .iter()
        .filter(|route| &route.modifier_id == modifier_id && route.local.node.as_str() == node_id)
        .collect();
    assert_eq!(routes.len(), 1, "exact periodic modifier-local route");
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

fn mean_abs_diff(a: &[u8], b: &[u8]) -> f32 {
    assert_eq!(a.len(), b.len(), "same-sized captures");
    let sum: u64 = a
        .iter()
        .zip(b)
        .map(|(left, right)| u64::from(left.abs_diff(*right)))
        .sum();
    sum as f32 / a.len() as f32
}

fn periodic_wraps(graph: &EffectGraphDef) -> (bool, bool, bool, bool) {
    let host = graph
        .preset_metadata
        .as_ref()
        .expect("periodic host metadata");
    let modifier = graph.scene_modifiers.first().expect("periodic modifier");
    let local = modifier
        .graph
        .preset_metadata
        .as_ref()
        .expect("periodic local metadata");
    let spec = |metadata: &manifold_core::effect_graph_def::PresetMetadata, id: &str| {
        metadata
            .params
            .iter()
            .find(|param| param.id == id)
            .unwrap_or_else(|| panic!("missing periodic spec {id}"))
            .wraps
    };
    let host_id = |id: &str| {
        host.bindings
            .iter()
            .find_map(|binding| {
                matches!(&binding.target, BindingTarget::SceneModifier { modifier_id, param_id }
            if modifier_id == &modifier.id && param_id == id)
                .then_some(binding.id.as_str())
            })
            .expect("periodic host binding")
    };
    (
        spec(host, host_id("phase")),
        spec(host, host_id("mask_yaw")),
        spec(local, "phase"),
        spec(local, "mask_yaw"),
    )
}

#[test]
fn scene_modifier_periodic_app_control_journey() {
    let output_dir = PathBuf::from(PERIODIC_OUTPUT_DIR);
    std::fs::create_dir_all(&output_dir).expect("periodic artifact directory");
    let layer_id = LayerId::new("math-grid");
    let modifier_id = NodeId::new("vortex_a");

    let mut project = math_view_project();
    let (phase_binding, mask_yaw_binding) = {
        (
            host_binding(&project, &layer_id, &modifier_id, "phase"),
            host_binding(&project, &layer_id, &modifier_id, "mask_yaw"),
        )
    };
    {
        let owner = project
            .graph_target_owner_mut(&manifold_core::GraphTarget::Generator(layer_id.clone()))
            .expect("periodic generator owner");
        let graph = owner.graph.as_mut().expect("periodic generator graph");
        mark_stale_periodic_specs(graph);
        owner.refresh_manifest_from_graph();
    }
    let stale_graph = graph_owner(&project, &layer_id);
    assert_eq!(periodic_wraps(stale_graph), (false, false, false, false));

    let driver_param = phase_binding.clone();
    project
        .graph_target_owner_mut(&manifold_core::GraphTarget::Generator(layer_id.clone()))
        .expect("periodic driver owner")
        .drivers_mut()
        .push({
            let mut driver = ParameterDriver::new(
                ParamId::from(driver_param),
                BeatDivision::Quarter,
                DriverWaveform::Sawtooth,
            );
            driver.trim_max = 1.5;
            driver.free_period_beats = Some(1.0);
            driver
        });

    let notices = crate::project_io::migrate_project_scene_graphs(&mut project);
    assert!(
        notices.is_empty(),
        "periodic migration notices: {notices:?}"
    );
    let repaired_graph = graph_owner(&project, &layer_id);
    assert_eq!(periodic_wraps(repaired_graph), (true, true, true, true));

    let original_modifier_ids = modifier_ids(&project, &layer_id);
    let original_driver = repaired_graph
        .preset_metadata
        .as_ref()
        .expect("periodic repaired metadata")
        .bindings
        .iter()
        .find(|binding| binding.id == phase_binding)
        .expect("periodic phase binding");
    assert_eq!(
        original_modifier_ids,
        vec![modifier_id.clone(), NodeId::new("math_view")]
    );
    assert_eq!(original_driver.id.as_str(), phase_binding.as_str());

    let mut ct = headless_content_thread(Project::default(), WIDTH, HEIGHT);
    let (state_tx, _state_rx) = crossbeam_channel::unbounded::<ContentState>();
    ct.watched_graph_target = Some(manifold_core::GraphTarget::Generator(layer_id.clone()));
    ct.handle_command(ContentCommand::LoadProject(Box::new(project)));
    assert!(
        ct.graph_edit_diagnostic.is_none(),
        "periodic fixture rejected: {:?}",
        ct.graph_edit_diagnostic
    );
    ct.timer.set_frame_clocked(true);
    warm_project(&mut ct, &state_tx);

    // A nonneutral, oblique mask makes its yaw rotation observable on the two
    // saved cube frames while leaving Math View in Scene mode (its default).
    for (param, value) in [
        ("mask_amount", 0.9),
        ("mask_width", 0.55),
        ("mask_feather", 0.08),
        ("mask_pitch", 0.7),
    ] {
        let binding = host_binding(
            ct.engine.project().expect("periodic loaded project"),
            &layer_id,
            &modifier_id,
            param,
        );
        set_generator_param(&mut ct, &layer_id, &binding, value);
    }

    ct.handle_command(ContentCommand::Pause);
    ct.handle_command(ContentCommand::SeekToBeat(Beats(0.75)));
    ct.tick_frame(&state_tx);
    let actual_beat = ct.engine.current_beat_f64();
    let normalized = ParameterDriver::evaluate_with_period(
        Beats(actual_beat),
        1.0,
        DriverWaveform::Sawtooth,
        0.0,
    );
    let raw = normalized * 1.5;
    assert!(
        raw > 1.0,
        "seek must exercise the trim overshoot: raw={raw}"
    );
    let expected_phase = raw.rem_euclid(1.0);
    let reference_beat = actual_beat;
    let host_phase_value = ct
        .engine
        .project()
        .expect("periodic project")
        .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
        .expect("periodic host")
        .get_param(&phase_binding);
    assert!(
        (host_phase_value - expected_phase).abs() < 1.0e-4,
        "wrapped live host phase {host_phase_value} != expected {expected_phase}"
    );
    let native_phase = live_param_for_layer(&ct, &layer_id, &modifier_id, "patch", "phase");
    assert!(
        !native_phase.is_empty(),
        "prepared native phase route has copies"
    );
    assert!(
        native_phase
            .iter()
            .all(|value| (*value - expected_phase).abs() < 1.0e-4)
    );
    let (wrapped_frame, wrapped_nonzero) =
        capture_output(&ct, &output_dir.join("phase-overshoot-wrapped.png"));
    assert!(
        wrapped_nonzero > 0,
        "wrapped phase output contains geometry"
    );

    // Hold the same engine beat, disable only the driver, and set the
    // equivalent wrapped phase by hand. This is the native output reference.
    let disable_driver_param = phase_binding.clone();
    ct.handle_command(ContentCommand::MutateProject(Box::new({
        let layer_id = layer_id.clone();
        move |project| {
            project
                .graph_target_owner_mut(&manifold_core::GraphTarget::Generator(layer_id))
                .expect("periodic driver owner")
                .drivers_mut()
                .iter_mut()
                .find(|driver| driver.param_id == disable_driver_param)
                .expect("periodic phase driver")
                .enabled = false;
        }
    })));
    ct.handle_command(ContentCommand::SeekToBeat(Beats(reference_beat)));
    set_generator_param(&mut ct, &layer_id, &phase_binding, expected_phase);
    ct.tick_frame(&state_tx);
    let (manual_frame, manual_nonzero) =
        capture_output(&ct, &output_dir.join("phase-manual-wrapped.png"));
    assert!(
        manual_nonzero > 0,
        "manual wrapped phase output contains geometry"
    );
    assert!(
        mean_abs_diff(&wrapped_frame, &manual_frame) < 0.05,
        "driver wrap and manual phase diverge"
    );

    let different_phase = (expected_phase + 0.27).rem_euclid(1.0);
    set_generator_param(&mut ct, &layer_id, &phase_binding, different_phase);
    ct.tick_frame(&state_tx);
    let (different_frame, different_nonzero) =
        capture_output(&ct, &output_dir.join("phase-different.png"));
    assert!(
        different_nonzero > 0,
        "different phase output contains geometry"
    );
    assert!(
        mean_abs_diff(&manual_frame, &different_frame) > 0.5,
        "a different phase must visibly deform the scene"
    );

    set_generator_param(&mut ct, &layer_id, &phase_binding, expected_phase);
    // The small cube fixture fits entirely inside width=0.55. Narrow the
    // band for the orientation proof so yaw moves a real mask boundary.
    for (name, value) in [("mask_width", 0.12), ("mask_feather", 0.04)] {
        let binding = host_binding(ct.engine.project().unwrap(), &layer_id, &modifier_id, name);
        set_generator_param(&mut ct, &layer_id, &binding, value);
    }
    let endpoint_binding = host_binding(
        ct.engine.project().expect("periodic project"),
        &layer_id,
        &modifier_id,
        "mask_yaw",
    );
    let (minus, minus_nonzero) = {
        set_generator_param(&mut ct, &layer_id, &endpoint_binding, -std::f32::consts::PI);
        ct.tick_frame(&state_tx);
        capture_output(&ct, &output_dir.join("mask-yaw-minus-pi.png"))
    };
    assert!(minus_nonzero > 0, "mask_yaw -PI output contains geometry");
    let (plus, plus_nonzero) = {
        set_generator_param(&mut ct, &layer_id, &endpoint_binding, std::f32::consts::PI);
        ct.tick_frame(&state_tx);
        capture_output(&ct, &output_dir.join("mask-yaw-plus-pi.png"))
    };
    assert!(plus_nonzero > 0, "mask_yaw +PI output contains geometry");
    // Native readback bytes can differ slightly around the f32 trig endpoint.
    assert!(
        mean_abs_diff(&minus, &plus) < 0.05,
        "-PI/+PI mask yaw parity"
    );

    let interior_binding = endpoint_binding.clone();
    set_generator_param(&mut ct, &layer_id, &interior_binding, 0.37);
    ct.tick_frame(&state_tx);
    let (interior, interior_nonzero) =
        capture_output(&ct, &output_dir.join("mask-yaw-interior.png"));
    assert!(
        interior_nonzero > 0,
        "interior mask yaw output contains geometry"
    );
    assert!(
        mean_abs_diff(&plus, &interior) > 0.5,
        "interior mask yaw must change the nonneutral mask output"
    );
    eprintln!("periodic native proof: phase equivalent MAD={}, phase change MAD={}, mask endpoint MAD={}, mask interior MAD={}",
        mean_abs_diff(&wrapped_frame, &manual_frame), mean_abs_diff(&manual_frame, &different_frame),
        mean_abs_diff(&minus, &plus), mean_abs_diff(&plus, &interior));

    // Restore the driver setting and a stable base value before the round trip.
    set_generator_param(&mut ct, &layer_id, &interior_binding, std::f32::consts::PI);
    set_generator_param(&mut ct, &layer_id, &phase_binding, expected_phase);
    let enable_driver_param = phase_binding.clone();
    ct.handle_command(ContentCommand::MutateProject(Box::new({
        let layer_id = layer_id.clone();
        move |project| {
            project
                .graph_target_owner_mut(&manifold_core::GraphTarget::Generator(layer_id))
                .expect("periodic driver owner")
                .drivers_mut()
                .iter_mut()
                .find(|driver| driver.param_id == enable_driver_param)
                .expect("periodic phase driver")
                .enabled = true;
        }
    })));

    let saved_path = output_dir.join("scene-modifier-periodic.manifold");
    manifold_io::saver::save_project_v1(
        ct.engine.project().expect("periodic project"),
        &saved_path,
    )
    .expect("save periodic fixture");
    let mut reopened =
        manifold_io::loader::load_project(&saved_path).expect("reopen periodic fixture");
    let reopen_notices = crate::project_io::migrate_project_scene_graphs(&mut reopened);
    assert!(
        reopen_notices.is_empty(),
        "reopened periodic migration notices: {reopen_notices:?}"
    );
    let reopened_graph = graph_owner(&reopened, &layer_id);
    assert_eq!(modifier_ids(&reopened, &layer_id), original_modifier_ids);
    assert_eq!(periodic_wraps(reopened_graph), (true, true, true, true));
    let reopened_host = reopened
        .graph_target_owner(&manifold_core::GraphTarget::Generator(layer_id.clone()))
        .expect("reopened periodic host");
    let reopened_driver = reopened_host
        .get_drivers_list()
        .expect("reopened periodic driver")
        .iter()
        .find(|driver| driver.param_id == phase_binding)
        .expect("reopened phase driver");
    assert!(
        reopened_driver.enabled,
        "reopened phase driver remains enabled"
    );
    assert_eq!(reopened_driver.beat_division, BeatDivision::Quarter);
    assert_eq!(reopened_driver.phase, 0.0);
    assert_eq!(reopened_driver.trim_min, 0.0);
    assert_eq!(reopened_driver.waveform, DriverWaveform::Sawtooth);
    assert_eq!(reopened_driver.free_period_beats, Some(1.0));
    assert_eq!(reopened_driver.trim_max, 1.5);
    assert_eq!(
        reopened_host.get_base_param(&interior_binding),
        std::f32::consts::PI
    );
    assert!((reopened_host.get_base_param(&phase_binding) - expected_phase).abs() < 1.0e-4);
    assert_eq!(
        host_binding(&reopened, &layer_id, &modifier_id, "phase"),
        phase_binding
    );
    assert_eq!(
        host_binding(&reopened, &layer_id, &modifier_id, "mask_yaw"),
        mask_yaw_binding
    );

    ct.handle_command(ContentCommand::LoadProject(Box::new(reopened)));
    assert!(
        ct.graph_edit_diagnostic.is_none(),
        "reopened periodic fixture rejected: {:?}",
        ct.graph_edit_diagnostic
    );
    warm_project(&mut ct, &state_tx);
}

fn graph_owner<'a>(project: &'a Project, layer_id: &LayerId) -> &'a EffectGraphDef {
    generator_graph(project, layer_id)
}
