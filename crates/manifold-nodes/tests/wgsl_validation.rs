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
    "smooth_lattice_dense_reference.wgsl",
    "particle_volume_dense_reference.wgsl",
    // Preludes of `render_scene`'s composed shader and the water passes; each
    // user validates composed below.
    "instance_transform.wgsl",
    "scene_shading.wgsl",
    "look_ripple_detail.wgsl",
    "foam_field.wgsl",
    "look_foam.wgsl",
    "look_mist.wgsl",
    "look_thin_water_light.wgsl",
    "look_wet_surface.wgsl",
    "water_field_common.wgsl",
    "water_march_core.wgsl",
];

const NOISE_COMMON: &str = manifold_node_engine::gpu::shader_sources::NOISE_COMMON_WGSL;
const PBR_BRDF: &str =
    include_str!("../../manifold-nodes-scene/src/node_graph/primitives/shaders/pbr_brdf.wgsl");
const TONEMAP_COMMON: &str =
    include_str!("../../manifold-compositor/src/effects/shaders/tonemap_common.wgsl");
const DEPTH_COMMON: &str =
    include_str!("../../manifold-nodes-image/src/generators/shaders/depth_common.wgsl");
const VIEW_NORMAL_COMMON: &str = include_str!(
    "../../manifold-nodes-image/src/node_graph/primitives/shaders/view_normal_common.wgsl"
);
const INSTANCE_TRANSFORM: &str = include_str!(
    "../../manifold-nodes-scene/src/node_graph/primitives/shaders/instance_transform.wgsl"
);
const SCENE_SHADING: &str =
    include_str!("../../manifold-nodes-scene/src/node_graph/primitives/shaders/scene_shading.wgsl");
const LOOK_RIPPLE_DETAIL: &str = include_str!(
    "../../manifold-nodes-scene/src/node_graph/primitives/shaders/look_ripple_detail.wgsl"
);
const FOAM_FIELD: &str =
    include_str!("../../manifold-nodes-scene/src/node_graph/primitives/shaders/foam_field.wgsl");
const LOOK_FOAM: &str =
    include_str!("../../manifold-nodes-scene/src/node_graph/primitives/shaders/look_foam.wgsl");
const LOOK_MIST: &str =
    include_str!("../../manifold-nodes-scene/src/node_graph/primitives/shaders/look_mist.wgsl");
const LOOK_THIN_WATER_LIGHT: &str = include_str!(
    "../../manifold-nodes-scene/src/node_graph/primitives/shaders/look_thin_water_light.wgsl"
);
const LOOK_WET_SURFACE: &str = include_str!(
    "../../manifold-nodes-scene/src/node_graph/primitives/shaders/look_wet_surface.wgsl"
);
const WATER_FIELD_COMMON: &str = include_str!(
    "../../manifold-nodes-scene/src/node_graph/primitives/shaders/water_field_common.wgsl"
);
const WATER_MARCH_CORE: &str = include_str!(
    "../../manifold-nodes-scene/src/node_graph/primitives/shaders/water_march_core.wgsl"
);
const SAMPLE_FACE_COMMON: &str = include_str!(
    "../../manifold-nodes-scene/src/node_graph/primitives/shaders/sample_face_common.wgsl"
);
/// Shaders whose pipeline prepends a shared helper file at creation time.
/// Each validates in that composed form, the way production builds it.
fn composed_shaders() -> Vec<(&'static str, String)> {
    use manifold_water_liquid::{grid, whitewater};
    let mut composed: Vec<(&'static str, String)> = [
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
    ]
    .map(|(name, prefix)| (name, prefix.to_owned()))
    .into();
    composed.push((
        "temporal_accumulate.wgsl",
        [DEPTH_COMMON, VIEW_NORMAL_COMMON].join("\n"),
    ));
    // `render_scene`'s composed shader (`RENDER_SCENE_WGSL`): the instance
    // transforms, the scene shading and the five look hooks, then the mesh
    // shader. The per-copy cull and the water march have their own preludes.
    composed.push((
        "render_scene.wgsl",
        [
            INSTANCE_TRANSFORM,
            SCENE_SHADING,
            LOOK_RIPPLE_DETAIL,
            FOAM_FIELD,
            LOOK_FOAM,
            LOOK_MIST,
            LOOK_THIN_WATER_LIGHT,
            LOOK_WET_SURFACE,
        ]
        .join("\n"),
    ));
    composed.push(("cull_instances.wgsl", INSTANCE_TRANSFORM.to_owned()));
    composed.push((
        "water_march.wgsl",
        [WATER_FIELD_COMMON, WATER_MARCH_CORE].join("\n"),
    ));
    composed.push((
        "water_mist.wgsl",
        [WATER_FIELD_COMMON, WATER_MARCH_CORE].join("\n"),
    ));
    composed.push(("water_cover.wgsl", WATER_MARCH_CORE.to_owned()));
    // The water shade (`WATER_SHADE_WGSL`): the scene shading and the five
    // look hooks, then the field helpers.
    composed.push((
        "water_shade.wgsl",
        [
            SCENE_SHADING,
            LOOK_RIPPLE_DETAIL,
            FOAM_FIELD,
            LOOK_FOAM,
            LOOK_MIST,
            LOOK_THIN_WATER_LIGHT,
            LOOK_WET_SURFACE,
            WATER_FIELD_COMMON,
        ]
        .join("\n"),
    ));
    composed.push((
        "whitewater_fused.wgsl",
        [
            whitewater::WHITEWATER_COMMON,
            grid::LIQUID_FACES,
            manifold_nodes_physics::force_lattice::FORCE_LATTICE,
        ]
        .join("\n"),
    ));
    composed
}

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
    let composed = composed_shaders();
    let mut files = find_wgsl_files(&shader_dir());
    let leaf_shaders = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../manifold-compositor/src");
    assert!(
        leaf_shaders.is_dir(),
        "manifold-compositor shader directory must exist"
    );
    files.extend(find_wgsl_files(&leaf_shaders));
    let leaf_shaders =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../manifold-nodes-scene/src");
    assert!(
        leaf_shaders.is_dir(),
        "manifold-nodes-scene shader directory must exist"
    );
    files.extend(find_wgsl_files(&leaf_shaders));
    let leaf_shaders =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../manifold-nodes-image/src");
    assert!(
        leaf_shaders.is_dir(),
        "manifold-nodes-image shader directory must exist"
    );
    files.extend(find_wgsl_files(&leaf_shaders));
    let shared_shaders =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../manifold-node-engine/src/gpu/shaders");
    assert!(
        shared_shaders.is_dir(),
        "engine shared shader directory must exist"
    );
    files.extend(find_wgsl_files(&shared_shaders));
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
        let prefix = composed
            .iter()
            .find(|(name, _)| path.file_name().is_some_and(|n| n == *name))
            .map(|(_, prefix)| prefix);
        let source = match prefix {
            Some(prefix) => format!("{prefix}\n{source}"),
            None => source,
        };
        let source = if path.file_name().is_some_and(|name| {
            [
                "liquid_stats.wgsl",
                "particle_publication.wgsl",
            ]
            .iter()
            .any(|shader| name == *shader)
        }) {
            manifold_water_liquid::primitives::liquid_stats::with_stats_layout(&source)
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
