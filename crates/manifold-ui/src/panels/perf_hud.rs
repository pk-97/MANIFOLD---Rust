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
const HUD_HEIGHT: f32 = 380.0;
const ROW_HEIGHT: f32 = 14.0;
const LABEL_FONT: u16 = 9;
const VALUE_FONT: u16 = 9;
const SECTION_GAP: f32 = 6.0;
const PAD: f32 = 8.0;

/// Number of samples in the rolling frame time graph.
const GRAPH_SAMPLES: usize = 120;
/// Height of the frame time graph area in logical pixels.
const GRAPH_HEIGHT: f32 = 50.0;
/// Max frame time for graph scale (ms). Bars at or above this clip to full height.
const GRAPH_MAX_MS: f32 = 33.3;

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
    /// Whether a profiling session is actively recording.
    pub profiling_active: bool,
    /// Number of frames captured in the current profiling session.
    pub profiling_frame_count: u64,
}

/// Rolling ring buffer for frame time history.
struct FrameTimeHistory {
    samples: [f32; GRAPH_SAMPLES],
    write_pos: usize,
}

impl FrameTimeHistory {
    fn new() -> Self {
        Self {
            samples: [0.0; GRAPH_SAMPLES],
            write_pos: 0,
        }
    }

    fn push(&mut self, ms: f32) {
        self.samples[self.write_pos] = ms;
        self.write_pos = (self.write_pos + 1) % GRAPH_SAMPLES;
    }

    /// Iterate samples oldest-first (for left-to-right graph drawing).
    fn iter_oldest_first(&self) -> impl Iterator<Item = f32> + '_ {
        let start = self.write_pos;
        (0..GRAPH_SAMPLES).map(move |i| self.samples[(start + i) % GRAPH_SAMPLES])
    }
}

pub struct PerfHudPanel {
    visible: bool,
    metrics: PerfMetrics,
    first_node: usize,
    node_count: usize,
    // Rolling frame time histories
    ui_dt_history: FrameTimeHistory,
    render_dt_history: FrameTimeHistory,
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
    profiling_id: i32,
    // Node IDs for graph bars (stored for per-frame color/size updates)
    ui_graph_bar_ids: Vec<i32>,
    render_graph_bar_ids: Vec<i32>,
    /// Y position of the UI graph area (set during build).
    ui_graph_y: f32,
    /// Y position of the render graph area (set during build).
    render_graph_y: f32,
    /// X position of the graph area.
    graph_x: f32,
    /// Width of the graph area.
    graph_w: f32,
}

impl Default for PerfHudPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl PerfHudPanel {
    pub fn new() -> Self {
        Self {
            visible: false,
            metrics: PerfMetrics::default(),
            first_node: 0,
            node_count: 0,
            ui_dt_history: FrameTimeHistory::new(),
            render_dt_history: FrameTimeHistory::new(),
            ui_fps_value_id: -1,
            ui_frame_time_id: -1,
            render_fps_value_id: -1,
            render_frame_time_id: -1,
            active_clips_id: -1,
            beat_id: -1,
            time_id: -1,
            bpm_id: -1,
            clock_id: -1,
            profiling_id: -1,
            ui_graph_bar_ids: Vec::new(),
            render_graph_bar_ids: Vec::new(),
            ui_graph_y: 0.0,
            render_graph_y: 0.0,
            graph_x: 0.0,
            graph_w: 0.0,
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
        // Push new samples into ring buffers
        self.ui_dt_history.push(metrics.ui_frame_time_ms);
        self.render_dt_history.push(metrics.render_frame_time_ms);
        self.metrics = metrics;
    }

    /// Push metric values and graph bars to the tree without rebuilding.
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
        if self.profiling_id >= 0 {
            if m.profiling_active {
                tree.set_text(
                    self.profiling_id as u32,
                    &format!("REC {}", m.profiling_frame_count),
                );
                tree.set_style(self.profiling_id as u32, UIStyle {
                    text_color: color::STATUS_BAD,
                    font_size: VALUE_FONT,
                    text_align: TextAlign::Right,
                    ..UIStyle::default()
                });
            } else {
                tree.set_text(self.profiling_id as u32, "—");
                tree.set_style(self.profiling_id as u32, UIStyle {
                    text_color: color::TEXT_NORMAL,
                    font_size: VALUE_FONT,
                    text_align: TextAlign::Right,
                    ..UIStyle::default()
                });
            }
        }

        // Update graph bars — resize height + recolor per sample value
        self.update_graph_bars(tree, &self.ui_graph_bar_ids, &self.ui_dt_history, self.ui_graph_y);
        self.update_graph_bars(tree, &self.render_graph_bar_ids, &self.render_dt_history, self.render_graph_y);
    }

    /// Update bar heights and colors from the ring buffer.
    fn update_graph_bars(
        &self,
        tree: &mut UITree,
        bar_ids: &[i32],
        history: &FrameTimeHistory,
        graph_y: f32,
    ) {
        if bar_ids.is_empty() { return; }
        for (i, ms) in history.iter_oldest_first().enumerate() {
            if i >= bar_ids.len() { break; }
            let id = bar_ids[i];
            if id < 0 { continue; }

            let frac = (ms / GRAPH_MAX_MS).clamp(0.0, 1.0);
            let bar_h = (frac * GRAPH_HEIGHT).max(1.0);
            let bar_y = graph_y + GRAPH_HEIGHT - bar_h;
            let col = dt_color(ms);

            // Preserve x/w, update y/h
            let old = tree.get_bounds(id as u32);
            tree.set_bounds(id as u32, Rect {
                x: old.x,
                y: bar_y,
                width: old.width,
                height: bar_h,
            });
            tree.set_style(id as u32, UIStyle {
                bg_color: col,
                ..UIStyle::default()
            });
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

    /// Build the bar graph nodes for a frame time history.
    /// Each bar is a 2px-wide panel node; height/color updated per frame.
    fn build_graph_bars(
        tree: &mut UITree,
        x: f32,
        y: f32,
        width: f32,
    ) -> Vec<i32> {
        let bar_w = width / GRAPH_SAMPLES as f32;
        let mut ids = Vec::with_capacity(GRAPH_SAMPLES);
        for i in 0..GRAPH_SAMPLES {
            let bx = x + i as f32 * bar_w;
            let id = tree.add_panel(-1, bx, y, bar_w.max(1.0), 1.0,
                UIStyle {
                    bg_color: Color32::new(40, 40, 44, 255),
                    ..UIStyle::default()
                },
            ) as i32;
            ids.push(id);
        }
        ids
    }
}

/// Map frame time to color: green (<8ms), yellow (8-16ms), red (>16ms).
fn dt_color(ms: f32) -> Color32 {
    if ms < 8.5 {
        color::STATUS_GOOD
    } else if ms < 16.7 {
        color::STATUS_WARNING
    } else {
        color::STATUS_BAD
    }
}

impl Panel for PerfHudPanel {
    fn build(&mut self, tree: &mut UITree, layout: &ScreenLayout) {
        self.first_node = tree.count();
        if !self.visible {
            self.node_count = 0;
            self.ui_graph_bar_ids.clear();
            self.render_graph_bar_ids.clear();
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

        let lx = x + PAD;
        self.graph_x = lx;
        self.graph_w = inner_w;

        // ── UI timing ────────────────────────────────────────────
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "UI FPS");
        self.ui_fps_value_id = id; cy = ny;
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "UI Frame");
        self.ui_frame_time_id = id; cy = ny;

        // UI frame time graph
        self.ui_graph_y = cy;
        self.ui_graph_bar_ids = Self::build_graph_bars(tree, lx, cy, inner_w);
        cy += GRAPH_HEIGHT + SECTION_GAP;

        // ── Render timing ────────────────────────────────────────
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "Render FPS");
        self.render_fps_value_id = id; cy = ny;
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "Render Frame");
        self.render_frame_time_id = id; cy = ny;

        // Render frame time graph
        self.render_graph_y = cy;
        self.render_graph_bar_ids = Self::build_graph_bars(tree, lx, cy, inner_w);
        cy += GRAPH_HEIGHT + SECTION_GAP;

        // ── Clip scheduling ──────────────────────────────────────
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "Active / Prep");
        self.active_clips_id = id; cy = ny;
        cy += SECTION_GAP;

        // ── Playback position ────────────────────────────────────
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "Beat");
        self.beat_id = id; cy = ny;
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "Time");
        self.time_id = id; cy = ny;
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "BPM");
        self.bpm_id = id; cy = ny;
        let (id, ny) = Self::add_row(tree, lx, cy, inner_w, "Clock");
        self.clock_id = id; cy = ny;

        // ── Profiling status ─────────────────────────────────────
        let (id, _ny) = Self::add_row(tree, lx, cy, inner_w, "Profiling");
        self.profiling_id = id;

        self.node_count = tree.count() - self.first_node;
    }

    fn update(&mut self, tree: &mut UITree) {
        self.push_values(tree);
    }

    fn handle_event(&mut self, _event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        Vec::new()
    }
}
