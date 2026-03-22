//! Imported audio waveform lane panel.
//!
//! Hybrid bitmap + UITree node architecture (matching Unity's pattern):
//! - **Bitmap** (pixel buffer): waveform spectral bars, playhead, lane background
//! - **UITree nodes**: transparent scrub/drag overlay, buttons with text labels
//!
//! Unity: `ImportedAudioWaveformLane` + `ImportedAudioWaveformScrubHandler`
//!        + `ImportedAudioWaveformDragHandler`.

use manifold_core::ClipId;

use crate::color;
use crate::coordinate_mapper::CoordinateMapper;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::{Color32, Rect, UIStyle, TextAlign, FontWeight};
use crate::tree::UITree;
use crate::waveform_painter;
use crate::waveform_renderer::WaveformRenderer;

use super::{Panel, PanelAction};

/// Imported audio waveform lane — displays the master audio waveform
/// with spectral coloring, playhead, and overlay buttons.
///
/// Unity: `ImportedAudioWaveformLane` (457 lines).
pub struct WaveformLanePanel {
    // ── Waveform data ──
    renderer: WaveformRenderer,
    has_audio: bool,

    // ── Pixel buffer ──
    pub pixel_buffer: Vec<Color32>,
    pub buffer_width: usize,
    pub buffer_height: usize,
    pub dirty: bool,

    // ── Layout state ──
    waveform_start_beat: f32,
    waveform_duration_beats: f32,
    playhead_beat: f32,
    scroll_offset_x: f32,
    content_width: f32,
    visible: bool,

    // ── Stems state (from ImportedAudioWaveformLane) ──
    stems_expanded: bool,
    stems_available: bool,

    // ── Drag handler state (ImportedAudioWaveformDragHandler.cs) ──
    is_dragging: bool,
    accumulated_beats: f32,
    total_snapped_delta: f32,
    /// Pre-drag audio_start_beat — captured on first DragDelta for undo.
    drag_start_beat: Option<f32>,
    /// Snapshot of ALL clips before drag: (clip_id, original start_beat, layer_index).
    /// Unity: `waveformDragClipSnapshots` (EditingService.cs line 1367).
    pub waveform_drag_clip_snapshots: Vec<(ClipId, f32, i32)>,

    // ── Scrub handler state (ImportedAudioWaveformScrubHandler.cs) ──
    is_scrubbing: bool,

    // ── UITree node IDs (interactive overlay + buttons) ──
    overlay_id: i32,
    remove_btn_id: i32,
    expand_btn_id: i32,
    reanalyze_ids: [i32; 5],

    // ── Cached values for dirty checking ──
    cached_waveform_x: f32,
    cached_waveform_width: f32,
    cached_playhead_x: f32,
    cached_scroll_offset: f32,
}

/// Playhead color: PlayheadRed @ 0.85 alpha.
/// Unity: `ImportedAudioWaveformLane.PlayheadColor` (lines 57-60).
const PLAYHEAD_COLOR: Color32 = Color32::new(217, 64, 56, 217);

/// Snap step for waveform drag: 1 beat.
/// Unity: `ImportedAudioWaveformDragHandler.SnapStepBeats = 1f` (line 22).
const SNAP_STEP_BEATS: f32 = 1.0;

// ── Button layout constants (from Unity BuildUI) ──
const REMOVE_BTN_W: f32 = 20.0;
const REMOVE_BTN_H: f32 = 16.0;
const REMOVE_BTN_MARGIN_RIGHT: f32 = 4.0;
const REMOVE_BTN_MARGIN_TOP: f32 = 2.0;

const EXPAND_BTN_W: f32 = 20.0;
const EXPAND_BTN_H: f32 = 16.0;
const EXPAND_BTN_MARGIN_RIGHT: f32 = 28.0;
const EXPAND_BTN_MARGIN_TOP: f32 = 2.0;

const REANALYZE_BTN_H: f32 = 16.0;
const REANALYZE_BTN_MARGIN_LEFT: f32 = 4.0;
const REANALYZE_BTN_MARGIN_TOP: f32 = 2.0;
const REANALYZE_BTN_SPACING: f32 = 3.0;

/// Re-analyze button definitions: (label, width).
/// Unity: CreateReAnalyzeButton calls (lines 272-276).
const REANALYZE_BUTTONS: [(&str, f32); 5] = [
    ("DRUMS", 48.0),
    ("BASS", 40.0),
    ("SYNTH", 46.0),
    ("VOCAL", 48.0),
    ("STEMS", 48.0),
];

/// Button style for re-analyze buttons (matches Unity WaveformButtonNormal/Highlighted/Pressed).
fn reanalyze_btn_style() -> UIStyle {
    UIStyle {
        bg_color: color::WAVEFORM_BTN_NORMAL,
        hover_bg_color: color::WAVEFORM_BTN_HIGHLIGHTED,
        pressed_bg_color: color::WAVEFORM_BTN_PRESSED,
        text_color: Color32::new(173, 173, 179, 255), // Unity: DropdownInactiveText
        font_size: color::FONT_SMALL,
        font_weight: FontWeight::Regular,
        corner_radius: color::BUTTON_RADIUS,
        text_align: TextAlign::Center,
        ..UIStyle::default()
    }
}

impl Default for WaveformLanePanel {
    fn default() -> Self {
        Self::new()
    }
}

impl WaveformLanePanel {
    pub fn new() -> Self {
        Self {
            renderer: WaveformRenderer::new(),
            has_audio: false,
            pixel_buffer: Vec::new(),
            buffer_width: 0,
            buffer_height: 0,
            dirty: true,
            waveform_start_beat: 0.0,
            waveform_duration_beats: 0.0,
            playhead_beat: 0.0,
            scroll_offset_x: 0.0,
            content_width: 0.0,
            visible: true,
            stems_expanded: false,
            stems_available: false,
            is_dragging: false,
            accumulated_beats: 0.0,
            total_snapped_delta: 0.0,
            drag_start_beat: None,
            waveform_drag_clip_snapshots: Vec::new(),
            is_scrubbing: false,
            overlay_id: -1,
            remove_btn_id: -1,
            expand_btn_id: -1,
            reanalyze_ids: [-1; 5],
            cached_waveform_x: f32::NAN,
            cached_waveform_width: -1.0,
            cached_playhead_x: f32::NAN,
            cached_scroll_offset: f32::NAN,
        }
    }

    /// Set audio data from raw PCM samples.
    /// Called when audio is loaded/changed.
    ///
    /// Unity: `SetAudioClip(AudioClip clip)` (lines 336-373).
    pub fn set_audio_data(&mut self, samples: &[f32], channels: usize, sample_rate: u32) {
        self.renderer.set_audio_data(samples, channels, sample_rate);
        self.has_audio = self.renderer.is_ready();
        self.dirty = true;
    }

    /// Clear audio data.
    pub fn clear_audio(&mut self) {
        self.renderer.clear();
        self.has_audio = false;
        self.dirty = true;
    }

    /// Get the clip duration in seconds (for beat duration computation).
    pub fn clip_duration_seconds(&self) -> f32 {
        self.renderer.clip_duration_seconds()
    }

    pub fn is_ready(&self) -> bool {
        self.renderer.is_ready()
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn set_visible(&mut self, visible: bool) {
        if self.visible != visible {
            self.visible = visible;
            self.dirty = true;
        }
    }

    /// Unity: `SetStemsAvailable(bool available)` (lines 284-289).
    pub fn set_stems_available(&mut self, available: bool) {
        self.stems_available = available;
    }

    /// Unity: `SetExpandedState(bool expanded)` (lines 294-299).
    pub fn set_expanded_state(&mut self, expanded: bool) {
        self.stems_expanded = expanded;
    }

    pub fn stems_expanded(&self) -> bool {
        self.stems_expanded
    }

    pub fn stems_available(&self) -> bool {
        self.stems_available
    }

    /// Build UITree nodes for interactive elements (overlay + buttons).
    /// Called from UIRoot after viewport.build() so wf_rect is available.
    pub fn build_nodes(&mut self, tree: &mut UITree, screen_rect: Rect) {
        // Transparent scrub/drag overlay covering entire waveform area.
        // This is the hit-test target that makes the input system generate events.
        // Unity: DragOverlay (transparent Image, raycastTarget=true).
        self.overlay_id = tree.add_button(
            -1,
            screen_rect.x,
            screen_rect.y,
            screen_rect.width,
            screen_rect.height,
            UIStyle { bg_color: Color32::TRANSPARENT, ..UIStyle::default() },
            "",
        ) as i32;

        // Remove button (top-right). Unity: anchoredPosition(-4, -2), 20×16.
        let remove_x = screen_rect.x + screen_rect.width - REMOVE_BTN_MARGIN_RIGHT - REMOVE_BTN_W;
        let remove_y = screen_rect.y + REMOVE_BTN_MARGIN_TOP;
        self.remove_btn_id = tree.add_button(
            -1,
            remove_x,
            remove_y,
            REMOVE_BTN_W,
            REMOVE_BTN_H,
            UIStyle {
                bg_color: color::WAVEFORM_BTN_NORMAL,
                hover_bg_color: color::WAVEFORM_REMOVE_HIGHLIGHTED,
                pressed_bg_color: color::WAVEFORM_REMOVE_PRESSED,
                text_color: Color32::new(209, 209, 214, 255),
                font_size: color::FONT_SMALL,
                corner_radius: color::BUTTON_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            "X",
        ) as i32;

        // Expand stems button (next to remove). Unity: anchoredPosition(-28, -2).
        let expand_x = screen_rect.x + screen_rect.width
            - EXPAND_BTN_MARGIN_RIGHT - EXPAND_BTN_W;
        let expand_y = screen_rect.y + EXPAND_BTN_MARGIN_TOP;
        self.expand_btn_id = tree.add_button(
            -1,
            expand_x,
            expand_y,
            EXPAND_BTN_W,
            EXPAND_BTN_H,
            UIStyle {
                bg_color: color::WAVEFORM_BTN_NORMAL,
                hover_bg_color: color::WAVEFORM_EXPAND_HIGHLIGHTED,
                pressed_bg_color: color::WAVEFORM_EXPAND_PRESSED,
                text_color: Color32::new(191, 191, 191, 255),
                font_size: color::FONT_SMALL,
                corner_radius: color::BUTTON_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            "\u{25B6}", // ▶ right-pointing triangle (collapsed)
        ) as i32;

        // Re-analyze buttons (top-left). Unity: HorizontalLayoutGroup, spacing=3.
        let style = reanalyze_btn_style();
        let mut btn_x = screen_rect.x + REANALYZE_BTN_MARGIN_LEFT;
        for (i, &(label, width)) in REANALYZE_BUTTONS.iter().enumerate() {
            self.reanalyze_ids[i] = tree.add_button(
                -1,
                btn_x,
                screen_rect.y + REANALYZE_BTN_MARGIN_TOP,
                width,
                REANALYZE_BTN_H,
                style,
                label,
            ) as i32;
            btn_x += width + REANALYZE_BTN_SPACING;
        }

        // Set initial visibility based on current state.
        self.apply_button_visibility(tree);
    }

    /// Update UITree node visibility and text each frame.
    pub fn update_nodes(&mut self, tree: &mut UITree) {
        self.apply_button_visibility(tree);

        // Update expand chevron direction.
        if self.expand_btn_id >= 0 {
            let chevron = if self.stems_expanded { "\u{25BC}" } else { "\u{25B6}" };
            tree.set_text(self.expand_btn_id as u32, chevron);
        }
    }

    fn apply_button_visibility(&self, tree: &mut UITree) {
        if self.remove_btn_id >= 0 {
            tree.set_visible(self.remove_btn_id as u32, self.has_audio);
        }
        if self.expand_btn_id >= 0 {
            tree.set_visible(
                self.expand_btn_id as u32,
                self.has_audio && self.stems_available,
            );
        }
        for &id in &self.reanalyze_ids {
            if id >= 0 {
                tree.set_visible(id as u32, self.has_audio);
            }
        }
    }

    /// Update overlay state each frame.
    ///
    /// Unity: `UpdateOverlay(...)` (lines 375-442).
    pub fn update_overlay(
        &mut self,
        waveform_start_beat: f32,
        waveform_duration_beats: f32,
        playhead_beat: f32,
        scroll_offset_x: f32,
        content_width: f32,
        mapper: &CoordinateMapper,
    ) {
        self.waveform_start_beat = waveform_start_beat;
        self.waveform_duration_beats = waveform_duration_beats;
        self.playhead_beat = playhead_beat;
        self.scroll_offset_x = scroll_offset_x;
        self.content_width = content_width;

        // Check if anything changed that requires repaint
        let has_waveform = self.renderer.is_ready()
            && waveform_duration_beats > 0.0
            && self.renderer.clip_duration_seconds() > 0.0;

        if has_waveform {
            let waveform_x =
                mapper.beat_to_pixel_absolute(waveform_start_beat.max(0.0));
            let waveform_width =
                mapper.beat_duration_to_width(waveform_duration_beats).max(1.0);
            let playhead_x =
                mapper.beat_to_pixel_absolute(playhead_beat);

            // Dirty check (Unity: Mathf.Approximately comparisons, lines 418-441)
            if (waveform_x - self.cached_waveform_x).abs() > 0.5
                || (waveform_width - self.cached_waveform_width).abs() > 0.5
                || (playhead_x - self.cached_playhead_x).abs() > 0.5
                || (scroll_offset_x - self.cached_scroll_offset).abs() > 0.5
            {
                self.cached_waveform_x = waveform_x;
                self.cached_waveform_width = waveform_width;
                self.cached_playhead_x = playhead_x;
                self.cached_scroll_offset = scroll_offset_x;
                self.dirty = true;
            }
        }
    }

    /// Repaint the pixel buffer (waveform visual + playhead only, no buttons).
    pub fn repaint(&mut self, viewport_width: usize) {
        let lane_height = color::WAVEFORM_LANE_HEIGHT as usize;

        // Ensure buffer is correct size
        if self.buffer_width != viewport_width || self.buffer_height != lane_height {
            self.buffer_width = viewport_width;
            self.buffer_height = lane_height;
            self.pixel_buffer
                .resize(viewport_width * lane_height, Color32::TRANSPARENT);
        }

        // Clear buffer with lane background
        let bg = color::WAVEFORM_LANE_BG;
        for px in self.pixel_buffer.iter_mut() {
            *px = bg;
        }

        let buf_w = self.buffer_width;
        let buf_h = self.buffer_height;

        if !self.has_audio || !self.renderer.is_ready() {
            self.dirty = false;
            return;
        }

        // Draw waveform
        let waveform_x = self.cached_waveform_x;
        let waveform_width = self.cached_waveform_width;

        if waveform_width > 0.0
            && let Some(level) = self.renderer.select_level_for_zoom(waveform_width, 1.0) {
                // Clamp drawing to visible region
                let draw_left = (waveform_x - self.scroll_offset_x) as i32;
                let draw_right =
                    ((waveform_x + waveform_width - self.scroll_offset_x) as i32).min(buf_w as i32);

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
                        0,
                        buf_h as i32,
                        waveform_x - self.scroll_offset_x,
                        waveform_width,
                    );
                }
            }

        // Draw playhead
        let playhead_screen_x = (self.cached_playhead_x - self.scroll_offset_x) as i32;
        if playhead_screen_x >= 0 && playhead_screen_x < buf_w as i32 {
            waveform_painter::draw_playhead(
                &mut self.pixel_buffer,
                buf_w,
                buf_h,
                playhead_screen_x,
                0,
                buf_h as i32,
                PLAYHEAD_COLOR,
                color::PLAYHEAD_WIDTH as i32,
            );
        }

        // Buttons are UITree nodes — not drawn in the bitmap.

        self.dirty = false;
    }
}

impl Panel for WaveformLanePanel {
    fn build(&mut self, _tree: &mut UITree, _layout: &ScreenLayout) {
        // Node creation happens in build_nodes() called from UIRoot,
        // after viewport.build() provides the screen rect.
        self.dirty = true;
    }

    fn update(&mut self, _tree: &mut UITree) {
        // State updates are pushed via update_overlay() and update_nodes().
    }

    fn handle_event(&mut self, event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        let mut actions = Vec::new();

        match event {
            UIEvent::Click { node_id, pos, .. } => {
                let id = *node_id as i32;

                // Button clicks (matched by node ID).
                if id == self.remove_btn_id {
                    actions.push(PanelAction::RemoveAudioClicked);
                } else if id == self.expand_btn_id {
                    actions.push(PanelAction::ExpandStemsToggled(!self.stems_expanded));
                } else if id == self.reanalyze_ids[0] {
                    actions.push(PanelAction::ReAnalyzeDrums);
                } else if id == self.reanalyze_ids[1] {
                    actions.push(PanelAction::ReAnalyzeBass);
                } else if id == self.reanalyze_ids[2] {
                    actions.push(PanelAction::ReAnalyzeSynth);
                } else if id == self.reanalyze_ids[3] {
                    actions.push(PanelAction::ReAnalyzeVocal);
                } else if id == self.reanalyze_ids[4] {
                    actions.push(PanelAction::ReImportStems);
                } else if !self.has_audio {
                    // No audio: entire lane is import target.
                    actions.push(PanelAction::ImportAudioClicked);
                } else {
                    // Click on overlay (waveform area) → scrub.
                    actions.push(PanelAction::WaveformScrub(pos.x, pos.y));
                }
            }
            UIEvent::PointerDown { node_id, pos, .. } => {
                let id = *node_id as i32;
                // Start scrub only on the overlay (not on buttons).
                if id == self.overlay_id && self.has_audio {
                    self.is_scrubbing = true;
                    actions.push(PanelAction::WaveformScrub(pos.x, pos.y));
                }
            }
            UIEvent::DragBegin { node_id, .. } => {
                let id = *node_id as i32;
                // Start waveform drag only on the overlay.
                // Drag takes priority over scrub — stop scrubbing.
                if id == self.overlay_id && self.has_audio {
                    self.is_dragging = true;
                    self.is_scrubbing = false;
                    self.accumulated_beats = 0.0;
                    self.total_snapped_delta = 0.0;
                }
            }
            UIEvent::Drag { delta, pos, .. } => {
                // Handle scrub drag (only when not in waveform-offset drag)
                if self.is_scrubbing && !self.is_dragging {
                    actions.push(PanelAction::WaveformScrub(pos.x, pos.y));
                }

                // Handle waveform drag (ImportedAudioWaveformDragHandler.OnDrag)
                if self.is_dragging {
                    let delta_beats = delta.x / self.pixels_per_beat();
                    self.accumulated_beats += delta_beats;

                    // Snap: emit in whole-beat increments
                    let snapped = (self.accumulated_beats / SNAP_STEP_BEATS) as i32 as f32
                        * SNAP_STEP_BEATS;
                    if snapped.abs() >= SNAP_STEP_BEATS {
                        actions.push(PanelAction::WaveformDragDelta(snapped));
                        self.total_snapped_delta += snapped;
                        self.accumulated_beats -= snapped;
                    }
                }
            }
            UIEvent::DragEnd { .. } => {
                if self.is_dragging {
                    actions.push(PanelAction::WaveformDragEnd(self.total_snapped_delta));
                    self.is_dragging = false;
                    self.accumulated_beats = 0.0;
                    self.total_snapped_delta = 0.0;
                }
            }
            UIEvent::PointerUp { .. } => {
                if self.is_scrubbing {
                    self.is_scrubbing = false;
                }
            }
            _ => {}
        }

        actions
    }
}

impl WaveformLanePanel {
    /// True when the panel has an active scrub or drag in progress.
    pub fn is_interacting(&self) -> bool {
        self.is_scrubbing || self.is_dragging
    }

    /// Returns true if the given node_id belongs to this panel.
    pub fn owns_node(&self, node_id: i32) -> bool {
        node_id == self.overlay_id
            || node_id == self.remove_btn_id
            || node_id == self.expand_btn_id
            || self.reanalyze_ids.contains(&node_id)
    }

    /// Whether drag_start_beat has been captured (first delta already happened).
    pub fn has_drag_start_beat(&self) -> bool {
        self.drag_start_beat.is_some()
    }

    /// Capture the pre-drag audio start beat for undo (first call only).
    pub fn set_drag_start_beat(&mut self, beat: f32) {
        if self.drag_start_beat.is_none() {
            self.drag_start_beat = Some(beat);
        }
    }

    /// Take and clear the pre-drag start beat (called on drag end).
    pub fn take_drag_start_beat(&mut self) -> Option<f32> {
        self.drag_start_beat.take()
    }

    /// Current pixels per beat from cached mapper state.
    fn pixels_per_beat(&self) -> f32 {
        if self.waveform_duration_beats > 0.0 && self.cached_waveform_width > 0.0 {
            self.cached_waveform_width / self.waveform_duration_beats
        } else {
            120.0 // default ppb
        }
    }
}
