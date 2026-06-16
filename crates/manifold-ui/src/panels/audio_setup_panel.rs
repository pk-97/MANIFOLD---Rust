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

use manifold_core::AudioSendId;

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

/// Channel routing bounds for the steppers.
const MAX_CHANNEL: u16 = 63;
/// Gain trim bounds (dB) and step.
const GAIN_MIN: f32 = -24.0;
const GAIN_MAX: f32 = 24.0;
const GAIN_STEP: f32 = 1.0;

/// One send's display data, supplied by `configure`.
#[derive(Clone, Debug)]
pub struct AudioSendRow {
    pub id: AudioSendId,
    pub label: String,
    /// First routed channel (the panel edits a single channel per send in v1).
    pub channel: u16,
    pub gain_db: f32,
}

/// Per-send interactive node ids.
#[derive(Default, Clone)]
struct SendRowIds {
    ch_minus: i32,
    ch_plus: i32,
    gain_minus: i32,
    gain_plus: i32,
    delete: i32,
}

/// The Audio Setup modal panel.
#[derive(Default)]
pub struct AudioSetupPanel {
    open: bool,
    // Configured data.
    devices: Vec<String>,
    current_device: Option<String>,
    sends: Vec<AudioSendRow>,
    // Node ids (set by `build`).
    bg_id: i32,
    close_id: i32,
    device_prev_id: i32,
    device_next_id: i32,
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

    /// Update the data the panel renders. Called each frame from `state_sync`
    /// while the panel is open.
    pub fn configure(
        &mut self,
        devices: Vec<String>,
        current_device: Option<String>,
        sends: Vec<AudioSendRow>,
    ) {
        self.devices = devices;
        self.current_device = current_device;
        self.sends = sends;
    }

    /// The device-cycle ring: `None` (system default) then each enumerated
    /// device. Cycling wraps through it.
    fn device_ring(&self) -> Vec<Option<String>> {
        let mut ring = vec![None];
        ring.extend(self.devices.iter().cloned().map(Some));
        ring
    }

    /// The next/previous device in the ring relative to the current selection.
    fn cycled_device(&self, forward: bool) -> Option<String> {
        let ring = self.device_ring();
        let cur = ring.iter().position(|d| *d == self.current_device).unwrap_or(0);
        let n = ring.len();
        let idx = if forward { (cur + 1) % n } else { (cur + n - 1) % n };
        ring[idx].clone()
    }

    fn device_label(&self) -> String {
        self.current_device.clone().unwrap_or_else(|| "System Default".to_string())
    }

    /// Total body height for the configured send count.
    fn body_height(&self) -> f32 {
        let rows = self.sends.len();
        TITLE_H
            + ROW_H // device row
            + ROW_GAP
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
            "\u{2715}", // ✕
        ) as i32;
        cy += TITLE_H;

        // Device row: ◂ [name] ▸
        tree.add_label(
            self.bg_id,
            inner_x,
            cy,
            70.0,
            ROW_H,
            "Device",
            label_style(),
        );
        self.device_prev_id = tree.add_button(
            self.bg_id,
            inner_x + 74.0,
            cy,
            STEP_W,
            ROW_H,
            btn_style(false),
            "\u{25C2}", // ◂
        ) as i32;
        tree.add_label(
            self.bg_id,
            inner_x + 74.0 + STEP_W,
            cy,
            inner_w - 74.0 - STEP_W * 2.0,
            ROW_H,
            &self.device_label(),
            value_style(),
        );
        self.device_next_id = tree.add_button(
            self.bg_id,
            inner_x + inner_w - STEP_W,
            cy,
            STEP_W,
            ROW_H,
            btn_style(false),
            "\u{25B8}", // ▸
        ) as i32;
        cy += ROW_H + ROW_GAP;

        // Send rows: label | Ch ◂ N ▸ | Gain ◂ dB ▸ | ✕
        self.send_ids = vec![SendRowIds::default(); rows];
        for (i, send) in self.sends.iter().enumerate() {
            let mut rx = inner_x;
            tree.add_label(self.bg_id, rx, cy, 70.0, ROW_H, &send.label, label_style());
            rx += 74.0;

            // Channel stepper.
            self.send_ids[i].ch_minus =
                tree.add_button(self.bg_id, rx, cy, STEP_W, ROW_H, btn_style(false), "\u{25C2}") as i32;
            rx += STEP_W;
            tree.add_label(
                self.bg_id,
                rx,
                cy,
                42.0,
                ROW_H,
                &format!("Ch {}", send.channel),
                value_style(),
            );
            rx += 42.0;
            self.send_ids[i].ch_plus =
                tree.add_button(self.bg_id, rx, cy, STEP_W, ROW_H, btn_style(false), "\u{25B8}") as i32;
            rx += STEP_W + 8.0;

            // Gain stepper.
            self.send_ids[i].gain_minus =
                tree.add_button(self.bg_id, rx, cy, STEP_W, ROW_H, btn_style(false), "\u{25C2}") as i32;
            rx += STEP_W;
            tree.add_label(
                self.bg_id,
                rx,
                cy,
                52.0,
                ROW_H,
                &format!("{:+.0} dB", send.gain_db),
                value_style(),
            );
            rx += 52.0;
            self.send_ids[i].gain_plus =
                tree.add_button(self.bg_id, rx, cy, STEP_W, ROW_H, btn_style(false), "\u{25B8}") as i32;

            // Delete (right-aligned).
            self.send_ids[i].delete = tree.add_button(
                self.bg_id,
                inner_x + inner_w - STEP_W,
                cy,
                STEP_W,
                ROW_H,
                btn_style(false),
                "\u{2715}",
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
            || id == self.device_prev_id
            || id == self.device_next_id
            || id == self.add_send_id
        {
            return true;
        }
        self.send_ids.iter().any(|r| {
            id == r.ch_minus
                || id == r.ch_plus
                || id == r.gain_minus
                || id == r.gain_plus
                || id == r.delete
        })
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
        if id == self.device_prev_id {
            return Some(PanelAction::AudioSetDevice(self.cycled_device(false)));
        }
        if id == self.device_next_id {
            return Some(PanelAction::AudioSetDevice(self.cycled_device(true)));
        }
        if id == self.add_send_id {
            return Some(PanelAction::AudioAddSend);
        }
        for (i, ids) in self.send_ids.iter().enumerate() {
            let send = &self.sends[i];
            if id == ids.delete {
                return Some(PanelAction::AudioRemoveSend(send.id.clone()));
            }
            if id == ids.ch_minus {
                let ch = send.channel.saturating_sub(1);
                return Some(PanelAction::AudioSetSendChannels(send.id.clone(), vec![ch]));
            }
            if id == ids.ch_plus {
                let ch = (send.channel + 1).min(MAX_CHANNEL);
                return Some(PanelAction::AudioSetSendChannels(send.id.clone(), vec![ch]));
            }
            if id == ids.gain_minus {
                let g = (send.gain_db - GAIN_STEP).max(GAIN_MIN);
                return Some(PanelAction::AudioSetSendGain(send.id.clone(), g));
            }
            if id == ids.gain_plus {
                let g = (send.gain_db + GAIN_STEP).min(GAIN_MAX);
                return Some(PanelAction::AudioSetSendGain(send.id.clone(), g));
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

    fn close(&mut self) {
        self.open = false;
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

fn value_style() -> UIStyle {
    UIStyle {
        text_color: Color32::new(210, 210, 216, 255),
        font_size: color::FONT_LABEL,
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
            vec!["BlackHole".into(), "Scarlett".into()],
            None,
            vec![
                AudioSendRow { id: AudioSendId::new("s1"), label: "Audio 1".into(), channel: 0, gain_db: 0.0 },
                AudioSendRow { id: AudioSendId::new("s2"), label: "Audio 2".into(), channel: 2, gain_db: -3.0 },
            ],
        );
        p
    }

    #[test]
    fn device_cycle_rings_through_default_and_devices() {
        let p = panel_with_two_sends(); // current = None (default)
        assert_eq!(p.cycled_device(true), Some("BlackHole".into()));
        assert_eq!(p.cycled_device(false), Some("Scarlett".into())); // wraps backward to last
    }

    #[test]
    fn clicks_resolve_to_actions() {
        let mut p = panel_with_two_sends();
        let mut tree = UITree::new();
        p.build(&mut tree, 1280.0, 720.0);

        // Add send.
        assert!(matches!(p.handle_click(p.add_send_id), Some(PanelAction::AudioAddSend)));

        // Channel + on send 2 (channel 2 → 3).
        let ch_plus = p.send_ids[1].ch_plus;
        match p.handle_click(ch_plus) {
            Some(PanelAction::AudioSetSendChannels(id, ch)) => {
                assert_eq!(id.as_str(), "s2");
                assert_eq!(ch, vec![3]);
            }
            other => panic!("expected channel set, got {other:?}"),
        }

        // Gain - on send 2 (-3 → -4).
        let gain_minus = p.send_ids[1].gain_minus;
        match p.handle_click(gain_minus) {
            Some(PanelAction::AudioSetSendGain(id, g)) => {
                assert_eq!(id.as_str(), "s2");
                assert!((g - (-4.0)).abs() < 1e-6);
            }
            other => panic!("expected gain set, got {other:?}"),
        }

        // Delete send 1.
        let del = p.send_ids[0].delete;
        assert!(matches!(p.handle_click(del), Some(PanelAction::AudioRemoveSend(_))));

        // Close button toggles closed and yields no action.
        assert!(p.handle_click(p.close_id).is_none());
        assert!(!p.is_open());
    }
}
