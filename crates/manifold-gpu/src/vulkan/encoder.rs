//! Vulkan `GpuEncoder` (P1 steps 11 and 12). Compute dispatches, copies,
//! clears and uploads record into command buffers from a recycled
//! `FrameResources`, with the barriers the hazard tracker asks for (design
//! section 5). Event waits and signals split the recording into batches that
//! one `vkQueueSubmit2` sends together; the last batch signals the queue's
//! timeline, whose waiter runs the completion handlers and recycles the
//! resources (D6, D8). Render passes and profiling name the phase that
//! builds them.

// Stub bodies name the phase that builds them (design section 9); their
// parameters are unused until then.
#![allow(unused_variables)]

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ash::vk;
use ash::vk::{AccessFlags2 as A, Handle, PipelineStageFlags2 as S};

use super::core::Core;
use super::span_timer::SpanSlot;
use super::submit::{Batch, FrameResources, Keep, Queue, is_device_lost};
use super::tracker::Tracker;
use super::*;
use crate::*;

// Descriptor records are built as words; the layout word below holds a
// 32-bit enum in its low half.
const _: () = assert!(cfg!(target_endian = "little"));

pub struct GpuEncoder {
    core: Arc<Core>,
    queue: Arc<Queue>,
    label: Arc<str>,
    /// `None` only once submitted.
    res: Option<FrameResources>,
    /// The open command buffer.
    cmd: vk::CommandBuffer,
    /// Whether `cmd` holds a command yet.
    dirty: bool,
    /// Closed command buffers, in order, with their semaphore operations.
    batches: Vec<Batch>,
    /// Semaphore values the open command buffer waits for.
    waits: Vec<(vk::Semaphore, u64)>,
    tracker: Tracker,
    bound_pipeline: vk::Pipeline,
    bound_set: vk::DescriptorSet,
    handlers: RefCell<Vec<Box<dyn FnOnce() + Send>>>,
    scopes: Vec<String>,
    /// One dispatch's set layout and descriptor records: the set cache key.
    key: Vec<u64>,
    /// The open encode-replay span's cache (`replay.rs`).
    pub(crate) replay: Option<super::replay::GpuReplayCache>,
    /// Hears every submit's GPU start and end ([`Self::tap_gpu_time`]).
    tap: Option<GpuTimeTap>,
    /// The open [`GpuSpanTimer`] span every submit counts toward.
    pub(crate) span: Option<Arc<SpanSlot>>,
    /// The span the next submit ends.
    pub(crate) span_closing: Option<Arc<SpanSlot>>,
}

type GpuTimeTap = Arc<dyn Fn(f64, f64) + Send + Sync>;

/// One descriptor record: a `VkDescriptorBufferInfo` or
/// `VkDescriptorImageInfo` as three words.
type Record = [u64; 3];

fn buffer_record(buffer: vk::Buffer, offset: u64, range: u64) -> Record {
    [buffer.as_raw(), offset, range]
}

fn image_record(sampler: vk::Sampler, view: vk::ImageView, layout: vk::ImageLayout) -> Record {
    [
        sampler.as_raw(),
        view.as_raw(),
        layout.as_raw() as u32 as u64,
    ]
}

fn buffer_key(buffer: &GpuBuffer) -> usize {
    Arc::as_ptr(&buffer.inner) as usize
}

fn image_key(texture: &GpuTexture) -> usize {
    Arc::as_ptr(&texture.view.image) as usize
}

const OPEN: &str = "a submitted encoder records nothing";

/// A blocking wait that failed: dev and test builds panic so a GPU error is
/// loud and local; release builds log and carry on rather than crash a show.
fn verify(result: Result<(), String>) {
    if let Err(msg) = result {
        if cfg!(debug_assertions) {
            panic!("{msg}");
        }
        log::error!("{msg}");
    }
}

impl GpuEncoder {
    pub(crate) fn new(core: Arc<Core>, queue: Arc<Queue>, label: &str) -> Self {
        let res = FrameResources::take(&core);
        let mut encoder = Self {
            core,
            queue,
            label: label.into(),
            res: Some(res),
            cmd: vk::CommandBuffer::null(),
            dirty: false,
            batches: Vec::new(),
            waits: Vec::new(),
            tracker: Tracker::default(),
            bound_pipeline: vk::Pipeline::null(),
            bound_set: vk::DescriptorSet::null(),
            handlers: RefCell::new(Vec::new()),
            scopes: Vec::new(),
            key: Vec::new(),
            replay: None,
            tap: None,
            span: None,
            span_closing: None,
        };
        encoder.open();
        encoder
    }

    /// Begin a command buffer: a global barrier orders it after everything
    /// submitted before, so the tracker starts empty.
    fn open(&mut self) {
        let res = self.res.as_mut().expect(OPEN);
        self.cmd = res.begin_cmd(&self.core);
        self.core.global_barrier(self.cmd);
        self.dirty = false;
        self.tracker.reset();
        self.bound_pipeline = vk::Pipeline::null();
        self.bound_set = vk::DescriptorSet::null();
    }

    /// End the open command buffer as a batch that signals `signals`. Its
    /// writes become visible to the host once a signal is seen.
    fn close(&mut self, signals: Vec<(vk::Semaphore, u64)>) {
        let barrier = [vk::MemoryBarrier2::default()
            .src_stage_mask(S::ALL_COMMANDS)
            .src_access_mask(A::MEMORY_WRITE)
            .dst_stage_mask(S::HOST)
            .dst_access_mask(A::HOST_READ)];
        let device = &self.core.device;
        unsafe {
            device.cmd_pipeline_barrier2(
                self.cmd,
                &vk::DependencyInfo::default().memory_barriers(&barrier),
            );
            device.end_command_buffer(self.cmd)
        }
        .expect("Vulkan: end command buffer");
        self.batches.push(Batch {
            cmd: self.cmd,
            waits: std::mem::take(&mut self.waits),
            signals,
        });
    }

    fn keep(&mut self, keep: Keep) {
        self.res.as_mut().expect(OPEN).keep.push(keep);
    }

    /// The tracker's pending barrier, then the open command buffer for one
    /// more command.
    fn record(&mut self) -> vk::CommandBuffer {
        self.tracker.flush(&self.core.device, self.cmd);
        self.dirty = true;
        self.cmd
    }

    /// Track a transfer reading `reads` and writing `writes`, then the
    /// command buffer to record it in.
    fn transfer(&mut self, reads: &[usize], writes: &[usize]) -> vk::CommandBuffer {
        for &key in reads {
            self.tracker.read(key, S::ALL_TRANSFER);
        }
        for &key in writes {
            self.tracker.write(key, S::ALL_TRANSFER, A::TRANSFER_WRITE);
        }
        self.record()
    }

    fn keep_buffer(&mut self, buffer: &GpuBuffer) -> usize {
        self.keep(Keep::Buffer(buffer.inner.clone()));
        buffer_key(buffer)
    }

    fn keep_texture(&mut self, texture: &GpuTexture) -> usize {
        self.keep(Keep::View(texture.view.clone()));
        image_key(texture)
    }

    /// Bind `pipeline` and a descriptor set for `bindings`, tracking every
    /// access and keeping every resource alive until the submit completes.
    fn bind_compute(&mut self, pipeline: &GpuComputePipeline, bindings: &[GpuBinding]) {
        let inner = &pipeline.inner;
        let Self {
            core,
            res,
            tracker,
            key,
            ..
        } = self;
        let res = res.as_mut().expect(OPEN);
        key.clear();
        key.push(inner.set_layout.as_raw());
        for &(wgsl, slot) in &inner.slots {
            let binding = bindings
                .iter()
                .find(|b| b.binding() == wgsl)
                .unwrap_or_else(|| panic!("{}: @binding({wgsl}) is not bound", pipeline.label));
            let record = match (binding, slot.kind) {
                (
                    GpuBinding::Buffer { buffer, offset, .. },
                    SlotKind::UniformBuffer | SlotKind::StorageBuffer,
                ) => {
                    let range = if slot.kind == SlotKind::UniformBuffer {
                        (buffer.size.max(4) - offset)
                            .min(u64::from(core.limits.max_uniform_buffer_range))
                    } else {
                        vk::WHOLE_SIZE
                    };
                    if slot.writable {
                        tracker.write(
                            buffer_key(buffer),
                            S::COMPUTE_SHADER,
                            A::SHADER_STORAGE_READ | A::SHADER_STORAGE_WRITE,
                        );
                    } else {
                        tracker.read(buffer_key(buffer), S::COMPUTE_SHADER);
                    }
                    res.keep.push(Keep::Buffer(buffer.inner.clone()));
                    buffer_record(buffer.inner.raw, *offset, range)
                }
                (
                    GpuBinding::Bytes { data, .. },
                    SlotKind::UniformBuffer | SlotKind::StorageBuffer,
                ) => {
                    let len = (data.len() as u64).max(4);
                    let (block, offset) = res.ring_alloc(core, len);
                    // Safety: the ring range is fresh and `len` long.
                    unsafe { block.write(offset, data) };
                    buffer_record(block.inner.raw, offset, len)
                }
                (
                    GpuBinding::Texture { texture, .. },
                    SlotKind::SampledImage | SlotKind::StorageImage,
                ) => {
                    if slot.writable {
                        tracker.write(
                            image_key(texture),
                            S::COMPUTE_SHADER,
                            A::SHADER_STORAGE_READ | A::SHADER_STORAGE_WRITE,
                        );
                    } else {
                        tracker.read(image_key(texture), S::COMPUTE_SHADER);
                    }
                    res.keep.push(Keep::View(texture.view.clone()));
                    image_record(
                        vk::Sampler::null(),
                        texture.view.raw,
                        vk::ImageLayout::GENERAL,
                    )
                }
                (GpuBinding::Sampler { sampler, .. }, SlotKind::Sampler) => {
                    res.keep.push(Keep::Sampler(sampler.inner.clone()));
                    image_record(
                        sampler.inner.raw,
                        vk::ImageView::null(),
                        vk::ImageLayout::UNDEFINED,
                    )
                }
                _ => panic!(
                    "{}: @binding({wgsl}) is a {:?} slot",
                    pipeline.label, slot.kind
                ),
            };
            key.extend_from_slice(&record);
        }
        let set = if inner.template == vk::DescriptorUpdateTemplate::null() {
            None
        } else if let Some(&set) = res.sets.get(key.as_slice()) {
            Some(set)
        } else {
            let set = res.allocate_set(core, inner.set_layout);
            unsafe {
                core.device.update_descriptor_set_with_template(
                    set,
                    inner.template,
                    key[1..].as_ptr().cast(),
                )
            };
            res.sets.insert(key.as_slice().into(), set);
            Some(set)
        };
        if self.bound_pipeline != inner.raw {
            unsafe {
                self.core.device.cmd_bind_pipeline(
                    self.cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    inner.raw,
                )
            };
            self.bound_pipeline = inner.raw;
            self.bound_set = vk::DescriptorSet::null();
            self.keep(Keep::Pipeline(inner.clone()));
        }
        if let Some(set) = set
            && set != self.bound_set
        {
            unsafe {
                self.core.device.cmd_bind_descriptor_sets(
                    self.cmd,
                    vk::PipelineBindPoint::COMPUTE,
                    inner.layout,
                    0,
                    &[set],
                    &[],
                )
            };
            self.bound_set = set;
        }
    }

    /// Close the open command buffer and submit every batch; the queue's
    /// waiter runs the handlers and recycles the resources. `seconds`, when
    /// given, receives the submit's GPU seconds as `f64` bits.
    fn submit(&mut self, seconds: Option<Arc<AtomicU64>>) -> Result<u64, String> {
        self.stamp(seconds);
        self.close(Vec::new());
        let mut handlers = std::mem::take(self.handlers.get_mut());
        handlers.push(self.fault_handler());
        let res = self.res.take().expect(OPEN);
        let result = self
            .queue
            .submit(&self.core.device, &self.batches, handlers, res);
        self.batches.clear();
        if let Some(span) = self.span_closing.take() {
            span.closed();
        }
        result
    }

    /// Bracket this submit with timestamps when anything reads its GPU time:
    /// the tap, a span, or a wait returning it in `seconds`. The handler
    /// reads them before the waiter recycles the query pool. A span hears
    /// every submit, timed or not; one that failed counts as infinitely long.
    fn stamp(&mut self, seconds: Option<Arc<AtomicU64>>) {
        let tap = self.tap.clone();
        let spans = [self.span.clone(), self.span_closing.clone()];
        if tap.is_none() && seconds.is_none() && spans.iter().all(Option::is_none) {
            return;
        }
        let res = self.res.as_mut().expect(OPEN);
        let queries = res.queries;
        let timestamps = self.core.timestamps;
        if timestamps.is_some() {
            let start = res.begin_cmd(&self.core);
            let device = &self.core.device;
            unsafe {
                device.cmd_reset_query_pool(start, queries, 0, 2);
                device.cmd_write_timestamp2(start, S::ALL_COMMANDS, queries, 0);
                device.end_command_buffer(start)
            }
            .expect("Vulkan: end timestamp command buffer");
            self.batches.insert(
                0,
                Batch {
                    cmd: start,
                    waits: Vec::new(),
                    signals: Vec::new(),
                },
            );
            unsafe { device.cmd_write_timestamp2(self.cmd, S::ALL_COMMANDS, queries, 1) };
        }
        for span in spans.iter().flatten() {
            span.submitted();
        }
        let core = self.core.clone();
        self.handlers.get_mut().push(Box::new(move || {
            let times = timestamps.and_then(|(tick_ns, mask)| {
                let mut stamps = [0u64; 2];
                unsafe {
                    core.device.get_query_pool_results(
                        queries,
                        0,
                        &mut stamps,
                        vk::QueryResultFlags::TYPE_64,
                    )
                }
                .ok()?;
                let to_seconds = |ticks: u64| (ticks & mask) as f64 * tick_ns * 1e-9;
                let start = to_seconds(stamps[0]);
                Some((start, start + to_seconds(stamps[1].wrapping_sub(stamps[0]))))
            });
            if let Some((start, end)) = times.filter(|_| !is_device_lost()) {
                if let Some(tap) = &tap {
                    tap(start, end);
                }
                if let Some(seconds) = &seconds {
                    seconds.store((end - start).to_bits(), Ordering::Relaxed);
                }
            }
            let failed = is_device_lost() || (timestamps.is_some() && times.is_none());
            let span_seconds = match times {
                _ if failed => f64::INFINITY,
                Some((start, end)) => end - start,
                None => 0.0,
            };
            for span in spans.iter().flatten() {
                span.finished(span_seconds);
            }
        }));
    }

    /// Submit, then block until the waiter has finished with the submit:
    /// the GPU is done and every completion handler ran. `seconds` as in
    /// [`Self::submit`].
    fn submit_and_wait(
        &mut self,
        context: &str,
        seconds: Option<Arc<AtomicU64>>,
    ) -> Result<(), String> {
        let value = self.submit(seconds)?;
        self.queue.wait_retired(value);
        if is_device_lost() {
            return Err(format!(
                "[GPU] {context}: command buffer '{}' did not complete: device lost",
                self.label
            ));
        }
        Ok(())
    }

    /// A fresh command buffer from fresh resources after a submit.
    fn restart(&mut self) {
        self.res = Some(FrameResources::take(&self.core));
        self.open();
    }

    /// The completion handler every submit carries: on a lost device it logs
    /// the buffer and the scopes encoded into it.
    fn fault_handler(&mut self) -> Box<dyn FnOnce() + Send> {
        let scopes = std::mem::take(&mut self.scopes);
        let label = self.label.clone();
        if crate::gpu_fault::diagnostics_enabled() {
            log::info!("[GPU-DIAG] submitting buffer={label} scopes={scopes:?}");
        }
        crate::gpu_fault::begin_submission();
        Box::new(move || {
            if is_device_lost() {
                crate::gpu_fault::emit_diagnostic(format_args!(
                    "[GPU] Command buffer '{label}' error: device lost — encoded scopes: {}",
                    scopes.join(" | ")
                ));
            }
            crate::gpu_fault::complete_submission();
        })
    }

    /// `data` in this submit's ring: the block and the offset.
    pub(super) fn ring_upload(&mut self, data: &[u8]) -> (GpuBuffer, u64) {
        let res = self.res.as_mut().expect(OPEN);
        let (block, offset) = res.ring_alloc(&self.core, (data.len() as u64).max(4));
        // Safety: the ring range is fresh and at least `data.len()` long.
        unsafe { block.write(offset, data) };
        (block, offset)
    }

    /// Signal `semaphore` at `value` once everything recorded so far is done.
    fn signal(&mut self, semaphore: vk::Semaphore, value: u64) {
        if !self.dirty
            && self.waits.is_empty()
            && let Some(last) = self.batches.last_mut()
        {
            last.signals.push((semaphore, value));
            return;
        }
        self.close(vec![(semaphore, value)]);
        self.open();
    }
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
        self.bind_compute(pipeline, bindings);
        let cmd = self.record();
        let [x, y, z] = workgroups;
        unsafe { self.core.device.cmd_dispatch(cmd, x, y, z) };
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
        self.bind_compute(pipeline, bindings);
        let key = self.keep_buffer(args);
        self.tracker.read(key, S::DRAW_INDIRECT);
        let cmd = self.record();
        unsafe {
            self.core
                .device
                .cmd_dispatch_indirect(cmd, args.inner.raw, offset)
        };
    }

    /// Insert a buffer-scope memory barrier on the active compute encoder.
    pub fn compute_memory_barrier_buffers(&mut self) {
        let barrier = [vk::MemoryBarrier2::default()
            .src_stage_mask(S::COMPUTE_SHADER)
            .src_access_mask(A::SHADER_STORAGE_WRITE)
            .dst_stage_mask(S::COMPUTE_SHADER)
            .dst_access_mask(A::SHADER_STORAGE_READ | A::SHADER_STORAGE_WRITE)];
        let cmd = self.record();
        unsafe {
            self.core.device.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().memory_barriers(&barrier),
            )
        };
        self.tracker.compute_barrier_recorded();
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
        let key = self.keep_texture(texture);
        let cmd = self.transfer(&[], &[key]);
        let view = &texture.view;
        let device = &self.core.device;
        if texture.format == GpuTextureFormat::Depth32Float {
            let value = vk::ClearDepthStencilValue {
                depth: r as f32,
                stencil: 0,
            };
            unsafe {
                device.cmd_clear_depth_stencil_image(
                    cmd,
                    view.image.raw,
                    vk::ImageLayout::GENERAL,
                    &value,
                    &[view.range()],
                )
            };
            return;
        }
        let value = if texture.format == GpuTextureFormat::R32Uint {
            vk::ClearColorValue {
                uint32: [r as u32, g as u32, b as u32, a as u32],
            }
        } else {
            vk::ClearColorValue {
                float32: [r as f32, g as f32, b as f32, a as f32],
            }
        };
        unsafe {
            device.cmd_clear_color_image(
                cmd,
                view.image.raw,
                vk::ImageLayout::GENERAL,
                &value,
                &[view.range()],
            )
        };
    }

    /// Copy a single-sample depth texture into an R32Float storage texture.
    pub fn copy_depth_to_float(&mut self, source: &GpuTexture, target: &GpuTexture) {
        todo!("vulkan P2")
    }

    /// Fill a buffer with zeros via blit encoder.
    pub fn clear_buffer(&mut self, buffer: &GpuBuffer) {
        let key = self.keep_buffer(buffer);
        let cmd = self.transfer(&[], &[key]);
        unsafe {
            self.core
                .device
                .cmd_fill_buffer(cmd, buffer.inner.raw, 0, vk::WHOLE_SIZE, 0)
        };
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
        assert_eq!(
            src.format, dst.format,
            "copy_texture_to_texture: pixel format mismatch — src {:?}, dst {:?}",
            src.format, dst.format,
        );
        assert!(
            src.width == dst.width && src.height == dst.height,
            "copy_texture_to_texture is a same-size copy (origin 0, no scaling) — src {}×{} != dst {}×{}. \
             To change resolution, sample: use GpuEncoder::resize_sample.",
            src.width,
            src.height,
            dst.width,
            dst.height,
        );
        assert!(
            width <= src.width && height <= src.height,
            "copy_texture_to_texture: copy region {width}×{height} exceeds the {}×{} textures",
            src.width,
            src.height,
        );
        let src_key = self.keep_texture(src);
        let dst_key = self.keep_texture(dst);
        let cmd = self.transfer(&[src_key], &[dst_key]);
        let region = [vk::ImageCopy::default()
            .src_subresource(subresource(src))
            .dst_subresource(subresource(dst))
            .extent(copy_extent(src, width, height, depth))];
        unsafe {
            self.core.device.cmd_copy_image(
                cmd,
                src.view.image.raw,
                vk::ImageLayout::GENERAL,
                dst.view.image.raw,
                vk::ImageLayout::GENERAL,
                &region,
            )
        };
    }

    /// Copy `size` bytes from `src` buffer to `dst` buffer via blit
    /// encoder.
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
        self.copy_buffer(src, 0, dst, 0, size);
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
        assert!(
            src_offset.is_multiple_of(4) && dst_offset.is_multiple_of(4) && size.is_multiple_of(4),
            "copy_buffer_range: offsets {src_offset}, {dst_offset} and size {size} must be multiples of 4",
        );
        assert!(
            src_offset + size <= src.size,
            "copy_buffer_range: {size} bytes at {src_offset} exceed source buffer ({} bytes)",
            src.size,
        );
        assert!(
            dst_offset + size <= dst.size,
            "copy_buffer_range: {size} bytes at {dst_offset} exceed destination buffer ({} bytes)",
            dst.size,
        );
        self.copy_buffer(src, src_offset, dst, dst_offset, size);
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
        assert!(
            width <= src.width && height <= src.height,
            "copy_texture_to_buffer: copy region {width}×{height} exceeds source {}×{} ({:?})",
            src.width,
            src.height,
            src.format,
        );
        self.copy_image_to_buffer(src, dst, [width, height, 1], bytes_per_row);
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
        assert!(
            width <= src.width && height <= src.height && depth <= src.depth,
            "copy_texture_3d_to_buffer: copy region {width}×{height}×{depth} exceeds source {}×{}×{} ({:?})",
            src.width,
            src.height,
            src.depth,
            src.format,
        );
        self.copy_image_to_buffer(src, dst, [width, height, depth], bytes_per_row);
    }

    /// Insert a pipeline barrier between dispatches.
    pub fn pipeline_barrier(&mut self, reads: &[&GpuTexture], writes: &[&GpuTexture]) {
        // The hazard tracker places every barrier this asks for.
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
        let view = &texture.view;
        assert!(
            view.image.usage.contains(GpuTextureUsage::CPU_UPLOAD),
            "texture CPU upload requires CPU_UPLOAD usage (CPU-visible storage)"
        );
        let bytes =
            u64::from(width) * u64::from(height) * u64::from(texture.format.bytes_per_pixel());
        assert!(
            data.len() as u64 >= bytes,
            "upload_texture: {} bytes for a {width}x{height} {:?} region",
            data.len(),
            texture.format
        );
        let (staging, offset) = self
            .res
            .as_mut()
            .expect(OPEN)
            .ring_alloc(&self.core, bytes.max(4));
        // Safety: the ring range is fresh and `bytes` long.
        unsafe { staging.write(offset, &data[..bytes as usize]) };
        let key = self.keep_texture(texture);
        let cmd = self.transfer(&[], &[key]);
        let region = [vk::BufferImageCopy::default()
            .buffer_offset(offset)
            .image_subresource(subresource(texture))
            .image_extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })];
        unsafe {
            self.core.device.cmd_copy_buffer_to_image(
                cmd,
                staging.inner.raw,
                view.image.raw,
                vk::ImageLayout::GENERAL,
                &region,
            )
        };
    }

    /// Signal a shared event on the GPU timeline.
    pub fn signal_event(&mut self, event: &GpuEvent) {
        let value = event.inner.counter.fetch_add(1, Ordering::Relaxed) + 1;
        self.signal_event_value(event, value);
    }

    /// Signal a shared event with a specific value (does NOT auto-increment).
    pub fn signal_event_value(&mut self, event: &GpuEvent, value: u64) {
        self.keep(Keep::Event(event.inner.clone()));
        self.signal(event.inner.semaphore, value);
    }

    /// Wait for a shared event to reach a specific value before executing
    /// subsequent GPU work on this command buffer.
    pub fn wait_event(&mut self, event: &GpuEvent, value: u64) {
        self.keep(Keep::Event(event.inner.clone()));
        if self.dirty {
            self.close(Vec::new());
            self.open();
        }
        self.waits.push((event.inner.semaphore, value));
    }

    /// Encode what follows on the device's second queue, beside the main
    /// lane's work.
    pub fn use_async_lane(&mut self, device: &GpuDevice, handoff: &GpuEvent) -> bool {
        // One lane: the device keeps all work on its main queue for now, and
        // callers stay on the main lane when this refuses.
        false
    }

    /// Encode on the main queue again.
    pub fn use_main_lane(&mut self) {
        // Never left: `use_async_lane` refuses.
    }

    /// Signal `handoff` on the async lane and commit its buffer, never
    /// blocking, and remember the value.
    pub fn finish_async_lane(&mut self, handoff: &GpuEvent) {
        // No async lane was opened: `use_async_lane` refuses.
    }

    /// The main lane waits, on the GPU, for the value the last
    /// [`Self::finish_async_lane`] remembered: work encoded after this runs
    /// after everything the async lane did.
    pub fn wait_async_lane(&mut self, handoff: &GpuEvent) {
        // No async lane was opened: `use_async_lane` refuses.
    }

    /// GPU proofs: how many async lanes this thread has finished.
    #[cfg(feature = "gpu-proofs")]
    pub fn async_lanes_on_this_thread() -> u64 {
        0
    }

    /// GPU proofs: how many main-to-async handoffs this thread has made.
    #[cfg(feature = "gpu-proofs")]
    pub fn handoffs_on_this_thread() -> u64 {
        0
    }

    /// Register a callback to run when the GPU finishes executing this command buffer.
    pub fn add_completed_handler<F: Fn() + Send + 'static>(&self, callback: F) {
        self.handlers.borrow_mut().push(Box::new(callback));
    }

    /// Register a diagnostic completed handler that logs GPU errors.
    pub fn add_completed_handler_with_status(&self, label: &str) {
        let label = label.to_string();
        crate::gpu_fault::begin_submission();
        self.handlers.borrow_mut().push(Box::new(move || {
            if is_device_lost() {
                log::error!("[GPU] Command buffer '{label}' error: device lost");
            }
            crate::gpu_fault::complete_submission();
        }));
    }

    /// Record a human-readable scope (card, layer, pass) against this
    /// encoder.
    pub fn note_scope(&mut self, scope: &str) {
        if crate::gpu_fault::diagnostics_enabled() {
            log::info!("[GPU-DIAG] scope buffer={:?} {scope}", self.label);
        }
        self.scopes.push(scope.to_string());
    }

    /// Report the GPU start and end time (seconds, the device's clock) of
    /// every submit this encoder makes to `tap`, however it commits: the
    /// final one and each chunk a `commit_and_continue` split off. Each
    /// tapped submit carries two timestamps.
    pub fn tap_gpu_time(&mut self, tap: std::sync::Arc<dyn Fn(f64, f64) + Send + Sync>) {
        self.tap = Some(tap);
    }

    /// Commit the command buffer to the GPU queue.
    pub fn commit(mut self) {
        if let Err(e) = self.submit(None) {
            log::error!("{e}");
        }
    }

    /// Commit the current command buffer to the GPU queue and continue
    /// encoding into a fresh command buffer from the same queue.
    pub fn commit_and_continue(&mut self, device: &GpuDevice) {
        if let Err(e) = self.submit(None) {
            log::error!("{e}");
        }
        self.restart();
    }

    /// [`Self::commit_and_continue`], then block until the GPU has completed
    /// everything committed so far.
    pub fn commit_wait_and_continue(&mut self, device: &GpuDevice) {
        let result = self.submit_and_wait("commit_wait_and_continue", None);
        self.restart();
        verify(result);
    }

    /// Commit and block until the GPU has completed the work and every
    /// completion handler ran. `Ok` holds this submit's GPU seconds (0 when
    /// the device has no timestamps). `Err` on a lost device (D19): the
    /// caller skips whatever needed the result.
    pub fn commit_and_wait(mut self) -> Result<f64, String> {
        let seconds = Arc::new(AtomicU64::new(0));
        self.submit_and_wait("commit_and_wait", Some(seconds.clone()))?;
        Ok(f64::from_bits(seconds.load(Ordering::Relaxed)))
    }

    fn copy_buffer(
        &mut self,
        src: &GpuBuffer,
        src_offset: u64,
        dst: &GpuBuffer,
        dst_offset: u64,
        size: u64,
    ) {
        if size == 0 {
            return;
        }
        let src_key = self.keep_buffer(src);
        let dst_key = self.keep_buffer(dst);
        let cmd = self.transfer(&[src_key], &[dst_key]);
        let region = [vk::BufferCopy {
            src_offset,
            dst_offset,
            size,
        }];
        unsafe {
            self.core
                .device
                .cmd_copy_buffer(cmd, src.inner.raw, dst.inner.raw, &region)
        };
    }

    /// Rows of `bytes_per_row` bytes, slices of `bytes_per_row × height`.
    fn copy_image_to_buffer(
        &mut self,
        src: &GpuTexture,
        dst: &GpuBuffer,
        size: [u32; 3],
        bytes_per_row: u32,
    ) {
        let [width, height, depth] = size;
        let bpp = src.format.bytes_per_pixel();
        assert!(
            bytes_per_row.is_multiple_of(bpp) && bytes_per_row / bpp >= width,
            "copy to buffer: {bytes_per_row} bytes per row for {width} {:?} pixels",
            src.format,
        );
        let required = u64::from(bytes_per_row) * u64::from(height) * u64::from(depth);
        assert!(
            required <= dst.size,
            "copy to buffer: destination buffer too small — needed {required} bytes, dst buffer is {} bytes",
            dst.size,
        );
        let src_key = self.keep_texture(src);
        let dst_key = self.keep_buffer(dst);
        let cmd = self.transfer(&[src_key], &[dst_key]);
        let region = [vk::BufferImageCopy::default()
            .buffer_row_length(bytes_per_row / bpp)
            .buffer_image_height(height)
            .image_subresource(subresource(src))
            .image_extent(copy_extent(src, width, height, depth))];
        unsafe {
            self.core.device.cmd_copy_image_to_buffer(
                cmd,
                src.view.image.raw,
                vk::ImageLayout::GENERAL,
                dst.inner.raw,
                &region,
            )
        };
    }
}

/// The first mip and layer a texture's view sees.
fn subresource(texture: &GpuTexture) -> vk::ImageSubresourceLayers {
    let view = &texture.view;
    vk::ImageSubresourceLayers {
        aspect_mask: view.image.aspect,
        mip_level: view.base_mip,
        base_array_layer: view.base_layer,
        layer_count: 1,
    }
}

/// A copy's extent: `depth` slices of a volume, one layer of anything else,
/// as Metal copies slice 0.
fn copy_extent(texture: &GpuTexture, width: u32, height: u32, depth: u32) -> vk::Extent3D {
    let volume = texture.view.layer_count == 1 && texture.depth > 1;
    vk::Extent3D {
        width,
        height,
        depth: if volume { depth } else { 1 },
    }
}

impl Drop for GpuEncoder {
    fn drop(&mut self) {
        // Never submitted: nothing on the GPU references the resources.
        if let Some(res) = self.res.take() {
            res.recycle(&self.core);
        }
    }
}
