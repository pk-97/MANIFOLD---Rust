/// Data for a prewarm candidate. Port of C# PlaybackEngine prewarm system.
#[derive(Debug, Clone)]
pub struct PrewarmCandidate {
    pub video_clip_id: String,
    pub file_path: String,
}

/// Pure video time computation. Port of C# VideoTimeCalculator.Compute().
/// Matches Unity exactly: Max(0, localTime), Max(0, rate), 0.01 threshold,
/// mediaLength - inPoint for available region.
pub fn compute_video_time(
    current_time: f32,
    clip_start_time: f32,
    in_point: f32,
    is_looping: bool,
    loop_duration_seconds: f32,
    media_length: f32,
    playback_rate: f32,
) -> f32 {
    let local_time = (current_time - clip_start_time).max(0.0);
    let safe_rate = playback_rate.max(0.0);
    let source_local_time = local_time * safe_rate;
    let video_time = in_point + source_local_time;

    if is_looping && media_length > 0.01 {
        let loop_len_sec = if loop_duration_seconds > 0.0 {
            (loop_duration_seconds * safe_rate).min(media_length - in_point)
        } else {
            media_length - in_point
        };

        if loop_len_sec > 0.01 {
            let wrapped = source_local_time % loop_len_sec;
            return in_point + wrapped;
        }
    }

    video_time
}

/// Beat-domain overload. Port of C# VideoTimeCalculator.Compute(... loopDurationBeats, currentSpb).
pub fn compute_video_time_beats(
    current_time: f32,
    clip_start_time: f32,
    in_point: f32,
    is_looping: bool,
    loop_duration_beats: f32,
    media_length: f32,
    current_spb: f32,
    playback_rate: f32,
) -> f32 {
    let loop_duration_seconds = if loop_duration_beats > 0.0 {
        loop_duration_beats * current_spb
    } else {
        0.0
    };

    compute_video_time(
        current_time,
        clip_start_time,
        in_point,
        is_looping,
        loop_duration_seconds,
        media_length,
        playback_rate,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_playback() {
        let t = compute_video_time(2.0, 1.0, 0.0, false, 0.0, 10.0, 1.0);
        assert!((t - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_in_point() {
        let t = compute_video_time(2.0, 1.0, 5.0, false, 0.0, 10.0, 1.0);
        assert!((t - 6.0).abs() < 0.001);
    }

    #[test]
    fn test_looping() {
        // 11s local, available = 5.0 - 0.0 = 5.0, 11 % 5 = 1.0
        let t = compute_video_time(12.0, 1.0, 0.0, true, 0.0, 5.0, 1.0);
        assert!((t - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_looping_with_in_point() {
        // 3s local, available = 10.0 - 2.0 = 8.0, 3 % 8 = 3.0, video = 2.0 + 3.0 = 5.0
        let t = compute_video_time(4.0, 1.0, 2.0, true, 0.0, 10.0, 1.0);
        assert!((t - 5.0).abs() < 0.001);
    }

    #[test]
    fn test_beats_overload() {
        // loop_duration_beats=2.0, spb=0.5 → loop_seconds=1.0
        let t = compute_video_time_beats(3.0, 1.0, 0.0, true, 2.0, 10.0, 0.5, 1.0);
        // 2s local, loop=1.0, 2 % 1 = 0.0
        assert!(t.abs() < 0.001);
    }

    #[test]
    fn test_playback_rate() {
        let t = compute_video_time(2.0, 1.0, 0.0, false, 0.0, 10.0, 2.0);
        assert!((t - 2.0).abs() < 0.001);
    }
}
