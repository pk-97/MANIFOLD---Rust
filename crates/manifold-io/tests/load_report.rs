//! BUG-063 — `Project::load_report` surfaces what a load silently repaired.
//! `docs/PROJECT_FILE_INTEGRITY_DESIGN.md` §3.6 / P3.

use manifold_core::effects::PresetInstance;
use manifold_core::preset_type_id::PresetTypeId;
use manifold_core::project::Project;
use manifold_core::types::LayerType;
use manifold_core::Beats;
use manifold_core::clip::TimelineClip;
use manifold_io::loader;

/// Build a project with a known-unknown master effect and a layer holding
/// two overlapping clips — the two repairs P3 surfaces.
fn repairing_project() -> Project {
    let mut project = Project::default();

    // Unknown effect on the master chain (strip_unknown_effects's job).
    project
        .settings
        .master_effects
        .push(PresetInstance::new(PresetTypeId::BLOOM));
    project
        .settings
        .master_effects
        .push(PresetInstance::new(PresetTypeId::UNKNOWN));

    // Overlapping clips on a layer — `restore_clip` bypasses the write-time
    // overlap invariant (`Layer::add_clip`), the same way a legacy file's
    // raw clip list would deserialize before repair runs.
    let idx = project
        .timeline
        .add_layer("Video 1", LayerType::Video, PresetTypeId::NONE);
    let layer = &mut project.timeline.layers[idx];
    layer.restore_clip(TimelineClip {
        start_beat: Beats(0.0),
        duration_beats: Beats(4.0),
        ..TimelineClip::default()
    });
    layer.restore_clip(TimelineClip {
        start_beat: Beats(2.0),
        duration_beats: Beats(4.0),
        ..TimelineClip::default()
    });
    assert!(layer.has_overlapping_clips());

    project
}

/// Exercises the two production write sites in order: `strip_unknown_effects`
/// (called by `load_project_from_json_with`) and `run_post_load_validation`
/// (overlap repair, purge, missing-file detection).
fn load_and_report(mut project: Project) -> Project {
    project.load_report.unknown_effects_removed = project.strip_unknown_effects();
    loader::run_post_load_validation(&mut project);
    project
}

#[test]
fn load_report_surfaces_unknown_effect_and_overlap_repair() {
    let project = load_and_report(repairing_project());

    assert!(
        project.load_report.unknown_effects_removed >= 1,
        "expected the UNKNOWN master effect to be counted, got {:?}",
        project.load_report
    );
    assert!(
        project.load_report.overlapping_clips_repaired >= 1,
        "expected the overlapping clip repair to be counted, got {:?}",
        project.load_report
    );

    let lines = project.load_report.human_lines();
    assert!(
        lines.iter().any(|l| l.contains("unknown effect")),
        "human_lines should name the unknown-effect repair: {lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("overlapping clip")),
        "human_lines should name the overlap repair: {lines:?}"
    );

    // The repairs themselves still happened — visibility-only, no behavior
    // change (P3's forbidden move).
    assert_eq!(project.settings.master_effects.len(), 1);
    assert!(!project.timeline.layers[0].has_overlapping_clips());
}

#[test]
fn load_report_is_empty_for_a_clean_project() {
    let mut project = Project::default();
    project.load_report.unknown_effects_removed = project.strip_unknown_effects();
    loader::run_post_load_validation(&mut project);

    assert!(project.load_report.is_empty());
    assert!(project.load_report.human_lines().is_empty());
}

/// BUG-079: a master effect referencing an unregistered preset def (no
/// `preset_definition_registry` entry, no inline generator `meta.params`)
/// must not degrade silently. `build_param_manifest`'s keep-don't-drop
/// branch already kept the saved param on a placeholder spec instead of
/// dropping it (BUG-036) — this proves the loader now ALSO counts it into
/// `load_report.unresolved_preset_templates`, so the "opened with repairs"
/// toast in `manifold-app` names it instead of the count staying console-only
/// (the `eprintln!` in `effects.rs::build_param_manifest`).
#[test]
fn load_report_surfaces_an_unresolvable_preset_def() {
    let project = Project::default();
    let json = serde_json::to_string(&project).expect("serialize");
    let mut value: serde_json::Value = serde_json::from_str(&json).expect("reparse");

    let effects = value["settings"]["masterEffects"]
        .as_array_mut()
        .expect("masterEffects should serialize as an array");
    effects.push(serde_json::json!({
        "id": "ghost-effect-1",
        "effectType": "TotallyMadeUpEffectTypeNobodyRegisters",
        "enabled": true,
        "collapsed": false,
        "params": {
            "someSavedParam": { "value": 0.42, "exposed": true }
        }
    }));

    let rewritten = serde_json::to_string(&value).expect("reserialize");
    let loaded =
        manifold_io::loader::load_project_from_json(&rewritten).expect("project must still load");

    assert_eq!(
        loaded.load_report.unresolved_preset_templates, 1,
        "expected the unregistered preset def to be counted, got {:?}",
        loaded.load_report
    );
    assert!(!loaded.load_report.is_empty());

    let lines = loaded.load_report.human_lines();
    assert!(
        lines.iter().any(|l| l.contains("unresolved preset")),
        "human_lines should name the unresolved-preset repair: {lines:?}"
    );

    // Keep-don't-drop (BUG-036): the saved param survives on the instance
    // rather than vanishing.
    let ghost = loaded
        .settings
        .master_effects
        .iter()
        .find(|fx| fx.effect_type().as_str() == "TotallyMadeUpEffectTypeNobodyRegisters")
        .expect("the unresolvable effect must still be present, not dropped");
    assert!(
        ghost.params.get("someSavedParam").is_some(),
        "the saved param must be kept on a placeholder spec, not dropped"
    );
}

/// `#[serde(skip)]` proof: a save→reload round-trip never writes
/// `loadReport`/`load_report` onto disk, and a freshly-parsed project's
/// report starts empty regardless of what the in-memory source held.
#[test]
fn load_report_never_serializes() {
    let mut project = repairing_project();
    project.load_report.unknown_effects_removed = 3;
    project.load_report.missing_media_files = vec!["missing.mp4".to_string()];

    let json = serde_json::to_string(&project).expect("serialize");
    assert!(
        !json.to_lowercase().contains("loadreport") && !json.to_lowercase().contains("load_report"),
        "load_report must never appear in serialized JSON"
    );

    let reloaded: Project = serde_json::from_str(&json).expect("deserialize");
    assert!(
        reloaded.load_report.is_empty(),
        "a freshly deserialized project must start with an empty load_report"
    );
}
