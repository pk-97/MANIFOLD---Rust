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
const PARTIAL_SHADERS: &[&str] = &[
    "particle_common.wgsl",
    "oily_fluid.wgsl",
    "noise_common.wgsl",
    // Per-instance noise / jitter primitives prepend noise_common
    // at pipeline-creation time; each has a dedicated composed
    // validator below.
    "simplex_per_instance.wgsl",
    "fbm_per_instance.wgsl",
    "instance_position_jitter.wgsl",
    "instance_rotation_jitter.wgsl",
    // Specialization templates whose missing symbols are injected at
    // pipeline creation: `gaussian_blur_variable_width` has its
    // QUALITY_LEVEL / WEIGHTING_MODE consts replaced by the preprocessor;
    // `radial_burst_force_field` has `noise_common.wgsl` (→ `simplex3d`)
    // prepended. The composed forms are validated at pipeline creation and
    // exercised by the bundled-preset execute tests.
    "gaussian_blur_variable_width.wgsl",
    "radial_burst_force_field.wgsl",
];

fn is_partial(path: &std::path::Path) -> bool {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        // `*_body.wgsl` are `primitive!`-macro body fragments: the macro wraps
        // them with the struct/uniform/helper preamble (`Element`,
        // `BodyOutputs`, injected `simplex3d`, etc.) at pipeline creation, so
        // they reference symbols that only exist post-composition and cannot
        // validate standalone. The composed shader is validated when its
        // pipeline is built and run by the execute-one-frame tests.
        name.ends_with("_body.wgsl") || PARTIAL_SHADERS.contains(&name)
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

/// Composed-source validators for shaders that prepend noise_common.wgsl
/// at pipeline-creation time (simplex3d / fbm / hash_u32 live there).
fn validate_composed_with_noise_common(label: &str, main_src: &str) {
    let noise = include_str!("../src/generators/shaders/noise_common.wgsl");
    let composed = format!("{noise}\n{main_src}");
    let module = naga::front::wgsl::parse_str(&composed)
        .unwrap_or_else(|e| panic!("{label} composed parse error: {e}"));
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .unwrap_or_else(|e| panic!("{label} composed validation error: {e}"));
}

#[test]
fn simplex_per_instance_composed_validates() {
    validate_composed_with_noise_common(
        "simplex_per_instance",
        include_str!(
            "../src/node_graph/primitives/shaders/simplex_per_instance.wgsl"
        ),
    );
}

#[test]
fn fbm_per_instance_composed_validates() {
    validate_composed_with_noise_common(
        "fbm_per_instance",
        include_str!(
            "../src/node_graph/primitives/shaders/fbm_per_instance.wgsl"
        ),
    );
}

#[test]
fn instance_position_jitter_composed_validates() {
    validate_composed_with_noise_common(
        "instance_position_jitter",
        include_str!(
            "../src/node_graph/primitives/shaders/instance_position_jitter.wgsl"
        ),
    );
}

#[test]
fn instance_rotation_jitter_composed_validates() {
    validate_composed_with_noise_common(
        "instance_rotation_jitter",
        include_str!(
            "../src/node_graph/primitives/shaders/instance_rotation_jitter.wgsl"
        ),
    );
}

// `oily_fluid` was decomposed into atomic primitives; the all-in-one
// shader composition no longer exists. The component primitives
// (simplex_field_2d, gradient_central_diff, texture_advect, etc.) are
// each validated by the all-WGSL sweep above.
