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
//! instead of presenting it. Window input and exit run through the same
//! handlers: the App from winit events and `about_to_wait`, the runner from
//! its script.

use std::sync::Arc;

use manifold_core::types::ClockAuthority;
use manifold_gpu::{GpuDevice, GpuEncoder, GpuLoadAction, GpuTexture};
use manifold_playback::ableton_bridge::AbletonSession;
#[cfg(feature = "ui-snapshot")]
use manifold_ui::hit_targets::{HitTargetEntry, HitTargets};
use manifold_ui::node::{Rect, Vec2};
use manifold_ui::text::TextMeasure;
use manifold_ui_paint::ui_cache_manager::UICacheManager;
use manifold_ui_paint::ui_renderer::UIRenderer;

use super::{cue, macros as perform_macros, render, tracks, PerformModeState};
use crate::content_state::ContentState;
use crate::text_input::TextInputState;
use crate::ui_root::UIRoot;

/// Mapped Ableton macros shown in the left column; more would overflow a
/// tall project.
const MACRO_DISPLAY_LIMIT: usize = 12;

/// Entry once an output window exists: quiesce in-flight UI state so nothing
/// is left dangling when normal mode resumes, force Show gizmos off (no
/// editor chrome is armed over a show, SCENE_SETUP_PROGRAMME D6), then switch
/// the main window to the perform screen. `held_pointer` is the cursor and
/// clock of a mouse button still down, released here.
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
    ui_root.scene_setup_panel.hide_gizmos();
    if let Some((pos, time)) = held_pointer {
        ui_root.pointer_event(pos, manifold_ui::input::PointerAction::Up, time);
    }
    perform.exit_button_hover = false;
    perform.shown = None;
    perform.active = true;
}

/// Leaves perform mode when an exit is pending (the exit button, or the
/// output window closing): the frozen tree repaints whole on the first frame
/// back. The output window stays open, so the audience never goes black. True
/// when it exited; the caller then rebuilds. The planner compares against the
/// last normal frame's inputs, so whatever moved during the set re-projects.
pub(crate) fn take_pending_exit(
    perform: &mut PerformModeState,
    ui_root: &mut UIRoot,
    cache: Option<&mut UICacheManager>,
) -> bool {
    if !std::mem::take(&mut perform.pending_exit) || !perform.active {
        return false;
    }
    perform.active = false;
    perform.exit_button_hover = false;
    ui_root.tree.mark_all_dirty();
    if let Some(cm) = cache {
        cm.invalidate_all();
    }
    true
}

/// The cursor moved over the perform screen: the exit button's hover follows
/// it. The hover is a HUD input, so a change redraws the HUD.
pub(crate) fn pointer_moved(perform: &mut PerformModeState, pos: Vec2) {
    perform.exit_button_hover = perform.exit_button_rect.contains(pos);
}

/// The cursor left the window, which clears the hover.
pub(crate) fn pointer_left(perform: &mut PerformModeState) {
    perform.exit_button_hover = false;
}

/// A left-button release: over the exit button it asks to leave. Every other
/// click is dropped.
pub(crate) fn pointer_released(perform: &mut PerformModeState, pos: Vec2) {
    if perform.exit_button_rect.contains(pos) {
        perform.pending_exit = true;
    }
}

/// Whether perform mode takes (and drops) every key. Hardware controllers and
/// OSC drive the show, and only the on-screen button exits, so a stray key,
/// Escape included, cannot drop the HUD mid-set.
pub(crate) fn takes_keys(perform: &PerformModeState) -> bool {
    perform.active
}

/// The exit button as a hit-test surface the automation dump can address
/// (`UI_AUTOMATION_DESIGN.md` D5). It is drawn immediate-mode, outside any tree.
/// The ui-snap runner is its one reader.
#[cfg(feature = "ui-snapshot")]
pub(crate) struct PerformHitTargets(pub Rect);

#[cfg(feature = "ui-snapshot")]
impl HitTargets for PerformHitTargets {
    fn surface_id(&self) -> &'static str {
        "perform"
    }

    fn enumerate(&self, out: &mut Vec<HitTargetEntry>) {
        out.push(HitTargetEntry { kind: "button", label: "Exit".into(), rect: self.0, payload: "exit".into() });
    }
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
    if ui_root.intake_shared(content_state) {
        perform.intake.session += 1;
    }
    perform.intake.take_clock_text(content_state);
    let layout = PerformLayout::compute(logical_w, logical_h);
    perform.exit_button_rect = layout.exit_button;
    layout
}

/// The shared values the HUD shows, compared by retained pointer, never by
/// content (UI_INVALIDATION_DESIGN.md section 3): a version per source.
#[derive(Default)]
pub(crate) struct PerformIntake {
    /// Moves when a new Ableton session arrives: cue, track and macro names,
    /// macro values, the PLAY group.
    pub session: u64,
    /// Moves when the MIDI clock device name or position text changes.
    pub clock_text: u64,
    clock_device: Option<Arc<str>>,
    clock_position: Option<Arc<str>>,
}

impl PerformIntake {
    fn take_clock_text(&mut self, content_state: &ContentState) {
        let mut moved = false;
        for (held, sent) in [
            (&mut self.clock_device, &content_state.midi_clock_device_name),
            (&mut self.clock_position, &content_state.midi_clock_position_display),
        ] {
            if !held.as_ref().is_some_and(|h| Arc::ptr_eq(h, sent)) {
                *held = Some(Arc::clone(sent));
                moved = true;
            }
        }
        if moved {
            self.clock_text += 1;
        }
    }
}

/// Text as the HUD prints it, held on the stack so deriving it per frame
/// allocates nothing.
pub(crate) type ShownText = manifold_core::scratch::StackText<15>;

/// `value` printed with one decimal, exactly as the HUD's `{:.1}`.
fn one_decimal(value: f64) -> ShownText {
    use std::fmt::Write;
    let mut text = ShownText::default();
    let _ = write!(text, "{value:.1}");
    text
}

/// The track rows a section lists, as indices into the PLAY group, at most
/// [`render::MAX_SECTION_TRACKS`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShownTracks {
    index: [u16; render::MAX_SECTION_TRACKS],
    len: u8,
}

impl ShownTracks {
    fn of(session: Option<&AbletonSession>, start: f64, end: f64) -> Self {
        let mut shown = Self::default();
        let Some(group) = session.and_then(|s| s.play_group.as_ref()) else {
            return shown;
        };
        for (i, track) in group.tracks.iter().enumerate() {
            if shown.len as usize == shown.index.len() {
                break;
            }
            if tracks::plays_in_range(track, start, end) {
                shown.index[shown.len as usize] = i as u16;
                shown.len += 1;
            }
        }
        shown
    }
}

/// Everything the perform HUD draws, field by field from `draw_cue_hud`,
/// `draw_sync_indicators`, `draw_macros_column` and `draw_exit_button`, at
/// the resolution it is drawn (UI_INVALIDATION_DESIGN.md D17). Equal inputs
/// keep the frame already on screen, so an idle perform frame derives no
/// strings and lists and draws nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PerformHudInputs {
    /// Cue, track and macro names, macro values and the PLAY group.
    pub session: u64,
    /// The project's mapping set, which picks the macros shown.
    pub project: u64,
    pub current_cue: Option<u16>,
    pub next_cue: Option<u16>,
    /// The countdown number as printed, in bars.
    pub countdown: Option<ShownText>,
    /// Bar, beat and sixteenth, 1-indexed.
    pub bar_beat: [i64; 3],
    /// The section-progress fill width in whole logical pixels.
    pub section_fill_px: Option<u16>,
    pub now_tracks: ShownTracks,
    pub next_tracks: ShownTracks,
    pub bpm: ShownText,
    pub playing: bool,
    pub ableton_connected: bool,
    pub cues_empty: bool,
    pub clock_authority: u8,
    pub link_enabled: bool,
    pub link_peers: i32,
    pub midi_clock_enabled: bool,
    pub midi_clock_receiving: bool,
    pub clock_text: u64,
    pub exit_hover: bool,
    pub size_px: [u32; 2],
    /// The output aspect the preview beside the HUD is fitted to.
    pub preview_aspect_bits: u32,
}

impl PerformHudInputs {
    /// Derive this frame's inputs; allocates nothing.
    pub(crate) fn derive(
        perform: &PerformModeState,
        content_state: &ContentState,
        session: Option<&AbletonSession>,
        project_key: u64,
        layout: &PerformLayout,
        preview_aspect: f32,
    ) -> Self {
        let beat = content_state.current_beat.0;
        let beats_per_bar = content_state.time_signature_numerator.max(1) as u32;
        let cues: &[manifold_playback::ableton_bridge::CuePoint] =
            session.map(|s| s.cue_points.as_slice()).unwrap_or(&[]);
        let current = cues.iter().rposition(|c| c.time <= beat);
        let next = cues.iter().position(|c| c.time > beat);
        let analysis = cue::analyze(cues, beat);
        let now_tracks = match analysis.current {
            Some(cur) => ShownTracks::of(session, cur.time, analysis.next.map_or(f64::INFINITY, |c| c.time)),
            None => ShownTracks::default(),
        };
        let next_tracks = match analysis.next {
            Some(n) => {
                let end = cues.iter().find(|c| c.time > n.time).map_or(f64::INFINITY, |c| c.time);
                ShownTracks::of(session, n.time, end)
            }
            None => ShownTracks::default(),
        };
        let bar_beat = cue::bar_beat_numbers(beat, beats_per_bar);
        let progress = cue::section_progress(analysis.current, analysis.next, beat);
        Self {
            session: perform.intake.session,
            project: project_key,
            current_cue: current.map(|i| i as u16),
            next_cue: next.map(|i| i as u16),
            countdown: analysis
                .beats_to_next
                .map(|b| one_decimal(cue::countdown_bars(b, beats_per_bar))),
            bar_beat,
            section_fill_px: progress.map(|p| render::section_fill_px(layout.logical_w as f32, p)),
            now_tracks,
            next_tracks,
            bpm: one_decimal(content_state.bpm),
            playing: content_state.is_playing,
            ableton_connected: content_state.ableton_connected,
            cues_empty: cues.is_empty(),
            clock_authority: content_state.clock_authority as u8,
            link_enabled: content_state.link_enabled,
            link_peers: content_state.link_peers,
            midi_clock_enabled: content_state.midi_clock_enabled,
            midi_clock_receiving: content_state.midi_clock_receiving,
            clock_text: perform.intake.clock_text,
            exit_hover: perform.exit_button_hover,
            size_px: [layout.logical_w, layout.logical_h],
            preview_aspect_bits: preview_aspect.to_bits(),
        }
    }
}

/// The project parts the macro column reads: which params are mapped to
/// Ableton macros, on which effects and layers.
pub(crate) const HUD_PROJECT_PARTS: manifold_core::change::ChangeSet = manifold_core::change::ChangeSet::of(&[
    manifold_core::change::Part::Mappings,
    manifold_core::change::Part::Effects,
    manifold_core::change::Part::Layers,
]);

/// Whether the HUD must be drawn this frame: its inputs moved since the
/// frame on screen, or `force` (a fresh reference frame).
pub(crate) fn hud_moved(perform: &PerformModeState, inputs: &PerformHudInputs, force: bool) -> bool {
    force || perform.shown.as_ref() != Some(inputs)
}

/// The output aspect the live preview beside the HUD takes; 16:9 without a
/// pipeline, as the preview blit falls back.
pub(crate) fn preview_aspect(dims: Option<(u32, u32)>) -> f32 {
    let (w, h) = dims.unwrap_or((1920, 1080));
    w as f32 / h.max(1) as f32
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

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(perform: &PerformModeState, content: &ContentState) -> PerformHudInputs {
        PerformHudInputs::derive(perform, content, None, 0, &PerformLayout::compute(1920, 1080), preview_aspect(None))
    }

    #[test]
    fn an_unchanged_hud_is_not_drawn_again() {
        let mut perform = PerformModeState::new();
        let content = ContentState::default();
        let first = inputs(&perform, &content);
        assert!(hud_moved(&perform, &first, false), "the first frame draws");
        perform.shown = Some(first);
        assert!(!hud_moved(&perform, &inputs(&perform, &content), false));
        assert!(hud_moved(&perform, &inputs(&perform, &content), true), "a fresh reference always draws");
    }

    #[test]
    fn the_hud_redraws_at_its_shown_resolution() {
        let mut perform = PerformModeState::new();
        let mut content = ContentState::default();
        perform.shown = Some(inputs(&perform, &content));
        // A beat change inside one sixteenth prints the same position.
        content.current_beat = manifold_core::Beats(0.01);
        assert!(!hud_moved(&perform, &inputs(&perform, &content), false));
        content.current_beat = manifold_core::Beats(0.25);
        assert!(hud_moved(&perform, &inputs(&perform, &content), false));
        // A tempo change below the printed decimal draws nothing new.
        perform.shown = Some(inputs(&perform, &content));
        content.bpm += 0.01;
        assert!(!hud_moved(&perform, &inputs(&perform, &content), false));
        content.bpm += 0.1;
        assert!(hud_moved(&perform, &inputs(&perform, &content), false));
    }
}
