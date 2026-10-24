use crate::units::{Beats, Seconds};

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

    pub fn quantize_beat(beat: Beats) -> Beats {
        let step = Self::BEAT_STEP as f64;
        Beats((beat.0 / step).round() * step)
    }

    pub fn quantize_time_seconds(seconds: Seconds) -> Seconds {
        // Keep negative sentinel values (e.g. -1 for unknown) as-is.
        // Unity BeatQuantizer.cs lines 30-36.
        if seconds.0 < 0.0 {
            return seconds;
        }
        let step = Self::TIME_SECONDS_STEP as f64;
        Seconds((seconds.0 / step).round() * step)
    }
}

/// Utility math functions.
pub struct MathUtils;

impl MathUtils {
    pub fn is_finite(value: f32) -> bool {
        value.is_finite()
    }
}

/// A fresh entity id: 64 random bits as 16 lowercase hex chars.
///
/// 64 bits keeps a duplicate out of reach at any project size (a million
/// clips is about a 3-in-100-million chance), so minting needs no lookup
/// against the live project. Ids are opaque strings everywhere: files written
/// before this carry 8-char ids and load and save back unchanged, and an older
/// build reads these 16-char ids the same way.
pub fn short_id() -> String {
    format!("{:016x}", random_u64(uuid::Uuid::new_v4()))
}

/// 64 uniform bits from a v4 UUID. Each half has fixed bits (the version
/// nibble in the high half, the two variant bits in the low half) at
/// positions where the other half is random, so their XOR is uniform.
fn random_u64(uuid: uuid::Uuid) -> u64 {
    let (hi, lo) = uuid.as_u64_pair();
    hi ^ lo
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_id_is_16_hex_chars() {
        for _ in 0..1000 {
            let id = short_id();
            assert_eq!(id.len(), 16, "{id}");
            assert!(
                id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')),
                "{id}"
            );
        }
    }

    /// Every one of the 64 bits must vary, including the positions a v4
    /// UUID pins in each half.
    #[test]
    fn short_id_uses_all_64_bits() {
        let (mut ones, mut zeros) = (0u64, 0u64);
        for _ in 0..4096 {
            let v = u64::from_str_radix(&short_id(), 16).unwrap();
            ones |= v;
            zeros |= !v;
        }
        assert_eq!(ones, u64::MAX, "a bit never set");
        assert_eq!(zeros, u64::MAX, "a bit never cleared");
    }

    /// A million ids from seeded UUIDs collide nowhere. Seeded so the run is
    /// reproducible; the bits go through the same `random_u64` as production.
    #[test]
    fn million_ids_have_no_duplicate() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            // splitmix64
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        let mut seen = std::collections::HashSet::with_capacity(1 << 20);
        for _ in 0..1_000_000 {
            let mut bytes = [0u8; 16];
            bytes[..8].copy_from_slice(&next().to_le_bytes());
            bytes[8..].copy_from_slice(&next().to_le_bytes());
            let uuid = uuid::Builder::from_random_bytes(bytes).into_uuid();
            assert!(seen.insert(random_u64(uuid)), "duplicate id");
        }
    }

    #[test]
    fn test_quantize_bpm() {
        assert_eq!(BeatQuantizer::quantize_bpm(120.0), 120.0);
        assert!((BeatQuantizer::quantize_bpm(120.005) - 120.01).abs() < 0.001);
        assert_eq!(BeatQuantizer::quantize_bpm(15.0), 20.0);
        assert_eq!(BeatQuantizer::quantize_bpm(350.0), 300.0);
    }
}
