//! Vulkan `GpuEncoder`: records one command buffer. Not built yet: each
//! method names the phase that builds it.

// Stub bodies name the phase that builds them (design section 9); their
// parameters are unused until then.
#![allow(unused_variables)]

use super::*;
use crate::*;

pub struct GpuEncoder {
    /// The open encode-replay span's cache (`replay.rs`).
    pub(crate) replay: Option<super::replay::GpuReplayCache>,
}

impl GpuEncoder {
    /// Enable per-dispatch GPU timestamp profiling for this frame's command
    /// buffer.
    pub fn enable_dispatch_profiling(&mut self, sampler: GpuTimestampSampler, device: &GpuDevice) {
        todo!("vulkan P2")
    }

    /// [`Self::enable_dispatch_profiling`] at a chosen [`ProfileGranularity`].
    pub fn enable_profiling_at(
        &mut self,
        sampler: GpuTimestampSampler,
        device: &GpuDevice,
        granularity: ProfileGranularity,
    ) {
        todo!("vulkan P2")
    }

    /// Name the next profiled encoder's existing boundary samples.
    #[cfg(feature = "gpu-proofs")]
    pub fn profile_next_encoder(&mut self, label: &'static str) {
        todo!("vulkan P2")
    }

    /// Set the attribution tag stamped onto subsequently profiled spans.
    pub fn set_profile_tag(&mut self, tag: &str) {
        todo!("vulkan P2")
    }

    /// Commit, wait for completion, and resolve the frame's profiled spans.
    pub fn commit_and_wait_profiled(self, device: &GpuDevice) -> GpuFrameProfile {
        todo!("vulkan P2")
    }

    /// Dispatch a compute shader.
    pub fn dispatch_compute(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        workgroups: [u32; 3],
        label: &str,
    ) {
        todo!("vulkan P1")
    }

    /// Dispatch a compute shader over a GPU-written grid: three u32
    /// threadgroup counts at `offset` in `args`, written earlier in this
    /// command buffer.
    pub fn dispatch_compute_indirect(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        args: &GpuBuffer,
        offset: u64,
        label: &str,
    ) {
        todo!("vulkan P1")
    }

    /// Insert a buffer-scope memory barrier on the active compute encoder.
    pub fn compute_memory_barrier_buffers(&mut self) {
        todo!("vulkan P1")
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
        todo!("vulkan P2")
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
        todo!("vulkan P2")
    }

    /// Draw instanced geometry with a render pipeline.
    pub fn draw_instanced(
        &mut self,
        pipeline: &GpuRenderPipeline,
        target: &GpuTexture,
        bindings: &[GpuBinding],
        count: DrawCount,
        load_action: crate::GpuLoadAction,
        label: &str,
    ) {
        todo!("vulkan P2")
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
        todo!("vulkan P2")
    }

    /// One draw in a [`Self::draw_instanced_depth_msaa_batch`] call.
    pub fn depth_msaa_draw<'a>(
        pipeline: &'a GpuRenderPipeline,
        bindings: &'a [GpuBinding<'a>],
        vertex_count: u32,
        instance_count: u32,
    ) -> DepthMsaaDraw<'a> {
        todo!("vulkan P2")
    }

    /// [`Self::depth_msaa_draw`] with an explicit triangle fill mode — the
    /// wireframe path (SCENE_RENDER_MODE_DESIGN.md D6).
    pub fn depth_msaa_draw_fill_mode<'a>(
        pipeline: &'a GpuRenderPipeline,
        bindings: &'a [GpuBinding<'a>],
        vertex_count: u32,
        instance_count: u32,
        fill_mode: crate::GpuTriangleFillMode,
    ) -> DepthMsaaDraw<'a> {
        todo!("vulkan P2")
    }

    /// [`Self::depth_msaa_draw`] with `Point` topology — the Points render
    /// mode (SCENE_RENDER_MODE_DESIGN.md D8).
    pub fn depth_msaa_draw_points<'a>(
        pipeline: &'a GpuRenderPipeline,
        bindings: &'a [GpuBinding<'a>],
        vertex_count: u32,
        instance_count: u32,
    ) -> DepthMsaaDraw<'a> {
        todo!("vulkan P2")
    }

    /// Draw a batch of depth-tested triangle meshes into ONE 4x-MSAA pass,
    /// resolving to `resolve_target`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_instanced_depth_msaa_batch(
        &mut self,
        msaa_color: &GpuTexture,
        resolve_target: &GpuTexture,
        msaa_depth: &GpuTexture,
        depth_stencil_state: &GpuDepthStencilState,
        draws: &[DepthMsaaDraw],
        label: &str,
    ) {
        todo!("vulkan P2")
    }

    /// [`Self::draw_instanced_depth_msaa_batch`]'s desc-driven superset.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_instanced_depth_msaa_batch_desc(
        &mut self,
        desc: &DepthMsaaPassDesc,
        draws: &[DepthMsaaDraw],
        label: &str,
    ) {
        todo!("vulkan P2")
    }

    /// Draw a batch of meshes into ONE single-sample colour+depth pass with
    /// independently controlled load actions per attachment
    /// The non-MSAA sibling of
    /// [`Self::draw_instanced_depth_msaa_batch_desc`]: no memoryless
    /// multisample scratch, no resolve — `target`/`depth_target` are real,
    /// already-populated single-sample textures.
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
        todo!("vulkan P2")
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
        todo!("vulkan P2")
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
        todo!("vulkan P2")
    }

    /// Draw a batch of meshes into ONE **depth-only** render pass — no colour
    /// attachment at all.
    pub fn draw_instanced_depth_only_batch(
        &mut self,
        depth_target: &GpuTexture,
        depth_stencil_state: &GpuDepthStencilState,
        draws: &[DepthMsaaDraw],
        label: &str,
    ) {
        todo!("vulkan P2")
    }

    /// Clear `depth_target` and draw into its top-left `viewport` square only.
    pub fn draw_instanced_depth_only_batch_viewport(
        &mut self,
        depth_target: &GpuTexture,
        depth_stencil_state: &GpuDepthStencilState,
        draws: &[DepthMsaaDraw],
        viewport: [u32; 2],
        label: &str,
    ) {
        todo!("vulkan P2")
    }

    /// Draw a depth-only batch while preserving the existing depth attachment.
    pub fn draw_instanced_depth_only_batch_loaded(
        &mut self,
        depth_target: &GpuTexture,
        depth_stencil_state: &GpuDepthStencilState,
        draws: &[DepthMsaaDraw],
        label: &str,
    ) {
        todo!("vulkan P2")
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
        todo!("vulkan P2")
    }

    /// Begin a render pass that stays alive across multiple draw calls.
    pub fn begin_render_pass(
        &mut self,
        target: &GpuTexture,
        load_action: crate::GpuLoadAction,
        label: &str,
    ) {
        todo!("vulkan P2")
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
        todo!("vulkan P2")
    }

    /// Set the scissor rectangle on the active render pass.
    pub fn set_scissor_rect(&mut self, x: u32, y: u32, w: u32, h: u32) {
        todo!("vulkan P2")
    }

    /// End the active render pass.
    pub fn end_render_pass(&mut self) {
        todo!("vulkan P2")
    }

    /// Clear a texture to a solid color.
    pub fn clear_texture(&mut self, texture: &GpuTexture, r: f64, g: f64, b: f64, a: f64) {
        todo!("vulkan P1")
    }

    /// Copy a single-sample depth texture into an R32Float storage texture.
    pub fn copy_depth_to_float(&mut self, source: &GpuTexture, target: &GpuTexture) {
        todo!("vulkan P2")
    }

    /// Fill a buffer with zeros via blit encoder.
    pub fn clear_buffer(&mut self, buffer: &GpuBuffer) {
        todo!("vulkan P1")
    }

    /// Generate the mipmap chain for a texture using Metal's optimized
    /// blit-encoder path.
    pub fn generate_mipmaps(&mut self, texture: &GpuTexture) {
        todo!("vulkan P2")
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
        todo!("vulkan P1")
    }

    /// Copy `size` bytes from `src` buffer to `dst` buffer via blit
    /// encoder.
    pub fn copy_buffer_to_buffer(&mut self, src: &GpuBuffer, dst: &GpuBuffer, size: u64) {
        todo!("vulkan P1")
    }

    /// Copy `size` bytes from `src` at `src_offset` to `dst` at
    /// `dst_offset` via blit encoder: one row of a row-major array into
    /// another.
    pub fn copy_buffer_range(
        &mut self,
        src: &GpuBuffer,
        src_offset: u64,
        dst: &GpuBuffer,
        dst_offset: u64,
        size: u64,
    ) {
        todo!("vulkan P1")
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
        todo!("vulkan P1")
    }

    /// Copy a full 3D texture volume (`width × height × depth`) to a buffer.
    pub fn copy_texture_3d_to_buffer(
        &mut self,
        src: &GpuTexture,
        dst: &GpuBuffer,
        width: u32,
        height: u32,
        depth: u32,
        bytes_per_row: u32,
    ) {
        todo!("vulkan P1")
    }

    /// Insert a pipeline barrier between dispatches.
    pub fn pipeline_barrier(&mut self, reads: &[&GpuTexture], writes: &[&GpuTexture]) {
        todo!("vulkan P1")
    }

    /// Upload CPU data to a CPU_UPLOAD 2D texture region via replaceRegion.
    pub fn upload_texture(
        &mut self,
        texture: &GpuTexture,
        width: u32,
        height: u32,
        _depth: u32,
        data: &[u8],
    ) {
        todo!("vulkan P1")
    }

    /// Signal a shared event on the GPU timeline.
    pub fn signal_event(&mut self, event: &GpuEvent) {
        todo!("vulkan P1")
    }

    /// Signal a shared event with a specific value (does NOT auto-increment).
    pub fn signal_event_value(&mut self, event: &GpuEvent, value: u64) {
        todo!("vulkan P1")
    }

    /// Wait for a shared event to reach a specific value before executing
    /// subsequent GPU work on this command buffer.
    pub fn wait_event(&mut self, event: &GpuEvent, value: u64) {
        todo!("vulkan P1")
    }

    /// Encode what follows on the device's second queue, beside the main
    /// lane's work.
    pub fn use_async_lane(&mut self, device: &GpuDevice, handoff: &GpuEvent) -> bool {
        todo!("vulkan P1")
    }

    /// Encode on the main queue again.
    pub fn use_main_lane(&mut self) {
        todo!("vulkan P1")
    }

    /// Signal `handoff` on the async lane and commit its buffer, never
    /// blocking, and remember the value.
    pub fn finish_async_lane(&mut self, handoff: &GpuEvent) {
        todo!("vulkan P1")
    }

    /// The main lane waits, on the GPU, for the value the last
    /// [`Self::finish_async_lane`] remembered: work encoded after this runs
    /// after everything the async lane did.
    pub fn wait_async_lane(&mut self, handoff: &GpuEvent) {
        todo!("vulkan P1")
    }

    /// GPU proofs: how many async lanes this thread has finished.
    #[cfg(feature = "gpu-proofs")]
    pub fn async_lanes_on_this_thread() -> u64 {
        todo!("vulkan P1")
    }

    /// GPU proofs: how many main-to-async handoffs this thread has made.
    #[cfg(feature = "gpu-proofs")]
    pub fn handoffs_on_this_thread() -> u64 {
        todo!("vulkan P1")
    }

    /// Register a callback to run when the GPU finishes executing this command buffer.
    pub fn add_completed_handler<F: Fn() + Send + 'static>(&self, callback: F) {
        todo!("vulkan P1")
    }

    /// Register a completed handler that reports this buffer's **true GPU
    /// execution time** (`GPUEndTime - GPUStartTime`, seconds) once the GPU
    /// finishes.
    pub fn add_gpu_time_handler<F: Fn(f64) + Send + 'static>(&self, callback: F) {
        todo!("vulkan P2")
    }

    /// Register a diagnostic completed handler that logs GPU errors.
    pub fn add_completed_handler_with_status(&self, label: &str) {
        todo!("vulkan P1")
    }

    /// Record a human-readable scope (card, layer, pass) against this
    /// encoder.
    pub fn note_scope(&mut self, scope: &str) {
        todo!("vulkan P1")
    }

    /// Report the GPU start and end time (seconds, the device's clock) of
    /// every command buffer this encoder commits to `tap`: the final one and
    /// each chunk a `commit_and_continue` split off.
    pub fn tap_gpu_time(&mut self, tap: std::sync::Arc<dyn Fn(f64, f64) + Send + Sync>) {
        todo!("vulkan P2")
    }

    /// Commit the command buffer to the GPU queue.
    pub fn commit(self) {
        todo!("vulkan P1")
    }

    /// Commit the current command buffer to the GPU queue and continue
    /// encoding into a fresh command buffer from the same queue.
    pub fn commit_and_continue(&mut self, device: &GpuDevice) {
        todo!("vulkan P1")
    }

    /// [`Self::commit_and_continue`], then block until the GPU has completed
    /// everything committed so far.
    pub fn commit_wait_and_continue(&mut self, device: &GpuDevice) {
        todo!("vulkan P1")
    }

    /// Commit and block until the GPU has fully completed the work.
    pub fn commit_and_wait_completed(self) {
        todo!("vulkan P1")
    }

    /// Commit, wait for completion, and return a failure instead of panicking.
    pub fn try_commit_and_wait_completed(self) -> Result<(), String> {
        todo!("vulkan P1")
    }

    /// Commit, block until completion, and return the **true GPU execution
    /// time** of this command buffer in seconds (`GPUEndTime - GPUStartTime`).
    pub fn commit_and_wait_completed_timed(self) -> f64 {
        todo!("vulkan P1")
    }
}
