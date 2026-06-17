//! Audio Setup panel — the central place to route audio in and manage the
//! named sends that the per-slider audio drawers reference.
//!
//! A floating modal (over the main UI) with an input-device picker and one row
//! per send: channel, gain, and delete, plus an "Add send" button. Self-
//! contained like [`super::browser_popup`]: it builds `UITree` nodes from data
//! handed in via [`AudioSetupPanel::configure`] and maps a clicked node id back
//! to a [`PanelAction`] (the project-level audio-setup actions, already routed
//! through `ui_bridge`). See `docs/AUDIO_MODULATION_DESIGN.md` §10.1.
//!
//! v1 scope: device cycle, add/remove send, per-send single-channel routing and
//! gain trim. Per-send labels are auto-assigned ("Audio N") until a text-field
//! rename lands; multi-channel downmix and the v2 analysis toggles are future.

use manifold_core::{AudioDeviceRef, AudioSendId};

use crate::color;
use crate::input::{Key, UIEvent};
use crate::node::*;
use crate::tree::UITree;

use super::PanelAction;
use super::overlay::{Anchor, Modality, Overlay, OverlayPlacement, OverlayResponse};

// ── Layout ──
const PANEL_W: f32 = 360.0;
const TITLE_H: f32 = 26.0;
const ROW_H: f32 = 24.0;
const ROW_GAP: f32 = 4.0;
const PAD: f32 = 10.0;
const STEP_W: f32 = 22.0;
const BTN_FONT: u16 = color::FONT_LABEL;

/// One send's display data, supplied by `configure`.
#[derive(Clone, Debug)]
pub struct AudioSendRow {
    pub id: AudioSendId,
    pub label: String,
    /// Routed channels (0-based). One channel = mono; two = a stereo pair.
    pub channels: Vec<u16>,
    /// Pre-resolved channel label for the trigger, e.g. "BH_IN_L", "BH_IN_L +
    /// BH_IN_R", or "Not routed". Resolved against the device directory by the
    /// data layer so the panel stays free of platform queries.
    pub channel_label: String,
}

/// Per-send interactive node ids.
#[derive(Default, Clone)]
struct SendRowIds {
    label: i32,
    ch_dropdown: i32,
    stereo: i32,
    delete: i32,
}

/// The Audio Setup modal panel.
#[derive(Default)]
pub struct AudioSetupPanel {
    open: bool,
    // Configured data.
    current_device: Option<AudioDeviceRef>,
    sends: Vec<AudioSendRow>,
    /// A reliability warning to surface below the device row (device offline,
    /// mic permission blocked), or `None` when all is well.
    status_warning: Option<String>,
    // Node ids (set by `build`).
    bg_id: i32,
    close_id: i32,
    device_dropdown_id: i32,
    add_send_id: i32,
    send_ids: Vec<SendRowIds>,
}

impl AudioSetupPanel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// The currently selected input device (`None` = system default). The app
    /// reads this to scope the channel dropdown to the device's channels,
    /// resolving by UID with a name fallback.
    pub fn current_device(&self) -> Option<&AudioDeviceRef> {
        self.current_device.as_ref()
    }

    /// Update the data the panel renders. Called from `state_sync` on a
    /// structural sync while the panel is open. The device list itself is
    /// enumerated lazily by the app when the device dropdown opens, so it isn't
    /// passed here.
    pub fn configure(
        &mut self,
        current_device: Option<AudioDeviceRef>,
        sends: Vec<AudioSendRow>,
        status_warning: Option<String>,
    ) {
        self.current_device = current_device;
        self.sends = sends;
        self.status_warning = status_warning;
    }

    fn device_label(&self) -> String {
        self.current_device
            .as_ref()
            .map(|d| d.name.clone())
            .unwrap_or_else(|| "System Default".to_string())
    }

    /// Total body height for the configured send count.
    fn body_height(&self) -> f32 {
        let rows = self.sends.len();
        let warning = if self.status_warning.is_some() { ROW_H + ROW_GAP } else { 0.0 };
        TITLE_H
            + ROW_H // device row
            + ROW_GAP
            + warning
            + (ROW_H + ROW_GAP) * rows as f32
            + ROW_H // add-send button
            + PAD * 2.0
    }

    /// Build the modal's nodes, centered in a `(width, height)` viewport.
    pub fn build(&mut self, tree: &mut UITree, viewport_w: f32, viewport_h: f32) {
        if !self.open {
            return;
        }
        let body_h = self.body_height();
        let x = ((viewport_w - PANEL_W) * 0.5).max(0.0);
        let y = ((viewport_h - body_h) * 0.5).max(0.0);
        self.build_nodes(tree, x, y);
    }

    /// Build the modal's nodes with the panel's top-left at `(x, y)`.
    fn build_nodes(&mut self, tree: &mut UITree, x: f32, y: f32) {
        let rows = self.sends.len();
        let body_h = self.body_height();
        self.bg_id = tree.add_panel(
            -1,
            x,
            y,
            PANEL_W,
            body_h,
            UIStyle {
                bg_color: Color32::new(19, 19, 22, 250),
                border_color: Color32::new(48, 48, 52, 255),
                border_width: 1.0,
                corner_radius: 6.0,
                ..UIStyle::default()
            },
        ) as i32;

        let inner_x = x + PAD;
        let inner_w = PANEL_W - PAD * 2.0;
        let mut cy = y + PAD;

        // Title + close.
        tree.add_label(
            self.bg_id,
            inner_x,
            cy,
            inner_w - STEP_W,
            TITLE_H,
            "Audio Setup",
            UIStyle {
                text_color: Color32::new(224, 224, 228, 255),
                font_size: color::FONT_BODY,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        self.close_id = tree.add_button(
            self.bg_id,
            inner_x + inner_w - STEP_W,
            cy,
            STEP_W,
            TITLE_H,
            btn_style(false),
            "\u{00D7}", // × close
        ) as i32;
        cy += TITLE_H;

        // Device row: [Device]  [ current device            ▼ ]
        tree.add_label(self.bg_id, inner_x, cy, 70.0, ROW_H, "Device", label_style());
        self.device_dropdown_id = tree.add_button(
            self.bg_id,
            inner_x + 74.0,
            cy,
            inner_w - 74.0,
            ROW_H,
            dropdown_trigger_style(),
            &format!("{}   \u{25BC}", self.device_label()), // value … ▼
        ) as i32;
        cy += ROW_H + ROW_GAP;

        // Reliability warning (device offline / mic blocked), if any.
        if let Some(warning) = &self.status_warning {
            tree.add_label(
                self.bg_id,
                inner_x,
                cy,
                inner_w,
                ROW_H,
                warning,
                UIStyle {
                    text_color: Color32::new(232, 168, 92, 255), // amber
                    font_size: color::FONT_LABEL,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            cy += ROW_H + ROW_GAP;
        }

        // Send rows: [swatch] label | [ channel name ▼ ] | ×
        const SWATCH_W: f32 = 8.0;
        const LABEL_W: f32 = 70.0;
        self.send_ids = vec![SendRowIds::default(); rows];
        for (i, send) in self.sends.iter().enumerate() {
            // Identity color swatch.
            tree.add_panel(
                self.bg_id,
                inner_x,
                cy + (ROW_H - 12.0) * 0.5,
                SWATCH_W,
                12.0,
                UIStyle {
                    bg_color: super::audio_send_color(&send.id),
                    corner_radius: 2.0,
                    ..UIStyle::default()
                },
            );

            // Label is a button — clicking it opens the inline rename editor.
            let label_x = inner_x + SWATCH_W + 6.0;
            self.send_ids[i].label = tree.add_button(
                self.bg_id,
                label_x,
                cy,
                LABEL_W,
                ROW_H,
                label_button_style(),
                &send.label,
            ) as i32;

            // Delete (right-aligned), then mono/stereo toggle to its left.
            self.send_ids[i].delete = tree.add_button(
                self.bg_id,
                inner_x + inner_w - STEP_W,
                cy,
                STEP_W,
                ROW_H,
                btn_style(false),
                "\u{00D7}",
            ) as i32;

            let stereo_on = send.channels.len() >= 2;
            const STEREO_W: f32 = 30.0;
            let stereo_x = inner_x + inner_w - STEP_W - 4.0 - STEREO_W;
            self.send_ids[i].stereo = tree.add_button(
                self.bg_id,
                stereo_x,
                cy,
                STEREO_W,
                ROW_H,
                btn_style(stereo_on),
                if stereo_on { "St" } else { "Mo" },
            ) as i32;

            // Channel dropdown fills the gap, showing the resolved name(s).
            let ch_x = label_x + LABEL_W + 4.0;
            self.send_ids[i].ch_dropdown = tree.add_button(
                self.bg_id,
                ch_x,
                cy,
                (stereo_x - 4.0 - ch_x).max(40.0),
                ROW_H,
                dropdown_trigger_style(),
                &format!("{}   \u{25BC}", send.channel_label),
            ) as i32;
            cy += ROW_H + ROW_GAP;
        }

        // Add-send button.
        self.add_send_id = tree.add_button(
            self.bg_id,
            inner_x,
            cy,
            inner_w,
            ROW_H,
            btn_style(false),
            "+ Add Send",
        ) as i32;
    }

    /// Whether `id` is any node this panel owns (background or an interactive
    /// control) — the caller swallows such clicks so they don't fall through to
    /// the canvas behind the modal.
    pub fn owns_node(&self, id: i32) -> bool {
        if id == self.bg_id
            || id == self.close_id
            || id == self.device_dropdown_id
            || id == self.add_send_id
        {
            return true;
        }
        self.send_ids
            .iter()
            .any(|r| id == r.label || id == r.ch_dropdown || id == r.stereo || id == r.delete)
    }

    /// Whether a send is currently routed as a stereo pair (≥2 channels).
    pub fn is_send_stereo(&self, id: &AudioSendId) -> bool {
        self.sends
            .iter()
            .find(|s| &s.id == id)
            .is_some_and(|s| s.channels.len() >= 2)
    }

    /// Screen rect of a send's label button (the inline-rename anchor), or
    /// `None` if the send isn't currently built.
    pub fn send_label_rect(&self, tree: &UITree, id: &AudioSendId) -> Option<Rect> {
        let i = self.sends.iter().position(|s| &s.id == id)?;
        let node = self.send_ids.get(i)?;
        (node.label >= 0).then(|| tree.get_bounds(node.label as u32))
    }

    /// Resolve a clicked node id to a [`PanelAction`], or `None` if it hit
    /// nothing interactive. Closing the panel is handled here (returns `None`
    /// after toggling closed) so the caller just dispatches the action.
    pub fn handle_click(&mut self, id: i32) -> Option<PanelAction> {
        if id == self.close_id {
            self.open = false;
            return None;
        }
        self.handle_click_inner(id)
    }

    fn handle_click_inner(&self, id: i32) -> Option<PanelAction> {
        if id == self.device_dropdown_id {
            // App opens the device dropdown anchored to this trigger.
            return Some(PanelAction::AudioSetupDeviceClicked);
        }
        if id == self.add_send_id {
            return Some(PanelAction::AudioAddSend);
        }
        for (i, ids) in self.send_ids.iter().enumerate() {
            let send = &self.sends[i];
            if id == ids.label {
                // App opens the inline rename editor anchored to this label.
                return Some(PanelAction::AudioSendLabelClicked(send.id.clone()));
            }
            if id == ids.delete {
                return Some(PanelAction::AudioRemoveSend(send.id.clone()));
            }
            if id == ids.ch_dropdown {
                // App opens the channel dropdown anchored to this trigger.
                return Some(PanelAction::AudioSendChannelClicked(send.id.clone()));
            }
            if id == ids.stereo {
                return Some(PanelAction::AudioSendStereoToggle(send.id.clone()));
            }
        }
        None
    }
}

impl Overlay for AudioSetupPanel {
    fn is_open(&self) -> bool {
        self.open
    }

    fn modality(&self) -> Modality {
        Modality::Modal { dim_background: true }
    }

    fn anchor(&self) -> Anchor {
        Anchor::Centered
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
                let id = *node_id as i32;
                if id == self.close_id {
                    self.open = false;
                    OverlayResponse::Consumed(Vec::new())
                } else if let Some(action) = self.handle_click_inner(id) {
                    OverlayResponse::Consumed(vec![action])
                } else if self.owns_node(id) {
                    // Panel background or a non-action control — swallow, stay open.
                    OverlayResponse::Consumed(Vec::new())
                } else {
                    // Click landed on the dim backdrop / outside the panel — close.
                    self.open = false;
                    OverlayResponse::Consumed(Vec::new())
                }
            }
            _ => OverlayResponse::Ignored,
        }
    }
}

fn btn_style(active: bool) -> UIStyle {
    UIStyle {
        bg_color: if active {
            Color32::new(46, 46, 52, 255)
        } else {
            Color32::new(36, 36, 40, 255)
        },
        hover_bg_color: Color32::new(51, 51, 58, 255),
        pressed_bg_color: Color32::new(28, 28, 32, 255),
        text_color: Color32::new(210, 210, 216, 255),
        font_size: BTN_FONT,
        corner_radius: 2.0,
        text_align: TextAlign::Center,
        ..UIStyle::default()
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

/// The send-name button — looks like a label, hovers like an editable field.
fn label_button_style() -> UIStyle {
    UIStyle {
        bg_color: Color32::new(0, 0, 0, 0),
        hover_bg_color: Color32::new(44, 44, 50, 255),
        pressed_bg_color: Color32::new(30, 30, 34, 255),
        text_color: Color32::new(214, 214, 220, 255),
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        corner_radius: 2.0,
        ..UIStyle::default()
    }
}

/// A dropdown trigger — a bordered field showing the current value with a ▼
/// caret, the standard "click to choose" affordance.
fn dropdown_trigger_style() -> UIStyle {
    UIStyle {
        bg_color: Color32::new(30, 30, 34, 255),
        hover_bg_color: Color32::new(44, 44, 50, 255),
        pressed_bg_color: Color32::new(26, 26, 30, 255),
        text_color: Color32::new(214, 214, 220, 255),
        border_color: Color32::new(58, 58, 64, 255),
        border_width: 1.0,
        corner_radius: 3.0,
        font_size: BTN_FONT,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel_with_two_sends() -> AudioSetupPanel {
        let mut p = AudioSetupPanel::new();
        p.toggle(); // open
        p.configure(
            None,
            vec![
                AudioSendRow {
                    id: AudioSendId::new("s1"),
                    label: "Audio 1".into(),
                    channels: vec![0],
                    channel_label: "Channel 1".into(),
                },
                AudioSendRow {
                    id: AudioSendId::new("s2"),
                    label: "Audio 2".into(),
                    channels: vec![2],
                    channel_label: "MacBook Mic".into(),
                },
            ],
            None,
        );
        p
    }

    #[test]
    fn clicks_resolve_to_actions() {
        let mut p = panel_with_two_sends();
        let mut tree = UITree::new();
        p.build(&mut tree, 1280.0, 720.0);

        // Add send.
        assert!(matches!(p.handle_click(p.add_send_id), Some(PanelAction::AudioAddSend)));

        // Device trigger opens the device dropdown (app builds the list).
        assert!(matches!(
            p.handle_click(p.device_dropdown_id),
            Some(PanelAction::AudioSetupDeviceClicked)
        ));

        // Channel trigger on send 2 opens its channel dropdown.
        let ch_dropdown = p.send_ids[1].ch_dropdown;
        match p.handle_click(ch_dropdown) {
            Some(PanelAction::AudioSendChannelClicked(id)) => assert_eq!(id.as_str(), "s2"),
            other => panic!("expected channel dropdown open, got {other:?}"),
        }

        // Delete send 1.
        let del = p.send_ids[0].delete;
        assert!(matches!(p.handle_click(del), Some(PanelAction::AudioRemoveSend(_))));

        // Close button toggles closed and yields no action.
        assert!(p.handle_click(p.close_id).is_none());
        assert!(!p.is_open());
    }

    #[test]
    fn overlay_escape_self_closes() {
        let mut p = panel_with_two_sends();
        let mut tree = UITree::new();
        let resp = p.on_event(
            &UIEvent::KeyDown {
                node_id: 0,
                key: Key::Escape,
                modifiers: crate::input::Modifiers::default(),
            },
            &mut tree,
        );
        assert!(matches!(resp, OverlayResponse::Consumed(_)));
        assert!(!p.is_open(), "Escape should self-close the modal");
    }
}
