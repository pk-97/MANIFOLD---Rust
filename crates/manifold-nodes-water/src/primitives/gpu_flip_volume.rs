//! The water volume a surface mesh holds in the 4 m Dam Break tank, measured
//! the same way for every solver in the water race
//! (docs/GPU_FLIP_PRESSURE_SOLVE.md section 6 (measures)).

/// The water a closed surface mesh holds inside the tank (the cube of side
/// `size` from `min`) and its free surface's area, in m³ and m². The mesh
/// closes just past the walls, floor and lid, where the seam's solid caps it,
/// so the volume inside the tank is exactly the flux of
/// F = (0, clamp(y, floor, lid) − floor, 0) · [x and z inside the tank]:
/// div F is 1 inside the tank and 0 outside, and F has no x or z part, so its
/// jumps at the side walls carry no divergence. Each triangle is sampled at
/// its centroid. The free surface is the triangles inside the tank; every
/// closing face sits past a wall.
pub fn volume_and_area(triangles: impl Iterator<Item = [[f32; 3]; 3]>, min: [f64; 3], size: f64) -> (f64, f64) {
    let inside = |centroid: &[f64; 3], axis: usize| (min[axis]..=min[axis] + size).contains(&centroid[axis]);
    triangles.fold((0.0, 0.0), |(volume, area), t| {
        let [a, b, c] = t.map(|p| p.map(f64::from));
        let centroid: [f64; 3] = std::array::from_fn(|i| (a[i] + b[i] + c[i]) / 3.0);
        let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
        let normal = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        if !(inside(&centroid, 0) && inside(&centroid, 2)) {
            return (volume, area);
        }
        let height = centroid[1].clamp(min[1], min[1] + size) - min[1];
        let free = if inside(&centroid, 1) { 0.5 * normal.iter().map(|x| x * x).sum::<f64>().sqrt() } else { 0.0 };
        (volume + height * 0.5 * normal[1], area + free)
    })
}

/// The water volume a mesh holds, as a fraction of the particles' own
/// volume `truth`, less 1. A particle surface's free surface sits a skin δ
/// outside the water it wraps (about 3 cm at 64³, on the Dam Break and the
/// resting pool alike), so the volume overstates the water by δ × free-surface
/// area, and the Dam Break's free surface halves as the column falls: frame 0's
/// raw volume is the wrong baseline. δ is calibrated at frame 0, where the
/// seeded packing makes `truth` exact: δ = (V₀ − truth) / A₀. The resting
/// pool's skin (`gpu_flip_still_pool_keeps_its_meshed_volume`) checks the
/// model on a second shape.
pub struct VolumeDrift {
    truth: f64,
    skin: f64,
}

impl VolumeDrift {
    pub fn new(first: (f64, f64), truth: f64) -> Self {
        Self { truth, skin: (first.0 - truth) / first.1 }
    }

    pub fn skin(&self) -> f64 {
        self.skin
    }

    pub fn drift(&self, (volume, area): (f64, f64)) -> f64 {
        (volume - self.skin * area) / self.truth - 1.0
    }
}
