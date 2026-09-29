//! Particle look metrics, GPU_MPM_SOLVER_DESIGN.md section 7 (look gates) A1–A6,
//! over particle frames of the seam. Matter and FLIP frames go through the same
//! functions, so a gate compares solvers, not measurement code. A record is
//! live when its radius is positive and its position finite (FLIP frames carry
//! no ids).

use crate::node_graph::fluid::FluidDomainLayout;
use crate::node_graph::fluid_particles::FluidParticle;

/// The authored cell grid of a domain.
#[derive(Clone, Copy, Debug)]
pub struct Cells {
    pub min: [f32; 3],
    pub size: f32,
    pub count: [u32; 3],
}

impl Cells {
    pub fn from_layout(layout: &FluidDomainLayout) -> Self {
        Self { min: layout.min, size: layout.cell_size as f32, count: layout.cells }
    }

    fn cell_of(&self, p: [f32; 3]) -> Option<[u32; 3]> {
        let mut c = [0u32; 3];
        for axis in 0..3 {
            let q = ((p[axis] - self.min[axis]) / self.size).floor();
            if !(q >= 0.0 && q < self.count[axis] as f32) {
                return None;
            }
            c[axis] = q as u32;
        }
        Some(c)
    }

    fn index(&self, c: [u32; 3]) -> usize {
        (c[0] as usize) + self.count[0] as usize * (c[1] as usize + self.count[1] as usize * c[2] as usize)
    }
}

fn position(p: &FluidParticle) -> [f32; 3] {
    [p.position_radius[0], p.position_radius[1], p.position_radius[2]]
}

fn is_live(p: &FluidParticle) -> bool {
    p.position_radius[3] > 0.0 && p.position_radius.iter().all(|v| v.is_finite())
}

/// Live records of a frame.
pub fn live(frame: &[FluidParticle]) -> Vec<FluidParticle> {
    frame.iter().copied().filter(is_live).collect()
}

/// A1: for interior points (every cell within two cells of the point's own
/// cell holds a point), the 16-bin histogram of the fractional cell coordinate
/// per axis; returns the largest bin over the mean bin, per axis, and the
/// number of interior points. A lattice-aligned arrangement piles points into
/// a few bins.
pub fn lattice_alignment(frame: &[FluidParticle], cells: &Cells) -> ([f32; 3], usize) {
    const BINS: usize = 16;
    const REACH: i64 = 2;
    let total = cells.count.iter().map(|&n| n as usize).product();
    let mut occupied = vec![false; total];
    let live = live(frame);
    for p in &live {
        if let Some(c) = cells.cell_of(position(p)) {
            occupied[cells.index(c)] = true;
        }
    }
    let full = |c: [u32; 3]| {
        for dz in -REACH..=REACH {
            for dy in -REACH..=REACH {
                for dx in -REACH..=REACH {
                    let n = [c[0] as i64 + dx, c[1] as i64 + dy, c[2] as i64 + dz];
                    if (0..3).any(|a| n[a] < 0 || n[a] >= cells.count[a] as i64) {
                        return false;
                    }
                    if !occupied[cells.index([n[0] as u32, n[1] as u32, n[2] as u32])] {
                        return false;
                    }
                }
            }
        }
        true
    };
    let mut hist = [[0u64; BINS]; 3];
    let mut interior = 0usize;
    for p in &live {
        let pos = position(p);
        let Some(c) = cells.cell_of(pos) else { continue };
        if !full(c) {
            continue;
        }
        interior += 1;
        for axis in 0..3 {
            let q = (pos[axis] - cells.min[axis]) / cells.size;
            let bin = (((q - q.floor()) * BINS as f32) as usize).min(BINS - 1);
            hist[axis][bin] += 1;
        }
    }
    let ratio = std::array::from_fn(|axis| {
        let mean = interior as f64 / BINS as f64;
        let max = *hist[axis].iter().max().expect("16 bins") as f64;
        if mean > 0.0 { (max / mean) as f32 } else { 0.0 }
    });
    (ratio, interior)
}

/// Mean speed of the live records, m/s.
pub fn mean_speed(frame: &[FluidParticle]) -> f32 {
    let live = live(frame);
    if live.is_empty() {
        return 0.0;
    }
    let sum: f64 = live
        .iter()
        .map(|p| f64::from(p.velocity[0].hypot(p.velocity[1]).hypot(p.velocity[2])))
        .sum();
    (sum / live.len() as f64) as f32
}

/// Mean free-surface height: the highest live point in each occupied X/Z
/// cell column, averaged over those columns.
pub fn mean_surface_height(frame: &[FluidParticle], cells: &Cells) -> f32 {
    let columns = cells.count[0] as usize * cells.count[2] as usize;
    let mut top = vec![f32::NEG_INFINITY; columns];
    for p in live(frame) {
        let pos = position(&p);
        if let Some(c) = cells.cell_of(pos) {
            let i = c[0] as usize + cells.count[0] as usize * c[2] as usize;
            top[i] = top[i].max(pos[1]);
        }
    }
    let heights: Vec<f32> = top.into_iter().filter(|h| h.is_finite()).collect();
    if heights.is_empty() {
        return 0.0;
    }
    (heights.iter().map(|&h| f64::from(h)).sum::<f64>() / heights.len() as f64) as f32
}

/// The surge front along +X: the `rank`-th largest X of the live records, so
/// a few stray droplets ahead of the front do not move it.
pub fn front_position(frame: &[FluidParticle], rank: usize) -> f32 {
    let mut xs: Vec<f32> = live(frame).iter().map(|p| p.position_radius[0]).collect();
    if xs.is_empty() {
        return f32::NAN;
    }
    let k = rank.min(xs.len() - 1);
    xs.select_nth_unstable_by(k, |a, b| b.total_cmp(a));
    xs[k]
}

/// A uniform bucket grid over the live points' bounding box.
struct Buckets {
    origin: [f32; 3],
    size: f32,
    dims: [usize; 3],
    start: Vec<u32>,
    order: Vec<u32>,
}

impl Buckets {
    fn new(points: &[[f32; 3]], size: f32) -> Self {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for p in points {
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        let dims = std::array::from_fn(|a| (((hi[a] - lo[a]) / size).floor() as usize + 1).max(1));
        let mut buckets = Self { origin: lo, size, dims, start: Vec::new(), order: Vec::new() };
        let total = dims[0] * dims[1] * dims[2];
        let mut count = vec![0u32; total + 1];
        let keys: Vec<usize> = points.iter().map(|p| buckets.key(buckets.bucket(*p))).collect();
        for &k in &keys {
            count[k + 1] += 1;
        }
        for i in 1..count.len() {
            count[i] += count[i - 1];
        }
        let mut fill = count.clone();
        let mut order = vec![0u32; points.len()];
        for (i, &k) in keys.iter().enumerate() {
            order[fill[k] as usize] = i as u32;
            fill[k] += 1;
        }
        buckets.start = count;
        buckets.order = order;
        buckets
    }

    fn bucket(&self, p: [f32; 3]) -> [usize; 3] {
        std::array::from_fn(|a| (((p[a] - self.origin[a]) / self.size).floor() as usize).min(self.dims[a] - 1))
    }

    fn key(&self, b: [usize; 3]) -> usize {
        b[0] + self.dims[0] * (b[1] + self.dims[1] * b[2])
    }

    /// Calls `visit(j)` for every point in the 27 buckets around `p`.
    fn around(&self, p: [f32; 3], mut visit: impl FnMut(usize)) {
        let b = self.bucket(p);
        for z in b[2].saturating_sub(1)..=(b[2] + 1).min(self.dims[2] - 1) {
            for y in b[1].saturating_sub(1)..=(b[1] + 1).min(self.dims[1] - 1) {
                for x in b[0].saturating_sub(1)..=(b[0] + 1).min(self.dims[0] - 1) {
                    let k = self.key([x, y, z]);
                    for &j in &self.order[self.start[k] as usize..self.start[k + 1] as usize] {
                        visit(j as usize);
                    }
                }
            }
        }
    }
}

fn distance2(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

fn find(parent: &mut [u32], mut i: usize) -> usize {
    while parent[i] as usize != i {
        parent[i] = parent[parent[i] as usize];
        i = parent[i] as usize;
    }
    i
}

/// A5: the fraction of live points outside the largest body, where two
/// points closer than 1.5 × `spacing` belong to the same body.
pub fn detached_fraction(frame: &[FluidParticle], spacing: f32) -> f32 {
    let points: Vec<[f32; 3]> = live(frame).iter().map(position).collect();
    if points.is_empty() {
        return 0.0;
    }
    let link = 1.5 * spacing;
    let buckets = Buckets::new(&points, link);
    let mut parent: Vec<u32> = (0..points.len() as u32).collect();
    for i in 0..points.len() {
        buckets.around(points[i], |j| {
            if j > i && distance2(points[i], points[j]) <= link * link {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[a.max(b)] = a.min(b) as u32;
                }
            }
        });
    }
    let mut size = vec![0u32; points.len()];
    for i in 0..points.len() {
        let root = find(&mut parent, i);
        size[root] += 1;
    }
    let largest = *size.iter().max().expect("points") as f64;
    (1.0 - largest / points.len() as f64) as f32
}

/// Eigenvalues of a symmetric 3×3 matrix, ascending (trigonometric closed form).
fn symmetric_eigenvalues(m: [[f64; 3]; 3]) -> [f64; 3] {
    let p1 = m[0][1].powi(2) + m[0][2].powi(2) + m[1][2].powi(2);
    if p1 <= f64::EPSILON * (m[0][0].abs() + m[1][1].abs() + m[2][2].abs()).powi(2) {
        let mut d = [m[0][0], m[1][1], m[2][2]];
        d.sort_by(f64::total_cmp);
        return d;
    }
    let q = (m[0][0] + m[1][1] + m[2][2]) / 3.0;
    let p2 = (m[0][0] - q).powi(2) + (m[1][1] - q).powi(2) + (m[2][2] - q).powi(2) + 2.0 * p1;
    let p = (p2 / 6.0).sqrt();
    let b: [[f64; 3]; 3] = std::array::from_fn(|r| std::array::from_fn(|c| (m[r][c] - if r == c { q } else { 0.0 }) / p));
    let det = b[0][0] * (b[1][1] * b[2][2] - b[1][2] * b[2][1]) - b[0][1] * (b[1][0] * b[2][2] - b[1][2] * b[2][0])
        + b[0][2] * (b[1][0] * b[2][1] - b[1][1] * b[2][0]);
    let phi = (det / 2.0).clamp(-1.0, 1.0).acos() / 3.0;
    let largest = q + 2.0 * p * phi.cos();
    let smallest = q + 2.0 * p * (phi + 2.0 * std::f64::consts::PI / 3.0).cos();
    [smallest, 3.0 * q - largest - smallest, largest]
}

/// A6: the fraction of live points whose neighbourhood (the points within
/// 2 × `spacing`, at least 6 besides the point) is planar: the smallest
/// eigenvalue of its position covariance is below 0.1 × the largest.
pub fn sheet_fraction(frame: &[FluidParticle], spacing: f32) -> f32 {
    let points: Vec<[f32; 3]> = live(frame).iter().map(position).collect();
    if points.is_empty() {
        return 0.0;
    }
    let radius = 2.0 * spacing;
    let buckets = Buckets::new(&points, radius);
    let mut planar = 0usize;
    let mut neighbours: Vec<[f64; 3]> = Vec::with_capacity(64);
    for &p in &points {
        neighbours.clear();
        buckets.around(p, |j| {
            if distance2(p, points[j]) <= radius * radius {
                neighbours.push(points[j].map(f64::from));
            }
        });
        // The point itself is among them.
        if neighbours.len() - 1 < 6 {
            continue;
        }
        let n = neighbours.len() as f64;
        let mean: [f64; 3] = std::array::from_fn(|a| neighbours.iter().map(|q| q[a]).sum::<f64>() / n);
        let mut cov = [[0.0f64; 3]; 3];
        for q in &neighbours {
            let d = [q[0] - mean[0], q[1] - mean[1], q[2] - mean[2]];
            for (r, row) in cov.iter_mut().enumerate() {
                for (c, v) in row.iter_mut().enumerate() {
                    *v += d[r] * d[c] / n;
                }
            }
        }
        let e = symmetric_eigenvalues(cov);
        if e[2] > 0.0 && e[0] < 0.1 * e[2] {
            planar += 1;
        }
    }
    (planar as f64 / points.len() as f64) as f32
}

/// A2's reading of a run sampled once per tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settling {
    /// First time the mean speed falls below the threshold.
    pub settle_time: Option<f32>,
    /// Largest peak-to-peak of the mean surface height over any window of
    /// `window` seconds starting at or after the settle time.
    pub ringing: f32,
}

/// A2: settle time and ringing from `(time, mean_speed, surface_height)`
/// samples in time order. Settling is looked for after the fastest sample, so
/// a scene that starts at rest has not "settled" before it moves.
pub fn settling(samples: &[(f32, f32, f32)], speed_threshold: f32, window: f32) -> Settling {
    let peak = samples
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.1.total_cmp(&b.1.1))
        .map_or(0, |(i, _)| i);
    let Some(start) = samples[peak..].iter().position(|s| s.1 < speed_threshold).map(|i| peak + i) else {
        return Settling { settle_time: None, ringing: 0.0 };
    };
    let mut ringing = 0.0f32;
    for i in start..samples.len() {
        let t0 = samples[i].0;
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for s in samples[i..].iter().take_while(|s| s.0 <= t0 + window) {
            lo = lo.min(s.2);
            hi = hi.max(s.2);
        }
        ringing = ringing.max(hi - lo);
    }
    Settling { settle_time: Some(samples[start].0), ringing }
}

/// Martin & Moyce 1952 (J. C. Martin and W. J. Moyce, "An experimental study
/// of the collapse of liquid columns on a rigid horizontal plane", Phil.
/// Trans. R. Soc. Lond. A 244, 312–324), Figure 3, n² = 2 (column twice as
/// tall as wide), a = 2.25 in: surge front Z = z/a against T = t·√(2g/a),
/// where a is the column width and z the front's distance from the back
/// wall. Digitised by PySPH (`pysph/examples/db_exp_data.py`,
/// `get_martin_moyce_2`); its a = 1.125 in series agrees within 3% for T in
/// [1, 3].
pub const MARTIN_MOYCE_N2_2: [(f32, f32); 15] = [
    (0.832, 1.217),
    (1.219, 1.474),
    (1.997, 2.292),
    (2.547, 2.995),
    (3.345, 4.134),
    (4.034, 4.944),
    (4.418, 5.881),
    (5.091, 6.980),
    (5.685, 7.945),
    (6.306, 8.966),
    (6.822, 9.986),
    (7.439, 10.963),
    (8.031, 11.977),
    (8.633, 13.005),
    (9.237, 13.970),
];

/// Martin & Moyce's Z at `t_star`, linear between the digitised points.
pub fn martin_moyce_front(t_star: f32) -> Option<f32> {
    MARTIN_MOYCE_N2_2.windows(2).find(|w| w[0].0 <= t_star && t_star <= w[1].0).map(|w| {
        let s = (t_star - w[0].0) / (w[1].0 - w[0].0);
        w[0].1 + s * (w[1].1 - w[0].1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn particle(p: [f32; 3]) -> FluidParticle {
        FluidParticle { position_radius: [p[0], p[1], p[2], 0.01], velocity: [0.0; 3], id: 0 }
    }

    fn unit(seed: &mut u64) -> f32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 40) as f32 / (1u64 << 24) as f32
    }

    fn cells() -> Cells {
        Cells { min: [0.0; 3], size: 0.1, count: [10, 10, 10] }
    }

    fn block(per_axis: usize, spacing: f32, jitter: f32, seed: &mut u64) -> Vec<FluidParticle> {
        let mut v = Vec::new();
        for z in 0..per_axis {
            for y in 0..per_axis {
                for x in 0..per_axis {
                    let base = [x, y, z].map(|i| (i as f32 + 0.5) * spacing);
                    v.push(particle(std::array::from_fn(|a| base[a] + jitter * (unit(seed) - 0.5) * spacing)));
                }
            }
        }
        v
    }

    #[test]
    fn look_alignment_flags_lattice_points_and_passes_jittered() {
        let mut seed = 0x9e37_79b9_7f4a_7c15;
        let (aligned, n) = lattice_alignment(&block(20, 0.05, 0.0, &mut seed), &cells());
        assert!(n > 0);
        assert!(aligned.iter().all(|&r| r > 3.0), "{aligned:?}");
        let (jittered, _) = lattice_alignment(&block(40, 0.025, 1.0, &mut seed), &cells());
        assert!(jittered.iter().all(|&r| r < 1.3), "{jittered:?}");
    }

    #[test]
    fn look_detached_fraction_counts_points_off_the_body() {
        let mut seed = 7;
        let mut frame = block(10, 0.05, 0.0, &mut seed);
        frame.push(particle([2.0, 2.0, 2.0]));
        frame.push(particle([3.0, 2.0, 2.0]));
        let expected = 2.0 / frame.len() as f32;
        assert!((detached_fraction(&frame, 0.05) - expected).abs() < 1e-6);
    }

    #[test]
    fn look_sheet_fraction_finds_planes_not_blocks() {
        let mut seed = 11;
        let mut sheet = Vec::new();
        for z in 0..30 {
            for x in 0..30 {
                sheet.push(particle([x as f32 * 0.05, 1.0, z as f32 * 0.05]));
            }
        }
        assert!(sheet_fraction(&sheet, 0.05) > 0.9);
        let solid = block(16, 0.05, 0.3, &mut seed);
        assert!(sheet_fraction(&solid, 0.05) < 0.02, "{}", sheet_fraction(&solid, 0.05));
    }

    #[test]
    fn look_eigenvalues_match_diagonal_and_rotated() {
        let e = symmetric_eigenvalues([[3.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 2.0]]);
        assert!((e[0] - 1.0).abs() < 1e-12 && (e[1] - 2.0).abs() < 1e-12 && (e[2] - 3.0).abs() < 1e-12);
        // diag(1, 3) rotated 45° in XY, plus 5 on Z.
        let e = symmetric_eigenvalues([[2.0, 1.0, 0.0], [1.0, 2.0, 0.0], [0.0, 0.0, 5.0]]);
        assert!((e[0] - 1.0).abs() < 1e-9 && (e[1] - 3.0).abs() < 1e-9 && (e[2] - 5.0).abs() < 1e-9, "{e:?}");
    }

    #[test]
    fn look_settling_reads_time_and_ringing() {
        let samples: Vec<(f32, f32, f32)> = (0..600)
            .map(|i| {
                let t = i as f32 / 60.0;
                let speed = if t < 0.5 { 0.0 } else if t < 4.0 { 0.5 } else { 0.01 };
                (t, speed, 1.0 + 0.002 * (t * 3.0).sin())
            })
            .collect();
        let s = settling(&samples, 0.02, 2.0);
        assert_eq!(s.settle_time, Some(4.0));
        assert!((s.ringing - 0.004).abs() < 2e-4, "{s:?}");
        assert_eq!(settling(&samples[..100], 0.02, 2.0).settle_time, None);
    }

    #[test]
    fn look_surface_and_front() {
        let mut seed = 3;
        let frame = block(10, 0.05, 0.0, &mut seed);
        assert!((mean_surface_height(&frame, &cells()) - 0.475).abs() < 1e-5);
        let mut front = frame.clone();
        front.push(particle([4.0, 0.0, 0.0]));
        assert!((front_position(&front, 1) - 0.475).abs() < 1e-5);
    }

    #[test]
    fn look_martin_moyce_interpolates_inside_the_series() {
        assert_eq!(martin_moyce_front(1.219), Some(1.474));
        let mid = martin_moyce_front(1.608).unwrap();
        assert!((mid - 1.883).abs() < 1e-3, "{mid}");
        assert_eq!(martin_moyce_front(0.5), None);
    }
}
