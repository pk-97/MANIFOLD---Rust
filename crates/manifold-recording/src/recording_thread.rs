//! Dedicated recording thread.
//!
//! Drains video frames from the content thread channel and audio samples from
//! the Core Audio ring buffer, feeding both to the native AVAssetWriter encoder.
//! This thread does ALL encoding work — the content thread never blocks on it.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::Receiver;
use ringbuf::traits::{Consumer as ConsumerTrait, Observer as ObserverTrait};

use crate::ffi;
use crate::texture_pool::PoolSlot;

/// GPU completion fence — replaces AtomicBool spin-loop with a condvar.
///
/// The GPU completion handler calls `signal()` (sets flag + notifies condvar).
/// The recording thread calls `wait()` (sleeps until signaled or timeout).
/// Zero CPU during the wait.
pub struct GpuFence {
    state: std::sync::Mutex<bool>,
    condvar: std::sync::Condvar,
}

impl Default for GpuFence {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuFence {
    pub fn new() -> Self {
        Self {
            state: std::sync::Mutex::new(false),
            condvar: std::sync::Condvar::new(),
        }
    }

    /// Signal that the GPU has completed. Called from the completion handler.
    pub fn signal(&self) {
        if let Ok(mut done) = self.state.lock() {
            *done = true;
            self.condvar.notify_one();
        }
    }

    /// Wait until signaled or timeout. Returns `true` if signaled.
    pub fn wait(&self, timeout: Duration) -> bool {
        let guard = self.state.lock().unwrap();
        let (_guard, result) = self
            .condvar
            .wait_timeout_while(guard, timeout, |done| !*done)
            .unwrap();
        !result.timed_out()
    }
}

/// A single frame submitted by the content thread.
pub(crate) struct RecordingFrame {
    /// Pool slot holding the raw texture pointer. Released after encoding.
    pub pool_slot: PoolSlot,
    /// Elapsed time since recording started, computed at submit time
    /// (`LiveRecordingSession::submit_frame_at`) — carries `Duration`, not
    /// `Instant`, so the harness can fabricate adversarial timing without
    /// pacing tests by wall clock (docs/LIVE_RECORDING_PROOFS_DESIGN.md D1).
    pub elapsed: Duration,
    /// Fence: signaled by the GPU completion handler when the blit
    /// to this pool texture is finished. The recording thread waits
    /// on this before reading the texture.
    pub gpu_complete: Arc<GpuFence>,
}

// PoolSlot contains a raw pointer to a Metal texture — safe to send.
unsafe impl Send for RecordingFrame {}

/// Recording-thread outcome, read by `LiveRecordingSession::stop()`.
///
/// `frames_recorded` is the native ground truth — read from
/// `LiveRecorder_Finalize`'s return value AFTER it drains the async append
/// queue, not accumulated from `LiveRecorder_EncodeVideoFrame`'s synchronous
/// return (BUG-085: that return happens before the real, async
/// `appendPixelBuffer:` call, so it can't tell success from a later silent
/// drop).
pub(crate) struct RecordingStats {
    /// Video frames actually appended to the file (native `videoFramesAppended`
    /// counter, read after the append queue is drained).
    pub frames_recorded: u32,
    /// Video frames that failed synchronously — before ever being queued for
    /// async append (GPU fence timeout, blit failure, writer-not-ready spin
    /// exhausted, etc.).
    pub frames_sync_failed: u32,
    /// Video frames queued for async append but dropped there (native
    /// `videoFramesAppendDropped` counter — BUG-085's instrument).
    pub video_append_dropped: u32,
}

/// Run the recording thread main loop.
///
/// This function blocks until `stop` is set to `true` and all remaining
/// frames are drained. Finalizes the native encoder before returning.
pub(crate) fn run(
    frame_rx: Receiver<RecordingFrame>,
    mut audio_consumer: Option<manifold_audio::capture::AudioConsumer>,
    encoder_handle: *mut c_void,
    sample_rate: u32,
    channels: u16,
    stop: Arc<AtomicBool>,
) -> RecordingStats {
    let mut frames_sync_failed: u32 = 0;

    // Scratch buffer for draining audio ring buffer.
    let mut audio_scratch = vec![0.0f32; 4096];
    let mut total_audio_frames: u64 = 0;

    log::info!("[RecordingThread] Started");

    /// Encode a single video frame: wait for GPU fence, encode, release pool slot.
    ///
    /// BUG-085: `LiveRecorder_EncodeVideoFrame` returning 0 means the frame
    /// was successfully queued for the native async `appendPixelBuffer:`
    /// call, NOT that it was appended — the real success/failure happens
    /// later, off this thread, on the native encoder's own append queue. So
    /// a 0 return only rules out *synchronous* failure (GPU fence timeout,
    /// blit failure, writer-not-ready spin exhausted); it does not count
    /// toward `frames_recorded`. The ground truth is read once, after
    /// `LiveRecorder_Finalize` drains the append queue — see the tail of
    /// `run()` below.
    #[inline]
    fn encode_frame(frame: RecordingFrame, encoder_handle: *mut c_void, frames_sync_failed: &mut u32) {
        // Wait for GPU blit to complete (kernel notification, zero CPU).
        let fence_ok = frame.gpu_complete.wait(Duration::from_secs(5));
        if !fence_ok {
            log::error!(
                "[RecordingThread] GPU fence timeout (5s) — \
                 skipping frame, possible GPU hang"
            );
            frame.pool_slot.release();
            *frames_sync_failed += 1;
            return;
        }

        let elapsed = frame.elapsed.as_secs_f64();
        let texture_ptr = frame.pool_slot.raw_ptr;

        let result =
            unsafe { ffi::LiveRecorder_EncodeVideoFrame(encoder_handle, texture_ptr, elapsed) };

        // Release the pool slot AFTER encoding.
        frame.pool_slot.release();

        if result != 0 {
            *frames_sync_failed += 1;
            log::warn!("[RecordingThread] Video encode failed at {elapsed:.3}s: error {result}");
        }
    }

    // Audio drain interval: 2ms. At 48kHz stereo this is ~192 samples —
    // well within the scratch buffer. Short enough for continuous audio,
    // long enough that the thread sleeps in the kernel between frames
    // instead of polling 2000×/sec.
    const AUDIO_DRAIN_INTERVAL: Duration = Duration::from_millis(2);

    loop {
        let stopping = stop.load(Ordering::Acquire);

        // -- Wait for video frames (kernel-level block, zero CPU) --
        // When running: recv_timeout blocks until a frame arrives or the
        // audio drain interval elapses. When stopping: non-blocking drain.
        let first_frame = if stopping {
            frame_rx.try_recv().ok()
        } else {
            match frame_rx.recv_timeout(AUDIO_DRAIN_INTERVAL) {
                Ok(f) => Some(f),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    log::warn!("[RecordingThread] Frame channel disconnected");
                    stop.store(true, Ordering::Release);
                    None
                }
            }
        };

        // Encode the first frame, then drain any additional queued frames.
        if let Some(frame) = first_frame {
            encode_frame(frame, encoder_handle, &mut frames_sync_failed);
            while let Ok(frame) = frame_rx.try_recv() {
                encode_frame(frame, encoder_handle, &mut frames_sync_failed);
            }
        }

        // -- Drain audio samples --
        if let Some(ref mut consumer) = audio_consumer {
            drain_audio(
                consumer,
                encoder_handle,
                &mut audio_scratch,
                &mut total_audio_frames,
                sample_rate,
                channels,
            );
        }

        // -- Check stop condition --
        if stopping {
            // Final audio drain after all video frames are processed.
            if let Some(ref mut consumer) = audio_consumer {
                drain_audio(
                    consumer,
                    encoder_handle,
                    &mut audio_scratch,
                    &mut total_audio_frames,
                    sample_rate,
                    channels,
                );
            }
            break;
        }
    }

    // BUG-085: read the async append-drop counter BEFORE Finalize — it frees
    // the native state, after which polling is unsafe.
    let video_append_dropped =
        unsafe { ffi::LiveRecorder_GetVideoFramesAppendDropped(encoder_handle) }.max(0) as u32;

    // Finalize the native encoder. Its return value is the ground truth
    // appended-frame count (`videoFramesAppended`, read after the append
    // queue is fully drained) — this is `frames_recorded`, not the
    // synchronous-LR_OK accumulator this function used to keep.
    let finalize_result = unsafe { ffi::LiveRecorder_Finalize(encoder_handle) };
    let frames_recorded = if finalize_result >= 0 {
        finalize_result as u32
    } else {
        log::error!("[RecordingThread] Encoder finalization failed: {finalize_result}");
        0
    };

    log::info!(
        "[RecordingThread] Finished: {frames_recorded} frames recorded, \
         {frames_sync_failed} sync-failed, {video_append_dropped} async-append-dropped"
    );

    RecordingStats {
        frames_recorded,
        frames_sync_failed,
        video_append_dropped,
    }
}

/// Drain available audio samples from the ring buffer and write to the encoder.
fn drain_audio(
    consumer: &mut manifold_audio::capture::AudioConsumer,
    encoder_handle: *mut c_void,
    scratch: &mut [f32],
    total_frames: &mut u64,
    sample_rate: u32,
    channels: u16,
) {
    loop {
        let available = consumer.occupied_len();
        if available == 0 {
            break;
        }

        let channels_usize = channels as usize;
        let max_read = scratch.len() - (scratch.len() % channels_usize);
        let to_read = available.min(max_read);
        let to_read = to_read - (to_read % channels_usize);
        if to_read == 0 {
            break;
        }

        let popped = consumer.pop_slice(&mut scratch[..to_read]);
        if popped == 0 {
            break;
        }

        // PTS from total frames written (sample-accurate).
        let elapsed_seconds = *total_frames as f64 / sample_rate as f64;

        let result = unsafe {
            ffi::LiveRecorder_WriteAudioSamples(
                encoder_handle,
                scratch.as_ptr(),
                popped as i32,
                elapsed_seconds,
            )
        };

        if result != 0 {
            log::warn!("[RecordingThread] Audio write failed: error {result}");
        }

        *total_frames += popped as u64 / channels as u64;
    }
}
