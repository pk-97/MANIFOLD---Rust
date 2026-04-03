use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::selection::SelectionRegion;
use manifold_core::types::*;
use manifold_core::units::Bpm;
use manifold_core::{Beats, ClipId};
use manifold_editing::command::Command;
use manifold_editing::service::EditingService;

fn make_project() -> Project {
    let mut project = Project::default();
    project.settings.bpm = Bpm(120.0);
    project.settings.time_signature_numerator = 4;
    project
        .timeline
        .insert_layer(0, Layer::new("Video 1".into(), LayerType::Video, 0));
    project
        .timeline
        .insert_layer(1, Layer::new("Video 2".into(), LayerType::Video, 1));
    project.timeline.rebuild_clip_lookup();
    project
}

/// Build a SelectionRegion from layer index range + project layers.
fn make_region(
    project: &Project,
    start_beat: f32,
    end_beat: f32,
    start_layer: usize,
    end_layer: usize,
) -> SelectionRegion {
    use std::collections::HashSet;
    let layers = &project.timeline.layers;
    let lo = start_layer.min(end_layer);
    let hi = start_layer
        .max(end_layer)
        .min(layers.len().saturating_sub(1));
    let mut selected = HashSet::new();
    for layer in layers.iter().skip(lo).take(hi - lo + 1) {
        selected.insert(layer.layer_id.clone());
    }
    SelectionRegion {
        start_beat: Beats::from_f32(start_beat),
        end_beat: Beats::from_f32(end_beat),
        is_active: true,
        start_layer_id: layers.get(lo).map(|l| l.layer_id.clone()),
        end_layer_id: layers.get(hi).map(|l| l.layer_id.clone()),
        selected_layer_ids: selected,
    }
}

fn add_clip(project: &mut Project, layer: usize, start: f32, dur: f32) -> ClipId {
    let clip = TimelineClip {
        video_clip_id: "test_video".into(),
        start_beat: Beats::from_f32(start),
        duration_beats: Beats::from_f32(dur),
        ..Default::default()
    };
    let id = clip.id.clone();
    project.timeline.layers[layer].restore_clip(clip);
    project.timeline.mark_clip_lookup_dirty();
    id
}

// ─── Overlap enforcement ───

#[test]
fn overlap_covers_both_deletes() {
    let mut project = make_project();
    let existing_id = add_clip(&mut project, 0, 2.0, 2.0); // [2..4]

    let layer_id = project.timeline.layers[0].layer_id.clone();
    let placed = TimelineClip {
        start_beat: Beats(1.0),
        duration_beats: Beats(5.0), // [1..6] covers [2..4]
        layer_id: layer_id.clone(),
        ..Default::default()
    };

    let spb = 60.0 / project.settings.bpm.0;
    let cmds = EditingService::enforce_non_overlap(&project, &placed, 0, &Default::default(), spb);
    assert_eq!(cmds.len(), 1);

    // Execute the delete command
    let mut service = EditingService::new();
    service.execute_batch(cmds, "overlap".into(), &mut project);
    assert!(project.timeline.find_clip_by_id(&existing_id).is_none());
}

#[test]
fn overlap_covers_start_trims() {
    let mut project = make_project();
    let existing_id = add_clip(&mut project, 0, 2.0, 4.0); // [2..6]

    let layer_id = project.timeline.layers[0].layer_id.clone();
    let placed = TimelineClip {
        start_beat: Beats(1.0),
        duration_beats: Beats(3.0), // [1..4] covers start of [2..6]
        layer_id: layer_id.clone(),
        ..Default::default()
    };

    let spb = 60.0 / project.settings.bpm.0;
    let cmds = EditingService::enforce_non_overlap(&project, &placed, 0, &Default::default(), spb);
    assert_eq!(cmds.len(), 1);

    let mut service = EditingService::new();
    service.execute_batch(cmds, "overlap".into(), &mut project);

    let clip = project.timeline.find_clip_by_id(&existing_id).unwrap();
    assert!((clip.start_beat - Beats(4.0)).abs() < Beats(0.001)); // trimmed to start at placed_end
    assert!((clip.end_beat() - Beats(6.0)).abs() < Beats(0.001));
}

#[test]
fn overlap_covers_end_trims() {
    let mut project = make_project();
    let existing_id = add_clip(&mut project, 0, 2.0, 4.0); // [2..6]

    let layer_id = project.timeline.layers[0].layer_id.clone();
    let placed = TimelineClip {
        start_beat: Beats(4.0),
        duration_beats: Beats(4.0), // [4..8] covers end of [2..6]
        layer_id: layer_id.clone(),
        ..Default::default()
    };

    let spb = 60.0 / project.settings.bpm.0;
    let cmds = EditingService::enforce_non_overlap(&project, &placed, 0, &Default::default(), spb);
    assert_eq!(cmds.len(), 1);

    let mut service = EditingService::new();
    service.execute_batch(cmds, "overlap".into(), &mut project);

    let clip = project.timeline.find_clip_by_id(&existing_id).unwrap();
    assert!((clip.start_beat - Beats(2.0)).abs() < Beats(0.001));
    assert!((clip.duration_beats - Beats(2.0)).abs() < Beats(0.001)); // trimmed to end at placed_start
}

#[test]
fn overlap_splits_middle() {
    let mut project = make_project();
    let existing_id = add_clip(&mut project, 0, 0.0, 8.0); // [0..8]

    let layer_id = project.timeline.layers[0].layer_id.clone();
    let placed = TimelineClip {
        start_beat: Beats(3.0),
        duration_beats: Beats(2.0), // [3..5] in middle of [0..8]
        layer_id: layer_id.clone(),
        ..Default::default()
    };

    let spb = 60.0 / project.settings.bpm.0;
    let cmds = EditingService::enforce_non_overlap(&project, &placed, 0, &Default::default(), spb);
    assert_eq!(cmds.len(), 2); // trim + add tail

    let mut service = EditingService::new();
    service.execute_batch(cmds, "overlap".into(), &mut project);

    // Original clip trimmed to [0..3]
    let clip = project.timeline.find_clip_by_id(&existing_id).unwrap();
    assert!((clip.start_beat - Beats(0.0)).abs() < Beats(0.001));
    assert!((clip.duration_beats - Beats(3.0)).abs() < Beats(0.001));

    // Tail clip added at [5..8]
    assert_eq!(project.timeline.layers[0].clips.len(), 2);
    let tail = project.timeline.layers[0]
        .clips
        .iter()
        .find(|c| c.id != existing_id)
        .expect("tail clip should exist");
    assert!((tail.start_beat - Beats(5.0)).abs() < Beats(0.001));
    assert!((tail.duration_beats - Beats(3.0)).abs() < Beats(0.001));
}

// ─── Clipboard ───

#[test]
fn copy_paste_roundtrip() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 4.0);
    let id2 = add_clip(&mut project, 0, 4.0, 4.0);

    let mut service = EditingService::new();
    service.copy_clips(&project, &[id1.clone(), id2.clone()], None, 0.5);
    assert!(service.has_clipboard());

    let result = service.paste_clips(&mut project, Beats(10.0), 0, 0.5);
    assert_eq!(result.pasted_clip_ids.len(), 2);

    // Execute all paste commands
    for mut cmd in result.commands {
        cmd.execute(&mut project);
    }
    project.timeline.rebuild_clip_lookup();

    // Pasted clips should have new IDs
    assert!(
        result
            .pasted_clip_ids
            .iter()
            .all(|id| id != &id1 && id != &id2)
    );

    // Pasted clips should exist
    for id in &result.pasted_clip_ids {
        assert!(project.timeline.find_clip_by_id(id).is_some());
    }
}

#[test]
fn paste_preserves_relative_offsets() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 2.0);
    let id2 = add_clip(&mut project, 0, 4.0, 2.0);
    let id3 = add_clip(&mut project, 1, 2.0, 2.0);

    let mut service = EditingService::new();
    service.copy_clips(&project, &[id1, id2, id3], None, 0.5);

    let result = service.paste_clips(&mut project, Beats(10.0), 0, 0.5);
    for mut cmd in result.commands {
        cmd.execute(&mut project);
    }
    project.timeline.rebuild_clip_lookup();

    // Collect pasted clip beats
    let mut pasted: Vec<(f32, usize)> = result
        .pasted_clip_ids
        .iter()
        .map(|id| {
            let mut beat = 0.0f32;
            let mut layer_idx = 0usize;
            for (li, layer) in project.timeline.layers.iter().enumerate() {
                if let Some(c) = layer.clips.iter().find(|c| &c.id == id) {
                    beat = c.start_beat.as_f32();
                    layer_idx = li;
                    break;
                }
            }
            (beat, layer_idx)
        })
        .collect();
    pasted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    // Offsets: clip1 was at beat 0 layer 0, clip3 at beat 2 layer 1, clip2 at beat 4 layer 0
    assert!((pasted[0].0 - 10.0).abs() < 0.001); // first at target
    assert_eq!(pasted[0].1, 0);
    assert!((pasted[1].0 - 12.0).abs() < 0.001); // +2 offset
    assert_eq!(pasted[1].1, 1);
    assert!((pasted[2].0 - 14.0).abs() < 0.001); // +4 offset
    assert_eq!(pasted[2].1, 0);
}

// ─── Duplicate ───

#[test]
fn duplicate_region_shifts_forward() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 4.0);

    let region = make_region(&project, 0.0, 4.0, 0, 0);

    let cmds = EditingService::duplicate_clips(&project, &[id1.clone()], &region, 0.5);
    assert_eq!(cmds.len(), 1);

    let mut service = EditingService::new();
    service.execute_batch(cmds, "dup".into(), &mut project);
    project.timeline.rebuild_clip_lookup();

    assert_eq!(project.timeline.layers[0].clips.len(), 2);
    let dup = project.timeline.layers[0]
        .clips
        .iter()
        .find(|c| c.id != id1)
        .unwrap();
    assert!((dup.start_beat - Beats(4.0)).abs() < Beats(0.001)); // shifted by region duration (4)
}

// ─── Delete ───

#[test]
fn delete_clips_removes() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 4.0);
    let _id2 = add_clip(&mut project, 0, 4.0, 4.0);

    let cmds = EditingService::delete_clips(&project, &[id1.clone()], None, 0.5);
    assert_eq!(cmds.len(), 1);

    let mut service = EditingService::new();
    service.execute_batch(cmds, "del".into(), &mut project);

    assert_eq!(project.timeline.layers[0].clips.len(), 1);
    assert!(project.timeline.find_clip_by_id(&id1).is_none());
}

// ─── Create clip ───

#[test]
fn create_clip_at_position() {
    let mut project = make_project();
    let initial = project.timeline.layers[0].clips.len();

    let (mut cmd, _clip_id) =
        EditingService::create_clip_at_position(&mut project, Beats(2.0), 0, Beats(4.0), 0.5)
            .expect("should create clip on non-group layer");
    cmd.execute(&mut project);
    project.timeline.rebuild_clip_lookup();

    assert_eq!(project.timeline.layers[0].clips.len(), initial + 1);
    let clip = &project.timeline.layers[0].clips[initial];
    assert!((clip.start_beat - Beats(2.0)).abs() < Beats(0.001));
    assert!((clip.duration_beats - Beats(4.0)).abs() < Beats(0.001));
}

// ─── Nudge ───

#[test]
fn nudge_selected_clips() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 2.0, 4.0);

    let cmds = EditingService::nudge_clips(&project, &[id1.clone()], Beats(1.0), 0.5);
    assert_eq!(cmds.len(), 1);

    let mut service = EditingService::new();
    service.execute_batch(cmds, "nudge".into(), &mut project);

    let clip = project.timeline.find_clip_by_id(&id1).unwrap();
    assert!((clip.start_beat - Beats(3.0)).abs() < Beats(0.001));
}

// ─── Undo/Redo ───

#[test]
fn multi_step_undo_redo() {
    let mut project = make_project();
    let mut service = EditingService::new();

    // Execute 5 operations
    for i in 0..5 {
        let (cmd, _) = EditingService::create_clip_at_position(
            &mut project,
            Beats(i as f64 * 4.0),
            0,
            Beats(4.0),
            0.5,
        )
        .unwrap();
        service.execute(cmd, &mut project);
    }
    assert_eq!(project.timeline.layers[0].clips.len(), 5);

    // Undo all
    for _ in 0..5 {
        assert!(service.undo(&mut project));
    }
    assert_eq!(project.timeline.layers[0].clips.len(), 0);
    assert!(!service.can_undo());

    // Redo all
    for _ in 0..5 {
        assert!(service.redo(&mut project));
    }
    assert_eq!(project.timeline.layers[0].clips.len(), 5);
    assert!(!service.can_redo());
}

#[test]
fn data_version_increments() {
    let mut project = make_project();
    let mut service = EditingService::new();
    assert_eq!(service.data_version(), 0);

    let (cmd, _) = EditingService::create_clip_at_position(&mut project, Beats(0.0), 0, Beats(4.0), 0.5).unwrap();
    service.execute(cmd, &mut project);
    assert_eq!(service.data_version(), 1);

    let _ = service.undo(&mut project);
    assert_eq!(service.data_version(), 2);

    let _ = service.redo(&mut project);
    assert_eq!(service.data_version(), 3);
}

#[test]
fn dirty_flag_tracks_saves() {
    let mut project = make_project();
    let mut service = EditingService::new();

    assert!(!service.is_dirty());

    let (cmd, _) = EditingService::create_clip_at_position(&mut project, Beats(0.0), 0, Beats(4.0), 0.5).unwrap();
    service.execute(cmd, &mut project);
    assert!(service.is_dirty());

    service.mark_clean();
    assert!(!service.is_dirty());

    let _ = service.undo(&mut project);
    assert!(service.is_dirty());
}

// ─── Split ───

#[test]
fn split_at_beat() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 8.0);

    let spb = 60.0 / project.settings.bpm.0;
    let cmd = EditingService::split_clip_at_beat(&project, &id1, Beats(4.0), spb);
    assert!(cmd.is_some());

    let mut cmd = cmd.unwrap();
    cmd.execute(&mut project);
    project.timeline.rebuild_clip_lookup();

    let original = project.timeline.find_clip_by_id(&id1).unwrap();
    assert!((original.duration_beats - Beats(4.0)).abs() < Beats(0.001));

    assert_eq!(project.timeline.layers[0].clips.len(), 2);
    let tail = project.timeline.layers[0]
        .clips
        .iter()
        .find(|c| c.id != id1)
        .unwrap();
    assert!((tail.start_beat - Beats(4.0)).abs() < Beats(0.001));
    assert!((tail.duration_beats - Beats(4.0)).abs() < Beats(0.001));
}

#[test]
fn split_at_boundary_returns_none() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 8.0);

    let spb = 60.0 / project.settings.bpm.0;
    // Split at start — invalid
    assert!(EditingService::split_clip_at_beat(&project, &id1, Beats(0.0), spb).is_none());
    // Split at end — invalid
    assert!(EditingService::split_clip_at_beat(&project, &id1, Beats(8.0), spb).is_none());
}

// ─── Extend/Shrink ───

#[test]
fn extend_shrink_by_grid() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 4.0);

    // Extend
    let cmds = EditingService::extend_clips_by_grid(&project, &[id1.clone()], Beats(1.0));
    let mut service = EditingService::new();
    service.execute_batch(cmds, "ext".into(), &mut project);
    let clip = project.timeline.find_clip_by_id(&id1).unwrap();
    assert!((clip.duration_beats - Beats(5.0)).abs() < Beats(0.001));

    // Shrink
    let cmds = EditingService::shrink_clips_by_grid(&project, &[id1.clone()], Beats(1.0));
    service.execute_batch(cmds, "shrink".into(), &mut project);
    let clip = project.timeline.find_clip_by_id(&id1).unwrap();
    assert!((clip.duration_beats - Beats(4.0)).abs() < Beats(0.001));
}

// ─── Move clip to layer ───

#[test]
fn move_clip_to_layer() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 4.0);

    let cmd = EditingService::move_clip_to_layer(&project, &id1, 1);
    assert!(cmd.is_some());

    let mut service = EditingService::new();
    service.execute(cmd.unwrap(), &mut project);

    // Verify the clip is now on layer 1
    assert!(project.timeline.layers[1].clips.iter().any(|c| c.id == id1));
}

// ─── Selection region ───

#[test]
fn get_clips_in_region() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 4.0);
    let _id2 = add_clip(&mut project, 0, 8.0, 4.0); // outside region
    let id3 = add_clip(&mut project, 1, 2.0, 4.0);

    let region = make_region(&project, 1.0, 5.0, 0, 1);

    let results = EditingService::get_clips_in_region(&project, &region);
    let result_ids: Vec<&ClipId> = results.iter().map(|(_, id)| id).collect();
    assert!(result_ids.contains(&&id1));
    assert!(result_ids.contains(&&id3));
    assert_eq!(results.len(), 2);
}

// ─── Trim clip to region ───

#[test]
fn trim_clip_to_region_fully_inside() {
    let mut project = make_project();
    let _id = add_clip(&mut project, 0, 2.0, 4.0); // beats 2..6

    let region = make_region(&project, 0.0, 8.0, 0, 0);
    let clip = &project.timeline.layers[0].clips[0];
    let trimmed = EditingService::trim_clip_to_region(clip, &region, 0.5);

    // Fully inside region — no trimming
    assert!((trimmed.start_beat - Beats(2.0)).abs() < Beats(0.001));
    assert!((trimmed.duration_beats - Beats(4.0)).abs() < Beats(0.001));
}

#[test]
fn trim_clip_to_region_straddles_start() {
    let mut project = make_project();
    let _id = add_clip(&mut project, 0, 0.0, 8.0); // beats 0..8

    let region = make_region(&project, 2.0, 10.0, 0, 0);
    let clip = &project.timeline.layers[0].clips[0];
    let trimmed = EditingService::trim_clip_to_region(clip, &region, 0.5);

    // Trimmed at start: should start at 2.0, duration 6.0
    assert!((trimmed.start_beat - Beats(2.0)).abs() < Beats(0.001));
    assert!((trimmed.duration_beats - Beats(6.0)).abs() < Beats(0.001));
    // InPoint adjusted by (2.0 - 0.0) * 0.5 = 1.0 seconds
    assert!((trimmed.in_point.0 - 1.0).abs() < 0.001);
}

#[test]
fn trim_clip_to_region_straddles_end() {
    let mut project = make_project();
    let _id = add_clip(&mut project, 0, 4.0, 8.0); // beats 4..12

    let region = make_region(&project, 0.0, 8.0, 0, 0);
    let clip = &project.timeline.layers[0].clips[0];
    let trimmed = EditingService::trim_clip_to_region(clip, &region, 0.5);

    // Trimmed at end: should start at 4.0, duration 4.0
    assert!((trimmed.start_beat - Beats(4.0)).abs() < Beats(0.001));
    assert!((trimmed.duration_beats - Beats(4.0)).abs() < Beats(0.001));
}

#[test]
fn trim_clip_to_region_straddles_both() {
    let mut project = make_project();
    let _id = add_clip(&mut project, 0, 0.0, 16.0); // beats 0..16

    let region = make_region(&project, 4.0, 12.0, 0, 0);
    let clip = &project.timeline.layers[0].clips[0];
    let trimmed = EditingService::trim_clip_to_region(clip, &region, 0.5);

    // Trimmed at both: 4.0..12.0
    assert!((trimmed.start_beat - Beats(4.0)).abs() < Beats(0.001));
    assert!((trimmed.duration_beats - Beats(8.0)).abs() < Beats(0.001));
    assert!((trimmed.in_point.0 - 2.0).abs() < 0.001); // (4.0 - 0.0) * 0.5
}

// ─── Region-aware copy ───

#[test]
fn copy_clips_region_mode_trims() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 8.0); // beats 0..8

    let region = make_region(&project, 2.0, 6.0, 0, 0);

    let mut service = EditingService::new();
    service.copy_clips(&project, &[id1], Some(&region), 0.5);
    assert!(service.has_clipboard());

    // Paste to verify trimmed content
    let result = service.paste_clips(&mut project, Beats(10.0), 0, 0.5);
    assert_eq!(result.pasted_clip_ids.len(), 1);

    for mut cmd in result.commands {
        cmd.execute(&mut project);
    }
    project.timeline.rebuild_clip_lookup();

    // Find the pasted clip
    let pasted = project.timeline.layers[0]
        .clips
        .iter()
        .find(|c| c.id == result.pasted_clip_ids[0])
        .unwrap();
    // Should be trimmed: start=10.0 (paste target + 0 offset), duration=4.0 (6-2)
    assert!((pasted.start_beat - Beats(10.0)).abs() < Beats(0.001));
    assert!((pasted.duration_beats - Beats(4.0)).abs() < Beats(0.001));
}

// ─── Region-aware duplicate ───

#[test]
fn duplicate_clips_region_mode_trims() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 8.0); // beats 0..8

    let region = make_region(&project, 2.0, 6.0, 0, 0);

    let cmds = EditingService::duplicate_clips(&project, &[id1.clone()], &region, 0.5);
    // 1 command: AddClipCommand (overlap enforcement is internal)
    assert_eq!(cmds.len(), 1);

    let mut service = EditingService::new();
    service.execute_batch(cmds, "dup".into(), &mut project);

    // Should have 2 clips: original (0..6, trimmed) + duplicate (6..10)
    assert_eq!(project.timeline.layers[0].clips.len(), 2);
    let orig = project.timeline.layers[0]
        .clips
        .iter()
        .find(|c| c.id == id1)
        .unwrap();
    assert!(
        (orig.duration_beats - Beats(6.0)).abs() < Beats(0.001),
        "original should be trimmed to 6 beats, got {}",
        orig.duration_beats.0
    );
    let dup = project.timeline.layers[0]
        .clips
        .iter()
        .find(|c| c.start_beat > Beats(5.0))
        .unwrap();
    // Region duration is 4.0, so duplicate starts at 2.0 + 4.0 = 6.0
    assert!((dup.start_beat - Beats(6.0)).abs() < Beats(0.001));
    assert!((dup.duration_beats - Beats(4.0)).abs() < Beats(0.001));
    // No overlaps
    assert!(!project.timeline.layers[0].has_overlapping_clips());
}

// ─── Region Delete ───

#[test]
fn delete_region_straddles_both_boundaries() {
    // Clip [0..8], region [2..6] → delete interior [2..6], keep [0..2] and [6..8]
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 8.0);

    let region = make_region(&project, 2.0, 6.0, 0, 0);
    let spb = 60.0 / project.settings.bpm.0;
    let cmds = EditingService::delete_clips(&project, &[id1.clone()], Some(&region), spb);
    assert!(!cmds.is_empty(), "should produce split + delete commands");

    let mut service = EditingService::new();
    service.execute_batch(cmds, "del".into(), &mut project);

    // Should have 2 clips: [0..2] and [6..8]
    let clips = &project.timeline.layers[0].clips;
    assert_eq!(clips.len(), 2, "expected 2 clips, got {}", clips.len());

    let mut sorted: Vec<_> = clips.iter().collect();
    sorted.sort_by(|a, b| a.start_beat.partial_cmp(&b.start_beat).unwrap());

    assert!((sorted[0].start_beat - Beats(0.0)).abs() < Beats(0.001));
    assert!((sorted[0].end_beat() - Beats(2.0)).abs() < Beats(0.001));
    assert!((sorted[1].start_beat - Beats(6.0)).abs() < Beats(0.001));
    assert!((sorted[1].end_beat() - Beats(8.0)).abs() < Beats(0.001));
}

#[test]
fn delete_region_straddles_start_only() {
    // Clip [0..4], region [2..6] → delete interior [2..4], keep [0..2]
    let mut project = make_project();
    let _id1 = add_clip(&mut project, 0, 0.0, 4.0);

    let region = make_region(&project, 2.0, 6.0, 0, 0);
    let spb = 60.0 / project.settings.bpm.0;
    let cmds = EditingService::delete_clips(&project, &[], Some(&region), spb);

    let mut service = EditingService::new();
    service.execute_batch(cmds, "del".into(), &mut project);

    let clips = &project.timeline.layers[0].clips;
    assert_eq!(clips.len(), 1, "expected 1 clip, got {}", clips.len());
    assert!((clips[0].start_beat - Beats(0.0)).abs() < Beats(0.001));
    assert!((clips[0].end_beat() - Beats(2.0)).abs() < Beats(0.001));
}

#[test]
fn delete_region_straddles_end_only() {
    // Clip [4..10], region [2..6] → delete interior [4..6], keep [6..10]
    let mut project = make_project();
    let _id1 = add_clip(&mut project, 0, 4.0, 6.0);

    let region = make_region(&project, 2.0, 6.0, 0, 0);
    let spb = 60.0 / project.settings.bpm.0;
    let cmds = EditingService::delete_clips(&project, &[], Some(&region), spb);

    let mut service = EditingService::new();
    service.execute_batch(cmds, "del".into(), &mut project);

    let clips = &project.timeline.layers[0].clips;
    assert_eq!(clips.len(), 1, "expected 1 clip, got {}", clips.len());
    assert!((clips[0].start_beat - Beats(6.0)).abs() < Beats(0.001));
    assert!((clips[0].end_beat() - Beats(10.0)).abs() < Beats(0.001));
}

#[test]
fn delete_region_clip_fully_inside() {
    // Clip [3..5], region [2..6] → delete entirely
    let mut project = make_project();
    let _id1 = add_clip(&mut project, 0, 3.0, 2.0);

    let region = make_region(&project, 2.0, 6.0, 0, 0);
    let spb = 60.0 / project.settings.bpm.0;
    let cmds = EditingService::delete_clips(&project, &[], Some(&region), spb);

    let mut service = EditingService::new();
    service.execute_batch(cmds, "del".into(), &mut project);

    let clips = &project.timeline.layers[0].clips;
    assert_eq!(clips.len(), 0, "expected 0 clips, got {}", clips.len());
}

#[test]
fn delete_region_undo_restores_original() {
    // Clip [0..8], region [2..6] → delete → undo → original [0..8] restored
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 8.0);

    let region = make_region(&project, 2.0, 6.0, 0, 0);
    let spb = 60.0 / project.settings.bpm.0;
    let cmds = EditingService::delete_clips(&project, &[id1.clone()], Some(&region), spb);

    let mut service = EditingService::new();
    service.execute_batch(cmds, "del".into(), &mut project);
    assert_eq!(project.timeline.layers[0].clips.len(), 2);

    // Undo
    service.undo(&mut project);

    let clips = &project.timeline.layers[0].clips;
    assert_eq!(clips.len(), 1, "undo should restore to 1 clip, got {}", clips.len());
    let clip = &clips[0];
    assert_eq!(clip.id, id1);
    assert!((clip.start_beat - Beats(0.0)).abs() < Beats(0.001));
    assert!((clip.duration_beats - Beats(8.0)).abs() < Beats(0.001));
}
