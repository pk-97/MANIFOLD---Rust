//! Native Metal backend for macOS content thread.
//!
//! Owns metal::Device, metal::CommandQueue, metal::CommandBuffer directly.
//! Zero wgpu types, zero wgpu submission tracking, zero "(wgpu internal) Signal"
//! overhead. WGSL→MSL compilation via naga, shader loading via
//! metal::Device::new_library_with_source().

use crate::types::*;

// Raw ObjC retain/release — avoids dependency on objc::msg_send! macro.
unsafe extern "C" {
    fn objc_retain(obj: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    fn objc_release(obj: *mut std::ffi::c_void);
}

// ─── Slot mapping ─────────────────────────────────────────────────────

/// Maps WGSL @binding(N) to Metal argument indices.
/// Built during pipeline creation from naga module introspection.
#[derive(Clone, Debug)]
pub struct SlotMap {
    /// Indexed by WGSL @binding(N). Each entry gives the Metal argument type and index.
    slots: Vec<Option<Slot>>,
}

#[derive(Clone, Copy, Debug)]
pub struct Slot {
    pub kind: SlotKind,
    pub metal_index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
    Buffer,
    Texture,
    Sampler,
}

impl SlotMap {
    fn new() -> Self {
        Self { slots: Vec::new() }
    }

    fn insert(&mut self, binding: u32, slot: Slot) {
        let idx = binding as usize;
        if idx >= self.slots.len() {
            self.slots.resize(idx + 1, None);
        }
        self.slots[idx] = Some(slot);
    }

    /// Look up the Metal argument index for a WGSL @binding(N).
    #[inline]
    pub fn get(&self, binding: u32) -> Option<&Slot> {
        self.slots.get(binding as usize).and_then(|s| s.as_ref())
    }
}

// ─── GpuDevice ────────────────────────────────────────────────────────

/// Native Metal device + command queue for the content thread.
/// Created once at startup. Owns the Metal device and a dedicated command queue
/// for content-thread GPU work (separate from the UI thread's wgpu queue).
pub struct GpuDevice {
    device: metal::Device,
    queue: metal::CommandQueue,
}

// Safety: metal::Device and metal::CommandQueue are thread-safe (Metal guarantee).
unsafe impl Send for GpuDevice {}
unsafe impl Sync for GpuDevice {}

impl Default for GpuDevice {
    fn default() -> Self { Self::new() }
}

impl GpuDevice {
    /// Create from the system default Metal device.
    /// Uses a dedicated command queue for content-thread work.
    pub fn new() -> Self {
        let device = metal::Device::system_default().expect("No Metal device found");
        let queue = device.new_command_queue();
        Self { device, queue }
    }

    /// Raw Metal device reference (for advanced interop).
    pub fn raw_device(&self) -> &metal::DeviceRef {
        &self.device
    }

    /// Raw Metal command queue reference (for advanced interop).
    pub fn raw_queue(&self) -> &metal::CommandQueueRef {
        &self.queue
    }

    /// Create a GPU texture.
    pub fn create_texture(&self, desc: &GpuTextureDesc) -> GpuTexture {
        let mtl_desc = metal::TextureDescriptor::new();
        mtl_desc.set_pixel_format(to_mtl_pixel_format(desc.format));
        mtl_desc.set_width(desc.width as u64);
        mtl_desc.set_height(desc.height as u64);
        mtl_desc.set_depth(desc.depth as u64);
        mtl_desc.set_texture_type(to_mtl_texture_type(desc.dimension, desc.depth));
        mtl_desc.set_usage(to_mtl_texture_usage(desc.usage));
        mtl_desc.set_storage_mode(metal::MTLStorageMode::Private);
        mtl_desc.set_mipmap_level_count(1);
        mtl_desc.set_sample_count(1);
        let raw = self.device.new_texture(&mtl_desc);
        GpuTexture {
            raw,
            width: desc.width,
            height: desc.height,
            depth: desc.depth,
            format: desc.format,
        }
    }

    /// Create a GPU buffer with private storage (GPU-only).
    pub fn create_buffer(&self, size: u64, _usage: GpuBufferUsage) -> GpuBuffer {
        let raw = self.device.new_buffer(
            size,
            metal::MTLResourceOptions::StorageModePrivate,
        );
        GpuBuffer {
            raw,
            size,
            mapped_ptr: None,
        }
    }

    /// Create a GPU buffer with shared memory (CPU+GPU coherent).
    /// Returns a buffer with a persistent mapped pointer for zero-copy writes.
    pub fn create_buffer_shared(&self, size: u64) -> GpuBuffer {
        let raw = self.device.new_buffer(
            size,
            metal::MTLResourceOptions::StorageModeShared,
        );
        let ptr = raw.contents() as *mut u8;
        GpuBuffer {
            raw,
            size,
            mapped_ptr: if ptr.is_null() { None } else { Some(ptr) },
        }
    }

    /// Create a sampler state.
    pub fn create_sampler(&self, desc: &GpuSamplerDesc) -> GpuSampler {
        let mtl_desc = metal::SamplerDescriptor::new();
        mtl_desc.set_min_filter(to_mtl_filter(desc.min_filter));
        mtl_desc.set_mag_filter(to_mtl_filter(desc.mag_filter));
        mtl_desc.set_mip_filter(to_mtl_mip_filter(desc.mip_filter));
        mtl_desc.set_address_mode_s(to_mtl_address(desc.address_mode_u));
        mtl_desc.set_address_mode_t(to_mtl_address(desc.address_mode_v));
        mtl_desc.set_address_mode_r(to_mtl_address(desc.address_mode_w));
        let raw = self.device.new_sampler(&mtl_desc);
        GpuSampler { raw }
    }

    /// Create a compute pipeline from WGSL source.
    ///
    /// 1. Parse WGSL → naga Module
    /// 2. Introspect bindings → build slot map
    /// 3. Compile naga → MSL with slot assignments
    /// 4. Create MTLLibrary from MSL source
    /// 5. Create MTLComputePipelineState from entry function
    pub fn create_compute_pipeline(
        &self,
        wgsl_source: &str,
        entry_point: &str,
        label: &str,
    ) -> GpuComputePipeline {
        let (slot_map, msl_source, msl_entry_name, workgroup_size) =
            compile_wgsl_to_msl(wgsl_source, entry_point, label);

        let compile_opts = metal::CompileOptions::new();
        compile_opts.set_language_version(metal::MTLLanguageVersion::V2_4);
        compile_opts.set_fast_math_enabled(true);
        let library = self
            .device
            .new_library_with_source(&msl_source, &compile_opts)
            .unwrap_or_else(|e| {
                panic!("{label}: MTL library compile error: {e}\nMSL source:\n{msl_source}")
            });

        let function = library
            .get_function(&msl_entry_name, None)
            .unwrap_or_else(|e| {
                let names = library.function_names();
                panic!(
                    "{label}: function '{msl_entry_name}' not found: {e}. \
                     Available: {names:?}"
                )
            });

        let state = self
            .device
            .new_compute_pipeline_state_with_function(&function)
            .unwrap_or_else(|e| panic!("{label}: MTL compute PSO error: {e}"));

        let needs_sizes_buffer = slot_map.get(SIZES_BUFFER_BINDING).is_some();
        GpuComputePipeline {
            state,
            slot_map,
            label: label.to_string(),
            workgroup_size,
            needs_sizes_buffer,
        }
    }

    /// Create a render pipeline from WGSL source (fullscreen triangle pattern).
    ///
    /// Vertex shader generates a fullscreen triangle from vertex_index.
    /// No vertex buffers needed. Single color attachment.
    pub fn create_render_pipeline(
        &self,
        wgsl_source: &str,
        vs_entry: &str,
        fs_entry: &str,
        color_format: GpuTextureFormat,
        blend: Option<GpuBlendState>,
        label: &str,
    ) -> GpuRenderPipeline {
        // Compile both entry points
        let (slot_map, msl_source, _) =
            compile_wgsl_to_msl_render(wgsl_source, vs_entry, fs_entry, label);

        let compile_opts = metal::CompileOptions::new();
        compile_opts.set_language_version(metal::MTLLanguageVersion::V2_4);
        compile_opts.set_fast_math_enabled(true);
        let library = self
            .device
            .new_library_with_source(&msl_source, &compile_opts)
            .unwrap_or_else(|e| {
                panic!("{label}: MTL library compile error: {e}\nMSL source:\n{msl_source}")
            });

        // Try naga-mangled names first, then original names
        let available = library.function_names();
        let vs_func = find_entry_function(&library, vs_entry, &available, label, "vertex");
        let fs_func = find_entry_function(&library, fs_entry, &available, label, "fragment");

        let desc = metal::RenderPipelineDescriptor::new();
        desc.set_vertex_function(Some(&vs_func));
        desc.set_fragment_function(Some(&fs_func));

        let color_attach = desc
            .color_attachments()
            .object_at(0)
            .expect("color attachment 0");
        color_attach.set_pixel_format(to_mtl_pixel_format(color_format));

        if let Some(blend) = blend {
            color_attach.set_blending_enabled(true);
            color_attach.set_rgb_blend_operation(to_mtl_blend_op(blend.operation));
            color_attach.set_alpha_blend_operation(to_mtl_blend_op(blend.alpha_operation));
            color_attach.set_source_rgb_blend_factor(to_mtl_blend_factor(blend.src_factor));
            color_attach
                .set_destination_rgb_blend_factor(to_mtl_blend_factor(blend.dst_factor));
            color_attach
                .set_source_alpha_blend_factor(to_mtl_blend_factor(blend.src_alpha_factor));
            color_attach
                .set_destination_alpha_blend_factor(to_mtl_blend_factor(blend.dst_alpha_factor));
        }

        let state = self
            .device
            .new_render_pipeline_state(&desc)
            .unwrap_or_else(|e| panic!("{label}: MTL render PSO error: {e}"));

        GpuRenderPipeline {
            state,
            slot_map,
            label: label.to_string(),
        }
    }

    /// Create a new command encoder for one frame's GPU work.
    pub fn create_encoder(&self, label: &str) -> GpuEncoder {
        // Use retained references — Metal retains all resources set on encoders.
        // Slightly higher overhead than unretained, but guarantees resources
        // survive until GPU execution completes. Required because we extract
        // temporary GpuTexture wrappers (via extract_native_texture) that are
        // dropped before commit.
        let cmd_buf = self.queue.new_command_buffer();
        cmd_buf.set_label(label);
        // Retain the command buffer so it outlives the autorelease pool drain.
        let ptr = cmd_buf as *const metal::CommandBufferRef as *mut std::ffi::c_void;
        unsafe { objc_retain(ptr); }
        GpuEncoder {
            cmd_buf_ptr: ptr,
            state: EncoderState::None,
        }
    }

    /// Create a shared event for CPU↔GPU synchronization.
    pub fn create_event(&self) -> GpuEvent {
        let raw = self.device.new_shared_event();
        GpuEvent {
            raw,
            counter: std::cell::Cell::new(0),
        }
    }
}

// ─── GpuTexture ───────────────────────────────────────────────────────

/// GPU texture backed by a native Metal texture.
pub struct GpuTexture {
    pub(crate) raw: metal::Texture,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub format: GpuTextureFormat,
}

unsafe impl Send for GpuTexture {}
unsafe impl Sync for GpuTexture {}

impl GpuTexture {
    /// Wrap an existing metal::Texture (e.g. from IOSurface).
    pub fn from_raw(
        raw: metal::Texture,
        width: u32,
        height: u32,
        depth: u32,
        format: GpuTextureFormat,
    ) -> Self {
        Self { raw, width, height, depth, format }
    }

    /// Raw Metal texture reference.
    pub fn raw(&self) -> &metal::TextureRef {
        &self.raw
    }
}

// ─── GpuBuffer ────────────────────────────────────────────────────────

/// GPU buffer backed by a native Metal buffer.
pub struct GpuBuffer {
    pub(crate) raw: metal::Buffer,
    pub size: u64,
    /// Persistent mapped pointer for shared-memory buffers.
    /// Some for MTLStorageMode::Shared, None for Private.
    mapped_ptr: Option<*mut u8>,
}

unsafe impl Send for GpuBuffer {}
unsafe impl Sync for GpuBuffer {}

impl GpuBuffer {
    /// Wrap an existing metal::Buffer (e.g. extracted from wgpu).
    pub fn from_raw(raw: metal::Buffer, size: u64) -> Self {
        let ptr = raw.contents() as *mut u8;
        Self {
            raw,
            size,
            mapped_ptr: if ptr.is_null() { None } else { Some(ptr) },
        }
    }

    /// Persistent mapped pointer (shared-memory buffers only).
    /// Direct CPU→GPU writes with zero API overhead.
    pub fn mapped_ptr(&self) -> Option<*mut u8> {
        self.mapped_ptr
    }

    /// Write data at offset via memcpy (shared-memory buffers only).
    ///
    /// # Safety
    /// Caller must ensure offset + data.len() <= buffer size,
    /// and no GPU reads overlap this write.
    pub unsafe fn write(&self, offset: u64, data: &[u8]) {
        let ptr = self.mapped_ptr.expect("write() requires shared-memory buffer");
        unsafe {
            std::ptr::copy_nonoverlapping(
                data.as_ptr(),
                ptr.add(offset as usize),
                data.len(),
            );
        }
    }

    /// Raw Metal buffer reference.
    pub fn raw(&self) -> &metal::BufferRef {
        &self.raw
    }

    pub fn size(&self) -> u64 {
        self.size
    }
}

// ─── GpuSampler ───────────────────────────────────────────────────────

pub struct GpuSampler {
    pub(crate) raw: metal::SamplerState,
}

unsafe impl Send for GpuSampler {}
unsafe impl Sync for GpuSampler {}

// ─── GpuComputePipeline ───────────────────────────────────────────────

/// Reserved WGSL "binding" index for the naga sizes buffer.
/// Not a real @binding — used internally by the slot map.
pub const SIZES_BUFFER_BINDING: u32 = 0xFFFF;

pub struct GpuComputePipeline {
    pub(crate) state: metal::ComputePipelineState,
    pub slot_map: SlotMap,
    pub label: String,
    /// Workgroup size from the shader's @workgroup_size declaration.
    /// Used for dispatch_thread_groups second argument.
    pub workgroup_size: [u32; 3],
    /// Whether this pipeline needs a sizes buffer for runtime-sized arrays.
    pub needs_sizes_buffer: bool,
}

unsafe impl Send for GpuComputePipeline {}
unsafe impl Sync for GpuComputePipeline {}

// ─── GpuRenderPipeline ───────────────────────────────────────────────

pub struct GpuRenderPipeline {
    pub(crate) state: metal::RenderPipelineState,
    pub slot_map: SlotMap,
    pub label: String,
}

unsafe impl Send for GpuRenderPipeline {}
unsafe impl Sync for GpuRenderPipeline {}

// ─── GpuEvent ─────────────────────────────────────────────────────────

/// GPU↔CPU synchronization via MTLSharedEvent.
/// Near-zero overhead polling (direct counter read).
pub struct GpuEvent {
    raw: metal::SharedEvent,
    counter: std::cell::Cell<u64>,
}

unsafe impl Send for GpuEvent {}
unsafe impl Sync for GpuEvent {}

impl GpuEvent {
    /// Check if the GPU has completed work signaled at `value`.
    pub fn is_done(&self, value: u64) -> bool {
        self.raw.signaled_value() >= value
    }

    /// Current signal counter (store after signal_event).
    pub fn current_value(&self) -> u64 {
        self.counter.get()
    }

    /// Read the GPU-side signaled value directly.
    pub fn signaled_value(&self) -> u64 {
        self.raw.signaled_value()
    }

    /// Raw Metal shared event reference.
    pub fn raw(&self) -> &metal::SharedEventRef {
        &self.raw
    }
}

// ─── GpuEncoder ───────────────────────────────────────────────────────

/// Encoder state — tracks the current active Metal encoder.
#[allow(dead_code)]
enum EncoderState {
    None,
    /// Active compute command encoder.
    Compute(*const metal::ComputeCommandEncoderRef),
    /// Active render command encoder.
    Render(*const metal::RenderCommandEncoderRef),
    /// Active blit command encoder.
    Blit(*const metal::BlitCommandEncoderRef),
}

/// Per-frame GPU command encoder. Wraps a retained Metal command buffer.
///
/// Automatically manages compute/render/blit encoder transitions.
/// Compute encoders are kept alive across dispatches for efficiency.
/// Render/blit encoders are ended after each pass.
pub struct GpuEncoder {
    /// Retained MTLCommandBuffer. Released on drop.
    cmd_buf_ptr: *mut std::ffi::c_void,
    state: EncoderState,
}

unsafe impl Send for GpuEncoder {}

impl GpuEncoder {
    fn cmd_buf(&self) -> &metal::CommandBufferRef {
        unsafe { &*(self.cmd_buf_ptr as *const metal::CommandBufferRef) }
    }

    /// Ensure a compute encoder is active. Returns a raw pointer to it.
    fn ensure_compute(&mut self) -> *const metal::ComputeCommandEncoderRef {
        if let EncoderState::Compute(ptr) = self.state {
            return ptr;
        }
        self.end_current();
        let enc = self.cmd_buf().new_compute_command_encoder();
        let ptr = enc as *const metal::ComputeCommandEncoderRef;
        self.state = EncoderState::Compute(ptr);
        ptr
    }

    /// End the current encoder (if any).
    fn end_current(&mut self) {
        match self.state {
            EncoderState::None => {}
            EncoderState::Compute(ptr) => {
                unsafe { &*ptr }.end_encoding();
            }
            EncoderState::Render(ptr) => {
                unsafe { &*ptr }.end_encoding();
            }
            EncoderState::Blit(ptr) => {
                unsafe { &*ptr }.end_encoding();
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
        _label: &str,
    ) {
        let enc_ptr = self.ensure_compute();
        let enc = unsafe { &*enc_ptr };
        enc.set_compute_pipeline_state(&pipeline.state);

        // Collect buffer sizes for the sizes buffer (runtime-sized arrays).
        // naga's MSL backend reads arrayLength() from this auxiliary buffer.
        let mut buffer_sizes: Vec<u32> = Vec::new();

        for binding in bindings {
            match binding {
                GpuBinding::Buffer { binding: b, buffer, offset } => {
                    // Skip bindings not used by this entry point. Metal ignores
                    // unused argument slots, so this is safe. Multi-entry-point
                    // shaders have per-entry slot maps that may exclude globals
                    // not referenced by the specific entry point.
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_buffer(
                        slot.metal_index as _,
                        Some(&buffer.raw),
                        *offset as _,
                    );
                    // Track buffer size for sizes buffer generation.
                    // Indexed by Metal buffer argument index.
                    let idx = slot.metal_index as usize;
                    if idx >= buffer_sizes.len() {
                        buffer_sizes.resize(idx + 1, 0);
                    }
                    buffer_sizes[idx] = buffer.size as u32;
                }
                GpuBinding::Texture { binding: b, texture } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_texture(slot.metal_index as _, Some(&texture.raw));
                }
                GpuBinding::Sampler { binding: b, sampler } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                }
                GpuBinding::Bytes { binding: b, data } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
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
            let slot = pipeline.slot_map.get(SIZES_BUFFER_BINDING)
                .expect("sizes buffer slot missing");
            enc.set_bytes(
                slot.metal_index as _,
                (buffer_sizes.len() * 4) as _,
                buffer_sizes.as_ptr() as *const _,
            );
        }

        let wg = pipeline.workgroup_size;
        enc.dispatch_thread_groups(
            metal::MTLSize::new(workgroups[0] as _, workgroups[1] as _, workgroups[2] as _),
            metal::MTLSize::new(wg[0] as _, wg[1] as _, wg[2] as _),
        );
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
        _label: &str,
    ) {
        self.end_current();

        let desc = metal::RenderPassDescriptor::new();
        let color = desc.color_attachments().object_at(0).unwrap();
        color.set_texture(Some(&target.raw));
        color.set_load_action(if clear {
            metal::MTLLoadAction::Clear
        } else {
            metal::MTLLoadAction::DontCare
        });
        color.set_store_action(if store {
            metal::MTLStoreAction::Store
        } else {
            metal::MTLStoreAction::DontCare
        });
        color.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 0.0));

        let enc = self.cmd_buf().new_render_command_encoder(desc);
        enc.set_render_pipeline_state(&pipeline.state);

        for binding in bindings {
            match binding {
                GpuBinding::Buffer { binding: b, buffer, offset } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_fragment_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                }
                GpuBinding::Texture { binding: b, texture } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_fragment_texture(slot.metal_index as _, Some(&texture.raw));
                }
                GpuBinding::Sampler { binding: b, sampler } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_fragment_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                }
                GpuBinding::Bytes { binding: b, data } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_fragment_bytes(
                        slot.metal_index as _,
                        data.len() as _,
                        data.as_ptr() as *const _,
                    );
                }
            }
        }

        enc.draw_primitives(metal::MTLPrimitiveType::Triangle, 0, 3);
        enc.end_encoding();
        // State goes back to None (render encoder consumed).
    }

    /// Draw instanced geometry with a render pipeline.
    ///
    /// Unlike `draw_fullscreen()` which only sets fragment bindings,
    /// this sets bindings on BOTH vertex and fragment stages.
    /// Used by LinePipeline for instanced line/dot rendering.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_instanced(
        &mut self,
        pipeline: &GpuRenderPipeline,
        target: &GpuTexture,
        bindings: &[GpuBinding],
        vertex_count: u32,
        instance_count: u32,
        clear: bool,
        _label: &str,
    ) {
        self.end_current();

        let desc = metal::RenderPassDescriptor::new();
        let color = desc.color_attachments().object_at(0).unwrap();
        color.set_texture(Some(&target.raw));
        color.set_load_action(if clear {
            metal::MTLLoadAction::Clear
        } else {
            metal::MTLLoadAction::DontCare
        });
        color.set_store_action(metal::MTLStoreAction::Store);
        color.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 0.0));

        let enc = self.cmd_buf().new_render_command_encoder(desc);
        enc.set_render_pipeline_state(&pipeline.state);

        for binding in bindings {
            match binding {
                GpuBinding::Buffer { binding: b, buffer, offset } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    // Set on both vertex and fragment stages
                    enc.set_vertex_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                    enc.set_fragment_buffer(slot.metal_index as _, Some(&buffer.raw), *offset as _);
                }
                GpuBinding::Texture { binding: b, texture } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_vertex_texture(slot.metal_index as _, Some(&texture.raw));
                    enc.set_fragment_texture(slot.metal_index as _, Some(&texture.raw));
                }
                GpuBinding::Sampler { binding: b, sampler } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_vertex_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                    enc.set_fragment_sampler_state(slot.metal_index as _, Some(&sampler.raw));
                }
                GpuBinding::Bytes { binding: b, data } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_vertex_bytes(
                        slot.metal_index as _, data.len() as _,
                        data.as_ptr() as *const _,
                    );
                    enc.set_fragment_bytes(
                        slot.metal_index as _, data.len() as _,
                        data.as_ptr() as *const _,
                    );
                }
            }
        }

        if instance_count > 0 {
            enc.draw_primitives_instanced(
                metal::MTLPrimitiveType::Triangle,
                0, vertex_count as u64, instance_count as u64,
            );
        }
        enc.end_encoding();
    }

    /// Clear a texture to a solid color via a render pass with MTLLoadAction::Clear.
    /// No draw call — just load-clear + store.
    pub fn clear_texture(&mut self, texture: &GpuTexture, r: f64, g: f64, b: f64, a: f64) {
        self.end_current();
        let desc = metal::RenderPassDescriptor::new();
        let color = desc.color_attachments().object_at(0).unwrap();
        color.set_texture(Some(&texture.raw));
        color.set_load_action(metal::MTLLoadAction::Clear);
        color.set_store_action(metal::MTLStoreAction::Store);
        color.set_clear_color(metal::MTLClearColor::new(r, g, b, a));
        let enc = self.cmd_buf().new_render_command_encoder(desc);
        enc.end_encoding();
    }

    /// Fill a buffer with zeros via blit encoder.
    pub fn clear_buffer(&mut self, buffer: &GpuBuffer) {
        self.end_current();
        let enc = self.cmd_buf().new_blit_command_encoder();
        enc.fill_buffer(&buffer.raw, metal::NSRange::new(0, buffer.size), 0);
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
            0,                      // destination_offset
            bytes_per_row as u64,   // destination_bytes_per_row
            bytes_per_row as u64 * height as u64, // destination_bytes_per_image
            metal::MTLBlitOption::empty(),
        );
        enc.end_encoding();
    }

    /// Signal a shared event on the GPU timeline.
    /// Creates a lightweight command buffer that encodes the signal and commits it.
    /// The event value is incremented automatically.
    pub fn signal_event(&mut self, event: &GpuEvent) {
        let value = event.counter.get() + 1;
        event.counter.set(value);
        // Encode signal on current command buffer (after all work).
        self.end_current();
        let enc = self.cmd_buf().new_blit_command_encoder();
        // Use encode_signal_event through the command buffer's blit encoder
        // to add the signal to this command buffer.
        enc.end_encoding();
        // Signal on the command buffer directly
        self.cmd_buf().encode_signal_event(event.raw(), value);
    }

    /// Commit the command buffer to the GPU queue.
    /// Ends any active encoder and commits. Consumes the encoder.
    pub fn commit(mut self) {
        self.end_current();
        self.cmd_buf().commit();
        // Don't release in commit — Drop handles it
    }

    /// Commit and wait for GPU completion, checking for errors.
    /// Use for debugging — blocks until the GPU finishes this command buffer.
    pub fn commit_and_check(mut self) {
        self.end_current();
        let cmd_buf = self.cmd_buf();
        cmd_buf.commit();
        cmd_buf.wait_until_completed();
        let status = cmd_buf.status();
        if status == metal::MTLCommandBufferStatus::Error {
            // MTLCommandBufferError codes:
            // 1=Internal, 2=Timeout, 3=PageFault, 4=AccessRevoked,
            // 5=NotPermitted, 7=OutOfMemory, 8=InvalidResource, 12=StackOverflow
            eprintln!(
                "[GPU ERROR] Command buffer FAILED! status={status:?} \
                 label={:?}",
                cmd_buf.label(),
            );
        }
    }
}

impl Drop for GpuEncoder {
    fn drop(&mut self) {
        if !self.cmd_buf_ptr.is_null() {
            unsafe { objc_release(self.cmd_buf_ptr); }
        }
    }
}

// ─── WGSL→MSL compilation ─────────────────────────────────────────────

/// Parse WGSL, introspect bindings, compile to MSL for a compute entry point.
/// Returns (slot_map, msl_source, msl_entry_name).
fn compile_wgsl_to_msl(
    wgsl_source: &str,
    entry_point: &str,
    label: &str,
) -> (SlotMap, String, String, [u32; 3]) {
    // Step 1: Parse WGSL
    let module = naga::front::wgsl::parse_str(wgsl_source)
        .unwrap_or_else(|e| panic!("{label}: WGSL parse error: {e}"));

    // Step 2: Validate
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{label}: WGSL validation error: {e}"));

    // Step 3: Introspect bindings and build slot map + naga MSL options
    let (slot_map, entry_resources) = build_slot_map(&module, entry_point);

    let mut per_entry_point_map = naga::back::msl::EntryPointResourceMap::default();
    per_entry_point_map.insert(entry_point.to_string(), entry_resources);

    let options = naga::back::msl::Options {
        lang_version: (2, 4),
        per_entry_point_map,
        // Multi-entry-point shaders (e.g. fluid_scatter.wgsl with splat_main +
        // resolve_main) have globals at the same @binding that differ per entry.
        // fake_missing_bindings generates dummy targets for globals not in our
        // per_entry_point_map — they're never used by the compiled function.
        fake_missing_bindings: true,
        zero_initialize_workgroup_memory: true,
        ..Default::default()
    };

    let pipeline_options = naga::back::msl::PipelineOptions {
        allow_and_force_point_size: false,
        entry_point: None,
        vertex_pulling_transform: false,
        vertex_buffer_mappings: Vec::new(),
    };

    // Step 4: Compile to MSL
    let (msl_source, translation_info) =
        naga::back::msl::write_string(&module, &info, &options, &pipeline_options)
            .unwrap_or_else(|e| panic!("{label}: MSL compilation error: {e}"));

    // Step 5: Get actual MSL entry point name and workgroup size
    let entry_idx = module
        .entry_points
        .iter()
        .position(|ep| ep.name == entry_point)
        .unwrap_or_else(|| panic!("{label}: entry point '{entry_point}' not found in module"));
    let msl_entry_name = translation_info.entry_point_names[entry_idx]
        .as_ref()
        .unwrap_or_else(|e| panic!("{label}: entry point error: {e:?}"))
        .clone();

    let workgroup_size = module.entry_points[entry_idx].workgroup_size;

    (slot_map, msl_source, msl_entry_name, workgroup_size)
}

/// Parse WGSL, introspect bindings, compile to MSL for render (vertex + fragment).
/// Returns (slot_map_for_fragment, msl_source, dummy).
fn compile_wgsl_to_msl_render(
    wgsl_source: &str,
    vs_entry: &str,
    fs_entry: &str,
    label: &str,
) -> (SlotMap, String, String) {
    let module = naga::front::wgsl::parse_str(wgsl_source)
        .unwrap_or_else(|e| panic!("{label}: WGSL parse error: {e}"));
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap_or_else(|e| panic!("{label}: WGSL validation error: {e}"));

    // Build a UNIFIED slot map from the union of both entry points' globals.
    // VS and FS share the same Metal argument table, so bindings visible in
    // either stage need slots (e.g. line pipeline: positions/edges in VS only).
    let (unified_slot_map, entry_resources_vs, entry_resources_fs) =
        build_slot_map_render(&module, vs_entry, fs_entry);

    let mut per_entry_point_map = naga::back::msl::EntryPointResourceMap::default();
    per_entry_point_map.insert(vs_entry.to_string(), entry_resources_vs);
    per_entry_point_map.insert(fs_entry.to_string(), entry_resources_fs);

    let options = naga::back::msl::Options {
        lang_version: (2, 4),
        per_entry_point_map,
        fake_missing_bindings: true,
        zero_initialize_workgroup_memory: false,
        ..Default::default()
    };

    let pipeline_options = naga::back::msl::PipelineOptions {
        allow_and_force_point_size: false,
        entry_point: None,
        vertex_pulling_transform: false,
        vertex_buffer_mappings: Vec::new(),
    };

    let (msl_source, _info) =
        naga::back::msl::write_string(&module, &info, &options, &pipeline_options)
            .unwrap_or_else(|e| panic!("{label}: MSL compilation error: {e}"));

    (unified_slot_map, msl_source, String::new())
}

/// Build a unified SlotMap + per-entry-point EntryPointResources for a render
/// pipeline (vertex + fragment). Both stages share the same Metal argument table,
/// so the slot map includes globals from the union of both entry points.
/// Each stage gets its own EntryPointResources with the shared index assignments.
fn build_slot_map_render(
    module: &naga::Module,
    vs_entry: &str,
    fs_entry: &str,
) -> (SlotMap, naga::back::msl::EntryPointResources, naga::back::msl::EntryPointResources) {
    use naga::back::msl;

    // Collect globals from both entry points
    fn collect_ep_globals(
        module: &naga::Module,
        entry_name: &str,
    ) -> std::collections::HashSet<naga::Handle<naga::GlobalVariable>> {
        let ep = module.entry_points.iter().find(|ep| ep.name == entry_name);
        if let Some(ep) = ep {
            let mut called_fns: std::collections::HashSet<naga::Handle<naga::Function>> =
                std::collections::HashSet::new();
            collect_called_functions(&ep.function, module, &mut called_fns);
            let mut globals: std::collections::HashSet<naga::Handle<naga::GlobalVariable>> =
                std::collections::HashSet::new();
            collect_globals_from_function(&ep.function, &mut globals);
            for &fn_handle in &called_fns {
                collect_globals_from_function(&module.functions[fn_handle], &mut globals);
            }
            globals
        } else {
            module.global_variables.iter().map(|(h, _)| h).collect()
        }
    }

    let vs_globals = collect_ep_globals(module, vs_entry);
    let fs_globals = collect_ep_globals(module, fs_entry);

    // Union of both entry points' globals
    let all_globals: std::collections::HashSet<_> =
        vs_globals.union(&fs_globals).copied().collect();

    // Collect bindings from the union
    let mut bindings: Vec<(u32, naga::ResourceBinding, &naga::GlobalVariable)> = Vec::new();
    for (handle, gv) in module.global_variables.iter() {
        if let Some(ref binding) = gv.binding
            && all_globals.contains(&handle)
        {
            bindings.push((binding.binding, *binding, gv));
        }
    }
    bindings.sort_by_key(|(b, _, _)| *b);

    // Build unified slot map + per-entry-point resources with shared indices
    let mut slot_map = SlotMap::new();
    let mut resources_vs = msl::EntryPointResources::default();
    let mut resources_fs = msl::EntryPointResources::default();
    let mut next_buffer: u32 = 0;
    let mut next_texture: u32 = 0;
    let mut next_sampler: u32 = 0;

    for (binding_num, resource_binding, gv) in &bindings {
        let ty = &module.types[gv.ty];
        let is_buffer = matches!(
            gv.space,
            naga::AddressSpace::Uniform | naga::AddressSpace::Storage { .. }
        );
        let is_sampler = matches!(ty.inner, naga::TypeInner::Sampler { .. });
        let is_texture = matches!(ty.inner, naga::TypeInner::Image { .. });
        let is_writable = match gv.space {
            naga::AddressSpace::Storage { access } => {
                access.contains(naga::StorageAccess::STORE)
            }
            _ => false,
        } || matches!(
            ty.inner,
            naga::TypeInner::Image {
                class: naga::ImageClass::Storage { access, .. },
                ..
            } if access.contains(naga::StorageAccess::STORE)
        );

        let mut bind_target = msl::BindTarget::default();

        if is_buffer {
            let idx = next_buffer;
            next_buffer += 1;
            bind_target.buffer = Some(idx as u8);
            bind_target.mutable = is_writable;
            slot_map.insert(*binding_num, Slot {
                kind: SlotKind::Buffer,
                metal_index: idx,
            });
        } else if is_sampler {
            let idx = next_sampler;
            next_sampler += 1;
            bind_target.sampler = Some(msl::BindSamplerTarget::Resource(idx as u8));
            slot_map.insert(*binding_num, Slot {
                kind: SlotKind::Sampler,
                metal_index: idx,
            });
        } else if is_texture {
            let idx = next_texture;
            next_texture += 1;
            bind_target.texture = Some(idx as u8);
            bind_target.mutable = is_writable;
            slot_map.insert(*binding_num, Slot {
                kind: SlotKind::Texture,
                metal_index: idx,
            });
        }

        // Add to both entry points' resources — naga + fake_missing_bindings
        // handles the case where a binding is only used in one stage.
        resources_vs.resources.insert(*resource_binding, bind_target.clone());
        resources_fs.resources.insert(*resource_binding, bind_target);
    }

    (slot_map, resources_vs, resources_fs)
}

/// Build a SlotMap and naga EntryPointResources from a naga module.
///
/// Iterates over global variables used by the entry point and assigns
/// sequential Metal argument indices per resource type:
/// - Buffers (uniform + storage) → buffer(0), buffer(1), ...
/// - Textures (sampled + storage) → texture(0), texture(1), ...
/// - Samplers → sampler(0), sampler(1), ...
fn build_slot_map(
    module: &naga::Module,
    entry_point: &str,
) -> (SlotMap, naga::back::msl::EntryPointResources) {
    use naga::back::msl;

    let mut slot_map = SlotMap::new();
    let mut resources = msl::EntryPointResources::default();

    let mut next_buffer: u32 = 0;
    let mut next_texture: u32 = 0;
    let mut next_sampler: u32 = 0;

    // Find which global variables are actually used by this entry point.
    // Multi-entry-point shaders (e.g. fluid_scatter.wgsl) reuse @binding(N)
    // for different types per entry point — we must only map the ones used.
    let ep = module
        .entry_points
        .iter()
        .find(|ep| ep.name == entry_point);

    // Scan entry point AND all reachable functions for GlobalVariable references.
    // The entry point's function body may call helper functions that reference
    // globals (e.g. bloom_compute.wgsl: cs_main → blur13 → source_tex_b).
    // We must include globals from called functions too, or bindings get dropped.
    let used_globals: std::collections::HashSet<naga::Handle<naga::GlobalVariable>> =
        if let Some(ep) = ep {
            // First collect all functions called from the entry point (transitively).
            let mut called_fns: std::collections::HashSet<naga::Handle<naga::Function>> =
                std::collections::HashSet::new();
            collect_called_functions(&ep.function, module, &mut called_fns);

            // Scan entry point + all called functions for GlobalVariable refs.
            let mut globals: std::collections::HashSet<naga::Handle<naga::GlobalVariable>> =
                std::collections::HashSet::new();
            collect_globals_from_function(&ep.function, &mut globals);
            for &fn_handle in &called_fns {
                collect_globals_from_function(&module.functions[fn_handle], &mut globals);
            }
            globals
        } else {
            // Fallback: include all globals if entry point not found
            module.global_variables.iter().map(|(h, _)| h).collect()
        };

    // Collect bindings only for globals referenced by this entry point
    let mut bindings: Vec<(u32, naga::ResourceBinding, &naga::GlobalVariable)> = Vec::new();
    for (handle, gv) in module.global_variables.iter() {
        if let Some(ref binding) = gv.binding
            && used_globals.contains(&handle)
        {
            bindings.push((binding.binding, *binding, gv));
        }
    }
    // Sort by binding number for deterministic index assignment
    bindings.sort_by_key(|(b, _, _)| *b);

    for (binding_num, resource_binding, gv) in &bindings {
        let ty = &module.types[gv.ty];
        let is_buffer = matches!(
            gv.space,
            naga::AddressSpace::Uniform | naga::AddressSpace::Storage { .. }
        );
        let is_sampler = matches!(ty.inner, naga::TypeInner::Sampler { .. });
        let is_texture = matches!(
            ty.inner,
            naga::TypeInner::Image { .. }
        );

        let is_writable = match gv.space {
            naga::AddressSpace::Storage { access } => {
                access.contains(naga::StorageAccess::STORE)
            }
            _ => false,
        } || matches!(
            ty.inner,
            naga::TypeInner::Image {
                class: naga::ImageClass::Storage { access, .. },
                ..
            } if access.contains(naga::StorageAccess::STORE)
        );

        let mut bind_target = msl::BindTarget::default();

        if is_buffer {
            let idx = next_buffer;
            next_buffer += 1;
            bind_target.buffer = Some(idx as u8);
            bind_target.mutable = is_writable;
            slot_map.insert(*binding_num, Slot {
                kind: SlotKind::Buffer,
                metal_index: idx,
            });
        } else if is_sampler {
            let idx = next_sampler;
            next_sampler += 1;
            bind_target.sampler = Some(msl::BindSamplerTarget::Resource(idx as u8));
            slot_map.insert(*binding_num, Slot {
                kind: SlotKind::Sampler,
                metal_index: idx,
            });
        } else if is_texture {
            let idx = next_texture;
            next_texture += 1;
            bind_target.texture = Some(idx as u8);
            bind_target.mutable = is_writable;
            slot_map.insert(*binding_num, Slot {
                kind: SlotKind::Texture,
                metal_index: idx,
            });
        }

        resources
            .resources
            .insert(*resource_binding, bind_target);
    }

    // Detect runtime-sized arrays in storage buffers.
    // naga's MSL backend needs a "sizes buffer" containing the byte size of each
    // runtime-sized buffer so it can resolve arrayLength() calls.
    // Covers both top-level `array<T>` and struct with last member `array<T>`.
    let has_runtime_array = bindings.iter().any(|(_, _, gv)| {
        matches!(gv.space, naga::AddressSpace::Storage { .. }) && {
            let ty = &module.types[gv.ty];
            match &ty.inner {
                // Top-level runtime-sized array: var<storage> foo: array<T>
                naga::TypeInner::Array { size: naga::ArraySize::Dynamic, .. } => true,
                // Struct with last member being a runtime-sized array
                naga::TypeInner::Struct { members, .. } => {
                    members.last().is_some_and(|m| {
                        matches!(
                            module.types[m.ty].inner,
                            naga::TypeInner::Array { size: naga::ArraySize::Dynamic, .. }
                        )
                    })
                }
                // Binding array (runtime array of resources)
                naga::TypeInner::BindingArray { size: naga::ArraySize::Dynamic, .. } => true,
                _ => false,
            }
        }
    });

    if has_runtime_array {
        // Assign the sizes buffer to the next available buffer index.
        resources.sizes_buffer = Some(next_buffer as u8);
        // Store in slot map so dispatch can bind it.
        slot_map.insert(SIZES_BUFFER_BINDING, Slot {
            kind: SlotKind::Buffer,
            metal_index: next_buffer,
        });
        next_buffer += 1;
    }

    let _ = (ep, next_buffer); // suppress unused warnings

    (slot_map, resources)
}

/// Collect GlobalVariable handles referenced in a function's expressions.
fn collect_globals_from_function(
    func: &naga::Function,
    out: &mut std::collections::HashSet<naga::Handle<naga::GlobalVariable>>,
) {
    for (_, expr) in func.expressions.iter() {
        if let naga::Expression::GlobalVariable(handle) = *expr {
            out.insert(handle);
        }
    }
}

/// Recursively collect all functions called from `func` (transitive closure).
fn collect_called_functions(
    func: &naga::Function,
    module: &naga::Module,
    out: &mut std::collections::HashSet<naga::Handle<naga::Function>>,
) {
    for (_, expr) in func.expressions.iter() {
        if let naga::Expression::CallResult(fn_handle) = *expr
            && out.insert(fn_handle)
        {
            collect_called_functions(&module.functions[fn_handle], module, out);
        }
    }
    // Also scan block statements for Call statements (not all calls have results)
    collect_calls_from_block(&func.body, module, out);
}

/// Scan a naga Block for Call statements and collect called function handles.
fn collect_calls_from_block(
    block: &naga::Block,
    module: &naga::Module,
    out: &mut std::collections::HashSet<naga::Handle<naga::Function>>,
) {
    for stmt in block.iter() {
        match *stmt {
            naga::Statement::Call { function, .. } => {
                if out.insert(function) {
                    collect_called_functions(&module.functions[function], module, out);
                }
            }
            naga::Statement::Block(ref inner) => {
                collect_calls_from_block(inner, module, out);
            }
            naga::Statement::If { ref accept, ref reject, .. } => {
                collect_calls_from_block(accept, module, out);
                collect_calls_from_block(reject, module, out);
            }
            naga::Statement::Switch { ref cases, .. } => {
                for case in cases {
                    collect_calls_from_block(&case.body, module, out);
                }
            }
            naga::Statement::Loop { ref body, ref continuing, .. } => {
                collect_calls_from_block(body, module, out);
                collect_calls_from_block(continuing, module, out);
            }
            _ => {}
        }
    }
}

/// Find an entry function in a Metal library. Tries the exact name first,
/// then looks for naga-mangled versions (e.g. "cs_main" → "cs_main_").
fn find_entry_function(
    library: &metal::LibraryRef,
    entry_name: &str,
    available: &[String],
    label: &str,
    stage: &str,
) -> metal::Function {
    // Try exact name
    if let Ok(f) = library.get_function(entry_name, None) {
        return f;
    }
    // Try with underscore suffix (naga sometimes appends)
    let mangled = format!("{entry_name}_");
    if let Ok(f) = library.get_function(&mangled, None) {
        return f;
    }
    // Try matching prefix
    for name in available {
        if name.starts_with(entry_name)
            && let Ok(f) = library.get_function(name, None)
        {
            return f;
        }
    }
    panic!(
        "{label}: {stage} function '{entry_name}' not found. Available: {available:?}"
    );
}

// ─── Format conversion helpers ────────────────────────────────────────

fn to_mtl_pixel_format(format: GpuTextureFormat) -> metal::MTLPixelFormat {
    match format {
        GpuTextureFormat::Rgba16Float => metal::MTLPixelFormat::RGBA16Float,
        GpuTextureFormat::Rgba32Float => metal::MTLPixelFormat::RGBA32Float,
        GpuTextureFormat::Rgba8Unorm => metal::MTLPixelFormat::RGBA8Unorm,
        GpuTextureFormat::R32Float => metal::MTLPixelFormat::R32Float,
        GpuTextureFormat::Rg32Float => metal::MTLPixelFormat::RG32Float,
        GpuTextureFormat::R16Float => metal::MTLPixelFormat::R16Float,
        GpuTextureFormat::Rg16Float => metal::MTLPixelFormat::RG16Float,
        GpuTextureFormat::R32Uint => metal::MTLPixelFormat::R32Uint,
    }
}

fn to_mtl_texture_type(dim: GpuTextureDimension, _depth: u32) -> metal::MTLTextureType {
    match dim {
        GpuTextureDimension::D2 => metal::MTLTextureType::D2,
        GpuTextureDimension::D3 => metal::MTLTextureType::D3,
    }
}

fn to_mtl_texture_usage(usage: GpuTextureUsage) -> metal::MTLTextureUsage {
    let mut mtl = metal::MTLTextureUsage::Unknown;
    if usage.contains(GpuTextureUsage::SHADER_READ) {
        mtl |= metal::MTLTextureUsage::ShaderRead;
    }
    if usage.contains(GpuTextureUsage::SHADER_WRITE) {
        mtl |= metal::MTLTextureUsage::ShaderWrite;
    }
    if usage.contains(GpuTextureUsage::RENDER_TARGET) {
        mtl |= metal::MTLTextureUsage::RenderTarget;
    }
    mtl
}

fn to_mtl_filter(filter: GpuFilterMode) -> metal::MTLSamplerMinMagFilter {
    match filter {
        GpuFilterMode::Nearest => metal::MTLSamplerMinMagFilter::Nearest,
        GpuFilterMode::Linear => metal::MTLSamplerMinMagFilter::Linear,
    }
}

fn to_mtl_mip_filter(filter: GpuFilterMode) -> metal::MTLSamplerMipFilter {
    match filter {
        GpuFilterMode::Nearest => metal::MTLSamplerMipFilter::Nearest,
        GpuFilterMode::Linear => metal::MTLSamplerMipFilter::Linear,
    }
}

fn to_mtl_address(mode: GpuAddressMode) -> metal::MTLSamplerAddressMode {
    match mode {
        GpuAddressMode::ClampToEdge => metal::MTLSamplerAddressMode::ClampToEdge,
        GpuAddressMode::Repeat => metal::MTLSamplerAddressMode::Repeat,
        GpuAddressMode::MirrorRepeat => metal::MTLSamplerAddressMode::MirrorRepeat,
        GpuAddressMode::ClampToZero => metal::MTLSamplerAddressMode::ClampToZero,
    }
}

fn to_mtl_blend_factor(factor: GpuBlendFactor) -> metal::MTLBlendFactor {
    match factor {
        GpuBlendFactor::Zero => metal::MTLBlendFactor::Zero,
        GpuBlendFactor::One => metal::MTLBlendFactor::One,
        GpuBlendFactor::SrcAlpha => metal::MTLBlendFactor::SourceAlpha,
        GpuBlendFactor::OneMinusSrcAlpha => metal::MTLBlendFactor::OneMinusSourceAlpha,
        GpuBlendFactor::DstAlpha => metal::MTLBlendFactor::DestinationAlpha,
        GpuBlendFactor::OneMinusDstAlpha => metal::MTLBlendFactor::OneMinusDestinationAlpha,
        GpuBlendFactor::SrcColor => metal::MTLBlendFactor::SourceColor,
        GpuBlendFactor::OneMinusSrcColor => metal::MTLBlendFactor::OneMinusSourceColor,
        GpuBlendFactor::DstColor => metal::MTLBlendFactor::DestinationColor,
        GpuBlendFactor::OneMinusDstColor => metal::MTLBlendFactor::OneMinusDestinationColor,
    }
}

fn to_mtl_blend_op(op: GpuBlendOp) -> metal::MTLBlendOperation {
    match op {
        GpuBlendOp::Add => metal::MTLBlendOperation::Add,
        GpuBlendOp::Subtract => metal::MTLBlendOperation::Subtract,
        GpuBlendOp::ReverseSubtract => metal::MTLBlendOperation::ReverseSubtract,
        GpuBlendOp::Min => metal::MTLBlendOperation::Min,
        GpuBlendOp::Max => metal::MTLBlendOperation::Max,
    }
}

