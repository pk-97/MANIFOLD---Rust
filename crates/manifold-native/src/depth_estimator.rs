/// Packed optical flow result per pixel: [flowX_uv, flowY_uv, confidence, validMask].
/// Matches DepthEstimatorPlugin.cpp output layout.
#[derive(Clone, Copy, Debug, Default)]
pub struct FlowPixel {
    pub flow_x: f32,
    pub flow_y: f32,
    pub confidence: f32,
    pub valid_mask: f32,
}

/// Trait for depth estimation / optical flow / subject segmentation backends.
///
/// Today: FFI to DepthEstimator.bundle (OpenCV DNN + Farneback).
/// Future: ort + CoreML/Metal for GPU-accelerated DNN inference.
///
/// Matches Unity's `DepthEstimatorNative` wrapper semantics:
/// - All inputs are RGBA8 byte arrays
/// - All outputs are pre-allocated float slices
/// - Returns non-zero on success, 0 on failure
/// - Models are optional; missing models return 0 gracefully
pub trait DepthEstimator: Send {
    /// Monocular depth inference (MiDaS-style).
    ///
    /// - `rgba`: input RGBA8 pixels (width * height * 4 bytes)
    /// - `out_depth`: pre-allocated, out_width * out_height floats, normalized 0..1
    ///
    /// Returns non-zero on success, 0 on failure (model missing, invalid params).
    fn process(
        &mut self,
        rgba: &[u8],
        width: i32,
        height: i32,
        out_depth: &mut [f32],
        out_width: i32,
        out_height: i32,
    ) -> i32;

    /// Foreground/subject segmentation.
    ///
    /// - `out_mask`: pre-allocated, out_width * out_height floats, normalized 0..1
    ///
    /// Returns non-zero on success, 0 on failure.
    fn process_subject_mask(
        &mut self,
        rgba: &[u8],
        width: i32,
        height: i32,
        out_mask: &mut [f32],
        out_width: i32,
        out_height: i32,
    ) -> i32;

    /// Dense optical flow (Farneback) with global motion compensation.
    ///
    /// - `prev_rgba`, `curr_rgba`: two consecutive RGBA8 frames
    /// - `out_flow_packed`: pre-allocated, out_width * out_height * 4 floats
    ///   Layout per pixel: [flowX_uv, flowY_uv, confidence, validMask]
    /// - `out_cut_score`: pre-allocated, >= 1 float (scene cut score 0..1)
    ///
    /// Returns non-zero on success, 0 on failure.
    fn compute_flow(
        &mut self,
        prev_rgba: &[u8],
        curr_rgba: &[u8],
        width: i32,
        height: i32,
        out_flow_packed: &mut [f32],
        out_width: i32,
        out_height: i32,
        out_cut_score: &mut [f32],
    ) -> i32;
}
