//! General 2D affine transform — the capability behind scale-about-pivot and
//! rotation on the bitmap UI's draw path (`docs/UI_TRANSFORM_STACK_DESIGN.md`).
//!
//! Do NOT confuse with [`crate::transform`]'s `Axis` — that's a 1D pan/zoom
//! coordinate map (timeline beats↔px, graph canvas graph-space↔screen), used
//! nowhere near this. `Affine2` is the general 2×3 matrix a rotated/scaled
//! rounded rect or glyph quad needs.
//!
//! Convention matches CoreGraphics/SVG/Cairo: `(a, b, c, d, tx, ty)` maps
//! `x' = a*x + c*y + tx`, `y' = b*x + d*y + ty`. Composition (`mul`) is
//! matrix multiplication: `p.apply(other.apply(pt))` for one point equals
//! `self.mul(&other).apply(pt)` — i.e. `self.mul(&other)` means "apply
//! `other` first, then `self`".

/// A 2D affine transform: rotation/scale/skew (`a, b, c, d`) + translation
/// (`tx, ty`). Six `f32`s — small enough to capture per draw command, the way
/// depth and clip already are.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Affine2 {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Affine2 {
    /// The identity transform — `apply` is a no-op.
    pub const IDENTITY: Affine2 = Affine2 { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 };

    pub fn identity() -> Self {
        Self::IDENTITY
    }

    /// Pure translation by `(tx, ty)`.
    pub fn translate(tx: f32, ty: f32) -> Self {
        Self { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx, ty }
    }

    /// Pure scale about the origin — `(sx, sy)` independent axis factors.
    /// Non-uniform (`sx != sy`) warps corner-radius AA; see the design doc's
    /// "honest boundaries" section.
    pub fn scale(sx: f32, sy: f32) -> Self {
        Self { a: sx, b: 0.0, c: 0.0, d: sy, tx: 0.0, ty: 0.0 }
    }

    /// Pure rotation about the origin, `radians` clockwise (screen Y grows
    /// downward, so a positive angle turns the +X axis toward +Y visually).
    pub fn rotate(radians: f32) -> Self {
        let (sin_r, cos_r) = radians.sin_cos();
        Self { a: cos_r, b: sin_r, c: -sin_r, d: cos_r, tx: 0.0, ty: 0.0 }
    }

    /// Rotation by `radians` about `pivot` — the pivot point is left fixed.
    /// `= translate(pivot) ∘ rotate(radians) ∘ translate(-pivot)`.
    pub fn rotate_about(pivot: (f32, f32), radians: f32) -> Self {
        Affine2::translate(pivot.0, pivot.1)
            .mul(&Affine2::rotate(radians))
            .mul(&Affine2::translate(-pivot.0, -pivot.1))
    }

    /// Scale by `(sx, sy)` about `pivot` — the pivot point is left fixed.
    /// `= translate(pivot) ∘ scale(sx, sy) ∘ translate(-pivot)`.
    pub fn scale_about(pivot: (f32, f32), sx: f32, sy: f32) -> Self {
        Affine2::translate(pivot.0, pivot.1)
            .mul(&Affine2::scale(sx, sy))
            .mul(&Affine2::translate(-pivot.0, -pivot.1))
    }

    /// Compose `self ∘ other`: apply `other` first, then `self`. Matches the
    /// module doc's convention — `self.mul(&other).apply(p) == self.apply(other.apply(p))`.
    pub fn mul(&self, other: &Affine2) -> Self {
        Self {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            tx: self.a * other.tx + self.c * other.ty + self.tx,
            ty: self.b * other.tx + self.d * other.ty + self.ty,
        }
    }

    /// Apply the transform to a point.
    pub fn apply(&self, p: (f32, f32)) -> (f32, f32) {
        (self.a * p.0 + self.c * p.1 + self.tx, self.b * p.0 + self.d * p.1 + self.ty)
    }
}

impl Default for Affine2 {
    fn default() -> Self {
        Self::IDENTITY
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const EPS: f32 = 1e-4;

    fn approx_eq(a: (f32, f32), b: (f32, f32)) {
        assert!(
            (a.0 - b.0).abs() < EPS && (a.1 - b.1).abs() < EPS,
            "expected {b:?}, got {a:?}"
        );
    }

    #[test]
    fn identity_is_noop() {
        let p = (3.0, -4.5);
        approx_eq(Affine2::identity().apply(p), p);
    }

    #[test]
    fn translate_moves_point() {
        let t = Affine2::translate(10.0, -5.0);
        approx_eq(t.apply((1.0, 2.0)), (11.0, -3.0));
    }

    #[test]
    fn scale_scales_point_about_origin() {
        let s = Affine2::scale(2.0, 3.0);
        approx_eq(s.apply((4.0, 5.0)), (8.0, 15.0));
    }

    #[test]
    fn rotate_90_maps_x_axis_to_y_axis() {
        let r = Affine2::rotate(PI / 2.0);
        approx_eq(r.apply((1.0, 0.0)), (0.0, 1.0));
        approx_eq(r.apply((0.0, 1.0)), (-1.0, 0.0));
    }

    #[test]
    fn rotate_about_pivot_fixes_the_pivot() {
        let pivot = (10.0, 20.0);
        let r = Affine2::rotate_about(pivot, PI / 3.0);
        approx_eq(r.apply(pivot), pivot);
    }

    #[test]
    fn rotate_about_pivot_matches_manual_compose() {
        let pivot = (5.0, 5.0);
        let theta = 0.7_f32;
        let via_helper = Affine2::rotate_about(pivot, theta);
        let manual = Affine2::translate(pivot.0, pivot.1)
            .mul(&Affine2::rotate(theta))
            .mul(&Affine2::translate(-pivot.0, -pivot.1));
        let p = (12.0, -3.0);
        approx_eq(via_helper.apply(p), manual.apply(p));
    }

    #[test]
    fn scale_about_pivot_fixes_the_pivot() {
        let pivot = (100.0, 40.0);
        let s = Affine2::scale_about(pivot, 2.0, 0.5);
        approx_eq(s.apply(pivot), pivot);
    }

    #[test]
    fn scale_about_center_scales_corner_correctly() {
        // A 100x100 rect at origin, center (50,50), scaled 2x about its center:
        // the far corner (100,100) should land at (150,150).
        let pivot = (50.0, 50.0);
        let s = Affine2::scale_about(pivot, 2.0, 2.0);
        approx_eq(s.apply((100.0, 100.0)), (150.0, 150.0));
        approx_eq(s.apply((0.0, 0.0)), (-50.0, -50.0));
    }

    #[test]
    fn mul_composes_other_first_then_self() {
        // translate then rotate: point (1,0) translated by (1,0) -> (2,0),
        // then rotated 90deg -> (0,2).
        let translate = Affine2::translate(1.0, 0.0);
        let rotate = Affine2::rotate(PI / 2.0);
        let composed = rotate.mul(&translate);
        approx_eq(composed.apply((1.0, 0.0)), (0.0, 2.0));
    }

    #[test]
    fn mul_associative() {
        let a = Affine2::translate(3.0, -2.0);
        let b = Affine2::rotate(0.4);
        let c = Affine2::scale(1.5, 0.8);
        let p = (7.0, 11.0);
        let left = a.mul(&b).mul(&c).apply(p);
        let right = a.mul(&b.mul(&c)).apply(p);
        approx_eq(left, right);
    }

    #[test]
    fn identity_is_mul_identity_element() {
        let t = Affine2::rotate_about((3.0, 4.0), 1.1);
        let p = (9.0, -2.0);
        approx_eq(t.mul(&Affine2::identity()).apply(p), t.apply(p));
        approx_eq(Affine2::identity().mul(&t).apply(p), t.apply(p));
    }
}
