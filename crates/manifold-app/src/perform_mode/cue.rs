//! Pure cue point derivation for the perform-mode HUD.
//!
//! These functions assume `current_beat` is the **absolute** Ableton song
//! position in beats. MANIFOLD's MIDI clock handler parses Song Position
//! Pointer, so `content_state.current_beat` already satisfies this when
//! slaved to Ableton MIDI clock.

use manifold_playback::ableton_bridge::CuePoint;

/// Result of cue analysis for the current playhead position.
#[derive(Debug, Clone)]
pub(crate) struct CueAnalysis<'a> {
    /// The cue point we are currently inside (latest cue with `time <= now`).
    pub current: Option<&'a CuePoint>,
    /// The next cue point ahead of the playhead.
    pub next: Option<&'a CuePoint>,
    /// Beats remaining until `next`. `None` if `next` is `None`.
    pub beats_to_next: Option<f64>,
}

/// Analyze cue points relative to the current beat position.
///
/// `cues` MUST be sorted by `time` ascending (the bridge sorts on receipt).
pub(crate) fn analyze<'a>(cues: &'a [CuePoint], current_beat: f64) -> CueAnalysis<'a> {
    // current = last cue with time <= current_beat
    let current = cues
        .iter()
        .rev()
        .find(|c| c.time <= current_beat);
    // next = first cue with time > current_beat
    let next = cues.iter().find(|c| c.time > current_beat);
    let beats_to_next = next.map(|c| c.time - current_beat);
    CueAnalysis {
        current,
        next,
        beats_to_next,
    }
}

/// A countdown display split into a number and a unit so the renderer can
/// anchor each part to a fixed position (stable visual layout).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CountdownDisplay {
    pub number: String,
    pub unit: String,
}

/// Format a beats-remaining countdown as a smoothly-updating decimal in
/// bars. Always reports in bars so the readout never switches units —
/// sub-bar values render as "0.5 BARS", "0.2 BARS", etc.
///
/// Returns a `(number, unit)` split so the renderer can right-align the
/// number and left-align the unit against a fixed center axis. Combined
/// with fixed-column digit rendering in the renderer, the layout stays
/// pixel-stable while updating every frame.
///
/// Examples (4 beats/bar):
/// - 240.0 beats → ("60.0", "BARS")
/// - 16.0  beats → ("4.0",  "BARS")
/// - 4.0   beats → ("1.0",  "BARS")
/// - 3.5   beats → ("0.9",  "BARS")
/// - 0.5   beats → ("0.1",  "BARS")
pub(crate) fn format_countdown(beats: f64, beats_per_bar: u32) -> CountdownDisplay {
    let beats = beats.max(0.0);
    let bpb = beats_per_bar.max(1) as f64;
    let bars = beats / bpb;
    CountdownDisplay {
        number: format!("{bars:.1}"),
        unit: "BARS".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(time: f64, name: &str) -> CuePoint {
        CuePoint {
            time,
            name: name.to_string(),
        }
    }

    #[test]
    fn analyze_empty_list() {
        let r = analyze(&[], 12.0);
        assert!(r.current.is_none());
        assert!(r.next.is_none());
        assert!(r.beats_to_next.is_none());
    }

    #[test]
    fn analyze_before_first_cue() {
        let cues = vec![cue(8.0, "Verse"), cue(32.0, "Drop")];
        let r = analyze(&cues, 0.0);
        assert!(r.current.is_none());
        assert_eq!(r.next.map(|c| c.name.as_str()), Some("Verse"));
        assert_eq!(r.beats_to_next, Some(8.0));
    }

    #[test]
    fn analyze_inside_section() {
        let cues = vec![cue(0.0, "Intro"), cue(16.0, "Verse"), cue(48.0, "Drop")];
        let r = analyze(&cues, 24.0);
        assert_eq!(r.current.map(|c| c.name.as_str()), Some("Verse"));
        assert_eq!(r.next.map(|c| c.name.as_str()), Some("Drop"));
        assert_eq!(r.beats_to_next, Some(24.0));
    }

    #[test]
    fn analyze_at_cue_boundary() {
        let cues = vec![cue(0.0, "A"), cue(16.0, "B")];
        let r = analyze(&cues, 16.0);
        assert_eq!(r.current.map(|c| c.name.as_str()), Some("B"));
        assert!(r.next.is_none());
    }

    #[test]
    fn analyze_after_last_cue() {
        let cues = vec![cue(0.0, "A"), cue(16.0, "B")];
        let r = analyze(&cues, 100.0);
        assert_eq!(r.current.map(|c| c.name.as_str()), Some("B"));
        assert!(r.next.is_none());
        assert!(r.beats_to_next.is_none());
    }

    fn cd(number: &str, unit: &str) -> CountdownDisplay {
        CountdownDisplay {
            number: number.to_string(),
            unit: unit.to_string(),
        }
    }

    #[test]
    fn format_whole_bars() {
        assert_eq!(format_countdown(16.0, 4), cd("4.0", "BARS"));
        assert_eq!(format_countdown(4.0, 4), cd("1.0", "BARS"));
        assert_eq!(format_countdown(8.0, 4), cd("2.0", "BARS"));
        assert_eq!(format_countdown(240.0, 4), cd("60.0", "BARS"));
    }

    #[test]
    fn format_partial_bars_decimal() {
        // 6.0 / 4 = 1.5
        assert_eq!(format_countdown(6.0, 4), cd("1.5", "BARS"));
        // 10.0 / 4 = 2.5
        assert_eq!(format_countdown(10.0, 4), cd("2.5", "BARS"));
        // Just under a bar boundary
        assert_eq!(format_countdown(15.6, 4), cd("3.9", "BARS"));
    }

    #[test]
    fn format_sub_bar_still_uses_bars() {
        // 3 beats / 4 bpb = 0.75 → "0.8" (one decimal, banker's rounding)
        assert_eq!(format_countdown(3.0, 4), cd("0.8", "BARS"));
        // 1 beat / 4 bpb = 0.25 → "0.2"
        assert_eq!(format_countdown(1.0, 4), cd("0.2", "BARS"));
        // 0.5 beat / 4 bpb = 0.125 → "0.1"
        assert_eq!(format_countdown(0.5, 4), cd("0.1", "BARS"));
    }

    #[test]
    fn format_negative_clamps_to_zero() {
        assert_eq!(format_countdown(-5.0, 4), cd("0.0", "BARS"));
    }

    #[test]
    fn format_three_four_time() {
        assert_eq!(format_countdown(6.0, 3), cd("2.0", "BARS"));
        // 2 beats / 3 bpb = 0.666… → "0.7"
        assert_eq!(format_countdown(2.0, 3), cd("0.7", "BARS"));
    }
}
