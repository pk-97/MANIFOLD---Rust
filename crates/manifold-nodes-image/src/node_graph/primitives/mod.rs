/// The frame height post-effect blur sizes are set at. Motion blur's and
/// depth of field's longest blur scale from it to the frame they render, so
/// one setting looks the same at 1080p and 4K.
manifold_core::testkit_visible! {
pub(crate) const BLUR_REFERENCE_HEIGHT: f32 = 1080.0;
}

mod abs_texture;
mod affine_transform;
mod anti_clump_particles;
mod array_connect_nearest;
mod array_diffuse_particles;
mod array_filter_detections;
mod audio_waveform;
mod glyph_atlas;
mod render_glyph_grid;
pub mod terminal_analysis;
mod terminal_detail;
mod terminal_passages;
mod terminal_reaction;
mod terminal_stream;
mod terminal_vocabulary;
manifold_core::testkit_visible! { mod array_feedback; }
mod array_math;
mod array_unpack_vec2;
pub(crate) mod beat_gate;
mod beat_ramp;
mod bilateral_blur;
mod blob_detect_ffi;
manifold_core::testkit_visible! { mod blob_overlay_render; }
mod basic_shape;
mod blinn_specular;
mod block_displace_field;
mod block_sample;
mod blur_3d_separable;
pub mod bokeh_gather;
mod box_mask;
mod checkerboard;
mod chroma_key;
mod chromatic_displace;
mod clamp_texture;
mod clip_trigger_cycle;
mod clip_trigger_index;
mod coc_dilate;
mod mirror_axis;
mod pack_channels;
manifold_core::testkit_visible! {
    testkit { pub(crate) mod coc_from_depth; }
    production { mod coc_from_depth; }
}
mod color;
pub use color::{
    BRIGHTNESS_TYPE_ID, Brightness, CHANNEL_MIX_TYPE_ID, COLOR_RAMP_TYPE_ID, ChannelMix, ColorRamp,
};
mod color_sample;
mod colorize;
mod compressor_envelope;
manifold_core::testkit_visible! {
    testkit { pub(crate) mod contrast; }
    production { mod contrast; }
}
mod convolution_2d_9tap;
manifold_core::testkit_visible! { mod cycle_table_row; }
mod depth_estimate_midas;
pub mod detect_regions;
mod distance_to_point;
mod dither;
mod dither_pattern;
mod downsample;
mod mask_extrema;
mod region_mask;
mod region_types;
mod resize_limit;
mod rgb_distance;
mod track_regions;
manifold_core::testkit_visible! { mod draw_connections; }
manifold_core::testkit_visible! { mod draw_dots; }
manifold_core::testkit_visible! { mod draw_gauge; }
manifold_core::testkit_visible! { mod draw_markers; }
mod draw_scanlines;
manifold_core::testkit_visible! { mod draw_ticks; }
mod edge_detect;
mod envelope_beats;
mod envelope_decay;
mod envelope_follower_ar;
mod field_combine;
mod film_grain;
pub mod filter;
mod flash;
manifold_core::testkit_visible! { mod flow_field_noise; }
mod container_repel_force_3d;
mod curl_slope_force_3d;
mod diffuse_force_3d_at_particles;
mod fract_texture;
mod frequency_ratio;
mod fresnel_rim;
mod gradient_central_diff_3d;
mod sample_texture_3d_at_particles;
mod simplex_noise_force_3d_at_particles;
manifold_core::testkit_visible! { mod container_bounds_3d; }
mod ellipse_mask;
mod flatten_to_camera_plane;
pub mod gaussian_blur_variable_width;
mod generate_range;
mod gradient_central_diff;
mod gradient_ramp;
mod grid_uv_field;
mod hash_field_by_seed;
mod hdr_retention_mix;
mod heightmap_to_normal;
mod hue_saturation;
mod image_folder;
mod inject_burst;
mod pack_vec4;
mod scatter_particles_camera;
manifold_core::testkit_visible! { mod sample_texture_at_particles; }
manifold_core::testkit_visible! { mod wrap_particles_torus; }
manifold_core::testkit_visible! { mod wave_field_3d; }
mod inverse_fft_2d;
mod over;
manifold_core::testkit_visible! {
    testkit { pub(crate) mod invert; }
    production { mod invert; }
}
mod lambert_directional;
mod length_vec2;
mod levels;
manifold_core::testkit_visible! { mod lfo; }
pub mod layer_source;
mod lic_integrate;
mod lightning_bolt;
mod linear_gradient;
mod luminance;
mod lut1d;
mod magnitude_db;
mod matcap_two_tone;
pub(crate) mod math;
pub mod motion_blur;
pub mod multi_blend;
mod mux_array;
mod mux_scalar;
mod switch_indices;
mod switch_instances;
mod switch_mesh;
mod zero_array;
manifold_core::testkit_visible! { mod neighbor_smooth; }
mod mirror_fold_uv;
mod noise;
mod normalize_vec2;
mod note_rates;
mod one_euro_filter;
mod optical_flow_estimate;
mod peak;
mod person_segment;
mod polar_field;
mod posterize;
mod power_texture;
pub use note_rates::{NOTE_RATE_LABELS, NOTE_RATE_VALUES};
mod radial_burst_force_field;
mod radial_fold_uv;
mod radial_offset_field;
mod reinhard_tone_map;
mod remap;
mod remove_drift_3d;
mod render_filled_rects;
mod uv_strip_clamp;
manifold_core::testkit_visible! { mod render_text; }
mod render_value_overlay;
mod resolve_3d_accumulator;
manifold_core::testkit_visible! { mod resolve_accumulator; }
mod rotate_vec2_by_angle;
mod sample_and_hold;
mod sample_volume_2d;
mod saturation;
mod scalar_array_accumulator;
mod scale_offset_texture;
mod scanline_jitter_field;
manifold_core::testkit_visible! { mod scatter_particles; }
mod scatter_particles_3d;
pub mod seed_particles_from_texture;
manifold_core::testkit_visible! { mod seed_particles; }
mod separable_gaussian;
pub use separable_gaussian::{
    GAUSSIAN_BLUR_AXES, GAUSSIAN_BLUR_KERNELS, GAUSSIAN_BLUR_TYPE_ID, GaussianBlur,
};
mod set_alpha;
manifold_core::testkit_visible! {
    testkit { pub(crate) mod sharpen; }
    production { mod sharpen; }
}
mod affine_scalar;
mod canvas_area_scale;
mod centered_uv;
mod rotate_2d;
mod simplex_field_2d;
mod simplex_noise_force_at_particles;
mod sin_term;
mod slope_displace;
mod smoothing;
mod smoothstep_texture;
mod spawn_from_mesh;
mod ssao_gtao;
mod temporal;
mod temporal_accumulate;
mod texture_sum_5;
mod track_persist;
mod trig_texture;
pub use temporal::{FEEDBACK_TYPE_ID, Feedback};
mod texture_advect;
mod texture_dimensions;
mod tone_map;
mod transform_components;
mod trigger_ease_to;
mod trigger_gate;
manifold_core::testkit_visible! { pub(crate) mod divide_by_value; }
mod compose_vec3;
mod uv_displace_by_flow;
mod uv_field;
mod vignette;
mod voronoi_2d;
pub mod watercolor;
mod wet_dry_mix;
pub use wet_dry_mix::{WET_DRY_TYPE_ID, WetDry};
manifold_core::testkit_visible! { mod interpolate_particle_frames; }
mod mix_arrays;
