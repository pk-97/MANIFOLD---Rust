//! The one gizmo line renderer: world-space lines drawn over a finished
//! picture and tested against the scene's own depth, in the mode the user
//! picked. The output preview, the editor viewport and an export all call
//! [`GizmoPass::draw`]; none of them draws gizmo lines any other way.
//!
//! Lines are clipped to the camera's near plane on the CPU, uploaded to a
//! small ring of shared buffers, and expanded to screen-space quads in the
//! vertex shader. The fragment shader reads the scene's raw depth texture, so
//! a hidden line is dropped (Occluded), drawn anyway (On Top) or drawn faded
//! (Ghost).

use manifold_foundation::scene_surface::GizmoDrawMode;
use manifold_gpu::BufferUse;
use manifold_gpu::{
    DrawCount, GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuBuffer, GpuDevice,
    GpuEncoder, GpuLoadAction, GpuRenderPipeline, GpuTexture, GpuTextureDesc, GpuTextureDimension,
    GpuTextureFormat, GpuTextureUsage,
};
use manifold_node_engine::scene::camera::Camera;

use crate::node_graph::viewport_overlay::{WorldLine, clip_to_near};

const SHADER: &str = include_str!("gizmo_pass.wgsl");
/// Line width on screen, in pixels: one and a half points on a Retina
/// preview, with a one-pixel coverage ramp at each edge.
const LINE_WIDTH_PX: f32 = 3.0;
/// How much of a hidden line's colour a Ghost line keeps.
const GHOST_ALPHA: f32 = 0.22;
/// Buffers in flight, so the CPU never rewrites one the GPU still reads.
const RING: usize = 3;
const FLOATS_PER_LINE: usize = 12;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    view_proj: [[f32; 4]; 4],
    target_size: [f32; 2],
    depth_size: [f32; 2],
    half_width: f32,
    ghost_alpha: f32,
    mode: u32,
    _pad: f32,
}

/// The pass's pipelines (one per target format), line buffers and scratch.
#[derive(Default)]
pub struct GizmoPass {
    pipelines: Vec<(GpuTextureFormat, GpuRenderPipeline)>,
    buffers: Vec<GpuBuffer>,
    next: usize,
    /// `a.xyz, 0, b.xyz, 0, rgba` per line, reused every draw.
    scratch: Vec<f32>,
    /// Bound when no scene depth exists, so the mode falls back to On Top.
    stub_depth: Option<GpuTexture>,
}

impl GizmoPass {
    /// Draw `lines` over `target` through `camera`. `depth` is the scene's
    /// raw depth; without it every mode draws On Top. The camera's aspect is
    /// the depth's (the scene render's), or the target's with no depth, so a
    /// target of another size still lines up.
    pub fn draw(
        &mut self,
        device: &GpuDevice,
        enc: &mut GpuEncoder,
        target: &GpuTexture,
        depth: Option<&GpuTexture>,
        camera: &Camera,
        lines: &[WorldLine],
        mode: GizmoDrawMode,
    ) {
        self.scratch.clear();
        for line in lines {
            let Some((a, b)) = clip_to_near(camera, line.a, line.b) else {
                continue;
            };
            let [r, g, b8, a8] = line.color;
            self.scratch.extend_from_slice(&[
                a[0],
                a[1],
                a[2],
                if line.caps { 1.0 } else { 0.0 },
                b[0],
                b[1],
                b[2],
                0.0,
                f32::from(r) / 255.0,
                f32::from(g) / 255.0,
                f32::from(b8) / 255.0,
                f32::from(a8) / 255.0,
            ]);
        }
        let count = self.scratch.len() / FLOATS_PER_LINE;
        if count == 0 || target.width == 0 || target.height == 0 {
            return;
        }
        let needed = (self.scratch.len() * std::mem::size_of::<f32>()) as u64;
        if self.buffers.len() < RING || self.buffers[0].size < needed {
            let size = needed.max(4096).next_power_of_two();
            self.buffers = (0..RING)
                .map(|_| device.create_buffer(size, BufferUse::Upload))
                .collect();
        }
        self.next = (self.next + 1) % RING;
        // SAFETY: the buffer holds at least `needed` bytes, and the ring keeps
        // it clear of any frame still reading it.
        unsafe { self.buffers[self.next].write(0, bytemuck::cast_slice(&self.scratch)) };

        if !self
            .pipelines
            .iter()
            .any(|(format, _)| *format == target.format)
        {
            let blend = GpuBlendState {
                src_factor: GpuBlendFactor::SrcAlpha,
                dst_factor: GpuBlendFactor::OneMinusSrcAlpha,
                operation: GpuBlendOp::Add,
                src_alpha_factor: GpuBlendFactor::One,
                dst_alpha_factor: GpuBlendFactor::OneMinusSrcAlpha,
                alpha_operation: GpuBlendOp::Add,
            };
            let pipeline = device.create_render_pipeline(
                &manifold_gpu::RenderPipelineDesc::new(SHADER, "vs_main", "fs_main", "gizmo lines")
                    .color(target.format, Some(blend)),
            );
            self.pipelines.push((target.format, pipeline));
        }
        let pipeline = &self
            .pipelines
            .iter()
            .find(|(format, _)| *format == target.format)
            .expect("just ensured")
            .1;
        if self.stub_depth.is_none() {
            self.stub_depth = Some(device.create_texture(&GpuTextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                format: GpuTextureFormat::R32Float,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
                label: "gizmo lines stub depth",
                mip_levels: 1,
            }));
        }
        let (scene_depth, mode) = match depth {
            Some(depth) => (depth, mode),
            None => (
                self.stub_depth.as_ref().expect("just ensured"),
                GizmoDrawMode::OnTop,
            ),
        };
        let sized = depth.unwrap_or(target);
        let aspect = sized.width as f32 / sized.height.max(1) as f32;
        let params = Params {
            view_proj: camera.view_proj(aspect),
            target_size: [target.width as f32, target.height as f32],
            depth_size: [scene_depth.width as f32, scene_depth.height as f32],
            half_width: LINE_WIDTH_PX * 0.5,
            ghost_alpha: GHOST_ALPHA,
            mode: match mode {
                GizmoDrawMode::Occluded => 0,
                GizmoDrawMode::OnTop => 1,
                GizmoDrawMode::Ghost => 2,
            },
            _pad: 0.0,
        };
        enc.draw_instanced(
            pipeline,
            target,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &self.buffers[self.next],
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 2,
                    texture: scene_depth,
                },
            ],
            DrawCount::Direct {
                vertices: 6 * count as u32,
                instances: 1,
            },
            GpuLoadAction::Load,
            "gizmo lines",
        );
    }
}
