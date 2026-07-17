//! GPU per-clip content textures (§24 5b). Paints each visible audio clip's
//! waveform into its OWN small texture and draws it as a quad inside the clip
//! body — so the waveform is part of the clip, on the GPU, rather than baked into
//! a layer-wide CPU bitmap laid over the bodies.
//!
//! The rasterizer is reused verbatim: `manifold_ui::waveform_painter::draw_waveform`
//! (the same spectral-colour / MIP / source-window logic the audio lanes use). We
//! only change *where* it paints — a per-clip buffer instead of a per-layer one.
//!
//! Textures are pooled by `ClipId`. A clip's waveform depends only on its trim /
//! warp / zoom (NOT scroll), so a fully-visible clip's fingerprint is stable as
//! the timeline scrolls → cache hit, zero re-raster / re-upload. Only edge-clipped
//! clips (whose visible window changes) re-rasterise. Entries for clips that leave
//! the visible set are dropped each frame, bounding memory.
//!
//! Layering: drawn AFTER the clip bodies (`clip_draw`) and BEFORE the overlays +
//! names, in its own pass. Reuses the same textured-quad pipeline as
//! `layer_bitmap_gpu` (rounded corners are respected by insetting the waveform
//! horizontally by the clip radius, so bars never reach the rounded corners).

use std::sync::Arc;

use ahash::{AHashMap, AHashSet};
use manifold_gpu::{
    FrameFence, GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuBuffer, GpuDevice,
    GpuEncoder, GpuFilterMode, GpuLoadAction, GpuRenderPipeline, GpuSampler, GpuSamplerDesc,
    GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    GpuVertexAttribute, GpuVertexFormat, GpuVertexLayout,
};
use manifold_foundation::ClipId;
use manifold_ui::node::{Color32, Rect};
use manifold_ui::panels::viewport::ClipScreenRect;

/// Vertex for a textured quad (matches `layer_bitmap_gpu::BitmapVertex`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ContentVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

// Same shader as the layer-bitmap path: a positioned textured quad with
// transparent-pixel discard (waveforms are sparse — only bars + centre line are
// opaque, so the rounded body shows through everywhere else).
const CONTENT_SHADER: &str = r#"
struct Globals {
    screen_size: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var t_clip: texture_2d<f32>;
@group(0) @binding(2) var s_clip: sampler;

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let ndc_x = (in.position.x / globals.screen_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (in.position.y / globals.screen_size.y) * 2.0;
    out.position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(t_clip, s_clip, in.uv);
    if color.a < 0.004 {
        discard;
    }
    return color;
}
"#;

/// One pooled per-clip waveform texture + the fingerprint of the content it holds.
struct ClipTexture {
    texture: GpuTexture,
    width: u32,
    height: u32,
    /// Hash of the geometry/source-window that produced the current pixels. When
    /// it matches the frame's recomputed value, the texture is reused as-is.
    fingerprint: u64,
}

/// One quad to draw this frame: the clip's texture key + its screen rect.
struct PendingDraw {
    clip_id: ClipId,
    rect: Rect,
}

/// Ring depth for stall-frequency relief; correctness against slot reuse
/// comes from `frame_fence`, not from this depth alone (see `guard_slot`).
const VBUF_RING_SIZE: usize = 6;
/// Max audio-clip quads drawn per frame (visible audio clips; well above any real
/// on-screen count). Extras are safely dropped; a `debug_assert` in `render`
/// trips in debug builds if the cap is ever hit so it can be raised.
const MAX_CLIP_QUADS: usize = 512;
/// A waveform texture narrower than this (logical px, after the corner inset) is
/// not worth painting — the clip is a sliver.
const MIN_CONTENT_W: f32 = 3.0;
/// Hard cap on a content texture's pixel extent (mirrors the layer-bitmap clamp).
const MAX_CONTENT_PX: u32 = 8192;

/// Manages per-clip waveform textures and draws them as positioned quads.
pub struct ClipContentGpu {
    pool: AHashMap<ClipId, ClipTexture>,
    pipeline: GpuRenderPipeline,
    sampler: GpuSampler,
    index_buf: GpuBuffer,
    vbuf_ring: [GpuBuffer; VBUF_RING_SIZE],
    vbuf_ring_idx: usize,
    /// Frame each vbuf_ring slot was last claimed at (0 = never claimed);
    /// checked/updated via `frame_fence`.
    vbuf_stamps: [u64; VBUF_RING_SIZE],
    /// Shared GPU-completion fence — `None` in the headless test harness.
    frame_fence: Option<Arc<FrameFence>>,
    /// Rate limiter for `[frame-fence]` stall logging.
    fence_wait_events: u64,
    /// CPU scratch the waveform rasteriser paints into before upload (reused).
    scratch: Vec<Color32>,
    /// Per-clip breakpoint-segment geometry, reused across clips within a
    /// frame (cleared + rebuilt per clip, capacity persists). Each entry is
    /// `(x_start_px, x_end_px, waveform_x_px, waveform_w_px, src_start,
    /// src_end, texel_count)` for one consecutive breakpoint pair — computed
    /// once, read twice: to build the fingerprint, then (only on repaint) to
    /// re-select the MIP level and draw.
    seg_scratch: Vec<(i32, i32, f32, f32, f32, f32, usize)>,
    /// This frame's draw list (reused).
    draws: Vec<PendingDraw>,
    /// This frame's seen clip ids, for pool eviction (reused). A set so eviction
    /// is O(pool) not O(pool × visible).
    seen: AHashSet<ClipId>,
}

impl ClipContentGpu {
    pub fn new(device: &GpuDevice, format: GpuTextureFormat) -> Self {
        let vertex_layout = GpuVertexLayout {
            stride: std::mem::size_of::<ContentVertex>() as u32,
            attributes: vec![
                GpuVertexAttribute {
                    format: GpuVertexFormat::Float32x2,
                    offset: 0,
                    shader_location: 0,
                },
                GpuVertexAttribute {
                    format: GpuVertexFormat::Float32x2,
                    offset: 8,
                    shader_location: 1,
                },
            ],
        };

        let blend = GpuBlendState {
            src_factor: GpuBlendFactor::SrcAlpha,
            dst_factor: GpuBlendFactor::OneMinusSrcAlpha,
            operation: GpuBlendOp::Add,
            src_alpha_factor: GpuBlendFactor::One,
            dst_alpha_factor: GpuBlendFactor::OneMinusSrcAlpha,
            alpha_operation: GpuBlendOp::Add,
        };

        let pipeline = device.create_render_pipeline_with_vertex_layout(
            CONTENT_SHADER,
            "vs_main",
            "fs_main",
            format,
            Some(blend),
            &vertex_layout,
            "Clip Content Pipeline",
        );

        let sampler = device.create_sampler(&GpuSamplerDesc {
            min_filter: GpuFilterMode::Nearest,
            mag_filter: GpuFilterMode::Nearest,
            mip_filter: GpuFilterMode::Nearest,
            ..Default::default()
        });

        let index_data: [u32; 6] = [0, 1, 2, 0, 2, 3];
        let index_buf = device.create_buffer_shared(24);
        unsafe {
            std::ptr::copy_nonoverlapping(
                index_data.as_ptr(),
                index_buf.mapped_ptr().unwrap() as *mut u32,
                6,
            );
        }

        let vbuf_size = (MAX_CLIP_QUADS * 4 * std::mem::size_of::<ContentVertex>()) as u64;
        let vbuf_ring = std::array::from_fn(|_| device.create_buffer_shared(vbuf_size));

        Self {
            pool: AHashMap::new(),
            pipeline,
            sampler,
            index_buf,
            vbuf_ring,
            vbuf_ring_idx: 0,
            vbuf_stamps: [0; VBUF_RING_SIZE],
            frame_fence: None,
            fence_wait_events: 0,
            scratch: Vec::new(),
            seg_scratch: Vec::new(),
            draws: Vec::with_capacity(MAX_CLIP_QUADS),
            seen: AHashSet::with_capacity(MAX_CLIP_QUADS),
        }
    }

    /// Install the shared GPU-completion fence used to gate vbuf ring-slot
    /// reuse. Not set by the headless test harness.
    pub fn set_frame_fence(&mut self, fence: Arc<FrameFence>) {
        self.frame_fence = Some(fence);
    }

    /// Rasterise + upload the waveform texture for every visible audio clip whose
    /// content changed, then draw them all inside their bodies. `tracks_rect`
    /// bounds the textures (so an off-screen clip extent costs nothing); `scale`
    /// is the HiDPI render scale (textures are physical-pixel sized for crispness,
    /// drawn at logical rects). Call between the clip-body pass and the overlay
    /// pass.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        target: &GpuTexture,
        screen_w: u32,
        screen_h: u32,
        scale: f32,
        tracks_rect: Rect,
        clips: &[ClipScreenRect],
    ) {
        self.draws.clear();
        self.seen.clear();

        let tx0 = tracks_rect.x;
        let tx1 = tracks_rect.x + tracks_rect.width;
        let ty0 = tracks_rect.y;
        let ty1 = tracks_rect.y + tracks_rect.height;

        for clip in clips {
            if !clip.is_audio {
                continue;
            }
            let renderer = match clip.waveform.as_ref() {
                Some(rr) => rr,
                None => continue,
            };
            let cx = clip.rect.x;
            let cw = clip.rect.width;
            if cw <= 0.0 || clip.rect.height <= 0.0 {
                continue;
            }

            // Visible intersection with the tracks rect. The waveform spans the
            // clip's FULL width (time-aligned to the body, like the old bitmap
            // path) — no horizontal corner inset: the painter's own vertical
            // padding (+ amplitude ≤ 0.45·height) keeps bars well clear of the
            // ~4px rounded corners, and corner texels are transparent → discarded,
            // so the rounded body shows through there regardless.
            let vis_x0 = cx.max(tx0);
            let vis_x1 = (cx + cw).min(tx1);
            let vis_y0 = clip.rect.y.max(ty0);
            let vis_y1 = (clip.rect.y + clip.rect.height).min(ty1);
            let draw_x0 = vis_x0;
            let draw_x1 = vis_x1;
            let draw_w = draw_x1 - draw_x0;
            let draw_h = vis_y1 - vis_y0;
            if draw_w < MIN_CONTENT_W || draw_h < 2.0 {
                continue;
            }

            let tex_w = ((draw_w * scale).round() as u32).clamp(1, MAX_CONTENT_PX);
            let tex_h = ((draw_h * scale).round() as u32).clamp(1, MAX_CONTENT_PX);

            // Source window (Ableton model), now PIECEWISE per the clip's
            // tempo-map breakpoints (`crates/manifold-app/src/ui_bridge/
            // state_sync.rs::audio_waveform_breakpoints`) instead of one
            // constant-spb window for the whole clip — a varying tempo map
            // makes seconds-per-beat non-constant, so the source window must
            // be re-derived per segment between consecutive breakpoints.
            // `x_frac` is beat-linear (matches pixel x); a constant-tempo clip
            // has exactly 2 breakpoints → one segment, identical to the old
            // single-window draw.
            let full_w_px = cw * scale; // the clip's FULL width in physical px
            let full_x_px = (cx - draw_x0) * scale; // clip-left relative to texture origin (≤ 0)
            let file_secs = renderer.clip_duration_seconds();
            if clip.waveform_breakpoints.len() < 2 || !renderer.is_ready() {
                continue; // zero-duration clip, or decode not ready yet
            }

            // Pass 1: geometry only (cheap arithmetic, no painting) — builds
            // the fingerprint. `texel_count` is folded in so a background
            // decode refinement under an unchanged window still repaints.
            self.seg_scratch.clear();
            for seg in clip.waveform_breakpoints.windows(2) {
                let (x0_frac, secs0) = seg[0];
                let (x1_frac, secs1) = seg[1];
                let seg_w_px = (x1_frac - x0_frac) * full_w_px;
                if seg_w_px <= 0.0 {
                    continue; // degenerate/duplicate breakpoint
                }
                let seg_x0_px = full_x_px + x0_frac * full_w_px;
                let x_start = (seg_x0_px.round() as i32).clamp(0, tex_w as i32);
                let x_end =
                    ((full_x_px + x1_frac * full_w_px).round() as i32).clamp(0, tex_w as i32);
                if x_end <= x_start {
                    continue; // segment fully outside the visible texture
                }
                let (src_start, src_end) = if file_secs > 0.0 {
                    (
                        (secs0 / file_secs).clamp(0.0, 1.0),
                        (secs1 / file_secs).clamp(0.0, 1.0),
                    )
                } else {
                    (0.0, 1.0)
                };
                let frac = (src_end - src_start).max(1e-4);
                // Pick the MIP at the resolution the whole file would occupy if
                // THIS segment's window were stretched to full width (so a
                // zoomed-in trim isn't coarse).
                let Some(level) = renderer.select_level_for_zoom(seg_w_px / frac, 1.0) else {
                    continue; // shouldn't happen once is_ready() passed; skip defensively
                };
                self.seg_scratch.push((
                    x_start,
                    x_end,
                    seg_x0_px,
                    seg_w_px,
                    src_start,
                    src_end,
                    level.texel_count(),
                ));
            }
            if self.seg_scratch.is_empty() {
                continue; // nothing visible/valid to paint this frame
            }

            let fp = fingerprint(tex_w, tex_h, &self.seg_scratch);

            let needs_paint = match self.pool.get(&clip.clip_id) {
                Some(t) => t.fingerprint != fp || t.width != tex_w || t.height != tex_h,
                None => true,
            };

            if needs_paint {
                // Rasterise into the scratch buffer, then upload.
                let px_count = (tex_w * tex_h) as usize;
                self.scratch.clear();
                self.scratch.resize(px_count, Color32::TRANSPARENT);

                // Pass 2: re-select each segment's MIP level (cheap, same
                // deterministic lookup as pass 1) and draw it into its own
                // pixel range — non-overlapping ranges, so the painter's
                // center-line stroke is drawn exactly once per column.
                for &(x_start, x_end, seg_x0_px, seg_w_px, src_start, src_end, _texels) in
                    &self.seg_scratch
                {
                    let frac = (src_end - src_start).max(1e-4);
                    let Some(level) = renderer.select_level_for_zoom(seg_w_px / frac, 1.0) else {
                        continue;
                    };
                    manifold_ui::waveform_painter::draw_waveform(
                        &mut self.scratch,
                        tex_w as usize,
                        tex_h as usize,
                        level,
                        x_start,
                        x_end,
                        0,
                        tex_h as i32,
                        seg_x0_px,
                        seg_w_px,
                        src_start,
                        src_end,
                    );
                }

                let recreate = match self.pool.get(&clip.clip_id) {
                    Some(t) => t.width != tex_w || t.height != tex_h,
                    None => true,
                };
                if recreate {
                    let texture = device.create_texture(&GpuTextureDesc {
                        width: tex_w,
                        height: tex_h,
                        depth: 1,
                        format: GpuTextureFormat::Rgba8UnormSrgb,
                        dimension: GpuTextureDimension::D2,
                        usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::CPU_UPLOAD,
                        label: "Clip Waveform",
                        mip_levels: 1,
                    });
                    self.pool.insert(
                        clip.clip_id.clone(),
                        ClipTexture {
                            texture,
                            width: tex_w,
                            height: tex_h,
                            fingerprint: fp,
                        },
                    );
                }

                if let Some(t) = self.pool.get_mut(&clip.clip_id) {
                    let bytes: &[u8] = unsafe {
                        std::slice::from_raw_parts(
                            self.scratch.as_ptr() as *const u8,
                            self.scratch.len() * 4,
                        )
                    };
                    device.upload_texture(&t.texture, bytes);
                    t.fingerprint = fp;
                }
            }

            self.seen.insert(clip.clip_id.clone());
            if self.draws.len() < MAX_CLIP_QUADS {
                self.draws.push(PendingDraw {
                    clip_id: clip.clip_id.clone(),
                    rect: Rect::new(draw_x0, vis_y0, draw_w, draw_h),
                });
            } else {
                // Safe (capped), but should never happen at any real on-screen
                // count — trip it loudly in debug so the cap can be raised if a
                // genuine scene exceeds it.
                debug_assert!(
                    false,
                    "ClipContentGpu: >{MAX_CLIP_QUADS} visible audio clips — dropping waveforms"
                );
            }
        }

        // Evict textures for clips no longer visible (bounds memory).
        if self.pool.len() != self.seen.len() {
            self.pool.retain(|k, _| self.seen.contains(k));
        }

        if self.draws.is_empty() {
            return;
        }

        // Write all quad vertices into the ring buffer in one batch.
        let globals: [f32; 2] = [screen_w as f32, screen_h as f32];
        let globals_bytes: &[u8] = bytemuck::bytes_of(&globals);
        let slot = self.vbuf_ring_idx;
        self.vbuf_ring_idx = (slot + 1) % VBUF_RING_SIZE;
        if let Some(fence) = &self.frame_fence {
            fence.guard_slot(
                "ClipContentGpu",
                slot,
                &mut self.vbuf_stamps[slot],
                &mut self.fence_wait_events,
            );
        }
        let vbuf = &self.vbuf_ring[slot];
        let ptr = vbuf.mapped_ptr().unwrap() as *mut ContentVertex;
        for (i, d) in self.draws.iter().enumerate() {
            let (x0, y0) = (d.rect.x, d.rect.y);
            let (x1, y1) = (d.rect.x + d.rect.width, d.rect.y + d.rect.height);
            let verts = [
                ContentVertex { position: [x0, y0], uv: [0.0, 0.0] },
                ContentVertex { position: [x1, y0], uv: [1.0, 0.0] },
                ContentVertex { position: [x1, y1], uv: [1.0, 1.0] },
                ContentVertex { position: [x0, y1], uv: [0.0, 1.0] },
            ];
            unsafe {
                std::ptr::copy_nonoverlapping(verts.as_ptr(), ptr.add(i * 4), 4);
            }
        }

        // One render pass for all clip-content quads.
        encoder.begin_render_pass(target, GpuLoadAction::Load, "Clip Content");
        for (i, d) in self.draws.iter().enumerate() {
            let t = match self.pool.get(&d.clip_id) {
                Some(t) => t,
                None => continue,
            };
            let vertex_offset = (i * 4 * std::mem::size_of::<ContentVertex>()) as u64;
            encoder.draw_in_render_pass(
                &self.pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: globals_bytes },
                    GpuBinding::Texture { binding: 1, texture: &t.texture },
                    GpuBinding::Sampler { binding: 2, sampler: &self.sampler },
                ],
                vbuf,
                vertex_offset,
                &self.index_buf,
                6,
                0,
                None,
                "Clip Content Quad",
            );
        }
        encoder.end_render_pass();
    }
}

/// Deterministic content fingerprint (no RNG — wrapping integer mix). Captures
/// everything that changes the painted pixels: texture size, and — per
/// breakpoint segment — its pixel range, its position/scale within the
/// texture, its source window, and its MIP resolution (so a tempo-map edit,
/// which changes segment count/position, and a background decode refinement,
/// which changes `texel_count` under an unchanged window, both repaint).
/// Scroll alone does NOT change any of these for a fully-visible clip → cache hit.
fn fingerprint(tex_w: u32, tex_h: u32, segments: &[(i32, i32, f32, f32, f32, f32, usize)]) -> u64 {
    let mut h: u64 = 1469598103934665603; // FNV offset basis
    let mut mix = |v: u64| {
        h ^= v;
        h = h.wrapping_mul(1099511628211);
    };
    mix(tex_w as u64);
    mix(tex_h as u64);
    mix(segments.len() as u64);
    for &(x_start, x_end, wx, ww, src_start, src_end, texel_count) in segments {
        mix(x_start as u64);
        mix(x_end as u64);
        mix(wx.round() as i64 as u64);
        mix(ww.round() as i64 as u64);
        mix(src_start.to_bits() as u64);
        mix(src_end.to_bits() as u64);
        mix(texel_count as u64);
    }
    h
}
