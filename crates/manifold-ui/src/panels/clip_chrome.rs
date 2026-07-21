//! Clip inspector card on the Chrome API (hybrid).
//!
//! Host owns the declarative chrome — the name row, dividers, the video / audio /
//! generator mode sections, every button (keyed + `.inert()`, routed by
//! `handle_click`), the `slider_row` slots for the onset + per-instrument
//! sensitivity sliders, and keyed slots for the dropdown triggers and the
//! detection progress bar (imperative sub-widgets dropped into their slots, the
//! next blocks to typify). The sliders are materialised by the host; their drag
//! stays with `SliderDragState`. Public interface unchanged → inspector untouched.

use crate::{ClipAction};
use super::PanelAction;
use super::param_slider_shared::dropdown_trigger_view;
use crate::chrome::{Align, ChromeHost, Pad, Sizing, SliderSpec, View, components};
use crate::color;
use crate::node::*;
use crate::slider::{BitmapSlider, SliderColors, SliderDragState};
use crate::tree::UITree;
use manifold_foundation::Beats;

// ── Layout constants (from ClipChromeBitmapPanel.cs) ──────────────

const NAME_ROW_H: f32 = 20.0;
const SECTION_LABEL_H: f32 = 18.0;
const SMALL_ROW_H: f32 = 18.0;
const BPM_ROW_H: f32 = 22.5;
const LOOP_BUTTON_H: f32 = 24.0;
const PROGRESS_H: f32 = 6.0;
const INSTR_ROW_H: f32 = 20.0;
const DISABLED_ROW_H: f32 = 15.0;
const QO_ROW_H: f32 = 22.0;
const TOGGLE_W: f32 = 18.0;
const ONSET_MIN_MS: f32 = -50.0;
const ONSET_MAX_MS: f32 = 50.0;
const DIVIDER_H: f32 = 1.0;
const PAD_H: f32 = color::SECTION_CONTENT_INSET; // §14.5 C: align with card param-label column
const PAD_V: f32 = 2.0;
const GAP: f32 = 4.0;
const SOURCE_LABEL_W: f32 = 52.0;
const BPM_LABEL_W: f32 = 52.0;
const ROW_BTN_H: f32 = 18.0;
const FONT_SIZE: u16 = color::FONT_BODY;
const NAME_FONT_SIZE: u16 = color::FONT_SUBHEADING;
const SMALL_FONT_SIZE: u16 = color::FONT_LABEL;

use crate::color::{BPM_BTN_COLOR, GEN_TYPE_COLOR, LOOP_ON_COLOR, darken};

// Stable keys.
const KEY_BPM: u64 = 1;
const KEY_WARP: u64 = 2;
const KEY_LOOP: u64 = 3;
const KEY_DETECT: u64 = 4;
const KEY_CLEAR: u64 = 5;
const KEY_QUANTIZE: u64 = 6;
const KEY_ONSET: u64 = 7;
const KEY_PROGRESS_SLOT: u64 = 8;
const KEY_REPLACE_AUDIO: u64 = 9;
const KEY_INSTR_ENABLE_BASE: u64 = 100;
const KEY_INSTR_SLIDER_BASE: u64 = 200;
const KEY_INSTR_LAYER_BASE: u64 = 300;

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

/// Everything the detection inspector renders, assembled in `state_sync`.
#[derive(Clone, Debug, Default)]
pub struct DetectionView {
    pub quantize_label: String,
    pub onset_ms: f32,
    pub has_analysis: bool,
    pub instruments: Vec<DetectInstrumentRow>,
}

pub struct ClipChromePanel {
    host: ChromeHost,
    chrome_rect: Rect,

    onset_slider: SliderDragState,
    instrument_sens_sliders: Vec<SliderDragState>,
    /// Resolved at build from the host keys — used by `handle_click`'s position().
    instrument_enable_btn_ids: Vec<Option<NodeId>>,
    instrument_layer_btn_ids: Vec<Option<NodeId>>,
    /// Detection progress bar (built into its keyed slot, updated by sync).
    detect_progress_bg_id: Option<NodeId>,
    detect_progress_fill_id: Option<NodeId>,
    detect_progress_bg_rect: Rect,

    // State
    is_collapsed: bool,
    has_clip: bool,
    mode_video: bool,
    mode_generator: bool,
    mode_audio: bool,
    mode_looping: bool,

    // Cached values (the source the chrome_view reads).
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

    first_node: usize,
    node_count: usize,
}

impl ClipChromePanel {
    pub fn new() -> Self {
        Self {
            host: ChromeHost::new(),
            chrome_rect: Rect::ZERO,
            onset_slider: SliderDragState::with_range(ONSET_MIN_MS, ONSET_MAX_MS, true),
            instrument_sens_sliders: Vec::new(),
            instrument_enable_btn_ids: Vec::new(),
            instrument_layer_btn_ids: Vec::new(),
            detect_progress_bg_id: None,
            detect_progress_fill_id: None,
            detect_progress_bg_rect: Rect::ZERO,
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
                h += SECTION_LABEL_H + SMALL_ROW_H + LOOP_BUTTON_H + BPM_ROW_H;
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
    pub fn has_clip(&self) -> bool {
        self.has_clip
    }
    pub fn clear_nodes(&mut self) {
        self.first_node = usize::MAX;
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

    // Kept as a no-op for callers that still reference it
    pub fn set_loop_range(&mut self, _max_beats: Beats) {}

    // ── View description ──────────────────────────────────────────

    fn divider() -> View {
        View::panel().fill_w().h(Sizing::Fixed(DIVIDER_H)).bg(color::DIVIDER_C32)
    }

    fn section_label(text: &str) -> View {
        View::label(text)
            .fill_w()
            .h(Sizing::Fixed(SECTION_LABEL_H))
            .font(SMALL_FONT_SIZE)
            .text_color(color::TEXT_DIMMED_C32)
            .align_text(TextAlign::Left)
    }

    fn toggle_button(text: &str, on: bool, key: u64) -> View {
        // The kit state button — fills with the loop-blue when on, recesses to the
        // neutral chip when off.
        View::button(text)
            .fill_w()
            .h(Sizing::Fixed(LOOP_BUTTON_H))
            .style(UIStyle {
                font_size: SMALL_FONT_SIZE,
                ..components::state_button_style(LOOP_ON_COLOR, on)
            })
            .inert()
            .key(key)
    }

    /// The "<label>  [ value button ]" tempo row shared by video + audio.
    fn bpm_row(&self, label: &str) -> View {
        View::row(GAP)
            .fill_w()
            .h(Sizing::Fixed(BPM_ROW_H))
            .cross_align(Align::Center)
            .child(
                View::label(label)
                    .w(Sizing::Fixed(BPM_LABEL_W))
                    .fill_h()
                    .font(FONT_SIZE)
                    .text_color(color::TEXT_DIMMED_C32)
                    .align_text(TextAlign::Left),
            )
            .child(
                View::button(self.cached_bpm_text.as_str())
                    .fill_w()
                    .h(Sizing::Fixed(ROW_BTN_H))
                    .style(UIStyle {
                        font_size: FONT_SIZE,
                        ..components::button_secondary_style()
                    })
                    .inert()
                    .key(KEY_BPM),
            )
    }

    fn video_section(&self, children: &mut Vec<View>) {
        children.push(Self::section_label("Source"));
        // Source name, indented under the section label.
        children.push(
            View::row(0.0)
                .fill_w()
                .h(Sizing::Fixed(SMALL_ROW_H))
                .child(View::panel().w(Sizing::Fixed(SOURCE_LABEL_W + GAP)).fill_h())
                .child(
                    View::label(self.cached_source_name.as_str())
                        .fill_w()
                        .fill_h()
                        .font(FONT_SIZE)
                        .text_color(color::TEXT_DIMMED_C32)
                        .align_text(TextAlign::Left),
                ),
        );
        children.push(self.bpm_row("Src BPM"));
        children.push(Self::toggle_button(
            if self.cached_loop_enabled { "Loop ON" } else { "Loop OFF" },
            self.cached_loop_enabled,
            KEY_LOOP,
        ));
    }

    fn gen_section(&self, children: &mut Vec<View>) {
        children.push(
            View::label(format!("Type: {}", self.cached_gen_type))
                .fill_w()
                .h(Sizing::Fixed(SMALL_ROW_H))
                .font(SMALL_FONT_SIZE)
                .text_color(GEN_TYPE_COLOR)
                .align_text(TextAlign::Left),
        );
    }

    fn audio_section(&self, children: &mut Vec<View>) {
        children.push(Self::section_label("Source"));
        // The filename row is a button: click opens a file dialog and replaces
        // the clip's audio source (ReplaceAudioFileCommand), keeping the clip,
        // its lane, and its detection config/routing. See TIMELINE_INGEST_DESIGN D6/D7.
        children.push(
            View::button(self.cached_source_name.as_str())
                .fill_w()
                .h(Sizing::Fixed(SMALL_ROW_H))
                .style(UIStyle {
                    font_size: FONT_SIZE,
                    ..components::button_secondary_style()
                })
                .align_text(TextAlign::Left)
                .inert()
                .key(KEY_REPLACE_AUDIO),
        );
        children.push(Self::toggle_button(
            if self.cached_warp_enabled { "Warp ON" } else { "Warp OFF" },
            self.cached_warp_enabled,
            KEY_WARP,
        ));
        children.push(self.bpm_row("Clip BPM"));

        // Detection section.
        children.push(Self::section_label("Detection"));
        children.push(
            View::label(self.cached_detect_status.as_str())
                .fill_w()
                .h(Sizing::Fixed(SMALL_ROW_H))
                .font(SMALL_FONT_SIZE)
                .text_color(color::TEXT_DIMMED_C32)
                .align_text(TextAlign::Left),
        );
        // Progress-bar slot (bg + fill built imperatively, updated by sync).
        children.push(
            View::panel()
                .fill_w()
                .h(Sizing::Fixed(PROGRESS_H))
                .key(KEY_PROGRESS_SLOT),
        );
        let detect_label = if self.cached_detect_show {
            "Detecting\u{2026}"
        } else if self.cached_has_analysis {
            "Re-detect & Group"
        } else {
            "Detect and Group"
        };
        children.push(
            View::button(detect_label)
                .fill_w()
                .h(Sizing::Fixed(LOOP_BUTTON_H))
                .style(UIStyle {
                    font_size: SMALL_FONT_SIZE,
                    ..components::button_secondary_style()
                })
                .inert()
                .key(KEY_DETECT),
        );
        // Quantize dropdown (left half) + onset slider (right half).
        children.push(
            View::row(GAP)
                .fill_w()
                .h(Sizing::Fixed(QO_ROW_H))
                .cross_align(Align::Center)
                .child(
                    dropdown_trigger_view(
                        &format!("Grid {}", self.cached_quantize_label),
                        SMALL_FONT_SIZE,
                    )
                    .fill_w()
                    .h(Sizing::Fixed(ROW_BTN_H))
                    .inert()
                    .key(KEY_QUANTIZE),
                )
                .child(
                    View::slider_row(SliderSpec {
                        label: Some("Onset".to_string()),
                        value: BitmapSlider::value_to_normalized(
                            self.cached_onset_ms,
                            ONSET_MIN_MS,
                            ONSET_MAX_MS,
                        ),
                        // 0ms is the natural "no compensation" default.
                        default: BitmapSlider::value_to_normalized(0.0, ONSET_MIN_MS, ONSET_MAX_MS),
                        value_text: format!("{:+.0}ms", self.cached_onset_ms),
                        colors: SliderColors::default_slider(),
                        font_size: SMALL_FONT_SIZE,
                        label_width: 34.0,
                        // A detection-config slider excluded from BUG-061 (no
                        // Snapshot/Commit undo trio — `ClipDetectOnsetChanged`
                        // writes the model directly on every change, no open/
                        // close phase). The reset is still a real, working
                        // trio: reusing the one existing action for all three
                        // slots IS "a drag that lands on the default" — there
                        // is no separate snapshot/commit to invent.
                        reset: PanelAction::slider_reset(
                            PanelAction::Clip(ClipAction::ClipDetectOnsetChanged(0.0)),
                            PanelAction::Clip(ClipAction::ClipDetectOnsetChanged(0.0)),
                            PanelAction::Clip(ClipAction::ClipDetectOnsetChanged(0.0)),
                        ),
                    })
                    .fill_w()
                    .h(Sizing::Fixed(ROW_BTN_H))
                    .key(KEY_ONSET),
                ),
        );
        // Per-instrument rows.
        for (i, inst) in self.cached_instruments.iter().enumerate() {
            children.push(self.instrument_row(i, inst));
        }
        children.push(
            View::button("Clear Triggers")
                .fill_w()
                .h(Sizing::Fixed(LOOP_BUTTON_H))
                .style(UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: SMALL_FONT_SIZE,
                    ..components::button_secondary_style()
                })
                .inert()
                .key(KEY_CLEAR),
        );
    }

    fn instrument_row(&self, i: usize, inst: &DetectInstrumentRow) -> View {
        let row_h = if inst.enabled { INSTR_ROW_H } else { DISABLED_ROW_H };
        let toggle_h = (row_h - 2.0).max(10.0);
        let enable = View::button(if inst.enabled { "\u{2713}" } else { "" })
            .w(Sizing::Fixed(TOGGLE_W))
            .h(Sizing::Fixed(toggle_h))
            .style(UIStyle {
                font_size: color::FONT_CAPTION,
                ..components::state_button_style(LOOP_ON_COLOR, inst.enabled)
            })
            .inert()
            .key(KEY_INSTR_ENABLE_BASE + i as u64);

        let mut row = View::row(GAP)
            .fill_w()
            .h(Sizing::Fixed(row_h))
            .cross_align(Align::Center)
            .child(enable);

        if !inst.enabled {
            // Collapsed line: just the dimmed name.
            return row.child(
                View::label(inst.label.as_str())
                    .fill_w()
                    .fill_h()
                    .font(SMALL_FONT_SIZE)
                    .text_color(color::TEXT_DIMMED_C32)
                    .align_text(TextAlign::Left),
            );
        }

        let name_w = 46.0;
        let layer_w = 64.0;
        let count_w = 24.0;
        row = row
            .child(
                View::label(inst.label.as_str())
                    .w(Sizing::Fixed(name_w))
                    .fill_h()
                    .font(SMALL_FONT_SIZE)
                    .text_color(color::TEXT_PRIMARY_C32)
                    .align_text(TextAlign::Left),
            )
            .child(
                View::slider_row(SliderSpec {
                    label: None,
                    value: inst.sensitivity.clamp(0.0, 1.0),
                    // No semantic default exists per-instrument; neutral
                    // midpoint.
                    default: 0.5,
                    value_text: String::new(),
                    colors: SliderColors::default_slider(),
                    font_size: SMALL_FONT_SIZE,
                    label_width: 0.0,
                    // Same "excluded from BUG-061, real reset via the one
                    // existing action" reasoning as the Onset slider above.
                    reset: PanelAction::slider_reset(
                        PanelAction::Clip(ClipAction::ClipDetectSensitivityChanged(i, 0.5)),
                        PanelAction::Clip(ClipAction::ClipDetectSensitivityChanged(i, 0.5)),
                        PanelAction::Clip(ClipAction::ClipDetectSensitivityChanged(i, 0.5)),
                    ),
                })
                .fill_w()
                .fill_h()
                .key(KEY_INSTR_SLIDER_BASE + i as u64),
            )
            .child(
                View::label(inst.count.to_string())
                    .w(Sizing::Fixed(count_w))
                    .fill_h()
                    .font(color::FONT_CAPTION)
                    .text_color(color::TEXT_DIMMED_C32)
                    .align_text(TextAlign::Right),
            )
            .child(
                dropdown_trigger_view(inst.layer_label.as_str(), color::FONT_CAPTION)
                    .w(Sizing::Fixed(layer_w))
                    .h(Sizing::Fixed(16.0))
                    .inert()
                    .key(KEY_INSTR_LAYER_BASE + i as u64),
            );
        row
    }

    fn chrome_view(&self) -> View {
        let mut root = View::column(0.0)
            .fill()
            .pad(Pad { l: PAD_H, t: PAD_V, r: PAD_H, b: PAD_V })
            .child(
                View::label(self.cached_name.as_str())
                    .fill_w()
                    .h(Sizing::Fixed(NAME_ROW_H))
                    .font(NAME_FONT_SIZE)
                    .text_color(color::TEXT_PRIMARY_C32)
                    .align_text(TextAlign::Center),
            );

        if self.has_clip {
            let mut section = Vec::new();
            if self.mode_video {
                self.video_section(&mut section);
            } else if self.mode_audio {
                self.audio_section(&mut section);
            } else if self.mode_generator {
                self.gen_section(&mut section);
            }
            root = root.child(Self::divider()).children(section).child(Self::divider()).child(
                View::label("Effects")
                    .fill_w()
                    .h(Sizing::Fixed(SECTION_LABEL_H))
                    .font(SMALL_FONT_SIZE)
                    .text_color(color::TEXT_DIMMED_C32)
                    .align_text(TextAlign::Left),
            );
        }
        root
    }

    // ── Build ────────────────────────────────────────────────────

    pub fn build(&mut self, tree: &mut UITree, rect: Rect) {
        self.chrome_rect = rect;
        let view = self.chrome_view();
        self.host.build(tree, &view, rect);
        self.first_node = self.host.first_node();

        // Onset + per-instrument sensitivity sliders (host-materialised).
        match self.host.slider_ids(KEY_ONSET) {
            Some(ids) => self.onset_slider.set_ids(ids),
            None => self.onset_slider.clear(),
        }
        for (i, s) in self.instrument_sens_sliders.iter_mut().enumerate() {
            match self.host.slider_ids(KEY_INSTR_SLIDER_BASE + i as u64) {
                Some(ids) => s.set_ids(ids),
                None => s.clear(),
            }
        }

        // The quantize + per-instrument layer dropdown triggers are now typed
        // View components (resolved by key); only the progress bar is still an
        // imperative sub-widget dropped into its keyed slot.
        self.detect_progress_bg_id = None;
        self.detect_progress_fill_id = None;
        self.instrument_enable_btn_ids.clear();
        self.instrument_layer_btn_ids.clear();

        if self.mode_audio && self.has_clip {
            if let Some(slot) = self.slot_rect(KEY_PROGRESS_SLOT, tree) {
                self.detect_progress_bg_rect = slot;
                let bg = tree.add_panel(
                    None,
                    slot.x,
                    slot.y,
                    slot.width,
                    slot.height,
                    UIStyle {
                        bg_color: darken(BPM_BTN_COLOR, 35),
                        corner_radius: color::SMALL_RADIUS,
                        ..UIStyle::default()
                    },
                );
                let fill_w = (slot.width * self.cached_detect_progress.clamp(0.0, 1.0)).max(0.0);
                let fill = tree.add_panel(
                    None,
                    slot.x,
                    slot.y,
                    fill_w,
                    slot.height,
                    UIStyle {
                        bg_color: BPM_BTN_COLOR,
                        corner_radius: color::SMALL_RADIUS,
                        ..UIStyle::default()
                    },
                );
                tree.set_visible(bg, self.cached_detect_show);
                tree.set_visible(fill, self.cached_detect_show);
                self.detect_progress_bg_id = Some(bg);
                self.detect_progress_fill_id = Some(fill);
            }
            for i in 0..self.cached_instruments.len() {
                self.instrument_enable_btn_ids
                    .push(self.host.node_id_for_key(KEY_INSTR_ENABLE_BASE + i as u64));
                self.instrument_layer_btn_ids
                    .push(self.host.node_id_for_key(KEY_INSTR_LAYER_BASE + i as u64));
            }
        }

        self.node_count = tree.count() - self.first_node;
    }

    fn slot_rect(&self, key: u64, tree: &UITree) -> Option<Rect> {
        self.host.node_id_for_key(key).map(|id| tree.get_bounds(id))
    }

    fn reconcile_chrome(&mut self, tree: &mut UITree) {
        if !self.host.is_built() {
            return;
        }
        let view = self.chrome_view();
        let _ = self.host.update(tree, &view, self.chrome_rect);
    }

    // ── Sync methods ─────────────────────────────────────────────

    pub fn sync_name(&mut self, tree: &mut UITree, name: &str) {
        if self.cached_name == name {
            return;
        }
        self.cached_name = name.into();
        self.reconcile_chrome(tree);
    }

    pub fn sync_collapsed(&mut self, _tree: &mut UITree, collapsed: bool) {
        self.is_collapsed = collapsed;
    }

    pub fn sync_source_name(&mut self, tree: &mut UITree, name: &str) {
        if self.cached_source_name == name {
            return;
        }
        self.cached_source_name = name.into();
        self.reconcile_chrome(tree);
    }

    pub fn sync_loop_duration(&mut self, _tree: &mut UITree, _beats: Beats) {}

    pub fn sync_bpm(&mut self, tree: &mut UITree, text: &str) {
        if self.cached_bpm_text == text {
            return;
        }
        self.cached_bpm_text = text.into();
        self.reconcile_chrome(tree);
    }

    pub fn sync_warp_enabled(&mut self, tree: &mut UITree, enabled: bool) {
        if self.cached_warp_enabled == enabled {
            return;
        }
        self.cached_warp_enabled = enabled;
        self.reconcile_chrome(tree);
    }

    /// Set the detection view the rows render from. Called before build.
    pub fn set_detection(&mut self, view: &DetectionView) {
        self.cached_quantize_label = view.quantize_label.clone();
        self.cached_onset_ms = view.onset_ms;
        self.cached_has_analysis = view.has_analysis;
        self.cached_instruments = view.instruments.clone();
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
        // Status line text rides on the chrome description.
        self.reconcile_chrome(tree);
        if let Some(id) = self.detect_progress_bg_id {
            tree.set_visible(id, show);
        }
        if let Some(id) = self.detect_progress_fill_id {
            tree.set_visible(id, show);
            let bg = self.detect_progress_bg_rect;
            tree.set_bounds(
                id,
                Rect::new(bg.x, bg.y, bg.width * self.cached_detect_progress, bg.height),
            );
        }
    }

    pub fn sync_gen_type(&mut self, tree: &mut UITree, gen_type: &str) {
        if self.cached_gen_type == gen_type {
            return;
        }
        self.cached_gen_type = gen_type.into();
        self.reconcile_chrome(tree);
    }

    pub fn sync_loop_enabled(&mut self, tree: &mut UITree, enabled: bool) {
        if self.cached_loop_enabled == enabled {
            return;
        }
        self.cached_loop_enabled = enabled;
        self.reconcile_chrome(tree);
    }

    // ── Event handling ───────────────────────────────────────────

    pub fn handle_click(&self, node_id: NodeId) -> Vec<PanelAction> {
        let key_is = |k: u64| self.host.node_id_for_key(k) == Some(node_id);

        if key_is(KEY_BPM) && (self.mode_video || self.mode_audio) {
            return vec![PanelAction::Clip(ClipAction::ClipBpmClicked)];
        }
        if key_is(KEY_WARP) && self.mode_audio {
            return vec![PanelAction::Clip(ClipAction::ClipWarpToggled)];
        }
        if key_is(KEY_DETECT) && self.mode_audio && !self.cached_detect_show {
            return vec![PanelAction::Clip(ClipAction::ClipDetectClicked)];
        }
        if key_is(KEY_CLEAR) && self.mode_audio {
            return vec![PanelAction::Clip(ClipAction::ClipClearTriggersClicked)];
        }
        if key_is(KEY_REPLACE_AUDIO) && self.mode_audio {
            return vec![PanelAction::Clip(ClipAction::ClipReplaceAudioClicked)];
        }
        if key_is(KEY_QUANTIZE) && self.mode_audio {
            return vec![PanelAction::Clip(ClipAction::ClipDetectQuantizeClicked)];
        }
        if self.mode_audio {
            if let Some(idx) = self
                .instrument_enable_btn_ids
                .iter()
                .position(|&b| b == Some(node_id))
            {
                return vec![PanelAction::Clip(ClipAction::ClipDetectInstrumentToggled(idx))];
            }
            if let Some(idx) = self
                .instrument_layer_btn_ids
                .iter()
                .position(|&b| b == Some(node_id))
            {
                return vec![PanelAction::Clip(ClipAction::ClipDetectLayerClicked(idx))];
            }
        }
        if key_is(KEY_LOOP) && self.mode_video {
            return vec![PanelAction::Clip(ClipAction::ClipLoopToggle)];
        }
        Vec::new()
    }

    pub fn handle_pointer_down(&mut self, node_id: NodeId, pos: Vec2) -> Vec<PanelAction> {
        if !self.mode_audio {
            return Vec::new();
        }
        if self.onset_slider.try_start_drag(node_id, pos.x).is_some() {
            return Vec::new();
        }
        for slider in self.instrument_sens_sliders.iter_mut() {
            if slider.try_start_drag(node_id, pos.x).is_some() {
                return Vec::new();
            }
        }
        Vec::new()
    }

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

    pub fn handle_drag_end(&mut self, _tree: &mut UITree) -> Vec<PanelAction> {
        if self.onset_slider.end_drag() {
            return vec![PanelAction::Clip(ClipAction::ClipDetectOnsetChanged(self.onset_slider.cached_value()))];
        }
        for (i, slider) in self.instrument_sens_sliders.iter_mut().enumerate() {
            if slider.end_drag() {
                return vec![PanelAction::Clip(ClipAction::ClipDetectSensitivityChanged(i, slider.cached_value()))];
            }
        }
        Vec::new()
    }

    pub fn detect_instruments(&self) -> &[DetectInstrumentRow] {
        &self.cached_instruments
    }

    pub fn bpm_button_rect(&self, tree: &UITree) -> Rect {
        self.host
            .node_id_for_key(KEY_BPM)
            .map(|id| tree.get_bounds(id))
            .unwrap_or(Rect::ZERO)
    }
}

impl Default for ClipChromePanel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::UITree;

    fn rect() -> Rect {
        Rect::new(0.0, 0.0, 280.0, 400.0)
    }

    fn audio_view() -> DetectionView {
        DetectionView {
            quantize_label: "1/16".into(),
            onset_ms: 0.0,
            has_analysis: false,
            instruments: vec![
                DetectInstrumentRow {
                    label: "Kick".into(),
                    enabled: true,
                    sensitivity: 0.5,
                    count: 3,
                    layer_label: "Auto".into(),
                },
                DetectInstrumentRow {
                    label: "Snare".into(),
                    enabled: false,
                    sensitivity: 0.5,
                    count: 0,
                    layer_label: "Auto".into(),
                },
            ],
        }
    }

    #[test]
    fn video_mode_builds_bpm_and_loop() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.set_mode(true, true, false, false, false);
        panel.build(&mut tree, rect());
        assert!(panel.host.node_id_for_key(KEY_BPM).is_some());
        assert!(panel.host.node_id_for_key(KEY_LOOP).is_some());
        assert!(panel.host.node_id_for_key(KEY_WARP).is_none());
    }

    #[test]
    fn audio_mode_materialises_onset_and_instrument_sliders() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.set_mode(true, false, false, true, false);
        panel.set_detection(&audio_view());
        panel.build(&mut tree, rect());

        assert!(panel.onset_slider.ids().is_some(), "onset slider materialised");
        // Enabled instrument (idx 0) has a sensitivity slider; disabled (idx 1) doesn't.
        assert!(panel.instrument_sens_sliders[0].ids().is_some());
        assert!(panel.instrument_sens_sliders[1].ids().is_none());
        assert!(panel.detect_progress_bg_id.is_some());
        assert!(panel.host.node_id_for_key(KEY_QUANTIZE).is_some());
        assert!(panel.host.node_id_for_key(KEY_INSTR_LAYER_BASE).is_some());
    }

    #[test]
    fn audio_clicks_route_by_key() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.set_mode(true, false, false, true, false);
        panel.set_detection(&audio_view());
        panel.build(&mut tree, rect());

        let warp = panel.host.node_id_for_key(KEY_WARP).unwrap();
        assert!(matches!(
            panel.handle_click(warp).as_slice(),
            [PanelAction::Clip(ClipAction::ClipWarpToggled)]
        ));
        let en0 = panel.host.node_id_for_key(KEY_INSTR_ENABLE_BASE).unwrap();
        assert!(matches!(
            panel.handle_click(en0).as_slice(),
            [PanelAction::Clip(ClipAction::ClipDetectInstrumentToggled(0))]
        ));
        let clear = panel.host.node_id_for_key(KEY_CLEAR).unwrap();
        assert!(matches!(
            panel.handle_click(clear).as_slice(),
            [PanelAction::Clip(ClipAction::ClipClearTriggersClicked)]
        ));
        let replace = panel.host.node_id_for_key(KEY_REPLACE_AUDIO).unwrap();
        assert!(matches!(
            panel.handle_click(replace).as_slice(),
            [PanelAction::Clip(ClipAction::ClipReplaceAudioClicked)]
        ));
    }

    #[test]
    fn no_clip_builds_only_name() {
        let mut tree = UITree::new();
        let mut panel = ClipChromePanel::new();
        panel.set_mode(false, false, false, false, false);
        panel.build(&mut tree, rect());
        assert!(panel.host.node_id_for_key(KEY_BPM).is_none());
        assert!(panel.onset_slider.ids().is_none());
    }
}
