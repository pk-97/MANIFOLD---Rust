mod atmosphere;
mod array_replicate_polyline_rings;
mod bend_mesh;
manifold_core::testkit_visible! { mod wave_shear_mesh; }
mod transform_mesh_patches;
mod ordered_recon_mesh;
mod mesh_cut_map;
mod mesh_cut_remap;
manifold_core::testkit_visible! { mod remap_mesh_cut; }
manifold_core::testkit_visible! { mod remap_cut_weights; }
manifold_core::testkit_visible! { mod normal_wave_mesh; }
manifold_core::testkit_visible! { mod mesh_spatial_mask; }
mod sample_mesh_triangles;
manifold_core::testkit_visible! { mod mesh_stagger_envelope; }
manifold_core::testkit_visible! { mod analytic_echo_instances; }
mod bake_equirect_envmap;
mod pack_curve_xy;
mod consecutive_edges;
manifold_core::testkit_visible! { mod copy_positions; }
mod cylinder_wrap_field;
mod digital_plants_render;
mod displace_mesh;
manifold_core::testkit_visible! { mod displace_copies; }
mod fbm_per_instance;
mod edges_from_grid_uv;
mod edges_from_mesh;
mod edges_from_hypercube;
mod facet_normals;
mod fold_mesh;
manifold_core::testkit_visible! { mod generate_cube_mesh; }
mod generate_grid_mesh;
mod sample_triangle_grid;
pub mod render_mesh_diagram;
mod generate_grid_uv;
mod generate_instance_transforms;
mod plane_mesh;
mod glitch_jitter;
pub(crate) mod gltf_anim_shared;
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
manifold_core::testkit_visible! { mod instance_rotation_jitter; }
pub(crate) mod ocean_spectrum;
pub(crate) mod ocean_displace;
mod projected_grid;
mod cut_out_box;
mod camera_sky;
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
mod mesh_ramp;
mod melt_mesh;
manifold_core::testkit_visible! { mod morph_mesh; }
mod morph_targets_blend;
mod skin_mesh;
mod push_along_normals;
#[cfg(all(test, feature = "gpu-proofs"))]
mod mesh_snapshot;
manifold_core::testkit_visible! { mod nested_cubes_geometry; }
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
manifold_core::testkit_visible! { mod rotate_3d; }
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
manifold_core::testkit_visible! { mod smooth_surface_mesh; }
manifold_core::testkit_visible! { mod surface_mesh_normals; }
mod transform_shake;
manifold_core::testkit_visible! { mod scene_object; }
mod revolve_curve;
mod extrude_curve;
mod voxelize_mesh;
mod platonic_mesh;
