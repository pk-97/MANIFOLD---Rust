//! `MappingPopover` — a small in-place editor for one card binding's
//! mapping (`min`, `max`, `invert`, `curve`).
//!
//! It is deliberately **surface-agnostic**: it owns nothing about the
//! graph canvas, the effect card, or Ableton. You hand it the binding's
//! current `{label, min, max, invert, curve}` plus an anchor rect, and it
//! draws a floating panel near that anchor and emits the same
//! `PanelAction` mapping edits the effect card's slider already routes
//! through `EditUserParamBindingCommand`. So the live card slider and the
//! rendered output update the moment you drag a handle or click a button —
//! no separate plumbing.
//!
//! Why it draws with [`Painter`](crate::draw::Painter) rect/text primitives
//! rather than the `param_slider_shared.rs` builders: those builders are
//! `UITree`-based (`tree.add_button`, parent ids), and the host surface here —
//! the graph canvas — renders entirely through `Painter` immediate-mode
//! primitives with no `UITree`. So the popover mirrors the *visual
//! conventions* of the shared widgets (a min/max trim track with two drag
//! bars, an `INV` toggle, a 4-option curve dropdown) while staying inside
//! the canvas's draw model. The min/max drag reuses the existing
//! snapshot/changed/commit `PanelAction` triad, so a range drag still
//! coalesces into one undo entry.
//!
//! Label AND section are live text fields (UI_WIDGET_UNIFICATION P5c, closes
//! BUG-102): both embed [`crate::text_edit::TextEditModel`] — the same
//! caret/selection primitive `manifold-app`'s `TextInputState` and the
//! canvas numeric type-in (P5d) share (I7, one editing home). Click places
//! the caret, click-drag selects a range within the field, typing replaces
//! the selection; Enter/blur commits, Esc cancels (D16). The popover has no
//! `UITree`, so caret + ranged-selection highlight are drawn as `Painter`
//! rects/text rather than through a retained text-field widget — the same
//! immediate-mode convention every other row here already uses.

use crate::{RootAction};
use crate::MacroCurve;
use crate::PanelAction;
use crate::apply_card_reshape;
use crate::draw::Painter;
use crate::node::Color32;
use crate::text_edit::{TextEditModel, byte_offset_for_x, x_for_byte_offset};

use super::Rect;

// ── Layout constants ────────────────────────────────────────────────
const POPOVER_W: f32 = 188.0;
const PAD: f32 = 8.0;
const ROW_H: f32 = 18.0;
const ROW_GAP: f32 = 6.0;
const HEADER_H: f32 = 16.0;
const CURVE_ROW_H: f32 = 16.0;
const FONT: f32 = 11.0;
const FONT_SMALL: f32 = 10.0;
/// Height of the live response-curve preview plotted under the header.
const PREVIEW_H: f32 = 52.0;
/// How many points to sample the reshape at across the input span when
/// drawing the preview curve. 48 is smooth at this size without being a cost.
const PREVIEW_SAMPLES: usize = 48;

// ── Colors ──────────────────────────────────────────────────────────
// Plain sRGB `Color32`, the app-wide colour currency; the `Painter` adapter is
// the single sRGB → linear conversion site.
const PANEL_BG: Color32 = Color32::new(33, 33, 41, 255);
const PANEL_BORDER: Color32 = Color32::new(128, 199, 255, 217);
const BTN_BG: Color32 = Color32::new(56, 56, 69, 255);
const BTN_BG_ACTIVE: Color32 = Color32::new(107, 76, 158, 255); // Ableton-purple INV
const CURVE_BG: Color32 = Color32::new(51, 51, 64, 255);
const CURVE_BG_ACTIVE: Color32 = Color32::new(71, 87, 128, 255);
const TEXT_PRIMARY: [u8; 4] = [220, 220, 230, 255];
const TEXT_SECONDARY: [u8; 4] = [150, 150, 165, 255];
// Preview plot: a darker inset box, a bright response line, and a live dot.
const PREVIEW_BG: Color32 = Color32::new(20, 20, 26, 255);
const PREVIEW_LINE: Color32 = Color32::new(128, 199, 255, 242);
const PREVIEW_GRID: Color32 = Color32::new(255, 255, 255, 15);
const PREVIEW_DOT: Color32 = Color32::new(255, 219, 115, 255);

/// The four curve options in display order. Indexed by the order shown,
/// not by enum discriminant, so the dropdown order is stable here.
const CURVES: [MacroCurve; 4] = [
    MacroCurve::Linear,
    MacroCurve::Exponential,
    MacroCurve::Logarithmic,
    MacroCurve::SCurve,
];

fn curve_label(c: MacroCurve) -> &'static str {
    match c {
        MacroCurve::Linear => "Linear",
        MacroCurve::Exponential => "Exp",
        MacroCurve::Logarithmic => "Log",
        MacroCurve::SCurve => "S-Curve",
    }
}

/// Compact value formatting for the live readout: whole numbers show clean,
/// others show up to two decimals with trailing zeros trimmed (`64`, `0.01`,
/// `1.5`). Falls back to scientific for very large/small magnitudes.
fn trim_num(v: f32) -> String {
    let a = v.abs();
    if a != 0.0 && !(0.001..1_000_000.0).contains(&a) {
        return format!("{v:.2e}");
    }
    if v.fract() == 0.0 {
        return format!("{}", v as i64);
    }
    crate::fmt::fmt_trimmed(v, 2)
}

/// Format a scale/offset value for the popover field, picking a precision
/// that reads for both tiny conversions (`0.0174`) and large folds (`1e6`).
fn format_affine(v: f32) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    let a = v.abs();
    if !(0.001..100_000.0).contains(&a) {
        format!("{v:.3e}")
    } else if a >= 100.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.4}")
    }
}

/// Pixels-to-value gain for the scale/offset scrub, scaled by the field's
/// magnitude (floored) so a value near `0.0174` (deg→rad) nudges finely and
/// one near `1e6` (a particle-count fold) moves fast. Precise values come
/// from the fold; this scrub is for manual tweaks near the current value.
const SCRUB_K: f32 = 0.004;
const SCRUB_FLOOR: f32 = 0.05;

/// Pointer travel (px) below which a press-release on a scale/offset field
/// counts as a click (→ enter numeric edit) rather than a scrub drag.
const CLICK_SLOP: f32 = 3.0;

/// Which draggable element (if any) the pointer grabbed on press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragTarget {
    Min,
    Max,
    Scale,
    Offset,
}

/// Which field is being typed into. The four numeric fields parse as `f32`;
/// `Label` is a free-text rename committed via `EffectMappingLabel`; `Section`
/// is a free-text (or empty-to-clear) commit via `EffectMappingSection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditField {
    Min,
    Max,
    Scale,
    Offset,
    Label,
    Section,
}

impl From<DragTarget> for EditField {
    fn from(d: DragTarget) -> Self {
        match d {
            DragTarget::Min => EditField::Min,
            DragTarget::Max => EditField::Max,
            DragTarget::Scale => EditField::Scale,
            DragTarget::Offset => EditField::Offset,
        }
    }
}

/// In-place mapping editor for one binding. `open()` seeds it with the
/// binding's current state + anchor; `render`/hit-test/`drain_actions`
/// drive it each frame while open.
pub struct MappingPopover {
    open: bool,
    /// Stable id of the binding being edited — addresses every emitted
    /// `PanelAction` so the app routes to the right `UserParamBinding`.
    binding_id: String,
    label: String,
    /// The binding's card section (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2
    /// D5). `None` = unsectioned. Editable via `EditField::Section` (P5c).
    section: Option<String>,
    /// The binding's declared min/max *range bounds* (the slider's full
    /// span). The trim handles select a sub-window within this; we map a
    /// handle's pixel position to a value in `[range_lo, range_hi]`.
    range_lo: f32,
    range_hi: f32,
    /// Current selected min/max (the binding's `min`/`max`).
    cur_min: f32,
    cur_max: f32,
    invert: bool,
    curve: MacroCurve,
    /// Card→consumer affine remap: `out = value * scale + offset`. The home
    /// for a folded `affine_scalar` (`scale = 1.0`, `offset = 0.0` is
    /// identity).
    cur_scale: f32,
    cur_offset: f32,
    /// Top-left of the popover panel in screen space, resolved at `open`
    /// from the anchor rect.
    origin: (f32, f32),
    /// Active drag, if any. For `Min`/`Max` the track maps position to a
    /// value; for `Scale`/`Offset` the field scrubs relative to its start.
    dragging: Option<DragTarget>,
    /// Press-anchor for a scale/offset scrub: screen-x at press and the
    /// field's value at press, so the delta is taken from the start rather
    /// than chained per frame.
    scrub_press_x: f32,
    scrub_start: f32,
    /// True once a scale/offset press has moved past [`CLICK_SLOP`], so the
    /// release commits a scrub rather than entering numeric edit. A press that
    /// never moves is a click → type the value.
    drag_moved: bool,
    /// The field currently being typed into (`None` = not editing). Clicking a
    /// value (min / max / scale / offset / section) or the header label enters
    /// edit mode; the host feeds keystrokes via [`Self::on_text_char`] /
    /// [`Self::on_backspace`] and commits with [`Self::commit_edit`] (Enter) or
    /// cancels with [`Self::cancel_edit`] (Esc). A press elsewhere inside the
    /// panel also commits (D16 blur-commit); closing the popover (a press
    /// outside it) discards instead, unchanged overlay-teardown semantics.
    edit: Option<EditField>,
    /// The active [`Self::edit`] field's editing model (P5c, I7) — caret +
    /// selection + text, shared shape with every other text session in the
    /// app. Seeded fresh by [`Self::enter_edit`]; numeric fields seed empty
    /// (type a fresh value), Label/Section seed with the current text
    /// pre-selected (edit in place). Commit parses/validates per field.
    model: TextEditModel,
    /// True between a press inside the field currently being edited and its
    /// release — `on_move` extends the selection via `drag_to` while set
    /// (P5c mouse press/drag routing, mirrors `manifold-app`'s
    /// `TextInputState::dragging`).
    text_dragging: bool,
    /// The card slider's current (post-modulation) value, pushed by the host
    /// each frame via [`Self::set_live_value`]. Drives the live dot on the
    /// preview so you can watch where the knob sits — and where drivers /
    /// Ableton / envelopes move it — on the response curve. `None` hides the
    /// dot (host couldn't resolve the value, or no host feeds it).
    live_value: Option<f32>,
    /// Actions accrued this frame; drained by the host after each event.
    pending_actions: Vec<PanelAction>,
}

impl MappingPopover {
    pub fn new() -> Self {
        Self {
            open: false,
            binding_id: String::new(),
            label: String::new(),
            section: None,
            range_lo: 0.0,
            range_hi: 1.0,
            cur_min: 0.0,
            cur_max: 1.0,
            invert: false,
            curve: MacroCurve::Linear,
            cur_scale: 1.0,
            cur_offset: 0.0,
            origin: (0.0, 0.0),
            dragging: None,
            scrub_press_x: 0.0,
            scrub_start: 0.0,
            drag_moved: false,
            edit: None,
            model: TextEditModel::new(""),
            text_dragging: false,
            live_value: None,
            pending_actions: Vec::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// True while a value field is being typed into — the host routes
    /// keystrokes here instead of firing canvas shortcuts.
    pub fn is_editing(&self) -> bool {
        self.edit.is_some()
    }

    /// Stable id of the binding currently being edited — the host reads this
    /// to look up the live value to feed back via [`Self::set_live_value`].
    pub fn binding_id(&self) -> &str {
        &self.binding_id
    }

    /// Push the card slider's current value so the preview can mark where the
    /// live knob sits on the response curve. Called by the host each frame
    /// while open; `None` hides the dot.
    pub fn set_live_value(&mut self, value: Option<f32>) {
        self.live_value = value;
    }

    /// Drain queued mapping edits. Mirrors `GraphCanvas::drain_actions`.
    pub fn drain_actions(&mut self) -> Vec<PanelAction> {
        std::mem::take(&mut self.pending_actions)
    }

    /// Open the popover for `binding_id`, seeded with its current
    /// mapping. `anchor` is the screen-space rect of the row that
    /// triggered it (e.g. an on-node param row); the panel is placed just
    /// to its right, nudged back on-screen against `clip` so it never
    /// renders past the canvas edge. `range` is the binding's declared
    /// bounds — the trim track spans it; `None` falls back to the current
    /// min/max so the handles still have a usable span.
    #[allow(clippy::too_many_arguments)]
    pub fn open(
        &mut self,
        binding_id: String,
        label: String,
        min: f32,
        max: f32,
        invert: bool,
        curve: MacroCurve,
        scale: f32,
        offset: f32,
        range: Option<(f32, f32)>,
        section: Option<String>,
        anchor: Rect,
        clip: Rect,
    ) {
        self.binding_id = binding_id;
        self.label = label;
        self.section = section;
        self.cur_min = min;
        self.cur_max = max;
        self.invert = invert;
        self.curve = curve;
        self.cur_scale = scale;
        self.cur_offset = offset;
        // Range bounds: prefer the declared range; otherwise span the
        // current min/max (with a tiny pad so a zero-width range still
        // gives the handles room).
        let (lo, hi) = range.unwrap_or((min, max));
        let (mut lo, mut hi) = if hi > lo {
            (lo, hi)
        } else {
            (min.min(max), min.max(max) + 1.0)
        };
        // Always span the current selection so a note whose min/max was typed
        // past the param's nominal range still shows its handles on the track
        // instead of pinned off an edge.
        lo = lo.min(self.cur_min);
        hi = hi.max(self.cur_max);
        self.range_lo = lo;
        self.range_hi = hi;
        self.dragging = None;
        self.drag_moved = false;
        self.edit = None;
        self.text_dragging = false;
        self.model = TextEditModel::new("");
        self.live_value = None;

        let h = self.panel_height();
        // Place to the right of the anchor row; flip to the left if it
        // would overflow the clip rect's right edge.
        let mut x = anchor.x + anchor.w + 6.0;
        if x + POPOVER_W > clip.x + clip.w {
            x = anchor.x - POPOVER_W - 6.0;
        }
        x = x.clamp(clip.x + 2.0, (clip.x + clip.w - POPOVER_W - 2.0).max(clip.x + 2.0));
        let mut y = anchor.y;
        if y + h > clip.y + clip.h {
            y = (clip.y + clip.h - h - 2.0).max(clip.y + 2.0);
        }
        y = y.max(clip.y + 2.0);
        self.origin = (x, y);
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.dragging = None;
        self.edit = None;
        self.text_dragging = false;
        self.model = TextEditModel::new("");
    }

    // ── Geometry ────────────────────────────────────────────────────

    fn panel_height(&self) -> f32 {
        // header + preview + min + max + invert + curve + scale + offset +
        // section + goto-node, padded.
        PAD + HEADER_H
            + ROW_GAP + PREVIEW_H    // live response preview
            + ROW_GAP + ROW_H        // min
            + ROW_GAP + ROW_H        // max
            + ROW_GAP + ROW_H        // invert
            + ROW_GAP + CURVE_ROW_H  // curve
            + ROW_GAP + ROW_H        // scale
            + ROW_GAP + ROW_H        // offset
            + ROW_GAP + ROW_H        // section (P5c)
            + ROW_GAP + ROW_H        // go to node
            + PAD
    }

    fn panel_rect(&self) -> Rect {
        Rect::new(self.origin.0, self.origin.1, POPOVER_W, self.panel_height())
    }

    /// The clickable header-label region (left of the live readout). Click to
    /// rename the knob.
    fn header_rect(&self) -> Rect {
        Rect::new(
            self.origin.0 + PAD,
            self.origin.1 + PAD - 2.0,
            POPOVER_W * 0.55,
            HEADER_H,
        )
    }

    /// The live response-curve preview box, inset under the header. With the
    /// trim track gone, this plot IS the range picture — its x-axis spans the
    /// current min..max, so you read the shape against the bounds below it.
    fn preview_rect(&self) -> Rect {
        let y = self.origin.1 + PAD + HEADER_H + ROW_GAP;
        Rect::new(self.origin.0 + PAD, y, POPOVER_W - 2.0 * PAD, PREVIEW_H)
    }

    fn min_row_y(&self) -> f32 {
        self.preview_rect().y + PREVIEW_H + ROW_GAP
    }
    fn max_row_y(&self) -> f32 {
        self.min_row_y() + ROW_H + ROW_GAP
    }
    fn invert_row_y(&self) -> f32 {
        self.max_row_y() + ROW_H + ROW_GAP
    }
    fn curve_row_y(&self) -> f32 {
        self.invert_row_y() + ROW_H + ROW_GAP
    }
    fn scale_row_y(&self) -> f32 {
        self.curve_row_y() + CURVE_ROW_H + ROW_GAP
    }
    fn offset_row_y(&self) -> f32 {
        self.scale_row_y() + ROW_H + ROW_GAP
    }
    fn section_row_y(&self) -> f32 {
        self.offset_row_y() + ROW_H + ROW_GAP
    }
    fn goto_row_y(&self) -> f32 {
        self.section_row_y() + ROW_H + ROW_GAP
    }

    /// Right-aligned click-to-type value box for the card `section` field
    /// (P5c) — same shape as [`Self::value_field_rect`], its own row since
    /// `Section` isn't a [`DragTarget`] (no scrub, click-to-type only).
    fn section_field_rect(&self) -> Rect {
        Rect::new(
            self.origin.0 + POPOVER_W - PAD - 76.0,
            self.section_row_y(),
            76.0,
            ROW_H,
        )
    }

    /// Full-width "Go to node" button at the popover's foot — the discoverable
    /// "where is this slider mapped from?" affordance. Click navigates the
    /// editor canvas to the node this binding is exposed from.
    fn goto_btn_rect(&self) -> Rect {
        Rect::new(
            self.origin.0 + PAD,
            self.goto_row_y(),
            POPOVER_W - 2.0 * PAD,
            ROW_H,
        )
    }

    /// The row-Y for a value field (Min / Max / Scale / Offset).
    fn value_field_row_y(&self, which: DragTarget) -> f32 {
        match which {
            DragTarget::Min => self.min_row_y(),
            DragTarget::Max => self.max_row_y(),
            DragTarget::Scale => self.scale_row_y(),
            DragTarget::Offset => self.offset_row_y(),
        }
    }

    /// Right-aligned click-to-type value box for a Min/Max/Scale/Offset field;
    /// its label fills the rest of the row to the left.
    fn value_field_rect(&self, which: DragTarget) -> Rect {
        Rect::new(
            self.origin.0 + POPOVER_W - PAD - 76.0,
            self.value_field_row_y(which),
            76.0,
            ROW_H,
        )
    }

    fn invert_btn_rect(&self) -> Rect {
        let y = self.invert_row_y();
        // Right-aligned 36px INV button, label fills the rest.
        Rect::new(self.origin.0 + POPOVER_W - PAD - 36.0, y, 36.0, ROW_H)
    }

    fn curve_cell_rect(&self, idx: usize) -> Rect {
        let n = CURVES.len() as f32;
        let avail = POPOVER_W - 2.0 * PAD;
        let gap = 2.0;
        let cell_w = (avail - gap * (n - 1.0)) / n;
        let x = self.origin.0 + PAD + idx as f32 * (cell_w + gap);
        Rect::new(x, self.curve_row_y(), cell_w, CURVE_ROW_H)
    }

    fn point_in(r: Rect, sx: f32, sy: f32) -> bool {
        sx >= r.x && sx <= r.x + r.w && sy >= r.y && sy <= r.y + r.h
    }

    /// True when the point is anywhere inside the popover panel. The host
    /// uses this to decide whether a click is "inside" (route to the
    /// popover) or "outside" (dismiss).
    pub fn contains_point(&self, sx: f32, sy: f32) -> bool {
        self.open && Self::point_in(self.panel_rect(), sx, sy)
    }

    // ── Input ───────────────────────────────────────────────────────

    /// Screen geometry of the field currently being edited, plus its font
    /// size and whether its text right-aligns (the four numeric fields +
    /// Section) or left-aligns (Label) — the input the pointer-routing and
    /// caret-rendering math below share (P5c).
    fn active_field_geometry(&self) -> Option<(Rect, f32, bool)> {
        match self.edit? {
            EditField::Label => Some((self.header_rect(), FONT, false)),
            EditField::Section => Some((self.section_field_rect(), FONT_SMALL, true)),
            EditField::Min => Some((self.value_field_rect(DragTarget::Min), FONT_SMALL, true)),
            EditField::Max => Some((self.value_field_rect(DragTarget::Max), FONT_SMALL, true)),
            EditField::Scale => Some((self.value_field_rect(DragTarget::Scale), FONT_SMALL, true)),
            EditField::Offset => Some((self.value_field_rect(DragTarget::Offset), FONT_SMALL, true)),
        }
    }

    /// The byte offset under screen-x `sx` within the field currently being
    /// edited, via [`byte_offset_for_x`] — `draw::text_width` is the
    /// measurer, same width function the rendered text uses, so hit-testing
    /// and drawing never disagree. `0` if nothing is being edited (callers
    /// only reach this while `is_editing()`).
    fn byte_at_field_x(&self, sx: f32) -> usize {
        let Some((rect, font, right_aligned)) = self.active_field_geometry() else {
            return 0;
        };
        let text = self.model.text();
        let mut measure = |s: &str| crate::draw::text_width(s, font);
        let text_start_x = if right_aligned {
            rect.x + rect.w - measure(text) - 5.0
        } else {
            rect.x
        };
        byte_offset_for_x(text, sx - text_start_x, &mut measure)
    }

    /// Pointer press. Returns `true` if the popover consumed it. A press
    /// outside the panel returns `false` so the host can dismiss.
    pub fn on_press(&mut self, sx: f32, sy: f32) -> bool {
        if !self.open {
            return false;
        }
        if !Self::point_in(self.panel_rect(), sx, sy) {
            return false;
        }
        // A press inside the field ALREADY being edited repositions the
        // caret and arms drag-select (P5c pointer routing) — it does NOT
        // commit-and-reopen the session, which would wipe the selection a
        // click-drag is trying to make.
        if let Some((field_rect, ..)) = self.active_field_geometry()
            && Self::point_in(field_rect, sx, sy)
        {
            let byte = self.byte_at_field_x(sx);
            self.model.caret_to(byte, false);
            self.text_dragging = true;
            return true;
        }
        // Any OTHER press inside the panel commits a pending edit first
        // (click-away to confirm — D16 blur-commit), then proceeds to
        // handle the new press.
        if self.is_editing() {
            self.commit_edit();
        }
        // Header label — click to rename the knob (seeded with the current
        // name, pre-selected). Drivers / Ableton / OSC address this binding
        // by stable id, so the rename never re-keys them; only the displayed
        // label changes.
        if Self::point_in(self.header_rect(), sx, sy) {
            self.enter_edit(EditField::Label);
            return true;
        }
        // Section field — click to type/clear the card section.
        if Self::point_in(self.section_field_rect(), sx, sy) {
            self.enter_edit(EditField::Section);
            return true;
        }
        // Min / Max value fields — click to type an exact bound (including past
        // the param's nominal range).
        for which in [DragTarget::Min, DragTarget::Max] {
            if Self::point_in(self.value_field_rect(which), sx, sy) {
                self.enter_edit(which.into());
                return true;
            }
        }
        // INV toggle.
        if Self::point_in(self.invert_btn_rect(), sx, sy) {
            self.invert = !self.invert;
            self.pending_actions.push(PanelAction::Root(RootAction::EffectMappingInvert {
                binding_id: self.binding_id.clone(),
                invert: self.invert,
            }));
            return true;
        }
        // Curve cells.
        for (idx, &c) in CURVES.iter().enumerate() {
            if Self::point_in(self.curve_cell_rect(idx), sx, sy) {
                if c != self.curve {
                    self.curve = c;
                    self.pending_actions.push(PanelAction::Root(RootAction::EffectMappingCurve {
                        binding_id: self.binding_id.clone(),
                        // `c` is already the UI `MacroCurve` the action carries.
                        curve: c,
                    }));
                }
                return true;
            }
        }
        // Scale / Offset scrub fields. Snapshot the pre-drag pair so the
        // commit records one undo entry; anchor the scrub on press-x and
        // the field's start value.
        for which in [DragTarget::Scale, DragTarget::Offset] {
            if Self::point_in(self.value_field_rect(which), sx, sy) {
                self.pending_actions
                    .push(PanelAction::Root(RootAction::EffectMappingAffineSnapshot {
                        binding_id: self.binding_id.clone(),
                    }));
                self.dragging = Some(which);
                self.drag_moved = false;
                self.scrub_press_x = sx;
                self.scrub_start = match which {
                    DragTarget::Scale => self.cur_scale,
                    _ => self.cur_offset,
                };
                return true;
            }
        }
        // "Go to node" — jump the editor canvas to the node this binding is
        // exposed from. Read-only navigation; emit and close so the canvas is
        // unobstructed when it centres.
        if Self::point_in(self.goto_btn_rect(), sx, sy) {
            self.pending_actions.push(PanelAction::Root(RootAction::EffectMappingGotoNode {
                binding_id: self.binding_id.clone(),
            }));
            self.open = false;
            return true;
        }
        // Inside the panel but on dead space — consume so the click
        // doesn't fall through to the canvas behind it.
        true
    }

    /// Pointer move. Drives an in-progress text drag-select over the
    /// editing field (P5c) when one is armed; otherwise the scale/offset
    /// scrub (the only drag now that the range is set by typing — no trim
    /// handles).
    pub fn on_move(&mut self, sx: f32, _sy: f32) {
        if !self.open {
            return;
        }
        if self.text_dragging {
            let byte = self.byte_at_field_x(sx);
            self.model.drag_to(byte);
            return;
        }
        if let Some(which) = self.dragging {
            self.apply_scrub(which, sx);
        }
    }

    /// Pointer release. Ends a text drag-select if one was armed. The only
    /// OTHER drag is a scale/offset scrub: a real scrub commits as one undo
    /// entry; a press that never moved is a click → enter numeric edit.
    pub fn on_release(&mut self) {
        self.text_dragging = false;
        if let Some(which) = self.dragging.take() {
            if self.drag_moved {
                self.pending_actions
                    .push(PanelAction::Root(RootAction::EffectMappingAffineCommit {
                        binding_id: self.binding_id.clone(),
                    }));
            } else {
                self.enter_edit(which.into());
            }
        }
    }

    // ── Text entry (P5c: TextEditModel-backed, I7) ────────────────────

    /// Begin typing into `field`. Numeric fields seed empty (type a fresh
    /// value); Label/Section seed with the current text, pre-selected (edit
    /// in place — first keystroke replaces it, same convention as every
    /// other text session in the app). Enter commits, Esc cancels. Cancels
    /// any in-progress drag.
    fn enter_edit(&mut self, field: EditField) {
        self.dragging = None;
        self.text_dragging = false;
        let seed = match field {
            EditField::Label => self.label.clone(),
            EditField::Section => self.section.clone().unwrap_or_default(),
            EditField::Min | EditField::Max | EditField::Scale | EditField::Offset => String::new(),
        };
        self.model = TextEditModel::new(&seed);
        if matches!(field, EditField::Label | EditField::Section) {
            self.model.select_all();
        }
        self.edit = Some(field);
    }

    /// Feed one typed character to the active field, replacing the current
    /// selection if any (typing replaces the selection, D16). Label/Section
    /// take any printable character; numeric fields take digits, a single
    /// decimal point, and a leading minus.
    pub fn on_text_char(&mut self, c: char) {
        match self.edit {
            Some(EditField::Label) | Some(EditField::Section) => {
                if !c.is_control() {
                    self.model.insert_char(c);
                }
            }
            Some(_) => {
                let allowed = c.is_ascii_digit()
                    || (c == '.' && !self.model.text().contains('.'))
                    || (c == '-' && self.model.text().is_empty());
                if allowed {
                    self.model.insert_char(c);
                }
            }
            None => {}
        }
    }

    /// Delete the selection, or (no selection) the char before the caret.
    pub fn on_backspace(&mut self) {
        if self.edit.is_some() {
            self.model.backspace();
        }
    }

    /// Cancel the edit, discarding the typed text.
    pub fn cancel_edit(&mut self) {
        self.edit = None;
        self.text_dragging = false;
        self.model = TextEditModel::new("");
    }

    /// Commit the typed value to the active field and emit the matching
    /// snapshot → changed → commit triad (one undo entry). An empty or
    /// unparseable buffer leaves the value unchanged. A min/max past the
    /// track's current span widens the span so the handle stays visible.
    pub fn commit_edit(&mut self) {
        let Some(field) = self.edit.take() else {
            return;
        };
        self.text_dragging = false;
        let buffer = self.model.take_text();
        let id = self.binding_id.clone();

        // Label: free-text rename. Emits the (already-existing) label edit and
        // updates the header locally so it shows immediately. Blank or
        // unchanged → no-op.
        if field == EditField::Label {
            let label = buffer.trim().to_string();
            if !label.is_empty() && label != self.label {
                self.label = label.clone();
                self.pending_actions
                    .push(PanelAction::Root(RootAction::EffectMappingLabel {
                        binding_id: id,
                        label,
                    }));
            }
            return;
        }

        // Section: free-text, empty clears back to unsectioned.
        // Outer touched-ness is implicit in emitting the action at
        // all; the inner `Option<String>` carries value-vs-clear.
        if field == EditField::Section {
            let trimmed = buffer.trim();
            let new_section = if trimmed.is_empty() { None } else { Some(trimmed.to_string()) };
            if new_section != self.section {
                self.section = new_section.clone();
                self.pending_actions.push(PanelAction::Root(RootAction::EffectMappingSection {
                    binding_id: id,
                    section: new_section,
                }));
            }
            return;
        }

        // Numeric fields. Empty / unparseable buffer leaves the value unchanged.
        let Some(v) = buffer.trim().parse::<f32>().ok().filter(|v| v.is_finite()) else {
            return;
        };
        match field {
            EditField::Min | EditField::Max => {
                self.pending_actions
                    .push(PanelAction::Root(RootAction::EffectMappingRangeSnapshot {
                        binding_id: id.clone(),
                    }));
                if field == EditField::Min {
                    self.cur_min = v.min(self.cur_max);
                    self.range_lo = self.range_lo.min(self.cur_min);
                } else {
                    self.cur_max = v.max(self.cur_min);
                    self.range_hi = self.range_hi.max(self.cur_max);
                }
                self.pending_actions
                    .push(PanelAction::Root(RootAction::EffectMappingRangeChanged {
                        binding_id: id.clone(),
                        min: self.cur_min,
                        max: self.cur_max,
                    }));
                self.pending_actions
                    .push(PanelAction::Root(RootAction::EffectMappingRangeCommit { binding_id: id }));
            }
            EditField::Scale | EditField::Offset => {
                self.pending_actions
                    .push(PanelAction::Root(RootAction::EffectMappingAffineSnapshot {
                        binding_id: id.clone(),
                    }));
                if field == EditField::Scale {
                    self.cur_scale = v;
                } else {
                    self.cur_offset = v;
                }
                self.pending_actions
                    .push(PanelAction::Root(RootAction::EffectMappingAffineChanged {
                        binding_id: id.clone(),
                        scale: self.cur_scale,
                        offset: self.cur_offset,
                    }));
                self.pending_actions
                    .push(PanelAction::Root(RootAction::EffectMappingAffineCommit { binding_id: id }));
            }
            EditField::Label | EditField::Section => unreachable!("handled above"),
        }
    }

    /// Scrub the scale or offset field relative to its press-anchor and
    /// emit the live `EffectMappingAffineChanged` with the current pair.
    /// Gain scales with the field's start magnitude (floored) so fine
    /// values nudge gently and large ones move fast.
    fn apply_scrub(&mut self, which: DragTarget, sx: f32) {
        let dpx = sx - self.scrub_press_x;
        // Below the slop, treat the press as a (so-far) click — don't scrub, so
        // a release without travel can enter numeric edit instead.
        if !self.drag_moved && dpx.abs() < CLICK_SLOP {
            return;
        }
        self.drag_moved = true;
        let gain = SCRUB_K * self.scrub_start.abs().max(SCRUB_FLOOR);
        let new = self.scrub_start + dpx * gain;
        match which {
            DragTarget::Scale => self.cur_scale = new,
            DragTarget::Offset => self.cur_offset = new,
            _ => {}
        }
        self.pending_actions
            .push(PanelAction::Root(RootAction::EffectMappingAffineChanged {
                binding_id: self.binding_id.clone(),
                scale: self.cur_scale,
                offset: self.cur_offset,
            }));
    }

    // ── Render ──────────────────────────────────────────────────────

    /// Evaluate the shared reshape pipeline — the exact math the runtime
    /// applies at the write boundary — for a raw card value. Used to plot the
    /// preview and place the live dot, so the picture can never disagree with
    /// what the engine does to the value.
    fn reshape_output(&self, input: f32) -> f32 {
        apply_card_reshape(
            input,
            self.cur_min,
            self.cur_max,
            self.invert,
            self.curve,
            self.cur_scale,
            self.cur_offset,
        )
    }

    /// Draw the live response-curve preview: the composed transform (range →
    /// invert → curve → scale/offset) plotted as input→output, with a dot at
    /// the current live value. This is the "see what you do" surface — picking
    /// S-Curve or dragging the range reshapes this line in real time.
    fn render_preview(&self, ui: &mut dyn Painter) {
        let r = self.preview_rect();
        ui.draw_rounded_rect(r.x, r.y, r.w, r.h, PREVIEW_BG, 3.0);
        // Faint centre cross for orientation.
        ui.draw_line(r.x + r.w * 0.5, r.y, r.x + r.w * 0.5, r.y + r.h, 1.0, PREVIEW_GRID);
        ui.draw_line(r.x, r.y + r.h * 0.5, r.x + r.w, r.y + r.h * 0.5, 1.0, PREVIEW_GRID);

        // Sample output across the full input span, tracking the output range
        // so the curve auto-fits the box vertically.
        let span = (self.range_hi - self.range_lo).max(f32::EPSILON);
        let mut outs = [0f32; PREVIEW_SAMPLES + 1];
        let (mut out_lo, mut out_hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for (i, slot) in outs.iter_mut().enumerate() {
            let t = i as f32 / PREVIEW_SAMPLES as f32;
            let o = self.reshape_output(self.range_lo + t * span);
            *slot = o;
            out_lo = out_lo.min(o);
            out_hi = out_hi.max(o);
        }
        let out_span = (out_hi - out_lo).max(f32::EPSILON);
        // Inset a hair so the line doesn't kiss the box edges.
        let pad_y = 3.0;
        let plot = |i: usize| -> (f32, f32) {
            let t = i as f32 / PREVIEW_SAMPLES as f32;
            let ny = (outs[i] - out_lo) / out_span;
            (
                r.x + t * r.w,
                r.y + r.h - pad_y - ny * (r.h - 2.0 * pad_y),
            )
        };
        for i in 0..PREVIEW_SAMPLES {
            let (x0, y0) = plot(i);
            let (x1, y1) = plot(i + 1);
            ui.draw_line(x0, y0, x1, y1, 1.5, PREVIEW_LINE);
        }

        // Live dot at the current value's position on the curve.
        if let Some(v) = self.live_value {
            let tn = ((v - self.range_lo) / span).clamp(0.0, 1.0);
            let ny = ((self.reshape_output(v) - out_lo) / out_span).clamp(0.0, 1.0);
            let x = r.x + tn * r.w;
            let y = r.y + r.h - pad_y - ny * (r.h - 2.0 * pad_y);
            ui.draw_rounded_rect(x - 2.5, y - 2.5, 5.0, 5.0, PREVIEW_DOT, 2.5);
        }
    }

    /// Draw one labeled click-to-type value field (Min / Max / Scale / Offset):
    /// the label on the left, a right-aligned box showing the formatted value
    /// or, while that field is being edited, the typed buffer + caret.
    fn draw_value_field(
        &self,
        ui: &mut dyn Painter,
        panel_x: f32,
        which: DragTarget,
        name: &str,
        value_text: String,
    ) {
        let r = self.value_field_rect(which);
        ui.draw_text(panel_x + PAD, r.y + 3.0, name, FONT, TEXT_SECONDARY);
        let editing = self.edit == Some(which.into());
        let active = self.dragging == Some(which) || editing;
        let bg = if active { CURVE_BG_ACTIVE } else { BTN_BG };
        ui.draw_rounded_rect(r.x, r.y, r.w, r.h, bg, 2.0);
        let txt = if editing { self.model.text().to_string() } else { value_text };
        let tw = crate::draw::text_width(&txt, FONT_SMALL);
        ui.draw_text(r.x + r.w - tw - 5.0, r.y + 3.0, &txt, FONT_SMALL, TEXT_PRIMARY);
        if editing {
            self.draw_caret_and_selection(ui, r, FONT_SMALL, true);
        }
    }

    /// Draw the card-`section` field (P5c): label on the left, a right-aligned
    /// click-to-type box on the right — `"—"` when unsectioned, the section
    /// name otherwise, or the live edit buffer + caret while typing.
    fn draw_section_field(&self, ui: &mut dyn Painter, panel_x: f32) {
        let r = self.section_field_rect();
        ui.draw_text(panel_x + PAD, r.y + 3.0, "Section", FONT, TEXT_SECONDARY);
        let editing = self.edit == Some(EditField::Section);
        let bg = if editing { CURVE_BG_ACTIVE } else { BTN_BG };
        ui.draw_rounded_rect(r.x, r.y, r.w, r.h, bg, 2.0);
        let txt = if editing {
            self.model.text().to_string()
        } else {
            self.section.clone().unwrap_or_else(|| "\u{2014}".to_string())
        };
        let tw = crate::draw::text_width(&txt, FONT_SMALL);
        ui.draw_text(r.x + r.w - tw - 5.0, r.y + 3.0, &txt, FONT_SMALL, TEXT_PRIMARY);
        if editing {
            self.draw_caret_and_selection(ui, r, FONT_SMALL, true);
        }
    }

    /// Draws the ranged-selection highlight + caret for the field currently
    /// being edited, from `self.model` (P5c — caret + selection instead of
    /// the old whole-buffer `"|"` suffix). `rect`/`font`/`right_aligned`
    /// mirror [`Self::active_field_geometry`] / [`Self::byte_at_field_x`]'s
    /// text-origin math, so hit-testing and drawing never disagree.
    fn draw_caret_and_selection(&self, ui: &mut dyn Painter, rect: Rect, font: f32, right_aligned: bool) {
        let text = self.model.text();
        let mut measure = |s: &str| crate::draw::text_width(s, font);
        let text_start_x = if right_aligned {
            rect.x + rect.w - measure(text) - 5.0
        } else {
            rect.x
        };
        if self.model.has_selection() {
            let sel = self.model.selection();
            let hx = text_start_x + x_for_byte_offset(text, sel.start, &mut measure);
            let hend = text_start_x + x_for_byte_offset(text, sel.end, &mut measure);
            ui.draw_rect(hx, rect.y + 2.0, (hend - hx).max(1.0), rect.h - 4.0, crate::color::TEXT_EDIT_SELECT_BG);
        }
        let cx = text_start_x + x_for_byte_offset(text, self.model.caret(), &mut measure);
        ui.draw_rect(cx, rect.y + 2.0, 1.0, rect.h - 4.0, crate::color::TEXT_EDIT_CARET);
    }

    pub fn render(&self, ui: &mut dyn Painter) {
        if !self.open {
            return;
        }
        let panel = self.panel_rect();
        ui.draw_bordered_rect(
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            PANEL_BG,
            6.0,
            1.0,
            PANEL_BORDER,
        );

        // Header: the binding label (left, click to rename) and the live
        // input→output readout (right). While renaming, the label shows the
        // model's live text with a ranged-selection highlight + caret drawn
        // over it (P5c — was a whole-buffer `"|"` suffix); the readout hides
        // so a long name has room.
        let renaming = self.edit == Some(EditField::Label);
        let hr = self.header_rect();
        if renaming {
            ui.draw_rounded_rect(hr.x - 3.0, hr.y, hr.w, hr.h, CURVE_BG_ACTIVE, 2.0);
        }
        let header_txt = if renaming { self.model.text().to_string() } else { self.label.clone() };
        ui.draw_text(panel.x + PAD, panel.y + PAD, &header_txt, FONT, TEXT_PRIMARY);
        if renaming {
            self.draw_caret_and_selection(ui, hr, FONT, false);
        }
        if let Some(v) = self.live_value.filter(|_| !renaming) {
            let readout = format!("{} → {}", trim_num(v), trim_num(self.reshape_output(v)));
            let tw = readout.chars().count() as f32 * FONT_SMALL * 0.55;
            ui.draw_text(
                panel.x + POPOVER_W - PAD - tw,
                panel.y + PAD + 1.0,
                &readout,
                FONT_SMALL,
                TEXT_SECONDARY,
            );
        }

        // Live response-curve preview.
        self.render_preview(ui);

        // ── Min / Max value fields ──
        // The slider's range, set by typing (the trim track + drag handles are
        // gone — the preview above is the range picture). Click a box to type;
        // a value past the param's nominal range is accepted and widens the
        // preview's span.
        for (which, name, val) in [
            (DragTarget::Min, "Min", self.cur_min),
            (DragTarget::Max, "Max", self.cur_max),
        ] {
            self.draw_value_field(ui, panel.x, which, name, format!("{val:.2}"));
        }

        // ── Invert toggle row ──
        let inv = self.invert_btn_rect();
        ui.draw_text(panel.x + PAD, inv.y + 3.0, "Invert", FONT, TEXT_SECONDARY);
        let inv_bg = if self.invert { BTN_BG_ACTIVE } else { BTN_BG };
        ui.draw_rounded_rect(inv.x, inv.y, inv.w, inv.h, inv_bg, 2.0);
        let inv_tc = if self.invert {
            TEXT_PRIMARY
        } else {
            TEXT_SECONDARY
        };
        ui.draw_text(inv.x + 6.0, inv.y + 3.0, "INV", FONT_SMALL, inv_tc);

        // ── Curve dropdown row (4 inline cells) ──
        ui.draw_text(
            panel.x + PAD,
            self.curve_row_y() - 13.0,
            "Curve",
            FONT_SMALL,
            TEXT_SECONDARY,
        );
        for (idx, &c) in CURVES.iter().enumerate() {
            let r = self.curve_cell_rect(idx);
            let active = c == self.curve;
            let bg = if active { CURVE_BG_ACTIVE } else { CURVE_BG };
            ui.draw_rounded_rect(r.x, r.y, r.w, r.h, bg, 2.0);
            let tc = if active { TEXT_PRIMARY } else { TEXT_SECONDARY };
            // Center the (short) label.
            let label = curve_label(c);
            let tw = label.chars().count() as f32 * FONT_SMALL * 0.5;
            ui.draw_text(
                r.x + (r.w - tw) * 0.5,
                r.y + 3.0,
                label,
                FONT_SMALL,
                tc,
            );
        }

        // ── Scale / Offset affine rows ──
        // The card→consumer remap (out = value * scale + offset). This is where
        // a folded affine_scalar node lives. Click the box to type; drag it to
        // scrub.
        for (which, name, val) in [
            (DragTarget::Scale, "Scale", self.cur_scale),
            (DragTarget::Offset, "Offset", self.cur_offset),
        ] {
            self.draw_value_field(ui, panel.x, which, name, format_affine(val));
        }

        // ── Section field ──
        self.draw_section_field(ui, panel.x);

        // ── Go to node ──
        // The discoverable "where is this slider mapped from?" action: a
        // full-width button that navigates the editor canvas to the node this
        // binding is exposed from.
        let g = self.goto_btn_rect();
        ui.draw_rounded_rect(g.x, g.y, g.w, g.h, BTN_BG, 2.0);
        let label = "\u{2192} Go to node";
        let tw = label.chars().count() as f32 * FONT * 0.5;
        ui.draw_text(
            g.x + (g.w - tw) * 0.5,
            g.y + 3.0,
            label,
            FONT,
            TEXT_PRIMARY,
        );
    }
}

impl Default for MappingPopover {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_popover() -> MappingPopover {
        let mut p = MappingPopover::new();
        p.open(
            "user.uv.rotation.1".to_string(),
            "Rotation".to_string(),
            0.2,
            0.8,
            false,
            MacroCurve::Linear,
            1.0,
            0.0,
            Some((0.0, 1.0)),
            None,
            Rect::new(100.0, 100.0, 168.0, 18.0),
            Rect::new(0.0, 0.0, 1000.0, 800.0),
        );
        p
    }

    #[test]
    fn opens_and_seeds_state() {
        let p = open_popover();
        assert!(p.is_open());
        assert_eq!(p.cur_min, 0.2);
        assert_eq!(p.cur_max, 0.8);
        assert!(!p.invert);
        assert_eq!(p.curve, MacroCurve::Linear);
    }

    #[test]
    fn live_value_round_trips_and_preview_uses_shared_math() {
        let mut p = open_popover();
        assert_eq!(p.binding_id(), "user.uv.rotation.1");
        assert!(p.live_value.is_none(), "no live value until the host feeds one");
        p.set_live_value(Some(0.5));
        assert_eq!(p.live_value, Some(0.5));
        // Identity mapping (Linear, no invert, scale 1 / offset 0): the preview
        // evaluates the same core pipeline the runtime uses → passthrough.
        assert!((p.reshape_output(0.5) - 0.5).abs() < 1e-6);
        // Opening a fresh binding clears the stale live value.
        p.open(
            "other".to_string(),
            "Other".to_string(),
            0.0,
            1.0,
            false,
            MacroCurve::Linear,
            1.0,
            0.0,
            Some((0.0, 1.0)),
            None,
            Rect::new(0.0, 0.0, 168.0, 18.0),
            Rect::new(0.0, 0.0, 1000.0, 800.0),
        );
        assert!(p.live_value.is_none(), "open() resets the live value");
    }

    #[test]
    fn invert_click_emits_action() {
        let mut p = open_popover();
        let r = p.invert_btn_rect();
        let consumed = p.on_press(r.x + 2.0, r.y + 2.0);
        assert!(consumed);
        let actions = p.drain_actions();
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Root(RootAction::EffectMappingInvert { invert: true, .. })]
        ));
        assert!(p.invert);
    }

    #[test]
    fn curve_click_emits_action() {
        let mut p = open_popover();
        // Cell index 1 is Exponential.
        let r = p.curve_cell_rect(1);
        let consumed = p.on_press(r.x + 2.0, r.y + 2.0);
        assert!(consumed);
        let actions = p.drain_actions();
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Root(RootAction::EffectMappingCurve {
                curve: MacroCurve::Exponential,
                ..
            })]
        ));
        assert_eq!(p.curve, MacroCurve::Exponential);
    }


    #[test]
    fn scale_scrub_snapshots_changes_and_commits() {
        let mut p = open_popover(); // cur_scale = 1.0
        let r = p.value_field_rect(DragTarget::Scale);
        let consumed = p.on_press(r.x + r.w * 0.5, r.y + r.h * 0.5);
        assert!(consumed);
        // First action is the affine snapshot.
        let after_press = p.drain_actions();
        assert!(matches!(
            after_press.first(),
            Some(PanelAction::Root(RootAction::EffectMappingAffineSnapshot { .. }))
        ));
        // Drag right → scale increases above its 1.0 start.
        p.on_move(r.x + r.w * 0.5 + 80.0, r.y);
        let changed = p.drain_actions();
        assert!(matches!(
            changed.last(),
            Some(PanelAction::Root(RootAction::EffectMappingAffineChanged { .. }))
        ));
        assert!(p.cur_scale > 1.0, "drag right raises scale, got {}", p.cur_scale);
        // Offset untouched by a scale scrub.
        assert_eq!(p.cur_offset, 0.0);
        // Release commits the affine drag as one undo entry.
        p.on_release();
        let commit = p.drain_actions();
        assert!(matches!(
            commit.as_slice(),
            [PanelAction::Root(RootAction::EffectMappingAffineCommit { .. })]
        ));
    }

    #[test]
    fn press_outside_panel_not_consumed() {
        let mut p = open_popover();
        let panel = p.panel_rect();
        let consumed = p.on_press(panel.x - 50.0, panel.y - 50.0);
        assert!(!consumed);
    }

    #[test]
    fn typed_min_clamps_to_max() {
        let mut p = open_popover(); // cur_min 0.2, cur_max 0.8
        p.enter_edit(EditField::Min);
        for ch in "5".chars() {
            p.on_text_char(ch); // type a min above the current max
        }
        p.commit_edit();
        assert!(p.cur_min <= p.cur_max, "typed min clamps to <= max");
    }

    #[test]
    fn click_max_value_types_past_nominal_range() {
        let mut p = open_popover(); // range (0,1), cur_max 0.8
        let box_r = p.value_field_rect(DragTarget::Max);
        assert!(p.on_press(box_r.x + 2.0, box_r.y + 2.0));
        assert!(p.is_editing(), "clicking the max value enters edit");
        p.drain_actions();
        for ch in "128".chars() {
            p.on_text_char(ch);
        }
        p.commit_edit();
        assert!(!p.is_editing());
        assert_eq!(p.cur_max, 128.0, "typed value past nominal max is accepted");
        assert!(p.range_hi >= 128.0, "track span widened to keep the handle visible");
        let actions = p.drain_actions();
        assert!(matches!(
            actions.first(),
            Some(PanelAction::Root(RootAction::EffectMappingRangeSnapshot { .. }))
        ));
        assert!(actions.iter().any(|a| matches!(
            a,
            PanelAction::Root(RootAction::EffectMappingRangeChanged { max, .. }) if (*max - 128.0).abs() < 1e-3
        )));
        assert!(matches!(
            actions.last(),
            Some(PanelAction::Root(RootAction::EffectMappingRangeCommit { .. }))
        ));
    }

    #[test]
    fn scale_click_without_drag_enters_edit_then_commits() {
        let mut p = open_popover(); // cur_scale 1.0
        let r = p.value_field_rect(DragTarget::Scale);
        // Press and release with no movement → a click, not a scrub.
        assert!(p.on_press(r.x + r.w * 0.5, r.y + r.h * 0.5));
        p.on_release();
        assert!(p.is_editing(), "a no-drag click enters numeric edit");
        p.drain_actions();
        for ch in "2.5".chars() {
            p.on_text_char(ch);
        }
        p.commit_edit();
        assert_eq!(p.cur_scale, 2.5);
        let actions = p.drain_actions();
        assert!(actions.iter().any(|a| matches!(
            a,
            PanelAction::Root(RootAction::EffectMappingAffineChanged { scale, .. }) if (*scale - 2.5).abs() < 1e-3
        )));
        assert!(matches!(
            actions.last(),
            Some(PanelAction::Root(RootAction::EffectMappingAffineCommit { .. }))
        ));
    }

    #[test]
    fn text_entry_filters_input_and_cancel_discards() {
        let mut p = open_popover();
        p.enter_edit(EditField::Offset);
        // Letters ignored; one decimal point; minus only leading.
        for ch in "-1a2.3.4".chars() {
            p.on_text_char(ch);
        }
        assert_eq!(p.model.text(), "-12.34");
        p.on_backspace();
        assert_eq!(p.model.text(), "-12.3");
        // Cancel discards without emitting or changing the value.
        let before = p.cur_offset;
        p.drain_actions();
        p.cancel_edit();
        assert!(!p.is_editing());
        assert_eq!(p.cur_offset, before);
        assert!(p.drain_actions().is_empty());
    }

    #[test]
    fn click_header_renames_via_label_action() {
        let mut p = open_popover(); // label "Rotation"
        let hr = p.header_rect();
        assert!(p.on_press(hr.x + 2.0, hr.y + 2.0));
        assert!(p.is_editing(), "clicking the header enters label edit");
        // Label seeds with the current name, pre-selected (D16); the field
        // takes free text + spaces.
        assert_eq!(p.model.text(), "Rotation");
        while !p.model.text().is_empty() {
            p.on_backspace();
        }
        for ch in "Chaos".chars() {
            p.on_text_char(ch);
        }
        p.on_text_char(' ');
        p.on_text_char('X');
        p.drain_actions();
        p.commit_edit();
        assert!(!p.is_editing());
        assert_eq!(p.label, "Chaos X");
        let actions = p.drain_actions();
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Root(RootAction::EffectMappingLabel { label, .. })] if label == "Chaos X"
        ));
    }

    // ── Section field ───────────────────────────

    #[test]
    fn click_section_field_enters_edit_and_types_a_value() {
        let mut p = open_popover(); // unsectioned
        assert_eq!(p.section, None);
        let r = p.section_field_rect();
        assert!(p.on_press(r.x + 2.0, r.y + 2.0));
        assert!(p.is_editing(), "clicking the section box enters edit");
        p.drain_actions();
        for ch in "Lights".chars() {
            p.on_text_char(ch);
        }
        p.commit_edit();
        assert!(!p.is_editing());
        assert_eq!(p.section, Some("Lights".to_string()));
        let actions = p.drain_actions();
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Root(RootAction::EffectMappingSection { section: Some(s), .. })] if s == "Lights"
        ));
    }

    #[test]
    fn committing_an_empty_section_clears_it() {
        let mut p = open_popover();
        p.enter_edit(EditField::Section);
        p.model.select_all();
        // Seed a section, commit, then clear it.
        for ch in "Lights".chars() {
            p.on_text_char(ch);
        }
        p.commit_edit();
        p.drain_actions();
        assert_eq!(p.section, Some("Lights".to_string()));

        p.enter_edit(EditField::Section);
        // enter_edit pre-selects the seeded text; typing nothing and
        // committing an emptied buffer clears the section back to None.
        while !p.model.text().is_empty() {
            p.on_backspace();
        }
        p.commit_edit();
        assert_eq!(p.section, None);
        let actions = p.drain_actions();
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Root(RootAction::EffectMappingSection { section: None, .. })]
        ));
    }

    #[test]
    fn section_seeds_pre_selected_so_typing_replaces_it() {
        let mut p = MappingPopover::new();
        p.open(
            "b".to_string(),
            "Label".to_string(),
            0.0,
            1.0,
            false,
            MacroCurve::Linear,
            1.0,
            0.0,
            Some((0.0, 1.0)),
            Some("Old".to_string()),
            Rect::new(100.0, 100.0, 168.0, 18.0),
            Rect::new(0.0, 0.0, 1000.0, 800.0),
        );
        p.enter_edit(EditField::Section);
        assert_eq!(p.model.text(), "Old");
        assert!(p.model.has_selection(), "seeded text starts pre-selected (D16)");
        p.on_text_char('N');
        assert_eq!(p.model.text(), "N", "typing over the selection replaces it, not appends");
    }

    // ── P5c: pointer-driven caret placement + drag-select ─────────────

    #[test]
    fn clicking_inside_the_field_already_being_edited_repositions_the_caret() {
        let mut p = open_popover();
        p.enter_edit(EditField::Offset);
        for ch in "12.5".chars() {
            p.on_text_char(ch);
        }
        assert_eq!(p.model.caret(), 4, "caret at the end after typing");
        // A second press inside the SAME field (not commit-and-reopen)
        // repositions the caret instead of resetting the buffer.
        let r = p.value_field_rect(DragTarget::Offset);
        assert!(p.on_press(r.x + 1.0, r.y + 2.0), "press inside the active field is consumed");
        assert!(p.is_editing(), "still editing — not committed by an in-field press");
        assert_eq!(p.model.text(), "12.5", "buffer untouched by the reposition click");
    }

    #[test]
    fn dragging_inside_the_active_field_extends_the_selection() {
        let mut p = open_popover();
        p.enter_edit(EditField::Offset);
        for ch in "12.5".chars() {
            p.on_text_char(ch);
        }
        let r = p.value_field_rect(DragTarget::Offset);
        // Press near the box's right edge (end of the right-aligned text)…
        assert!(p.on_press(r.x + r.w - 1.0, r.y + 2.0));
        // …then drag toward its left edge — a selection should grow.
        p.on_move(r.x + 1.0, r.y + 2.0);
        assert!(p.model.has_selection(), "drag inside the field grows a selection");
        p.on_release();
        // Release ends the drag; the selection (and edit session) persists
        // until commit/cancel.
        assert!(p.is_editing());
    }

    #[test]
    fn pressing_a_different_field_while_editing_commits_first_then_opens_the_new_one() {
        let mut p = open_popover(); // cur_scale 1.0
        p.enter_edit(EditField::Offset);
        for ch in "5".chars() {
            p.on_text_char(ch);
        }
        // Press the Scale field (not Offset) — blur-commits Offset (D16).
        // Scale/Offset are scrub fields: a press-with-no-drag then arms a
        // click, which `on_release` resolves into entering edit on Scale.
        let scale_r = p.value_field_rect(DragTarget::Scale);
        assert!(p.on_press(scale_r.x + scale_r.w * 0.5, scale_r.y + scale_r.h * 0.5));
        assert_eq!(p.cur_offset, 5.0, "the offset edit was committed on blur");
        p.on_release();
        assert!(p.is_editing(), "the Scale click (no drag) enters its own edit");
        assert_eq!(p.model.text(), "", "the newly-opened Scale field seeds empty");
    }
}
