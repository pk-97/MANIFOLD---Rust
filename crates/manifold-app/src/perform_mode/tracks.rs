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
/// clip whose `[start, end)` interval contains `current_beat`.
pub(crate) fn is_playing(track: &TrackArrangement, current_beat: f64) -> bool {
    if track.muted {
        return false;
    }
    track
        .clips
        .iter()
        .any(|c| !c.muted && c.start <= current_beat && current_beat < c.end)
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
        assert!(!is_playing(&t, 0.0));
        assert!(!is_playing(&t, 100.0));
    }

    #[test]
    fn muted_track_never_plays_even_with_active_clip() {
        let t = track_with(true, vec![clip(0.0, 16.0)]);
        assert!(!is_playing(&t, 8.0));
    }

    #[test]
    fn muted_clip_never_plays() {
        let t = track_with(false, vec![muted_clip(0.0, 16.0)]);
        assert!(!is_playing(&t, 8.0));
    }

    #[test]
    fn muted_clip_does_not_block_unmuted_overlapping_clip() {
        // Two clips at the same range, one muted, one not. Track plays.
        let t = track_with(
            false,
            vec![muted_clip(0.0, 16.0), clip(0.0, 16.0)],
        );
        assert!(is_playing(&t, 8.0));
    }

    #[test]
    fn inside_clip() {
        let t = track_with(false, vec![clip(0.0, 16.0)]);
        assert!(is_playing(&t, 0.0));
        assert!(is_playing(&t, 8.0));
        assert!(is_playing(&t, 15.999));
    }

    #[test]
    fn at_clip_boundaries() {
        let t = track_with(false, vec![clip(8.0, 16.0)]);
        // Start is inclusive
        assert!(is_playing(&t, 8.0));
        // Just before start
        assert!(!is_playing(&t, 7.999));
        // End is exclusive
        assert!(!is_playing(&t, 16.0));
        // Just before end
        assert!(is_playing(&t, 15.999));
    }

    #[test]
    fn between_clips() {
        let t = track_with(false, vec![clip(0.0, 8.0), clip(16.0, 24.0)]);
        assert!(is_playing(&t, 4.0));
        assert!(!is_playing(&t, 12.0));
        assert!(is_playing(&t, 20.0));
        assert!(!is_playing(&t, 30.0));
    }

    #[test]
    fn looped_clip_full_arrangement_footprint() {
        // Crucial test: a 1-bar MIDI loop dragged out to fill 16 bars in
        // arrangement view. With the AbletonOSC patch, we get the full
        // visible end_time (64 beats), not just the loop length (4 beats).
        let t = track_with(false, vec![clip(0.0, 64.0)]);
        // Should be playing throughout the entire visible footprint.
        assert!(is_playing(&t, 0.0));
        assert!(is_playing(&t, 4.0));   // past the original loop length
        assert!(is_playing(&t, 32.0));
        assert!(is_playing(&t, 63.999));
        assert!(!is_playing(&t, 64.0));
    }

    #[test]
    fn multiple_overlapping_clips() {
        // Two clips overlap. As long as ONE unmuted clip contains the
        // playhead, the track is playing.
        let t = track_with(false, vec![clip(0.0, 16.0), clip(8.0, 24.0)]);
        assert!(is_playing(&t, 4.0));  // only in clip 1
        assert!(is_playing(&t, 12.0)); // in both
        assert!(is_playing(&t, 20.0)); // only in clip 2
        assert!(!is_playing(&t, 30.0));
    }

    #[test]
    fn negative_current_beat_is_safe() {
        let t = track_with(false, vec![clip(0.0, 16.0)]);
        assert!(!is_playing(&t, -1.0));
    }
}
