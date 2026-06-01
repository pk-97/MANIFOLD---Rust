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
//! Why it draws with `UIRenderer` rect/text primitives rather than the
//! `param_slider_shared.rs` builders: those builders are `UITree`-based
//! (`tree.add_button`, parent ids), and the host surface here — the graph
//! canvas — renders entirely through `UIRenderer` immediate-mode
//! primitives with no `UITree`. So the popover mirrors the *visual
//! conventions* of the shared widgets (a min/max trim track with two drag
//! bars, an `INV` toggle, a 4-option curve dropdown) while staying inside
//! the canvas's draw model. The min/max drag reuses the existing
//! snapshot/changed/commit `PanelAction` triad, so a range drag still
//! coalesces into one undo entry.
//!
//! Label editing is intentionally **deferred**: a real text field on the
//! `UIRenderer` canvas would need caret/selection/IME handling that
//! doesn't exist on this surface yet. The label is shown read-only in the
//! popover header so you know which binding you're reshaping; renaming
//! still works from the (existing) `EffectMappingLabel` action whenever a
//! text-field surface lands.

use manifold_core::macro_bank::MacroCurve;
use manifold_renderer::ui_renderer::UIRenderer;
use manifold_ui::PanelAction;

use crate::graph_canvas::Rect;

// ── Layout constants ────────────────────────────────────────────────
const POPOVER_W: f32 = 188.0;
const PAD: f32 = 8.0;
const ROW_H: f32 = 18.0;
const ROW_GAP: f32 = 6.0;
const HEADER_H: f32 = 16.0;
const TRACK_H: f32 = 14.0;
const HANDLE_W: f32 = 4.0;
const CURVE_ROW_H: f32 = 16.0;
const FONT: f32 = 11.0;
const FONT_SMALL: f32 = 10.0;

// ── Colors ──────────────────────────────────────────────────────────
const PANEL_BG: [f32; 4] = [0.13, 0.13, 0.16, 0.98];
const PANEL_BORDER: [f32; 4] = [0.50, 0.78, 1.00, 0.85];
const TRACK_BG: [f32; 4] = [1.0, 1.0, 1.0, 0.08];
const TRACK_FILL: [f32; 4] = [0.50, 0.78, 1.00, 0.45];
const HANDLE_COLOR: [f32; 4] = [0.50, 0.78, 1.00, 1.0];
const HANDLE_HOVER: [f32; 4] = [0.72, 0.90, 1.00, 1.0];
const BTN_BG: [f32; 4] = [0.22, 0.22, 0.27, 1.0];
const BTN_BG_ACTIVE: [f32; 4] = [0.42, 0.30, 0.62, 1.0]; // Ableton-purple INV
const CURVE_BG: [f32; 4] = [0.20, 0.20, 0.25, 1.0];
const CURVE_BG_ACTIVE: [f32; 4] = [0.28, 0.34, 0.50, 1.0];
const TEXT_PRIMARY: [u8; 4] = [220, 220, 230, 255];
const TEXT_SECONDARY: [u8; 4] = [150, 150, 165, 255];

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

/// Which draggable element (if any) the pointer grabbed on press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragTarget {
    Min,
    Max,
    Scale,
    Offset,
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
    /// Which handle the cursor is hovering (for the highlight) when not
    /// dragging.
    hover: Option<DragTarget>,
    /// Actions accrued this frame; drained by the host after each event.
    pending_actions: Vec<PanelAction>,
}

impl MappingPopover {
    pub fn new() -> Self {
        Self {
            open: false,
            binding_id: String::new(),
            label: String::new(),
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
            hover: None,
            pending_actions: Vec::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
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
        anchor: Rect,
        clip: Rect,
    ) {
        self.binding_id = binding_id;
        self.label = label;
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
        if hi > lo {
            self.range_lo = lo;
            self.range_hi = hi;
        } else {
            self.range_lo = min.min(max);
            self.range_hi = (min.max(max)) + 1.0;
        }
        self.dragging = None;
        self.hover = None;

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
        self.hover = None;
    }

    // ── Geometry ────────────────────────────────────────────────────

    fn panel_height(&self) -> f32 {
        // header + range track + invert + curve + scale + offset rows, padded.
        PAD + HEADER_H
            + ROW_GAP + ROW_H        // range track
            + ROW_GAP + ROW_H        // invert
            + ROW_GAP + CURVE_ROW_H  // curve
            + ROW_GAP + ROW_H        // scale
            + ROW_GAP + ROW_H        // offset
            + PAD
    }

    fn panel_rect(&self) -> Rect {
        Rect::new(self.origin.0, self.origin.1, POPOVER_W, self.panel_height())
    }

    /// Y of the range-track row's top edge.
    fn track_row_y(&self) -> f32 {
        self.origin.1 + PAD + HEADER_H + ROW_GAP
    }

    /// The trim track rect (where the min/max handles live).
    fn track_rect(&self) -> Rect {
        let y = self.track_row_y() + (ROW_H - TRACK_H) * 0.5;
        Rect::new(self.origin.0 + PAD, y, POPOVER_W - 2.0 * PAD, TRACK_H)
    }

    /// Map a value in `[range_lo, range_hi]` to a normalized 0..1 across
    /// the track.
    fn value_to_norm(&self, v: f32) -> f32 {
        let span = (self.range_hi - self.range_lo).max(f32::EPSILON);
        ((v - self.range_lo) / span).clamp(0.0, 1.0)
    }

    /// Map a screen x on the track to a value in `[range_lo, range_hi]`.
    fn x_to_value(&self, sx: f32) -> f32 {
        let t = self.track_rect();
        let norm = ((sx - t.x) / t.w.max(f32::EPSILON)).clamp(0.0, 1.0);
        self.range_lo + norm * (self.range_hi - self.range_lo)
    }

    fn handle_center_x(&self, which: DragTarget) -> f32 {
        let t = self.track_rect();
        let v = match which {
            DragTarget::Min => self.cur_min,
            DragTarget::Max => self.cur_max,
            // Scale/Offset aren't track handles — never reached here.
            DragTarget::Scale | DragTarget::Offset => return t.x,
        };
        t.x + self.value_to_norm(v) * t.w
    }

    fn handle_rect(&self, which: DragTarget) -> Rect {
        let t = self.track_rect();
        let cx = self.handle_center_x(which);
        Rect::new(cx - HANDLE_W * 0.5, t.y - 2.0, HANDLE_W, t.h + 4.0)
    }

    fn invert_btn_rect(&self) -> Rect {
        let y = self.track_row_y() + ROW_H + ROW_GAP;
        // Right-aligned 36px INV button, label fills the rest.
        Rect::new(self.origin.0 + POPOVER_W - PAD - 36.0, y, 36.0, ROW_H)
    }

    fn curve_row_y(&self) -> f32 {
        self.track_row_y() + ROW_H + ROW_GAP + ROW_H + ROW_GAP
    }

    fn curve_cell_rect(&self, idx: usize) -> Rect {
        let n = CURVES.len() as f32;
        let avail = POPOVER_W - 2.0 * PAD;
        let gap = 2.0;
        let cell_w = (avail - gap * (n - 1.0)) / n;
        let x = self.origin.0 + PAD + idx as f32 * (cell_w + gap);
        Rect::new(x, self.curve_row_y(), cell_w, CURVE_ROW_H)
    }

    fn scale_row_y(&self) -> f32 {
        self.curve_row_y() + CURVE_ROW_H + ROW_GAP
    }

    fn offset_row_y(&self) -> f32 {
        self.scale_row_y() + ROW_H + ROW_GAP
    }

    /// Right-aligned draggable value box for the scale or offset field.
    /// The label sits to its left and fills the rest of the row.
    fn affine_value_rect(&self, which: DragTarget) -> Rect {
        let y = match which {
            DragTarget::Offset => self.offset_row_y(),
            _ => self.scale_row_y(),
        };
        Rect::new(self.origin.0 + POPOVER_W - PAD - 76.0, y, 76.0, ROW_H)
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

    /// Pointer press. Returns `true` if the popover consumed it. A press
    /// outside the panel returns `false` so the host can dismiss.
    pub fn on_press(&mut self, sx: f32, sy: f32) -> bool {
        if !self.open {
            return false;
        }
        if !Self::point_in(self.panel_rect(), sx, sy) {
            return false;
        }
        // Min/max handles — start a range drag. Snapshot the pre-drag
        // (min, max) first so the commit records a single undo entry.
        for which in [DragTarget::Min, DragTarget::Max] {
            // Widen the hit target a little so thin handles are grabbable.
            let r = self.handle_rect(which);
            let hit = Rect::new(r.x - 4.0, r.y, r.w + 8.0, r.h);
            if Self::point_in(hit, sx, sy) {
                self.pending_actions
                    .push(PanelAction::EffectMappingRangeSnapshot {
                        binding_id: self.binding_id.clone(),
                    });
                self.dragging = Some(which);
                return true;
            }
        }
        // Clicking the track body (not on a handle) moves the nearer
        // handle to the click, then drags it — matches the trim-bar feel.
        if Self::point_in(self.track_rect(), sx, sy) {
            let v = self.x_to_value(sx);
            let which = if (v - self.cur_min).abs() <= (v - self.cur_max).abs() {
                DragTarget::Min
            } else {
                DragTarget::Max
            };
            self.pending_actions
                .push(PanelAction::EffectMappingRangeSnapshot {
                    binding_id: self.binding_id.clone(),
                });
            self.dragging = Some(which);
            self.apply_drag_value(which, v);
            return true;
        }
        // INV toggle.
        if Self::point_in(self.invert_btn_rect(), sx, sy) {
            self.invert = !self.invert;
            self.pending_actions.push(PanelAction::EffectMappingInvert {
                binding_id: self.binding_id.clone(),
                invert: self.invert,
            });
            return true;
        }
        // Curve cells.
        for (idx, &c) in CURVES.iter().enumerate() {
            if Self::point_in(self.curve_cell_rect(idx), sx, sy) {
                if c != self.curve {
                    self.curve = c;
                    self.pending_actions.push(PanelAction::EffectMappingCurve {
                        binding_id: self.binding_id.clone(),
                        curve: c,
                    });
                }
                return true;
            }
        }
        // Scale / Offset scrub fields. Snapshot the pre-drag pair so the
        // commit records one undo entry; anchor the scrub on press-x and
        // the field's start value.
        for which in [DragTarget::Scale, DragTarget::Offset] {
            if Self::point_in(self.affine_value_rect(which), sx, sy) {
                self.pending_actions
                    .push(PanelAction::EffectMappingAffineSnapshot {
                        binding_id: self.binding_id.clone(),
                    });
                self.dragging = Some(which);
                self.scrub_press_x = sx;
                self.scrub_start = match which {
                    DragTarget::Scale => self.cur_scale,
                    _ => self.cur_offset,
                };
                return true;
            }
        }
        // Inside the panel but on dead space — consume so the click
        // doesn't fall through to the canvas behind it.
        true
    }

    /// Pointer move. Drives the live range drag (emits
    /// `EffectMappingRangeChanged`) or updates the handle hover.
    pub fn on_move(&mut self, sx: f32, sy: f32) {
        if !self.open {
            return;
        }
        if let Some(which) = self.dragging {
            match which {
                DragTarget::Min | DragTarget::Max => {
                    let v = self.x_to_value(sx);
                    self.apply_drag_value(which, v);
                }
                DragTarget::Scale | DragTarget::Offset => self.apply_scrub(which, sx),
            }
            return;
        }
        // Hover highlight when idle.
        self.hover = [DragTarget::Min, DragTarget::Max].into_iter().find(|&w| {
            let r = self.handle_rect(w);
            Self::point_in(Rect::new(r.x - 4.0, r.y, r.w + 8.0, r.h), sx, sy)
        });
    }

    /// Pointer release. Commits an in-progress drag into one undo command:
    /// `EffectMappingRangeCommit` for a min/max drag, or
    /// `EffectMappingAffineCommit` for a scale/offset scrub.
    pub fn on_release(&mut self) {
        if let Some(which) = self.dragging.take() {
            let action = match which {
                DragTarget::Min | DragTarget::Max => PanelAction::EffectMappingRangeCommit {
                    binding_id: self.binding_id.clone(),
                },
                DragTarget::Scale | DragTarget::Offset => {
                    PanelAction::EffectMappingAffineCommit {
                        binding_id: self.binding_id.clone(),
                    }
                }
            };
            self.pending_actions.push(action);
        }
    }

    /// Apply a dragged value to the grabbed handle, keeping min <= max,
    /// and emit the live `EffectMappingRangeChanged`.
    fn apply_drag_value(&mut self, which: DragTarget, v: f32) {
        let v = v.clamp(self.range_lo, self.range_hi);
        match which {
            DragTarget::Min => self.cur_min = v.min(self.cur_max),
            DragTarget::Max => self.cur_max = v.max(self.cur_min),
            // Only Min/Max reach here (on_move dispatches the rest).
            DragTarget::Scale | DragTarget::Offset => return,
        }
        self.pending_actions
            .push(PanelAction::EffectMappingRangeChanged {
                binding_id: self.binding_id.clone(),
                min: self.cur_min,
                max: self.cur_max,
            });
    }

    /// Scrub the scale or offset field relative to its press-anchor and
    /// emit the live `EffectMappingAffineChanged` with the current pair.
    /// Gain scales with the field's start magnitude (floored) so fine
    /// values nudge gently and large ones move fast.
    fn apply_scrub(&mut self, which: DragTarget, sx: f32) {
        let dpx = sx - self.scrub_press_x;
        let gain = SCRUB_K * self.scrub_start.abs().max(SCRUB_FLOOR);
        let new = self.scrub_start + dpx * gain;
        match which {
            DragTarget::Scale => self.cur_scale = new,
            DragTarget::Offset => self.cur_offset = new,
            _ => {}
        }
        self.pending_actions
            .push(PanelAction::EffectMappingAffineChanged {
                binding_id: self.binding_id.clone(),
                scale: self.cur_scale,
                offset: self.cur_offset,
            });
    }

    // ── Render ──────────────────────────────────────────────────────

    pub fn render(&self, ui: &mut UIRenderer) {
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

        // Header: the binding label (read-only — label editing deferred).
        ui.draw_text(
            panel.x + PAD,
            panel.y + PAD,
            &self.label,
            FONT,
            TEXT_PRIMARY,
        );

        // ── Range track with min/max handles ──
        let track = self.track_rect();
        ui.draw_rounded_rect(track.x, track.y, track.w, track.h, TRACK_BG, 2.0);
        let min_x = self.handle_center_x(DragTarget::Min);
        let max_x = self.handle_center_x(DragTarget::Max);
        let fill_x = min_x.min(max_x);
        let fill_w = (max_x - min_x).abs();
        if fill_w > 0.0 {
            ui.draw_rounded_rect(fill_x, track.y, fill_w, track.h, TRACK_FILL, 2.0);
        }
        for which in [DragTarget::Min, DragTarget::Max] {
            let r = self.handle_rect(which);
            let active = self.dragging == Some(which) || self.hover == Some(which);
            let color = if active { HANDLE_HOVER } else { HANDLE_COLOR };
            ui.draw_rounded_rect(r.x, r.y, r.w, r.h, color, 1.0);
        }
        // min/max value labels under the track ends.
        let val_y = track.y + track.h + 1.0;
        ui.draw_text(
            track.x,
            val_y,
            &format!("{:.2}", self.cur_min),
            FONT_SMALL,
            TEXT_SECONDARY,
        );
        let max_text = format!("{:.2}", self.cur_max);
        let max_text_w = max_text.chars().count() as f32 * FONT_SMALL * 0.55;
        ui.draw_text(
            track.x + track.w - max_text_w,
            val_y,
            &max_text,
            FONT_SMALL,
            TEXT_SECONDARY,
        );

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
        // The card→consumer remap (out = value * scale + offset). This is
        // where a folded affine_scalar node lives. Drag the value box to
        // scrub; precise values arrive from a fold.
        for (which, name, val) in [
            (DragTarget::Scale, "Scale", self.cur_scale),
            (DragTarget::Offset, "Offset", self.cur_offset),
        ] {
            let r = self.affine_value_rect(which);
            ui.draw_text(panel.x + PAD, r.y + 3.0, name, FONT, TEXT_SECONDARY);
            let active = self.dragging == Some(which);
            let bg = if active { CURVE_BG_ACTIVE } else { BTN_BG };
            ui.draw_rounded_rect(r.x, r.y, r.w, r.h, bg, 2.0);
            let txt = format_affine(val);
            let tw = txt.chars().count() as f32 * FONT_SMALL * 0.55;
            ui.draw_text(
                r.x + r.w - tw - 5.0,
                r.y + 3.0,
                &txt,
                FONT_SMALL,
                TEXT_PRIMARY,
            );
        }
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
    fn invert_click_emits_action() {
        let mut p = open_popover();
        let r = p.invert_btn_rect();
        let consumed = p.on_press(r.x + 2.0, r.y + 2.0);
        assert!(consumed);
        let actions = p.drain_actions();
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::EffectMappingInvert { invert: true, .. }]
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
            [PanelAction::EffectMappingCurve {
                curve: MacroCurve::Exponential,
                ..
            }]
        ));
        assert_eq!(p.curve, MacroCurve::Exponential);
    }

    #[test]
    fn range_drag_snapshots_changes_and_commits() {
        let mut p = open_popover();
        // Grab the max handle and drag it left.
        let r = p.handle_rect(DragTarget::Max);
        let consumed = p.on_press(r.x + r.w * 0.5, r.y + r.h * 0.5);
        assert!(consumed);
        // First action is the snapshot.
        let after_press = p.drain_actions();
        assert!(matches!(
            after_press.first(),
            Some(PanelAction::EffectMappingRangeSnapshot { .. })
        ));
        // Move toward the track start; min stays, max shrinks.
        let track = p.track_rect();
        p.on_move(track.x + track.w * 0.5, r.y);
        let changed = p.drain_actions();
        assert!(matches!(
            changed.last(),
            Some(PanelAction::EffectMappingRangeChanged { .. })
        ));
        assert!(p.cur_max < 0.8);
        assert!(p.cur_max >= p.cur_min);
        // Release commits.
        p.on_release();
        let commit = p.drain_actions();
        assert!(matches!(
            commit.as_slice(),
            [PanelAction::EffectMappingRangeCommit { .. }]
        ));
    }

    #[test]
    fn scale_scrub_snapshots_changes_and_commits() {
        let mut p = open_popover(); // cur_scale = 1.0
        let r = p.affine_value_rect(DragTarget::Scale);
        let consumed = p.on_press(r.x + r.w * 0.5, r.y + r.h * 0.5);
        assert!(consumed);
        // First action is the affine snapshot.
        let after_press = p.drain_actions();
        assert!(matches!(
            after_press.first(),
            Some(PanelAction::EffectMappingAffineSnapshot { .. })
        ));
        // Drag right → scale increases above its 1.0 start.
        p.on_move(r.x + r.w * 0.5 + 80.0, r.y);
        let changed = p.drain_actions();
        assert!(matches!(
            changed.last(),
            Some(PanelAction::EffectMappingAffineChanged { .. })
        ));
        assert!(p.cur_scale > 1.0, "drag right raises scale, got {}", p.cur_scale);
        // Offset untouched by a scale scrub.
        assert_eq!(p.cur_offset, 0.0);
        // Release commits the affine drag as one undo entry.
        p.on_release();
        let commit = p.drain_actions();
        assert!(matches!(
            commit.as_slice(),
            [PanelAction::EffectMappingAffineCommit { .. }]
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
    fn min_stays_below_max_when_dragged_past() {
        let mut p = open_popover();
        let r = p.handle_rect(DragTarget::Min);
        p.on_press(r.x + r.w * 0.5, r.y + r.h * 0.5);
        // Drag min way past max (to the far right of the track).
        let track = p.track_rect();
        p.on_move(track.x + track.w, r.y);
        assert!(p.cur_min <= p.cur_max);
    }
}
