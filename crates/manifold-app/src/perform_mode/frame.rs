//! The shared perform-mode seam (BUG-12iyh (UI runner drives editor window,
//! perform mode and text input), perform part). `editor_frame.rs`'s shape for
//! the perform screen: what entry does once an output window exists, and what
//! one perform frame takes in, derives and draws. The live App
//! (`lifecycle.rs`, `render.rs`) and the ui-snap script runner call the same
//! functions.
//!
//! What differs is input presence only. The live App has an output window
//! (entry aborts without one), a surface-sized offscreen, the preview bridge
//! and a drawable to present; the runner has none of them, so it enters
//! directly, draws at the fixture's logical size and reads the offscreen back
//! instead of presenting it.

use std::sync::Arc;

use manifold_core::types::ClockAuthority;
use manifold_gpu::{GpuDevice, GpuEncoder, GpuLoadAction, GpuTexture};
use manifold_playback::ableton_bridge::AbletonSession;
use manifold_ui::node::{Rect, Vec2};
use manifold_ui::text::TextMeasure;
use manifold_ui_paint::ui_renderer::UIRenderer;

use super::{cue, macros as perform_macros, render, tracks, PerformModeState};
use crate::content_state::ContentState;
use crate::text_input::TextInputState;
use crate::ui_root::UIRoot;

/// Mapped Ableton macros shown in the left column; more would overflow a
/// tall project.
const MACRO_DISPLAY_LIMIT: usize = 12;

/// Entry once an output window exists: quiesce in-flight UI state so nothing
/// is left dangling when normal mode resumes, then switch the main window to
/// the perform screen. `held_pointer` is the cursor and clock of a mouse
/// button still down, released here.
pub(crate) fn enter(
    perform: &mut PerformModeState,
    ui_root: &mut UIRoot,
    text_input: &mut TextInputState,
    held_pointer: Option<(Vec2, f32)>,
) {
    text_input.cancel();
    ui_root.dropdown.close(&mut ui_root.tree);
    ui_root.browser_popup.close();
    ui_root.ableton_picker.close();
    if let Some((pos, time)) = held_pointer {
        ui_root.pointer_event(pos, manifold_ui::input::PointerAction::Up, time);
    }
    perform.exit_button_hover = false;
    perform.active = true;
}

/// The perform screen's geometry for one frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PerformLayout {
    pub logical_w: u32,
    pub logical_h: u32,
    /// Small and bottom-centred, so it is hard to hit by accident mid-show.
    pub exit_button: Rect,
}

impl PerformLayout {
    pub(crate) fn compute(logical_w: u32, logical_h: u32) -> Self {
        let (lw, lh) = (logical_w as f32, logical_h as f32);
        let (btn_w, btn_h) = (180.0_f32, 36.0_f32);
        let exit_button = Rect::new((lw - btn_w) * 0.5, lh - btn_h - 24.0, btn_w, btn_h);
        Self { logical_w, logical_h, exit_button }
    }
}

/// One perform frame's intake: takes the content thread's shared lists by
/// pointer (the event drain that normally takes the Ableton session does not
/// run in perform mode), lays the screen out and stores the exit button's hit
/// rect for the input handlers.
pub(crate) fn begin_perform_frame(
    perform: &mut PerformModeState,
    ui_root: &mut UIRoot,
    content_state: &ContentState,
    logical_w: u32,
    logical_h: u32,
) -> PerformLayout {
    ui_root.intake_shared(content_state);
    let layout = PerformLayout::compute(logical_w, logical_h);
    perform.exit_button_rect = layout.exit_button;
    layout
}

/// Sync source readout for the top-left badges.
#[derive(Debug)]
pub(crate) struct SyncStatus {
    pub authority: ClockAuthority,
    pub link_enabled: bool,
    pub link_peers: i32,
    pub midi_clock_enabled: bool,
    pub midi_clock_receiving: bool,
    pub midi_clock_position_display: Arc<str>,
    pub midi_clock_device_name: Arc<str>,
}

/// Everything the perform screen shows, derived from the content state, the
/// Ableton session and the project. Cue and track names borrow from the
/// session snapshot, so a frame copies no cue list.
#[derive(Debug)]
pub(crate) struct PerformHud<'a> {
    pub current_name: &'a str,
    pub next_name: &'a str,
    pub countdown: Option<cue::CountdownDisplay>,
    pub section_progress: Option<f64>,
    pub bar_beat: cue::BarBeatDisplay,
    pub now_section_tracks: Vec<&'a str>,
    pub next_section_tracks: Vec<&'a str>,
    pub bpm: f64,
    pub is_playing: bool,
    pub ableton_connected: bool,
    pub cues_empty: bool,
    pub sync: SyncStatus,
    pub macros: Vec<perform_macros::MacroDisplay>,
}

impl<'a> PerformHud<'a> {
    pub(crate) fn derive(
        content_state: &ContentState,
        session: Option<&'a AbletonSession>,
        project: &manifold_core::project::Project,
    ) -> Self {
        let current_beat = content_state.current_beat.0;
        let beats_per_bar = content_state.time_signature_numerator.max(1) as u32;
        let cue_points: &'a [manifold_playback::ableton_bridge::CuePoint] =
            session.map(|s| s.cue_points.as_slice()).unwrap_or(&[]);
        let analysis = cue::analyze(cue_points, current_beat);
        // A section's tracks: any non-muted clip overlapping
        // `[start, end)`, straddlers included.
        let section_tracks = |start: f64, end: f64| -> Vec<&'a str> {
            session
                .and_then(|s| s.play_group.as_ref())
                .map(|g| {
                    g.tracks
                        .iter()
                        .filter(|t| tracks::plays_in_range(t, start, end))
                        .map(|t| t.name.as_str())
                        .collect()
                })
                .unwrap_or_default()
        };
        let now_section_tracks = match analysis.current {
            Some(cur) => section_tracks(cur.time, analysis.next.map(|c| c.time).unwrap_or(f64::INFINITY)),
            None => Vec::new(),
        };
        // The next section ends at the cue after `next`, or never.
        let next_section_tracks = match analysis.next {
            Some(next) => {
                let end = cue_points
                    .iter()
                    .find(|c| c.time > next.time)
                    .map(|c| c.time)
                    .unwrap_or(f64::INFINITY);
                section_tracks(next.time, end)
            }
            None => Vec::new(),
        };
        Self {
            current_name: analysis.current.map(|c| c.name.as_str()).unwrap_or("—"),
            next_name: analysis.next.map(|c| c.name.as_str()).unwrap_or("—"),
            countdown: analysis.beats_to_next.map(|b| cue::format_countdown(b, beats_per_bar)),
            section_progress: cue::section_progress(analysis.current, analysis.next, current_beat),
            bar_beat: cue::format_bar_beat(current_beat, beats_per_bar),
            now_section_tracks,
            next_section_tracks,
            bpm: content_state.bpm,
            is_playing: content_state.is_playing,
            ableton_connected: content_state.ableton_connected,
            cues_empty: cue_points.is_empty(),
            sync: SyncStatus {
                authority: content_state.clock_authority,
                link_enabled: content_state.link_enabled,
                link_peers: content_state.link_peers,
                midi_clock_enabled: content_state.midi_clock_enabled,
                midi_clock_receiving: content_state.midi_clock_receiving,
                midi_clock_position_display: content_state.midi_clock_position_display.clone(),
                midi_clock_device_name: content_state.midi_clock_device_name.clone(),
            },
            macros: session
                .map(|s| perform_macros::snapshot(project, s, MACRO_DISPLAY_LIMIT))
                .unwrap_or_default(),
        }
    }
}

/// Composites the perform screen into `offscreen` on `encoder`: clear to
/// black, then the HUD's draw list (cues, sync badges, macros, exit button)
/// at `layout`'s logical size. Does not commit, so the caller can add the
/// compositor preview blit to the same encoder; does not present.
#[allow(clippy::too_many_arguments)]
pub(crate) fn composite_perform_frame(
    device: &GpuDevice,
    encoder: &mut GpuEncoder,
    ui_renderer: Option<&mut UIRenderer>,
    tm: &dyn TextMeasure,
    layout: &PerformLayout,
    hud: &PerformHud<'_>,
    exit_hover: bool,
    offscreen: &GpuTexture,
    scale: f64,
) {
    encoder.clear_texture(offscreen, 0.0, 0.0, 0.0, 1.0);
    let Some(ui) = ui_renderer else { return };
    let (lw, lh) = (layout.logical_w as f32, layout.logical_h as f32);
    ui.begin_frame();
    manifold_core::work_count::record_work(manifold_core::work_count::WorkKind::PerformHudBuild);
    render::draw_cue_hud(ui, tm, lw, lh, hud);
    render::draw_sync_indicators(ui, tm, &hud.sync);
    if !hud.macros.is_empty() {
        render::draw_macros_column(ui, lh, &hud.macros);
    }
    render::draw_exit_button(ui, tm, layout.exit_button, exit_hover);
    if ui.prepare(device, layout.logical_w, layout.logical_h, scale) {
        ui.render(encoder, offscreen, GpuLoadAction::Load);
    }
}
