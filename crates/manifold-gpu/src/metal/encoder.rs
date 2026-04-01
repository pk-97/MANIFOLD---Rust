//! GpuEncoder — per-frame GPU command encoder wrapping a retained Metal command buffer.

use crate::types::*;
use super::*;

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

/// Per-frame GPU command encoder. Wraps a retained Metal command buffer.
///
/// Automatically manages compute/render/blit encoder transitions.
/// Compute encoders are kept alive across dispatches for efficiency.
/// Render/blit encoders are ended after each pass.
pub struct GpuEncoder {
    /// Retained MTLCommandBuffer. Released on drop.
    pub(crate) cmd_buf_ptr: *mut std::ffi::c_void,
    pub(crate) state: EncoderState,
}

unsafe impl Send for GpuEncoder {}

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
        unsafe { objc_retain(ptr as *mut std::ffi::c_void); }
        self.state = EncoderState::Compute(ptr);
        ptr
    }

    /// End the current encoder (if any).
    pub(super) fn end_current(&mut self) {
        match self.state {
            EncoderState::None => {}
            EncoderState::Compute(ptr) => {
                unsafe { &*ptr }.end_encoding();
                unsafe { objc_release(ptr as *mut std::ffi::c_void); }
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
                    if idx < MAX_BUFFER_SLOTS {
                        buffer_sizes[idx] = buffer.size as u32;
                        if idx >= buffer_sizes_len {
                            buffer_sizes_len = idx + 1;
                        }
                    }
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
                GpuBinding::Buffer { binding: b, buffer, offset } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_fragment_buffer(
                        slot.metal_index as _, Some(&buffer.raw), *offset as _,
                    );
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
                GpuBinding::Buffer { binding: b, buffer, offset } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_fragment_buffer(
                        slot.metal_index as _, Some(&buffer.raw), *offset as _,
                    );
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
        enc.pop_debug_group();
        enc.end_encoding();
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
                GpuBinding::Buffer { binding: b, buffer, offset } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    // Set on both vertex and fragment stages
                    enc.set_vertex_buffer(
                        slot.metal_index as _, Some(&buffer.raw), *offset as _,
                    );
                    enc.set_fragment_buffer(
                        slot.metal_index as _, Some(&buffer.raw), *offset as _,
                    );
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
        enc.pop_debug_group();
        enc.end_encoding();
    }

    /// Draw indexed geometry with a render pipeline and vertex/index buffers.
    ///
    /// Sets bindings on BOTH vertex and fragment stages (same as `draw_instanced`).
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
        enc.set_vertex_buffer(VERTEX_BUFFER_INDEX, Some(&vertex_buffer.raw), vertex_offset as _);

        // Set all bindings on both vertex and fragment stages.
        for binding in bindings {
            match binding {
                GpuBinding::Buffer { binding: b, buffer, offset } => {
                    let Some(slot) = pipeline.slot_map.get(*b) else { continue };
                    enc.set_vertex_buffer(
                        slot.metal_index as _, Some(&buffer.raw), *offset as _,
                    );
                    enc.set_fragment_buffer(
                        slot.metal_index as _, Some(&buffer.raw), *offset as _,
                    );
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

    /// Commit the command buffer to the GPU queue.
    /// Ends any active encoder and commits. Consumes the encoder.
    pub fn commit(mut self) {
        self.end_current();
        self.cmd_buf().commit();
        // Don't release in commit — Drop handles it
    }

}

impl Drop for GpuEncoder {
    fn drop(&mut self) {
        self.end_current();
        if !self.cmd_buf_ptr.is_null() {
            unsafe { objc_release(self.cmd_buf_ptr); }
        }
    }
}
