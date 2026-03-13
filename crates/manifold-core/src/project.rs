use serde::{Deserialize, Serialize};
use crate::timeline::Timeline;
use crate::video::VideoLibrary;
use crate::midi::MidiMappingConfig;
use crate::settings::ProjectSettings;
use crate::tempo::TempoMap;
use crate::recording::RecordingProvenance;
use crate::percussion::PercussionImportState;

/// Root project aggregate. Contains all project data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    #[serde(default)]
    pub project_name: String,
    #[serde(default = "default_version")]
    pub project_version: String,
    #[serde(default)]
    pub timeline: Timeline,
    #[serde(default)]
    pub video_library: VideoLibrary,
    #[serde(default, rename = "midiConfig")]
    pub midi_config: MidiMappingConfig,
    #[serde(default)]
    pub settings: ProjectSettings,
    #[serde(default)]
    pub tempo_map: TempoMap,
    #[serde(default)]
    pub recording_provenance: RecordingProvenance,
    #[serde(default)]
    pub percussion_import: Option<PercussionImportState>,
    #[serde(default)]
    pub last_saved_path: String,
    #[serde(default)]
    pub saved_playhead_time: f32,

    // ── Legacy top-level fields from V1.0.0 (before percussionImport nesting) ──
    #[serde(default, rename = "importedPercussionAudioPath")]
    pub legacy_perc_audio_path: Option<String>,
    #[serde(default, rename = "importedPercussionAudioStartBeat")]
    pub legacy_perc_audio_start_beat: Option<f32>,
    #[serde(default, rename = "importedPercussionClipPlacements")]
    pub legacy_perc_clip_placements: Option<serde_json::Value>,
    #[serde(default, rename = "percussionEnergyEnvelope")]
    pub legacy_perc_energy_envelope: Option<Vec<f32>>,
    #[serde(default, rename = "importedStemPaths")]
    pub legacy_imported_stem_paths: Option<Vec<String>>,
    #[serde(default, rename = "importedPercussionAudioHash")]
    pub legacy_perc_audio_hash: Option<String>,
}

impl Project {
    /// Post-deserialization initialization. Rebuild caches and run migrations.
    pub fn on_after_deserialize(&mut self) {
        // Rebuild runtime caches
        self.video_library.rebuild_lookup();
        self.midi_config.rebuild_dictionary();
        self.timeline.rebuild_clip_lookup();
        self.tempo_map.ensure_default_at_beat_zero(
            self.settings.bpm,
            crate::TempoPointSource::Unknown,
        );

        // Sync layer indices
        for (i, layer) in self.timeline.layers.iter_mut().enumerate() {
            layer.index = i as i32;
            for clip in &mut layer.clips {
                clip.layer_index = i as i32;
            }
        }
    }

    pub fn layer_count(&self) -> usize {
        self.timeline.layers.len()
    }

    pub fn total_clip_count(&self) -> usize {
        self.timeline.total_clip_count()
    }
}

impl Default for Project {
    fn default() -> Self {
        Self {
            project_name: String::new(),
            project_version: "1.1.0".to_string(),
            timeline: Timeline::default(),
            video_library: VideoLibrary::default(),
            midi_config: MidiMappingConfig::default(),
            settings: ProjectSettings::default(),
            tempo_map: TempoMap::default(),
            recording_provenance: RecordingProvenance::default(),
            percussion_import: None,
            last_saved_path: String::new(),
            saved_playhead_time: 0.0,
            legacy_perc_audio_path: None,
            legacy_perc_audio_start_beat: None,
            legacy_perc_clip_placements: None,
            legacy_perc_energy_envelope: None,
            legacy_imported_stem_paths: None,
            legacy_perc_audio_hash: None,
        }
    }
}

fn default_version() -> String { "1.1.0".to_string() }
