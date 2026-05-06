/// Resolution divisor for HDR effect intermediate buffers (bloom, halation).
/// 4 = quarter-res, 2 = half-res. Tune this to trade quality vs GPU cost.
pub const HDR_BUFFER_DIVISOR: u32 = 1;

pub mod auto_gain;
pub mod blob_tracking;
pub mod bloom;
pub mod chromatic_aberration;
pub mod color_grade;
pub mod compute_blit_helper;
pub mod registration;
pub mod compute_dual_blit_helper;
pub mod depth_of_field;
pub mod dither;
pub mod edge_detect;
pub mod edge_stretch;
pub mod glitch;
pub mod halation;
pub mod hdr_boost;
pub mod infrared;
pub mod invert_colors;
pub mod kaleidoscope;
pub mod mirror;
pub mod mirror_graph;
pub mod node_graph_test;
pub mod quad_mirror;
pub mod strobe;
pub mod stylized_feedback;
pub mod transform;
pub mod voronoi_prism;
pub mod watercolor;
pub mod wireframe_depth;
