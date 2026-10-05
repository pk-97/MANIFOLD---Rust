//! Checked against FLIP Fluids particlesheeter.cpp (MIT); see THIRD_PARTY_NOTICES.md.
//! FLIP's own sheet seeding as a test oracle for the GPU sheeting port.
//! Built only with the `whitewater-oracle` feature; nothing in the product
//! calls it. The output is the sheeter's, before the engine's fill-rate draw.

use std::sync::Mutex;

use crate::{FluidError, native_result};

unsafe extern "C" {
    fn manifold_fluids_oracle_sheet_particles(
        positions: *const f32,
        count: usize,
        phi: *const f32,
        isize: u32,
        jsize: u32,
        ksize: u32,
        dx: f64,
        fill_threshold: f32,
        seeds_out: *mut f32,
        capacity: usize,
        seed_count_out: *mut usize,
    ) -> i32;
}

/// The native call pins the process-global FLIP thread count to 1 and
/// restores it; two overlapping calls would restore each other's value.
static SHEET_ORACLE: Mutex<()> = Mutex::new(());

/// The engine's default `sheetFillThreshold`.
pub const DEFAULT_FILL_THRESHOLD: f32 = -0.95;

/// `ParticleSheeter::generateSheetParticles` on markers at `positions`
/// (grid-local, inside the grid) and the cell-centred surface level set `phi`
/// (x fastest). Writes the first `out.len()` seeds in the sheeter's order and
/// returns the true seed count, which may exceed `out.len()`.
pub fn sheet_particles_into(
    positions: &[[f32; 3]],
    phi: &[f32],
    cells: [u32; 3],
    dx: f64,
    fill_threshold: f32,
    out: &mut [[f32; 3]],
) -> Result<usize, FluidError> {
    let count = cells
        .iter()
        .try_fold(1usize, |n, &c| n.checked_mul(c as usize))
        .ok_or_else(|| FluidError::input("oracle sheet grid is too large"))?;
    if phi.len() != count {
        return Err(FluidError::input(format!(
            "oracle sheet level set holds {} values; a {cells:?} grid needs {count}",
            phi.len()
        )));
    }
    let _serial = SHEET_ORACLE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut seeds = 0usize;
    // SAFETY: `phi` covers the grid (checked above); positions and out are
    // slices of float triples whose lengths are passed alongside.
    let ok = unsafe {
        manifold_fluids_oracle_sheet_particles(
            positions.as_ptr().cast(),
            positions.len(),
            phi.as_ptr(),
            cells[0],
            cells[1],
            cells[2],
            dx,
            fill_threshold,
            out.as_mut_ptr().cast(),
            out.len(),
            &mut seeds,
        )
    };
    native_result(ok, "oracle sheet particles")?;
    Ok(seeds)
}

/// Every seed of [`sheet_particles_into`], in the sheeter's order.
pub fn sheet_particles(
    positions: &[[f32; 3]],
    phi: &[f32],
    cells: [u32; 3],
    dx: f64,
    fill_threshold: f32,
) -> Result<Vec<[f32; 3]>, FluidError> {
    let count = sheet_particles_into(positions, phi, cells, dx, fill_threshold, &mut [])?;
    let mut seeds = vec![[0.0; 3]; count];
    let again = sheet_particles_into(positions, phi, cells, dx, fill_threshold, &mut seeds)?;
    if again != count {
        return Err(FluidError::input(format!("oracle sheet gave {count} then {again} seeds")));
    }
    Ok(seeds)
}

/// Test fixture: thin curved sheets with random holes on a dx 0.25 grid: a spherical
/// shell and a rippled horizontal sheet, both 0.3 m thick, a few
/// thousand markers, fixed seed.
pub fn splash_fixture() -> (Vec<[f32; 3]>, Vec<f32>, [u32; 3], f64) {
    const M: u32 = 40;
    const H: f64 = 0.25;
    const T: f32 = 0.15;
    let centre = [5.0f32, 4.5, 5.0];
    let radius = 2.2f32;
    let ripple = |x: f32, z: f32| 8.3 + 0.4 * (1.3 * x).sin() * (0.9 * z).cos();
    let m = M as usize;
    let phi: Vec<f32> = (0..m * m * m)
        .map(|index| {
            let c = [index % m, (index / m) % m, index / (m * m)].map(|v| (v as f32 + 0.5) * H as f32);
            let r = ((c[0] - centre[0]).powi(2) + (c[1] - centre[1]).powi(2) + (c[2] - centre[2]).powi(2)).sqrt();
            let shell = (r - radius).abs() - T;
            let sheet = (c[1] - ripple(c[0], c[2])).abs() - T;
            shell.min(sheet)
        })
        .collect();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut uniform = move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32
    };
    fn direction(u: &mut dyn FnMut() -> f32) -> [f32; 3] {
        let z = 2.0 * u() - 1.0;
        let a = std::f32::consts::TAU * u();
        let s = (1.0 - z * z).sqrt();
        [s * a.cos(), z, s * a.sin()]
    }
    let shell_holes: Vec<[f32; 3]> = (0..6).map(|_| direction(&mut uniform)).collect();
    // Sheet holes: centre x, radius 0.3–0.6 m, centre z.
    let mut sheet_holes = Vec::new();
    for _ in 0..6 {
        let x = 1.5 + 7.0 * uniform();
        let r = 0.3 + 0.3 * uniform();
        sheet_holes.push([x, r, 1.5 + 7.0 * uniform()]);
    }
    let mut markers = Vec::new();
    while markers.len() < 2500 {
        let d = direction(&mut uniform);
        let r = radius + T * 0.8 * (2.0 * uniform() - 1.0);
        if shell_holes.iter().any(|h| d[0] * h[0] + d[1] * h[1] + d[2] * h[2] > 0.25f32.cos()) {
            continue;
        }
        markers.push([centre[0] + r * d[0], centre[1] + r * d[1], centre[2] + r * d[2]]);
    }
    while markers.len() < 4000 {
        let (x, z) = (1.5 + 7.0 * uniform(), 1.5 + 7.0 * uniform());
        if sheet_holes.iter().any(|h| (x - h[0]).hypot(z - h[2]) < h[1]) {
            continue;
        }
        markers.push([x, ripple(x, z) + T * 0.8 * (2.0 * uniform() - 1.0), z]);
    }
    (markers, phi, [M; 3], H)
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_FILL_THRESHOLD, sheet_particles, sheet_particles_into};

    // All fixtures use one-metre cells, so the sheeter's gradient (the
    // trilinear interpolant's derivative, not divided by dx) is the true one,
    // and keep everything 4+ cells from the border, where interpolation reads
    // zero outside the grid and the sheeter clears a 3-cell band.
    const N: u32 = 16;
    const DX: f64 = 1.0;
    const CELLS: [u32; 3] = [N; 3];
    const HOLE: [f32; 2] = [8.0, 8.0];
    const HOLE_RADIUS: f32 = 1.0;
    const SHEET_Y: f32 = 8.1;

    fn field(phi_at_centre_y: impl Fn(f32) -> f32) -> Vec<f32> {
        let n = N as usize;
        (0..n * n * n).map(|index| phi_at_centre_y(((index / n) % n) as f32 + 0.5)).collect()
    }

    /// Four markers per cell (a 2×2 lattice at sub-cell centres in x and z)
    /// over cells 4..12 in x and z, at the given heights.
    fn lattice(heights: &[f32]) -> Vec<[f32; 3]> {
        let mut out = Vec::new();
        for &y in heights {
            for zi in 8..24 {
                for xi in 8..24 {
                    out.push([0.25 + 0.5 * xi as f32, y, 0.25 + 0.5 * zi as f32]);
                }
            }
        }
        out
    }

    fn in_hole(p: &[f32; 3]) -> bool {
        (p[0] - HOLE[0]).hypot(p[2] - HOLE[1]) < HOLE_RADIUS
    }

    /// A slab whose interpolated φ runs 0 at y 7.5, −0.5 at 8.5, +0.5 at 9.5
    /// (cell centres |y − 8.25| − 0.75), and one marker layer at y 8.1 with
    /// the markers within 1 m of (8, 8) in xz removed. The level set still
    /// covers the hole: the particles thinned, the surface did not.
    fn holed_sheet() -> (Vec<[f32; 3]>, Vec<f32>, Vec<[f32; 3]>) {
        let phi = field(|y| (y - 8.25).abs() - 0.75);
        let (removed, kept): (Vec<_>, Vec<_>) = lattice(&[SHEET_Y]).into_iter().partition(in_hole);
        (kept, phi, removed)
    }

    /// Every marker is a sheet particle: φ = −0.3, −∇φ points +y, and the
    /// inward walk reads −0.4 at 0.5 m then +0.1 at 1 m, φ ≥ 0, so the depth
    /// test passes. Candidates are the half-cell centres with φ in [−1, 0):
    /// y 7.75, 8.25, 8.75, each pulled 0.75 of the way to the y 8.1 plane of
    /// its three nearest markers, into y [8.0, 8.5). Every such sub-cell over
    /// a marker is already masked, and past the lattice edge all neighbours
    /// lie on one side, so no neighbour opposes the centroid (mindot stays
    /// above −0.95). Only the hole, ringed by markers, can seed, and the
    /// projection moves along y alone, so a seed's xz is a removed marker's.
    /// The four inner hole columns never seed: e.g. from (7.75, 7.75) the
    /// three nearest markers, (7.25, 7.25), (6.75, 7.75), (7.75, 6.75), lie
    /// on one line, so the plane guard (|cross| < eps) skips them.
    #[test]
    fn oracle_sheet_seeds_fill_a_hole_in_a_thin_sheet() {
        let (markers, phi, removed) = holed_sheet();
        assert_eq!(removed.len(), 12, "the hole removes the 12 markers within 1 m");
        let seeds = sheet_particles(&markers, &phi, CELLS, DX, DEFAULT_FILL_THRESHOLD).expect("oracle");
        assert!(!seeds.is_empty(), "a hole in a thin sheet seeds");
        for s in &seeds {
            assert!(
                removed.iter().any(|r| (r[0] - s[0]).abs() < 1e-5 && (r[2] - s[2]).abs() < 1e-5),
                "{s:?} is not over a removed marker"
            );
            assert!((8.0..8.5).contains(&s[1]), "{s:?} is not projected into the sheet's sub-layer");
        }
    }

    /// The sheeter visits candidates by 2-cell bucket (k, j, i), so in every
    /// hole column the y 7.75 candidate (bucket j 3) comes before the y 8.25
    /// one (bucket j 4); both project into the one sub-cell y [8.0, 8.5), at
    /// 8.0125 and 8.1375. At the default threshold the 8.25 candidate seeds
    /// (the hole test). mindot does not depend on the threshold, so at
    /// threshold 0 it still passes, and the only thing that can stop it after
    /// the 7.75 candidate seeds is the mask, set as each seed is chosen.
    #[test]
    fn oracle_sheet_seeds_one_per_sub_cell() {
        let (markers, phi, _) = holed_sheet();
        let strict = sheet_particles(&markers, &phi, CELLS, DX, DEFAULT_FILL_THRESHOLD).expect("oracle");
        let loose = sheet_particles(&markers, &phi, CELLS, DX, 0.0).expect("oracle");
        assert!(!strict.is_empty());
        let column = |p: &[f32; 3]| [(p[0] / 0.5).floor() as i32, (p[2] / 0.5).floor() as i32];
        for s in &strict {
            assert!((s[1] - 8.1375).abs() < 1e-4, "{s:?} is not the y 8.25 candidate");
            let here: Vec<_> = loose.iter().filter(|t| column(t) == column(s)).collect();
            assert_eq!(here.len(), 1, "column of {s:?} seeded {here:?}");
            assert!((here[0][1] - 8.0125).abs() < 1e-4, "{:?} is not the y 7.75 candidate", here[0]);
        }
        let sub = |p: &[f32; 3]| p.map(|c| (c / 0.5).floor() as i32);
        for (a, s) in loose.iter().enumerate() {
            for t in &loose[a + 1..] {
                assert_ne!(sub(s), sub(t), "{s:?} and {t:?} share a sub-cell");
            }
        }
    }

    /// A deep pool, φ = y − 8, markers at y 6.3 and 7.3 (φ −1.7 and −0.7, so
    /// near the surface and four to a cell, under the six-per-cell density
    /// cut). −∇φ points straight down and φ falls by 0.5 a step, never rising
    /// or reaching 0, so no marker is a sheet particle and nothing seeds.
    #[test]
    fn oracle_sheet_still_pool_seeds_nothing() {
        let phi = field(|y| y - 8.0);
        let markers = lattice(&[6.3, 7.3]);
        let seeds = sheet_particles(&markers, &phi, CELLS, DX, DEFAULT_FILL_THRESHOLD).expect("oracle");
        assert!(seeds.is_empty(), "a still pool seeded {seeds:?}");
    }

    /// The CPU port against the native sheeter: same count, same order, and
    /// positions within `ULPS` (the engine build fuses multiply-adds, Rust
    /// does not). Returns the seed count.
    fn port_matches(markers: &[[f32; 3]], phi: &[f32], cells: [u32; 3], dx: f64, threshold: f32) -> usize {
        const ULPS: i32 = 1;
        let native = sheet_particles(markers, phi, cells, dx, threshold).expect("oracle");
        let port = crate::sheeter::generate_sheet_particles(markers, phi, cells, dx, threshold).expect("port");
        let worst = native
            .iter()
            .zip(&port)
            .flat_map(|(a, b)| (0..3).map(move |c| (a[c].to_bits() as i32 - b[c].to_bits() as i32).abs()))
            .max()
            .unwrap_or(0);
        assert_eq!(native.len(), port.len(), "seed counts differ");
        assert!(worst <= ULPS, "positions differ by {worst} ulp");
        native.len()
    }

    #[test]
    fn port_matches_oracle_on_the_holed_sheet() {
        let (markers, phi, _) = holed_sheet();
        assert_eq!(port_matches(&markers, &phi, CELLS, DX, DEFAULT_FILL_THRESHOLD), 8);
        assert_eq!(port_matches(&markers, &phi, CELLS, DX, 0.0), 8);
    }

    #[test]
    fn port_matches_oracle_on_the_still_pool() {
        let phi = field(|y| y - 8.0);
        assert_eq!(port_matches(&lattice(&[6.3, 7.3]), &phi, CELLS, DX, DEFAULT_FILL_THRESHOLD), 0);
    }

    #[test]
    fn port_matches_oracle_on_a_splash() {
        let (markers, phi, cells, dx) = super::splash_fixture();
        let strict = port_matches(&markers, &phi, cells, dx, DEFAULT_FILL_THRESHOLD);
        let loose = port_matches(&markers, &phi, cells, dx, -0.5);
        assert!(strict >= 20 && loose > strict, "the splash seeds {strict} at -0.95 and {loose} at -0.5");
    }

    /// A short buffer gets the first seeds and the true count, never a
    /// silently truncated one; bad input is refused.
    #[test]
    fn oracle_sheet_reports_the_true_count() {
        let (markers, phi, _) = holed_sheet();
        let all = sheet_particles(&markers, &phi, CELLS, DX, DEFAULT_FILL_THRESHOLD).expect("oracle");
        assert!(all.len() >= 2, "the fixture needs two or more seeds, got {}", all.len());
        let mut one = [[0.0f32; 3]; 1];
        let count = sheet_particles_into(&markers, &phi, CELLS, DX, DEFAULT_FILL_THRESHOLD, &mut one).expect("oracle");
        assert_eq!(count, all.len());
        assert_eq!(one[0], all[0]);
        assert_eq!(sheet_particles(&[], &phi, CELLS, DX, DEFAULT_FILL_THRESHOLD).expect("empty"), Vec::<[f32; 3]>::new());
        assert!(sheet_particles(&markers, &phi[1..], CELLS, DX, DEFAULT_FILL_THRESHOLD).is_err(), "short field");
        assert!(sheet_particles(&[[16.5, 8.0, 8.0]], &phi, CELLS, DX, DEFAULT_FILL_THRESHOLD).is_err(), "outside the grid");
        assert!(sheet_particles(&markers, &phi, CELLS, 0.0, DEFAULT_FILL_THRESHOLD).is_err(), "zero dx");
        assert!(sheet_particles(&markers, &phi, CELLS, DX, 0.5).is_err(), "threshold out of range");
    }
}
