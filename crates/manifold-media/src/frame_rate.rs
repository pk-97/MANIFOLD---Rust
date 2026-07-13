//! Fractional frame rate → exact rational timebase.
//!
//! BUG-129: the native Metal encoder used to derive its CMTime timescale by
//! rounding the project's `fps: f32` to the nearest integer
//! (`(int)(fps + 0.5)`), then stamped every frame's presentation time as
//! `frameIndex / roundedFps`. For an NTSC-family rate like 29.97 fps that
//! rounds to a 30 timescale, so the exported video's picture track runs
//! ~0.1% "fast" relative to true 29.97 fps — while the muxed audio track
//! (timed from wall-clock samples) does not share that error. Over a long
//! export the picture and audio drift by roughly 60ms per minute.
//!
//! The fix is to never round the frame rate at all: convert it once, in
//! Rust, to an exact rational `(num, den)` such that `fps == num / den`,
//! and pass both integers across the FFI boundary so the native encoder can
//! stamp frame N at exactly `N * den / num` seconds — a rational CMTime,
//! not a float approximation.

/// Convert an f32 frame rate to an exact rational `(numerator, denominator)`
/// such that `fps ≈ numerator / denominator`, suitable for a CMTime
/// timescale (`denominator` = CMTime timescale units per frame is not it —
/// see the native side: presentation time of frame N becomes
/// `CMTimeMake(N * denominator, numerator)`, i.e. `N * denominator /
/// numerator` seconds).
///
/// Method:
/// 1. **NTSC family** — 23.976, 29.97, 59.94 (and their /1.001 kin) are
///    matched by name against a small epsilon (0.005), since the stored
///    `f32` is never bit-exact to the repeating decimal, and mapped to
///    their broadcast-exact rationals: 24000/1001, 30000/1001, 60000/1001.
///    These are overwhelmingly the fractional rates real projects use, and
///    they have one universally-agreed-on exact rational — approximating
///    them generically could land on a different (still technically valid)
///    fraction that no other tool recognizes.
/// 2. **Integer rates** (24, 25, 30, 50, 60, ...) map to `n/1` exactly.
/// 3. **Anything else** falls through to a continued-fraction best-rational
///    approximation bounded by a maximum denominator (see
///    `best_rational_approximation`), which converges to the exact value
///    for any rate a human would plausibly type and stays well within
///    CMTime's practical timescale range.
pub fn fps_to_rational(fps: f32) -> (i32, i32) {
    const NTSC_EPSILON: f32 = 0.005;
    const NTSC_TABLE: [(f32, i32, i32); 3] = [
        (23.976, 24000, 1001),
        (29.97, 30000, 1001),
        (59.94, 60000, 1001),
    ];

    for &(target, num, den) in &NTSC_TABLE {
        if (fps - target).abs() < NTSC_EPSILON {
            return (num, den);
        }
    }

    let rounded = fps.round();
    if (fps - rounded).abs() < 1e-4 && rounded >= 1.0 {
        return (rounded as i32, 1);
    }

    best_rational_approximation(fps as f64, 100_000)
}

/// Best rational approximation of `x` with denominator `<= max_denominator`,
/// via the standard continued-fraction convergent algorithm: build successive
/// convergents `p/q` from the continued-fraction expansion of `x`, stopping
/// just before a convergent's denominator would exceed `max_denominator`.
/// This is the standard method for "best rational approximation with bounded
/// denominator" (see e.g. Khinchin, *Continued Fractions*) — each convergent
/// is the closest rational to `x` among all fractions with a denominator no
/// larger than its own.
fn best_rational_approximation(x: f64, max_denominator: i64) -> (i32, i32) {
    if !x.is_finite() || x <= 0.0 {
        return (30, 1);
    }

    let mut p0: i64 = 0;
    let mut q0: i64 = 1;
    let mut p1: i64 = 1;
    let mut q1: i64 = 0;
    let mut val = x;

    loop {
        let a = val.floor() as i64;
        let p2 = a * p1 + p0;
        let q2 = a * q1 + q0;
        if q2 > max_denominator || q2 <= 0 {
            break;
        }
        p0 = p1;
        q0 = q1;
        p1 = p2;
        q1 = q2;

        let frac = val - a as f64;
        if frac.abs() < 1e-9 {
            break;
        }
        val = 1.0 / frac;
    }

    if q1 == 0 {
        // x was < 1 and never converged past the initial state; fall back
        // to a direct scaled fraction.
        return ((x * 1000.0).round().max(1.0) as i32, 1000);
    }

    (p1 as i32, q1 as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_rational_close(fps: f32, expected_num: i32, expected_den: i32) {
        let (num, den) = fps_to_rational(fps);
        assert_eq!(
            (num, den),
            (expected_num, expected_den),
            "fps_to_rational({fps}) = ({num}, {den}), expected ({expected_num}, {expected_den})"
        );
    }

    #[test]
    fn ntsc_family_maps_to_exact_broadcast_rationals() {
        assert_rational_close(23.976, 24000, 1001);
        assert_rational_close(29.97, 30000, 1001);
        assert_rational_close(59.94, 60000, 1001);
    }

    #[test]
    fn integer_rates_map_to_n_over_one() {
        assert_rational_close(24.0, 24, 1);
        assert_rational_close(25.0, 25, 1);
        assert_rational_close(30.0, 30, 1);
        assert_rational_close(60.0, 60, 1);
    }

    #[test]
    fn arbitrary_fractional_rate_uses_general_approximation() {
        // 47.95 fps is not in the NTSC family; verify the general continued-
        // fraction path converges to something that reproduces the input to
        // well within float precision, rather than silently rounding to 48.
        let fps = 47.95_f32;
        let (num, den) = fps_to_rational(fps);
        assert_ne!(
            (num, den),
            (48, 1),
            "47.95 must not silently round to the integer rate 48"
        );
        let reconstructed = num as f64 / den as f64;
        assert!(
            (reconstructed - fps as f64).abs() < 1e-4,
            "reconstructed {reconstructed} too far from input {fps}"
        );
    }

    #[test]
    fn ntsc_rationals_reconstruct_within_broadcast_tolerance() {
        for &(target, num, den) in &[(23.976_f32, 24000, 1001), (29.97, 30000, 1001), (59.94, 60000, 1001)]
        {
            let reconstructed = num as f64 / den as f64;
            assert!(
                (reconstructed - target as f64).abs() < 0.001,
                "rational {num}/{den} = {reconstructed} too far from NTSC target {target}"
            );
        }
    }

    #[test]
    fn zero_or_negative_falls_back_safely() {
        let (num, den) = fps_to_rational(0.0);
        assert!(num >= 1 && den >= 1);
    }
}
