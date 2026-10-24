//! The operating log for external sync (FRAME_EXACT_TRANSPORT_DESIGN
//! section 5 (Proof plan), rig log): one `[Transport]` line per seed, seek,
//! catch-up window and handover, and a lock line every 5 s while an external
//! source drives. Guard latches and adopted offsets log where they happen
//! (`external_sync.rs`, `sync_calibration.rs`) under the same prefix.
//!
//! Lines are formatted only when written: on an event, or once per 5 s.

use crate::external_follow::{ExternalFollower, FollowEvent};
use crate::sync_calibration::SyncCalibration;

/// s between lock lines.
pub const LOCK_LINE_INTERVAL: f64 = 5.0;

/// One tick's view of the follower, after the engine's tick.
pub struct SyncTick<'a> {
    /// s on the content thread's monotonic origin.
    pub now: f64,
    /// The driving source's log name ("Link", "CLK", "OSC", "timecode"), or
    /// `None` while the beat follows the map.
    pub source: Option<&'static str>,
    pub playing: bool,
    pub follower: &'a ExternalFollower,
    pub event: FollowEvent,
    pub calibration: &'a SyncCalibration,
    /// The driving source's learned offset against Link, s; `None` for Link.
    pub offset: Option<f64>,
}

#[derive(Debug, Default)]
pub struct SyncLog {
    source: Option<&'static str>,
    catch_up_open: bool,
    next_lock_line: f64,
}

impl SyncLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Once per tick: writes this tick's lines to the info log.
    pub fn observe(&mut self, tick: &SyncTick) {
        self.observe_with(tick, |line| log::info!("{line}"));
    }

    pub fn observe_with(&mut self, tick: &SyncTick, mut write: impl FnMut(&str)) {
        if tick.source != self.source {
            write(&format!(
                "[Transport] handover {} -> {}",
                self.source.unwrap_or("map"),
                tick.source.unwrap_or("map")
            ));
            self.source = tick.source;
            self.catch_up_open = false;
            self.next_lock_line = tick.now + LOCK_LINE_INTERVAL;
        }
        let Some(source) = tick.source else { return };
        let f = tick.follower;
        match tick.event {
            FollowEvent::Seeded => write(&format!(
                "[Transport] {source} seeded at {:.2} BPM",
                f.base_bpm().0
            )),
            FollowEvent::Seek { error, .. } => write(&format!(
                "[Transport] {source} seek {:+.0} ms",
                error * 1000.0
            )),
            FollowEvent::None => {}
        }
        if f.catch_up_open() != self.catch_up_open {
            self.catch_up_open = f.catch_up_open();
            let state = if self.catch_up_open {
                "opened"
            } else {
                "closed"
            };
            write(&format!(
                "[Transport] {source} catch-up window {state}, trim {:+.2}%",
                f.trim() * 100.0
            ));
        }
        if !tick.playing {
            self.next_lock_line = tick.now + LOCK_LINE_INTERVAL;
            return;
        }
        if tick.now >= self.next_lock_line {
            self.next_lock_line = tick.now + LOCK_LINE_INTERVAL;
            write(&lock_line(source, tick));
        }
    }
}

fn lock_line(source: &str, tick: &SyncTick) -> String {
    let (f, c) = (tick.follower, tick.calibration);
    let ms = |s: f64| s * 1000.0;
    let mut line = format!(
        "[Transport] {source} lock: phase {:+.1} ms, trim {:+.2}%, base {:.2} BPM, L {:.1} ms (present {:.1}, extra {:.1})",
        ms(f.filtered_error()),
        f.trim() * 100.0,
        f.base_bpm().0,
        ms(c.read_delay()),
        ms(c.present_delay()),
        ms(c.extra_delay()),
    );
    if let Some(offset) = tick.offset {
        line.push_str(&format!(", {source} offset {:+.1} ms", ms(offset)));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_follow::FollowProfile;
    use manifold_core::{Beats, Bpm, Seconds};

    fn lines(log: &mut SyncLog, tick: &SyncTick) -> Vec<String> {
        let mut out = Vec::new();
        log.observe_with(tick, |l| out.push(l.to_string()));
        out
    }

    #[test]
    fn events_and_a_lock_line_every_five_seconds() {
        let follower = ExternalFollower::starting_at(
            FollowProfile::FAST,
            Seconds(0.0),
            Beats(0.0),
            Bpm(128.0),
        );
        let calibration = SyncCalibration::new();
        let mut log = SyncLog::new();
        let tick = |now: f64, source: Option<&'static str>, event: FollowEvent| SyncTick {
            now,
            source,
            playing: true,
            follower: &follower,
            event,
            calibration: &calibration,
            offset: Some(0.0061),
        };
        assert_eq!(
            lines(&mut log, &tick(0.0, Some("CLK"), FollowEvent::Seeded)),
            [
                "[Transport] handover map -> CLK",
                "[Transport] CLK seeded at 128.00 BPM"
            ]
        );
        assert!(
            lines(&mut log, &tick(4.9, Some("CLK"), FollowEvent::None)).is_empty(),
            "no lock line before 5 s"
        );
        let lock = lines(&mut log, &tick(5.0, Some("CLK"), FollowEvent::None));
        assert_eq!(
            lock,
            [
                "[Transport] CLK lock: phase +0.0 ms, trim +0.00%, base 128.00 BPM, L 17.0 ms (present 17.0, extra 0.0), CLK offset +6.1 ms"
            ]
        );
        let seek = FollowEvent::Seek {
            beat: Beats(12.0),
            error: -0.312,
        };
        assert_eq!(
            lines(&mut log, &tick(6.0, Some("CLK"), seek)),
            ["[Transport] CLK seek -312 ms"]
        );
        assert_eq!(
            lines(&mut log, &tick(7.0, None, FollowEvent::None)),
            ["[Transport] handover CLK -> map"]
        );
        assert!(
            lines(&mut log, &tick(20.0, None, FollowEvent::None)).is_empty(),
            "the map logs nothing"
        );
    }
}
