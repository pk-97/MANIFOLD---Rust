use crate::color;
use crate::node::*;
use crate::tree::UITree;

// ── Layout constants ────────────────────────────────────────────────

pub const DEFAULT_LABEL_WIDTH: f32 = 80.0;
pub const VALUE_WIDTH: f32 = 44.0;
pub const GAP: f32 = 4.0;
pub const TRACK_RADIUS: f32 = 2.0;
const FILL_INSET: f32 = 1.0;
const THUMB_WIDTH: f32 = 8.0;
const THUMB_INSET: f32 = 1.0;

/// Identifies the nodes that make up a single slider instance.
/// Stored by the owning panel for event routing and value updates.
#[derive(Debug, Clone, Copy)]
pub struct SliderNodeIds {
    pub label: i32,            // -1 if no label
    pub track: u32,            // interactive — drag target
    pub fill: u32,             // non-interactive — subtle fill from left to value
    pub thumb: u32,            // non-interactive — thin vertical bar at value position
    pub value_text: u32,       // interactive — click to type
    pub track_rect: Rect,      // cached for x_to_normalized()
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
pub struct SliderColors {
    pub track: Color32,
    pub track_hover: Color32,
    pub track_pressed: Color32,
    pub fill: Color32,
    pub thumb: Color32,
    pub text: Color32,
}

impl SliderColors {
    /// Default slider colors from UIConstants.
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

    /// Envelope slider colors.
    pub fn envelope() -> Self {
        Self {
            track: color::ENV_TRACK_C32,
            track_hover: color::ENV_TRACK_HOVER_C32,
            track_pressed: color::ENV_TRACK_PRESSED_C32,
            fill: color::ENV_FILL_C32,
            thumb: color::ENV_THUMB_C32,
            text: color::SLIDER_TEXT_C32,
        }
    }
}

impl BitmapSlider {
    /// Build slider nodes into the tree. Returns node IDs for event routing.
    /// `rect` is the full bounding box for the entire slider row (label + track + value).
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        tree: &mut UITree,
        parent_id: i32,
        rect: Rect,
        label: Option<&str>,
        normalized_value: f32,
        value_text: &str,
        colors: &SliderColors,
        font_size: u16,
        label_width: f32,
    ) -> SliderNodeIds {
        let mut ids = SliderNodeIds {
            label: -1,
            track: 0,
            fill: 0,
            thumb: 0,
            value_text: 0,
            track_rect: Rect::ZERO,
            default_normalized: normalized_value,
        };

        let mut x = rect.x;
        let y = rect.y;
        let h = rect.height;

        // ── Label (fixed width, left) ──
        if let Some(label_text) = label {
            if !label_text.is_empty() {
                ids.label = tree.add_label(
                    parent_id,
                    x, y, label_width, h,
                    label_text,
                    UIStyle {
                        text_color: colors.text,
                        font_size,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
                ) as i32;
                x += label_width + GAP;
            }
        }

        // ── Value text (fixed width, right) ──
        let value_x = rect.x + rect.width - VALUE_WIDTH;
        ids.value_text = tree.add_button(
            parent_id,
            value_x, y, VALUE_WIDTH, h,
            UIStyle {
                bg_color: Color32::TRANSPARENT,
                hover_bg_color: color::HOVER_OVERLAY,
                pressed_bg_color: color::PRESS_OVERLAY,
                text_color: colors.text,
                font_size,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            value_text,
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
            ids.track as i32,
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
            ids.track as i32,
            thumb_rect,
            UINodeType::Panel,
            UIStyle {
                bg_color: colors.thumb,
                corner_radius: 1.0,
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
        if (ids.track as usize) >= tree.count() {
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
        let root = tree.add_panel(-1, 0.0, 0.0, 400.0, 20.0, UIStyle::default());

        let ids = BitmapSlider::build(
            &mut tree,
            root as i32,
            Rect::new(0.0, 0.0, 400.0, 20.0),
            Some("Opacity"),
            0.75,
            "0.75",
            &SliderColors::default_slider(),
            11,
            DEFAULT_LABEL_WIDTH,
        );

        assert!(ids.label >= 0);
        assert!(ids.track > 0);
        assert!(ids.fill > 0);
        assert!(ids.thumb > 0);
        assert!(ids.value_text > 0);
    }

    #[test]
    fn slider_without_label() {
        let mut tree = UITree::new();
        tree.add_panel(-1, 0.0, 0.0, 400.0, 20.0, UIStyle::default());

        let ids = BitmapSlider::build(
            &mut tree,
            0,
            Rect::new(0.0, 0.0, 400.0, 20.0),
            None,
            0.5,
            "0.50",
            &SliderColors::default_slider(),
            11,
            DEFAULT_LABEL_WIDTH,
        );

        assert_eq!(ids.label, -1);
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
        tree.add_panel(-1, 0.0, 0.0, 400.0, 20.0, UIStyle::default());

        let ids = BitmapSlider::build(
            &mut tree,
            0,
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
