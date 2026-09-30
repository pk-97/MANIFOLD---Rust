//! The water volume a surface mesh holds in the 4 m Dam Break tank, measured
//! the same way for every solver in the FFT water race
//! (docs/FFT_WATER_SOLVER_DESIGN.md P3).

/// The mesh's enclosed volume and its area, in m³ and m². The mesh stays
/// open where the water meets a wall or the floor, so the volume is the flux
/// of (0, y − floor, 0) through it, Σ ȳ · (n·A)_y per triangle: the missing
/// wall pieces carry no y-flux and the floor piece sits at y − floor = 0.
/// Only water on the lid would be missed. The area is the free surface's.
pub(crate) fn volume_and_area(triangles: impl Iterator<Item = [[f32; 3]; 3]>, floor: f64) -> (f64, f64) {
    triangles.fold((0.0, 0.0), |(volume, area), t| {
        let [a, b, c] = t.map(|p| p.map(f64::from));
        let (u, v) = ([b[0] - a[0], b[1] - a[1], b[2] - a[2]], [c[0] - a[0], c[1] - a[1], c[2] - a[2]]);
        let normal = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
        let length = normal.iter().map(|x| x * x).sum::<f64>().sqrt();
        (volume + ((a[1] + b[1] + c[1]) / 3.0 - floor) * 0.5 * normal[1], area + 0.5 * length)
    })
}

/// The water volume a mesh holds, as a fraction of the particles' own
/// volume `truth`, less 1. Any particle surface sits a skin δ outside the
/// water it wraps, so the enclosed volume overstates the water by δ × area,
/// and the Dam Break's free surface halves as the column falls: frame 0's
/// raw volume is the wrong baseline. δ is calibrated at frame 0, where the
/// seeded packing makes `truth` exact: δ = (V₀ − truth) / A₀. The resting
/// pool's skin (`fft_water_still_pool_keeps_its_meshed_volume`) checks the
/// model on a second shape.
pub(crate) struct VolumeDrift {
    truth: f64,
    skin: f64,
}

impl VolumeDrift {
    pub(crate) fn new(first: (f64, f64), truth: f64) -> Self {
        Self { truth, skin: (first.0 - truth) / first.1 }
    }

    pub(crate) fn skin(&self) -> f64 {
        self.skin
    }

    #[cfg_attr(
        not(feature = "water-race-probes"),
        expect(dead_code, reason = "the race probes under water-race-probes read the drift per frame")
    )]
    pub(crate) fn drift(&self, (volume, area): (f64, f64)) -> f64 {
        (volume - self.skin * area) / self.truth - 1.0
    }
}
