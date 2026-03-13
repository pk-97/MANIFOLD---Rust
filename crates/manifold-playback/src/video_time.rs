/// Pure video time computation. Port of C# VideoTimeCalculator.
pub fn compute_video_time(
    current_time: f32,
    clip_start_time: f32,
    in_point: f32,
    is_looping: bool,
    loop_duration_seconds: f32,
    media_length: f32,
    playback_rate: f32,
) -> f32 {
    let local_time = current_time - clip_start_time;
    let source_local_time = local_time * playback_rate;
    let video_time = in_point + source_local_time;

    if is_looping && media_length > 0.0 {
        let effective_loop = if loop_duration_seconds > 0.0 {
            loop_duration_seconds
        } else {
            media_length
        };
        if effective_loop > 0.0 {
            let wrapped = (video_time - in_point) % effective_loop;
            return in_point + if wrapped < 0.0 { wrapped + effective_loop } else { wrapped };
        }
    }

    video_time
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
        let t = compute_video_time(12.0, 1.0, 0.0, true, 0.0, 5.0, 1.0);
        // 11 seconds local, modulo 5 = 1.0
        assert!((t - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_playback_rate() {
        let t = compute_video_time(2.0, 1.0, 0.0, false, 0.0, 10.0, 2.0);
        assert!((t - 2.0).abs() < 0.001);
    }
}
