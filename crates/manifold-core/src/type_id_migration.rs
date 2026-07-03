//! Load-time rename table for node/preset `type_id` strings — the
//! infrastructure half of `docs/NODE_VOCABULARY_AUDIT.md`.
//!
//! One flat string→string table serves every id namespace that identifies a
//! node or preset by name: graph-node `type_id`s (`node.gain`) and
//! [`crate::PresetTypeId`] values (`"Bloom"`, `"EdgeGlow"`). The two
//! namespaces never collide (`node.`/`system.` prefix vs. bare PascalCase),
//! so one table and one lookup function cover both — matching §3's "one
//! static table" shape rather than one per document kind.
//!
//! **Old ids are never reused** (§2 rule 5): once an id is retired here it is
//! retired permanently, so a stale entry can never resolve to the wrong
//! current node.
//!
//! **What this table is NOT for:** the bundled preset JSON library, parity/
//! gpu tests, `hand_descriptor!` entries, and `primitive!` registrations are
//! rewritten directly in the repo as part of the rename commit (§3, fourth
//! bullet) — they never carry an old id in the first place, so migration
//! never runs on them. This table exists only for **content that already
//! shipped**: saved project files (clips, effect/generator instances, their
//! embedded [`crate::effect_graph_def::EffectGraphDef`] graphs) that may
//! still carry an id from before a rename.
//!
//! Choke points (both wired in P1 — see `docs/NODE_VOCABULARY_AUDIT.md` §3,
//! §9 P1):
//! - `EffectGraphDef` node `type_id`s — migrated in
//!   `manifold_renderer::node_graph::graph_loader::instantiate_def`, before
//!   the group flatten, recursing into group bodies. Every loader (generator
//!   load, effect splice, freeze/proof harnesses) converges on
//!   `instantiate_def`, so this is the single place a graph gets built from a
//!   def.
//! - [`crate::PresetTypeId`] on clips (`generator_type`) and effect/generator
//!   instances — migrated inside `preset_type_id`'s deserializers
//!   (`deserialize_effect_type`, `deserialize_generator_type`, and the plain
//!   `Deserialize` impl), chained after the existing `remap_legacy_string`
//!   step. That module already had exactly this choke point for one
//!   hardcoded legacy rename (`BasicShapesSnap` → `BasicShapes`); this table
//!   generalizes it rather than adding a second mechanism.

use crate::effect_graph_def::SerializedParamValue;

/// The real rename table. **Empty in every shipped build** — P1 lands the
/// infrastructure only; P2/P3 populate real entries one rename-commit at a
/// time. The one entry below is a fixture id, not a real node/preset — it
/// exists so cross-crate tests (`manifold-core`, `manifold-renderer`,
/// `manifold-io`) can exercise every choke point without depending on a
/// `#[cfg(test)]` item from a dependency, which wouldn't compile in when this
/// crate is built as a normal (non-test) library dependency of another
/// crate's test binary.
pub static TYPE_ID_MIGRATIONS: &[(&str, &str)] = &[
    (
        "__vocab_migration_test_old__",
        "__vocab_migration_test_new__",
    ),
    // --- VOCAB P2 1/8: Color & Tone / Composite (docs/NODE_VOCABULARY_AUDIT.md §4) ---
    ("node.gain", "node.exposure"),
    ("node.color_ramp", "node.gradient_map"),
    ("node.channel_mix", "node.channel_mixer"),
    ("node.clamp_texture", "node.clamp"),
    ("node.hdr_retention_mix", "node.hdr_mix"),
    // --- VOCAB P2 2/8: Blur / Distort / Stylize (docs/NODE_VOCABULARY_AUDIT.md §4) ---
    ("node.blur_3d_separable", "node.blur_3d"),
    ("node.convolution_2d_9tap", "node.custom_convolution"),
    ("node.gaussian_blur_variable_width", "node.variable_blur"),
    ("node.chromatic_displace", "node.rgb_split"),
    ("node.mirror_axis", "node.flip"),
    ("node.mirror_fold_uv", "node.mirror"),
    ("node.radial_fold_uv", "node.kaleidoscope"),
    ("node.uv_strip_clamp", "node.edge_stretch"),
    ("node.affine_transform", "node.transform"),
    // --- VOCAB P2 3/8: Generate / Mask / Noise (docs/NODE_VOCABULARY_AUDIT.md §4) ---
    ("node.gradient_ramp", "node.gradient"),
    ("node.render_filled_rects", "node.draw_rectangles"),
    ("node.render_lines", "node.draw_lines"),
    ("node.render_value_overlay", "node.value_overlay"),
    ("node.box_mask", "node.rectangle_mask"),
    ("node.ellipse_mask", "node.circle_mask"),
    // --- VOCAB P2 4/8: Math & Convert (docs/NODE_VOCABULARY_AUDIT.md §4) ---
    // Note: node.array_math and node.array_feedback keep their ids (label-only
    // changes: Array Math / Array Feedback) - no migration entry needed.
    ("node.abs_texture", "node.absolute_value"),
    ("node.fract_texture", "node.wrap"),
    ("node.power_texture", "node.power"),
    ("node.trig_texture", "node.sine_cosine"),
    ("node.smoothstep_texture", "node.smoothstep"),
    ("node.scale_offset_texture", "node.scale_offset_image"),
    ("node.affine_scalar", "node.scale_offset_value"),
    ("node.array_unpack_vec2", "node.split_xy"),
    ("node.array_connect_nearest", "node.connect_nearest"),
    ("node.pack_curve_xy", "node.combine_xy"),
    ("node.pack_vec4", "node.combine_xyzw"),
    ("node.pack_channels", "node.pack_rgba"),
    ("node.length_vec2", "node.vector_length"),
    ("node.normalize_vec2", "node.normalize"),
    ("node.generate_range", "node.range"),
    ("node.scalar_array_accumulator", "node.sum_into_bins"),
    ("node.resolve_accumulator", "node.resolve_scatter"),
    ("node.resolve_3d_accumulator", "node.resolve_scatter_3d"),
    ("node.texture_dimensions", "node.texture_size"),
    // --- VOCAB P2 5/8: Fields & Coordinates (docs/NODE_VOCABULARY_AUDIT.md §4) ---
    ("node.gradient_central_diff", "node.edge_slope"),
    ("node.gradient_central_diff_3d", "node.edge_slope_3d"),
    ("node.lic_integrate", "node.flow_lines"),
    ("node.rotate_2d", "node.rotate_coordinates"),
    ("node.rotate_vec2_by_angle", "node.rotate_vector"),
    ("node.sin_term", "node.sine_wave"),
    ("node.sample_volume_2d", "node.slice_volume"),
    // --- VOCAB P2 6/8: 3D Geometry / Materials (docs/NODE_VOCABULARY_AUDIT.md §4) ---
    ("node.camera_orbit", "node.orbit_camera"),
    ("node.consecutive_edges", "node.edge_pairs"),
    ("node.displace_mesh", "node.push_mesh"),
    ("node.edges_from_grid_uv", "node.grid_edges"),
    ("node.edges_from_hypercube", "node.hypercube_edges"),
    ("node.hypercube_vertices", "node.hypercube_points"),
    ("node.generate_cube_mesh", "node.cube_mesh"),
    ("node.generate_grid_mesh", "node.grid_mesh"),
    ("node.generate_grid_uv", "node.grid_points"),
    ("node.generate_instance_transforms", "node.arrange_copies"),
    ("node.polytope_edges", "node.platonic_solid_edges"),
    ("node.polytope_vertices", "node.platonic_solid_points"),
    ("node.project_3d", "node.flatten_3d"),
    ("node.project_4d", "node.flatten_4d"),
    ("node.render_3d_mesh", "node.render_mesh"),
    ("node.render_instanced_3d_mesh", "node.render_copies"),
    ("node.triangulate_grid", "node.make_triangles"),
    ("node.array_replicate_polyline_rings", "node.repeat_outline"),
    ("node.bake_equirect_envmap", "node.bake_environment"),
    ("node.blinn_specular", "node.shininess"),
    ("node.fresnel_rim", "node.rim_light"),
    ("node.lambert_directional", "node.basic_light"),
    ("node.heightmap_to_normal", "node.surface_bumps"),
    // --- VOCAB P2 7/8: Particles (2D + 3D) (docs/NODE_VOCABULARY_AUDIT.md §4) ---
    ("node.apply_radial_burst_to_particles", "node.add_burst"),
    ("node.apply_radial_burst_3d_to_particles", "node.add_burst_3d"),
    ("node.array_diffuse_particles", "node.spread_out"),
    ("node.diffuse_force_3d_at_particles", "node.spread_out_3d"),
    ("node.euler_step_particles", "node.move_particles"),
    ("node.euler_step_particles_3d", "node.move_particles_3d"),
    ("node.seed_particles", "node.spawn_particles"),
    ("node.seed_particles_from_texture", "node.spawn_from_image"),
    ("node.scatter_particles", "node.draw_particles"),
    ("node.scatter_particles_3d", "node.draw_particles_3d"),
    ("node.scatter_particles_camera", "node.draw_particles_camera"),
    ("node.simplex_noise_force_at_particles", "node.turbulence"),
    ("node.simplex_noise_force_3d_at_particles", "node.turbulence_3d"),
    ("node.curl_slope_force_3d", "node.swirl_force_3d"),
    ("node.container_bounds_3d", "node.keep_in_box_3d"),
    ("node.container_repel_force_3d", "node.push_from_walls_3d"),
    ("node.radial_burst_force_field", "node.explosion_force"),
    ("node.wrap_particles_torus", "node.wrap_around"),
    ("node.fbm_per_instance", "node.fractal_noise_per_copy"),
    ("node.simplex_per_instance", "node.simplex_noise_per_copy"),
    ("node.instance_position_jitter", "node.position_jitter"),
    ("node.instance_rotation_jitter", "node.rotation_jitter"),
    ("node.lerp_instance_fields", "node.blend_copies"),
    ("node.sample_texture_at_particles", "node.sample_image_at_particles"),
    ("node.sample_texture_3d_at_particles", "node.sample_volume_at_particles"),
    // --- VOCAB P2 8/8: Detection / Routing (docs/NODE_VOCABULARY_AUDIT.md §4) ---
    ("node.blob_detect_ffi", "node.blob_tracker"),
    ("node.blob_overlay_render", "node.blob_overlay"),
    ("node.depth_estimate_midas", "node.depth_map"),
    ("node.optical_flow_estimate", "node.optical_flow"),
    ("node.person_segment", "node.person_mask"),
    ("node.mux_array", "node.switch_array"),
    ("node.mux_scalar", "node.switch_value"),
    ("node.mux_texture", "node.switch_texture"),
];

/// One legacy-fold entry: `(old_id, new_id, seed_params)` — the params to
/// write onto the successor node so it reproduces the retired node's fixed
/// behavior (e.g. `angle = 90` for `node.rotate_vec2_90` folding into
/// `node.rotate_vector`).
pub type ParamSeedMigration = (&'static str, &'static str, &'static [(&'static str, SerializedParamValue)]);

/// Param-seeding table for §7 legacy folds: a retired node (`old_id`) with no
/// direct id-for-id equivalent folds into a parameterized successor
/// (`new_id`), seeding the params a plain rename can't express. Empty until
/// P4 — folding is content work, not infrastructure, and each fold needs its
/// §7 port-parity verification run first.
pub static PARAM_SEED_MIGRATIONS: &[ParamSeedMigration] = &[];

/// Map an old `type_id`/[`crate::PresetTypeId`] string to its current name.
/// Identity for any id not in [`TYPE_ID_MIGRATIONS`] — covers every current
/// id (the overwhelming majority) and any id this table doesn't know about.
pub fn migrate_type_id(id: &str) -> &str {
    TYPE_ID_MIGRATIONS
        .iter()
        .find(|(old, _)| *old == id)
        .map(|(_, new)| *new)
        .unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_id_is_identity() {
        // `node.mix` and `Bloom` are never renamed by the vocabulary audit
        // (docs/NODE_VOCABULARY_AUDIT.md §4/§6) — safe stand-ins for "any
        // current id with no migration entry".
        assert_eq!(migrate_type_id("node.mix"), "node.mix");
        assert_eq!(migrate_type_id("Bloom"), "Bloom");
    }

    #[test]
    fn fixture_entry_migrates() {
        assert_eq!(
            migrate_type_id("__vocab_migration_test_old__"),
            "__vocab_migration_test_new__"
        );
    }

    #[test]
    fn empty_string_is_identity() {
        assert_eq!(migrate_type_id(""), "");
    }
}
