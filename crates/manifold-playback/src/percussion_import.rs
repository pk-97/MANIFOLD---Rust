use manifold_core::units::Bpm;
use manifold_core::{Beats, ClipId, Seconds};
// Port of Unity PercussionImportService.cs (405 lines).
// Application-layer service for applying percussion import results to the timeline.
// Owns layer resolution, clip creation, BPM auto-apply, and undo recording.

use std::collections::HashSet;

use manifold_core::PresetTypeId;
use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::math::{BeatQuantizer, MathUtils};
use manifold_core::percussion_analysis::{
    ClipDetectionAnchor, PercussionAnalysisData, PercussionClipBinding, PercussionImportOptions,
    PercussionPlacementPlan, PercussionTriggerType,
};
use manifold_core::audio_clip_detection::DetectionConfig;
use manifold_core::percussion_settings::{PercussionImportOptionsFactory, PercussionPipelineSettings};
use manifold_core::project::Project;
use manifold_core::types::{LayerType, TempoPointSource};

use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::commands::clip::{AddClipCommand, DeleteClipCommand};
use manifold_editing::commands::settings::ChangeBpmCommand;

// ─── PercussionBpmDecision ───

/// Port of Unity PercussionBpmDecision enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PercussionBpmDecision {
    None = 0,
    AutoApplied = 1,
    SuggestedLowConfidence = 2,
}

// ─── PercussionImportResult ───

/// Port of Unity PercussionImportResult struct.
#[derive(Default)]
pub struct PercussionImportResult {
    pub added_clips: i32,
    pub cleared_clips: i32,
    pub cleared_layers: i32,
    pub success: bool,
    pub undo_command: Option<Box<dyn Command>>,
}

// ─── PercussionImportService ───

const DEFAULT_BPM_AUTO_APPLY_CONFIDENCE_THRESHOLD: f32 = 0.72;

/// Port of Unity PercussionImportService.
/// Application-layer service for applying percussion import results to the timeline.
/// Owns layer resolution, clip creation, BPM auto-apply, and undo recording.
/// No UI or MonoBehaviour dependencies.
pub struct PercussionImportService {
    bpm_auto_apply_confidence_threshold: f32,
}

impl PercussionImportService {
    pub fn new() -> Self {
        Self {
            bpm_auto_apply_confidence_threshold: DEFAULT_BPM_AUTO_APPLY_CONFIDENCE_THRESHOLD,
        }
    }

    pub fn new_with_settings(settings: Option<&PercussionPipelineSettings>) -> Self {
        let threshold = match settings {
            Some(s) => s.global.bpm_auto_apply_confidence,
            None => DEFAULT_BPM_AUTO_APPLY_CONFIDENCE_THRESHOLD,
        };
        Self {
            bpm_auto_apply_confidence_threshold: threshold,
        }
    }

    /// Apply a placement plan to the timeline.
    ///
    /// `source_clip_id` is the source audio clip for per-clip detection
    /// (audio-clip-detection): generated triggers are tagged
    /// `detection_source = id`, so a re-detect of that clip clears only its own
    /// prior triggers and no project-global state is written.
    pub fn apply_placement_plan(
        &self,
        project: &mut Project,
        plan: Option<&PercussionPlacementPlan>,
        options: Option<&PercussionImportOptions>,
        source_clip_id: Option<&ClipId>,
    ) -> PercussionImportResult {
        let mut result = PercussionImportResult::default();

        let plan = match plan {
            Some(p) => p,
            None => return result,
        };

        let placements = plan.placements();
        if project.timeline.layers.is_empty() && placements.is_empty() {
            return result;
        }

        let layout_map = self.resolve_import_layer_layout(project, options);

        let mut commands: Vec<Box<dyn Command>> = Vec::new();

        // First pass: resolve target layers and collect unique indices to clear.
        let mut target_layer_indices: Vec<i32> = vec![-1; placements.len()];
        let mut layers_to_clear: HashSet<i32> = HashSet::new();

        for (i, placement) in placements.iter().enumerate() {
            let idx = if let Some(&mapped) = layout_map.get(&placement.trigger_type) {
                mapped
            } else {
                self.resolve_target_layer_index_for_placement(project, placement)
            };
            target_layer_indices[i] = idx;
            if idx >= 0 {
                layers_to_clear.insert(idx);
            }
        }

        // Clear existing clips on reused layers. Per-clip detection clears only
        // the triggers this audio clip produced (multi-source safe, preserves
        // hand-placed clips and other clips' triggers); the legacy wizard clears
        // the whole layer (preserving layer effects, blend mode, gen params).
        for &layer_index in &layers_to_clear {
            let layer = match project.timeline.layers.get_mut(layer_index as usize) {
                Some(l) => l,
                None => continue,
            };
            let layer_lid = layer.layer_id.clone();
            let existing_clips: Vec<TimelineClip> = layer.clips.clone();
            for existing in existing_clips {
                if let Some(src) = source_clip_id
                    && existing.detection_source.as_ref() != Some(src)
                {
                    // Per-clip mode: leave clips this source didn't create.
                    continue;
                }
                commands.push(Box::new(DeleteClipCommand::new(
                    existing.clone(),
                    layer_lid.clone(),
                )));
                layer.remove_clip(&existing.id);
                result.cleared_clips += 1;
            }
        }

        result.cleared_layers = layers_to_clear.len() as i32;

        // Second pass: add new clips to the (now-empty) target layers.
        for (i, placement) in placements.iter().enumerate() {
            let target_layer_index = target_layer_indices[i];
            if target_layer_index < 0 {
                continue;
            }

            // Set trigger-based layer name if not already matching. Legacy wizard
            // only: per-clip detection may route to a user-named layer ("Drums"),
            // so renaming it to the trigger ("Kick") would be wrong.
            if source_clip_id.is_none() {
                let trigger_layer_name = get_trigger_layer_name(placement.trigger_type);
                if !trigger_layer_name.is_empty()
                    && let Some(target_layer) =
                        project.timeline.layers.get_mut(target_layer_index as usize)
                    && target_layer.name != trigger_layer_name
                {
                    target_layer.name = trigger_layer_name.clone();
                }
            }

            let target_layer_lid = project
                .timeline
                .layers
                .get(target_layer_index as usize)
                .map(|l| l.layer_id.clone())
                .unwrap_or_default();

            let mut timeline_clip: TimelineClip = if placement.is_generator() {
                TimelineClip::new_generator(placement.start_beat, placement.duration_beats)
            } else {
                let video_clip_id = match &placement.video_clip_id {
                    Some(id) if !id.is_empty() => id.clone(),
                    _ => continue,
                };
                if !project.video_library.has_clip(&video_clip_id) {
                    continue;
                }

                TimelineClip::new_video(
                    video_clip_id,
                    placement.start_beat,
                    placement.duration_beats,
                    Seconds(0.0),
                )
            };

            // Enforce non-overlap: trim any existing clip that extends past this clip's start,
            // and remove any fully-contained clips.
            {
                let target_layer =
                    match project.timeline.layers.get_mut(target_layer_index as usize) {
                        Some(l) => l,
                        None => continue,
                    };
                let clip_start = timeline_clip.start_beat;
                let clip_end = timeline_clip.end_beat();

                // Collect indices to remove and IDs to trim — avoid borrow issues.
                let mut ids_to_remove: Vec<ClipId> = Vec::new();
                let mut ids_to_trim: Vec<(ClipId, Beats)> = Vec::new();

                for existing in target_layer.clips.iter() {
                    if existing.start_beat < clip_start && existing.end_beat() > clip_start {
                        // Overlapping from the left: trim its duration.
                        ids_to_trim.push((existing.id.clone(), clip_start - existing.start_beat));
                    } else if existing.start_beat >= clip_start && existing.end_beat() <= clip_end {
                        // Fully contained: remove.
                        ids_to_remove.push(existing.id.clone());
                    }
                }

                for id in &ids_to_remove {
                    target_layer.remove_clip(id);
                }
                for (id, new_duration) in &ids_to_trim {
                    if let Some(c) = target_layer.find_clip_mut(id) {
                        c.duration_beats = *new_duration;
                    }
                }
            }

            // Tag the trigger with its source audio clip (per-clip detection),
            // so a later re-detect of that clip clears only these triggers.
            timeline_clip.detection_source = source_clip_id.cloned();

            let spb = 60.0 / project.settings.bpm.0.max(1.0);
            let mut add_cmd = AddClipCommand::new(timeline_clip, target_layer_lid.clone(), spb);
            add_cmd.execute(project);
            commands.push(Box::new(add_cmd));
            result.added_clips += 1;
        }

        if result.added_clips <= 0 {
            log::warn!(
                "[PercussionImportService] Placement plan had entries but no clips could be added."
            );
            return result;
        }

        let command: Box<dyn Command> = if commands.len() == 1 {
            commands.remove(0)
        } else {
            Box::new(CompositeCommand::new(
                commands,
                "Import percussion clips".to_string(),
            ))
        };

        result.undo_command = Some(command);
        result.success = true;

        if result.cleared_clips > 0 {
            log::info!(
                "[PercussionImportService] Replaced {} existing clip(s) across {} layer(s).",
                result.cleared_clips,
                result.cleared_layers,
            );
        }

        result
    }

    pub fn apply_detected_bpm(
        &self,
        project: &mut Project,
        analysis: Option<&PercussionAnalysisData>,
        source_label: &str,
    ) -> (PercussionBpmDecision, Option<Box<dyn Command>>) {
        let analysis = match analysis {
            Some(a) => a,
            None => return (PercussionBpmDecision::None, None),
        };

        let detected_bpm_raw = analysis.bpm.0;
        if !MathUtils::is_finite(detected_bpm_raw) || detected_bpm_raw <= 0.0 {
            return (PercussionBpmDecision::None, None);
        }

        let detected_bpm = detected_bpm_raw.clamp(20.0, 300.0);
        let current_bpm = project.settings.bpm.0;
        if (current_bpm - detected_bpm).abs() < (BeatQuantizer::BPM_STEP * 0.5) {
            return (PercussionBpmDecision::None, None);
        }

        let bpm_confidence = analysis.bpm_confidence;
        if bpm_confidence < self.bpm_auto_apply_confidence_threshold {
            log::warn!(
                "[PercussionImportService] Detected BPM {:.2} from '{}' \
                not auto-applied (confidence={:.2} < {:.2}).",
                detected_bpm,
                source_label,
                bpm_confidence,
                self.bpm_auto_apply_confidence_threshold,
            );
            return (PercussionBpmDecision::SuggestedLowConfidence, None);
        }

        let flatten_tempo_map = project.tempo_map.point_count() <= 1;
        let old_tempo_points = project.tempo_map.clone_points();

        let mut change_bpm_command = ChangeBpmCommand::with_tempo_map(
            Bpm(current_bpm),
            Bpm(detected_bpm),
            TempoPointSource::Recorded,
            flatten_tempo_map,
            old_tempo_points,
        );

        change_bpm_command.execute(project);

        project.recording_provenance.set_recorded_project_bpm(
            Bpm(detected_bpm),
            TempoPointSource::Recorded,
            true,
        );

        log::info!(
            "[PercussionImportService] Applied detected BPM {:.2} from '{}' (confidence={:.2}).",
            detected_bpm,
            source_label,
            bpm_confidence,
        );

        (
            PercussionBpmDecision::AutoApplied,
            Some(Box::new(change_bpm_command)),
        )
    }

    fn resolve_target_layer_index_for_placement(
        &self,
        project: &mut Project,
        placement: &manifold_core::percussion_analysis::PercussionClipPlacement,
    ) -> i32 {
        if placement.is_generator() {
            self.resolve_generator_layer_index(
                project,
                placement.layer_index,
                placement.generator_type.clone(),
                placement.trigger_type,
            )
        } else {
            self.resolve_video_layer_index(project, placement.layer_index)
        }
    }

    fn resolve_generator_layer_index(
        &self,
        project: &mut Project,
        preferred_index: i32,
        generator_type: PresetTypeId,
        trigger_type: PercussionTriggerType,
    ) -> i32 {
        if generator_type == PresetTypeId::NONE {
            return -1;
        }

        // Exact match: preferred index with matching generator type.
        if preferred_index >= 0 && (preferred_index as usize) < project.timeline.layers.len() {
            let preferred = &project.timeline.layers[preferred_index as usize];
            if preferred.layer_type == LayerType::Generator
                && *preferred.generator_type() == generator_type
            {
                return preferred_index;
            }
        }

        // Scan for exact generator type match.
        for i in 0..project.timeline.layers.len() {
            let layer = &project.timeline.layers[i];
            if layer.layer_type == LayerType::Generator && *layer.generator_type() == generator_type
            {
                return i as i32;
            }
        }

        // Fallback: match by trigger name — reuses layers whose generator type was customized by the user.
        let trigger_name = get_trigger_layer_name(trigger_type);
        if !trigger_name.is_empty() {
            for i in 0..project.timeline.layers.len() {
                let layer = &project.timeline.layers[i];
                if layer.layer_type == LayerType::Generator && layer.name == trigger_name {
                    return i as i32;
                }
            }
        }

        let idx = project.timeline.add_layer(
            &format!("{:?} Auto", generator_type),
            LayerType::Generator,
            generator_type,
        );
        idx as i32
    }

    fn resolve_video_layer_index(&self, project: &mut Project, preferred_index: i32) -> i32 {
        if preferred_index >= 0 && (preferred_index as usize) < project.timeline.layers.len() {
            let preferred = &project.timeline.layers[preferred_index as usize];
            if preferred.layer_type != LayerType::Generator
                && preferred.layer_type != LayerType::Group
            {
                return preferred_index;
            }
        }

        for i in 0..project.timeline.layers.len() {
            let layer = &project.timeline.layers[i];
            if layer.layer_type != LayerType::Generator && layer.layer_type != LayerType::Group {
                return i as i32;
            }
        }

        let idx = project.timeline.add_layer(
            &format!("Layer {}", project.timeline.layers.len()),
            LayerType::Video,
            PresetTypeId::NONE,
        );
        idx as i32
    }

    /// Resolves each binding's trigger type to an actual project layer index
    /// by searching for existing layers by name or creating new ones in layout order.
    /// Returns a map of trigger type → resolved project layer index.
    fn resolve_import_layer_layout(
        &self,
        project: &mut Project,
        options: Option<&PercussionImportOptions>,
    ) -> std::collections::HashMap<PercussionTriggerType, i32> {
        let mut layout_map = std::collections::HashMap::new();

        let options = match options {
            Some(o) => o,
            None => return layout_map,
        };

        let bindings = &options.bindings;
        if bindings.is_empty() {
            return layout_map;
        }

        // Sort bindings by layer_index (layout order).
        let mut sorted_bindings: Vec<PercussionClipBinding> = bindings.clone();
        sorted_bindings.sort_by(|a, b| a.layer_index.cmp(&b.layer_index));

        for binding in &sorted_bindings {
            if layout_map.contains_key(&binding.trigger_type) {
                continue;
            }

            // Explicit per-instrument routing (audio-clip inspector) wins: if the
            // chosen layer still exists, place there directly and skip the
            // trigger-name layout. A stale id (layer deleted) falls through to
            // name resolution.
            if let Some(target) = &binding.target_layer
                && let Some(idx) = project.timeline.layer_index_for_id(target)
            {
                layout_map.insert(binding.trigger_type, idx as i32);
                continue;
            }

            let layer_name = get_trigger_layer_name(binding.trigger_type);
            let existing_index = find_layer_by_name_index(&project.timeline.layers, &layer_name);

            if let Some(idx) = existing_index {
                // Preserve the layer's current generator type and parameters.
                // The user may have customised the generator after initial import.
                layout_map.insert(binding.trigger_type, idx as i32);
            } else {
                let idx = if binding.uses_generator() {
                    project.timeline.add_layer(
                        &layer_name,
                        LayerType::Generator,
                        binding.generator_type.clone(),
                    )
                } else {
                    project
                        .timeline
                        .add_layer(&layer_name, LayerType::Video, PresetTypeId::NONE)
                };
                layout_map.insert(binding.trigger_type, idx as i32);
            }
        }

        layout_map
    }
}

impl Default for PercussionImportService {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Per-clip detection options ───

/// Build `PercussionImportOptions` for one audio clip from its `DetectionConfig`.
///
/// Starts from the default/project options factory (which carries the proven
/// per-instrument generator + duration defaults), then overlays the clip's own
/// knobs: quantize, onset compensation, the enabled instrument set, and the
/// sensitivity → `minimum_confidence` mapping. The `anchor` makes placement
/// clip-anchored and warp-aware (the planner maps event times through it and
/// trims to the clip window), so `start_beat_offset` stays zero. Per-instrument
/// `target_layer` (set by the inspector dropdown) is carried onto each binding
/// and honoured at apply time; `None` routes by trigger name (the default).
/// See `docs/AUDIO_CLIP_DETECTION_DESIGN.md`.
pub fn build_clip_detection_options(
    project: &Project,
    pipeline_settings: Option<&PercussionPipelineSettings>,
    config: &DetectionConfig,
    anchor: ClipDetectionAnchor,
) -> PercussionImportOptions {
    // The anchor owns placement, so the factory's start-beat offset is zero.
    let mut options = PercussionImportOptionsFactory::create_default_with_settings(
        project,
        pipeline_settings,
        Beats::ZERO,
    );

    options.quantize_to_grid = config.quantize_on;
    options.quantize_step_beats = config.quantize_step_beats;
    options.onset_compensation_seconds = config.onset_compensation;
    options.clip_anchor = Some(anchor);

    // Keep only instruments the clip has enabled, and apply per-instrument
    // sensitivity as the confidence gate.
    options.bindings.retain(|b| {
        config
            .instrument(b.trigger_type)
            .is_some_and(|i| i.enabled)
    });
    for binding in options.bindings.iter_mut() {
        if let Some(instrument) = config.instrument(binding.trigger_type) {
            binding.minimum_confidence = instrument.min_confidence();
            binding.target_layer = instrument.target_layer.clone();
        }
    }

    options
}

// ─── Helper functions ───

pub(crate) fn get_trigger_layer_name(trigger_type: PercussionTriggerType) -> String {
    if trigger_type == PercussionTriggerType::Unknown {
        return "Percussion".to_string();
    }
    format!("{:?}", trigger_type)
}

fn find_layer_by_name_index(layers: &[Layer], name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    for (i, layer) in layers.iter().enumerate() {
        if layer.layer_type == LayerType::Group {
            continue;
        }
        if layer.name == name {
            return Some(i);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::audio_clip_detection::DetectionConfig;
    use manifold_core::clip::TimelineClip;
    use manifold_core::percussion_analysis::PercussionClipPlacement;

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
        let mut options = PercussionImportOptions::default();
        options.bindings.push(PercussionClipBinding::new(
            PercussionTriggerType::Kick,
            0,
            None,
            gen_type(),
            Beats(0.25),
            0.0,
        ));
        options
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
        let svc = PercussionImportService::new();
        let clip_id = ClipId::new("audioA");

        let result = svc.apply_placement_plan(
            &mut project,
            Some(&kick_plan(1.0)),
            Some(&kick_options()),
            Some(&clip_id),
        );

        assert!(result.success);
        assert_eq!(result.added_clips, 1);
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
            layer.restore_clip(TimelineClip::new_generator(Beats::from_f32(20.0), Beats(0.25)));
        }
        project.timeline.mark_clip_lookup_dirty();

        let svc = PercussionImportService::new();
        let clip_id = ClipId::new("audioA");

        svc.apply_placement_plan(
            &mut project,
            Some(&kick_plan(1.0)),
            Some(&kick_options()),
            Some(&clip_id),
        );
        assert_eq!(project.timeline.layers[idx].clips.len(), 3);

        // Re-detect audioA: clears only audioA's prior trigger, re-adds one.
        svc.apply_placement_plan(
            &mut project,
            Some(&kick_plan(1.0)),
            Some(&kick_options()),
            Some(&clip_id),
        );

        let clips = &project.timeline.layers[idx].clips;
        assert_eq!(clips.len(), 3, "foreign + hand-placed preserved, audioA replaced");
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
    }

    #[test]
    fn explicit_target_layer_routes_there_and_skips_rename() {
        // Two layers: a user-named "Drums" layer and a separate "Kick" layer.
        // A Kick binding routed explicitly to "Drums" must land on "Drums" and
        // must NOT rename it to "Kick".
        let mut project = Project::default();
        let drums_idx =
            project
                .timeline
                .add_layer("Drums", LayerType::Generator, gen_type());
        let drums_lid = project.timeline.layers[drums_idx].layer_id.clone();
        project
            .timeline
            .add_layer("Kick", LayerType::Generator, gen_type());

        let mut options = kick_options();
        options.bindings[0].target_layer = Some(drums_lid.clone());

        let svc = PercussionImportService::new();
        let clip_id = ClipId::new("audioA");
        let result = svc.apply_placement_plan(
            &mut project,
            Some(&kick_plan(1.0)),
            Some(&options),
            Some(&clip_id),
        );

        assert!(result.success);
        assert_eq!(project.timeline.layers[drums_idx].clips.len(), 1, "routed to Drums");
        assert_eq!(project.timeline.layers[drums_idx].name, "Drums", "user layer not renamed");
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
                _ => {}
            }
        }

        let anchor = ClipDetectionAnchor::new(
            Beats(4.0),
            Beats(8.0),
            Seconds::ZERO,
            120.0,
            manifold_core::units::Bpm(120.0),
        );
        let options = build_clip_detection_options(&project, None, &config, anchor);

        // The anchor owns placement; the legacy start-beat offset stays zero.
        assert_eq!(options.start_beat_offset, Beats::ZERO);
        let a = options.clip_anchor.expect("anchor is set");
        assert_eq!(a.start_beat, Beats(4.0));
        let kick = options
            .bindings
            .iter()
            .find(|b| b.trigger_type == PercussionTriggerType::Kick)
            .expect("enabled Kick survives");
        assert!(kick.minimum_confidence.abs() < 1e-6, "max sensitivity -> 0 threshold");
        assert!(
            !options
                .bindings
                .iter()
                .any(|b| b.trigger_type == PercussionTriggerType::Snare),
            "disabled Snare is dropped"
        );
    }
}
