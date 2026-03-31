use manifold_ui::node::Rect;
use manifold_ui::tree::UITree;

use crate::panel_cache::PanelCacheTexture;
use crate::panel_compositor::PanelCompositor;
use crate::ui_renderer::{TextMode, UIRenderer};

/// Identifies a cacheable panel slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum PanelSlot {
    Transport = 0,
    Header = 1,
    Footer = 2,
    Inspector = 3,
    SplitHandles = 4,
    LayerHeaders = 5,
    Viewport = 6,
}

const PANEL_SLOT_COUNT: usize = 7;

/// Panel cache info provided by UIRoot each frame.
pub struct PanelCacheInfo {
    pub slot: PanelSlot,
    pub node_start: usize,
    pub node_end: usize,
    pub rect: Rect,
    /// Optional sub-regions for incremental re-rendering (e.g. effect cards
    /// within the inspector). When present and only a few are dirty, the
    /// panel cache is partially updated via LoadOp::Load instead of full
    /// re-render.
    pub sub_regions: Option<Vec<(usize, usize)>>,
}

/// Max dirty sub-regions before falling back to full panel re-render.
const INCREMENTAL_THRESHOLD: usize = 4;

/// Manages per-panel GPU texture caches and orchestrates dirty re-rendering.
pub struct UICacheManager {
    caches: [PanelCacheTexture; PANEL_SLOT_COUNT],
    format: wgpu::TextureFormat,
    scale_factor: f64,
}

impl UICacheManager {
    pub fn new(format: wgpu::TextureFormat, scale_factor: f64) -> Self {
        Self {
            caches: std::array::from_fn(|_| PanelCacheTexture::new()),
            format,
            scale_factor,
        }
    }

    pub fn set_scale_factor(&mut self, scale_factor: f64) {
        if (self.scale_factor - scale_factor).abs() > 0.001 {
            self.scale_factor = scale_factor;
            self.invalidate_all();
        }
    }

    /// Invalidate all panel caches (full rebuild, resize).
    pub fn invalidate_all(&mut self) {
        for cache in &mut self.caches {
            cache.invalidate();
        }
    }

    /// Invalidate only scroll-panel caches.
    pub fn invalidate_scroll_panels(&mut self) {
        self.caches[PanelSlot::LayerHeaders as usize].invalidate();
        self.caches[PanelSlot::Viewport as usize].invalidate();
    }

    /// Ensure all panel cache textures match their current screen rects.
    pub fn update_sizes(
        &mut self,
        device: &wgpu::Device,
        compositor: &PanelCompositor,
        panels: &[PanelCacheInfo],
    ) {
        let sf = self.scale_factor;
        for info in panels {
            let w = (info.rect.width as f64 * sf).ceil() as u32;
            let h = (info.rect.height as f64 * sf).ceil() as u32;
            self.caches[info.slot as usize].ensure_size(
                device,
                self.format,
                compositor.bind_group_layout(),
                compositor.sampler(),
                w,
                h,
            );
        }
    }

    /// Re-render dirty panels to their cache textures.
    ///
    /// Each panel gets its own command encoder + submit. This is necessary
    /// because `prepare_with_offset` writes per-panel viewport/offset data
    /// to shared GPU buffers via `queue.write_buffer`. Multiple writes to
    /// the same buffer within one submission alias — only the last write
    /// is visible to all render passes. Separate submissions ensure each
    /// panel's buffer state is correct.
    ///
    /// For panels with sub-regions (e.g. inspector effect cards), when the
    /// cache is valid and only a few sub-regions are dirty, an incremental
    /// re-render is done with LoadOp::Load (preserving clean areas) instead
    /// of a full re-render.
    pub fn render_dirty_panels(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        ui_renderer: &mut UIRenderer,
        tree: &UITree,
        panels: &[PanelCacheInfo],
    ) -> usize {
        let mut rendered = 0;

        for info in panels {
            let idx = info.slot as usize;
            let cache = &self.caches[idx];

            // Skip if cache is valid and no nodes are dirty
            if cache.is_valid() && !tree.has_dirty_in_range(info.node_start, info.node_end) {
                continue;
            }

            let view = match cache.view() {
                Some(v) => v,
                None => continue,
            };

            // ── Sub-region incremental path ──
            // If the cache is valid and the panel has sub-regions, check if
            // only a few sub-regions are dirty. If so, render just those
            // into the existing cache texture (LoadOp::Load preserves the rest).
            if cache.is_valid()
                && let Some(ref subs) = info.sub_regions
            {
                    let dirty: Vec<&(usize, usize)> = subs
                        .iter()
                        .filter(|(s, e)| tree.has_dirty_in_range(*s, *e))
                        .collect();

                    if !dirty.is_empty() && dirty.len() <= INCREMENTAL_THRESHOLD {
                        // Accumulate all dirty sub-region nodes into one draw
                        for &(s, e) in &dirty {
                            ui_renderer.render_tree_range(tree, *s, *e);
                        }
                        let vp_w = info.rect.width.ceil() as u32;
                        let vp_h = info.rect.height.ceil() as u32;
                        if ui_renderer.prepare_with_offset(
                            device, queue,
                            vp_w.max(1), vp_h.max(1),
                            info.rect.x, info.rect.y,
                            self.scale_factor, TextMode::Overlay,
                        ) {
                            let mut enc = device.create_command_encoder(
                                &wgpu::CommandEncoderDescriptor {
                                    label: Some("Panel Incremental"),
                                },
                            );
                            {
                                let mut pass = enc.begin_render_pass(
                                    &wgpu::RenderPassDescriptor {
                                        label: Some("Panel Cache Incremental"),
                                        color_attachments: &[Some(
                                            wgpu::RenderPassColorAttachment {
                                                view,
                                                resolve_target: None,
                                                depth_slice: None,
                                                ops: wgpu::Operations {
                                                    load: wgpu::LoadOp::Load,
                                                    store: wgpu::StoreOp::Store,
                                                },
                                            },
                                        )],
                                        depth_stencil_attachment: None,
                                        timestamp_writes: None,
                                        occlusion_query_set: None,
                                        multiview_mask: None,
                                    },
                                );
                                ui_renderer.draw(&mut pass);
                            }
                            queue.submit(std::iter::once(enc.finish()));
                        }
                        // Cache stays valid (we only updated dirty sub-regions)
                        rendered += 1;
                        continue;
                    }
            }

            // ── Full render path ──
            ui_renderer.render_tree_range(tree, info.node_start, info.node_end);
            let vp_w = info.rect.width.ceil() as u32;
            let vp_h = info.rect.height.ceil() as u32;

            if !ui_renderer.prepare_with_offset(
                device, queue,
                vp_w.max(1), vp_h.max(1),
                info.rect.x, info.rect.y,
                self.scale_factor, TextMode::Overlay,
            ) {
                let mut enc = device.create_command_encoder(
                    &wgpu::CommandEncoderDescriptor { label: Some("Panel Clear") },
                );
                {
                    let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Panel Cache Clear"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view,
                            resolve_target: None,
                            depth_slice: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                }
                queue.submit(std::iter::once(enc.finish()));
                self.caches[idx].mark_valid();
                rendered += 1;
                continue;
            }

            let mut enc = device.create_command_encoder(
                &wgpu::CommandEncoderDescriptor { label: Some("Panel Cache") },
            );
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Panel Cache Render"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                ui_renderer.draw(&mut pass);
            }
            queue.submit(std::iter::once(enc.finish()));

            self.caches[idx].mark_valid();
            rendered += 1;
        }

        rendered
    }

    /// Collect (Rect, BindGroup) pairs for compositing valid caches.
    pub fn compositing_data(&self, panels: &[PanelCacheInfo]) -> Vec<(Rect, &wgpu::BindGroup)> {
        let mut result = Vec::with_capacity(PANEL_SLOT_COUNT);
        for info in panels {
            let cache = &self.caches[info.slot as usize];
            if let Some(bg) = cache.bind_group()
                && info.rect.width > 0.0 && info.rect.height > 0.0
            {
                result.push((info.rect, bg));
            }
        }
        result
    }
}
