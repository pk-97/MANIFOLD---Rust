/// Result from a single blob detection: center + half-size, all normalized 0..1.
/// Matches the 4-float layout from BlobDetectorPlugin.cpp:
///   [center_x, center_y, half_width, half_height]
#[derive(Clone, Copy, Debug, Default)]
pub struct BlobResult {
    pub center_x: f32,
    pub center_y: f32,
    pub half_width: f32,
    pub half_height: f32,
}

/// Trait for blob detection backends.
///
/// Today: FFI to BlobDetector.bundle (OpenCV Canny + contours).
/// Future: ort + CoreML backend, or pure-Rust CV.
///
/// Matches Unity's `BlobDetectorNative` wrapper semantics:
/// - `process()` takes RGBA8 pixels, returns blob count
/// - Output is written to a pre-allocated float slice: max_blobs * 4 floats
/// - Y coordinates are flipped for UV space (1.0 - computed_y)
pub trait BlobDetector: Send {
    /// Process an RGBA8 frame and detect blobs.
    ///
    /// - `rgba`: raw RGBA8 pixel data (width * height * 4 bytes)
    /// - `width`, `height`: frame dimensions
    /// - `threshold`: Canny edge sensitivity 0..1
    /// - `sensitivity`: blur/dilation/min-area control 0..1
    /// - `out_blob_data`: pre-allocated slice, must be >= max_blobs * 4 floats
    ///   Layout per blob: [center_x, center_y, half_width, half_height]
    ///
    /// Returns the number of blobs detected (0..max_blobs).
    fn process(
        &mut self,
        rgba: &[u8],
        width: i32,
        height: i32,
        threshold: f32,
        sensitivity: f32,
        out_blob_data: &mut [f32],
    ) -> i32;
}
