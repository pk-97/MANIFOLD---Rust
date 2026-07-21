//! Project validation and orphaned-reference purge; the load-repair report.

use super::*;

impl Project {
    /// Validate project structure. Returns list of error strings.
    /// Port of C# Project.Validate (lines 245-286).
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        // Validate timeline clip references
        for layer in &self.timeline.layers {
            for clip in &layer.clips {
                if layer.layer_type == crate::types::LayerType::Generator
                    || clip.video_clip_id.is_empty()
                {
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
        let valid_ids: HashSet<String> = self
            .video_library
            .clips
            .iter()
            .map(|c| c.id.clone())
            .collect();

        // Stage 1: Remove timeline clips referencing missing library entries
        for layer in &mut self.timeline.layers {
            let before = layer.clips.len();
            let is_gen_layer = layer.layer_type == crate::types::LayerType::Generator;
            layer.clips.retain(|clip| {
                // Keep generator layer clips — they have no video reference
                if is_gen_layer {
                    return true;
                }
                if clip.video_clip_id.is_empty() {
                    return true;
                }
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

/// What the last `load_project` call silently repaired. Written by both
/// post-load call sites (`strip_unknown_effects` inside
/// `load_project_from_json_with`; `run_post_load_validation`'s overlap
/// repair, orphan purge, and missing-file detection) into
/// `Project::load_report` — the single owner of "what this load altered"
/// (`BUG-063`, `docs/PROJECT_FILE_INTEGRITY_DESIGN.md` §3.6).
#[derive(Debug, Clone, Default)]
pub struct LoadReport {
    pub unknown_effects_removed: usize,
    pub overlapping_clips_repaired: usize,
    pub orphaned_clips_purged: usize,
    pub orphaned_midi_purged: usize,
    pub missing_media_files: Vec<String>,
    /// BUG-079: preset instances whose def couldn't be resolved at load
    /// (deleted/unregistered/missing) — kept on a placeholder spec
    /// (keep-don't-drop) rather than dropped. Was console-only (`eprintln!`
    /// in `effects.rs` / `preset_runtime.rs`) before this field existed.
    pub unresolved_preset_templates: usize,
}

impl LoadReport {
    pub fn is_empty(&self) -> bool {
        self.unknown_effects_removed == 0
            && self.overlapping_clips_repaired == 0
            && self.orphaned_clips_purged == 0
            && self.orphaned_midi_purged == 0
            && self.missing_media_files.is_empty()
            && self.unresolved_preset_templates == 0
    }

    /// One human line per non-zero entry, e.g. "3 unknown effects removed".
    /// Singular/plural is correct for count == 1.
    pub fn human_lines(&self) -> Vec<String> {
        fn plural(n: usize, singular: &str, plural: &str) -> String {
            if n == 1 {
                singular.to_string()
            } else {
                plural.to_string()
            }
        }

        let mut lines = Vec::new();
        if self.unknown_effects_removed > 0 {
            lines.push(format!(
                "{} unknown {} removed",
                self.unknown_effects_removed,
                plural(self.unknown_effects_removed, "effect", "effects")
            ));
        }
        if self.overlapping_clips_repaired > 0 {
            lines.push(format!(
                "{} overlapping {} repaired",
                self.overlapping_clips_repaired,
                plural(self.overlapping_clips_repaired, "clip", "clips")
            ));
        }
        if self.orphaned_clips_purged > 0 {
            lines.push(format!(
                "{} orphaned {} purged",
                self.orphaned_clips_purged,
                plural(self.orphaned_clips_purged, "clip", "clips")
            ));
        }
        if self.orphaned_midi_purged > 0 {
            lines.push(format!(
                "{} orphaned MIDI {} purged",
                self.orphaned_midi_purged,
                plural(self.orphaned_midi_purged, "mapping", "mappings")
            ));
        }
        if !self.missing_media_files.is_empty() {
            lines.push(format!(
                "{} missing media {}",
                self.missing_media_files.len(),
                plural(self.missing_media_files.len(), "file", "files")
            ));
        }
        if self.unresolved_preset_templates > 0 {
            lines.push(format!(
                "{} unresolved preset {} kept with saved values (preset not registered)",
                self.unresolved_preset_templates,
                plural(self.unresolved_preset_templates, "reference", "references")
            ));
        }
        lines
    }
}
