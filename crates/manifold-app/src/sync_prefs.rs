//! The UI side of the sync calibration (FRAME_EXACT_TRANSPORT_DESIGN D19):
//! the stored values sent before the content thread's first tick, the
//! learned offsets kept per rig in `UserPrefs`, and the output display's own
//! delay.

use crate::content_command::ContentCommand;
use crate::user_prefs::UserPrefs;

/// Hidden override, ms, 0–250. No footer field and no menu entry.
pub const EXTRA_DELAY_KEY: &str = "MANIFOLD_ExtraOutputDelayMs";
/// Learned offsets against Link, ms: MIDI clock, AbletonOSC, timecode.
pub const OFFSET_KEYS: [&str; 3] = ["MANIFOLD_SyncOffsetClkMs", "MANIFOLD_SyncOffsetOscMs", "MANIFOLD_SyncOffsetTimecodeMs"];
/// UI frames between output-display checks.
const DISPLAY_CHECK_FRAMES: u64 = 60;
/// s: offsets closer than this to the saved ones are not re-saved.
const SAVE_EPSILON: f64 = 1e-6;

/// What the UI remembers between frames.
#[derive(Debug, Default)]
pub struct SyncPrefs {
    /// The offsets last written, s.
    saved: [f64; 3],
    /// The output window's display and when it was last checked.
    display: Option<u32>,
    next_display_check: u64,
}

impl SyncPrefs {
    /// The stored values as the command that restores them, sent before the
    /// content thread starts. Remembers them as saved.
    pub fn stored(&mut self, prefs: &UserPrefs) -> ContentCommand {
        let raw = prefs.get_string(EXTRA_DELAY_KEY, "0");
        let extra_delay_ms = match raw.trim().parse::<f64>() {
            Ok(ms) if ms.is_finite() && (0.0..=250.0).contains(&ms) => ms,
            _ => {
                log::warn!("[Transport] {EXTRA_DELAY_KEY}={raw:?} is not 0-250 ms; using 0");
                0.0
            }
        };
        let offsets_ms = OFFSET_KEYS.map(|key| prefs.get_string(key, "").trim().parse::<f64>().unwrap_or(f64::NAN));
        self.saved = offsets_ms.map(|ms| if ms.is_finite() { (ms / 1000.0).clamp(-0.05, 0.05) } else { 0.0 });
        ContentCommand::SetSyncCalibration { extra_delay_ms, offsets_ms }
    }

    /// Writes learned offsets the content thread reports, when they changed.
    pub fn save_learned(&mut self, prefs: &mut UserPrefs, offsets: [f64; 3]) {
        if offsets.iter().zip(&self.saved).all(|(a, b)| (a - b).abs() <= SAVE_EPSILON) {
            return;
        }
        for (key, offset) in OFFSET_KEYS.iter().zip(offsets) {
            prefs.set_string(key, &format!("{:.3}", offset * 1000.0));
        }
        prefs.save();
        self.saved = offsets;
    }

    /// Once a second of UI frames the output window's display is checked.
    pub fn display_check_due(&mut self, frame: u64) -> bool {
        if frame < self.next_display_check {
            return false;
        }
        self.next_display_check = frame + DISPLAY_CHECK_FRAMES;
        true
    }

    /// On a change of the output window's display, the delay it reports (0
    /// when it reports none).
    pub fn display_changed(&mut self, display: Option<u32>) -> Option<ContentCommand> {
        if display == self.display {
            return None;
        }
        self.display = display;
        let delay = display.and_then(crate::sync_rig_log::display::output_latency_seconds).unwrap_or(0.0);
        log::info!("[Transport] output display {display:?}: reported delay {:.1} ms", delay * 1000.0);
        Some(ContentCommand::SetDisplayDelay(delay))
    }
}

impl crate::app::Application {
    /// Once per UI frame, after the content state drain: save learned
    /// offsets, and tell the content thread when the output window moved to
    /// another display.
    pub(crate) fn tick_sync_calibration(&mut self) {
        self.sync_prefs.save_learned(&mut self.user_prefs, self.content_state.sync_offsets);
        if self.sync_prefs.display_check_due(self.frame_count) {
            let display = self.output_display_id();
            if let Some(cmd) = self.sync_prefs.display_changed(display) {
                self.send_content_cmd(cmd);
            }
        }
    }

    fn output_display_id(&self) -> Option<u32> {
        let window = self
            .window_registry
            .iter()
            .find(|(_, s)| matches!(s.role, crate::window_registry::WindowRole::Output { .. }))
            .map(|(_, s)| &s.window)?;
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::MonitorHandleExtMacOS;
            window.current_monitor().map(|m| m.native_id())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = window;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_values_round_trip_and_bad_ones_load_as_zero() {
        let mut prefs = UserPrefs::in_memory();
        let mut sync = SyncPrefs::default();
        match sync.stored(&prefs) {
            ContentCommand::SetSyncCalibration { extra_delay_ms, offsets_ms } => {
                assert_eq!(extra_delay_ms, 0.0);
                assert!(offsets_ms.iter().all(|o| o.is_nan()), "nothing stored");
            }
            _ => unreachable!(),
        }
        sync.save_learned(&mut prefs, [0.007, 0.0, -0.002]);
        assert_eq!(prefs.get_string(OFFSET_KEYS[0], ""), "7.000");
        prefs.set_string(EXTRA_DELAY_KEY, "300");
        match SyncPrefs::default().stored(&prefs) {
            ContentCommand::SetSyncCalibration { extra_delay_ms, offsets_ms } => {
                assert_eq!(extra_delay_ms, 0.0, "300 ms is out of range");
                assert_eq!(offsets_ms, [7.0, 0.0, -2.0]);
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn display_is_checked_once_a_second_and_only_on_change() {
        let mut sync = SyncPrefs::default();
        assert!(sync.display_check_due(0));
        assert!(!sync.display_check_due(1), "checked within the second");
        assert!(sync.display_check_due(60));
        assert!(matches!(sync.display_changed(Some(1)), Some(ContentCommand::SetDisplayDelay(_))));
        assert!(sync.display_changed(Some(1)).is_none(), "same display");
        assert!(sync.display_changed(Some(2)).is_some(), "moved to another display");
    }
}
