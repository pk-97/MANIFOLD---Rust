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
use std::ffi::c_void;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::thread;

use ahash::AHashMap;
use crossbeam_channel::{Receiver, Sender, TryRecvError};

use crate::decoder::{DecodeStatus, DecoderHandle, DecoderPool};

/// Number of decode worker threads.
const WORKER_COUNT: usize = 4;

/// Job submitted to a decode worker.
pub enum DecodeJob {
    /// Open a video file and assign it to a clip ID.
    Open { clip_id: String, path: String },
    /// Create AVAssetReader and decode first frame.
    Prepare { clip_id: String },
    /// Seek to a specific time (recreates reader, decodes one frame).
    Seek { clip_id: String, target_time: f32 },
    /// Decode the next sequential frame.
    DecodeNext { clip_id: String },
    /// Close and release a decoder handle.
    Close { clip_id: String },
    /// Open a video file for warm cache (keyed by video_clip_id, not clip_id).
    WarmOpen { video_clip_id: String, path: String },
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
/// Variants that indicate a decoded frame is available include `handle_ptr` —
/// the raw DecoderHandle pointer. The content thread uses this to call
/// `VideoDecoder_CopyFrameToTexture` with the destination Metal texture.
/// This is safe because no decode jobs are in-flight for the clip when
/// the content thread processes the result (decode_pending flag prevents it).
pub enum DecodeResultStatus {
    /// File opened successfully — includes metadata.
    Opened {
        duration: f32,
        width: i32,
        height: i32,
        frame_rate: f32,
    },
    /// First frame decoded — decoder is prepared.
    /// `handle_ptr` is the raw native DecoderHandle for CopyFrameToTexture.
    Prepared { handle_ptr: *mut c_void },
    /// A new frame is ready at the given presentation time.
    /// `handle_ptr` is the raw native DecoderHandle for CopyFrameToTexture.
    FrameReady {
        frame_time: f32,
        handle_ptr: *mut c_void,
    },
    /// Reached end of file.
    EndOfFile,
    /// Seek completed — frame at new position is ready.
    /// `handle_ptr` is the raw native DecoderHandle for CopyFrameToTexture.
    Seeked {
        frame_time: f32,
        handle_ptr: *mut c_void,
    },
    /// Warm cache decoder is ready (keyed by video_clip_id).
    WarmReady { video_clip_id: String },
    /// An error occurred.
    Error(String),
}

// DecodeResult contains raw pointers but they're only dereferenced on the
// content thread while no decode jobs are in-flight for the same clip.
unsafe impl Send for DecodeResult {}

/// Decode scheduler owning a fixed thread pool with affinity routing.
///
/// Each worker has its own job channel. Jobs are routed by hashing the
/// clip_id so all jobs for the same clip go to the same worker (ensuring
/// the worker's local handle map stays consistent).
pub struct DecodeScheduler {
    worker_txs: Vec<Sender<DecodeJob>>,
    result_rx: Receiver<DecodeResult>,
    workers: Vec<thread::JoinHandle<()>>,
    pool: Arc<DecoderPool>,
}

impl DecodeScheduler {
    /// Create a new scheduler with `WORKER_COUNT` threads.
    pub fn new(pool: Arc<DecoderPool>) -> Self {
        let (result_tx, result_rx) = crossbeam_channel::unbounded::<DecodeResult>();

        let mut workers = Vec::with_capacity(WORKER_COUNT);
        let mut worker_txs = Vec::with_capacity(WORKER_COUNT);

        for i in 0..WORKER_COUNT {
            let (job_tx, job_rx) = crossbeam_channel::unbounded::<DecodeJob>();
            let tx = result_tx.clone();
            let pool_ref = Arc::clone(&pool);

            let handle = thread::Builder::new()
                .name(format!("manifold-decode-{i}"))
                .spawn(move || {
                    worker_loop(job_rx, tx, &pool_ref);
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
            pool,
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

    /// Block until at least one result arrives, then drain remaining.
    /// Returns empty only if the channel is disconnected.
    pub fn recv_results_blocking(&self) -> Vec<DecodeResult> {
        let mut results = Vec::new();
        // Block for the first result
        match self.result_rx.recv() {
            Ok(result) => results.push(result),
            Err(_) => return results,
        }
        // Drain any additional results that arrived
        while let Ok(result) = self.result_rx.try_recv() {
            results.push(result);
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

    /// Get a reference to the shared decoder pool.
    pub fn pool(&self) -> &Arc<DecoderPool> {
        &self.pool
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
/// Each worker owns a local map of decoder handles (active clips)
/// and warm cache handles (pre-opened for MIDI). All FFI calls happen here.
/// Affinity routing guarantees all jobs for the same clip arrive at the
/// same worker.
fn worker_loop(job_rx: Receiver<DecodeJob>, result_tx: Sender<DecodeResult>, pool: &DecoderPool) {
    let mut active: AHashMap<String, DecoderHandle> = AHashMap::new();
    let mut warm: AHashMap<String, DecoderHandle> = AHashMap::new();

    while let Ok(job) = job_rx.recv() {
        match job {
            DecodeJob::Open { clip_id, path } => match pool.open(&path) {
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
                            let handle_ptr = handle.raw_handle();
                            let _ = result_tx.send(DecodeResult {
                                clip_id,
                                status: DecodeResultStatus::Prepared { handle_ptr },
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
                            let handle_ptr = handle.raw_handle();
                            let _ = result_tx.send(DecodeResult {
                                clip_id,
                                status: DecodeResultStatus::Seeked {
                                    frame_time,
                                    handle_ptr,
                                },
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
                            let handle_ptr = handle.raw_handle();
                            let _ = result_tx.send(DecodeResult {
                                clip_id,
                                status: DecodeResultStatus::FrameReady {
                                    frame_time,
                                    handle_ptr,
                                },
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
            } => {
                if warm.contains_key(&video_clip_id) {
                    continue;
                }
                match pool.open(&path) {
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
    use std::time::Duration;

    // BUG-127: a job for a clip_id the worker never opened a handle for
    // (e.g. the preceding `Open` failed) must still produce exactly one
    // result, so `decode_pending` on the content side always resolves
    // instead of hanging forever.

    fn scheduler() -> DecodeScheduler {
        let pool = Arc::new(DecoderPool::new().expect("failed to create decoder pool"));
        DecodeScheduler::new(pool)
    }

    #[test]
    fn prepare_for_unknown_clip_replies_with_error_not_silence() {
        let sched = scheduler();
        sched.submit(DecodeJob::Prepare {
            clip_id: "never-opened".to_string(),
        });
        let results = sched.recv_results_timeout(Duration::from_secs(5));
        assert_eq!(results.len(), 1, "expected exactly one reply, got none (hang)");
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
        assert_eq!(results.len(), 1, "expected exactly one reply, got none (hang)");
        assert!(matches!(results[0].status, DecodeResultStatus::Error(_)));
    }

    #[test]
    fn decode_next_for_unknown_clip_replies_with_error_not_silence() {
        let sched = scheduler();
        sched.submit(DecodeJob::DecodeNext {
            clip_id: "never-opened".to_string(),
        });
        let results = sched.recv_results_timeout(Duration::from_secs(5));
        assert_eq!(results.len(), 1, "expected exactly one reply, got none (hang)");
        assert!(matches!(results[0].status, DecodeResultStatus::Error(_)));
    }

    #[test]
    fn recv_results_timeout_returns_empty_when_nothing_arrives() {
        let sched = scheduler();
        let results = sched.recv_results_timeout(Duration::from_millis(50));
        assert!(results.is_empty());
    }
}
