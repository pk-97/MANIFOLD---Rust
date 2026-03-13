/// Beat quantization constants matching C# BeatQuantizer.
pub struct BeatQuantizer;

impl BeatQuantizer {
    pub const BPM_STEP: f32 = 0.01;
    pub const BEAT_STEP: f32 = 0.0001;
    pub const TIME_SECONDS_STEP: f32 = 0.0001;

    pub fn quantize_bpm(bpm: f32) -> f32 {
        let clamped = bpm.clamp(20.0, 300.0);
        (clamped / Self::BPM_STEP).round() * Self::BPM_STEP
    }

    pub fn quantize_beat(beat: f32) -> f32 {
        (beat / Self::BEAT_STEP).round() * Self::BEAT_STEP
    }

    pub fn quantize_time_seconds(seconds: f32) -> f32 {
        (seconds / Self::TIME_SECONDS_STEP).round() * Self::TIME_SECONDS_STEP
    }
}

/// Utility math functions.
pub struct MathUtils;

impl MathUtils {
    pub fn is_finite(value: f32) -> bool {
        value.is_finite()
    }
}

/// ID generation matching C# IdUtil.ShortId().
pub fn short_id() -> String {
    let uuid = uuid::Uuid::new_v4();
    format!("{:08x}", uuid.as_u128() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quantize_bpm() {
        assert_eq!(BeatQuantizer::quantize_bpm(120.0), 120.0);
        assert!((BeatQuantizer::quantize_bpm(120.005) - 120.01).abs() < 0.001);
        assert_eq!(BeatQuantizer::quantize_bpm(15.0), 20.0);
        assert_eq!(BeatQuantizer::quantize_bpm(350.0), 300.0);
    }

    #[test]
    fn test_short_id_length() {
        let id = short_id();
        assert_eq!(id.len(), 8);
    }
}
