/// Resolution divisor for HDR effect intermediate buffers (bloom, halation).
/// 4 = quarter-res, 2 = half-res. Tune this to trade quality vs GPU cost.
pub const HDR_BUFFER_DIVISOR: u32 = 1;

pub mod compute_blit_helper;
pub mod compute_dual_blit_helper;
pub mod wireframe_depth;
