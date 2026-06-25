use crate::color;
use crate::drag::DragController;
use crate::node::*;
use crate::tree::UITree;

// ── Layout constants ────────────────────────────────────────────────

pub const DEFAULT_LABEL_WIDTH: f32 = 60.0;
pub const VALUE_WIDTH: f32 = 60.0;
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
pub const TRACK_RADIUS: f32 = 2.0;
const FILL_INSET: f32 = 1.0;
const THUMB_WIDTH: f32 = 8.0;
const THUMB_INSET: f32 = 1.0;

/// Identifies the nodes that make up a single slider instance.
/// Stored by the owning panel for event routing and value updates.
#[derive(Debug, Clone, Copy)]
pub struct SliderNodeIds {
    pub label: Option<NodeId>,   // None if no label
    pub track: NodeId,           // interactive — drag target
    pub fill: NodeId,            // non-interactive — subtle fill from left to value
    pub thumb: NodeId,           // non-interactive — thin vertical bar at value position
    pub value_text: NodeId,      // interactive — click to type
    pub track_rect: Rect,        // cached for x_to_normalized()
    pub default_normalized: f32, // for right-click reset
}

/// Stateless helper for building and updating bitmap slider widgets.
/// Composes 5 existing node types (Label, Button, Panel, Panel, Button).
///
/// Visual: `[Label]  [====fill====|thumb|.........track.........] [Value]`
///
/// The owning panel manages all state, events, and undo. This struct only
/// builds nodes and provides math.
pub struct BitmapSlider;

/// Colors for a slider instance.
#[derive(Clone)]
pub struct SliderColors {
    pub track: Color32,
    pub track_hover: Color32,
    pub track_pressed: Color32,
    pub fill: Color32,
    pub thumb: Color32,
    pub text: Color32,
    /// Background for the value text label. Must be opaque to clear old text
    /// during incremental atlas re-rendering (LoadAction::Load preserves the
    /// previous frame's glyphs — a transparent bg leaves stale text visible).
    pub value_bg: Color32,
}

impl SliderColors {
    /// Default slider colors (effect cards).
    pub fn default_slider() -> Self {
        Self {
            track: color::SLIDER_TRACK_C32,
            track_hover: color::SLIDER_TRACK_HOVER_C32,
            track_pressed: color::SLIDER_TRACK_PRESSED_C32,
            fill: color::SLIDER_FILL_C32,
            thumb: color::SLIDER_THUMB_C32,
            text: color::SLIDER_TEXT_C32,
            value_bg: color::EFFECT_CARD_INNER_BG_C32,
        }
    }

    /// Generator param slider colors (uses gen card background).
    pub fn gen_param() -> Self {
        Self {
            track: color::SLIDER_TRACK_C32,
            track_hover: color::SLIDER_TRACK_HOVER_C32,
            track_pressed: color::SLIDER_TRACK_PRESSED_C32,
            fill: color::SLIDER_FILL_C32,
            thumb: color::SLIDER_THUMB_C32,
            text: color::SLIDER_TEXT_C32,
            value_bg: color::GEN_CARD_INNER_BG_C32,
        }
    }

    /// Envelope slider colors.
    pub fn envelope() -> Self {
        Self {
            track: color::ENV_TRACK_C32,
            track_hover: color::ENV_TRACK_HOVER_C32,
            track_pressed: color::ENV_TRACK_PRESSED_C32,
            fill: color::ENV_FILL_C32,
            thumb: color::ENV_THUMB_C32,
            text: color::SLIDER_TEXT_C32,
            value_bg: color::EFFECT_CARD_INNER_BG_C32,
        }
    }
}

impl BitmapSlider {
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
    ) -> SliderNodeIds {
        // track/fill/thumb/value_text are placeholders overwritten below before
        // any read; label stays None unless a label is actually built.
        let mut ids = SliderNodeIds {
            label: None,
            track: NodeId::PLACEHOLDER,
            fill: NodeId::PLACEHOLDER,
            thumb: NodeId::PLACEHOLDER,
            value_text: NodeId::PLACEHOLDER,
            track_rect: Rect::ZERO,
            default_normalized: normalized_value,
        };

        let mut x = rect.x;
        let y = rect.y;
        let h = rect.height;

        // ── Label (fixed width, left-aligned, interactive for right-click mapping) ──
        // Name sits at the row's left edge; tracks all start at the same x, so a
        // column of rows reads as an aligned grid (Ableton/Resolve inspector
        // style). The value cell stays right-aligned like a mixer column.
        if let Some(label_text) = label
            && !label_text.is_empty()
        {
            ids.label = Some(tree.add_node(
                parent_id,
                Rect::new(x, y, label_width, h),
                UINodeType::Label,
                UIStyle {
                    text_color: colors.text,
                    font_size,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
                Some(label_text),
                UIFlags::VISIBLE | UIFlags::INTERACTIVE,
            ));
            x += label_width + GAP;
        }

        // ── Value text (fixed width, right) ──
        let value_x = rect.x + rect.width - VALUE_WIDTH;
        ids.value_text = tree.add_label(
            parent_id,
            value_x,
            y,
            VALUE_WIDTH,
            h,
            value_text,
            UIStyle {
                bg_color: colors.value_bg,
                text_color: colors.text,
                font_size,
                // Right-aligned so a stacked column of values lines up at the
                // decimal edge and reads like a mixer, instead of each value
                // floating centered in its cell.
                text_align: TextAlign::Right,
                ..UIStyle::default()
            },
        );

        // ── Track (flexible width, between label and value) ──
        let track_w = (value_x - GAP - x).max(1.0);
        let track_rect = Rect::new(x, y, track_w, h);
        ids.track_rect = track_rect;

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
        let fill_w = compute_fill_width(track_w, normalized_value);
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
        let thumb_rect = compute_thumb_rect(track_rect, normalized_value);
        ids.thumb = tree.add_node(
            Some(ids.track),
            thumb_rect,
            UINodeType::Panel,
            UIStyle {
                bg_color: colors.thumb,
                corner_radius: color::HAIRLINE_RADIUS,
                ..UIStyle::default()
            },
            None,
            UIFlags::empty(),
        );

        ids
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
        let fill_w = compute_fill_width(track_rect.width, normalized_value);
        let fill_rect = Rect::new(
            track_rect.x + FILL_INSET,
            track_rect.y + FILL_INSET,
            fill_w,
            track_rect.height - FILL_INSET * 2.0,
        );
        tree.set_bounds(ids.fill, fill_rect);

        // Thumb
        tree.set_bounds(ids.thumb, compute_thumb_rect(track_rect, normalized_value));

        // Value text
        tree.set_text(ids.value_text, value_text);
    }

    // ── Math ────────────────────────────────────────────────────────

    /// Convert a panel-local X coordinate to a 0–1 normalized value
    /// relative to the track bounds.
    pub fn x_to_normalized(track_rect: Rect, local_x: f32) -> f32 {
        if track_rect.width <= 0.0 {
            return 0.0;
        }
        let t = (local_x - track_rect.x) / track_rect.width;
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

    /// Node IDs (for panels that need to read track_rect, etc.).
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
        let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos_x);
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
        let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos_x);
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
            .map(|ids| BitmapSlider::x_to_normalized(ids.track_rect, pos_x))
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

fn compute_fill_width(track_width: f32, normalized_value: f32) -> f32 {
    let usable = track_width - FILL_INSET * 2.0;
    if usable <= 0.0 {
        return 0.0;
    }
    (normalized_value * usable).clamp(0.0, usable)
}

fn compute_thumb_rect(track_rect: Rect, normalized_value: f32) -> Rect {
    let usable = track_rect.width - FILL_INSET * 2.0;
    let thumb_x = track_rect.x + FILL_INSET + normalized_value * usable - THUMB_WIDTH * 0.5;
    let clamp_min = track_rect.x + FILL_INSET;
    let clamp_max = track_rect.x_max() - FILL_INSET - THUMB_WIDTH;
    // Guard against tracks too narrow for the thumb
    let thumb_x = if clamp_min <= clamp_max {
        thumb_x.clamp(clamp_min, clamp_max)
    } else {
        clamp_min
    };
    Rect::new(
        thumb_x,
        track_rect.y + THUMB_INSET,
        THUMB_WIDTH,
        track_rect.height - THUMB_INSET * 2.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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
        );

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
        );

        assert_eq!(ids.label, None);
    }

    #[test]
    fn x_to_normalized_edges() {
        let track = Rect::new(100.0, 0.0, 200.0, 20.0);
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
        );

        tree.clear_dirty();
        BitmapSlider::update_value(&mut tree, &ids, 0.25, "0.25");
        assert!(tree.has_dirty());
        assert_eq!(tree.get_node(ids.value_text).text.as_deref(), Some("0.25"));
    }
}
