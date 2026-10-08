mod atmosphere;
mod array_replicate_polyline_rings;
mod bend_mesh;
#[cfg(not(any(test, feature = "testkit")))]
mod wave_shear_mesh;
#[cfg(any(test, feature = "testkit"))]
pub mod wave_shear_mesh;
mod transform_mesh_patches;
mod ordered_recon_mesh;
mod mesh_cut_map;
mod mesh_cut_remap;
#[cfg(not(any(test, feature = "testkit")))]
mod remap_mesh_cut;
#[cfg(any(test, feature = "testkit"))]
pub mod remap_mesh_cut;
#[cfg(not(any(test, feature = "testkit")))]
mod remap_cut_weights;
#[cfg(any(test, feature = "testkit"))]
pub mod remap_cut_weights;
#[cfg(not(any(test, feature = "testkit")))]
mod normal_wave_mesh;
#[cfg(any(test, feature = "testkit"))]
pub mod normal_wave_mesh;
#[cfg(not(any(test, feature = "testkit")))]
mod mesh_spatial_mask;
#[cfg(any(test, feature = "testkit"))]
pub mod mesh_spatial_mask;
mod sample_mesh_triangles;
#[cfg(not(any(test, feature = "testkit")))]
mod mesh_stagger_envelope;
#[cfg(any(test, feature = "testkit"))]
pub mod mesh_stagger_envelope;
#[cfg(not(any(test, feature = "testkit")))]
mod analytic_echo_instances;
#[cfg(any(test, feature = "testkit"))]
pub mod analytic_echo_instances;
mod bake_equirect_envmap;
mod pack_curve_xy;
mod consecutive_edges;
#[cfg(not(any(test, feature = "testkit")))]
mod copy_positions;
#[cfg(any(test, feature = "testkit"))]
pub mod copy_positions;
mod cylinder_wrap_field;
mod digital_plants_render;
mod displace_mesh;
#[cfg(not(any(test, feature = "testkit")))]
mod displace_copies;
#[cfg(any(test, feature = "testkit"))]
pub mod displace_copies;
mod fbm_per_instance;
mod edges_from_grid_uv;
mod edges_from_mesh;
mod edges_from_hypercube;
mod facet_normals;
mod fold_mesh;
#[cfg(not(any(test, feature = "testkit")))]
mod generate_cube_mesh;
#[cfg(any(test, feature = "testkit"))]
pub mod generate_cube_mesh;
mod generate_grid_mesh;
mod sample_triangle_grid;
pub mod render_mesh_diagram;
mod generate_grid_uv;
mod generate_instance_transforms;
mod plane_mesh;
mod glitch_jitter;
pub(crate) mod gltf_anim_shared;
#[cfg(not(any(test, feature = "testkit")))]
mod gltf_animation_source;
#[cfg(any(test, feature = "testkit"))]
pub mod gltf_animation_source;
pub(crate) mod gltf_mesh_source;
mod gltf_morph_deltas_source;
mod gltf_morph_weights;
mod gltf_skeleton_pose;
mod gltf_skinned_mesh_source;
pub mod gltf_texture_source;
pub mod hdri_source;
mod hypercube_vertices;
mod instance_position_jitter;
#[cfg(not(any(test, feature = "testkit")))]
mod instance_rotation_jitter;
#[cfg(any(test, feature = "testkit"))]
pub mod instance_rotation_jitter;
pub(crate) mod ocean_spectrum;
pub(crate) mod ocean_displace;
mod projected_grid;
mod cut_out_box;
mod camera_sky;
mod sea_horizon_env;
#[cfg(not(any(test, feature = "testkit")))]
mod lerp_instance_fields;
#[cfg(any(test, feature = "testkit"))]
pub mod lerp_instance_fields;
#[cfg(not(any(test, feature = "testkit")))]
mod light;
#[cfg(any(test, feature = "testkit"))]
pub mod light;
#[cfg(not(any(test, feature = "testkit")))]
mod particles_to_copies;
#[cfg(any(test, feature = "testkit"))]
pub mod particles_to_copies;
#[cfg(not(any(test, feature = "testkit")))]
pub(crate) mod unlit_material;
#[cfg(any(test, feature = "testkit"))]
pub mod unlit_material;
#[cfg(not(any(test, feature = "testkit")))]
mod pbr_material;
#[cfg(any(test, feature = "testkit"))]
pub(crate) mod pbr_material;
#[cfg(not(any(test, feature = "testkit")))]
mod cel_material;
#[cfg(any(test, feature = "testkit"))]
pub mod cel_material;
mod mesh_ramp;
mod melt_mesh;
#[cfg(not(any(test, feature = "testkit")))]
mod morph_mesh;
#[cfg(any(test, feature = "testkit"))]
pub mod morph_mesh;
mod morph_targets_blend;
mod skin_mesh;
mod push_along_normals;
#[cfg(all(test, feature = "gpu-proofs"))]
mod mesh_snapshot;
#[cfg(not(any(test, feature = "testkit")))]
mod nested_cubes_geometry;
#[cfg(any(test, feature = "testkit"))]
pub mod nested_cubes_geometry;
mod noise_displace;
mod polytope_edges;
mod polytope_vertices;
mod project_3d;
mod project_4d;
mod reflect_array;
mod render_3d_mesh;
mod render_instanced_3d_mesh;
mod render_mode;
pub mod render_scene;
mod render_lines;
mod ripple_mesh;
#[cfg(not(any(test, feature = "testkit")))]
mod rotate_3d;
#[cfg(any(test, feature = "testkit"))]
pub mod rotate_3d;
mod rotate_4d;
mod loop_camera;
mod scene_array;
mod scene_fx_default_passthrough;
mod shatter_mesh;
mod slice_mesh;
pub mod scatter_on_mesh;
mod simplex_per_instance;
pub(crate) mod camera_orbit;
mod camera_switch;
mod free_camera;
mod look_at_camera;
mod camera_lens;
mod taper_mesh;
mod torus_wrap_field;
mod triangulate_grid;
mod tube_from_path;
mod twist_mesh;
mod transform_3d;
#[cfg(not(any(test, feature = "testkit")))]
mod smooth_surface_mesh;
#[cfg(any(test, feature = "testkit"))]
pub mod smooth_surface_mesh;
#[cfg(not(any(test, feature = "testkit")))]
mod surface_mesh_normals;
#[cfg(any(test, feature = "testkit"))]
pub mod surface_mesh_normals;
mod transform_shake;
#[cfg(not(any(test, feature = "testkit")))]
mod scene_object;
#[cfg(any(test, feature = "testkit"))]
pub mod scene_object;
mod revolve_curve;
mod extrude_curve;
mod voxelize_mesh;
mod platonic_mesh;
