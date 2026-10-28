//! Decode thread pool and job scheduler for video playback.
//!
//! Owns a fixed pool of worker threads that perform video decode operations
//! (open, prepare, seek, decode) off the content thread. The content thread
//! submits jobs and drains results without ever blocking.
//!
//! Jobs are routed to workers by clip_id affinity — all jobs for the same
//! clip always go to the same worker, so the worker's local handle map
//! stays consistent.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::thread;

use ahash::AHashMap;
use crossbeam_channel::{Receiver, Sender, TryRecvError};

use crate::decoder::{DecodeStatus, DecoderBackend, FrameSource, VideoDecoder};

/// Number of decode worker threads.
const WORKER_COUNT: usize = 4;

/// Job submitted to a decode worker.
pub enum DecodeJob {
    /// Open a video file for `width × height` targets and assign it to a
    /// clip ID.
    Open {
        clip_id: String,
        path: String,
        width: u32,
        height: u32,
    },
    /// Start reading and decode the first frame.
    Prepare { clip_id: String },
    /// Seek to a specific time and decode the frame there.
    Seek { clip_id: String, target_time: f32 },
    /// Decode the next sequential frame.
    DecodeNext { clip_id: String },
    /// Close and release a decoder handle.
    Close { clip_id: String },
    /// Open a video file for warm cache (keyed by video_clip_id, not clip_id).
    WarmOpen {
        video_clip_id: String,
        path: String,
        width: u32,
        height: u32,
    },
    /// Shutdown the worker thread.
    Shutdown,
}

impl DecodeJob {
    /// Get the routing key for affinity. Jobs for the same clip go to the same worker.
    fn routing_key(&self) -> Option<&str> {
        match self {
            Self::Open { clip_id, .. }
            | Self::Prepare { clip_id }
            | Self::Seek { clip_id, .. }
            | Self::DecodeNext { clip_id }
            | Self::Close { clip_id } => Some(clip_id),
            Self::WarmOpen { video_clip_id, .. } => Some(video_clip_id),
            Self::Shutdown => None,
        }
    }
}

/// Result sent back from a decode worker to the content thread.
pub struct DecodeResult {
    pub clip_id: String,
    pub status: DecodeResultStatus,
}

/// Status of a completed decode operation.
///
/// Variants that carry a decoded frame hand over its decoder as a
/// `FrameSource`; the content thread copies the frame into the clip's
/// texture while no job for the clip is in flight (its `decode_pending`
/// flag holds the next one back).
pub enum DecodeResultStatus {
    /// File opened successfully — includes metadata.
    Opened {
        duration: f32,
        width: i32,
        height: i32,
        frame_rate: f32,
    },
    /// First frame decoded — decoder is prepared.
    Prepared { frame: FrameSource },
    /// A new frame is ready at the given presentation time.
    FrameReady { frame_time: f32, frame: FrameSource },
    /// Reached end of file.
    EndOfFile,
    /// Seek completed — frame at new position is ready.
    Seeked { frame_time: f32, frame: FrameSource },
    /// Warm cache decoder is ready (keyed by video_clip_id).
    WarmReady { video_clip_id: String },
    /// An error occurred.
    Error(String),
}

/// Decode scheduler owning a fixed thread pool with affinity routing.
///
/// Each worker has its own job channel. Jobs are routed by hashing the
/// clip_id so all jobs for the same clip go to the same worker (ensuring
/// the worker's local handle map stays consistent).
pub struct DecodeScheduler {
    worker_txs: Vec<Sender<DecodeJob>>,
    result_rx: Receiver<DecodeResult>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl DecodeScheduler {
    /// Create a new scheduler with `WORKER_COUNT` threads.
    pub fn new(backend: Arc<dyn DecoderBackend>) -> Self {
        let (result_tx, result_rx) = crossbeam_channel::unbounded::<DecodeResult>();

        let mut workers = Vec::with_capacity(WORKER_COUNT);
        let mut worker_txs = Vec::with_capacity(WORKER_COUNT);

        for i in 0..WORKER_COUNT {
            let (job_tx, job_rx) = crossbeam_channel::unbounded::<DecodeJob>();
            let tx = result_tx.clone();
            let backend = Arc::clone(&backend);

            let handle = thread::Builder::new()
                .name(format!("manifold-decode-{i}"))
                .spawn(move || {
                    worker_loop(job_rx, tx, &*backend);
                })
                .expect("failed to spawn decode worker");

            worker_txs.push(job_tx);
            workers.push(handle);
        }

        log::info!(
            "[DecodeScheduler] Started {} decode workers (affinity routing)",
            WORKER_COUNT
        );

        Self {
            worker_txs,
            result_rx,
            workers,
        }
    }

    /// Submit a job to the decode thread pool.
    /// Jobs are routed to a specific worker by clip_id hash for affinity.
    pub fn submit(&self, job: DecodeJob) {
        let worker_idx = match job.routing_key() {
            Some(key) => {
                let mut hasher = DefaultHasher::new();
                key.hash(&mut hasher);
                hasher.finish() as usize % WORKER_COUNT
            }
            None => 0, // Shutdown goes to worker 0
        };
        let _ = self.worker_txs[worker_idx].send(job);
    }

    /// Drain all available results without blocking.
    pub fn drain_results(&self) -> Vec<DecodeResult> {
        let mut results = Vec::new();
        loop {
            match self.result_rx.try_recv() {
                Ok(result) => results.push(result),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        results
    }

    /// Block for at most `timeout` waiting for the first result, then drain
    /// any others that arrived without further waiting.
    ///
    /// Returns empty if the timeout elapses with nothing received, or if the
    /// channel is disconnected. Defense-in-depth backstop (BUG-127): every
    /// worker reply path is expected to always answer a submitted job, but
    /// this bounds a caller's wait even if that protocol is ever violated,
    /// instead of hanging forever.
    pub fn recv_results_timeout(&self, timeout: std::time::Duration) -> Vec<DecodeResult> {
        let mut results = Vec::new();
        match self.result_rx.recv_timeout(timeout) {
            Ok(result) => results.push(result),
            Err(_) => return results, // timed out or disconnected
        }
        while let Ok(result) = self.result_rx.try_recv() {
            results.push(result);
        }
        results
    }

    /// Shut down all worker threads.
    pub fn shutdown(&mut self) {
        for tx in &self.worker_txs {
            let _ = tx.send(DecodeJob::Shutdown);
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
        log::info!("[DecodeScheduler] All workers shut down");
    }
}

impl Drop for DecodeScheduler {
    fn drop(&mut self) {
        if !self.workers.is_empty() {
            self.shutdown();
        }
    }
}

/// Worker thread main loop.
///
/// Each worker owns a local map of decoders (active clips) and warm cache
/// decoders (pre-opened for MIDI). All decoding happens here.
/// Affinity routing guarantees all jobs for the same clip arrive at the
/// same worker.
fn worker_loop(
    job_rx: Receiver<DecodeJob>,
    result_tx: Sender<DecodeResult>,
    backend: &dyn DecoderBackend,
) {
    let mut active: AHashMap<String, Box<dyn VideoDecoder>> = AHashMap::new();
    let mut warm: AHashMap<String, Box<dyn VideoDecoder>> = AHashMap::new();

    while let Ok(job) = job_rx.recv() {
        match job {
            DecodeJob::Open {
                clip_id,
                path,
                width,
                height,
            } => match backend.open(&path, width, height) {
                Ok(handle) => {
                    let duration = handle.duration();
                    let width = handle.width();
                    let height = handle.height();
                    let frame_rate = handle.frame_rate();
                    active.insert(clip_id.clone(), handle);
                    let _ = result_tx.send(DecodeResult {
                        clip_id,
                        status: DecodeResultStatus::Opened {
                            duration,
                            width,
                            height,
                            frame_rate,
                        },
                    });
                }
                Err(e) => {
                    let _ = result_tx.send(DecodeResult {
                        clip_id,
                        status: DecodeResultStatus::Error(format!("open failed: {e}")),
                    });
                }
            },

            DecodeJob::Prepare { clip_id } => {
                if let Some(handle) = active.get_mut(&clip_id) {
                    match handle.prepare() {
                        Ok(()) => {
                            let frame = FrameSource::new(handle.as_ref());
                            let _ = result_tx.send(DecodeResult {
                                clip_id,
                                status: DecodeResultStatus::Prepared { frame },
                            });
                        }
                        Err(e) => {
                            let _ = result_tx.send(DecodeResult {
                                clip_id,
                                status: DecodeResultStatus::Error(format!("prepare failed: {e}")),
                            });
                        }
                    }
                } else {
                    // No handle for this clip (e.g. the preceding Open failed on a
                    // missing/corrupt file). The content thread's decode_pending
                    // invariant assumes every submitted job produces exactly one
                    // result — reply with an error so it always resolves instead
                    // of hanging forever (BUG-127).
                    let _ = result_tx.send(DecodeResult {
                        clip_id: clip_id.clone(),
                        status: DecodeResultStatus::Error(format!(
                            "no handle for clip {clip_id} (Prepare)"
                        )),
                    });
                }
            }

            DecodeJob::Seek {
                clip_id,
                target_time,
            } => {
                if let Some(handle) = active.get_mut(&clip_id) {
                    match handle.seek_to(target_time) {
                        Ok(()) => {
                            let frame_time = handle.frame_time();
                            let frame = FrameSource::new(handle.as_ref());
                            let _ = result_tx.send(DecodeResult {
                                clip_id,
                                status: DecodeResultStatus::Seeked { frame_time, frame },
                            });
                        }
                        Err(e) => {
                            let _ = result_tx.send(DecodeResult {
                                clip_id,
                                status: DecodeResultStatus::Error(format!("seek failed: {e}")),
                            });
                        }
                    }
                } else {
                    // See Prepare's else-branch: no handle means the job would
                    // otherwise be silently dropped, wedging decode_pending forever.
                    let _ = result_tx.send(DecodeResult {
                        clip_id: clip_id.clone(),
                        status: DecodeResultStatus::Error(format!(
                            "no handle for clip {clip_id} (Seek)"
                        )),
                    });
                }
            }

            DecodeJob::DecodeNext { clip_id } => {
                if let Some(handle) = active.get_mut(&clip_id) {
                    match handle.decode_next_frame() {
                        Ok(DecodeStatus::FrameReady) => {
                            let frame_time = handle.frame_time();
                            let frame = FrameSource::new(handle.as_ref());
                            let _ = result_tx.send(DecodeResult {
                                clip_id,
                                status: DecodeResultStatus::FrameReady { frame_time, frame },
                            });
                        }
                        Ok(DecodeStatus::EndOfFile) => {
                            let _ = result_tx.send(DecodeResult {
                                clip_id,
                                status: DecodeResultStatus::EndOfFile,
                            });
                        }
                        Err(e) => {
                            let _ = result_tx.send(DecodeResult {
                                clip_id,
                                status: DecodeResultStatus::Error(format!("decode failed: {e}")),
                            });
                        }
                    }
                } else {
                    // See Prepare's else-branch: no handle means the job would
                    // otherwise be silently dropped, wedging decode_pending forever.
                    let _ = result_tx.send(DecodeResult {
                        clip_id: clip_id.clone(),
                        status: DecodeResultStatus::Error(format!(
                            "no handle for clip {clip_id} (DecodeNext)"
                        )),
                    });
                }
            }

            DecodeJob::Close { clip_id } => {
                active.remove(&clip_id);
            }

            DecodeJob::WarmOpen {
                video_clip_id,
                path,
                width,
                height,
            } => {
                if warm.contains_key(&video_clip_id) {
                    continue;
                }
                match backend.open(&path, width, height) {
                    Ok(mut handle) => {
                        if handle.prepare().is_ok() {
                            warm.insert(video_clip_id.clone(), handle);
                            let _ = result_tx.send(DecodeResult {
                                clip_id: video_clip_id.clone(),
                                status: DecodeResultStatus::WarmReady { video_clip_id },
                            });
                        }
                    }
                    Err(e) => {
                        log::warn!("[DecodeWorker] Warm open failed for {video_clip_id}: {e}");
                    }
                }
            }

            DecodeJob::Shutdown => {
                active.clear();
                warm.clear();
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_gpu::{
        BufferUse, GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    };
    use std::time::Duration;

    // BUG-127: a job for a clip_id the worker never opened a handle for
    // (e.g. the preceding `Open` failed) must still produce exactly one
    // result, so `decode_pending` on the content side always resolves
    // instead of hanging forever.

    fn scheduler() -> DecodeScheduler {
        DecodeScheduler::new(
            crate::decoder::platform_backend().expect("failed to create the video decoder"),
        )
    }

    #[test]
    fn prepare_for_unknown_clip_replies_with_error_not_silence() {
        let sched = scheduler();
        sched.submit(DecodeJob::Prepare {
            clip_id: "never-opened".to_string(),
        });
        let results = sched.recv_results_timeout(Duration::from_secs(5));
        assert_eq!(
            results.len(),
            1,
            "expected exactly one reply, got none (hang)"
        );
        assert!(
            matches!(results[0].status, DecodeResultStatus::Error(_)),
            "expected an Error status for a job with no handle"
        );
        assert_eq!(results[0].clip_id, "never-opened");
    }

    #[test]
    fn seek_for_unknown_clip_replies_with_error_not_silence() {
        let sched = scheduler();
        sched.submit(DecodeJob::Seek {
            clip_id: "never-opened".to_string(),
            target_time: 0.0,
        });
        let results = sched.recv_results_timeout(Duration::from_secs(5));
        assert_eq!(
            results.len(),
            1,
            "expected exactly one reply, got none (hang)"
        );
        assert!(matches!(results[0].status, DecodeResultStatus::Error(_)));
    }

    #[test]
    fn decode_next_for_unknown_clip_replies_with_error_not_silence() {
        let sched = scheduler();
        sched.submit(DecodeJob::DecodeNext {
            clip_id: "never-opened".to_string(),
        });
        let results = sched.recv_results_timeout(Duration::from_secs(5));
        assert_eq!(
            results.len(),
            1,
            "expected exactly one reply, got none (hang)"
        );
        assert!(matches!(results[0].status, DecodeResultStatus::Error(_)));
    }

    /// The one reply to the job just submitted.
    fn reply(sched: &DecodeScheduler) -> DecodeResultStatus {
        let mut results = sched.recv_results_timeout(Duration::from_secs(5));
        assert_eq!(results.len(), 1, "one reply per job");
        match results.remove(0).status {
            DecodeResultStatus::Error(e) => panic!("{e}"),
            status => status,
        }
    }

    /// `texture`'s pixels as linear RGBA.
    fn read_linear(device: &GpuDevice, texture: &GpuTexture) -> Vec<[f32; 4]> {
        let (w, h) = (texture.width, texture.height);
        let bpp = texture.format.bytes_per_pixel();
        let buf = device.create_buffer(u64::from(w * h * bpp), BufferUse::Readback);
        let mut enc = device.create_encoder("video test readback");
        enc.copy_texture_to_buffer(texture, &buf, w, h, w * bpp);
        enc.commit_and_wait().expect("readback");
        let ptr = buf.mapped_ptr().expect("readback buffers are mapped");
        let bytes = unsafe { std::slice::from_raw_parts(ptr.cast_const(), (w * h * bpp) as usize) };
        let srgb = |v: f32| {
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        match texture.format {
            GpuTextureFormat::Rgba16Float => bytes
                .chunks_exact(8)
                .map(|p| {
                    std::array::from_fn(|c| {
                        half::f16::from_le_bytes([p[2 * c], p[2 * c + 1]]).to_f32()
                    })
                })
                .collect(),
            GpuTextureFormat::Rgba8UnormSrgb => bytes
                .chunks_exact(4)
                .map(|p| {
                    std::array::from_fn(|c| {
                        let v = f32::from(p[c]) / 255.0;
                        if c == 3 { v } else { srgb(v) }
                    })
                })
                .collect(),
            other => panic!("no readback for {other:?}"),
        }
    }

    // The platform decoder on a 64x36 clip, red on the left and blue on the
    // right, five frames at 10 fps: its frames land fitted in a square
    // texture, play to the end in order, and a seek lands at the target.
    #[test]
    fn a_clip_decodes_fitted_plays_in_order_and_seeks() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/red_blue_64x36.mp4"
        );
        let device = GpuDevice::new();
        let backend = crate::decoder::platform_backend().expect("video decoder");
        let texture = device.create_texture(&GpuTextureDesc {
            width: 64,
            height: 64,
            depth: 1,
            format: backend.target_format(),
            dimension: GpuTextureDimension::D2,
            usage: backend.target_usage(),
            label: "video test",
            mip_levels: 1,
        });
        let sched = DecodeScheduler::new(backend);
        let clip = || "clip".to_string();

        sched.submit(DecodeJob::Open {
            clip_id: clip(),
            path: path.to_string(),
            width: 64,
            height: 64,
        });
        let DecodeResultStatus::Opened {
            duration,
            width,
            height,
            frame_rate,
        } = reply(&sched)
        else {
            panic!("expected Opened");
        };
        assert_eq!((width, height), (64, 36));
        assert!((duration - 0.5).abs() < 0.02, "{duration}");
        assert!((frame_rate - 10.0).abs() < 0.01, "{frame_rate}");

        sched.submit(DecodeJob::Prepare { clip_id: clip() });
        let DecodeResultStatus::Prepared { frame } = reply(&sched) else {
            panic!("expected Prepared");
        };
        unsafe { frame.copy_to(&device, &texture) }.expect("frame copy");
        let px = read_linear(&device, &texture);
        let at = |x: usize, y: usize| px[y * 64 + x];
        // The Mac's converter leaves video-range chroma unexpanded, so a
        // saturated primary lands near 0.77 there and near 1.0 through FFmpeg.
        let (red, blue) = (at(16, 32), at(48, 32));
        assert!(red[0] > 0.7 && red[1] < 0.02 && red[2] < 0.02, "{red:?}");
        assert!(
            blue[2] > 0.7 && blue[0] < 0.02 && blue[1] < 0.02,
            "{blue:?}"
        );
        // The clip fills rows 14..50, opaque; transparent black around it.
        assert_eq!((at(32, 14)[3], at(32, 49)[3]), (1.0, 1.0));
        for y in [0, 13, 50, 63] {
            assert_eq!(at(32, y), [0.0; 4], "row {y}");
        }

        let mut times = Vec::new();
        loop {
            sched.submit(DecodeJob::DecodeNext { clip_id: clip() });
            match reply(&sched) {
                DecodeResultStatus::FrameReady { frame_time, .. } => times.push(frame_time),
                DecodeResultStatus::EndOfFile => break,
                _ => panic!("expected a frame or the end"),
            }
            assert!(times.len() <= 4, "{times:?}");
        }
        assert_eq!(times.len(), 4, "{times:?}");
        for (i, t) in times.iter().enumerate() {
            assert!((t - 0.1 * (i + 1) as f32).abs() < 0.01, "{times:?}");
        }

        sched.submit(DecodeJob::Seek {
            clip_id: clip(),
            target_time: 0.22,
        });
        let DecodeResultStatus::Seeked { frame_time, frame } = reply(&sched) else {
            panic!("expected Seeked");
        };
        // The frame on screen at 0.22 s starts at 0.2 s; a seek reports
        // the target time inside it.
        assert!((frame_time - 0.22).abs() < 0.01, "{frame_time}");
        unsafe { frame.copy_to(&device, &texture) }.expect("frame copy after the seek");
    }

    #[test]
    fn recv_results_timeout_returns_empty_when_nothing_arrives() {
        let sched = scheduler();
        let results = sched.recv_results_timeout(Duration::from_millis(50));
        assert!(results.is_empty());
    }
}
