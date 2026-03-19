use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use crate::timeline::Timeline;
use crate::video::VideoLibrary;
use crate::midi::MidiMappingConfig;
use crate::settings::ProjectSettings;
use crate::tempo::TempoMap;
use crate::recording::RecordingProvenance;
use crate::percussion::PercussionImportState;
use crate::types::ClipDurationMode;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percussion_import: Option<PercussionImportState>,
    #[serde(skip)]
    pub last_saved_path: String,
    #[serde(default)]
    pub saved_playhead_time: f32,

    // ── Legacy top-level fields from V1.0.0 (before percussionImport nesting) ──
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "importedPercussionAudioPath")]
    pub legacy_perc_audio_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "importedPercussionAudioStartBeat")]
    pub legacy_perc_audio_start_beat: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "importedPercussionClipPlacements")]
    pub legacy_perc_clip_placements: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "percussionEnergyEnvelope")]
    pub legacy_perc_energy_envelope: Option<Vec<f32>>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "importedStemPaths")]
    pub legacy_imported_stem_paths: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "importedPercussionAudioHash")]
    pub legacy_perc_audio_hash: Option<String>,
}

impl Project {
    /// Post-deserialization initialization. Rebuild caches and run migrations.
    pub fn on_after_deserialize(&mut self) {
        // Rebuild runtime caches
        self.video_library.rebuild_lookup();
        self.midi_config.rebuild_dictionary();
        self.timeline.rebuild_clip_lookup();

        // Validate tempo map data
        self.tempo_map.ensure_valid();
        self.tempo_map.ensure_default_at_beat_zero(
            self.settings.bpm,
            crate::TempoPointSource::Manual,
        );

        // Sync BPM from tempo map at beat 0
        self.settings.bpm = self.tempo_map.get_bpm_at_beat(0.0, self.settings.bpm);

        // Clamp saved playhead
        self.saved_playhead_time = self.saved_playhead_time.max(0.0);

        // Align all effect params to current definitions
        self.align_all_effect_params();

        // Sync layer indices
        for (i, layer) in self.timeline.layers.iter_mut().enumerate() {
            layer.index = i as i32;
            for clip in &mut layer.clips {
                clip.layer_index = i as i32;
            }
        }
    }

    /// Resize all effect param arrays to match their definitions.
    fn align_all_effect_params(&mut self) {
        // Master effects
        for fx in &mut self.settings.master_effects {
            fx.align_to_definition();
        }
        // Layer effects + clip effects
        for layer in &mut self.timeline.layers {
            if let Some(ref mut effects) = layer.effects {
                for fx in effects.iter_mut() {
                    fx.align_to_definition();
                }
            }
            for clip in &mut layer.clips {
                for fx in &mut clip.effects {
                    fx.align_to_definition();
                }
            }
        }
    }

    pub fn layer_count(&self) -> usize {
        self.timeline.layers.len()
    }

    /// Port of Unity Project.ImportedPercussionClipPlacements property.
    /// Returns a mutable reference to the clip placements slice inside percussion_import.
    /// Initializes percussion_import if absent (matches Unity's lazy-init pattern).
    pub fn imported_percussion_clip_placements_mut(
        &mut self,
    ) -> &mut Vec<crate::percussion::ImportedPercussionClipPlacement> {
        if self.percussion_import.is_none() {
            self.percussion_import = Some(crate::percussion::PercussionImportState::default());
        }
        &mut self.percussion_import.as_mut().unwrap().clip_placements
    }

    /// Port of Unity Project.ImportedPercussionClipPlacements (read-only path).
    pub fn imported_percussion_clip_placements(
        &self,
    ) -> Option<&Vec<crate::percussion::ImportedPercussionClipPlacement>> {
        self.percussion_import.as_ref().map(|s| &s.clip_placements)
    }

    /// Port of Unity Project.ImportedPercussionAudioStartBeat getter.
    pub fn imported_percussion_audio_start_beat(&self) -> f32 {
        self.percussion_import
            .as_ref()
            .map_or(0.0, |s| s.audio_start_beat)
    }

    /// Port of Unity Project.ImportedPercussionAudioStartBeat setter (Mathf.Max(0f, value)).
    pub fn set_imported_percussion_audio_start_beat(&mut self, value: f32) {
        if self.percussion_import.is_none() {
            self.percussion_import = Some(crate::percussion::PercussionImportState::default());
        }
        self.percussion_import.as_mut().unwrap().audio_start_beat = value.max(0.0);
    }

    pub fn total_clip_count(&self) -> usize {
        self.timeline.total_clip_count()
    }

    /// Migrate old projects: force all layers to NoteOff duration mode.
    /// Port of C# ProjectSerializer.cs lines 45-50.
    pub fn migrate_duration_modes(&mut self) {
        for layer in &mut self.timeline.layers {
            if layer.duration_mode != Some(ClipDurationMode::NoteOff) {
                layer.duration_mode = Some(ClipDurationMode::NoteOff);
            }
        }
    }

    /// Sync BPM from tempo map beat 0, clamped to 20-300.
    /// Port of C# ProjectSerializer.cs lines 39-43.
    pub fn sync_bpm_from_tempo_map(&mut self) {
        let start_bpm = self.tempo_map.get_bpm_at_beat(0.0, self.settings.bpm);
        self.settings.bpm = start_bpm.clamp(20.0, 300.0);
    }

    /// Validate project structure. Returns list of error strings.
    /// Port of C# Project.Validate (lines 245-286).
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // Validate timeline clip references
        for layer in &self.timeline.layers {
            for clip in &layer.clips {
                if clip.is_generator() || clip.video_clip_id.is_empty() {
                    continue;
                }
                if !self.video_library.has_clip(&clip.video_clip_id) {
                    errors.push(format!(
                        "Timeline clip {} references missing video {}",
                        clip.id, clip.video_clip_id
                    ));
                }
            }
        }

        errors
    }

    /// Purge orphaned references: timeline clips pointing at missing library entries,
    /// stale MIDI mappings. Port of C# Project.PurgeOrphanedReferences (lines 305-358).
    pub fn purge_orphaned_references(&mut self) -> PurgeResult {
        let mut result = PurgeResult::default();

        // Build set of all valid video clip IDs in the library
        let valid_ids: HashSet<String> = self.video_library.clips
            .iter()
            .map(|c| c.id.clone())
            .collect();

        // Stage 1: Remove timeline clips referencing missing library entries
        for layer in &mut self.timeline.layers {
            let before = layer.clips.len();
            layer.clips.retain(|clip| {
                // Keep generators — they have no video reference
                if clip.is_generator() { return true; }
                if clip.video_clip_id.is_empty() { return true; }
                valid_ids.contains(&clip.video_clip_id)
            });
            result.timeline_clips_removed += before - layer.clips.len();
        }

        // Stage 2: Purge stale clip IDs from MIDI mappings
        result.midi_mappings_removed = self.midi_config.purge_orphaned_clip_ids(&valid_ids);

        // Stage 3: Rebuild clip lookup cache if anything changed
        if result.total_removed() > 0 {
            self.timeline.rebuild_clip_lookup();
        }

        result
    }
}

/// Result of purge_orphaned_references().
/// Port of C# Project.PurgeResult.
#[derive(Debug, Clone, Default)]
pub struct PurgeResult {
    pub timeline_clips_removed: usize,
    pub midi_mappings_removed: usize,
}

impl PurgeResult {
    pub fn total_removed(&self) -> usize {
        self.timeline_clips_removed + self.midi_mappings_removed
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
