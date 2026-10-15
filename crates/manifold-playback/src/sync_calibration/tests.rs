use super::*;

/// Seeded noise in ±`amp`.
struct Noise(u64);

impl Noise {
    fn next(&mut self, amp: f64) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) * amp
    }
}

const STEP: f64 = 1.0 / 60.0;

#[test]
fn screen_delay_settles_and_ignores_jitter() {
    let mut cal = SyncCalibration::new();
    assert_eq!(cal.read_delay(), DEFAULT_PRESENT_DELAY);
    let mut noise = Noise(0xC0FFEE);
    let mut settled_at = None;
    let mut values = Vec::new();
    for k in 0..(60 * 30) {
        let now = k as f64 * STEP;
        cal.presented(0.025 + noise.next(0.003), now);
        if k % 7 == 0 {
            // A dropped frame: presentedTime 0, long before the deadline.
            cal.presented(-1234.5, now);
        }
        if settled_at.is_none() && (cal.read_delay() - 0.025).abs() <= 0.001 {
            settled_at = Some(now);
        }
        if settled_at.is_some() && values.last() != Some(&cal.read_delay()) {
            values.push(cal.read_delay());
        }
    }
    let settled_at = settled_at.expect("settles");
    assert!(settled_at <= 3.0, "settled at {settled_at:.2} s");
    assert_eq!(values.len(), 1, "moved on jitter after settling: {values:?}");
}

#[test]
fn extra_delay_accepts_only_its_range() {
    let mut cal = SyncCalibration::new();
    cal.set_extra_delay_ms(10.0);
    assert!((cal.read_delay() - DEFAULT_PRESENT_DELAY - 0.010).abs() < 1e-12);
    for bad in [-1.0, 251.0, f64::NAN, f64::INFINITY] {
        cal.set_extra_delay_ms(bad);
        assert_eq!(cal.extra_delay(), 0.0, "{bad} loads as 0");
    }
}

#[test]
fn offset_is_learned_over_steady_play_and_then_holds() {
    let mut cal = SyncCalibration::new();
    let mut noise = Noise(7);
    let src = CalibratedSource::MidiClock;
    let mut adopted_at = None;
    for k in 0..(60 * 60) {
        let now = k as f64 * STEP;
        // The source runs 7 ms behind Link; once its offset is learned it is
        // read 7 ms later and the difference closes.
        let difference = 0.007 - cal.offset(src) + noise.next(0.0005);
        if cal.observe_offset(src, now, difference, true).is_some() && adopted_at.is_none() {
            adopted_at = Some(now);
        }
    }
    let at = adopted_at.expect("learned");
    assert!((LEARN_WINDOW..LEARN_WINDOW + 1.5).contains(&at), "learned at {at:.2} s");
    assert!((cal.offset(src) - 0.007).abs() < 0.0003, "offset {}", cal.offset(src));
    assert_eq!(cal.offset(CalibratedSource::Timecode), 0.0, "other sources untouched");
}

#[test]
fn unsteady_play_restarts_the_window() {
    let mut cal = SyncCalibration::new();
    let src = CalibratedSource::AbletonOsc;
    for k in 0..(60 * 45) {
        let now = k as f64 * STEP;
        // A catch-up window every 20 s, a gap at 25 s: never 30 s steady.
        let steady = k % (60 * 20) != 0;
        let now = if k >= 60 * 25 { now + 1.0 } else { now };
        assert_eq!(cal.observe_offset(src, now, 0.004, steady), None);
    }
    for k in 0..(60 * 32) {
        // Large or noisy differences are not learned either.
        let d = if k % 2 == 0 { 0.008 } else { 0.0 };
        assert_eq!(cal.observe_offset(src, 100.0 + k as f64 * STEP, d, true), None, "spread too large");
    }
    assert_eq!(cal.offset(src), 0.0);
}

#[test]
fn restored_offsets_are_bounded() {
    let mut cal = SyncCalibration::new();
    cal.restore_offset(CalibratedSource::Timecode, 0.2);
    cal.restore_offset(CalibratedSource::MidiClock, f64::NAN);
    assert_eq!(cal.offsets(), [0.0, 0.0, OFFSET_LIMIT]);
}

#[test]
fn difference_folds_into_one_quantum() {
    // Link counts from another origin, 64 beats apart: 7 ms at 120 BPM.
    let d = difference_seconds(100.0 + 0.014, 36.0, 120.0, 4.0);
    assert!((d - 0.007).abs() < 1e-9, "{d}");
    let d = difference_seconds(100.0, 36.0 + 0.014, 120.0, 4.0);
    assert!((d + 0.007).abs() < 1e-9, "{d}");
    assert!(difference_seconds(1.0, 1.0, 0.0, 4.0).is_nan());
}
