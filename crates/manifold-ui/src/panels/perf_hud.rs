/// Performance HUD overlay panel.
/// 1:1 port of Unity PerformanceHUDPanel.cs (493 lines).
/// Shows FPS, frame time, sync state, MIDI state, clip scheduling metrics.
/// Toggled via Backtick (`) key.

use crate::color;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::*;
use crate::tree::UITree;
use super::{Panel, PanelAction};

const HUD_WIDTH: f32 = 250.0;
const HUD_HEIGHT: f32 = 320.0;
const ROW_HEIGHT: f32 = 14.0;
const LABEL_FONT: u16 = 9;
const VALUE_FONT: u16 = 9;
const SECTION_GAP: f32 = 6.0;
const PAD: f32 = 8.0;

/// Performance metrics pushed from the app each frame.
#[derive(Debug, Clone, Default)]
pub struct PerfMetrics {
    pub ui_fps: f32,
    pub ui_frame_time_ms: f32,
    pub render_fps: f32,
    pub render_frame_time_ms: f32,
    pub active_clips: usize,
    pub preparing_clips: usize,
    pub current_beat: f32,
    pub current_time_secs: f32,
    pub bpm: f32,
    pub clock_source: String,
    pub is_playing: bool,
    pub data_version: u64,
}

pub struct PerfHudPanel {
    visible: bool,
    metrics: PerfMetrics,
    first_node: usize,
    node_count: usize,
    // Node IDs for push-based value updates
    ui_fps_value_id: i32,
    ui_frame_time_id: i32,
    render_fps_value_id: i32,
    render_frame_time_id: i32,
    active_clips_id: i32,
    beat_id: i32,
    time_id: i32,
    bpm_id: i32,
    clock_id: i32,
}

impl PerfHudPanel {
    pub fn new() -> Self {
        Self {
            visible: false,
            metrics: PerfMetrics::default(),
            first_node: 0,
            node_count: 0,
            ui_fps_value_id: -1,
            ui_frame_time_id: -1,
            render_fps_value_id: -1,
            render_frame_time_id: -1,
            active_clips_id: -1,
            beat_id: -1,
            time_id: -1,
            bpm_id: -1,
            clock_id: -1,
        }
    }

    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// First node index in the tree (for overlay rendering — skip in Pass 1,
    /// render in a later pass so the HUD draws on top of bitmap textures).
    pub fn first_node(&self) -> usize {
        self.first_node
    }

    pub fn set_metrics(&mut self, metrics: PerfMetrics) {
        self.metrics = metrics;
    }

    /// Push metric values to the tree without rebuilding.
    /// From Unity PerformanceHUDPanel.UpdateMetrics (per-frame push).
    pub fn push_values(&self, tree: &mut UITree) {
        if !self.visible { return; }
        let m = &self.metrics;

        if self.ui_fps_value_id >= 0 {
            tree.set_text(self.ui_fps_value_id as u32, &format!("{:.0}", m.ui_fps));
        }
        if self.ui_frame_time_id >= 0 {
            tree.set_text(self.ui_frame_time_id as u32, &format!("{:.1} ms", m.ui_frame_time_ms));
        }
        if self.render_fps_value_id >= 0 {
            tree.set_text(self.render_fps_value_id as u32, &format!("{:.0}", m.render_fps));
        }
        if self.render_frame_time_id >= 0 {
            tree.set_text(self.render_frame_time_id as u32, &format!("{:.1} ms", m.render_frame_time_ms));
        }
        if self.active_clips_id >= 0 {
            tree.set_text(self.active_clips_id as u32, &format!("{} / {}", m.active_clips, m.preparing_clips));
        }
        if self.beat_id >= 0 {
            tree.set_text(self.beat_id as u32, &format!("{:.2}", m.current_beat));
        }
        if self.time_id >= 0 {
            let secs = m.current_time_secs;
            let mins = (secs / 60.0).floor() as u32;
            let s = secs % 60.0;
            tree.set_text(self.time_id as u32, &format!("{}:{:05.2}", mins, s));
        }
        if self.bpm_id >= 0 {
            tree.set_text(self.bpm_id as u32, &format!("{:.1}", m.bpm));
        }
        if self.clock_id >= 0 {
            tree.set_text(self.clock_id as u32, &m.clock_source);
        }
    }

    fn add_row(tree: &mut UITree, x: f32, y: f32, width: f32, label: &str) -> (i32, f32) {
        // Label on left
        tree.add_label(-1, x, y, width * 0.5, ROW_HEIGHT, label,
            UIStyle {
                text_color: color::TEXT_DIMMED,
                font_size: LABEL_FONT,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        // Value on right (returns node ID for push updates)
        let val_id = tree.add_label(-1, x + width * 0.5, y, width * 0.5, ROW_HEIGHT, "—",
            UIStyle {
                text_color: color::TEXT_NORMAL,
                font_size: VALUE_FONT,
                text_align: TextAlign::Right,
                ..UIStyle::default()
            },
        ) as i32;
        (val_id, y + ROW_HEIGHT)
    }
}

impl Panel for PerfHudPanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        self.first_node = tree.count();
        if !self.visible {
            self.node_count = 0;
            return;
        }

        // Position: bottom-right corner
        let x = layout.screen_width - HUD_WIDTH - PAD;
        let y = layout.screen_height - HUD_HEIGHT - PAD;

        // Background
        tree.add_panel(-1, x, y, HUD_WIDTH, HUD_HEIGHT,
            UIStyle {
                bg_color: color::HUD_BG,
                corner_radius: color::CARD_RADIUS,
                ..UIStyle::default()
            },
        );

        // Title
        let mut cy = y + PAD;
        let inner_w = HUD_WIDTH - PAD * 2.0;
        tree.add_label(-1, x + PAD, cy, inner_w, 14.0, "PERFORMANCE",
            UIStyle {
                text_color: color::TEXT_NEAR_WHITE,
                font_size: 10,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        cy += 16.0 + SECTION_GAP;

        // UI timing section
        let lx = x + PAD;
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "UI FPS");
        self.ui_fps_value_id = id; cy = ny;
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "UI Frame");
        self.ui_frame_time_id = id; cy = ny;
        cy += SECTION_GAP;

        // Render timing section
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "Render FPS");
        self.render_fps_value_id = id; cy = ny;
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "Render Frame");
        self.render_frame_time_id = id; cy = ny;
        cy += SECTION_GAP;

        // Clip scheduling
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "Active / Prep");
        self.active_clips_id = id; cy = ny;
        cy += SECTION_GAP;

        // Playback position
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "Beat");
        self.beat_id = id; cy = ny;
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "Time");
        self.time_id = id; cy = ny;
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "BPM");
        self.bpm_id = id; cy = ny;
        let (id, _ny) = Self::add_row(tree, lx, cy, inner_w, "Clock");
        self.clock_id = id;

        self.node_count = tree.count() - self.first_node;
    }

    fn update(&mut self, tree: &mut UITree) {
        self.push_values(tree);
    }

    fn handle_event(&mut self, _event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        Vec::new()
    }
}
