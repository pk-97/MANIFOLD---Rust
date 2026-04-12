//! Macros panel: 8 horizontal sliders spanning the full inspector width.
//!
//! Positioned above the dual-column Master/Layer layout inside the inspector.
//! Each slider controls a macro slot (0–1) that fans out to mapped parameters.

use super::PanelAction;
use super::copy_to_clipboard_label::CopyToClipboardLabelState;
use super::param_slider_shared::{
    ABL_CONFIG_HEIGHT, AbletonConfigIds, AbletonConfigClick, AbletonMappingDisplay,
    TrimHandleIds, build_ableton_config, build_trim_handles_explicit,
    check_ableton_config_click, OVERLAY_INSET, TRIM_BAR_W,
};
use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderDragState};
use crate::tree::UITree;
use manifold_core::macro_bank::MACRO_COUNT;

// ── Layout constants ───────────────────────────────────────────────

const ROW_HEIGHT: f32 = 18.0;
const ROW_SPACING: f32 = 2.0;
const HEADER_ROW_H: f32 = 22.0;
const PAD_TOP: f32 = 4.0;
const PAD_BOTTOM: f32 = 4.0;
const PAD_H: f32 = 4.0;
const CHEVRON_W: f32 = 18.0;
const GAP: f32 = 4.0;
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
    /// Ableton trim handle node IDs per macro slot.
    ableton_trim_ids: [Option<TrimHandleIds>; MACRO_COUNT],
    /// Ableton config drawer node IDs per macro slot (status dot + name + INV).
    ableton_config_ids: [Option<AbletonConfigIds>; MACRO_COUNT],
    /// Cached Ableton display data per slot (for build).
    ableton_displays: [Option<AbletonMappingDisplay>; MACRO_COUNT],
    /// Cached Ableton range per slot (for drag updates + build).
    ableton_ranges: [Option<(f32, f32)>; MACRO_COUNT],
    /// Which macro slot's Ableton trim bar is being dragged (-1 = none).
    dragging_ableton_trim: i32,
    dragging_ableton_trim_is_min: bool,
    // Collapse state
    is_collapsed: bool,
    header_label_id: i32,
    chevron_btn_id: i32,
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
            ableton_trim_ids: std::array::from_fn(|_| None),
            ableton_config_ids: std::array::from_fn(|_| None),
            ableton_displays: std::array::from_fn(|_| None),
            ableton_ranges: [None; MACRO_COUNT],
            dragging_ableton_trim: -1,
            dragging_ableton_trim_is_min: false,
            is_collapsed: false,
            header_label_id: -1,
            chevron_btn_id: -1,
        }
    }

    /// Total height of the macros panel (for inspector column Y offset).
    /// Dynamic: includes Ableton config drawers for mapped slots.
    pub fn height(&self) -> f32 {
        if self.is_collapsed {
            return PAD_TOP + HEADER_ROW_H + PAD_BOTTOM;
        }
        let mut h = PAD_TOP + HEADER_ROW_H + PAD_BOTTOM;
        for i in 0..MACRO_COUNT {
            h += ROW_HEIGHT;
            if self.ableton_displays[i].is_some() {
                h += ABL_CONFIG_HEIGHT;
            }
            if i + 1 < MACRO_COUNT {
                h += ROW_SPACING;
            }
        }
        h
    }

    pub fn is_collapsed(&self) -> bool {
        self.is_collapsed
    }

    pub fn toggle_collapsed(&mut self) {
        self.is_collapsed = !self.is_collapsed;
    }

    pub fn first_node(&self) -> usize {
        self.first_node
    }

    /// Set Ableton display data per macro slot (call before build).
    pub fn set_ableton_displays(&mut self, displays: &[Option<AbletonMappingDisplay>]) {
        for (i, d) in displays.iter().enumerate().take(MACRO_COUNT) {
            self.ableton_displays[i] = d.clone();
        }
    }

    /// Set Ableton trim ranges per macro slot (call before build or sync).
    pub fn set_ableton_ranges(&mut self, ranges: &[Option<(f32, f32)>]) {
        for (i, r) in ranges.iter().enumerate().take(MACRO_COUNT) {
            self.ableton_ranges[i] = *r;
        }
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

        // Header row: "Macros" label + chevron
        let label_w = inner_w - CHEVRON_W - GAP;
        self.header_label_id = tree.add_label(
            -1,
            inner_x,
            cy,
            label_w,
            HEADER_ROW_H,
            "Macros",
            UIStyle {
                text_color: color::TEXT_PRIMARY_C32,
                font_size: color::FONT_HEADING,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;

        let chev_x = inner_x + inner_w - CHEVRON_W;
        self.chevron_btn_id = tree.add_button(
            -1,
            chev_x,
            cy + (HEADER_ROW_H - 16.0) * 0.5,
            CHEVRON_W,
            16.0,
            UIStyle {
                bg_color: Color32::TRANSPARENT,
                hover_bg_color: color::HOVER_OVERLAY,
                pressed_bg_color: color::PRESS_OVERLAY,
                text_color: color::CHEVRON_COLOR,
                font_size: FONT_SIZE,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            if self.is_collapsed {
                "\u{25B6}"
            } else {
                "\u{25BC}"
            },
        ) as i32;

        cy += HEADER_ROW_H;

        if self.is_collapsed {
            // Clear slider IDs so sync_values skips them
            for s in &mut self.sliders {
                s.clear();
            }
            self.node_count = tree.count() - self.first_node;
            return;
        }

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

            // Ableton trim handles (when macro has an Ableton mapping)
            if let Some((amin, amax)) = self.ableton_ranges[i] {
                self.ableton_trim_ids[i] = Some(build_trim_handles_explicit(
                    tree,
                    ids.track as i32,
                    ids.track_rect,
                    amin,
                    amax,
                    color::ABL_TRIM_BAR_C32,
                    color::ABL_TRIM_BAR_HOVER_C32,
                    color::ABL_TRIM_FILL_C32,
                ));
            } else {
                self.ableton_trim_ids[i] = None;
            }

            cy += ROW_HEIGHT;

            // Ableton config drawer (status dot + macro name + INV button)
            if let Some(ref display) = self.ableton_displays[i] {
                self.ableton_config_ids[i] = Some(build_ableton_config(
                    tree, -1, inner_x, cy, inner_w, display,
                ));
                cy += ABL_CONFIG_HEIGHT;
            } else {
                self.ableton_config_ids[i] = None;
            }

            if i + 1 < MACRO_COUNT {
                cy += ROW_SPACING;
            }
        }

        self.node_count = tree.count() - self.first_node;
    }

    /// Handle press on a slider track or trim bar.
    pub fn handle_press(&mut self, node_id: u32, pos_x: f32) -> Vec<PanelAction> {
        // Check Ableton trim bars first (higher z-order)
        for (i, trim) in self.ableton_trim_ids.iter().enumerate() {
            if let Some(t) = trim {
                if node_id as i32 == t.min_bar_id {
                    self.dragging_ableton_trim = i as i32;
                    self.dragging_ableton_trim_is_min = true;
                    return vec![PanelAction::AbletonMacroTrimSnapshot(i)];
                }
                if node_id as i32 == t.max_bar_id {
                    self.dragging_ableton_trim = i as i32;
                    self.dragging_ableton_trim_is_min = false;
                    return vec![PanelAction::AbletonMacroTrimSnapshot(i)];
                }
            }
        }
        // Slider track drag
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
        // Ableton trim drag
        if self.dragging_ableton_trim >= 0 {
            let i = self.dragging_ableton_trim as usize;
            if let Some((cur_min, cur_max)) = self.ableton_ranges[i]
                && let Some(ids) = self.sliders[i].ids()
            {
                let norm = BitmapSlider::x_to_normalized(ids.track_rect, pos_x);
                let (new_min, new_max) = if self.dragging_ableton_trim_is_min {
                    (norm.clamp(0.0, cur_max), cur_max)
                } else {
                    (cur_min, norm.clamp(cur_min, 1.0))
                };
                self.ableton_ranges[i] = Some((new_min, new_max));

                if let Some(t) = &self.ableton_trim_ids[i] {
                    let usable = ids.track_rect.width - OVERLAY_INSET * 2.0;
                    let base_x = ids.track_rect.x + OVERLAY_INSET;
                    let fill_x = base_x + new_min * usable;
                    let fill_w = (new_max - new_min) * usable;
                    let fill_h = ids.track_rect.height - OVERLAY_INSET * 2.0;
                    tree.set_bounds(
                        t.fill_id as u32,
                        Rect::new(
                            fill_x,
                            ids.track_rect.y + OVERLAY_INSET,
                            fill_w,
                            fill_h,
                        ),
                    );
                    tree.set_bounds(
                        t.min_bar_id as u32,
                        Rect::new(
                            base_x + new_min * usable - TRIM_BAR_W * 0.5,
                            ids.track_rect.y,
                            TRIM_BAR_W,
                            ids.track_rect.height,
                        ),
                    );
                    tree.set_bounds(
                        t.max_bar_id as u32,
                        Rect::new(
                            base_x + new_max * usable - TRIM_BAR_W * 0.5,
                            ids.track_rect.y,
                            TRIM_BAR_W,
                            ids.track_rect.height,
                        ),
                    );
                }

                return vec![PanelAction::AbletonMacroTrimChanged(
                    i, new_min, new_max,
                )];
            }
        }
        // Slider drag
        for (i, s) in self.sliders.iter_mut().enumerate() {
            if let Some(val) = s.apply_drag(pos_x, tree, &fmt_macro) {
                return vec![PanelAction::MacroChanged(i, val)];
            }
        }
        Vec::new()
    }

    /// Handle pointer up — commit the drag.
    pub fn handle_release(&mut self) -> Vec<PanelAction> {
        if self.dragging_ableton_trim >= 0 {
            let i = self.dragging_ableton_trim as usize;
            self.dragging_ableton_trim = -1;
            return vec![PanelAction::AbletonMacroTrimCommit(i)];
        }
        for (i, s) in self.sliders.iter_mut().enumerate() {
            if s.end_drag() {
                return vec![PanelAction::MacroCommit(i)];
            }
        }
        Vec::new()
    }

    /// Handle click — chevron toggles collapse, label click copies OSC address,
    /// INV button toggles invert.
    pub fn handle_click(&mut self, node_id: u32) -> Vec<PanelAction> {
        // Chevron collapse toggle
        if node_id as i32 == self.chevron_btn_id {
            return vec![PanelAction::MacrosCollapseToggle];
        }

        // Ableton config INV button
        if let Some((slot_idx, AbletonConfigClick::Invert)) =
            check_ableton_config_click(node_id as i32, &self.ableton_config_ids)
        {
            return vec![PanelAction::AbletonMacroInvertToggle(slot_idx)];
        }

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
