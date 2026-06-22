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
use std::sync::Arc;

use ahash::AHashMap;
use crossbeam_channel::{Receiver, Sender};

use manifold_core::clip::TimelineClip;
use manifold_core::layer::Layer;
use manifold_core::{Beats, ClipId, Seconds};
use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use manifold_playback::renderer::ClipRenderer;

/// The full-resolution decoded source, RGBA8. Decoded from disk exactly
/// once per clip and cached so a canvas resize re-fits from memory instead
/// of re-opening and re-decoding the file (disk decode dominates cost).
#[cfg_attr(test, derive(Debug))]
struct NativeImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

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
    /// The freshly decoded source, present only on a from-disk decode so the
    /// content thread can cache it. `None` on a re-fit from cached pixels.
    native: Option<Arc<NativeImage>>,
    result: Result<FittedImage, String>,
}

/// Per-clip active state.
struct ActiveImageClip {
    /// Absolute path to the source image. Retained so a resize can re-fit
    /// (or, if the first decode hasn't landed yet, re-decode) the clip.
    path: String,
    /// Cached full-resolution decode, shared with the re-fit worker. `None`
    /// until the first from-disk decode lands.
    native: Option<Arc<NativeImage>>,
    /// Canvas-sized texture holding the fit image. `None` until the first
    /// decode lands.
    texture: Option<GpuTexture>,
    /// True once a decode has been uploaded into `texture`.
    has_frame: bool,
    /// True while a background decode/re-fit is in flight for this clip.
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

    /// Spawn a from-disk decode for `clip_id`, fitting to the current canvas.
    /// Decodes the full-resolution source and returns it for caching so later
    /// resizes never touch the disk again.
    fn spawn_decode_from_disk(&self, clip_id: ClipId, path: String) {
        let tx = self.result_tx.clone();
        let (w, h) = (self.width, self.height);
        std::thread::spawn(move || {
            let msg = match decode_native(&path) {
                Ok(native) => {
                    let native = Arc::new(native);
                    let result = fit_native(&native, w, h);
                    DecodeResult {
                        clip_id,
                        target_w: w,
                        target_h: h,
                        native: Some(native),
                        result,
                    }
                }
                Err(e) => DecodeResult {
                    clip_id,
                    target_w: w,
                    target_h: h,
                    native: None,
                    result: Err(e),
                },
            };
            // Receiver lives as long as the renderer; a send error only
            // happens at shutdown, where dropping the result is correct.
            let _ = tx.send(msg);
        });
    }

    /// Spawn a re-fit for `clip_id` from already-decoded source pixels — used
    /// on resize. No disk access, no image decode; just a scale + letterbox.
    fn spawn_refit(&self, clip_id: ClipId, native: Arc<NativeImage>) {
        let tx = self.result_tx.clone();
        let (w, h) = (self.width, self.height);
        std::thread::spawn(move || {
            let result = fit_native(&native, w, h);
            let _ = tx.send(DecodeResult {
                clip_id,
                target_w: w,
                target_h: h,
                native: None,
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
            // sRGB-typed: the compositor samples this with a sampler, so the GPU
            // converts the sRGB-encoded PNG/JPEG bytes to linear on read, matching
            // the linear/EDR compositing pipeline (otherwise colours blow out).
            format: GpuTextureFormat::Rgba8UnormSrgb,
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
        // Cache the full-resolution decode so future resizes re-fit from
        // memory (a from-disk decode carries it; a re-fit does not).
        if res.native.is_some() {
            clip.native = res.native;
        }
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
                native: None,
                texture: None,
                has_frame: false,
                decode_pending: true,
            },
        );
        self.spawn_decode_from_disk(clip.id.clone(), clip.image_path.clone());
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
        // Re-fit every active clip to the new canvas. Re-fit from the cached
        // full-res decode when we have it (no disk, no image decode); fall
        // back to a from-disk decode only when the first decode is still in
        // flight. The old texture stays on screen until the new one lands
        // (has_frame stays true), so resize doesn't flash black.
        let work: Vec<(ClipId, Option<Arc<NativeImage>>, String)> = self
            .active_clips
            .iter_mut()
            .map(|(id, clip)| {
                clip.decode_pending = true;
                (id.clone(), clip.native.clone(), clip.path.clone())
            })
            .collect();
        for (id, native, path) in work {
            match native {
                Some(native) => self.spawn_refit(id, native),
                None => self.spawn_decode_from_disk(id, path),
            }
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

/// Decode an image file from disk to full-resolution RGBA8. The expensive,
/// once-per-clip step — `fit_native` then scales from this cached buffer on
/// every resize without re-touching the disk.
fn decode_native(path: &str) -> Result<NativeImage, String> {
    let t0 = std::time::Instant::now();
    let img = image::open(path).map_err(|e| format!("open {path}: {e}"))?;
    let rgba = img.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    log::info!(
        "[ImageRenderer] decode '{path}': {width}x{height} in {:.0}ms",
        t0.elapsed().as_secs_f32() * 1000.0,
    );
    Ok(NativeImage {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

/// Aspect-fit `native`, letterboxed, into a transparent `target_w × target_h`
/// RGBA8 canvas. Pure CPU scale from cached pixels — no disk, no decode.
fn fit_native(native: &NativeImage, target_w: u32, target_h: u32) -> Result<FittedImage, String> {
    if target_w == 0 || target_h == 0 {
        return Err("zero canvas size".into());
    }
    let (nw, nh) = (native.width, native.height);
    let src_buf: image::RgbaImage = image::ImageBuffer::from_raw(nw, nh, native.rgba.clone())
        .ok_or_else(|| "native buffer size mismatch".to_string())?;

    // Fit within the canvas preserving aspect ratio (scales up or down so the
    // longest edge meets the canvas, matching DynamicImage::resize). Triangle
    // (bilinear) over Lanczos3: a large source takes multiple seconds to
    // downscale with Lanczos3's wide kernel; Triangle is ~3x fewer samples and
    // visually indistinguishable for a still shown at output resolution.
    let t0 = std::time::Instant::now();
    let scale = (target_w as f32 / nw.max(1) as f32).min(target_h as f32 / nh.max(1) as f32);
    let fw = ((nw as f32 * scale).round() as u32).clamp(1, target_w);
    let fh = ((nh as f32 * scale).round() as u32).clamp(1, target_h);
    let fitted = image::imageops::resize(&src_buf, fw, fh, image::imageops::FilterType::Triangle);
    log::info!(
        "[ImageRenderer] fit: native {nw}x{nh} (aspect {:.3}) -> fitted {fw}x{fh} in canvas {target_w}x{target_h} (aspect {:.3}); resize {:.0}ms",
        nw as f32 / nh.max(1) as f32,
        target_w as f32 / target_h.max(1) as f32,
        t0.elapsed().as_secs_f32() * 1000.0,
    );
    let src = fitted.as_raw();

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
    // The compositor blends layers with premultiplied alpha; image decode yields
    // straight alpha. Premultiply RGB by alpha so transparent pixels carry no
    // colour — otherwise alpha-over adds the hidden RGB and the background bleeds
    // through. Letterbox pixels are already (0,0,0,0). (Edge approximation: the
    // multiply is in sRGB space, exact at alpha 0 and 1.)
    for px in canvas.chunks_exact_mut(4) {
        let a = px[3] as u16;
        px[0] = (px[0] as u16 * a / 255) as u8;
        px[1] = (px[1] as u16 * a / 255) as u8;
        px[2] = (px[2] as u16 * a / 255) as u8;
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

    fn solid_native(w: u32, h: u32) -> NativeImage {
        NativeImage {
            width: w,
            height: h,
            rgba: vec![255u8; (w * h * 4) as usize],
        }
    }

    #[test]
    fn fit_native_rejects_zero_canvas() {
        let n = solid_native(2, 2);
        assert!(fit_native(&n, 0, 100).is_err());
        assert!(fit_native(&n, 100, 0).is_err());
    }

    #[test]
    fn decode_native_reports_missing_file() {
        let err = decode_native("/definitely/not/here.png").unwrap_err();
        assert!(err.contains("open"));
    }

    #[test]
    fn fit_premultiplies_straight_alpha() {
        // 1x1 source, straight alpha. The compositor wants premultiplied alpha,
        // so RGB must come out scaled by alpha (else the background bleeds).
        let native = NativeImage { width: 1, height: 1, rgba: vec![200, 100, 50, 128] };
        let fit = fit_native(&native, 1, 1).unwrap();
        assert_eq!(fit.rgba[0], (200u16 * 128 / 255) as u8); // 100
        assert_eq!(fit.rgba[1], (100u16 * 128 / 255) as u8); // 50
        assert_eq!(fit.rgba[2], (50u16 * 128 / 255) as u8);  // 25
        assert_eq!(fit.rgba[3], 128, "alpha is preserved straight");
    }

    #[test]
    fn fit_letterboxes_into_exact_canvas() {
        // A 2x1 source fit into 4x4 → 4x2 centered vertically, transparent
        // top/bottom rows.
        let native = solid_native(2, 1);
        let fit = fit_native(&native, 4, 4).unwrap();
        assert_eq!(fit.width, 4);
        assert_eq!(fit.height, 4);
        assert_eq!(fit.rgba.len(), 4 * 4 * 4);
        let top_left_alpha = fit.rgba[3];
        assert_eq!(top_left_alpha, 0, "top row should be transparent letterbox");
    }

    #[test]
    fn decode_then_fit_round_trips_through_a_png() {
        // Decode-once / fit-from-cache: decode a 2x1 PNG, then fit twice at
        // different canvases from the same cached native — no second decode.
        use image::{ImageBuffer, Rgba};
        let mut buf: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(2, 1);
        for p in buf.pixels_mut() {
            *p = Rgba([255, 0, 0, 255]);
        }
        let path = std::env::temp_dir().join("manifold_image_renderer_test_2x1.png");
        buf.save(&path).unwrap();

        let native = decode_native(path.to_str().unwrap()).unwrap();
        assert_eq!((native.width, native.height), (2, 1));

        let a = fit_native(&native, 4, 4).unwrap();
        assert_eq!((a.width, a.height), (4, 4));
        let b = fit_native(&native, 8, 2).unwrap();
        assert_eq!((b.width, b.height), (8, 2));

        std::fs::remove_file(&path).ok();
    }
}
