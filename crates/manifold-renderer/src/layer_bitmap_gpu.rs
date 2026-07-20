//! GPU texture management and rendering for per-layer bitmap textures.
//!
//! Each layer gets a native Metal texture uploaded from CPU pixel buffers produced by
//! `manifold_ui::bitmap_renderer::LayerBitmapRenderer`. Textures are rendered
//! as positioned quads in the viewport area via `draw_indexed`.

use std::sync::Arc;

use manifold_gpu::{
    FrameFence, GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuBuffer, GpuDevice,
    GpuEncoder, GpuFilterMode, GpuLoadAction, GpuRenderPipeline, GpuSampler, GpuSamplerDesc,
    GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    GpuVertexAttribute, GpuVertexFormat, GpuVertexLayout,
};
use manifold_ui::node::{Color32, Rect};

/// Vertex for textured quad rendering.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BitmapVertex {
    position: [f32; 2],
    uv: [f32; 2],
}

const BITMAP_SHADER: &str = r#"
struct Globals {
    screen_size: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var t_layer: texture_2d<f32>;
@group(0) @binding(2) var s_layer: sampler;

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
    // Convert pixel coordinates to NDC: (0,0) top-left, (w,h) bottom-right
    let ndc_x = (in.position.x / globals.screen_size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (in.position.y / globals.screen_size.y) * 2.0;
    out.position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.uv = in.uv;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(t_layer, s_layer, in.uv);
    // Skip fully transparent pixels (bitmap background)
    if color.a < 0.004 {
        discard;
    }
    return color;
}
"#;

/// Per-layer GPU texture.
struct LayerTexture {
    texture: GpuTexture,
    width: u32,
    height: u32,
    // No view or bind_group — manifold-gpu uses slot-based bindings
}

/// Number of ring-buffer slots. Sized for stall-frequency relief under GPU
/// backlog; correctness against slot reuse comes from `frame_fence`
/// stamping each slot claim and blocking if the GPU hasn't retired the
/// command buffer that last wrote it — not from this depth alone.
/// 16 = 8 frames of cover at this ring's 2 claims per frame (Pass 4a grid +
/// Pass 4c overview/lanes): fence logs at 4K project load
/// showed the UI queue running 4-5 frames behind, so the previous 8-slot
/// depth (4 frames) sat inside the backlog window and hit wait timeouts.
const VBUF_RING_SIZE: usize = 16;
/// Max layers per frame in the pre-allocated vertex buffer.
const MAX_LAYER_QUADS: usize = 64;

/// Manages GPU textures for all layer bitmaps and renders them as positioned quads.
pub struct LayerBitmapGpu {
    textures: Vec<Option<LayerTexture>>,
    pipeline: GpuRenderPipeline,
    sampler: GpuSampler,
    /// Pre-allocated shared index buffer: [0u32, 1, 2, 0, 2, 3] — one quad.
    index_buf: GpuBuffer,
    /// Ring-buffered vertex buffers to avoid per-frame Metal allocations.
    /// Each buffer holds MAX_LAYER_QUADS * 4 vertices.
    vbuf_ring: [GpuBuffer; VBUF_RING_SIZE],
    vbuf_ring_idx: usize,
    /// Frame each vbuf_ring slot was last claimed at (0 = never claimed).
    /// Checked against `frame_fence` before a slot is reused; this struct
    /// consumes the ring twice per frame (Pass 4a + 4c in ui_frame.rs), so
    /// depth alone can't guarantee the GPU has retired a slot's prior use.
    vbuf_stamps: [u64; VBUF_RING_SIZE],
    /// Shared GPU-completion fence — `None` in the headless test harness,
    /// which never constructs one (unchanged stamp-0 behavior).
    frame_fence: Option<Arc<FrameFence>>,
    /// Rate limiter for `[frame-fence]` stall logging (see `FrameFence::guard_slot`).
    fence_wait_events: u64,
    /// Pre-allocated scratch for draw list (reused each frame).
    draw_list: Vec<(usize, usize)>,
}

impl LayerBitmapGpu {
    pub fn new(device: &GpuDevice, format: GpuTextureFormat) -> Self {
        let vertex_layout = GpuVertexLayout {
            stride: std::mem::size_of::<BitmapVertex>() as u32,
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
            BITMAP_SHADER,
            "vs_main",
            "fs_main",
            format,
            Some(blend),
            &vertex_layout,
            "Bitmap Pipeline",
        );

        // Nearest-neighbor sampler (matches Unity FilterMode.Point)
        let sampler = device.create_sampler(&GpuSamplerDesc {
            min_filter: GpuFilterMode::Nearest,
            mag_filter: GpuFilterMode::Nearest,
            mip_filter: GpuFilterMode::Nearest,
            ..Default::default()
        });

        // Pre-allocated index buffer for one quad
        let index_data: [u32; 6] = [0, 1, 2, 0, 2, 3];
        let index_buf = device.create_buffer_shared(24); // 6 × 4 bytes
        unsafe {
            std::ptr::copy_nonoverlapping(
                index_data.as_ptr(),
                index_buf.mapped_ptr().unwrap() as *mut u32,
                6,
            );
        }

        // Pre-allocate ring-buffered vertex buffers for layer quads.
        let vbuf_size = (MAX_LAYER_QUADS * 4 * std::mem::size_of::<BitmapVertex>()) as u64;
        let vbuf_ring = std::array::from_fn(|_| device.create_buffer_shared(vbuf_size));

        Self {
            textures: Vec::new(),
            pipeline,
            sampler,
            index_buf,
            vbuf_ring,
            vbuf_ring_idx: 0,
            vbuf_stamps: [0; VBUF_RING_SIZE],
            frame_fence: None,
            fence_wait_events: 0,
            draw_list: Vec::with_capacity(MAX_LAYER_QUADS),
        }
    }

    /// Install the shared GPU-completion fence used to gate vbuf ring-slot
    /// reuse. Not set by the headless test harness.
    pub fn set_frame_fence(&mut self, fence: Arc<FrameFence>) {
        self.frame_fence = Some(fence);
    }

    /// Upload CPU pixel buffer to GPU texture for a layer.
    /// Creates or resizes texture as needed.
    pub fn upload_layer(
        &mut self,
        device: &GpuDevice,
        layer_index: usize,
        pixels: &[Color32],
        tex_w: u32,
        tex_h: u32,
    ) {
        if tex_w == 0 || tex_h == 0 {
            return;
        }

        if layer_index >= self.textures.len() {
            self.textures.resize_with(layer_index + 1, || None);
        }

        let needs_create = match &self.textures[layer_index] {
            Some(lt) => lt.width != tex_w || lt.height != tex_h,
            None => true,
        };

        if needs_create {
            let texture = device.create_texture(&GpuTextureDesc {
                width: tex_w,
                height: tex_h,
                depth: 1,
                format: GpuTextureFormat::Rgba8UnormSrgb,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::CPU_UPLOAD,
                label: &format!("Layer Bitmap {layer_index}"),
                mip_levels: 1,
            });
            self.textures[layer_index] = Some(LayerTexture {
                texture,
                width: tex_w,
                height: tex_h,
            });
        }

        // Upload pixel data via replace_region (CPU_UPLOAD texture)
        // Color32 is #[repr(C)] with 4 u8 fields — safe to reinterpret as &[u8]
        if let Some(lt) = &self.textures[layer_index] {
            let bytes: &[u8] = unsafe {
                std::slice::from_raw_parts(pixels.as_ptr() as *const u8, pixels.len() * 4)
            };
            device.upload_texture(&lt.texture, bytes);
        }
    }

    /// Render all active layer bitmap textures as positioned quads.
    /// `layer_rects`: slice of `(layer_index, rect)` in logical pixels.
    pub fn render_layers(
        &mut self,
        _device: &GpuDevice,
        encoder: &mut GpuEncoder,
        target: &GpuTexture,
        screen_w: u32,
        screen_h: u32,
        layer_rects: &[(usize, Rect)],
    ) {
        if layer_rects.is_empty() {
            return;
        }

        let globals: [f32; 2] = [screen_w as f32, screen_h as f32];
        let globals_bytes: &[u8] = bytemuck::bytes_of(&globals);

        // Rotate ring buffer. Depth alone doesn't guarantee the GPU has
        // retired this slot's last use (this struct's ring is consumed
        // twice per frame, and heavy scenes can back up the queue past the
        // ring depth) — frame_fence blocks the claim if it hasn't.
        let slot = self.vbuf_ring_idx;
        self.vbuf_ring_idx = (slot + 1) % VBUF_RING_SIZE;
        if let Some(fence) = &self.frame_fence {
            fence.guard_slot(
                "LayerBitmapGpu",
                slot,
                &mut self.vbuf_stamps[slot],
                &mut self.fence_wait_events,
            );
        }
        let vbuf = &self.vbuf_ring[slot];

        // Write all layer quad vertices into the ring buffer in one batch.
        let ptr = vbuf.mapped_ptr().unwrap() as *mut BitmapVertex;
        let mut quad_count = 0usize;

        // Collect which layers are valid and write their vertices.
        self.draw_list.clear();
        for &(layer_idx, rect) in layer_rects {
            if layer_idx >= self.textures.len() || self.textures[layer_idx].is_none() {
                continue;
            }
            if rect.width <= 0.0 || rect.height <= 0.0 {
                continue;
            }
            if quad_count >= MAX_LAYER_QUADS {
                break;
            }

            let (x0, y0) = (rect.x, rect.y);
            let (x1, y1) = (rect.x + rect.width, rect.y + rect.height);
            let verts = [
                BitmapVertex {
                    position: [x0, y0],
                    uv: [0.0, 0.0],
                },
                BitmapVertex {
                    position: [x1, y0],
                    uv: [1.0, 0.0],
                },
                BitmapVertex {
                    position: [x1, y1],
                    uv: [1.0, 1.0],
                },
                BitmapVertex {
                    position: [x0, y1],
                    uv: [0.0, 1.0],
                },
            ];
            unsafe {
                std::ptr::copy_nonoverlapping(verts.as_ptr(), ptr.add(quad_count * 4), 4);
            }
            self.draw_list.push((layer_idx, quad_count));
            quad_count += 1;
        }

        if quad_count == 0 {
            return;
        }

        // Single render pass for all layer bitmap draws — avoids per-layer
        // render encoder creation (each costs a TBDR tile load/store cycle).
        encoder.begin_render_pass(target, GpuLoadAction::Load, "Layer Bitmaps");

        for &(layer_idx, quad_offset) in &self.draw_list {
            let lt = self.textures[layer_idx].as_ref().unwrap();
            let vertex_offset = (quad_offset * 4 * std::mem::size_of::<BitmapVertex>()) as u64;
            encoder.draw_in_render_pass(
                &self.pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: globals_bytes,
                    },
                    GpuBinding::Texture {
                        binding: 1,
                        texture: &lt.texture,
                    },
                    GpuBinding::Sampler {
                        binding: 2,
                        sampler: &self.sampler,
                    },
                ],
                vbuf,
                vertex_offset,
                &self.index_buf,
                6,
                0,
                None,
                "Bitmap Layer",
            );
        }

        encoder.end_render_pass();
    }

    /// Remove textures for layers that no longer exist.
    pub fn trim_to_layer_count(&mut self, count: usize) {
        if self.textures.len() > count {
            self.textures.truncate(count);
        }
    }
}
