//! Native observation of live Clip Trigger controls on the consolidated cards.
use std::path::PathBuf;

use manifold_core::project::Project;
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
fn consolidated_modifier_native_toggle_journey() {
    let output = PathBuf::from("target/journey-proofs/modifier-consolidation");
    std::fs::create_dir_all(&output).unwrap();
    let layer = LayerId::new("math-grid");
    let modifier = NodeId::new("vortex_math_view");
    for recipe in ["SurfacePeel", "OrderedRecon"] {
        // Reuse the established saved two-object frame fixture in Scene mode.
        let mut project = math_view_project();
        let target = GraphTarget::Generator(layer.clone());
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
        assert!(crate::project_io::migrate_project_scene_graphs(&mut project).is_empty());
        let clip = host_binding(&project, &layer, &modifier, "clip_trigger");
        assert_eq!(
            project
                .graph_target_owner(&target)
                .unwrap()
                .get_base_param(&clip),
            0.0
        );
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
        let settings: &[(&str, f32)] = if recipe == "SurfacePeel" {
            &[
                ("lift", 0.25),
                ("curl", 0.8),
                ("burst_strength", 0.6),
                ("burst_duration", 2.0),
            ]
        } else {
            &[
                ("progress", 0.35),
                ("attack_beats", 1.0),
                ("hold_beats", 0.25),
                ("return_beats", 2.0),
            ]
        };
        for &(name, value) in settings.iter().chain(
            [
                ("mask_amount", 0.8),
                ("mask_width", 0.12),
                ("mask_feather", 0.04),
                ("mask_pitch", 0.7),
            ]
            .iter(),
        ) {
            let id = host_binding(ct.engine.project().unwrap(), &layer, &modifier, name);
            set_generator_param(&mut ct, &layer, &id, value);
        }
        ct.handle_command(ContentCommand::Pause);
        ct.handle_command(ContentCommand::SeekToBeat(Beats(0.1)));
        ct.tick_frame(&tx);
        let (manual, _) = capture_output(&ct, &output.join(format!("{recipe}-manual.png")));

        // A real first clip edge, then a paused frame during the response.
        set_generator_param(&mut ct, &layer, &clip, 1.0);
        ct.handle_command(ContentCommand::Stop);
        ct.handle_command(ContentCommand::Play);
        ct.tick_frame(&tx);
        ct.handle_command(ContentCommand::Pause);
        ct.tick_frame(&tx);
        let (hit, _) = capture_output(&ct, &output.join(format!("{recipe}-hit.png")));
        assert!(
            difference(&manual, &hit) > 0.05,
            "{recipe}: clip trigger made no visible change"
        );

        set_generator_param(&mut ct, &layer, &clip, 0.0);
        ct.tick_frame(&tx);
        let (off, _) = capture_output(&ct, &output.join(format!("{recipe}-off-mid-hit.png")));
        assert!(
            difference(&manual, &off) < 0.05,
            "{recipe}: off did not restore manual output"
        );
        ct.handle_command(ContentCommand::Undo);
        ct.tick_frame(&tx);
        assert_eq!(
            ct.engine
                .project()
                .unwrap()
                .graph_target_owner(&target)
                .unwrap()
                .get_base_param(&clip),
            1.0
        );
        ct.handle_command(ContentCommand::Redo);
        ct.tick_frame(&tx);
        assert_eq!(
            ct.engine
                .project()
                .unwrap()
                .graph_target_owner(&target)
                .unwrap()
                .get_base_param(&clip),
            0.0
        );

        let before = ct
            .engine
            .project()
            .unwrap()
            .graph_target_owner(&target)
            .unwrap()
            .clone();
        let saved = output.join(format!("{recipe}.manifold"));
        manifold_io::saver::save_project_v1(ct.engine.project().unwrap(), &saved).unwrap();
        let mut reopened = manifold_io::loader::load_project(&saved).unwrap();
        assert!(crate::project_io::migrate_project_scene_graphs(&mut reopened).is_empty());
        let after = reopened.graph_target_owner(&target).unwrap();
        assert_eq!(after.get_base_param(&clip), 0.0);
        for &(name, value) in settings {
            let id = host_binding(&reopened, &layer, &modifier, name);
            assert_eq!(after.get_base_param(&id), value);
        }
        assert_eq!(
            after.graph.as_ref().unwrap().scene_modifiers,
            before.graph.as_ref().unwrap().scene_modifiers,
            "saved modifier snapshots are preserved"
        );
        assert_eq!(
            generator_graph(&reopened, &layer).scene_modifiers[0].id,
            modifier
        );
        ct.handle_command(ContentCommand::LoadProject(Box::new(reopened)));
        assert!(ct.graph_edit_diagnostic.is_none());
        warm_project(&mut ct, &tx);
        ct.handle_command(ContentCommand::Pause);
        ct.handle_command(ContentCommand::SeekToBeat(Beats(0.1)));
        ct.tick_frame(&tx);
        let (reload, _) = capture_output(&ct, &output.join(format!("{recipe}-reloaded.png")));
        assert!(
            difference(&off, &reload) < 0.05,
            "{recipe}: saved off state changed rendering"
        );
        eprintln!(
            "{recipe} native: trigger difference={}, off/manual difference={}, reload difference={}",
            difference(&manual, &hit),
            difference(&manual, &off),
            difference(&off, &reload)
        );
    }
}
