//! Native endpoint, saw reset and persistence proof for existing angle knobs.
use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::path::PathBuf;

use manifold_core::effects::{ParamId, ParameterDriver};
use manifold_core::project::Project;
use manifold_core::types::{BeatDivision, DriverWaveform};
use manifold_core::{Beats, GraphTarget, LayerId, NodeId, PresetTypeId};
use manifold_renderer::node_graph::bundled_preset_def;

use super::{
    capture_output, generator_graph, host_binding, math_view_project, set_generator_param,
    warm_project,
};
use crate::content_command::ContentCommand;
use crate::content_state::ContentState;
use crate::headless_harness::headless_content_thread;

fn difference(a: &[u8], b: &[u8]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(a, b)| u64::from(a.abs_diff(*b)))
        .sum::<u64>() as f32
        / a.len() as f32
}

#[test]
fn scene_modifier_angular_wrap_journey() {
    let output = PathBuf::from("target/journey-proofs/modifier-angular-wrap");
    std::fs::create_dir_all(&output).unwrap();
    let layer = LayerId::new("math-grid");
    let modifier = NodeId::new("vortex_a");
    let target = GraphTarget::Generator(layer.clone());
    for (recipe, angle) in [
        ("VortexFragments", "orbit"),
        ("SurfacePeel", "curl"),
        ("OrderedRecon", "rotation"),
    ] {
        let mut project = math_view_project();
        let owner = project.graph_target_owner_mut(&target).unwrap();
        let graph = owner.graph.as_mut().unwrap();
        *graph.scene_modifiers[0].graph = bundled_preset_def(&PresetTypeId::new(recipe))
            .unwrap()
            .clone();
        *graph = manifold_core::scene_modifier_edit::reconcile_scene_modifier_parameters(
            graph, &modifier,
        )
        .unwrap()
        .graph;
        owner.refresh_manifest_from_graph();
        let binding = host_binding(&project, &layer, &modifier, angle);
        let owner = project.graph_target_owner_mut(&target).unwrap();
        let graph = owner.graph.as_mut().unwrap();
        let count = graph.scene_modifiers[0]
            .graph
            .preset_metadata
            .as_ref()
            .unwrap()
            .params
            .len();
        // Exercise production stale-flag repair while preserving the existing IDs.
        graph.scene_modifiers[0]
            .graph
            .preset_metadata
            .as_mut()
            .unwrap()
            .params
            .iter_mut()
            .find(|p| p.id == angle)
            .unwrap()
            .wraps = false;
        graph
            .preset_metadata
            .as_mut()
            .unwrap()
            .params
            .iter_mut()
            .find(|p| p.id == binding)
            .unwrap()
            .wraps = false;
        owner.refresh_manifest_from_graph();
        owner.drivers_mut().push({
            let mut driver = ParameterDriver::new(
                ParamId::from(binding.clone()),
                BeatDivision::Quarter,
                DriverWaveform::Sawtooth,
            );
            driver.free_period_beats = Some(1.0);
            driver.enabled = false;
            driver
        });
        assert!(crate::project_io::migrate_project_scene_graphs(&mut project).is_empty());
        let graph = generator_graph(&project, &layer);
        let local = graph.scene_modifiers[0]
            .graph
            .preset_metadata
            .as_ref()
            .unwrap();
        assert_eq!(local.params.len(), count, "no new knobs");
        for spec in [
            local.params.iter().find(|p| p.id == angle).unwrap(),
            graph
                .preset_metadata
                .as_ref()
                .unwrap()
                .params
                .iter()
                .find(|p| p.id == binding)
                .unwrap(),
        ] {
            assert!(spec.wraps);
            assert_eq!(spec.min, 0.0);
            assert!((spec.max - TAU).abs() < 1.0e-6);
        }

        let mut ct = headless_content_thread(Project::default(), 320, 180);
        let (tx, _rx) = crossbeam_channel::unbounded::<ContentState>();
        ct.watched_graph_target = Some(target.clone());
        ct.handle_command(ContentCommand::LoadProject(Box::new(project)));
        assert!(
            ct.graph_edit_diagnostic.is_none(),
            "{recipe}: {:?}",
            ct.graph_edit_diagnostic
        );
        ct.timer.set_frame_clocked(true);
        warm_project(&mut ct, &tx);
        ct.handle_command(ContentCommand::Pause);
        ct.handle_command(ContentCommand::SeekToBeat(Beats(0.25)));
        for (name, value) in [
            ("mask_amount", 0.8),
            ("mask_width", 0.12),
            ("mask_feather", 0.04),
            ("mask_pitch", 0.7),
        ] {
            let id = host_binding(ct.engine.project().unwrap(), &layer, &modifier, name);
            set_generator_param(&mut ct, &layer, &id, value);
        }
        let settings: &[(&str, f32)] = match recipe {
            "VortexFragments" => &[("phase", 0.17), ("pitch", 0.7)],
            "SurfacePeel" => &[("phase", 0.17), ("lift", 0.25)],
            _ => &[("progress", 0.35)],
        };
        for &(name, value) in settings {
            let id = host_binding(ct.engine.project().unwrap(), &layer, &modifier, name);
            set_generator_param(&mut ct, &layer, &id, value);
        }
        let mut frames = Vec::new();
        for (label, value) in [
            ("zero", 0.0),
            ("full-turn", TAU),
            ("quarter-turn", FRAC_PI_2),
            ("half-turn", PI),
        ] {
            set_generator_param(&mut ct, &layer, &binding, value);
            ct.tick_frame(&tx);
            let (frame, nonzero) =
                capture_output(&ct, &output.join(format!("{recipe}-{label}.png")));
            assert!(nonzero > 0, "{recipe} {label} contains geometry");
            frames.push(frame);
        }
        assert!(
            difference(&frames[0], &frames[1]) < 0.05,
            "{recipe} full turn changed output"
        );
        assert!(
            difference(&frames[0], &frames[2]) > 0.05,
            "{recipe} angle must affect rendering"
        );
        ct.handle_command(ContentCommand::Undo);
        ct.tick_frame(&tx);
        assert_eq!(
            ct.engine
                .project()
                .unwrap()
                .graph_target_owner(&target)
                .unwrap()
                .get_base_param(&binding),
            FRAC_PI_2
        );
        ct.handle_command(ContentCommand::Redo);
        ct.tick_frame(&tx);
        assert_eq!(
            ct.engine
                .project()
                .unwrap()
                .graph_target_owner(&target)
                .unwrap()
                .get_base_param(&binding),
            PI
        );

        ct.handle_command(ContentCommand::MutateProject(Box::new({
            let target = target.clone();
            let binding = binding.clone();
            move |project| {
                project
                    .graph_target_owner_mut(&target)
                    .unwrap()
                    .drivers_mut()
                    .iter_mut()
                    .find(|d| d.param_id == binding)
                    .unwrap()
                    .enabled = true;
            }
        })));
        let mut seam_frames = Vec::new();
        for (label, beat) in [("before-reset", 0.99999), ("after-reset", 1.00001)] {
            ct.handle_command(ContentCommand::SeekToBeat(Beats(beat)));
            ct.tick_frame(&tx);
            let actual = ct.engine.current_beat_f64();
            assert!((actual - beat).abs() < 1.0e-6, "paused saw position");
            let expected = ParameterDriver::evaluate_with_period(
                Beats(actual),
                1.0,
                DriverWaveform::Sawtooth,
                0.0,
            ) * TAU;
            let live = ct
                .engine
                .project()
                .unwrap()
                .graph_target_owner(&target)
                .unwrap()
                .get_param(&binding);
            assert!(
                (live - expected).abs() < 1.0e-4,
                "{recipe} saw did not drive angle: {live} != {expected}"
            );
            let (frame, _) = capture_output(&ct, &output.join(format!("{recipe}-{label}.png")));
            seam_frames.push(frame);
        }
        let seam_diff = difference(&seam_frames[0], &seam_frames[1]);
        assert!(seam_diff < 0.1, "{recipe} saw reset jumps: MAD={seam_diff}");

        let saved = output.join(format!("{recipe}.manifold"));
        manifold_io::saver::save_project_v1(ct.engine.project().unwrap(), &saved).unwrap();
        let mut reopened = manifold_io::loader::load_project(&saved).unwrap();
        assert!(crate::project_io::migrate_project_scene_graphs(&mut reopened).is_empty());
        assert_eq!(host_binding(&reopened, &layer, &modifier, angle), binding);
        let owner = reopened.graph_target_owner(&target).unwrap();
        assert_eq!(owner.get_base_param(&binding), PI);
        let driver = owner
            .get_drivers_list()
            .unwrap()
            .iter()
            .find(|d| d.param_id == binding)
            .unwrap();
        assert!(driver.enabled);
        assert_eq!(driver.waveform, DriverWaveform::Sawtooth);
        assert_eq!(driver.free_period_beats, Some(1.0));
        assert_eq!(
            generator_graph(&reopened, &layer).scene_modifiers,
            generator_graph(ct.engine.project().unwrap(), &layer).scene_modifiers
        );
        ct.handle_command(ContentCommand::LoadProject(Box::new(reopened)));
        assert!(ct.graph_edit_diagnostic.is_none());
        warm_project(&mut ct, &tx);
        ct.handle_command(ContentCommand::Pause);
        ct.handle_command(ContentCommand::SeekToBeat(Beats(1.00001)));
        ct.tick_frame(&tx);
        let (reload, _) = capture_output(&ct, &output.join(format!("{recipe}-reloaded.png")));
        assert!(
            difference(&reload, &seam_frames[1]) < 0.05,
            "{recipe} reload changed output"
        );
        eprintln!(
            "{recipe}: endpoint MAD={}, quarter-turn MAD={}, saw reset MAD={seam_diff}",
            difference(&frames[0], &frames[1]),
            difference(&frames[0], &frames[2])
        );
    }
}
