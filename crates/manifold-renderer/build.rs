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

const EFFECT_PRESETS_DIR: &str = "assets/effect-presets";
const GENERATOR_PRESETS_DIR: &str = "assets/generator-presets";

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    // ── Effect presets ─────────────────────────────────────────────
    let effect_dir = manifest_dir.join(EFFECT_PRESETS_DIR);
    register_rerun_triggers(&effect_dir);
    let effect_entries = scan_presets(&effect_dir, /*require_dir=*/ true);
    fs::write(
        out_dir.join("bundled_presets_generated.rs"),
        render_generated_file(&effect_entries, "BUNDLED_PRESETS_GENERATED", "effect-presets"),
    )
    .expect("write bundled_presets_generated.rs");

    // ── Generator presets ──────────────────────────────────────────
    // Optional directory at the moment — generators are mid-migration
    // from Rust factories into JSON. An empty/missing directory emits
    // an empty array, which is fine.
    let generator_dir = manifest_dir.join(GENERATOR_PRESETS_DIR);
    register_rerun_triggers(&generator_dir);
    let generator_entries = scan_presets(&generator_dir, /*require_dir=*/ false);
    fs::write(
        out_dir.join("bundled_generator_presets_generated.rs"),
        render_generated_file(
            &generator_entries,
            "BUNDLED_GENERATOR_PRESETS_GENERATED",
            "generator-presets",
        ),
    )
    .expect("write bundled_generator_presets_generated.rs");
}

/// Tell cargo to rerun the build script when a preset directory or
/// any file inside changes.
fn register_rerun_triggers(dir: &Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
    if dir.is_dir() {
        for entry in fs::read_dir(dir).expect("read preset dir") {
            let entry = entry.expect("read dir entry");
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }
}

/// One scanned preset file — type id (filename stem) + absolute path
/// to the JSON content for `include_str!`.
struct PresetEntry {
    type_id: String,
    json_path: PathBuf,
}

/// Walk the presets dir, validate each `*.json`, return entries sorted
/// by type id. Sort is stable for diff hygiene — same order every build.
///
/// `require_dir = true` panics if the directory is missing (the original
/// behaviour for effect-presets, which always must exist).
/// `require_dir = false` returns an empty list silently — used for
/// generator-presets, which is mid-migration and may be absent in some
/// development branches.
fn scan_presets(dir: &Path, require_dir: bool) -> Vec<PresetEntry> {
    if !dir.is_dir() {
        if require_dir {
            panic!(
                "presets directory not found at {} — build.rs expects \
                 this directory relative to crate root",
                dir.display()
            );
        }
        return Vec::new();
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

        validate(&path, &type_id);

        entries.push(PresetEntry {
            type_id,
            json_path: path,
        });
    }

    entries.sort_by(|a, b| a.type_id.cmp(&b.type_id));
    entries
}

/// Structural validation — parses as JSON, requires a numeric
/// `version` field, and (if present) requires `presetMetadata.id` to
/// match the file stem so the bundled-preset registry (keyed by file
/// stem) lines up with the effect-definition registry (keyed by
/// `presetMetadata.id`). A mismatch causes a silent chain-build None
/// at runtime — see the WireframeDepthGraph incident, 2026-05-28.
/// Deeper validation runs at runtime when the file is loaded into
/// [`EffectGraphDef`] with the full type system; we keep build-time
/// checks structural so build.rs doesn't need to drag in the
/// renderer's type defs.
fn validate(path: &Path, expected_type_id: &str) {
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

    if let Some(metadata_id) = obj
        .get("presetMetadata")
        .and_then(|m| m.as_object())
        .and_then(|m| m.get("id"))
        .and_then(|v| v.as_str())
        && metadata_id != expected_type_id
    {
        panic!(
            "preset {}: file stem `{expected_type_id}` does not match \
             `presetMetadata.id` `{metadata_id}` — these must agree, or the \
             chain build silently fails at runtime (the bundled-preset \
             registry keys by file stem; the effect-definition registry \
             keys by `presetMetadata.id`; mismatched names cause \
             `loaded_preset_view_by_id` to return None and the layer \
             falls back to source passthrough with no log output until \
             the [chain-build-fail] instrumentation lands). Either rename \
             the file to `{metadata_id}.json` or change `presetMetadata.id` \
             to `{expected_type_id}`.",
            path.display()
        );
    }
}

fn render_generated_file(
    entries: &[PresetEntry],
    array_name: &str,
    source_subdir: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "// Generated by build.rs from assets/{source_subdir}/*.json.\n"
    ));
    out.push_str("// Do not hand-edit; regenerate by adding/removing JSON files.\n\n");
    out.push_str(&format!(
        "/// Bundled preset JSON, one entry per `assets/{source_subdir}/*.json` file,\n\
         /// sorted alphabetically by type id. Each entry is `(type_id, json_bytes)`.\n\
         pub const {array_name}: &[(&str, &str)] = &[\n",
    ));
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
