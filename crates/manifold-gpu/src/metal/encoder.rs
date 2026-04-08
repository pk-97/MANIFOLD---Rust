//! GpuEncoder — per-frame GPU command encoder wrapping a retained Metal command buffer.

use super::*;
use crate::types::*;

/// Encoder state — tracks the current active Metal encoder.
#[allow(dead_code)]
pub(crate) enum EncoderState {
    None,
    /// Active compute command encoder.
    Compute(*const metal::ComputeCommandEncoderRef),
    /// Active render command encoder.
    Render(*const metal::RenderCommandEncoderRef),
    /// Active blit command encoder.
    Blit(*const metal::BlitCommandEncoderRef),
}

/// Cached compute bind state — skips redundant Metal API calls when the same
/// resource is already bound at a slot from a previous dispatch.
/// Only valid while the compute encoder stays alive across dispatches.
const CACHE_SLOTS: usize = 16;

pub(super) struct ComputeBindCache {
    /// Raw ObjC pointers for currently bound textures, indexed by Metal texture slot.
    textures: [*const std::ffi::c_void; CACHE_SLOTS],
    /// Raw ObjC pointers for currently bound samplers, indexed by Metal sampler slot.
    samplers: [*const std::ffi::c_void; CACHE_SLOTS],
    /// (Raw ObjC pointer, offset) for currently bound buffers, indexed by Metal buffer slot.
    buffers: [(*const std::ffi::c_void, u64); CACHE_SLOTS],
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

/// Cached render bind state for `draw_in_render_pass` — skips redundant
/// bindings across consecutive draws within the same render pass.
pub(super) struct RenderBindCache {
    frag_textures: [*const std::ffi::c_void; CACHE_SLOTS],
    frag_samplers: [*const std::ffi::c_void; CACHE_SLOTS],
    /// Buffers: (ObjC pointer, offset) per slot — same identity = skip both stages.
    buffers: [(*const std::ffi::c_void, u64); CACHE_SLOTS],
    /// Vertex buffer at index 30: ObjC pointer. When the same buffer is
    /// re-bound at a different offset, use setVertexBufferOffset instead.
    vertex_buf_30: *const std::ffi::c_void,
    /// Bytes: (data pointer, length) per slot — same slice = skip both stages.
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
///
/// Automatically manages compute/render/blit encoder transitions.
/// Compute encoders are kept alive across dispatches for efficiency.
/// Render/blit encoders are ended after each pass.
pub struct GpuEncoder {
    /// Retained MTLCommandBuffer. Released on drop.
    pub(crate) cmd_buf_ptr: *mut std::ffi::c_void,
    pub(crate) state: EncoderState,
    /// Bind cache for the active compute encoder — eliminates redundant
    /// set_texture/set_sampler/set_buffer calls across consecutive dispatches.
    pub(super) compute_cache: ComputeBindCache,
    /// Bind cache for multi-draw render passes (`draw_in_render_pass`) —
    /// eliminates redundant fragment texture/sampler bindings across draws.
    pub(super) render_cache: RenderBindCache,
    /// Pre-compiled compute clear pipelines per texture format.
    pub(super) clear_pipelines: *const super::device::ClearPipelines,
}

unsafe impl Send for GpuEncoder {}

/// Extract the raw ObjC pointer from a Metal texture for bind cache comparison.
#[inline]
fn texture_identity(tex: &metal::Texture) -> *const std::ffi::c_void {
    &**tex as *const metal::TextureRef as *const std::ffi::c_void
}

/// Extract the raw ObjC pointer from a Metal sampler for bind cache comparison.
#[inline]
fn sampler_identity(s: &metal::SamplerState) -> *const std::ffi::c_void {
    &**s as *const metal::SamplerStateRef as *const std::ffi::c_void
}

/// Extract the raw ObjC pointer from a Metal buffer for bind cache comparison.
#[inline]
fn buffer_identity(buf: &metal::Buffer) -> *const std::ffi::c_void {
    &**buf as *const metal::BufferRef as *const std::ffi::c_void
}

impl GpuEncoder {
    pub(super) fn cmd_buf(&self) -> &metal::CommandBufferRef {
        unsafe { &*(self.cmd_buf_ptr as *const metal::CommandBufferRef) }
    }

    /// Get the raw command buffer for direct encoding (MPS kernels, MetalFX).
    /// Ends any active encoder first to avoid encoding conflicts.
    pub fn raw_cmd_buf(&mut self) -> &metal::CommandBufferRef {
        self.end_current();
        self.cmd_buf()
    }

    /// Ensure a compute encoder is active. Returns a raw pointer to it.
    fn ensure_compute(&mut self) -> *const metal::ComputeCommandEncoderRef {
        if let EncoderState::Compute(ptr) = self.state {
            return ptr;
        }
        self.end_current();
        let enc = self.cmd_buf().new_compute_command_encoder();
        let ptr = enc as *const metal::ComputeCommandEncoderRef;
        // Retain the encoder so it survives autorelease pool drains.
        // The autoreleased reference from new_compute_command_encoder() could
        // be freed by an outer pool drain in release builds.
        unsafe {
            objc_retain(ptr as *mut std::ffi::c_void);
        }
        self.state = EncoderState::Compute(ptr);
        ptr
    }

    /// End the current encoder (if any).
    pub(super) fn end_current(&mut self) {
        match self.state {
            EncoderState::None => {}
            EncoderState::Compute(ptr) => {
                unsafe { &*ptr }.end_encoding();
                unsafe {
                    objc_release(ptr as *mut std::ffi::c_void);
                }
                self.compute_cache.clear();
            }
            EncoderState::Render(ptr) => {
                unsafe { &*ptr }.end_encoding();
                // Render encoders are not retained (created+ended in same scope)
            }
            EncoderState::Blit(ptr) => {
                unsafe { &*ptr }.end_encoding();
                // Blit encoders are not retained (created+ended in same scope)
            }
        }
        self.state = EncoderState::None;
    }

    /// Dispatch a compute shader.
    ///
    /// Automatically manages encoder state — if a compute encoder is already
    /// active, reuses it. If a render/blit encoder is active, ends it first.
    ///
    /// `bindings` use WGSL @binding(N) indices. The pipeline's slot map
    /// translates to Metal buffer/texture/sampler argument indices.
    pub fn dispatch_compute(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        workgroups: [u32; 3],
        label: &str,
    ) {
        let enc_ptr = self.ensure_compute();
        let enc = unsafe { &*enc_ptr };
        enc.push_debug_group(label);
        enc.set_compute_pipeline_state(&pipeline.state);

        // Collect buffer sizes for the sizes buffer (runtime-sized arrays).
        // naga's MSL backend reads arrayLength() from this auxiliary buffer.
        // Fixed-size stack array — Metal argument indices are < 31 in practice.
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
                    // Skip bindings not used by this entry point. Metal ignores
                    // unused argument slots, so this is safe. Multi-entry-point
                    // shaders have per-entry slot maps that may exclude globals
                    // not referenced by the specific entry point.
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    let idx = slot.metal_index as usize;
                    let id = buffer_identity(&buffer.raw);
                    if idx >= CACHE_SLOTS || self.compute_cache.buffers[idx] != (id, *offset) {
                        enc.set_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                        if idx < CACHE_SLOTS {
                            self.compute_cache.buffers[idx] = (id, *offset);
                        }
                    }
                    // Track buffer size for sizes buffer generation.
                    // Indexed by Metal buffer argument index.
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
                        enc.set_texture(slot.metal_index as _, Some(&texture.raw));
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
                        enc.set_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                        if idx < CACHE_SLOTS {
                            self.compute_cache.samplers[idx] = id;
                        }
                    }
                }
                GpuBinding::Bytes { binding: b, data } => {
                    // Always re-bind: inline bytes change every dispatch (uniforms).
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    enc.set_bytes(
                        slot.metal_index as _,
                        data.len() as _,
                        data.as_ptr() as *const _,
                    );
                }
            }
        }

        // Bind the sizes buffer if this pipeline has runtime-sized arrays.
        if pipeline.needs_sizes_buffer {
            let slot = pipeline
                .slot_map
                .get(SIZES_BUFFER_BINDING)
                .expect("sizes buffer slot missing");
            enc.set_bytes(
                slot.metal_index as _,
                (buffer_sizes_len * 4) as _,
                buffer_sizes.as_ptr() as *const _,
            );
        }

        let wg = pipeline.workgroup_size;
        enc.dispatch_thread_groups(
            metal::MTLSize::new(workgroups[0] as _, workgroups[1] as _, workgroups[2] as _),
            metal::MTLSize::new(wg[0] as _, wg[1] as _, wg[2] as _),
        );
        enc.pop_debug_group();
    }

    /// Draw a fullscreen triangle with a render pipeline.
    ///
    /// Creates a new render encoder for each call (render targets may differ).
    /// Used by SimpleBlitHelper, DualTextureBlitHelper, etc.
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

        let desc = metal::RenderPassDescriptor::new();
        let color = desc.color_attachments().object_at(0).unwrap();
        color.set_texture(Some(&target.raw));
        color.set_load_action(if clear {
            metal::MTLLoadAction::Clear
        } else {
            metal::MTLLoadAction::Load
        });
        color.set_store_action(if store {
            metal::MTLStoreAction::Store
        } else {
            metal::MTLStoreAction::DontCare
        });
        color.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 0.0));

        let enc = self.cmd_buf().new_render_command_encoder(desc);
        enc.push_debug_group(label);
        enc.set_render_pipeline_state(&pipeline.state);

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
                    enc.set_fragment_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                }
                GpuBinding::Texture {
                    binding: b,
                    texture,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    // Bind to both stages — vertex shaders may sample textures
                    // (e.g. displacement maps). Metal ignores unused bindings.
                    enc.set_vertex_texture(slot.metal_index as _, Some(&texture.raw));
                    enc.set_fragment_texture(slot.metal_index as _, Some(&texture.raw));
                }
                GpuBinding::Sampler {
                    binding: b,
                    sampler,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    enc.set_vertex_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                    enc.set_fragment_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                }
                GpuBinding::Bytes { binding: b, data } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    enc.set_fragment_bytes(
                        slot.metal_index as _,
                        data.len() as _,
                        data.as_ptr() as *const _,
                    );
                }
            }
        }

        enc.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
        enc.pop_debug_group();
        enc.end_encoding();
        // State goes back to None (render encoder consumed).
    }

    /// Draw a fullscreen triangle with viewport positioning.
    ///
    /// Like `draw_fullscreen()` but sets a viewport for sub-region rendering
    /// (e.g. aspect-fit blit into a panel). Load action preserves existing content.
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

        let desc = metal::RenderPassDescriptor::new();
        let color = desc.color_attachments().object_at(0).unwrap();
        color.set_texture(Some(&target.raw));
        color.set_load_action(match load_action {
            crate::GpuLoadAction::Clear => metal::MTLLoadAction::Clear,
            crate::GpuLoadAction::Load => metal::MTLLoadAction::Load,
            crate::GpuLoadAction::DontCare => metal::MTLLoadAction::DontCare,
        });
        color.set_store_action(metal::MTLStoreAction::Store);
        color.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 0.0));

        let enc = self.cmd_buf().new_render_command_encoder(desc);
        enc.push_debug_group(label);
        enc.set_render_pipeline_state(&pipeline.state);

        let (x, y, w, h) = viewport;
        enc.set_viewport(metal::MTLViewport {
            originX: x as f64,
            originY: y as f64,
            width: w as f64,
            height: h as f64,
            znear: 0.0,
            zfar: 1.0,
        });

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
                    enc.set_fragment_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                }
                GpuBinding::Texture {
                    binding: b,
                    texture,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    // Bind to both stages — vertex shaders may sample textures
                    // (e.g. displacement maps). Metal ignores unused bindings.
                    enc.set_vertex_texture(slot.metal_index as _, Some(&texture.raw));
                    enc.set_fragment_texture(slot.metal_index as _, Some(&texture.raw));
                }
                GpuBinding::Sampler {
                    binding: b,
                    sampler,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    enc.set_vertex_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                    enc.set_fragment_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                }
                GpuBinding::Bytes { binding: b, data } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    enc.set_fragment_bytes(
                        slot.metal_index as _,
                        data.len() as _,
                        data.as_ptr() as *const _,
                    );
                }
            }
        }

        enc.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
        enc.pop_debug_group();
        enc.end_encoding();
    }

    /// Draw instanced geometry with a render pipeline.
    ///
    /// Buffer/Bytes bindings are set on BOTH vertex and fragment stages.
    /// Texture/Sampler bindings are fragment-only (no current vertex shader
    /// samples textures — avoids redundant vertex-stage bindings).
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

        let desc = metal::RenderPassDescriptor::new();
        let color = desc.color_attachments().object_at(0).unwrap();
        color.set_texture(Some(&target.raw));
        color.set_load_action(match load_action {
            crate::GpuLoadAction::Clear => metal::MTLLoadAction::Clear,
            crate::GpuLoadAction::Load => metal::MTLLoadAction::Load,
            crate::GpuLoadAction::DontCare => metal::MTLLoadAction::DontCare,
        });
        color.set_store_action(metal::MTLStoreAction::Store);
        color.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 0.0));

        let enc = self.cmd_buf().new_render_command_encoder(desc);
        enc.push_debug_group(label);
        enc.set_render_pipeline_state(&pipeline.state);

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
                    enc.set_vertex_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                    enc.set_fragment_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                }
                GpuBinding::Texture {
                    binding: b,
                    texture,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    // Bind to both stages — vertex shaders may sample textures
                    // (e.g. displacement maps). Metal ignores unused bindings.
                    enc.set_vertex_texture(slot.metal_index as _, Some(&texture.raw));
                    enc.set_fragment_texture(slot.metal_index as _, Some(&texture.raw));
                }
                GpuBinding::Sampler {
                    binding: b,
                    sampler,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    enc.set_vertex_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                    enc.set_fragment_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                }
                GpuBinding::Bytes { binding: b, data } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    enc.set_vertex_bytes(
                        slot.metal_index as _,
                        data.len() as _,
                        data.as_ptr() as *const _,
                    );
                    enc.set_fragment_bytes(
                        slot.metal_index as _,
                        data.len() as _,
                        data.as_ptr() as *const _,
                    );
                }
            }
        }

        if instance_count > 0 {
            enc.draw_primitives_instanced(
                metal::MTLPrimitiveType::Triangle,
                0,
                vertex_count as u64,
                instance_count as u64,
            );
        }
        enc.pop_debug_group();
        enc.end_encoding();
    }

    /// Draw instanced with MSAA: render to a multisample target, resolve to
    /// a single-sample texture. The MSAA target should be memoryless (tile
    /// memory only on Apple Silicon — zero VRAM cost). The resolved result
    /// is written to `resolve_target`.
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

        let desc = metal::RenderPassDescriptor::new();
        let color = desc.color_attachments().object_at(0).unwrap();
        color.set_texture(Some(&msaa_target.raw));
        color.set_resolve_texture(Some(&resolve_target.raw));
        color.set_load_action(match load_action {
            crate::GpuLoadAction::Clear => metal::MTLLoadAction::Clear,
            crate::GpuLoadAction::Load => metal::MTLLoadAction::Load,
            crate::GpuLoadAction::DontCare => metal::MTLLoadAction::DontCare,
        });
        color.set_store_action(metal::MTLStoreAction::MultisampleResolve);
        color.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 0.0));

        let enc = self.cmd_buf().new_render_command_encoder(desc);
        enc.push_debug_group(label);
        enc.set_render_pipeline_state(&pipeline.state);

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
                    enc.set_vertex_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                    enc.set_fragment_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                }
                GpuBinding::Texture {
                    binding: b,
                    texture,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    // Bind to both stages — vertex shaders may sample textures
                    // (e.g. displacement maps). Metal ignores unused bindings.
                    enc.set_vertex_texture(slot.metal_index as _, Some(&texture.raw));
                    enc.set_fragment_texture(slot.metal_index as _, Some(&texture.raw));
                }
                GpuBinding::Sampler {
                    binding: b,
                    sampler,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    enc.set_vertex_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                    enc.set_fragment_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                }
                GpuBinding::Bytes { binding: b, data } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    enc.set_vertex_bytes(
                        slot.metal_index as _,
                        data.len() as _,
                        data.as_ptr() as *const _,
                    );
                    enc.set_fragment_bytes(
                        slot.metal_index as _,
                        data.len() as _,
                        data.as_ptr() as *const _,
                    );
                }
            }
        }

        if instance_count > 0 {
            enc.draw_primitives_instanced(
                metal::MTLPrimitiveType::Triangle,
                0,
                vertex_count as u64,
                instance_count as u64,
            );
        }
        enc.pop_debug_group();
        enc.end_encoding();
    }

    /// Draw instanced geometry with depth testing.
    ///
    /// Renders to `target` (color) and `depth_target` (Depth32Float) with the given
    /// depth-stencil state. Buffer/Bytes on both stages; Texture/Sampler fragment-only.
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

        let desc = metal::RenderPassDescriptor::new();

        // Color attachment
        let color = desc.color_attachments().object_at(0).unwrap();
        color.set_texture(Some(&target.raw));
        color.set_load_action(match load_action {
            crate::GpuLoadAction::Clear => metal::MTLLoadAction::Clear,
            crate::GpuLoadAction::Load => metal::MTLLoadAction::Load,
            crate::GpuLoadAction::DontCare => metal::MTLLoadAction::DontCare,
        });
        color.set_store_action(metal::MTLStoreAction::Store);
        color.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 0.0));

        // Depth attachment
        let depth = desc.depth_attachment().unwrap();
        depth.set_texture(Some(&depth_target.raw));
        depth.set_load_action(match load_action {
            crate::GpuLoadAction::Clear => metal::MTLLoadAction::Clear,
            crate::GpuLoadAction::Load => metal::MTLLoadAction::Load,
            crate::GpuLoadAction::DontCare => metal::MTLLoadAction::DontCare,
        });
        depth.set_store_action(metal::MTLStoreAction::Store);
        depth.set_clear_depth(1.0);

        let enc = self.cmd_buf().new_render_command_encoder(desc);
        enc.push_debug_group(label);
        enc.set_render_pipeline_state(&pipeline.state);
        enc.set_depth_stencil_state(&depth_stencil_state.raw);

        // Set viewport with depth range
        enc.set_viewport(metal::MTLViewport {
            originX: 0.0,
            originY: 0.0,
            width: target.width as f64,
            height: target.height as f64,
            znear: 0.0,
            zfar: 1.0,
        });

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
                    enc.set_vertex_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                    enc.set_fragment_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                }
                GpuBinding::Texture {
                    binding: b,
                    texture,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    // Bind to both stages — vertex shaders may sample textures
                    // (e.g. displacement maps). Metal ignores unused bindings.
                    enc.set_vertex_texture(slot.metal_index as _, Some(&texture.raw));
                    enc.set_fragment_texture(slot.metal_index as _, Some(&texture.raw));
                }
                GpuBinding::Sampler {
                    binding: b,
                    sampler,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    enc.set_vertex_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                    enc.set_fragment_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                }
                GpuBinding::Bytes { binding: b, data } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    enc.set_vertex_bytes(
                        slot.metal_index as _,
                        data.len() as _,
                        data.as_ptr() as *const _,
                    );
                    enc.set_fragment_bytes(
                        slot.metal_index as _,
                        data.len() as _,
                        data.as_ptr() as *const _,
                    );
                }
            }
        }

        if instance_count > 0 {
            enc.draw_primitives_instanced(
                metal::MTLPrimitiveType::Triangle,
                0,
                vertex_count as u64,
                instance_count as u64,
            );
        }
        enc.pop_debug_group();
        enc.end_encoding();
    }

    /// Draw indexed geometry with a render pipeline and vertex/index buffers.
    ///
    /// Buffer/Bytes on both stages; Texture/Sampler fragment-only.
    /// Vertex buffer bound at index 30 (matching the vertex descriptor buffer index).
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

        let desc = metal::RenderPassDescriptor::new();
        let color = desc.color_attachments().object_at(0).unwrap();
        color.set_texture(Some(&target.raw));
        color.set_load_action(match load_action {
            crate::GpuLoadAction::Clear => metal::MTLLoadAction::Clear,
            crate::GpuLoadAction::Load => metal::MTLLoadAction::Load,
            crate::GpuLoadAction::DontCare => metal::MTLLoadAction::DontCare,
        });
        color.set_store_action(metal::MTLStoreAction::Store);
        color.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 0.0));

        let enc = self.cmd_buf().new_render_command_encoder(desc);
        enc.push_debug_group(label);
        enc.set_render_pipeline_state(&pipeline.state);

        // Set viewport if provided, otherwise full texture dimensions.
        if let Some((x, y, w, h)) = viewport {
            enc.set_viewport(metal::MTLViewport {
                originX: x as f64,
                originY: y as f64,
                width: w as f64,
                height: h as f64,
                znear: 0.0,
                zfar: 1.0,
            });
        } else {
            enc.set_viewport(metal::MTLViewport {
                originX: 0.0,
                originY: 0.0,
                width: target.width as f64,
                height: target.height as f64,
                znear: 0.0,
                zfar: 1.0,
            });
        }

        // Bind vertex buffer at index 30 (same as vertex descriptor buffer index).
        const VERTEX_BUFFER_INDEX: u64 = 30;
        enc.set_vertex_buffer(
            VERTEX_BUFFER_INDEX,
            Some(&vertex_buffer.raw),
            vertex_offset as _,
        );

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
                    enc.set_vertex_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                    enc.set_fragment_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                }
                GpuBinding::Texture {
                    binding: b,
                    texture,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    // Bind to both stages — vertex shaders may sample textures
                    // (e.g. displacement maps). Metal ignores unused bindings.
                    enc.set_vertex_texture(slot.metal_index as _, Some(&texture.raw));
                    enc.set_fragment_texture(slot.metal_index as _, Some(&texture.raw));
                }
                GpuBinding::Sampler {
                    binding: b,
                    sampler,
                } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    enc.set_vertex_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                    enc.set_fragment_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                }
                GpuBinding::Bytes { binding: b, data } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else {
                        continue;
                    };
                    enc.set_vertex_bytes(
                        slot.metal_index as _,
                        data.len() as _,
                        data.as_ptr() as *const _,
                    );
                    enc.set_fragment_bytes(
                        slot.metal_index as _,
                        data.len() as _,
                        data.as_ptr() as *const _,
                    );
                }
            }
        }

        enc.draw_indexed_primitives(
            metal::MTLPrimitiveType::Triangle,
            index_count as u64,
            metal::MTLIndexType::UInt32,
            &index_buffer.raw,
            0,
        );
        enc.pop_debug_group();
        enc.end_encoding();
    }

    /// Begin a render pass that stays alive across multiple draw calls.
    /// Use `draw_in_render_pass` for each draw, then `end_render_pass` when done.
    /// This avoids creating a new render encoder per draw when multiple draws
    /// target the same texture (e.g. layer bitmap quads).
    pub fn begin_render_pass(
        &mut self,
        target: &GpuTexture,
        load_action: crate::GpuLoadAction,
        label: &str,
    ) {
        self.end_current();
        self.render_cache.clear();

        let desc = metal::RenderPassDescriptor::new();
        let color = desc.color_attachments().object_at(0).unwrap();
        color.set_texture(Some(&target.raw));
        color.set_load_action(match load_action {
            crate::GpuLoadAction::Clear => metal::MTLLoadAction::Clear,
            crate::GpuLoadAction::Load => metal::MTLLoadAction::Load,
            crate::GpuLoadAction::DontCare => metal::MTLLoadAction::DontCare,
        });
        color.set_store_action(metal::MTLStoreAction::Store);
        color.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 0.0));

        let enc = self.cmd_buf().new_render_command_encoder(desc);
        enc.push_debug_group(label);

        let vp = metal::MTLViewport {
            originX: 0.0,
            originY: 0.0,
            width: target.width as f64,
            height: target.height as f64,
            znear: 0.0,
            zfar: 1.0,
        };
        enc.set_viewport(vp);

        // Store as active render encoder (not retained — caller must end the pass).
        self.state = EncoderState::Render(enc as *const metal::RenderCommandEncoderRef);
    }

    /// Draw indexed geometry within an active render pass (from `begin_render_pass`).
    /// Does NOT create or end the render encoder — multiple draws share one pass.
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
        let EncoderState::Render(ptr) = self.state else {
            panic!("draw_in_render_pass called without active render pass");
        };
        let enc = unsafe { &*ptr };

        enc.push_debug_group(label);
        enc.set_render_pipeline_state(&pipeline.state);

        if let Some((x, y, w, h)) = viewport {
            enc.set_viewport(metal::MTLViewport {
                originX: x as f64,
                originY: y as f64,
                width: w as f64,
                height: h as f64,
                znear: 0.0,
                zfar: 1.0,
            });
        }

        const VERTEX_BUFFER_INDEX: u64 = 30;
        let vb_id = buffer_identity(&vertex_buffer.raw);
        if self.render_cache.vertex_buf_30 == vb_id {
            // Same buffer already bound — lightweight offset-only update.
            enc.set_vertex_buffer_offset(VERTEX_BUFFER_INDEX, vertex_offset as _);
        } else {
            enc.set_vertex_buffer(
                VERTEX_BUFFER_INDEX,
                Some(&vertex_buffer.raw),
                vertex_offset as _,
            );
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
                        enc.set_vertex_buffer(
                            slot.metal_index as _,
                            Some(&buffer.raw),
                            *offset as _,
                        );
                        enc.set_fragment_buffer(
                            slot.metal_index as _,
                            Some(&buffer.raw),
                            *offset as _,
                        );
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
                        // Bind to both stages — vertex shaders may sample textures
                    // (e.g. displacement maps). Metal ignores unused bindings.
                    enc.set_vertex_texture(slot.metal_index as _, Some(&texture.raw));
                    enc.set_fragment_texture(slot.metal_index as _, Some(&texture.raw));
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
                        enc.set_vertex_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                    enc.set_fragment_sampler_state(slot.metal_index as _, Some(&sampler.raw));
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
                        enc.set_vertex_bytes(
                            slot.metal_index as _,
                            data.len() as _,
                            data.as_ptr() as *const _,
                        );
                        enc.set_fragment_bytes(
                            slot.metal_index as _,
                            data.len() as _,
                            data.as_ptr() as *const _,
                        );
                        if idx < CACHE_SLOTS {
                            self.render_cache.bytes[idx] = id;
                        }
                    }
                }
            }
        }

        enc.draw_indexed_primitives(
            metal::MTLPrimitiveType::Triangle,
            index_count as u64,
            metal::MTLIndexType::UInt32,
            &index_buffer.raw,
            index_buffer_offset,
        );
        enc.pop_debug_group();
        // Do NOT end encoding — pass stays alive for more draws.
    }

    /// Set the scissor rectangle on the active render pass.
    /// Coordinates are in physical pixels of the render target.
    pub fn set_scissor_rect(&mut self, x: u32, y: u32, w: u32, h: u32) {
        let EncoderState::Render(ptr) = self.state else {
            panic!("set_scissor_rect called without active render pass");
        };
        let enc = unsafe { &*ptr };
        enc.set_scissor_rect(metal::MTLScissorRect {
            x: x as u64,
            y: y as u64,
            width: w as u64,
            height: h as u64,
        });
    }

    /// End the active render pass (started by `begin_render_pass`).
    /// Also called implicitly by `end_current()` if the encoder transitions.
    pub fn end_render_pass(&mut self) {
        if let EncoderState::Render(ptr) = self.state {
            let enc = unsafe { &*ptr };
            enc.pop_debug_group(); // matches begin_render_pass push
            enc.end_encoding();
            self.state = EncoderState::None;
        }
    }

    /// Clear a texture to a solid color.
    /// Uses compute dispatch for formats with storage write support (avoids
    /// render encoder creation and TBDR tile overhead). Falls back to
    /// render-pass clear for formats without storage support (R16Float, etc.).
    pub fn clear_texture(&mut self, texture: &GpuTexture, r: f64, g: f64, b: f64, a: f64) {
        let pipelines = unsafe { &*self.clear_pipelines };
        let has_write = texture
            .raw
            .usage()
            .contains(metal::MTLTextureUsage::ShaderWrite);
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
            // Formats without storage write support (R16Float, R8Unorm, etc.).
            self.end_current();
            let desc = metal::RenderPassDescriptor::new();
            let color_att = desc.color_attachments().object_at(0).unwrap();
            color_att.set_texture(Some(&texture.raw));
            color_att.set_load_action(metal::MTLLoadAction::Clear);
            color_att.set_store_action(metal::MTLStoreAction::Store);
            color_att.set_clear_color(metal::MTLClearColor::new(r, g, b, a));
            let enc = self.cmd_buf().new_render_command_encoder(desc);
            enc.end_encoding();
        }
    }

    /// Fill a buffer with zeros via blit encoder.
    pub fn clear_buffer(&mut self, buffer: &GpuBuffer) {
        self.end_current();
        let enc = self.cmd_buf().new_blit_command_encoder();
        enc.fill_buffer(&buffer.raw, metal::NSRange::new(0, buffer.size), 0);
        enc.end_encoding();
    }

    /// Generate the mipmap chain for a texture using Metal's optimized
    /// blit-encoder path. The texture must have been created with
    /// `mip_levels > 1` and a usage that allows GPU read+write of all
    /// mip levels (the standard `RENDER_TARGET_FULL` set is enough).
    /// Apple Silicon implements this in hardware.
    ///
    /// Single-tap mip sampling in shaders becomes the cheapest possible
    /// wide-blur primitive: each mip level k stores the average of a
    /// 2^k × 2^k region of the source, so `textureSampleLevel(.., k)`
    /// gives a 2^(k+1)-pixel-wide box-blurred value in one fetch.
    pub fn generate_mipmaps(&mut self, texture: &GpuTexture) {
        self.end_current();
        let enc = self.cmd_buf().new_blit_command_encoder();
        enc.generate_mipmaps(&texture.raw);
        enc.end_encoding();
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
        let enc = self.cmd_buf().new_blit_command_encoder();
        enc.copy_from_texture(
            &src.raw,
            0, // source_slice
            0, // source_level
            metal::MTLOrigin { x: 0, y: 0, z: 0 },
            metal::MTLSize::new(width as _, height as _, depth as _),
            &dst.raw,
            0, // dest_slice
            0, // dest_level
            metal::MTLOrigin { x: 0, y: 0, z: 0 },
        );
        enc.end_encoding();
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
        let enc = self.cmd_buf().new_blit_command_encoder();
        let src_size = metal::MTLSize::new(width as _, height as _, 1);
        let src_origin = metal::MTLOrigin { x: 0, y: 0, z: 0 };
        enc.copy_from_texture_to_buffer(
            &src.raw,
            0, // slice
            0, // level
            src_origin,
            src_size,
            &dst.raw,
            0,                                    // destination_offset
            bytes_per_row as u64,                 // destination_bytes_per_row
            bytes_per_row as u64 * height as u64, // destination_bytes_per_image
            metal::MTLBlitOption::empty(),
        );
        enc.end_encoding();
    }

    /// Upload CPU data to a 2D texture region via blit encoder.
    /// `bytes_per_pixel` is inferred from the texture format.
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
        let region = metal::MTLRegion::new_2d(0, 0, width as _, height as _);
        texture.raw.replace_region(
            region,
            0, // mipmap level
            data.as_ptr() as *const _,
            bytes_per_row,
        );
    }

    /// Signal a shared event on the GPU timeline.
    /// The event value is incremented automatically.
    pub fn signal_event(&mut self, event: &GpuEvent) {
        let value = event.counter.get() + 1;
        event.counter.set(value);
        // Encode signal on current command buffer (after all work).
        self.end_current();
        self.cmd_buf().encode_signal_event(event.raw(), value);
    }

    /// Signal a shared event with a specific value (does NOT auto-increment).
    /// Used for per-layer completion signals in async compute.
    pub fn signal_event_value(&mut self, event: &GpuEvent, value: u64) {
        self.end_current();
        self.cmd_buf().encode_signal_event(event.raw(), value);
    }

    /// Wait for a shared event to reach a specific value before executing
    /// subsequent GPU work on this command buffer.
    /// Used by the compositor to wait for all layer generation to complete.
    pub fn wait_event(&mut self, event: &GpuEvent, value: u64) {
        self.end_current();
        self.cmd_buf().encode_wait_for_event(event.raw(), value);
    }

    /// Encode a MetalFX spatial upscale (src → dst).
    /// Ends any active encoder first. The scaler must match the texture dimensions.
    pub fn encode_metalfx_upscale(
        &mut self,
        scaler: &metalfx::MetalFxSpatialScaler,
        src: &GpuTexture,
        dst: &GpuTexture,
    ) {
        self.end_current();
        scaler.encode(self.cmd_buf(), src, dst);
    }

    /// Encode an MPS Lanczos upscale (src → dst).
    /// Automatically computes the scale transform from texture dimensions.
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
        scaler.encode(self.cmd_buf(), &src.raw, &dst.raw);
    }

    /// Register a callback to run when the GPU finishes executing this command buffer.
    /// Uses Metal's `addCompletedHandler` — fires immediately on GPU completion,
    /// no polling or next-frame delay.
    pub fn add_completed_handler<F: Fn() + Send + 'static>(&self, callback: F) {
        let block = block::ConcreteBlock::new(move |_: &metal::CommandBufferRef| {
            callback();
        });
        let block = block.copy();
        self.cmd_buf().add_completed_handler(&block);
    }

    /// Register a diagnostic completed handler that logs GPU errors with
    /// the Metal error code and description.
    pub fn add_completed_handler_with_status(&self, label: &str) {
        let label = label.to_string();
        let block =
            block::ConcreteBlock::new(move |buf: &metal::CommandBufferRef| {
                let status = buf.status();
                if status == metal::MTLCommandBufferStatus::Error {
                    // Extract NSError via ObjC runtime — not exposed by metal-rs.
                    let (code, desc) = unsafe {
                        let err: *const objc::runtime::Object =
                            objc::msg_send![buf, error];
                        if err.is_null() {
                            (-1i64, String::from("(nil)"))
                        } else {
                            let code: i64 = objc::msg_send![err, code];
                            let ns_desc: *const objc::runtime::Object =
                                objc::msg_send![err, localizedDescription];
                            let desc = if ns_desc.is_null() {
                                String::from("(no description)")
                            } else {
                                let cstr: *const std::ffi::c_char =
                                    objc::msg_send![ns_desc, UTF8String];
                                if cstr.is_null() {
                                    String::from("(UTF8 nil)")
                                } else {
                                    std::ffi::CStr::from_ptr(cstr)
                                        .to_string_lossy()
                                        .into_owned()
                                }
                            };
                            (code, desc)
                        }
                    };
                    log::error!(
                        "[GPU] Command buffer '{}' error (code={}): {}",
                        label, code, desc,
                    );
                }
            });
        let block = block.copy();
        self.cmd_buf().add_completed_handler(&block);
    }

    /// Commit the command buffer to the GPU queue.
    /// Ends any active encoder and commits. Consumes the encoder.
    pub fn commit(mut self) {
        self.end_current();
        self.cmd_buf().commit();
        // Don't release in commit — Drop handles it
    }

    /// Commit and block until the GPU has scheduled (not completed) the work.
    ///
    /// Used with `presentsWithTransaction = true` on CAMetalLayer: commit the
    /// blit work, wait until the GPU has it queued, then call
    /// `drawable.present_after_scheduled()` to sync with Core Animation.
    /// Does NOT call `presentDrawable` — the caller presents manually.
    pub fn commit_and_wait_scheduled(mut self) {
        self.end_current();
        let cb = self.cmd_buf();
        cb.commit();
        cb.wait_until_scheduled();
    }
}

impl Drop for GpuEncoder {
    fn drop(&mut self) {
        self.end_current();
        if !self.cmd_buf_ptr.is_null() {
            unsafe {
                objc_release(self.cmd_buf_ptr);
            }
        }
    }
}
