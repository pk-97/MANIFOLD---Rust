use manifold_core::{Beats, ClipId};
use manifold_core::project::Project;
use manifold_editing::command::Command;

// ──────────────────────────────────────
// SetImportedAudioCommand
// ──────────────────────────────────────

/// Port of Unity SetImportedAudioCommand.
/// Undoable command for setting/clearing imported audio state on the project.
/// Captures path, startBeat, hash, and stemPaths snapshots.
/// Uses a callback to trigger async audio reload/reset — the command itself is synchronous.
#[derive(Debug)]
pub struct SetImportedAudioCommand {
    old_path: Option<String>,
    old_start_beat: f32,
    old_hash: Option<String>,
    old_stem_paths: Option<Vec<String>>,
    new_path: Option<String>,
    new_start_beat: f32,
    new_hash: Option<String>,
    new_stem_paths: Option<Vec<String>>,
    desc: String,
}

impl SetImportedAudioCommand {
    pub fn new(
        old_path: Option<String>,
        old_start_beat: f32,
        old_hash: Option<String>,
        old_stem_paths: Option<Vec<String>>,
        new_path: Option<String>,
        new_start_beat: f32,
        new_hash: Option<String>,
        new_stem_paths: Option<Vec<String>>,
        description: &str,
    ) -> Self {
        let desc = if description.trim().is_empty() {
            "Set imported audio".to_string()
        } else {
            description.to_string()
        };
        Self {
            old_path,
            old_start_beat,
            old_hash,
            old_stem_paths: old_stem_paths.as_deref().map(|s| s.to_vec()),
            new_path,
            new_start_beat,
            new_hash,
            new_stem_paths: new_stem_paths.as_deref().map(|s| s.to_vec()),
            desc,
        }
    }

    fn apply_state_to_project(
        project: &mut Project,
        path: Option<&str>,
        start_beat: f32,
        hash: Option<&str>,
        stem_paths: Option<&[String]>,
    ) {
        let state = project.percussion_import.get_or_insert_with(Default::default);
        state.audio_path = path.map(|s| s.to_string());
        state.audio_start_beat = start_beat;
        state.audio_hash = hash.map(|s| s.to_string());
        state.stem_paths = stem_paths.map(|s| s.to_vec());
    }
}

impl Command for SetImportedAudioCommand {
    fn execute(&mut self, project: &mut Project) {
        Self::apply_state_to_project(
            project,
            self.new_path.as_deref(),
            self.new_start_beat,
            self.new_hash.as_deref(),
            self.new_stem_paths.as_deref(),
        );
    }

    fn undo(&mut self, project: &mut Project) {
        Self::apply_state_to_project(
            project,
            self.old_path.as_deref(),
            self.old_start_beat,
            self.old_hash.as_deref(),
            self.old_stem_paths.as_deref(),
        );
    }

    fn description(&self) -> &str {
        &self.desc
    }
}

// ──────────────────────────────────────
// Inline command types for beat shifting
// (Unity uses MoveClipCommand + SetAudioStartBeatCommand from EditingService)
// ──────────────────────────────────────

/// Port of Unity SetAudioStartBeatCommand.
/// Applies the new audio start beat to project.percussion_import.
#[derive(Debug)]
pub struct SetAudioStartBeatCommand {
    old_start_beat: f32,
    new_start_beat: f32,
    desc: String,
}

impl SetAudioStartBeatCommand {
    pub fn new(old_start_beat: f32, new_start_beat: f32, description: &str) -> Self {
        Self {
            old_start_beat,
            new_start_beat,
            desc: description.to_string(),
        }
    }
}

impl Command for SetAudioStartBeatCommand {
    fn execute(&mut self, project: &mut Project) {
        let state = project.percussion_import.get_or_insert_with(Default::default);
        state.audio_start_beat = self.new_start_beat;
    }

    fn undo(&mut self, project: &mut Project) {
        let state = project.percussion_import.get_or_insert_with(Default::default);
        state.audio_start_beat = self.old_start_beat;
    }

    fn description(&self) -> &str {
        &self.desc
    }
}

/// Lightweight command to move a clip's start beat in place.
/// Port of the move-beat portion of Unity MoveClipCommand.
#[derive(Debug)]
pub struct MoveClipBeatCommand {
    clip_id: ClipId,
    layer_index: i32,
    old_start_beat: Beats,
    new_start_beat: Beats,
}

impl MoveClipBeatCommand {
    pub fn new(clip_id: ClipId, layer_index: i32, old_start_beat: Beats, new_start_beat: Beats) -> Self {
        Self {
            clip_id,
            layer_index,
            old_start_beat,
            new_start_beat,
        }
    }

    fn apply(project: &mut Project, clip_id: &str, layer_index: i32, start_beat: Beats) {
        if let Some(layer) = project.timeline.layers.get_mut(layer_index as usize)
            && let Some(clip) = layer.clips.iter_mut().find(|c| c.id == clip_id) {
                clip.start_beat = start_beat;
            }
    }
}

impl Command for MoveClipBeatCommand {
    fn execute(&mut self, project: &mut Project) {
        Self::apply(project, &self.clip_id, self.layer_index, self.new_start_beat);
    }

    fn undo(&mut self, project: &mut Project) {
        Self::apply(project, &self.clip_id, self.layer_index, self.old_start_beat);
    }

    fn description(&self) -> &str {
        "Move clip beat"
    }
}
