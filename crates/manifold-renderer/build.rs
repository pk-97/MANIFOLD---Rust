//! Build-time codegen for the bundled preset table.
//!
//! Scans `assets/effect-presets/*.json` and emits an alphabetically
//! sorted `pub const BUNDLED_PRESETS_GENERATED` array containing
//! `(type_id, include_str!(json))` tuples — one entry per JSON file.
//! The runtime preset loader (`bundled_presets.rs`) `include!`s the
//! generated file instead of hand-maintaining the table, so adding a
//! preset is just dropping a JSON file in the directory.
//!
//! Validation at this stage is structural only: each file must be
//! parseable JSON with an integer `version` field. Deeper validation
//! (every `typeId` in `nodes` references a registered primitive, every
//! `bindings.target.handle` exists in the canonical graph) happens at
//! runtime when the parsed [`EffectGraphDef`] hits the loader — that's
//! where the [`PrimitiveRegistry`] is available.
//!
//! Re-runs only when `assets/effect-presets/` changes.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const PRESETS_DIR: &str = "assets/effect-presets";

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let presets_dir = manifest_dir.join(PRESETS_DIR);

    println!("cargo:rerun-if-changed={}", presets_dir.display());
    // Rerun if any file *inside* the directory changes too. Touching
    // the directory alone (e.g., new file) is covered by the line
    // above; this catches edits to existing files on platforms that
    // don't bubble file mtime up to the dir.
    if presets_dir.is_dir() {
        for entry in fs::read_dir(&presets_dir).expect("read effect-presets dir") {
            let entry = entry.expect("read dir entry");
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }

    let entries = scan_presets(&presets_dir);

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let out_path = out_dir.join("bundled_presets_generated.rs");
    let generated = render_generated_file(&entries);
    fs::write(&out_path, generated).expect("write bundled_presets_generated.rs");
}

/// One scanned preset file — type id (filename stem) + absolute path
/// to the JSON content for `include_str!`.
struct PresetEntry {
    type_id: String,
    json_path: PathBuf,
}

/// Walk the presets dir, validate each `*.json`, return entries sorted
/// by type id. Sort is stable for diff hygiene — same order every build.
fn scan_presets(dir: &Path) -> Vec<PresetEntry> {
    if !dir.is_dir() {
        panic!(
            "effect-presets directory not found at {} — build.rs expects \
             `assets/effect-presets/` relative to crate root",
            dir.display()
        );
    }

    let mut entries = Vec::new();

    for entry in fs::read_dir(dir).expect("read effect-presets dir") {
        let entry = entry.expect("read dir entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        let type_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_else(|| {
                panic!(
                    "preset file has no valid stem: {}",
                    path.display()
                )
            })
            .to_string();

        validate(&path);

        entries.push(PresetEntry {
            type_id,
            json_path: path,
        });
    }

    entries.sort_by(|a, b| a.type_id.cmp(&b.type_id));
    entries
}

/// Structural validation — parses as JSON, requires a numeric
/// `version` field. Deeper validation runs at runtime when the file is
/// loaded into [`EffectGraphDef`] with the full type system; we keep
/// build-time checks structural so build.rs doesn't need to drag in
/// the renderer's type defs.
fn validate(path: &Path) {
    let content = fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("read {}: {e}", path.display());
    });

    let value: serde_json::Value = serde_json::from_str(&content).unwrap_or_else(|e| {
        panic!(
            "preset {} is not valid JSON: {e}",
            path.display()
        );
    });

    let obj = value.as_object().unwrap_or_else(|| {
        panic!(
            "preset {} top-level must be a JSON object",
            path.display()
        );
    });

    let version = obj
        .get("version")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| {
            panic!(
                "preset {} missing integer `version` field",
                path.display()
            );
        });

    if version > 2 {
        panic!(
            "preset {} declares version {version}, max supported is 2 \
             (EFFECT_GRAPH_VERSION_WITH_METADATA)",
            path.display()
        );
    }

    if obj.get("nodes").is_none() {
        panic!(
            "preset {} missing required `nodes` field",
            path.display()
        );
    }
    if obj.get("wires").is_none() {
        panic!(
            "preset {} missing required `wires` field",
            path.display()
        );
    }
}

fn render_generated_file(entries: &[PresetEntry]) -> String {
    let mut out = String::new();
    out.push_str("// Generated by build.rs from assets/effect-presets/*.json.\n");
    out.push_str("// Do not hand-edit; regenerate by adding/removing JSON files.\n\n");
    out.push_str(
        "/// Bundled preset JSON, one entry per `assets/effect-presets/*.json` file,\n\
         /// sorted alphabetically by type id. Each entry is `(type_id, json_bytes)`.\n\
         pub const BUNDLED_PRESETS_GENERATED: &[(&str, &str)] = &[\n",
    );
    for e in entries {
        // Use forward slashes in the include_str! path so it works on
        // both Unix and Windows (build.rs runs on all platforms even
        // if Manifold's targets are Apple-only).
        let path_str = e.json_path.to_string_lossy().replace('\\', "/");
        out.push_str(&format!(
            "    (\"{}\", include_str!(\"{}\")),\n",
            e.type_id, path_str
        ));
    }
    out.push_str("];\n");
    out
}
