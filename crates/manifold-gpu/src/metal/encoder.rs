//! GpuEncoder — per-frame GPU command encoder wrapping a retained Metal command buffer.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBlitOption, MTLCommandBuffer, MTLCommandEncoder,
    MTLComputeCommandEncoder, MTLIndexType, MTLLoadAction, MTLOrigin, MTLPrimitiveType,
    MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLScissorRect, MTLSize, MTLStoreAction,
    MTLTexture, MTLTextureUsage, MTLViewport,
};

use super::*;
use crate::types::*;

/// Encoder state — tracks the current active Metal encoder.
#[allow(dead_code)]
pub(crate) enum EncoderState {
    None,
    Compute(Retained<ProtocolObject<dyn MTLComputeCommandEncoder>>),
    Render(Retained<ProtocolObject<dyn MTLRenderCommandEncoder>>),
    Blit(Retained<ProtocolObject<dyn MTLBlitCommandEncoder>>),
}

/// Cached compute bind state — skips redundant Metal API calls when the same
/// resource is already bound at a slot from a previous dispatch.
const CACHE_SLOTS: usize = 16;

pub(super) struct ComputeBindCache {
    textures: [*const c_void; CACHE_SLOTS],
    samplers: [*const c_void; CACHE_SLOTS],
    buffers: [(*const c_void, u64); CACHE_SLOTS],
}

impl ComputeBindCache {
    pub(super) fn new() -> Self {
        Self {
            textures: [std::ptr::null(); CACHE_SLOTS],
            samplers: [std::ptr::null(); CACHE_SLOTS],
            buffers: [(std::ptr::null(), 0); CACHE_SLOTS],
        }
    }

    fn clear(&mut self) {
        self.textures = [std::ptr::null(); CACHE_SLOTS];
        self.samplers = [std::ptr::null(); CACHE_SLOTS];
        self.buffers = [(std::ptr::null(), 0); CACHE_SLOTS];
    }
}

pub(super) struct RenderBindCache {
    frag_textures: [*const c_void; CACHE_SLOTS],
    frag_samplers: [*const c_void; CACHE_SLOTS],
    buffers: [(*const c_void, u64); CACHE_SLOTS],
    vertex_buf_30: *const c_void,
    bytes: [(*const u8, usize); CACHE_SLOTS],
}

impl RenderBindCache {
    pub(super) fn new() -> Self {
        Self {
            frag_textures: [std::ptr::null(); CACHE_SLOTS],
            frag_samplers: [std::ptr::null(); CACHE_SLOTS],
            buffers: [(std::ptr::null(), 0); CACHE_SLOTS],
            vertex_buf_30: std::ptr::null(),
            bytes: [(std::ptr::null(), 0); CACHE_SLOTS],
        }
    }

    fn clear(&mut self) {
        self.frag_textures = [std::ptr::null(); CACHE_SLOTS];
        self.frag_samplers = [std::ptr::null(); CACHE_SLOTS];
        self.buffers = [(std::ptr::null(), 0); CACHE_SLOTS];
        self.vertex_buf_30 = std::ptr::null();
        self.bytes = [(std::ptr::null(), 0); CACHE_SLOTS];
    }
}

/// Per-frame GPU command encoder. Wraps a retained Metal command buffer.
pub struct GpuEncoder {
    /// Retained MTLCommandBuffer. Released on drop.
    pub(crate) cmd_buf: Retained<ProtocolObject<dyn MTLCommandBuffer>>,
    pub(crate) state: EncoderState,
    pub(super) compute_cache: ComputeBindCache,
    pub(super) render_cache: RenderBindCache,
    pub(super) clear_pipelines: *const super::device::ClearPipelines,
}

unsafe impl Send for GpuEncoder {}

#[inline]
fn texture_identity(tex: &ProtocolObject<dyn objc2_metal::MTLTexture>) -> *const c_void {
    tex as *const _ as *const c_void
}

#[inline]
fn sampler_identity(s: &ProtocolObject<dyn objc2_metal::MTLSamplerState>) -> *const c_void {
    s as *const _ as *const c_void
}

#[inline]
fn buffer_identity(buf: &ProtocolObject<dyn objc2_metal::MTLBuffer>) -> *const c_void {
    buf as *const _ as *const c_void
}

impl GpuEncoder {
    pub(super) fn cmd_buf(&self) -> &ProtocolObject<dyn MTLCommandBuffer> {
        &self.cmd_buf
    }

    /// Get the raw command buffer for direct encoding (MPS kernels, MetalFX).
    /// Ends any active encoder first to avoid encoding conflicts.
    pub fn raw_cmd_buf(&mut self) -> &ProtocolObject<dyn MTLCommandBuffer> {
        self.end_current();
        &self.cmd_buf
    }

    /// Ensure a compute encoder is active. Returns a retained handle.
    fn ensure_compute(&mut self) -> Retained<ProtocolObject<dyn MTLComputeCommandEncoder>> {
        if let EncoderState::Compute(ref enc) = self.state {
            return enc.clone();
        }
        self.end_current();
        let enc = self
            .cmd_buf
            .computeCommandEncoder()
            .expect("Failed to create compute encoder");
        self.state = EncoderState::Compute(enc.clone());
        enc
    }

    /// End the current encoder (if any).
    pub(super) fn end_current(&mut self) {
        let state = std::mem::replace(&mut self.state, EncoderState::None);
        match state {
            EncoderState::None => {}
            EncoderState::Compute(enc) => {
                enc.endEncoding();
                self.compute_cache.clear();
            }
            EncoderState::Render(enc) => {
                enc.endEncoding();
            }
            EncoderState::Blit(enc) => {
                enc.endEncoding();
            }
        }
    }

    /// Dispatch a compute shader.
    pub fn dispatch_compute(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        workgroups: [u32; 3],
        label: &str,
    ) {
        let enc = self.ensure_compute();
        unsafe {
            enc.pushDebugGroup(&NSString::from_str(label));
            enc.setComputePipelineState(&pipeline.state);
        }

        const MAX_BUFFER_SLOTS: usize = 32;
        let mut buffer_sizes = [0u32; MAX_BUFFER_SLOTS];
        let mut buffer_sizes_len: usize = 0;

        for binding in bindings {
            match binding {
                GpuBinding::Buffer {
                    binding: b,
                    buffer,
                    offset,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    let idx = slot.metal_index as usize;
                    let id = buffer_identity(&buffer.raw);
                    if idx >= CACHE_SLOTS || self.compute_cache.buffers[idx] != (id, *offset) {
                        unsafe {
                            enc.setBuffer_offset_atIndex(
                                Some(&buffer.raw),
                                *offset as usize,
                                slot.metal_index as usize,
                            );
                        }
                        if idx < CACHE_SLOTS {
                            self.compute_cache.buffers[idx] = (id, *offset);
                        }
                    }
                    if idx < MAX_BUFFER_SLOTS {
                        buffer_sizes[idx] = buffer.size as u32;
                        if idx >= buffer_sizes_len {
                            buffer_sizes_len = idx + 1;
                        }
                    }
                }
                GpuBinding::Texture {
                    binding: b,
                    texture,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    let idx = slot.metal_index as usize;
                    let id = texture_identity(&texture.raw);
                    if idx >= CACHE_SLOTS || self.compute_cache.textures[idx] != id {
                        unsafe {
                            enc.setTexture_atIndex(Some(&texture.raw), slot.metal_index as usize);
                        }
                        if idx < CACHE_SLOTS {
                            self.compute_cache.textures[idx] = id;
                        }
                    }
                }
                GpuBinding::Sampler {
                    binding: b,
                    sampler,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    let idx = slot.metal_index as usize;
                    let id = sampler_identity(&sampler.raw);
                    if idx >= CACHE_SLOTS || self.compute_cache.samplers[idx] != id {
                        unsafe {
                            enc.setSamplerState_atIndex(
                                Some(&sampler.raw),
                                slot.metal_index as usize,
                            );
                        }
                        if idx < CACHE_SLOTS {
                            self.compute_cache.samplers[idx] = id;
                        }
                    }
                }
                GpuBinding::Bytes { binding: b, data } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    unsafe {
                        enc.setBytes_length_atIndex(
                            NonNull::new(data.as_ptr() as *mut c_void).unwrap(),
                            data.len(),
                            slot.metal_index as usize,
                        );
                    }
                }
            }
        }

        if pipeline.needs_sizes_buffer {
            let slot = pipeline
                .slot_map
                .get(SIZES_BUFFER_BINDING)
                .expect("sizes buffer slot missing");
            unsafe {
                enc.setBytes_length_atIndex(
                    NonNull::new(buffer_sizes.as_ptr() as *mut c_void).unwrap(),
                    buffer_sizes_len * 4,
                    slot.metal_index as usize,
                );
            }
        }

        let wg = pipeline.workgroup_size;
        unsafe {
            enc.dispatchThreadgroups_threadsPerThreadgroup(
                MTLSize {
                    width: workgroups[0] as usize,
                    height: workgroups[1] as usize,
                    depth: workgroups[2] as usize,
                },
                MTLSize {
                    width: wg[0] as usize,
                    height: wg[1] as usize,
                    depth: wg[2] as usize,
                },
            );
            enc.popDebugGroup();
        }
    }

    /// Draw a fullscreen triangle with a render pipeline.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_fullscreen(
        &mut self,
        pipeline: &GpuRenderPipeline,
        target: &GpuTexture,
        bindings: &[GpuBinding],
        clear: bool,
        store: bool,
        label: &str,
    ) {
        self.end_current();

        let desc = new_render_pass_descriptor();
        let color = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        unsafe {
            color.setTexture(Some(&target.raw));
            color.setLoadAction(if clear {
                MTLLoadAction::Clear
            } else {
                MTLLoadAction::Load
            });
            color.setStoreAction(if store {
                MTLStoreAction::Store
            } else {
                MTLStoreAction::DontCare
            });
            color.setClearColor(objc2_metal::MTLClearColor {
                red: 0.0,
                green: 0.0,
                blue: 0.0,
                alpha: 0.0,
            });
        }

        let enc = self
            .cmd_buf
            .renderCommandEncoderWithDescriptor(&desc)
            .expect("renderCommandEncoderWithDescriptor failed");
        unsafe {
            enc.pushDebugGroup(&NSString::from_str(label));
            enc.setRenderPipelineState(&pipeline.state);
        }

        apply_bindings_draw_fullscreen(&enc, pipeline, bindings);

        unsafe {
            enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
            enc.popDebugGroup();
            enc.endEncoding();
        }
    }

    /// Draw a fullscreen triangle with viewport positioning.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_fullscreen_viewport(
        &mut self,
        pipeline: &GpuRenderPipeline,
        target: &GpuTexture,
        bindings: &[GpuBinding],
        viewport: (f32, f32, f32, f32),
        load_action: crate::GpuLoadAction,
        label: &str,
    ) {
        self.end_current();

        let desc = new_render_pass_descriptor();
        let color = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        unsafe {
            color.setTexture(Some(&target.raw));
            color.setLoadAction(convert_load_action(load_action));
            color.setStoreAction(MTLStoreAction::Store);
            color.setClearColor(objc2_metal::MTLClearColor {
                red: 0.0,
                green: 0.0,
                blue: 0.0,
                alpha: 0.0,
            });
        }

        let enc = self
            .cmd_buf
            .renderCommandEncoderWithDescriptor(&desc)
            .expect("renderCommandEncoderWithDescriptor failed");
        unsafe {
            enc.pushDebugGroup(&NSString::from_str(label));
            enc.setRenderPipelineState(&pipeline.state);
        }

        let (x, y, w, h) = viewport;
        unsafe {
            enc.setViewport(MTLViewport {
                originX: x as f64,
                originY: y as f64,
                width: w as f64,
                height: h as f64,
                znear: 0.0,
                zfar: 1.0,
            });
        }

        apply_bindings_draw_fullscreen(&enc, pipeline, bindings);

        unsafe {
            enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
            enc.popDebugGroup();
            enc.endEncoding();
        }
    }

    /// Draw instanced geometry with a render pipeline.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_instanced(
        &mut self,
        pipeline: &GpuRenderPipeline,
        target: &GpuTexture,
        bindings: &[GpuBinding],
        vertex_count: u32,
        instance_count: u32,
        load_action: crate::GpuLoadAction,
        label: &str,
    ) {
        self.end_current();

        let desc = new_render_pass_descriptor();
        let color = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        unsafe {
            color.setTexture(Some(&target.raw));
            color.setLoadAction(convert_load_action(load_action));
            color.setStoreAction(MTLStoreAction::Store);
            color.setClearColor(objc2_metal::MTLClearColor {
                red: 0.0,
                green: 0.0,
                blue: 0.0,
                alpha: 0.0,
            });
        }

        let enc = self
            .cmd_buf
            .renderCommandEncoderWithDescriptor(&desc)
            .expect("renderCommandEncoderWithDescriptor failed");
        unsafe {
            enc.pushDebugGroup(&NSString::from_str(label));
            enc.setRenderPipelineState(&pipeline.state);
        }

        apply_bindings_draw_both_stages(&enc, pipeline, bindings);

        if instance_count > 0 {
            unsafe {
                enc.drawPrimitives_vertexStart_vertexCount_instanceCount(
                    MTLPrimitiveType::Triangle,
                    0,
                    vertex_count as usize,
                    instance_count as usize,
                );
            }
        }
        unsafe {
            enc.popDebugGroup();
            enc.endEncoding();
        }
    }

    /// Draw instanced with MSAA: render to a multisample target, resolve to
    /// a single-sample texture.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_instanced_msaa(
        &mut self,
        pipeline: &GpuRenderPipeline,
        msaa_target: &GpuTexture,
        resolve_target: &GpuTexture,
        bindings: &[GpuBinding],
        vertex_count: u32,
        instance_count: u32,
        load_action: crate::GpuLoadAction,
        label: &str,
    ) {
        self.end_current();

        let desc = new_render_pass_descriptor();
        let color = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        unsafe {
            color.setTexture(Some(&msaa_target.raw));
            color.setResolveTexture(Some(&resolve_target.raw));
            color.setLoadAction(convert_load_action(load_action));
            color.setStoreAction(MTLStoreAction::MultisampleResolve);
            color.setClearColor(objc2_metal::MTLClearColor {
                red: 0.0,
                green: 0.0,
                blue: 0.0,
                alpha: 0.0,
            });
        }

        let enc = self
            .cmd_buf
            .renderCommandEncoderWithDescriptor(&desc)
            .expect("renderCommandEncoderWithDescriptor failed");
        unsafe {
            enc.pushDebugGroup(&NSString::from_str(label));
            enc.setRenderPipelineState(&pipeline.state);
        }

        apply_bindings_draw_both_stages(&enc, pipeline, bindings);

        if instance_count > 0 {
            unsafe {
                enc.drawPrimitives_vertexStart_vertexCount_instanceCount(
                    MTLPrimitiveType::Triangle,
                    0,
                    vertex_count as usize,
                    instance_count as usize,
                );
            }
        }
        unsafe {
            enc.popDebugGroup();
            enc.endEncoding();
        }
    }

    /// Draw instanced geometry with depth testing.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_instanced_depth(
        &mut self,
        pipeline: &GpuRenderPipeline,
        target: &GpuTexture,
        depth_target: &GpuTexture,
        depth_stencil_state: &GpuDepthStencilState,
        bindings: &[GpuBinding],
        vertex_count: u32,
        instance_count: u32,
        load_action: crate::GpuLoadAction,
        label: &str,
    ) {
        self.end_current();

        let desc = new_render_pass_descriptor();

        let color = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        unsafe {
            color.setTexture(Some(&target.raw));
            color.setLoadAction(convert_load_action(load_action));
            color.setStoreAction(MTLStoreAction::Store);
            color.setClearColor(objc2_metal::MTLClearColor {
                red: 0.0,
                green: 0.0,
                blue: 0.0,
                alpha: 0.0,
            });
        }

        let depth = unsafe { desc.depthAttachment() };
        unsafe {
            depth.setTexture(Some(&depth_target.raw));
            depth.setLoadAction(convert_load_action(load_action));
            depth.setStoreAction(MTLStoreAction::Store);
            depth.setClearDepth(1.0);
        }

        let enc = self
            .cmd_buf
            .renderCommandEncoderWithDescriptor(&desc)
            .expect("renderCommandEncoderWithDescriptor failed");
        unsafe {
            enc.pushDebugGroup(&NSString::from_str(label));
            enc.setRenderPipelineState(&pipeline.state);
            enc.setDepthStencilState(Some(&depth_stencil_state.raw));
            enc.setViewport(MTLViewport {
                originX: 0.0,
                originY: 0.0,
                width: target.width as f64,
                height: target.height as f64,
                znear: 0.0,
                zfar: 1.0,
            });
        }

        apply_bindings_draw_both_stages(&enc, pipeline, bindings);

        if instance_count > 0 {
            unsafe {
                enc.drawPrimitives_vertexStart_vertexCount_instanceCount(
                    MTLPrimitiveType::Triangle,
                    0,
                    vertex_count as usize,
                    instance_count as usize,
                );
            }
        }
        unsafe {
            enc.popDebugGroup();
            enc.endEncoding();
        }
    }

    /// Draw instanced geometry with depth testing, fill mode, and optional depth bias.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_instanced_depth_ex(
        &mut self,
        pipeline: &GpuRenderPipeline,
        target: &GpuTexture,
        depth_target: &GpuTexture,
        depth_stencil_state: &GpuDepthStencilState,
        bindings: &[GpuBinding],
        vertex_count: u32,
        instance_count: u32,
        load_action: crate::GpuLoadAction,
        fill_mode: crate::GpuTriangleFillMode,
        primitive_type: crate::GpuPrimitiveType,
        depth_bias: Option<(f32, f32, f32)>,
        label: &str,
    ) {
        self.end_current();

        let desc = new_render_pass_descriptor();

        let color = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        unsafe {
            color.setTexture(Some(&target.raw));
            color.setLoadAction(convert_load_action(load_action));
            color.setStoreAction(MTLStoreAction::Store);
            color.setClearColor(objc2_metal::MTLClearColor {
                red: 0.0,
                green: 0.0,
                blue: 0.0,
                alpha: 1.0,
            });
        }

        let depth = unsafe { desc.depthAttachment() };
        unsafe {
            depth.setTexture(Some(&depth_target.raw));
            depth.setLoadAction(convert_load_action(load_action));
            depth.setStoreAction(MTLStoreAction::Store);
            depth.setClearDepth(1.0);
        }

        let enc = self
            .cmd_buf
            .renderCommandEncoderWithDescriptor(&desc)
            .expect("renderCommandEncoderWithDescriptor failed");
        unsafe {
            enc.pushDebugGroup(&NSString::from_str(label));
            enc.setRenderPipelineState(&pipeline.state);
            enc.setDepthStencilState(Some(&depth_stencil_state.raw));
            enc.setTriangleFillMode(format::to_mtl_triangle_fill_mode(fill_mode));

            if let Some((bias, slope_scale, clamp)) = depth_bias {
                enc.setDepthBias_slopeScale_clamp(bias, slope_scale, clamp);
            }

            enc.setViewport(MTLViewport {
                originX: 0.0,
                originY: 0.0,
                width: target.width as f64,
                height: target.height as f64,
                znear: 0.0,
                zfar: 1.0,
            });
        }

        apply_bindings_draw_both_stages(&enc, pipeline, bindings);

        if instance_count > 0 {
            unsafe {
                enc.drawPrimitives_vertexStart_vertexCount_instanceCount(
                    format::to_mtl_primitive_type(primitive_type),
                    0,
                    vertex_count as usize,
                    instance_count as usize,
                );
            }
        }
        unsafe {
            enc.popDebugGroup();
            enc.endEncoding();
        }
    }

    /// Draw indexed geometry with a render pipeline and vertex/index buffers.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_indexed(
        &mut self,
        pipeline: &GpuRenderPipeline,
        target: &GpuTexture,
        bindings: &[GpuBinding],
        vertex_buffer: &GpuBuffer,
        vertex_offset: u64,
        index_buffer: &GpuBuffer,
        index_count: u32,
        viewport: Option<(f32, f32, f32, f32)>,
        load_action: crate::GpuLoadAction,
        label: &str,
    ) {
        self.end_current();

        let desc = new_render_pass_descriptor();
        let color = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        unsafe {
            color.setTexture(Some(&target.raw));
            color.setLoadAction(convert_load_action(load_action));
            color.setStoreAction(MTLStoreAction::Store);
            color.setClearColor(objc2_metal::MTLClearColor {
                red: 0.0,
                green: 0.0,
                blue: 0.0,
                alpha: 0.0,
            });
        }

        let enc = self
            .cmd_buf
            .renderCommandEncoderWithDescriptor(&desc)
            .expect("renderCommandEncoderWithDescriptor failed");
        unsafe {
            enc.pushDebugGroup(&NSString::from_str(label));
            enc.setRenderPipelineState(&pipeline.state);

            if let Some((x, y, w, h)) = viewport {
                enc.setViewport(MTLViewport {
                    originX: x as f64,
                    originY: y as f64,
                    width: w as f64,
                    height: h as f64,
                    znear: 0.0,
                    zfar: 1.0,
                });
            } else {
                enc.setViewport(MTLViewport {
                    originX: 0.0,
                    originY: 0.0,
                    width: target.width as f64,
                    height: target.height as f64,
                    znear: 0.0,
                    zfar: 1.0,
                });
            }

            const VERTEX_BUFFER_INDEX: usize = 30;
            enc.setVertexBuffer_offset_atIndex(
                Some(&vertex_buffer.raw),
                vertex_offset as usize,
                VERTEX_BUFFER_INDEX,
            );
        }

        apply_bindings_draw_both_stages(&enc, pipeline, bindings);

        unsafe {
            enc.drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferOffset(
                MTLPrimitiveType::Triangle,
                index_count as usize,
                MTLIndexType::UInt32,
                &index_buffer.raw,
                0,
            );
            enc.popDebugGroup();
            enc.endEncoding();
        }
    }

    /// Begin a render pass that stays alive across multiple draw calls.
    pub fn begin_render_pass(
        &mut self,
        target: &GpuTexture,
        load_action: crate::GpuLoadAction,
        label: &str,
    ) {
        self.end_current();
        self.render_cache.clear();

        let desc = new_render_pass_descriptor();
        let color = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        unsafe {
            color.setTexture(Some(&target.raw));
            color.setLoadAction(convert_load_action(load_action));
            color.setStoreAction(MTLStoreAction::Store);
            color.setClearColor(objc2_metal::MTLClearColor {
                red: 0.0,
                green: 0.0,
                blue: 0.0,
                alpha: 0.0,
            });
        }

        let enc = self
            .cmd_buf
            .renderCommandEncoderWithDescriptor(&desc)
            .expect("renderCommandEncoderWithDescriptor failed");
        unsafe {
            enc.pushDebugGroup(&NSString::from_str(label));
            enc.setViewport(MTLViewport {
                originX: 0.0,
                originY: 0.0,
                width: target.width as f64,
                height: target.height as f64,
                znear: 0.0,
                zfar: 1.0,
            });
        }

        self.state = EncoderState::Render(enc);
    }

    /// Draw indexed geometry within an active render pass.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_in_render_pass(
        &mut self,
        pipeline: &GpuRenderPipeline,
        bindings: &[GpuBinding],
        vertex_buffer: &GpuBuffer,
        vertex_offset: u64,
        index_buffer: &GpuBuffer,
        index_count: u32,
        index_buffer_offset: u64,
        viewport: Option<(f32, f32, f32, f32)>,
        label: &str,
    ) {
        let enc = match &self.state {
            EncoderState::Render(enc) => enc.clone(),
            _ => panic!("draw_in_render_pass called without active render pass"),
        };

        unsafe {
            enc.pushDebugGroup(&NSString::from_str(label));
            enc.setRenderPipelineState(&pipeline.state);

            if let Some((x, y, w, h)) = viewport {
                enc.setViewport(MTLViewport {
                    originX: x as f64,
                    originY: y as f64,
                    width: w as f64,
                    height: h as f64,
                    znear: 0.0,
                    zfar: 1.0,
                });
            }
        }

        const VERTEX_BUFFER_INDEX: usize = 30;
        let vb_id = buffer_identity(&vertex_buffer.raw);
        if self.render_cache.vertex_buf_30 == vb_id {
            unsafe {
                enc.setVertexBufferOffset_atIndex(vertex_offset as usize, VERTEX_BUFFER_INDEX);
            }
        } else {
            unsafe {
                enc.setVertexBuffer_offset_atIndex(
                    Some(&vertex_buffer.raw),
                    vertex_offset as usize,
                    VERTEX_BUFFER_INDEX,
                );
            }
            self.render_cache.vertex_buf_30 = vb_id;
        }

        for binding in bindings {
            match binding {
                GpuBinding::Buffer {
                    binding: b,
                    buffer,
                    offset,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    let idx = slot.metal_index as usize;
                    let id = buffer_identity(&buffer.raw);
                    if idx >= CACHE_SLOTS || self.render_cache.buffers[idx] != (id, *offset) {
                        unsafe {
                            enc.setVertexBuffer_offset_atIndex(
                                Some(&buffer.raw),
                                *offset as usize,
                                slot.metal_index as usize,
                            );
                            enc.setFragmentBuffer_offset_atIndex(
                                Some(&buffer.raw),
                                *offset as usize,
                                slot.metal_index as usize,
                            );
                        }
                        if idx < CACHE_SLOTS {
                            self.render_cache.buffers[idx] = (id, *offset);
                        }
                    }
                }
                GpuBinding::Texture {
                    binding: b,
                    texture,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    let idx = slot.metal_index as usize;
                    let id = texture_identity(&texture.raw);
                    if idx >= CACHE_SLOTS || self.render_cache.frag_textures[idx] != id {
                        unsafe {
                            enc.setVertexTexture_atIndex(
                                Some(&texture.raw),
                                slot.metal_index as usize,
                            );
                            enc.setFragmentTexture_atIndex(
                                Some(&texture.raw),
                                slot.metal_index as usize,
                            );
                        }
                        if idx < CACHE_SLOTS {
                            self.render_cache.frag_textures[idx] = id;
                        }
                    }
                }
                GpuBinding::Sampler {
                    binding: b,
                    sampler,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    let idx = slot.metal_index as usize;
                    let id = sampler_identity(&sampler.raw);
                    if idx >= CACHE_SLOTS || self.render_cache.frag_samplers[idx] != id {
                        unsafe {
                            enc.setVertexSamplerState_atIndex(
                                Some(&sampler.raw),
                                slot.metal_index as usize,
                            );
                            enc.setFragmentSamplerState_atIndex(
                                Some(&sampler.raw),
                                slot.metal_index as usize,
                            );
                        }
                        if idx < CACHE_SLOTS {
                            self.render_cache.frag_samplers[idx] = id;
                        }
                    }
                }
                GpuBinding::Bytes { binding: b, data } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    let idx = slot.metal_index as usize;
                    let id = (data.as_ptr(), data.len());
                    if idx >= CACHE_SLOTS || self.render_cache.bytes[idx] != id {
                        unsafe {
                            enc.setVertexBytes_length_atIndex(
                                NonNull::new(data.as_ptr() as *mut c_void).unwrap(),
                                data.len(),
                                slot.metal_index as usize,
                            );
                            enc.setFragmentBytes_length_atIndex(
                                NonNull::new(data.as_ptr() as *mut c_void).unwrap(),
                                data.len(),
                                slot.metal_index as usize,
                            );
                        }
                        if idx < CACHE_SLOTS {
                            self.render_cache.bytes[idx] = id;
                        }
                    }
                }
            }
        }

        unsafe {
            enc.drawIndexedPrimitives_indexCount_indexType_indexBuffer_indexBufferOffset(
                MTLPrimitiveType::Triangle,
                index_count as usize,
                MTLIndexType::UInt32,
                &index_buffer.raw,
                index_buffer_offset as usize,
            );
            enc.popDebugGroup();
        }
    }

    /// Set the scissor rectangle on the active render pass.
    pub fn set_scissor_rect(&mut self, x: u32, y: u32, w: u32, h: u32) {
        let enc = match &self.state {
            EncoderState::Render(enc) => enc.clone(),
            _ => panic!("set_scissor_rect called without active render pass"),
        };
        unsafe {
            enc.setScissorRect(MTLScissorRect {
                x: x as usize,
                y: y as usize,
                width: w as usize,
                height: h as usize,
            });
        }
    }

    /// End the active render pass.
    pub fn end_render_pass(&mut self) {
        if let EncoderState::Render(ref enc) = self.state {
            unsafe {
                enc.popDebugGroup();
                enc.endEncoding();
            }
            self.state = EncoderState::None;
        }
    }

    /// Clear a texture to a solid color.
    pub fn clear_texture(&mut self, texture: &GpuTexture, r: f64, g: f64, b: f64, a: f64) {
        let pipelines = unsafe { &*self.clear_pipelines };
        let has_write = unsafe { texture.raw.usage() }.contains(MTLTextureUsage::ShaderWrite);
        if let Some(pipeline) = pipelines.get(texture.format).filter(|_| has_write) {
            #[repr(C)]
            #[derive(Clone, Copy)]
            struct ClearColor {
                r: f32,
                g: f32,
                b: f32,
                a: f32,
            }

            let color = ClearColor {
                r: r as f32,
                g: g as f32,
                b: b as f32,
                a: a as f32,
            };
            let color_bytes: &[u8] = unsafe {
                std::slice::from_raw_parts(
                    &color as *const ClearColor as *const u8,
                    std::mem::size_of::<ClearColor>(),
                )
            };
            self.dispatch_compute(
                pipeline,
                &[
                    GpuBinding::Texture {
                        binding: 0,
                        texture,
                    },
                    GpuBinding::Bytes {
                        binding: 1,
                        data: color_bytes,
                    },
                ],
                [texture.width.div_ceil(16), texture.height.div_ceil(16), 1],
                "Clear Texture",
            );
        } else {
            self.end_current();
            let desc = new_render_pass_descriptor();
            let color_att = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
            unsafe {
                color_att.setTexture(Some(&texture.raw));
                color_att.setLoadAction(MTLLoadAction::Clear);
                color_att.setStoreAction(MTLStoreAction::Store);
                color_att.setClearColor(objc2_metal::MTLClearColor {
                    red: r,
                    green: g,
                    blue: b,
                    alpha: a,
                });
            }
            let enc = self
                .cmd_buf
                .renderCommandEncoderWithDescriptor(&desc)
                .expect("renderCommandEncoderWithDescriptor failed");
            enc.endEncoding();
        }
    }

    /// Fill a buffer with zeros via blit encoder.
    pub fn clear_buffer(&mut self, buffer: &GpuBuffer) {
        self.end_current();
        let enc = self
            .cmd_buf
            .blitCommandEncoder()
            .expect("blitCommandEncoder failed");
        unsafe {
            enc.fillBuffer_range_value(
                &buffer.raw,
                objc2_foundation::NSRange {
                    location: 0,
                    length: buffer.size as usize,
                },
                0,
            );
        }
        enc.endEncoding();
    }

    /// Generate the mipmap chain for a texture using Metal's optimized
    /// blit-encoder path.
    pub fn generate_mipmaps(&mut self, texture: &GpuTexture) {
        self.end_current();
        let enc = self
            .cmd_buf
            .blitCommandEncoder()
            .expect("blitCommandEncoder failed");
        unsafe { enc.generateMipmapsForTexture(&texture.raw) };
        enc.endEncoding();
    }

    /// Copy texture to texture via blit encoder.
    pub fn copy_texture_to_texture(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        width: u32,
        height: u32,
        depth: u32,
    ) {
        self.end_current();
        let enc = self
            .cmd_buf
            .blitCommandEncoder()
            .expect("blitCommandEncoder failed");
        unsafe {
            enc.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
                &src.raw,
                0,
                0,
                MTLOrigin { x: 0, y: 0, z: 0 },
                MTLSize {
                    width: width as usize,
                    height: height as usize,
                    depth: depth as usize,
                },
                &dst.raw,
                0,
                0,
                MTLOrigin { x: 0, y: 0, z: 0 },
            );
        }
        enc.endEncoding();
    }

    /// Copy texture to buffer via blit encoder (for readback).
    pub fn copy_texture_to_buffer(
        &mut self,
        src: &GpuTexture,
        dst: &GpuBuffer,
        width: u32,
        height: u32,
        bytes_per_row: u32,
    ) {
        self.end_current();
        let enc = self
            .cmd_buf
            .blitCommandEncoder()
            .expect("blitCommandEncoder failed");
        unsafe {
            enc.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage_options(
                &src.raw,
                0,
                0,
                MTLOrigin { x: 0, y: 0, z: 0 },
                MTLSize {
                    width: width as usize,
                    height: height as usize,
                    depth: 1,
                },
                &dst.raw,
                0,
                bytes_per_row as usize,
                (bytes_per_row as usize) * (height as usize),
                MTLBlitOption::empty(),
            );
        }
        enc.endEncoding();
    }

    /// Upload CPU data to a 2D texture region via replaceRegion.
    pub fn upload_texture(
        &mut self,
        texture: &GpuTexture,
        width: u32,
        height: u32,
        _depth: u32,
        data: &[u8],
    ) {
        self.end_current();
        let bpp = texture.format.bytes_per_pixel();
        let bytes_per_row = width as u64 * bpp as u64;
        let region = objc2_metal::MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize {
                width: width as usize,
                height: height as usize,
                depth: 1,
            },
        };
        unsafe {
            texture.raw.replaceRegion_mipmapLevel_withBytes_bytesPerRow(
                region,
                0,
                NonNull::new(data.as_ptr() as *mut c_void).unwrap(),
                bytes_per_row as usize,
            );
        }
    }

    /// Signal a shared event on the GPU timeline.
    pub fn signal_event(&mut self, event: &GpuEvent) {
        let value = event.counter.get() + 1;
        event.counter.set(value);
        self.end_current();
        unsafe {
            self.cmd_buf.encodeSignalEvent_value(
                ProtocolObject::from_ref(event.raw()),
                value,
            );
        }
    }

    /// Signal a shared event with a specific value (does NOT auto-increment).
    pub fn signal_event_value(&mut self, event: &GpuEvent, value: u64) {
        self.end_current();
        unsafe {
            self.cmd_buf.encodeSignalEvent_value(
                ProtocolObject::from_ref(event.raw()),
                value,
            );
        }
    }

    /// Wait for a shared event to reach a specific value before executing
    /// subsequent GPU work on this command buffer.
    pub fn wait_event(&mut self, event: &GpuEvent, value: u64) {
        self.end_current();
        unsafe {
            self.cmd_buf.encodeWaitForEvent_value(
                ProtocolObject::from_ref(event.raw()),
                value,
            );
        }
    }

    /// Encode a MetalFX spatial upscale (src → dst).
    pub fn encode_metalfx_upscale(
        &mut self,
        scaler: &metalfx::MetalFxSpatialScaler,
        src: &GpuTexture,
        dst: &GpuTexture,
    ) {
        self.end_current();
        scaler.encode(&self.cmd_buf, src, dst);
    }

    /// Encode an MPS Lanczos upscale (src → dst).
    pub fn encode_mps_upscale(
        &mut self,
        scaler: &mps::MpsLanczosScale,
        src: &GpuTexture,
        dst: &GpuTexture,
    ) {
        self.end_current();
        scaler.set_transform(&mps::MpsScaleTransform {
            scale_x: dst.width as f64 / src.width as f64,
            scale_y: dst.height as f64 / src.height as f64,
            translate_x: 0.0,
            translate_y: 0.0,
        });
        scaler.encode(&self.cmd_buf, &src.raw, &dst.raw);
    }

    /// Register a callback to run when the GPU finishes executing this command buffer.
    pub fn add_completed_handler<F: Fn() + Send + 'static>(&self, callback: F) {
        use block2::RcBlock;
        let block = RcBlock::new(move |_buf: NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
            callback();
        });
        unsafe {
            self.cmd_buf.addCompletedHandler(RcBlock::as_ptr(&block));
        }
    }

    /// Register a diagnostic completed handler that logs GPU errors.
    pub fn add_completed_handler_with_status(&self, label: &str) {
        use block2::RcBlock;
        use objc2_metal::MTLCommandBufferStatus;

        let label = label.to_string();
        let block = RcBlock::new(move |buf: NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
            let cb = unsafe { buf.as_ref() };
            let status = unsafe { cb.status() };
            if status == MTLCommandBufferStatus::Error {
                let err = unsafe { cb.error() };
                let (code, desc) = match err {
                    None => (-1i64, String::from("(nil)")),
                    Some(err) => {
                        let code = err.code() as i64;
                        let desc = err.localizedDescription().to_string();
                        (code, desc)
                    }
                };
                log::error!(
                    "[GPU] Command buffer '{}' error (code={}): {}",
                    label, code, desc,
                );
            }
        });
        unsafe {
            self.cmd_buf.addCompletedHandler(RcBlock::as_ptr(&block));
        }
    }

    /// Commit the command buffer to the GPU queue.
    pub fn commit(mut self) {
        self.end_current();
        self.cmd_buf.commit();
    }

    /// Commit and block until the GPU has scheduled (not completed) the work.
    pub fn commit_and_wait_scheduled(mut self) {
        self.end_current();
        self.cmd_buf.commit();
        unsafe { self.cmd_buf.waitUntilScheduled() };
    }
}

impl Drop for GpuEncoder {
    fn drop(&mut self) {
        self.end_current();
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────

fn new_render_pass_descriptor() -> Retained<MTLRenderPassDescriptor> {
    // MTLRenderPassDescriptor is a class (NSObject subclass); use new.
    unsafe {
        use objc2::AnyThread;
        MTLRenderPassDescriptor::init(MTLRenderPassDescriptor::alloc())
    }
}

fn convert_load_action(la: crate::GpuLoadAction) -> MTLLoadAction {
    match la {
        crate::GpuLoadAction::Clear => MTLLoadAction::Clear,
        crate::GpuLoadAction::Load => MTLLoadAction::Load,
        crate::GpuLoadAction::DontCare => MTLLoadAction::DontCare,
    }
}

/// Apply bindings for a fullscreen-triangle draw: buffer→fragment only, texture/sampler
/// on both stages (vertex may sample too, Metal ignores unused bindings).
fn apply_bindings_draw_fullscreen(
    enc: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    pipeline: &GpuRenderPipeline,
    bindings: &[GpuBinding],
) {
    for binding in bindings {
        match binding {
            GpuBinding::Buffer {
                binding: b,
                buffer,
                offset,
            } => {
                let Some(slot) = pipeline.slot_map.get(*b) else {
                    continue;
                };
                unsafe {
                    enc.setFragmentBuffer_offset_atIndex(
                        Some(&buffer.raw),
                        *offset as usize,
                        slot.metal_index as usize,
                    );
                }
            }
            GpuBinding::Texture {
                binding: b,
                texture,
            } => {
                let Some(slot) = pipeline.slot_map.get(*b) else {
                    continue;
                };
                unsafe {
                    enc.setVertexTexture_atIndex(Some(&texture.raw), slot.metal_index as usize);
                    enc.setFragmentTexture_atIndex(Some(&texture.raw), slot.metal_index as usize);
                }
            }
            GpuBinding::Sampler {
                binding: b,
                sampler,
            } => {
                let Some(slot) = pipeline.slot_map.get(*b) else {
                    continue;
                };
                unsafe {
                    enc.setVertexSamplerState_atIndex(
                        Some(&sampler.raw),
                        slot.metal_index as usize,
                    );
                    enc.setFragmentSamplerState_atIndex(
                        Some(&sampler.raw),
                        slot.metal_index as usize,
                    );
                }
            }
            GpuBinding::Bytes { binding: b, data } => {
                let Some(slot) = pipeline.slot_map.get(*b) else {
                    continue;
                };
                unsafe {
                    enc.setFragmentBytes_length_atIndex(
                        NonNull::new(data.as_ptr() as *mut c_void).unwrap(),
                        data.len(),
                        slot.metal_index as usize,
                    );
                }
            }
        }
    }
}

/// Apply bindings on both vertex and fragment stages (used for draw_instanced et al.).
fn apply_bindings_draw_both_stages(
    enc: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    pipeline: &GpuRenderPipeline,
    bindings: &[GpuBinding],
) {
    for binding in bindings {
        match binding {
            GpuBinding::Buffer {
                binding: b,
                buffer,
                offset,
            } => {
                let Some(slot) = pipeline.slot_map.get(*b) else {
                    continue;
                };
                unsafe {
                    enc.setVertexBuffer_offset_atIndex(
                        Some(&buffer.raw),
                        *offset as usize,
                        slot.metal_index as usize,
                    );
                    enc.setFragmentBuffer_offset_atIndex(
                        Some(&buffer.raw),
                        *offset as usize,
                        slot.metal_index as usize,
                    );
                }
            }
            GpuBinding::Texture {
                binding: b,
                texture,
            } => {
                let Some(slot) = pipeline.slot_map.get(*b) else {
                    continue;
                };
                unsafe {
                    enc.setVertexTexture_atIndex(Some(&texture.raw), slot.metal_index as usize);
                    enc.setFragmentTexture_atIndex(Some(&texture.raw), slot.metal_index as usize);
                }
            }
            GpuBinding::Sampler {
                binding: b,
                sampler,
            } => {
                let Some(slot) = pipeline.slot_map.get(*b) else {
                    continue;
                };
                unsafe {
                    enc.setVertexSamplerState_atIndex(
                        Some(&sampler.raw),
                        slot.metal_index as usize,
                    );
                    enc.setFragmentSamplerState_atIndex(
                        Some(&sampler.raw),
                        slot.metal_index as usize,
                    );
                }
            }
            GpuBinding::Bytes { binding: b, data } => {
                let Some(slot) = pipeline.slot_map.get(*b) else {
                    continue;
                };
                unsafe {
                    enc.setVertexBytes_length_atIndex(
                        NonNull::new(data.as_ptr() as *mut c_void).unwrap(),
                        data.len(),
                        slot.metal_index as usize,
                    );
                    enc.setFragmentBytes_length_atIndex(
                        NonNull::new(data.as_ptr() as *mut c_void).unwrap(),
                        data.len(),
                        slot.metal_index as usize,
                    );
                }
            }
        }
    }
}

