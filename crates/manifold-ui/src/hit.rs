//! Shared hit-test primitives.
//!
//! Containment is reimplemented all over the UI with subtly different boundary
//! conventions: the chrome hit-tests pixel rects ([`Rect::contains`](crate::node::Rect::contains),
//! half-open), the timeline hit-tests beat intervals and a Y band
//! ([`ClipHitTester`](crate::clip_hit_tester::ClipHitTester)), and box-select
//! tests interval overlap. Same idea, three hand-inlined copies.
//!
//! [`Span`] is that idea once — a 1D interval `[start, end)` with `contains`
//! (half-open), `contains_inclusive` (closed, for bands that include their far
//! edge), and `overlaps`. The chrome's `Rect::contains` is two half-open spans;
//! the timeline's clip hit-test is a beat span plus a closed Y span; box-select
//! is a span overlap. Both surfaces express this type, so the boundary
//! convention lives in one tested place instead of drifting between call sites.

/// A 1D interval `[start, end)`. The building block of axis-aligned hit-testing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Span {
    pub start: f32,
    pub end: f32,
}

impl Span {
    #[inline]
    pub fn new(start: f32, end: f32) -> Self {
        Self { start, end }
    }

    /// Half-open containment: `start <= v < end`. The default convention — it
    /// tiles without double-counting shared edges (a point on a boundary
    /// belongs to exactly one of two abutting spans).
    #[inline]
    pub fn contains(self, v: f32) -> bool {
        v >= self.start && v < self.end
    }

    /// Closed containment: `start <= v <= end`. For bands whose far edge is
    /// inclusive (e.g. the timeline's clip-area Y band).
    #[inline]
    pub fn contains_inclusive(self, v: f32) -> bool {
        v >= self.start && v <= self.end
    }

    /// Do two intervals overlap? `self.end > other.start && self.start <
    /// other.end` — touching-but-not-crossing (`self.end == other.start`) does
    /// not count, matching the half-open convention.
    #[inline]
    pub fn overlaps(self, other: Span) -> bool {
        self.end > other.start && self.start < other.end
    }

    /// Interval length, floored at zero (an inverted span has no extent).
    #[inline]
    pub fn len(self) -> f32 {
        (self.end - self.start).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_half_open() {
        let s = Span::new(1.0, 3.0);
        assert!(!s.contains(0.9));
        assert!(s.contains(1.0)); // start included
        assert!(s.contains(2.99));
        assert!(!s.contains(3.0)); // end excluded
    }

    #[test]
    fn inclusive_includes_far_edge() {
        let s = Span::new(1.0, 3.0);
        assert!(s.contains_inclusive(3.0));
        assert!(!s.contains_inclusive(3.01));
    }

    #[test]
    fn overlap_rules() {
        let a = Span::new(0.0, 2.0);
        assert!(a.overlaps(Span::new(1.0, 4.0))); // crossing
        assert!(a.overlaps(Span::new(-1.0, 0.5))); // crossing from left
        assert!(!a.overlaps(Span::new(2.0, 4.0))); // touching at edge — no
        assert!(!a.overlaps(Span::new(3.0, 4.0))); // disjoint
    }

    #[test]
    fn len_floors_at_zero() {
        assert_eq!(Span::new(1.0, 4.0).len(), 3.0);
        assert_eq!(Span::new(4.0, 1.0).len(), 0.0);
    }
}
