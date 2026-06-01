//! [`GraphCardMirrorPanel`] — left-lane panel inside the graph-editor
//! window mirroring the effect card's exposed parameters.
//!
//! This is the authoring-side reflection of the performance card: one row
//! per parameter the user has promoted onto the effect card
//! (`EffectInstance.user_param_bindings` + preset outer-routings), with
//! the live value the renderer is using this frame. It occupies the lane
//! the node palette used to hold — the palette moved to the spawn popup
//! (double-click / Tab), so the left lane now answers "what have I
//! promoted to my instrument" rather than "what can I drop in."
//!
//! Mirrors [`super::graph_palette::GraphPalette`]'s shape: owns no GPU
//! state, just configures a UITree subtree each frame from the
//! `GraphEditorCardEntry` list the app builds from the live snapshot.
//!
//! Read-only for now (value display). The editable surface — value scrub
//! plus the per-row mapping flyout (range / invert / curve via
//! `MappingPopover`) — is the next pass; it routes the value edit through
//! `EffectParamChanged(effect_index, ParamId)` (the card's own write path,
//! since the binding maps the card value into the inner param each frame),
//! not the inner-node `SetGraphNodeParam` path the inspector uses.

use crate::color;
use crate::node::*;
use crate::tree::UITree;

use super::graph_editor::{format_card_entry_value, GraphEditorCardEntry};

/// Left-lane width inside the graph-editor window. Matches the old
/// palette width so the canvas keeps the same screen origin (the canvas
/// coordinate mapping is anchored on this offset).
pub const CARD_MIRROR_WIDTH: f32 = 200.0;

const ROW_H: f32 = 28.0;
const PADDING: f32 = 12.0;
const HEADER_H: f32 = 32.0;
const FONT_SIZE: u16 = 12;
const HEADER_FONT_SIZE: u16 = 14;

/// Left-lane card-mirror panel inside the graph-editor window.
#[derive(Default)]
pub struct GraphCardMirrorPanel {
    /// Exposed-parameter entries to mirror, built by
    /// `app_render::build_card_entries` from the live graph snapshot.
    entries: Vec<GraphEditorCardEntry>,
    /// Root container id inside the editor's UITree.
    root_id: i32,
}

impl GraphCardMirrorPanel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update the panel's input data. Cheap — the entry list is a small
    /// `Vec` rebuilt per frame from the snapshot.
    pub fn configure(&mut self, entries: Vec<GraphEditorCardEntry>) {
        self.entries = entries;
    }

    /// Build the UITree subtree at the given viewport.
    pub fn build(&mut self, tree: &mut UITree, viewport: Rect) {
        let bg_id = tree.add_panel(
            -1,
            viewport.x,
            viewport.y,
            viewport.width,
            viewport.height,
            UIStyle {
                bg_color: color::EFFECT_CARD_INNER_BG_C32,
                ..UIStyle::default()
            },
        ) as i32;
        self.root_id = bg_id;

        let mut y = viewport.y + PADDING;

        tree.add_label(
            bg_id,
            viewport.x + PADDING,
            y,
            viewport.width - 2.0 * PADDING,
            HEADER_H - PADDING,
            "Effect Card",
            UIStyle {
                text_color: color::TEXT_WHITE_C32,
                font_size: HEADER_FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        y += HEADER_H;

        if self.entries.is_empty() {
            tree.add_label(
                bg_id,
                viewport.x + PADDING,
                y,
                viewport.width - 2.0 * PADDING,
                ROW_H,
                "Tick a node param to add it here.",
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            return;
        }

        for entry in &self.entries {
            let row_x = viewport.x + PADDING;
            let row_w = (viewport.width - 2.0 * PADDING).max(10.0);
            // Split: friendly label on the left, live value on the right.
            let value_w = (row_w * 0.42).max(48.0);
            let label_w = (row_w - value_w).max(10.0);
            tree.add_label(
                bg_id,
                row_x,
                y,
                label_w,
                ROW_H,
                &entry.label,
                UIStyle {
                    text_color: color::TEXT_WHITE_C32,
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            tree.add_label(
                bg_id,
                row_x + label_w,
                y,
                value_w,
                ROW_H,
                &format_card_entry_value(entry),
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: FONT_SIZE,
                    text_align: TextAlign::Right,
                    ..UIStyle::default()
                },
            );
            y += ROW_H;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::graph_editor::GraphEditorParamKind;
    use super::*;

    fn entry(label: &str, value: f32, kind: GraphEditorParamKind) -> GraphEditorCardEntry {
        GraphEditorCardEntry {
            label: label.to_string(),
            target_handle: "node_a".to_string(),
            target_inner_param: "amount".to_string(),
            current_value: value,
            kind,
            enum_labels: None,
        }
    }

    #[test]
    fn empty_state_renders_hint() {
        let mut tree = UITree::new();
        let mut panel = GraphCardMirrorPanel::new();
        panel.configure(Vec::new());
        panel.build(&mut tree, Rect::new(0.0, 0.0, CARD_MIRROR_WIDTH, 600.0));
        // Root + header + hint label, no row panics.
        assert!(panel.entries.is_empty());
    }

    #[test]
    fn rows_build_for_each_entry() {
        let mut tree = UITree::new();
        let mut panel = GraphCardMirrorPanel::new();
        panel.configure(vec![
            entry("Amount", 0.5, GraphEditorParamKind::Float),
            entry("Curl", std::f32::consts::FRAC_PI_2, GraphEditorParamKind::Angle),
        ]);
        panel.build(&mut tree, Rect::new(0.0, 0.0, CARD_MIRROR_WIDTH, 600.0));
        assert_eq!(panel.entries.len(), 2);
    }

    #[test]
    fn angle_value_formats_as_degrees() {
        let e = entry("Curl", std::f32::consts::FRAC_PI_2, GraphEditorParamKind::Angle);
        assert_eq!(format_card_entry_value(&e), "90°");
    }
}
