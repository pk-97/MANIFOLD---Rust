use crate::command::Command;
use manifold_core::ClipId;
use manifold_core::audio_clip_detection::{AudioClipDetection, DetectionConfig};
use manifold_core::change::{ChangeSet, Part};
use manifold_core::project::Project;

/// Set an audio clip's detection config (the inspector's knobs). Preserves any
/// cached analysis on the clip — only the settings change, so the host can
/// re-plan from the existing events without re-running the backend.
#[derive(Debug)]
pub struct SetClipDetectionConfigCommand {
    clip_id: ClipId,
    new_config: DetectionConfig,
    /// The previous config, captured on first execute for undo. `None` means the
    /// clip had no detection state at all (undo removes it again).
    old_config: Option<DetectionConfig>,
    had_detection: bool,
    captured: bool,
}

impl SetClipDetectionConfigCommand {
    pub fn new(clip_id: ClipId, new_config: DetectionConfig) -> Self {
        Self {
            clip_id,
            new_config,
            old_config: None,
            had_detection: false,
            captured: false,
        }
    }
}

impl Command for SetClipDetectionConfigCommand {
    fn touches(&self) -> ChangeSet {
        ChangeSet::of(&[Part::Clips])
    }
    fn do_execute(&mut self, project: &mut Project) {
        let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) else {
            return;
        };

        // Capture the prior state once, for undo.
        if !self.captured {
            self.had_detection = clip.detection.is_some();
            self.old_config = clip.detection.as_ref().map(|d| d.config.clone());
            self.captured = true;
        }

        match clip.detection.as_mut() {
            Some(detection) => detection.config = self.new_config.clone(),
            None => {
                clip.detection = Some(AudioClipDetection {
                    config: self.new_config.clone(),
                    analysis: None,
                    ..Default::default()
                });
            }
        }
    }

    fn do_undo(&mut self, project: &mut Project) {
        let Some(clip) = project.timeline.find_clip_by_id_mut(&self.clip_id) else {
            return;
        };

        if !self.had_detection {
            // The clip had no detection before; restore that (keep no cached analysis).
            clip.detection = None;
            return;
        }

        if let Some(old) = self.old_config.clone()
            && let Some(detection) = clip.detection.as_mut()
        {
            detection.config = old;
        }
    }

    fn description(&self) -> &str {
        "Set clip detection config"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::PresetTypeId;
    use manifold_core::clip::TimelineClip;
    use manifold_core::layer::Layer;
    use manifold_core::types::LayerType;
    use manifold_core::units::{Beats, Seconds};

    fn project_with_audio_clip() -> (Project, ClipId) {
        let mut project = Project::default();
        let idx = project
            .timeline
            .add_layer("Audio", LayerType::Audio, PresetTypeId::NONE);
        let clip = TimelineClip::new_audio(
            "track.wav".to_string(),
            Beats(0.0),
            Beats(8.0),
            Seconds(0.0),
            Seconds(120.0),
        );
        let clip_id = clip.id.clone();
        let layer: &mut Layer = &mut project.timeline.layers[idx];
        layer.restore_clip(clip);
        project.timeline.mark_clip_lookup_dirty();
        (project, clip_id)
    }

    #[test]
    fn execute_sets_config_and_undo_clears_when_absent() {
        let (mut project, clip_id) = project_with_audio_clip();
        let mut cmd =
            SetClipDetectionConfigCommand::new(clip_id.clone(), DetectionConfig::default());

        cmd.execute(&mut project);
        let clip = project.timeline.find_clip_by_id_mut(&clip_id).unwrap();
        assert!(clip.detection.is_some());

        cmd.undo(&mut project);
        let clip = project.timeline.find_clip_by_id_mut(&clip_id).unwrap();
        assert!(clip.detection.is_none());
    }

    #[test]
    fn undo_restores_previous_config_and_keeps_analysis() {
        let (mut project, clip_id) = project_with_audio_clip();

        // Seed an existing detection with a marker config + a cached analysis.
        {
            let clip = project.timeline.find_clip_by_id_mut(&clip_id).unwrap();
            let cfg = DetectionConfig {
                quantize_on: false,
                ..Default::default()
            };
            clip.detection = Some(AudioClipDetection {
                config: cfg,
                analysis: Some(
                    manifold_core::percussion_analysis::PercussionAnalysisData::new(vec![]),
                ),
                ..Default::default()
            });
        }

        let new_cfg = DetectionConfig {
            quantize_on: true,
            ..Default::default()
        };
        let mut cmd = SetClipDetectionConfigCommand::new(clip_id.clone(), new_cfg);

        cmd.execute(&mut project);
        let clip = project.timeline.find_clip_by_id_mut(&clip_id).unwrap();
        assert!(clip.detection.as_ref().unwrap().config.quantize_on);
        // Cached analysis survives a config change.
        assert!(clip.detection.as_ref().unwrap().has_analysis());

        cmd.undo(&mut project);
        let clip = project.timeline.find_clip_by_id_mut(&clip_id).unwrap();
        assert!(!clip.detection.as_ref().unwrap().config.quantize_on);
        assert!(clip.detection.as_ref().unwrap().has_analysis());
    }
}
