//! Settings popup — a small floating modal for render configuration that used
//! to clutter the transport/footer bars (resolution, render scale, SDR tone map,
//! HDR). Opened from `MANIFOLD ▸ Settings…` (⌘,) or the native menu.
//!
//! Self-contained like [`super::audio_setup_panel`]: it builds `UITree` nodes
//! and maps a clicked node id back to a [`PanelAction`] — the *same* actions the
//! old footer/transport buttons emitted (`SetRenderScale`, `SetTonemapCurve`,
//! `ResolutionClicked`, `ToggleHdr`), already routed through `ui_bridge`.
//! Current state is pushed in via the `set_*` setters each sync so the
//! segmented controls highlight the active option.
//!
//! Note: HDR here is the *export* format flag (`settings.export_hdr`), consumed
//! only by the video-export encoder — live on-screen HDR is automatic, driven by
//! the display's EDR headroom.

use crate::{ProjectAction, RootAction, TransportAction};
use crate::chrome::{ChromeHost, Pad, Sizing, View, components};
use crate::color;
use crate::input::{Key, UIEvent};
use crate::node::*;
use crate::tree::UITree;
use crate::types::TonemapCurve;

use super::PanelAction;
use super::overlay::{
    Anchor, Modality, Overlay, OverlayPlacement, OverlayResponse, SizePolicy,
};

// Stable keys for the host-owned modal chrome (background + title strip).
const KEY_BG: u64 = 71_001;
const KEY_CLOSE: u64 = 71_002;

// ── Layout ──
const PANEL_W: f32 = 340.0;
const PAD: f32 = 12.0;
const TITLE_H: f32 = 26.0;
const SECTION_H: f32 = 16.0;
const ROW_H: f32 = 24.0;
const ROW_GAP: f32 = 8.0;
const SECTION_GAP: f32 = 12.0;
const LABEL_W: f32 = 96.0;
const SEG_GAP: f32 = 4.0;
const BTN_FONT: u16 = color::FONT_LABEL;

/// Number of control rows under the single "Render" section. Kept in lockstep
/// with `build_rows` so `body_height` matches the imperative layout.
const ROW_COUNT: f32 = 7.0;

pub struct SettingsPopup {
    open: bool,
    host: ChromeHost,
    bg_id: NodeId,
    close_id: NodeId,
    /// Clicked-node → action map, rebuilt each `build_nodes`.
    actions: Vec<(NodeId, PanelAction)>,

    // ── Current state (fed each sync; drives active highlighting) ──
    resolution_text: String,
    render_scale: f32,
    tonemap: TonemapCurve,
    tonemap_enabled: bool,
    sdr_preview: bool,
    hdr_on: bool,
    split_sections: bool,

    /// The `(x, y)` origin `build_at` last resolved from `Anchor::Centered`
    /// — stashed on every `build_at` (still needed so `build_nodes` has an
    /// origin without re-deriving `Anchor::Centered` itself).
    last_placement: Option<(f32, f32)>,
}

impl Default for SettingsPopup {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsPopup {
    pub fn new() -> Self {
        Self {
            open: false,
            host: ChromeHost::new(),
            bg_id: NodeId::PLACEHOLDER,
            close_id: NodeId::PLACEHOLDER,
            actions: Vec::new(),
            resolution_text: "1080p".into(),
            render_scale: 1.0,
            tonemap: TonemapCurve::AcesNarkowicz,
            tonemap_enabled: false,
            sdr_preview: false,
            hdr_on: false,
            split_sections: false,
            last_placement: None,
        }
    }

    // ── Open/close ──
    pub fn is_open(&self) -> bool {
        self.open
    }
    /// Always `false` now — popups no longer have an entrance tween to
    /// settle. Kept so call sites polling it (to force a rebuild while
    /// animating) don't need special-casing.
    pub fn is_animating(&self) -> bool {
        false
    }
    pub fn open(&mut self) {
        self.open = true;
    }
    pub fn toggle(&mut self) {
        self.open = !self.open;
    }
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Popups open instantly at full size/opacity (no
    /// enter/exit motion). Kept as a no-op so callers can still call it
    /// unconditionally every frame without special-casing.
    pub fn update(&mut self, _tree: &mut UITree) {}

    // ── State setters (store only; the next build applies them) ──
    pub fn set_resolution_text(&mut self, text: &str) {
        self.resolution_text = text.into();
    }
    pub fn set_render_scale(&mut self, scale: f32) {
        self.render_scale = scale;
    }
    pub fn set_tonemap_curve(&mut self, curve: TonemapCurve) {
        self.tonemap = curve;
    }
    pub fn set_tonemap_enabled(&mut self, enabled: bool) {
        self.tonemap_enabled = enabled;
    }
    pub fn set_sdr_preview(&mut self, sdr_preview: bool) -> bool {
        let changed = self.sdr_preview != sdr_preview;
        self.sdr_preview = sdr_preview;
        changed
    }
    pub fn set_hdr(&mut self, on: bool) {
        self.hdr_on = on;
    }
    pub fn set_split_sections(&mut self, on: bool) {
        self.split_sections = on;
    }

    fn body_height(&self) -> f32 {
        PAD + TITLE_H
            + SECTION_GAP
            + SECTION_H
            + ROW_COUNT * ROW_H
            + (ROW_COUNT - 1.0) * ROW_GAP
            + PAD
    }

    // ── Chrome (background + title strip + close), as a host View ──
    fn chrome_view(&self) -> View {
        View::panel()
            .fill()
            .style(UIStyle {
                bg_color: Color32::new(19, 19, 22, 250),
                border_color: Color32::new(48, 48, 52, 255),
                border_width: 1.0,
                corner_radius: color::POPUP_RADIUS,
                ..UIStyle::default()
            })
            .interactive()
            .inert()
            .key(KEY_BG)
            .pad(Pad::all(PAD))
            .child(
                View::row(0.0)
                    .fill_w()
                    .h(Sizing::Fixed(TITLE_H))
                    .child(
                        View::label("Settings")
                            .fill_w()
                            .fill_h()
                            .font(color::FONT_BODY)
                            .text_color(Color32::new(224, 224, 228, 255))
                            .align_text(TextAlign::Left),
                    )
                    .child(
                        View::button("\u{00D7}")
                            .w(Sizing::Fixed(22.0))
                            .fill_h()
                            .style(btn_style(false))
                            .inert()
                            .key(KEY_CLOSE),
                    ),
            )
    }

    fn build_nodes(&mut self, tree: &mut UITree, x: f32, y: f32) {
        self.actions.clear();
        self.last_placement = Some((x, y));

        let chrome = self.chrome_view();
        self.host
            .build(tree, &chrome, Rect::new(x, y, PANEL_W, self.body_height()));
        self.bg_id = self.host.node_id_for_key(KEY_BG).unwrap_or(NodeId::PLACEHOLDER);
        self.close_id = self
            .host
            .node_id_for_key(KEY_CLOSE)
            .unwrap_or(NodeId::PLACEHOLDER);

        let inner_x = x + PAD;
        let inner_w = PANEL_W - PAD * 2.0;
        let ctrl_x = inner_x + LABEL_W;
        let ctrl_w = inner_w - LABEL_W;
        let mut cy = y + PAD + TITLE_H + SECTION_GAP;

        // Section header.
        tree.add_label(
            Some(self.bg_id),
            inner_x,
            cy,
            inner_w,
            SECTION_H,
            "RENDER",
            section_style(),
        );
        cy += SECTION_H;

        // Resolution: label + dropdown trigger (opens the existing picker).
        self.row_label(tree, inner_x, cy, "Resolution");
        let res_id = tree.add_button(
            Some(self.bg_id),
            ctrl_x,
            cy,
            ctrl_w,
            ROW_H,
            components::dropdown_trigger_style(BTN_FONT),
            &self.resolution_text,
        );
        self.actions.push((res_id, PanelAction::Transport(TransportAction::ResolutionClicked)));
        cy += ROW_H + ROW_GAP;

        // Render scale: 1× / 75% / 50% segmented.
        self.row_label(tree, inner_x, cy, "Render Scale");
        let scales = [("1\u{00D7}", 1.0_f32), ("75%", 0.75), ("50%", 0.5)];
        let seg_w = (ctrl_w - SEG_GAP * (scales.len() as f32 - 1.0)) / scales.len() as f32;
        for (i, (label, scale)) in scales.iter().enumerate() {
            let sx = ctrl_x + i as f32 * (seg_w + SEG_GAP);
            let active = (scale - self.render_scale).abs() < 0.01;
            let id = tree.add_button(Some(self.bg_id), sx, cy, seg_w, ROW_H, btn_style(active), label);
            self.actions.push((id, PanelAction::Project(ProjectAction::SetRenderScale(*scale))));
        }
        cy += ROW_H + ROW_GAP;

        // SDR tone map: Off / ACE / Hill / AgX / Khr segmented.
        self.row_label(tree, inner_x, cy, "SDR Tone Map");
        let segment_count = 5.0_f32;
        let seg_w = (ctrl_w - SEG_GAP * (segment_count - 1.0)) / segment_count;
        let off_id = tree.add_button(
            Some(self.bg_id),
            ctrl_x,
            cy,
            seg_w,
            ROW_H,
            btn_style(!self.tonemap_enabled),
            "Off",
        );
        self.actions.push((
            off_id,
            PanelAction::Project(ProjectAction::SetTonemapEnabled(false)),
        ));
        let curves = [
            ("ACE", TonemapCurve::AcesNarkowicz),
            ("Hill", TonemapCurve::AcesHill),
            ("AgX", TonemapCurve::Agx),
            ("Khr", TonemapCurve::KhronosPbrNeutral),
        ];
        for (i, (label, curve)) in curves.iter().enumerate() {
            let sx = ctrl_x + (i as f32 + 1.0) * (seg_w + SEG_GAP);
            let active = self.tonemap_enabled && *curve == self.tonemap;
            let id = tree.add_button(Some(self.bg_id), sx, cy, seg_w, ROW_H, btn_style(active), label);
            self.actions.push((id, PanelAction::Project(ProjectAction::SetTonemapCurve(*curve))));
        }
        cy += ROW_H + ROW_GAP;

        // Preview selects the live output transform only; it never changes the project.
        self.row_label(tree, inner_x, cy, "Preview");
        let preview = [("Display", false), ("SDR", true)];
        let seg_w = (ctrl_w - SEG_GAP) / preview.len() as f32;
        for (i, (label, sdr)) in preview.iter().enumerate() {
            let sx = ctrl_x + i as f32 * (seg_w + SEG_GAP);
            let id = tree.add_button(
                Some(self.bg_id),
                sx,
                cy,
                seg_w,
                ROW_H,
                btn_style(*sdr == self.sdr_preview),
                label,
            );
            self.actions
                .push((id, PanelAction::Project(ProjectAction::SetSdrPreview(*sdr))));
        }
        cy += ROW_H + ROW_GAP;

        // HDR export-format toggle. Affects the recorded/exported file only —
        // live on-screen HDR is automatic (driven by the display's EDR headroom).
        self.row_label(tree, inner_x, cy, "HDR Export");
        let hdr_id = tree.add_button(
            Some(self.bg_id),
            ctrl_x,
            cy,
            ctrl_w,
            ROW_H,
            toggle_style(self.hdr_on),
            if self.hdr_on { "On" } else { "Off" },
        );
        self.actions.push((hdr_id, PanelAction::Project(ProjectAction::ToggleHdr)));
        cy += ROW_H + ROW_GAP;

        // Split at markers — one export file per marker inside the export range
        // (docs/SECTION_EXPORT_DESIGN.md D4). Wired to
        // `ExportConfig.split_at_markers` at export start.
        self.row_label(tree, inner_x, cy, "Split at Markers");
        let split_id = tree.add_button(
            Some(self.bg_id),
            ctrl_x,
            cy,
            ctrl_w,
            ROW_H,
            toggle_style(self.split_sections),
            if self.split_sections { "On" } else { "Off" },
        );
        self.actions.push((split_id, PanelAction::Project(ProjectAction::ToggleSplitSections)));
        cy += ROW_H + ROW_GAP;

        // RT Quality… opens the floating RT tier panel (RT_QUALITY_SETTINGS_DESIGN.md).
        self.row_label(tree, inner_x, cy, "RT Quality");
        let rt_id = tree.add_button(
            Some(self.bg_id),
            ctrl_x,
            cy,
            ctrl_w,
            ROW_H,
            components::dropdown_trigger_style(BTN_FONT),
            "Configure…",
        );
        self.actions.push((rt_id, PanelAction::Root(RootAction::OpenRtQuality)));
    }

    fn row_label(&self, tree: &mut UITree, x: f32, y: f32, text: &str) {
        tree.add_label(Some(self.bg_id), x, y, LABEL_W, ROW_H, text, label_style());
    }

    fn action_for(&self, id: NodeId) -> Option<PanelAction> {
        self.actions
            .iter()
            .find(|(n, _)| *n == id)
            .map(|(_, a)| a.clone())
    }

    fn owns_node(&self, id: NodeId) -> bool {
        id == self.bg_id || id == self.close_id || self.actions.iter().any(|(n, _)| *n == id)
    }
}

impl Overlay for SettingsPopup {
    fn is_open(&self) -> bool {
        self.open
    }

    fn modality(&self) -> Modality {
        Modality::Modal { dim_background: true }
    }

    fn anchor(&self) -> Anchor {
        Anchor::Centered
    }

    fn size_policy(&self) -> SizePolicy {
        SizePolicy::Content
    }

    fn desired_size(&self) -> Vec2 {
        Vec2::new(PANEL_W, self.body_height())
    }

    fn build_at(&mut self, tree: &mut UITree, placement: OverlayPlacement) {
        if !self.open {
            return;
        }
        self.build_nodes(tree, placement.rect.x, placement.rect.y);
    }

    fn on_event(&mut self, event: &UIEvent, _tree: &mut UITree) -> OverlayResponse {
        match event {
            UIEvent::KeyDown { key: Key::Escape, .. } => {
                self.open = false;
                OverlayResponse::Consumed(Vec::new())
            }
            UIEvent::Click { node_id, .. } => {
                let id = *node_id;
                if id == self.close_id {
                    self.open = false;
                    OverlayResponse::Consumed(Vec::new())
                } else if let Some(action) = self.action_for(id) {
                    // A control was clicked — emit its action, keep the popup open.
                    OverlayResponse::Consumed(vec![action])
                } else if self.owns_node(id) {
                    // Panel body / non-action chrome — swallow, don't close.
                    OverlayResponse::Consumed(Vec::new())
                } else {
                    // Dim backdrop / outside — dismiss.
                    self.open = false;
                    OverlayResponse::Consumed(Vec::new())
                }
            }
            _ => OverlayResponse::Ignored,
        }
    }
}

// ── Local styles ──

/// Segmented-control cell (render scale / tonemap) — the kit segment style, the
/// same selector the footer and audio panel use.
fn btn_style(active: bool) -> UIStyle {
    UIStyle {
        font_size: BTN_FONT,
        ..components::segment_style(active)
    }
}

/// A boolean toggle (HDR export): filled accent when on, neutral chip off.
fn toggle_style(on: bool) -> UIStyle {
    UIStyle {
        font_size: BTN_FONT,
        ..components::state_button_style(color::SYNC_ACTIVE, on)
    }
}

fn label_style() -> UIStyle {
    UIStyle {
        text_color: Color32::new(150, 150, 160, 255),
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

fn section_style() -> UIStyle {
    UIStyle {
        text_color: Color32::new(120, 120, 130, 255),
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::overlay::{Overlay, OverlayPlacement};

    fn preview_styles(tree: &UITree) -> (UIStyle, UIStyle) {
        let mut display = None;
        let mut sdr = None;
        for node in tree.nodes() {
            match node.text.as_deref() {
                Some("Display") => display = Some(node.style),
                Some("SDR") => sdr = Some(node.style),
                _ => {}
            }
        }
        (display.expect("Display preview segment"), sdr.expect("SDR preview segment"))
    }

    #[test]
    fn preview_segment_active_style_tracks_runtime_state() {
        let mut popup = SettingsPopup::new();
        popup.open();
        let placement = OverlayPlacement {
            rect: Rect::new(0.0, 0.0, 340.0, 320.0),
            screen: Vec2::new(1280.0, 720.0),
        };
        let mut tree = UITree::new();

        popup.build_at(&mut tree, placement);
        let (display, sdr) = preview_styles(&tree);
        assert_ne!(display.bg_color, sdr.bg_color);

        assert!(popup.set_sdr_preview(true));
        tree.clear();
        popup.build_at(&mut tree, placement);
        let (display_after, sdr_after) = preview_styles(&tree);
        assert_eq!(display.bg_color, sdr_after.bg_color);
        assert_eq!(sdr.bg_color, display_after.bg_color);
    }
}
