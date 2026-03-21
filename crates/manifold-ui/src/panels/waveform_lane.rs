//! Imported audio waveform lane panel.
//!
//! Mechanical translation of `Assets/Scripts/UI/Timeline/ImportedAudioWaveformLane.cs`
//! adapted to the Rust bitmap UI architecture.
//!
//! In Unity this creates RawImage tiles via WaveformRenderer and positions them
//! with RectTransforms. In Rust we paint the waveform directly into a pixel buffer
//! using `waveform_painter`.
//!
//! Also ports the scrub handler (`ImportedAudioWaveformScrubHandler.cs`) and
//! drag handler (`ImportedAudioWaveformDragHandler.cs`) as inline state.

use crate::color;
use crate::coordinate_mapper::CoordinateMapper;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::Color32;
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

    // ── Scrub handler state (ImportedAudioWaveformScrubHandler.cs) ──
    is_scrubbing: bool,

    // ── Button hover tracking ──
    hovered_button: Option<WaveformButton>,

    // ── Cached values for dirty checking ──
    cached_waveform_x: f32,
    cached_waveform_width: f32,
    cached_playhead_x: f32,
    cached_scroll_offset: f32,
}

/// Button regions in the waveform lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveformButton {
    Import,
    Remove,
    Expand,
    ReAnalyzeDrums,
    ReAnalyzeBass,
    ReAnalyzeSynth,
    ReAnalyzeVocal,
    ReImportStems,
}

/// Playhead color: PlayheadRed @ 0.85 alpha.
/// Unity: `ImportedAudioWaveformLane.PlayheadColor` (lines 57-60).
const PLAYHEAD_COLOR: Color32 = Color32::new(217, 64, 56, 217);

/// Snap step for waveform drag: 1 beat.
/// Unity: `ImportedAudioWaveformDragHandler.SnapStepBeats = 1f` (line 22).
const SNAP_STEP_BEATS: f32 = 1.0;

// ── Button layout constants (from Unity BuildUI) ──
const REMOVE_BTN_W: i32 = 20;
const REMOVE_BTN_H: i32 = 16;
const REMOVE_BTN_MARGIN_RIGHT: i32 = 4;
const REMOVE_BTN_MARGIN_TOP: i32 = 2;

const EXPAND_BTN_W: i32 = 20;
const EXPAND_BTN_H: i32 = 16;
const EXPAND_BTN_MARGIN_RIGHT: i32 = 28; // Unity: anchoredPosition.x = -28
const EXPAND_BTN_MARGIN_TOP: i32 = 2;

const REANALYZE_BTN_H: i32 = 16;
const REANALYZE_BTN_MARGIN_LEFT: i32 = 4;
const REANALYZE_BTN_MARGIN_TOP: i32 = 2;
const REANALYZE_BTN_SPACING: i32 = 3;

/// Re-analyze button definitions: (label, width).
/// Unity: CreateReAnalyzeButton calls (lines 272-276).
const REANALYZE_BUTTONS: [(&str, i32); 5] = [
    ("DRUMS", 48),
    ("BASS", 40),
    ("SYNTH", 46),
    ("VOCAL", 48),
    ("STEMS", 48),
];

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
            is_scrubbing: false,
            hovered_button: None,
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

    /// Repaint the pixel buffer.
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
            // Empty state — just the background (text "Click to import audio" is
            // drawn by the text system in update())
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

        // Draw overlay buttons when audio is loaded
        self.draw_buttons(buf_w, buf_h);

        self.dirty = false;
    }

    /// Draw overlay buttons (remove, expand, reanalyze).
    fn draw_buttons(&mut self, buf_w: usize, buf_h: usize) {
        if !self.has_audio {
            return;
        }

        // Remove button (top-right, Unity lines 179-215)
        let remove_x = buf_w as i32 - REMOVE_BTN_MARGIN_RIGHT - REMOVE_BTN_W;
        let remove_y = REMOVE_BTN_MARGIN_TOP;
        let is_remove_hovered = self.hovered_button == Some(WaveformButton::Remove);
        waveform_painter::draw_waveform_button(
            &mut self.pixel_buffer,
            buf_w,
            buf_h,
            remove_x,
            remove_y,
            REMOVE_BTN_W,
            REMOVE_BTN_H,
            color::WAVEFORM_BTN_NORMAL,
            is_remove_hovered,
            false,
            color::WAVEFORM_REMOVE_HIGHLIGHTED,
            color::WAVEFORM_REMOVE_PRESSED,
        );

        // Expand stems button (next to remove, Unity lines 218-254)
        if self.stems_available {
            let expand_x = buf_w as i32 - EXPAND_BTN_MARGIN_RIGHT - EXPAND_BTN_W;
            let expand_y = EXPAND_BTN_MARGIN_TOP;
            let is_expand_hovered = self.hovered_button == Some(WaveformButton::Expand);
            waveform_painter::draw_waveform_button(
                &mut self.pixel_buffer,
                buf_w,
                buf_h,
                expand_x,
                expand_y,
                EXPAND_BTN_W,
                EXPAND_BTN_H,
                color::WAVEFORM_BTN_NORMAL,
                is_expand_hovered,
                false,
                color::WAVEFORM_EXPAND_HIGHLIGHTED,
                color::WAVEFORM_EXPAND_PRESSED,
            );
        }

        // Re-analyze buttons (top-left, Unity lines 257-278)
        let mut btn_x = REANALYZE_BTN_MARGIN_LEFT;
        for (i, &(_label, width)) in REANALYZE_BUTTONS.iter().enumerate() {
            let btn_type = match i {
                0 => WaveformButton::ReAnalyzeDrums,
                1 => WaveformButton::ReAnalyzeBass,
                2 => WaveformButton::ReAnalyzeSynth,
                3 => WaveformButton::ReAnalyzeVocal,
                4 => WaveformButton::ReImportStems,
                _ => continue,
            };
            let is_hovered = self.hovered_button == Some(btn_type);
            waveform_painter::draw_waveform_button(
                &mut self.pixel_buffer,
                buf_w,
                buf_h,
                btn_x,
                REANALYZE_BTN_MARGIN_TOP,
                width,
                REANALYZE_BTN_H,
                color::WAVEFORM_BTN_NORMAL,
                is_hovered,
                false,
                color::WAVEFORM_BTN_HIGHLIGHTED,
                color::WAVEFORM_BTN_PRESSED,
            );
            btn_x += width + REANALYZE_BTN_SPACING;
        }
    }

    /// Hit-test a screen position against buttons.
    fn hit_test_button(&self, local_x: f32, local_y: f32) -> Option<WaveformButton> {
        if !self.has_audio {
            // When no audio, the whole lane is the import button
            return Some(WaveformButton::Import);
        }

        let buf_w = self.buffer_width as f32;

        // Remove button
        let remove_x = buf_w - REMOVE_BTN_MARGIN_RIGHT as f32 - REMOVE_BTN_W as f32;
        let remove_y = REMOVE_BTN_MARGIN_TOP as f32;
        if local_x >= remove_x
            && local_x < remove_x + REMOVE_BTN_W as f32
            && local_y >= remove_y
            && local_y < remove_y + REMOVE_BTN_H as f32
        {
            return Some(WaveformButton::Remove);
        }

        // Expand button
        if self.stems_available {
            let expand_x = buf_w - EXPAND_BTN_MARGIN_RIGHT as f32 - EXPAND_BTN_W as f32;
            let expand_y = EXPAND_BTN_MARGIN_TOP as f32;
            if local_x >= expand_x
                && local_x < expand_x + EXPAND_BTN_W as f32
                && local_y >= expand_y
                && local_y < expand_y + EXPAND_BTN_H as f32
            {
                return Some(WaveformButton::Expand);
            }
        }

        // Reanalyze buttons
        let mut btn_x = REANALYZE_BTN_MARGIN_LEFT as f32;
        for (i, &(_label, width)) in REANALYZE_BUTTONS.iter().enumerate() {
            if local_x >= btn_x
                && local_x < btn_x + width as f32
                && local_y >= REANALYZE_BTN_MARGIN_TOP as f32
                && local_y < (REANALYZE_BTN_MARGIN_TOP + REANALYZE_BTN_H) as f32
            {
                return match i {
                    0 => Some(WaveformButton::ReAnalyzeDrums),
                    1 => Some(WaveformButton::ReAnalyzeBass),
                    2 => Some(WaveformButton::ReAnalyzeSynth),
                    3 => Some(WaveformButton::ReAnalyzeVocal),
                    4 => Some(WaveformButton::ReImportStems),
                    _ => None,
                };
            }
            btn_x += width as f32 + REANALYZE_BTN_SPACING as f32;
        }

        None
    }

    /// Convert a button hit to a PanelAction.
    fn button_to_action(button: WaveformButton, stems_expanded: bool) -> Option<PanelAction> {
        match button {
            WaveformButton::Import => Some(PanelAction::ImportAudioClicked),
            WaveformButton::Remove => Some(PanelAction::RemoveAudioClicked),
            WaveformButton::Expand => {
                Some(PanelAction::ExpandStemsToggled(!stems_expanded))
            }
            WaveformButton::ReAnalyzeDrums => Some(PanelAction::ReAnalyzeDrums),
            WaveformButton::ReAnalyzeBass => Some(PanelAction::ReAnalyzeBass),
            WaveformButton::ReAnalyzeSynth => Some(PanelAction::ReAnalyzeSynth),
            WaveformButton::ReAnalyzeVocal => Some(PanelAction::ReAnalyzeVocal),
            WaveformButton::ReImportStems => Some(PanelAction::ReImportStems),
        }
    }
}

impl Panel for WaveformLanePanel {
    fn build(&mut self, _tree: &mut UITree, _layout: &ScreenLayout) {
        // Pixel buffer allocation happens in repaint() based on viewport width.
        // No UITree nodes needed — waveform is fully bitmap-rendered.
        self.dirty = true;
    }

    fn update(&mut self, _tree: &mut UITree) {
        // State updates are pushed via update_overlay().
        // Repaint is triggered by the app layer when dirty.
    }

    fn handle_event(&mut self, event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        let mut actions = Vec::new();

        match event {
            UIEvent::Click { pos, .. } => {
                // Check for button clicks
                if let Some(button) = self.hit_test_button(pos.x, pos.y) {
                    if let Some(action) =
                        Self::button_to_action(button, self.stems_expanded)
                    {
                        actions.push(action);
                    }
                } else if self.has_audio {
                    // Click without drag → scrub
                    actions.push(PanelAction::WaveformScrub(pos.x, pos.y));
                }
            }
            UIEvent::PointerDown { pos, .. } => {
                if self.has_audio && self.hit_test_button(pos.x, pos.y).is_none() {
                    // Start scrub (ImportedAudioWaveformScrubHandler.OnPointerDown)
                    self.is_scrubbing = true;
                    actions.push(PanelAction::WaveformScrub(pos.x, pos.y));
                }
            }
            UIEvent::HoverEnter { pos, .. } => {
                let new_hover = self.hit_test_button(pos.x, pos.y);
                if new_hover != self.hovered_button {
                    self.hovered_button = new_hover;
                    self.dirty = true;
                }
            }
            UIEvent::HoverExit { .. } => {
                if self.hovered_button.is_some() {
                    self.hovered_button = None;
                    self.dirty = true;
                }
            }
            UIEvent::DragBegin { pos, .. } => {
                // Start waveform drag (ImportedAudioWaveformDragHandler.OnBeginDrag)
                if self.has_audio && self.hit_test_button(pos.x, pos.y).is_none() {
                    self.is_dragging = true;
                    self.accumulated_beats = 0.0;
                    self.total_snapped_delta = 0.0;
                }
            }
            UIEvent::Drag { delta, pos, .. } => {
                // Handle scrub drag
                if self.is_scrubbing {
                    actions.push(PanelAction::WaveformScrub(pos.x, pos.y));
                }

                // Handle waveform drag (ImportedAudioWaveformDragHandler.OnDrag)
                if self.is_dragging {
                    let delta_beats = delta.x / self.pixels_per_beat();
                    self.accumulated_beats += delta_beats;

                    // Snap: emit in whole-beat increments
                    // Unity: `float snapped = (int)(accumulatedBeats / SnapStepBeats) * SnapStepBeats;`
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

    /// Current pixels per beat from cached mapper state.
    fn pixels_per_beat(&self) -> f32 {
        if self.waveform_duration_beats > 0.0 && self.cached_waveform_width > 0.0 {
            self.cached_waveform_width / self.waveform_duration_beats
        } else {
            120.0 // default ppb
        }
    }
}
