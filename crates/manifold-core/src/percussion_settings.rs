//! The trigger templates Detect places: per instrument, the layer slot, the preset and the clip length.

use crate::percussion_analysis::{
    ClipDetectionAnchor, PercussionClipBinding, PercussionImportOptions, PercussionTriggerType,
};
use crate::preset_type_id::PresetTypeId;
use crate::project::Project;
use crate::settings::ProjectSettings;
use crate::types::QuantizeMode;
use crate::units::{Beats, Seconds};

pub struct PercussionImportOptionsFactory;

impl PercussionImportOptionsFactory {
    /// Detect's defaults for `project`, placing clips through the detected clip's `clip_anchor`.
    pub fn create_default(
        project: &Project,
        clip_anchor: ClipDetectionAnchor,
    ) -> PercussionImportOptions {
        PercussionImportOptions {
            quantize_to_grid: true,
            quantize_step_beats: resolve_default_quantize_step(&project.settings),
            default_clip_duration_beats: Beats(0.75),
            onset_compensation_seconds: Seconds(0.010),
            bindings: vec![
                // Kick — punchy geometric wireframes, top of stack.
                PercussionClipBinding::new(
                    PercussionTriggerType::Kick,
                    0,
                    PresetTypeId::WIREFRAME_ZOO,
                    Beats(0.5),
                    0.0,
                ),
                // Snare — snapping shapes.
                PercussionClipBinding::new(
                    PercussionTriggerType::Snare,
                    1,
                    PresetTypeId::BASIC_SHAPES,
                    Beats(0.75),
                    0.0,
                ),
            ],
            clip_anchor,
        }
    }
}

/// The default quantize step from the project's quantize mode.
fn resolve_default_quantize_step(settings: &ProjectSettings) -> Beats {
    match settings.quantize_mode {
        QuantizeMode::Off | QuantizeMode::QuarterBeat => Beats(0.25),
        QuantizeMode::Beat => Beats(1.0),
        QuantizeMode::Bar => Beats((settings.time_signature_numerator as f64).max(1.0)),
    }
}
