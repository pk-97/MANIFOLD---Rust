mod array_replicate_polyline_rings;
mod atmosphere;
mod bend_mesh;
manifold_core::testkit_visible! { mod wave_shear_mesh; }
mod mesh_cut_map;
mod mesh_cut_remap;
mod ordered_recon_mesh;
mod transform_mesh_patches;
manifold_core::testkit_visible! { mod remap_mesh_cut; }
manifold_core::testkit_visible! { mod remap_cut_weights; }
manifold_core::testkit_visible! { mod normal_wave_mesh; }
manifold_core::testkit_visible! { mod mesh_spatial_mask; }
mod sample_mesh_triangles;
manifold_core::testkit_visible! { mod mesh_stagger_envelope; }
manifold_core::testkit_visible! { mod analytic_echo_instances; }
mod bake_equirect_envmap;
mod daylight;
mod consecutive_edges;
mod curve_solid_source;
mod mesh_fragment;
mod pack_curve_xy;
mod polygon_curve;
manifold_core::testkit_visible! { mod copy_positions; }
mod cylinder_wrap_field;
mod digital_plants_render;
mod displace_mesh;
manifold_core::testkit_visible! { mod displace_copies; }
mod drop_hidden_triangles;
mod edges_from_grid_uv;
mod edges_from_hypercube;
mod edges_from_mesh;
mod facet_normals;
mod fbm_per_instance;
mod fold_mesh;
mod mesh_texture_weights;
mod smooth_normals;
mod subdivide_mesh;
manifold_core::testkit_visible! { mod generate_cube_mesh; }
#[cfg(test)]
mod builtin_mesh_bounds_tests;
mod generate_grid_mesh;
mod generate_grid_uv;
mod generate_instance_transforms;
mod glitch_jitter;
pub(crate) mod gltf_anim_shared;
mod plane_mesh;
pub mod render_mesh_diagram;
mod sample_triangle_grid;
mod sphere_mesh;
manifold_core::testkit_visible! { mod gltf_animation_source; }
pub(crate) mod gltf_mesh_source;
mod gltf_morph_deltas_source;
mod gltf_morph_weights;
mod gltf_skeleton_pose;
mod gltf_skinned_mesh_source;
pub mod gltf_texture_source;
pub mod hdri_source;
mod hypercube_vertices;
mod instance_position_jitter;
pub(crate) mod mesh_lods;
pub(crate) mod mesh_clusters;
manifold_core::testkit_visible! { mod instance_rotation_jitter; }
mod camera_sky;
mod cut_out_box;
pub(crate) mod ocean_displace;
pub(crate) mod ocean_spectrum;
mod projected_grid;
mod sea_horizon_env;
manifold_core::testkit_visible! { mod lerp_instance_fields; }
manifold_core::testkit_visible! { mod light; }
manifold_core::testkit_visible! { mod particles_to_copies; }
manifold_core::testkit_visible! { pub(crate) mod unlit_material; }
manifold_core::testkit_visible! {
    testkit { pub(crate) mod pbr_material; }
    production { mod pbr_material; }
}
manifold_core::testkit_visible! { mod cel_material; }
mod melt_mesh;
mod mesh_ramp;
manifold_core::testkit_visible! { mod morph_mesh; }
#[cfg(all(test, feature = "gpu-proofs"))]
mod mesh_snapshot;
mod morph_targets_blend;
mod push_along_normals;
mod skin_mesh;
manifold_core::testkit_visible! { mod nested_cubes_geometry; }
mod noise_displace;
mod polytope_edges;
mod polytope_vertices;
mod project_3d;
mod project_4d;
mod reflect_array;
mod render_3d_mesh;
mod render_instanced_3d_mesh;
mod render_lines;
mod render_mode;
pub mod render_scene;
mod ripple_mesh;
manifold_core::testkit_visible! { mod rotate_3d; }
pub mod auto_focus;
mod camera_lens;
pub(crate) mod camera_orbit;
pub(crate) mod camera_switch;
mod free_camera;
mod grid_indices;
mod look_at_camera;
mod loop_camera;
pub mod loop_cell;
pub mod merge_meshes;
pub mod nest_copies;
mod rotate_4d;
pub mod scatter_on_mesh;
mod scene_array;
mod scene_fx_default_passthrough;
mod shatter_mesh;
mod simplex_per_instance;
mod slice_mesh;
mod taper_mesh;
mod terrain;
mod torus_wrap_field;
mod transform_3d;
mod transform_shake;
mod tree;
mod triangulate_grid;
mod tube_from_path;
mod twist_mesh;
manifold_core::testkit_visible! { mod scene_object; }
mod extrude_curve;
mod platonic_mesh;
mod revolve_curve;
mod voxelize_mesh;
