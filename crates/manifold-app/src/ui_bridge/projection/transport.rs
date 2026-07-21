//! Transport projection: the per-frame transport display cache and the
//! playhead auto-scroll check. Moved from state_sync.rs (P-P,
//! UI_FUNNEL_DECOMPOSITION_DESIGN.md).

use manifold_core::project::Project;
use crate::ui_root::UIRoot;

/// Cached transport display strings — avoids per-frame `format!` allocations
/// when beat/time/bpm haven't changed (which is most frames when paused).
pub struct TransportDisplayCache {
    // Time display: "MM:SS.T  |  bar.beat.sixteenth"
    prev_mins: i32,
    prev_secs: i32,
    prev_tenths: i32,
    prev_bar: i32,
    prev_beat_in_bar: i32,
    prev_sixteenth: i32,
    cached_display: String,
    // BPM display: "120.0"
    prev_bpm_tenths: i32, // bpm * 10, rounded
    cached_bpm: String,
    // Link peers display: "1 peer" / "N peers"
    prev_link_peers: i32,
    cached_link_peers: String,
}

impl TransportDisplayCache {
    pub fn new() -> Self {
        Self {
            prev_mins: -1,
            prev_secs: -1,
            prev_tenths: -1,
            prev_bar: -1,
            prev_beat_in_bar: -1,
            prev_sixteenth: -1,
            cached_display: String::new(),
            prev_bpm_tenths: -1,
            cached_bpm: String::new(),
            prev_link_peers: -1,
            cached_link_peers: String::new(),
        }
    }

    /// Returns the formatted display string, only reformatting when values change.
    pub(crate) fn time_display(
        &mut self,
        mins: i32,
        secs: i32,
        tenths: i32,
        bar: i32,
        beat_in_bar: i32,
        sixteenth: i32,
    ) -> &str {
        if mins != self.prev_mins
            || secs != self.prev_secs
            || tenths != self.prev_tenths
            || bar != self.prev_bar
            || beat_in_bar != self.prev_beat_in_bar
            || sixteenth != self.prev_sixteenth
        {
            self.prev_mins = mins;
            self.prev_secs = secs;
            self.prev_tenths = tenths;
            self.prev_bar = bar;
            self.prev_beat_in_bar = beat_in_bar;
            self.prev_sixteenth = sixteenth;
            self.cached_display = format!(
                "{:02}:{:02}.{}  |  {}.{}.{}",
                mins, secs, tenths, bar, beat_in_bar, sixteenth,
            );
        }
        &self.cached_display
    }

    /// Returns the formatted BPM string, only reformatting when value changes.
    pub(crate) fn bpm_display(&mut self, bpm: f32) -> &str {
        let bpm_tenths = (bpm * 10.0).round() as i32;
        if bpm_tenths != self.prev_bpm_tenths {
            self.prev_bpm_tenths = bpm_tenths;
            self.cached_bpm = format!("{:.1}", bpm);
        }
        &self.cached_bpm
    }

    /// Returns the formatted Link peers string, only reformatting when count changes.
    pub fn link_peers_display(&mut self, peers: u32) -> &str {
        if peers as i32 != self.prev_link_peers {
            self.prev_link_peers = peers as i32;
            self.cached_link_peers = match peers {
                0 => String::new(),
                1 => "1 peer".to_string(),
                n => format!("{n} peers"),
            };
        }
        &self.cached_link_peers
    }
}

/// Check auto-scroll during playback and return true if viewport scroll changed.
/// Must run BEFORE build() so the rebuild includes the new scroll position.
/// From Unity ViewportManager.UpdatePlayheadPosition (lines 327-357).
/// BUG-159: playhead-follow yields to an active or just-finished user scroll
/// gesture (wheel, trackpad pan, scrollbar drag) instead of fighting it —
/// Ableton's feel. Re-engage is automatic: once this grace window elapses
/// with no further user gesture, the next `check_auto_scroll` call resumes
/// following on its own, no separate "re-engage" event needed.
const USER_SCROLL_GRACE: std::time::Duration = std::time::Duration::from_millis(800);

pub fn check_auto_scroll(
    ui: &mut UIRoot,
    content_state: &crate::content_state::ContentState,
    project: &Project,
) -> bool {
    if !content_state.is_playing {
        return false;
    }
    // BUG-159: a user scroll gesture (in progress, or within the grace
    // window) owns the viewport — auto-follow must not overwrite it.
    if ui.viewport.scrollbar_h_dragging() || ui.viewport.user_scroll_x_recent(USER_SCROLL_GRACE) {
        return false;
    }

    let playhead_beat = content_state.current_beat.as_f32();
    let ppb = ui.viewport.pixels_per_beat();
    let viewport_w = ui.viewport.tracks_rect().width;
    if viewport_w <= 0.0 || ppb <= 0.0 {
        return false;
    }

    let scroll_x_beats = ui.viewport.scroll_x_beats().as_f32();
    let playhead_px = (playhead_beat - scroll_x_beats) * ppb; // pixel offset from viewport left

    // Content expansion: if playhead approaches end of content, grow it.
    // From Unity ViewportManager.UpdatePlayheadPosition (lines 314-324).
    let content_beats = project.timeline.duration_beats();
    let content_w_px = content_beats.as_f32() * ppb;
    let playhead_abs_px = playhead_beat * ppb;
    if playhead_abs_px > content_w_px - 50.0 {
        // Content would need to grow — handled by sync_project_data setting clips
        // which automatically extends the viewport range. No explicit action needed here
        // since the viewport always shows scroll_x..scroll_x + viewport_w.
    }

    // Right edge margin: 50px. When playhead approaches right, scroll to 25% from left.
    let right_margin_px = 50.0;
    if playhead_px > viewport_w - right_margin_px {
        // Scroll so playhead is at 25% from left (75% ahead)
        let target_scroll_beat = playhead_beat - (viewport_w * 0.25) / ppb;
        ui.viewport
            .set_scroll(target_scroll_beat.max(0.0), ui.viewport.scroll_y_px());
        return true;
    }

    // Left edge margin: 20px. When playhead goes behind left edge, scroll back.
    let left_margin_px = 20.0;
    if playhead_px < left_margin_px {
        let target_scroll_beat = playhead_beat - left_margin_px / ppb;
        ui.viewport
            .set_scroll(target_scroll_beat.max(0.0), ui.viewport.scroll_y_px());
        return true;
    }

    false
}

#[cfg(test)]
mod bug159_auto_scroll_yield_tests {
    use super::*;
    use manifold_core::Beats;

    fn playing_state(beat: f32) -> crate::content_state::ContentState {
        crate::content_state::ContentState {
            current_beat: Beats::from_f32(beat),
            is_playing: true,
            ..Default::default()
        }
    }

    /// A UIRoot laid out through the real production path (one `build()`
    /// pass, same as every live frame) so `viewport.tracks_rect()` is a real
    /// nonzero rect — `check_auto_scroll`'s edge margins (50px right, 20px
    /// left) need that to be reachable at all.
    fn wide_ui_root() -> UIRoot {
        let mut ui = UIRoot::new();
        ui.build();
        ui.viewport.set_zoom(20.0); // pixels-per-beat, so a few hundred beats span the viewport
        ui
    }

    #[test]
    fn auto_scroll_moves_when_no_user_gesture_is_active() {
        let mut ui = wide_ui_root();
        let project = Project::default();
        // Push the playhead far enough right to cross the right-edge margin.
        let state = playing_state(500.0);
        let moved = check_auto_scroll(&mut ui, &state, &project);
        assert!(moved, "auto-scroll must engage with no competing user gesture");
    }

    #[test]
    fn auto_scroll_yields_to_a_recent_user_scroll_gesture() {
        let mut ui = wide_ui_root();
        ui.viewport.note_user_scroll_x();
        let project = Project::default();
        let state = playing_state(500.0);
        let moved = check_auto_scroll(&mut ui, &state, &project);
        assert!(
            !moved,
            "auto-scroll must yield while a user scroll gesture is recent — \
             BUG-159's violent snap-back is exactly this check missing"
        );
    }
}
