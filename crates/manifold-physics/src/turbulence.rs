//! Turbulence: a smooth swirling vector field, the curl of three gradient
//! noise potentials (curl noise, Bridson et al. 2007). Curl noise has no
//! divergence, so it stirs water and swarms without squeezing them, which is
//! why Houdini's and Blender's wind turbulence use it.

use crate::PhysicsError;
use crate::interaction::VectorField;

/// Scales the raw curl so its typical length is about 1 at any size.
const STRENGTH_NORMALIZE: f32 = 0.6;

/// Offsets that decorrelate the three potentials.
const POTENTIAL_OFFSETS: [[f32; 3]; 3] = [[0.0; 3], [31.416, -47.853, 12.734], [-73.19, 19.27, 58.61]];

/// The noise repeats every this many cells (`size` metres each) along every
/// axis, so a drift offset can be wrapped to one period and keep its
/// precision through a long set.
pub const TURBULENCE_PERIOD_CELLS: u32 = 256;

/// Curl noise with features about `size` metres across, sampled at
/// `(x − offset) / size`, so moving `offset` drifts the swirls through space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TurbulenceField {
    size: f32,
    offset: [f32; 3],
    seed: u32,
}

impl TurbulenceField {
    pub fn new(size: f32, offset: [f32; 3], seed: u32) -> Result<Self, PhysicsError> {
        if !size.is_finite() || size <= 0.0 {
            return Err(PhysicsError::InvalidInput("turbulence size must be finite and positive"));
        }
        if !offset.iter().all(|c| c.is_finite()) {
            return Err(PhysicsError::InvalidInput("turbulence offset must be finite"));
        }
        Ok(Self { size, offset, seed })
    }

    pub fn size(&self) -> f32 {
        self.size
    }

    pub fn offset(&self) -> [f32; 3] {
        self.offset
    }

    pub fn seed(&self) -> u32 {
        self.seed
    }

    /// A drift offset in metres, wrapped to the noise's period on each axis:
    /// the same swirls, with f32 precision kept however far they have
    /// travelled (an hour at 50 m/s is 180 km).
    pub fn wrap_offset(size: f32, offset: [f64; 3]) -> [f32; 3] {
        let period = f64::from(TURBULENCE_PERIOD_CELLS) * f64::from(size);
        offset.map(|c| c.rem_euclid(period) as f32)
    }
}

impl VectorField for TurbulenceField {
    fn sample(&self, position: [f32; 3]) -> [f32; 3] {
        if !position.iter().all(|c| c.is_finite()) {
            return [0.0; 3];
        }
        let p: [f32; 3] = std::array::from_fn(|axis| (position[axis] - self.offset[axis]) / self.size);
        let gradients: [[f32; 3]; 3] = std::array::from_fn(|potential| {
            let q: [f32; 3] = std::array::from_fn(|axis| p[axis] + POTENTIAL_OFFSETS[potential][axis]);
            noise_gradient(q, self.seed.wrapping_add((potential as u32).wrapping_mul(0x9e37_79b9)))
        });
        let [x, y, z] = gradients;
        // curl ψ = (∂ψz/∂y − ∂ψy/∂z, ∂ψx/∂z − ∂ψz/∂x, ∂ψy/∂x − ∂ψx/∂y), in
        // noise units; the noise-space curl is the world curl times size, so
        // the result is already dimensionless.
        [
            (z[1] - y[2]) * STRENGTH_NORMALIZE,
            (x[2] - z[0]) * STRENGTH_NORMALIZE,
            (y[0] - x[1]) * STRENGTH_NORMALIZE,
        ]
    }
}

fn hash(x: i32, y: i32, z: i32, seed: u32) -> u32 {
    let [x, y, z] = [x, y, z].map(|c| c.rem_euclid(TURBULENCE_PERIOD_CELLS as i32));
    let mut h = seed ^ (x as u32).wrapping_mul(0x8da6_b343) ^ (y as u32).wrapping_mul(0xd816_3841) ^ (z as u32).wrapping_mul(0xcb1a_b31f);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

/// One of Perlin's twelve cube-edge gradients.
fn gradient(x: i32, y: i32, z: i32, seed: u32) -> [f32; 3] {
    const EDGES: [[f32; 3]; 12] = [
        [1.0, 1.0, 0.0], [-1.0, 1.0, 0.0], [1.0, -1.0, 0.0], [-1.0, -1.0, 0.0],
        [1.0, 0.0, 1.0], [-1.0, 0.0, 1.0], [1.0, 0.0, -1.0], [-1.0, 0.0, -1.0],
        [0.0, 1.0, 1.0], [0.0, -1.0, 1.0], [0.0, 1.0, -1.0], [0.0, -1.0, -1.0],
    ];
    EDGES[(hash(x, y, z, seed) % 12) as usize]
}

/// The gradient of quintic-faded gradient noise at `p`, analytically.
fn noise_gradient(p: [f32; 3], seed: u32) -> [f32; 3] {
    let cell = p.map(|c| c.floor());
    let i = cell.map(|c| c as i32);
    let f: [f32; 3] = std::array::from_fn(|a| p[a] - cell[a]);
    let u = f.map(|t| t * t * t * (t * (t * 6.0 - 15.0) + 10.0));
    let du = f.map(|t| 30.0 * t * t * (t * (t - 2.0) + 1.0));
    // Corner k has offset (k & 1, k >> 1 & 1, k >> 2 & 1).
    let mut g = [[0.0f32; 3]; 8];
    let mut v = [0.0f32; 8];
    for (k, (gk, vk)) in g.iter_mut().zip(v.iter_mut()).enumerate() {
        let o = [(k & 1) as i32, ((k >> 1) & 1) as i32, ((k >> 2) & 1) as i32];
        *gk = gradient(i[0] + o[0], i[1] + o[1], i[2] + o[2], seed);
        *vk = (0..3).map(|a| gk[a] * (f[a] - o[a] as f32)).sum();
    }
    // Trilinear blend of the corner values under the faded weights u:
    // n = Σ_k w_k(u) v_k, so ∇n = Σ_k w_k ∇v_k + Σ_k v_k ∇w_k, ∇v_k = g_k.
    let mut grad = [0.0f32; 3];
    for k in 0..8 {
        let o = [k & 1, (k >> 1) & 1, (k >> 2) & 1];
        let w: [f32; 3] = std::array::from_fn(|a| if o[a] == 1 { u[a] } else { 1.0 - u[a] });
        let dw: [f32; 3] = std::array::from_fn(|a| if o[a] == 1 { du[a] } else { -du[a] });
        let weight = w[0] * w[1] * w[2];
        for a in 0..3 {
            let others: f32 = (0..3).filter(|&b| b != a).map(|b| w[b]).product();
            grad[a] += weight * g[k][a] + v[k] * dw[a] * others;
        }
    }
    grad
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise_value(p: [f32; 3], seed: u32) -> f32 {
        let cell = p.map(|c| c.floor());
        let f: [f32; 3] = std::array::from_fn(|a| p[a] - cell[a]);
        let u = f.map(|t| t * t * t * (t * (t * 6.0 - 15.0) + 10.0));
        (0..8)
            .map(|k| {
                let o = [k & 1, (k >> 1) & 1, (k >> 2) & 1];
                let g = gradient(cell[0] as i32 + o[0], cell[1] as i32 + o[1], cell[2] as i32 + o[2], seed);
                let value: f32 = (0..3).map(|a| g[a] * (f[a] - o[a] as f32)).sum();
                let weight: f32 = (0..3).map(|a| if o[a] == 1 { u[a] } else { 1.0 - u[a] }).product();
                weight * value
            })
            .sum()
    }

    fn sample_points() -> impl Iterator<Item = [f32; 3]> {
        (0..400).map(|i| {
            let t = i as f32;
            [(t * 0.731).sin() * 7.3, (t * 0.377).cos() * 5.1 + 0.3, (t * 0.913).sin() * 6.7 - 1.1]
        })
    }

    #[test]
    fn turbulence_noise_gradient_matches_finite_differences() {
        let h = 1.0e-3;
        for p in sample_points() {
            let grad = noise_gradient(p, 7);
            for a in 0..3 {
                let mut plus = p;
                let mut minus = p;
                plus[a] += h;
                minus[a] -= h;
                let numeric = (noise_value(plus, 7) - noise_value(minus, 7)) / (2.0 * h);
                assert!((numeric - grad[a]).abs() < 2.0e-2, "{p:?} axis {a}: {numeric} vs {}", grad[a]);
            }
        }
    }

    /// Divergence free (the property that keeps a liquid from being
    /// squeezed), unit-ish strength at any size, and drift moves the swirls.
    #[test]
    fn turbulence_is_divergence_free_sized_and_drifts() {
        let field = TurbulenceField::new(2.0, [0.0; 3], 3).unwrap();
        let h = 1.0e-2;
        let mut sum_squares = 0.0;
        for p in sample_points() {
            let mut divergence = 0.0;
            for a in 0..3 {
                let mut plus = p;
                let mut minus = p;
                plus[a] += h;
                minus[a] -= h;
                divergence += (field.sample(plus)[a] - field.sample(minus)[a]) / (2.0 * h);
            }
            assert!(divergence.abs() < 2.0e-2, "{p:?}: divergence {divergence}");
            let v = field.sample(p);
            sum_squares += v.iter().map(|c| c * c).sum::<f32>();
        }
        let rms = (sum_squares / 400.0).sqrt();
        assert!((0.6..1.6).contains(&rms), "rms {rms}");
        let large = TurbulenceField::new(8.0, [0.0; 3], 3).unwrap();
        assert_eq!(large.sample([8.0, 4.0, -4.0]), field.sample([2.0, 1.0, -1.0]));
        let drifted = TurbulenceField::new(2.0, [1.0, 0.0, 0.0], 3).unwrap();
        assert_eq!(drifted.sample([1.5, 0.2, 0.3]), field.sample([0.5, 0.2, 0.3]));
        assert!(TurbulenceField::new(0.0, [0.0; 3], 0).is_err());
    }

    /// The swirls repeat every period on each axis, so a drift that has
    /// travelled any distance wraps to the same swirls at full precision.
    #[test]
    fn turbulence_repeats_so_a_long_drift_wraps_exactly() {
        let field = TurbulenceField::new(2.0, [0.0; 3], 3).unwrap();
        let period = TURBULENCE_PERIOD_CELLS as f32 * 2.0;
        // Equal up to the f32 rounding of a coordinate a period out.
        for p in [[1.5, 0.25, -0.75], [-3.0, 7.5, 2.25]] {
            let (far, near) = (field.sample([p[0] + period, p[1] - period, p[2] + 2.0 * period]), field.sample(p));
            assert!(far.iter().zip(near).all(|(a, b)| (a - b).abs() < 1e-3), "{far:?} vs {near:?}");
        }
        // An hour at 50 m/s, plus a quarter metre.
        let wrapped = TurbulenceField::wrap_offset(2.0, [180_000.25 + 512.0 * 351.0, -0.75, 0.0]);
        assert_eq!(wrapped, [288.25, 511.25, 0.0]);
        assert!(wrapped.iter().all(|c| (0.0..period).contains(c)));
    }
}
