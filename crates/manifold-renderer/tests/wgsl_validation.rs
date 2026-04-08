//! Auto-discovers and validates all WGSL shader files via naga.
//! Catches syntax errors, type mismatches, and binding declaration errors
//! at test time instead of first render. Zero maintenance — new shaders
//! are auto-discovered, modified shaders auto-re-validated.

use std::path::PathBuf;

fn shader_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Recursively find all .wgsl files under a directory.
fn find_wgsl_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(find_wgsl_files(&path));
            } else if path.extension().is_some_and(|ext| ext == "wgsl") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Files that are partials (no entry points, included by other shaders).
/// These won't validate standalone — skip them.
const PARTIAL_SHADERS: &[&str] = &["particle_common.wgsl", "oily_fluid.wgsl"];

fn is_partial(path: &std::path::Path) -> bool {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        PARTIAL_SHADERS.contains(&name)
    } else {
        false
    }
}

#[test]
fn all_wgsl_shaders_validate() {
    let files = find_wgsl_files(&shader_dir());
    assert!(
        !files.is_empty(),
        "No .wgsl files found — test infrastructure broken"
    );

    let mut validated = 0;
    let mut skipped = 0;
    let mut errors = Vec::new();

    for path in &files {
        if is_partial(path) {
            skipped += 1;
            continue;
        }

        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));

        let relative = path.strip_prefix(shader_dir()).unwrap_or(path);

        // Parse WGSL
        let module = match naga::front::wgsl::parse_str(&source) {
            Ok(m) => m,
            Err(e) => {
                errors.push(format!("{}: parse error: {e}", relative.display()));
                continue;
            }
        };

        // Validate
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = validator.validate(&module) {
            errors.push(format!("{}: validation error: {e}", relative.display()));
            continue;
        }

        validated += 1;
    }

    if !errors.is_empty() {
        panic!(
            "{} shader(s) failed validation:\n{}",
            errors.len(),
            errors.join("\n"),
        );
    }

    assert!(
        validated > 0,
        "No shaders were validated (all skipped?). Found {} files, skipped {}",
        files.len(),
        skipped,
    );

    eprintln!("Validated {validated} shaders, skipped {skipped} partials");
}

/// Validates the composed (particle_common + oily_fluid) source as dispatched
/// at runtime. Skipped by the standalone sweep because it depends on
/// particle_common for `simplex_noise_3d` / `wang_hash`.
#[test]
fn oily_fluid_composed_validates() {
    let common = include_str!("../src/generators/shaders/particle_common.wgsl");
    let oily = include_str!("../src/generators/shaders/oily_fluid.wgsl");
    let composed = format!("{common}\n{oily}");

    let module = naga::front::wgsl::parse_str(&composed)
        .unwrap_or_else(|e| panic!("oily_fluid composed parse error: {e}"));
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .unwrap_or_else(|e| panic!("oily_fluid composed validation error: {e}"));
}
