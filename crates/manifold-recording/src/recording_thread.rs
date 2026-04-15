//! Dedicated recording thread.
//!
//! Drains video frames from the content thread channel and audio samples from
//! the Core Audio ring buffer, feeding both to the native AVAssetWriter encoder.
//! This thread does ALL encoding work — the content thread never blocks on it.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    /// Wall-clock timestamp when the frame was produced.
    pub wall_timestamp: Instant,
    /// Fence: signaled by the GPU completion handler when the blit
    /// to this pool texture is finished. The recording thread waits
    /// on this before reading the texture.
    pub gpu_complete: Arc<GpuFence>,
}

// PoolSlot contains a raw pointer to a Metal texture — safe to send.
unsafe impl Send for RecordingFrame {}

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
    start_time: Instant,
) -> (u32, u32) {
    let mut frames_encoded: u32 = 0;
    let mut frames_failed: u32 = 0;

    // Scratch buffer for draining audio ring buffer.
    let mut audio_scratch = vec![0.0f32; 4096];
    let mut total_audio_frames: u64 = 0;

    log::info!("[RecordingThread] Started");

    loop {
        let stopping = stop.load(Ordering::Acquire);

        // -- Drain video frames --
        loop {
            let frame = if stopping {
                match frame_rx.try_recv() {
                    Ok(f) => f,
                    Err(_) => break,
                }
            } else {
                match frame_rx.try_recv() {
                    Ok(f) => f,
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        log::warn!("[RecordingThread] Frame channel disconnected");
                        stop.store(true, Ordering::Release);
                        break;
                    }
                }
            };

            // Wait for GPU blit to complete (kernel notification, zero CPU).
            let fence_ok = frame.gpu_complete.wait(Duration::from_secs(5));
            if !fence_ok {
                log::error!(
                    "[RecordingThread] GPU fence timeout (5s) — \
                     skipping frame, possible GPU hang"
                );
            }

            if !fence_ok {
                frame.pool_slot.release();
                frames_failed += 1;
                continue;
            }

            // Encode the video frame.
            let elapsed = frame.wall_timestamp.duration_since(start_time).as_secs_f64();
            let texture_ptr = frame.pool_slot.raw_ptr;

            let result = unsafe {
                ffi::LiveRecorder_EncodeVideoFrame(encoder_handle, texture_ptr, elapsed)
            };

            // Release the pool slot AFTER encoding.
            frame.pool_slot.release();

            if result == 0 {
                frames_encoded += 1;
            } else {
                frames_failed += 1;
                log::warn!(
                    "[RecordingThread] Video encode failed at {elapsed:.3}s: error {result}"
                );
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
                start_time,
            );
        }

        // -- Check stop condition --
        if stopping {
            if let Some(ref mut consumer) = audio_consumer {
                drain_audio(
                    consumer,
                    encoder_handle,
                    &mut audio_scratch,
                    &mut total_audio_frames,
                    sample_rate,
                    channels,
                    start_time,
                );
            }
            break;
        }

        // Brief yield to avoid busy-spinning when both queues are empty.
        std::thread::sleep(Duration::from_micros(500));
    }

    // Finalize the native encoder.
    let finalize_result = unsafe { ffi::LiveRecorder_Finalize(encoder_handle) };
    if finalize_result < 0 {
        log::error!("[RecordingThread] Encoder finalization failed: {finalize_result}");
    }

    log::info!(
        "[RecordingThread] Finished: {frames_encoded} frames encoded, {frames_failed} failed"
    );

    (frames_encoded, frames_failed)
}

/// Drain available audio samples from the ring buffer and write to the encoder.
fn drain_audio(
    consumer: &mut manifold_audio::capture::AudioConsumer,
    encoder_handle: *mut c_void,
    scratch: &mut [f32],
    total_frames: &mut u64,
    sample_rate: u32,
    channels: u16,
    _start_time: Instant,
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
