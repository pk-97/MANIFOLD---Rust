// Pure functions for clip trim calculations.
//
// These match the Unity InteractionOverlay.cs trim behavior:
// - Video clips cannot extend left past original start
// - Generator clips can extend freely in either direction
// - Minimum clip duration is 0.25 beats (1/16 note)
// - Right trim on non-looping video clips clamps to source length

/// Minimum clip duration in beats (1/16 note).
pub const MIN_CLIP_DURATION_BEATS: f32 = 0.25;

/// Result of a trim computation.
#[derive(Debug, Clone, PartialEq)]
pub struct TrimResult {
    pub new_start_beat: f32,
    pub new_duration_beats: f32,
    pub new_in_point: f32,
}

/// Compute the result of trimming a clip's left edge.
///
/// # Arguments
/// - `mouse_beat`: the beat position the user has dragged to (already snapped)
/// - `original_start`: the clip's original start beat before this trim began
/// - `original_duration`: the clip's original duration before this trim began
/// - `original_in_point`: the clip's original InPoint (seconds) before this trim began
/// - `spb`: seconds per beat (for InPoint adjustment)
/// - `is_generator`: true if generator clip (can extend left freely)
/// - `min_duration`: minimum duration in beats (0.25)
///
/// # Behavior
/// - Video clips: left edge cannot go before `original_start` (would mean negative InPoint)
/// - Generator clips: left edge can extend freely to the left
/// - Duration is always >= `min_duration`
/// - InPoint is advanced proportionally to how much the start moves right
pub fn compute_left_trim(
    mouse_beat: f32,
    original_start: f32,
    original_duration: f32,
    original_in_point: f32,
    spb: f32,
    is_generator: bool,
    min_duration: f32,
) -> TrimResult {
    let original_end = original_start + original_duration;

    // Clamp: video clips can't extend left past original start
    let mut new_start = if is_generator {
        mouse_beat
    } else {
        mouse_beat.max(original_start)
    };

    // Enforce minimum duration
    new_start = new_start.min(original_end - min_duration);

    // Can't go below beat 0
    new_start = new_start.max(0.0);

    let new_duration = original_end - new_start;
    let beat_delta = new_start - original_start;

    // Adjust InPoint proportionally (seconds = beats * spb)
    let new_in_point = (original_in_point + beat_delta * spb).max(0.0);

    TrimResult {
        new_start_beat: new_start,
        new_duration_beats: new_duration,
        new_in_point,
    }
}

/// Compute the result of trimming a clip's right edge.
///
/// # Arguments
/// - `mouse_beat`: the beat position the user has dragged to (already snapped)
/// - `clip_start`: the clip's current start beat
/// - `original_in_point`: the clip's InPoint (seconds)
/// - `spb`: seconds per beat
/// - `is_generator`: true if generator clip (can extend freely)
/// - `is_looping`: true if looping is enabled (can extend past source length)
/// - `video_length_seconds`: source video length in seconds (None for generators)
/// - `min_duration`: minimum duration in beats (0.25)
///
/// # Behavior
/// - Non-looping video clips: right edge clamped to source length minus InPoint
/// - Looping video clips and generators: extend freely
/// - Duration is always >= `min_duration`
pub fn compute_right_trim(
    mouse_beat: f32,
    clip_start: f32,
    original_in_point: f32,
    spb: f32,
    is_generator: bool,
    is_looping: bool,
    video_length_seconds: Option<f32>,
    min_duration: f32,
) -> TrimResult {
    // Enforce minimum duration
    let mut new_end = mouse_beat.max(clip_start + min_duration);

    // Non-looping video clips: clamp to source length
    if !is_generator && !is_looping
        && let Some(video_len) = video_length_seconds
            && spb > 0.0 {
                let max_duration_beats = (video_len - original_in_point).max(0.0) / spb;
                new_end = new_end.min(clip_start + max_duration_beats);
                // Re-enforce minimum after clamping
                new_end = new_end.max(clip_start + min_duration);
            }

    let new_duration = new_end - clip_start;

    TrimResult {
        new_start_beat: clip_start,
        new_duration_beats: new_duration,
        new_in_point: original_in_point,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPB: f32 = 0.5; // 120 BPM

    #[test]
    fn left_trim_video_clamps_to_original_start() {
        // Video clip at beat 4, duration 4. Try to extend left to beat 2.
        let result = compute_left_trim(2.0, 4.0, 4.0, 0.0, SPB, false, MIN_CLIP_DURATION_BEATS);
        assert_eq!(result.new_start_beat, 4.0); // Clamped to original start
        assert_eq!(result.new_duration_beats, 4.0); // Unchanged
        assert_eq!(result.new_in_point, 0.0); // Unchanged
    }

    #[test]
    fn left_trim_video_shortens_from_start() {
        // Video clip at beat 4, duration 4. Trim left to beat 6.
        let result = compute_left_trim(6.0, 4.0, 4.0, 0.0, SPB, false, MIN_CLIP_DURATION_BEATS);
        assert_eq!(result.new_start_beat, 6.0);
        assert_eq!(result.new_duration_beats, 2.0);
        assert_eq!(result.new_in_point, 1.0); // 2 beats * 0.5 spb = 1.0s
    }

    #[test]
    fn left_trim_generator_extends_freely() {
        // Generator clip at beat 4, duration 4. Extend left to beat 2.
        let result = compute_left_trim(2.0, 4.0, 4.0, 0.0, SPB, true, MIN_CLIP_DURATION_BEATS);
        assert_eq!(result.new_start_beat, 2.0); // Extended to beat 2
        assert_eq!(result.new_duration_beats, 6.0); // 8 - 2
    }

    #[test]
    fn left_trim_enforces_minimum_duration() {
        // Video clip at beat 4, duration 1. Try to trim to beat 7.9 (would leave 0.1 beats).
        let result = compute_left_trim(7.9, 4.0, 4.0, 0.0, SPB, false, MIN_CLIP_DURATION_BEATS);
        assert_eq!(result.new_start_beat, 7.75); // 8.0 - 0.25
        assert_eq!(result.new_duration_beats, MIN_CLIP_DURATION_BEATS);
    }

    #[test]
    fn left_trim_clamps_to_beat_zero() {
        // Generator clip at beat 2. Extend left to beat -1.
        let result = compute_left_trim(-1.0, 2.0, 4.0, 0.0, SPB, true, MIN_CLIP_DURATION_BEATS);
        assert_eq!(result.new_start_beat, 0.0); // Clamped to 0
        assert_eq!(result.new_duration_beats, 6.0);
    }

    #[test]
    fn right_trim_minimum_duration() {
        // Clip at beat 4, try to trim right to beat 4.1 (duration 0.1 < 0.25).
        let result = compute_right_trim(
            4.1, 4.0, 0.0, SPB, false, false, Some(10.0), MIN_CLIP_DURATION_BEATS,
        );
        assert_eq!(result.new_duration_beats, MIN_CLIP_DURATION_BEATS);
    }

    #[test]
    fn right_trim_video_clamps_to_source_length() {
        // Video is 5 seconds long, InPoint = 1s, SPB = 0.5.
        // Max duration = (5-1)/0.5 = 8 beats. Clip starts at beat 4.
        // Max end = beat 12. Try to extend to beat 20.
        let result = compute_right_trim(
            20.0, 4.0, 1.0, SPB, false, false, Some(5.0), MIN_CLIP_DURATION_BEATS,
        );
        assert_eq!(result.new_duration_beats, 8.0);
    }

    #[test]
    fn right_trim_looping_video_extends_freely() {
        // Same video, but looping enabled. Can extend past source length.
        let result = compute_right_trim(
            20.0, 4.0, 1.0, SPB, false, true, Some(5.0), MIN_CLIP_DURATION_BEATS,
        );
        assert_eq!(result.new_duration_beats, 16.0); // 20 - 4
    }

    #[test]
    fn right_trim_generator_extends_freely() {
        // Generator clip at beat 4. Extend to beat 100.
        let result = compute_right_trim(
            100.0, 4.0, 0.0, SPB, true, false, None, MIN_CLIP_DURATION_BEATS,
        );
        assert_eq!(result.new_duration_beats, 96.0);
    }
}
