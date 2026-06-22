//! ImageRenderer — ClipRenderer implementation for static image clips.
//!
//! A still image dropped onto a Video layer becomes an image clip: a clip
//! that displays one decoded image, aspect-fit into the canvas, for its
//! timeline duration. This is the "one-frame video" path — the compositor
//! samples a per-clip `GpuTexture` exactly as it does for video, so no
//! compositor changes are needed (see `docs` / the seam through
//! `ClipRenderer::get_clip_texture` + `CompositeClipDescriptor`).
//!
//! Decode happens on a background thread (`std::thread::spawn` + a shared
//! crossbeam channel) so the 60 FPS content thread never stalls on an
//! image decode. The decoded RGBA is aspect-fit + letterboxed into a
//! canvas-sized buffer on that same thread, then uploaded to a
//! canvas-sized `Rgba8Unorm` texture on the content thread in `pre_render`
//! via the synchronous `GpuDevice::upload_texture` blit (no encoder
//! needed). Until the upload lands, `get_clip_texture` returns `None` and
//! the compositor simply skips the clip for those few frames.
//!
//! On resize the canvas dimensions change, so each active clip re-decodes
//! and re-fits at the new size. Resize is rare (window resize), so a brief
//! re-decode hitch mirrors `VideoRenderer`'s resize behaviour.

use std::any::Any;

use ahash::AHashMap;
use crossbeam_channel::{Receiver, Sender};

use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::{Beats, ClipId, Seconds};
use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use manifold_playback::renderer::ClipRenderer;

/// A decoded + canvas-fit image ready for GPU upload. `width`/`height`
/// equal the canvas dimensions the decode was issued for; `rgba` is
/// exactly `width * height * 4` bytes (RGBA8, transparent letterbox).
#[cfg_attr(test, derive(Debug))]
struct FittedImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// A background-thread decode result, tagged with the clip it belongs to.
struct DecodeResult {
    clip_id: ClipId,
    /// Canvas dims this decode targeted — lets the content thread discard a
    /// result that a resize has already made stale.
    target_w: u32,
    target_h: u32,
    result: Result<FittedImage, String>,
}

/// Per-clip active state.
struct ActiveImageClip {
    /// Absolute path to the source image. Retained so a resize can re-issue
    /// the decode at the new canvas size.
    path: String,
    /// Canvas-sized texture holding the fit image. `None` until the first
    /// decode lands.
    texture: Option<GpuTexture>,
    /// True once a decode has been uploaded into `texture`.
    has_frame: bool,
    /// True while a background decode is in flight for this clip.
    decode_pending: bool,
}

/// Static-image renderer implementing the ClipRenderer trait.
pub struct ImageRenderer {
    /// Cached pointer to the GpuDevice owned by ContentPipeline (same
    /// thread, same lifetime) — mirrors `VideoRenderer`.
    device_ptr: *const GpuDevice,
    width: u32,
    height: u32,
    active_clips: AHashMap<ClipId, ActiveImageClip>,
    result_tx: Sender<DecodeResult>,
    result_rx: Receiver<DecodeResult>,
}

// Safety: device_ptr points to GpuDevice on the content thread.
// ImageRenderer is only used on the content thread. The background decode
// threads never touch the device — they only read a path and build a Vec.
unsafe impl Send for ImageRenderer {}

impl ImageRenderer {
    pub fn new(device: &GpuDevice, width: u32, height: u32) -> Self {
        let (result_tx, result_rx) = crossbeam_channel::unbounded();
        Self {
            device_ptr: device as *const GpuDevice,
            width: width.max(1),
            height: height.max(1),
            active_clips: AHashMap::new(),
            result_tx,
            result_rx,
        }
    }

    /// Reset the device pointer after the GpuDevice has been moved to its
    /// final location (inside ContentPipeline). Mirrors `VideoRenderer`.
    pub fn set_device(&mut self, device: &GpuDevice) {
        self.device_ptr = device as *const GpuDevice;
    }

    /// Get the texture for a loaded image clip. `None` until decode lands.
    pub fn get_clip_texture(&self, clip_id: &str) -> Option<&GpuTexture> {
        self.active_clips.get(clip_id).and_then(|c| {
            if c.has_frame {
                c.texture.as_ref()
            } else {
                None
            }
        })
    }

    /// Spawn a background decode for `clip_id` at the current canvas size.
    fn spawn_decode(&self, clip_id: ClipId, path: String) {
        let tx = self.result_tx.clone();
        let (w, h) = (self.width, self.height);
        std::thread::spawn(move || {
            let result = decode_and_fit(&path, w, h);
            // Receiver lives as long as the renderer; a send error only
            // happens at shutdown, where dropping the result is correct.
            let _ = tx.send(DecodeResult {
                clip_id,
                target_w: w,
                target_h: h,
                result,
            });
        });
    }

    /// Ensure `clip.texture` is a canvas-sized texture of the given dims,
    /// (re)creating it if absent or the wrong size.
    fn ensure_texture(clip: &mut ActiveImageClip, device: &GpuDevice, w: u32, h: u32) {
        let right_size = clip
            .texture
            .as_ref()
            .is_some_and(|t| t.width == w && t.height == h);
        if right_size {
            return;
        }
        clip.texture = Some(device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
            label: "ImageClip",
            mip_levels: 1,
        }));
    }

    /// Apply one decode result: upload into the clip's texture if it is
    /// still active and the result matches the current canvas size.
    fn apply_result(&mut self, res: DecodeResult) {
        // A resize since the decode was issued makes this result stale.
        if res.target_w != self.width || res.target_h != self.height {
            return;
        }
        // Copy the raw pointer so `device` borrows the GpuDevice, not `self`
        // — otherwise the immutable self-borrow would conflict with the
        // mutable `active_clips` borrow below.
        let device: &GpuDevice = unsafe { &*self.device_ptr };
        let Some(clip) = self.active_clips.get_mut(res.clip_id.as_str()) else {
            return; // clip stopped while decoding
        };
        clip.decode_pending = false;
        match res.result {
            Ok(img) => {
                Self::ensure_texture(clip, device, img.width, img.height);
                if let Some(tex) = &clip.texture {
                    device.upload_texture(tex, &img.rgba);
                    clip.has_frame = true;
                }
            }
            Err(e) => {
                log::error!("[ImageRenderer] decode failed for {}: {e}", clip.path);
            }
        }
    }
}

impl ClipRenderer for ImageRenderer {
    fn can_handle(&self, clip: &TimelineClip) -> bool {
        clip.is_image()
    }

    fn start_clip(
        &mut self,
        clip: &TimelineClip,
        _current_time: Seconds,
        _layers: &[Layer],
        _layer_index: i32,
    ) -> bool {
        if self.active_clips.contains_key(clip.id.as_ref()) {
            return true;
        }
        if clip.image_path.is_empty() {
            return false;
        }
        self.active_clips.insert(
            clip.id.clone(),
            ActiveImageClip {
                path: clip.image_path.clone(),
                texture: None,
                has_frame: false,
                decode_pending: true,
            },
        );
        self.spawn_decode(clip.id.clone(), clip.image_path.clone());
        true
    }

    fn stop_clip(&mut self, clip_id: &str) {
        self.active_clips.remove(clip_id);
    }

    fn release_all(&mut self) {
        self.active_clips.clear();
    }

    fn is_clip_ready(&self, clip_id: &str) -> bool {
        // "Ready" means a texture exists to show. Decode proceeds via
        // pre_render regardless, so this only gates compositor inclusion.
        self.active_clips.get(clip_id).is_some_and(|c| c.has_frame)
    }

    fn is_active(&self, clip_id: &str) -> bool {
        self.active_clips.contains_key(clip_id)
    }

    fn is_clip_playing(&self, clip_id: &str) -> bool {
        // A static image is always "playing" while active (mirrors generators).
        self.active_clips.contains_key(clip_id)
    }

    fn needs_prepare_phase(&self) -> bool {
        false
    }
    fn needs_drift_correction(&self) -> bool {
        false
    }
    fn needs_pending_pause(&self) -> bool {
        false
    }

    fn get_clip_playback_time(&self, _clip_id: &str) -> f32 {
        0.0
    }
    fn get_clip_media_length(&self, _clip_id: &str) -> f32 {
        0.0
    }

    fn resume_clip(&mut self, _clip_id: &str) {}
    fn pause_clip(&mut self, _clip_id: &str) {}
    fn seek_clip(&mut self, _clip_id: &str, _video_time: f32) {}
    fn set_clip_looping(&mut self, _clip_id: &str, _looping: bool) {}
    fn set_clip_playback_rate(&mut self, _clip_id: &str, _rate: f32) {}

    fn pre_render(&mut self, _time: Seconds, _beat: Beats, _dt: f32) {
        // Drain any completed background decodes and upload them. Bounded
        // by the number of clips that started this frame, so no per-frame
        // allocation churn beyond the decode results themselves.
        while let Ok(res) = self.result_rx.try_recv() {
            self.apply_result(res);
        }
    }

    fn resize(&mut self, width: i32, height: i32) {
        let w = (width.max(1)) as u32;
        let h = (height.max(1)) as u32;
        if self.width == w && self.height == h {
            return;
        }
        self.width = w;
        self.height = h;
        // Re-decode every active clip at the new canvas size. The old
        // texture stays on screen until the new one lands (has_frame stays
        // true), so resize doesn't flash black.
        let pending: Vec<(ClipId, String)> = self
            .active_clips
            .iter_mut()
            .map(|(id, clip)| {
                clip.decode_pending = true;
                (id.clone(), clip.path.clone())
            })
            .collect();
        for (id, path) in pending {
            self.spawn_decode(id, path);
        }
    }

    fn has_pending_decodes(&self) -> bool {
        self.active_clips.values().any(|c| c.decode_pending)
    }

    fn flush_pending_decodes(&mut self) {
        // Block until every in-flight decode has been applied. Used by the
        // engine's export/prepare flush path.
        while self.active_clips.values().any(|c| c.decode_pending) {
            match self.result_rx.recv() {
                Ok(res) => self.apply_result(res),
                Err(_) => break, // all senders dropped
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// Decode an image file and aspect-fit it, letterboxed, into a transparent
/// `target_w × target_h` RGBA8 canvas. Runs on a background thread.
fn decode_and_fit(path: &str, target_w: u32, target_h: u32) -> Result<FittedImage, String> {
    if target_w == 0 || target_h == 0 {
        return Err("zero canvas size".into());
    }
    let t0 = std::time::Instant::now();
    let img = image::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let decode_ms = t0.elapsed().as_secs_f32() * 1000.0;
    let (nw, nh) = (img.width(), img.height());
    // Fit within the canvas preserving aspect ratio (never upscales past
    // the canvas; smaller images are centered at native size). Triangle
    // (bilinear) over Lanczos3: a large source (e.g. a 24MP photo) takes
    // multiple seconds to downscale with Lanczos3's wide kernel, stalling
    // the clip's first paint. Triangle is ~3x fewer samples and visually
    // indistinguishable for a still shown at output resolution.
    let fitted = img.resize(target_w, target_h, image::imageops::FilterType::Triangle);
    let fw = fitted.width();
    let fh = fitted.height();
    let total_ms = t0.elapsed().as_secs_f32() * 1000.0;
    log::info!(
        "[ImageRenderer] fit '{path}': native {nw}x{nh} (aspect {:.3}) -> fitted {fw}x{fh} centered in canvas {target_w}x{target_h} (aspect {:.3}); decode {decode_ms:.0}ms, +resize {:.0}ms",
        nw as f32 / nh.max(1) as f32,
        target_w as f32 / target_h.max(1) as f32,
        total_ms - decode_ms,
    );
    let src = fitted.to_rgba8();
    let src = src.as_raw();

    let cw = target_w as usize;
    let ch = target_h as usize;
    let mut canvas = vec![0u8; cw * ch * 4];
    let ox = ((target_w.saturating_sub(fw)) / 2) as usize;
    let oy = ((target_h.saturating_sub(fh)) / 2) as usize;
    let src_row = fw as usize * 4;
    let dst_row = cw * 4;
    for y in 0..fh as usize {
        let s = y * src_row;
        let d = (oy + y) * dst_row + ox * 4;
        canvas[d..d + src_row].copy_from_slice(&src[s..s + src_row]);
    }
    Ok(FittedImage {
        width: target_w,
        height: target_h,
        rgba: canvas,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_and_fit_rejects_zero_canvas() {
        assert!(decode_and_fit("/nonexistent.png", 0, 100).is_err());
        assert!(decode_and_fit("/nonexistent.png", 100, 0).is_err());
    }

    #[test]
    fn decode_and_fit_reports_missing_file() {
        let err = decode_and_fit("/definitely/not/here.png", 64, 64).unwrap_err();
        assert!(err.contains("open"));
    }

    #[test]
    fn fit_letterboxes_into_exact_canvas() {
        // Encode a 2x1 red PNG in memory, write to a temp file, fit to 4x4.
        use image::{ImageBuffer, Rgba};
        let mut buf: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(2, 1);
        for p in buf.pixels_mut() {
            *p = Rgba([255, 0, 0, 255]);
        }
        let dir = std::env::temp_dir();
        let path = dir.join("manifold_image_renderer_test_2x1.png");
        buf.save(&path).unwrap();

        let fit = decode_and_fit(path.to_str().unwrap(), 4, 4).unwrap();
        assert_eq!(fit.width, 4);
        assert_eq!(fit.height, 4);
        assert_eq!(fit.rgba.len(), 4 * 4 * 4);
        // A 2:1 source fit into 4x4 → 4x2 image centered vertically, so the
        // top and bottom rows are transparent (alpha 0).
        let top_left_alpha = fit.rgba[3];
        assert_eq!(top_left_alpha, 0, "top row should be transparent letterbox");

        std::fs::remove_file(&path).ok();
    }
}
