//! Forward-version guard (BUG-062) — PROJECT_FILE_INTEGRITY_DESIGN.md §3.2/§3.3, P2.
//!
//! Proves the guard fires BEFORE migration for a file newer than this build,
//! still opens a current-version file, still migrates an older file forward,
//! and that the archive-container guard (D5 site 2) refuses independently.

use manifold_core::project::{Project, CURRENT_PROJECT_VERSION};
use manifold_io::loader::{self, LoadError};
use manifold_io::manifest::CURRENT_ARCHIVE_FORMAT_VERSION;
use std::io::Write as _;

fn temp_path(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "manifold_fwd_guard_{}_{}_{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default()
    ));
    p
}

/// Save `Project::default()` as plain V1 JSON, rewrite its `projectVersion`
/// field, and return the resulting file path. Plain-JSON V1 is the simplest
/// vehicle for this: `load_project_with` falls back to it whenever the bytes
/// aren't a valid ZIP, and rewriting a top-level string field via `serde_json`
/// keeps the rest of the document in valid, fully-current shape.
fn write_project_with_version(version: &str, path: &std::path::Path) {
    let project = Project::default();
    let json = serde_json::to_string_pretty(&project).expect("serialize");
    let mut value: serde_json::Value = serde_json::from_str(&json).expect("reparse");
    value["projectVersion"] = serde_json::Value::String(version.to_string());
    let rewritten = serde_json::to_string_pretty(&value).expect("reserialize");
    std::fs::write(path, rewritten).expect("write temp project file");
}

#[test]
fn refuses_a_file_newer_than_this_build() {
    let path = temp_path("too_new.manifold");
    write_project_with_version("1.99.0", &path);

    let err = loader::load_project(&path).expect_err("a newer file must be refused");
    match &err {
        LoadError::TooNew {
            file_version,
            this_version,
        } => {
            assert!(
                file_version.contains("1.99"),
                "file_version should name 1.99, got {file_version}"
            );
            assert!(
                this_version.contains(CURRENT_PROJECT_VERSION),
                "this_version should name {CURRENT_PROJECT_VERSION}, got {this_version}"
            );
        }
        other => panic!("expected LoadError::TooNew, got {other:?}"),
    }

    let message = err.to_string();
    assert!(message.contains("1.99"), "message: {message}");
    assert!(message.contains(CURRENT_PROJECT_VERSION), "message: {message}");
    assert!(
        message.contains("Update MANIFOLD to open it"),
        "message: {message}"
    );
    println!("TooNew Display: {message}");

    std::fs::remove_file(&path).ok();
}

#[test]
fn opens_a_file_at_exactly_the_current_version() {
    let path = temp_path("current.manifold");
    write_project_with_version(CURRENT_PROJECT_VERSION, &path);

    loader::load_project(&path).expect("a current-version file must still open");

    std::fs::remove_file(&path).ok();
}

#[test]
fn migrates_an_older_file_forward() {
    let path = temp_path("older.manifold");
    write_project_with_version("1.5.0", &path);

    loader::load_project(&path).expect("an older file must still migrate and open");

    std::fs::remove_file(&path).ok();
}

/// Held-out input: a hand-written minimal JSON the fixture-shape tests above
/// never produce — an unknown top-level field alongside a too-new version —
/// proving the guard fires on the raw `projectVersion` value alone, not on
/// anything specific to `Project::default()`'s shape.
#[test]
fn refuses_a_hand_written_minimal_json_with_unknown_field() {
    let json = r#"{
        "projectVersion": "2.0.0",
        "someFutureFieldThisBuildHasNeverHeardOf": true
    }"#;

    let err =
        loader::load_project_from_json(json).expect_err("a too-new held-out file must be refused");
    match err {
        LoadError::TooNew { file_version, .. } => {
            assert_eq!(file_version, "2.0.0");
        }
        other => panic!("expected LoadError::TooNew, got {other:?}"),
    }
}

/// Never panics on a malformed `projectVersion` — degrades to "1.0.0"
/// (never too new) instead.
#[test]
fn malformed_project_version_degrades_instead_of_panicking() {
    let json = r#"{ "projectVersion": 42 }"#;
    // Not a string — degrades to "1.0.0", which is never newer than
    // CURRENT_PROJECT_VERSION, so this must NOT come back as TooNew.
    let result = loader::load_project_from_json(json);
    // Deserialize/Migration error or Ok are both fine — the point is no panic, no false TooNew.
    if let Err(LoadError::TooNew { .. }) = result {
        panic!("malformed projectVersion must not read as too-new");
    }
}

/// Build a minimal, hand-rolled V2 archive (manifest.json + project.json)
/// with an arbitrary `format_version`, bypassing `archive::save_v2_archive`
/// so the manifest's format_version can be set beyond what this build knows.
fn write_v2_archive_with_format_version(format_version: i32, path: &std::path::Path) {
    let project = Project::default();
    let project_json = serde_json::to_string_pretty(&project).expect("serialize project");

    let manifest_json = format!(
        r#"{{"formatVersion":{format_version},"name":"guard-test","currentHash":"","savedAt":"","history":[]}}"#
    );

    let file = std::fs::File::create(path).expect("create archive file");
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);

    zip.start_file("project.json", options)
        .expect("start project.json");
    zip.write_all(project_json.as_bytes())
        .expect("write project.json");

    zip.start_file("manifest.json", options)
        .expect("start manifest.json");
    zip.write_all(manifest_json.as_bytes())
        .expect("write manifest.json");

    zip.finish().expect("finish zip");
}

#[test]
fn refuses_an_archive_with_a_too_new_container_format() {
    let path = temp_path("too_new_container.manifold");
    let too_new_format = CURRENT_ARCHIVE_FORMAT_VERSION + 1;
    write_v2_archive_with_format_version(too_new_format, &path);

    let err = loader::load_project(&path).expect_err("a too-new archive container must be refused");
    match err {
        LoadError::TooNew {
            file_version,
            this_version,
        } => {
            assert_eq!(file_version, format!("archive v{too_new_format}"));
            assert_eq!(
                this_version,
                format!("archive v{CURRENT_ARCHIVE_FORMAT_VERSION}")
            );
        }
        other => panic!("expected LoadError::TooNew, got {other:?}"),
    }

    std::fs::remove_file(&path).ok();
}

#[test]
fn opens_an_archive_at_the_current_container_format() {
    let path = temp_path("current_container.manifold");
    write_v2_archive_with_format_version(CURRENT_ARCHIVE_FORMAT_VERSION, &path);

    loader::load_project(&path).expect("an archive at the current container format must open");

    std::fs::remove_file(&path).ok();
}
