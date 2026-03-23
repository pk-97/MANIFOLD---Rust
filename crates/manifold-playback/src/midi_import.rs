use manifold_core::clip::TimelineClip;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::GeneratorTypeId;
use manifold_core::LayerId;
use manifold_editing::command::{Command, CompositeCommand};
use manifold_editing::commands::clip::AddClipCommand;

use crate::midi_parser::MidiNote;

/// Result of a MIDI import operation.
/// Port of C# MidiImportService.cs MidiImportResult struct.
#[derive(Default)]
pub struct MidiImportResult {
    pub added_clips: i32,
    pub success: bool,
    pub undo_command: Option<Box<dyn Command>>,
}


/// Applies parsed MIDI notes to a timeline layer as clips.
/// Handles overlap trimming, clip type resolution, and undo recording.
/// Plain service — no MonoBehaviour dependencies.
/// Port of C# MidiImportService.cs.
pub struct MidiImportService;

impl MidiImportService {
    /// Import MIDI notes onto a single layer. Notes are trimmed to prevent overlap
    /// (later note-on truncates previous clip). All clips are created as a single
    /// undoable composite command.
    ///
    /// `notes` — Parsed MIDI notes (beat-domain).
    /// `target_layer_id` — Layer to place clips on (resolved to index internally).
    /// `start_beat_offset` — Beat position of the drop point (added to all note start beats).
    pub fn import_to_layer(
        notes: &[MidiNote],
        target_layer_id: &LayerId,
        start_beat_offset: f32,
        project: &mut Project,
    ) -> MidiImportResult {
        let mut result = MidiImportResult::default();

        if notes.is_empty() {
            return result;
        }

        // Resolve LayerId to positional index
        let target_layer_index = match project.timeline.find_layer_index_by_id(target_layer_id) {
            Some(idx) => idx,
            None => {
                log::warn!(
                    "[MidiImportService] Target layer '{}' not found, appending new layer.",
                    target_layer_id,
                );
                project.timeline.add_layer_default();
                project.timeline.layers.len() - 1
            }
        };

        // Build trimmed placement list
        let placements = build_trimmed_placements(notes, start_beat_offset);
        if placements.is_empty() {
            return result;
        }

        // Resolve clip creation strategy
        let is_generator = project.timeline.layers[target_layer_index].layer_type == LayerType::Generator;
        let gen_type = project.timeline.layers[target_layer_index].generator_type();
        let source_clip_ids_empty = project.timeline.layers[target_layer_index].source_clip_ids.is_empty();

        let (use_generator, resolved_gen_type) = if !is_generator && source_clip_ids_empty {
            // Video layer with no source clips — fall back to BasicShapesSnap generator
            log::warn!(
                "[MidiImportService] Target video layer has no source clips. \
                 Falling back to BasicShapesSnap generator clips."
            );
            (true, GeneratorTypeId::BASIC_SHAPES_SNAP)
        } else {
            (is_generator, gen_type.clone())
        };

        // Snapshot source_clip_ids to avoid borrow issues during iteration
        let source_clip_ids: Vec<String> = if !use_generator {
            project.timeline.layers[target_layer_index].source_clip_ids.clone()
        } else {
            Vec::new()
        };

        // Create clips
        let mut commands: Vec<Box<dyn Command>> = Vec::with_capacity(placements.len());
        let mut source_index: usize = 0;
        let target_layer_lid = project.timeline.layers[target_layer_index].layer_id.clone();

        for note in &placements {
            let clip: TimelineClip = if use_generator {
                TimelineClip::new_generator(
                    resolved_gen_type.clone(),
                    target_layer_lid.clone(),
                    note.start_beat,
                    note.duration_beats,
                )
            } else {
                // Round-robin through source clips
                let video_clip_id = source_clip_ids[source_index % source_clip_ids.len()].clone();
                source_index += 1;

                TimelineClip::new_video(
                    video_clip_id,
                    target_layer_lid.clone(),
                    note.start_beat,
                    note.duration_beats,
                    0.0,
                )
            };

            project.timeline.layers[target_layer_index].add_clip(clip.clone());
            commands.push(Box::new(AddClipCommand::new(clip, target_layer_lid.clone())));
            result.added_clips += 1;
        }

        if result.added_clips <= 0 {
            return result;
        }

        let command: Box<dyn Command> = if commands.len() == 1 {
            commands.remove(0)
        } else {
            Box::new(CompositeCommand::new(commands, "Import MIDI clips".to_string()))
        };

        result.undo_command = Some(command);
        result.success = true;

        log::info!(
            "[MidiImportService] Imported {} clip(s) onto layer '{}' from MIDI file.",
            result.added_clips,
            target_layer_id,
        );

        result
    }
}

/// Sort notes by start beat, apply offset, and trim overlaps so no two clips
/// occupy the same beat range. Later notes truncate earlier notes.
/// Port of C# MidiImportService.cs BuildTrimmedPlacements.
fn build_trimmed_placements(notes: &[MidiNote], start_beat_offset: f32) -> Vec<MidiNote> {
    // Copy and sort by start beat
    let mut sorted: Vec<MidiNote> = notes.to_vec();
    sorted.sort_by(compare_by_start_beat);

    // Apply offset
    for n in &mut sorted {
        n.start_beat += start_beat_offset;
    }

    // Trim overlaps: each note truncates the previous if they overlap
    let mut result: Vec<MidiNote> = Vec::with_capacity(sorted.len());

    for current in &sorted {
        let current = *current;

        if !result.is_empty() {
            let prev = result.last_mut().unwrap();
            if prev.end_beat() > current.start_beat {
                prev.duration_beats = current.start_beat - prev.start_beat;
                if prev.duration_beats <= 0.0 {
                    result.pop();
                }
            }
        }

        if current.duration_beats > 0.0 {
            result.push(current);
        }
    }

    result
}

fn compare_by_start_beat(a: &MidiNote, b: &MidiNote) -> std::cmp::Ordering {
    a.start_beat
        .partial_cmp(&b.start_beat)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.pitch.cmp(&b.pitch))
}
