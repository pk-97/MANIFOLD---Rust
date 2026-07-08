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

    // Per-panel signature of the sub-regions as last rendered into the atlas:
    // (start, end, bounds-of-first-node). The incremental Load path
    // (`render_sub_region` with LoadOp::Load) is only correct while each
    // sub-region keeps the SAME extent it had when last rendered — Load preserves
    // whatever pixels lie outside the redrawn nodes, so a region that shrank,
    // grew, or moved would leave stale pixels (ghosting, or a dark gap where the
    // section background no longer reaches). A structural change already
    // invalidates the whole panel (`invalidate_all` → full, self-clearing render),
    // so in practice extents are stable here; this signature makes that an
    // *enforced* invariant rather than an implicit one. If an extent ever differs
    // on an incremental frame, `render_dirty_panels` falls back to a full panel
    // render (whose opaque background repaints the whole region) and a debug build
    // asserts. One slot per panel; only the inspector populates it today.
    last_sub_regions: [Vec<(usize, usize, Rect)>; PANEL_SLOT_COUNT],

    format: GpuTextureFormat,
    scale_factor: f64,

    // BUG-060 footer-leak trace: counts render_dirty_panels calls so the atlas
    // is dumped to PNG every N frames when MANIFOLD_TRACE_FOOTER_LEAK is set.
    // Remove with the trace.
    debug_frame_counter: u64,
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
            last_sub_regions: std::array::from_fn(|_| Vec::new()),
            format,
            scale_factor,
            debug_frame_counter: 0,
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

    /// Invalidate only the inspector region — for an in-place inspector scroll
    /// that offset the content nodes without rebuilding the tree. No atlas clear
    /// is needed: the inspector's opaque full-rect background re-paints over the
    /// old content when the slot re-renders. Avoids the full-screen
    /// `invalidate_all` (whole-atlas clear + all panels) that a `needs_rebuild`
    /// scroll would otherwise force every frame.
    pub fn invalidate_inspector(&mut self) {
        self.panel_valid[PanelSlot::Inspector as usize] = false;
    }

    /// Ensure atlas texture matches the screen size.
    pub fn ensure_atlas(&mut self, device: &GpuDevice, logical_w: u32, logical_h: u32) {
        let sf = self.scale_factor;
        let w = (logical_w as f64 * sf).ceil() as u32;
        let h = (logical_h as f64 * sf).ceil() as u32;
        if w == 0 || h == 0 {
            return;
        }
        if self.atlas_physical_w == w && self.atlas_physical_h == h && self.atlas_texture.is_some()
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
            mip_levels: 1,
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

        // BUG-060 footer-leak trace: the footer line = the footer panel's top
        // edge. Any non-footer draw below it is painting where the footer's
        // controls live. Set once. No-op unless MANIFOLD_TRACE_FOOTER_LEAK is set.
        ui_renderer.set_debug_footer_top(
            panels
                .iter()
                .find(|p| p.slot == PanelSlot::Footer)
                .map(|p| p.rect.y),
        );

        for info in panels {
            let idx = info.slot as usize;

            // BUG-060 footer-leak trace: label this panel's render pass.
            ui_renderer.set_debug_pass(match info.slot {
                PanelSlot::Transport => "transport",
                PanelSlot::Header => "header",
                PanelSlot::Footer => "footer",
                PanelSlot::Inspector => "inspector",
                PanelSlot::SplitHandles => "split-handles",
                PanelSlot::LayerHeaders => "layer-headers",
                PanelSlot::Viewport => "viewport",
            });

            // Skip if panel region is valid and no nodes are dirty.
            if self.panel_valid[idx] && !tree.has_dirty_in_range(info.node_start, info.node_end) {
                continue;
            }

            // ── Sub-region incremental path (doesn't count against budget) ──
            // Sound only while every sub-region keeps the extent it had when last
            // rendered (see `last_sub_regions`) AND no dirt sits outside the
            // sub-regions — chrome (tab strip, cog/Collapse, scrollbar) lives in
            // no sub-region, so the Load path never repaints it and a stale pixel
            // there would survive. `incremental_path_safe` enforces both; either
            // failure drops to the full, self-clearing panel render below.
            if self.panel_valid[idx]
                && let Some(ref subs) = info.sub_regions
                && Self::incremental_path_safe(
                    &self.last_sub_regions[idx],
                    subs,
                    info.node_start,
                    info.node_end,
                    tree,
                )
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
                    // The incremental path only fires when there is no dirt outside
                    // the sub-regions (guaranteed by `incremental_path_safe`), so the
                    // whole panel range is clean after this repaint. Return the full
                    // range — clearing it (not just the dirty sub-regions) keeps
                    // panel-range dirty-flag ownership with the cache manager, so the
                    // caller's blanket clear can be narrowed to the overlay region.
                    rendered_ranges.push((info.node_start, info.node_end));
                    continue;
                }
            }

            // ── Full panel render ──
            // The panel's first node is its opaque, full-rect background, so a full
            // render is self-clearing under LoadOp::Load — no atlas clear needed,
            // and any pre-existing ghost in the region is painted over.
            ui_renderer.render_tree_range(tree, info.node_start, info.node_end);
            if self.prepare_and_draw(device, ui_renderer) {
                self.panel_valid[idx] = true;
                rendered += 1;
            } else {
                self.panel_valid[idx] = true;
            }
            // Record the sub-region extents this full render established, so the
            // next incremental frame can confirm nothing moved before trusting Load.
            self.last_sub_regions[idx] = Self::sub_region_sig(info.sub_regions.as_deref(), tree);
            rendered_ranges.push((info.node_start, info.node_end));
        }

        // BUG-060 footer-leak trace: dump the composited atlas to PNG every ~30
        // frames so the footer region can be inspected directly — settles whether
        // a panel wrote blue INTO the atlas or the blue is added after the atlas.
        if ui_renderer.debug_footer_leak_enabled() {
            self.debug_frame_counter += 1;
            if self.debug_frame_counter.is_multiple_of(30) {
                self.debug_dump_atlas(device);
            }
        }

        (rendered, rendered_ranges)
    }

    /// BUG-060 footer-leak trace: read the atlas back and write it to
    /// `/tmp/atlas_latest.png` (overwritten each dump). Bgra8Unorm → RGBA. Stalls
    /// the GPU (readback), so only ever called under the env flag. Remove with the trace.
    fn debug_dump_atlas(&self, device: &GpuDevice) {
        let Some(tex) = self.atlas_texture.as_ref() else {
            return;
        };
        let (w, h) = (self.atlas_physical_w, self.atlas_physical_h);
        if w == 0 || h == 0 {
            return;
        }
        let bytes_per_row = w * 4;
        let buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
        let mut enc = device.create_encoder("atlas-dump");
        enc.copy_texture_to_buffer(tex, &buf, w, h, bytes_per_row);
        enc.commit_and_wait_completed();
        let Some(ptr) = buf.mapped_ptr() else { return };
        let bgra = unsafe { std::slice::from_raw_parts(ptr, (w * h * 4) as usize) };
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            rgba[i * 4] = bgra[i * 4 + 2]; // R <- B
            rgba[i * 4 + 1] = bgra[i * 4 + 1]; // G
            rgba[i * 4 + 2] = bgra[i * 4]; // B <- R
            rgba[i * 4 + 3] = 255; // opaque, so a viewer doesn't show transparent as white
        }
        if let Err(e) = image::save_buffer(
            "/tmp/atlas_latest.png",
            &rgba,
            w,
            h,
            image::ColorType::Rgba8,
        ) {
            eprintln!("[FOOTER-LEAK] atlas dump failed: {e}");
        } else {
            eprintln!("[FOOTER-LEAK] dumped atlas {w}x{h} -> /tmp/atlas_latest.png");
        }
    }

    /// Signature of a panel's sub-regions: each `(start, end, bounds-of-first-node)`.
    /// The first node of a sound sub-region is its opaque frame, so its bounds are
    /// the region's extent — what the incremental Load path must not let drift.
    fn sub_region_sig(
        subs: Option<&[(usize, usize)]>,
        tree: &UITree,
    ) -> Vec<(usize, usize, Rect)> {
        subs.unwrap_or(&[])
            .iter()
            .map(|&(s, e)| (s, e, tree.get_bounds(tree.id_at(s))))
            .collect()
    }

    /// Whether the incremental sub-region Load path is safe for a panel this
    /// frame. Two conditions are both required; either failure forces the caller
    /// onto the full, self-clearing panel render.
    ///
    /// First, `extents_unchanged`: every sub-region keeps the extent it had when
    /// last fully rendered (Load preserves pixels outside the redrawn nodes, so a
    /// moved/resized region would leave stale pixels).
    ///
    /// Second, no dirt outside the sub-regions: the tab strip, cog/Collapse
    /// controls and scrollbar sit in NO sub-region, so the Load path never
    /// repaints them. If one of them is dirty (e.g. un-hovering a tab while an
    /// audio-modulated card repaints its slider every frame), the incremental
    /// path would drop that repaint and the stale chrome would persist — the
    /// BUG-015 hole.
    fn incremental_path_safe(
        last: &[(usize, usize, Rect)],
        subs: &[(usize, usize)],
        node_start: usize,
        node_end: usize,
        tree: &UITree,
    ) -> bool {
        Self::extents_unchanged(last, subs, tree)
            && !tree.has_dirty_outside_ranges(node_start, node_end, subs)
    }

    /// Whether `subs` matches `last` region-for-region (same ranges and same
    /// first-node bounds). A mismatch means a sub-region's extent changed since
    /// the panel was last fully rendered, so the incremental Load path is no longer
    /// safe and the caller falls back to a full render. Extents are expected to be
    /// stable here (structural changes invalidate the whole panel), so a mismatch
    /// is logged: it flags an in-place mutation path that moved/resized cached
    /// nodes without invalidating. Non-fatal — the fallback is correct — so it
    /// stays a log, never a panic, on the live render path.
    fn extents_unchanged(
        last: &[(usize, usize, Rect)],
        subs: &[(usize, usize)],
        tree: &UITree,
    ) -> bool {
        let matches = last.len() == subs.len()
            && last.iter().zip(subs.iter()).all(|(&(ls, le, lb), &(s, e))| {
                ls == s && le == e && lb == tree.get_bounds(tree.id_at(s))
            });
        if !matches {
            log::debug!(
                "UI cache: sub-region extent changed on an incremental frame — \
                 falling back to a full panel render (correct, slightly slower). An \
                 in-place mutation moved/resized cached nodes without invalidating."
            );
        }
        matches
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

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_ui::node::UIStyle;

    /// Two sub-regions: nodes [0,2) and [2,3). Returns the tree and the partition.
    fn tree_with_subregions() -> (UITree, Vec<(usize, usize)>) {
        let mut tree = UITree::new();
        // Sub-region 0 — first node is the opaque frame at (0,0,100,50).
        tree.add_panel(None, 0.0, 0.0, 100.0, 50.0, UIStyle::default());
        tree.add_panel(None, 10.0, 10.0, 20.0, 20.0, UIStyle::default());
        // Sub-region 1 — first node frame at (0,60,100,40).
        tree.add_panel(None, 0.0, 60.0, 100.0, 40.0, UIStyle::default());
        (tree, vec![(0, 2), (2, 3)])
    }

    #[test]
    fn extents_unchanged_when_bounds_stable() {
        let (tree, subs) = tree_with_subregions();
        let sig = UICacheManager::sub_region_sig(Some(&subs), &tree);
        // In-place content change (no bounds change) is the incremental fast path.
        assert!(UICacheManager::extents_unchanged(&sig, &subs, &tree));
    }

    #[test]
    fn extent_change_forces_fallback() {
        let (mut tree, subs) = tree_with_subregions();
        let sig = UICacheManager::sub_region_sig(Some(&subs), &tree);
        // A sub-region's first node grows — Load would leave stale pixels below it,
        // so the guard must report the extent changed (caller falls back to full).
        tree.set_bounds(tree.id_at(2), Rect::new(0.0, 60.0, 100.0, 80.0));
        assert!(!UICacheManager::extents_unchanged(&sig, &subs, &tree));
    }

    #[test]
    fn partition_change_forces_fallback() {
        let (tree, subs) = tree_with_subregions();
        let sig = UICacheManager::sub_region_sig(Some(&subs), &tree);
        // A different sub-region partition (count / ranges differ) is never safe to
        // trust against an older signature.
        let repartitioned = vec![(0, 3)];
        assert!(!UICacheManager::extents_unchanged(&sig, &repartitioned, &tree));
    }

    #[test]
    fn no_subregions_signature_is_empty() {
        let (tree, _) = tree_with_subregions();
        assert!(UICacheManager::sub_region_sig(None, &tree).is_empty());
    }

    /// A panel [0,4) shaped like the inspector: node 0 = chrome (background /
    /// tab strip), nodes 1-2 = one card sub-region, node 3 = chrome (scrollbar).
    /// Nodes 0 and 3 sit in NO sub-region — the incremental Load path can never
    /// repaint them. Returns the tree and the sub-region partition `[(1,3)]`.
    fn tree_with_chrome_and_card() -> (UITree, Vec<(usize, usize)>) {
        let mut tree = UITree::new();
        tree.add_panel(None, 0.0, 0.0, 100.0, 100.0, UIStyle::default()); // 0: chrome bg
        tree.add_panel(None, 5.0, 10.0, 90.0, 30.0, UIStyle::default()); // 1: card frame
        tree.add_panel(None, 8.0, 12.0, 40.0, 10.0, UIStyle::default()); // 2: card content
        tree.add_panel(None, 0.0, 95.0, 100.0, 5.0, UIStyle::default()); // 3: chrome scrollbar
        (tree, vec![(1, 3)])
    }

    #[test]
    fn incremental_used_when_only_card_dirt() {
        let (mut tree, subs) = tree_with_chrome_and_card();
        let sig = UICacheManager::sub_region_sig(Some(&subs), &tree);
        tree.clear_dirty();
        // Dirt confined to a node INSIDE the card sub-region — the Load path
        // reaches it, and the sub-region's first node (index 1) hasn't moved, so
        // the incremental path stays safe.
        tree.set_bounds(tree.id_at(2), Rect::new(8.0, 12.0, 50.0, 10.0));
        assert!(UICacheManager::incremental_path_safe(&sig, &subs, 0, 4, &tree));
    }

    #[test]
    fn out_of_subregion_dirt_forces_full_render() {
        let (mut tree, subs) = tree_with_chrome_and_card();
        let sig = UICacheManager::sub_region_sig(Some(&subs), &tree);
        tree.clear_dirty();
        // Dirty a chrome node (the scrollbar, index 3) that lies in NO sub-region.
        // Its first-node-of-a-sub-region bounds are untouched, so `extents_unchanged`
        // still passes — the ONLY reason the incremental path must be rejected is
        // the out-of-sub-region dirt, which the Load path would never repaint. The
        // guard must force the full, self-clearing panel render (BUG-015).
        tree.set_bounds(tree.id_at(3), Rect::new(0.0, 95.0, 80.0, 5.0));
        assert!(UICacheManager::extents_unchanged(&sig, &subs, &tree));
        assert!(!UICacheManager::incremental_path_safe(&sig, &subs, 0, 4, &tree));
    }
}
