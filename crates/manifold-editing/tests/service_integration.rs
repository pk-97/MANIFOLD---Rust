use manifold_editing::service::EditingService;
use manifold_core::clip::TimelineClip;
use manifold_core::project::Project;
use manifold_core::layer::Layer;
use manifold_core::selection::SelectionRegion;
use manifold_core::types::*;

fn make_project() -> Project {
    let mut project = Project::default();
    project.settings.bpm = 120.0;
    project.settings.time_signature_numerator = 4;
    project.timeline.insert_layer(0, Layer::new("Video 1".into(), LayerType::Video, 0));
    project.timeline.insert_layer(1, Layer::new("Video 2".into(), LayerType::Video, 1));
    project.timeline.rebuild_clip_lookup();
    project
}

fn add_clip(project: &mut Project, layer: usize, start: f32, dur: f32) -> String {
    let clip = TimelineClip {
        start_beat: start,
        duration_beats: dur,
        layer_index: layer as i32,
        ..Default::default()
    };
    let id = clip.id.clone();
    project.timeline.layers[layer].add_clip(clip);
    project.timeline.mark_clip_lookup_dirty();
    id
}

// ─── Overlap enforcement ───

#[test]
fn overlap_covers_both_deletes() {
    let mut project = make_project();
    let existing_id = add_clip(&mut project, 0, 2.0, 2.0); // [2..4]

    let placed = TimelineClip {
        start_beat: 1.0,
        duration_beats: 5.0, // [1..6] covers [2..4]
        layer_index: 0,
        ..Default::default()
    };

    let cmds = EditingService::enforce_non_overlap(&project, &placed, 0, &Default::default());
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

    let placed = TimelineClip {
        start_beat: 1.0,
        duration_beats: 3.0, // [1..4] covers start of [2..6]
        layer_index: 0,
        ..Default::default()
    };

    let cmds = EditingService::enforce_non_overlap(&project, &placed, 0, &Default::default());
    assert_eq!(cmds.len(), 1);

    let mut service = EditingService::new();
    service.execute_batch(cmds, "overlap".into(), &mut project);

    let clip = project.timeline.find_clip_by_id(&existing_id).unwrap();
    assert!((clip.start_beat - 4.0).abs() < 0.001); // trimmed to start at placed_end
    assert!((clip.end_beat() - 6.0).abs() < 0.001);
}

#[test]
fn overlap_covers_end_trims() {
    let mut project = make_project();
    let existing_id = add_clip(&mut project, 0, 2.0, 4.0); // [2..6]

    let placed = TimelineClip {
        start_beat: 4.0,
        duration_beats: 4.0, // [4..8] covers end of [2..6]
        layer_index: 0,
        ..Default::default()
    };

    let cmds = EditingService::enforce_non_overlap(&project, &placed, 0, &Default::default());
    assert_eq!(cmds.len(), 1);

    let mut service = EditingService::new();
    service.execute_batch(cmds, "overlap".into(), &mut project);

    let clip = project.timeline.find_clip_by_id(&existing_id).unwrap();
    assert!((clip.start_beat - 2.0).abs() < 0.001);
    assert!((clip.duration_beats - 2.0).abs() < 0.001); // trimmed to end at placed_start
}

#[test]
fn overlap_splits_middle() {
    let mut project = make_project();
    let existing_id = add_clip(&mut project, 0, 0.0, 8.0); // [0..8]

    let placed = TimelineClip {
        start_beat: 3.0,
        duration_beats: 2.0, // [3..5] in middle of [0..8]
        layer_index: 0,
        ..Default::default()
    };

    let cmds = EditingService::enforce_non_overlap(&project, &placed, 0, &Default::default());
    assert_eq!(cmds.len(), 2); // trim + add tail

    let mut service = EditingService::new();
    service.execute_batch(cmds, "overlap".into(), &mut project);

    // Original clip trimmed to [0..3]
    let clip = project.timeline.find_clip_by_id(&existing_id).unwrap();
    assert!((clip.start_beat - 0.0).abs() < 0.001);
    assert!((clip.duration_beats - 3.0).abs() < 0.001);

    // Tail clip added at [5..8]
    assert_eq!(project.timeline.layers[0].clips.len(), 2);
    let tail = project.timeline.layers[0].clips.iter()
        .find(|c| c.id != existing_id)
        .expect("tail clip should exist");
    assert!((tail.start_beat - 5.0).abs() < 0.001);
    assert!((tail.duration_beats - 3.0).abs() < 0.001);
}

// ─── Clipboard ───

#[test]
fn copy_paste_roundtrip() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 4.0);
    let id2 = add_clip(&mut project, 0, 4.0, 4.0);

    let mut service = EditingService::new();
    service.copy_clips(&project, &[id1.clone(), id2.clone()]);
    assert!(service.has_clipboard());

    let result = service.paste_clips(&mut project, 10.0, 0);
    assert_eq!(result.pasted_clip_ids.len(), 2);

    // Execute all paste commands
    for mut cmd in result.commands {
        cmd.execute(&mut project);
    }
    project.timeline.rebuild_clip_lookup();

    // Pasted clips should have new IDs
    assert!(result.pasted_clip_ids.iter().all(|id| id != &id1 && id != &id2));

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
    service.copy_clips(&project, &[id1, id2, id3]);

    let result = service.paste_clips(&mut project, 10.0, 0);
    for mut cmd in result.commands {
        cmd.execute(&mut project);
    }
    project.timeline.rebuild_clip_lookup();

    // Collect pasted clip beats
    let mut pasted: Vec<(f32, i32)> = result.pasted_clip_ids.iter()
        .map(|id| {
            let c = project.timeline.find_clip_by_id(id).unwrap();
            (c.start_beat, c.layer_index)
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

    let region = SelectionRegion {
        start_beat: 0.0,
        end_beat: 4.0,
        start_layer_index: 0,
        end_layer_index: 0,
        is_active: true,
    };

    let cmds = EditingService::duplicate_clips(&project, &[id1.clone()], &region);
    assert_eq!(cmds.len(), 1);

    let mut service = EditingService::new();
    service.execute_batch(cmds, "dup".into(), &mut project);
    project.timeline.rebuild_clip_lookup();

    assert_eq!(project.timeline.layers[0].clips.len(), 2);
    let dup = project.timeline.layers[0].clips.iter()
        .find(|c| c.id != id1)
        .unwrap();
    assert!((dup.start_beat - 4.0).abs() < 0.001); // shifted by region duration (4)
}

// ─── Delete ───

#[test]
fn delete_clips_removes() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 4.0);
    let _id2 = add_clip(&mut project, 0, 4.0, 4.0);

    let cmds = EditingService::delete_clips(&project, &[id1.clone()]);
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

    let mut cmd = EditingService::create_clip_at_position(&mut project, 2.0, 0, 4.0);
    cmd.execute(&mut project);
    project.timeline.rebuild_clip_lookup();

    assert_eq!(project.timeline.layers[0].clips.len(), initial + 1);
    let clip = &project.timeline.layers[0].clips[initial];
    assert!((clip.start_beat - 2.0).abs() < 0.001);
    assert!((clip.duration_beats - 4.0).abs() < 0.001);
}

// ─── Nudge ───

#[test]
fn nudge_selected_clips() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 2.0, 4.0);

    let cmds = EditingService::nudge_clips(&project, &[id1.clone()], 1.0);
    assert_eq!(cmds.len(), 1);

    let mut service = EditingService::new();
    service.execute_batch(cmds, "nudge".into(), &mut project);

    let clip = project.timeline.find_clip_by_id(&id1).unwrap();
    assert!((clip.start_beat - 3.0).abs() < 0.001);
}

// ─── Undo/Redo ───

#[test]
fn multi_step_undo_redo() {
    let mut project = make_project();
    let mut service = EditingService::new();

    // Execute 5 operations
    for i in 0..5 {
        let cmd = EditingService::create_clip_at_position(
            &mut project, i as f32 * 4.0, 0, 4.0,
        );
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

    let cmd = EditingService::create_clip_at_position(&mut project, 0.0, 0, 4.0);
    service.execute(cmd, &mut project);
    assert_eq!(service.data_version(), 1);

    service.undo(&mut project);
    assert_eq!(service.data_version(), 2);

    service.redo(&mut project);
    assert_eq!(service.data_version(), 3);
}

#[test]
fn dirty_flag_tracks_saves() {
    let mut project = make_project();
    let mut service = EditingService::new();

    assert!(!service.is_dirty());

    let cmd = EditingService::create_clip_at_position(&mut project, 0.0, 0, 4.0);
    service.execute(cmd, &mut project);
    assert!(service.is_dirty());

    service.mark_clean();
    assert!(!service.is_dirty());

    service.undo(&mut project);
    assert!(service.is_dirty());
}

// ─── Split ───

#[test]
fn split_at_beat() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 8.0);

    let cmd = EditingService::split_clip_at_beat(&project, &id1, 4.0);
    assert!(cmd.is_some());

    let mut cmd = cmd.unwrap();
    cmd.execute(&mut project);
    project.timeline.rebuild_clip_lookup();

    let original = project.timeline.find_clip_by_id(&id1).unwrap();
    assert!((original.duration_beats - 4.0).abs() < 0.001);

    assert_eq!(project.timeline.layers[0].clips.len(), 2);
    let tail = project.timeline.layers[0].clips.iter()
        .find(|c| c.id != id1)
        .unwrap();
    assert!((tail.start_beat - 4.0).abs() < 0.001);
    assert!((tail.duration_beats - 4.0).abs() < 0.001);
}

#[test]
fn split_at_boundary_returns_none() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 8.0);

    // Split at start — invalid
    assert!(EditingService::split_clip_at_beat(&project, &id1, 0.0).is_none());
    // Split at end — invalid
    assert!(EditingService::split_clip_at_beat(&project, &id1, 8.0).is_none());
}

// ─── Extend/Shrink ───

#[test]
fn extend_shrink_by_grid() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 4.0);

    // Extend
    let cmds = EditingService::extend_clips_by_grid(&project, &[id1.clone()], 1.0);
    let mut service = EditingService::new();
    service.execute_batch(cmds, "ext".into(), &mut project);
    let clip = project.timeline.find_clip_by_id(&id1).unwrap();
    assert!((clip.duration_beats - 5.0).abs() < 0.001);

    // Shrink
    let cmds = EditingService::shrink_clips_by_grid(&project, &[id1.clone()], 1.0);
    service.execute_batch(cmds, "shrink".into(), &mut project);
    let clip = project.timeline.find_clip_by_id(&id1).unwrap();
    assert!((clip.duration_beats - 4.0).abs() < 0.001);
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

    let clip = project.timeline.find_clip_by_id(&id1).unwrap();
    assert_eq!(clip.layer_index, 1);
}

// ─── Selection region ───

#[test]
fn get_clips_in_region() {
    let mut project = make_project();
    let id1 = add_clip(&mut project, 0, 0.0, 4.0);
    let _id2 = add_clip(&mut project, 0, 8.0, 4.0); // outside region
    let id3 = add_clip(&mut project, 1, 2.0, 4.0);

    let region = SelectionRegion {
        start_beat: 1.0,
        end_beat: 5.0,
        start_layer_index: 0,
        end_layer_index: 1,
        is_active: true,
    };

    let results = EditingService::get_clips_in_region(&project, &region);
    let result_ids: Vec<&String> = results.iter().map(|(_, id)| id).collect();
    assert!(result_ids.contains(&&id1));
    assert!(result_ids.contains(&&id3));
    assert_eq!(results.len(), 2);
}
