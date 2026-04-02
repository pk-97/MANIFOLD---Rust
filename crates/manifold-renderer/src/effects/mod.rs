/// Resolution divisor for HDR effect intermediate buffers (bloom, halation).
/// 4 = quarter-res, 2 = half-res. Tune this to trade quality vs GPU cost.
pub const HDR_BUFFER_DIVISOR: u32 = 2;

pub mod compute_blit_helper;
pub mod compute_dual_blit_helper;
pub mod invert_colors;
pub mod color_grade;
pub mod mirror;
pub mod bloom;
pub mod chromatic_aberration;
pub mod glitch;
pub mod dither;
pub mod halation;
pub mod kaleidoscope;
pub mod edge_stretch;
pub mod quad_mirror;
pub mod strobe;
pub mod stylized_feedback;
pub mod edge_detect;
pub mod transform;
pub mod infrared;
pub mod voronoi_prism;
pub mod blob_tracking;
pub mod wireframe_depth;
pub mod depth_of_field;
pub mod hdr_boost;
pub mod auto_gain;
