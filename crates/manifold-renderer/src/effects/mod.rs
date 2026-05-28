/// Resolution divisor for HDR effect intermediate buffers (bloom, halation).
/// 4 = quarter-res, 2 = half-res. Tune this to trade quality vs GPU cost.
pub const HDR_BUFFER_DIVISOR: u32 = 1;

pub mod auto_gain;
pub mod compute_blit_helper;
pub mod compute_dual_blit_helper;
pub mod depth_of_field;
pub mod infrared;
pub mod quad_mirror;
pub mod wireframe_depth;
