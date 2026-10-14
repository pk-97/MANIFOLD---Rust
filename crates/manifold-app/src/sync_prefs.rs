//! The UI side of the sync calibration (FRAME_EXACT_TRANSPORT_DESIGN D19):
//! the stored values sent before the content thread's first tick, and the
//! learned offsets kept per rig in `UserPrefs`.

use crate::content_command::ContentCommand;
use crate::user_prefs::UserPrefs;

/// Hidden override, ms, 0–250. No footer field and no menu entry.
pub const EXTRA_DELAY_KEY: &str = "MANIFOLD_ExtraOutputDelayMs";
/// Learned offsets against Link, ms: MIDI clock, AbletonOSC, timecode.
pub const OFFSET_KEYS: [&str; 3] = ["MANIFOLD_SyncOffsetClkMs", "MANIFOLD_SyncOffsetOscMs", "MANIFOLD_SyncOffsetTimecodeMs"];
/// s: offsets closer than this to the saved ones are not re-saved.
const SAVE_EPSILON: f64 = 1e-6;

/// What the UI remembers between frames.
#[derive(Debug, Default)]
pub struct SyncPrefs {
    /// The offsets last written, s.
    saved: [f64; 3],
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
}

impl crate::app::Application {
    /// Once per UI frame, after the content state drain: save learned offsets.
    pub(crate) fn tick_sync_calibration(&mut self) {
        self.sync_prefs.save_learned(&mut self.user_prefs, self.content_state.sync_offsets);
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
}
