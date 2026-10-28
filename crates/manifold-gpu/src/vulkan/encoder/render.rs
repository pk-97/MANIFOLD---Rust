//! Render passes on dynamic rendering (P2). Images stay in `GENERAL`: a pass
//! registers every access of its attachments and draws, records one
//! barrier, then begins rendering. A negative viewport height flips clip
//! space to Metal's, so front faces wind counter-clockwise on both.

use super::*;
use crate::vulkan::pipeline::{RenderCore, Variant};

/// Colour targets one pass can have: the colour and its auxiliary targets.
const MAX_COLOR_TARGETS: usize = 4;

/// Stages that read a draw's bindings: both, as on Metal.
const DRAW_STAGES: S = S::from_raw(S::VERTEX_SHADER.as_raw() | S::FRAGMENT_SHADER.as_raw());

/// The open long-lived pass: its target, so a draw that needs a barrier can
/// pause rendering around it.
pub(super) struct UiPass {
    view: vk::ImageView,
    extent: (u32, u32),
}

fn load_op(load: GpuLoadAction) -> vk::AttachmentLoadOp {
    match load {
        GpuLoadAction::Clear => vk::AttachmentLoadOp::CLEAR,
        GpuLoadAction::Load => vk::AttachmentLoadOp::LOAD,
        GpuLoadAction::DontCare => vk::AttachmentLoadOp::DONT_CARE,
    }
}

fn color_attachment(
    view: vk::ImageView,
    resolve: Option<vk::ImageView>,
    load: GpuLoadAction,
    clear: [f32; 4],
) -> vk::RenderingAttachmentInfo<'static> {
    let info = vk::RenderingAttachmentInfo::default()
        .image_view(view)
        .image_layout(vk::ImageLayout::GENERAL)
        .load_op(load_op(load))
        .clear_value(vk::ClearValue {
            color: vk::ClearColorValue { float32: clear },
        });
    match resolve {
        // Metal's multisample resolve keeps only the resolved image.
        Some(resolve) => info
            .store_op(vk::AttachmentStoreOp::DONT_CARE)
            .resolve_mode(vk::ResolveModeFlags::AVERAGE)
            .resolve_image_view(resolve)
            .resolve_image_layout(vk::ImageLayout::GENERAL),
        None => info.store_op(vk::AttachmentStoreOp::STORE),
    }
}

/// The viewport with Metal's clip space: y up, so a negative height.
fn flipped_viewport((x, y, w, h): (f32, f32, f32, f32)) -> vk::Viewport {
    vk::Viewport {
        x,
        y: y + h,
        width: w,
        height: -h,
        min_depth: 0.0,
        max_depth: 1.0,
    }
}

fn whole(width: u32, height: u32) -> vk::Rect2D {
    vk::Rect2D {
        offset: vk::Offset2D { x: 0, y: 0 },
        extent: vk::Extent2D { width, height },
    }
}

impl GpuEncoder {
    /// The fill a draw gets: wireframe only where the device has it (boot
    /// logs when it does not).
    fn fill_on_device(&self, fill: GpuTriangleFillMode) -> GpuTriangleFillMode {
        if fill == GpuTriangleFillMode::Lines && !self.core.fill_mode_non_solid {
            return GpuTriangleFillMode::Fill;
        }
        fill
    }

    /// Track the buffers a draw's count and vertices read.
    fn track_draw_inputs(&mut self, count: &DrawCount, vertices: Option<(&GpuBuffer, u64)>) {
        let (indices, args) = match *count {
            DrawCount::Direct { .. } => (None, None),
            DrawCount::Indirect { args, .. } => (None, Some(args)),
            DrawCount::IndexedDirect { indices, .. } => (Some(indices), None),
            DrawCount::IndexedIndirect { indices, args, .. } => (Some(indices), Some(args)),
        };
        if let Some(args) = args {
            let key = self.keep_buffer(args);
            self.tracker.read(key, S::DRAW_INDIRECT);
        }
        if let Some(indices) = indices {
            let key = self.keep_buffer(indices);
            self.tracker.read(key, S::INDEX_INPUT);
        }
        if let Some((buffer, _)) = vertices {
            let key = self.keep_buffer(buffer);
            self.tracker.read(key, S::VERTEX_ATTRIBUTE_INPUT);
        }
    }

    /// Track a colour attachment and its resolve target.
    fn track_color_target(&mut self, texture: &GpuTexture, resolve: Option<&GpuTexture>) {
        let key = self.keep_texture(texture);
        self.tracker.write(
            key,
            S::COLOR_ATTACHMENT_OUTPUT,
            A::COLOR_ATTACHMENT_READ | A::COLOR_ATTACHMENT_WRITE,
        );
        if let Some(resolve) = resolve {
            let key = self.keep_texture(resolve);
            self.tracker
                .write(key, S::COLOR_ATTACHMENT_OUTPUT, A::COLOR_ATTACHMENT_WRITE);
        }
    }

    /// A draw's descriptor set, its accesses tracked; `None` when the draw
    /// is skipped: empty, or refused for bindings that do not fit.
    fn draw_set(&mut self, draw: &Draw) -> Option<Option<vk::DescriptorSet>> {
        if draw.count.is_empty() {
            return None;
        }
        let pipeline = draw.pipeline;
        match self.descriptor_set(
            &pipeline.inner.layouts,
            &pipeline.label,
            draw.bindings,
            DRAW_STAGES,
        ) {
            Ok(set) => {
                self.track_draw_inputs(&draw.count, draw.vertices);
                Some(set)
            }
            Err(msg) => {
                refuse(msg);
                None
            }
        }
    }

    /// Bind `inner`'s pipeline for `variant` and `set`.
    fn bind_graphics(
        &mut self,
        inner: &Arc<RenderCore>,
        variant: Variant,
        set: Option<vk::DescriptorSet>,
    ) {
        let raw = inner.pipeline(variant);
        let device = &self.core.device;
        if self.bound_pipeline != raw {
            unsafe { device.cmd_bind_pipeline(self.cmd, vk::PipelineBindPoint::GRAPHICS, raw) };
            self.bound_pipeline = raw;
            self.bound_set = vk::DescriptorSet::null();
            self.keep(Keep::Render(inner.clone()));
        }
        if let Some(set) = set
            && set != self.bound_set
        {
            unsafe {
                self.core.device.cmd_bind_descriptor_sets(
                    self.cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    inner.layouts.layout,
                    0,
                    &[set],
                    &[],
                )
            };
            self.bound_set = set;
        }
    }

    /// Bind and record one draw. A depth-only pass always fills triangles.
    fn record_draw(&mut self, draw: &Draw, set: Option<vk::DescriptorSet>, depth_only: bool) {
        let variant = if depth_only {
            (GpuPrimitiveType::Triangle, GpuTriangleFillMode::Fill)
        } else {
            (draw.primitive, self.fill_on_device(draw.fill_mode))
        };
        self.bind_graphics(&draw.pipeline.inner, variant, set);
        let device = &self.core.device;
        let cmd = self.cmd;
        let (constant, slope, clamp) = draw.depth_bias.unwrap_or((0.0, 0.0, 0.0));
        unsafe {
            device.cmd_set_cull_mode(
                cmd,
                if draw.cull_back {
                    vk::CullModeFlags::BACK
                } else {
                    vk::CullModeFlags::NONE
                },
            );
            device.cmd_set_depth_bias_enable(cmd, draw.depth_bias.is_some());
            device.cmd_set_depth_bias(cmd, constant, clamp, slope);
            if let Some((buffer, offset)) = draw.vertices {
                device.cmd_bind_vertex_buffers(cmd, 0, &[buffer.inner.raw], &[offset]);
            }
            match draw.count {
                DrawCount::Direct {
                    vertices,
                    instances,
                } => device.cmd_draw(cmd, vertices, instances, 0, 0),
                DrawCount::Indirect { args, offset } => {
                    device.cmd_draw_indirect(cmd, args.inner.raw, offset, 1, 16)
                }
                DrawCount::IndexedDirect {
                    indices,
                    index_count,
                    instances,
                } => {
                    device.cmd_bind_index_buffer(cmd, indices.inner.raw, 0, vk::IndexType::UINT32);
                    device.cmd_draw_indexed(cmd, index_count, instances, 0, 0, 0);
                }
                DrawCount::IndexedIndirect {
                    indices,
                    args,
                    offset,
                } => {
                    device.cmd_bind_index_buffer(cmd, indices.inner.raw, 0, vk::IndexType::UINT32);
                    device.cmd_draw_indexed_indirect(cmd, args.inner.raw, offset, 1, 20);
                }
            }
        }
    }

    /// Depth test, write and compare for `state`; none without a depth
    /// target. Dynamic state must be set before every draw's first use.
    fn set_depth_state(&self, state: Option<&GpuDepthStencilState>) {
        let device = &self.core.device;
        unsafe {
            device.cmd_set_depth_test_enable(self.cmd, state.is_some());
            device.cmd_set_depth_write_enable(self.cmd, state.is_some_and(|s| s.write));
            device.cmd_set_depth_compare_op(
                self.cmd,
                state.map_or(vk::CompareOp::ALWAYS, |s| s.compare),
            );
        }
    }

    /// Run `draws` in one render pass described by `pass`, then the pass's
    /// second group after switching its depth state.
    pub fn draw(&mut self, pass: &DrawPass, draws: &[Draw], label: &str) {
        let second: &[Draw] = pass.second_group.map_or(&[], |(_, d)| d);
        let mut sets = std::mem::take(&mut self.draw_sets);
        sets.clear();
        for draw in draws.iter().chain(second) {
            let set = self.draw_set(draw);
            sets.push(set);
        }
        if let Some(color) = &pass.color {
            self.track_color_target(color.texture, color.resolve);
        }
        for aux in pass.aux_color {
            self.track_color_target(aux.msaa, Some(aux.resolve));
        }
        let depth = pass.depth.as_ref().map(|target| {
            let key = self.keep_texture(target.texture);
            self.tracker.write(
                key,
                S::EARLY_FRAGMENT_TESTS | S::LATE_FRAGMENT_TESTS,
                A::DEPTH_STENCIL_ATTACHMENT_READ | A::DEPTH_STENCIL_ATTACHMENT_WRITE,
            );
            let info = vk::RenderingAttachmentInfo::default()
                .image_view(target.texture.view.raw)
                .image_layout(vk::ImageLayout::GENERAL)
                .load_op(load_op(target.load))
                .clear_value(vk::ClearValue {
                    depth_stencil: vk::ClearDepthStencilValue {
                        depth: target.state.clear_depth,
                        stencil: 0,
                    },
                });
            match target.store {
                DepthStore::Store => info.store_op(vk::AttachmentStoreOp::STORE),
                DepthStore::Discard => info.store_op(vk::AttachmentStoreOp::DONT_CARE),
                DepthStore::Resolve(resolve) => {
                    // Resolves write in the colour output stage, depth too.
                    let key = self.keep_texture(resolve);
                    self.tracker.write(
                        key,
                        S::COLOR_ATTACHMENT_OUTPUT | S::LATE_FRAGMENT_TESTS,
                        A::COLOR_ATTACHMENT_WRITE | A::DEPTH_STENCIL_ATTACHMENT_WRITE,
                    );
                    info.store_op(vk::AttachmentStoreOp::DONT_CARE)
                        .resolve_mode(vk::ResolveModeFlags::SAMPLE_ZERO)
                        .resolve_image_view(resolve.view.raw)
                        .resolve_image_layout(vk::ImageLayout::GENERAL)
                }
            }
        });
        let mut colors = [vk::RenderingAttachmentInfo::default(); MAX_COLOR_TARGETS];
        let mut count = 0;
        if let Some(color) = &pass.color {
            colors[0] = color_attachment(
                color.texture.view.raw,
                color.resolve.map(|r| r.view.raw),
                color.load,
                [0.0; 4],
            );
            count = 1;
        }
        for aux in pass.aux_color {
            colors[count] = color_attachment(
                aux.msaa.view.raw,
                Some(aux.resolve.view.raw),
                GpuLoadAction::Clear,
                aux.clear.map(|c| c as f32),
            );
            count += 1;
        }
        let (width, height) = pass.size();
        let mut info = vk::RenderingInfo::default()
            .render_area(whole(width, height))
            .layer_count(1)
            .color_attachments(&colors[..count]);
        if let Some(depth) = &depth {
            info = info.depth_attachment(depth);
        }
        let cmd = self.record();
        let viewport = pass
            .viewport
            .unwrap_or((0.0, 0.0, width as f32, height as f32));
        let device = &self.core.device;
        unsafe {
            device.cmd_begin_rendering(cmd, &info);
            device.cmd_set_viewport(cmd, 0, &[flipped_viewport(viewport)]);
            device.cmd_set_scissor(cmd, 0, &[whole(width, height)]);
            device.cmd_set_front_face(cmd, vk::FrontFace::COUNTER_CLOCKWISE);
        }
        self.set_depth_state(pass.depth.as_ref().map(|d| d.state));
        let depth_only = pass.color.is_none();
        for (draw, set) in draws.iter().zip(&sets) {
            if let Some(set) = *set {
                self.record_draw(draw, set, depth_only);
            }
        }
        if let Some((state, _)) = pass.second_group {
            self.set_depth_state(Some(state));
            for (draw, set) in second.iter().zip(&sets[draws.len()..]) {
                if let Some(set) = *set {
                    self.record_draw(draw, set, depth_only);
                }
            }
        }
        unsafe { self.core.device.cmd_end_rendering(cmd) };
        self.draw_sets = sets;
    }

    fn begin_ui_rendering(&self, view: vk::ImageView, extent: (u32, u32), load: GpuLoadAction) {
        let colors = [color_attachment(view, None, load, [0.0; 4])];
        let info = vk::RenderingInfo::default()
            .render_area(whole(extent.0, extent.1))
            .layer_count(1)
            .color_attachments(&colors);
        unsafe { self.core.device.cmd_begin_rendering(self.cmd, &info) };
    }

    /// Begin a render pass that stays alive across multiple draw calls.
    pub fn begin_render_pass(
        &mut self,
        target: &GpuTexture,
        load_action: GpuLoadAction,
        label: &str,
    ) {
        self.track_color_target(target, None);
        let cmd = self.record();
        let extent = (target.width, target.height);
        self.begin_ui_rendering(target.view.raw, extent, load_action);
        let device = &self.core.device;
        unsafe {
            device.cmd_set_viewport(
                cmd,
                0,
                &[flipped_viewport((
                    0.0,
                    0.0,
                    extent.0 as f32,
                    extent.1 as f32,
                ))],
            );
            device.cmd_set_scissor(cmd, 0, &[whole(extent.0, extent.1)]);
            device.cmd_set_front_face(cmd, vk::FrontFace::COUNTER_CLOCKWISE);
            device.cmd_set_cull_mode(cmd, vk::CullModeFlags::NONE);
            device.cmd_set_depth_bias_enable(cmd, false);
            device.cmd_set_depth_bias(cmd, 0.0, 0.0, 0.0);
        }
        self.set_depth_state(None);
        self.ui_pass = Some(UiPass {
            view: target.view.raw,
            extent,
        });
    }

    /// Draw indexed geometry within an active render pass. A draw reading
    /// something written earlier in the command buffer pauses rendering
    /// around the barrier it needs.
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
        let (view, extent) = self
            .ui_pass
            .as_ref()
            .map(|p| (p.view, p.extent))
            .expect("draw_in_render_pass called without active render pass");
        let set = match self.descriptor_set(
            &pipeline.inner.layouts,
            &pipeline.label,
            bindings,
            DRAW_STAGES,
        ) {
            Ok(set) => set,
            Err(msg) => return refuse(msg),
        };
        let count = DrawCount::IndexedDirect {
            indices: index_buffer,
            index_count,
            instances: 1,
        };
        self.track_draw_inputs(&count, Some((vertex_buffer, vertex_offset)));
        if self.tracker.pending() {
            unsafe { self.core.device.cmd_end_rendering(self.cmd) };
            self.tracker.flush(&self.core.device, self.cmd);
            self.begin_ui_rendering(view, extent, GpuLoadAction::Load);
        }
        self.bind_graphics(
            &pipeline.inner,
            (GpuPrimitiveType::Triangle, GpuTriangleFillMode::Fill),
            set,
        );
        let device = &self.core.device;
        let cmd = self.cmd;
        unsafe {
            if let Some(viewport) = viewport {
                device.cmd_set_viewport(cmd, 0, &[flipped_viewport(viewport)]);
            }
            device.cmd_bind_vertex_buffers(cmd, 0, &[vertex_buffer.inner.raw], &[vertex_offset]);
            device.cmd_bind_index_buffer(
                cmd,
                index_buffer.inner.raw,
                index_buffer_offset,
                vk::IndexType::UINT32,
            );
            device.cmd_draw_indexed(cmd, index_count, 1, 0, 0, 0);
        }
    }

    /// Set the scissor rectangle on the active render pass.
    pub fn set_scissor_rect(&mut self, x: u32, y: u32, w: u32, h: u32) {
        assert!(
            self.ui_pass.is_some(),
            "set_scissor_rect called without active render pass"
        );
        let rect = vk::Rect2D {
            offset: vk::Offset2D {
                x: x as i32,
                y: y as i32,
            },
            extent: vk::Extent2D {
                width: w,
                height: h,
            },
        };
        unsafe { self.core.device.cmd_set_scissor(self.cmd, 0, &[rect]) };
    }

    /// End the active render pass.
    pub fn end_render_pass(&mut self) {
        if self.ui_pass.take().is_some() {
            unsafe { self.core.device.cmd_end_rendering(self.cmd) };
        }
    }
}
