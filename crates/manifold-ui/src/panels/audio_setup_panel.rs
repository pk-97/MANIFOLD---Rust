//! Audio Setup panel — the central place to route audio in and manage the
//! named sends that the per-slider audio drawers reference.
//!
//! A non-dimming overlay docked to the viewport's right edge (D6 —
//! `docs/AUDIO_SENDS_UX_DESIGN.md` §2/§3.3), full height, with an input-device
//! picker and one row per send: channel, gain, and delete, plus an "Add send"
//! button. The show stays visible underneath and outside clicks pass through
//! (it's a calibration surface used while performing — accidental dismissal
//! is the failure mode); Escape and the header Audio button still close it.
//! Self-contained like [`super::browser_popup`]: it builds `UITree` nodes from
//! data handed in via [`AudioSetupPanel::configure`] and maps a clicked node
//! id back to a [`PanelAction`] (the project-level audio-setup actions,
//! already routed through `ui_bridge`). See
//! `docs/AUDIO_MODULATION_DESIGN.md` §10.1.
//!
//! v1 scope: device cycle, add/remove send, per-send single-channel routing and
//! gain trim. Per-send labels are auto-assigned ("Audio N") until a text-field
//! rename lands; multi-channel downmix and the v2 analysis toggles are future.

use crate::types::AudioDeviceRef;
use manifold_foundation::{AudioSendId, LayerId};

use crate::chrome::{ChromeHost, Pad, Sizing, View};
use crate::color;
use crate::drag::DragController;
use crate::input::{Modifiers, UIEvent};
use crate::node::*;
use crate::tree::UITree;

use super::{BandDivider, PanelAction};

// Stable keys for the host-owned modal chrome (background + title strip).
const KEY_BG: u64 = 70_001;
const KEY_CLOSE: u64 = 70_002;

// ── Stable keys for every interactive control in the panel body ──
//
// The panel is modeless and consumes `PointerDown` on its own owned nodes
// (BUG-059 — a press that arms nothing must not leak to the timeline
// beneath). Consuming an event marks the overlay dirty, so it rebuilds on
// every press. Any `add_button` call built WITHOUT a key re-mints its
// `WidgetId` from its sibling index on that rebuild; `Click` only fires
// when press and release resolve to the SAME `WidgetId`, so an unkeyed
// button silently needed a second click (BUG — see
// `docs/INPUT_IDENTITY_UNIFICATION.md`). Every interactive node below is
// therefore keyed so its identity survives the rebuild. Non-interactive
// `add_label` nodes (the gain/sensitivity value labels used only as D7
// drag-arm targets) are exempt — they carry no `INTERACTIVE` flag and
// never receive a `Click`.
//
// Singletons: one instance per panel build.
const KEY_DEVICE_DROPDOWN: u64 = 70_010;
const KEY_ADD_SEND: u64 = 70_011;
const KEY_FLOOR_MINUS: u64 = 70_012;
const KEY_FLOOR_PLUS: u64 = 70_013;

/// Per-send row controls (dynamic list, indexed by the send's position in
/// `self.sends`): swatch, label, delete, gain_minus, gain_plus, ch_dropdown.
/// Stride 20 leaves headroom; offsets 3 and 6 are
/// retired, not reused.
const KEY_SEND_ROW_BASE: u64 = 71_000;
const KEY_SEND_ROW_STRIDE: u64 = 20;
const SEND_OFF_SWATCH: u64 = 0;
const SEND_OFF_LABEL: u64 = 1;
const SEND_OFF_DELETE: u64 = 2;
const SEND_OFF_GAIN_MINUS: u64 = 4;
const SEND_OFF_GAIN_PLUS: u64 = 5;
const SEND_OFF_CH_DROPDOWN: u64 = 7;

/// Stable key for a per-send row control at index `i` with the given
/// control offset (`SEND_OFF_*`).
const fn send_row_key(i: usize, offset: u64) -> u64 {
    KEY_SEND_ROW_BASE + (i as u64) * KEY_SEND_ROW_STRIDE + offset
}

/// Consumers section: one row button per consumer of the selected send,
/// indexed by position in that send's `consumers`.
const KEY_CONSUMER_ROW_BASE: u64 = 74_000;
use crate::scroll_container::{SCROLLBAR_W, ScrollContainer, ScrollbarStyle};

// ── Layout ──
/// Minimum panel width. The dock (`AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN`
/// D1) supplies the width from `ScreenLayout::audio_setup()`; the panel builds
/// its rows into whatever it's given, floored here so the send row's columns
/// stay legible.
const PANEL_W_MIN: f32 = 460.0;
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

/// Sentinel `state_sync.rs` resolves a feeding layer's name to when its
/// `LayerId` no longer exists in the project (routed, then the layer was
/// deleted). One constant, not a literal duplicated in each crate, so the
/// D8 "(missing layer)" repair-copy check in [`AudioSetupPanel`] can't drift
/// out of sync with the string `state_sync` actually emits.
pub const MISSING_LAYER_LABEL: &str = "(missing layer)";

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
    /// Pre-analysis noise floor (dB) for the spectrogram squelch. `<= FLOOR_DB_OFF`
    /// reads as "Off" on the scope's Floor stepper.
    pub floor_db: f32,
    /// Number of parameters this send currently drives. Surfaced on the row and
    /// gates a confirm-before-delete so a bound send isn't silently severed.
    pub driven_count: usize,
    /// Human-readable routing lines for the read-only Inputs section — the
    /// capture device (when channels are assigned) plus one line per feeding
    /// layer. Built by `state_sync`. (Formerly also fed a row-level "Cap"
    /// chip and its click-to-reveal routings dropdown; both deleted §7.2
    /// item 7, P8, 2026-07-11 — this is the ONE place the detail lives now.)
    pub routings: Vec<String>,
    // `consumers` below is the panel's sole surviving
    // trigger display.
    /// Whether any enabled `LayerClipTrigger` sources this send — drives the
    /// send label's amber "active trigger" accent (P3; replaces the matrix's
    /// per-route `enabled` check). Built by `state_sync`.
    pub has_clip_triggers: bool,
    /// Audio layers feeding this send (id + name), for the Inputs section's
    /// per-layer remove row. Built by `state_sync` from the send's
    /// `source.layers` — the single source of truth for the layer↔send
    /// binding (`docs/AUDIO_SENDS_UX_DESIGN.md` D1/D2).
    pub feeding_layers: Vec<(LayerId, String)>,
    /// This send's consumers — one row per enabled audio mod reading it plus
    /// one per enabled trigger route on it — for the Consumers section.
    /// Navigational only (D3): clicking a row selects the owning layer, it
    /// never edits. Built by `state_sync`.
    pub consumers: Vec<SendConsumerRow>,
}

/// One consumer row in the Audio Setup modal's Consumers section: a named
/// audio mod ("Layer • Effect • Param") or an enabled clip trigger ("Clip
/// trigger • Layer • Band", §7.2 item 7, P8, 2026-07-11), each clickable to
/// jump to the owning layer. See `docs/AUDIO_SENDS_UX_DESIGN.md` §3.1.
#[derive(Clone, Debug)]
pub struct SendConsumerRow {
    pub label: String,
    /// Jump target — the layer the mod/route lives on (or fires into).
    /// `None` if unresolvable (e.g. the route's "Auto" target couldn't be
    /// matched to a layer by name), in which case the row still shows but
    /// doesn't navigate on click.
    pub layer_id: Option<LayerId>,
}

// `TriggerRouteRow` (the Audio Setup Triggers matrix's row display state) is
// deleted (P3, D2) along with the matrix that built it.

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
/// Onset lane-legend chip geometry (D7 readability): the small backed chip
/// each lane label sits in, over the waterfall's left edge for the three
/// frequency-named lanes (see [`AudioSetupPanel::update_scope_lane_labels`]),
/// or in the axis gutter for any other lane.
const LANE_CHIP_W: f32 = 34.0;
const LANE_CHIP_H: f32 = 14.0;

/// Section header row height (Triggers/Inputs/Consumers all shared this before
/// the matrix's deletion; kept for the two survivors).
const TRIG_TITLE_H: f32 = 18.0;

/// Per-send interactive node ids.
///
/// Every id is assigned from an `add_*` call in `build_nodes` before the row is
/// stored, so the fields always hold real nodes; the `Default` impl's `NodeId(0)`
/// placeholders (matching `slider.rs`) only exist transiently while the row is
/// being filled in.
#[derive(Clone)]
struct SendRowIds {
    /// Identity-colour swatch — clicking it selects the send for the scope.
    swatch: NodeId,
    label: NodeId,
    ch_dropdown: NodeId,
    gain_minus: NodeId,
    gain_plus: NodeId,
    /// The gain value label between the steppers — a D7 horizontal drag zone
    /// (`pointer-down` arms [`AudioSetupPanel::gain_drag_target`]).
    gain_value: NodeId,
    delete: NodeId,
    /// Level-meter track + fill nodes. [`AudioSetupPanel::update_meters`]
    /// resizes `meter_fill` in place each frame, reading `meter_track`'s
    /// CURRENT tree bounds (not a build-time cache) so a scroll shift to the
    /// track carries the fill along with it.
    meter_track: NodeId,
    meter_fill: NodeId,
}

impl Default for SendRowIds {
    fn default() -> Self {
        Self {
            swatch: NodeId::PLACEHOLDER,
            label: NodeId::PLACEHOLDER,
            ch_dropdown: NodeId::PLACEHOLDER,
            gain_minus: NodeId::PLACEHOLDER,
            gain_plus: NodeId::PLACEHOLDER,
            gain_value: NodeId::PLACEHOLDER,
            delete: NodeId::PLACEHOLDER,
            meter_track: NodeId::PLACEHOLDER,
            meter_fill: NodeId::PLACEHOLDER,
        }
    }
}

/// A D7 calibration drag armed by pointer-down on a gain or trigger
/// sensitivity value label. Carries the pre-drag pointer x + value so
/// `on_event`'s `Drag` arm can compute the live absolute value from
/// horizontal movement alone (1 px = 0.1 dB / 0.5%, see
/// `docs/AUDIO_SENDS_UX_DESIGN.md` §3.4) without re-deriving it from the
/// project each frame. Exactly one drag (crossover OR calibration) is ever
/// armed at a time.
#[derive(Clone)]
enum CalibrationDrag {
    Gain {
        send: AudioSendId,
        start_x: f32,
        start_db: f32,
        /// Shift held at drag-start (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md
        /// P4, D8's audio-dock sibling): the applied per-pixel delta is
        /// multiplied by 0.1 for the life of the drag.
        fine: bool,
    },
    // `Sensitivity` (the matrix's per-band sensitivity drag) is deleted with
    // the Audio Setup Triggers matrix (P3, D2).
}

/// P7.6 (D12): the fold target for the `dragging_band`/`calibration_drag`
/// pair — two `Option`s that could both be armed at once (a bug class, never
/// a feature) now become one `DragController<AudioSetupDrag>` session.
#[derive(Clone)]
enum AudioSetupDrag {
    Band(BandDivider),
    Calibration(CalibrationDrag),
}

/// The Audio Setup modal panel.
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
    /// Resolved panel width and waterfall height, set by
    /// [`AudioSetupPanel::build_docked`] from the dock rect (D1). The width is
    /// the dock column; the scope takes a fixed fraction of the height so the
    /// control rows above/below can overflow and scroll.
    panel_w: f32,
    scope_h: f32,
    /// Host for the declarative modal chrome (background + title strip). The
    /// device / send / scope rows are still built imperatively into `bg_id`.
    host: ChromeHost,
    /// D8 (BUG-047): the panel body is a `ScrollContainer` so the control rows
    /// (device / sends / inputs / scope / triggers / consumers) clip to the
    /// dock's height via GPU scissor and scroll instead of overflowing past the
    /// bottom edge. Built each `build_docked`.
    scroll: ScrollContainer,
    /// Parent node every body row is built under — the scroll clip region when
    /// the body scrolls (set in `build_nodes`). Distinct from `bg_id` (the
    /// panel background/chrome) so the chrome stays fixed while the body clips.
    content_parent: NodeId,
    // Node ids (set by `build`).
    bg_id: NodeId,
    close_id: NodeId,
    device_dropdown_id: NodeId,
    add_send_id: NodeId,
    send_ids: Vec<SendRowIds>,
    /// Right-aligned label in the scope title row showing the freq + dB under
    /// the cursor. Lives in the title strip (not over the waterfall, which the
    /// present pass blits on top), updated in place each frame by
    /// [`AudioSetupPanel::update_scope_readout`]. `None` when not built.
    scope_readout_label: Option<NodeId>,
    /// Onset tick-lane legend: (name, colour) per lane bottom-up + each lane's
    /// height fraction of the scope, fed once by the app from the one lane
    /// definition (`manifold_spectral::ScopeOnsets` — this crate deliberately
    /// doesn't depend on spectral) via
    /// [`AudioSetupPanel::set_scope_lane_legend`]. Labels are created by
    /// `build` and positioned each frame by
    /// [`AudioSetupPanel::update_scope_lane_labels`] (D7: the three frequency-
    /// named lanes — "Low"/"Mid"/"High" — are pulled onto their divider lines
    /// as small backed chips over the waterfall's left edge; any other lane
    /// name, e.g. "Kick", keeps the original bottom-stacked axis-gutter
    /// position, now uncrowded since only non-band lanes land there).
    scope_lane_legend: Vec<(String, Color32)>,
    scope_lane_frac: f32,
    scope_lane_label_ids: Vec<NodeId>,
    /// Backing chip panel per lane, parallel to `scope_lane_label_ids` (D7
    /// readability: a plain label is illegible over a busy spectrogram frame).
    scope_lane_chip_ids: Vec<NodeId>,
    /// Pre-analysis floor stepper [−]/[＋] in the scope title row (the spectrogram
    /// squelch). `None` when not built.
    floor_minus_id: Option<NodeId>,
    floor_plus_id: Option<NodeId>,
    /// Current Low/Mid/High crossovers (Hz) and the scope's analysed frequency
    /// range, pushed every frame by [`AudioSetupPanel::set_scope_bands`]. The
    /// divider lines are drawn shader-side; the panel keeps these only to
    /// hit-test a press against a line for dragging. `scope_fmin <= 0` means the
    /// scope is dark (no capture) — dividers aren't draggable then.
    scope_low_hz: f32,
    scope_mid_hz: f32,
    scope_fmin: f32,
    scope_fmax: f32,
    /// Band-divider drag OR the D7 calibration drag (gain value label),
    /// whichever is currently armed (P7.6: `DragController<AudioSetupDrag>`
    /// replaces the `dragging_band`/`calibration_drag` `Option` pair — only
    /// one is ever armed at a time, now unrepresentable otherwise).
    drag: DragController<AudioSetupDrag>,
    /// Screen rect of the whole panel, set by `build_nodes` — the ownership
    /// test `claims_drag`/`point_in_panel` use (and nothing else; node-level
    /// hit-testing stays the authority for everything with a node).
    panel_rect: Rect,
    /// Per-band (Low/Mid/High) level-meter nodes `(track, fill, label)` in the
    /// scope's right margin. Created by `build`, repositioned + resized every
    /// frame by [`AudioSetupPanel::update_band_meters`] so they track the moving
    /// crossovers. `None` when not built.
    band_meter_ids: [(Option<NodeId>, Option<NodeId>, Option<NodeId>); 3],
    /// Consumers section (selected send): one row button per consumer,
    /// index-aligned with that send's `consumers`. Rebuilt with the panel.
    consumer_row_ids: Vec<NodeId>,
}

impl Default for AudioSetupPanel {
    fn default() -> Self {
        Self {
            open: false,
            current_device: None,
            sends: Vec::new(),
            status_warning: None,
            delete_armed: None,
            selected_send: None,
            scope_rect: None,
            panel_w: 0.0,
            scope_h: 0.0,
            host: ChromeHost::new(),
            scroll: ScrollContainer::new(),
            content_parent: NodeId::PLACEHOLDER,
            // Set by `build`; `NodeId::PLACEHOLDER` is a pre-build placeholder,
            // never a hit target before the panel is built (matches `slider.rs`).
            bg_id: NodeId::PLACEHOLDER,
            close_id: NodeId::PLACEHOLDER,
            device_dropdown_id: NodeId::PLACEHOLDER,
            add_send_id: NodeId::PLACEHOLDER,
            send_ids: Vec::new(),
            scope_readout_label: None,
            scope_lane_legend: Vec::new(),
            scope_lane_frac: 0.0,
            scope_lane_label_ids: Vec::new(),
            scope_lane_chip_ids: Vec::new(),
            floor_minus_id: None,
            floor_plus_id: None,
            scope_low_hz: 0.0,
            scope_mid_hz: 0.0,
            scope_fmin: 0.0,
            scope_fmax: 0.0,
            drag: DragController::new(),
            panel_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            band_meter_ids: [(None, None, None); 3],
            consumer_row_ids: Vec::new(),
        }
    }
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

    /// Open the modal (idempotent) — the headless harness's `open:audio_setup`
    /// interact verb uses this so a repeated call can't accidentally close it,
    /// unlike [`Self::toggle`].
    pub fn open(&mut self) {
        self.open = true;
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
        self.normalize_selection();
    }

    /// Default the scope selection to the first send when it is unset or no
    /// longer refers to a configured send. Runs in [`configure`] — the only
    /// place the send list changes — so `build_nodes` sees a settled selection.
    ///
    /// [`configure`]: Self::configure
    fn normalize_selection(&mut self) {
        if !self.sends.iter().any(|s| Some(&s.id) == self.selected_send.as_ref()) {
            self.selected_send = self.sends.first().map(|s| s.id.clone());
        }
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

    /// Build the panel as a docked column into `rect`
    /// (`ScreenLayout::audio_setup()`, D1). The width is the panel; the scope
    /// takes a fixed fraction of the height so the control rows above/below it
    /// can overflow and SCROLL (D8/BUG-047) rather than the scope absorbing all
    /// slack and the tail sections clipping past the bottom. No-op when closed.
    pub fn build_docked(&mut self, tree: &mut UITree, rect: Rect) {
        if !self.open {
            return;
        }
        self.panel_w = rect.width.max(PANEL_W_MIN);
        // Fixed-fraction scope: big enough to read the waterfall, small enough
        // that the surrounding rows drive the scroll when there are many.
        self.scope_h = (rect.height * 0.38).max(SCOPE_H_MIN);
        self.build_nodes(tree, rect.x, rect.y, rect.height);
    }

    /// Mouse-wheel scroll for the docked body. Returns true if the offset moved
    /// (caller schedules a rebuild so the offset is re-applied). Sign convention
    /// matches the inspector's wheel handler (positive `delta` = wheel up).
    pub fn handle_scroll(&mut self, delta: f32) -> bool {
        self.scroll.apply_scroll_delta(delta)
    }

    /// The modal chrome as a host `View`: the hit-testable background (it must
    /// swallow stray clicks — see below) and the title strip with its close
    /// button. The device / send / scope rows are built imperatively into
    /// `bg_id` afterwards.
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
                        View::label("Audio Setup")
                            .fill_w()
                            .fill_h()
                            .font(color::FONT_BODY)
                            .text_color(Color32::new(224, 224, 228, 255))
                            .align_text(TextAlign::Left),
                    )
                    .child(
                        View::button("\u{00D7}")
                            .w(Sizing::Fixed(STEP_W))
                            .fill_h()
                            .style(btn_style(false))
                            .inert()
                            .key(KEY_CLOSE),
                    ),
            )
    }

    /// Build the docked panel's nodes with its top-left at `(x, y)`, filling
    /// `panel_h`. The chrome (background + title strip) fills the whole dock;
    /// the body (every control row) is built into a `ScrollContainer` clip so it
    /// scrolls within the fixed dock height instead of overflowing (D8/BUG-047).
    fn build_nodes(&mut self, tree: &mut UITree, x: f32, y: f32, panel_h: f32) {
        let rows = self.sends.len();

        // ── Chrome (background + title strip) on the host. The background is
        // interactive so a press anywhere on it emits a PointerDown and the
        // panel swallows stray clicks. It fills the full dock height; the body
        // rows below are built into the scroll clip.
        let chrome = self.chrome_view();
        self.host.build(tree, &chrome, Rect::new(x, y, self.panel_w, panel_h));
        self.bg_id = self.host.node_id_for_key(KEY_BG).unwrap_or(NodeId::PLACEHOLDER);
        self.close_id = self
            .host
            .node_id_for_key(KEY_CLOSE)
            .unwrap_or(NodeId::PLACEHOLDER);
        // Ownership rect for `claims_drag`/`point_in_panel` — the whole dock.
        self.panel_rect = Rect::new(x, y, self.panel_w, panel_h);

        let inner_x = x + PAD;
        let inner_w = self.panel_w - PAD * 2.0;
        // The body content top; the scroll viewport spans from here to the
        // panel's bottom padding. Content is built at absolute coords starting
        // at `content_top` and reparented under the clip at the end.
        let content_top = y + PAD + TITLE_H;
        let body_viewport = Rect::new(
            x,
            content_top,
            self.panel_w,
            (y + panel_h - PAD - content_top).max(0.0),
        );
        let clip_id = self.scroll.begin(tree, body_viewport);
        self.content_parent = clip_id;
        let content_start = tree.count();
        let mut cy = content_top;

        // Device row: [Device]  [ current device            ▼ ]
        tree.add_label(Some(self.content_parent), inner_x, cy, 70.0, ROW_H, "Device", label_style());
        self.device_dropdown_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + 74.0,
            cy,
            inner_w - 74.0,
            ROW_H,
            dropdown_trigger_style(),
            &self.device_label(),
            KEY_DEVICE_DROPDOWN,
        );
        cy += ROW_H + ROW_GAP;

        // Notice line: delete-confirm prompt or reliability warning, if any.
        if let Some(warning) = &self.active_notice() {
            tree.add_label(
                Some(self.content_parent),
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

        // Selection is normalized in `configure` (it must be settled BEFORE
        // `build_at` sizes the scope from `chrome_height()`); re-assert here as
        // a belt for callers that mutate `sends` directly (tests).
        self.normalize_selection();

        // Send rows: [swatch] label | [ channel name ▼ ] | ×
        const SWATCH_W: f32 = 8.0;
        const LABEL_W: f32 = 70.0;
        self.send_ids = vec![SendRowIds::default(); rows];
        for (i, send) in self.sends.iter().enumerate() {
            // Identity-colour swatch — a button that selects this send for the
            // scope. The selected row's swatch fills the row height; others are a
            // small dot.
            let selected = Some(&send.id) == self.selected_send.as_ref();
            // D7: an explicit row highlight for the selected send — the swatch
            // height difference alone reads as noise, not selection (the
            // master-detail scoping below, "Spectrogram — <name>", was
            // otherwise invisible). Painted first so every row control below
            // draws on top of it; identity-coloured at low alpha so it stays
            // legible without competing with the row's own accents.
            if selected {
                let sc = super::audio_send_color(&send.id);
                tree.add_panel(
                    Some(self.content_parent),
                    inner_x - 4.0,
                    cy - 1.0,
                    inner_w + 8.0,
                    ROW_H + 2.0,
                    UIStyle {
                        bg_color: Color32::new(sc.r, sc.g, sc.b, 28),
                        border_color: Color32::new(sc.r, sc.g, sc.b, 110),
                        border_width: 1.0,
                        corner_radius: color::SMALL_RADIUS,
                        ..UIStyle::default()
                    },
                );
            }
            let (swatch_h, swatch_y) = if selected {
                (ROW_H, cy)
            } else {
                (12.0, cy + (ROW_H - 12.0) * 0.5)
            };
            self.send_ids[i].swatch = tree.add_button_keyed(
                Some(self.content_parent),
                inner_x,
                swatch_y,
                SWATCH_W,
                swatch_h,
                UIStyle {
                    bg_color: super::audio_send_color(&send.id),
                    hover_bg_color: super::audio_send_color(&send.id),
                    corner_radius: color::SMALL_RADIUS,
                    ..UIStyle::default()
                },
                "",
                send_row_key(i, SEND_OFF_SWATCH),
            );

            // Label is a button — clicking it opens the inline rename editor. A
            // send with active trigger routes reads in an amber accent, so which
            // sends fire visuals is legible without selecting each one.
            let label_x = inner_x + SWATCH_W + 6.0;
            let has_triggers = send.has_clip_triggers;
            let mut lbl_style = label_button_style();
            if has_triggers {
                lbl_style.text_color = Color32::new(240, 196, 110, 255); // amber
            }
            self.send_ids[i].label = tree.add_button_keyed(
                Some(self.content_parent),
                label_x,
                cy,
                LABEL_W,
                ROW_H,
                lbl_style,
                &send.label,
                send_row_key(i, SEND_OFF_LABEL),
            );

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
            self.send_ids[i].delete = tree.add_button_keyed(
                Some(self.content_parent),
                inner_x + inner_w - STEP_W,
                cy,
                STEP_W,
                ROW_H,
                del_style,
                delete_label,
                send_row_key(i, SEND_OFF_DELETE),
            );

            // Gain stepper [−] value [＋], left of the delete button. Discrete
            // 1 dB steps; the value is read-only display (0 dB = unity).
            // the channel
            // dropdown below enumerates stereo pairs AND single channels
            // directly, so mono falls out of picking one.
            let gain_x = inner_x + inner_w - STEP_W - 4.0 - GAIN_W;
            self.send_ids[i].gain_minus = tree.add_button_keyed(
                Some(self.content_parent),
                gain_x,
                cy,
                GAIN_BTN_W,
                ROW_H,
                btn_style(false),
                "\u{2212}", // −
                send_row_key(i, SEND_OFF_GAIN_MINUS),
            );
            // Stable structural names (D8 flow-testing seam): the row's own
            // "−"/"+" glyphs collide with the timeline zoom control's
            // identical text, so an automation `Query{text:"+"}` picks the
            // wrong widget without a `name` to disambiguate by node type.
            // `nth` still does the per-row pick among rows sharing this name
            // (`&'static str` per `set_name`'s contract — see `tree.rs`).
            tree.set_name(self.send_ids[i].gain_minus, "audio_setup.gain_minus");
            // Value label doubles as a D7 horizontal drag zone — pointer-down
            // arms `gain_drag_target`, matching the crossover-drag pattern.
            self.send_ids[i].gain_value = tree.add_label(
                Some(self.content_parent),
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
            self.send_ids[i].gain_plus = tree.add_button_keyed(
                Some(self.content_parent),
                gain_x + GAIN_BTN_W + GAIN_VAL_W,
                cy,
                GAIN_BTN_W,
                ROW_H,
                btn_style(false),
                "\u{002B}", // +
                send_row_key(i, SEND_OFF_GAIN_PLUS),
            );
            tree.set_name(self.send_ids[i].gain_plus, "audio_setup.gain_plus");
            tree.set_name(self.send_ids[i].gain_value, "audio_setup.gain_value");

            // Channel dropdown fills the row (Cap chip removed, §7.2 item 7,
            // P8, 2026-07-11 — device-vs-layer-fed detail lives in the
            // read-only Inputs section now, not a row-level chip), showing
            // the resolved channel name(s).
            let ch_x = label_x + LABEL_W + 4.0;
            let ch_w = (gain_x - 4.0 - ch_x).max(40.0);
            self.send_ids[i].ch_dropdown = tree.add_button_keyed(
                Some(self.content_parent),
                ch_x,
                cy,
                ch_w,
                ROW_H,
                dropdown_trigger_style(),
                &send.channel_label,
                send_row_key(i, SEND_OFF_CH_DROPDOWN),
            );

            // Level meter: a thin track under the channel dropdown with a fill
            // node resized each frame from the live send level. Identity-colored.
            let meter_h = 2.0;
            let meter_x = ch_x;
            let meter_y = cy + ROW_H - meter_h;
            let meter_w = ch_w;
            let track = tree.add_panel(
                Some(self.content_parent),
                meter_x,
                meter_y,
                meter_w,
                meter_h,
                UIStyle { bg_color: Color32::new(40, 40, 46, 255), ..UIStyle::default() },
            );
            let fill = tree.add_panel(
                Some(self.content_parent),
                meter_x,
                meter_y,
                0.0, // width set per frame by update_meters
                meter_h,
                UIStyle { bg_color: super::audio_send_color(&send.id), ..UIStyle::default() },
            );
            self.send_ids[i].meter_track = track;
            self.send_ids[i].meter_fill = fill;

            cy += ROW_H + ROW_GAP;
        }

        // Add-send button.
        self.add_send_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            btn_style(false),
            "+ Add Source",
            KEY_ADD_SEND,
        );
        cy += ROW_H;

        // ── Spectrogram scope (selected send) ──
        self.scope_rect = None;
        self.scope_readout_label = None;
        self.band_meter_ids = [(None, None, None); 3];
        if !self.sends.is_empty() {
            cy += ROW_GAP;
            cy = self.build_inputs_section(tree, inner_x, inner_w, cy);
            let sel_label = self
                .selected_send
                .as_ref()
                .and_then(|id| self.sends.iter().find(|s| &s.id == id))
                .map(|s| s.label.as_str())
                .unwrap_or("—");
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                SCOPE_TITLE_H,
                &format!("Spectrogram — {sel_label}"),
                UIStyle {
                    text_color: color::TEXT_DIMMED,
                    font_size: color::FONT_LABEL,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            // Pre-analysis floor stepper ("Floor [−] val [＋]"): the spectrogram
            // squelch. Bins below it are gated before display + detection, so the
            // wash blacks out as it's raised. Sits in the title row, left of the
            // hover readout. "Off" = no gate.
            self.floor_minus_id = None;
            self.floor_plus_id = None;
            let floor_db = self
                .selected_send
                .as_ref()
                .and_then(|id| self.sends.iter().find(|s| &s.id == id))
                .map(|s| s.floor_db)
                .unwrap_or(crate::types::FLOOR_DB_OFF);
            let floor_text = if floor_db <= crate::types::FLOOR_DB_OFF {
                "Off".to_string()
            } else {
                format!("{floor_db:.0} dB")
            };
            let fl_label_w = 30.0;
            let fl_btn = 16.0;
            let fl_val = 52.0;
            let mut fx = inner_x + inner_w * 0.40;
            tree.add_label(
                Some(self.content_parent),
                fx,
                cy,
                fl_label_w,
                SCOPE_TITLE_H,
                "Floor",
                UIStyle {
                    text_color: Color32::new(150, 150, 160, 255),
                    font_size: color::FONT_LABEL,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            fx += fl_label_w;
            self.floor_minus_id = Some(tree.add_button_keyed(
                Some(self.content_parent),
                fx,
                cy,
                fl_btn,
                SCOPE_TITLE_H,
                btn_style(false),
                "\u{2212}",
                KEY_FLOOR_MINUS,
            ));
            tree.add_label(
                Some(self.content_parent),
                fx + fl_btn,
                cy,
                fl_val,
                SCOPE_TITLE_H,
                &floor_text,
                UIStyle {
                    text_color: Color32::new(190, 190, 198, 255),
                    font_size: color::FONT_LABEL,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
            );
            self.floor_plus_id = Some(tree.add_button_keyed(
                Some(self.content_parent),
                fx + fl_btn + fl_val,
                cy,
                fl_btn,
                SCOPE_TITLE_H,
                btn_style(false),
                "\u{002B}",
                KEY_FLOOR_PLUS,
            ));

            // Hover readout (freq + dB at the cursor), right-aligned in the same
            // title row — outside the waterfall rect, so the present pass's blit
            // doesn't cover it. Empty until the app feeds a value on hover.
            self.scope_readout_label = Some(tree.add_label(
                Some(self.content_parent),
                inner_x + inner_w * 0.62,
                cy,
                inner_w * 0.38,
                SCOPE_TITLE_H,
                "",
                UIStyle {
                    text_color: Color32::new(150, 200, 230, 255),
                    font_size: color::FONT_LABEL,
                    text_align: TextAlign::Right,
                    ..UIStyle::default()
                },
            ));
            cy += SCOPE_TITLE_H;

            // Backing panel behind the whole scope (axis margin + waterfall).
            tree.add_panel(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                self.scope_h,
                UIStyle {
                    bg_color: Color32::new(10, 10, 12, 255),
                    border_color: Color32::new(48, 48, 52, 255),
                    border_width: 1.0,
                    corner_radius: color::BUTTON_RADIUS,
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
                    Some(self.content_parent),
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
            // Onset tick-lane legend, as small backed chips (D7 readability —
            // plain text was illegible stacked over a busy spectrogram
            // frame). "Low"/"Mid"/"High" are repositioned onto their divider
            // lines every frame by `update_scope_lane_labels`; any other lane
            // (e.g. "Kick") keeps the original bottom-stacked axis-gutter
            // slot. Chip background + label created at zero size here; both
            // positioned/resized every frame by `update_scope_lane_labels`.
            self.scope_lane_label_ids.clear();
            self.scope_lane_chip_ids.clear();
            for (name, color) in &self.scope_lane_legend {
                let chip = tree.add_panel(
                    Some(self.content_parent),
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    UIStyle {
                        bg_color: Color32::new(12, 12, 15, 220),
                        border_color: Color32::new(color.r, color.g, color.b, 130),
                        border_width: 1.0,
                        corner_radius: color::HAIRLINE_RADIUS,
                        ..UIStyle::default()
                    },
                );
                let id = tree.add_label(
                    Some(self.content_parent),
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    name,
                    UIStyle {
                        text_color: *color,
                        font_size: color::FONT_LABEL,
                        text_align: TextAlign::Center,
                        ..UIStyle::default()
                    },
                );
                self.scope_lane_chip_ids.push(chip);
                self.scope_lane_label_ids.push(id);
            }

            for (band, slot) in self.band_meter_ids.iter_mut().enumerate() {
                let label = tree.add_label(
                    Some(self.content_parent),
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
                );
                let track = tree.add_panel(
                    Some(self.content_parent),
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    UIStyle {
                        // Visibly lighter than the black scope so the empty part
                        // of the bar reads as a scale, not just background.
                        bg_color: Color32::new(54, 54, 62, 255),
                        corner_radius: color::HAIRLINE_RADIUS,
                        ..UIStyle::default()
                    },
                );
                let fill = tree.add_panel(
                    Some(self.content_parent),
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    UIStyle { bg_color: band_color(band), corner_radius: color::HAIRLINE_RADIUS, ..UIStyle::default() },
                );
                *slot = (Some(track), Some(fill), Some(label));
            }

            // Position the lane legend now so it is correct from the first
            // frame (and in headless snapshots, which don't run the per-frame
            // meter updates); `update_scope_lane_labels` re-tracks resizes.
            self.update_scope_lane_labels(tree);

            // The Audio Setup Triggers matrix (`build_trigger_section`) is
            // deleted (P3, D2): clip triggers are authored on the layer only.
            // Consumers (below) is the panel's sole surviving trigger display.
            cy += self.scope_h + ROW_GAP;

            // ── Consumers (selected send) ──
            cy = self.build_consumers_section(tree, inner_x, inner_w, cy);
        }

        // ── Close the scroll body (D8/BUG-047) ──
        // Content was built at absolute coords from `content_top`; the total
        // content height is the distance from there to the last row's bottom
        // (plus a trailing pad so the final row isn't flush to the clip edge).
        let content_height = (cy - content_top + PAD).max(0.0);
        self.scroll.set_content_height(content_height);
        self.scroll.reparent_content(tree, content_start);
        // Apply the current scroll offset by shifting the reparented content up.
        let offset = self.scroll.scroll_offset();
        if offset != 0.0 {
            self.scroll.offset_content(tree, -offset);
            // `scope_rect` isn't a tree node — it's plain geometry the present
            // pass reads directly for the spectrogram blit and the hit-tests
            // (`point_in_scope`, `scope_line_y`/`scope_y_to_hz`) use for divider
            // dragging. It was captured pre-scroll above and never shifted, so a
            // scrolled body left the waterfall detached from its section header
            // (BUG-101). Apply the same shift the tree content just got.
            if let Some(r) = self.scope_rect.as_mut() {
                r.y -= offset;
            }
        }
        // Scrollbar in the panel's right padding gutter, only visible when the
        // body overflows (the container hides it otherwise).
        let sb_x = x + self.panel_w - SCROLLBAR_W - 2.0;
        self.scroll.build_scrollbar(tree, sb_x, &scrollbar_style());
    }

    /// Build the Inputs section for the selected send: READ-ONLY routing
    /// display — the device line (when capturing) plus one line per feeding
    /// layer, straight from `AudioSendRow::routings`.
    /// Authoring ("+ Layer", per-layer ×, `AudioSendAddLayerClicked`)
    /// is gone: the panel's job is device in → sends → scope → who's
    /// listening, not routing edits — the layer header's Send dropdown is
    /// the one surviving authoring path (same `SetLayerAudioSend` command,
    /// one owner). Returns the y past the section.
    fn build_inputs_section(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32) -> f32 {
        let routings: Vec<String> = self
            .selected_send
            .as_ref()
            .and_then(|id| self.sends.iter().find(|s| &s.id == id))
            .map(|s| s.routings.clone())
            .unwrap_or_default();

        // Per-send section header, matching "Spectrogram — X" / "Triggers — X".
        let sel_label = self
            .selected_send
            .as_ref()
            .and_then(|id| self.sends.iter().find(|s| &s.id == id))
            .map(|s| s.label.as_str())
            .unwrap_or("—");
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            TRIG_TITLE_H,
            &format!("Inputs — {sel_label}"),
            UIStyle {
                text_color: color::TEXT_DIMMED,
                font_size: color::FONT_LABEL,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        cy += TRIG_TITLE_H;

        if routings.is_empty() {
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                "Not routed",
                label_style(),
            );
            cy += ROW_H + ROW_GAP;
            return cy;
        }

        for line in &routings {
            // D8: a feeding layer that no longer exists (deleted since it was
            // routed) reads as bare "Layer • (missing layer)" — the sentinel
            // `state_sync.rs` returns when `layer_name` can't resolve the id.
            // Say what happened and point at the ONE surviving repair path
            // now that the Inputs section itself is read-only.
            let (row_text, row_style) = if line.ends_with(MISSING_LAYER_LABEL) {
                (
                    "Input layer was deleted \u{2014} choose a replacement from the \
                     layer header's Send dropdown"
                        .to_string(),
                    UIStyle {
                        text_color: Color32::new(232, 168, 92, 255), // amber
                        font_size: color::FONT_LABEL,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
                )
            } else {
                (line.clone(), label_style())
            };
            tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, &row_text, row_style);
            cy += ROW_H + ROW_GAP;
        }
        cy
    }

    /// Build the Consumers section for the selected send: one plain-button
    /// row per consumer (or a dim "no consumers yet" line when there are
    /// none). Click emits [`PanelAction::LayerClicked`] for the owning layer —
    /// navigational only, never editable (D3). Returns the y past the section.
    fn build_consumers_section(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32) -> f32 {
        self.consumer_row_ids.clear();
        let consumers: Vec<SendConsumerRow> = self
            .selected_send
            .as_ref()
            .and_then(|id| self.sends.iter().find(|s| &s.id == id))
            .map(|s| s.consumers.clone())
            .unwrap_or_default();

        // Per-send section header, matching "Spectrogram — X" / "Triggers — X".
        let sel_label = self
            .selected_send
            .as_ref()
            .and_then(|id| self.sends.iter().find(|s| &s.id == id))
            .map(|s| s.label.as_str())
            .unwrap_or("—");
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            TRIG_TITLE_H,
            &format!("Consumers — {sel_label}"),
            UIStyle {
                text_color: color::TEXT_DIMMED,
                font_size: color::FONT_LABEL,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        cy += TRIG_TITLE_H;

        if consumers.is_empty() {
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                "No consumers yet",
                UIStyle {
                    text_color: Color32::new(120, 120, 130, 255),
                    font_size: color::FONT_LABEL,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            cy += ROW_H + ROW_GAP;
        } else {
            for (i, c) in consumers.iter().enumerate() {
                let id = tree.add_button_keyed(
                    Some(self.content_parent),
                    inner_x,
                    cy,
                    inner_w,
                    ROW_H,
                    label_button_style(),
                    &c.label,
                    KEY_CONSUMER_ROW_BASE + i as u64,
                );
                // Jump affordance: a muted "›" at the row's right edge (same
                // muted tone as the panel's dropdown chevrons), so the only
                // navigational rows in the panel read as "click to jump".
                // A plain label drawn over the button — non-interactive, so
                // the button under it stays the hit target.
                tree.add_label(
                    Some(self.content_parent),
                    inner_x + inner_w - STEP_W,
                    cy,
                    STEP_W,
                    ROW_H,
                    "\u{203A}", // ›
                    UIStyle {
                        text_color: Color32::new(150, 150, 160, 255),
                        font_size: color::FONT_LABEL,
                        text_align: TextAlign::Center,
                        ..UIStyle::default()
                    },
                );
                self.consumer_row_ids.push(id);
                cy += ROW_H + ROW_GAP;
            }
        }
        cy
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

    /// The Floor `−` stepper button's current node id, if built (a send is
    /// selected). Exposed for the cross-crate double-click regression test in
    /// `manifold-app` (BUG-059 keyed-parent churn) — the field is otherwise
    /// crate-private.
    pub fn floor_minus_id(&self) -> Option<NodeId> {
        self.floor_minus_id
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
        matches!(self.drag.payload(), Some(AudioSetupDrag::Band(_)))
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

    /// Whether a screen point lies inside the built panel — the ownership test
    /// `claims_drag` uses (BUG-059, superseded by `docs/DRAG_CAPTURE_DESIGN.md`
    /// P2). False before first build (zero rect contains nothing).
    fn point_in_panel(&self, pos: Vec2) -> bool {
        pos.x >= self.panel_rect.x
            && pos.x <= self.panel_rect.x + self.panel_rect.width
            && pos.y >= self.panel_rect.y
            && pos.y <= self.panel_rect.y + self.panel_rect.height
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

    /// Send id + current gain (dB) for a gain-value label node, if `id` is
    /// one — arms the D7 gain drag on `PointerDown`.
    fn gain_drag_target(&self, id: NodeId) -> Option<(AudioSendId, f32)> {
        self.send_ids
            .iter()
            .zip(self.sends.iter())
            .find(|(ids, _)| ids.gain_value == id)
            .map(|(_, send)| (send.id.clone(), send.gain_db))
    }

    // `sensitivity_drag_target` (the matrix's D7 sensitivity drag) is deleted
    // with the Audio Setup Triggers matrix (P3, D2).

    /// Whether `id` is any node this panel owns (background or an interactive
    /// control) — the caller swallows such clicks so they don't fall through to
    /// the canvas behind the modal.
    pub fn owns_node(&self, id: NodeId) -> bool {
        if id == self.bg_id
            || id == self.close_id
            || id == self.device_dropdown_id
            || id == self.add_send_id
            || self.floor_minus_id == Some(id)
            || self.floor_plus_id == Some(id)
        {
            return true;
        }
        if self.send_ids.iter().any(|r| {
            id == r.swatch
                || id == r.label
                || id == r.ch_dropdown
                || id == r.gain_minus
                || id == r.gain_plus
                || id == r.gain_value
                || id == r.delete
        }) {
            return true;
        }
        self.consumer_row_ids.contains(&id)
    }

    /// Resize each send's meter fill from live levels (RMS 0..1). Called every
    /// frame while open — mutates existing nodes in place, no rebuild. Levels are
    /// indexed by send order. A small visual gain makes quiet signals legible.
    pub fn update_meters(&self, tree: &mut UITree, levels: &[f32]) {
        for (i, ids) in self.send_ids.iter().enumerate() {
            let level = levels.get(i).copied().unwrap_or(0.0);
            let shown = (level * 2.5).clamp(0.0, 1.0); // ~ -8 dB reaches full scale
            // Read the track's CURRENT bounds, not a build-time cache, so an
            // in-place scroll shift lands under the fill too.
            let track_rect = tree.get_bounds(ids.meter_track);
            let w = track_rect.width * shown;
            tree.set_bounds(
                ids.meter_fill,
                Rect::new(track_rect.x, track_rect.y, w, track_rect.height),
            );
        }
    }

    /// Position + fill the per-band level meters from the tapped send's per-band
    /// amplitudes `[low, mid, high]` (each 0..1), every frame while open. Each
    /// bar sits at the geometric centre of its band slab — so it lines up with
    /// the frequency axis — and follows the crossovers as they're dragged.
    /// `None`, or a dark scope, hides the bars.
    /// Feed the onset tick-lane legend (name + colour per lane, bottom-up, and
    /// the per-lane height fraction) from the one lane definition in
    /// `manifold_spectral::scope`. Called once by the app at startup, before
    /// the panel first builds.
    pub fn set_scope_lane_legend(&mut self, legend: Vec<(String, Color32)>, lane_frac: f32) {
        self.scope_lane_legend = legend;
        self.scope_lane_frac = lane_frac;
    }

    /// Position the tick-lane legend (D7 readability fix). The original
    /// scheme packed every lane's label bottom-up inside its `lane_frac` of
    /// the scope (~1.4% each) — four rows of text crammed into a corner a
    /// few px tall, which is the collision Peter's 2026-07-09 screenshot
    /// showed. The three frequency-named lanes now anchor to the SAME
    /// divider-line y [`Self::scope_line_y`] already computes for the drag
    /// hit-test, as a small backed chip over the waterfall's left edge:
    /// "Low" sits on the low/mid divider, "Mid" on the mid/high divider,
    /// "High" (no divider above it) sits just under the scope's top edge.
    /// Any other lane name (e.g. "Kick" — not a frequency band) keeps the
    /// original bottom-stacked axis-gutter slot; with the three band lanes
    /// pulled out, that slot no longer collides. Called every frame beside
    /// [`Self::update_band_meters`].
    pub fn update_scope_lane_labels(&self, tree: &mut UITree) {
        let label_h = LANE_CHIP_H;
        let Some(rect) = self.scope_rect().filter(|_| self.scope_lane_frac > 0.0) else {
            for &id in &self.scope_lane_label_ids {
                tree.set_visible(id, false);
            }
            for &id in &self.scope_lane_chip_ids {
                tree.set_visible(id, false);
            }
            return;
        };
        let gutter_x = rect.x - SCOPE_AXIS_W + 2.0;
        let gutter_w = SCOPE_AXIS_W - 6.0;
        let chip_x = rect.x + 4.0;
        let range_valid = self.scope_fmin > 0.0 && self.scope_fmax > self.scope_fmin;
        let mut prev_top = f32::INFINITY;
        for (i, (&label_id, &chip_id)) in
            self.scope_lane_label_ids.iter().zip(self.scope_lane_chip_ids.iter()).enumerate()
        {
            let name = self.scope_lane_legend.get(i).map(|(n, _)| n.as_str()).unwrap_or("");
            let divider_y = match name {
                "Low" => self.scope_line_y(self.scope_low_hz),
                "Mid" => self.scope_line_y(self.scope_mid_hz),
                "High" if range_valid => Some(rect.y + 2.0 + label_h * 0.5),
                _ => None,
            };
            let (x, w, y) = if let Some(center_y) = divider_y {
                (chip_x, LANE_CHIP_W, center_y - label_h * 0.5)
            } else {
                // Bottom-stacked fallback: any lane not named above (e.g.
                // "Kick"), or a band lane whose divider isn't valid yet
                // (scope dark / crossovers not pushed this frame).
                let lane_center =
                    rect.y + rect.height * (1.0 - self.scope_lane_frac * (i as f32 + 0.5));
                let y = (lane_center - label_h * 0.5).min(prev_top - label_h);
                prev_top = y;
                (gutter_x, gutter_w, y)
            };
            tree.set_bounds(chip_id, Rect::new(x, y, w, label_h));
            tree.set_bounds(label_id, Rect::new(x, y, w, label_h));
            tree.set_visible(chip_id, true);
            tree.set_visible(label_id, true);
        }
    }

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
            let (Some(track), Some(fill), Some(label)) = (track, fill, label) else {
                continue;
            };
            let (lo, hi) = edges[i];
            let center_y = amps
                .filter(|_| lo > 0.0 && hi > lo)
                .and_then(|_| self.scope_line_y((lo * hi).sqrt()));
            match (amps, center_y) {
                (Some(a), Some(y)) => {
                    let amp = a[i].clamp(0.0, 1.0);
                    let top = y - BAND_METER_HALF_H;
                    let h = BAND_METER_HALF_H * 2.0;
                    tree.set_bounds(track, Rect::new(bar_x, top, bar_w, h));
                    tree.set_bounds(fill, Rect::new(bar_x, top, bar_w * amp, h));
                    tree.set_bounds(
                        label,
                        Rect::new(label_x, y - label_h * 0.5, BAND_METER_LABEL_W, label_h),
                    );
                    tree.set_visible(track, true);
                    tree.set_visible(fill, true);
                    tree.set_visible(label, true);
                }
                _ => {
                    tree.set_visible(track, false);
                    tree.set_visible(fill, false);
                    tree.set_visible(label, false);
                }
            }
        }
    }

    // `update_trigger_levels` (the matrix's per-row live meter) is deleted
    // with the Audio Setup Triggers matrix (P3, D2). The D6 fire meter that
    // replaces it lives in the audio-mod drawer, not this panel — deferred to
    // a follow-up phase (see this phase's landing notes).

    /// Update the scope's hover readout text in place (no rebuild). `Some(text)`
    /// shows it (freq + dB under the cursor); `None` hides it. Called every frame
    /// while open, mirroring [`update_meters`](Self::update_meters).
    pub fn update_scope_readout(&self, tree: &mut UITree, text: Option<&str>) {
        let Some(id) = self.scope_readout_label else {
            return;
        };
        match text {
            Some(t) => {
                tree.set_text(id, t);
                tree.set_visible(id, true);
            }
            None => tree.set_visible(id, false),
        }
    }

    /// Screen rect of a send's label button (the inline-rename anchor), or
    /// `None` if the send isn't currently built.
    pub fn send_label_rect(&self, tree: &UITree, id: &AudioSendId) -> Option<Rect> {
        let i = self.sends.iter().position(|s| &s.id == id)?;
        let node = self.send_ids.get(i)?;
        Some(tree.get_bounds(node.label))
    }

    /// Resolve a clicked node id to a [`PanelAction`], or `None` if it hit
    /// nothing interactive. Closing the panel is handled here (returns `None`
    /// after toggling closed) so the caller just dispatches the action.
    pub fn handle_click(&mut self, id: NodeId) -> Option<PanelAction> {
        if id == self.close_id {
            self.open = false;
            return None;
        }
        self.handle_click_inner(id)
    }

    fn handle_click_inner(&mut self, id: NodeId) -> Option<PanelAction> {
        if id == self.device_dropdown_id {
            self.delete_armed = None;
            // App opens the device dropdown anchored to this trigger.
            return Some(PanelAction::AudioSetupDeviceClicked);
        }
        if id == self.add_send_id {
            self.delete_armed = None;
            return Some(PanelAction::AudioAddSend);
        }
        // Pre-analysis floor stepper (the spectrogram squelch) for the selected send.
        if self.floor_minus_id == Some(id) || self.floor_plus_id == Some(id) {
            self.delete_armed = None;
            let send = self.selected_send.clone()?;
            let delta = if self.floor_plus_id == Some(id) { 6.0 } else { -6.0 };
            return Some(PanelAction::AudioSendFloorStep(send, delta));
        }
        // Inputs section authoring ("+ Layer" / per-layer ×) deleted —
        // the section is read-only routing display
        // now; the layer header's Send dropdown is the one surviving path to
        // `SetLayerAudioSend`.
        // Consumers section: navigate to the owning layer — read-only, no
        // edit (D3).
        if let Some(i) = self.consumer_row_ids.iter().position(|&r| r == id) {
            self.delete_armed = None;
            let consumers = self
                .selected_send
                .as_ref()
                .and_then(|sid| self.sends.iter().find(|s| &s.id == sid))
                .map(|s| s.consumers.clone())
                .unwrap_or_default();
            let layer_id = consumers.get(i)?.layer_id.clone()?;
            return Some(PanelAction::LayerClicked(layer_id, Modifiers::NONE));
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
            } else if id == ids.delete {
                Some((i, RowControl::Delete))
            } else {
                None
            }
        });
        // The Audio Setup Triggers matrix's click block (band routes:
        // toggle/sensitivity/length/layer) is deleted with the matrix (P3, D2).

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

impl AudioSetupPanel {
    /// Route one event to the docked panel. Returns `(consumed, actions)` —
    /// `consumed` means the caller stops routing this event to lower panels.
    /// Called from `UIRoot::process_events` at the docked-panel site (the panel
    /// is no longer an `Overlay`; D1/§3.5). Escape is NOT handled here — it
    /// moved to the app's single key-dispatch site so there is ONE close path,
    /// not two. The close (×) button and the header Audio button both toggle
    /// the dock via `PanelAction::OpenAudioSetup`.
    pub fn handle_event(&mut self, event: &UIEvent) -> (bool, Vec<PanelAction>) {
        match event {
            UIEvent::Click { node_id, pos, .. } => {
                let id = *node_id;
                if id == self.close_id {
                    // One toggle path: the app closes the dock (width → 0 +
                    // panel.close()) on this action, same as Escape / header.
                    (true, vec![PanelAction::OpenAudioSetup])
                } else if let Some(action) = self.handle_click_inner(id) {
                    (true, vec![action])
                } else if self.owns_node(id) || self.point_in_scope(*pos) {
                    // A panel-owned control or the waterfall — swallow so the
                    // click doesn't fall through to a panel below the dock.
                    (true, Vec::new())
                } else {
                    (false, Vec::new())
                }
            }
            // right-click resets the gain stepper to
            // unity (0 dB) — the SAME intrinsic-reset gesture every other
            // value control in the app uses (`PanelAction::slider_reset`,
            // e.g. the layer header's audio-gain fader at
            // `layer_header.rs:1997` and the audio-mod drawer's Amount/
            // Attack/Release/Decay sliders, `param_slider_shared.rs`).
            // Verified, not assumed: this panel's own gesture inventory has
            // exactly one OTHER reset — double-click on the dock's resize
            // HANDLE (`window_input.rs:343`) — but that resets a layout
            // WIDTH, a different affordance from resetting a CONTROL's
            // VALUE; every value-reset precedent in the codebase is
            // right-click, so the gain stepper (a value control) matches
            // that family, not the resize handle. Hits any of the three
            // gain-row nodes (minus / value / plus) for the row it belongs
            // to. Replays the drag trio at 0.0 so undo == a drag to unity,
            // same as every other `slider_reset` site.
            UIEvent::RightClick { node_id: Some(id), .. } => {
                let id = *id;
                if let Some((zone, send)) =
                    self.send_ids.iter().zip(self.sends.iter()).find_map(|(ids, send)| {
                        if id == ids.gain_minus {
                            Some((crate::stepper::StepperZone::Minus, send))
                        } else if id == ids.gain_value {
                            Some((crate::stepper::StepperZone::Value, send))
                        } else if id == ids.gain_plus {
                            Some((crate::stepper::StepperZone::Plus, send))
                        } else {
                            None
                        }
                    })
                    && let Some(crate::stepper::StepperIntent::ResetToDefault) =
                        crate::stepper::Stepper::intent_for(zone, crate::intent::Gesture::RightClick)
                {
                    let send_id = send.id.clone();
                    (
                        true,
                        vec![PanelAction::slider_reset(
                            PanelAction::AudioSendGainDragBegin(send_id.clone()),
                            PanelAction::AudioSendGainDragChanged(send_id.clone(), 0.0),
                            PanelAction::AudioSendGainDragCommit(send_id),
                        )],
                    )
                } else {
                    (false, Vec::new())
                }
            }
            // P4 (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D8's audio-dock
            // sibling, `stepper.rs`'s contract-table amendment): double-click
            // on the gain value cell opens its type-in box. The gain row IS
            // this crate's one live `Stepper` instance (stepper.rs's own
            // module doc), so its contract — not `value_cell.rs`'s — is the
            // right one to consult here, same as the `RightClick` reset arm
            // above. Uses the SAME `gain_drag_target` lookup `PointerDown`
            // below uses to arm the calibration drag, so drag-arming and
            // type-in registration can't drift apart.
            UIEvent::DoubleClick { node_id, .. } => {
                if let Some((send, db)) = self.gain_drag_target(*node_id)
                    && let Some(crate::stepper::StepperIntent::EditValue) = crate::stepper::Stepper::intent_for(
                        crate::stepper::StepperZone::Value,
                        crate::intent::Gesture::DoubleClick,
                    )
                {
                    (true, vec![PanelAction::AudioSendGainBeginTextInput(send, db, *node_id)])
                } else {
                    (false, Vec::new())
                }
            }
            // ── Band-divider drag (Low/Mid/High crossovers) + D7 gain-value
            // calibration drag ──────────────────────────────────────
            // Arm on press if it lands on a divider line or a value label;
            // thereafter the drag owns the gesture until release. Divider
            // lines are drawn shader-side (hit-test by position); value
            // labels are real nodes (hit-test by node id). (The matrix's
            // sensitivity-value drag arm is deleted with the matrix, P3 D2.)
            UIEvent::PointerDown { node_id, pos, modifiers } => {
                if let Some(band) = self.divider_at(*pos) {
                    self.drag.start(AudioSetupDrag::Band(band), *pos);
                    (true, vec![PanelAction::AudioCrossoverDragBegin])
                } else if let Some((send, start_db)) = self.gain_drag_target(*node_id) {
                    self.drag.start(
                        AudioSetupDrag::Calibration(CalibrationDrag::Gain {
                            send: send.clone(),
                            start_x: pos.x,
                            start_db,
                            fine: modifiers.shift,
                        }),
                        *pos,
                    );
                    (true, vec![PanelAction::AudioSendGainDragBegin(send)])
                } else if self.owns_node(*node_id) || self.point_in_scope(*pos) {
                    (true, Vec::new())
                } else {
                    (false, Vec::new())
                }
            }
            UIEvent::DragBegin { .. } => {
                if self.drag.is_active() {
                    (true, Vec::new())
                } else {
                    (false, Vec::new())
                }
            }
            UIEvent::Drag { pos, .. } => match self.drag.payload().cloned() {
                Some(AudioSetupDrag::Band(band)) => match self.scope_y_to_hz(pos.y) {
                    Some(hz) => (true, vec![PanelAction::AudioCrossoverChanged(band, hz)]),
                    None => (true, Vec::new()),
                },
                // 1 px = 0.1 dB / 0.5% (D7, `docs/AUDIO_SENDS_UX_DESIGN.md`
                // §3.4); the host clamps the candidate to the real range.
                // P4, D8: Shift held at drag-start ("fine") multiplies the
                // applied per-pixel delta by 0.1.
                Some(AudioSetupDrag::Calibration(CalibrationDrag::Gain { send, start_x, start_db, fine })) => {
                    let db_per_px = if fine { 0.01 } else { 0.1 };
                    let new_db = start_db + (pos.x - start_x) * db_per_px;
                    (true, vec![PanelAction::AudioSendGainDragChanged(send, new_db)])
                }
                None => (false, Vec::new()),
            },
            UIEvent::DragEnd { .. } | UIEvent::PointerUp { .. } => match self.drag.release() {
                Some(AudioSetupDrag::Band(_)) => (true, vec![PanelAction::AudioCrossoverCommit]),
                Some(AudioSetupDrag::Calibration(CalibrationDrag::Gain { send, .. })) => {
                    (true, vec![PanelAction::AudioSendGainDragCommit(send)])
                }
                None => (false, Vec::new()),
            },
            // mouse-wheel scroll over the docked body, routed here by
            // `window_input.rs`'s `primary_mouse_wheel` through the generic
            // `UIEvent::Scroll` pipeline (same mechanism the dropdown uses) —
            // `window_input` already gated on `layout.audio_setup().contains(pos)`
            // before emitting this, so no further position check is needed here.
            // `window_input.rs`'s dock-scroll branch also sets
            // `needs_rebuild` so the next frame actually re-applies the
            // new offset.
            UIEvent::Scroll { delta, .. } => {
                self.handle_scroll(delta.y);
                (true, Vec::new())
            }
            _ => (false, Vec::new()),
        }
    }

    /// Does a drag ORIGINATING at `origin` belong to the dock — an armed
    /// band/calibration grab, or the origin lands inside the dock rect. Read by
    /// `UIRoot::resolve_drag_owner` once, at the gesture's first `DragBegin`.
    pub fn claims_drag(&self, origin: Vec2) -> bool {
        self.drag.is_active() || self.point_in_panel(origin)
    }

    /// Idempotent end-of-gesture clear (band + calibration drags).
    pub fn gesture_ended(&mut self) {
        self.drag.cancel();
    }

    /// True iff the last `PointerDown` just armed a band-divider grab — the
    /// caller requests zero-threshold drag so a 1px move starts the drag.
    pub fn wants_immediate_drag(&self) -> bool {
        matches!(self.drag.payload(), Some(AudioSetupDrag::Band(_)))
    }
}

/// Scrollbar chrome for the docked body (D8) — the shared inspector palette.
fn scrollbar_style() -> ScrollbarStyle {
    ScrollbarStyle {
        track_color: color::SCROLLBAR_TRACK_C32,
        thumb_color: color::SCROLLBAR_THUMB_C32,
        thumb_hover_color: color::SCROLLBAR_THUMB_HOVER_C32,
        corner_radius: color::SMALL_RADIUS,
    }
}

fn btn_style(active: bool) -> UIStyle {
    // An option selector — the kit segmented-control cell (selected raises onto the
    // control level, the rest sit at panel level).
    UIStyle {
        font_size: BTN_FONT,
        ..crate::chrome::components::segment_style(active)
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

// `trigger_swatch_style` (the matrix row's enable-swatch style) is deleted
// with the matrix (P3, D2).

/// The send-name button — looks like a label, hovers like an editable field.
fn label_button_style() -> UIStyle {
    UIStyle {
        bg_color: Color32::new(0, 0, 0, 0),
        hover_bg_color: Color32::new(44, 44, 50, 255),
        pressed_bg_color: Color32::new(30, 30, 34, 255),
        text_color: Color32::new(214, 214, 220, 255),
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        corner_radius: color::SMALL_RADIUS,
        ..UIStyle::default()
    }
}

/// A dropdown trigger — the canonical neutral dropdown chip
/// (`components::dropdown_chip_style` on the grey ramp), the same control the
/// layer-header routing chips and the inspector pickers use. The renderer paints
/// the caret from the `dropdown_caret` flag, so call sites pass the bare value.
fn dropdown_trigger_style() -> UIStyle {
    crate::chrome::components::dropdown_trigger_style(BTN_FONT)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The docked column rect for a 1280×720 screen with the default inspector
    /// (500) + dock (460): x = 1280 - 500 - 460 = 320, full content height
    /// 720 - 36 transport - 36 footer = 648, from y = 36. The panel builds its
    /// rows into this via `build_docked` (D1) — the old overlay `build(w,h)`
    /// path is gone.
    fn test_dock_rect() -> Rect {
        Rect::new(320.0, 36.0, 460.0, 648.0)
    }

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
                    floor_db: crate::types::FLOOR_DB_OFF,
                    driven_count: 0,
                    routings: vec!["Capture: Channel 1".into()],
                    has_clip_triggers: false,
                    feeding_layers: Vec::new(),
                    consumers: Vec::new(),
                },
                AudioSendRow {
                    id: AudioSendId::new("s2"),
                    label: "Audio 2".into(),
                    channels: vec![2],
                    channel_label: "MacBook Mic".into(),
                    gain_db: 0.0,
                    floor_db: crate::types::FLOOR_DB_OFF,
                    driven_count: 0,
                    routings: vec!["Capture: Channel 1".into()],
                    has_clip_triggers: false,
                    feeding_layers: Vec::new(),
                    consumers: Vec::new(),
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
        p.build_docked(&mut tree, test_dock_rect());

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

        // Close (×) routes through handle_event and emits the toggle action
        // (the app closes the dock — one path with Escape / header button).
        let (consumed, acts) = p.handle_event(&UIEvent::Click {
            node_id: p.close_id,
            pos: Vec2::ZERO,
            modifiers: Modifiers::default(),
        });
        assert!(consumed);
        assert!(matches!(acts.as_slice(), [PanelAction::OpenAudioSetup]));
    }

    // `trigger_row_clicks_resolve_to_actions` (the matrix row-click test) is
    // deleted with the Audio Setup Triggers matrix (P3, D2).

    #[test]
    fn swatch_click_selects_send_and_scope_rect_present() {
        let mut p = panel_with_two_sends();
        let mut tree = UITree::new();
        p.build_docked(&mut tree, test_dock_rect());

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

    /// a drag starting inside the dock that grabs nothing (missed
    /// a divider) is still CLAIMED by ownership (`claims_drag`), which
    /// `UIRoot::resolve_drag_owner` reads so the gesture doesn't leak to the
    /// timeline. A `PointerDown` on an owned node is consumed; a `DragBegin`
    /// that armed nothing is not (ownership handles it one level up).
    #[test]
    fn missed_grab_origin_inside_panel_is_claimed_by_ownership() {
        let mut p = panel_with_two_sends();
        let mut tree = UITree::new();
        p.build_docked(&mut tree, test_dock_rect());

        // Top-left chrome corner: inside the panel, far from any divider/label.
        let inside = Vec2::new(p.panel_rect.x + 6.0, p.panel_rect.y + 6.0);
        let modifiers = Modifiers::default();

        let (consumed, _) = p.handle_event(&UIEvent::PointerDown {
            node_id: p.bg_id,
            pos: inside,
            modifiers,
        });
        assert!(consumed, "a press on an owned node is swallowed");
        assert!(
            p.claims_drag(inside),
            "a drag originating inside the panel is claimed even though it grabs nothing"
        );
        let (drag_consumed, _) = p.handle_event(&UIEvent::DragBegin {
            node_id: Some(p.bg_id),
            pos: inside,
            origin: inside,
            modifiers,
        });
        assert!(!drag_consumed, "handle_event needn't consume the missed-grab case — ownership does");
    }

    /// a timeline drag whose origin is outside the dock is
    /// never claimed, even with nothing armed.
    #[test]
    fn claims_drag_false_for_origin_outside_panel_with_nothing_armed() {
        let mut p = panel_with_two_sends();
        let mut tree = UITree::new();
        p.build_docked(&mut tree, test_dock_rect());

        let outside = Vec2::new(p.panel_rect.x - 40.0, p.panel_rect.y - 40.0);
        let modifiers = Modifiers::default();

        assert!(!p.claims_drag(outside));
        let (consumed, _) = p.handle_event(&UIEvent::DragBegin {
            node_id: Some(p.bg_id),
            pos: outside,
            origin: outside,
            modifiers,
        });
        assert!(!consumed);
    }

    /// `gesture_ended` is the idempotent end-of-gesture clear
    /// `UIRoot::broadcast_gesture_end` calls on every OPEN overlay regardless
    /// of ownership — it replaces the per-arm drag-swallow resets,
    /// and must be safe to call when
    /// nothing is armed too. (`dragging_band`/`calibration_drag` could
    /// previously both be armed at once — a bug class, never a feature, per
    /// D12 — `DragController<AudioSetupDrag>` makes that unrepresentable;
    /// this test now covers each drag kind separately, plus the "a fresh
    /// grab always wins" replacement semantics.)
    #[test]
    fn gesture_ended_clears_armed_band_drag() {
        let mut p = panel_with_two_sends();
        p.drag.start(AudioSetupDrag::Band(BandDivider::Low), Vec2::ZERO);
        assert!(p.is_dragging_band());

        p.gesture_ended();
        assert!(!p.drag.is_active());

        // Idempotent — a broadcast reaching every open overlay must not panic
        // when this one didn't own the gesture.
        p.gesture_ended();
        assert!(!p.drag.is_active());
    }

    #[test]
    fn gesture_ended_clears_armed_calibration_drag() {
        let mut p = panel_with_two_sends();
        p.drag.start(
            AudioSetupDrag::Calibration(CalibrationDrag::Gain {
                send: AudioSendId::new("s1"),
                start_x: 0.0,
                start_db: 0.0,
                fine: false,
            }),
            Vec2::ZERO,
        );
        assert!(p.drag.is_active());
        assert!(!p.is_dragging_band(), "a calibration drag must not read as a band drag");

        p.gesture_ended();
        assert!(!p.drag.is_active());
    }

    #[test]
    fn a_fresh_grab_replaces_an_armed_drag() {
        let mut p = panel_with_two_sends();
        p.drag.start(AudioSetupDrag::Band(BandDivider::Low), Vec2::ZERO);
        assert!(p.is_dragging_band());

        // D8: arming a second gesture always wins — the type makes the old
        // "both armed" state unrepresentable.
        p.drag.start(
            AudioSetupDrag::Calibration(CalibrationDrag::Gain {
                send: AudioSendId::new("s1"),
                start_x: 0.0,
                start_db: 0.0,
                fine: false,
            }),
            Vec2::ZERO,
        );
        assert!(!p.is_dragging_band(), "the fresh calibration grab must replace the band drag");
    }

    #[test]
    fn gain_buttons_emit_signed_steps() {
        let mut p = panel_with_two_sends();
        let mut tree = UITree::new();
        p.build_docked(&mut tree, test_dock_rect());

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
        p.build_docked(&mut tree, test_dock_rect());

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
        p.build_docked(&mut tree, test_dock_rect());

        assert!(p.handle_click(p.send_ids[0].delete).is_none()); // arm
        // Clicking the channel dropdown clears the arm instead of deleting.
        assert!(matches!(
            p.handle_click(p.send_ids[0].ch_dropdown),
            Some(PanelAction::AudioSendChannelClicked(_))
        ));
        assert!(p.active_notice().is_none());
    }

    // ─── Docked panel + calibration drags ───

    #[test]
    fn unowned_click_is_not_consumed() {
        // A click outside the dock's owned nodes and the scope is not consumed
        // — the caller routes it to lower panels. The dock never self-closes on
        // an outside click (close is the × / Escape / header toggle only).
        let mut p = panel_with_two_sends();
        let mut tree = UITree::new();
        p.build_docked(&mut tree, test_dock_rect());
        assert!(p.is_open());

        let (consumed, acts) = p.handle_event(&UIEvent::Click {
            node_id: NodeId::PLACEHOLDER,
            pos: Vec2::new(1.0, 1.0),
            modifiers: Modifiers::default(),
        });
        assert!(!consumed, "outside click must not be consumed");
        assert!(acts.is_empty());
        assert!(p.is_open(), "panel must not self-close on outside click");
    }

    /// BUG-070's remainder (P2: now sourced from `Stepper::intent_for`
    /// instead of a bare id match) — a right-click on any of the three
    /// stepper zones (minus / value / plus) replays the drag trio at 0.0 dB,
    /// undoable as one drag to unity, same as every other `slider_reset`
    /// site. No pre-existing test pinned this path directly before P2;
    /// added here as part of converting it onto the contract.
    #[test]
    fn right_click_any_stepper_zone_resets_gain_to_unity() {
        let mut p = panel_with_two_sends();
        let mut tree = UITree::new();
        p.build_docked(&mut tree, test_dock_rect());
        for id in [p.send_ids[0].gain_minus, p.send_ids[0].gain_value, p.send_ids[0].gain_plus] {
            let (consumed, actions) = p.handle_event(&UIEvent::RightClick {
                node_id: Some(id),
                pos: Vec2::new(100.0, 50.0),
                modifiers: Modifiers::default(),
            });
            assert!(consumed, "right-click on a stepper zone must be consumed");
            match actions.as_slice() {
                [PanelAction::SliderReset { changed, .. }] => match changed.as_ref() {
                    PanelAction::AudioSendGainDragChanged(sid, db) => {
                        assert_eq!(sid.as_str(), "s1");
                        assert!((db - 0.0).abs() < 1e-6, "reset must target unity (0 dB), got {db}");
                    }
                    other => panic!("expected AudioSendGainDragChanged, got {other:?}"),
                },
                other => panic!("expected a SliderReset trio, got {other:?}"),
            }
        }
    }

    #[test]
    fn gain_drag_begin_changed_commit_sequence() {
        let mut p = panel_with_two_sends();
        let mut tree = UITree::new();
        p.build_docked(&mut tree, test_dock_rect());
        let gain_value = p.send_ids[0].gain_value;
        let modifiers = Modifiers::default();

        let (consumed, actions) = p.handle_event(&UIEvent::PointerDown {
            node_id: gain_value,
            pos: Vec2::new(100.0, 50.0),
            modifiers,
        });
        assert!(consumed);
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::AudioSendGainDragBegin(id)] if id.as_str() == "s1"
        ));

        // 20 px right at 0.1 dB/px, starting from send 1's 0 dB.
        let (_, actions) = p.handle_event(&UIEvent::Drag {
            node_id: Some(gain_value),
            pos: Vec2::new(120.0, 50.0),
            delta: Vec2::new(20.0, 0.0),
        });
        match actions.as_slice() {
            [PanelAction::AudioSendGainDragChanged(id, db)] => {
                assert_eq!(id.as_str(), "s1");
                assert!((db - 2.0).abs() < 1e-4, "expected +2.0 dB, got {db}");
            }
            other => panic!("expected AudioSendGainDragChanged, got {other:?}"),
        }

        let (_, actions) = p.handle_event(&UIEvent::PointerUp {
            node_id: Some(gain_value),
            pos: Vec2::new(120.0, 50.0),
        });
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::AudioSendGainDragCommit(id)] if id.as_str() == "s1"
        ));
        assert!(!p.is_dragging_band(), "gain drag must not arm the crossover drag flag");
    }

    // `sensitivity_drag_begin_changed_commit_sequence` (the matrix's D7
    // sensitivity-drag test) is deleted with the matrix (P3, D2).

    #[test]
    fn tall_source_body_scrolls_and_scope_present() {
        // D8/BUG-047: with many input + consumer rows the body content exceeds
        // the dock viewport, so the ScrollContainer reports it can scroll (the
        // sections clip via GPU scissor instead of overflowing past the bottom).
        let mut p = AudioSetupPanel::new();
        p.toggle(); // open
        let consumers: Vec<SendConsumerRow> = (0..12)
            .map(|i| SendConsumerRow {
                label: format!("LAYER {i} \u{2022} Effect \u{2022} Param"),
                layer_id: Some(LayerId::new(format!("layer{i}"))),
            })
            .collect();
        let feeding: Vec<(LayerId, String)> = (0..6)
            .map(|i| (LayerId::new(format!("src{i}")), format!("SRC {i}")))
            .collect();
        p.configure(
            None,
            vec![AudioSendRow {
                id: AudioSendId::new("s1"),
                label: "Kick".into(),
                channels: vec![0],
                channel_label: "Channel 1".into(),
                gain_db: 0.0,
                floor_db: crate::types::FLOOR_DB_OFF,
                driven_count: 0,
                routings: vec!["Capture: Channel 1".into()],
                has_clip_triggers: false,
                feeding_layers: feeding,
                consumers,
            }],
            None,
        );
        let mut tree = UITree::new();
        p.build_docked(&mut tree, test_dock_rect());

        assert_eq!(p.consumer_row_ids.len(), 12);
        assert!(
            p.scroll.can_scroll(),
            "a tall source's body must overflow the dock height and be scrollable (BUG-047)"
        );
        assert!(p.scope_rect().is_some(), "scope present while open");
    }

    #[test]
    fn scope_rect_follows_body_scroll_offset() {
        // BUG-101: the spectrogram blit rect is plain geometry captured at
        // build time, not a tree node — a scroll that shifts the tree's
        // content nodes must shift `scope_rect` by the same amount, or the
        // waterfall detaches from its section header once scrolled.
        let mut p = AudioSetupPanel::new();
        p.toggle(); // open
        let consumers: Vec<SendConsumerRow> = (0..12)
            .map(|i| SendConsumerRow {
                label: format!("LAYER {i} \u{2022} Effect \u{2022} Param"),
                layer_id: Some(LayerId::new(&format!("layer{i}"))),
            })
            .collect();
        let feeding: Vec<(LayerId, String)> = (0..6)
            .map(|i| (LayerId::new(&format!("src{i}")), format!("SRC {i}")))
            .collect();
        p.configure(
            None,
            vec![AudioSendRow {
                id: AudioSendId::new("s1"),
                label: "Kick".into(),
                channels: vec![0],
                channel_label: "Channel 1".into(),
                gain_db: 0.0,
                floor_db: crate::types::FLOOR_DB_OFF,
                driven_count: 0,
                routings: vec!["Capture: Channel 1".into()],
                has_clip_triggers: false,
                feeding_layers: feeding,
                consumers,
            }],
            None,
        );
        let mut tree = UITree::new();
        p.build_docked(&mut tree, test_dock_rect());
        assert!(p.scroll.can_scroll());
        let unscrolled_y = p.scope_rect().expect("scope present while open").y;

        let moved = p.handle_scroll(-40.0);
        assert!(moved, "scroll delta within scrollable range must move the offset");
        p.build_docked(&mut tree, test_dock_rect());
        let offset = p.scroll.scroll_offset();
        assert!(offset > 0.0, "expected a nonzero scroll offset after handle_scroll");
        let scrolled_y = p.scope_rect().expect("scope still present after scroll").y;
        assert_eq!(
            scrolled_y,
            unscrolled_y - offset,
            "scope_rect must shift by the same offset applied to the tree content"
        );
    }

    // ─── Inputs / Consumers sections (AUDIO_SENDS_UX_DESIGN Phase 2) ───

    #[test]
    fn inputs_section_is_read_only_routing_display() {
        // no buttons in the Inputs section anymore — the
        // routing lines (device + feeding layers) are plain read-only text,
        // straight from `AudioSendRow::routings` (`state_sync`'s single
        // source now — no separate `feeding_layers` walk). Exercises the
        // missing-layer line too (D8 repair copy, now pointing at the layer
        // header instead of the deleted "+ Layer" row) without panicking.
        let mut p = panel_with_two_sends();
        p.sends[0].routings = vec![
            "Capture \u{2022} Channel 1".to_string(),
            "Layer \u{2022} KICK".to_string(),
            format!("Layer \u{2022} {MISSING_LAYER_LABEL}"),
        ];
        let mut tree = UITree::new();
        p.build_docked(&mut tree, test_dock_rect());
        assert!(p.owns_node(p.bg_id)); // sanity: panel still builds/owns nodes
    }

    #[test]
    fn consumer_row_click_selects_owning_layer_and_does_not_edit() {
        let mut p = panel_with_two_sends();
        p.sends[0].consumers = vec![SendConsumerRow {
            label: "BLOOM LAYER \u{2022} Bloom \u{2022} Intensity".to_string(),
            layer_id: Some(LayerId::new("bloom-layer")),
        }];
        let mut tree = UITree::new();
        p.build_docked(&mut tree, test_dock_rect());
        assert_eq!(p.consumer_row_ids.len(), 1);

        match p.handle_click(p.consumer_row_ids[0]) {
            Some(PanelAction::LayerClicked(layer, modifiers)) => {
                assert_eq!(layer.as_str(), "bloom-layer");
                assert!(!modifiers.shift && !modifiers.ctrl && !modifiers.command);
            }
            other => panic!("expected LayerClicked (navigate, not edit), got {other:?}"),
        }
    }


    // ─── Stable WidgetId across rebuild (the double-click bug) ───
    //
    // The panel consumes `PointerDown` on its own owned nodes to block the
    // BUG-059 leak, and consuming an event marks the overlay dirty, so
    // `ui_root.rs` rebuilds it (clear + rebuild) on the next frame. A plain
    // back-to-back rebuild of UNCHANGED panel data is NOT enough to
    // reproduce the churn — sibling-index auto-salts are deterministic
    // given an identical build order, so an identical double-build is
    // stable even for an unkeyed button. The real trigger is a PRECEDING
    // sibling count changing between builds: e.g. the delete-confirm
    // notice line (`active_notice()`) appearing after a delete-arm click
    // inserts one label BEFORE every send row, the add-send button, the
    // floor stepper, and everything below — shifting every later unkeyed
    // control's auto-salt out from under it (exactly the scenario
    // `add_node_keyed`'s doc comment names: "a row control when an
    // earlier row grows a drawer"). These tests force that shift and
    // assert the keyed controls' `WidgetId`s survive it unchanged.
    //
    // Confirmed empirically before writing the fix's final version: with
    // the floor-minus and send-delete call sites temporarily reverted to
    // unkeyed `add_button`, both tests below FAIL (the widget ids differ
    // pre/post the notice-line insertion); with the keys in place (the
    // code as it ships), both PASS.

    #[test]
    fn floor_button_widget_id_survives_notice_line_insertion() {
        let mut p = panel_with_two_sends();
        // driven_count > 0 so the first delete click ARMS (inserts the
        // notice line on the next build) instead of deleting immediately.
        p.sends[0].driven_count = 3;
        let mut tree = UITree::new();
        p.build_docked(&mut tree, test_dock_rect());
        assert!(p.active_notice().is_none(), "no notice line yet — nothing armed");
        let floor_minus_1 = p.floor_minus_id.expect("floor stepper built with sends present");
        let w1 = tree.widget_of(floor_minus_1);
        assert_ne!(w1, WidgetId::NONE);

        // Arm the delete confirm (state-only — no rebuild yet), then
        // rebuild exactly as `ui_root.rs` does when `overlay_dirty` is set
        // after the PointerDown that triggered this exact click was
        // consumed. This is the layout-shifting rebuild.
        assert!(p.handle_click(p.send_ids[0].delete).is_none(), "first click arms, no action yet");
        tree.clear();
        p.build_docked(&mut tree, test_dock_rect());
        assert!(p.active_notice().is_some(), "delete-confirm notice line now present");
        let floor_minus_2 = p.floor_minus_id.expect("floor stepper rebuilt");
        let w2 = tree.widget_of(floor_minus_2);

        assert_ne!(
            floor_minus_1, floor_minus_2,
            "NodeId gets a fresh generation on rebuild (expected)"
        );
        assert_eq!(
            w1, w2,
            "floor [-] button's WidgetId must survive a preceding sibling (the notice \
             line) appearing — this is the double-click bug's churn-is-gone proof"
        );
    }

    #[test]
    fn send_row_control_widget_ids_survive_notice_line_insertion_and_stay_distinct() {
        let mut p = panel_with_two_sends();
        p.sends[0].driven_count = 3;
        let mut tree = UITree::new();
        p.build_docked(&mut tree, test_dock_rect());

        let row0_delete_1 = tree.widget_of(p.send_ids[0].delete);
        let row1_delete_1 = tree.widget_of(p.send_ids[1].delete);
        assert_ne!(row0_delete_1, WidgetId::NONE);
        assert_ne!(row1_delete_1, WidgetId::NONE);
        assert_ne!(
            row0_delete_1, row1_delete_1,
            "row 0 and row 1's delete buttons must get DISTINCT keys — no collision"
        );

        // Arm row 0's delete (state-only), then rebuild — the notice line
        // now precedes BOTH send rows, shifting every later auto-salted
        // sibling if unkeyed.
        assert!(p.handle_click(p.send_ids[0].delete).is_none());
        tree.clear();
        p.build_docked(&mut tree, test_dock_rect());
        assert!(p.active_notice().is_some());

        let row0_delete_2 = tree.widget_of(p.send_ids[0].delete);
        let row1_delete_2 = tree.widget_of(p.send_ids[1].delete);
        assert_eq!(
            row0_delete_1, row0_delete_2,
            "row 0 delete button's WidgetId is stable across the notice-line insertion"
        );
        assert_eq!(
            row1_delete_1, row1_delete_2,
            "row 1 delete button's WidgetId is stable across the notice-line insertion"
        );
        assert_ne!(
            row0_delete_2, row1_delete_2,
            "still distinct after rebuild — stability didn't collapse the rows together"
        );
    }
}
