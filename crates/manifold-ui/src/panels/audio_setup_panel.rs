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
/// Minimum panel width (small screens / compact fallback). The modal targets a
/// fraction of the viewport (see [`AudioSetupPanel::resize_to_viewport`]) but
/// never shrinks below this.
const PANEL_W_MIN: f32 = 460.0;
/// Fraction of the viewport the enlarged modal fills, width and height.
const PANEL_W_FRAC: f32 = 0.8;
const PANEL_H_FRAC: f32 = 0.8;
const TITLE_H: f32 = 26.0;
const ROW_H: f32 = 24.0;
const ROW_GAP: f32 = 4.0;
const PAD: f32 = 10.0;
const STEP_W: f32 = 22.0;
const BTN_FONT: u16 = color::FONT_LABEL;
/// Gain stepper geometry: [−] value [＋].
const GAIN_BTN_W: f32 = 16.0;
const GAIN_VAL_W: f32 = 50.0;
const GAIN_W: f32 = GAIN_BTN_W * 2.0 + GAIN_VAL_W;

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
    /// Input gain trim in decibels (0 = unity). Shown on the row's −/＋ stepper.
    pub gain_db: f32,
    /// Number of parameters this send currently drives. Surfaced on the row and
    /// gates a confirm-before-delete so a bound send isn't silently severed.
    pub driven_count: usize,
}

/// Height of the spectrogram scope section (title + waterfall).
const SCOPE_TITLE_H: f32 = 18.0;
/// Minimum waterfall height. When the modal is enlarged to the viewport
/// fraction, the scope absorbs all the extra vertical space above this floor.
const SCOPE_H_MIN: f32 = 200.0;
/// Left margin inside the scope for the frequency-axis labels.
const SCOPE_AXIS_W: f32 = 34.0;

/// Per-send interactive node ids.
#[derive(Default, Clone)]
struct SendRowIds {
    /// Identity-colour swatch — clicking it selects the send for the scope.
    swatch: i32,
    label: i32,
    ch_dropdown: i32,
    gain_minus: i32,
    gain_plus: i32,
    stereo: i32,
    delete: i32,
    /// Level-meter fill node + its full-scale geometry, resized in place each
    /// frame by [`AudioSetupPanel::update_meters`] (no rebuild).
    meter_fill: i32,
    meter_x: f32,
    meter_y: f32,
    meter_w: f32,
    meter_h: f32,
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
    /// Send whose delete button is armed for confirmation (it drives params, so
    /// the first click arms and the second confirms). Cleared on any other click
    /// or on close.
    delete_armed: Option<AudioSendId>,
    /// Send whose spectrogram the scope is showing. Defaults to the first send;
    /// clicking a row's swatch reselects. Read by the app each frame to drive
    /// the worker's column producer and the waterfall.
    selected_send: Option<AudioSendId>,
    /// Screen-space rect of the waterfall image (logical units), set by `build`.
    /// The present pass blits the spectrogram texture here. `None` when closed.
    scope_rect: Option<Rect>,
    /// Resolved panel width and waterfall height for the current viewport, set by
    /// [`AudioSetupPanel::resize_to_viewport`]. The modal targets
    /// [`PANEL_W_FRAC`]×[`PANEL_H_FRAC`] of the screen; the control rows are
    /// fixed-height, so the scope absorbs the extra vertical space.
    panel_w: f32,
    scope_h: f32,
    // Node ids (set by `build`).
    bg_id: i32,
    close_id: i32,
    device_dropdown_id: i32,
    add_send_id: i32,
    send_ids: Vec<SendRowIds>,
}

impl AudioSetupPanel {
    pub fn new() -> Self {
        Self {
            panel_w: PANEL_W_MIN,
            scope_h: SCOPE_H_MIN,
            ..Self::default()
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.delete_armed = None;
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

    /// The single notice line shown below the device row: a delete-confirm
    /// prompt (takes priority while a delete is armed), else a reliability
    /// warning (device offline / mic blocked), else nothing.
    fn active_notice(&self) -> Option<String> {
        if let Some(id) = &self.delete_armed
            && let Some(send) = self.sends.iter().find(|s| &s.id == id)
        {
            return Some(format!(
                "\u{26A0} Delete \"{}\"? It drives {} param{} — click \u{00D7} again",
                send.label,
                send.driven_count,
                if send.driven_count == 1 { "" } else { "s" },
            ));
        }
        self.status_warning.clone()
    }

    /// Body height of everything except the waterfall image: title, device row,
    /// notice, send rows, add-send button, scope title, and padding. The scope's
    /// waterfall (`self.scope_h`) is added on top by [`body_height`].
    fn chrome_height(&self) -> f32 {
        let rows = self.sends.len();
        let warning = if self.active_notice().is_some() { ROW_H + ROW_GAP } else { 0.0 };
        let scope_chrome = if self.sends.is_empty() { 0.0 } else { ROW_GAP + SCOPE_TITLE_H };
        TITLE_H
            + ROW_H // device row
            + ROW_GAP
            + warning
            + (ROW_H + ROW_GAP) * rows as f32
            + ROW_H // add-send button
            + scope_chrome
            + PAD * 2.0
    }

    /// Total body height for the configured send count.
    fn body_height(&self) -> f32 {
        let scope = if self.sends.is_empty() { 0.0 } else { self.scope_h };
        self.chrome_height() + scope
    }

    /// Resolve [`panel_w`](Self::panel_w)/[`scope_h`](Self::scope_h) to the
    /// target viewport fraction, never below the compact minimums. The control
    /// rows are fixed-height, so all the extra height goes to the waterfall.
    fn resize_to_viewport(&mut self, viewport_w: f32, viewport_h: f32) {
        self.panel_w = (viewport_w * PANEL_W_FRAC).max(PANEL_W_MIN);
        let target_h = viewport_h * PANEL_H_FRAC;
        self.scope_h = (target_h - self.chrome_height()).max(SCOPE_H_MIN);
    }

    /// Build the modal's nodes, centered in a `(width, height)` viewport.
    pub fn build(&mut self, tree: &mut UITree, viewport_w: f32, viewport_h: f32) {
        if !self.open {
            return;
        }
        self.resize_to_viewport(viewport_w, viewport_h);
        let body_h = self.body_height();
        let x = ((viewport_w - self.panel_w) * 0.5).max(0.0);
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
            self.panel_w,
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
        let inner_w = self.panel_w - PAD * 2.0;
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

        // Notice line: delete-confirm prompt or reliability warning, if any.
        if let Some(warning) = &self.active_notice() {
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

        // Default the scope selection to the first send; keep it if still valid.
        if !self.sends.iter().any(|s| Some(&s.id) == self.selected_send.as_ref()) {
            self.selected_send = self.sends.first().map(|s| s.id.clone());
        }

        // Send rows: [swatch] label | [ channel name ▼ ] | ×
        const SWATCH_W: f32 = 8.0;
        const LABEL_W: f32 = 70.0;
        self.send_ids = vec![SendRowIds::default(); rows];
        for (i, send) in self.sends.iter().enumerate() {
            // Identity-colour swatch — a button that selects this send for the
            // scope. The selected row's swatch fills the row height; others are a
            // small dot.
            let selected = Some(&send.id) == self.selected_send.as_ref();
            let (swatch_h, swatch_y) = if selected {
                (ROW_H, cy)
            } else {
                (12.0, cy + (ROW_H - 12.0) * 0.5)
            };
            self.send_ids[i].swatch = tree.add_button(
                self.bg_id,
                inner_x,
                swatch_y,
                SWATCH_W,
                swatch_h,
                UIStyle {
                    bg_color: super::audio_send_color(&send.id),
                    hover_bg_color: super::audio_send_color(&send.id),
                    corner_radius: 2.0,
                    ..UIStyle::default()
                },
                "",
            ) as i32;

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

            // Delete (right-aligned). Armed (awaiting confirm) shows a warning
            // glyph; an in-use send tints amber as a "this drives params" cue.
            let armed = self.delete_armed.as_ref() == Some(&send.id);
            let delete_label = if armed { "!" } else { "\u{00D7}" };
            let mut del_style = btn_style(false);
            if armed {
                del_style.text_color = Color32::new(236, 110, 110, 255); // red
            } else if send.driven_count > 0 {
                del_style.text_color = Color32::new(232, 168, 92, 255); // amber
            }
            self.send_ids[i].delete = tree.add_button(
                self.bg_id,
                inner_x + inner_w - STEP_W,
                cy,
                STEP_W,
                ROW_H,
                del_style,
                delete_label,
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

            // Gain stepper [−] value [＋], left of the stereo toggle. Discrete
            // 1 dB steps; the value is read-only display (0 dB = unity).
            let gain_x = stereo_x - 4.0 - GAIN_W;
            self.send_ids[i].gain_minus = tree.add_button(
                self.bg_id,
                gain_x,
                cy,
                GAIN_BTN_W,
                ROW_H,
                btn_style(false),
                "\u{2212}", // −
            ) as i32;
            tree.add_label(
                self.bg_id,
                gain_x + GAIN_BTN_W,
                cy,
                GAIN_VAL_W,
                ROW_H,
                &format_gain_db(send.gain_db),
                UIStyle {
                    text_color: Color32::new(190, 190, 198, 255),
                    font_size: color::FONT_LABEL,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
            );
            self.send_ids[i].gain_plus = tree.add_button(
                self.bg_id,
                gain_x + GAIN_BTN_W + GAIN_VAL_W,
                cy,
                GAIN_BTN_W,
                ROW_H,
                btn_style(false),
                "\u{002B}", // +
            ) as i32;

            // Channel dropdown fills the gap, showing the resolved name(s).
            let ch_x = label_x + LABEL_W + 4.0;
            let ch_w = (gain_x - 4.0 - ch_x).max(40.0);
            self.send_ids[i].ch_dropdown = tree.add_button(
                self.bg_id,
                ch_x,
                cy,
                ch_w,
                ROW_H,
                dropdown_trigger_style(),
                &format!("{}   \u{25BC}", send.channel_label),
            ) as i32;

            // Level meter: a thin track under the channel dropdown with a fill
            // node resized each frame from the live send level. Identity-colored.
            let meter_h = 2.0;
            let meter_x = ch_x;
            let meter_y = cy + ROW_H - meter_h;
            let meter_w = ch_w;
            tree.add_panel(
                self.bg_id,
                meter_x,
                meter_y,
                meter_w,
                meter_h,
                UIStyle { bg_color: Color32::new(40, 40, 46, 255), ..UIStyle::default() },
            );
            let fill = tree.add_panel(
                self.bg_id,
                meter_x,
                meter_y,
                0.0, // width set per frame by update_meters
                meter_h,
                UIStyle { bg_color: super::audio_send_color(&send.id), ..UIStyle::default() },
            ) as i32;
            self.send_ids[i].meter_fill = fill;
            self.send_ids[i].meter_x = meter_x;
            self.send_ids[i].meter_y = meter_y;
            self.send_ids[i].meter_w = meter_w;
            self.send_ids[i].meter_h = meter_h;

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
        cy += ROW_H;

        // ── Spectrogram scope (selected send) ──
        self.scope_rect = None;
        if !self.sends.is_empty() {
            cy += ROW_GAP;
            let sel_label = self
                .selected_send
                .as_ref()
                .and_then(|id| self.sends.iter().find(|s| &s.id == id))
                .map(|s| s.label.as_str())
                .unwrap_or("—");
            tree.add_label(
                self.bg_id,
                inner_x,
                cy,
                inner_w,
                SCOPE_TITLE_H,
                &format!("Spectrogram — {sel_label}"),
                UIStyle {
                    text_color: Color32::new(170, 170, 180, 255),
                    font_size: color::FONT_LABEL,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            cy += SCOPE_TITLE_H;

            // Backing panel behind the whole scope (axis margin + waterfall).
            tree.add_panel(
                self.bg_id,
                inner_x,
                cy,
                inner_w,
                self.scope_h,
                UIStyle {
                    bg_color: Color32::new(10, 10, 12, 255),
                    border_color: Color32::new(48, 48, 52, 255),
                    border_width: 1.0,
                    corner_radius: 3.0,
                    ..UIStyle::default()
                },
            );

            // Frequency-axis tick labels in the left margin (log scale: the
            // present pass draws the waterfall to the right of this margin).
            // Range must track `manifold_spectral::SpectrogramConfig` defaults
            // (10 Hz–22 kHz); ticks match the Analyzer VST's axis.
            let (fmin, fmax) = (10.0_f32, 22_000.0_f32);
            for &(hz, txt) in &[
                (20.0, "20"),
                (50.0, "50"),
                (100.0, "100"),
                (200.0, "200"),
                (500.0, "500"),
                (1000.0, "1k"),
                (2000.0, "2k"),
                (5000.0, "5k"),
                (10_000.0, "10k"),
                (20_000.0, "20k"),
            ] {
                let yn = (hz / fmin).log2() / (fmax / fmin).log2();
                let ly = cy + self.scope_h * (1.0 - yn) - 6.0;
                tree.add_label(
                    self.bg_id,
                    inner_x + 2.0,
                    ly,
                    SCOPE_AXIS_W - 4.0,
                    12.0,
                    txt,
                    UIStyle {
                        text_color: Color32::new(120, 120, 130, 255),
                        font_size: color::FONT_LABEL,
                        text_align: TextAlign::Right,
                        ..UIStyle::default()
                    },
                );
            }

            // The waterfall image rect (logical) — the present pass blits the
            // spectrogram texture here, on top of the backing panel.
            self.scope_rect = Some(Rect::new(
                inner_x + SCOPE_AXIS_W,
                cy,
                inner_w - SCOPE_AXIS_W,
                self.scope_h,
            ));
        }
    }

    /// The send the scope is showing, if any.
    pub fn selected_send(&self) -> Option<&AudioSendId> {
        self.selected_send.as_ref()
    }

    /// Screen-space rect (logical units) the present pass blits the spectrogram
    /// texture into, or `None` when the panel is closed / has no sends.
    pub fn scope_rect(&self) -> Option<Rect> {
        self.open.then_some(self.scope_rect).flatten()
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
        self.send_ids.iter().any(|r| {
            id == r.swatch
                || id == r.label
                || id == r.ch_dropdown
                || id == r.gain_minus
                || id == r.gain_plus
                || id == r.stereo
                || id == r.delete
        })
    }

    /// Resize each send's meter fill from live levels (RMS 0..1). Called every
    /// frame while open — mutates existing nodes in place, no rebuild. Levels are
    /// indexed by send order. A small visual gain makes quiet signals legible.
    pub fn update_meters(&self, tree: &mut UITree, levels: &[f32]) {
        for (i, ids) in self.send_ids.iter().enumerate() {
            if ids.meter_fill < 0 {
                continue;
            }
            let level = levels.get(i).copied().unwrap_or(0.0);
            let shown = (level * 2.5).clamp(0.0, 1.0); // ~ -8 dB reaches full scale
            let w = ids.meter_w * shown;
            tree.set_bounds(
                ids.meter_fill as u32,
                Rect::new(ids.meter_x, ids.meter_y, w, ids.meter_h),
            );
        }
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

    fn handle_click_inner(&mut self, id: i32) -> Option<PanelAction> {
        if id == self.device_dropdown_id {
            self.delete_armed = None;
            // App opens the device dropdown anchored to this trigger.
            return Some(PanelAction::AudioSetupDeviceClicked);
        }
        if id == self.add_send_id {
            self.delete_armed = None;
            return Some(PanelAction::AudioAddSend);
        }
        // Find which send row + control was hit (clone out so we don't hold a
        // borrow across the delete-arm mutation).
        let hit = self.send_ids.iter().enumerate().find_map(|(i, ids)| {
            if id == ids.swatch {
                Some((i, RowControl::Select))
            } else if id == ids.label {
                Some((i, RowControl::Label))
            } else if id == ids.ch_dropdown {
                Some((i, RowControl::Channel))
            } else if id == ids.gain_minus {
                Some((i, RowControl::GainDown))
            } else if id == ids.gain_plus {
                Some((i, RowControl::GainUp))
            } else if id == ids.stereo {
                Some((i, RowControl::Stereo))
            } else if id == ids.delete {
                Some((i, RowControl::Delete))
            } else {
                None
            }
        });
        let (i, control) = hit?;
        let send_id = self.sends[i].id.clone();
        match control {
            RowControl::Select => {
                // Pure UI state — the app reads `selected_send()` each frame to
                // drive the scope + worker. Swallow (no command).
                self.delete_armed = None;
                self.selected_send = Some(send_id);
                None
            }
            RowControl::Label => {
                self.delete_armed = None;
                Some(PanelAction::AudioSendLabelClicked(send_id))
            }
            RowControl::Channel => {
                self.delete_armed = None;
                Some(PanelAction::AudioSendChannelClicked(send_id))
            }
            RowControl::GainDown => {
                self.delete_armed = None;
                Some(PanelAction::AudioSendGainStep(send_id, -1.0))
            }
            RowControl::GainUp => {
                self.delete_armed = None;
                Some(PanelAction::AudioSendGainStep(send_id, 1.0))
            }
            RowControl::Stereo => {
                self.delete_armed = None;
                Some(PanelAction::AudioSendStereoToggle(send_id))
            }
            RowControl::Delete => {
                // Confirm before deleting a send that still drives params: the
                // first click arms (re-render shows the prompt), the second
                // confirms. A send driving nothing deletes immediately.
                if self.sends[i].driven_count == 0
                    || self.delete_armed.as_ref() == Some(&send_id)
                {
                    self.delete_armed = None;
                    Some(PanelAction::AudioRemoveSend(send_id))
                } else {
                    self.delete_armed = Some(send_id);
                    None
                }
            }
        }
    }
}

/// Which interactive control of a send row was clicked.
enum RowControl {
    Select,
    Label,
    Channel,
    GainDown,
    GainUp,
    Stereo,
    Delete,
}

/// Format a send's gain trim for the row stepper. Unity reads "0 dB"; non-zero
/// shows a signed integer dB (steps are 1 dB).
fn format_gain_db(db: f32) -> String {
    if db.abs() < 0.05 {
        "0 dB".to_string()
    } else {
        format!("{db:+.0} dB")
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
        Vec2::new(self.panel_w, self.body_height())
    }

    fn build_at(&mut self, tree: &mut UITree, placement: OverlayPlacement) {
        if !self.open {
            return;
        }
        // Size to the viewport and self-center. The modal targets a screen
        // fraction larger than the driver's content-based placement rect, so we
        // recompute the centered origin from the full screen size here rather
        // than using `placement.rect` (which was laid out for `desired_size`).
        self.resize_to_viewport(placement.screen.x, placement.screen.y);
        let body_h = self.body_height();
        let x = ((placement.screen.x - self.panel_w) * 0.5).max(0.0);
        let y = ((placement.screen.y - body_h) * 0.5).max(0.0);
        self.build_nodes(tree, x, y);
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
                    gain_db: 0.0,
                    driven_count: 0,
                },
                AudioSendRow {
                    id: AudioSendId::new("s2"),
                    label: "Audio 2".into(),
                    channels: vec![2],
                    channel_label: "MacBook Mic".into(),
                    gain_db: 0.0,
                    driven_count: 0,
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
    fn swatch_click_selects_send_and_scope_rect_present() {
        let mut p = panel_with_two_sends();
        let mut tree = UITree::new();
        p.build(&mut tree, 1280.0, 720.0);

        // Defaults to the first send; the scope rect exists while open.
        assert_eq!(p.selected_send().map(|s| s.as_str()), Some("s1"));
        assert!(p.scope_rect().is_some());

        // Clicking send 2's swatch selects it (no PanelAction — pure UI state).
        assert!(p.handle_click(p.send_ids[1].swatch).is_none());
        assert_eq!(p.selected_send().map(|s| s.as_str()), Some("s2"));

        // Closed → no scope rect.
        p.close();
        assert!(p.scope_rect().is_none());
    }

    #[test]
    fn gain_buttons_emit_signed_steps() {
        let mut p = panel_with_two_sends();
        let mut tree = UITree::new();
        p.build(&mut tree, 1280.0, 720.0);

        match p.handle_click(p.send_ids[0].gain_plus) {
            Some(PanelAction::AudioSendGainStep(id, d)) => {
                assert_eq!(id.as_str(), "s1");
                assert_eq!(d, 1.0);
            }
            other => panic!("expected gain step +1, got {other:?}"),
        }
        match p.handle_click(p.send_ids[1].gain_minus) {
            Some(PanelAction::AudioSendGainStep(id, d)) => {
                assert_eq!(id.as_str(), "s2");
                assert_eq!(d, -1.0);
            }
            other => panic!("expected gain step -1, got {other:?}"),
        }
    }

    #[test]
    fn format_gain_db_unity_and_signed() {
        assert_eq!(format_gain_db(0.0), "0 dB");
        assert_eq!(format_gain_db(6.0), "+6 dB");
        assert_eq!(format_gain_db(-3.0), "-3 dB");
    }

    #[test]
    fn in_use_send_delete_requires_confirm() {
        let mut p = panel_with_two_sends();
        // Mark send 1 as driving two params.
        p.sends[0].driven_count = 2;
        let mut tree = UITree::new();
        p.build(&mut tree, 1280.0, 720.0);

        let del = p.send_ids[0].delete;
        // First click arms — no action, and the confirm notice appears.
        assert!(p.handle_click(del).is_none());
        assert!(p.active_notice().is_some_and(|n| n.contains("drives 2 params")));
        // Second click confirms the delete.
        assert!(matches!(p.handle_click(del), Some(PanelAction::AudioRemoveSend(_))));
        assert!(p.active_notice().is_none(), "arm cleared after confirm");
    }

    #[test]
    fn clicking_elsewhere_clears_delete_arm() {
        let mut p = panel_with_two_sends();
        p.sends[0].driven_count = 1;
        let mut tree = UITree::new();
        p.build(&mut tree, 1280.0, 720.0);

        assert!(p.handle_click(p.send_ids[0].delete).is_none()); // arm
        // Clicking the stereo toggle clears the arm instead of deleting.
        assert!(matches!(
            p.handle_click(p.send_ids[0].stereo),
            Some(PanelAction::AudioSendStereoToggle(_))
        ));
        assert!(p.active_notice().is_none());
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
