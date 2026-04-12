//! Pure derivation: which tracks in the PLAY group are currently playing.
//!
//! Same architectural pattern as `cue.rs`: the static structure
//! (`GroupTracks`) is fetched once from Ableton at connect time. Each
//! frame the renderer asks "given this `current_beat`, which tracks are
//! playing?" — pure function, fully testable, zero OSC traffic.
//!
//! Honors both track-level and clip-level mute (clip mute requires the
//! `arrangement_clips/muted` AbletonOSC patch — see
//! `assets/abletonosc-patches/README.md`).

use manifold_playback::ableton_bridge::TrackArrangement;

/// Returns `true` if the track is unmuted AND has at least one unmuted
/// clip whose `[start, end)` interval overlaps the half-open beat range
/// `[range_start, range_end)`. Used for "what plays in the next section"
/// — variant (a): straddlers (clips that started before `range_start` and
/// continue into the range) count as active.
pub(crate) fn plays_in_range(
    track: &TrackArrangement,
    range_start: f64,
    range_end: f64,
) -> bool {
    if track.muted || range_end <= range_start {
        return false;
    }
    // Tracks with "ignore" anywhere in the name are hidden from the HUD.
    if track.name.to_ascii_lowercase().contains("ignore") {
        return false;
    }
    track
        .clips
        .iter()
        .any(|c| !c.muted && c.start < range_end && c.end > range_start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_playback::ableton_bridge::ArrangementClip;

    fn clip(start: f64, end: f64) -> ArrangementClip {
        ArrangementClip {
            start,
            end,
            muted: false,
        }
    }

    fn muted_clip(start: f64, end: f64) -> ArrangementClip {
        ArrangementClip {
            start,
            end,
            muted: true,
        }
    }

    fn track_with(muted: bool, clips: Vec<ArrangementClip>) -> TrackArrangement {
        TrackArrangement {
            track_id: 0,
            name: "test".to_string(),
            muted,
            clips,
        }
    }

    #[test]
    fn empty_clip_list_never_plays() {
        let t = track_with(false, vec![]);
        assert!(!plays_in_range(&t, 0.0, 16.0));
    }

    #[test]
    fn muted_track_never_plays() {
        let t = track_with(true, vec![clip(0.0, 16.0)]);
        assert!(!plays_in_range(&t, 0.0, 16.0));
    }

    #[test]
    fn muted_clip_skipped() {
        let t = track_with(false, vec![muted_clip(0.0, 16.0)]);
        assert!(!plays_in_range(&t, 0.0, 16.0));
    }

    #[test]
    fn straddling_clip_counts() {
        // Clip starts before the range but ends inside it: variant (a) → yes.
        let t = track_with(false, vec![clip(0.0, 32.0)]);
        assert!(plays_in_range(&t, 16.0, 48.0));
    }

    #[test]
    fn fully_inside_range() {
        let t = track_with(false, vec![clip(20.0, 28.0)]);
        assert!(plays_in_range(&t, 16.0, 32.0));
    }

    #[test]
    fn touching_boundary_excluded() {
        // Clip ends exactly at range_start → no overlap (half-open).
        let t = track_with(false, vec![clip(0.0, 16.0)]);
        assert!(!plays_in_range(&t, 16.0, 32.0));
        // Clip starts exactly at range_end → no overlap.
        let t = track_with(false, vec![clip(32.0, 48.0)]);
        assert!(!plays_in_range(&t, 16.0, 32.0));
    }

    #[test]
    fn empty_or_inverted_range_is_false() {
        let t = track_with(false, vec![clip(0.0, 100.0)]);
        assert!(!plays_in_range(&t, 16.0, 16.0));
        assert!(!plays_in_range(&t, 32.0, 16.0));
    }

    #[test]
    fn infinite_range_end() {
        // Final-section case: section_end is +∞.
        let t = track_with(false, vec![clip(20.0, 28.0)]);
        assert!(plays_in_range(&t, 16.0, f64::INFINITY));
    }
}
