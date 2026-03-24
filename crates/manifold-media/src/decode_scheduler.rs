//! Decode thread pool and job scheduler for video playback.
//!
//! Owns a fixed pool of worker threads that perform video decode operations
//! (open, prepare, seek, decode) off the content thread. The content thread
//! submits jobs and drains results without ever blocking.

use std::ffi::c_void;
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
    Open {
        clip_id: String,
        path: String,
    },
    /// Create AVAssetReader and decode first frame.
    Prepare {
        clip_id: String,
    },
    /// Seek to a specific time (recreates reader, decodes one frame).
    Seek {
        clip_id: String,
        target_time: f32,
    },
    /// Decode the next sequential frame.
    DecodeNext {
        clip_id: String,
    },
    /// Close and release a decoder handle.
    Close {
        clip_id: String,
    },
    /// Open a video file for warm cache (keyed by video_clip_id, not clip_id).
    WarmOpen {
        video_clip_id: String,
        path: String,
    },
    /// Promote a warm decoder to active (move from warm to active by clip_id).
    #[allow(dead_code)]
    PromoteWarm {
        video_clip_id: String,
        clip_id: String,
    },
    /// Close a warm cache entry.
    #[allow(dead_code)]
    WarmClose {
        video_clip_id: String,
    },
    /// Shutdown the worker thread.
    Shutdown,
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
/// `VideoDecoder_CopyFrameToTexture` with the wgpu destination texture.
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
    Prepared {
        handle_ptr: *mut c_void,
    },
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
    WarmReady {
        video_clip_id: String,
    },
    /// An error occurred.
    Error(String),
}

// DecodeResult contains raw pointers but they're only dereferenced on the
// content thread while no decode jobs are in-flight for the same clip.
unsafe impl Send for DecodeResult {}

/// Decode scheduler owning a fixed thread pool.
///
/// The content thread submits `DecodeJob`s and drains `DecodeResult`s.
/// Worker threads own the decoder handles and perform all FFI calls.
pub struct DecodeScheduler {
    job_tx: Sender<DecodeJob>,
    result_rx: Receiver<DecodeResult>,
    workers: Vec<thread::JoinHandle<()>>,
    pool: Arc<DecoderPool>,
}

impl DecodeScheduler {
    /// Create a new scheduler with `WORKER_COUNT` threads.
    pub fn new(pool: Arc<DecoderPool>) -> Self {
        let (job_tx, job_rx) = crossbeam_channel::unbounded::<DecodeJob>();
        let (result_tx, result_rx) = crossbeam_channel::unbounded::<DecodeResult>();

        let mut workers = Vec::with_capacity(WORKER_COUNT);

        for i in 0..WORKER_COUNT {
            let rx = job_rx.clone();
            let tx = result_tx.clone();
            let pool_ref = Arc::clone(&pool);

            let handle = thread::Builder::new()
                .name(format!("manifold-decode-{i}"))
                .spawn(move || {
                    worker_loop(rx, tx, &pool_ref);
                })
                .expect("failed to spawn decode worker");

            workers.push(handle);
        }

        log::info!(
            "[DecodeScheduler] Started {} decode workers",
            WORKER_COUNT
        );

        Self {
            job_tx,
            result_rx,
            workers,
            pool,
        }
    }

    /// Submit a job to the decode thread pool.
    pub fn submit(&self, job: DecodeJob) {
        let _ = self.job_tx.send(job);
    }

    /// Drain all available results without blocking.
    /// Returns a Vec of results (may be empty).
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

    /// Get a reference to the shared decoder pool.
    pub fn pool(&self) -> &Arc<DecoderPool> {
        &self.pool
    }

    /// Shut down all worker threads.
    pub fn shutdown(&mut self) {
        // Send shutdown to all workers
        for _ in &self.workers {
            let _ = self.job_tx.send(DecodeJob::Shutdown);
        }
        // Join all workers
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
fn worker_loop(
    job_rx: Receiver<DecodeJob>,
    result_tx: Sender<DecodeResult>,
    pool: &DecoderPool,
) {
    let mut active: AHashMap<String, DecoderHandle> = AHashMap::new();
    let mut warm: AHashMap<String, DecoderHandle> = AHashMap::new();

    while let Ok(job) = job_rx.recv() {
        match job {
            DecodeJob::Open { clip_id, path } => {
                match pool.open(&path) {
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
                            status: DecodeResultStatus::Error(format!(
                                "open failed: {e}"
                            )),
                        });
                    }
                }
            }

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
                                status: DecodeResultStatus::Error(format!(
                                    "prepare failed: {e}"
                                )),
                            });
                        }
                    }
                }
            }

            DecodeJob::Seek { clip_id, target_time } => {
                if let Some(handle) = active.get_mut(&clip_id) {
                    match handle.seek_to(target_time) {
                        Ok(()) => {
                            let frame_time = handle.frame_time();
                            let handle_ptr = handle.raw_handle();
                            let _ = result_tx.send(DecodeResult {
                                clip_id,
                                status: DecodeResultStatus::Seeked { frame_time, handle_ptr },
                            });
                        }
                        Err(e) => {
                            let _ = result_tx.send(DecodeResult {
                                clip_id,
                                status: DecodeResultStatus::Error(format!(
                                    "seek failed: {e}"
                                )),
                            });
                        }
                    }
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
                                status: DecodeResultStatus::Error(format!(
                                    "decode failed: {e}"
                                )),
                            });
                        }
                    }
                }
            }

            DecodeJob::Close { clip_id } => {
                // Drop the handle — triggers VideoDecoder_Close via Drop
                active.remove(&clip_id);
            }

            DecodeJob::WarmOpen {
                video_clip_id,
                path,
            } => {
                if warm.contains_key(&video_clip_id) {
                    continue; // already warm
                }
                match pool.open(&path) {
                    Ok(mut handle) => {
                        if handle.prepare().is_ok() {
                            warm.insert(video_clip_id.clone(), handle);
                            let _ = result_tx.send(DecodeResult {
                                clip_id: video_clip_id.clone(),
                                status: DecodeResultStatus::WarmReady {
                                    video_clip_id,
                                },
                            });
                        }
                    }
                    Err(e) => {
                        log::warn!(
                            "[DecodeWorker] Warm open failed for {video_clip_id}: {e}"
                        );
                    }
                }
            }

            DecodeJob::PromoteWarm {
                video_clip_id,
                clip_id,
            } => {
                if let Some(handle) = warm.remove(&video_clip_id) {
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
                    // Also send Prepared since warm handles already have first frame
                    // (the content thread needs both Opened + Prepared signals)
                }
            }

            DecodeJob::WarmClose { video_clip_id } => {
                warm.remove(&video_clip_id);
            }

            DecodeJob::Shutdown => {
                // Drop all handles
                active.clear();
                warm.clear();
                break;
            }
        }
    }
}
