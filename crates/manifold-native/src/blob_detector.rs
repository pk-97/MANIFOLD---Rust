// Mechanical port of BlobDetectorNative.cs interface.
// The Unity side is an FFI wrapper; the Rust side is a trait so the FFI impl
// can be swapped for a stub when the native plugin is unavailable.

/// Trait matching BlobDetectorNative.cs external API surface.
/// BlobDetector_Process returns the number of blobs found.
/// Output is packed as [x, y, w, h] * max_blobs floats.
pub trait BlobDetector: Send {
    /// Process an RGBA pixel buffer and detect blobs.
    /// Returns the number of blobs detected (0..=max_blobs).
    /// `out_blob_data` receives `count * 4` floats: [x, y, w, h] per blob
    /// in normalised [0..1] coordinates.
    fn process(
        &self,
        rgba_data: &[u8],
        width: i32,
        height: i32,
        threshold: f32,
        sensitivity: f32,
        out_blob_data: &mut [f32],
    ) -> i32;
}
