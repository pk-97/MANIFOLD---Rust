//! [`GraphEditorPanel`] — the graph-editor window's **node-output inspector**.
//!
//! Post on-node-params migration (`docs/GRAPH_EDITOR_REDESIGN.md` Phase 6), this
//! panel no longer authors parameters — every param control (scrub, expose,
//! enum / bool / trigger, colour / vec, string / path / table / WGSL, and the
//! wire / outer driver hints) now lives **on the node face in the canvas**. What
//! remains here are the two jobs tied to the node-output *preview*, not to
//! authoring:
//!
//! 1. **Value inspector** — for a previewed node that emits no image (a control /
//!    math / envelope node), [`render_node_inspector`](GraphEditorPanel::render_node_inspector)
//!    fills the node-output pane with the node's title, one-line description, and
//!    its live OUTPUT / INPUT scalar values in place of the (absent) image.
//! 2. **Smart-preview toggle** — [`render_smart_preview_toggle`](GraphEditorPanel::render_smart_preview_toggle)
//!    draws the auto-gain checkbox next to the node-output monitor so dark /
//!    signed intermediates are legible. Preview-only; never touches the render.
//!
//! Both are set each frame by the host (`set_node_inspector` /
//! `set_node_preview_normalize`) and drawn into the LEFT preview column. Discrete
//! clicks (the toggle) fold through the host's shared `IntentRegistry`, same as
//! every other chrome panel.

use crate::color;
use crate::node::*;
use crate::tree::UITree;

/// Preview-sidebar DEFAULT width inside the graph-editor window — docks on the
/// LEFT. The live width is now owned by the editor workspace's [`manifold_ui::Dock`]
/// (the user can drag the divider); this constant is just the seed. The sidebar
/// is monitors-only, so the width sizes the two stacked 16:9 preview panes —
/// wider means bigger Node Output / Master Out monitors. Single source of the
/// number: [`crate::dock`].
pub const SIDEBAR_WIDTH: f32 = crate::dock::EDITOR_LEFT_DEFAULT;

/// Card-lane DEFAULT width inside the graph-editor window — docks on the RIGHT
/// (same side as the main timeline's inspector). The lane renders the real
/// `ParamCardPanel` for the edited effect/generator. The live width is now owned
/// by the editor workspace's [`manifold_ui::Dock`]; render and input both read
/// `dock.rects(area)`. Single source of the number: [`crate::dock`].
pub const EDITOR_CARD_LANE_WIDTH: f32 = crate::dock::EDITOR_RIGHT_DEFAULT;

const PADDING: f32 = 12.0;
const HEADER_H: f32 = 32.0;
const CHECKBOX_W: f32 = 22.0;
const CHECKBOX_H: f32 = 22.0;
const CHECKBOX_GAP: f32 = 10.0;
const FONT_SIZE: u16 = 12;
const HEADER_FONT_SIZE: u16 = 14;

/// Value inspector for a previewed node that carries no image: its title,
/// one-line "what it does", and the live scalar I/O for this frame. The host
/// builds it from the descriptor + the node-preview info and hands it over via
/// [`GraphEditorPanel::set_node_inspector`]; it renders into the node-output
/// pane in place of the (absent) image.
#[derive(Debug, Clone)]
pub struct NodeInspector {
    /// Node display title.
    pub title: String,
    /// One-line "what it does" (the descriptor summary, else purpose). May be
    /// empty if the node carries no descriptor text.
    pub description: String,
    /// Live scalar input port values `(port_name, value)` this frame.
    pub inputs: Vec<(String, f32)>,
    /// Live scalar output port values — the signal the node is producing.
    pub outputs: Vec<(String, f32)>,
}

/// The graph-editor window's node-output inspector (see the module docs). Holds
/// only the two preview-side concerns; all param authoring moved onto the node
/// face in the canvas.
#[derive(Debug, Default)]
pub struct GraphEditorPanel {
    /// Whether the node-output preview pane is applying auto-gain / normalization.
    /// Mirrors the app-side state (pushed each frame via
    /// [`Self::set_node_preview_normalize`]); drives the toggle's checkmark.
    normalize_preview: bool,
    /// Value inspector for a previewed non-image node, or `None` when the preview
    /// is an image (or nothing is previewed). Set each frame by the host.
    node_inspector: Option<NodeInspector>,
}

impl GraphEditorPanel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set whether the node-output preview pane shows auto-gain / normalization,
    /// so the toggle renders the right checkmark. Mirrors the app-side state;
    /// called each frame before the render pass.
    pub fn set_node_preview_normalize(&mut self, on: bool) {
        self.normalize_preview = on;
    }

    /// Set (or clear) the value inspector shown when a non-image node is
    /// previewed. Called each frame by the host; `None` when the preview is an
    /// image (or nothing is selected).
    pub fn set_node_inspector(&mut self, inspector: Option<NodeInspector>) {
        self.node_inspector = inspector;
    }

    /// Draw the "Smart preview" auto-gain toggle into `region` (a row beside /
    /// above the node-output monitor). Returns the checkbox button's tree id so
    /// the host can register its `SetNodePreviewNormalize` click intent through
    /// the shared registry. Only meaningful when a node-output IMAGE is on screen
    /// (a value-inspector node has no image to normalize), so the host gates the
    /// call on `has_image`.
    pub fn render_smart_preview_toggle(&self, tree: &mut UITree, region: Rect) -> NodeId {
        let cb_y = region.y + (region.height - CHECKBOX_H) * 0.5;
        let cb_id = tree.add_button(
            None,
            region.x,
            cb_y,
            CHECKBOX_W,
            CHECKBOX_H,
            checkbox_style(self.normalize_preview, true),
            if self.normalize_preview { "✓" } else { "" },
        );
        // Naming pass (UI_AUTOMATION_DESIGN.md D8/§3): graph-editor chrome.
        tree.set_name(cb_id, "graph_editor.smart_preview_toggle");
        let label_x = region.x + CHECKBOX_W + CHECKBOX_GAP;
        tree.add_label(
            None,
            label_x,
            region.y,
            (region.x + region.width - label_x).max(10.0),
            region.height,
            "Smart preview",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        cb_id
    }

    /// Whether the smart-preview toggle should be drawn this frame: only when a
    /// node-output *image* is on screen (a non-image node fills the pane with the
    /// value inspector, which has no gain to normalize).
    pub fn wants_smart_preview_toggle(&self) -> bool {
        self.node_inspector.is_none()
    }

    /// Render the value inspector for the previewed node into the pinned
    /// node-output pane (`region`) when the node carries no image — its title,
    /// one-line "what it does", then the live OUTPUT signal and INPUT values.
    /// Returns `true` when it drew (a non-image node is selected), so the host
    /// knows the pane is occupied by text rather than an image and skips the
    /// generic "Node Output" title. No row state — nothing here is clickable.
    pub fn render_node_inspector(&self, tree: &mut UITree, region: Rect) -> bool {
        let Some(insp) = self.node_inspector.as_ref() else {
            return false;
        };
        Self::render_inspector_block(tree, None, region, insp);
        true
    }

    /// Draw the inspector block — title, description, OUTPUT/INPUT rows — into
    /// `region`, parented at `parent_id`. Coordinates are absolute; `region.x`
    /// is the left edge of the text (already padded by the caller).
    fn render_inspector_block(
        tree: &mut UITree,
        parent_id: Option<NodeId>,
        region: Rect,
        insp: &NodeInspector,
    ) {
        const DESC_LINE_H: f32 = 16.0;
        const IO_ROW_H: f32 = 20.0;
        let x = region.x;
        let w = region.width.max(10.0);
        let bg_id = parent_id;
        let mut y = region.y;

        // Title.
        tree.add_label(
            bg_id,
            x,
            y,
            w,
            HEADER_H - PADDING,
            &insp.title,
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: HEADER_FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        y += HEADER_H;

        // "What it does", wrapped, capped at 4 lines.
        if !insp.description.is_empty() {
            let max_chars = ((w / 6.5).floor() as usize).max(8);
            for line in crate::graph_canvas::wrap_text(&insp.description, max_chars)
                .into_iter()
                .take(4)
            {
                tree.add_label(
                    bg_id,
                    x,
                    y,
                    w,
                    DESC_LINE_H,
                    &line,
                    UIStyle {
                        text_color: color::TEXT_DIMMED_C32,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
                );
                y += DESC_LINE_H;
            }
        }
        y += PADDING * 0.5;

        // OUTPUT then INPUT value sections. Output first — it's the live signal.
        for (heading, rows) in [("OUTPUT", &insp.outputs), ("INPUT", &insp.inputs)] {
            if rows.is_empty() {
                continue;
            }
            tree.add_label(
                bg_id,
                x,
                y,
                w,
                IO_ROW_H,
                heading,
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            y += IO_ROW_H;
            for (name, value) in rows {
                let value_w = (w * 0.4).max(50.0);
                let name_w = (w - value_w).max(10.0);
                tree.add_label(
                    bg_id,
                    x,
                    y,
                    name_w,
                    IO_ROW_H,
                    name,
                    UIStyle {
                        text_color: color::TEXT_WHITE_C32,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
                );
                tree.add_label(
                    bg_id,
                    x + name_w,
                    y,
                    value_w,
                    IO_ROW_H,
                    &fmt_value(*value),
                    UIStyle {
                        text_color: color::TEXT_WHITE_C32,
                        font_size: FONT_SIZE,
                        text_align: TextAlign::Right,
                        ..UIStyle::default()
                    },
                );
                y += IO_ROW_H;
            }
            y += PADDING * 0.5;
        }
    }
}

/// Compact scalar formatting for the inspector's I/O readout — integers without
/// a decimal point, fractionals to three trimmed places.
fn fmt_value(v: f32) -> String {
    if v.is_finite() && (v - v.round()).abs() < 1e-4 && v.abs() < 1e6 {
        format!("{v:.0}")
    } else {
        crate::fmt::fmt_trimmed(v, 3)
    }
}

/// The smart-preview checkbox style — accent-filled when on, neutral gray when
/// off, matching the kit's other checkboxes.
fn checkbox_style(checked: bool, supported: bool) -> UIStyle {
    let bg_color = match (checked, supported) {
        (true, true) => color::ACCENT_BLUE_C32,
        (false, true) => color::BUTTON_INACTIVE_C32,
        (_, false) => color::BUTTON_INACTIVE_C32,
    };
    let mut style = UIStyle {
        bg_color,
        text_color: color::TEXT_WHITE_C32,
        font_size: HEADER_FONT_SIZE,
        text_align: TextAlign::Center,
        corner_radius: color::BUTTON_RADIUS,
        border_color: color::TEXT_DIMMED_C32,
        border_width: 1.0,
        ..UIStyle::default()
    };
    if !supported {
        style.text_color = color::TEXT_DIMMED_C32;
    }
    style
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inspector() -> NodeInspector {
        NodeInspector {
            title: "Envelope".to_string(),
            description: "Follows the audio envelope.".to_string(),
            inputs: vec![("audio".to_string(), 0.5)],
            outputs: vec![("level".to_string(), 0.82)],
        }
    }

    #[test]
    fn render_node_inspector_draws_for_a_non_image_node() {
        let mut panel = GraphEditorPanel::new();
        panel.set_node_inspector(Some(inspector()));
        let mut tree = UITree::new();
        let drew = panel.render_node_inspector(&mut tree, Rect::new(0.0, 0.0, 240.0, 200.0));
        assert!(drew, "a value-inspector node fills the pane");
        assert!(!panel.wants_smart_preview_toggle(), "no toggle for a non-image node");
    }

    #[test]
    fn render_node_inspector_skips_when_none() {
        let panel = GraphEditorPanel::new();
        let mut tree = UITree::new();
        assert!(!panel.render_node_inspector(&mut tree, Rect::new(0.0, 0.0, 240.0, 200.0)));
        assert!(panel.wants_smart_preview_toggle(), "image preview → toggle shows");
    }

    #[test]
    fn smart_preview_toggle_returns_a_button_id() {
        let mut panel = GraphEditorPanel::new();
        panel.set_node_preview_normalize(true);
        let mut tree = UITree::new();
        // Returns a real tree id the host can hang the normalize intent on.
        let _id = panel.render_smart_preview_toggle(&mut tree, Rect::new(0.0, 0.0, 200.0, 24.0));
    }
}
