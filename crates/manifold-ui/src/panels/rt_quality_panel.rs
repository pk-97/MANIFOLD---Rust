//! RT Quality panel — per-project raytracing quality settings for live performance
//! and export rendering.
//!
//! A floating modal for configuring RT sample counts (shadows/AO/GI/reflections)
//! and ray dispatch resolution, with separate "Real-time" and "Export" columns.
//! Opened from the Settings popup (⌘,) — the RT Quality row's Configure… button
//! dispatches `RootAction::OpenRtQuality`.
//!
//! Self-contained like [`super::settings_popup`]: builds `UITree` nodes and maps
//! clicked node ids back to [`PanelAction`]. Value cells are dropdown triggers —
//! a click emits `RootAction::OpenRtQuality{Tier,Res}Dropdown` and the app opens
//! the shared dropdown overlay with items built from its own current snapshot.
//! Current state is pushed in via `configure` each sync so the triggers show the
//! active tier.

use crate::chrome::components::dropdown_trigger_style;
use crate::chrome::{ChromeHost, Pad, Sizing, View};
use crate::color;
use crate::input::{Key, UIEvent};
use crate::node::*;
use crate::tree::UITree;
use manifold_foundation::settings::{RtQualitySettings, RtTierField};

use super::PanelAction;
use super::RootAction;
use super::overlay::{
    Anchor, Modality, Overlay, OverlayPlacement, OverlayResponse, SizePolicy,
};

// Stable keys for the host-owned modal chrome (background + title strip).
const KEY_BG: u64 = 72_001;
const KEY_CLOSE: u64 = 72_002;

// ── Layout ──
const PANEL_W: f32 = 560.0;
const PAD: f32 = 12.0;
const TITLE_H: f32 = 26.0;
const SECTION_H: f32 = 16.0;
const ROW_H: f32 = 24.0;
const ROW_GAP: f32 = 4.0;
const SECTION_GAP: f32 = 12.0;
const COL_LABEL_W: f32 = 120.0;
const BTN_FONT: u16 = color::FONT_LABEL;

/// Number of setting rows (shadows, AO, GI, reflections, ray resolution, spatial denoise).
const ROW_COUNT: f32 = 6.0;

pub struct RtQualityPanel {
    open: bool,
    host: ChromeHost,
    bg_id: NodeId,
    close_id: NodeId,
    /// Clicked-node → action map, rebuilt each `build_nodes`.
    actions: Vec<(NodeId, PanelAction)>,

    // ── Current state (fed each sync; drives active highlighting) ──
    settings: RtQualitySettings,

    /// The `(x, y)` origin `build_at` last resolved from `Anchor::Centered`
    /// — stashed on every `build_at` so `build_nodes` has an origin without
    /// re-deriving `Anchor::Centered` itself.
    last_placement: Option<(f32, f32)>,
}

impl Default for RtQualityPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl RtQualityPanel {
    pub fn new() -> Self {
        Self {
            open: false,
            host: ChromeHost::new(),
            bg_id: NodeId::PLACEHOLDER,
            close_id: NodeId::PLACEHOLDER,
            actions: Vec::new(),
            settings: RtQualitySettings::default(),
            last_placement: None,
        }
    }

    // ── Open/close ──
    pub fn is_open(&self) -> bool {
        self.open
    }
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

    /// Popups open instantly at full size/opacity (no enter/exit motion).
    pub fn update(&mut self, _tree: &mut UITree) {}

    // ── State setter (fed each sync from the project snapshot) ──
    pub fn configure(&mut self, settings: RtQualitySettings) {
        self.settings = settings;
    }

    /// The last snapshot pushed by `configure` — the app reads this to build
    /// dropdown items against the current values (never a stale base).
    pub fn current_settings(&self) -> RtQualitySettings {
        self.settings
    }

    fn body_height(&self) -> f32 {
        // Section header + 5 rows + row gaps + padding
        PAD + TITLE_H + SECTION_GAP + SECTION_H + ROW_COUNT * ROW_H + (ROW_COUNT - 1.0) * ROW_GAP + PAD
    }

    // ── Chrome (background + title strip + close), as a host View ──
    fn chrome_view(&self) -> View {
        View::panel()
            .fill()
            .style(UIStyle {
                bg_color: Color32::new(19, 19, 22, 250), // design-token-exempt: modal popup background matches settings_popup precedent
                border_color: Color32::new(48, 48, 52, 255), // design-token-exempt: modal popup border matches settings_popup precedent
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
                        View::label("RT Quality")
                            .fill_w()
                            .fill_h()
                            .font(color::FONT_BODY)
                            // design-token-exempt: title text color matches settings_popup precedent
                            .text_color(Color32::new(224, 224, 228, 255)) // design-token-exempt: title text color matches settings_popup precedent
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
        self.host.build(
            tree,
            &chrome,
            Rect::new(x, y, PANEL_W, self.body_height()),
        );
        self.bg_id = self.host.node_id_for_key(KEY_BG).unwrap_or(NodeId::PLACEHOLDER);
        self.close_id = self.host.node_id_for_key(KEY_CLOSE).unwrap_or(NodeId::PLACEHOLDER);

        let inner_x = x + PAD;
        let inner_w = PANEL_W - PAD * 2.0;
        let mut cy = y + PAD + TITLE_H + SECTION_GAP;

        // Section header: column labels
        tree.add_label(
            Some(self.bg_id),
            inner_x,
            cy,
            COL_LABEL_W,
            SECTION_H,
            "",
            section_style(),
        );

        // Column headers
        let realtime_x = inner_x + COL_LABEL_W;
        let realtime_w = (inner_w - COL_LABEL_W) / 2.0 - ROW_GAP;
        let export_x = realtime_x + realtime_w + ROW_GAP;
        let export_w = inner_w - COL_LABEL_W - realtime_w - ROW_GAP;

        tree.add_label(
            Some(self.bg_id),
            realtime_x,
            cy,
            realtime_w,
            SECTION_H,
            "Real-time",
            section_style(),
        );
        tree.add_label(
            Some(self.bg_id),
            export_x,
            cy,
            export_w,
            SECTION_H,
            "Export",
            section_style(),
        );
        cy += SECTION_H;

        // Build rows: Shadows, AO, GI, Reflections, Ray Resolution, Spatial Denoise
        self.build_tier_row(tree, inner_x, cy, realtime_x, realtime_w, export_x, export_w, "Shadows", RtTierField::Shadows);
        cy += ROW_H + ROW_GAP;
        self.build_tier_row(tree, inner_x, cy, realtime_x, realtime_w, export_x, export_w, "AO", RtTierField::Ao);
        cy += ROW_H + ROW_GAP;
        self.build_tier_row(tree, inner_x, cy, realtime_x, realtime_w, export_x, export_w, "GI", RtTierField::Gi);
        cy += ROW_H + ROW_GAP;
        self.build_tier_row(tree, inner_x, cy, realtime_x, realtime_w, export_x, export_w, "Reflections", RtTierField::Reflections);
        cy += ROW_H + ROW_GAP;
        self.build_resolution_row(tree, inner_x, cy, realtime_x, realtime_w, export_x, export_w);
        cy += ROW_H + ROW_GAP;
        self.build_denoise_row(tree, inner_x, cy, realtime_x, realtime_w, export_x, export_w);
    }

    /// Build one tier row (shadows/AO/GI/reflections) with two dropdown
    /// trigger columns. A click asks the app to open the shared dropdown
    /// overlay (`RootAction::OpenRtQualityTierDropdown`) — the app builds
    /// the items from its own current snapshot, so a selection can never
    /// edit a stale base (the cycle-button multi-click bug class).
    fn build_tier_row(
        &mut self,
        tree: &mut UITree,
        label_x: f32,
        y: f32,
        realtime_x: f32,
        realtime_w: f32,
        export_x: f32,
        export_w: f32,
        label: &str,
        field: RtTierField,
    ) {
        // Row label
        tree.add_label(
            Some(self.bg_id),
            label_x,
            y,
            COL_LABEL_W,
            ROW_H,
            label,
            label_style(),
        );

        // Real-time dropdown trigger
        let realtime_id = tree.add_button(
            Some(self.bg_id),
            realtime_x,
            y,
            realtime_w,
            ROW_H,
            dropdown_trigger_style(BTN_FONT),
            field.get(&self.settings.realtime).label(),
        );
        self.actions.push((
            realtime_id,
            PanelAction::Root(RootAction::OpenRtQualityTierDropdown {
                field,
                realtime: true,
                anchor: Rect::new(realtime_x, y, realtime_w, ROW_H),
            }),
        ));

        // Export dropdown trigger
        let export_id = tree.add_button(
            Some(self.bg_id),
            export_x,
            y,
            export_w,
            ROW_H,
            dropdown_trigger_style(BTN_FONT),
            field.get(&self.settings.export).label(),
        );
        self.actions.push((
            export_id,
            PanelAction::Root(RootAction::OpenRtQualityTierDropdown {
                field,
                realtime: false,
                anchor: Rect::new(export_x, y, export_w, ROW_H),
            }),
        ));
    }

    /// Build the ray resolution row with two dropdown trigger columns.
    fn build_resolution_row(
        &mut self,
        tree: &mut UITree,
        label_x: f32,
        y: f32,
        realtime_x: f32,
        realtime_w: f32,
        export_x: f32,
        export_w: f32,
    ) {
        // Row label
        tree.add_label(
            Some(self.bg_id),
            label_x,
            y,
            COL_LABEL_W,
            ROW_H,
            "Ray Resolution",
            label_style(),
        );

        let realtime_id = tree.add_button(
            Some(self.bg_id),
            realtime_x,
            y,
            realtime_w,
            ROW_H,
            dropdown_trigger_style(BTN_FONT),
            self.settings.realtime.ray_resolution.label(),
        );
        self.actions.push((
            realtime_id,
            PanelAction::Root(RootAction::OpenRtQualityResDropdown {
                realtime: true,
                anchor: Rect::new(realtime_x, y, realtime_w, ROW_H),
            }),
        ));

        let export_id = tree.add_button(
            Some(self.bg_id),
            export_x,
            y,
            export_w,
            ROW_H,
            dropdown_trigger_style(BTN_FONT),
            self.settings.export.ray_resolution.label(),
        );
        self.actions.push((
            export_id,
            PanelAction::Root(RootAction::OpenRtQualityResDropdown {
                realtime: false,
                anchor: Rect::new(export_x, y, export_w, ROW_H),
            }),
        ));
    }

    /// Build the spatial denoise row with two dropdown trigger columns.
    fn build_denoise_row(
        &mut self,
        tree: &mut UITree,
        label_x: f32,
        y: f32,
        realtime_x: f32,
        realtime_w: f32,
        export_x: f32,
        export_w: f32,
    ) {
        // Row label
        tree.add_label(
            Some(self.bg_id),
            label_x,
            y,
            COL_LABEL_W,
            ROW_H,
            "Spatial Denoise",
            label_style(),
        );

        let realtime_id = tree.add_button(
            Some(self.bg_id),
            realtime_x,
            y,
            realtime_w,
            ROW_H,
            dropdown_trigger_style(BTN_FONT),
            self.settings.realtime.spatial_denoise.label(),
        );
        self.actions.push((
            realtime_id,
            PanelAction::Root(RootAction::OpenRtQualityDenoiseDropdown {
                realtime: true,
                anchor: Rect::new(realtime_x, y, realtime_w, ROW_H),
            }),
        ));

        let export_id = tree.add_button(
            Some(self.bg_id),
            export_x,
            y,
            export_w,
            ROW_H,
            dropdown_trigger_style(BTN_FONT),
            self.settings.export.spatial_denoise.label(),
        );
        self.actions.push((
            export_id,
            PanelAction::Root(RootAction::OpenRtQualityDenoiseDropdown {
                realtime: false,
                anchor: Rect::new(export_x, y, export_w, ROW_H),
            }),
        ));
    }

    fn action_for(&self, id: NodeId) -> Option<PanelAction> {
        self.actions
            .iter()
            .find(|(n, _)| *n == id)
            .map(|(_, a)| a.clone())
    }
}

impl Overlay for RtQualityPanel {
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
                self.close();
                OverlayResponse::Consumed(Vec::new())
            }
            UIEvent::Click { node_id, .. } => {
                if *node_id == self.close_id {
                    self.close();
                    OverlayResponse::Consumed(Vec::new())
                } else if let Some(action) = self.action_for(*node_id) {
                    OverlayResponse::Consumed(vec![action])
                } else {
                    OverlayResponse::Ignored
                }
            }
            _ => OverlayResponse::Ignored,
        }
    }
}

// ── Style helpers ──

fn section_style() -> UIStyle {
    UIStyle {
        text_color: Color32::new(140, 140, 150, 255), // design-token-exempt: section header color matches settings_popup precedent
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

fn label_style() -> UIStyle {
    UIStyle {
        text_color: Color32::new(200, 200, 205, 255), // design-token-exempt: label color matches settings_popup precedent
        font_size: color::FONT_BODY,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

fn btn_style(active: bool) -> UIStyle {
    let text_color = if active {
        Color32::new(224, 224, 228, 255) // design-token-exempt: active button text matches settings_popup precedent
    } else {
        Color32::new(160, 160, 165, 255) // design-token-exempt: inactive button text matches settings_popup precedent
    };

    if active {
        UIStyle {
            text_color,
            bg_color: Color32::new(60, 60, 70, 255), // design-token-exempt: active button bg matches settings_popup precedent
            font_size: color::FONT_BODY,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    } else {
        UIStyle {
            text_color,
            font_size: color::FONT_BODY,
            text_align: TextAlign::Center,
            ..UIStyle::default()
        }
    }
}