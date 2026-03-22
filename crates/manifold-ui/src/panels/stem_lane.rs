//! Stem waveform lanes — 4 per-stem lanes (Drums, Bass, Other, Vocals)
//! managed as a collapsible group.
//!
//! Hybrid bitmap + UITree node architecture:
//! - **Bitmap**: stem waveform bars, playhead, lane backgrounds
//! - **UITree nodes**: transparent scrub overlays, mute/solo buttons, stem name labels
//!
//! Unity: `StemWaveformLane` + `StemLaneGroup`.

use crate::bitmap_painter::fill_rect;
use crate::color;
use crate::coordinate_mapper::CoordinateMapper;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::{Color32, Rect, UIStyle, TextAlign, FontWeight};
use crate::tree::UITree;
use crate::waveform_painter;
use crate::waveform_renderer::WaveformRenderer;

use super::{Panel, PanelAction};

/// Number of stems (Unity: StemAudioController.StemCount = 4).
pub const STEM_COUNT: usize = 4;

/// Stem names (Unity: StemAudioController.StemNames).
pub const STEM_NAMES: [&str; STEM_COUNT] = ["Drums", "Bass", "Other", "Vocals"];

/// Per-stem background colors (Unity: StemLaneGroup.StemBackgroundColors).
const STEM_BG_COLORS: [Color32; STEM_COUNT] = [
    color::STEM_LANE_BG_DRUMS,
    color::STEM_LANE_BG_BASS,
    color::STEM_LANE_BG_OTHER,
    color::STEM_LANE_BG_VOCALS,
];

/// Playhead color (same as master lane).
const PLAYHEAD_COLOR: Color32 = Color32::new(217, 64, 56, 217);

// ── Mute/Solo button layout ──
const MUTE_SOLO_BTN_W: f32 = 20.0;
const MUTE_SOLO_BTN_H: f32 = 16.0;
const HEADER_X: f32 = 4.0;
const BUTTON_SPACING: f32 = 2.0;

/// UITree node IDs for a single stem lane.
#[derive(Clone, Copy)]
struct StemLaneNodeIds {
    overlay_id: i32,
    name_label_id: i32,
    mute_btn_id: i32,
    solo_btn_id: i32,
}

impl Default for StemLaneNodeIds {
    fn default() -> Self {
        Self {
            overlay_id: -1,
            name_label_id: -1,
            mute_btn_id: -1,
            solo_btn_id: -1,
        }
    }
}

/// State for a single stem lane.
///
/// Unity: `StemWaveformLane` (312 lines).
struct StemLane {
    renderer: WaveformRenderer,
    #[allow(dead_code)]
    stem_index: usize,
    is_muted: bool,
    is_soloed: bool,
}

impl StemLane {
    fn new(stem_index: usize) -> Self {
        Self {
            renderer: WaveformRenderer::new(),
            stem_index,
            is_muted: false,
            is_soloed: false,
        }
    }
}

/// Manages 4 stem waveform lanes as a collapsible group.
///
/// Unity: `StemLaneGroup` (145 lines).
pub struct StemLaneGroupPanel {
    lanes: [StemLane; STEM_COUNT],
    expanded: bool,

    // ── Pixel buffer ──
    pub pixel_buffer: Vec<Color32>,
    pub buffer_width: usize,
    pub buffer_height: usize,
    pub dirty: bool,

    // ── State for overlay ──
    waveform_start_beat: f32,
    playhead_beat: f32,
    scroll_offset_x: f32,
    bpm: f32,

    // ── UITree node IDs ──
    lane_nodes: [StemLaneNodeIds; STEM_COUNT],
}

impl Default for StemLaneGroupPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl StemLaneGroupPanel {
    pub fn new() -> Self {
        Self {
            lanes: [
                StemLane::new(0),
                StemLane::new(1),
                StemLane::new(2),
                StemLane::new(3),
            ],
            expanded: false,
            pixel_buffer: Vec::new(),
            buffer_width: 0,
            buffer_height: 0,
            dirty: true,
            waveform_start_beat: 0.0,
            playhead_beat: 0.0,
            scroll_offset_x: 0.0,
            bpm: 120.0,
            lane_nodes: [StemLaneNodeIds::default(); STEM_COUNT],
        }
    }

    /// Total pixel height of the stem lane area.
    ///
    /// Unity: `StemLaneGroup.TotalHeight` (lines 31-33).
    pub fn total_height(&self) -> f32 {
        if self.expanded {
            STEM_COUNT as f32 * color::STEM_LANE_HEIGHT
        } else {
            0.0
        }
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Unity: `SetExpanded(bool expand)` (lines 79-83).
    pub fn set_expanded(&mut self, expand: bool) {
        if self.expanded != expand {
            self.expanded = expand;
            self.dirty = true;
        }
    }

    /// Set audio data for a stem.
    ///
    /// Unity: `SetStemClip(int index, AudioClip clip)` (lines 86-89).
    pub fn set_stem_audio(
        &mut self,
        index: usize,
        samples: &[f32],
        channels: usize,
        sample_rate: u32,
    ) {
        if index < STEM_COUNT {
            self.lanes[index]
                .renderer
                .set_audio_data(samples, channels, sample_rate);
            self.dirty = true;
        }
    }

    /// Clear all stem waveforms.
    ///
    /// Unity: `ClearAllStems()` (lines 95-105).
    pub fn clear_all_stems(&mut self) {
        self.expanded = false;
        for lane in &mut self.lanes {
            lane.renderer.clear();
            lane.is_muted = false;
            lane.is_soloed = false;
        }
        self.dirty = true;
    }

    /// Unity: `SetMuteState(int index, bool muted)` (lines 107-110).
    pub fn set_mute_state(&mut self, index: usize, muted: bool) {
        if index < STEM_COUNT {
            self.lanes[index].is_muted = muted;
            self.dirty = true;
        }
    }

    /// Unity: `SetSoloState(int index, bool soloed)` (lines 112-116).
    pub fn set_solo_state(&mut self, index: usize, soloed: bool) {
        if index < STEM_COUNT {
            self.lanes[index].is_soloed = soloed;
            self.dirty = true;
        }
    }

    /// Get clip duration for a stem (for beat computation).
    pub fn stem_clip_duration_seconds(&self, index: usize) -> f32 {
        if index < STEM_COUNT {
            self.lanes[index].renderer.clip_duration_seconds()
        } else {
            0.0
        }
    }

    /// Build UITree nodes for interactive elements (overlays + buttons + labels).
    /// Called from UIRoot after viewport.build() so screen_rect is available.
    pub fn build_nodes(&mut self, tree: &mut UITree, screen_rect: Rect) {
        let lane_h = color::STEM_LANE_HEIGHT;

        #[allow(clippy::needless_range_loop)] // index used for lane_nodes[] and positioning
        for i in 0..STEM_COUNT {
            let lane_y = screen_rect.y + i as f32 * lane_h;

            // Transparent scrub overlay covering entire stem lane.
            self.lane_nodes[i].overlay_id = tree.add_button(
                -1,
                screen_rect.x,
                lane_y,
                screen_rect.width,
                lane_h,
                UIStyle { bg_color: Color32::TRANSPARENT, ..UIStyle::default() },
                "",
            ) as i32;

            // Stem name label (top-left header area).
            // Unity: fontSize=9, MiddleLeft, color=(0.65, 0.65, 0.65).
            self.lane_nodes[i].name_label_id = tree.add_label(
                -1,
                screen_rect.x + HEADER_X,
                lane_y + 2.0,
                52.0,
                lane_h / 2.0 - 2.0,
                STEM_NAMES[i],
                UIStyle {
                    text_color: Color32::new(166, 166, 166, 255),
                    font_size: color::FONT_SMALL,
                    font_weight: FontWeight::Regular,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            ) as i32;

            // Mute button ("M") — bottom half of header area.
            let btn_y = lane_y + lane_h / 2.0 + 1.0;
            self.lane_nodes[i].mute_btn_id = tree.add_button(
                -1,
                screen_rect.x + HEADER_X,
                btn_y,
                MUTE_SOLO_BTN_W,
                MUTE_SOLO_BTN_H,
                mute_btn_style(false),
                "M",
            ) as i32;

            // Solo button ("S") — next to mute.
            self.lane_nodes[i].solo_btn_id = tree.add_button(
                -1,
                screen_rect.x + HEADER_X + MUTE_SOLO_BTN_W + BUTTON_SPACING,
                btn_y,
                MUTE_SOLO_BTN_W,
                MUTE_SOLO_BTN_H,
                solo_btn_style(false),
                "S",
            ) as i32;
        }

        // Initially hidden (shown when expanded).
        self.apply_node_visibility(tree);
    }

    /// Update UITree node visibility and mute/solo colors each frame.
    pub fn update_nodes(&mut self, tree: &mut UITree) {
        self.apply_node_visibility(tree);

        // Update mute/solo button colors based on state.
        for (i, lane) in self.lanes.iter().enumerate() {
            let ids = &self.lane_nodes[i];
            if ids.mute_btn_id >= 0 {
                tree.set_style(ids.mute_btn_id as u32, mute_btn_style(lane.is_muted));
            }
            if ids.solo_btn_id >= 0 {
                tree.set_style(ids.solo_btn_id as u32, solo_btn_style(lane.is_soloed));
            }
        }
    }

    fn apply_node_visibility(&self, tree: &mut UITree) {
        for ids in &self.lane_nodes {
            let vis = self.expanded;
            if ids.overlay_id >= 0 {
                tree.set_visible(ids.overlay_id as u32, vis);
            }
            if ids.name_label_id >= 0 {
                tree.set_visible(ids.name_label_id as u32, vis);
            }
            if ids.mute_btn_id >= 0 {
                tree.set_visible(ids.mute_btn_id as u32, vis);
            }
            if ids.solo_btn_id >= 0 {
                tree.set_visible(ids.solo_btn_id as u32, vis);
            }
        }
    }

    /// Returns true if the given node_id belongs to this panel.
    pub fn owns_node(&self, node_id: i32) -> bool {
        for ids in &self.lane_nodes {
            if node_id == ids.overlay_id
                || node_id == ids.name_label_id
                || node_id == ids.mute_btn_id
                || node_id == ids.solo_btn_id
            {
                return true;
            }
        }
        false
    }

    /// Update overlay state each frame.
    ///
    /// Unity: `UpdateOverlay(...)` (lines 119-137).
    pub fn update_overlay(
        &mut self,
        waveform_start_beat: f32,
        playhead_beat: f32,
        scroll_offset_x: f32,
        bpm: f32,
        _mapper: &CoordinateMapper,
    ) {
        if !self.expanded {
            return;
        }

        let changed = (self.waveform_start_beat - waveform_start_beat).abs() > 0.001
            || (self.playhead_beat - playhead_beat).abs() > 0.001
            || (self.scroll_offset_x - scroll_offset_x).abs() > 0.5
            || (self.bpm - bpm).abs() > 0.01;

        if changed {
            self.waveform_start_beat = waveform_start_beat;
            self.playhead_beat = playhead_beat;
            self.scroll_offset_x = scroll_offset_x;
            self.bpm = bpm;
            self.dirty = true;
        }
    }

    /// Repaint the pixel buffer (waveform visuals + playhead, no buttons).
    pub fn repaint(&mut self, viewport_width: usize, mapper: &CoordinateMapper) {
        if !self.expanded {
            self.buffer_width = 0;
            self.buffer_height = 0;
            self.pixel_buffer.clear();
            self.dirty = false;
            return;
        }

        let lane_h = color::STEM_LANE_HEIGHT as usize;
        let total_h = STEM_COUNT * lane_h;

        if self.buffer_width != viewport_width || self.buffer_height != total_h {
            self.buffer_width = viewport_width;
            self.buffer_height = total_h;
            self.pixel_buffer
                .resize(viewport_width * total_h, Color32::TRANSPARENT);
        }

        let buf_w = self.buffer_width;
        let buf_h = self.buffer_height;

        // Draw each stem lane
        for (i, lane) in self.lanes.iter().enumerate() {
            let y_offset = (i * lane_h) as i32;

            // Lane background
            fill_rect(
                &mut self.pixel_buffer,
                buf_w,
                buf_h,
                0,
                y_offset,
                buf_w as i32,
                lane_h as i32,
                STEM_BG_COLORS[i],
            );

            // Draw waveform if this stem has audio
            if lane.renderer.is_ready() && lane.renderer.clip_duration_seconds() > 0.0 {
                let waveform_x =
                    mapper.beat_to_pixel_absolute(self.waveform_start_beat.max(0.0));

                let stem_width = mapper.beat_duration_to_width(
                    self.waveform_duration_beats_for_stem(i),
                );

                if stem_width > 0.0
                    && let Some(level) = lane.renderer.select_level_for_zoom(stem_width, 1.0) {
                        let draw_left = (waveform_x - self.scroll_offset_x) as i32;
                        let draw_right =
                            ((waveform_x + stem_width - self.scroll_offset_x) as i32)
                                .min(buf_w as i32);
                        let x_start = draw_left.max(0);
                        let x_end = draw_right.min(buf_w as i32);

                        if x_end > x_start {
                            waveform_painter::draw_waveform(
                                &mut self.pixel_buffer,
                                buf_w,
                                buf_h,
                                level,
                                x_start,
                                x_end,
                                y_offset,
                                lane_h as i32,
                                waveform_x - self.scroll_offset_x,
                                stem_width,
                            );
                        }
                    }
            }

            // Draw playhead
            let playhead_x =
                (mapper.beat_to_pixel_absolute(self.playhead_beat) - self.scroll_offset_x) as i32;
            if playhead_x >= 0 && playhead_x < buf_w as i32 {
                waveform_painter::draw_playhead(
                    &mut self.pixel_buffer,
                    buf_w,
                    buf_h,
                    playhead_x,
                    y_offset,
                    lane_h as i32,
                    PLAYHEAD_COLOR,
                    color::PLAYHEAD_WIDTH as i32,
                );
            }

            // Mute/Solo buttons are UITree nodes — not drawn in the bitmap.
        }

        self.dirty = false;
    }

    /// Compute beat duration for a stem from its audio length.
    fn waveform_duration_beats_for_stem(&self, index: usize) -> f32 {
        if index >= STEM_COUNT {
            return 0.0;
        }
        let dur_sec = self.lanes[index].renderer.clip_duration_seconds();
        if dur_sec <= 0.0 || self.bpm <= 0.0 {
            return 0.0;
        }
        dur_sec * (self.bpm / 60.0)
    }
}

impl Panel for StemLaneGroupPanel {
    fn build(&mut self, _tree: &mut UITree, _layout: &ScreenLayout) {
        self.dirty = true;
    }

    fn update(&mut self, _tree: &mut UITree) {
        // State updates pushed via update_overlay() and update_nodes().
    }

    fn handle_event(&mut self, event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        let mut actions = Vec::new();
        if !self.expanded {
            return actions;
        }

        match event {
            UIEvent::Click { node_id, pos, .. } => {
                let id = *node_id as i32;
                // Check mute/solo buttons by node ID.
                for (i, ids) in self.lane_nodes.iter().enumerate() {
                    if id == ids.mute_btn_id {
                        actions.push(PanelAction::StemMuteToggled(i));
                        return actions;
                    }
                    if id == ids.solo_btn_id {
                        actions.push(PanelAction::StemSoloToggled(i));
                        return actions;
                    }
                }
                // Click on overlay → scrub.
                actions.push(PanelAction::WaveformScrub(pos.x, pos.y));
            }
            UIEvent::PointerDown { node_id, pos, .. } => {
                let id = *node_id as i32;
                // Scrub only on overlay nodes (not on buttons).
                for ids in &self.lane_nodes {
                    if id == ids.overlay_id {
                        actions.push(PanelAction::WaveformScrub(pos.x, pos.y));
                        return actions;
                    }
                }
            }
            UIEvent::Drag { pos, .. } => {
                // Continuous scrub while dragging.
                actions.push(PanelAction::WaveformScrub(pos.x, pos.y));
            }
            _ => {}
        }

        actions
    }
}

/// Button style for mute toggle.
fn mute_btn_style(active: bool) -> UIStyle {
    let bg = if active { color::MUTE_BTN_ACTIVE } else { color::MUTE_SOLO_BTN_INACTIVE };
    UIStyle {
        bg_color: bg,
        hover_bg_color: bg, // manual color control, no hover transition
        pressed_bg_color: bg,
        text_color: Color32::new(255, 255, 255, 255),
        font_size: color::FONT_SMALL,
        font_weight: FontWeight::Bold,
        corner_radius: color::BUTTON_RADIUS,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}

/// Button style for solo toggle.
fn solo_btn_style(active: bool) -> UIStyle {
    let bg = if active { color::SOLO_BTN_ACTIVE } else { color::MUTE_SOLO_BTN_INACTIVE };
    UIStyle {
        bg_color: bg,
        hover_bg_color: bg,
        pressed_bg_color: bg,
        text_color: Color32::new(255, 255, 255, 255),
        font_size: color::FONT_SMALL,
        font_weight: FontWeight::Bold,
        corner_radius: color::BUTTON_RADIUS,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}
