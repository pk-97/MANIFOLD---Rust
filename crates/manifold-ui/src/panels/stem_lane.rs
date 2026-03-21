//! Stem waveform lanes — 4 per-stem lanes (Drums, Bass, Other, Vocals)
//! managed as a collapsible group.
//!
//! Mechanical translation of:
//! - `Assets/Scripts/UI/Timeline/StemWaveformLane.cs` (312 lines)
//! - `Assets/Scripts/UI/Timeline/StemLaneGroup.cs` (145 lines)
//!
//! In Unity these are positioned via RectTransforms. In Rust we paint
//! all 4 lanes into a single pixel buffer.

use crate::bitmap_painter::fill_rect;
use crate::color;
use crate::coordinate_mapper::CoordinateMapper;
use crate::input::UIEvent;
use crate::layout::ScreenLayout;
use crate::node::Color32;
use crate::tree::UITree;
use crate::waveform_painter;
use crate::waveform_renderer::WaveformRenderer;

use super::{Panel, PanelAction};

/// Number of stems (Unity: StemAudioController.StemCount = 4).
pub const STEM_COUNT: usize = 4;

/// Stem names (Unity: StemAudioController.StemNames).
pub const STEM_NAMES: [&str; STEM_COUNT] = ["Drums", "Bass", "Other", "Vocals"];

/// Per-stem background colors (Unity: StemLaneGroup.StemBackgroundColors, UIConstants.cs lines 247-250).
const STEM_BG_COLORS: [Color32; STEM_COUNT] = [
    color::STEM_LANE_BG_DRUMS,
    color::STEM_LANE_BG_BASS,
    color::STEM_LANE_BG_OTHER,
    color::STEM_LANE_BG_VOCALS,
];

/// Playhead color (same as master lane).
const PLAYHEAD_COLOR: Color32 = Color32::new(217, 64, 56, 217);

// ── Mute/Solo button layout ──
const MUTE_SOLO_BTN_W: i32 = 20;
const MUTE_SOLO_BTN_H: i32 = 16;
const HEADER_X: i32 = 4;
const BUTTON_SPACING: i32 = 2;

/// State for a single stem lane.
///
/// Unity: `StemWaveformLane` (312 lines).
struct StemLane {
    renderer: WaveformRenderer,
    #[allow(dead_code)] // used when stem lane events reference the index
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

    // ── Hover tracking (used for button highlight feedback) ──
    #[allow(dead_code)]
    hovered_stem: Option<usize>,
    #[allow(dead_code)]
    hovered_mute: bool,
    #[allow(dead_code)]
    hovered_solo: bool,
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
            hovered_stem: None,
            hovered_mute: false,
            hovered_solo: false,
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

    /// Repaint the pixel buffer for all 4 stem lanes.
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

            // Lane background (Unity: StemWaveformLane.BuildUI laneBg.color = backgroundColor)
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
                // Stem duration in beats
                // Unity: StemWaveformLane.UpdateOverlay computes stemDurationBeats
                // using PlaybackController.TimelineBeatToTime/TimelineTimeToBeat.
                // We approximate using the same beat-per-second ratio as the master.
                let waveform_x =
                    mapper.beat_to_pixel_absolute(self.waveform_start_beat.max(0.0));

                // For stems, we need to compute beat duration from seconds.
                // The stem's actual audio duration determines its beat span.
                // Use a simple duration estimate: same proportional width as master
                let stem_width = mapper.beat_duration_to_width(
                    self.waveform_duration_beats_for_stem(i),
                );

                if stem_width > 0.0 {
                    if let Some(level) = lane.renderer.select_level_for_zoom(stem_width, 1.0) {
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

            // Draw mute/solo buttons (Unity: StemWaveformLane header overlay)
            let btn_y = y_offset + (lane_h as i32 / 2) + 1; // bottom half
            waveform_painter::draw_mute_solo_button(
                &mut self.pixel_buffer,
                buf_w,
                buf_h,
                HEADER_X,
                btn_y,
                MUTE_SOLO_BTN_W,
                MUTE_SOLO_BTN_H,
                lane.is_muted,
                true, // is_mute
            );
            waveform_painter::draw_mute_solo_button(
                &mut self.pixel_buffer,
                buf_w,
                buf_h,
                HEADER_X + MUTE_SOLO_BTN_W + BUTTON_SPACING,
                btn_y,
                MUTE_SOLO_BTN_W,
                MUTE_SOLO_BTN_H,
                lane.is_soloed,
                false, // is_solo
            );
        }

        self.dirty = false;
    }

    /// Compute beat duration for a stem from its audio length.
    /// Unity: StemWaveformLane.UpdateOverlay uses PlaybackController.TimelineBeatToTime/
    /// TimelineTimeToBeat. We convert seconds to beats using BPM.
    fn waveform_duration_beats_for_stem(&self, index: usize) -> f32 {
        if index >= STEM_COUNT {
            return 0.0;
        }
        let dur_sec = self.lanes[index].renderer.clip_duration_seconds();
        if dur_sec <= 0.0 || self.bpm <= 0.0 {
            return 0.0;
        }
        // seconds_to_beats: dur_sec * (bpm / 60)
        dur_sec * (self.bpm / 60.0)
    }

    /// Hit-test a position against mute/solo buttons.
    fn hit_test_button(&self, local_x: f32, local_y: f32) -> Option<(usize, bool)> {
        let lane_h = color::STEM_LANE_HEIGHT;
        let stem_index = (local_y / lane_h) as usize;
        if stem_index >= STEM_COUNT {
            return None;
        }

        let lane_local_y = local_y - stem_index as f32 * lane_h;
        let btn_y = lane_h / 2.0 + 1.0;

        if lane_local_y >= btn_y && lane_local_y < btn_y + MUTE_SOLO_BTN_H as f32 {
            // Mute button
            if local_x >= HEADER_X as f32
                && local_x < (HEADER_X + MUTE_SOLO_BTN_W) as f32
            {
                return Some((stem_index, true)); // is_mute
            }
            // Solo button
            let solo_x = (HEADER_X + MUTE_SOLO_BTN_W + BUTTON_SPACING) as f32;
            if local_x >= solo_x && local_x < solo_x + MUTE_SOLO_BTN_W as f32 {
                return Some((stem_index, false)); // is_solo
            }
        }
        None
    }
}

impl Panel for StemLaneGroupPanel {
    fn build(&mut self, _tree: &mut UITree, _layout: &ScreenLayout) {
        self.dirty = true;
    }

    fn update(&mut self, _tree: &mut UITree) {
        // State updates pushed via update_overlay().
    }

    fn handle_event(&mut self, event: &UIEvent, _tree: &UITree) -> Vec<PanelAction> {
        let mut actions = Vec::new();
        if !self.expanded {
            return actions;
        }

        match event {
            UIEvent::Click { pos, .. } => {
                if let Some((stem_index, is_mute)) = self.hit_test_button(pos.x, pos.y) {
                    if is_mute {
                        actions.push(PanelAction::StemMuteToggled(stem_index));
                    } else {
                        actions.push(PanelAction::StemSoloToggled(stem_index));
                    }
                } else {
                    // Scrub (same as master lane)
                    actions.push(PanelAction::WaveformScrub(pos.x, pos.y));
                }
            }
            UIEvent::PointerDown { pos, .. } => {
                if self.hit_test_button(pos.x, pos.y).is_none() {
                    actions.push(PanelAction::WaveformScrub(pos.x, pos.y));
                }
            }
            UIEvent::Drag { pos, .. } => {
                actions.push(PanelAction::WaveformScrub(pos.x, pos.y));
            }
            _ => {}
        }

        actions
    }
}
