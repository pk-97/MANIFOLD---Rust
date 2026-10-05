//! Checked against FLIP Fluids particlesheeter.cpp (MIT); see THIRD_PARTY_NOTICES.md.
//! FLIP's own sheet seeding as a test oracle for the GPU sheeting port.
//! Built only with the `whitewater-oracle` feature; nothing in the product
//! calls it. The output is the sheeter's, before the engine's fill-rate draw.

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
    fn manifold_fluids_oracle_thread_count(count_out: *mut i32) -> i32;
}

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

/// The process-wide FLIP thread count. The sheet oracle pins it to 1 for its
/// call and restores it, under the bridge lock every native entry holds.
pub fn thread_count() -> Result<i32, FluidError> {
    let mut count = 0;
    // SAFETY: a valid out pointer for one int.
    let ok = unsafe { manifold_fluids_oracle_thread_count(&mut count) };
    native_result(ok, "oracle thread count")?;
    Ok(count)
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

    /// The CPU port against the native sheeter, by identity: equal counts,
    /// and each native seed, in order, is bit for bit the projection of
    /// exactly one port candidate, the one the port seeded at that place in
    /// the sequence. Returns the seed count.
    fn port_matches(markers: &[[f32; 3]], phi: &[f32], cells: [u32; 3], dx: f64, threshold: f32) -> usize {
        let native = sheet_particles(markers, phi, cells, dx, threshold).expect("oracle");
        let trace = crate::sheeter::trace_sheet_particles(markers, phi, cells, dx, threshold).expect("port");
        assert_eq!(native.len(), trace.seeds.len(), "seed counts differ");
        let near = |a: &[f32; 3], b: &[f32; 3]| (0..3).all(|c| a[c].to_bits() == b[c].to_bits());
        for (i, seed) in native.iter().enumerate() {
            let sources: Vec<usize> = (0..trace.candidates.len())
                .filter(|&j| trace.projections[j].is_some_and(|p| near(&p, seed)))
                .collect();
            assert_eq!(sources, vec![trace.seed_candidates[i]], "native seed {i} {seed:?} is not the port's seed {i}");
        }
        native.len()
    }

    /// One ulp along `x`, signed.
    fn ulp_step(x: f32, k: i32) -> f32 {
        (0..k.unsigned_abs()).fold(x, |v, _| if k > 0 { v.next_up() } else { v.next_down() })
    }

    /// The two adjacent floats in [lo, hi] where `observe` (the port's
    /// decisions) changes, by bisection; the observable differs at the ends.
    fn knife_edge<T: PartialEq>(mut lo: f32, mut hi: f32, observe: impl Fn(f32) -> T) -> (f32, f32) {
        let low = observe(lo);
        assert!(observe(hi) != low, "the decision does not change over [{lo}, {hi}]");
        loop {
            let mid = 0.5 * (lo + hi);
            if mid == lo || mid == hi {
                return (lo, hi);
            }
            if observe(mid) == low { lo = mid } else { hi = mid }
        }
    }

    /// Native and port agree by identity on the two floats either side of a
    /// decision edge and one ulp beyond each.
    fn agree_across(edge: (f32, f32), fixture: impl Fn(f32) -> (Vec<[f32; 3]>, Vec<f32>, f64, f32)) {
        assert_eq!(edge.0.next_up(), edge.1, "not adjacent");
        for x in [ulp_step(edge.0, -1), edge.0, edge.1, ulp_step(edge.1, 1)] {
            let (markers, phi, dx, threshold) = fixture(x);
            port_matches(&markers, &phi, CELLS, dx, threshold);
        }
    }

    fn shifted_sheet(shift: f32) -> (Vec<[f32; 3]>, Vec<f32>, f64, f32) {
        let (markers, phi, _) = holed_sheet();
        (markers, phi.iter().map(|v| v + shift).collect(), DX, DEFAULT_FILL_THRESHOLD)
    }

    /// Candidates need φ < 0: the shift where the y 7.75 candidates sample 0.
    #[test]
    fn port_matches_oracle_where_candidates_reach_the_surface() {
        let observe = |s| crate::sheeter::trace_sheet_particles(&shifted_sheet(s).0, &shifted_sheet(s).1, CELLS, DX, DEFAULT_FILL_THRESHOLD).unwrap().candidates.len();
        agree_across(knife_edge(0.1, 0.15, observe), shifted_sheet);
    }

    /// Candidates need φ ≥ −dx: the shift where the y 8.25 candidates sample −1.
    #[test]
    fn port_matches_oracle_at_the_candidate_depth() {
        let observe = |s| crate::sheeter::trace_sheet_particles(&shifted_sheet(s).0, &shifted_sheet(s).1, CELLS, DX, DEFAULT_FILL_THRESHOLD).unwrap().candidates.len();
        agree_across(knife_edge(-0.7, -0.55, observe), shifted_sheet);
    }

    /// Markers need −2dx ≤ φ < 2dx: the shift where they sample −2.
    #[test]
    fn port_matches_oracle_at_the_marker_band() {
        let observe = |s| crate::sheeter::trace_sheet_particles(&shifted_sheet(s).0, &shifted_sheet(s).1, CELLS, DX, DEFAULT_FILL_THRESHOLD).unwrap().thin;
        agree_across(knife_edge(-1.8, -1.6, observe), shifted_sheet);
    }

    /// Neighbours count within 2dx, strictly: one ring marker slid toward a
    /// hole candidate until it enters the radius.
    #[test]
    fn port_matches_oracle_at_the_search_radius() {
        let fixture = |t: f32| {
            let (mut markers, phi, _) = holed_sheet();
            let m = markers.iter().position(|p| *p == [9.75, SHEET_Y, 8.25]).expect("ring marker");
            markers[m][0] -= t;
            (markers, phi, DX, DEFAULT_FILL_THRESHOLD)
        };
        let observe = |t| {
            let (m, p, ..) = fixture(t);
            crate::sheeter::trace_sheet_particles(&m, &p, CELLS, DX, DEFAULT_FILL_THRESHOLD).unwrap().mindots
        };
        agree_across(knife_edge(0.0, 0.02, observe), fixture);
    }

    /// Seeds need mindot < threshold, strictly: the threshold at a seed's own
    /// score and one ulp either side.
    #[test]
    fn port_matches_oracle_at_the_fill_threshold() {
        let (markers, phi, _) = holed_sheet();
        let trace = crate::sheeter::trace_sheet_particles(&markers, &phi, CELLS, DX, DEFAULT_FILL_THRESHOLD).unwrap();
        let score = trace.mindots[trace.seed_candidates[0]].expect("a seed has a score");
        let fixture = |t: f32| (markers.clone(), phi.clone(), DX, t);
        agree_across((score.next_down(), score), fixture);
        let at = port_matches(&markers, &phi, CELLS, DX, score);
        let above = port_matches(&markers, &phi, CELLS, DX, score.next_up());
        assert!(at < above, "the seed's own score must reject it ({at} vs {above})");
    }

    /// A projection lands on a half-cell boundary: the marker layer height
    /// where the y 7.75 candidates project to y 8.0.
    #[test]
    fn port_matches_oracle_at_a_half_cell_boundary() {
        let fixture = |y: f32| {
            let (markers, phi, _) = holed_sheet();
            (markers.into_iter().map(|p| [p[0], y, p[2]]).collect::<Vec<_>>(), phi, DX, 0.0f32)
        };
        let observe = |y| {
            let (m, p, ..) = fixture(y);
            crate::sheeter::trace_sheet_particles(&m, &p, CELLS, DX, 0.0)
                .unwrap()
                .seeds
                .iter()
                .map(|s| (s[1] / 0.5).floor() as i32)
                .collect::<Vec<_>>()
        };
        agree_across(knife_edge(8.07, 8.1, observe), fixture);
    }

    /// The holed sheet at non-binary cell sizes: every position and distance
    /// scaled, the lattice's equal-distance ties kept.
    #[test]
    fn port_matches_oracle_at_non_binary_cell_sizes() {
        let (markers, phi, _) = holed_sheet();
        for dx in [0.3f32, 0.1, 0.7] {
            let m: Vec<[f32; 3]> = markers.iter().map(|p| p.map(|c| c * dx)).collect();
            let f: Vec<f32> = phi.iter().map(|v| v * dx).collect();
            let seeds = port_matches(&m, &f, CELLS, f64::from(dx), DEFAULT_FILL_THRESHOLD);
            assert!(seeds > 0, "dx {dx}: the scaled hole seeds");
        }
    }

    /// Oracle calls racing native world construction (which sets the
    /// process-wide thread count to 4) from other threads: each call is
    /// serialised by the bridge lock, gives the same seeds, and leaves the
    /// count as the worlds set it.
    #[test]
    fn oracle_sheet_is_isolated_from_concurrent_native_work() {
        let (markers, phi, _) = holed_sheet();
        let want = sheet_particles(&markers, &phi, CELLS, DX, DEFAULT_FILL_THRESHOLD).expect("oracle");
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..25 {
                        assert_eq!(sheet_particles(&markers, &phi, CELLS, DX, DEFAULT_FILL_THRESHOLD).expect("oracle"), want);
                    }
                });
            }
            for _ in 0..2 {
                scope.spawn(|| {
                    for _ in 0..10 {
                        let world = crate::FluidWorld::new(crate::Config { cells: [8; 3], cell_size: 0.5, surface_subdivisions: 0, apic: false });
                        drop(world.expect("world"));
                    }
                });
            }
        });
        assert_eq!(super::thread_count().expect("count"), 4, "the oracle restored a stale thread count");
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
        let (markers, phi, cells, dx) = crate::sheeter::fixtures::splash();
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
