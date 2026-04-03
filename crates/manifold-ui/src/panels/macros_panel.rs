//! Macros panel: 8 horizontal sliders spanning the full inspector width.
//!
//! Positioned above the dual-column Master/Layer layout inside the inspector.
//! Each slider controls a macro slot (0–1) that fans out to mapped parameters.

use super::PanelAction;
use super::copy_to_clipboard_label::CopyToClipboardLabelState;
use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderDragState};
use crate::tree::UITree;
use manifold_core::macro_bank::MACRO_COUNT;

// ── Layout constants ───────────────────────────────────────────────

const ROW_HEIGHT: f32 = 18.0;
const ROW_SPACING: f32 = 2.0;
const PAD_TOP: f32 = 4.0;
const PAD_BOTTOM: f32 = 4.0;
const PAD_H: f32 = 4.0;
const LABEL_WIDTH: f32 = crate::slider::DEFAULT_LABEL_WIDTH;
const FONT_SIZE: u16 = color::FONT_BODY;

const SECTION_BORDER: Color32 = Color32::new(50, 50, 54, 255);
const SECTION_BG: Color32 = Color32::new(22, 22, 23, 255);
const SECTION_RADIUS: f32 = 4.0;

fn fmt_macro(v: f32) -> String {
    format!("{:.2}", v)
}

// ── MacrosPanel ────────────────────────────────────────────────────

pub struct MacrosPanel {
    sliders: [SliderDragState; MACRO_COUNT],
    first_node: usize,
    node_count: usize,
    copied_flash: CopyToClipboardLabelState,
}

impl Default for MacrosPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl MacrosPanel {
    fn display_label(labels: &[String], index: usize) -> String {
        labels
            .get(index)
            .filter(|label| !label.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("M{}", index + 1))
    }

    pub fn new() -> Self {
        Self {
            sliders: std::array::from_fn(|_| SliderDragState::with_range(0.0, 1.0, false)),
            first_node: usize::MAX,
            node_count: 0,
            copied_flash: CopyToClipboardLabelState::default(),
        }
    }

    /// Total height of the macros panel (for inspector column Y offset).
    pub fn height() -> f32 {
        PAD_TOP
            + MACRO_COUNT as f32 * ROW_HEIGHT
            + (MACRO_COUNT - 1) as f32 * ROW_SPACING
            + PAD_BOTTOM
    }

    pub fn first_node(&self) -> usize {
        self.first_node
    }
    pub fn node_count(&self) -> usize {
        self.node_count
    }

    pub fn is_dragging(&self) -> bool {
        self.sliders.iter().any(|s| s.is_dragging())
    }

    /// Sync cached macro values from project state.
    pub fn sync_values(&mut self, tree: &mut UITree, values: &[f32], labels: &[String]) {
        let copied_label = self
            .copied_flash
            .label_id()
            .map(|label_id| {
                self.sliders
                    .iter()
                    .enumerate()
                    .find_map(|(i, s)| {
                        s.ids()
                            .filter(|ids| ids.label >= 0 && ids.label as u32 == label_id)
                            .map(|_| Self::display_label(labels, i))
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        self.copied_flash.sync(tree, FONT_SIZE, &copied_label);

        for (i, s) in self.sliders.iter_mut().enumerate() {
            if let Some(ids) = s.ids()
                && ids.label >= 0
                && Some(ids.label as u32) != self.copied_flash.label_id()
            {
                tree.set_text(ids.label as u32, &Self::display_label(labels, i));
            }
            if let Some(&v) = values.get(i) {
                s.sync(tree, v, &fmt_macro);
            }
        }
    }

    /// Build the macros panel into the tree at the given rect.
    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        self.first_node = tree.count();

        let colors = SliderColors::default_slider();

        // Section card (border + inner bg)
        tree.add_panel(
            -1,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            UIStyle {
                bg_color: SECTION_BORDER,
                corner_radius: SECTION_RADIUS,
                ..UIStyle::default()
            },
        );
        tree.add_panel(
            -1,
            rect.x + 1.0,
            rect.y + 1.0,
            rect.width - 2.0,
            rect.height - 2.0,
            UIStyle {
                bg_color: SECTION_BG,
                corner_radius: SECTION_RADIUS - 1.0,
                ..UIStyle::default()
            },
        );

        let inner_x = rect.x + PAD_H;
        let inner_w = rect.width - PAD_H * 2.0;
        let mut cy = rect.y + PAD_TOP;

        for i in 0..MACRO_COUNT {
            let label = format!("M{}", i + 1);
            let v = self.sliders[i].cached_value();
            let v = if v.is_nan() { 0.0 } else { v };

            let ids = BitmapSlider::build(
                tree,
                -1,
                Rect::new(inner_x, cy, inner_w, ROW_HEIGHT),
                Some(&label),
                v,
                &fmt_macro(v),
                &colors,
                FONT_SIZE,
                LABEL_WIDTH,
            );

            self.sliders[i].set_ids(ids);
            cy += ROW_HEIGHT + ROW_SPACING;
        }

        self.node_count = tree.count() - self.first_node;
    }

    /// Handle press on a slider track. Returns Snapshot action if hit.
    pub fn handle_press(&mut self, node_id: u32, pos_x: f32) -> Vec<PanelAction> {
        for (i, s) in self.sliders.iter_mut().enumerate() {
            if let Some(val) = s.try_start_drag(node_id, pos_x) {
                return vec![
                    PanelAction::MacroSnapshot(i),
                    PanelAction::MacroChanged(i, val),
                ];
            }
        }
        Vec::new()
    }

    /// Handle drag (pointer move while pressed).
    pub fn handle_drag(&mut self, pos_x: f32, tree: &mut UITree) -> Vec<PanelAction> {
        for (i, s) in self.sliders.iter_mut().enumerate() {
            if let Some(val) = s.apply_drag(pos_x, tree, &fmt_macro) {
                return vec![PanelAction::MacroChanged(i, val)];
            }
        }
        Vec::new()
    }

    /// Handle pointer up — commit the drag.
    pub fn handle_release(&mut self) -> Vec<PanelAction> {
        for (i, s) in self.sliders.iter_mut().enumerate() {
            if s.end_drag() {
                return vec![PanelAction::MacroCommit(i)];
            }
        }
        Vec::new()
    }

    /// Handle click — label click copies OSC address to clipboard.
    pub fn handle_click(&mut self, node_id: u32) -> Vec<PanelAction> {
        for (i, s) in self.sliders.iter().enumerate() {
            if let Some(ids) = s.ids()
                && ids.label >= 0
                && node_id == ids.label as u32
            {
                self.copied_flash.trigger(ids.label as u32);
                let addr = format!("/macro/{}", i + 1);
                return vec![PanelAction::CopyOscAddress(addr)];
            }
        }
        Vec::new()
    }

    /// Handle right-click.
    /// Track right-click → reset to 0 (direct, like effect param slider).
    /// Label right-click → open dropdown showing mapped params.
    pub fn handle_right_click(&self, node_id: u32) -> Vec<PanelAction> {
        for (i, s) in self.sliders.iter().enumerate() {
            // Right-click slider track → direct reset to 0
            if s.track_id() == Some(node_id) {
                return vec![PanelAction::MacroRightClick(i)];
            }
            // Right-click label → open mappings dropdown
            if let Some(ids) = s.ids()
                && ids.label >= 0
                && node_id == ids.label as u32
            {
                return vec![PanelAction::MacroLabelRightClick(i)];
            }
        }
        Vec::new()
    }

    /// Check if a node belongs to this panel.
    pub fn owns_node(&self, node_id: u32) -> bool {
        if self.first_node == usize::MAX {
            return false;
        }
        let id = node_id as usize;
        id >= self.first_node && id < self.first_node + self.node_count
    }

    pub fn label_rect(&self, tree: &UITree, index: usize) -> Option<Rect> {
        self.sliders
            .get(index)
            .and_then(|slider| slider.ids())
            .filter(|ids| ids.label >= 0)
            .map(|ids| tree.get_bounds(ids.label as u32))
    }
}
