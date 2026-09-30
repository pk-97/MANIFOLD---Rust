//! Node implementations for the catalog defined in `docs/NODE_CATALOG.md`.
//!
//! This module hosts both atoms (small generic composable building blocks
//! like Mix, Feedback, Gaussian Blur) and the wrapped legacy effects
//! (Bloom, Watercolor, Halation, etc.) — all as `EffectNode` impls behind
//! flat `node.*` type IDs. The atom/effect split is presentation metadata,
//! not a structural divide.

mod abs_texture;
pub(crate) mod fluid_surface;
mod glyph_atlas;
mod render_glyph_grid;
pub(crate) mod terminal_analysis;
mod terminal_detail;
mod terminal_reaction;
mod terminal_stream;
mod terminal_vocabulary;
mod audio_waveform;
mod affine_transform;
mod atmosphere;
mod anti_clump_particles;
mod apply_radial_burst_to_particles;
mod array_connect_nearest;
mod array_diffuse_particles;
mod array_filter_detections;
mod array_feedback;
mod array_math;
mod array_replicate_polyline_rings;
mod array_unpack_vec2;
mod beat_gate;
mod beat_ramp;
mod bend_mesh;
mod wave_shear_mesh;
mod transform_mesh_patches;
mod ordered_recon_mesh;
mod mesh_cut_map;
mod mesh_cut_remap;
mod remap_mesh_cut;
mod remap_cut_weights;
mod normal_wave_mesh;
mod mesh_spatial_mask;
mod sample_mesh_triangles;
mod mesh_stagger_envelope;
mod analytic_echo_instances;
mod bilateral_blur;
mod blob_detect_ffi;
mod blob_overlay_render;
mod block_displace_field;
mod block_sample;
mod bokeh_gather;
pub use bokeh_gather::BokehGather;
mod box_mask;
mod blur_3d_separable;
mod blinn_specular;
mod chroma_key;
mod checkerboard;
mod chromatic_displace;
mod bake_equirect_envmap;
mod basic_shape;
mod clamp_texture;
mod mirror_axis;
mod pack_channels;
mod pack_curve_xy;
mod clip_trigger_cycle;
mod clip_trigger_index;
mod coc_dilate;
mod coc_from_depth;
mod color;
mod color_sample;
mod colorize;
mod compose;
mod compressor_envelope;
mod consecutive_edges;
mod contrast;
mod copy_positions;
mod convolution_2d_9tap;
mod cycle_table_row;
mod cylinder_wrap_field;
mod depth_estimate_midas;
mod detect_regions;
pub use detect_regions::{RegionPerfSamples, start_region_perf_samples, take_region_perf_samples};
mod region_types;
mod track_regions;
mod mask_extrema;
mod region_mask;
mod rgb_distance;
mod digital_plants_render;
mod displace_mesh;
mod distance_to_point;
mod dither;
mod dither_pattern;
mod displace_copies;
mod downsample;
mod resize_limit;
mod draw_connections;
mod draw_dots;
mod draw_gauge;
mod draw_markers;
mod draw_scanlines;
mod draw_ticks;
mod edge_detect;
mod envelope_decay;
mod envelope_beats;
pub(crate) use envelope_beats::{BeatEnvelopeState, BeatEnvelopeDurations};
mod envelope_follower_ar;
mod fbm_per_instance;
mod field_combine;
mod vector_fields;
mod film_grain;
mod filter;
mod flash;
mod flow_field_noise;
mod fract_texture;
mod fresnel_rim;
mod frequency_ratio;
mod gradient_central_diff_3d;
mod curl_slope_force_3d;
mod sample_texture_3d_at_particles;
mod simplex_noise_force_3d_at_particles;
mod diffuse_force_3d_at_particles;
mod container_repel_force_3d;
mod euler_step_particles_3d;
mod container_bounds_3d;
mod flatten_to_camera_plane;
mod apply_radial_burst_3d_to_particles;
mod scatter_particles_camera;
mod gain;
pub mod gaussian_blur_variable_width;
mod edges_from_grid_uv;
mod edges_from_mesh;
mod edges_from_hypercube;
mod ellipse_mask;
mod facet_normals;
mod fold_mesh;
mod generate_cube_mesh;
mod generate_grid_mesh;
mod sample_triangle_grid;
mod render_mesh_diagram;
mod generate_grid_uv;
mod generate_instance_transforms;
mod plane_mesh;
mod generate_range;
mod glitch_jitter;
pub(crate) mod gltf_anim_shared;
mod gltf_animation_source;
pub(crate) mod gltf_mesh_source;
mod gltf_morph_deltas_source;
mod gltf_morph_weights;
mod gltf_skeleton_pose;
mod gltf_skinned_mesh_source;
mod gltf_texture_source;
mod pack_vec4;
mod gradient_central_diff;
mod gradient_ramp;
mod grid_uv_field;
mod hash_field_by_seed;
mod hdr_retention_mix;
pub(crate) mod hdri_source;
mod heightfield_shadow;
mod heightmap_to_normal;
mod hue_saturation;
mod hypercube_vertices;
mod image_folder;
mod instance_position_jitter;
mod instance_rotation_jitter;
mod inject_burst;
mod euler_step_particles;
mod sample_texture_at_particles;
mod wrap_particles_torus;
mod wave_field_3d;
mod invert;
mod lambert_directional;
mod length_vec2;
mod lerp_instance_fields;
mod levels;
mod lfo;
pub(crate) mod layer_source;
mod lic_integrate;
mod light;
mod lightning_bolt;
mod linear_gradient;
mod liquid_solid_distance;
mod luminance;
mod magnitude_db;
mod lut1d;
mod masked_mix;
mod matcap_two_tone;
mod math;
mod grid_to_matter;
mod matter_body_reaction;
mod matter_common;
mod matter_domain;
mod matter_fill;
mod matter_frame;
mod matter_grid_update;
mod matter_move_bodies;
mod matter_state;
mod matter_stats;
mod matter_to_grid;
mod particles_to_copies;
mod zero_array;
mod unlit_material;
mod pbr_material;
mod cel_material;
pub mod multi_blend;
mod mesh_ramp;
mod melt_mesh;
mod morph_mesh;
mod morph_targets_blend;
mod skin_mesh;
mod motion_blur;
mod push_along_normals;
#[cfg(all(test, feature = "gpu-proofs"))]
mod mesh_snapshot;
mod mux_array;
mod mux_scalar;
mod mux_texture;
mod neighbor_smooth;
mod nested_cubes_geometry;
mod normalize_vec2;
mod one_euro_filter;
mod optical_flow_estimate;
mod peak;
mod noise;
mod noise_displace;
mod person_segment;
mod polar_field;
mod polytope_edges;
mod polytope_vertices;
mod posterize;
mod power_texture;
mod project_3d;
mod project_4d;
mod mirror_fold_uv;
mod note_rates;
mod radial_burst_force_field;
mod radial_fold_uv;
mod radial_offset_field;
mod uv_strip_clamp;
mod reinhard_tone_map;
mod remap;
mod reflect_array;
mod remove_drift_3d;
mod render_3d_mesh;
mod render_instanced_3d_mesh;
mod render_mode;
pub(crate) mod render_scene;
#[cfg(feature = "gpu-proofs")]
pub use render_scene::rt_proof::{RtProbeObject, RtProbeScene};
mod render_filled_rects;
mod render_lines;
mod render_text;
mod render_value_overlay;
mod ripple_mesh;
mod resolve_3d_accumulator;
mod resolve_accumulator;
mod rotate_3d;
mod rotate_4d;
mod rotate_vec2_by_angle;
mod sample_and_hold;
mod sample_volume_2d;
mod saturation;
mod scalar_array_accumulator;
mod scale_offset_texture;
mod scanline_jitter_field;
mod scatter_particles;
mod scatter_particles_3d;
mod loop_camera;
mod scene_array;
mod scene_fx_default_passthrough;
mod seed_particles_from_texture;
mod seed_particles;
mod shatter_mesh;
mod separable_gaussian;
mod set_alpha;
mod sharpen;
mod ssao_gtao;
mod simplex_field_2d;
mod slice_mesh;
mod simplex_noise_force_at_particles;
mod spawn_from_mesh;
pub mod standalone_pipeline;
mod scatter_on_mesh;
mod simplex_per_instance;
mod affine_scalar;
mod camera_orbit;
mod camera_switch;
mod free_camera;
mod look_at_camera;
mod camera_lens;
mod canvas_area_scale;
mod centered_uv;
mod rotate_2d;
mod sin_term;
mod slope_displace;
mod texture_sum_5;
mod trig_texture;
mod smoothing;
mod smoothstep_texture;
mod track_persist;
mod temporal;
mod texture_advect;
mod texture_dimensions;
mod taper_mesh;
mod tone_map;
mod torus_wrap_field;
mod triangulate_grid;
mod tube_from_path;
// D7/P0 I6 test fixture only (docs/CINEMATIC_POST_DESIGN.md), never registered
// outside test builds. Its only user is `freeze::proof`'s GPU I6 test, hence
// the gpu-proofs gate. `pub(crate)` so that test can construct it directly (it
// is deliberately NOT in the global inventory-backed registry — see the
// module doc comment).
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) mod test_camera_pointwise_fixture;
// BUG-agfh (Codegen: buffer atom with several outputs, one atomic) test fixture,
// kept out of the global registry the same way. Not gpu-proofs-gated: its
// codegen tests run without a device.
#[cfg(test)]
pub(crate) mod test_multi_output_atomic_fixture;
mod twist_mesh;
mod trigger_ease_to;
mod trigger_gate;
mod transform_3d;
mod transform_components;
pub(crate) mod prefix_scan;
mod sort_particles_into_cells;
mod running_total;
mod shape_particle_blobs;
mod particle_volume;
mod smooth_lattice;
mod cosine_reorder;
mod cosine_spectrum;
mod cosine_half_spectrum;
mod cosine_poisson_divide;
mod cosine_surface_scale;
mod fft_3d;
mod dot_products;
mod combine_rows;
mod divide_by_value;
mod krylov_givens;
mod krylov_solve;
mod krylov_basis;
mod collar_cells;
mod select_flagged;
mod chart_entries;
mod chart_sums;
mod chart_spread;
mod collar_source;
mod collar_gather;
mod collar_pressure;
mod liquid_fill;
mod liquid_feedback;
mod cells_with_particles;
mod particles_to_faces;
mod face_gravity;
mod face_divergence;
mod density_source;
mod subtract_pressure;
mod extend_faces;
mod faces_to_particles;
mod face_sample_component;
mod matter_face_component;
#[cfg(test)]
mod face_grid_extent_tests;
#[cfg(test)]
mod face_grid_scenes;
#[cfg(all(test, feature = "gpu-proofs"))]
mod face_grid_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod face_grid_scene_tests;
#[cfg(test)]
mod swash_preset;
#[cfg(test)]
mod swash_extent_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod swash_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod swash_solve_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod swash_step_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
mod swash_scene_tests;
#[cfg(all(test, feature = "gpu-proofs"))]
pub(crate) mod swash_volume;
#[cfg(all(test, feature = "water-race-probes"))]
pub(crate) mod swash_race_tests;
#[cfg(all(test, feature = "water-race-probes"))]
pub(crate) mod swash_still;
#[cfg(all(test, feature = "water-race-probes"))]
mod swash_render_smoke_tests;
mod count_surface_triangles;
mod volume_surface_mesh;
#[cfg(all(test, feature = "gpu-proofs"))]
mod liquid_surface_tests;
#[cfg(test)]
mod matter_extent_tests;
mod transform_shake;
mod scene_object;
mod revolve_curve;
mod extrude_curve;
mod uv_displace_by_flow;
mod uv_field;
mod value;
mod compose_vec3;
mod vignette;
mod voronoi_2d;
mod voxelize_mesh;
// Crate-visible so the snapshot builder can key the `(WGSL)` header marker on
// the canonical `TYPE_ID` rather than a duplicated string literal.
pub(crate) mod wgsl_compute;
pub mod watercolor;
mod wet_dry_mix;

pub use abs_texture::AbsTexture;
pub use audio_waveform::{AudioSpectrum, AudioWaveform};
pub use affine_transform::AffineTransform;
pub use atmosphere::AtmosphereNode;
pub use render_mode::RenderModeNode;
pub use anti_clump_particles::AntiClumpParticles;
pub use apply_radial_burst_to_particles::ApplyRadialBurstToParticles;
pub use array_connect_nearest::ArrayConnectNearest;
pub use array_diffuse_particles::ArrayDiffuseParticles;
pub use array_filter_detections::ArrayFilterDetections;
pub use array_feedback::ArrayFeedback;
pub use array_math::{ARRAY_MATH_OPS, ArrayMath};
pub use array_replicate_polyline_rings::{
    ArrayReplicatePolylineRings, REPLICATE_MAX_RINGS,
};
pub use array_unpack_vec2::ArrayUnpackVec2;
pub use beat_gate::{BEAT_GATE_RATE_LABELS, BeatGate};
pub use beat_ramp::BeatRamp;
pub use blob_detect_ffi::BlobDetectFfi;
pub use detect_regions::DetectRegions;
pub use track_regions::TrackRegions;
pub use mask_extrema::MaskExtrema;
pub use region_mask::RegionMask;
pub use rgb_distance::RgbDistance;
pub use blob_overlay_render::BlobOverlayRender;
pub use block_displace_field::BlockDisplaceField;
pub use block_sample::BlockSample;
pub use box_mask::BoxMask;
pub use blinn_specular::BlinnSpecular;
pub use blur_3d_separable::{BLUR_3D_AXES, BLUR_3D_MODES, Blur3DSeparable};
pub use chroma_key::{CHROMA_KEY_MODES, ChromaKey};
pub use checkerboard::Checkerboard;
pub use chromatic_displace::ChromaticDisplace;
pub use bake_equirect_envmap::BakeEquirectEnvmap;
pub use basic_shape::{BASIC_SHAPE_SHAPES, BasicShape};
pub use clamp_texture::ClampTexture;
pub use coc_from_depth::CocFromDepth;
pub use mirror_axis::MirrorAxis;
pub use pack_channels::PackChannels;
pub use pack_curve_xy::PackCurveXy;
pub use color::{
    BRIGHTNESS_TYPE_ID, Brightness, CHANNEL_MIX_TYPE_ID, COLOR_RAMP_TYPE_ID, ChannelMix, ColorRamp,
};
pub use color_sample::ColorSample;
pub use colorize::Colorize;
pub use compose::{MIX_MODES, MIX_TYPE_ID, Mix};
pub use contrast::Contrast;
pub use copy_positions::CopyPositions;
pub use wave_shear_mesh::WaveShearMesh;
pub use transform_mesh_patches::TransformMeshPatches;
pub use ordered_recon_mesh::OrderedReconMesh;
pub use mesh_cut_map::{CutMeshBands, CutMeshCells};
pub(crate) use mesh_cut_map::scratch_bytes as cut_map_scratch_bytes;
pub use remap_mesh_cut::RemapMeshCut;
pub use remap_cut_weights::RemapCutWeights;
pub use morph_mesh::MorphMesh;
pub use normal_wave_mesh::NormalWaveMesh;
pub use mesh_spatial_mask::MeshSpatialMask;
pub use mesh_stagger_envelope::MeshStaggerEnvelope;
pub use analytic_echo_instances::AnalyticEchoInstances;
pub use compressor_envelope::{COMPRESSOR_ENVELOPE_TYPE_ID, CompressorEnvelope};
pub use consecutive_edges::{CONSECUTIVE_EDGES_MAX_CAPACITY, ConsecutiveEdges};
pub use convolution_2d_9tap::Convolution2D9Tap;
pub use cycle_table_row::CycleTableRow;
pub use cylinder_wrap_field::CylinderWrapField;
pub use depth_estimate_midas::DepthEstimateMidas;
pub use digital_plants_render::DigitalPlantsRender;
pub use displace_mesh::DisplaceMesh;
pub use displace_copies::DisplaceCopies;
pub use draw_connections::DrawConnections;
pub use draw_dots::DrawDots;
pub use draw_gauge::DrawGauge;
pub use draw_markers::DrawMarkers;
pub use draw_scanlines::DrawScanlines;
pub use draw_ticks::DrawTicks;
pub use distance_to_point::DistanceToPoint;
pub use dither::Dither;
pub use dither_pattern::DitherPattern;
pub use edge_detect::EdgeDetect;
pub use envelope_decay::{ENVELOPE_DECAY_TYPE_ID, EnvelopeDecay};
pub use envelope_follower_ar::{ENVELOPE_FOLLOWER_AR_TYPE_ID, EnvelopeFollowerAr};
pub use fbm_per_instance::FbmPerInstance;
pub use field_combine::FieldCombine;
pub use vector_fields::{
    AddVectorFields, MultiplyVectorFields, RadialVectorField, ScaleVectorField,
    UniformVectorField, VortexVectorField,
};
pub use film_grain::FilmGrain;
pub use filter::{BLUR_MODES, BLUR_TYPE_ID, Blur, THRESHOLD_TYPE_ID, Threshold};
pub use flash::{FLASH_MODES, Flash};
pub use flow_field_noise::FlowFieldNoise;
pub use fract_texture::FractTexture;
pub use fresnel_rim::FresnelRim;
pub use frequency_ratio::{FREQUENCY_RATIO_TABLE, FrequencyRatio};
pub use gradient_central_diff_3d::GradientCentralDiff3D;
pub use curl_slope_force_3d::CurlSlopeForce3D;
pub use sample_texture_3d_at_particles::SampleTexture3DAtParticles;
pub use simplex_noise_force_3d_at_particles::SimplexNoiseForce3DAtParticles;
pub use diffuse_force_3d_at_particles::DiffuseForce3DAtParticles;
pub use container_repel_force_3d::{CONTAINER_3D_MODES, ContainerRepelForce3D};
pub use euler_step_particles_3d::EulerStepParticles3D;
pub use container_bounds_3d::ContainerBounds3D;
pub use flatten_to_camera_plane::FlattenToCameraPlane;
pub use apply_radial_burst_3d_to_particles::ApplyRadialBurst3DToParticles;
pub use scatter_particles_camera::{SCATTER_CAMERA_MODES, ScatterParticlesCamera};
pub use gain::Gain;
pub use gaussian_blur_variable_width::{BLUR_VARIABLE_AXES, GaussianBlurVariableWidth};
pub use edges_from_grid_uv::EdgesFromGridUv;
pub use edges_from_mesh::EdgesFromMesh;
pub use edges_from_hypercube::EdgesFromHypercube;
pub use ellipse_mask::EllipseMask;
pub use fold_mesh::FoldMesh;
pub use generate_cube_mesh::{CUBE_VERTEX_COUNT, GenerateCubeMesh};
pub use generate_grid_mesh::GenerateGridMesh;
pub use sample_triangle_grid::{SampleTriangleGrid, SAMPLE_TRIANGLE_GRID_CAPACITY};
pub use render_mesh_diagram::RenderMeshDiagram;
pub use generate_grid_uv::{
    GRID_UV_DEFAULT_SIZE, GRID_UV_MAX_SIZE, GenerateGridUv,
};
pub use generate_instance_transforms::{
    GenerateInstanceTransforms, INSTANCE_LAYOUTS,
};
pub use generate_range::GenerateRange;
pub use glitch_jitter::GlitchJitter;
pub use gltf_animation_source::GltfAnimationSource;
pub use gltf_mesh_source::GltfMeshSource;
pub use gltf_skeleton_pose::GltfSkeletonPose;
pub use gltf_skinned_mesh_source::GltfSkinnedMeshSource;
pub use gltf_texture_source::GltfTextureSource;
pub use pack_vec4::PackVec4;
pub use gradient_central_diff::{GRADIENT_CHANNELS, GradientCentralDiff};
pub use gradient_ramp::GradientRamp;
pub use grid_uv_field::GridUvField;
pub use hash_field_by_seed::{HASH_FIELD_MODES, HashFieldBySeed};
pub use hdri_source::HdriSource;
pub use heightmap_to_normal::HeightmapToNormal;
pub use image_folder::ImageFolder;
pub use instance_position_jitter::InstancePositionJitter;
pub use instance_rotation_jitter::InstanceRotationJitter;
pub use inject_burst::{INJECT_BURST_TYPE_ID, InjectBurst};
pub use euler_step_particles::EulerStepParticles;
pub use sample_texture_at_particles::SampleTextureAtParticles;
pub use wrap_particles_torus::WrapParticlesTorus;
pub use wave_field_3d::WaveField3d;
pub use hue_saturation::HueSaturation;
pub use hypercube_vertices::HypercubeVertices;
pub use invert::Invert;
pub use lambert_directional::LambertDirectional;
pub use length_vec2::LengthVec2;
pub use lerp_instance_fields::LerpInstanceFields;
pub use levels::Levels;
pub use lfo::{LFO_RATE_LABELS, LFO_SHAPES, Lfo};
pub use layer_source::LayerSource;
pub use lic_integrate::LicIntegrate;
pub use light::LightNode;
pub use linear_gradient::LinearGradient;
pub use liquid_solid_distance::LiquidSolidDistance;
pub use loop_camera::{LOOP_CAMERA_AXIS_LABELS, LoopCamera};
pub use luminance::Luminance;
pub use magnitude_db::MagnitudeDb;
pub use lut1d::ColorLut;
pub use math::{MATH_OPS, Math};
pub use masked_mix::MaskedMix;
pub use matcap_two_tone::MatcapTwoTone;
pub use grid_to_matter::GridToMatter;
pub use matter_body_reaction::MatterBodyReaction;
pub use matter_domain::MatterDomain;
pub use matter_fill::MatterFill;
pub use matter_frame::MatterFrame;
pub use matter_grid_update::MatterGridUpdate;
pub use matter_move_bodies::MatterMoveBodies;
pub use matter_state::{MATTER_STATE_PORTS, MatterState};
pub use matter_stats::MatterStats;
pub use matter_to_grid::MatterToGrid;
pub use particles_to_copies::ParticlesToCopies;
pub use zero_array::ZeroArray;
pub use melt_mesh::MeltMesh;
pub use unlit_material::UnlitMaterial;
pub use pbr_material::PbrMaterial;
pub use cel_material::CelMaterial;
pub use multi_blend::MultiBlend;
pub use mux_array::MuxArray;
pub use mux_scalar::MuxScalar;
pub use mux_texture::MuxTexture;
pub use neighbor_smooth::NeighborSmooth;
pub use nested_cubes_geometry::{NESTED_CUBES_INSTANCE_COUNT, NestedCubesGeometry};
pub use noise_displace::NoiseDisplace;
pub use normalize_vec2::NormalizeVec2;
pub use one_euro_filter::OneEuroFilter;
pub use optical_flow_estimate::OpticalFlowEstimate;
pub use resize_limit::ResizeLimit;
pub use peak::Peak;
pub use plane_mesh::{PLANE_VERTEX_COUNT, GeneratePlaneMesh};
pub use noise::Noise;
pub use person_segment::PersonSegment;
pub use polar_field::PolarField;
pub use polytope_edges::PolytopeEdges;
pub use polytope_vertices::PolytopeVertices;
pub use posterize::Posterize;
pub use power_texture::PowerTexture;
pub use project_3d::{PROJECT_3D_MODES, Project3D};
pub use project_4d::Project4D;
pub use mirror_fold_uv::{MIRROR_FOLD_MODES, MirrorFoldUv};
pub use note_rates::{NOTE_RATE_LABELS, NOTE_RATE_VALUES};
pub use radial_burst_force_field::RadialBurstForceField;
pub use radial_fold_uv::RadialFoldUv;
pub use radial_offset_field::RadialOffsetField;
pub use uv_strip_clamp::{UV_STRIP_CLAMP_MODES, UvStripClamp};
pub use reinhard_tone_map::ReinhardToneMap;
pub use remap::{REMAP_WRAP_MODES, Remap};
pub use reflect_array::ReflectArray;
pub use remove_drift_3d::RemoveDrift3D;
pub use render_3d_mesh::Render3DMesh;
pub use render_instanced_3d_mesh::RenderInstanced3DMesh;
pub use render_scene::RenderScene;
pub use render_scene::{RtCaptureSlot, RT_CAPTURE_ARM, RT_CAPTURE_ARM_COMPOSITE, RT_CAPTURE_QUEUE};// ── RT washout probe re-exports (temporary) ──
pub use render_filled_rects::RenderFilledRects;
pub use render_lines::RenderLines;
pub use ripple_mesh::RippleMesh;
pub use render_text::RenderText;
pub use render_value_overlay::RenderValueOverlay;
pub use resolve_3d_accumulator::Resolve3DAccumulator;
pub use resolve_accumulator::ResolveAccumulator;
pub use rotate_3d::Rotate3D;
pub use rotate_4d::Rotate4D;
pub use rotate_vec2_by_angle::RotateVec2ByAngle;
pub use clip_trigger_cycle::ClipTriggerCycleNode;
pub use sample_and_hold::{SAMPLE_AND_HOLD_TYPE_ID, SampleAndHold};
pub use sample_volume_2d::SampleVolume2D;
pub use saturation::Saturation;
pub use scalar_array_accumulator::ScalarArrayAccumulator;
pub use scale_offset_texture::ScaleOffsetTexture;
pub use scanline_jitter_field::ScanlineJitterField;
pub use scatter_on_mesh::ScatterOnMesh;
pub use scatter_particles::ScatterParticles;
pub use scatter_particles_3d::ScatterParticles3D;
pub use seed_particles_from_texture::SeedParticlesFromTexture;
pub use seed_particles::SeedParticles;
pub use separable_gaussian::{
    GAUSSIAN_BLUR_AXES, GAUSSIAN_BLUR_KERNELS, GAUSSIAN_BLUR_TYPE_ID, GaussianBlur,
};
pub use sharpen::Sharpen;
pub use simplex_field_2d::{SIMPLEX_FIELD_OUTPUT_CHANNELS, SimplexField2D};
pub use simplex_noise_force_at_particles::SimplexNoiseForceAtParticles;
pub use simplex_per_instance::SimplexPerInstance;
pub use slice_mesh::SliceMesh;
pub use affine_scalar::AffineScalar;
pub use camera_orbit::{CameraOrbit, DEFAULT_FAR, DEFAULT_NEAR};
pub use free_camera::FreeCamera;
pub use look_at_camera::LookAtCamera;
pub use camera_lens::CameraLens;
pub use canvas_area_scale::CanvasAreaScale;
pub use centered_uv::CenteredUv;
pub use rotate_2d::Rotate2D;
pub use sin_term::SinTerm;
pub use slope_displace::SlopeDisplace;
pub use texture_sum_5::TextureSum5;
pub use trig_texture::{TRIG_MODES, TrigTexture};
pub use smoothing::{SMOOTHING_TYPE_ID, Smoothing};
pub use smoothstep_texture::SmoothstepTexture;
pub use temporal::{FEEDBACK_TYPE_ID, Feedback};
pub use texture_advect::{TEXTURE_ADVECT_BOUNDARIES, TextureAdvect};
pub use texture_dimensions::TextureDimensions;
pub use tone_map::{TONE_MAP_CURVES, TONE_MAP_MODES, ToneMap};
pub use torus_wrap_field::TorusWrapField;
pub use triangulate_grid::TriangulateGrid;
pub use trigger_ease_to::{TRIGGER_EASE_TO_TYPE_ID, TriggerEaseTo};
pub use track_persist::TrackPersist;
pub use trigger_gate::TriggerGate;
pub use transform_3d::Transform3D;
pub use transform_shake::TransformShake;
pub use scene_array::SceneArray;
pub use scene_object::SceneObjectNode;
pub use shatter_mesh::ShatterMesh;
pub use uv_displace_by_flow::UvDisplaceByFlow;
pub use uv_field::UvField;
pub use value::Value;
pub use vignette::{VIGNETTE_SHAPES, Vignette};
pub use voronoi_2d::Voronoi2D;
pub use voxelize_mesh::VoxelizeMesh;
pub use wgsl_compute::{DEFAULT_WGSL as DEFAULT_WGSL_COMPUTE, WgslCompute};
pub use watercolor::{WATERCOLOR_TYPE_ID, Watercolor};
pub use wet_dry_mix::{WET_DRY_TYPE_ID, WetDry};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use manifold_core::{Beats, Seconds};

    use crate::node_graph::{
        EffectNode, Executor, FinalOutput, FrameTime, Graph, ParamType, ParamValue, Source,
        compile, validate,
    };

    fn frame_time() -> FrameTime {
        FrameTime {
            beats: Beats(0.0),
            seconds: Seconds(0.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    /// One boxed instance per registered factory, paired with the id the
    /// factory registered under. Covers every primitive, present and future.
    fn registered_nodes() -> Vec<(&'static str, Box<dyn EffectNode>)> {
        inventory::iter::<crate::node_graph::persistence::PrimitiveFactory>
            .into_iter()
            .map(|f| (f.type_id, (f.create)()))
            .collect()
    }

    fn assert_no_violations(rule: &str, violations: &[String]) {
        assert!(violations.is_empty(), "{rule}:\n  {}", violations.join("\n  "));
    }

    /// A saved graph finds its node by the registered id, so the node a
    /// factory builds must report that id, or, for a legacy alias, another
    /// registered id. Every id is unique and namespaced, and every atom has
    /// an output. Param defaults match their declared type (an Int
    /// stores its default as a Float and reads back through `as_scalar`, the
    /// path every reader funnels through), and an Enum default indexes a
    /// real option.
    #[test]
    fn every_registered_node_is_well_formed() {
        let nodes = registered_nodes();
        let registered: HashSet<&str> = nodes.iter().map(|(id, _)| *id).collect();
        let mut seen = HashSet::new();
        let mut violations = Vec::new();
        for (type_id, node) in nodes {
            let reported = node.type_id().as_str();
            if reported != type_id && !registered.contains(reported) {
                violations.push(format!("{type_id}: factory builds an unregistered `{reported}`"));
            }
            if !seen.insert(type_id) {
                violations.push(format!("{type_id}: registered twice"));
            }
            if !type_id.starts_with("node.") && !type_id.starts_with("system.") {
                violations.push(format!("{type_id}: id lacks the `node.` or `system.` prefix"));
            }
            // `system.*` sinks end the graph; `node.__*` are test fixtures.
            let is_atom = type_id.starts_with("node.") && !type_id.starts_with("node.__");
            if is_atom && node.outputs().is_empty() {
                violations.push(format!("{type_id}: declares no outputs"));
            }
            for def in node.parameters() {
                let name = &def.name;
                let type_ok = matches!(
                    (def.ty, &def.default),
                    (ParamType::Float | ParamType::Angle | ParamType::Frequency, ParamValue::Float(_))
                        | (ParamType::Int | ParamType::Trigger, ParamValue::Float(_))
                        | (ParamType::Bool, ParamValue::Bool(_))
                        | (ParamType::Vec2, ParamValue::Vec2(_))
                        | (ParamType::Vec3, ParamValue::Vec3(_))
                        | (ParamType::Vec4, ParamValue::Vec4(_))
                        | (ParamType::Color, ParamValue::Color(_))
                        | (ParamType::Enum, ParamValue::Enum(_))
                        // Tables and Strings can't live in a const default, so
                        // a Float placeholder stands in until the preset
                        // overrides it.
                        | (ParamType::String, ParamValue::Float(_) | ParamValue::String(_))
                        | (ParamType::Table, ParamValue::Float(_) | ParamValue::Table(_))
                );
                if !type_ok {
                    violations.push(format!(
                        "{type_id} param `{name}`: default {:?} does not match declared type {:?}",
                        def.default, def.ty
                    ));
                    continue;
                }
                if def.ty == ParamType::Int
                    && let ParamValue::Float(stored) = def.default
                    && def.default.as_scalar() != Some(stored)
                {
                    violations.push(format!("{type_id} param `{name}`: Int default does not read back through as_scalar"));
                }
                if let ParamValue::Enum(idx) = def.default
                    && idx as usize >= def.enum_values.len()
                {
                    violations.push(format!(
                        "{type_id} param `{name}`: enum default {idx} out of range for {} options",
                        def.enum_values.len()
                    ));
                }
            }
        }
        assert_no_violations("registered-node shape violations", &violations);
    }

    /// Shadow inputs that are required, so their same-named param can never
    /// act as the unwired fallback. Known and pinned here; new ones fail.
    const REQUIRED_SHADOW_INPUTS: &[(&str, &str)] = &[
        ("node.math", "a"),
        ("node.scale_offset_value", "a"),
        ("node.switch_array", "selector"),
        ("node.switch_texture", "selector"),
        ("node.switch_value", "selector"),
    ];

    /// Angles that are winding amounts rather than orientations. A range on
    /// these clamps a wired or typed value at one turn, the saw-rotation-wrap
    /// class, so they must stay unbounded.
    const UNBOUNDED_WINDING_ANGLES: &[(&str, &str)] = &[
        ("node.bend_mesh", "angle"),
        ("node.revolve_curve", "sweep"),
        ("node.twist_mesh", "angle"),
    ];

    /// Port-shadow convention: an input named like a param lets a wire
    /// override that param. The wire is optional, since the param is the
    /// unwired fallback, and it carries the param's scalar type. Bool, Enum,
    /// Int and Trigger params travel as a plain f32 wire. Table and String
    /// params cannot be driven by a scalar wire, so they are never shadowed.
    #[test]
    fn port_shadow_inputs_are_optional_and_typed_like_their_param() {
        use crate::node_graph::ports::{PortType, ScalarType};
        let mut violations = Vec::new();
        let mut unbounded_seen = 0;
        for (type_id, node) in registered_nodes() {
            let params = node.parameters();
            for port in node.inputs() {
                let Some(def) = params.iter().find(|d| d.name == port.name) else {
                    continue;
                };
                let name: &str = &port.name;
                if port.required && !REQUIRED_SHADOW_INPUTS.contains(&(type_id, name)) {
                    violations.push(format!("{type_id}: shadow input `{name}` is required"));
                }
                let expected = match def.ty {
                    ParamType::Float
                    | ParamType::Angle
                    | ParamType::Frequency
                    | ParamType::Int
                    | ParamType::Bool
                    | ParamType::Enum
                    | ParamType::Trigger => Some(ScalarType::F32),
                    ParamType::Vec2 => Some(ScalarType::Vec2),
                    ParamType::Vec3 => Some(ScalarType::Vec3),
                    ParamType::Vec4 => Some(ScalarType::Vec4),
                    ParamType::Color => Some(ScalarType::Color),
                    ParamType::Table | ParamType::String => None,
                };
                match expected {
                    None => violations.push(format!(
                        "{type_id}: {:?} param `{name}` must not be port-shadowed",
                        def.ty
                    )),
                    Some(scalar) if port.ty != PortType::Scalar(scalar) => violations.push(format!(
                        "{type_id}: shadow input `{name}` is {:?}, param wants {scalar:?}",
                        port.ty
                    )),
                    Some(_) => {}
                }
            }
            for def in params {
                let name: &str = &def.name;
                if !UNBOUNDED_WINDING_ANGLES.contains(&(type_id, name)) {
                    continue;
                }
                unbounded_seen += 1;
                if def.range.is_some() {
                    violations.push(format!("{type_id}: winding angle `{name}` must be unbounded"));
                }
            }
        }
        if unbounded_seen != UNBOUNDED_WINDING_ANGLES.len() {
            violations.push("an UNBOUNDED_WINDING_ANGLES entry names a param that no longer exists".into());
        }
        assert_no_violations("port-shadow violations", &violations);
    }

    /// Integration test: assemble the decomposed Bloom shape (blur a
    /// copy of the source, mix it back) from primitives + boundary
    /// nodes, compile it, execute it. Validates that the trait shape and
    /// pool work for a real multi-node graph with source fan-out and a
    /// multi-input node. Mirrors how Bloom.json is built today
    /// (threshold → downsample → blur → mix), minus the prefilter.
    ///
    /// Topology:
    ///
    /// ```text
    ///   Source ──→ Blur ──→ Mix.b ─→ FinalOutput
    ///       └─────────────→ Mix.a
    /// ```
    #[test]
    fn decomposed_bloom_shape_compiles_and_executes() {
        let mut g = Graph::new();
        let src = g.add_node(Box::new(Source::new()));
        let blur = g.add_node(Box::new(Blur::new()));
        let mix = g.add_node(Box::new(Mix::new()));
        let out = g.add_node(Box::new(FinalOutput::new()));

        g.connect((src, "out"), (blur, "source")).unwrap();
        g.connect((src, "out"), (mix, "a")).unwrap();
        g.connect((blur, "out"), (mix, "b")).unwrap();
        g.connect((mix, "out"), (out, "in")).unwrap();

        validate(&g).unwrap();
        let plan = compile(&g).unwrap();
        assert_eq!(plan.steps().len(), 4);

        let mut exec = Executor::with_mock();
        exec.execute_frame(&mut g, &plan, frame_time());
    }

    /// Mix has two required inputs; both must be wired or validate() fails.
    #[test]
    fn mix_requires_both_inputs_to_be_wired() {
        let mut g = Graph::new();
        let _src = g.add_node(Box::new(Source::new()));
        let _mix = g.add_node(Box::new(Mix::new()));
        // Don't wire either of mix's inputs.
        assert!(matches!(
            validate(&g),
            Err(crate::node_graph::GraphError::RequiredInputUnwired { .. })
        ));
    }

    /// Every shipping primitive's Array ports must carry a declared
    /// Channels signature ([`ArrayType::specs`] non-empty). The
    /// signature is what makes wire validation refuse to connect
    /// byte-identical buffers whose conventions don't match —
    /// `CurvePoint` (channels `x, y`) vs `EdgePair` (channels
    /// `a_index, b_index`) are both 8/4 and would have connected
    /// silently under a pure size/align check. With named channels
    /// they don't.
    ///
    /// Empty-specs Array ports are the deliberate opt-out for
    /// genuinely untyped raw-byte buffers (escape-hatch nodes,
    /// scratch state). Allowed for `node.wgsl_compute*` (the wire
    /// shape derives from user WGSL via naga — `_pad*` fields skip,
    /// matrices and runtime arrays fall back to empty specs) and the
    /// `node.__smoke_test_*` fixtures. A `Channels[permissive]` port has
    /// no fixed signature by design; the Permissive allow-list test in
    /// `validation.rs` is its gate. Anywhere else it's a CI
    /// failure pointing at a missing `KnownItem::SPECS` or a missing
    /// inline `Channels[…]` declaration.
    ///
    /// Walks the live [`super::super::PrimitiveRegistry`] so new
    /// primitives are picked up automatically.
    #[test]
    fn every_conventional_array_port_declares_a_channels_signature() {
        use super::super::PrimitiveRegistry;
        use super::super::ports::PortType;

        let registry = PrimitiveRegistry::with_builtin();
        let mut violations: Vec<String> = Vec::new();
        for type_id in registry.known_type_ids() {
            // Carve-outs (see doc comment above for rationale).
            if type_id.starts_with("node.wgsl_compute")
                || type_id.starts_with("node.__smoke_test_")
                || type_id.starts_with("system.")
            {
                continue;
            }

            let Some(node) = registry.construct(type_id) else {
                continue;
            };

            let mut check_port = |kind_label: &str, port_name: &str, ty: &PortType| {
                if let PortType::Array(layout) = ty
                    && layout.specs.is_empty()
                    && layout.match_mode != super::super::ports::MatchMode::Permissive
                {
                    violations.push(format!(
                        "{type_id}: {kind_label} `{port_name}` is Array<…> \
                         with no Channels signature (specs is empty). \
                         Declare the port via `Array(T)` (with a \
                         `KnownItem` impl on T that sets `SPECS`), via \
                         inline `Channels[name: Type, …]` syntax, or — \
                         if the buffer is genuinely untyped scratch — \
                         extend this test's carve-out list.",
                    ));
                }
            };

            for port in node.inputs() {
                check_port("input", port.name.as_ref(), &port.ty);
            }
            for port in node.outputs() {
                check_port("output", port.name.as_ref(), &port.ty);
            }
        }
        assert!(
            violations.is_empty(),
            "Array-port Channels-signature invariant violations:\n  {}",
            violations.join("\n  "),
        );
    }

    /// Param values can be set on a primitive instance through the Graph API.
    #[test]
    fn primitive_params_accept_typed_overrides() {
        let mut g = Graph::new();
        let id = g.add_node(Box::new(Threshold::new()));
        g.set_param(id, "level", ParamValue::Float(0.7)).unwrap();
        g.set_param(id, "softness", ParamValue::Float(0.1)).unwrap();
        // Unknown param is rejected.
        assert!(g.set_param(id, "missing", ParamValue::Float(0.0)).is_err());
    }
}

mod rigid_body;
mod fluid_role_source;
pub(crate) mod physics_world;
pub(crate) use gltf_animation_source::quat_to_render_scene_euler;
mod platonic_mesh;
