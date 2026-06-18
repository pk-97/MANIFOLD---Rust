use super::PanelAction;
use crate::color;
use crate::node::*;
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

/// Coarse label for a 0..1 sensitivity (the per-instrument cycle button).
fn sensitivity_label(sensitivity: f32) -> &'static str {
    if sensitivity < 0.35 {
        "Lo"
    } else if sensitivity < 0.65 {
        "Md"
    } else {
        "Hi"
    }
}

// ── ClipChromePanel ──────────────────────────────────────────────

/// One row in the per-clip detection instrument list (audio-clip detection).
#[derive(Clone, Debug)]
pub struct DetectInstrumentRow {
    pub label: String,
    pub enabled: bool,
    pub sensitivity: f32,
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
    quantize_btn_id: i32,
    instrument_enable_btn_ids: Vec<i32>,
    instrument_sens_btn_ids: Vec<i32>,
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
    cached_quantize_on: bool,
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
            quantize_btn_id: -1,
            instrument_enable_btn_ids: Vec::new(),
            instrument_sens_btn_ids: Vec::new(),
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
            cached_quantize_on: true,
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
                // quantize toggle + one row per instrument.
                h += SECTION_LABEL_H + SMALL_ROW_H + PROGRESS_H + LOOP_BUTTON_H * 3.0;
                h += self.cached_instruments.len() as f32 * INSTR_ROW_H;
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
        false
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

        // Detect button — runs analysis on the clip's file and places triggers.
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
            "Detect",
        ) as i32;
        cy += LOOP_BUTTON_H;

        // Clear button — removes only this clip's own triggers.
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

        // Quantize toggle.
        let q_base = if self.cached_quantize_on {
            LOOP_ON_COLOR
        } else {
            LOOP_OFF_COLOR
        };
        self.quantize_btn_id = tree.add_button(
            -1,
            cx,
            cy,
            w,
            LOOP_BUTTON_H,
            UIStyle {
                bg_color: q_base,
                hover_bg_color: lighten(q_base, 10),
                pressed_bg_color: darken(q_base, 10),
                text_color: color::TEXT_PRIMARY_C32,
                font_size: SMALL_FONT_SIZE,
                corner_radius: color::BUTTON_RADIUS,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
            if self.cached_quantize_on {
                "Quantize ON"
            } else {
                "Quantize OFF"
            },
        ) as i32;
        cy += LOOP_BUTTON_H;

        // Per-instrument rows: name · enable · sensitivity. Buttons re-plan live.
        self.instrument_enable_btn_ids.clear();
        self.instrument_sens_btn_ids.clear();
        let name_w = (w * 0.45).max(20.0);
        let enable_w = (w * 0.22).max(16.0);
        let sens_w = (w - name_w - enable_w - GAP * 2.0).max(16.0);
        let btn_h = (INSTR_ROW_H - PAD_V).max(12.0);
        for inst in self.cached_instruments.clone().iter() {
            tree.add_label(
                -1,
                cx,
                cy,
                name_w,
                INSTR_ROW_H,
                &inst.label,
                UIStyle {
                    text_color: if inst.enabled {
                        color::TEXT_PRIMARY_C32
                    } else {
                        color::TEXT_DIMMED_C32
                    },
                    font_size: SMALL_FONT_SIZE,
                    text_align: TextAlign::Left,
                    ..UIStyle::default()
                },
            );
            let en_base = if inst.enabled { LOOP_ON_COLOR } else { LOOP_OFF_COLOR };
            let en_id = tree.add_button(
                -1,
                cx + name_w + GAP,
                cy + (INSTR_ROW_H - btn_h) * 0.5,
                enable_w,
                btn_h,
                UIStyle {
                    bg_color: en_base,
                    hover_bg_color: lighten(en_base, 10),
                    pressed_bg_color: darken(en_base, 10),
                    text_color: color::TEXT_PRIMARY_C32,
                    font_size: SMALL_FONT_SIZE,
                    corner_radius: color::SMALL_RADIUS,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
                if inst.enabled { "On" } else { "Off" },
            ) as i32;
            let sens_id = tree.add_button(
                -1,
                cx + name_w + GAP + enable_w + GAP,
                cy + (INSTR_ROW_H - btn_h) * 0.5,
                sens_w,
                btn_h,
                UIStyle {
                    bg_color: BPM_BTN_COLOR,
                    hover_bg_color: BPM_BTN_HOVER,
                    pressed_bg_color: darken(BPM_BTN_COLOR, 10),
                    text_color: color::TEXT_PRIMARY_C32,
                    font_size: SMALL_FONT_SIZE,
                    corner_radius: color::SMALL_RADIUS,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
                sensitivity_label(inst.sensitivity),
            ) as i32;
            self.instrument_enable_btn_ids.push(en_id);
            self.instrument_sens_btn_ids.push(sens_id);
            cy += INSTR_ROW_H;
        }

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

    /// Set the detection config the rows render from. Called before build (with
    /// `set_mode`) so the row count drives layout height. The actual values
    /// (enabled/sensitivity/quantize) are read by `build`.
    pub fn set_detection(
        &mut self,
        config: &manifold_core::audio_clip_detection::DetectionConfig,
    ) {
        self.cached_quantize_on = config.quantize_on;
        self.cached_instruments = config
            .instruments
            .iter()
            .map(|i| DetectInstrumentRow {
                label: format!("{:?}", i.trigger_type),
                enabled: i.enabled,
                sensitivity: i.sensitivity,
            })
            .collect();
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
        if id == self.detect_btn_id && self.mode_audio {
            return vec![PanelAction::ClipDetectClicked];
        }
        if id == self.clear_triggers_btn_id && self.mode_audio {
            return vec![PanelAction::ClipClearTriggersClicked];
        }
        if id == self.quantize_btn_id && self.mode_audio {
            return vec![PanelAction::ClipDetectQuantizeToggled];
        }
        if self.mode_audio {
            if let Some(idx) = self
                .instrument_enable_btn_ids
                .iter()
                .position(|&b| b == id)
            {
                return vec![PanelAction::ClipDetectInstrumentToggled(idx)];
            }
            if let Some(idx) = self.instrument_sens_btn_ids.iter().position(|&b| b == id) {
                return vec![PanelAction::ClipDetectSensitivityCycled(idx)];
            }
        }
        if id == self.loop_toggle_btn_id && self.mode_video {
            return vec![PanelAction::ClipLoopToggle];
        }
        Vec::new()
    }

    pub fn handle_pointer_down(&mut self, _node_id: u32, _pos: Vec2) -> Vec<PanelAction> {
        Vec::new()
    }

    pub fn handle_drag(&mut self, _pos: Vec2, _tree: &mut UITree) -> Vec<PanelAction> {
        Vec::new()
    }

    pub fn handle_drag_end(&mut self, _tree: &mut UITree) -> Vec<PanelAction> {
        Vec::new()
    }

    /// Coarse sensitivity label for the per-instrument cycle button.
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
        panel.set_detection(&manifold_core::audio_clip_detection::DetectionConfig::default());
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
        // Quantize toggle + per-instrument rows exist and fire indexed actions.
        assert!(panel.quantize_btn_id >= 0);
        let q = panel.handle_click(panel.quantize_btn_id as u32);
        assert!(matches!(q.as_slice(), [PanelAction::ClipDetectQuantizeToggled]));
        assert_eq!(panel.instrument_enable_btn_ids.len(), 9);
        assert_eq!(panel.instrument_sens_btn_ids.len(), 9);
        let en = panel.handle_click(panel.instrument_enable_btn_ids[2] as u32);
        assert!(matches!(en.as_slice(), [PanelAction::ClipDetectInstrumentToggled(2)]));
        let sn = panel.handle_click(panel.instrument_sens_btn_ids[4] as u32);
        assert!(matches!(sn.as_slice(), [PanelAction::ClipDetectSensitivityCycled(4)]));
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
    fn is_dragging_always_false() {
        let panel = ClipChromePanel::new();
        assert!(!panel.is_dragging());
    }
}
