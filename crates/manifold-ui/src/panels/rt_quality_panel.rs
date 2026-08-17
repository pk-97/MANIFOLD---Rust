//! RT Quality panel — per-project raytracing quality settings for live performance
//! and export rendering.
//!
//! A floating modal for configuring RT sample counts (shadows/AO/GI/reflections)
//! and ray dispatch resolution, with separate "Real-time" and "Export" columns.
//! Opened from the Settings popup (⌘,) — the RT Quality row's Configure… button
//! dispatches `RootAction::OpenRtQuality`.
//!
//! Self-contained like [`super::settings_popup`]: builds `UITree` nodes and maps
//! clicked node ids back to [`PanelAction`] (the `ChangeRtQuality` action), already
//! routed through `ui_bridge`. Current state is pushed in via `configure` each sync
//! so dropdowns highlight the active tier.

use crate::ProjectAction;
use crate::chrome::{ChromeHost, Pad, Sizing, View};
use crate::color;
use crate::input::{Key, UIEvent};
use crate::node::*;
use crate::tree::UITree;
use manifold_foundation::settings::{RtQualityColumn, RtQualitySettings, RtQualityTier, RtRayResolution};

use super::PanelAction;
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

/// Number of setting rows (shadows, AO, GI, reflections, ray resolution).
const ROW_COUNT: f32 = 5.0;

/// Quality tiers for dropdown options, in UI order (worst to best).
const TIERS: [RtQualityTier; 6] = [
    RtQualityTier::UltraLow,
    RtQualityTier::Low,
    RtQualityTier::Medium,
    RtQualityTier::High,
    RtQualityTier::ExtraHigh,
    RtQualityTier::Ultra,
];

/// Ray resolution options, in UI order (lowest to highest quality).
const RESOLUTIONS: [RtRayResolution; 4] = [
    RtRayResolution::Quarter,
    RtRayResolution::Half,
    RtRayResolution::ThreeQuarter,
    RtRayResolution::Native,
];

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

        // Build rows: Shadows, AO, GI, Reflections, Ray Resolution
        self.build_tier_row(tree, inner_x, cy, realtime_x, realtime_w, export_x, export_w, "Shadows", TierField::Shadows);
        cy += ROW_H + ROW_GAP;
        self.build_tier_row(tree, inner_x, cy, realtime_x, realtime_w, export_x, export_w, "AO", TierField::Ao);
        cy += ROW_H + ROW_GAP;
        self.build_tier_row(tree, inner_x, cy, realtime_x, realtime_w, export_x, export_w, "GI", TierField::Gi);
        cy += ROW_H + ROW_GAP;
        self.build_tier_row(tree, inner_x, cy, realtime_x, realtime_w, export_x, export_w, "Reflections", TierField::Reflections);
        cy += ROW_H + ROW_GAP;
        self.build_resolution_row(tree, inner_x, cy, realtime_x, realtime_w, export_x, export_w);
    }

    /// Build one tier row (shadows/AO/GI/reflections) with two dropdown columns.
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
        field: TierField,
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

        // Real-time dropdown
        let realtime_tier = field.get(&self.settings.realtime);
        let realtime_id = tree.add_button(
            Some(self.bg_id),
            realtime_x,
            y,
            realtime_w,
            ROW_H,
            dropdown_btn_style(BTN_FONT),
            realtime_tier.label(),
        );
        self.actions.push((realtime_id, build_tier_action(true, self.settings, field)));

        // Export dropdown
        let export_tier = field.get(&self.settings.export);
        let export_label = export_tier.label();
        let export_id = tree.add_button(
            Some(self.bg_id),
            export_x,
            y,
            export_w,
            ROW_H,
            dropdown_btn_style(BTN_FONT),
            export_label,
        );
        self.actions.push((export_id, build_tier_action(false, self.settings, field)));
    }

    /// Build the ray resolution row with fraction labels ("50% (half)", etc.).
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

        // Real-time resolution dropdown
        let realtime_res = self.settings.realtime.ray_resolution;
        let realtime_label = resolution_label(realtime_res);
        let realtime_id = tree.add_button(
            Some(self.bg_id),
            realtime_x,
            y,
            realtime_w,
            ROW_H,
            dropdown_btn_style(BTN_FONT),
            realtime_label,
        );
        self.actions.push((realtime_id, build_resolution_action(true, self.settings, realtime_res)));

        // Export resolution dropdown
        let export_res = self.settings.export.ray_resolution;
        let export_label = resolution_label(export_res);
        let export_id = tree.add_button(
            Some(self.bg_id),
            export_x,
            y,
            export_w,
            ROW_H,
            dropdown_btn_style(BTN_FONT),
            export_label,
        );
        self.actions.push((export_id, build_resolution_action(false, self.settings, export_res)));
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

fn dropdown_btn_style(font: u16) -> UIStyle {
    UIStyle {
        text_color: Color32::new(200, 200, 205, 255), // design-token-exempt: dropdown text color matches settings_popup precedent
        font_size: font,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}

// ── Action builders (cycle through tiers/resolutions) ──

/// Which tier row a click targets — by identity, never by matching the
/// current value (two rows sharing a tier would misroute the click).
#[derive(Clone, Copy, PartialEq, Eq)]
enum TierField {
    Shadows,
    Ao,
    Gi,
    Reflections,
}

impl TierField {
    fn get(self, col: &RtQualityColumn) -> RtQualityTier {
        match self {
            TierField::Shadows => col.shadows,
            TierField::Ao => col.ao,
            TierField::Gi => col.gi,
            TierField::Reflections => col.reflections,
        }
    }

    fn set(self, col: &mut RtQualityColumn, tier: RtQualityTier) {
        match self {
            TierField::Shadows => col.shadows = tier,
            TierField::Ao => col.ao = tier,
            TierField::Gi => col.gi = tier,
            TierField::Reflections => col.reflections = tier,
        }
    }
}

/// Build a PanelAction that cycles the given tier field to the next tier in
/// UI order. The dropdown shows the current tier; clicking cycles.
fn build_tier_action(
    is_realtime: bool,
    current_settings: RtQualitySettings,
    field: TierField,
) -> PanelAction {
    let current = field.get(if is_realtime {
        &current_settings.realtime
    } else {
        &current_settings.export
    });
    let current_idx = TIERS.iter().position(|&t| t == current).unwrap_or(0);
    let next_tier = TIERS[(current_idx + 1) % TIERS.len()];

    let mut new_settings = current_settings;
    let column = if is_realtime {
        &mut new_settings.realtime
    } else {
        &mut new_settings.export
    };
    field.set(column, next_tier);

    PanelAction::Project(ProjectAction::ChangeRtQuality(new_settings))
}

/// Build a PanelAction that cycles the ray resolution for the given column.
fn build_resolution_action(is_realtime: bool, current_settings: RtQualitySettings, current: RtRayResolution) -> PanelAction {
    let current_idx = RESOLUTIONS.iter().position(|&r| r == current).unwrap_or(0);
    let next_idx = (current_idx + 1) % RESOLUTIONS.len();
    let next_res = RESOLUTIONS[next_idx];

    let mut new_settings = current_settings;
    let target_column = if is_realtime {
        &mut new_settings.realtime
    } else {
        &mut new_settings.export
    };
    target_column.ray_resolution = next_res;

    PanelAction::Project(ProjectAction::ChangeRtQuality(new_settings))
}

/// Get the display label for a ray resolution variant.
fn resolution_label(res: RtRayResolution) -> &'static str {
    match res {
        RtRayResolution::Quarter => "25% (quarter)",
        RtRayResolution::Half => "50% (half)",
        RtRayResolution::ThreeQuarter => "75% (three-quarter)",
        RtRayResolution::Native => "100% (native)",
    }
}