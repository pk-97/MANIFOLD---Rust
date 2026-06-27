//! GPU clip thumbnails (§24 5c). Blits a cell of the shared content→UI thumbnail
//! atlas into a generator/video clip's body, so the clip shows what it looks like
//! instead of a flat colour.
//!
//! The thumbnail FILLS the body (unlike the sparse waveform), so a plain textured
//! quad would show square corners poking over the rounded clip. The fragment
//! shader therefore masks the sampled atlas texel by the SAME rounded-rect SDF the
//! body uses — the thumbnail is clipped to the clip's rounded shape, anti-aliased
//! at the edge. Linear sampling gives a clean downscale.
//!
//! This is only the *consumer*: the caller supplies the atlas texture (the
//! UI-side import of the content thread's IOSurface), each clip's body rect, and
//! the atlas-cell UV sub-rect (computed from the atlas geometry + the clip→cell
//! layout the content thread published). Producing the atlas is the content side.
//!
//! Drawn in the same 4b′ slot as the audio waveform (`clip_content_gpu`): a clip
//! is audio → waveform, or generator/video → thumbnail, never both.

use manifold_gpu::{
    GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuBuffer, GpuDevice, GpuEncoder,
    GpuFilterMode, GpuLoadAction, GpuRenderPipeline, GpuSampler, GpuSamplerDesc, GpuTexture,
    GpuTextureFormat, GpuVertexAttribute, GpuVertexFormat, GpuVertexLayout,
};
use manifold_ui::node::Rect;

/// One filmstrip cell quad. `rect` is the on-screen sub-rect this cell fills (one
/// bar of the clip, logical px). `body_rect` is the *whole* clip body, used for the
/// rounded-rect SDF mask — so interior filmstrip cells stay square and only the
/// clip's outer corners round (a single still has `rect == body_rect`). `radius` is
/// the corner radius (logical px); `uv_min`/`uv_max` is the atlas-cell sub-rect.
#[derive(Clone, Copy)]
pub struct ThumbQuad {
    pub rect: Rect,
    pub body_rect: Rect,
    pub radius: f32,
    pub uv_min: [f32; 2],
    pub uv_max: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ThumbVertex {
    position: [f32; 2],
    uv: [f32; 2],
    /// Rounded-rect SDF params in *logical* px: (center_x, center_y, half_w, half_h).
    rect: [f32; 4],
    /// (radius, scale, pad, pad) — scale converts logical→physical for the AA width.
    misc: [f32; 4],
}

// Textured quad + rounded-rect SDF alpha mask. `@builtin(position)` in the
// fragment stage is the physical pixel centre; we divide by `scale` back to
// logical space to match the rect params, then mask the sampled atlas by the
// rounded-box SDF (AA over one logical px).
const THUMB_SHADER: &str = r#"
struct Globals { screen_size: vec2<f32> };
@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var t_atlas: texture_2d<f32>;
@group(0) @binding(2) var s_atlas: sampler;

struct VsIn {
    @location(0) position: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) rect: vec4<f32>,
    @location(3) misc: vec4<f32>,
};
struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) rect: vec4<f32>,
    @location(2) misc: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let ndc_x = (in.position.x / globals.screen_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (in.position.y / globals.screen_size.y) * 2.0;
    out.position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = in.uv;
    out.rect = in.rect;
    out.misc = in.misc;
    return out;
}

fn sd_round_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2<f32>(r, r);
    return min(max(q.x, q.y), 0.0) + length(max(q, vec2<f32>(0.0, 0.0))) - r;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let scale = max(in.misc.y, 0.0001);
    let frag_logical = in.position.xy / scale;
    let center = in.rect.xy;
    let half = in.rect.zw;
    let radius = in.misc.x;
    let d = sd_round_box(frag_logical - center, half, radius);
    // AA over ~1 logical px at the rounded edge.
    let alpha = clamp(0.5 - d, 0.0, 1.0);
    if alpha <= 0.0 {
        discard;
    }
    let color = textureSample(t_atlas, s_atlas, in.uv);
    return vec4<f32>(color.rgb, color.a * alpha);
}
"#;

const VBUF_RING_SIZE: usize = 3;
const MAX_THUMB_QUADS: usize = 512;

/// Blits thumbnail-atlas cells into clip bodies.
pub struct ClipThumbGpu {
    pipeline: GpuRenderPipeline,
    sampler: GpuSampler,
    index_buf: GpuBuffer,
    vbuf_ring: [GpuBuffer; VBUF_RING_SIZE],
    vbuf_ring_idx: usize,
}

impl ClipThumbGpu {
    pub fn new(device: &GpuDevice, format: GpuTextureFormat) -> Self {
        let vertex_layout = GpuVertexLayout {
            stride: std::mem::size_of::<ThumbVertex>() as u32,
            attributes: vec![
                GpuVertexAttribute { format: GpuVertexFormat::Float32x2, offset: 0, shader_location: 0 },
                GpuVertexAttribute { format: GpuVertexFormat::Float32x2, offset: 8, shader_location: 1 },
                GpuVertexAttribute { format: GpuVertexFormat::Float32x4, offset: 16, shader_location: 2 },
                GpuVertexAttribute { format: GpuVertexFormat::Float32x4, offset: 32, shader_location: 3 },
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
            THUMB_SHADER,
            "vs_main",
            "fs_main",
            format,
            Some(blend),
            &vertex_layout,
            "Clip Thumbnail Pipeline",
        );

        // Linear: a thumbnail is a downscaled image, so smooth sampling beats the
        // waveform path's Nearest.
        let sampler = device.create_sampler(&GpuSamplerDesc {
            min_filter: GpuFilterMode::Linear,
            mag_filter: GpuFilterMode::Linear,
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

        let vbuf_size = (MAX_THUMB_QUADS * 4 * std::mem::size_of::<ThumbVertex>()) as u64;
        let vbuf_ring = std::array::from_fn(|_| device.create_buffer_shared(vbuf_size));

        Self { pipeline, sampler, index_buf, vbuf_ring, vbuf_ring_idx: 0 }
    }

    /// Draw each quad's atlas cell into its clip body, masked to the rounded shape.
    /// All quads sample the one `atlas` texture, so it's a single batched pass.
    ///
    /// `tracks_rect` (logical px) scissor-clips the draw to the timeline tracks
    /// area, exactly as the waveform pass does — so a clip whose body extends past
    /// the viewport (scrolled off the left edge under the track headers, or under a
    /// docked panel) never paints its thumbnail outside the timeline.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        _device: &GpuDevice,
        encoder: &mut GpuEncoder,
        target: &GpuTexture,
        screen_w: u32,
        screen_h: u32,
        scale: f32,
        tracks_rect: Rect,
        atlas: &GpuTexture,
        quads: &[ThumbQuad],
    ) {
        if quads.is_empty() {
            return;
        }

        let globals: [f32; 2] = [screen_w as f32, screen_h as f32];
        let globals_bytes: &[u8] = bytemuck::bytes_of(&globals);
        let vbuf = &self.vbuf_ring[self.vbuf_ring_idx];
        self.vbuf_ring_idx = (self.vbuf_ring_idx + 1) % VBUF_RING_SIZE;
        let ptr = vbuf.mapped_ptr().unwrap() as *mut ThumbVertex;

        let mut n = 0usize;
        for q in quads {
            if n >= MAX_THUMB_QUADS || q.rect.width <= 0.0 || q.rect.height <= 0.0 {
                break;
            }
            let (x0, y0) = (q.rect.x, q.rect.y);
            let (x1, y1) = (q.rect.x + q.rect.width, q.rect.y + q.rect.height);
            // The SDF mask is the full clip body, not this cell's sub-rect, so a
            // fragment near the clip's rounded corner is masked while interior cells
            // stay square (their seams abut). A single-still quad passes the same
            // rect for both, recovering the original behaviour.
            let cx = q.body_rect.x + q.body_rect.width * 0.5;
            let cy = q.body_rect.y + q.body_rect.height * 0.5;
            let hw = q.body_rect.width * 0.5;
            let hh = q.body_rect.height * 0.5;
            // Clamp the radius to half the smaller side (a tiny clip is a circle/oval).
            let r = q.radius.min(hw).min(hh).max(0.0);
            let rect = [cx, cy, hw, hh];
            let misc = [r, scale, 0.0, 0.0];
            let (u0, v0) = (q.uv_min[0], q.uv_min[1]);
            let (u1, v1) = (q.uv_max[0], q.uv_max[1]);
            let verts = [
                ThumbVertex { position: [x0, y0], uv: [u0, v0], rect, misc },
                ThumbVertex { position: [x1, y0], uv: [u1, v0], rect, misc },
                ThumbVertex { position: [x1, y1], uv: [u1, v1], rect, misc },
                ThumbVertex { position: [x0, y1], uv: [u0, v1], rect, misc },
            ];
            unsafe {
                std::ptr::copy_nonoverlapping(verts.as_ptr(), ptr.add(n * 4), 4);
            }
            n += 1;
        }
        if n == 0 {
            return;
        }

        encoder.begin_render_pass(target, GpuLoadAction::Load, "Clip Thumbnails");
        // Scissor to the tracks rect (physical px, clamped to the target) so a clip
        // body extending past the timeline never paints over the headers / panels.
        let phys_w = (screen_w as f32 * scale).round().max(0.0);
        let phys_h = (screen_h as f32 * scale).round().max(0.0);
        let sx0 = (tracks_rect.x * scale).round().clamp(0.0, phys_w);
        let sy0 = (tracks_rect.y * scale).round().clamp(0.0, phys_h);
        let sx1 = ((tracks_rect.x + tracks_rect.width) * scale).round().clamp(0.0, phys_w);
        let sy1 = ((tracks_rect.y + tracks_rect.height) * scale).round().clamp(0.0, phys_h);
        encoder.set_scissor_rect(
            sx0 as u32,
            sy0 as u32,
            (sx1 - sx0).max(0.0) as u32,
            (sy1 - sy0).max(0.0) as u32,
        );
        for i in 0..n {
            let vertex_offset = (i * 4 * std::mem::size_of::<ThumbVertex>()) as u64;
            encoder.draw_in_render_pass(
                &self.pipeline,
                &[
                    GpuBinding::Bytes { binding: 0, data: globals_bytes },
                    GpuBinding::Texture { binding: 1, texture: atlas },
                    GpuBinding::Sampler { binding: 2, sampler: &self.sampler },
                ],
                vbuf,
                vertex_offset,
                &self.index_buf,
                6,
                0,
                None,
                "Clip Thumbnail Quad",
            );
        }
        encoder.end_render_pass();
    }
}
