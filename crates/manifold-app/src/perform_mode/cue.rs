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

/// Compute the linear progress (0..=1) of `current_beat` through the
/// section bounded by `current_section` and `next_section`. Returns `None`
/// if either bound is missing or the section has zero length.
pub(crate) fn section_progress(
    current_section: Option<&CuePoint>,
    next_section: Option<&CuePoint>,
    current_beat: f64,
) -> Option<f64> {
    let cur = current_section?;
    let nxt = next_section?;
    let len = nxt.time - cur.time;
    if len <= 0.0 {
        return None;
    }
    let p = (current_beat - cur.time) / len;
    Some(p.clamp(0.0, 1.0))
}

/// Format an absolute beat position as Ableton-style `BAR.BEAT.SIXTEENTH`,
/// 1-indexed (Ableton's transport readout convention).
///
/// In 4/4 with 4 sixteenths per beat:
/// - beat 0.0   → "1.1.1"
/// - beat 4.0   → "2.1.1"  (start of bar 2)
/// - beat 5.5   → "2.2.3"  (bar 2, beat 2, third sixteenth)
/// - beat 167.0 → "42.4.1" (bar 42, beat 4)
///
/// Negative beats clamp to zero. The result is returned as three separate
/// fields so the renderer can lay them out with fixed-column digits and
/// dot separators that don't shift as values tick over.
pub(crate) fn format_bar_beat(current_beat: f64, beats_per_bar: u32) -> BarBeatDisplay {
    let beat = current_beat.max(0.0);
    let bpb = beats_per_bar.max(1) as f64;
    let bar_idx = (beat / bpb).floor() as i64;
    let beat_in_bar = beat - (bar_idx as f64 * bpb);
    let beat_idx_in_bar = beat_in_bar.floor() as i64;
    let frac = beat_in_bar - beat_idx_in_bar as f64;
    let sixteenth_idx = (frac * 4.0).floor() as i64;
    BarBeatDisplay {
        bar: (bar_idx + 1).to_string(),
        beat: (beat_idx_in_bar + 1).to_string(),
        sixteenth: (sixteenth_idx + 1).to_string(),
    }
}

/// 1-indexed bar/beat/sixteenth strings ready for the fixed-column digit
/// renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BarBeatDisplay {
    pub bar: String,
    pub beat: String,
    pub sixteenth: String,
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

    // ── section_progress ──────────────────────────────────────────

    #[test]
    fn section_progress_basic() {
        let a = cue(0.0, "A");
        let b = cue(16.0, "B");
        assert_eq!(section_progress(Some(&a), Some(&b), 0.0), Some(0.0));
        assert_eq!(section_progress(Some(&a), Some(&b), 8.0), Some(0.5));
        assert_eq!(section_progress(Some(&a), Some(&b), 16.0), Some(1.0));
    }

    #[test]
    fn section_progress_clamps() {
        let a = cue(0.0, "A");
        let b = cue(16.0, "B");
        // Before the section starts (shouldn't normally happen but be safe).
        assert_eq!(section_progress(Some(&a), Some(&b), -5.0), Some(0.0));
        // After the section ends.
        assert_eq!(section_progress(Some(&a), Some(&b), 100.0), Some(1.0));
    }

    #[test]
    fn section_progress_missing_bounds() {
        let a = cue(0.0, "A");
        assert_eq!(section_progress(None, Some(&a), 0.0), None);
        assert_eq!(section_progress(Some(&a), None, 0.0), None);
        assert_eq!(section_progress(None, None, 0.0), None);
    }

    #[test]
    fn section_progress_zero_length() {
        let a = cue(8.0, "A");
        let b = cue(8.0, "B");
        assert_eq!(section_progress(Some(&a), Some(&b), 8.0), None);
    }

    // ── format_bar_beat ───────────────────────────────────────────

    fn bb(bar: &str, beat: &str, sixteenth: &str) -> BarBeatDisplay {
        BarBeatDisplay {
            bar: bar.to_string(),
            beat: beat.to_string(),
            sixteenth: sixteenth.to_string(),
        }
    }

    #[test]
    fn bar_beat_zero() {
        assert_eq!(format_bar_beat(0.0, 4), bb("1", "1", "1"));
    }

    #[test]
    fn bar_beat_bar_boundaries() {
        // Start of bar 2 in 4/4
        assert_eq!(format_bar_beat(4.0, 4), bb("2", "1", "1"));
        // Start of bar 42 in 4/4 (beat 164)
        assert_eq!(format_bar_beat(164.0, 4), bb("42", "1", "1"));
    }

    #[test]
    fn bar_beat_beats_within_bar() {
        // Bar 2, beat 2 (1-indexed)
        assert_eq!(format_bar_beat(5.0, 4), bb("2", "2", "1"));
        // Bar 2, beat 4 (1-indexed)
        assert_eq!(format_bar_beat(7.0, 4), bb("2", "4", "1"));
    }

    #[test]
    fn bar_beat_sixteenths() {
        // Beat 0.0 → 1.1.1
        // Beat 0.25 → 1.1.2 (second sixteenth)
        // Beat 0.5 → 1.1.3 (third sixteenth)
        // Beat 0.75 → 1.1.4 (fourth sixteenth)
        assert_eq!(format_bar_beat(0.0, 4), bb("1", "1", "1"));
        assert_eq!(format_bar_beat(0.25, 4), bb("1", "1", "2"));
        assert_eq!(format_bar_beat(0.5, 4), bb("1", "1", "3"));
        assert_eq!(format_bar_beat(0.75, 4), bb("1", "1", "4"));
    }

    #[test]
    fn bar_beat_three_four_time() {
        // 3/4 time: bars are 3 beats long
        assert_eq!(format_bar_beat(0.0, 3), bb("1", "1", "1"));
        assert_eq!(format_bar_beat(3.0, 3), bb("2", "1", "1"));
        assert_eq!(format_bar_beat(6.0, 3), bb("3", "1", "1"));
        assert_eq!(format_bar_beat(7.5, 3), bb("3", "2", "3"));
    }

    #[test]
    fn bar_beat_negative_clamps_to_one_one_one() {
        assert_eq!(format_bar_beat(-5.0, 4), bb("1", "1", "1"));
    }
}
