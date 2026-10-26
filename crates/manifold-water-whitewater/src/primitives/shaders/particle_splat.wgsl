// Shared by the nodes that splat particles onto a lattice (foam cover, spray
// mist): the fixed-point scale of their accumulators and the cubic-spline shape
// of the smoothing kernel.

// Fixed-point scale of the accumulator (2^20). A particle's kernel-weighted
// area adds a few hundredths to a node (a few-millimetre disc on centimetre
// cells), and a node's sum stays far below 4096 (the u32 range at this scale).
const SPLAT_FIXED_SCALE: f32 = 1048576.0;

// The cubic-spline shape (Monaghan 1992) at q = distance / h: 1 - 1.5 q^2 +
// 0.75 q^3 below 1, 0.25 (2 - q)^3 below 2, 0 beyond. The caller supplies the
// normalisation for its dimension.
fn splat_spline(q: f32) -> f32 {
    var f = 0.0;
    if q < 1.0 {
        f = 1.0 - 1.5 * q * q + 0.75 * q * q * q;
    } else if q < 2.0 {
        let t = 2.0 - q;
        f = 0.25 * t * t * t;
    }
    return f;
}
