use manifold_editing::command::Command;
use manifold_editing::commands::clip::{MoveClipCommand, TrimClipCommand, MuteClipCommand, AddClipCommand, DeleteClipCommand};
use manifold_editing::commands::settings::ChangeBpmCommand;
use manifold_editing::undo::UndoRedoManager;
use manifold_core::clip::TimelineClip;

fn fixture_path(name: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../tests/fixtures");
    p.push(name);
    p
}

fn load_project(name: &str) -> manifold_core::project::Project {
    let path = fixture_path(name);
    manifold_io::loader::load_project(&path)
        .unwrap_or_else(|e| panic!("Failed to load {name}: {e}"))
}

#[test]
fn move_clip_undo_restores_position() {
    let mut project = load_project("Burn V5.manifold");

    let clip_id = project.timeline.layers[0].clips[0].id.clone();
    let original_beat = project.timeline.layers[0].clips[0].start_beat;

    let new_beat = original_beat + 4.0;
    let mut cmd = MoveClipCommand::new(clip_id.clone(), original_beat, new_beat, 0, 0);

    // Execute
    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.start_beat - new_beat).abs() < 0.001, "Clip should have moved to {new_beat}");

    // Undo
    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.start_beat - original_beat).abs() < 0.001, "Undo should restore to {original_beat}");
}

#[test]
fn trim_clip_undo_restores_duration() {
    let mut project = load_project("Burn V5.manifold");

    let clip_id = project.timeline.layers[0].clips[0].id.clone();
    let original_start = project.timeline.layers[0].clips[0].start_beat;
    let original_dur = project.timeline.layers[0].clips[0].duration_beats;
    let original_in = project.timeline.layers[0].clips[0].in_point;

    let new_dur = original_dur + 2.0;
    let mut cmd = TrimClipCommand::new(
        clip_id.clone(),
        original_start, original_start,
        original_dur, new_dur,
        original_in, original_in,
    );

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.duration_beats - new_dur).abs() < 0.001);

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.duration_beats - original_dur).abs() < 0.001);
}

#[test]
fn mute_clip_undo_roundtrip() {
    let mut project = load_project("Burn V5.manifold");

    let clip_id = project.timeline.layers[0].clips[0].id.clone();
    let was_muted = project.timeline.layers[0].clips[0].is_muted;

    let mut cmd = MuteClipCommand::new(clip_id.clone(), was_muted, !was_muted);

    cmd.execute(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert_eq!(clip.is_muted, !was_muted);

    cmd.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert_eq!(clip.is_muted, was_muted);
}

#[test]
fn change_bpm_undo_roundtrip() {
    let mut project = load_project("Burn V5.manifold");

    let old_bpm = project.settings.bpm;
    let new_bpm = 120.0;

    let mut cmd = ChangeBpmCommand::new(old_bpm, new_bpm);

    cmd.execute(&mut project);
    assert!((project.settings.bpm - new_bpm).abs() < 0.01);

    cmd.undo(&mut project);
    assert!((project.settings.bpm - old_bpm).abs() < 0.01);
}

#[test]
fn add_delete_clip_undo_roundtrip() {
    let mut project = load_project("Burn V5.manifold");

    let initial_count = project.timeline.total_clip_count();
    let new_clip = TimelineClip::default();
    let new_clip_id = new_clip.id.clone();

    // Add
    let mut add_cmd = AddClipCommand::new(new_clip, 0);
    add_cmd.execute(&mut project);
    assert_eq!(project.timeline.total_clip_count(), initial_count + 1);

    // Undo add
    add_cmd.undo(&mut project);
    assert_eq!(project.timeline.total_clip_count(), initial_count);

    // Redo add (execute again)
    add_cmd.execute(&mut project);
    assert_eq!(project.timeline.total_clip_count(), initial_count + 1);

    // Delete the clip we added
    let clip_to_delete = project.timeline.layers[0].find_clip(&new_clip_id).unwrap().clone();
    let mut del_cmd = DeleteClipCommand::new(clip_to_delete, 0);
    del_cmd.execute(&mut project);
    assert_eq!(project.timeline.total_clip_count(), initial_count);

    // Undo delete
    del_cmd.undo(&mut project);
    assert_eq!(project.timeline.total_clip_count(), initial_count + 1);
}

#[test]
fn undo_manager_multi_command_roundtrip() {
    let mut project = load_project("Burn V5.manifold");
    let mut undo_mgr = UndoRedoManager::new();

    let original_bpm = project.settings.bpm;
    let clip_id = project.timeline.layers[0].clips[0].id.clone();
    let original_beat = project.timeline.layers[0].clips[0].start_beat;

    // Command 1: change BPM
    let mut cmd1 = Box::new(ChangeBpmCommand::new(original_bpm, 120.0));
    cmd1.execute(&mut project);
    undo_mgr.record(cmd1);

    // Command 2: move clip
    let mut cmd2 = Box::new(MoveClipCommand::new(clip_id.clone(), original_beat, original_beat + 8.0, 0, 0));
    cmd2.execute(&mut project);
    undo_mgr.record(cmd2);

    // Verify state after both commands
    assert!((project.settings.bpm - 120.0).abs() < 0.01);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.start_beat - (original_beat + 8.0)).abs() < 0.001);

    // Undo command 2
    assert!(undo_mgr.can_undo());
    undo_mgr.undo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.start_beat - original_beat).abs() < 0.001);
    assert!((project.settings.bpm - 120.0).abs() < 0.01); // BPM unchanged

    // Undo command 1
    undo_mgr.undo(&mut project);
    assert!((project.settings.bpm - original_bpm).abs() < 0.01);

    // Redo both
    undo_mgr.redo(&mut project);
    assert!((project.settings.bpm - 120.0).abs() < 0.01);

    undo_mgr.redo(&mut project);
    let clip = project.timeline.find_clip_by_id(&clip_id).unwrap();
    assert!((clip.start_beat - (original_beat + 8.0)).abs() < 0.001);
}
