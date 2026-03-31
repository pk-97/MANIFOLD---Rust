use manifold_ui::node::Rect;
use manifold_ui::tree::UITree;

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
    /// within the inspector). When present and only a few are dirty, only
    /// those sub-regions are re-rendered via LoadOp::Load.
    pub sub_regions: Option<Vec<(usize, usize)>>,
}

/// Max dirty sub-regions before falling back to full panel re-render.
const INCREMENTAL_THRESHOLD: usize = 4;

/// Manages a single full-screen UI atlas texture. Dirty panels render
/// directly into the atlas at their screen position. The surface composite
/// blits the atlas in one draw call instead of 7 separate panel blits.
pub struct UICacheManager {
    // Atlas texture (full logical screen size × scale_factor)
    atlas_texture: Option<wgpu::Texture>,
    atlas_view: Option<wgpu::TextureView>,
    atlas_bind_group: Option<wgpu::BindGroup>,
    atlas_physical_w: u32,
    atlas_physical_h: u32,
    atlas_logical_w: u32,
    atlas_logical_h: u32,

    // Per-panel valid flags (true = panel region in atlas is up to date)
    panel_valid: [bool; PANEL_SLOT_COUNT],
    // True when the entire atlas needs clearing (resize, full rebuild)
    needs_clear: bool,

    format: wgpu::TextureFormat,
    scale_factor: f64,
}

impl UICacheManager {
    pub fn new(format: wgpu::TextureFormat, scale_factor: f64) -> Self {
        Self {
            atlas_texture: None,
            atlas_view: None,
            atlas_bind_group: None,
            atlas_physical_w: 0,
            atlas_physical_h: 0,
            atlas_logical_w: 0,
            atlas_logical_h: 0,
            panel_valid: [false; PANEL_SLOT_COUNT],
            needs_clear: true,
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

    /// Invalidate all panel regions (full rebuild, resize).
    pub fn invalidate_all(&mut self) {
        self.panel_valid = [false; PANEL_SLOT_COUNT];
        self.needs_clear = true;
    }

    /// Invalidate only scroll-panel regions.
    pub fn invalidate_scroll_panels(&mut self) {
        self.panel_valid[PanelSlot::LayerHeaders as usize] = false;
        self.panel_valid[PanelSlot::Viewport as usize] = false;
    }

    /// Ensure atlas texture matches the screen size.
    pub fn ensure_atlas(
        &mut self,
        device: &wgpu::Device,
        compositor: &PanelCompositor,
        logical_w: u32,
        logical_h: u32,
    ) {
        let sf = self.scale_factor;
        let w = (logical_w as f64 * sf).ceil() as u32;
        let h = (logical_h as f64 * sf).ceil() as u32;
        if w == 0 || h == 0 {
            return;
        }
        if self.atlas_physical_w == w && self.atlas_physical_h == h
            && self.atlas_texture.is_some()
        {
            return;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("UI Atlas"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("UI Atlas BG"),
            layout: compositor.bind_group_layout(),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(compositor.sampler()),
                },
            ],
        });
        self.atlas_texture = Some(texture);
        self.atlas_view = Some(view);
        self.atlas_bind_group = Some(bind_group);
        self.atlas_physical_w = w;
        self.atlas_physical_h = h;
        self.atlas_logical_w = logical_w;
        self.atlas_logical_h = logical_h;
        self.invalidate_all();
    }

    /// Atlas bind group for the single-blit surface composite.
    pub fn atlas_bind_group(&self) -> Option<&wgpu::BindGroup> {
        self.atlas_bind_group.as_ref()
    }

    /// Re-render dirty panels directly into the atlas texture.
    ///
    /// Each panel gets its own command encoder + submit (to avoid uniform
    /// buffer aliasing across panels with different viewport/offset).
    /// Panels render with LoadOp::Load to preserve other panels' content.
    /// On invalidate_all, the atlas is cleared first.
    pub fn render_dirty_panels(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        ui_renderer: &mut UIRenderer,
        tree: &UITree,
        panels: &[PanelCacheInfo],
    ) -> usize {
        let atlas_view = match &self.atlas_view {
            Some(v) => v,
            None => return 0,
        };

        // Clear atlas if needed (resize, full rebuild)
        if self.needs_clear {
            let mut enc = device.create_command_encoder(
                &wgpu::CommandEncoderDescriptor { label: Some("Atlas Clear") },
            );
            {
                let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("UI Atlas Clear"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: atlas_view,
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
            self.needs_clear = false;
        }

        let mut rendered = 0;

        for info in panels {
            let idx = info.slot as usize;

            // Skip if panel region is valid and no nodes are dirty
            if self.panel_valid[idx]
                && !tree.has_dirty_in_range(info.node_start, info.node_end)
            {
                continue;
            }

            // ── Sub-region incremental path ──
            if self.panel_valid[idx]
                && let Some(ref subs) = info.sub_regions
            {
                let dirty: Vec<&(usize, usize)> = subs
                    .iter()
                    .filter(|(s, e)| tree.has_dirty_in_range(*s, *e))
                    .collect();

                if !dirty.is_empty() && dirty.len() <= INCREMENTAL_THRESHOLD {
                    for &(s, e) in &dirty {
                        ui_renderer.render_tree_range(tree, *s, *e);
                    }
                    if self.prepare_and_draw(
                        device, queue, atlas_view, ui_renderer,
                    ) {
                        rendered += 1;
                    }
                    continue;
                }
            }

            // ── Full panel render ──
            ui_renderer.render_tree_range(tree, info.node_start, info.node_end);
            if self.prepare_and_draw(
                device, queue, atlas_view, ui_renderer,
            ) {
                self.panel_valid[idx] = true;
                rendered += 1;
            } else {
                // No content from this panel — mark valid anyway
                self.panel_valid[idx] = true;
            }
        }

        rendered
    }

    /// Prepare UIRenderer and draw into the atlas. Returns true if content was drawn.
    /// Always uses LoadOp::Load to preserve other panels in the atlas.
    fn prepare_and_draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas_view: &wgpu::TextureView,
        ui_renderer: &mut UIRenderer,
    ) -> bool {
        // Atlas = full screen. Nodes are at screen-space positions, so
        // viewport = logical screen size, offset = (0, 0).
        if !ui_renderer.prepare_with_offset(
            device, queue,
            self.atlas_logical_w.max(1),
            self.atlas_logical_h.max(1),
            0.0, 0.0,
            self.scale_factor,
            TextMode::Overlay,
        ) {
            return false;
        }

        let mut enc = device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor {
                label: Some("Panel Cache Incremental"),
            },
        );
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Panel Cache Incremental"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: atlas_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
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
        true
    }
}
