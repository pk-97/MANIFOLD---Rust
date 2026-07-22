//! G-buffer raster pass, BRIEF.md step 3. MRT: g_wpos (rgba32f, clear w=0),
//! g_nrm (rgba16f), g_alb (rgba16f), g_mat (rg16f-equivalent rgba16f with
//! zeros in ba), depth32float. Static camera framing the scan's bounding
//! sphere.

use manifold_gpu::{GpuBuffer, GpuTexture, GpuTextureFormat, GpuTextureUsage};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCullMode, MTLDepthStencilDescriptor,
    MTLDevice, MTLIndexType, MTLLibrary, MTLLoadAction, MTLPixelFormat, MTLPrimitiveType,
    MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
    MTLRenderPipelineState, MTLStoreAction,
};

use crate::gpu::Gpu;
use crate::types::CameraUniforms;

pub struct GBufferTargets {
    pub g_wpos: GpuTexture,
    pub g_nrm: GpuTexture,
    pub g_alb: GpuTexture,
    pub g_mat: GpuTexture,
    pub depth: GpuTexture,
}

impl GBufferTargets {
    pub fn new(gpu: &Gpu, width: u32, height: u32) -> Self {
        let rt_usage = GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ;
        Self {
            g_wpos: gpu.texture(GpuTextureFormat::Rgba32Float, width, height, rt_usage, false, "g_wpos"),
            g_nrm: gpu.texture(GpuTextureFormat::Rgba16Float, width, height, rt_usage, false, "g_nrm"),
            g_alb: gpu.texture(GpuTextureFormat::Rgba16Float, width, height, rt_usage, false, "g_alb"),
            g_mat: gpu.texture(GpuTextureFormat::Rgba16Float, width, height, rt_usage, false, "g_mat"),
            depth: gpu.texture(GpuTextureFormat::Depth32Float, width, height, GpuTextureUsage::RENDER_TARGET, false, "g_depth"),
        }
    }
}

pub struct GBufferPipeline {
    pub state: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    pub depth_state: Retained<ProtocolObject<dyn objc2_metal::MTLDepthStencilState>>,
}

impl GBufferPipeline {
    pub fn new(gpu: &Gpu, library: &ProtocolObject<dyn MTLLibrary>) -> Self {
        let vs = library
            .newFunctionWithName(&NSString::from_str("vs_gbuffer"))
            .expect("vs_gbuffer not found");
        let fs = library
            .newFunctionWithName(&NSString::from_str("fs_gbuffer"))
            .expect("fs_gbuffer not found");

        let desc = MTLRenderPipelineDescriptor::init(MTLRenderPipelineDescriptor::alloc());
        desc.setVertexFunction(Some(&vs));
        desc.setFragmentFunction(Some(&fs));
        desc.setDepthAttachmentPixelFormat(MTLPixelFormat::Depth32Float);
        let formats = [
            MTLPixelFormat::RGBA32Float,
            MTLPixelFormat::RGBA16Float,
            MTLPixelFormat::RGBA16Float,
            MTLPixelFormat::RGBA16Float,
        ];
        for (i, fmt) in formats.iter().enumerate() {
            let attach = unsafe { desc.colorAttachments().objectAtIndexedSubscript(i) };
            attach.setPixelFormat(*fmt);
        }
        let raw_device = gpu.device.raw_device();
        let state = raw_device
            .newRenderPipelineStateWithDescriptor_error(&desc)
            .unwrap_or_else(|e| panic!("gbuffer PSO error: {}", e.localizedDescription()));

        let ds_desc = MTLDepthStencilDescriptor::init(MTLDepthStencilDescriptor::alloc());
        ds_desc.setDepthCompareFunction(objc2_metal::MTLCompareFunction::Less);
        ds_desc.setDepthWriteEnabled(true);
        let depth_state = raw_device
            .newDepthStencilStateWithDescriptor(&ds_desc)
            .expect("newDepthStencilStateWithDescriptor failed");

        Self { state, depth_state }
    }

    /// Encode + commit + wait one G-buffer render pass. Returns GPU ms.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        gpu: &Gpu,
        targets: &GBufferTargets,
        positions: &GpuBuffer,
        normals: &GpuBuffer,
        material_ids: &GpuBuffer,
        materials: &GpuBuffer,
        camera: &GpuBuffer,
        index_buffer: &GpuBuffer,
        index_count: u32,
    ) -> f64 {
        let pass = MTLRenderPassDescriptor::init(MTLRenderPassDescriptor::alloc());
        let color_texs = [&targets.g_wpos, &targets.g_nrm, &targets.g_alb, &targets.g_mat];
        for (i, tex) in color_texs.iter().enumerate() {
            let attach = unsafe { pass.colorAttachments().objectAtIndexedSubscript(i) };
            attach.setTexture(Some(tex.raw()));
            attach.setLoadAction(MTLLoadAction::Clear);
            attach.setStoreAction(MTLStoreAction::Store);
            attach.setClearColor(MTLClearColor { red: 0.0, green: 0.0, blue: 0.0, alpha: 0.0 });
        }
        let depth_attach = pass.depthAttachment();
        depth_attach.setTexture(Some(targets.depth.raw()));
        depth_attach.setLoadAction(MTLLoadAction::Clear);
        depth_attach.setStoreAction(MTLStoreAction::Store);
        depth_attach.setClearDepth(1.0);

        let cb = gpu.command_buffer("gbuffer");
        let enc = cb
            .renderCommandEncoderWithDescriptor(&pass)
            .expect("renderCommandEncoderWithDescriptor failed");
        unsafe {
            enc.setRenderPipelineState(&self.state);
            enc.setDepthStencilState(Some(&self.depth_state));
            enc.setCullMode(MTLCullMode::None);
            enc.setVertexBuffer_offset_atIndex(Some(positions.raw()), 0, 0);
            enc.setVertexBuffer_offset_atIndex(Some(normals.raw()), 0, 1);
            enc.setVertexBuffer_offset_atIndex(Some(material_ids.raw()), 0, 2);
            enc.setVertexBuffer_offset_atIndex(Some(camera.raw()), 0, 3);
            enc.setFragmentBuffer_offset_atIndex(Some(materials.raw()), 0, 0);
            enc.setFragmentBuffer_offset_atIndex(Some(camera.raw()), 0, 1);
            // Indexed draw, not drawPrimitives: with vertex-pulling via
            // [[vertex_id]], an indexed draw is what makes Metal feed the
            // real index-buffer values as vertex_id (drawPrimitives would
            // instead feed 0..index_count sequentially, ignoring topology).
            enc.drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferOffset(
                MTLPrimitiveType::Triangle,
                index_count as usize,
                MTLIndexType::UInt32,
                index_buffer.raw(),
                0,
            );
        }
        enc.endEncoding();
        Gpu::commit_and_time(&cb)
    }
}

pub fn build_camera(
    center: glam::Vec3,
    radius: f32,
    aspect: f32,
) -> (CameraUniforms, glam::Vec3) {
    let dist = 2.2 * radius;
    let elevation = 15f32.to_radians();
    let eye = center + dist * glam::Vec3::new(0.0, elevation.sin(), elevation.cos());
    let view = glam::Mat4::look_at_rh(eye, center, glam::Vec3::Y);
    let near = (radius * 0.01).max(0.001);
    let far = dist + radius * 4.0;
    let proj = glam::Mat4::perspective_rh(45f32.to_radians(), aspect, near, far);
    let view_proj = proj * view;
    let cam = CameraUniforms {
        view_proj: view_proj.to_cols_array(),
        cam_pos: eye.into(),
        _pad0: 0.0,
    };
    (cam, eye)
}
