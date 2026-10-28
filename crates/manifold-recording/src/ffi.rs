//! Raw FFI declarations for `LiveRecordingPlugin.m`.
//!
//! These mirror the C API surface exposed by the Objective-C implementation.
//! All functions take/return opaque `*mut c_void` handles.

use std::ffi::{CString, c_char, c_void};

use manifold_gpu::GpuDevice;

use crate::config::{AudioCodec, LiveRecordingConfig};
use crate::texture_pool::PoolSlot;

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

/// The native recorder's audio-drop counter, readable from the content
/// thread. Only valid until the recorder finalizes.
pub(crate) struct AudioDrops(usize);

impl AudioDrops {
    pub(crate) fn get(&self) -> u32 {
        let n = unsafe { LiveRecorder_GetAudioFramesDropped(self.0 as *mut c_void) };
        n.max(0) as u32
    }
}

/// The Mac's recorder: AVAssetWriter in `LiveRecordingPlugin.m`, ProRes 422
/// Proxy for SDR and HEVC Main10 for HDR.
pub(crate) struct NativeRecorder(*mut c_void);

// Made on the content thread, then used only on the recording thread.
unsafe impl Send for NativeRecorder {}

impl NativeRecorder {
    /// Open `config.output_path` for `width × height` video at `fps`, with
    /// an audio track when `sample_rate` is not zero.
    pub(crate) fn new(
        config: &LiveRecordingConfig,
        device: &GpuDevice,
        width: u32,
        height: u32,
        fps: f32,
        sample_rate: u32,
        channels: u16,
    ) -> Result<Self, String> {
        let c_path = CString::new(config.output_path.as_str())
            .map_err(|_| "Invalid output path".to_string())?;
        let audio_codec = match config.audio_codec {
            AudioCodec::Aac => 0,
            AudioCodec::Alac => 1,
        };
        let handle = unsafe {
            LiveRecorder_Create(
                width as i32,
                height as i32,
                fps,
                c_path.as_ptr(),
                i32::from(config.hdr),
                device.raw_device_ptr(),
                sample_rate as i32,
                i32::from(channels),
                audio_codec,
            )
        };
        if handle.is_null() {
            return Err("Failed to create native live recorder".into());
        }
        Ok(Self(handle))
    }

    pub(crate) fn audio_drops(&self) -> AudioDrops {
        AudioDrops(self.0 as usize)
    }

    /// Queue the frame in `slot` for the native async append. Success only
    /// rules out a synchronous failure; `finalize` reports what was appended.
    pub(crate) fn encode_video(&mut self, slot: &PoolSlot, elapsed: f64) -> Result<(), String> {
        match unsafe { LiveRecorder_EncodeVideoFrame(self.0, slot.raw_ptr, elapsed) } {
            0 => Ok(()),
            code => Err(format!("error {code}")),
        }
    }

    pub(crate) fn write_audio(&mut self, samples: &[f32], elapsed: f64) -> Result<(), String> {
        match unsafe {
            LiveRecorder_WriteAudioSamples(self.0, samples.as_ptr(), samples.len() as i32, elapsed)
        } {
            0 => Ok(()),
            code => Err(format!("error {code}")),
        }
    }

    /// Frames queued for the async append but dropped there.
    pub(crate) fn video_frames_append_dropped(&self) -> u32 {
        unsafe { LiveRecorder_GetVideoFramesAppendDropped(self.0) }.max(0) as u32
    }

    /// Drain the append queue and close the file. Returns the frames
    /// appended, read after the queue is drained.
    pub(crate) fn finalize(self) -> Result<u32, String> {
        match unsafe { LiveRecorder_Finalize(self.0) } {
            n if n >= 0 => Ok(n as u32),
            code => Err(format!("error {code}")),
        }
    }
}
