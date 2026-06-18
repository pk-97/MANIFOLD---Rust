//! Unit-typed wrappers for beat and time values.
//!
//! `Beats` and `Seconds` prevent the compiler from allowing accidental mixing
//! of beat positions with time positions — both are `f64` raw values, so the
//! bare type gives no safety. Using these newtypes makes wrong-unit calls a
//! compile error instead of a silent precision or correctness bug.
//!
//! Serialization: both are `#[serde(transparent)]`, so they round-trip as
//! plain JSON numbers — no project file format changes required.
//!
//! GPU boundary: GPU uniforms always use `f32`. Convert with `.0 as f32`.
//! Serialized model: `TimelineClip` fields use `Beats`/`Seconds` directly.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, AddAssign, Div, DivAssign, Mul, MulAssign, Neg, Rem, Sub, SubAssign};

// ─── Beats ───────────────────────────────────────────────────────────────────

/// A timeline position or duration measured in beats.
///
/// Beat values power all clip scheduling, modulation, and generative timing.
/// They are independent of tempo — converting to/from `Seconds` requires the
/// tempo map.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Beats(pub f64);

impl Beats {
    pub const ZERO: Beats = Beats(0.0);
    pub const ONE: Beats = Beats(1.0);

    #[inline]
    pub fn from_f32(v: f32) -> Self {
        Beats(v as f64)
    }

    #[inline]
    pub fn as_f32(self) -> f32 {
        self.0 as f32
    }

    #[inline]
    pub fn abs(self) -> Self {
        Beats(self.0.abs())
    }

    #[inline]
    pub fn min(self, other: Beats) -> Self {
        Beats(self.0.min(other.0))
    }

    #[inline]
    pub fn max(self, other: Beats) -> Self {
        Beats(self.0.max(other.0))
    }

    #[inline]
    pub fn clamp(self, lo: Beats, hi: Beats) -> Self {
        Beats(self.0.clamp(lo.0, hi.0))
    }

    #[inline]
    pub fn floor(self) -> Self {
        Beats(self.0.floor())
    }

    #[inline]
    pub fn ceil(self) -> Self {
        Beats(self.0.ceil())
    }

    #[inline]
    pub fn round(self) -> Self {
        Beats(self.0.round())
    }

    /// `Mathf.Repeat(t, len)` — equivalent, not raw modulo.
    #[inline]
    pub fn repeat(self, len: Beats) -> Self {
        Beats(self.0 - (self.0 / len.0).floor() * len.0)
    }

    #[inline]
    pub fn is_finite(self) -> bool {
        self.0.is_finite()
    }

    #[inline]
    pub fn is_nan(self) -> bool {
        self.0.is_nan()
    }
}

impl fmt::Display for Beats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.4} beats", self.0)
    }
}

impl From<f32> for Beats {
    #[inline]
    fn from(v: f32) -> Self {
        Beats(v as f64)
    }
}

impl From<f64> for Beats {
    #[inline]
    fn from(v: f64) -> Self {
        Beats(v)
    }
}

impl From<Beats> for f64 {
    #[inline]
    fn from(b: Beats) -> f64 {
        b.0
    }
}

impl From<Beats> for f32 {
    #[inline]
    fn from(b: Beats) -> f32 {
        b.0 as f32
    }
}

impl Add for Beats {
    type Output = Beats;
    #[inline]
    fn add(self, rhs: Beats) -> Beats {
        Beats(self.0 + rhs.0)
    }
}

impl AddAssign for Beats {
    #[inline]
    fn add_assign(&mut self, rhs: Beats) {
        self.0 += rhs.0;
    }
}

impl Sub for Beats {
    type Output = Beats;
    #[inline]
    fn sub(self, rhs: Beats) -> Beats {
        Beats(self.0 - rhs.0)
    }
}

impl SubAssign for Beats {
    #[inline]
    fn sub_assign(&mut self, rhs: Beats) {
        self.0 -= rhs.0;
    }
}

impl Mul<f64> for Beats {
    type Output = Beats;
    #[inline]
    fn mul(self, rhs: f64) -> Beats {
        Beats(self.0 * rhs)
    }
}

impl Mul<f32> for Beats {
    type Output = Beats;
    #[inline]
    fn mul(self, rhs: f32) -> Beats {
        Beats(self.0 * rhs as f64)
    }
}

impl MulAssign<f64> for Beats {
    #[inline]
    fn mul_assign(&mut self, rhs: f64) {
        self.0 *= rhs;
    }
}

impl Div<f64> for Beats {
    type Output = Beats;
    #[inline]
    fn div(self, rhs: f64) -> Beats {
        Beats(self.0 / rhs)
    }
}

impl Div<f32> for Beats {
    type Output = Beats;
    #[inline]
    fn div(self, rhs: f32) -> Beats {
        Beats(self.0 / rhs as f64)
    }
}

impl Div<Beats> for Beats {
    type Output = f64;
    #[inline]
    fn div(self, rhs: Beats) -> f64 {
        self.0 / rhs.0
    }
}

impl DivAssign<f64> for Beats {
    #[inline]
    fn div_assign(&mut self, rhs: f64) {
        self.0 /= rhs;
    }
}

impl Rem<Beats> for Beats {
    type Output = Beats;
    #[inline]
    fn rem(self, rhs: Beats) -> Beats {
        Beats(self.0 % rhs.0)
    }
}

impl Neg for Beats {
    type Output = Beats;
    #[inline]
    fn neg(self) -> Beats {
        Beats(-self.0)
    }
}

// ─── Seconds ─────────────────────────────────────────────────────────────────

/// A wall-clock or playback duration measured in seconds.
///
/// Used for realtime clocks, delta-time values, video in-points, sync, and
/// any time value that is NOT a beat position.  Converting to/from `Beats`
/// requires the tempo map.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seconds(pub f64);

impl Seconds {
    pub const ZERO: Seconds = Seconds(0.0);
    pub const ONE: Seconds = Seconds(1.0);

    #[inline]
    pub fn from_f32(v: f32) -> Self {
        Seconds(v as f64)
    }

    #[inline]
    pub fn as_f32(self) -> f32 {
        self.0 as f32
    }

    /// True when exactly zero. Used by serde `skip_serializing_if` to keep the
    /// audio-only `source_duration` field out of non-audio clip JSON.
    #[inline]
    pub fn is_zero(&self) -> bool {
        self.0 == 0.0
    }

    #[inline]
    pub fn abs(self) -> Self {
        Seconds(self.0.abs())
    }

    #[inline]
    pub fn min(self, other: Seconds) -> Self {
        Seconds(self.0.min(other.0))
    }

    #[inline]
    pub fn max(self, other: Seconds) -> Self {
        Seconds(self.0.max(other.0))
    }

    #[inline]
    pub fn clamp(self, lo: Seconds, hi: Seconds) -> Self {
        Seconds(self.0.clamp(lo.0, hi.0))
    }

    #[inline]
    pub fn is_finite(self) -> bool {
        self.0.is_finite()
    }

    #[inline]
    pub fn is_nan(self) -> bool {
        self.0.is_nan()
    }
}

impl fmt::Display for Seconds {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.6} s", self.0)
    }
}

impl From<f32> for Seconds {
    #[inline]
    fn from(v: f32) -> Self {
        Seconds(v as f64)
    }
}

impl From<f64> for Seconds {
    #[inline]
    fn from(v: f64) -> Self {
        Seconds(v)
    }
}

impl From<Seconds> for f64 {
    #[inline]
    fn from(s: Seconds) -> f64 {
        s.0
    }
}

impl From<Seconds> for f32 {
    #[inline]
    fn from(s: Seconds) -> f32 {
        s.0 as f32
    }
}

impl Add for Seconds {
    type Output = Seconds;
    #[inline]
    fn add(self, rhs: Seconds) -> Seconds {
        Seconds(self.0 + rhs.0)
    }
}

impl AddAssign for Seconds {
    #[inline]
    fn add_assign(&mut self, rhs: Seconds) {
        self.0 += rhs.0;
    }
}

impl Sub for Seconds {
    type Output = Seconds;
    #[inline]
    fn sub(self, rhs: Seconds) -> Seconds {
        Seconds(self.0 - rhs.0)
    }
}

impl SubAssign for Seconds {
    #[inline]
    fn sub_assign(&mut self, rhs: Seconds) {
        self.0 -= rhs.0;
    }
}

impl Mul<f64> for Seconds {
    type Output = Seconds;
    #[inline]
    fn mul(self, rhs: f64) -> Seconds {
        Seconds(self.0 * rhs)
    }
}

impl Mul<f32> for Seconds {
    type Output = Seconds;
    #[inline]
    fn mul(self, rhs: f32) -> Seconds {
        Seconds(self.0 * rhs as f64)
    }
}

impl MulAssign<f64> for Seconds {
    #[inline]
    fn mul_assign(&mut self, rhs: f64) {
        self.0 *= rhs;
    }
}

impl Div<f64> for Seconds {
    type Output = Seconds;
    #[inline]
    fn div(self, rhs: f64) -> Seconds {
        Seconds(self.0 / rhs)
    }
}

impl Div<f32> for Seconds {
    type Output = Seconds;
    #[inline]
    fn div(self, rhs: f32) -> Seconds {
        Seconds(self.0 / rhs as f64)
    }
}

impl Div<Seconds> for Seconds {
    type Output = f64;
    #[inline]
    fn div(self, rhs: Seconds) -> f64 {
        self.0 / rhs.0
    }
}

impl DivAssign<f64> for Seconds {
    #[inline]
    fn div_assign(&mut self, rhs: f64) {
        self.0 /= rhs;
    }
}

impl Neg for Seconds {
    type Output = Seconds;
    #[inline]
    fn neg(self) -> Seconds {
        Seconds(-self.0)
    }
}

// ─── Bpm ─────────────────────────────────────────────────────────────────────

/// A tempo value in beats per minute.
///
/// Distinct from `Beats` (a count/position) and `Seconds` (a duration).
/// Clamped to 20–300 at all entry points to match Unity behaviour.
/// `f32` precision is sufficient — BPM accuracy needs at most 0.01 BPM.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bpm(pub f32);

impl Bpm {
    pub const DEFAULT: Bpm = Bpm(120.0);
    pub const MIN: Bpm = Bpm(20.0);
    pub const MAX: Bpm = Bpm(300.0);

    #[inline]
    pub fn clamped(v: f32) -> Self {
        Bpm(v.clamp(20.0, 300.0))
    }

    /// Beats per second derived from this BPM.
    #[inline]
    pub fn beats_per_second(self) -> f64 {
        self.0 as f64 / 60.0
    }

    /// Seconds per beat derived from this BPM.
    #[inline]
    pub fn seconds_per_beat(self) -> f64 {
        60.0 / self.0 as f64
    }

    #[inline]
    pub fn is_valid(self) -> bool {
        self.0 >= 20.0 && self.0 <= 300.0
    }
}

impl fmt::Display for Bpm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2} BPM", self.0)
    }
}

impl From<f32> for Bpm {
    #[inline]
    fn from(v: f32) -> Self {
        Bpm(v)
    }
}

impl From<Bpm> for f32 {
    #[inline]
    fn from(b: Bpm) -> f32 {
        b.0
    }
}

// ─── Cross-unit helpers ───────────────────────────────────────────────────────

/// `beats_per_second` = BPM / 60.
/// Called at audio/sync boundaries where ratio is already known.
#[inline]
pub fn beats_to_seconds(beats: Beats, beats_per_second: f64) -> Seconds {
    Seconds(beats.0 / beats_per_second)
}

/// `beats_per_second` = BPM / 60.
#[inline]
pub fn seconds_to_beats(seconds: Seconds, beats_per_second: f64) -> Beats {
    Beats(seconds.0 * beats_per_second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beats_arithmetic() {
        let a = Beats(4.0);
        let b = Beats(2.0);
        assert_eq!((a + b).0, 6.0);
        assert_eq!((a - b).0, 2.0);
        assert_eq!((a * 2.0_f64).0, 8.0);
        assert_eq!((a / 2.0_f64).0, 2.0);
        assert_eq!(a / b, 2.0_f64);
    }

    #[test]
    fn seconds_arithmetic() {
        let a = Seconds(4.0);
        let b = Seconds(2.0);
        assert_eq!((a + b).0, 6.0);
        assert_eq!((a - b).0, 2.0);
    }

    #[test]
    fn beats_repeat() {
        let t = Beats(3.5);
        let len = Beats(2.0);
        let r = t.repeat(len);
        assert!((r.0 - 1.5).abs() < 1e-10);
    }

    #[test]
    fn cross_unit_conversions() {
        let b = Beats(120.0);
        let s = beats_to_seconds(b, 2.0); // 2 beats/sec = 120 BPM
        assert!((s.0 - 60.0).abs() < 1e-10);
    }
}
