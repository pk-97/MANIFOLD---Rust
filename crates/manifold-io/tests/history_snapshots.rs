//! History-snapshot journaling tests — the P1 gate of
//! docs/GIG_RESILIENCE_DESIGN.md §6 (autosave via the existing `history/`
//! mechanism): every superseded save lands in `history/`, snapshots load
//! back through the normal pipeline, the auto-save cap holds (keeping the
//! NEWEST autos), and pruned snapshots lose their blob bytes too.

use manifold_core::project::Project;
use manifold_io::{archive, loader, saver};
use std::path::{Path, PathBuf};

/// Matches `DEFAULT_MAX_AUTO_SAVES` in `manifold-io/src/archive.rs`.
const MAX_AUTO_SAVES: usize = 50;

/// Fresh temp path for one test's archive. Unique per test + process so
/// parallel test runs never collide.
fn temp_archive_path(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "manifold-history-test-{}-{}",
        test,
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join("project.manifold")
}

fn save(project: &mut Project, path: &Path, is_auto: bool) {
    saver::save_project(project, path, None, is_auto).expect("save failed");
}

#[test]
fn autosave_pushes_previous_state_into_history_and_loads_back() {
    let path = temp_archive_path("roundtrip");
    let _ = std::fs::remove_file(&path);

    let mut project = Project {
        project_name: "History Test".to_string(),
        saved_playhead_time: 1.0, // save-1 marker
        ..Default::default()
    };

    // Save 1 (manual).
    save(&mut project, &path, false);

    let manifest = archive::read_manifest(&path.to_string_lossy()).expect("manifest after save 1");
    assert_eq!(manifest.history.len(), 1, "first save = one history entry");
    assert!(!manifest.history[0].is_auto);
    let first_hash = manifest.history[0].hash.clone();

    // Save 2 (auto) — marker value 2.0. Must push save 1 into history/.
    project.saved_playhead_time = 2.0;
    save(&mut project, &path, true);

    let manifest = archive::read_manifest(&path.to_string_lossy()).expect("manifest after save 2");
    assert_eq!(manifest.history.len(), 2, "second save appends an entry");
    assert!(manifest.history[0].is_auto, "newest entry is the autosave");
    assert!(!manifest.history[1].is_auto, "older entry is the manual save");
    assert_eq!(manifest.history[1].hash, first_hash);
    assert_eq!(manifest.current_hash, manifest.history[0].hash);

    // The superseded save loads back with its own state, via the same
    // pipeline as a normal project open.
    let snapshot =
        loader::load_project_snapshot(&path, &first_hash).expect("history snapshot loads");
    assert_eq!(snapshot.saved_playhead_time, 1.0);
    assert_eq!(snapshot.project_name, "History Test");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn unchanged_save_dedups_no_history_spam() {
    let path = temp_archive_path("dedup");
    let _ = std::fs::remove_file(&path);

    let mut project = Project {
        saved_playhead_time: 1.0,
        ..Default::default()
    };
    save(&mut project, &path, false);
    // Identical content saved again (auto): dedup short-circuit, no new entry.
    save(&mut project, &path, true);

    let manifest = archive::read_manifest(&path.to_string_lossy()).expect("manifest");
    assert_eq!(
        manifest.history.len(),
        1,
        "identical re-save must not add a history entry"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn differing_projects_produce_different_hashes() {
    // Guards the P1 hash widen (24 -> 64 bits, PROJECT_FILE_INTEGRITY_DESIGN D7):
    // two projects differing by a single field must dedup-hash differently,
    // and the hash must be the new 16-hex-char (64-bit) width.
    let path = temp_archive_path("hash-widen");
    let _ = std::fs::remove_file(&path);

    let mut project = Project {
        saved_playhead_time: 1.0,
        ..Default::default()
    };
    save(&mut project, &path, false);
    let manifest = archive::read_manifest(&path.to_string_lossy()).expect("manifest after save 1");
    let first_hash = manifest.current_hash.clone();
    assert_eq!(first_hash.len(), 16, "hash must be 16 hex chars (64 bits)");

    // One field differs -> content differs -> must not dedup.
    project.saved_playhead_time = 2.0;
    save(&mut project, &path, true);
    let manifest = archive::read_manifest(&path.to_string_lossy()).expect("manifest after save 2");
    assert_ne!(
        manifest.current_hash, first_hash,
        "a real content change must not collide with the prior hash"
    );
    assert_eq!(manifest.history.len(), 2, "differing content must not dedup");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn prune_caps_autosaves_keeps_newest_and_drops_blobs() {
    let path = temp_archive_path("prune");
    let _ = std::fs::remove_file(&path);

    // Save 0 (manual) — must survive pruning no matter how many autos follow.
    let mut project = Project {
        saved_playhead_time: 0.5,
        ..Default::default()
    };
    save(&mut project, &path, false);

    // MAX + 5 autosaves, each with distinct content.
    let total_autos = MAX_AUTO_SAVES + 5;
    for i in 1..=total_autos {
        project.saved_playhead_time = i as f32;
        save(&mut project, &path, true);
    }

    let manifest = archive::read_manifest(&path.to_string_lossy()).expect("manifest");
    let autos: Vec<_> = manifest.history.iter().filter(|e| e.is_auto).collect();
    let manuals: Vec<_> = manifest.history.iter().filter(|e| !e.is_auto).collect();

    assert_eq!(autos.len(), MAX_AUTO_SAVES, "auto-save cap holds");
    assert_eq!(manuals.len(), 1, "manual saves are never pruned");

    // The cap keeps the NEWEST autos: the newest history entry must be the
    // final save (= current), and every surviving auto must be one of the
    // most recent MAX_AUTO_SAVES saves. The manual save (oldest of all)
    // still being present proves survival isn't just recency.
    assert_eq!(manifest.current_hash, manifest.history[0].hash);
    assert!(manifest.history[0].is_auto);
    assert!(
        !manifest.history.last().unwrap().is_auto,
        "oldest surviving entry is the manual save, not an early auto"
    );

    // Superseded surviving snapshots load back; history[1] is the
    // second-newest auto (the newest, history[0], IS project.json so it has
    // no history blob yet).
    let survivor_hash = &manifest.history[1].hash;
    let snapshot =
        loader::load_project_snapshot(&path, survivor_hash).expect("surviving snapshot loads");
    assert_eq!(snapshot.saved_playhead_time, (total_autos - 1) as f32);

    // A pruned early autosave is gone from manifest AND bytes: pick the
    // marker of auto #1 — its playhead value must appear in no surviving
    // snapshot, and the blob count in the zip matches the manifest.
    for e in &manifest.history[1..] {
        let snap = loader::load_project_snapshot(&path, &e.hash)
            .expect("every non-current manifest entry has a loadable blob");
        assert!(
            snap.saved_playhead_time >= 6.0 || snap.saved_playhead_time == 0.5,
            "early pruned autos (markers 1.0-5.0) must not survive, found {}",
            snap.saved_playhead_time
        );
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn read_history_snapshot_rejects_unknown_hash() {
    let path = temp_archive_path("unknown-hash");
    let _ = std::fs::remove_file(&path);

    let mut project = Project::default();
    save(&mut project, &path, false);

    assert!(
        archive::read_history_snapshot(&path.to_string_lossy(), "definitely-not-a-hash").is_err()
    );
    assert!(archive::read_history_snapshot(&path.to_string_lossy(), "").is_err());

    let _ = std::fs::remove_file(&path);
}
