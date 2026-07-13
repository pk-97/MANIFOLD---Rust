//! Raw FFI declarations for `LiveRecordingPlugin.m`.
//!
//! These mirror the C API surface exposed by the Objective-C implementation.
//! All functions take/return opaque `*mut c_void` handles.

use std::ffi::{c_char, c_void};

unsafe extern "C" {
    /// Create a new live recording session.
    ///
    /// - `device_ptr`: valid `id<MTLDevice>` (shared with content pipeline)
    /// - `audio_sample_rate`: 0 to disable audio track
    /// - `audio_codec`: 0 = AAC, 1 = ALAC
    ///
    /// Returns an opaque handle, or NULL on failure.
    pub fn LiveRecorder_Create(
        width: i32,
        height: i32,
        fps: f32,
        output_path: *const c_char,
        hdr: i32,
        device_ptr: *mut c_void,
        audio_sample_rate: i32,
        audio_channels: i32,
        audio_codec: i32,
    ) -> *mut c_void;

    /// Encode a single video frame.
    ///
    /// - `texture_ptr`: valid `id<MTLTexture>` that has been fully written
    /// - `elapsed_seconds`: wall-clock seconds since recording started
    ///
    /// Returns 0 on success, non-zero error code on failure.
    pub fn LiveRecorder_EncodeVideoFrame(
        handle: *mut c_void,
        texture_ptr: *mut c_void,
        elapsed_seconds: f64,
    ) -> i32;

    /// Write interleaved Float32 audio samples.
    ///
    /// - `samples`: pointer to interleaved Float32 sample data
    /// - `sample_count`: number of samples (total, including all channels)
    /// - `elapsed_seconds`: wall-clock seconds since recording started
    ///
    /// Returns 0 on success, non-zero error code on failure.
    pub fn LiveRecorder_WriteAudioSamples(
        handle: *mut c_void,
        samples: *const f32,
        sample_count: i32,
        elapsed_seconds: f64,
    ) -> i32;

    /// Read the number of audio sample-frames dropped so far by the native
    /// encoder's backpressure gate (`AVAssetWriterInput.isReadyForMoreMediaData`
    /// returning false in `WriteAudioSamples`) — BUG-084's counter, and an
    /// instrument for BUG-086's still-unexplained audio-duration shortfall.
    ///
    /// Backed by an atomic counter; safe to poll from any thread at any
    /// point before `LiveRecorder_Finalize` is called on this handle (that
    /// call frees the native state — do not poll afterward).
    pub fn LiveRecorder_GetAudioFramesDropped(handle: *mut c_void) -> i32;

    /// Read the number of video frames whose async `appendPixelBuffer:` call
    /// (dispatched from `LiveRecorder_EncodeVideoFrame`) was skipped or
    /// failed — BUG-085's counter. `LiveRecorder_EncodeVideoFrame` returns
    /// success as soon as the synchronous GPU blit completes, before the
    /// async append runs; this counter is the only way to see an append
    /// that later silently drops.
    ///
    /// Backed by an atomic counter; safe to poll from any thread at any
    /// point before `LiveRecorder_Finalize` is called on this handle (that
    /// call frees the native state — do not poll afterward).
    pub fn LiveRecorder_GetVideoFramesAppendDropped(handle: *mut c_void) -> i32;

    /// Finalize the recording: drain buffers, close AVAssetWriter.
    ///
    /// Returns the total number of video frames encoded, or negative on error.
    /// The handle is invalid after this call.
    pub fn LiveRecorder_Finalize(handle: *mut c_void) -> i32;
}
