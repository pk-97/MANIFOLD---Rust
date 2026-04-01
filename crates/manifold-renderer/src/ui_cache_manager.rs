use manifold_gpu::{
    GpuDevice, GpuLoadAction, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};
use manifold_ui::node::Rect;
use manifold_ui::tree::UITree;

use crate::ui_renderer::UIRenderer;

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
/// With dirty-only rendering, per-sub-region cost is ~3 node draws (not ~40),
/// so handling many sub-regions incrementally is cheap.
const INCREMENTAL_THRESHOLD: usize = 16;

/// Manages a single full-screen UI atlas texture. Dirty panels render
/// directly into the atlas at their screen position. The surface composite
/// blits the atlas in one draw call instead of 7 separate panel blits.
pub struct UICacheManager {
    // Atlas texture (full logical screen size × scale_factor).
    atlas_texture: Option<GpuTexture>,
    atlas_physical_w: u32,
    atlas_physical_h: u32,
    atlas_logical_w: u32,
    atlas_logical_h: u32,

    // Per-panel valid flags (true = panel region in atlas is up to date).
    panel_valid: [bool; PANEL_SLOT_COUNT],
    // True when the entire atlas needs clearing (resize, full rebuild).
    needs_clear: bool,

    format: GpuTextureFormat,
    scale_factor: f64,
}

impl UICacheManager {
    pub fn new(format: GpuTextureFormat, scale_factor: f64) -> Self {
        Self {
            atlas_texture: None,
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
        // Clear atlas: panel positions may have changed (split handle drag,
        // inspector resize, window resize). Without clearing, old-position
        // content persists and gets blitted alongside new-position content
        // → visible ghosting. Scroll panels (LayerHeaders, Viewport) bypass
        // the amortization cap so they always render immediately. Static
        // panels (Transport, Header, Footer) are the first 3 rendered.
        // Inspector/SplitHandles may be deferred 1 frame — the black
        // background shows through briefly, imperceptible at 120Hz.
        self.needs_clear = true;
    }

    /// Invalidate only scroll-panel regions.
    pub fn invalidate_scroll_panels(&mut self) {
        self.panel_valid[PanelSlot::LayerHeaders as usize] = false;
        self.panel_valid[PanelSlot::Viewport as usize] = false;
    }

    /// Ensure atlas texture matches the screen size.
    pub fn ensure_atlas(&mut self, device: &GpuDevice, logical_w: u32, logical_h: u32) {
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
        let texture = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: self.format,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ,
            label: "UI Atlas",
        });
        self.atlas_texture = Some(texture);
        self.atlas_physical_w = w;
        self.atlas_physical_h = h;
        self.atlas_logical_w = logical_w;
        self.atlas_logical_h = logical_h;
        self.invalidate_all();
        // New texture has undefined content — must clear.
        self.needs_clear = true;
    }

    /// Atlas texture to blit to the surface.
    pub fn atlas_texture(&self) -> Option<&GpuTexture> {
        self.atlas_texture.as_ref()
    }

    /// Render dirty panels into the atlas. Returns `(panels_rendered, rendered_ranges)`.
    ///
    /// Each panel gets its own encoder + commit (to avoid shared buffer aliasing
    /// across panels with different viewport/offset). Panels render with
    /// LoadOp::Load to preserve other panels' content. On invalidate_all, the
    /// atlas is cleared first and ALL panels render in the same frame.
    /// The clean-frame fast path in present_all_windows ensures full rebuilds
    /// only happen on actual visual changes, not every idle frame.
    pub fn render_dirty_panels(
        &mut self,
        device: &GpuDevice,
        ui_renderer: &mut UIRenderer,
        tree: &UITree,
        panels: &[PanelCacheInfo],
    ) -> (usize, Vec<(usize, usize)>) {
        let mut rendered_ranges: Vec<(usize, usize)> = Vec::new();

        if self.atlas_texture.is_none() {
            return (0, rendered_ranges);
        }

        // Clear atlas if needed (resize, full rebuild).
        if self.needs_clear {
            let mut enc = device.create_encoder("Atlas Clear");
            enc.clear_texture(self.atlas_texture.as_ref().unwrap(), 0.0, 0.0, 0.0, 0.0);
            enc.commit();
            self.needs_clear = false;
        }

        let mut rendered = 0;

        for info in panels {
            let idx = info.slot as usize;

            // Skip if panel region is valid and no nodes are dirty.
            if self.panel_valid[idx]
                && !tree.has_dirty_in_range(info.node_start, info.node_end)
            {
                continue;
            }

            // ── Sub-region incremental path (doesn't count against budget) ──
            if self.panel_valid[idx]
                && let Some(ref subs) = info.sub_regions
            {
                let dirty: Vec<&(usize, usize)> = subs
                    .iter()
                    .filter(|(s, e)| tree.has_dirty_in_range(*s, *e))
                    .collect();

                if !dirty.is_empty() && dirty.len() <= INCREMENTAL_THRESHOLD {
                    for &(s, e) in &dirty {
                        // Render all visible nodes in the sub-region (not dirty-only).
                        // dirty_only=true causes ghosting when slider backgrounds
                        // need to be redrawn under moved fill/thumb elements.
                        // The flat traversal fix already limits work to ~40 nodes
                        // per card instead of the entire inspector.
                        ui_renderer.render_sub_region(tree, *s, *e, false);
                    }
                    if self.prepare_and_draw(device, ui_renderer) {
                        rendered += 1;
                    }
                    // Mark only the dirty sub-regions for clearing.
                    for &(s, e) in &dirty {
                        rendered_ranges.push((*s, *e));
                    }
                    continue;
                }
            }

            // ── Full panel render ──
            ui_renderer.render_tree_range(tree, info.node_start, info.node_end);
            if self.prepare_and_draw(device, ui_renderer) {
                self.panel_valid[idx] = true;
                rendered += 1;
            } else {
                self.panel_valid[idx] = true;
            }
            rendered_ranges.push((info.node_start, info.node_end));
        }

        (rendered, rendered_ranges)
    }

    /// Prepare UIRenderer and draw into the atlas. Returns true if content was drawn.
    /// Always uses LoadOp::Load to preserve other panels in the atlas.
    fn prepare_and_draw(&self, device: &GpuDevice, ui_renderer: &mut UIRenderer) -> bool {
        // Atlas = full screen. Nodes are at screen-space positions, so
        // viewport = logical screen size, offset = (0, 0).
        if !ui_renderer.prepare_with_offset(
            device,
            self.atlas_logical_w.max(1),
            self.atlas_logical_h.max(1),
            0.0,
            0.0,
            self.scale_factor,
        ) {
            return false;
        }

        let atlas = self.atlas_texture.as_ref().unwrap();
        let mut enc = device.create_encoder("Panel Cache");
        ui_renderer.render(&mut enc, atlas, GpuLoadAction::Load);
        enc.commit();
        true
    }
}
