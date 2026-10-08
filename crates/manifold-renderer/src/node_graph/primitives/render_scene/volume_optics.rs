//! Raster optical paths for closed volumes and embedded particle density.
//! Resources grow only when scene slots or dimensions change. No native API
//! escapes: both passes use manifold-gpu and WGSL.
use manifold_gpu::*;

use super::{ObjectDraw, mat4_inverse};

const SHADER: &str = include_str!("../shaders/volume_optics.wgsl");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    inverse_view_proj: [[f32; 4]; 4],
    eye: [f32; 4],
    parameters: [f32; 4],
    appearance: [f32; 4],
}

struct VolumeTextures {
    path: GpuTexture,
    nearest: GpuTexture,
}

#[derive(Default)]
pub(super) struct VolumeOptics {
    pipeline: Option<GpuRenderPipeline>,
    nearest_pipeline: Option<GpuRenderPipeline>,
    depth_state: Option<GpuDepthStencilState>,
    volumes: Vec<Option<VolumeTextures>>,
    density: Option<GpuTexture>,
}

fn texture(device: &GpuDevice, width: u32, height: u32, depth: bool) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width,
        height,
        depth: 1,
        mip_levels: 1,
        format: if depth {
            GpuTextureFormat::Depth32Float
        } else {
            GpuTextureFormat::R32Float
        },
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
        label: "closed volume optical path",
    })
}

impl VolumeOptics {
    pub(super) fn path(&self, index: usize) -> Option<&GpuTexture> {
        self.volumes.get(index)?.as_ref().map(|v| &v.path)
    }
    pub(super) fn nearest(&self, index: usize) -> Option<&GpuTexture> {
        self.volumes.get(index)?.as_ref().map(|v| &v.nearest)
    }
    pub(super) fn density(&self) -> Option<&GpuTexture> {
        self.density.as_ref()
    }

    pub(super) fn encode(
        &mut self,
        device: &GpuDevice,
        encoder: &mut GpuEncoder,
        draws: &[ObjectDraw<'_>],
        opaque_depth: &GpuTexture,
        identity: &GpuBuffer,
    ) -> Result<(), &'static str> {
        if !draws
            .iter()
            .any(|d| d.uniforms.volume_optics[0] > 0.5 && d.is_transmissive)
        {
            return Ok(());
        }
        let width = opaque_depth.width;
        let height = opaque_depth.height;
        if self.pipeline.is_none() {
            let blend = GpuBlendState {
                src_factor: GpuBlendFactor::One,
                dst_factor: GpuBlendFactor::One,
                operation: GpuBlendOp::Add,
                src_alpha_factor: GpuBlendFactor::One,
                dst_alpha_factor: GpuBlendFactor::One,
                alpha_operation: GpuBlendOp::Add,
            };
            self.pipeline = Some(device.create_render_pipeline(
                SHADER,
                "vs_main",
                "fs_path",
                GpuTextureFormat::R32Float,
                Some(blend),
                "volume signed path",
            ));
            self.nearest_pipeline = Some(device.create_render_pipeline_depth_only(
                SHADER,
                "vs_main",
                "fs_nearest",
                GpuTextureFormat::Depth32Float,
                "volume nearest surface",
            ));
            self.depth_state = Some(device.create_depth_stencil_state(&GpuDepthStencilDesc {
                compare: GpuCompareFunction::Greater,
                write_enabled: true,
            }));
        }
        if self
            .density
            .as_ref()
            .is_none_or(|t| t.width != width || t.height != height)
        {
            self.density = Some(texture(device, width, height, false));
        }
        encoder.clear_texture(self.density.as_ref().expect("ensured"), 0.0, 0.0, 0.0, 0.0);
        self.volumes
            .resize_with(draws.len().max(self.volumes.len()), || None);
        for (index, draw) in draws.iter().enumerate() {
            let geometric = draw.uniforms.volume_optics[0] > 0.5 && draw.is_transmissive;
            let density = draw.uniforms.volume_optics[2];
            if !geometric && density <= 0.0 {
                continue;
            }
            let inverse = mat4_inverse(draw.uniforms.view_proj)
                .ok_or("Volume optics: singular camera matrix")?;
            let mut u = Uniforms {
                view_proj: draw.uniforms.view_proj,
                model: draw.uniforms.model,
                inverse_view_proj: inverse,
                eye: draw.uniforms.camera_pos,
                parameters: [1.0, 0.0, 0.0, 0.0],
                appearance: draw.uniforms.appearance,
            };
            if geometric {
                if self.volumes[index]
                    .as_ref()
                    .is_none_or(|v| v.path.width != width || v.path.height != height)
                {
                    self.volumes[index] = Some(VolumeTextures {
                        path: texture(device, width, height, false),
                        nearest: texture(device, width, height, true),
                    });
                }
                let targets = self.volumes[index].as_ref().expect("ensured");
                let b = bindings(&u, draw, opaque_depth, identity);
                encoder.draw_instanced(
                    self.pipeline.as_ref().expect("ensured"),
                    &targets.path,
                    &b,
                    draw.draw_count(),
                    GpuLoadAction::Clear,
                    "volume signed path",
                );
                let call = draw.live(GpuEncoder::depth_msaa_draw(
                    self.nearest_pipeline.as_ref().expect("ensured"),
                    &b,
                    draw.vertex_count,
                    draw.instance_count,
                ));
                encoder.draw_instanced_depth_only_batch(
                    &targets.nearest,
                    self.depth_state.as_ref().expect("ensured"),
                    &[call],
                    "volume nearest surface",
                );
            }
            if density > 0.0 {
                u.parameters[0] = density;
                encoder.draw_instanced(
                    self.pipeline.as_ref().expect("ensured"),
                    self.density.as_ref().expect("ensured"),
                    &bindings(&u, draw, opaque_depth, identity),
                    draw.draw_count(),
                    GpuLoadAction::Load,
                    "volume embedded density",
                );
            }
        }
        Ok(())
    }
}

fn bindings<'a>(
    u: &'a Uniforms,
    draw: &'a ObjectDraw<'_>,
    opaque_depth: &'a GpuTexture,
    identity: &'a GpuBuffer,
) -> [GpuBinding<'a>; 5] {
    [
        GpuBinding::Bytes {
            binding: 0,
            data: bytemuck::bytes_of(u),
        },
        GpuBinding::Buffer {
            binding: 1,
            buffer: draw.vertices,
            offset: 0,
        },
        GpuBinding::Buffer {
            binding: 2,
            buffer: draw.instances.unwrap_or(identity),
            offset: 0,
        },
        GpuBinding::Texture {
            binding: 3,
            texture: opaque_depth,
        },
        GpuBinding::Buffer {
            binding: 4,
            buffer: draw.weights.unwrap_or(draw.vertices),
            offset: 0,
        },
    ]
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests;
