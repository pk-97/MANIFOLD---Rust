//! GpuEncoder — per-frame GPU command encoder wrapping a retained Metal command buffer.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLBlitOption, MTLBlitPassDescriptor, MTLCommandBuffer,
    MTLCommandEncoder, MTLComputeCommandEncoder, MTLComputePassDescriptor, MTLIndexType,
    MTLLoadAction, MTLMultisampleDepthResolveFilter, MTLOrigin, MTLPrimitiveType,
    MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLScissorRect, MTLSize, MTLStoreAction,
    MTLTexture, MTLTextureUsage, MTLViewport,
};

use super::profiling::{self, ProfileState};
use super::*;
use crate::types::*;

/// Encoder state — tracks the current active Metal encoder.
pub(crate) enum EncoderState {
    None,
    Compute(Retained<ProtocolObject<dyn MTLComputeCommandEncoder>>),
    Render(Retained<ProtocolObject<dyn MTLRenderCommandEncoder>>),
}

/// Cached compute bind state — skips redundant Metal API calls when the same
/// resource is already bound at a slot from a previous dispatch.
const CACHE_SLOTS: usize = 16;

/// Upper bound on the number of Metal buffer slots naga's sizes buffer can
/// describe. The sizes buffer is an array of `u32` indexed by metal buffer
/// slot — entry `i` holds the byte-size of the buffer bound at slot `i`,
/// used by SPIRV-Cross-emitted MSL to resolve `arrayLength()` calls on
/// runtime-sized storage arrays. Raising this means the `[u32; N]` scratch
/// grows; Metal itself allows up to 31 buffer args per stage so 32 covers
/// the entire addressable slot space.
const MAX_BUFFER_SLOTS: usize = 32;

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
    /// Per-dispatch timestamp profiling state. `None` (the default) costs
    /// one branch per dispatch; see [`Self::enable_dispatch_profiling`].
    pub(crate) profile: Option<ProfileState>,
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

/// One depth-tested mesh in a [`GpuEncoder::draw_instanced_depth_msaa_batch`]
/// batch: its own material pipeline, its own bindings, its own vertex count.
/// Construct via [`GpuEncoder::depth_msaa_draw`].
#[derive(Clone, Copy)]
pub struct DepthMsaaDraw<'a> {
    pipeline: &'a GpuRenderPipeline,
    bindings: &'a [GpuBinding<'a>],
    vertex_count: u32,
    instance_count: u32,
}

/// Committed shape (`docs/GBUFFER_DESIGN.md` §2 D3) for
/// [`GpuEncoder::draw_instanced_depth_msaa_batch_desc`] — the desc-struct
/// seam that lets ONE batch entry point grow optional G-buffer attachments
/// instead of a parallel `_with_depth` function per attachment combination.
/// [`GpuEncoder::draw_instanced_depth_msaa_batch`] (unchanged signature)
/// forwards here with `depth_resolve: None, aux_color: &[]` — byte-identical
/// to the pre-D3 pass (I1/I4).
pub struct DepthMsaaPassDesc<'a> {
    pub msaa_color: &'a GpuTexture,
    pub resolve_target: &'a GpuTexture,
    pub msaa_depth: &'a GpuTexture,
    /// `Some(tex)` → the depth attachment stores `MultisampleResolve`
    /// (filter `Sample0` — D2: deterministic, matches a single-sample
    /// render, unlike `Min`/`Max`) into this single-sample `R32Float`
    /// texture. `None` → `DontCare`, exactly today's memoryless depth.
    pub depth_resolve: Option<&'a GpuTexture>,
    /// Extra MRT color attachments (index 1..), each `(msaa_tex,
    /// resolve_tex)`. Reserved for P2's velocity output; empty slice today
    /// produces exactly the pre-D3 single-color-attachment pass.
    pub aux_color: &'a [(&'a GpuTexture, &'a GpuTexture)],
    pub depth_stencil_state: &'a GpuDepthStencilState,
    /// IMPORT_FIDELITY_DESIGN.md D8/F-P5: an optional second draw group,
    /// run immediately after `draws` within the SAME render pass — no
    /// second clear, no new pass, just a `setDepthStencilState` switch
    /// before the second group's draw calls. Used for the sorted
    /// transparent pass (depth test on, write off) layered over the
    /// opaque pass's already-resolved-on-tile depth. `None` reproduces
    /// exactly today's single-group pass (I1).
    pub second_pass: Option<(&'a GpuDepthStencilState, &'a [DepthMsaaDraw<'a>])>,
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
        }
    }

    // ─── Per-dispatch timestamp profiling ────────────────────────────

    /// Enable per-dispatch GPU timestamp profiling for this frame's command
    /// buffer. Every compute dispatch gets its own encoder with start/end
    /// timestamp samples; render and blit passes get boundary samples on
    /// their pass descriptors. Retrieve results via
    /// [`Self::commit_and_wait_profiled`]. The `device` is needed for the
    /// CPU/GPU timestamp calibration pair.
    pub fn enable_dispatch_profiling(&mut self, sampler: GpuTimestampSampler, device: &GpuDevice) {
        let calib_start = profiling::sample_cpu_gpu(device.raw_device());
        self.profile = Some(ProfileState {
            sampler,
            spans: Vec::new(),
            tag: String::new(),
            overflow: 0,
            calib_start,
        });
    }

    /// Set the attribution tag stamped onto subsequently profiled spans.
    /// The host (graph executor) calls this per step so GPU spans can be
    /// joined back to nodes. No-op when profiling is off.
    pub fn set_profile_tag(&mut self, tag: &str) {
        if let Some(p) = &mut self.profile {
            p.tag.clear();
            p.tag.push_str(tag);
        }
    }

    /// Commit, wait for completion, and resolve the frame's profiled spans.
    /// Works on an unprofiled encoder too (empty span list, total only).
    pub fn commit_and_wait_profiled(mut self, device: &GpuDevice) -> GpuFrameProfile {
        self.end_current();
        self.cmd_buf.commit();
        let total_ms = unsafe {
            self.cmd_buf.waitUntilCompleted();
            (self.cmd_buf.GPUEndTime() - self.cmd_buf.GPUStartTime()).max(0.0) * 1000.0
        };
        match self.profile.take() {
            Some(state) => {
                let calib_end = profiling::sample_cpu_gpu(device.raw_device());
                profiling::resolve(&state, calib_end, total_ms)
            }
            None => GpuFrameProfile {
                total_ms,
                ..Default::default()
            },
        }
    }

    /// Profiled-mode compute encoder: ends the current encoder and opens a
    /// fresh one whose stage boundaries write timestamp samples. Falls back
    /// to the plain shared encoder when the sample buffer is full.
    fn begin_profiled_compute(
        &mut self,
        label: &str,
    ) -> Retained<ProtocolObject<dyn MTLComputeCommandEncoder>> {
        self.end_current();
        let Some((start, end)) = self
            .profile
            .as_mut()
            .and_then(|p| p.reserve(label, GpuWorkKind::Compute))
        else {
            return self.ensure_compute();
        };
        let sample_buffer = self
            .profile
            .as_ref()
            .map(|p| p.sampler.buffer.clone())
            .expect("profile state present");
        let desc = MTLComputePassDescriptor::computePassDescriptor();
        unsafe {
            let att = desc.sampleBufferAttachments().objectAtIndexedSubscript(0);
            att.setSampleBuffer(Some(&sample_buffer));
            att.setStartOfEncoderSampleIndex(start);
            att.setEndOfEncoderSampleIndex(end);
        }
        let enc = self
            .cmd_buf
            .computeCommandEncoderWithDescriptor(&desc)
            .expect("computeCommandEncoderWithDescriptor failed");
        self.state = EncoderState::Compute(enc.clone());
        enc
    }

    /// Create a render encoder from `desc`, attaching boundary timestamp
    /// samples when profiling. All render-pass creation routes through here.
    fn make_render_encoder(
        &mut self,
        desc: &MTLRenderPassDescriptor,
        label: &str,
    ) -> Retained<ProtocolObject<dyn MTLRenderCommandEncoder>> {
        if let Some((start, end)) = self
            .profile
            .as_mut()
            .and_then(|p| p.reserve(label, GpuWorkKind::Render))
        {
            let sample_buffer = self
                .profile
                .as_ref()
                .map(|p| p.sampler.buffer.clone())
                .expect("profile state present");
            unsafe {
                let att = desc.sampleBufferAttachments().objectAtIndexedSubscript(0);
                att.setSampleBuffer(Some(&sample_buffer));
                att.setStartOfVertexSampleIndex(start);
                att.setEndOfFragmentSampleIndex(end);
            }
        }
        self.cmd_buf
            .renderCommandEncoderWithDescriptor(desc)
            .expect("renderCommandEncoderWithDescriptor failed")
    }

    /// Create a blit encoder, attaching boundary timestamp samples when
    /// profiling. All blit-encoder creation routes through here.
    fn make_blit_encoder(
        &mut self,
        label: &str,
    ) -> Retained<ProtocolObject<dyn MTLBlitCommandEncoder>> {
        if let Some((start, end)) = self
            .profile
            .as_mut()
            .and_then(|p| p.reserve(label, GpuWorkKind::Blit))
        {
            let sample_buffer = self
                .profile
                .as_ref()
                .map(|p| p.sampler.buffer.clone())
                .expect("profile state present");
            let desc = unsafe { MTLBlitPassDescriptor::blitPassDescriptor() };
            unsafe {
                let att = desc.sampleBufferAttachments().objectAtIndexedSubscript(0);
                att.setSampleBuffer(Some(&sample_buffer));
                att.setStartOfEncoderSampleIndex(start);
                att.setEndOfEncoderSampleIndex(end);
            }
            return self
                .cmd_buf
                .blitCommandEncoderWithDescriptor(&desc)
                .expect("blitCommandEncoderWithDescriptor failed");
        }
        self.cmd_buf
            .blitCommandEncoder()
            .expect("blitCommandEncoder failed")
    }

    /// Dispatch a compute shader.
    pub fn dispatch_compute(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        workgroups: [u32; 3],
        label: &str,
    ) {
        let enc = if self.profile.is_some() {
            self.begin_profiled_compute(label)
        } else {
            self.ensure_compute()
        };
        unsafe {
            enc.pushDebugGroup(&NSString::from_str(label));
            enc.setComputePipelineState(&pipeline.state);
        }

        let (buffer_sizes, buffer_sizes_len) = collect_buffer_sizes(&pipeline.slot_map, bindings);

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
                    let idx = slot.metal_index as usize;
                    unsafe {
                        enc.setBytes_length_atIndex(
                            NonNull::new(data.as_ptr() as *mut c_void).unwrap(),
                            data.len(),
                            idx,
                        );
                    }
                    // Invalidate the buffer cache for this slot — `setBytes`
                    // replaces the slot's active binding with inline data,
                    // so a subsequent `GpuBinding::Buffer` at the same slot
                    // MUST call `setBuffer` to restore the buffer binding.
                    // Without this, the cache hit on `(buffer_identity,
                    // offset)` from a prior frame's dispatch would skip the
                    // `setBuffer` call and leave the slot still pointing at
                    // these inline bytes — the shader then reads/writes
                    // through inline data sized like uniforms instead of
                    // the intended storage buffer, producing GPU page
                    // faults at the next dispatch. The fluid sim chain
                    // tripped this every frame: gaussian_blur's 32-byte
                    // params at metal slot 0 left the cache stale for
                    // fluid_simulate's particle buffer at the same slot.
                    if idx < CACHE_SLOTS {
                        self.compute_cache.buffers[idx] = (std::ptr::null(), 0);
                    }
                }
            }
        }

        if pipeline.needs_sizes_buffer {
            let slot_idx = pipeline
                .slot_map
                .get(SIZES_BUFFER_BINDING)
                .expect("sizes buffer slot missing")
                .metal_index as usize;
            unsafe {
                enc.setBytes_length_atIndex(
                    NonNull::new(buffer_sizes.as_ptr() as *mut c_void).unwrap(),
                    buffer_sizes_len * 4,
                    slot_idx,
                );
            }
            // Sizes-buffer slot index varies across pipelines (it's
            // assigned after the user bindings). A pipeline with N user
            // buffers uses slot N here; a subsequent pipeline that
            // binds a real buffer at slot N would otherwise hit a
            // stale-cache skip — same bug class as the user-side
            // `Bytes` arm above.
            if slot_idx < CACHE_SLOTS {
                self.compute_cache.buffers[slot_idx] = (std::ptr::null(), 0);
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

    /// Dispatch a compute shader that also binds a Metal acceleration
    /// structure (RAYTRACING_DESIGN.md P1). `dispatch_compute`'s
    /// `GpuBinding` set has no acceleration-structure variant — Metal
    /// ray tracing binds it via a distinct `setAccelerationStructure:
    /// atBufferIndex:` call, not `setBuffer:`. `accel_binding` is the
    /// pipeline's WGSL-style @binding(N) for the accel structure (looked
    /// up through the same `SlotMap` as every other binding); `bindings`
    /// covers the rest exactly like `dispatch_compute`. Not part of the
    /// per-slot resource cache (`ComputeBindCache`) that `dispatch_compute`
    /// uses — this dispatches once or twice per frame (the shadow-ray
    /// pass), not the many-dispatches-per-frame case the cache exists for.
    pub fn dispatch_compute_with_accel(
        &mut self,
        pipeline: &GpuComputePipeline,
        accel_binding: u32,
        accel: &super::raytrace::RtAccel,
        bindings: &[GpuBinding],
        workgroups: [u32; 3],
        label: &str,
    ) {
        let enc = if self.profile.is_some() {
            self.begin_profiled_compute(label)
        } else {
            self.ensure_compute()
        };
        unsafe {
            enc.pushDebugGroup(&NSString::from_str(label));
            enc.setComputePipelineState(&pipeline.state);
            if let Some(slot) = pipeline.slot_map.get(accel_binding) {
                enc.setAccelerationStructure_atBufferIndex(
                    Some(&accel.structure),
                    slot.metal_index as usize,
                );
            }
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
                    unsafe {
                        enc.setBuffer_offset_atIndex(
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
                        enc.setTexture_atIndex(Some(&texture.raw), slot.metal_index as usize);
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
                        enc.setSamplerState_atIndex(
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
                        enc.setBytes_length_atIndex(
                            NonNull::new(data.as_ptr() as *mut c_void).unwrap(),
                            data.len(),
                            slot.metal_index as usize,
                        );
                    }
                }
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
        // Cross-dispatch cache invalidation: this path doesn't populate
        // `compute_cache`, but a subsequent `dispatch_compute` call in the
        // same encoder must not skip a `setBuffer`/`setTexture` because
        // the cache still thinks a slot holds what it held before this
        // accel-structure dispatch touched it. Clear the cache wholesale
        // — cheap (one dispatch/frame) and correct, vs. tracking exactly
        // which slots this call touched.
        self.compute_cache.clear();
    }

    /// Insert a buffer-scope memory barrier on the active compute encoder.
    /// Required when a subsequent dispatch in the same encoder must observe
    /// the writes (storage buffers, atomics) from a preceding dispatch.
    /// Metal does NOT implicitly serialize dispatch-to-dispatch resource
    /// access within a single MTLComputeCommandEncoder — without this, a
    /// downstream compact→consume pattern can read partially-written data.
    /// No-op when no compute encoder is active.
    pub fn compute_memory_barrier_buffers(&mut self) {
        if let EncoderState::Compute(ref enc) = self.state {
            unsafe {
                enc.memoryBarrierWithScope(objc2_metal::MTLBarrierScope::Buffers);
            }
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

        let enc = self.make_render_encoder(&desc, label);
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

        let enc = self.make_render_encoder(&desc, label);
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

        let enc = self.make_render_encoder(&desc, label);
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

        let enc = self.make_render_encoder(&desc, label);
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

    /// One draw in a [`Self::draw_instanced_depth_msaa_batch`] call.
    ///
    /// Each entry carries its own pipeline and bindings so a scene of
    /// distinct materials composites in a single render pass.
    pub fn depth_msaa_draw<'a>(
        pipeline: &'a GpuRenderPipeline,
        bindings: &'a [GpuBinding<'a>],
        vertex_count: u32,
        instance_count: u32,
    ) -> DepthMsaaDraw<'a> {
        DepthMsaaDraw {
            pipeline,
            bindings,
            vertex_count,
            instance_count,
        }
    }

    /// Draw a batch of depth-tested triangle meshes into ONE 4x-MSAA pass,
    /// resolving to `resolve_target`.
    ///
    /// All `draws` share one memoryless multisample color + depth target
    /// (Apple Silicon tile memory — never leaves the GPU), cleared once at
    /// pass start, so the shared depth buffer resolves inter-object
    /// occlusion exactly as the old one-pass-per-object path did. The pass
    /// ends with `MultisampleResolve`, writing the antialiased result to
    /// the single-sample `resolve_target`. Pair with a pipeline built via
    /// `create_render_pipeline_depth_msaa` (alpha-to-coverage on) so cutout
    /// edges resolve too, not just silhouettes.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub fn draw_instanced_depth_msaa_batch(
        &mut self,
        msaa_color: &GpuTexture,
        resolve_target: &GpuTexture,
        msaa_depth: &GpuTexture,
        depth_stencil_state: &GpuDepthStencilState,
        draws: &[DepthMsaaDraw],
        label: &str,
    ) {
        let desc = DepthMsaaPassDesc {
            msaa_color,
            resolve_target,
            msaa_depth,
            depth_resolve: None,
            aux_color: &[],
            depth_stencil_state,
            second_pass: None,
        };
        self.draw_instanced_depth_msaa_batch_desc(&desc, draws, label);
    }

    /// [`Self::draw_instanced_depth_msaa_batch`]'s desc-driven superset
    /// (`docs/GBUFFER_DESIGN.md` §2 D3). Same single 4x-MSAA pass, same
    /// shared depth buffer resolving inter-object occlusion; additionally:
    /// `desc.depth_resolve` — `Some(tex)` stores the depth attachment via
    /// `MultisampleResolve` with filter `Sample0` into `tex` (single-sample
    /// `R32Float`, raw non-linear clip depth); `None` keeps today's
    /// `DontCare` (memoryless, never leaves the GPU). `desc.aux_color` —
    /// extra MRT color attachments (index 1..), reserved for P2's velocity
    /// output; empty today.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_instanced_depth_msaa_batch_desc(
        &mut self,
        desc: &DepthMsaaPassDesc,
        draws: &[DepthMsaaDraw],
        label: &str,
    ) {
        self.end_current();

        let pass_desc = new_render_pass_descriptor();

        let color = unsafe { pass_desc.colorAttachments().objectAtIndexedSubscript(0) };
        unsafe {
            color.setTexture(Some(&desc.msaa_color.raw));
            color.setResolveTexture(Some(&desc.resolve_target.raw));
            color.setLoadAction(MTLLoadAction::Clear);
            color.setStoreAction(MTLStoreAction::MultisampleResolve);
            color.setClearColor(objc2_metal::MTLClearColor {
                red: 0.0,
                green: 0.0,
                blue: 0.0,
                alpha: 0.0,
            });
        }

        // Reserved MRT slots (P2: one for velocity). Each aux attachment
        // gets its own memoryless multisample + resolve target, same
        // Clear/MultisampleResolve shape as the primary color attachment.
        for (i, (aux_msaa, aux_resolve)) in desc.aux_color.iter().enumerate() {
            let idx = i + 1;
            let aux = unsafe {
                pass_desc
                    .colorAttachments()
                    .objectAtIndexedSubscript(idx)
            };
            unsafe {
                aux.setTexture(Some(&aux_msaa.raw));
                aux.setResolveTexture(Some(&aux_resolve.raw));
                aux.setLoadAction(MTLLoadAction::Clear);
                aux.setStoreAction(MTLStoreAction::MultisampleResolve);
                aux.setClearColor(objc2_metal::MTLClearColor {
                    red: 0.0,
                    green: 0.0,
                    blue: 0.0,
                    alpha: 0.0,
                });
            }
        }

        let depth = unsafe { pass_desc.depthAttachment() };
        unsafe {
            depth.setTexture(Some(&desc.msaa_depth.raw));
            depth.setLoadAction(MTLLoadAction::Clear);
            depth.setClearDepth(1.0);
            match desc.depth_resolve {
                Some(resolve) => {
                    depth.setResolveTexture(Some(&resolve.raw));
                    depth.setStoreAction(MTLStoreAction::MultisampleResolve);
                    // D2: deterministic, matches a single-sample render —
                    // Min/Max bias edges toward one surface and can't be
                    // predicted by the CPU oracle without re-implementing
                    // MSAA sample positions.
                    depth.setDepthResolveFilter(MTLMultisampleDepthResolveFilter::Sample0);
                }
                None => {
                    depth.setStoreAction(MTLStoreAction::DontCare);
                }
            }
        }

        let enc = self.make_render_encoder(&pass_desc, label);
        unsafe {
            enc.pushDebugGroup(&NSString::from_str(label));
            enc.setDepthStencilState(Some(&desc.depth_stencil_state.raw));
            enc.setViewport(MTLViewport {
                originX: 0.0,
                originY: 0.0,
                width: desc.resolve_target.width as f64,
                height: desc.resolve_target.height as f64,
                znear: 0.0,
                zfar: 1.0,
            });
        }

        for draw in draws {
            if draw.vertex_count == 0 || draw.instance_count == 0 {
                continue;
            }
            unsafe {
                enc.setRenderPipelineState(&draw.pipeline.state);
            }
            apply_bindings_draw_both_stages(&enc, draw.pipeline, draw.bindings);
            unsafe {
                enc.drawPrimitives_vertexStart_vertexCount_instanceCount(
                    MTLPrimitiveType::Triangle,
                    0,
                    draw.vertex_count as usize,
                    draw.instance_count as usize,
                );
            }
        }

        // IMPORT_FIDELITY_DESIGN.md D8/F-P5: the sorted transparent group,
        // drawn into the SAME pass right after the opaque group — no second
        // clear, so the opaque pass's on-tile MSAA depth is still resolved
        // against (occlusion by opaque geometry works for free), just with
        // depth WRITE off so transparent objects never occlude each other
        // or later opaque work.
        if let Some((second_depth_stencil, second_draws)) = desc.second_pass {
            unsafe {
                enc.setDepthStencilState(Some(&second_depth_stencil.raw));
            }
            for draw in second_draws {
                if draw.vertex_count == 0 || draw.instance_count == 0 {
                    continue;
                }
                unsafe {
                    enc.setRenderPipelineState(&draw.pipeline.state);
                }
                apply_bindings_draw_both_stages(&enc, draw.pipeline, draw.bindings);
                unsafe {
                    enc.drawPrimitives_vertexStart_vertexCount_instanceCount(
                        MTLPrimitiveType::Triangle,
                        0,
                        draw.vertex_count as usize,
                        draw.instance_count as usize,
                    );
                }
            }
        }

        unsafe {
            enc.popDebugGroup();
            enc.endEncoding();
        }
    }

    /// Draw a batch of meshes into ONE single-sample colour+depth pass with
    /// independently controlled load actions per attachment
    /// (`GLTF_MATERIAL_EXTENSIONS_DESIGN.md` E2a). The non-MSAA sibling of
    /// [`Self::draw_instanced_depth_msaa_batch_desc`]: no memoryless
    /// multisample scratch, no resolve — `target`/`depth_target` are real,
    /// already-populated single-sample textures. `Load` on both lets the
    /// caller composite a sorted transparent group onto a prior opaque
    /// pass's already-resolved colour AND depth-test against that pass's
    /// depth, without re-clearing either attachment. Depth `StoreAction` is
    /// always `DontCare` — unlike [`Self::draw_instanced_depth_only_batch`]
    /// (the shadow-map primitive, whose Store IS the useful output),
    /// nothing reads this pass's depth afterward.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_instanced_depth_batch(
        &mut self,
        target: &GpuTexture,
        depth_target: &GpuTexture,
        depth_stencil_state: &GpuDepthStencilState,
        draws: &[DepthMsaaDraw],
        color_load: crate::GpuLoadAction,
        depth_load: crate::GpuLoadAction,
        label: &str,
    ) {
        self.end_current();

        let desc = new_render_pass_descriptor();

        let color = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        unsafe {
            color.setTexture(Some(&target.raw));
            color.setLoadAction(convert_load_action(color_load));
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
            depth.setLoadAction(convert_load_action(depth_load));
            depth.setStoreAction(MTLStoreAction::DontCare);
            depth.setClearDepth(1.0);
        }

        let enc = self.make_render_encoder(&desc, label);
        unsafe {
            enc.pushDebugGroup(&NSString::from_str(label));
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

        for draw in draws {
            if draw.vertex_count == 0 || draw.instance_count == 0 {
                continue;
            }
            unsafe {
                enc.setRenderPipelineState(&draw.pipeline.state);
            }
            apply_bindings_draw_both_stages(&enc, draw.pipeline, draw.bindings);
            unsafe {
                enc.drawPrimitives_vertexStart_vertexCount_instanceCount(
                    MTLPrimitiveType::Triangle,
                    0,
                    draw.vertex_count as usize,
                    draw.instance_count as usize,
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

        let enc = self.make_render_encoder(&desc, label);
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
            // Transparent-black clear: this renders generator geometry to an
            // offscreen target that is then composited (premultiplied alpha).
            // An opaque clear would paint a solid box over the layer below where
            // no geometry is drawn; the opaque geometry fragments write alpha=1
            // themselves, so the background must clear to alpha=0 to key.
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

        let enc = self.make_render_encoder(&desc, label);
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

    /// Draw a batch of meshes into ONE **depth-only** render pass — no colour
    /// attachment at all. The shadow-map primitive: every object's geometry
    /// is rasterised into a single caster's depth map (cleared once at pass
    /// start), and the fragment stage writes no colour (the pipeline is built
    /// with [`GpuDevice::create_render_pipeline_depth_only`], whose WGSL has a
    /// void `@fragment`). Depth `StoreAction::Store` — unlike the MSAA colour
    /// batch, the depth result IS the output: it is sampled as a shadow map
    /// (`texture_depth_2d`) in the later lit pass.
    ///
    /// One pass per caster with all objects batched inside keeps the shadow
    /// bill at K passes, not K×objects — the difference between a few passes
    /// and hundreds. `depth_target` must be created
    /// `RENDER_TARGET | SHADER_READ` so it can be sampled afterwards without
    /// tripping the AGX render-target-only bind crash (0x78).
    pub fn draw_instanced_depth_only_batch(
        &mut self,
        depth_target: &GpuTexture,
        depth_stencil_state: &GpuDepthStencilState,
        draws: &[DepthMsaaDraw],
        label: &str,
    ) {
        self.end_current();

        let desc = new_render_pass_descriptor();

        // No colour attachment — depth only. Set render-target dimensions
        // explicitly so a colourless pass is never ambiguous to Metal.
        let depth = unsafe { desc.depthAttachment() };
        unsafe {
            depth.setTexture(Some(&depth_target.raw));
            depth.setLoadAction(MTLLoadAction::Clear);
            depth.setStoreAction(MTLStoreAction::Store);
            depth.setClearDepth(1.0);
            desc.setRenderTargetWidth(depth_target.width as usize);
            desc.setRenderTargetHeight(depth_target.height as usize);
        }

        let enc = self.make_render_encoder(&desc, label);
        unsafe {
            enc.pushDebugGroup(&NSString::from_str(label));
            enc.setDepthStencilState(Some(&depth_stencil_state.raw));
            enc.setViewport(MTLViewport {
                originX: 0.0,
                originY: 0.0,
                width: depth_target.width as f64,
                height: depth_target.height as f64,
                znear: 0.0,
                zfar: 1.0,
            });
        }

        for draw in draws {
            if draw.vertex_count == 0 || draw.instance_count == 0 {
                continue;
            }
            unsafe {
                enc.setRenderPipelineState(&draw.pipeline.state);
            }
            apply_bindings_draw_both_stages(&enc, draw.pipeline, draw.bindings);
            unsafe {
                enc.drawPrimitives_vertexStart_vertexCount_instanceCount(
                    MTLPrimitiveType::Triangle,
                    0,
                    draw.vertex_count as usize,
                    draw.instance_count as usize,
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

        let enc = self.make_render_encoder(&desc, label);
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

        let enc = self.make_render_encoder(&desc, label);
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

        let (buffer_sizes, buffer_sizes_len) = collect_buffer_sizes(&pipeline.slot_map, bindings);

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
                            // setBuffer replaces any prior setBytes inline
                            // data at this slot. Invalidate the bytes cache
                            // so a later setBytes with the same (ptr, len)
                            // is forced to re-execute. Same bug class as
                            // the compute path's Bytes-clears-buffers fix.
                            self.render_cache.bytes[idx] = (std::ptr::null(), 0);
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
                            // setBytes replaces any prior setBuffer at this
                            // slot — invalidate the buffer cache so a later
                            // setBuffer with the same (id, offset) re-fires.
                            self.render_cache.buffers[idx] = (std::ptr::null(), 0);
                        }
                    }
                }
            }
        }

        if pipeline.needs_sizes_buffer {
            bind_sizes_buffer_render(
                &enc,
                pipeline,
                &buffer_sizes,
                buffer_sizes_len,
                RenderStages::Both,
            );
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
            let enc = self.make_render_encoder(&desc, "clear_texture");
            enc.endEncoding();
        }
    }

    /// Fill a buffer with zeros via blit encoder.
    pub fn clear_buffer(&mut self, buffer: &GpuBuffer) {
        self.end_current();
        let enc = self.make_blit_encoder("clear_buffer");
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
        let enc = self.make_blit_encoder("generate_mipmaps");
        unsafe { enc.generateMipmapsForTexture(&texture.raw) };
        enc.endEncoding();
    }

    /// Copy texture to texture via blit encoder.
    ///
    /// Validates dims and pixel format before encoding. Metal's blit
    /// encoder requires matching pixel formats and the copy region to
    /// fit within source bounds; in debug builds Metal's validation
    /// layer catches this loudly, but in release the result is
    /// undefined (silent corruption, no-op, or rare GPU fault). Sims
    /// that copy stale/zero into a feedback state texture freeze
    /// visibly instead of erroring — surfaced as days of "why is the
    /// preset not animating" before the dim/format mismatch is found.
    /// Asserting here turns every such bug into a one-frame panic with
    /// the actual offending sizes / formats, at the call site that
    /// introduced the mismatch.
    pub fn copy_texture_to_texture(
        &mut self,
        src: &GpuTexture,
        dst: &GpuTexture,
        width: u32,
        height: u32,
        depth: u32,
    ) {
        assert_eq!(
            src.format, dst.format,
            "copy_texture_to_texture: pixel format mismatch — \
             src {:?} ({}×{}) → dst {:?} ({}×{}), copy region {}×{}×{}. \
             Metal blit requires matching formats. If you need a cross-\
             format copy, use a compute-shader copy path instead.",
            src.format, src.width, src.height,
            dst.format, dst.width, dst.height,
            width, height, depth,
        );
        // Same-size guard. A Metal blit copies from origin (0,0) with NO
        // scaling, so a size mismatch silently copies the top-left
        // region — which reads as a crop everywhere a downscale was
        // intended. That is the cropped-DNN-analysis bug class (depth /
        // flow / person estimating on the top-left ~9% of a 4K frame).
        // Make it unwriteable: differently-sized textures must go through
        // a sampling resize (manifold-renderer's GpuEncoder::resize_sample),
        // never this blit. (Every current caller is a same-size full copy
        // — ping-pong, feedback capture, passthrough, LED tap.)
        assert!(
            src.width == dst.width && src.height == dst.height,
            "copy_texture_to_texture is a same-size blit (origin 0, no \
             scaling) — src {}×{} != dst {}×{}. To change resolution, \
             sample: use GpuEncoder::resize_sample. A size-mismatched blit \
             would silently crop the top-left corner.",
            src.width, src.height, dst.width, dst.height,
        );
        assert!(
            width <= src.width && height <= src.height,
            "copy_texture_to_texture: copy region exceeds source bounds — \
             src {}×{} ({:?}), copy region {}×{}×{}. Source extent out of bounds.",
            src.width, src.height, src.format, width, height, depth,
        );
        assert!(
            width <= dst.width && height <= dst.height,
            "copy_texture_to_texture: copy region exceeds destination bounds — \
             dst {}×{} ({:?}), copy region {}×{}×{}. Destination extent out of bounds.",
            dst.width, dst.height, dst.format, width, height, depth,
        );
        self.end_current();
        let enc = self.make_blit_encoder("copy_texture_to_texture");
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

    /// Copy `size` bytes from `src` buffer to `dst` buffer via blit
    /// encoder. Used by `ArrayFeedback` to snapshot a graph wire's
    /// buffer contents into a state-store-held persistent buffer (and
    /// vice versa, for the per-frame swap).
    ///
    /// Asserts both buffers are at least `size` bytes. Metal's blit
    /// encoder silently corrupts on out-of-bounds copies in release
    /// builds; loud asserts here turn a future undersized-buffer bug
    /// into a one-frame panic at the offending call site.
    pub fn copy_buffer_to_buffer(&mut self, src: &GpuBuffer, dst: &GpuBuffer, size: u64) {
        assert!(
            size <= src.size,
            "copy_buffer_to_buffer: copy size {size} exceeds source buffer ({} bytes)",
            src.size,
        );
        assert!(
            size <= dst.size,
            "copy_buffer_to_buffer: copy size {size} exceeds destination buffer ({} bytes)",
            dst.size,
        );
        self.end_current();
        let enc = self.make_blit_encoder("copy_buffer_to_buffer");
        unsafe {
            enc.copyFromBuffer_sourceOffset_toBuffer_destinationOffset_size(
                &src.raw,
                0,
                &dst.raw,
                0,
                size as usize,
            );
        }
        enc.endEncoding();
    }

    /// Copy texture to buffer via blit encoder (for readback).
    ///
    /// Asserts the copy region fits in source and the destination
    /// buffer has at least `bytes_per_row * height` bytes. Same loud-
    /// failure rationale as [`Self::copy_texture_to_texture`].
    pub fn copy_texture_to_buffer(
        &mut self,
        src: &GpuTexture,
        dst: &GpuBuffer,
        width: u32,
        height: u32,
        bytes_per_row: u32,
    ) {
        assert!(
            width <= src.width && height <= src.height,
            "copy_texture_to_buffer: copy region exceeds source bounds — \
             src {}×{} ({:?}), copy region {}×{}. Source extent out of bounds.",
            src.width, src.height, src.format, width, height,
        );
        let required = u64::from(bytes_per_row) * u64::from(height);
        assert!(
            required <= dst.size,
            "copy_texture_to_buffer: destination buffer too small — \
             needed {required} bytes (bytes_per_row {bytes_per_row} × height {height}), \
             dst buffer is {} bytes",
            dst.size,
        );
        self.end_current();
        let enc = self.make_blit_encoder("copy_texture_to_buffer");
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

    /// Copy a full 3D texture volume (`width × height × depth`) to a buffer.
    /// Layout is z-slice-major: each slice is `bytes_per_row × height` bytes,
    /// slices laid out consecutively. Used by the freeze codegen's volume parity
    /// tests, where `copy_texture_to_buffer` (which copies a single slice) isn't
    /// enough.
    pub fn copy_texture_3d_to_buffer(
        &mut self,
        src: &GpuTexture,
        dst: &GpuBuffer,
        width: u32,
        height: u32,
        depth: u32,
        bytes_per_row: u32,
    ) {
        assert!(
            width <= src.width && height <= src.height && depth <= src.depth,
            "copy_texture_3d_to_buffer: copy region exceeds source bounds — \
             src {}×{}×{} ({:?}), copy region {}×{}×{}.",
            src.width, src.height, src.depth, src.format, width, height, depth,
        );
        let bytes_per_image = u64::from(bytes_per_row) * u64::from(height);
        let required = bytes_per_image * u64::from(depth);
        assert!(
            required <= dst.size,
            "copy_texture_3d_to_buffer: destination buffer too small — \
             needed {required} bytes, dst buffer is {} bytes",
            dst.size,
        );
        self.end_current();
        let enc = self.make_blit_encoder("copy_texture_3d_to_buffer");
        unsafe {
            enc.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toBuffer_destinationOffset_destinationBytesPerRow_destinationBytesPerImage_options(
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
                bytes_per_row as usize,
                bytes_per_image as usize,
                MTLBlitOption::empty(),
            );
        }
        enc.endEncoding();
    }

    /// Insert a pipeline barrier between dispatches.
    ///
    /// On Metal this is a **no-op** — Metal serializes work within a single
    /// command queue automatically, so a read-after-write between two
    /// dispatches on the same queue is already ordered. The API exists for
    /// cross-platform compatibility.
    ///
    /// On a future Vulkan backend this would emit `vkCmdPipelineBarrier2`
    /// with the appropriate stage / access masks derived from the resource
    /// lists. Vulkan does NOT serialize automatically and requires explicit
    /// barriers between dependent work.
    ///
    /// `reads` and `writes` are the resources the *next* dispatch reads from
    /// and writes to, respectively. The barrier ensures all prior writes to
    /// these resources are visible.
    ///
    /// Callers (e.g. the node-graph executor) should compute `reads` and
    /// `writes` from the plan's resource lifetime analysis. Calling
    /// `pipeline_barrier` between every step is correct (over-conservative)
    /// on Vulkan; the optimal pattern is to only insert barriers where
    /// read-after-write hazards exist between consecutive steps.
    #[allow(unused_variables)]
    pub fn pipeline_barrier(&mut self, reads: &[&GpuTexture], writes: &[&GpuTexture]) {
        // Metal: no-op. Intra-queue ordering is automatic.
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
            self.cmd_buf
                .encodeSignalEvent_value(ProtocolObject::from_ref(event.raw()), value);
        }
    }

    /// Signal a shared event with a specific value (does NOT auto-increment).
    pub fn signal_event_value(&mut self, event: &GpuEvent, value: u64) {
        self.end_current();
        unsafe {
            self.cmd_buf
                .encodeSignalEvent_value(ProtocolObject::from_ref(event.raw()), value);
        }
    }

    /// Wait for a shared event to reach a specific value before executing
    /// subsequent GPU work on this command buffer.
    pub fn wait_event(&mut self, event: &GpuEvent, value: u64) {
        self.end_current();
        unsafe {
            self.cmd_buf
                .encodeWaitForEvent_value(ProtocolObject::from_ref(event.raw()), value);
        }
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

    /// Register a completed handler that reports this buffer's **true GPU
    /// execution time** (`GPUEndTime - GPUStartTime`, seconds) once the GPU
    /// finishes. Non-blocking: the closure runs on a background thread at
    /// completion. For profilers — feed the value into an atomic/channel, do
    /// not block. Reports `0.0` if the driver has no timestamps for the buffer.
    pub fn add_gpu_time_handler<F: Fn(f64) + Send + 'static>(&self, callback: F) {
        use block2::RcBlock;
        let block = RcBlock::new(move |buf: NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
            let cb = unsafe { buf.as_ref() };
            let start = unsafe { cb.GPUStartTime() };
            let end = unsafe { cb.GPUEndTime() };
            callback((end - start).max(0.0));
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
                    label,
                    code,
                    desc,
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

    /// Commit and block until the GPU has fully completed the work.
    /// Required for synchronous texture readback — a copy to a shared buffer
    /// is only readable from the CPU once the GPU reports completion.
    ///
    /// After the wait, verifies the buffer actually reached `Completed`.
    /// A GPU fault (page fault, timeout, invalid resource) leaves the
    /// destination buffer holding garbage the caller would otherwise read
    /// as valid pixels — the root of the parity/freeze-proof test flakes
    /// (BUG-013). See [`Self::verify_completed`] for the dev-vs-release split.
    pub fn commit_and_wait_completed(mut self) {
        self.end_current();
        self.cmd_buf.commit();
        unsafe { self.cmd_buf.waitUntilCompleted() };
        self.verify_completed("commit_and_wait_completed");
    }

    /// Assert the command buffer reached `Completed` after a blocking wait.
    /// Dev/test builds panic so a GPU error surfaces as a loud, localized
    /// failure instead of silent garbage in a readback; release builds (the
    /// live show) log and carry on rather than crash mid-set.
    fn verify_completed(&self, ctx: &str) {
        use objc2_metal::MTLCommandBufferStatus;

        let status = unsafe { self.cmd_buf.status() };
        if status == MTLCommandBufferStatus::Completed {
            return;
        }
        let (code, desc) = match unsafe { self.cmd_buf.error() } {
            None => (-1i64, String::from("(no error object)")),
            Some(err) => (err.code() as i64, err.localizedDescription().to_string()),
        };
        let msg = format!(
            "[GPU] {ctx}: command buffer did not reach Completed (status={}, code={}): {}",
            status.0, code, desc,
        );
        if cfg!(debug_assertions) {
            panic!("{msg}");
        } else {
            log::error!("{msg}");
        }
    }

    /// Commit, block until completion, and return the **true GPU execution
    /// time** of this command buffer in seconds (`GPUEndTime - GPUStartTime`).
    ///
    /// This is actual GPU-side time as reported by the driver, NOT CPU
    /// wall-clock around the submit — the correct source for profiling
    /// (wall-clock includes encode + scheduling + wait latency). Returns
    /// `0.0` if the driver reports no GPU timestamps for this buffer
    /// (degenerate / no-GPU-work case). Use for benches/profilers only.
    pub fn commit_and_wait_completed_timed(mut self) -> f64 {
        self.end_current();
        self.cmd_buf.commit();
        unsafe {
            self.cmd_buf.waitUntilCompleted();
            let start = self.cmd_buf.GPUStartTime();
            let end = self.cmd_buf.GPUEndTime();
            (end - start).max(0.0)
        }
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
    let (buffer_sizes, buffer_sizes_len) = collect_buffer_sizes(&pipeline.slot_map, bindings);

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

    if pipeline.needs_sizes_buffer {
        bind_sizes_buffer_render(
            enc,
            pipeline,
            &buffer_sizes,
            buffer_sizes_len,
            RenderStages::Fragment,
        );
    }
}

/// Apply bindings on both vertex and fragment stages (used for draw_instanced et al.).
fn apply_bindings_draw_both_stages(
    enc: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    pipeline: &GpuRenderPipeline,
    bindings: &[GpuBinding],
) {
    let (buffer_sizes, buffer_sizes_len) = collect_buffer_sizes(&pipeline.slot_map, bindings);

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

    if pipeline.needs_sizes_buffer {
        bind_sizes_buffer_render(
            enc,
            pipeline,
            &buffer_sizes,
            buffer_sizes_len,
            RenderStages::Both,
        );
    }
}

/// Which stage(s) receive the naga sizes buffer. Mirrors the stage set that
/// the corresponding `apply_bindings_draw_*` helper binds resources onto —
/// fragment-only for the fullscreen path (trivial VS has no bindings),
/// vertex+fragment for everything else.
#[derive(Copy, Clone)]
enum RenderStages {
    Fragment,
    Both,
}

/// Walk a binding list and build the naga "sizes buffer" — a `u32` array
/// indexed by Metal buffer slot where entry `i` holds the byte-size of the
/// buffer bound at slot `i`. SPIRV-Cross-generated MSL reads this to resolve
/// `arrayLength()` on runtime-sized storage arrays.
///
/// Returns `(sizes, len)`; `len` is one past the highest slot index that
/// received a buffer, so only the populated prefix is uploaded.
fn collect_buffer_sizes(
    slot_map: &SlotMap,
    bindings: &[GpuBinding],
) -> ([u32; MAX_BUFFER_SLOTS], usize) {
    let mut sizes = [0u32; MAX_BUFFER_SLOTS];
    let mut len: usize = 0;
    for binding in bindings {
        if let GpuBinding::Buffer {
            binding: b, buffer, ..
        } = binding
        {
            let Some(slot) = slot_map.get(*b) else {
                continue;
            };
            let idx = slot.metal_index as usize;
            if idx < MAX_BUFFER_SLOTS {
                sizes[idx] = buffer.size as u32;
                if idx >= len {
                    len = idx + 1;
                }
            }
        }
    }
    (sizes, len)
}

/// Upload the sizes buffer as inline bytes to one or both render stages.
/// Without this the shader's `arrayLength()` reads an unbound buffer slot —
/// on Apple Silicon that typically returns 0, which silently collapses any
/// `n < 2` early-out branch. See `spectrum_line.wgsl`'s `weighting_db()`
/// for the case that surfaced this bug (weighted curves went flat once the
/// LUT moved off the per-pixel biquad onto a runtime-sized array).
fn bind_sizes_buffer_render(
    enc: &ProtocolObject<dyn MTLRenderCommandEncoder>,
    pipeline: &GpuRenderPipeline,
    sizes: &[u32; MAX_BUFFER_SLOTS],
    len: usize,
    stages: RenderStages,
) {
    let slot_idx = pipeline
        .slot_map
        .get(SIZES_BUFFER_BINDING)
        .expect("sizes buffer slot missing in render pipeline despite needs_sizes_buffer")
        .metal_index as usize;
    let ptr = NonNull::new(sizes.as_ptr() as *mut c_void).unwrap();
    let byte_len = len * 4;
    unsafe {
        match stages {
            RenderStages::Fragment => {
                enc.setFragmentBytes_length_atIndex(ptr, byte_len, slot_idx);
            }
            RenderStages::Both => {
                enc.setVertexBytes_length_atIndex(ptr, byte_len, slot_idx);
                enc.setFragmentBytes_length_atIndex(ptr, byte_len, slot_idx);
            }
        }
    }
}
