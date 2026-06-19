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

use manifold_core::audio_mod::AudioBand;
use manifold_core::{AudioDeviceRef, AudioSendId};

use crate::color;
use crate::input::{Key, UIEvent};
use crate::node::*;
use crate::tree::UITree;

use super::{BandDivider, PanelAction};
use super::overlay::{
    Anchor, Modality, Overlay, OverlayPlacement, OverlayResponse, SizePolicy, compute_overlay_rect,
};

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
    /// Compact source indicator for the read-only source chip: "Cap" (device
    /// only), "Cap+N" (device + N layers), a layer name, or "Off". Resolved by the
    /// data layer.
    pub source_label: String,
    /// Whether the send is fed by an audio layer (controls the source chip's
    /// accent so a mixed/layer send reads distinctly from a device-only send).
    pub layer_fed: bool,
    /// Human-readable routing lines for the read-only routings dropdown — the
    /// capture device (when channels are assigned) plus one line per feeding
    /// layer. Built by `state_sync`.
    pub routings: Vec<String>,
    /// Live audio → visual trigger rows for this send, one per band in
    /// [`AudioBand::ALL`] order (Whole/Low/Mid/High). Always four entries (a
    /// band with no route reads as a disabled default), so the inspector renders
    /// a fixed four-row matrix. Built by `state_sync` from the send's routes.
    pub triggers: Vec<TriggerRouteRow>,
}

/// One band's trigger row in the Audio Setup modal — the display state of a
/// (potential) [`TriggerRoute`](manifold_core::audio_trigger::TriggerRoute).
#[derive(Clone, Debug, Default)]
pub struct TriggerRouteRow {
    /// Whether a route exists for this band and is enabled.
    pub enabled: bool,
    /// 0..1 sensitivity (the route threshold), shown as a percent on the stepper.
    pub sensitivity: f32,
    /// The fire line in transient-impulse space (0..1), derived from
    /// `sensitivity` by core's `TriggerRoute::threshold()`. The row meter marks
    /// it so tuning is "does the level cross this line," not a blind percent.
    pub threshold: f32,
    /// Resolved target label: "Auto" (route by name) or a layer's name.
    pub layer_label: String,
}

/// Height of the spectrogram scope section (title + waterfall).
const SCOPE_TITLE_H: f32 = 18.0;
/// Minimum waterfall height. When the modal is enlarged to the viewport
/// fraction, the scope absorbs all the extra vertical space above this floor.
const SCOPE_H_MIN: f32 = 200.0;
/// Left margin inside the scope for the frequency-axis labels.
const SCOPE_AXIS_W: f32 = 34.0;
/// Right margin inside the scope for the per-band (Low/Mid/High) level meters.
const SCOPE_METER_W: f32 = 44.0;
/// Gap between the waterfall and the band-meter column.
const SCOPE_METER_GAP: f32 = 4.0;
/// Vertical inset of the waterfall inside the backing panel. The blit is a hard
/// rectangle (a generic viewport quad — no rounded corners), so without this its
/// square top/bottom corners sit flush on the panel's rounded, bordered edge and
/// read as a boxy clash. Insetting floats the waterfall as a clean rectangle
/// inside the rounded frame. The frequency axis + dividers derive from the same
/// inset rect (`scope_rect`), so the inset keeps them aligned.
const SCOPE_PAD_Y: f32 = 3.0;
/// Width of the L/M/H letter label left of each meter bar.
const BAND_METER_LABEL_W: f32 = 11.0;
/// Half-height of a band meter bar (px).
const BAND_METER_HALF_H: f32 = 5.0;
/// Per-band meter/tick colours — Low = red, Mid = green, High = blue. Shared
/// language with the spectrogram's colour-coded transient ticks.
fn band_color(band: usize) -> Color32 {
    match band {
        0 => Color32::new(255, 95, 80, 255),   // Low — red
        1 => Color32::new(90, 230, 120, 255),  // Mid — green
        _ => Color32::new(105, 160, 255, 255), // High — blue
    }
}
/// L/M/H labels in band order.
const BAND_METER_LABELS: [&str; 3] = ["L", "M", "H"];

/// Trigger-row band colour, indexed in `TRIG_BANDS` order: Whole (0) is neutral
/// (it's the whole-signal onset, not a frequency slab), Low/Mid/High reuse the
/// spectrogram's red/green/blue so the row meters read against the same legend.
fn trigger_band_color(row: usize) -> Color32 {
    match row {
        0 => Color32::new(190, 190, 200, 255), // Whole — neutral
        n => band_color(n - 1),                 // Low/Mid/High
    }
}

/// Scale a colour's RGB toward black by `f` (0..1), preserving alpha. Used to dim
/// a band colour for the resting meter fill so the firing flash reads brighter.
fn dim_color(c: Color32, f: f32) -> Color32 {
    Color32::new(
        (c.r as f32 * f) as u8,
        (c.g as f32 * f) as u8,
        (c.b as f32 * f) as u8,
        c.a,
    )
}

/// Trigger section geometry.
const TRIG_TITLE_H: f32 = 18.0;
const TRIG_ROW_H: f32 = 22.0;
/// Row layout widths: source label, sensitivity stepper [−] val [＋].
const TRIG_LABEL_W: f32 = 52.0;
const TRIG_ENABLE_W: f32 = 22.0;
const TRIG_SENS_BTN_W: f32 = 16.0;
const TRIG_SENS_VAL_W: f32 = 40.0;
const TRIG_ARROW_W: f32 = 16.0;
/// Per-row band, in `AudioBand::ALL` order, with its display label. `Full` reads
/// as "Whole" — the whole-signal onset for a separated stem.
const TRIG_BANDS: [(AudioBand, &str); 4] = [
    (AudioBand::Full, "Whole"),
    (AudioBand::Low, "Low"),
    (AudioBand::Mid, "Mid"),
    (AudioBand::High, "High"),
];

/// Per-trigger-row interactive node ids (one row per band).
#[derive(Default, Clone)]
struct TriggerRowIds {
    enable: i32,
    sens_minus: i32,
    sens_plus: i32,
    layer: i32,
    // Live level meter (resized in place each frame by `update_trigger_levels`):
    // a track + a band-coloured fill = the transient level, with a threshold
    // tick marking the fire line. `flash` is the swatch styled bright on a fire.
    meter_track: i32,
    meter_fill: i32,
    thresh_tick: i32,
    meter_x: f32,
    meter_y: f32,
    meter_w: f32,
    meter_h: f32,
    /// Band index (0..3) for colour + level lookup, and the row's threshold +
    /// enabled state, captured at build so the per-frame update is self-contained.
    band: usize,
    threshold: f32,
    enabled: bool,
}

/// Per-send interactive node ids.
#[derive(Default, Clone)]
struct SendRowIds {
    /// Identity-colour swatch — clicking it selects the send for the scope.
    swatch: i32,
    label: i32,
    /// Signal-source cycle button: capture channels ↔ an audio layer.
    source: i32,
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
    /// Right-aligned label in the scope title row showing the freq + dB under
    /// the cursor. Lives in the title strip (not over the waterfall, which the
    /// present pass blits on top), updated in place each frame by
    /// [`AudioSetupPanel::update_scope_readout`]. `-1` when not built.
    scope_readout_label: i32,
    /// Current Low/Mid/High crossovers (Hz) and the scope's analysed frequency
    /// range, pushed every frame by [`AudioSetupPanel::set_scope_bands`]. The
    /// divider lines are drawn shader-side; the panel keeps these only to
    /// hit-test a press against a line for dragging. `scope_fmin <= 0` means the
    /// scope is dark (no capture) — dividers aren't draggable then.
    scope_low_hz: f32,
    scope_mid_hz: f32,
    scope_fmin: f32,
    scope_fmax: f32,
    /// Which divider line is currently being dragged, if any.
    dragging_band: Option<BandDivider>,
    /// Per-band (Low/Mid/High) level-meter nodes `(track, fill, label)` in the
    /// scope's right margin. Created by `build`, repositioned + resized every
    /// frame by [`AudioSetupPanel::update_band_meters`] so they track the moving
    /// crossovers. `-1` when not built.
    band_meter_ids: [(i32, i32, i32); 3],
    /// Per-band trigger-row node ids for the selected send (Whole/Low/Mid/High
    /// order). Rebuilt with the panel; clicks map back via [`TRIG_BANDS`].
    trigger_row_ids: Vec<TriggerRowIds>,
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

    /// (id, label) for each named send, in declaration order. Used to populate
    /// the layer-header Send dropdown so it stays in lockstep with Audio Setup.
    pub fn send_options(&self) -> Vec<(AudioSendId, String)> {
        self.sends
            .iter()
            .map(|s| (s.id.clone(), s.label.clone()))
            .collect()
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
            + self.trigger_section_height()
            + PAD * 2.0
    }

    /// Height of the trigger section (title + four band rows) under the scope.
    /// Zero when there are no sends (nothing is selected, so nothing to route).
    /// Fixed for a given send count, so the scope absorbs the rest deterministically.
    fn trigger_section_height(&self) -> f32 {
        if self.sends.is_empty() {
            0.0
        } else {
            ROW_GAP + TRIG_TITLE_H + (TRIG_ROW_H + ROW_GAP) * TRIG_BANDS.len() as f32
        }
    }

    /// Total body height for the configured send count.
    fn body_height(&self) -> f32 {
        let scope = if self.sends.is_empty() { 0.0 } else { self.scope_h };
        self.chrome_height() + scope
    }

    /// Build the modal's nodes, centered in a `(width, height)` viewport. Routes
    /// through the same size-policy + centering path the overlay driver uses, so
    /// the standalone path and the driven path lay out identically.
    pub fn build(&mut self, tree: &mut UITree, viewport_w: f32, viewport_h: f32) {
        if !self.open {
            return;
        }
        let screen = Vec2::new(viewport_w, viewport_h);
        let size = self.size_policy().resolve(screen, self.desired_size());
        let rect = compute_overlay_rect(&self.anchor(), size, screen, None);
        self.build_at(tree, OverlayPlacement { rect, screen });
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
        // The modal background must be hit-testable so a press anywhere on it
        // emits a PointerDown — `hit_test` only returns INTERACTIVE nodes, and
        // `process_pointer` only fires PointerDown when something is hit. Without
        // this, pressing on the spectrogram (a non-interactive backing panel)
        // only arms the band-divider drag when an interactive node from the UI
        // behind the modal happens to sit under the cursor — the source of the
        // "sometimes draggable" band lines. `bg_id` is already in `owns_node`,
        // so this also makes the modal reliably swallow stray clicks.
        tree.set_flag(self.bg_id as u32, UIFlags::INTERACTIVE);

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

            // Label is a button — clicking it opens the inline rename editor. A
            // send with active trigger routes reads in an amber accent, so which
            // sends fire visuals is legible without selecting each one.
            let label_x = inner_x + SWATCH_W + 6.0;
            let has_triggers = send.triggers.iter().any(|t| t.enabled);
            let mut lbl_style = label_button_style();
            if has_triggers {
                lbl_style.text_color = Color32::new(240, 196, 110, 255); // amber
            }
            self.send_ids[i].label = tree.add_button(
                self.bg_id,
                label_x,
                cy,
                LABEL_W,
                ROW_H,
                lbl_style,
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

            // Source indicator (read-only): "Cap" for a capture send, or the
            // feeding audio layer's name (accented). A send doesn't pick its
            // source — an audio layer routes itself to a send from the layer
            // header (design §3R). This is status, not a control.
            const SRC_W: f32 = 48.0;
            let src_x = label_x + LABEL_W + 4.0;
            self.send_ids[i].source = tree.add_button(
                self.bg_id,
                src_x,
                cy,
                SRC_W,
                ROW_H,
                btn_style(send.layer_fed),
                &send.source_label,
            ) as i32;

            // Channel dropdown fills the gap, showing the resolved name(s).
            let ch_x = src_x + SRC_W + 4.0;
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
        self.scope_readout_label = -1;
        self.band_meter_ids = [(-1, -1, -1); 3];
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
            // Hover readout (freq + dB at the cursor), right-aligned in the same
            // title row — outside the waterfall rect, so the present pass's blit
            // doesn't cover it. Empty until the app feeds a value on hover.
            self.scope_readout_label = tree.add_label(
                self.bg_id,
                inner_x + inner_w * 0.35,
                cy,
                inner_w * 0.65,
                SCOPE_TITLE_H,
                "",
                UIStyle {
                    text_color: Color32::new(150, 200, 230, 255),
                    font_size: color::FONT_LABEL,
                    text_align: TextAlign::Right,
                    ..UIStyle::default()
                },
            ) as i32;
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

            // Waterfall vertical span — inset inside the backing panel so the
            // hard-cornered blit floats within the rounded, bordered frame.
            let wf_y = cy + SCOPE_PAD_Y;
            let wf_h = (self.scope_h - SCOPE_PAD_Y * 2.0).max(1.0);

            // Frequency-axis tick labels in the left margin (log scale: the
            // present pass draws the waterfall to the right of this margin).
            // Range must track `manifold_spectral::SpectrogramConfig` defaults
            // (10 Hz–22 kHz); ticks match the Analyzer VST's axis. Mapped over the
            // waterfall's inset span so the ticks line up with the blit.
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
                let ly = wf_y + wf_h * (1.0 - yn) - 6.0;
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
            // spectrogram texture here, on top of the backing panel. A right
            // margin is reserved for the per-band level meters (drawn as UI
            // nodes, since the blit would otherwise cover them).
            self.scope_rect = Some(Rect::new(
                inner_x + SCOPE_AXIS_W,
                wf_y,
                inner_w - SCOPE_AXIS_W - SCOPE_METER_W,
                wf_h,
            ));

            // Per-band level meters (Low/Mid/High): a track + fill + letter label
            // each, in the reserved right margin. Created here at zero size;
            // positioned and filled every frame by `update_band_meters` so they
            // follow the crossovers and the live levels. Fill + label share the
            // band colour (red/green/blue), so the colours double as the legend
            // across the meters and the spectrogram's transient ticks.
            for (band, slot) in self.band_meter_ids.iter_mut().enumerate() {
                let label = tree.add_label(
                    self.bg_id,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    BAND_METER_LABELS[band],
                    UIStyle {
                        text_color: band_color(band),
                        font_size: color::FONT_LABEL,
                        text_align: TextAlign::Center,
                        ..UIStyle::default()
                    },
                ) as i32;
                let track = tree.add_panel(
                    self.bg_id,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    UIStyle {
                        // Visibly lighter than the black scope so the empty part
                        // of the bar reads as a scale, not just background.
                        bg_color: Color32::new(54, 54, 62, 255),
                        corner_radius: 1.0,
                        ..UIStyle::default()
                    },
                );
                let fill = tree.add_panel(
                    self.bg_id,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    UIStyle { bg_color: band_color(band), corner_radius: 1.0, ..UIStyle::default() },
                );
                *slot = (track as i32, fill as i32, label);
            }

            // ── Live triggers (selected send) — laid out below the scope ──
            cy += self.scope_h + ROW_GAP;
            self.build_trigger_section(tree, inner_x, inner_w, cy);
        }
    }

    /// Build the four-band trigger matrix for the selected send. Each row:
    /// `[enable swatch] [band] [−] sens% [＋]  ->  [ layer ▼ ]`. The swatch's
    /// colour matches the scope's per-band transient ticks (Whole = neutral), so
    /// the legend reads across the scope and the routes. Click-only controls,
    /// consistent with the panel's gain stepper and channel dropdown — no drag.
    fn build_trigger_section(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32) {
        self.trigger_row_ids.clear();

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
            TRIG_TITLE_H,
            &format!("Triggers — {sel_label}"),
            UIStyle {
                text_color: Color32::new(170, 170, 180, 255),
                font_size: color::FONT_LABEL,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        cy += TRIG_TITLE_H;

        // The selected send's four band rows (Whole/Low/Mid/High), defaulting to
        // a disabled row when the send carries no route for a band yet.
        let rows: Vec<TriggerRouteRow> = self
            .selected_send
            .as_ref()
            .and_then(|id| self.sends.iter().find(|s| &s.id == id))
            .map(|s| s.triggers.clone())
            .unwrap_or_default();

        for (i, (_band, band_label)) in TRIG_BANDS.iter().enumerate() {
            let row = rows.get(i).cloned().unwrap_or_default();
            let mut ids = TriggerRowIds::default();
            let mut x = inner_x;

            // Enable swatch — band-coloured (Whole = neutral), dim when disabled.
            ids.enable = tree.add_button(
                self.bg_id,
                x,
                cy,
                TRIG_ENABLE_W,
                TRIG_ROW_H,
                trigger_swatch_style(i, row.enabled),
                "",
            ) as i32;
            x += TRIG_ENABLE_W + 4.0;

            // Band label — brighter when the route is active.
            tree.add_label(
                self.bg_id,
                x,
                cy,
                TRIG_LABEL_W,
                TRIG_ROW_H,
                band_label,
                UIStyle {
                    text_color: if row.enabled {
                        Color32::new(214, 214, 220, 255)
                    } else {
                        Color32::new(120, 120, 130, 255)
                    },
                    font_size: BTN_FONT,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            x += TRIG_LABEL_W + 4.0;

            // Sensitivity stepper [−] value [＋] (percent), matching the gain
            // stepper's glyphs and discrete-step behaviour.
            ids.sens_minus = tree.add_button(
                self.bg_id,
                x,
                cy,
                TRIG_SENS_BTN_W,
                TRIG_ROW_H,
                btn_style(false),
                "\u{2212}",
            ) as i32;
            tree.add_label(
                self.bg_id,
                x + TRIG_SENS_BTN_W,
                cy,
                TRIG_SENS_VAL_W,
                TRIG_ROW_H,
                &format!("{}%", (row.sensitivity.clamp(0.0, 1.0) * 100.0).round() as i32),
                UIStyle {
                    text_color: Color32::new(190, 190, 198, 255),
                    font_size: color::FONT_LABEL,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
            );
            ids.sens_plus = tree.add_button(
                self.bg_id,
                x + TRIG_SENS_BTN_W + TRIG_SENS_VAL_W,
                cy,
                TRIG_SENS_BTN_W,
                TRIG_ROW_H,
                btn_style(false),
                "\u{002B}",
            ) as i32;
            x += TRIG_SENS_BTN_W * 2.0 + TRIG_SENS_VAL_W + 4.0;

            // "->" connector to the destination layer.
            tree.add_label(
                self.bg_id,
                x,
                cy,
                TRIG_ARROW_W,
                TRIG_ROW_H,
                "\u{2192}",
                UIStyle {
                    text_color: Color32::new(120, 120, 130, 255),
                    font_size: BTN_FONT,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
            );
            x += TRIG_ARROW_W + 4.0;

            // Target-layer dropdown trigger (Auto or a layer name).
            let layer_w = (inner_x + inner_w - x).max(40.0);
            let layer_left = x;
            ids.layer = tree.add_button(
                self.bg_id,
                x,
                cy,
                layer_w,
                TRIG_ROW_H,
                dropdown_trigger_style(),
                &format!("{}   \u{25BC}", row.layer_label),
            ) as i32;

            // Live level meter — a thin underline across the tuning zone (band
            // label → layer dropdown). Fill = the band's transient level; the
            // tick marks the fire threshold. Resized + flashed every frame by
            // `update_trigger_levels`. Added last so it draws over the row's
            // bottom edge as a clean underline.
            let meter_h = 3.0;
            let meter_x = inner_x + TRIG_ENABLE_W + 4.0;
            let meter_w = (layer_left - 6.0 - meter_x).max(8.0);
            let meter_y = cy + TRIG_ROW_H - meter_h;
            let bandc = trigger_band_color(i);
            ids.meter_track = tree.add_panel(
                self.bg_id,
                meter_x,
                meter_y,
                meter_w,
                meter_h,
                UIStyle { bg_color: Color32::new(40, 40, 46, 255), ..UIStyle::default() },
            ) as i32;
            ids.meter_fill = tree.add_panel(
                self.bg_id,
                meter_x,
                meter_y,
                0.0, // width set per frame
                meter_h,
                UIStyle { bg_color: dim_color(bandc, 0.55), ..UIStyle::default() },
            ) as i32;
            let tick_x = meter_x + row.threshold.clamp(0.0, 1.0) * meter_w;
            ids.thresh_tick = tree.add_panel(
                self.bg_id,
                tick_x,
                meter_y - 2.0,
                1.5,
                meter_h + 4.0,
                UIStyle { bg_color: Color32::new(225, 225, 235, 255), ..UIStyle::default() },
            ) as i32;
            ids.meter_x = meter_x;
            ids.meter_y = meter_y;
            ids.meter_w = meter_w;
            ids.meter_h = meter_h;
            ids.band = i;
            ids.threshold = row.threshold;
            ids.enabled = row.enabled;

            self.trigger_row_ids.push(ids);
            cy += TRIG_ROW_H + ROW_GAP;
        }
    }

    /// The send the scope is showing, if any.
    pub fn selected_send(&self) -> Option<&AudioSendId> {
        self.selected_send.as_ref()
    }

    /// The routing lines for `send` (device + layers), for the read-only routings
    /// dropdown. Empty slice if the send is unknown.
    pub fn send_routings(&self, send: &AudioSendId) -> &[String] {
        self.sends
            .iter()
            .find(|s| &s.id == send)
            .map(|s| s.routings.as_slice())
            .unwrap_or(&[])
    }

    /// Screen-space rect (logical units) the present pass blits the spectrogram
    /// texture into, or `None` when the panel is closed / has no sends.
    pub fn scope_rect(&self) -> Option<Rect> {
        self.open.then_some(self.scope_rect).flatten()
    }

    /// Push the current crossovers (Hz) and the scope's analysed frequency range,
    /// every frame while open. The panel hit-tests the band-divider lines against
    /// these for dragging; the lines themselves are drawn shader-side from the
    /// same values, so the grab target matches what's on screen.
    pub fn set_scope_bands(&mut self, low_hz: f32, mid_hz: f32, fmin: f32, fmax: f32) {
        self.scope_low_hz = low_hz;
        self.scope_mid_hz = mid_hz;
        self.scope_fmin = fmin;
        self.scope_fmax = fmax;
    }

    /// True while a band divider is being dragged — the app suppresses the hover
    /// readout then so the two don't fight over the same gesture.
    pub fn is_dragging_band(&self) -> bool {
        self.dragging_band.is_some()
    }

    /// Screen y (logical) of a frequency on the scope's log axis, or `None` if
    /// the range is invalid (scope dark) or the scope isn't laid out.
    fn scope_line_y(&self, hz: f32) -> Option<f32> {
        let rect = self.scope_rect?;
        if self.scope_fmin <= 0.0 || self.scope_fmax <= self.scope_fmin {
            return None;
        }
        let yn = (hz / self.scope_fmin).log2() / (self.scope_fmax / self.scope_fmin).log2();
        // yn: 0 at fmin (bottom), 1 at fmax (top). Screen y grows downward.
        Some(rect.y + rect.height * (1.0 - yn))
    }

    /// Frequency (Hz) for a screen y on the scope, clamped into the displayed
    /// range — the inverse of [`Self::scope_line_y`].
    fn scope_y_to_hz(&self, y: f32) -> Option<f32> {
        let rect = self.scope_rect?;
        if self.scope_fmin <= 0.0 || self.scope_fmax <= self.scope_fmin || rect.height <= 0.0 {
            return None;
        }
        let yn = (1.0 - (y - rect.y) / rect.height).clamp(0.0, 1.0);
        Some(self.scope_fmin * (self.scope_fmax / self.scope_fmin).powf(yn))
    }

    /// Whether a screen point lies within the waterfall rect.
    fn point_in_scope(&self, pos: Vec2) -> bool {
        self.scope_rect().is_some_and(|r| {
            pos.x >= r.x && pos.x <= r.x + r.width && pos.y >= r.y && pos.y <= r.y + r.height
        })
    }

    /// Vertical grab tolerance for a divider line (logical px). Generous so the
    /// thin line is easy to land on; the hover glow uses the SAME test (see
    /// [`Self::divider_hover_index`]) so what lights up is exactly what grabs.
    const DIVIDER_GRAB_PX: f32 = 12.0;

    /// Which divider line (if any) is within grab distance of a screen `y`,
    /// preferring the nearer when both are close. Pure y-distance — shared by the
    /// pointer hit-test and the hover affordance.
    fn nearest_divider_y(&self, screen_y: f32) -> Option<BandDivider> {
        let mut best: Option<(BandDivider, f32)> = None;
        for (band, hz) in
            [(BandDivider::Low, self.scope_low_hz), (BandDivider::Mid, self.scope_mid_hz)]
        {
            if let Some(ly) = self.scope_line_y(hz) {
                let d = (screen_y - ly).abs();
                if d <= Self::DIVIDER_GRAB_PX && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((band, d));
                }
            }
        }
        best.map(|(b, _)| b)
    }

    /// Which divider line (if any) is within grab distance of a screen point,
    /// preferring the nearer when both are close. Requires the point to be within
    /// the waterfall's horizontal span (plus a small slop so the left-edge grip
    /// is easy to grab).
    fn divider_at(&self, pos: Vec2) -> Option<BandDivider> {
        let rect = self.scope_rect?;
        if pos.x < rect.x - 4.0 || pos.x > rect.x + rect.width + 4.0 {
            return None;
        }
        self.nearest_divider_y(pos.y)
    }

    /// Divider index for the shader's hover affordance: `0` = low/mid over the
    /// cursor, `1` = mid/high, `< 0` = none. Uses the same test as the grab, so
    /// the glow and the grab zone are identical.
    pub fn divider_hover_index(&self, screen_y: f32) -> f32 {
        match self.nearest_divider_y(screen_y) {
            Some(BandDivider::Low) => 0.0,
            Some(BandDivider::Mid) => 1.0,
            None => -1.0,
        }
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
        if self.send_ids.iter().any(|r| {
            id == r.swatch
                || id == r.label
                || id == r.source
                || id == r.ch_dropdown
                || id == r.gain_minus
                || id == r.gain_plus
                || id == r.stereo
                || id == r.delete
        }) {
            return true;
        }
        self.trigger_row_ids.iter().any(|r| {
            id == r.enable || id == r.sens_minus || id == r.sens_plus || id == r.layer
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

    /// Position + fill the per-band level meters from the tapped send's per-band
    /// amplitudes `[low, mid, high]` (each 0..1), every frame while open. Each
    /// bar sits at the geometric centre of its band slab — so it lines up with
    /// the frequency axis — and follows the crossovers as they're dragged.
    /// `None`, or a dark scope, hides the bars.
    pub fn update_band_meters(&self, tree: &mut UITree, amps: Option<[f32; 3]>) {
        let Some(rect) = self.scope_rect() else { return };
        // Letter label on the left of the margin, bar filling the rest.
        let label_x = rect.x + rect.width + SCOPE_METER_GAP;
        let bar_x = label_x + BAND_METER_LABEL_W;
        let bar_w = (SCOPE_METER_W - SCOPE_METER_GAP - BAND_METER_LABEL_W).max(1.0);
        let label_h = 12.0;
        // Band slab frequency edges, in [low, mid, high] order.
        let edges = [
            (self.scope_fmin, self.scope_low_hz),
            (self.scope_low_hz, self.scope_mid_hz),
            (self.scope_mid_hz, self.scope_fmax),
        ];
        for (i, &(track, fill, label)) in self.band_meter_ids.iter().enumerate() {
            if track < 0 || fill < 0 || label < 0 {
                continue;
            }
            let (lo, hi) = edges[i];
            let center_y = amps
                .filter(|_| lo > 0.0 && hi > lo)
                .and_then(|_| self.scope_line_y((lo * hi).sqrt()));
            match (amps, center_y) {
                (Some(a), Some(y)) => {
                    let amp = a[i].clamp(0.0, 1.0);
                    let top = y - BAND_METER_HALF_H;
                    let h = BAND_METER_HALF_H * 2.0;
                    tree.set_bounds(track as u32, Rect::new(bar_x, top, bar_w, h));
                    tree.set_bounds(fill as u32, Rect::new(bar_x, top, bar_w * amp, h));
                    tree.set_bounds(
                        label as u32,
                        Rect::new(label_x, y - label_h * 0.5, BAND_METER_LABEL_W, label_h),
                    );
                    tree.set_visible(track as u32, true);
                    tree.set_visible(fill as u32, true);
                    tree.set_visible(label as u32, true);
                }
                _ => {
                    tree.set_visible(track as u32, false);
                    tree.set_visible(fill as u32, false);
                    tree.set_visible(label as u32, false);
                }
            }
        }
    }

    /// Drive the per-row trigger meters from the selected send's live per-band
    /// transient levels (`[whole, low, mid, high]`, each 0..1), every frame while
    /// open. The fill grows to the level; when the level crosses the row's
    /// threshold the fill flashes to the bright band colour (the fire cue). The
    /// transient impulse already decays, so the flash blinks once per onset with
    /// no extra timer. `None` / a dark scope rests every meter. No rebuild.
    pub fn update_trigger_levels(&self, tree: &mut UITree, levels: Option<[f32; 4]>) {
        for ids in &self.trigger_row_ids {
            if ids.meter_fill < 0 {
                continue;
            }
            let level = levels.map_or(0.0, |l| l[ids.band].clamp(0.0, 1.0));
            let w = ids.meter_w * level;
            tree.set_bounds(
                ids.meter_fill as u32,
                Rect::new(ids.meter_x, ids.meter_y, w, ids.meter_h),
            );
            // Flash: enabled row whose level has crossed its fire line.
            let bandc = trigger_band_color(ids.band);
            let firing = ids.enabled && level >= ids.threshold && ids.threshold > 0.0;
            let fill_color = if firing { bandc } else { dim_color(bandc, 0.55) };
            tree.set_style(
                ids.meter_fill as u32,
                UIStyle { bg_color: fill_color, ..UIStyle::default() },
            );
        }
    }

    /// Update the scope's hover readout text in place (no rebuild). `Some(text)`
    /// shows it (freq + dB under the cursor); `None` hides it. Called every frame
    /// while open, mirroring [`update_meters`](Self::update_meters).
    pub fn update_scope_readout(&self, tree: &mut UITree, text: Option<&str>) {
        if self.scope_readout_label < 0 {
            return;
        }
        let id = self.scope_readout_label as u32;
        match text {
            Some(t) => {
                tree.set_text(id, t);
                tree.set_visible(id, true);
            }
            None => tree.set_visible(id, false),
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
            } else if id == ids.source {
                Some((i, RowControl::Source))
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
        // Trigger-row controls (selected send's band routes). Checked before the
        // send-row `hit?` early-return so a trigger click isn't swallowed.
        if let Some((band, ctl)) = self.trigger_row_ids.iter().enumerate().find_map(|(ri, ids)| {
            let band = TRIG_BANDS.get(ri).map(|(b, _)| *b)?;
            if id == ids.enable {
                Some((band, TrigControl::Toggle))
            } else if id == ids.sens_minus {
                Some((band, TrigControl::SensDown))
            } else if id == ids.sens_plus {
                Some((band, TrigControl::SensUp))
            } else if id == ids.layer {
                Some((band, TrigControl::Layer))
            } else {
                None
            }
        }) {
            self.delete_armed = None;
            let send = self.selected_send.clone()?;
            return Some(match ctl {
                TrigControl::Toggle => PanelAction::AudioTriggerToggled(send, band),
                TrigControl::SensDown => {
                    PanelAction::AudioTriggerSensitivityStep(send, band, -0.1)
                }
                TrigControl::SensUp => PanelAction::AudioTriggerSensitivityStep(send, band, 0.1),
                TrigControl::Layer => PanelAction::AudioTriggerLayerClicked(send, band),
            });
        }

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
            RowControl::Source => {
                // Read-only: open a dropdown listing where this send is routed from
                // (device + layers). Routing is edited elsewhere — layers from the
                // layer header, channels from the channel control.
                self.delete_armed = None;
                Some(PanelAction::AudioSendRoutingsClicked(send_id))
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

/// Which interactive control of a trigger row was clicked.
enum TrigControl {
    Toggle,
    SensDown,
    SensUp,
    Layer,
}

/// Which interactive control of a send row was clicked.
enum RowControl {
    Select,
    Label,
    Source,
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

    fn size_policy(&self) -> SizePolicy {
        // 80% of the viewport, never below the compact minimums. The min height
        // is the fixed control chrome plus the smallest useful waterfall.
        SizePolicy::Fraction {
            frac: Vec2::new(PANEL_W_FRAC, PANEL_H_FRAC),
            min: Vec2::new(PANEL_W_MIN, self.chrome_height() + SCOPE_H_MIN),
        }
    }

    fn desired_size(&self) -> Vec2 {
        Vec2::new(self.panel_w, self.body_height())
    }

    fn build_at(&mut self, tree: &mut UITree, placement: OverlayPlacement) {
        if !self.open {
            return;
        }
        // The driver has already sized + centered the rect per `size_policy`.
        // Fill it: the width is the panel, and the waterfall absorbs whatever
        // height is left after the fixed-height control rows.
        self.panel_w = placement.rect.width;
        self.scope_h = (placement.rect.height - self.chrome_height()).max(SCOPE_H_MIN);
        self.build_nodes(tree, placement.rect.x, placement.rect.y);
    }

    fn on_event(&mut self, event: &UIEvent, _tree: &mut UITree) -> OverlayResponse {
        match event {
            UIEvent::KeyDown { key: Key::Escape, .. } => {
                self.open = false;
                OverlayResponse::Consumed(Vec::new())
            }
            UIEvent::Click { node_id, pos, .. } => {
                let id = *node_id as i32;
                if id == self.close_id {
                    self.open = false;
                    OverlayResponse::Consumed(Vec::new())
                } else if let Some(action) = self.handle_click_inner(id) {
                    OverlayResponse::Consumed(vec![action])
                } else if self.owns_node(id) || self.point_in_scope(*pos) {
                    // Panel background, a non-action control, or the waterfall
                    // (a tap on the scope must not close the modal) — swallow.
                    OverlayResponse::Consumed(Vec::new())
                } else {
                    // Click landed on the dim backdrop / outside the panel — close.
                    self.open = false;
                    OverlayResponse::Consumed(Vec::new())
                }
            }
            // ── Band-divider drag (Low/Mid/High crossovers) ──────────
            // Arm on press if it lands on a divider line; thereafter the drag
            // owns the gesture until release. The lines are drawn shader-side,
            // so this only hit-tests their positions (see `set_scope_bands`).
            UIEvent::PointerDown { pos, .. } => {
                if let Some(band) = self.divider_at(*pos) {
                    self.dragging_band = Some(band);
                    OverlayResponse::Consumed(vec![PanelAction::AudioCrossoverDragBegin])
                } else {
                    OverlayResponse::Ignored
                }
            }
            UIEvent::Drag { pos, .. } => {
                if let Some(band) = self.dragging_band {
                    match self.scope_y_to_hz(pos.y) {
                        Some(hz) => OverlayResponse::Consumed(vec![
                            PanelAction::AudioCrossoverChanged(band, hz),
                        ]),
                        None => OverlayResponse::Consumed(Vec::new()),
                    }
                } else {
                    OverlayResponse::Ignored
                }
            }
            UIEvent::DragEnd { .. } | UIEvent::PointerUp { .. } => {
                if self.dragging_band.take().is_some() {
                    OverlayResponse::Consumed(vec![PanelAction::AudioCrossoverCommit])
                } else {
                    OverlayResponse::Ignored
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

/// Enable swatch for a trigger row: filled with the band colour when enabled
/// (Whole = neutral white, Low/Mid/High = red/green/blue, matching the scope
/// ticks), a bordered dark cell when disabled. `row` is the row index in
/// [`TRIG_BANDS`] order.
fn trigger_swatch_style(row: usize, enabled: bool) -> UIStyle {
    let band = if row == 0 {
        Color32::new(190, 190, 200, 255) // Whole — neutral
    } else {
        band_color(row - 1)
    };
    UIStyle {
        bg_color: if enabled { band } else { Color32::new(40, 40, 46, 255) },
        hover_bg_color: if enabled { band } else { Color32::new(56, 56, 64, 255) },
        pressed_bg_color: Color32::new(30, 30, 34, 255),
        border_color: Color32::new(70, 70, 78, 255),
        border_width: 1.0,
        corner_radius: 3.0,
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
                    source_label: "Cap".into(),
                    layer_fed: false,
                    routings: vec!["Capture: Channel 1".into()],
                    triggers: Vec::new(),
                },
                AudioSendRow {
                    id: AudioSendId::new("s2"),
                    label: "Audio 2".into(),
                    channels: vec![2],
                    channel_label: "MacBook Mic".into(),
                    gain_db: 0.0,
                    driven_count: 0,
                    source_label: "Cap".into(),
                    layer_fed: false,
                    routings: vec!["Capture: Channel 1".into()],
                    triggers: Vec::new(),
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
    fn trigger_row_clicks_resolve_to_actions() {
        let mut p = panel_with_two_sends();
        // Selected send (s1) gets four band rows so the section renders.
        p.sends[0].triggers = vec![TriggerRouteRow::default(); 4];
        let mut tree = UITree::new();
        p.build(&mut tree, 1280.0, 720.0);
        assert_eq!(p.trigger_row_ids.len(), 4);

        // Whole (row 0) enable → toggle on the selected send + Full band.
        match p.handle_click(p.trigger_row_ids[0].enable) {
            Some(PanelAction::AudioTriggerToggled(id, band)) => {
                assert_eq!(id.as_str(), "s1");
                assert_eq!(band, AudioBand::Full);
            }
            other => panic!("expected toggle, got {other:?}"),
        }
        // Low (row 1) [＋] → positive sensitivity step.
        match p.handle_click(p.trigger_row_ids[1].sens_plus) {
            Some(PanelAction::AudioTriggerSensitivityStep(_, band, d)) => {
                assert_eq!(band, AudioBand::Low);
                assert!(d > 0.0);
            }
            other => panic!("expected sens step, got {other:?}"),
        }
        // High (row 3) layer field → opens the layer dropdown.
        match p.handle_click(p.trigger_row_ids[3].layer) {
            Some(PanelAction::AudioTriggerLayerClicked(_, band)) => {
                assert_eq!(band, AudioBand::High);
            }
            other => panic!("expected layer dropdown open, got {other:?}"),
        }
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
    fn source_chip_opens_routings_and_is_owned() {
        let mut p = panel_with_two_sends();
        let mut tree = UITree::new();
        p.build(&mut tree, 1280.0, 720.0);

        let src = p.send_ids[0].source;
        assert!(p.owns_node(src), "source chip is a panel-owned node");
        // The chip is read-only — clicking opens the routings dropdown, it doesn't
        // edit anything.
        match p.handle_click(src) {
            Some(PanelAction::AudioSendRoutingsClicked(id)) => assert_eq!(id.as_str(), "s1"),
            other => panic!("expected AudioSendRoutingsClicked, got {other:?}"),
        }
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
