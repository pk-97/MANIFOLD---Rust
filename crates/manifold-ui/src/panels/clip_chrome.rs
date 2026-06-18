use super::PanelAction;
use super::param_slider_shared::build_dropdown_trigger;
use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderDragState};
use crate::tree::UITree;
use manifold_core::{Beats, Seconds};

// ── Layout constants (from ClipChromeBitmapPanel.cs) ──────────────

const NAME_ROW_H: f32 = 20.0;
const SECTION_LABEL_H: f32 = 18.0;
const SMALL_ROW_H: f32 = 18.0;
const BPM_ROW_H: f32 = 22.5;
const LOOP_BUTTON_H: f32 = 24.0;
const PROGRESS_H: f32 = 6.0;
const INSTR_ROW_H: f32 = 20.0;
/// Disabled instruments collapse to a thin name + enable line.
const DISABLED_ROW_H: f32 = 15.0;
/// The quantize-grid + onset row (one line, two halves).
const QO_ROW_H: f32 = 22.0;
/// Square enable toggle at the left of each instrument row.
const TOGGLE_W: f32 = 18.0;
/// Bipolar onset-compensation slider range, in milliseconds.
const ONSET_MIN_MS: f32 = -50.0;
const ONSET_MAX_MS: f32 = 50.0;
const DIVIDER_H: f32 = 1.0;
const PAD_H: f32 = 2.0;
const PAD_V: f32 = 2.0;
const GAP: f32 = 4.0;
const SOURCE_LABEL_W: f32 = 52.0;
const BPM_LABEL_W: f32 = 52.0;
const FONT_SIZE: u16 = color::FONT_BODY;
const NAME_FONT_SIZE: u16 = color::FONT_SUBHEADING;
const SMALL_FONT_SIZE: u16 = color::FONT_LABEL;

// ── Panel-specific colors (imported from color module) ───────────

use crate::color::{BPM_BTN_COLOR, BPM_BTN_HOVER, GEN_TYPE_COLOR, LOOP_OFF_COLOR, LOOP_ON_COLOR};

// ── ClipChromePanel ──────────────────────────────────────────────

/// One row in the per-clip detection instrument list (audio-clip detection).
#[derive(Clone, Debug)]
pub struct DetectInstrumentRow {
    pub label: String,
    pub enabled: bool,
    /// 0..1 sensitivity (drives the drag slider).
    pub sensitivity: f32,
    /// Triggers this instrument placed on the last plan (shown as a count).
    pub count: u32,
    /// Display name of the routed target layer, or "Auto".
    pub layer_label: String,
}

/// Everything the detection inspector renders, assembled in `state_sync` from
/// the clip's `AudioClipDetection` (config + cached counts) and the project's
/// layer names. One struct so `set_detection` takes a single argument.
#[derive(Clone, Debug, Default)]
pub struct DetectionView {
    /// Grid label for the quantize dropdown ("Off", "1/4" … "1/32").
    pub quantize_label: String,
    /// Onset compensation in milliseconds (drives the onset slider).
    pub onset_ms: f32,
    /// Whether the clip has a cached analysis (drives Detect vs Re-detect).
    pub has_analysis: bool,
    pub instruments: Vec<DetectInstrumentRow>,
}

pub struct ClipChromePanel {
    // Node IDs
    header_label_id: i32,
    chevron_btn_id: i32,
    name_label_id: i32,
    source_section_label_id: i32,
    source_name_label_id: i32,
    bpm_label_id: i32,
    bpm_value_btn_id: i32,
    warp_toggle_btn_id: i32,
    loop_toggle_btn_id: i32,
    detect_btn_id: i32,
    clear_triggers_btn_id: i32,
    detect_status_label_id: i32,
    detect_progress_bg_id: i32,
    detect_progress_fill_id: i32,
    detect_progress_bg_rect: Rect,
    quantize_dropdown_id: i32,
    onset_slider: SliderDragState,
    instrument_enable_btn_ids: Vec<i32>,
    /// Per-instrument sensitivity drag sliders (same widget as the cards).
    instrument_sens_sliders: Vec<SliderDragState>,
    /// Per-instrument target-layer dropdown triggers.
    instrument_layer_btn_ids: Vec<i32>,
    gen_type_label_id: i32,
    effects_label_id: i32,
    divider_ids: [i32; 3],

    // State
    is_collapsed: bool,
    has_clip: bool,
    mode_video: bool,
    mode_generator: bool,
    mode_audio: bool,
    mode_looping: bool,

    // Cached values
    cached_name: String,
    cached_source_name: String,
    cached_bpm_text: String,
    cached_gen_type: String,
    cached_loop_enabled: bool,
    cached_warp_enabled: bool,
    cached_detect_status: String,
    cached_detect_progress: f32,
    cached_detect_show: bool,
    cached_quantize_label: String,
    cached_onset_ms: f32,
    cached_has_analysis: bool,
    cached_instruments: Vec<DetectInstrumentRow>,

    // Node range
    first_node: usize,
    node_count: usize,
}

impl ClipChromePanel {
    pub fn new() -> Self {
        Self {
            header_label_id: -1,
            chevron_btn_id: -1,
            name_label_id: -1,
            source_section_label_id: -1,
            source_name_label_id: -1,
            bpm_label_id: -1,
            bpm_value_btn_id: -1,
            warp_toggle_btn_id: -1,
            loop_toggle_btn_id: -1,
            detect_btn_id: -1,
            clear_triggers_btn_id: -1,
            detect_status_label_id: -1,
            detect_progress_bg_id: -1,
            detect_progress_fill_id: -1,
            detect_progress_bg_rect: Rect::ZERO,
            quantize_dropdown_id: -1,
            onset_slider: SliderDragState::with_range(ONSET_MIN_MS, ONSET_MAX_MS, true),
            instrument_enable_btn_ids: Vec::new(),
            instrument_sens_sliders: Vec::new(),
            instrument_layer_btn_ids: Vec::new(),
            gen_type_label_id: -1,
            effects_label_id: -1,
            divider_ids: [-1; 3],
            is_collapsed: false,
            has_clip: false,
            mode_video: false,
            mode_generator: false,
            mode_audio: false,
            mode_looping: false,
            cached_name: String::new(),
            cached_source_name: String::new(),
            cached_bpm_text: "Auto".into(),
            cached_gen_type: String::new(),
            cached_loop_enabled: false,
            cached_warp_enabled: false,
            cached_detect_status: String::new(),
            cached_detect_progress: 0.0,
            cached_detect_show: false,
            cached_quantize_label: "1/16".into(),
            cached_onset_ms: 0.0,
            cached_has_analysis: false,
            cached_instruments: Vec::new(),
            first_node: 0,
            node_count: 0,
        }
    }

    pub fn compute_height(&self) -> f32 {
        let mut h = PAD_V + NAME_ROW_H;
        if self.has_clip {
            h += DIVIDER_H;
            if self.mode_video {
                h += SECTION_LABEL_H + SMALL_ROW_H + BPM_ROW_H + LOOP_BUTTON_H;
            } else if self.mode_audio {
                // Source label + filename + warp toggle + clip-BPM row.
                h += SECTION_LABEL_H + SMALL_ROW_H + LOOP_BUTTON_H + BPM_ROW_H;
                // Detection: label + status + progress bar + Detect + Clear +
                // quantize/onset row, then one row per instrument (disabled rows
                // collapse to a thin line).
                h += SECTION_LABEL_H + SMALL_ROW_H + PROGRESS_H + LOOP_BUTTON_H * 2.0 + QO_ROW_H;
                h += self
                    .cached_instruments
                    .iter()
                    .map(|r| if r.enabled { INSTR_ROW_H } else { DISABLED_ROW_H })
                    .sum::<f32>();
            } else if self.mode_generator {
                h += SMALL_ROW_H;
            }
            h += DIVIDER_H + SECTION_LABEL_H;
        }
        h += DIVIDER_H + PAD_V;
        h
    }

    pub fn first_node(&self) -> usize {
        self.first_node
    }
    pub fn node_count(&self) -> usize {
        self.node_count
    }
    /// Whether a clip is currently selected (the chrome has content to show).
    pub fn has_clip(&self) -> bool {
        self.has_clip
    }
    /// Mark the panel as contributing no nodes this frame (used when the inspector
    /// skips building it, so a stale node range can't catch a later hit-test).
    pub fn clear_nodes(&mut self) {
        self.node_count = 0;
    }
    pub fn is_dragging(&self) -> bool {
        self.onset_slider.is_dragging()
            || self.instrument_sens_sliders.iter().any(|s| s.is_dragging())
    }
    pub fn is_collapsed(&self) -> bool {
        self.is_collapsed
    }

    pub fn toggle_collapsed(&mut self) {
        self.is_collapsed = !self.is_collapsed;
    }

    pub fn set_collapsed(&mut self, v: bool) {
        self.is_collapsed = v;
    }

    /// Returns true if mode changed (caller should rebuild).
    pub fn set_mode(
        &mut self,
        has_clip: bool,
        is_video: bool,
        is_generator: bool,
        is_audio: bool,
        is_looping: bool,
    ) -> bool {
        if self.has_clip == has_clip
            && self.mode_video == is_video
            && self.mode_generator == is_generator
            && self.mode_audio == is_audio
            && self.mode_looping == is_looping
        {
            return false;
        }
        self.has_clip = has_clip;
        self.mode_video = is_video;
        self.mode_generator = is_generator;
        self.mode_audio = is_audio;
        self.mode_looping = is_looping;
        true
    }

    // Kept as no-ops for callers that still reference them
    pub fn set_slip_range(&mut self, _max: Seconds) {}
    pub fn set_loop_range(&mut self, _max_beats: Beats) {}

    // ── Build ────────────────────────────────────────────────────

    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        self.first_node = tree.count();
        let content_w = rect.width - PAD_H * 2.0;
        let cx = rect.x + PAD_H;
        let mut cy = rect.y + PAD_V;

        let name = self.cached_name.clone();
        let source_name = self.cached_source_name.clone();
        let bpm_text = self.cached_bpm_text.clone();
        let gen_type = self.cached_gen_type.clone();

        self.header_label_id = -1;
        self.chevron_btn_id = -1;

        let mut div_idx = 0;

        // Name row
        self.name_label_id = tree.add_label(
            -1,
            cx,
            cy,
            content_w,
            NAME_ROW_H,
            &name,
            UIStyle {
                text_color: color::TEXT_PRIMARY_C32,
                font_size: NAME_FONT_SIZE,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        ) as i32;
        cy += NAME_ROW_H;

        if self.has_clip {
            // Divider
            self.divider_ids[div_idx] = tree.add_panel(
                -1,
                cx,
                cy,
                content_w,
                DIVIDER_H,
                UIStyle {
                    bg_color: color::DIVIDER_C32,
                    ..UIStyle::default()
                },
            ) as i32;
            div_idx += 1;
            cy += DIVIDER_H;

            if self.mode_video {
                cy = self.build_video_section(tree, cx, cy, content_w, &source_name, &bpm_text);
            } else if self.mode_audio {
                cy = self.build_audio_section(tree, cx, cy, content_w, &source_name, &bpm_text);
            } else if self.mode_generator {
                cy = self.build_gen_type_row(tree, cx, cy, content_w, &gen_type);
            }

            // Divider before effects label
            if div_idx < 3 {
                self.divider_ids[div_idx] = tree.add_panel(
                    -1,
                    cx,
                    cy,
                    content_w,
                    DIVIDER_H,
                    UIStyle {
                        bg_color: color::DIVIDER_C32,
                        ..UIStyle::default()
                    },
                ) as i32;
            }
            cy += DIVIDER_H;

            // Effects section label
            self.effects_label_id = tree.add_label(
                -1,
                cx,
                cy,
                content_w,
                SECTION_LABEL_H,
                "Effects",
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: SMALL_FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            ) as i32;
        }

        self.node_count = tree.count() - self.first_node;
    }

    fn build_video_section(
        &mut self,
        tree: &mut UITree,
        cx: f32,
        mut cy: f32,
        w: f32,
        source_name: &str,
        bpm_text: &str,
    ) -> f32 {
        // "Source" section label
        self.source_section_label_id = tree.add_label(
            -1,
            cx,
            cy,
            w,
            SECTION_LABEL_H,
            "Source",
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: SMALL_FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;
        cy += SECTION_LABEL_H;

        // Source name
        self.source_name_label_id = tree.add_label(
            -1,
            cx + SOURCE_LABEL_W + GAP,
            cy,
            (w - SOURCE_LABEL_W - GAP).max(10.0),
            SMALL_ROW_H,
            source_name,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;
        cy += SMALL_ROW_H;

        // BPM row
        self.bpm_label_id = tree.add_label(
            -1,
            cx,
            cy,
            BPM_LABEL_W,
            BPM_ROW_H,
            "Src BPM",
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;

        let bpm_btn_w = (w - BPM_LABEL_W - GAP).max(20.0);
        self.bpm_value_btn_id = tree.add_button(
            -1,
            cx + BPM_LABEL_W + GAP,
            cy + (BPM_ROW_H - 18.0) * 0.5,
            bpm_btn_w,
            18.0,
            UIStyle {
                bg_color: BPM_BTN_COLOR,
                hover_bg_color: BPM_BTN_HOVER,
                pressed_bg_color: color::SLIDER_TRACK_PRESSED_C32,
                text_color: color::TEXT_PRIMARY_C32,
                font_size: FONT_SIZE,
                corner_radius: color::SMALL_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            bpm_text,
        ) as i32;
        cy += BPM_ROW_H;

        // Loop toggle button
        let loop_base = if self.cached_loop_enabled {
            LOOP_ON_COLOR
        } else {
            LOOP_OFF_COLOR
        };
        self.loop_toggle_btn_id = tree.add_button(
            -1,
            cx,
            cy,
            w,
            LOOP_BUTTON_H,
            UIStyle {
                bg_color: loop_base,
                hover_bg_color: lighten(loop_base, 10),
                pressed_bg_color: darken(loop_base, 10),
                text_color: color::TEXT_PRIMARY_C32,
                font_size: SMALL_FONT_SIZE,
                corner_radius: color::BUTTON_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            if self.cached_loop_enabled {
                "Loop ON"
            } else {
                "Loop OFF"
            },
        ) as i32;
        cy += LOOP_BUTTON_H;

        cy
    }

    /// Audio-clip section: "Source" label, the file name, and the clip-BPM
    /// button (the recorded tempo warp locks to the project — Audio Layer §4.1).
    /// Reuses the same `bpm_value_btn_id` → `ClipBpmClicked` path as video.
    fn build_audio_section(
        &mut self,
        tree: &mut UITree,
        cx: f32,
        mut cy: f32,
        w: f32,
        source_name: &str,
        bpm_text: &str,
    ) -> f32 {
        // "Source" section label
        self.source_section_label_id = tree.add_label(
            -1,
            cx,
            cy,
            w,
            SECTION_LABEL_H,
            "Source",
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: SMALL_FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;
        cy += SECTION_LABEL_H;

        // File name
        self.source_name_label_id = tree.add_label(
            -1,
            cx,
            cy,
            w,
            SMALL_ROW_H,
            source_name,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;
        cy += SMALL_ROW_H;

        // Warp toggle — on locks the clip's bars to the project tempo (varispeed
        // for now); off plays the file at its native speed. Drives recorded_bpm
        // (project tempo / 0) via ChangeClipRecordedBpmCommand.
        let warp_base = if self.cached_warp_enabled {
            LOOP_ON_COLOR
        } else {
            LOOP_OFF_COLOR
        };
        self.warp_toggle_btn_id = tree.add_button(
            -1,
            cx,
            cy,
            w,
            LOOP_BUTTON_H,
            UIStyle {
                bg_color: warp_base,
                hover_bg_color: lighten(warp_base, 10),
                pressed_bg_color: darken(warp_base, 10),
                text_color: color::TEXT_PRIMARY_C32,
                font_size: SMALL_FONT_SIZE,
                corner_radius: color::BUTTON_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            if self.cached_warp_enabled {
                "Warp ON"
            } else {
                "Warp OFF"
            },
        ) as i32;
        cy += LOOP_BUTTON_H;

        // Clip-BPM row
        self.bpm_label_id = tree.add_label(
            -1,
            cx,
            cy,
            BPM_LABEL_W,
            BPM_ROW_H,
            "Clip BPM",
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;

        let bpm_btn_w = (w - BPM_LABEL_W - GAP).max(20.0);
        self.bpm_value_btn_id = tree.add_button(
            -1,
            cx + BPM_LABEL_W + GAP,
            cy + (BPM_ROW_H - 18.0) * 0.5,
            bpm_btn_w,
            18.0,
            UIStyle {
                bg_color: BPM_BTN_COLOR,
                hover_bg_color: BPM_BTN_HOVER,
                pressed_bg_color: color::SLIDER_TRACK_PRESSED_C32,
                text_color: color::TEXT_PRIMARY_C32,
                font_size: FONT_SIZE,
                corner_radius: color::SMALL_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            bpm_text,
        ) as i32;
        cy += BPM_ROW_H;

        // ── Detection section ──
        // "Detection" section label
        tree.add_label(
            -1,
            cx,
            cy,
            w,
            SECTION_LABEL_H,
            "Detection",
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: SMALL_FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        );
        cy += SECTION_LABEL_H;

        // Status line — what the pipeline is doing right now (or last result).
        self.detect_status_label_id = tree.add_label(
            -1,
            cx,
            cy,
            w,
            SMALL_ROW_H,
            &self.cached_detect_status,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: SMALL_FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;
        cy += SMALL_ROW_H;

        // Progress bar — track + fill. Width of the fill is set in sync.
        self.detect_progress_bg_rect = Rect::new(cx, cy, w, PROGRESS_H);
        self.detect_progress_bg_id = tree.add_panel(
            -1,
            cx,
            cy,
            w,
            PROGRESS_H,
            UIStyle {
                bg_color: darken(BPM_BTN_COLOR, 35),
                corner_radius: color::SMALL_RADIUS,
                ..UIStyle::default()
            },
        ) as i32;
        let fill_w = (w * self.cached_detect_progress.clamp(0.0, 1.0)).max(0.0);
        self.detect_progress_fill_id = tree.add_panel(
            -1,
            cx,
            cy,
            fill_w,
            PROGRESS_H,
            UIStyle {
                bg_color: BPM_BTN_COLOR,
                corner_radius: color::SMALL_RADIUS,
                ..UIStyle::default()
            },
        ) as i32;
        if self.detect_progress_bg_id >= 0 {
            tree.set_visible(self.detect_progress_bg_id as u32, self.cached_detect_show);
        }
        if self.detect_progress_fill_id >= 0 {
            tree.set_visible(self.detect_progress_fill_id as u32, self.cached_detect_show);
        }
        cy += PROGRESS_H;

        // Detect / Re-detect button — runs analysis on the clip's file. While a
        // run is in flight (progress bar shown) it reads "Detecting…".
        let detect_label = if self.cached_detect_show {
            "Detecting…"
        } else if self.cached_has_analysis {
            "Re-detect"
        } else {
            "Detect"
        };
        self.detect_btn_id = tree.add_button(
            -1,
            cx,
            cy,
            w,
            LOOP_BUTTON_H,
            UIStyle {
                bg_color: BPM_BTN_COLOR,
                hover_bg_color: BPM_BTN_HOVER,
                pressed_bg_color: darken(BPM_BTN_COLOR, 10),
                text_color: color::TEXT_PRIMARY_C32,
                font_size: SMALL_FONT_SIZE,
                corner_radius: color::BUTTON_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            detect_label,
        ) as i32;
        cy += LOOP_BUTTON_H;

        // Quantize grid + onset row: quantize dropdown (left half), onset slider
        // (right half). Both re-plan from the cache on change.
        let half = (w - GAP) * 0.5;
        self.quantize_dropdown_id = build_dropdown_trigger(
            tree,
            -1,
            Rect::new(cx, cy + (QO_ROW_H - 18.0) * 0.5, half, 18.0),
            &format!("Grid {}", self.cached_quantize_label),
            SMALL_FONT_SIZE,
        );
        // Onset slider — the same widget the effect cards use, here mapping a
        // bipolar ±ms compensation.
        self.onset_slider.clear();
        let onset_rect = Rect::new(cx + half + GAP, cy + (QO_ROW_H - 18.0) * 0.5, half, 18.0);
        let onset_norm =
            BitmapSlider::value_to_normalized(self.cached_onset_ms, ONSET_MIN_MS, ONSET_MAX_MS);
        let onset_ids = BitmapSlider::build(
            tree,
            -1,
            onset_rect,
            Some("Onset"),
            onset_norm,
            &format!("{:+.0}ms", self.cached_onset_ms),
            &SliderColors::default_slider(),
            SMALL_FONT_SIZE,
            34.0,
        );
        self.onset_slider.set_ids(onset_ids);
        cy += QO_ROW_H;

        // Per-instrument rows. Enabled rows show enable · name · sensitivity
        // slider · count · target-layer dropdown; disabled rows collapse to a
        // thin enable · name line.
        self.instrument_enable_btn_ids.clear();
        self.instrument_layer_btn_ids.clear();
        for slider in self.instrument_sens_sliders.iter_mut() {
            slider.clear();
        }
        for (i, inst) in self.cached_instruments.clone().iter().enumerate() {
            let row_h = if inst.enabled { INSTR_ROW_H } else { DISABLED_ROW_H };
            let toggle_h = (row_h - 2.0).max(10.0);
            let toggle_y = cy + (row_h - toggle_h) * 0.5;

            // Enable toggle (square, filled when on).
            let en_base = if inst.enabled { LOOP_ON_COLOR } else { LOOP_OFF_COLOR };
            let en_id = tree.add_button(
                -1,
                cx,
                toggle_y,
                TOGGLE_W,
                toggle_h,
                UIStyle {
                    bg_color: en_base,
                    hover_bg_color: lighten(en_base, 10),
                    pressed_bg_color: darken(en_base, 10),
                    text_color: color::TEXT_PRIMARY_C32,
                    font_size: color::FONT_CAPTION,
                    corner_radius: color::SMALL_RADIUS,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
                if inst.enabled { "\u{2713}" } else { "" },
            ) as i32;
            self.instrument_enable_btn_ids.push(en_id);

            let after_toggle = cx + TOGGLE_W + GAP;

            if !inst.enabled {
                // Collapsed line: just the dimmed name.
                tree.add_label(
                    -1,
                    after_toggle,
                    cy,
                    w - TOGGLE_W - GAP,
                    row_h,
                    &inst.label,
                    UIStyle {
                        text_color: color::TEXT_DIMMED_C32,
                        font_size: SMALL_FONT_SIZE,
                        text_align: TextAlign::Left,
                        ..UIStyle::default()
                    },
                );
                self.instrument_layer_btn_ids.push(-1);
                cy += row_h;
                continue;
            }

            // Layout: name | slider | count | layer dropdown.
            let name_w = 46.0;
            let layer_w = 64.0;
            let count_w = 24.0;
            let slider_w = (w - TOGGLE_W - GAP - name_w - count_w - layer_w - GAP * 3.0).max(30.0);
            let mut x = after_toggle;

            tree.add_label(
                -1,
                x,
                cy,
                name_w,
                row_h,
                &inst.label,
                UIStyle {
                    text_color: color::TEXT_PRIMARY_C32,
                    font_size: SMALL_FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            x += name_w + GAP;

            // Sensitivity slider (no label cell — the row name labels it).
            let sens_norm = inst.sensitivity.clamp(0.0, 1.0);
            let sens_ids = BitmapSlider::build(
                tree,
                -1,
                Rect::new(x, cy, slider_w, row_h),
                None,
                sens_norm,
                "",
                &SliderColors::default_slider(),
                SMALL_FONT_SIZE,
                0.0,
            );
            if let Some(slot) = self.instrument_sens_sliders.get_mut(i) {
                slot.set_ids(sens_ids);
            }
            x += slider_w + GAP;

            // Count of placed triggers.
            tree.add_label(
                -1,
                x,
                cy,
                count_w,
                row_h,
                &inst.count.to_string(),
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: color::FONT_CAPTION,
                    text_align: TextAlign::Right,
                    ..UIStyle::default()
                },
            );
            x += count_w + GAP;

            // Target-layer dropdown.
            let layer_id = build_dropdown_trigger(
                tree,
                -1,
                Rect::new(x, cy + (row_h - 16.0) * 0.5, layer_w, 16.0),
                &inst.layer_label,
                color::FONT_CAPTION,
            );
            self.instrument_layer_btn_ids.push(layer_id);

            cy += row_h;
        }

        // Clear button — removes only this clip's own triggers. Sits at the
        // bottom as a secondary action.
        self.clear_triggers_btn_id = tree.add_button(
            -1,
            cx,
            cy,
            w,
            LOOP_BUTTON_H,
            UIStyle {
                bg_color: LOOP_OFF_COLOR,
                hover_bg_color: lighten(LOOP_OFF_COLOR, 10),
                pressed_bg_color: darken(LOOP_OFF_COLOR, 10),
                text_color: color::TEXT_DIMMED_C32,
                font_size: SMALL_FONT_SIZE,
                corner_radius: color::BUTTON_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            "Clear Triggers",
        ) as i32;
        cy += LOOP_BUTTON_H;

        cy
    }

    fn build_gen_type_row(
        &mut self,
        tree: &mut UITree,
        cx: f32,
        cy: f32,
        w: f32,
        gen_type: &str,
    ) -> f32 {
        let label = format!("Type: {}", gen_type);
        self.gen_type_label_id = tree.add_label(
            -1,
            cx,
            cy,
            w,
            SMALL_ROW_H,
            &label,
            UIStyle {
                text_color: GEN_TYPE_COLOR,
                font_size: SMALL_FONT_SIZE,
                text_align: TextAlign::Left,
                ..UIStyle::default()
            },
        ) as i32;
        cy + SMALL_ROW_H
    }

    // ── Sync methods ─────────────────────────────────────────────

    pub fn sync_name(&mut self, tree: &mut UITree, name: &str) {
        self.cached_name = name.into();
        if self.name_label_id >= 0 {
            tree.set_text(self.name_label_id as u32, name);
        }
    }

    pub fn sync_collapsed(&mut self, tree: &mut UITree, collapsed: bool) {
        self.is_collapsed = collapsed;
        if self.chevron_btn_id >= 0 {
            tree.set_text(
                self.chevron_btn_id as u32,
                if collapsed { "\u{25B6}" } else { "\u{25BC}" },
            );
        }
    }

    pub fn sync_source_name(&mut self, tree: &mut UITree, name: &str) {
        self.cached_source_name = name.into();
        if self.source_name_label_id >= 0 {
            tree.set_text(self.source_name_label_id as u32, name);
        }
    }

    // Kept as no-ops for callers
    pub fn sync_slip(&mut self, _tree: &mut UITree, _value: Seconds) {}
    pub fn sync_loop_duration(&mut self, _tree: &mut UITree, _beats: Beats) {}

    pub fn sync_bpm(&mut self, tree: &mut UITree, text: &str) {
        self.cached_bpm_text = text.into();
        if self.bpm_value_btn_id >= 0 {
            tree.set_text(self.bpm_value_btn_id as u32, text);
        }
    }

    pub fn sync_warp_enabled(&mut self, tree: &mut UITree, enabled: bool) {
        self.cached_warp_enabled = enabled;
        if self.warp_toggle_btn_id >= 0 {
            tree.set_text(
                self.warp_toggle_btn_id as u32,
                if enabled { "Warp ON" } else { "Warp OFF" },
            );
            let base = if enabled { LOOP_ON_COLOR } else { LOOP_OFF_COLOR };
            tree.set_style(
                self.warp_toggle_btn_id as u32,
                UIStyle {
                    bg_color: base,
                    hover_bg_color: lighten(base, 10),
                    pressed_bg_color: darken(base, 10),
                    text_color: color::TEXT_PRIMARY_C32,
                    font_size: SMALL_FONT_SIZE,
                    corner_radius: color::BUTTON_RADIUS,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
            );
        }
    }

    /// Set the detection view the rows render from. Called before build (with
    /// `set_mode`) so the row count drives layout height. The view is assembled
    /// in `state_sync` from the clip's config + cached counts + layer names.
    pub fn set_detection(&mut self, view: &DetectionView) {
        self.cached_quantize_label = view.quantize_label.clone();
        self.cached_onset_ms = view.onset_ms;
        self.cached_has_analysis = view.has_analysis;
        self.cached_instruments = view.instruments.clone();
        // Keep one sensitivity drag-state per instrument row. Node ids are
        // (re)bound in `build`; here we only size the vec and set the 0..1 range.
        self.instrument_sens_sliders
            .resize_with(self.cached_instruments.len(), || {
                SliderDragState::with_range(0.0, 1.0, false)
            });
    }

    /// Update the detection status line + progress bar in place (no rebuild).
    pub fn sync_detect_status(&mut self, tree: &mut UITree, status: &str, progress: f32, show: bool) {
        self.cached_detect_status = status.into();
        self.cached_detect_progress = progress.clamp(0.0, 1.0);
        self.cached_detect_show = show;
        if self.detect_status_label_id >= 0 {
            tree.set_text(self.detect_status_label_id as u32, status);
        }
        if self.detect_progress_bg_id >= 0 {
            tree.set_visible(self.detect_progress_bg_id as u32, show);
        }
        if self.detect_progress_fill_id >= 0 {
            tree.set_visible(self.detect_progress_fill_id as u32, show);
            let bg = self.detect_progress_bg_rect;
            tree.set_bounds(
                self.detect_progress_fill_id as u32,
                Rect::new(bg.x, bg.y, bg.width * self.cached_detect_progress, bg.height),
            );
        }
    }

    pub fn sync_gen_type(&mut self, tree: &mut UITree, gen_type: &str) {
        self.cached_gen_type = gen_type.into();
        if self.gen_type_label_id >= 0 {
            let label = format!("Type: {}", gen_type);
            tree.set_text(self.gen_type_label_id as u32, &label);
        }
    }

    pub fn sync_loop_enabled(&mut self, tree: &mut UITree, enabled: bool) {
        self.cached_loop_enabled = enabled;
        if self.loop_toggle_btn_id >= 0 {
            tree.set_text(
                self.loop_toggle_btn_id as u32,
                if enabled { "Loop ON" } else { "Loop OFF" },
            );
            let base = if enabled {
                LOOP_ON_COLOR
            } else {
                LOOP_OFF_COLOR
            };
            tree.set_style(
                self.loop_toggle_btn_id as u32,
                UIStyle {
                    bg_color: base,
                    hover_bg_color: lighten(base, 10),
                    pressed_bg_color: darken(base, 10),
                    text_color: color::TEXT_PRIMARY_C32,
                    font_size: SMALL_FONT_SIZE,
                    corner_radius: color::BUTTON_RADIUS,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
            );
        }
    }

    // ── Event handling ───────────────────────────────────────────

    pub fn handle_click(&self, node_id: u32) -> Vec<PanelAction> {
        let id = node_id as i32;
        if id == self.chevron_btn_id {
            return vec![PanelAction::ClipChromeCollapseToggle];
        }
        if id == self.bpm_value_btn_id && (self.mode_video || self.mode_audio) {
            return vec![PanelAction::ClipBpmClicked];
        }
        if id == self.warp_toggle_btn_id && self.mode_audio {
            return vec![PanelAction::ClipWarpToggled];
        }
        // Detect is inert while a run is in flight (label reads "Detecting…").
        if id == self.detect_btn_id && self.mode_audio && !self.cached_detect_show {
            return vec![PanelAction::ClipDetectClicked];
        }
        if id == self.clear_triggers_btn_id && self.mode_audio {
            return vec![PanelAction::ClipClearTriggersClicked];
        }
        if id == self.quantize_dropdown_id && self.mode_audio {
            return vec![PanelAction::ClipDetectQuantizeClicked];
        }
        if self.mode_audio {
            if let Some(idx) = self
                .instrument_enable_btn_ids
                .iter()
                .position(|&b| b == id)
            {
                return vec![PanelAction::ClipDetectInstrumentToggled(idx)];
            }
            if let Some(idx) = self
                .instrument_layer_btn_ids
                .iter()
                .position(|&b| b == id)
            {
                return vec![PanelAction::ClipDetectLayerClicked(idx)];
            }
        }
        if id == self.loop_toggle_btn_id && self.mode_video {
            return vec![PanelAction::ClipLoopToggle];
        }
        Vec::new()
    }

    /// Begin a sensitivity/onset slider drag if the press hit a track.
    pub fn handle_pointer_down(&mut self, node_id: u32, pos: Vec2) -> Vec<PanelAction> {
        if !self.mode_audio {
            return Vec::new();
        }
        // Onset slider.
        if self.onset_slider.try_start_drag(node_id, pos.x).is_some() {
            return Vec::new();
        }
        // Per-instrument sensitivity sliders.
        for slider in self.instrument_sens_sliders.iter_mut() {
            if slider.try_start_drag(node_id, pos.x).is_some() {
                return Vec::new();
            }
        }
        Vec::new()
    }

    /// Continue an active slider drag — visual feedback only. The re-plan fires
    /// once on release (see `handle_drag_end`), so dragging never churns the
    /// timeline or spams the undo stack.
    pub fn handle_drag(&mut self, pos: Vec2, tree: &mut UITree) -> Vec<PanelAction> {
        if self.onset_slider.is_dragging() {
            self.onset_slider
                .apply_drag(pos.x, tree, &|v| format!("{v:+.0}ms"));
            return Vec::new();
        }
        for slider in self.instrument_sens_sliders.iter_mut() {
            if slider.is_dragging() {
                slider.apply_drag(pos.x, tree, &|_| String::new());
                return Vec::new();
            }
        }
        Vec::new()
    }

    /// Commit a slider drag on release: emit the change so the inspector records
    /// the config edit and re-plans from the cache (one undo step).
    pub fn handle_drag_end(&mut self, _tree: &mut UITree) -> Vec<PanelAction> {
        if self.onset_slider.end_drag() {
            return vec![PanelAction::ClipDetectOnsetChanged(
                self.onset_slider.cached_value(),
            )];
        }
        for (i, slider) in self.instrument_sens_sliders.iter_mut().enumerate() {
            if slider.end_drag() {
                return vec![PanelAction::ClipDetectSensitivityChanged(
                    i,
                    slider.cached_value(),
                )];
            }
        }
        Vec::new()
    }

    pub fn detect_instruments(&self) -> &[DetectInstrumentRow] {
        &self.cached_instruments
    }

    pub fn bpm_button_rect(&self, tree: &UITree) -> Rect {
        if self.bpm_value_btn_id >= 0 {
            tree.get_bounds(self.bpm_value_btn_id as u32)
        } else {
            Rect::ZERO
        }
    }
}

impl Default for ClipChromePanel {
    fn default() -> Self {
        Self::new()
    }
}

// ── Helpers ──────────────────────────────────────────────────────

fn lighten(c: Color32, amount: u8) -> Color32 {
    Color32::new(
        c.r.saturating_add(amount),
        c.g.saturating_add(amount),
        c.b.saturating_add(amount),
        c.a,
    )
}

fn darken(c: Color32, amount: u8) -> Color32 {
    Color32::new(
        c.r.saturating_sub(amount),
        c.g.saturating_sub(amount),
        c.b.saturating_sub(amount),
        c.a,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::UITree;

    /// Build a `DetectionView` from a config (test stand-in for state_sync's
    /// assembly — no counts, all rows route "Auto").
    fn detection_view_from(
        cfg: &manifold_core::audio_clip_detection::DetectionConfig,
    ) -> DetectionView {
        DetectionView {
            quantize_label: manifold_core::audio_clip_detection::quantize_grid_label(
                cfg.quantize_on,
                cfg.quantize_step_beats,
            ),
            onset_ms: (cfg.onset_compensation.0 * 1000.0) as f32,
            has_analysis: false,
            instruments: cfg
                .instruments
                .iter()
                .map(|i| DetectInstrumentRow {
                    label: format!("{:?}", i.trigger_type),
                    enabled: i.enabled,
                    sensitivity: i.sensitivity,
                    count: 0,
                    layer_label: "Auto".to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn build_clip_chrome_no_clip() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        // Header row removed — only name label + divider
        assert_eq!(panel.header_label_id, -1);
        assert_eq!(panel.chevron_btn_id, -1);
        assert!(panel.node_count > 0);
    }

    #[test]
    fn build_clip_chrome_video_mode() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.set_mode(true, true, false, false, false);
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        assert!(panel.bpm_value_btn_id >= 0);
        assert!(panel.loop_toggle_btn_id >= 0);
        assert!(panel.effects_label_id >= 0);
    }

    #[test]
    fn build_clip_chrome_audio_mode() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.set_mode(true, false, false, true, false);
        let cfg = manifold_core::audio_clip_detection::DetectionConfig::default();
        let view = detection_view_from(&cfg);
        panel.set_detection(&view);
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 600.0));

        // Audio mode exposes the clip-BPM button but no loop toggle.
        assert!(panel.bpm_value_btn_id >= 0);
        assert!(panel.source_name_label_id >= 0);
        assert!(panel.effects_label_id >= 0);
        // Audio mode adds a warp toggle (but no loop toggle).
        assert!(panel.warp_toggle_btn_id >= 0);
        assert_eq!(panel.loop_toggle_btn_id, -1);
        // BPM click is live in audio mode.
        let actions = panel.handle_click(panel.bpm_value_btn_id as u32);
        assert!(matches!(actions.as_slice(), [PanelAction::ClipBpmClicked]));
        // Warp toggle click fires the warp action.
        let warp = panel.handle_click(panel.warp_toggle_btn_id as u32);
        assert!(matches!(warp.as_slice(), [PanelAction::ClipWarpToggled]));
        // Detection buttons exist and fire their actions.
        assert!(panel.detect_btn_id >= 0);
        assert!(panel.clear_triggers_btn_id >= 0);
        let detect = panel.handle_click(panel.detect_btn_id as u32);
        assert!(matches!(detect.as_slice(), [PanelAction::ClipDetectClicked]));
        let clear = panel.handle_click(panel.clear_triggers_btn_id as u32);
        assert!(matches!(clear.as_slice(), [PanelAction::ClipClearTriggersClicked]));
        // Quantize dropdown opens its picker; per-instrument rows fire indexed actions.
        assert!(panel.quantize_dropdown_id >= 0);
        let q = panel.handle_click(panel.quantize_dropdown_id as u32);
        assert!(matches!(q.as_slice(), [PanelAction::ClipDetectQuantizeClicked]));
        assert_eq!(panel.instrument_enable_btn_ids.len(), 9);
        // Default config enables 4 drums → 4 sensitivity sliders + 4 layer dropdowns
        // are built (disabled rows collapse to name + toggle only).
        let en = panel.handle_click(panel.instrument_enable_btn_ids[2] as u32);
        assert!(matches!(en.as_slice(), [PanelAction::ClipDetectInstrumentToggled(2)]));
        // The first enabled instrument's layer dropdown opens its picker.
        let layer_id = panel.instrument_layer_btn_ids[0];
        assert!(layer_id >= 0, "enabled row has a layer dropdown");
        let ld = panel.handle_click(layer_id as u32);
        assert!(matches!(ld.as_slice(), [PanelAction::ClipDetectLayerClicked(0)]));
    }

    #[test]
    fn build_clip_chrome_gen_mode() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.set_mode(true, false, true, false, false);
        panel.cached_gen_type = "Plasma".into();
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        assert!(panel.gen_type_label_id >= 0);
        assert!(panel.effects_label_id >= 0);
    }

    #[test]
    fn set_mode_returns_changed() {
        let mut panel = ClipChromePanel::new();
        assert!(panel.set_mode(true, true, false, false, false));
        assert!(!panel.set_mode(true, true, false, false, false));
        assert!(panel.set_mode(true, true, false, false, true));
    }

    #[test]
    fn handle_click_bpm() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.set_mode(true, true, false, false, false);
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let actions = panel.handle_click(panel.bpm_value_btn_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::ClipBpmClicked));
    }

    #[test]
    fn handle_click_loop_toggle() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.set_mode(true, true, false, false, false);
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 300.0));

        let actions = panel.handle_click(panel.loop_toggle_btn_id as u32);
        assert_eq!(actions.len(), 1);
        assert!(matches!(actions[0], PanelAction::ClipLoopToggle));
    }

    #[test]
    fn is_dragging_false_when_idle() {
        let panel = ClipChromePanel::new();
        assert!(!panel.is_dragging());
    }

    #[test]
    fn sensitivity_drag_emits_change_on_release() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.set_mode(true, false, false, true, false);
        let cfg = manifold_core::audio_clip_detection::DetectionConfig::default();
        panel.set_detection(&detection_view_from(&cfg));
        panel.build(&mut tree, Rect::new(0.0, 0.0, 280.0, 600.0));

        // Grab the first enabled instrument's sensitivity slider track and drag.
        let track = panel.instrument_sens_sliders[0]
            .track_id()
            .expect("enabled row has a slider");
        let track_rect = tree.get_bounds(track);
        let down = panel.handle_pointer_down(track, Vec2::new(track_rect.x, track_rect.y));
        assert!(down.is_empty(), "press starts a drag, emits nothing");
        assert!(panel.is_dragging());
        // Release → one sensitivity-change action for instrument 0.
        let up = panel.handle_drag_end(&mut tree);
        assert!(matches!(
            up.as_slice(),
            [PanelAction::ClipDetectSensitivityChanged(0, _)]
        ));
        assert!(!panel.is_dragging());
    }
}
