//! Declarative drawer API for parameter-slider sub-panels.
//!
//! A "drawer" is the small downward sub-panel that opens under an effect-card
//! slider — the driver (LFO) config, the envelope decay slider, the Ableton
//! invert toggle, and (incoming) the audio-modulation controls. Historically
//! each was hand-built: bespoke id-allocation, layout math, hit-testing, and
//! draw, duplicated four ways across `param_slider_shared.rs` and
//! `param_card.rs`. This module is the single abstraction they share.
//!
//! A drawer is described declaratively as a stack of [`DrawerRow`]s; [`build`]
//! turns the spec into `UITree` nodes (the coupled part) and returns a
//! [`DrawerIds`] whose [`DrawerIds::resolve_button`] maps a clicked node id back
//! to a flat control index. The caller owns the meaning of that index (it maps
//! it onto a driver / envelope / audio-mod edit), so the layout + hit-test logic
//! lives here once.
//!
//! See `docs/AUDIO_MODULATION_DESIGN.md` §10.2.

use std::cell::Cell;

use super::PanelAction;
use crate::chrome::Theme;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderNodeIds};
use crate::tree::UITree;

/// Layout constants — match the existing driver/envelope drawers so a migrated
/// drawer renders identically.
const PAD_H: f32 = 5.0;
const ROW_H: f32 = 22.0;
const ROW_GAP: f32 = 4.0;
/// Top (and bottom) pad inside the drawer container. A caller sizing a custom
/// row (e.g. the Ableton status strip) subtracts `TOP_PAD * 2` from its target
/// container height to get the row height.
pub(crate) const TOP_PAD: f32 = 4.0;
const BTN_GAP: f32 = 1.0;
/// Width reserved for a [`DrawerRow::Buttons`] leading label. Matches the audio
/// shaping sliders' label width so feature/band rows line up with them.
const ROW_LABEL_W: f32 = 52.0;
/// Height of a row label's text box.
const ROW_LABEL_H: f32 = 14.0;
/// Horizontal inset for the leading/trailing elements of a [`DrawerRow::Status`]
/// strip — matches the Ableton drawer's original `pad`.
const STATUS_PAD: f32 = 6.0;
/// Label glyph box height inside a status strip — matches the Ableton drawer.
const STATUS_LABEL_H: f32 = 12.0;

/// D6 fire meter — height reserved below an Amount slider whose
/// `show_meter` is set: a thin track+fill+threshold-tick underline,
/// mirroring the deleted `TriggerRowIds`/`update_trigger_levels` meter,
/// generalized from a fixed per-send row
/// to every audio-mod drawer's Amount row (2026-07-11: every drawer, not just
/// fire-mode ones — see `show_amount_meter`). `pub(crate)` so
/// `param_slider_shared::audio_config_height` — a caller reserving height for
/// a drawer it isn't itself building — can add this term without duplicating
/// the literal and drifting from what [`DrawerRow::height`] actually builds.
pub(crate) const METER_STRIP_H: f32 = 6.0;
/// Bar thickness within the reserved strip.
const METER_BAR_H: f32 = 3.0;
/// Track/idle fill colors — a dim, always-visible well so the meter reads
/// even at level 0.
const METER_TRACK_COLOR: Color32 = Color32::new(40, 40, 46, 255);
/// The fixed 0.5 fire-threshold tick — bright and neutral so it reads over
/// any drawer accent color.
const METER_TICK_COLOR: Color32 = Color32::new(225, 225, 235, 255);

/// How a [`DrawerRow::Buttons`] row distributes width across its buttons.
pub enum ButtonWidth {
    /// Widths proportional to label length (`chars + 1`), so mixed-width labels
    /// ("1/32" vs "1") don't cramp — the driver beat-division row's rule, also
    /// used by the audio send/feature rows.
    Proportional,
    /// Every button the same width — the driver waveform row's rule (icon glyphs
    /// of equal visual weight).
    Uniform,
}

/// One labeled button in a [`DrawerRow::Buttons`] row.
pub struct DrawerButton {
    pub label: String,
    pub active: bool,
    /// Optional identity tint (e.g. an audio send's color). When set, the active
    /// button fills with this color and an inactive one tints its text.
    pub accent: Option<Color32>,
    /// When true the accent is used as text color only (never a full fill), and
    /// the active state shows the normal selection highlight. Keeps an identity
    /// tint legible without a button-sized block of saturated color.
    pub accent_text_only: bool,
}

impl DrawerButton {
    pub fn new(label: impl Into<String>, active: bool) -> Self {
        Self { label: label.into(), active, accent: None, accent_text_only: false }
    }

    /// Give the button an identity tint. See [`DrawerButton::accent`].
    pub fn with_accent(mut self, accent: Color32) -> Self {
        self.accent = Some(accent);
        self
    }

    /// Tint only the text with the identity color; the active state uses the
    /// standard selection highlight instead of a full accent fill.
    pub fn with_accent_text_only(mut self, accent: Color32) -> Self {
        self.accent = Some(accent);
        self.accent_text_only = true;
        self
    }
}

/// A leading status indicator dot for a [`DrawerRow::Status`] strip.
pub struct StatusDot {
    pub size: f32,
    pub color: Color32,
}

/// A right-aligned fixed-size action button for a [`DrawerRow::Status`] strip.
/// Addressable: it joins the drawer's flat button index list.
pub struct TrailingButton {
    pub label: String,
    pub width: f32,
    pub height: f32,
    pub style: UIStyle,
}

/// A single-row status strip: an optional leading [`StatusDot`], a left-aligned
/// dimmed label that expands to fill, and an optional right-aligned
/// [`TrailingButton`]. This is the shape of the Ableton mapping drawer (status +
/// audit label + invert toggle) — distinct from the controls-grid rows, so it
/// carries its own `height` and centers its elements vertically within the row.
pub struct StatusStrip {
    pub height: f32,
    pub dot: Option<StatusDot>,
    pub label: String,
    pub label_color: Color32,
    pub label_font: u16,
    pub trailing: Option<TrailingButton>,
}

/// One row of a drawer.
pub enum DrawerRow {
    /// A horizontal group of buttons. Each button is one addressable control;
    /// `width` selects proportional or uniform distribution. `label`, when set,
    /// reserves a leading label column (e.g. "Feature") that lines up with the
    /// slider rows' labels.
    Buttons {
        buttons: Vec<DrawerButton>,
        width: ButtonWidth,
        label: Option<String>,
    },
    /// A full-width value slider. Not click-addressed (the panel's existing
    /// slider-drag path handles it via the returned [`SliderNodeIds`]). Its colours
    /// resolve from the drawer's [`DrawerSpec::theme`] (fill = the source accent),
    /// so a drawer slider always belongs to its source without a per-row colour arg.
    Slider {
        label: String,
        /// Normalized fill 0..1.
        norm: f32,
        /// Normalized position 0..1 the slider resets to on right-click
        /// (BUG-061) — the source's own default (e.g. `AudioModShape`'s
        /// `sensitivity`/`attack_ms`/`release_ms` default), not the current
        /// live value.
        default_norm: f32,
        /// Display text shown in the slider's value field.
        value_text: String,
        /// Width reserved for the leading label.
        label_w: f32,
        /// The right-click reset action to fire on this slider's track
        /// (BUG-070 follow-through) — required so a drawer slider can never
        /// be built without declaring how it resets. The caller (param_card /
        /// param_slider_shared) supplies it; [`DrawerIds::slider_resets`]
        /// carries it back out in the same order as [`DrawerIds::sliders`].
        reset: PanelAction,
        /// D6 fire meter: this Amount slider tunes a fire-mode config against
        /// the fixed 0.5 edge — reserve a live shaped-signal meter strip
        /// beneath it (track + fill + threshold tick, [`METER_STRIP_H`]
        /// tall). `false` for every non-Amount slider row (Attack/Release/
        /// Step) and every non-fire-mode drawer; the caller
        /// (`build_audio_mod_drawer`) decides. The fill's live level is NEVER
        /// carried through this field — that would go stale between
        /// `configure()` calls — it's pushed in place every UI tick by
        /// [`MeterIds::update`], addressed via [`DrawerIds::meters`].
        show_meter: bool,
    },
    /// A status strip (see [`StatusStrip`]).
    Status(StatusStrip),
}

impl DrawerRow {
    /// The intrinsic height this row occupies. Buttons/Slider rows use the
    /// shared [`ROW_H`] (plus [`METER_STRIP_H`] when a Slider's `show_meter`
    /// is set); a status strip carries its own.
    fn height(&self) -> f32 {
        match self {
            DrawerRow::Status(s) => s.height,
            DrawerRow::Slider { show_meter: true, .. } => ROW_H + METER_STRIP_H,
            _ => ROW_H,
        }
    }
}

/// A drawer described as a stack of rows.
pub struct DrawerSpec {
    pub rows: Vec<DrawerRow>,
    /// Font size for buttons.
    pub btn_font_size: u16,
    /// Font size for slider labels/values.
    pub slider_font_size: u16,
    /// The drawer's styling context — its source identity ([`Theme::accent`]), the
    /// tinted-dark surface it paints, and the text colour. Every control inside
    /// (container fill, accent spine, option cells, sliders, labels) resolves its
    /// colour from this one value, so a drawer takes on its source's colour
    /// (orange Trigger / magenta LFO / green Audio / purple Ableton) in one place
    /// instead of threading a palette. Built by the caller as
    /// `Theme::INSPECTOR.with_accent(SOURCE).tinted()`.
    pub theme: Theme,
}

impl DrawerSpec {
    /// Total height this spec occupies (container height), given its rows.
    pub fn height(&self) -> f32 {
        let n = self.rows.len();
        if n == 0 {
            return 0.0;
        }
        let rows_h: f32 = self.rows.iter().map(DrawerRow::height).sum();
        TOP_PAD * 2.0 + rows_h + ROW_GAP * (n as f32 - 1.0)
    }
}

/// Container height for a drawer of `n` uniform button/slider rows (no status
/// strips). Lets a caller reserve vertical space for a drawer it isn't itself
/// building — keeps card-height math in sync with [`DrawerSpec::height`]
/// without constructing a throwaway spec.
pub(crate) fn uniform_rows_height(n: usize) -> f32 {
    if n == 0 {
        return 0.0;
    }
    TOP_PAD * 2.0 + ROW_H * n as f32 + ROW_GAP * (n as f32 - 1.0)
}

/// the content-thread signal this meter displays decays
/// in milliseconds (a transient's shaped envelope), but `ContentState`
/// snapshots only reach the UI at UI-tick cadence — an instantaneous fill
/// between two snapshots is invisible. `PEAK_HOLD_SECONDS` is the minimum
/// time a peak stays fully visible before it's allowed to fall; the "fire"
/// bright accent latches for the same window, so a 5 ms spike crossing 0.5
/// still reads as a fire.
const PEAK_HOLD_SECONDS: f32 = 0.25;
/// Fall rate once the hold expires, in meter-units (0..1) per second. Not
/// specified numerically by the design brief — chosen for a fast-but-visible
/// fall (full-scale 1.0 → 0.0 in ~200 ms), so consecutive kicks read as
/// distinct pulses rather than one smeared bar.
const PEAK_DECAY_PER_SEC: f32 = 5.0;

/// D6 fire meter node ids for one `Slider` row whose `show_meter` was set —
/// updated in place every UI tick from the live `FireMeterCapture` snapshot,
/// never rebuilt. Mirrors the deleted `TriggerRowIds`/`update_trigger_levels`
/// underline meter (470228ec).
#[derive(Clone)]
pub struct MeterIds {
    /// The meter's track node — its live bounds (not a cached copy) are the
    /// single source of truth for the fill's geometry every tick, so a
    /// `ScrollContainer::offset_content` shift (which moves the track's
    /// absolute bounds directly, no rebuild) carries the fill along with it.
    track: NodeId,
    fill: NodeId,
    /// UI-side peak-hold state (BUG-109 P5) — instant attack to a new peak,
    /// held `PEAK_HOLD_SECONDS`, then decays toward the live level. `Cell`,
    /// not `&mut self`, so `update()` stays callable through the existing
    /// `&self` chain (`ParamCardPanel::update_fire_meters` and friends) —
    /// display-only smoothing; the content-thread capture stays the raw
    /// conditioned value the fire edge reads (forbidden move: content-side
    /// smoothing of the meter signal).
    held_level: Cell<f32>,
    hold_remaining: Cell<f32>,
}

impl MeterIds {
    /// Push the current shaped-signal level (0..1) onto this meter's fill —
    /// in place, no rebuild. Peak-holds: a rising level snaps the display up
    /// instantly and restarts the hold window; once the hold expires the
    /// display decays at `PEAK_DECAY_PER_SEC` toward the live `level`,
    /// clamped so it never falls below whatever the signal is doing right
    /// now. `accent` recolors the fill to the drawer's own identity color
    /// while the HELD level (not the instantaneous one) is at/above the
    /// fixed 0.5 threshold — the fire-flash cue latches for the hold too.
    pub fn update(&self, tree: &mut UITree, level: f32, accent: Color32, dt: f32) {
        let level = level.clamp(0.0, 1.0);
        let dt = dt.max(0.0);
        let mut held = self.held_level.get();
        let mut hold = self.hold_remaining.get();
        if level >= held {
            held = level;
            hold = PEAK_HOLD_SECONDS;
        } else if hold > 0.0 {
            hold = (hold - dt).max(0.0);
        } else {
            held = (held - PEAK_DECAY_PER_SEC * dt).max(level);
        }
        self.held_level.set(held);
        self.hold_remaining.set(hold);

        // Read the track's CURRENT bounds — not a build-time cache — so an
        // in-place scroll (`ScrollContainer::offset_content`, which shifts
        // the track's absolute bounds directly) is reflected here too.
        let track_rect = tree.get_bounds(self.track);
        tree.set_bounds(
            self.fill,
            Rect::new(track_rect.x, track_rect.y, track_rect.width * held, track_rect.height),
        );
        let firing = held >= 0.5;
        let fill_color = if firing { accent } else { dim(accent, 0.55) };
        tree.set_style(self.fill, UIStyle { bg_color: fill_color, ..UIStyle::default() });
    }
}

/// Scale an RGB color's channels toward black by `factor` (0..1), keeping
/// alpha — the idle/dim state for a [`MeterIds`] fill. No generic color-blend
/// helper exists in this crate yet; kept local rather than inventing one for
/// a single caller.
fn dim(c: Color32, factor: f32) -> Color32 {
    Color32::new(
        (c.r as f32 * factor) as u8,
        (c.g as f32 * factor) as u8,
        (c.b as f32 * factor) as u8,
        c.a,
    )
}

/// The `UITree` node ids a built drawer produced, plus the mapping needed to
/// resolve a click. Buttons are enumerated **flat across all rows in order**
/// (row 0's buttons first, then row 1's, …) — that flat index is what
/// [`Self::resolve_button`] returns and what the caller maps to an action.
pub struct DrawerIds {
    pub container: NodeId,
    /// Node id per flat button index.
    button_ids: Vec<NodeId>,
    /// Slider node ids, in row order (one per `Slider` row).
    pub sliders: Vec<SliderNodeIds>,
    /// Right-click reset action for each entry in [`Self::sliders`], same
    /// order/index — a parallel array (rather than folding into `sliders`)
    /// so existing `sliders[i].track`/`.track_span` access sites are
    /// untouched (BUG-070 follow-through).
    pub slider_resets: Vec<PanelAction>,
    /// D6 fire meter node ids, parallel to [`Self::sliders`] (same index) —
    /// `Some` only for the `Slider` row whose `show_meter` was `true`.
    pub meters: Vec<Option<MeterIds>>,
    /// Total height the drawer occupied.
    pub height: f32,
}

impl DrawerIds {
    /// Flat control index of the button with this node id, if any.
    pub fn resolve_button(&self, id: NodeId) -> Option<usize> {
        self.button_ids.iter().position(|&b| b == id)
    }

    /// Number of addressable buttons.
    pub fn button_count(&self) -> usize {
        self.button_ids.len()
    }

    /// The flat button node ids, row 0 first. Callers that built a fixed-shape
    /// spec (e.g. the driver drawer) use this to recover their typed ids by
    /// position.
    pub fn button_ids(&self) -> &[NodeId] {
        &self.button_ids
    }
}

/// Proportional widths for a row of buttons: weight each by `label.len() + 1`
/// so fraction labels get room and integer labels give it back. Returns the
/// per-button widths summing to `content_w`.
fn proportional_widths(labels: &[&str], content_w: f32) -> Vec<f32> {
    let weights: Vec<f32> = labels.iter().map(|l| l.chars().count() as f32 + 1.0).collect();
    let total: f32 = weights.iter().sum::<f32>().max(1.0);
    weights.iter().map(|w| content_w * w / total).collect()
}

/// Equal width for every button in a row — `content_w / n`.
fn uniform_widths(n: usize, content_w: f32) -> Vec<f32> {
    if n == 0 {
        return Vec::new();
    }
    vec![content_w / n as f32; n]
}

/// Build a drawer's `UITree` nodes under `parent` at `(x, y)` spanning width
/// `w`. Returns the created ids + the height consumed.
pub fn build(
    tree: &mut UITree,
    parent: Option<NodeId>,
    x: f32,
    y: f32,
    w: f32,
    spec: &DrawerSpec,
) -> DrawerIds {
    let height = spec.height();
    // Contents-only container: the source-tinted backing + accent spine are drawn
    // by the MOD CARD that wraps the whole param (slider + drawer) in
    // `build_param_row`, so the drawer itself is transparent — its rows render on
    // that one shared card, which is what makes the drawer read as belonging to its
    // slider. The theme still colours the rows (option fills, slider fills, labels).
    let container = tree.add_panel(parent, x, y, w, height, UIStyle::default());

    let mut button_ids: Vec<NodeId> = Vec::new();
    let mut sliders: Vec<SliderNodeIds> = Vec::new();
    let mut slider_resets: Vec<PanelAction> = Vec::new();
    let mut meters: Vec<Option<MeterIds>> = Vec::new();
    let avail_w = w - PAD_H * 2.0;
    let mut row_y = y + TOP_PAD;

    for row in &spec.rows {
        match row {
            DrawerRow::Buttons { buttons, width, label } => {
                // Optional leading label column, aligned with the slider labels.
                let label_w = if label.is_some() { ROW_LABEL_W } else { 0.0 };
                if let Some(text) = label {
                    tree.add_label(
                        Some(container),
                        x + PAD_H,
                        row_y + (ROW_H - ROW_LABEL_H) * 0.5,
                        label_w,
                        ROW_LABEL_H,
                        text,
                        spec.theme.label_style(spec.slider_font_size),
                    );
                }
                let labels: Vec<&str> = buttons.iter().map(|b| b.label.as_str()).collect();
                let content_w =
                    avail_w - label_w - BTN_GAP * (buttons.len().max(1) as f32 - 1.0);
                let widths = match width {
                    ButtonWidth::Proportional => proportional_widths(&labels, content_w),
                    ButtonWidth::Uniform => uniform_widths(buttons.len(), content_w),
                };
                let mut cx = x + PAD_H + label_w;
                for (b, bw) in buttons.iter().zip(widths.iter()) {
                    // Base option look comes from the theme: selected fills the
                    // source accent, idle recesses to a dark well. A per-button
                    // accent (an audio send's own identity colour) overlays on top —
                    // genuinely local identity, not the subtree accent.
                    let mut style = spec.theme.option_style(b.active, spec.btn_font_size);
                    if let Some(accent) = b.accent {
                        if b.accent_text_only {
                            // Identity as text color only; active keeps the
                            // standard selection highlight (no full fill).
                            style.text_color = accent;
                        } else if b.active {
                            style.bg_color = accent;
                            style.text_color = Color32::new(20, 20, 24, 255);
                        } else {
                            style.text_color = accent;
                        }
                    }
                    let id = tree.add_button(
                        Some(container), cx, row_y, *bw, ROW_H, style, &b.label,
                    );
                    button_ids.push(id);
                    cx += bw + BTN_GAP;
                }
            }
            DrawerRow::Slider {
                label,
                norm,
                default_norm,
                value_text,
                label_w,
                reset,
                show_meter,
            } => {
                let sx = x + PAD_H;
                let slider_w = w - PAD_H * 2.0;
                // Slider colours resolve from the theme: fill = the source accent,
                // so a drawer slider belongs to its source (orange Decay, green
                // audio shaping) without a per-row colour arg.
                let sc = spec.theme.slider_colors();
                let built = BitmapSlider::build(
                    tree,
                    Some(container),
                    Rect::new(sx, row_y, slider_w, ROW_H),
                    Some(label.as_str()),
                    norm.clamp(0.0, 1.0),
                    value_text.as_str(),
                    &sc,
                    spec.slider_font_size,
                    *label_w,
                    default_norm.clamp(0.0, 1.0),
                    reset.clone(),
                );
                sliders.push(built.ids);
                slider_resets.push(built.reset);

                if *show_meter {
                    // D6: track + fill span the slider's tuning zone (after
                    // the label column, matching the track's own left edge);
                    // fixed 0.5 threshold tick — never configurable, the
                    // same edge every fire-mode evaluator detects on.
                    let meter_y = row_y + ROW_H + 1.0;
                    let meter_x = sx + *label_w;
                    let meter_w = (slider_w - *label_w).max(4.0);
                    let track = tree.add_panel(
                        Some(container),
                        meter_x,
                        meter_y,
                        meter_w,
                        METER_BAR_H,
                        UIStyle { bg_color: METER_TRACK_COLOR, ..UIStyle::default() },
                    );
                    let fill = tree.add_panel(
                        Some(container),
                        meter_x,
                        meter_y,
                        0.0, // live level set per UI tick by MeterIds::update
                        METER_BAR_H,
                        UIStyle { bg_color: dim(sc.fill, 0.55), ..UIStyle::default() },
                    );
                    let tick_x = meter_x + 0.5 * meter_w;
                    tree.add_panel(
                        Some(container),
                        tick_x,
                        meter_y - 2.0,
                        1.5,
                        METER_BAR_H + 4.0,
                        UIStyle { bg_color: METER_TICK_COLOR, ..UIStyle::default() },
                    );
                    meters.push(Some(MeterIds {
                        track,
                        fill,
                        held_level: Cell::new(0.0),
                        hold_remaining: Cell::new(0.0),
                    }));
                } else {
                    meters.push(None);
                }
            }
            DrawerRow::Status(s) => {
                // Leading dot (centered in the row).
                let dot_w = if let Some(dot) = &s.dot {
                    let dot_y = row_y + (s.height - dot.size) * 0.5;
                    tree.add_panel(
                        Some(container),
                        x + STATUS_PAD,
                        dot_y,
                        dot.size,
                        dot.size,
                        UIStyle {
                            bg_color: dot.color,
                            corner_radius: dot.size * 0.5,
                            ..UIStyle::default()
                        },
                    );
                    dot.size
                } else {
                    0.0
                };

                // Trailing button (right-aligned, centered). Addressable.
                let mut trailing_x = x + w - STATUS_PAD;
                if let Some(tb) = &s.trailing {
                    let bx = x + w - STATUS_PAD - tb.width;
                    let by = row_y + (s.height - tb.height) * 0.5;
                    let id = tree.add_button(
                        Some(container),
                        bx,
                        by,
                        tb.width,
                        tb.height,
                        tb.style,
                        &tb.label,
                    );
                    button_ids.push(id);
                    trailing_x = bx;
                }

                // Label fills the gap between dot and trailing button.
                let label_x = x + STATUS_PAD + dot_w + 4.0;
                let label_y = row_y + (s.height - STATUS_LABEL_H) * 0.5;
                let label_w = (trailing_x - label_x - 4.0).max(0.0);
                tree.add_label(
                    Some(container),
                    label_x,
                    label_y,
                    label_w,
                    STATUS_LABEL_H,
                    &s.label,
                    UIStyle {
                        text_color: s.label_color,
                        font_size: s.label_font,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
                );
            }
        }
        row_y += row.height() + ROW_GAP;
    }

    DrawerIds {
        container,
        button_ids,
        sliders,
        slider_resets,
        meters,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buttons(labels: &[(&str, bool)]) -> DrawerRow {
        DrawerRow::Buttons {
            buttons: labels.iter().map(|(l, a)| DrawerButton::new(*l, *a)).collect(),
            width: ButtonWidth::Proportional,
            label: None,
        }
    }

    fn uniform_buttons(labels: &[(&str, bool)]) -> DrawerRow {
        DrawerRow::Buttons {
            buttons: labels.iter().map(|(l, a)| DrawerButton::new(*l, *a)).collect(),
            width: ButtonWidth::Uniform,
            label: None,
        }
    }

    #[test]
    fn flat_button_indexing_across_rows() {
        // Mirror the driver drawer: row1 = 11 beat divs, row2 = dot/triplet/5
        // waves/rev (8). Flat indices 0..10 then 11..18.
        let spec = DrawerSpec {
            rows: vec![
                buttons(&[
                    ("1/32", false), ("1/16", false), ("1/8", false), ("1/4", true),
                    ("1/2", false), ("1", false), ("2", false), ("4", false),
                    ("8", false), ("16", false), ("32", false),
                ]),
                buttons(&[
                    (".", false), ("T", false), ("Sin", true), ("Tri", false),
                    ("Saw", false), ("Sqr", false), ("Rnd", false), ("Rev", false),
                ]),
            ],
            btn_font_size: 10,
            slider_font_size: 11,
            theme: Theme::INSPECTOR,
        };

        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 400.0, 200.0, UIStyle::default());
        let ids = build(&mut tree, Some(root), 0.0, 0.0, 240.0, &spec);

        assert_eq!(ids.button_count(), 19, "11 + 8 buttons");

        // The first button of row 2 (".") is flat index 11.
        let dot_node = ids.button_ids[11];
        assert_eq!(ids.resolve_button(dot_node), Some(11));
        // Last button ("Rev") is flat index 18.
        let rev_node = ids.button_ids[18];
        assert_eq!(ids.resolve_button(rev_node), Some(18));
        // An unrelated id resolves to nothing.
        assert_eq!(ids.resolve_button(NodeId::PLACEHOLDER), None);
    }

    fn placeholder_reset() -> PanelAction {
        PanelAction::slider_reset(
            PanelAction::MasterOpacitySnapshot,
            PanelAction::MasterOpacityChanged(1.0),
            PanelAction::MasterOpacityCommit,
        )
    }

    #[test]
    fn slider_row_yields_a_slider_not_a_button() {
        let spec = DrawerSpec {
            rows: vec![DrawerRow::Slider {
                label: "Decay".into(),
                norm: 0.25,
                default_norm: 0.25,
                value_text: "2.00".into(),
                label_w: 50.0,
                reset: placeholder_reset(),
                show_meter: false,
            }],
            btn_font_size: 10,
            slider_font_size: 11,
            theme: Theme::INSPECTOR,
        };
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 400.0, 200.0, UIStyle::default());
        let ids = build(&mut tree, Some(root), 0.0, 0.0, 240.0, &spec);

        assert_eq!(ids.button_count(), 0);
        assert_eq!(ids.sliders.len(), 1);
    }

    #[test]
    fn meter_fill_tracks_track_after_in_place_scroll() {
        // Regression: `MeterIds` used to cache the meter's build-time x/y/w/h
        // and re-assert them from `update()` every tick. An in-place scroll
        // (`ScrollContainer::offset_content`, simulated below exactly as it
        // operates — shift every content node's absolute bounds by delta_y,
        // no rebuild) moves the track's real bounds, but the pre-fix `update`
        // kept writing the fill back to its stale pre-scroll position, so the
        // fill visibly detached from the track it's meant to sit inside.
        // `MeterIds` now stores the track's `NodeId` and reads its CURRENT
        // bounds each call — this pins the fill to whatever the track's live
        // bounds are, scrolled or not.
        let spec = DrawerSpec {
            rows: vec![DrawerRow::Slider {
                label: "Amount".into(),
                norm: 0.5,
                default_norm: 0.5,
                value_text: "0.50".into(),
                label_w: 50.0,
                reset: placeholder_reset(),
                show_meter: true,
            }],
            btn_font_size: 10,
            slider_font_size: 11,
            theme: Theme::INSPECTOR,
        };
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 400.0, 200.0, UIStyle::default());
        let ids = build(&mut tree, Some(root), 0.0, 0.0, 240.0, &spec);
        let meter = ids.meters[0].as_ref().expect("show_meter=true builds a MeterIds");

        // Simulate `ScrollContainer::offset_content`: shift every node's
        // absolute bounds by delta_y in place, exactly as the real cheap
        // scroll path does — no rebuild, no call back into any widget code.
        let delta_y = -37.0;
        for i in 0..tree.count() {
            let id = tree.id_at(i);
            let mut b = tree.get_bounds(id);
            b.y += delta_y;
            tree.set_bounds(id, b);
        }

        meter.update(&mut tree, 0.8, Color32::WHITE, 0.016);

        let track_rect = tree.get_bounds(meter.track);
        let fill_rect = tree.get_bounds(meter.fill);
        assert!(
            (fill_rect.y - track_rect.y).abs() < 0.001,
            "fill must sit at the track's CURRENT (post-scroll) y, not a stale \
             build-time y: track.y={} fill.y={}",
            track_rect.y,
            fill_rect.y,
        );
    }

    #[test]
    fn uniform_widths_are_equal_proportional_are_not() {
        // Uniform: every button the same width (driver waveform row).
        let w = uniform_widths(4, 200.0);
        assert_eq!(w.len(), 4);
        assert!(w.iter().all(|&x| (x - 50.0).abs() < 0.001));
        // Proportional: a longer label gets a wider button.
        let p = proportional_widths(&["Rev", "."], 100.0);
        assert!(p[0] > p[1]);
    }

    #[test]
    fn driver_row2_uniform_keeps_all_eight_equal() {
        // ".", "T", 5 waveform glyphs, "Rev" — proportional would make "Rev"
        // wider; uniform keeps them equal (the original driver row-2 rule).
        let wave: Vec<String> = crate::icons::WAVEFORMS.iter().map(|w| w.text()).collect();
        let mut labels: Vec<(&str, bool)> = vec![(".", false), ("T", false)];
        labels.extend(wave.iter().map(|s| (s.as_str(), false)));
        labels.push(("Rev", false));
        let spec = DrawerSpec {
            rows: vec![uniform_buttons(&labels)],
            btn_font_size: 10,
            slider_font_size: 11,
            theme: Theme::INSPECTOR,
        };
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 400.0, 200.0, UIStyle::default());
        let ids = build(&mut tree, Some(root), 0.0, 0.0, 240.0, &spec);
        assert_eq!(ids.button_count(), 8);
        let w0 = tree.get_node(ids.button_ids[0]).unwrap().bounds.width;
        let w7 = tree.get_node(ids.button_ids[7]).unwrap().bounds.width;
        assert!((w0 - w7).abs() < 0.001, "uniform row keeps equal widths");
    }

    #[test]
    fn status_strip_yields_one_addressable_button() {
        let spec = DrawerSpec {
            rows: vec![DrawerRow::Status(StatusStrip {
                height: 16.0,
                dot: Some(StatusDot { size: 6.0, color: Color32::WHITE }),
                label: "Macro 1  ·  Track > Device".into(),
                label_color: Color32::WHITE,
                label_font: 9,
                trailing: Some(TrailingButton {
                    label: "INV".into(),
                    width: 28.0,
                    height: 16.0,
                    style: UIStyle::default(),
                }),
            })],
            btn_font_size: 10,
            slider_font_size: 11,
            theme: Theme::INSPECTOR,
        };
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 400.0, 200.0, UIStyle::default());
        let ids = build(&mut tree, Some(root), 0.0, 0.0, 240.0, &spec);
        // The INV button is the only addressable control.
        assert_eq!(ids.button_count(), 1);
        // Container height = TOP_PAD*2 + strip height = 8 + 16 = 24 (matches ABL).
        assert!((ids.height - 24.0).abs() < 0.001);
    }

    #[test]
    fn height_accounts_for_rows_and_gaps() {
        let one = DrawerSpec {
            rows: vec![buttons(&[("a", false)])],
            btn_font_size: 10,
            slider_font_size: 11,
            theme: Theme::INSPECTOR,
        };
        let two = DrawerSpec {
            rows: vec![buttons(&[("a", false)]), buttons(&[("b", false)])],
            btn_font_size: 10,
            slider_font_size: 11,
            theme: Theme::INSPECTOR,
        };
        assert!(two.height() > one.height());
        // Empty spec is zero height.
        let empty =
            DrawerSpec { rows: vec![], btn_font_size: 10, slider_font_size: 11, theme: Theme::INSPECTOR };
        assert_eq!(empty.height(), 0.0);
    }
}
