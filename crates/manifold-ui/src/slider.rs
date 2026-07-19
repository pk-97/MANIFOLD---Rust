use crate::color;
use crate::drag::DragController;
use crate::draw::{Painter, elide_to_width, text_width};
use crate::intent::{Gesture, IntentRegistry};
use crate::node::*;
use crate::panels::PanelAction;
use crate::tree::UITree;

// ── Layout constants ────────────────────────────────────────────────

pub const DEFAULT_LABEL_WIDTH: f32 = 60.0;
/// Width of the value box at the row's right end — a distinct cell (its own
/// rounded chip) showing the value, double-click-to-type (D13). Toggle/enum
/// controls that stand in for a value (e.g. a Clip Trigger ON/OFF) use this
/// same width so the right column stays aligned.
pub const VALUE_BOX_W: f32 = 56.0;
/// Padding between the slider track's right edge and the value box, so the value
/// reads as its own cell instead of being jammed against the fill.
pub const VALUE_GAP: f32 = 6.0;
pub const GAP: f32 = 4.0;

/// Label-column width that grows with the row, so widening a card gives the
/// param *name* more room instead of pouring every extra pixel into the track.
/// Floored at `DEFAULT_LABEL_WIDTH` (narrow timeline cards stay unchanged) and
/// capped so a very wide inspector doesn't starve the track. Right-aligned
/// labels overflow-left cleanly, so the wider cell only ever helps legibility.
pub const MAX_LABEL_WIDTH: f32 = 160.0;
pub fn label_width_for_row(row_w: f32) -> f32 {
    (row_w * 0.28).clamp(DEFAULT_LABEL_WIDTH, MAX_LABEL_WIDTH)
}
pub const TRACK_RADIUS: f32 = 6.0;
const FILL_INSET: f32 = 1.0;
/// The thumb is a slim WHITE trim flush at the fill's right end — one uniform
/// rounded bar `(======|)`, not a fat handle sitting in empty track. Narrow so
/// it reads as the fill's bright cap, not a competing element.
const THUMB_WIDTH: f32 = 4.0;
const THUMB_INSET: f32 = 1.0;

/// Horizontal span of a slider track: x + width, and NOTHING else. This is
/// the only track-layout data a panel may cache at build time. In-place
/// scroll (`ScrollContainer::offset_content`) shifts node y without
/// refreshing panel caches, and a cached y fed back into `set_bounds`
/// teleports overlay nodes to the pre-scroll row (BUG-257) — so y/height
/// are deliberately unrepresentable here (BUG-259). Anything that positions
/// nodes must read live bounds: `tree.get_bounds(ids.track)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackSpan {
    pub x: f32,
    pub width: f32,
}

impl TrackSpan {
    pub const ZERO: Self = Self { x: 0.0, width: 0.0 };

    /// The span of a full rect (use when you hold live bounds from the tree).
    pub fn of(rect: Rect) -> Self {
        Self { x: rect.x, width: rect.width }
    }
}

/// Identifies the nodes that make up a single slider instance.
/// Stored by the owning panel for event routing and value updates.
#[derive(Debug, Clone, Copy)]
pub struct SliderNodeIds {
    pub label: Option<NodeId>,   // None if no label
    pub track: NodeId,           // interactive — drag target
    pub fill: NodeId,            // non-interactive — subtle fill from left to value
    pub thumb: NodeId,           // non-interactive — thin vertical bar at value position
    pub value_text: NodeId,      // interactive — double-click to type (in the right gutter, D13)
    pub track_span: TrackSpan,   // cached x/width of the usable track (excludes value gutter);
                                 // scroll-invariant. For y/height read tree.get_bounds(track) —
                                 // never cache them (BUG-259).
    /// The slider's normalized default, for right-click reset.
    pub default_normalized: f32,
}

/// A built slider: its node ids plus the reset action to fire on a right-click
/// of its track. `build` returns this so ids and reset travel together into
/// panel storage — you cannot build a slider without stating its reset.
/// `reset` and `default_normalized` (on [`SliderNodeIds`]) both encode the
/// slider's default — `default_normalized` is the widget's own visual
/// default (0..1, used to snap the fill on reset), `reset` is the value-units
/// command the panel emits to actually write it back to the model. The mild
/// redundancy is intentional: one is geometry, the other is a `PanelAction`.
#[derive(Clone)]
pub struct Slider {
    pub ids: SliderNodeIds,
    pub reset: PanelAction,
}

impl Slider {
    /// Register this slider's contract-derived intents. `reset` is always
    /// translatable (P1); a label mapping action is optional — pass one via
    /// [`Slider::register_intents_with_mapping`] when this host has a mapping
    /// surface for the param (P3/D14). Delegates to
    /// [`BitmapSlider::register_track_reset`] so callers holding a `Slider`
    /// and callers holding a bare `(SliderNodeIds, PanelAction)` pair (the
    /// hand-registration sites, which never built a `Slider` struct) share
    /// the exact same contract walk.
    pub fn register_intents(&self, reg: &mut IntentRegistry) {
        BitmapSlider::register_track_reset(&self.ids, &self.reset, reg);
    }

    /// As [`Slider::register_intents`], plus the D14 build-time label-mapping
    /// twin ([`BitmapSlider::register_label_mapping`]) when this host has a
    /// mapping surface for the param.
    pub fn register_intents_with_mapping(&self, mapping: &PanelAction, reg: &mut IntentRegistry) {
        BitmapSlider::register_track_reset(&self.ids, &self.reset, reg);
        BitmapSlider::register_label_mapping(&self.ids, mapping, reg);
    }
}

// ── The gesture contract (UI_WIDGET_UNIFICATION_DESIGN.md §3) ──────────
//
// Zone geometry + gesture→intent mapping, in widget language. Hosts
// translate an intent into their own action type (chrome → `PanelAction` at
// build time via `register_intents`/`register_track_reset`; canvas →
// `GraphEditCommand` at input time in `graph_canvas/interaction.rs`). This
// is the ONE geometry/gesture source — `build`, `draw`, and the canvas
// hit-test all delegate to `zones()`/`intent_for()` rather than keeping
// private copies (D2, D7, I1, I3).

/// A slider's interactive zones, host-agnostic (no `NodeId`s — the canvas has
/// no tree nodes to name).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SliderZone {
    Label,
    Track,
    ValueCell,
}

/// What a gesture on a zone MEANS, in widget terms (D2). Hosts translate:
/// chrome → `PanelAction`, canvas → `GraphEditCommand`. A host may translate
/// an intent to nothing when its surface lacks the target (D3) — `EditValue`
/// on the canvas is an explicit dead stop until P5.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SliderIntent {
    /// Write the widget's default back through the value path, undoable as
    /// one drag (D4).
    ResetToDefault,
    /// Open the mapping/binding surface for this param.
    OpenMapping,
    /// Begin text entry on the value.
    EditValue,
}

/// Per-host widths driving zone geometry (D7 — the node face legitimately
/// uses wider label/value cells than the card, so the *numbers* are
/// per-host, never the *shape logic*). `gap`/`value_gap` are pre-scaled by
/// the caller exactly like `label_width`/`value_box_w` already are in
/// [`BitmapSlider::draw`] — `build` passes the raw module consts (scale 1),
/// `draw` passes its zoom-scaled locals. `label_width <= 0.0` means "no
/// label column" (mirrors `build`/`draw`'s own `label.is_some() &&
/// !is_empty()` gate — those callers zero it out when there's no label text
/// to draw, not just when the widget never carries a label at all).
#[derive(Clone, Copy)]
pub struct SliderMetrics {
    pub label_width: f32,
    pub value_box_w: f32,
    pub gap: f32,
    pub value_gap: f32,
}

/// Zone rects for a slider occupying `rect` — the one geometry source;
/// `build` and `draw` both delegate to it (P1 deleted their private copies).
#[derive(Clone, Copy, Debug)]
pub struct SliderZones {
    pub label: Option<Rect>,
    pub track: Rect,
    pub value_cell: Rect,
}

/// Stateless helper for building and updating bitmap slider widgets.
/// Composes 5 existing node types (Label, Button, Panel, Panel, Button).
///
/// Visual: `[Label]  [==fill==|thumb|......track......  Value]`
/// The value sits in a fixed gutter at the track's right; fill/thumb stop before
/// it, so they never collide with the number.
///
/// The owning panel manages all state, events, and undo. This struct only
/// builds nodes and provides math.
pub struct BitmapSlider;

/// Colors for a slider instance.
///
/// One theme drives every slider in the app — macros, effect params, generator
/// params. The value text now lives inside the track (its bg is `track`), so the
/// old per-context `value_bg` (which only differed by card background) is gone,
/// and with it the `default_slider`/`gen_param` split.
#[derive(Clone)]
pub struct SliderColors {
    pub track: Color32,
    pub track_hover: Color32,
    pub track_pressed: Color32,
    pub fill: Color32,
    pub thumb: Color32,
    pub text: Color32,
}

impl SliderColors {
    /// The unified slider theme. Every slider in the app renders through this —
    /// macros, effect params, generator params, and modulation drawers. Drawer
    /// context comes from the container's accent spine, not a recoloured slider.
    pub fn default_slider() -> Self {
        Self {
            track: color::SLIDER_TRACK_C32,
            track_hover: color::SLIDER_TRACK_HOVER_C32,
            track_pressed: color::SLIDER_TRACK_PRESSED_C32,
            fill: color::SLIDER_FILL_C32,
            thumb: color::SLIDER_THUMB_C32,
            text: color::SLIDER_TEXT_C32,
        }
    }
}

impl BitmapSlider {
    /// Zone rects for a slider occupying `rect` — see [`SliderZones`]. Pure,
    /// allocation-free (the canvas calls this per visible row per frame).
    pub fn zones(rect: Rect, metrics: &SliderMetrics) -> SliderZones {
        let y = rect.y;
        let h = rect.height;
        let (label, x) = if metrics.label_width > 0.0 {
            let r = Rect::new(rect.x, y, metrics.label_width, h);
            (Some(r), rect.x + metrics.label_width + metrics.gap)
        } else {
            (None, rect.x)
        };
        let value_box_x = rect.x + rect.width - metrics.value_box_w;
        let track_right = value_box_x - metrics.value_gap;
        let track_w = (track_right - x).max(1.0);
        SliderZones {
            label,
            track: Rect::new(x, y, track_w, h),
            value_cell: Rect::new(value_box_x, y, metrics.value_box_w, h),
        }
    }

    /// The gesture contract (D2/D5). Pure, total, allocation-free — covers
    /// discrete gestures only, drags stay host-stateful. Owns exactly three
    /// (zone, gesture) pairs (D13); every other pair is an explicit dead
    /// stop that hosts may freely bind to their own host-attached gestures
    /// (Label+Click OSC-copy on param cards/macros; Label+Click drawer-expand
    /// on audio-trigger rows — neither is a contract row, D13).
    ///
    /// Per-host translation (D15, recorded here so a future reader doesn't
    /// have to re-derive it from the call sites):
    /// - `param_card`/`gen-params`: all three intents live (Reset, OpenMapping,
    ///   EditValue).
    /// - `macros`: Reset + OpenMapping; EditValue → nothing (macros have no
    ///   type-in surface).
    /// - gain / master-chrome / layer-chrome / audio-trigger / other
    ///   chrome-spec sliders: Reset only; OpenMapping + EditValue → nothing
    ///   (no mapping popover or type-in surface on those hosts).
    pub fn intent_for(zone: SliderZone, g: Gesture) -> Option<SliderIntent> {
        use Gesture::*;
        use SliderZone::*;
        match (zone, g) {
            (Track, RightClick) => Some(SliderIntent::ResetToDefault),
            (Label, RightClick) => Some(SliderIntent::OpenMapping),
            // D13 correction (2026-07-13, P3): DoubleClick, not Click —
            // chrome's shipped type-in gesture (inspector.rs:2375 →
            // route_value_typein). P1's committed table said Click,
            // transcribing slider.rs's aspirational "click to type" comment
            // instead of the actual behavior; the canvas was and remains a
            // dead stop for this row until P5d, and chrome never consulted
            // this row (it derives EditValue at input time, D14), so the
            // flip changes nothing observable.
            (ValueCell, DoubleClick) => Some(SliderIntent::EditValue),
            _ => None,
        }
    }

    /// Register a slider's `Track + RightClick -> ResetToDefault` intent
    /// (P1's sole chrome translation; `OpenMapping`/`EditValue` derivation is
    /// P3) through the contract, replacing every hand-written
    /// `reg.on(ids.track, Gesture::RightClick, reset)` (I1). The single
    /// delegation point for the four hand-registration sites this design
    /// unifies: `chrome/diff.rs::register_slider_resets`,
    /// `layer_header.rs`'s gain slider, `param_card.rs::register_intents`
    /// (main rows + envelope decay + audio-shape drawer rows), and
    /// `audio_trigger_section.rs::register_intents`.
    pub fn register_track_reset(ids: &SliderNodeIds, reset: &PanelAction, reg: &mut IntentRegistry) {
        if let Some(SliderIntent::ResetToDefault) = Self::intent_for(SliderZone::Track, Gesture::RightClick) {
            reg.on(ids.track, Gesture::RightClick, reset.clone());
        }
    }

    /// Register a slider's `Label + RightClick -> OpenMapping` intent (P3,
    /// D14's build-time twin of `register_track_reset` — `OpenMapping`'s
    /// payload is constant at build time, unlike `EditValue`'s live-state
    /// payload). No-ops when `ids.label` is `None` (D15 — a host without a
    /// label can't translate a label gesture) or when a caller passes this
    /// for a host that has no mapping surface at all: callers simply don't
    /// call this fn for those hosts (gain / master-chrome / layer-chrome /
    /// audio-trigger — D15's dead-stop record on `intent_for`).
    pub fn register_label_mapping(ids: &SliderNodeIds, mapping: &PanelAction, reg: &mut IntentRegistry) {
        if let Some(label) = ids.label
            && let Some(SliderIntent::OpenMapping) = Self::intent_for(SliderZone::Label, Gesture::RightClick)
        {
            reg.on(label, Gesture::RightClick, mapping.clone());
        }
    }

    /// Build slider nodes into the tree. Returns node IDs for event routing.
    /// `rect` is the full bounding box for the entire slider row (label + track + value).
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        tree: &mut UITree,
        parent_id: Option<NodeId>,
        rect: Rect,
        label: Option<&str>,
        normalized_value: f32,
        value_text: &str,
        colors: &SliderColors,
        font_size: u16,
        label_width: f32,
        default_normalized: f32,
        reset: PanelAction,
    ) -> Slider {
        // track/fill/thumb/value_text are placeholders overwritten below before
        // any read; label stays None unless a label is actually built.
        let mut ids = SliderNodeIds {
            label: None,
            track: NodeId::PLACEHOLDER,
            fill: NodeId::PLACEHOLDER,
            thumb: NodeId::PLACEHOLDER,
            value_text: NodeId::PLACEHOLDER,
            track_span: TrackSpan::ZERO,
            default_normalized,
        };

        let y = rect.y;
        let h = rect.height;

        // ── Zones: the one geometry source (`zones()`), shared with `draw`
        //    and the graph-canvas hit-test. `label_width` zeroes out unless
        //    there's actual label text, matching the old inline gate. ──
        let has_label = label.is_some_and(|t| !t.is_empty());
        let metrics = SliderMetrics {
            label_width: if has_label { label_width } else { 0.0 },
            value_box_w: VALUE_BOX_W,
            gap: GAP,
            value_gap: VALUE_GAP,
        };
        let z = Self::zones(rect, &metrics);

        // ── Label (fixed width, right-aligned, interactive for right-click mapping) ──
        // Name right-aligns to the label cell so it hugs the slider track; tracks
        // all start at the same x, so a column of rows reads as an aligned grid
        // (Ableton/Resolve inspector style). Long names overflow left cleanly.
        if let Some(label_text) = label
            && !label_text.is_empty()
        {
            ids.label = Some(tree.add_node(
                parent_id,
                z.label.expect("has_label true implies zones() returns a label rect"),
                UINodeType::Label,
                UIStyle {
                    text_color: colors.text,
                    font_size,
                    text_align: TextAlign::Right,
                    ..UIStyle::default()
                },
                Some(label_text),
                UIFlags::VISIBLE | UIFlags::INTERACTIVE,
            ));
        }

        // ── Track (flexible width; the value lives in a fixed gutter at its
        //    right end, separated by VALUE_GAP, so the usable track stops short of
        //    it). The track node is the usable region — `track_rect` — so drag
        //    mapping, fill, and thumb all agree and never reach under the value. ──
        let track_rect = z.track;
        let value_box_x = z.value_cell.x;
        ids.track_span = TrackSpan::of(track_rect);

        ids.track = tree.add_node(
            parent_id,
            track_rect,
            UINodeType::Button,
            UIStyle {
                bg_color: colors.track,
                hover_bg_color: colors.track_hover,
                pressed_bg_color: colors.track_pressed,
                corner_radius: TRACK_RADIUS,
                ..UIStyle::default()
            },
            None,
            UIFlags::INTERACTIVE,
        );

        // ── Fill (child of track, non-interactive) ──
        let fill_w = compute_fill_width(track_rect.width, normalized_value, FILL_INSET);
        let fill_rect = Rect::new(
            track_rect.x + FILL_INSET,
            track_rect.y + FILL_INSET,
            fill_w,
            track_rect.height - FILL_INSET * 2.0,
        );
        ids.fill = tree.add_node(
            Some(ids.track),
            fill_rect,
            UINodeType::Panel,
            UIStyle {
                bg_color: colors.fill,
                corner_radius: (TRACK_RADIUS - FILL_INSET).max(0.0),
                ..UIStyle::default()
            },
            None,
            UIFlags::empty(),
        );

        // ── Thumb (child of track, non-interactive) ──
        let thumb_rect = compute_thumb_rect(track_rect, normalized_value, FILL_INSET, THUMB_WIDTH, THUMB_INSET);
        ids.thumb = tree.add_node(
            Some(ids.track),
            thumb_rect,
            UINodeType::Panel,
            UIStyle {
                bg_color: colors.thumb,
                corner_radius: THUMB_WIDTH * 0.5, // pill trim, matches the rounded fill
                ..UIStyle::default()
            },
            None,
            UIFlags::empty(),
        );

        // ── Value box (its own rounded cell, separated from the track by
        // VALUE_GAP, click-to-type). Centered in the cell. Opaque bg (track
        // colour) to clear stale glyphs during incremental atlas re-render.
        ids.value_text = tree.add_label(
            parent_id,
            value_box_x,
            y,
            VALUE_BOX_W,
            h,
            value_text,
            UIStyle {
                bg_color: colors.track,
                corner_radius: TRACK_RADIUS,
                text_color: colors.text,
                font_size,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        );
        // The value box is the `ValueCell` zone — interactive per this type's
        // own contract (`ids.value_text`: "interactive — double-click to
        // type"). Without the flag the hit-test skips it and every value-cell
        // gesture falls through to the full-row catcher behind it, which is
        // what made double-click type-in and enum click-to-change dead on the
        // real input path (BUG-250's root).
        tree.set_flag(ids.value_text, UIFlags::INTERACTIVE);

        Slider { ids, reset }
    }

    /// Update fill width, thumb position, and value text.
    /// Call during drag or when data changes.
    pub fn update_value(
        tree: &mut UITree,
        ids: &SliderNodeIds,
        normalized_value: f32,
        value_text: &str,
    ) {
        if ids.track.index() >= tree.count() {
            return;
        }

        let track_rect = tree.get_bounds(ids.track);

        // Fill
        let fill_w = compute_fill_width(track_rect.width, normalized_value, FILL_INSET);
        let fill_rect = Rect::new(
            track_rect.x + FILL_INSET,
            track_rect.y + FILL_INSET,
            fill_w,
            track_rect.height - FILL_INSET * 2.0,
        );
        tree.set_bounds(ids.fill, fill_rect);

        // Thumb
        tree.set_bounds(
            ids.thumb,
            compute_thumb_rect(track_rect, normalized_value, FILL_INSET, THUMB_WIDTH, THUMB_INSET),
        );

        // Value text
        tree.set_text(ids.value_text, value_text);
    }

    /// Immediate-mode twin of [`Self::build`] — draws the identical widget
    /// (track / fill / thumb / value cell, right-aligned label) through a
    /// [`Painter`] instead of building `UITree` nodes, for callers (the graph
    /// canvas) with no tree to build into. Shares `build`'s exact geometry math
    /// ([`compute_fill_width`], [`compute_thumb_rect`]) so the two renderers
    /// can't drift apart — `scale` (the canvas's zoom) multiplies every
    /// geometry constant; cards always call `build` and never this, so their
    /// look is untouched. Purely visual: no hover/pressed states, since the
    /// canvas has no per-row hover treatment to drive them.
    ///
    /// `label_width` and `value_box_w` are pre-scaled by the caller (same
    /// convention for both, unlike the internal geometry constants below,
    /// which `draw` scales itself since callers don't see them) — the graph
    /// canvas uses a wider `value_box_w` than the card's shared
    /// [`VALUE_BOX_W`] because a raw on-node param can be an unnormalized
    /// integer in the millions that the card never has to display.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        ui: &mut dyn Painter,
        rect: Rect,
        label: Option<&str>,
        normalized_value: f32,
        value_text: &str,
        colors: &SliderColors,
        font_size: f32,
        label_width: f32,
        value_box_w: f32,
        scale: f32,
    ) {
        let fill_inset = FILL_INSET * scale;
        let thumb_width = THUMB_WIDTH * scale;
        let thumb_inset = THUMB_INSET * scale;
        let track_radius = TRACK_RADIUS * scale;
        let value_gap = VALUE_GAP * scale;
        let gap = GAP * scale;

        let y = rect.y;
        let h = rect.height;

        // ── Zones: the one geometry source, shared with `build` and the
        //    graph-canvas hit-test (`zones()`). `label_width`/`value_box_w`
        //    are pre-scaled by the caller (unchanged convention); `gap`/
        //    `value_gap` are scaled here since callers never saw the raw
        //    consts. ──
        let has_label = label.is_some_and(|t| !t.is_empty());
        let metrics = SliderMetrics {
            label_width: if has_label { label_width } else { 0.0 },
            value_box_w,
            gap,
            value_gap,
        };
        let z = Self::zones(rect, &metrics);

        if let Some(label_text) = label
            && !label_text.is_empty()
        {
            let text = elide_to_width(label_text, font_size, label_width);
            let tw = text_width(&text, font_size);
            let label_rect = z.label.expect("has_label true implies zones() returns a label rect");
            let text_x = label_rect.x + (label_width - tw).max(0.0);
            let text_y = y + (h - font_size) * 0.5;
            ui.draw_text(text_x, text_y, &text, font_size, colors.text.to_array());
        }

        let value_box_x = z.value_cell.x;
        let track_rect = z.track;

        ui.draw_rounded_rect(
            track_rect.x, track_rect.y, track_rect.width, track_rect.height,
            colors.track, track_radius,
        );

        let fill_w = compute_fill_width(track_rect.width, normalized_value, fill_inset);
        if fill_w > 0.0 {
            ui.draw_rounded_rect(
                track_rect.x + fill_inset,
                track_rect.y + fill_inset,
                fill_w,
                track_rect.height - fill_inset * 2.0,
                colors.fill,
                (track_radius - fill_inset).max(0.0),
            );
        }

        let thumb_rect =
            compute_thumb_rect(track_rect, normalized_value, fill_inset, thumb_width, thumb_inset);
        ui.draw_rounded_rect(
            thumb_rect.x, thumb_rect.y, thumb_rect.width, thumb_rect.height,
            colors.thumb, thumb_width * 0.5,
        );

        ui.draw_rounded_rect(value_box_x, y, value_box_w, h, colors.track, track_radius);
        let vw = text_width(value_text, font_size);
        let vx = value_box_x + (value_box_w - vw) * 0.5;
        let vy = y + (h - font_size) * 0.5;
        ui.draw_text(vx, vy, value_text, font_size, colors.text.to_array());
    }

    // ── Math ────────────────────────────────────────────────────────

    /// Convert a panel-local X coordinate to a 0–1 normalized value
    /// relative to the track bounds.
    pub fn x_to_normalized(span: TrackSpan, pos_x: f32) -> f32 {
        if span.width <= 0.0 {
            return 0.0;
        }
        let t = (pos_x - span.x) / span.width;
        t.clamp(0.0, 1.0)
    }

    /// Convert a normalized 0–1 value to the actual parameter value.
    pub fn normalized_to_value(normalized: f32, min: f32, max: f32) -> f32 {
        min + normalized * (max - min)
    }

    /// Convert an actual parameter value to normalized 0–1.
    pub fn value_to_normalized(value: f32, min: f32, max: f32) -> f32 {
        let range = max - min;
        if range <= 0.0 {
            return 0.0;
        }
        ((value - min) / range).clamp(0.0, 1.0)
    }
}

// ── Slider drag state machine ────────────────────────────────────────
//
// Single source of truth for slider interaction. Every panel that has
// a draggable slider delegates to SliderDragState instead of managing
// its own dragging flag, cache, and sync logic. This eliminates the
// class of bugs where:
// - cache isn't updated during drag → sync_values snaps back
// - dragging flag isn't cleared on PointerUp → is_dragging() blocks rebuilds
// - visual isn't updated during pointer_down → one-frame delay
//
// Intentional divergence from Unity: Unity reimplements this pattern
// per-panel. We consolidate it because we're actively debugging it.
// See docs/KNOWN_DIVERGENCES.md.

/// Owns the drag state machine, value cache, and visual sync for one slider.
///
/// The grab→track→release lifecycle is delegated to the generic
/// [`DragController`]; this type adds the slider-specific interpretation
/// (absolute pos_x → value via the track rect) plus the value cache and visual
/// sync. The slider is the degenerate consumer — no per-drag payload (`()`),
/// absolute-position tracking — so it proves the controller's skeleton; the
/// timeline/canvas wrappers exercise the typed payload and delta.
#[derive(Debug, Clone)]
pub struct SliderDragState {
    ids: Option<SliderNodeIds>,
    cached_value: f32,
    drag: DragController<()>,
    pub min: f32,
    pub max: f32,
    pub whole_numbers: bool,
}

impl Default for SliderDragState {
    fn default() -> Self {
        Self {
            ids: None,
            cached_value: f32::NAN,
            drag: DragController::new(),
            min: 0.0,
            max: 1.0,
            whole_numbers: false,
        }
    }
}

impl SliderDragState {
    /// Create with explicit range.
    pub fn with_range(min: f32, max: f32, whole_numbers: bool) -> Self {
        Self {
            min,
            max,
            whole_numbers,
            ..Self::default()
        }
    }

    /// Store node IDs after build.
    pub fn set_ids(&mut self, ids: SliderNodeIds) {
        self.ids = Some(ids);
    }

    /// Clear node IDs (panel teardown / rebuild).
    pub fn clear(&mut self) {
        self.ids = None;
        self.drag.cancel();
        self.cached_value = f32::NAN;
    }

    /// Update range (e.g. when clip_chrome recalculates max_slip).
    pub fn set_range(&mut self, min: f32, max: f32, whole_numbers: bool) {
        self.min = min;
        self.max = max;
        self.whole_numbers = whole_numbers;
    }

    /// Node IDs (for panels that need the track span / node ids, etc.).
    pub fn ids(&self) -> Option<&SliderNodeIds> {
        self.ids.as_ref()
    }

    /// Track node ID for hit-testing.
    pub fn track_id(&self) -> Option<NodeId> {
        self.ids.as_ref().map(|ids| ids.track)
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_active()
    }
    pub fn cached_value(&self) -> f32 {
        self.cached_value
    }

    // ── Drag lifecycle ──────────────────────────────────────────

    /// Check if `node_id` is this slider's track. If so, begin drag,
    /// compute value from `pos_x`, update cache, and return the value.
    /// The caller emits Snapshot + Changed actions.
    pub fn try_start_drag(&mut self, node_id: NodeId, pos_x: f32) -> Option<f32> {
        let ids = self.ids.as_ref()?;
        if node_id != ids.track {
            return None;
        }
        self.drag.start((), Vec2::new(pos_x, 0.0));
        let norm = BitmapSlider::x_to_normalized(ids.track_span, pos_x);
        let val = BitmapSlider::normalized_to_value(norm, self.min, self.max);
        let val = if self.whole_numbers { val.round() } else { val };
        self.cached_value = val;
        Some(val)
    }

    /// Continue drag. Computes value, updates visual + cache.
    /// Returns `Some(value)` if currently dragging, `None` otherwise.
    /// `fmt` converts the actual value to display text.
    pub fn apply_drag(
        &mut self,
        pos_x: f32,
        tree: &mut UITree,
        fmt: &dyn Fn(f32) -> String,
    ) -> Option<f32> {
        if !self.drag.is_active() {
            return None;
        }
        self.drag.track(Vec2::new(pos_x, 0.0));
        let ids = self.ids.as_ref()?;
        let norm = BitmapSlider::x_to_normalized(ids.track_span, pos_x);
        let val = BitmapSlider::normalized_to_value(norm, self.min, self.max);
        let val = if self.whole_numbers { val.round() } else { val };
        let display_norm = BitmapSlider::value_to_normalized(val, self.min, self.max);
        BitmapSlider::update_value(tree, ids, display_norm, &fmt(val));
        self.cached_value = val;
        Some(val)
    }

    /// Continue drag with caller-computed value (for custom snapping etc.).
    /// `norm` is the display-normalized value, `val` is the actual value.
    pub fn apply_drag_custom(
        &mut self,
        val: f32,
        norm: f32,
        tree: &mut UITree,
        text: &str,
    ) -> bool {
        if !self.drag.is_active() {
            return false;
        }
        if let Some(ref ids) = self.ids {
            BitmapSlider::update_value(tree, ids, norm, text);
            self.cached_value = val;
            true
        } else {
            false
        }
    }

    /// Get raw normalized value from position (for callers that need custom
    /// value computation, e.g. snap_quarter_note).
    pub fn raw_norm(&self, pos_x: f32) -> f32 {
        self.ids
            .as_ref()
            .map(|ids| BitmapSlider::x_to_normalized(ids.track_span, pos_x))
            .unwrap_or(0.0)
    }

    /// End drag. Returns `true` if this slider was dragging (caller should
    /// emit Commit). Returns `false` if not dragging (no-op).
    pub fn end_drag(&mut self) -> bool {
        self.drag.release().is_some()
    }

    // ── Sync ────────────────────────────────────────────────────

    /// Sync from model value. Dirty-checks against cache. Updates visual
    /// only if value changed. `fmt` converts value to display text.
    pub fn sync(&mut self, tree: &mut UITree, value: f32, fmt: &dyn Fn(f32) -> String) {
        if (self.cached_value - value).abs() < f32::EPSILON && !self.cached_value.is_nan() {
            return;
        }
        self.cached_value = value;
        if let Some(ref ids) = self.ids {
            let norm = BitmapSlider::value_to_normalized(value, self.min, self.max);
            BitmapSlider::update_value(tree, ids, norm, &fmt(value));
        }
    }

    /// Sync with explicit normalized value (for sliders where norm != value,
    /// e.g. slip where value is seconds but norm is value/max_slip).
    pub fn sync_with_norm(&mut self, tree: &mut UITree, value: f32, norm: f32, text: &str) {
        if (self.cached_value - value).abs() < f32::EPSILON && !self.cached_value.is_nan() {
            return;
        }
        self.cached_value = value;
        if let Some(ref ids) = self.ids {
            BitmapSlider::update_value(tree, ids, norm, text);
        }
    }
}

// ── Internal ────────────────────────────────────────────────────────

/// `fill_inset` is a parameter (not the module `FILL_INSET` const directly) so
/// [`BitmapSlider::draw`] can call this with a zoom-scaled inset and get the
/// exact same math the tree-building `build()` path uses — one geometry
/// function, two renderers, can't drift apart.
fn compute_fill_width(track_width: f32, normalized_value: f32, fill_inset: f32) -> f32 {
    let usable = track_width - fill_inset * 2.0;
    if usable <= 0.0 {
        return 0.0;
    }
    (normalized_value * usable).clamp(0.0, usable)
}

/// See [`compute_fill_width`] on why `fill_inset`/`thumb_width`/`thumb_inset`
/// are parameters rather than the module consts.
fn compute_thumb_rect(
    track_rect: Rect,
    normalized_value: f32,
    fill_inset: f32,
    thumb_width: f32,
    thumb_inset: f32,
) -> Rect {
    let usable = track_rect.width - fill_inset * 2.0;
    // Right-align the trim to the fill's end: its right edge lands on the value
    // position, so the bright marker caps the fill instead of straddling the
    // boundary into empty track. `(======|)` reads as one bar + trim.
    let fill_right = track_rect.x + fill_inset + normalized_value * usable;
    let thumb_x = fill_right - thumb_width;
    let clamp_min = track_rect.x + fill_inset;
    let clamp_max = track_rect.x_max() - fill_inset - thumb_width;
    // Guard against tracks too narrow for the thumb
    let thumb_x = if clamp_min <= clamp_max {
        thumb_x.clamp(clamp_min, clamp_max)
    } else {
        clamp_min
    };
    Rect::new(
        thumb_x,
        track_rect.y + thumb_inset,
        thumb_width,
        track_rect.height - thumb_inset * 2.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholder_reset() -> PanelAction {
        PanelAction::slider_reset(
            PanelAction::MasterOpacitySnapshot,
            PanelAction::MasterOpacityChanged(1.0),
            PanelAction::MasterOpacityCommit,
        )
    }

    /// I2: pins the full contract table (§3/§4). Chrome and canvas both
    /// resolve `intent_for` — this is the one place the table itself is
    /// asserted, so a change here is a deliberate contract edit, not drift.
    #[test]
    fn intent_for_pins_the_full_contract_table() {
        use Gesture::*;
        use SliderZone::*;
        assert_eq!(BitmapSlider::intent_for(Track, RightClick), Some(SliderIntent::ResetToDefault));
        assert_eq!(BitmapSlider::intent_for(Label, RightClick), Some(SliderIntent::OpenMapping));
        // D13 correction (P3): DoubleClick, not Click — chrome's shipped
        // type-in gesture.
        assert_eq!(BitmapSlider::intent_for(ValueCell, DoubleClick), Some(SliderIntent::EditValue));
        // Every other (zone, gesture) pair is an explicit dead stop (D3),
        // including ValueCell+Click — chrome's value cell does NOT open on a
        // single click (that's the P1 mistake D13 corrects).
        for zone in [Label, Track, ValueCell] {
            for g in [Click, DoubleClick, RightClick] {
                let expected = matches!(
                    (zone, g),
                    (Track, RightClick) | (Label, RightClick) | (ValueCell, DoubleClick)
                );
                assert_eq!(BitmapSlider::intent_for(zone, g).is_some(), expected, "{zone:?} + {g:?}");
            }
        }
    }

    /// I3: zone geometry has one owner — `zones().track` must equal the
    /// track rect `build` actually materialises for identical inputs, so a
    /// future edit to either can't silently diverge from the other.
    #[test]
    fn zones_track_matches_build_track_rect() {
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 400.0, 20.0, UIStyle::default());
        let rect = Rect::new(0.0, 0.0, 400.0, 20.0);

        let ids = BitmapSlider::build(
            &mut tree,
            Some(root),
            rect,
            Some("Opacity"),
            0.75,
            "0.75",
            &SliderColors::default_slider(),
            11,
            DEFAULT_LABEL_WIDTH,
            0.75,
            placeholder_reset(),
        )
        .ids;

        let metrics = SliderMetrics {
            label_width: DEFAULT_LABEL_WIDTH,
            value_box_w: VALUE_BOX_W,
            gap: GAP,
            value_gap: VALUE_GAP,
        };
        let z = BitmapSlider::zones(rect, &metrics);
        assert_eq!(z.track.x, ids.track_span.x);
        assert_eq!(z.track.width, ids.track_span.width);
        assert_eq!(z.label.map(|l| l.x), Some(0.0));
    }

    /// I1/register_track_reset: the contract's chrome translation IS the
    /// slider's own declared `reset` action, registered on the track node —
    /// what every hand site used to write inline.
    #[test]
    fn register_track_reset_registers_the_declared_reset_on_track() {
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 400.0, 20.0, UIStyle::default());
        let reset = placeholder_reset();
        let slider = BitmapSlider::build(
            &mut tree,
            Some(root),
            Rect::new(0.0, 0.0, 400.0, 20.0),
            Some("Opacity"),
            0.75,
            "0.75",
            &SliderColors::default_slider(),
            11,
            DEFAULT_LABEL_WIDTH,
            0.75,
            reset.clone(),
        );

        let mut reg = IntentRegistry::new();
        slider.register_intents(&mut reg);
        let resolved = reg.resolve(&tree, Some(slider.ids.track), Gesture::RightClick);
        // PanelAction carries no PartialEq; SliderReset is the marker of
        // interest here (it's what `register_intents` should have replayed
        // — the same `reset` this slider was built with, above).
        assert!(matches!(resolved, Some(PanelAction::SliderReset { .. })));
    }

    /// P3/D14: `register_label_mapping` is the label's build-time contract
    /// translation — registers the given mapping action on `ids.label` when
    /// present, no-ops when the slider has no label.
    #[test]
    fn register_label_mapping_registers_on_label_when_present() {
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 400.0, 20.0, UIStyle::default());
        let ids = BitmapSlider::build(
            &mut tree,
            Some(root),
            Rect::new(0.0, 0.0, 400.0, 20.0),
            Some("Opacity"),
            0.75,
            "0.75",
            &SliderColors::default_slider(),
            11,
            DEFAULT_LABEL_WIDTH,
            0.75,
            placeholder_reset(),
        )
        .ids;
        assert!(ids.label.is_some());

        let mapping = PanelAction::MacroLabelRightClick(0);
        let mut reg = IntentRegistry::new();
        BitmapSlider::register_label_mapping(&ids, &mapping, &mut reg);
        let resolved = reg.resolve(&tree, ids.label, Gesture::RightClick);
        assert!(matches!(resolved, Some(PanelAction::MacroLabelRightClick(0))));
    }

    /// P3/D15: a labelless slider has nothing to register the mapping on —
    /// `register_label_mapping` no-ops rather than panicking or registering
    /// on some other node.
    #[test]
    fn register_label_mapping_noops_without_a_label() {
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 400.0, 20.0, UIStyle::default());
        let ids = BitmapSlider::build(
            &mut tree,
            Some(root),
            Rect::new(0.0, 0.0, 400.0, 20.0),
            None,
            0.75,
            "0.75",
            &SliderColors::default_slider(),
            11,
            DEFAULT_LABEL_WIDTH,
            0.75,
            placeholder_reset(),
        )
        .ids;
        assert!(ids.label.is_none());

        let mapping = PanelAction::MacroLabelRightClick(0);
        let mut reg = IntentRegistry::new();
        // Must not panic; nothing to assert-resolve since there's no label
        // node to register on.
        BitmapSlider::register_label_mapping(&ids, &mapping, &mut reg);
    }

    #[test]
    fn build_slider() {
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 400.0, 20.0, UIStyle::default());

        let ids = BitmapSlider::build(
            &mut tree,
            Some(root),
            Rect::new(0.0, 0.0, 400.0, 20.0),
            Some("Opacity"),
            0.75,
            "0.75",
            &SliderColors::default_slider(),
            11,
            DEFAULT_LABEL_WIDTH,
            0.75,
            placeholder_reset(),
        )
        .ids;

        assert!(ids.label.is_some());
        assert!(ids.track != NodeId::PLACEHOLDER);
        assert!(ids.fill != NodeId::PLACEHOLDER);
        assert!(ids.thumb != NodeId::PLACEHOLDER);
        assert!(ids.value_text != NodeId::PLACEHOLDER);
    }

    #[test]
    fn slider_without_label() {
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 400.0, 20.0, UIStyle::default());

        let ids = BitmapSlider::build(
            &mut tree,
            Some(root),
            Rect::new(0.0, 0.0, 400.0, 20.0),
            None,
            0.5,
            "0.50",
            &SliderColors::default_slider(),
            11,
            DEFAULT_LABEL_WIDTH,
            0.5,
            placeholder_reset(),
        )
        .ids;

        assert_eq!(ids.label, None);
    }

    #[test]
    fn build_stores_the_declared_default_not_the_initial_value() {
        // The slider's initial value and its right-click-reset default are
        // independent — a slider can be built showing a non-default live value
        // (e.g. 0.9) while its reset target stays at the param's real default
        // (e.g. 0.5). `default_normalized` must reflect the latter (BUG-061).
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 400.0, 20.0, UIStyle::default());

        let ids = BitmapSlider::build(
            &mut tree,
            Some(root),
            Rect::new(0.0, 0.0, 400.0, 20.0),
            Some("Amount"),
            0.9,
            "0.90",
            &SliderColors::default_slider(),
            11,
            DEFAULT_LABEL_WIDTH,
            0.5,
            placeholder_reset(),
        )
        .ids;

        assert!((ids.default_normalized - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn x_to_normalized_edges() {
        let track = TrackSpan::of(Rect::new(100.0, 0.0, 200.0, 20.0));
        assert_eq!(BitmapSlider::x_to_normalized(track, 100.0), 0.0);
        assert_eq!(BitmapSlider::x_to_normalized(track, 300.0), 1.0);
        assert!((BitmapSlider::x_to_normalized(track, 200.0) - 0.5).abs() < 0.01);
        // Clamped
        assert_eq!(BitmapSlider::x_to_normalized(track, 50.0), 0.0);
        assert_eq!(BitmapSlider::x_to_normalized(track, 400.0), 1.0);
    }

    #[test]
    fn value_conversions() {
        let norm = BitmapSlider::value_to_normalized(50.0, 0.0, 100.0);
        assert!((norm - 0.5).abs() < 0.01);

        let val = BitmapSlider::normalized_to_value(0.75, 0.0, 100.0);
        assert!((val - 75.0).abs() < 0.01);
    }

    #[test]
    fn update_value() {
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 400.0, 20.0, UIStyle::default());

        let ids = BitmapSlider::build(
            &mut tree,
            Some(root),
            Rect::new(0.0, 0.0, 400.0, 20.0),
            Some("Test"),
            0.5,
            "0.50",
            &SliderColors::default_slider(),
            11,
            DEFAULT_LABEL_WIDTH,
            0.5,
            placeholder_reset(),
        )
        .ids;

        tree.clear_dirty();
        BitmapSlider::update_value(&mut tree, &ids, 0.25, "0.25");
        assert!(tree.has_dirty());
        assert_eq!(tree.get_node(ids.value_text).unwrap().text.as_deref(), Some("0.25"));
    }
}
