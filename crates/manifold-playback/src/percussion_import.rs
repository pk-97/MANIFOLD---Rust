//! Writes a Detect plan onto the timeline: each instrument's layer is found or made, the audio clip's previous
//! triggers are removed and the new ones added, as one undo step.

use std::collections::HashMap;

use manifold_core::audio_clip_detection::DetectionConfig;
use manifold_core::clip::TimelineClip;
use manifold_core::id::LayerId;
use manifold_core::percussion_analysis::{
    ClipDetectionAnchor, PercussionImportOptions, PercussionPlacementPlan, PercussionTriggerType,
};
use manifold_core::percussion_settings::PercussionImportOptionsFactory;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::{ClipId, PresetTypeId, Seconds};

use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::commands::clip::{AddClipCommand, DeleteClipCommand};

/// Removes every trigger the audio clip `source` placed, on every layer, and returns the executed deletes. Other
/// clips' triggers and hand-placed clips stay.
pub fn remove_triggers(project: &mut Project, source: &ClipId) -> Vec<Box<dyn Command>> {
    let mut commands: Vec<Box<dyn Command>> = Vec::new();
    for layer in project.timeline.layers.iter_mut() {
        let owned: Vec<TimelineClip> = layer
            .clips
            .iter()
            .filter(|c| c.detection_source.as_ref() == Some(source))
            .cloned()
            .collect();
        for clip in owned {
            layer.remove_clip(&clip.id);
            commands.push(Box::new(DeleteClipCommand::new(
                clip,
                layer.layer_id.clone(),
            )));
        }
    }
    if !commands.is_empty() {
        project.timeline.mark_clip_lookup_dirty();
    }
    commands
}

/// Replaces the audio clip `source`'s triggers with `plan`'s, each tagged with `source`. Returns how many clips it
/// added and the executed undo step, `None` when nothing changed.
pub fn apply_placement_plan(
    project: &mut Project,
    plan: &PercussionPlacementPlan,
    options: &PercussionImportOptions,
    source: &ClipId,
) -> (usize, Option<Box<dyn Command>>) {
    let mut commands = remove_triggers(project, source);
    let layers = if plan.placements().is_empty() {
        HashMap::new()
    } else {
        resolve_layers(project, options)
    };
    let mut added = 0;
    for placement in plan.placements() {
        let layer = layers
            .get(&placement.trigger_type)
            .expect("every placement comes from one of the options' bindings")
            .clone();
        let mut clip = if placement.is_generator() {
            TimelineClip::new_generator(placement.start_beat, placement.duration_beats)
        } else {
            match &placement.video_clip_id {
                Some(id) if project.video_library.has_clip(id) => TimelineClip::new_video(
                    id.clone(),
                    placement.start_beat,
                    placement.duration_beats,
                    Seconds::ZERO,
                ),
                _ => continue,
            }
        };
        clip.detection_source = Some(source.clone());
        // The add trims or removes what it overlaps, and its undo restores them.
        let mut add = AddClipCommand::new(clip, layer);
        add.execute(project);
        commands.push(Box::new(add));
        added += 1;
    }
    let undo = (!commands.is_empty()).then(|| -> Box<dyn Command> {
        Box::new(CompositeCommand::new(
            commands,
            "Detect triggers".to_string(),
        ))
    });
    (added, undo)
}

/// Each binding's layer: the one the inspector routed it to while that layer exists, else a non-group layer named
/// after the trigger ("Kick"), else a new one of that name.
fn resolve_layers(
    project: &mut Project,
    options: &PercussionImportOptions,
) -> HashMap<PercussionTriggerType, LayerId> {
    let mut layers = HashMap::new();
    for binding in &options.bindings {
        if let Some(target) = &binding.target_layer
            && project.timeline.layer_index_for_id(target).is_some()
        {
            layers.insert(binding.trigger_type, target.clone());
            continue;
        }
        let name = get_trigger_layer_name(binding.trigger_type);
        let existing = project
            .timeline
            .layers
            .iter()
            .find(|l| l.layer_type != LayerType::Group && l.name == name);
        let id = match existing {
            Some(layer) => layer.layer_id.clone(),
            None => {
                let idx = if binding.uses_generator() {
                    project.timeline.add_layer(
                        &name,
                        LayerType::Generator,
                        binding.generator_type.clone(),
                    )
                } else {
                    project
                        .timeline
                        .add_layer(&name, LayerType::Video, PresetTypeId::NONE)
                };
                project.timeline.layers[idx].layer_id.clone()
            }
        };
        layers.insert(binding.trigger_type, id);
    }
    layers
}

// ─── Per-clip detection options ───

/// Build `PercussionImportOptions` for one audio clip from its `DetectionConfig`.
///
/// Starts from the default/project options factory (which carries the proven
/// per-instrument generator + duration defaults), then overlays the clip's own
/// knobs: quantize, onset compensation, the enabled instrument set, and the
/// sensitivity → `minimum_confidence` mapping. The `anchor` makes placement
/// clip-anchored and warp-aware (the planner maps event times through it and
/// trims to the clip window). Per-instrument
/// `target_layer` (set by the inspector dropdown) is carried onto each binding
/// and honoured at apply time; `None` routes by trigger name (the default).
pub fn build_clip_detection_options(
    project: &Project,
    config: &DetectionConfig,
    anchor: ClipDetectionAnchor,
) -> PercussionImportOptions {
    let mut options = PercussionImportOptionsFactory::create_default(project, anchor);

    options.quantize_to_grid = config.quantize_on;
    options.quantize_step_beats = config.quantize_step_beats;
    options.onset_compensation_seconds = config.onset_compensation;

    // Keep only instruments the clip has enabled, and apply per-instrument
    // sensitivity as the confidence gate.
    options
        .bindings
        .retain(|b| config.instrument(b.trigger_type).is_some_and(|i| i.enabled));
    for binding in options.bindings.iter_mut() {
        if let Some(instrument) = config.instrument(binding.trigger_type) {
            binding.minimum_confidence = instrument.min_confidence();
            binding.target_layer = instrument.target_layer.clone();
        }
    }

    options
}

pub(crate) fn get_trigger_layer_name(trigger_type: PercussionTriggerType) -> String {
    format!("{:?}", trigger_type)
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::Beats;
    use manifold_core::audio_clip_detection::DetectionConfig;
    use manifold_core::clip::TimelineClip;
    use manifold_core::percussion_analysis::{PercussionClipBinding, PercussionClipPlacement};

    fn gen_type() -> PresetTypeId {
        PresetTypeId::from_string("TestGen".to_string())
    }

    fn project_with_kick_layer() -> (Project, usize) {
        let mut project = Project::default();
        let idx = project
            .timeline
            .add_layer("Kick", LayerType::Generator, gen_type());
        (project, idx)
    }

    fn kick_options() -> PercussionImportOptions {
        PercussionImportOptions {
            quantize_to_grid: true,
            quantize_step_beats: Beats(0.25),
            default_clip_duration_beats: Beats(0.25),
            onset_compensation_seconds: Seconds::ZERO,
            bindings: vec![PercussionClipBinding::new(
                PercussionTriggerType::Kick,
                0,
                gen_type(),
                Beats(0.25),
                0.0,
            )],
            clip_anchor: ClipDetectionAnchor::new(
                Beats::ZERO,
                Beats(64.0),
                Seconds::ZERO,
                120.0,
                manifold_core::units::Bpm(120.0),
            ),
        }
    }

    fn kick_plan(beat: f32) -> PercussionPlacementPlan {
        let mut plan = PercussionPlacementPlan::new();
        plan.add_placement(PercussionClipPlacement::new(
            PercussionTriggerType::Kick,
            0,
            None,
            gen_type(),
            Beats::from_f32(beat),
            Beats(0.25),
            0.9,
            beat,
        ));
        plan
    }

    #[test]
    fn per_clip_apply_tags_source_and_skips_global_state() {
        let (mut project, idx) = project_with_kick_layer();
        let clip_id = ClipId::new("audioA");

        let (added, undo) =
            apply_placement_plan(&mut project, &kick_plan(1.0), &kick_options(), &clip_id);

        assert_eq!(added, 1);
        assert!(undo.is_some());
        let layer = &project.timeline.layers[idx];
        assert_eq!(layer.clips.len(), 1);
        assert_eq!(layer.clips[0].detection_source.as_ref(), Some(&clip_id));
    }

    #[test]
    fn per_clip_redetect_clears_only_own_triggers() {
        let (mut project, idx) = project_with_kick_layer();
        let foreign_id = ClipId::new("audioB");

        // Seed a foreign trigger + a hand-placed clip, away from the trigger beat.
        {
            let layer = &mut project.timeline.layers[idx];
            let mut foreign = TimelineClip::new_generator(Beats::from_f32(10.0), Beats(0.25));
            foreign.detection_source = Some(foreign_id.clone());
            layer.restore_clip(foreign);
            layer.restore_clip(TimelineClip::new_generator(
                Beats::from_f32(20.0),
                Beats(0.25),
            ));
        }
        project.timeline.mark_clip_lookup_dirty();
        let clip_id = ClipId::new("audioA");

        apply_placement_plan(&mut project, &kick_plan(1.0), &kick_options(), &clip_id);
        assert_eq!(project.timeline.layers[idx].clips.len(), 3);

        // Re-detect audioA: clears only audioA's prior trigger, re-adds one.
        apply_placement_plan(&mut project, &kick_plan(1.0), &kick_options(), &clip_id);

        let clips = &project.timeline.layers[idx].clips;
        assert_eq!(
            clips.len(),
            3,
            "foreign + hand-placed preserved, audioA replaced"
        );
        let own = clips
            .iter()
            .filter(|c| c.detection_source.as_ref() == Some(&clip_id))
            .count();
        assert_eq!(own, 1, "exactly one audioA trigger after re-detect");
        assert!(
            clips
                .iter()
                .any(|c| c.detection_source.as_ref() == Some(&foreign_id)),
            "another clip's triggers are untouched"
        );
        assert!(
            clips.iter().any(|c| c.detection_source.is_none()),
            "hand-placed clip is untouched"
        );

        // A replan that keeps no hits removes the clip's old triggers.
        apply_placement_plan(
            &mut project,
            &PercussionPlacementPlan::new(),
            &kick_options(),
            &clip_id,
        );
        assert!(
            project.timeline.layers[idx]
                .clips
                .iter()
                .all(|c| c.detection_source.as_ref() != Some(&clip_id))
        );
    }

    #[test]
    fn explicit_target_layer_routes_there() {
        // A Kick binding routed to a user-named "Drums" layer lands there, not on the "Kick" layer, and leaves its
        // name alone.
        let mut project = Project::default();
        let drums_idx = project
            .timeline
            .add_layer("Drums", LayerType::Generator, gen_type());
        let drums_lid = project.timeline.layers[drums_idx].layer_id.clone();
        project
            .timeline
            .add_layer("Kick", LayerType::Generator, gen_type());

        let mut options = kick_options();
        options.bindings[0].target_layer = Some(drums_lid.clone());
        let clip_id = ClipId::new("audioA");
        let (added, _) = apply_placement_plan(&mut project, &kick_plan(1.0), &options, &clip_id);

        assert_eq!(added, 1);
        assert_eq!(
            project.timeline.layers[drums_idx].clips.len(),
            1,
            "routed to Drums"
        );
        assert_eq!(
            project.timeline.layers[drums_idx].name, "Drums",
            "user layer not renamed"
        );
    }

    #[test]
    fn options_from_config_filters_disabled_and_maps_sensitivity() {
        let project = Project::default();
        let mut config = DetectionConfig::default();
        // Disable Snare; crank Kick sensitivity to max (-> confidence 0).
        for inst in config.instruments.iter_mut() {
            match inst.trigger_type {
                PercussionTriggerType::Snare => inst.enabled = false,
                PercussionTriggerType::Kick => inst.sensitivity = 1.0,
            }
        }

        let anchor = ClipDetectionAnchor::new(
            Beats(4.0),
            Beats(8.0),
            Seconds::ZERO,
            120.0,
            manifold_core::units::Bpm(120.0),
        );
        let options = build_clip_detection_options(&project, &config, anchor);

        assert_eq!(options.clip_anchor.start_beat, Beats(4.0));
        let kick = options
            .bindings
            .iter()
            .find(|b| b.trigger_type == PercussionTriggerType::Kick)
            .expect("enabled Kick survives");
        assert!(
            kick.minimum_confidence.abs() < 1e-6,
            "max sensitivity -> 0 threshold"
        );
        assert!(
            !options
                .bindings
                .iter()
                .any(|b| b.trigger_type == PercussionTriggerType::Snare),
            "disabled Snare is dropped"
        );
    }
}
