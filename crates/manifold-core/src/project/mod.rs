use crate::PresetTypeId;
use crate::id::EffectId;
use crate::effect_graph_def::EffectGraphDef;
use crate::midi::MidiMappingConfig;
use crate::preset_def::PresetKind;
use crate::recording::RecordingProvenance;
use crate::session::SessionGrid;
use crate::settings::ProjectSettings;
use crate::tempo::TempoMap;
use crate::timeline::Timeline;
use crate::types::ClipDurationMode;
use crate::units::Beats;
use crate::video::VideoLibrary;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
#[cfg(test)]
mod test_support;
mod validate;
pub use validate::{LoadReport, PurgeResult};
mod queries;
mod presets;
mod load_migration;

/// A project-scoped preset (a "fork"): a complete, self-contained preset
/// (graph + exposed params + ranges, carried in [`EffectGraphDef`]) that lives
/// inside the project file rather than the global catalog. Created when the
/// user diverges a shared preset (Phase 4 fork ergonomics) and resolvable in
/// the same id namespace as stock/user presets via the catalog overlay. The
/// preset's id and display name live in `def.preset_metadata`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedPreset {
    /// Effect vs generator — directory-derived for disk presets, explicit here.
    pub kind: PresetKind,
    /// The complete preset definition (graph + `preset_metadata`).
    pub def: EffectGraphDef,
    /// `Saved` = user pressed "Save to Project" / "Make Unique" / import
    /// (PRESET_LIBRARY_DESIGN D4/D9) — deliberate, resolves ON TOP of stock/
    /// user disk tiers. `Snapshot` = auto-captured at save for
    /// self-containment (D5) — pruned + refreshed every save, resolves
    /// BELOW disk (disk wins over a stale snapshot; the snapshot is the
    /// fallback when the library file is gone). Defaults to `Saved` so
    /// legacy files with no `origin` field keep today's on-top behavior.
    #[serde(default)]
    pub origin: EmbeddedOrigin,
}

/// See [`EmbeddedPreset::origin`].
#[derive(Default, Serialize, Deserialize, PartialEq, Eq, Clone, Copy, Debug)]
pub enum EmbeddedOrigin {
    #[default]
    Saved,
    Snapshot,
}

impl EmbeddedPreset {
    /// The preset's stable id (from its metadata), or `None` if unset.
    pub fn id(&self) -> Option<&crate::PresetTypeId> {
        self.def.preset_metadata.as_ref().map(|m| &m.id)
    }
}

/// The schema version this build writes and is the newest it can open. Bumped
/// by every migration step that changes on-disk field shape; the migrate chain's
/// final target and the forward-compat guard both read it. Single source of truth.
pub const CURRENT_PROJECT_VERSION: &str = "1.12.0";

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
    /// Audio input routing + named sends for audio modulation. Parallel to
    /// `midi_config`. Skipped on serialize when empty so projects that never
    /// configured audio round-trip byte-identically. See
    /// `docs/AUDIO_MODULATION_DESIGN.md`.
    #[serde(default, skip_serializing_if = "crate::audio_setup::AudioSetup::is_empty")]
    pub audio_setup: crate::audio_setup::AudioSetup,
    #[serde(default)]
    pub settings: ProjectSettings,
    #[serde(default)]
    pub tempo_map: TempoMap,
    #[serde(default)]
    pub recording_provenance: RecordingProvenance,
    #[serde(skip)]
    pub last_saved_path: String,
    #[serde(default)]
    pub saved_playhead_time: f32,

    /// What the last load silently repaired (unknown effects stripped,
    /// overlapping clips removed, orphaned references purged, missing media
    /// files). Transient runtime state, recomputed every load — never
    /// serialized, exactly the `clip.layer_id` pattern (`BUG-063`,
    /// `docs/PROJECT_FILE_INTEGRITY_DESIGN.md` §3.6).
    #[serde(skip)]
    pub load_report: LoadReport,

    /// Project-scoped presets ("forks") — self-contained preset defs that live
    /// in this project rather than the global catalog. Resolved by id via the
    /// catalog overlay when the project loads. Empty for projects that have
    /// never forked a preset; skipped on serialize when empty so existing
    /// fixtures round-trip byte-identically.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedded_presets: Vec<EmbeddedPreset>,

    /// Session-mode grid: scenes (rows) x layer slots, launched live like
    /// Ableton session clips. Skipped on serialize when empty so projects
    /// that never touch session mode round-trip byte-identically. See
    /// `docs/SESSION_MODE_DESIGN.md`.
    #[serde(default, skip_serializing_if = "SessionGrid::is_empty")]
    pub session: SessionGrid,

    // ── Legacy top-level fields from V1.0.0 (before percussionImport nesting) ──
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "importedPercussionAudioPath"
    )]
    pub legacy_perc_audio_path: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "importedPercussionAudioStartBeat"
    )]
    pub legacy_perc_audio_start_beat: Option<f32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "importedPercussionClipPlacements"
    )]
    pub legacy_perc_clip_placements: Option<serde_json::Value>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "percussionEnergyEnvelope"
    )]
    pub legacy_perc_energy_envelope: Option<Vec<f32>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "importedStemPaths"
    )]
    pub legacy_imported_stem_paths: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "importedPercussionAudioHash"
    )]
    pub legacy_perc_audio_hash: Option<String>,
}

impl Project {
    pub fn layer_count(&self) -> usize {
        self.timeline.layers.len()
    }

    pub fn total_clip_count(&self) -> usize {
        self.timeline.total_clip_count()
    }

    /// Sync BPM from tempo map beat 0, clamped to 20-300.
    /// Port of C# ProjectSerializer.cs lines 39-43.
    pub fn sync_bpm_from_tempo_map(&mut self) {
        self.settings.bpm = self
            .tempo_map
            .get_bpm_at_beat(Beats::ZERO, self.settings.bpm);
    }

}
impl Default for Project {
    fn default() -> Self {
        Self {
            project_name: String::new(),
            project_version: CURRENT_PROJECT_VERSION.to_string(),
            timeline: Timeline::default(),
            video_library: VideoLibrary::default(),
            midi_config: MidiMappingConfig::default(),
            audio_setup: crate::audio_setup::AudioSetup::default(),
            settings: ProjectSettings::default(),
            tempo_map: TempoMap::default(),
            recording_provenance: RecordingProvenance::default(),
            last_saved_path: String::new(),
            saved_playhead_time: 0.0,
            load_report: LoadReport::default(),
            embedded_presets: Vec::new(),
            session: SessionGrid::default(),
            legacy_perc_audio_path: None,
            legacy_perc_audio_start_beat: None,
            legacy_perc_clip_placements: None,
            legacy_perc_energy_envelope: None,
            legacy_imported_stem_paths: None,
            legacy_perc_audio_hash: None,
        }
    }
}

fn default_version() -> String {
    "1.4.0".to_string()
}
