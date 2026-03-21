// Port of Unity PercussionImportService.cs (405 lines).
// Application-layer service for applying percussion import results to the timeline.
// Owns layer resolution, clip creation, BPM auto-apply, and undo recording.

use std::collections::HashSet;

use manifold_core::math::{BeatQuantizer, MathUtils};
use manifold_core::percussion::ImportedPercussionClipPlacement;
use manifold_core::percussion_analysis::{
    PercussionAnalysisData, PercussionClipBinding, PercussionImportOptions, PercussionPlacementPlan,
    PercussionTriggerType,
};
use manifold_core::percussion_settings::PercussionPipelineSettings;
use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::types::{GeneratorType, LayerType, TempoPointSource};

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

    pub fn apply_placement_plan(
        &self,
        project: &mut Project,
        plan: Option<&PercussionPlacementPlan>,
        options: Option<&PercussionImportOptions>,
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
        let mut import_provenance: Vec<ImportedPercussionClipPlacement> =
            Vec::with_capacity(placements.len());

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

        // Clear existing clips on reused layers (preserves layer effects, blend mode, gen params, etc.)
        for &layer_index in &layers_to_clear {
            let layer = match project.timeline.layers.get_mut(layer_index as usize) {
                Some(l) => l,
                None => continue,
            };
            let existing_clips: Vec<TimelineClip> = layer.clips.clone();
            for existing in existing_clips {
                commands.push(Box::new(DeleteClipCommand::new(
                    existing.clone(),
                    layer_index,
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

            // Set trigger-based layer name if not already matching.
            let trigger_layer_name = get_trigger_layer_name(placement.trigger_type);
            if !trigger_layer_name.is_empty() {
                if let Some(target_layer) =
                    project.timeline.layers.get_mut(target_layer_index as usize)
                {
                    if target_layer.name != trigger_layer_name {
                        target_layer.name = trigger_layer_name.clone();
                    }
                }
            }

            let timeline_clip: TimelineClip = if placement.is_generator() {
                // Use the layer's current generator type — respects user customisation
                // (e.g. user swapped Flash→Voronoi on the Kick layer).
                let effective_gen_type = {
                    let target_layer = match project.timeline.layers.get(target_layer_index as usize) {
                        Some(l) => l,
                        None => continue,
                    };
                    let layer_gen = target_layer.generator_type();
                    if layer_gen != GeneratorType::None {
                        layer_gen
                    } else {
                        placement.generator_type
                    }
                };

                TimelineClip::new_generator(
                    effective_gen_type,
                    target_layer_index,
                    placement.start_beat,
                    placement.duration_beats,
                )
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
                    target_layer_index,
                    placement.start_beat,
                    placement.duration_beats,
                    0.0,
                )
            };

            // Enforce non-overlap: trim any existing clip that extends past this clip's start,
            // and remove any fully-contained clips.
            {
                let target_layer = match project.timeline.layers.get_mut(target_layer_index as usize) {
                    Some(l) => l,
                    None => continue,
                };
                let clip_start = timeline_clip.start_beat;
                let clip_end = timeline_clip.end_beat();

                // Collect indices to remove and IDs to trim — avoid borrow issues.
                let mut ids_to_remove: Vec<String> = Vec::new();
                let mut ids_to_trim: Vec<(String, f32)> = Vec::new();

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

            let clip_id = timeline_clip.id.clone();
            let add_cmd = AddClipCommand::new(timeline_clip.clone(), target_layer_index);

            if let Some(target_layer) = project.timeline.layers.get_mut(target_layer_index as usize) {
                target_layer.add_clip(timeline_clip);
            }
            commands.push(Box::new(add_cmd));
            result.added_clips += 1;

            import_provenance.push(ImportedPercussionClipPlacement {
                clip_id,
                source_time_seconds: placement.source_time_seconds,
                start_beat_offset: options.map_or(0.0, |o| o.start_beat_offset),
                quantize_to_grid: options.is_some_and(|o| o.quantize_to_grid),
                quantize_step_beats: options.map_or(0.0, |o| o.quantize_step_beats),
                alignment_offset_beats: 0.0,
                alignment_slope_beats_per_second: 0.0,
                alignment_pivot_seconds: 0.0,
            });
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

        // Update import provenance on the project.
        let perc_import = project.percussion_import.get_or_insert_with(Default::default);
        perc_import.clip_placements.clear();
        perc_import.clip_placements.extend(import_provenance);
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

        let detected_bpm = analysis.bpm;
        if !MathUtils::is_finite(detected_bpm) || detected_bpm <= 0.0 {
            return (PercussionBpmDecision::None, None);
        }

        let detected_bpm = detected_bpm.clamp(20.0, 300.0);
        let current_bpm = project.settings.bpm;
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

        let flatten_tempo_map = project.tempo_map.points.len() <= 1;
        let old_tempo_points = project.tempo_map.clone_points();

        let mut change_bpm_command = ChangeBpmCommand::with_tempo_map(
            current_bpm,
            detected_bpm,
            TempoPointSource::Recorded,
            flatten_tempo_map,
            old_tempo_points,
        );

        change_bpm_command.execute(project);

        project.recording_provenance.set_recorded_project_bpm(
            detected_bpm,
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
                placement.generator_type,
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
        generator_type: GeneratorType,
        trigger_type: PercussionTriggerType,
    ) -> i32 {
        if generator_type == GeneratorType::None {
            return -1;
        }

        // Exact match: preferred index with matching generator type.
        if preferred_index >= 0 && (preferred_index as usize) < project.timeline.layers.len() {
            let preferred = &project.timeline.layers[preferred_index as usize];
            if preferred.layer_type == LayerType::Generator
                && preferred.generator_type() == generator_type
            {
                return preferred_index;
            }
        }

        // Scan for exact generator type match.
        for i in 0..project.timeline.layers.len() {
            let layer = &project.timeline.layers[i];
            if layer.layer_type == LayerType::Generator && layer.generator_type() == generator_type
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

    fn resolve_video_layer_index(
        &self,
        project: &mut Project,
        preferred_index: i32,
    ) -> i32 {
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
            GeneratorType::None,
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

            let layer_name = get_trigger_layer_name(binding.trigger_type);
            let existing_index = find_layer_by_name_index(&project.timeline.layers, &layer_name);

            if let Some(idx) = existing_index {
                // Preserve the layer's current generator type and parameters.
                // The user may have customised the generator after initial import.
                layout_map.insert(binding.trigger_type, idx as i32);
            } else {
                let idx = if binding.uses_generator() {
                    project.timeline.add_layer(&layer_name, LayerType::Generator, binding.generator_type)
                } else {
                    project.timeline.add_layer(&layer_name, LayerType::Video, GeneratorType::None)
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

// ─── Helper functions ───

fn get_trigger_layer_name(trigger_type: PercussionTriggerType) -> String {
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
