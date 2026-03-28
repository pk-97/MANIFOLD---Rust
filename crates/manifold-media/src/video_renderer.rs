//! VideoRenderer — ClipRenderer implementation for native video decode.
//!
//! Replaces the StubRenderer::new_video() placeholder with real hardware-accelerated
//! video decode via AVAssetReader + VideoToolbox. Uses a decode thread pool for
//! async decode and Metal compute for NV12→Rgba16Float conversion.
//!
//! Frame copy mechanism: When a decode worker completes a frame, it sends the result
//! with the raw DecoderHandle pointer. The content thread (in pre_render) calls the
//! native CopyFrameToTexture directly using pool_handle + decoder_handle + dest_texture.
//! This is safe because no decode jobs are in-flight for the clip at that point.

use std::any::Any;
use std::ffi::c_void;
use std::sync::Arc;

use ahash::AHashMap;

use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::project::Project;
use manifold_core::video::VideoLibrary;
use manifold_gpu::{GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};
use manifold_playback::renderer::ClipRenderer;

use crate::decode_scheduler::{
    DecodeJob, DecodeResultStatus, DecodeScheduler,
};
use crate::decoder::DecoderPool;
use crate::decoder_ffi;

/// Initial render target pool size.
const INITIAL_RT_POOL_SIZE: usize = 8;

/// A native Metal texture for video output.
struct VideoRenderTarget {
    texture: GpuTexture,
}

impl VideoRenderTarget {
    fn new(
        device: &GpuDevice,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        index: usize,
    ) -> Self {
        let label = format!("VideoRT_{index:02}");
        let texture = device.create_texture(&GpuTextureDesc {
            width,
            height,
            depth: 1,
            format,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL,
            label: &label,
        });
        Self { texture }
    }
}

/// Per-clip active state.
struct ActiveVideoClip {
    video_clip_id: String,
    render_target: VideoRenderTarget,
    playing: bool,
    ready: bool,
    has_frame: bool,
    playback_time: f32,
    media_length: f32,
    frame_rate: f32,
    looping: bool,
    playback_rate: f32,
    decode_pending: bool,
    /// Queued seek target when a seek arrives while decode_pending is true.
    /// Dispatched when the pending decode completes.
    pending_seek_time: Option<f32>,
    /// Accumulated time since last frame advance (for pacing decode to video fps).
    time_accumulator: f32,
}

/// Native video renderer implementing the ClipRenderer trait.
pub struct VideoRenderer {
    /// Cached pointer to GpuDevice owned by ContentPipeline (same thread, same lifetime).
    device_ptr: *const GpuDevice,
    width: u32,
    height: u32,
    format: GpuTextureFormat,
    active_clips: AHashMap<String, ActiveVideoClip>,
    scheduler: DecodeScheduler,
    available_rts: Vec<VideoRenderTarget>,
    video_library: Option<VideoLibrary>,
    rt_counter: usize,
}

// Safety: device_ptr points to GpuDevice on the content thread.
// VideoRenderer is only used on the content thread.
unsafe impl Send for VideoRenderer {}

impl VideoRenderer {
    pub fn new(
        device: &GpuDevice,
        width: u32,
        height: u32,
        format: GpuTextureFormat,
        pool_size: usize,
    ) -> Self {
        let decoder_pool = Arc::new(
            DecoderPool::new().expect("Failed to create video decoder pool"),
        );
        let scheduler = DecodeScheduler::new(decoder_pool);

        let pool_size = pool_size.max(INITIAL_RT_POOL_SIZE);
        let mut available_rts = Vec::with_capacity(pool_size);
        for i in 0..pool_size {
            available_rts.push(VideoRenderTarget::new(
                device, width, height, format, i,
            ));
        }

        Self {
            device_ptr: device as *const GpuDevice,
            width,
            height,
            format,
            active_clips: AHashMap::new(),
            scheduler,
            available_rts,
            video_library: None,
            rt_counter: pool_size,
        }
    }

    /// Set the device pointer after the GpuDevice has been moved to its
    /// final location (inside ContentPipeline). Must be called before resize
    /// or pool-exhaustion triggers new texture creation.
    pub fn set_device(&mut self, device: &GpuDevice) {
        self.device_ptr = device as *const GpuDevice;
    }

    /// Safety: device_ptr is valid for the lifetime of ContentPipeline.
    fn device(&self) -> &GpuDevice {
        unsafe { &*self.device_ptr }
    }

    /// Get the texture for a rendered clip.
    pub fn get_clip_texture(&self, clip_id: &str) -> Option<&GpuTexture> {
        self.active_clips
            .get(clip_id)
            .and_then(|c| if c.has_frame { Some(&c.render_target.texture) } else { None })
    }

    /// Submit pre-warm requests for clips near the playhead.
    pub fn pre_warm_clips(
        &mut self,
        candidates: &AHashMap<String, manifold_core::video::VideoClip>,
    ) {
        for (video_clip_id, clip) in candidates {
            let already_active = self
                .active_clips
                .values()
                .any(|c| &c.video_clip_id == video_clip_id);
            if already_active {
                continue;
            }

            self.scheduler.submit(DecodeJob::WarmOpen {
                video_clip_id: video_clip_id.clone(),
                path: clip.file_path.clone(),
            });
        }
    }

    /// Submit pre-warm requests from engine's lookahead prewarm candidates.
    /// Accepts the PrewarmCandidate type from PlaybackEngine::compute_prewarm_candidates().
    pub fn pre_warm_from_candidates(
        &mut self,
        candidates: &std::collections::HashMap<String, manifold_playback::video_time::PrewarmCandidate>,
    ) {
        for (video_clip_id, candidate) in candidates {
            let already_active = self
                .active_clips
                .values()
                .any(|c| &c.video_clip_id == video_clip_id);
            if already_active {
                continue;
            }

            self.scheduler.submit(DecodeJob::WarmOpen {
                video_clip_id: video_clip_id.clone(),
                path: candidate.file_path.clone(),
            });
        }
    }

    fn acquire_rt(&mut self) -> VideoRenderTarget {
        if let Some(rt) = self.available_rts.pop() {
            rt
        } else {
            let idx = self.rt_counter;
            self.rt_counter += 1;
            log::debug!("[VideoRenderer] Pool exhausted, creating RT_{idx:02}");
            VideoRenderTarget::new(self.device(), self.width, self.height, self.format, idx)
        }
    }

    fn release_rt(&mut self, rt: VideoRenderTarget) {
        self.available_rts.push(rt);
    }

    /// Copy the decoded frame from the native decoder to the Metal render target.
    /// Called on the content thread when a FrameReady/Prepared/Seeked result arrives.
    ///
    /// # Safety
    /// `handle_ptr` must be a valid native DecoderHandle pointer.
    /// No decode jobs may be in-flight for this clip (decode_pending must be false).
    #[cfg(target_os = "macos")]
    unsafe fn copy_frame_to_rt(
        pool: &DecoderPool,
        handle_ptr: *mut c_void,
        render_target: &VideoRenderTarget,
    ) -> bool {
        let dest_ptr = render_target.texture.raw_ptr();

        let result = unsafe {
            decoder_ffi::VideoDecoder_CopyFrameToTexture(
                pool.raw_handle(),
                handle_ptr,
                dest_ptr,
            )
        };

        if result != 0 {
            log::warn!(
                "[VideoRenderer] CopyFrameToTexture failed (code={result})"
            );
            return false;
        }

        true
    }

    #[cfg(not(target_os = "macos"))]
    unsafe fn copy_frame_to_rt(
        _pool: &DecoderPool,
        _handle_ptr: *mut c_void,
        _render_target: &VideoRenderTarget,
    ) -> bool {
        false
    }

    /// Process a batch of decode results — shared by `pre_render` and
    /// `flush_pending_decodes` to avoid duplicating the match arms.
    fn process_decode_results(
        &mut self,
        results: Vec<crate::decode_scheduler::DecodeResult>,
    ) {
        let pool = Arc::clone(self.scheduler.pool());
        for result in results {
            let clip_id = result.clip_id;
            match result.status {
                DecodeResultStatus::Opened {
                    duration, frame_rate, ..
                } => {
                    if let Some(clip) =
                        self.active_clips.get_mut(clip_id.as_str())
                    {
                        clip.media_length = duration;
                        clip.frame_rate = frame_rate.max(1.0);
                    }
                }
                DecodeResultStatus::Prepared { handle_ptr } => {
                    if let Some(clip) =
                        self.active_clips.get_mut(clip_id.as_str())
                    {
                        clip.ready = true;
                        clip.decode_pending = false;
                        if unsafe {
                            Self::copy_frame_to_rt(
                                &pool, handle_ptr, &clip.render_target,
                            )
                        } {
                            clip.has_frame = true;
                        }
                    }
                }
                DecodeResultStatus::FrameReady {
                    frame_time, handle_ptr,
                } => {
                    if let Some(clip) =
                        self.active_clips.get_mut(clip_id.as_str())
                    {
                        clip.playback_time = frame_time;
                        clip.decode_pending = false;
                        if unsafe {
                            Self::copy_frame_to_rt(
                                &pool, handle_ptr, &clip.render_target,
                            )
                        } {
                            clip.has_frame = true;
                        }
                    }
                }
                DecodeResultStatus::EndOfFile => {
                    if let Some(clip) =
                        self.active_clips.get_mut(clip_id.as_str())
                    {
                        clip.decode_pending = false;
                        if clip.looping {
                            clip.decode_pending = true;
                            self.scheduler.submit(DecodeJob::Seek {
                                clip_id: clip_id.clone(),
                                target_time: 0.0,
                            });
                        } else {
                            clip.playing = false;
                        }
                    }
                }
                DecodeResultStatus::Seeked {
                    frame_time, handle_ptr,
                } => {
                    if let Some(clip) =
                        self.active_clips.get_mut(clip_id.as_str())
                    {
                        clip.playback_time = frame_time;
                        clip.decode_pending = false;
                        clip.time_accumulator = 0.0;
                        if unsafe {
                            Self::copy_frame_to_rt(
                                &pool, handle_ptr, &clip.render_target,
                            )
                        } {
                            clip.has_frame = true;
                        }
                    }
                }
                DecodeResultStatus::WarmReady { .. } => {}
                DecodeResultStatus::Error(msg) => {
                    log::error!("[VideoRenderer] Error for {clip_id}: {msg}");
                    if let Some(clip) =
                        self.active_clips.get_mut(clip_id.as_str())
                    {
                        clip.decode_pending = false;
                    }
                }
            }
        }
    }
}

impl ClipRenderer for VideoRenderer {
    fn can_handle(&self, clip: &TimelineClip) -> bool {
        !clip.is_generator()
    }

    fn start_clip(
        &mut self,
        clip: &TimelineClip,
        _current_time: f32,
        _layers: &[Layer],
    ) -> bool {
        if self.active_clips.contains_key(clip.id.as_ref()) {
            return true;
        }

        // Extract needed values from library before mutable borrow for acquire_rt
        let (path, duration) = {
            let Some(ref library) = self.video_library else {
                log::warn!("[VideoRenderer] No video library loaded");
                return false;
            };

            let Some(video_clip) = library.find_clip_by_id(&clip.video_clip_id) else {
                log::warn!(
                    "[VideoRenderer] Video clip not found: {}",
                    clip.video_clip_id
                );
                return false;
            };

            log::info!(
                "[VideoRenderer] start_clip: '{}' ({}x{}, {:.1}s)",
                video_clip.file_name,
                video_clip.resolution_width,
                video_clip.resolution_height,
                video_clip.duration,
            );

            (video_clip.file_path.clone(), video_clip.duration)
        };

        let clip_id_str: String = clip.id.as_ref().to_string();
        let rt = self.acquire_rt();

        self.active_clips.insert(
            clip_id_str.clone(),
            ActiveVideoClip {
                video_clip_id: clip.video_clip_id.clone(),
                render_target: rt,
                playing: true,
                ready: false,
                has_frame: false,
                playback_time: 0.0,
                media_length: duration,
                frame_rate: 30.0, // updated when Opened result arrives
                looping: clip.is_looping,
                playback_rate: 1.0,
                decode_pending: true,
                pending_seek_time: None,
                time_accumulator: 0.0,
            },
        );

        self.scheduler.submit(DecodeJob::Open {
            clip_id: clip_id_str.clone(),
            path,
        });
        self.scheduler.submit(DecodeJob::Prepare {
            clip_id: clip_id_str,
        });

        true
    }

    fn stop_clip(&mut self, clip_id: &str) {
        if let Some(clip) = self.active_clips.remove(clip_id) {
            self.scheduler.submit(DecodeJob::Close {
                clip_id: clip_id.to_string(),
            });
            self.release_rt(clip.render_target);
        }
    }

    fn release_all(&mut self) {
        let clip_ids: Vec<String> = self.active_clips.keys().cloned().collect();
        for clip_id in clip_ids {
            if let Some(clip) = self.active_clips.remove(&clip_id) {
                self.scheduler.submit(DecodeJob::Close { clip_id });
                self.release_rt(clip.render_target);
            }
        }
    }

    fn on_project_loaded(&mut self, project: &Project) {
        self.video_library = Some(project.video_library.clone());
    }

    fn is_clip_ready(&self, clip_id: &str) -> bool {
        self.active_clips
            .get(clip_id)
            .is_some_and(|c| c.ready && c.has_frame)
    }

    fn is_active(&self, clip_id: &str) -> bool {
        self.active_clips.contains_key(clip_id)
    }

    fn is_clip_playing(&self, clip_id: &str) -> bool {
        self.active_clips.get(clip_id).is_some_and(|c| c.playing)
    }

    fn needs_prepare_phase(&self) -> bool {
        true
    }

    fn needs_drift_correction(&self) -> bool {
        true
    }

    fn needs_pending_pause(&self) -> bool {
        true
    }

    fn get_clip_playback_time(&self, clip_id: &str) -> f32 {
        self.active_clips.get(clip_id).map_or(0.0, |c| c.playback_time)
    }

    fn get_clip_media_length(&self, clip_id: &str) -> f32 {
        self.active_clips.get(clip_id).map_or(0.0, |c| c.media_length)
    }

    fn resume_clip(&mut self, clip_id: &str) {
        if let Some(clip) = self.active_clips.get_mut(clip_id) {
            clip.playing = true;
            clip.time_accumulator = 0.0;
        }
    }

    fn pause_clip(&mut self, clip_id: &str) {
        if let Some(clip) = self.active_clips.get_mut(clip_id) {
            clip.playing = false;
        }
    }

    fn seek_clip(&mut self, clip_id: &str, video_time: f32) {
        if let Some(clip) = self.active_clips.get_mut(clip_id) {
            clip.playback_time = video_time;
            if clip.decode_pending {
                // Coalesce: worker is busy, queue the latest target.
                // Will be dispatched when the pending decode completes.
                clip.pending_seek_time = Some(video_time);
            } else {
                clip.decode_pending = true;
                clip.pending_seek_time = None;
                self.scheduler.submit(DecodeJob::Seek {
                    clip_id: clip_id.to_string(),
                    target_time: video_time,
                });
            }
        }
    }

    fn set_clip_looping(&mut self, clip_id: &str, looping: bool) {
        if let Some(clip) = self.active_clips.get_mut(clip_id) {
            clip.looping = looping;
        }
    }

    fn set_clip_playback_rate(&mut self, clip_id: &str, rate: f32) {
        if let Some(clip) = self.active_clips.get_mut(clip_id) {
            clip.playback_rate = rate.clamp(0.05, 8.0);
        }
    }

    fn pre_render(&mut self, _time: f32, _beat: f32, dt: f32) {
        // 1. Drain decode results and update clip state
        let results = self.scheduler.drain_results();
        self.process_decode_results(results);

        // 2. Dispatch any queued seeks that were coalesced while decode was pending.
        let pending: Vec<(String, f32)> = self.active_clips.iter_mut()
            .filter_map(|(id, clip)| {
                if !clip.decode_pending && clip.pending_seek_time.is_some() {
                    let t = clip.pending_seek_time.take().unwrap();
                    clip.decode_pending = true;
                    clip.playback_time = t;
                    Some((id.clone(), t))
                } else {
                    None
                }
            })
            .collect();
        for (clip_id, target_time) in pending {
            self.scheduler.submit(DecodeJob::Seek { clip_id, target_time });
        }

        // 3. Pacing: request next frame for playing clips based on video frame rate.
        // Accumulate dt and submit DecodeNext when enough time has elapsed for
        // the next video frame. This prevents the decoder from running at full
        // speed and flooding the result queue.
        let clip_ids: Vec<String> = self.active_clips.keys().cloned().collect();
        for clip_id in &clip_ids {
            let Some(clip) = self.active_clips.get_mut(clip_id.as_str()) else {
                continue;
            };
            if !clip.playing || !clip.ready || clip.decode_pending {
                continue;
            }

            clip.time_accumulator += dt * clip.playback_rate;
            clip.playback_time += dt * clip.playback_rate;
            let frame_interval = 1.0 / clip.frame_rate;

            if clip.time_accumulator >= frame_interval {
                clip.time_accumulator -= frame_interval;

                if clip.time_accumulator > frame_interval * 3.0 {
                    // Too far behind — seek to skip frames instead of sequential decode
                    let skip_time = clip.playback_time;
                    clip.time_accumulator = 0.0;
                    clip.decode_pending = true;
                    self.scheduler.submit(DecodeJob::Seek {
                        clip_id: clip_id.clone(),
                        target_time: skip_time,
                    });
                } else {
                    if clip.time_accumulator > frame_interval * 2.0 {
                        clip.time_accumulator = 0.0;
                    }
                    clip.decode_pending = true;
                    self.scheduler.submit(DecodeJob::DecodeNext {
                        clip_id: clip_id.clone(),
                    });
                }
            }
        }
    }

    fn resize(&mut self, width: i32, height: i32) {
        let w = width.max(1) as u32;
        let h = height.max(1) as u32;
        if self.width == w && self.height == h {
            return;
        }
        self.width = w;
        self.height = h;

        // Safety: device_ptr is valid for the lifetime of ContentPipeline.
        let device = unsafe { &*self.device_ptr };
        let fmt = self.format;

        self.available_rts.clear();
        for i in 0..INITIAL_RT_POOL_SIZE {
            self.available_rts.push(VideoRenderTarget::new(device, w, h, fmt, i));
        }
        self.rt_counter = INITIAL_RT_POOL_SIZE;

        for clip in self.active_clips.values_mut() {
            clip.render_target =
                VideoRenderTarget::new(device, w, h, fmt, self.rt_counter);
            self.rt_counter += 1;
            clip.has_frame = false;
        }
    }

    fn has_pending_decodes(&self) -> bool {
        self.active_clips.values().any(|c| c.decode_pending)
    }

    fn flush_pending_decodes(&mut self) {
        while self.active_clips.values().any(|c| c.decode_pending) {
            let results = self.scheduler.recv_results_blocking();
            if results.is_empty() {
                break; // Channel disconnected
            }
            self.process_decode_results(results);
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
