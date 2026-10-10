//! Marker level sets for surface proofs that FLIP sheeting and whitewater share.

/// Union of spheres of the engine's liquid SDF radius (0.5·√3·dx) around
/// each marker, capped at ±3 dx: the shape of ParticleLevelSet's input, not
/// its exact construction.
pub fn marker_phi(markers: &[[f32; 3]], cells: [u32; 3], dx: f32) -> Vec<f32> {
    let n = cells.map(|c| c as usize);
    let radius = 0.5 * 3f32.sqrt() * dx;
    let mut phi = vec![3.0 * dx; n[0] * n[1] * n[2]];
    for m in markers {
        let lo = m.map(|c| ((c / dx).floor() as isize - 3).max(0) as usize);
        for k in lo[2]..(lo[2] + 7).min(n[2]) {
            for j in lo[1]..(lo[1] + 7).min(n[1]) {
                for i in lo[0]..(lo[0] + 7).min(n[0]) {
                    let c = [i, j, k].map(|v| (v as f32 + 0.5) * dx);
                    let d = ((c[0] - m[0]).powi(2) + (c[1] - m[1]).powi(2) + (c[2] - m[2]).powi(2)).sqrt() - radius;
                    let slot = &mut phi[i + n[0] * (j + n[1] * k)];
                    *slot = slot.min(d.max(-3.0 * dx));
                }
            }
        }
    }
    phi
}
