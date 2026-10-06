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
    // Specialization templates whose missing symbols are injected at
    // pipeline creation: `gaussian_blur_variable_width` has its
    // QUALITY_LEVEL / WEIGHTING_MODE consts replaced by the preprocessor;
    // `radial_burst_force_field` has `noise_common.wgsl` (→ `simplex3d`)
    // prepended. The composed forms are validated at pipeline creation and
    // exercised by the bundled-preset execute tests.
    "gaussian_blur_variable_width.wgsl",
    "radial_burst_force_field.wgsl",
    // `wgsl_includes` of the marching-cubes atoms: reads the kernel's
    // `buf_levelset` binding. Their generated kernels are validated at
    // pipeline creation and by the liquid-surface GPU value tests.
    "marching_cubes_common.wgsl",
    // `wgsl_includes` of the liquid collider atoms: calls `liquid_pose.wgsl`
    // and the kernel's own `liquid_atlas_half`. Their generated kernels are
    // validated by each atom's codegen test and the matter GPU proofs.
    "liquid_collider.wgsl",
    // Liquid-brick `wgsl_includes` and dense reference kernels: they read the
    // generated kernel's bindings (`buf_levelset`, `buf_solid`, `buf_bricks`,
    // the marching-cubes tables). Their composed forms are validated by
    // `liquid_bricks_tests::fluid_bricks_generated_consumers_validate_on_cpu`.
    "liquid_bricks_common.wgsl",
    "smooth_lattice_element.wgsl",
    "clamp_liquid_to_solids_element.wgsl",
    "smooth_lattice_dense_reference.wgsl",
    "clamp_liquid_to_solids_dense_reference.wgsl",
    "particle_volume_dense_reference.wgsl",
    "count_surface_triangles_dense_reference.wgsl",
    "volume_surface_mesh_dense_reference.wgsl",
    "relax_surface_mesh_dense_reference.wgsl",
    // Welded-mesh `wgsl_includes`: they read the marching-cubes tables and the
    // kernel's `buf_levelset` / `buf_edge_scan`. Each user's generated kernel
    // validates on the CPU in its own module (count_surface_edges,
    // volume_surface_mesh, relax_surface_mesh, surface_mesh_normals).
    "surface_edge_index.wgsl",
    "surface_edge_ownership.wgsl",
    "surface_mesh_adjacency.wgsl",
];

const NOISE_COMMON: &str = include_str!("../src/generators/shaders/noise_common.wgsl");
const PBR_BRDF: &str = include_str!("../src/node_graph/primitives/shaders/pbr_brdf.wgsl");
const TONEMAP_COMMON: &str = include_str!("../src/effects/shaders/tonemap_common.wgsl");
const SAMPLE_FACE_COMMON: &str =
    include_str!("../src/node_graph/primitives/shaders/sample_face_common.wgsl");
/// `node.gpu_flip_step`'s prelude: pose, collider sampling and the force
/// field, in its `step_source` order.
const GPU_FLIP_STEP_PRELUDE: &str = concat!(
    include_str!("../src/node_graph/primitives/shaders/liquid_pose.wgsl"),
    "\n",
    include_str!("../src/node_graph/primitives/shaders/liquid_collider.wgsl"),
    "\n",
    include_str!("../src/node_graph/primitives/shaders/liquid_field.wgsl"),
);

/// Shaders whose pipeline prepends a shared helper file at creation time.
/// Each validates in that composed form, the way production builds it.
const COMPOSED_SHADERS: &[(&str, &str)] = &[
    ("render_mesh_diagram.wgsl", SAMPLE_FACE_COMMON),
    ("aces_tonemap_compute.wgsl", TONEMAP_COMMON),
    ("presentation.wgsl", TONEMAP_COMMON),
    ("simplex_per_instance.wgsl", NOISE_COMMON),
    ("fbm_per_instance.wgsl", NOISE_COMMON),
    ("instance_position_jitter.wgsl", NOISE_COMMON),
    ("instance_rotation_jitter.wgsl", NOISE_COMMON),
    ("ibl_prefilter_specular.wgsl", PBR_BRDF),
    ("ibl_irradiance.wgsl", PBR_BRDF),
    ("ibl_brdf_lut.wgsl", PBR_BRDF),
    ("gpu_flip_step.wgsl", GPU_FLIP_STEP_PRELUDE),
    ("whitewater_fused.wgsl", concat!(
        "const LF_PACKED: bool = false;\n",
        include_str!("../src/node_graph/primitives/shaders/whitewater_common.wgsl"), "\n",
        include_str!("../src/node_graph/primitives/shaders/liquid_faces.wgsl"), "\n",
        include_str!("../src/node_graph/primitives/shaders/liquid_field.wgsl"),
    )),
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
        let prefix = COMPOSED_SHADERS
            .iter()
            .find(|(name, _)| path.file_name().is_some_and(|n| n == *name))
            .map(|(_, prefix)| *prefix);
        let source = match prefix {
            Some(prefix) => format!("{prefix}\n{source}"),
            None => source,
        };
        let source = if path.file_name().is_some_and(|name| {
            ["gpu_flip_step.wgsl", "liquid_stats.wgsl", "particle_publication.wgsl", "liquid_frame_faces.wgsl"]
                .iter().any(|shader| name == *shader)
        }) {
            manifold_renderer::node_graph::with_liquid_stats_layout(&source)
        } else {
            source
        };

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
