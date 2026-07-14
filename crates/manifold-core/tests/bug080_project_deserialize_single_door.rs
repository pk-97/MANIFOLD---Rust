//! BUG-080 D3: the loader (`crates/manifold-io/`) is the single production
//! door for `Project`/`PresetInstance` deserialization — everything else
//! (a future ingest/paste/merge path) must go through it so
//! `reconcile_param_manifests()` always runs. This walks the workspace for
//! the raw deserialize call patterns and fails on any hit outside
//! `crates/manifold-io/`, test code, and `ui_snapshot` fixtures — see
//! `docs/PARAM_MANIFEST_GATE_DESIGN.md` D3.
//!
//! Sealing the `Deserialize` impl isn't an option (serde traits are public
//! API the loader and tests legitimately use) — this rg-shaped meta-test is
//! the enforceable form, mirrored on `docs_index_sync.rs`'s workspace-walk
//! pattern.

use std::fs;
use std::path::{Path, PathBuf};

const PATTERNS: &[&str] = &[
    "from_str::<Project>",
    "from_value::<Project>",
    "from_reader::<Project>",
    "from_str::<PresetInstance>",
    "from_value::<PresetInstance>",
    "from_reader::<PresetInstance>",
];

fn crates_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/crates/manifold-core
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("crates")
}

fn walk_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "target" {
                continue;
            }
            walk_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Test code, the loader crate itself, and `ui_snapshot` fixtures are the
/// declared exemptions (D3) — everything else is production surface.
fn is_allowed(path: &Path, contents: &str) -> bool {
    let s = path.to_string_lossy();
    s.contains("/manifold-io/")
        || s.contains("/tests/")
        || s.contains("ui_snapshot")
        || contents.contains("#[cfg(test)]")
}

#[test]
fn bug080_project_deserialize_single_door() {
    let mut files = Vec::new();
    walk_rs_files(&crates_dir(), &mut files);

    let mut violations = Vec::new();
    for path in files {
        let Ok(contents) = fs::read_to_string(&path) else {
            continue;
        };
        if is_allowed(&path, &contents) {
            continue;
        }
        for pattern in PATTERNS {
            if contents.contains(pattern) {
                violations.push(format!("{}: {pattern}", path.display()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "BUG-080 D3: found Project/PresetInstance deserialize outside the loader door \
         (crates/manifold-io/), test code, and ui_snapshot fixtures. Route through \
         manifold-io's loader (which reconciles param manifests) instead, or if this really \
         is test/fixture code, mark the file with #[cfg(test)] or move it under a `tests/` \
         dir:\n{}",
        violations.join("\n")
    );
}
