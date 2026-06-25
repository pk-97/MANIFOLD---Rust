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

use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderNodeIds};
use crate::tree::UITree;

use super::param_slider_shared::config_btn_style;

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
    /// slider-drag path handles it via the returned [`SliderNodeIds`]).
    Slider {
        label: String,
        /// Normalized fill 0..1.
        norm: f32,
        /// Display text shown in the slider's value field.
        value_text: String,
        colors: SliderColors,
        /// Width reserved for the leading label.
        label_w: f32,
    },
    /// A status strip (see [`StatusStrip`]).
    Status(StatusStrip),
}

impl DrawerRow {
    /// The intrinsic height this row occupies. Buttons/Slider rows use the
    /// shared [`ROW_H`]; a status strip carries its own.
    fn height(&self) -> f32 {
        match self {
            DrawerRow::Status(s) => s.height,
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
    let container = tree.add_panel(
        parent,
        x,
        y,
        w,
        height,
        UIStyle {
            bg_color: crate::color::CONFIG_BG_C32,
            corner_radius: crate::color::SMALL_RADIUS,
            ..UIStyle::default()
        },
    );

    let mut button_ids: Vec<NodeId> = Vec::new();
    let mut sliders: Vec<SliderNodeIds> = Vec::new();
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
                        UIStyle {
                            text_color: Color32::new(150, 150, 160, 255),
                            font_size: spec.slider_font_size,
                            text_align: TextAlign::Left,
                            ..UIStyle::default()
                        },
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
                    let mut style = config_btn_style(b.active, spec.btn_font_size);
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
                value_text,
                colors,
                label_w,
            } => {
                let sx = x + PAD_H;
                let slider_w = w - PAD_H * 2.0;
                let ids = BitmapSlider::build(
                    tree,
                    Some(container),
                    Rect::new(sx, row_y, slider_w, ROW_H),
                    Some(label.as_str()),
                    norm.clamp(0.0, 1.0),
                    value_text.as_str(),
                    colors,
                    spec.slider_font_size,
                    *label_w,
                );
                sliders.push(ids);
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

    #[test]
    fn slider_row_yields_a_slider_not_a_button() {
        let spec = DrawerSpec {
            rows: vec![DrawerRow::Slider {
                label: "Decay".into(),
                norm: 0.25,
                value_text: "2.00".into(),
                colors: SliderColors::envelope(),
                label_w: 50.0,
            }],
            btn_font_size: 10,
            slider_font_size: 11,
        };
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 400.0, 200.0, UIStyle::default());
        let ids = build(&mut tree, Some(root), 0.0, 0.0, 240.0, &spec);

        assert_eq!(ids.button_count(), 0);
        assert_eq!(ids.sliders.len(), 1);
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
        let spec = DrawerSpec {
            rows: vec![uniform_buttons(&[
                (".", false), ("T", false), ("\u{E000}", false), ("\u{E001}", false),
                ("\u{E002}", false), ("\u{E003}", false), ("\u{E004}", false), ("Rev", false),
            ])],
            btn_font_size: 10,
            slider_font_size: 11,
        };
        let mut tree = UITree::new();
        let root = tree.add_panel(None, 0.0, 0.0, 400.0, 200.0, UIStyle::default());
        let ids = build(&mut tree, Some(root), 0.0, 0.0, 240.0, &spec);
        assert_eq!(ids.button_count(), 8);
        let w0 = tree.get_node(ids.button_ids[0]).bounds.width;
        let w7 = tree.get_node(ids.button_ids[7]).bounds.width;
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
        };
        let two = DrawerSpec {
            rows: vec![buttons(&[("a", false)]), buttons(&[("b", false)])],
            btn_font_size: 10,
            slider_font_size: 11,
        };
        assert!(two.height() > one.height());
        // Empty spec is zero height.
        let empty = DrawerSpec { rows: vec![], btn_font_size: 10, slider_font_size: 11 };
        assert_eq!(empty.height(), 0.0);
    }
}
