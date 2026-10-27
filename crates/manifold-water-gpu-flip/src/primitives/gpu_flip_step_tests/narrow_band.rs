//! Narrow Band's passes (`narrow_band.wgsl`) against their CPU rules, on
//! fields held over every cell of the box's tiles.

use super::*;

/// R, the band's depth in cells, as the step sets it.
const BAND: f32 = 12.0;

/// The identity's epoch the proofs run under, and the status word that says
/// the interior was advected under it.
const EPOCH: u32 = 5;

fn band_params(mode: u32) -> StepParams {
    StepParams {
        narrow_band: mode,
        band_cells: BAND,
        tick_index: 1,
        step_dt: 0.1,
        capacity: 64,
        ..lattice()
    }
}

/// Every (world cell, pool index) of the pass's held tiles whose cells
/// within `margin` all lie in held tiles.
fn inner_cells(pass: &Pass, margin: i32) -> Vec<([i32; 3], usize)> {
    let held = pass.table.held();
    let tiles: Vec<[i32; 3]> = held.iter().map(|(t, _)| *t).collect();
    let lo: [i32; 3] = std::array::from_fn(|a| tiles.iter().map(|t| t[a]).min().unwrap() * 8);
    let hi: [i32; 3] = std::array::from_fn(|a| tiles.iter().map(|t| t[a]).max().unwrap() * 8 + 7);
    let mut cells = Vec::new();
    for (tile, slot) in held {
        for local in 0..512i32 {
            let c = [
                tile[0] * 8 + (local & 7),
                tile[1] * 8 + ((local >> 3) & 7),
                tile[2] * 8 + (local >> 6),
            ];
            if (0..3).all(|a| c[a] - margin >= lo[a] && c[a] + margin <= hi[a]) {
                cells.push((c, slot as usize * 512 + local as usize));
            }
        }
    }
    cells
}

fn identity() -> Vec<u32> {
    let mut words = vec![0u32; 16];
    words[1] = EPOCH;
    words
}

/// A plane distance through the box, rising along y and x, at world
/// position x in metres.
fn plane(x: [f64; 3]) -> f64 {
    x[1] - 0.2 + 0.3 * x[0]
}

fn centre(c: [i32; 3]) -> [f64; 3] {
    c.map(|v| f64::from(v) * f64::from(H))
}

/// A uniform flow carries a plane distance by v·dt exactly (RK4 and the
/// trilinear reads are exact on it): the interior is the plane at the
/// backtrace, the status names the identity's epoch, the advected faces are
/// the flow where the new interior holds water and absent above, and the
/// union takes the interior shrunk by a cell where it is below the
/// particles' distance.
#[test]
fn gpu_flip_band_advects_unites_and_keeps_its_faces() {
    let mut pass = Pass::new();
    let params = band_params(1);
    let v = [0.5_f32, -0.75, 0.25];
    let dt = f64::from(params.step_dt);
    let distance = pass.pooled_everywhere(3.0 * H, |c| plane(centre(c)) as f32);
    let flow = pass.pooled_everywhere(FaceSample::default(), |_| FaceSample {
        velocity: [v[0], v[1], v[2], 0.0],
        weight: [1.0, 1.0, 1.0, 0.0],
    });
    let len = distance.len();
    pass.bind(67, &distance, Layout::Dense)
        .bind(3, &flow, Layout::Dense)
        .bind(47, &identity(), Layout::Dense)
        .bind(69, &[0u32; 4], Layout::Dense)
        .bind(65, &vec![0.0f32; len], Layout::Dense)
        .bind(66, &vec![FaceSample::default(); len], Layout::Dense);
    let grid: Vec<f32> = pass.run_list("nb_advect_phi", &params, 65, len);
    let shift = f64::from(v[1]) + 0.3 * f64::from(v[0]);
    let inner = inner_cells(&pass, 2);
    assert!(!inner.is_empty());
    for &(c, idx) in &inner {
        let want = plane(centre(c)) - shift * dt;
        close(grid[idx], want, 1.0, &format!("interior at {c:?}"));
    }
    assert_eq!(
        pass.bound::<u32>(69, 4)[0],
        EPOCH + 1,
        "the status names the epoch"
    );
    let faces: Vec<FaceSample> = pass.run_list("nb_advect_faces", &params, 66, len);
    for &(c, idx) in &inner {
        for a in 0..3 {
            let mut x = centre(c);
            x[a] -= 0.5 * f64::from(H);
            let water = plane(x) - shift * dt < 0.0;
            let face = faces[idx];
            assert_eq!(
                face.weight[a],
                if water { 1.0 } else { 0.0 },
                "weight {a} at {c:?}"
            );
            let want = if water { f64::from(v[a]) } else { 0.0 };
            close(
                face.velocity[a],
                want,
                1.0,
                &format!("velocity {a} at {c:?}"),
            );
        }
    }
    // The particles' distance a constant; the union is the interior shrunk
    // by a cell wherever that is lower.
    let particles = vec![0.1f32; len];
    pass.bind(5, &particles, Layout::Dense);
    let united: Vec<f32> = pass.run_list("nb_union", &params, 5, len);
    for &(c, idx) in &inner {
        let want = (f64::from(grid[idx]) + f64::from(H)).min(0.1);
        close(united[idx], want, 1.0, &format!("union at {c:?}"));
    }
}

/// The faces deeper than r (R - 1 cells) take the grid's, and so does every
/// face no particle reached; the rest keep the gathered faces. Before the
/// interior was advected under this epoch nothing changes.
#[test]
fn gpu_flip_band_combines_deep_faces_with_the_grid() {
    let mut pass = Pass::on([4, 28, 4]);
    let params = band_params(1);
    let surface = 22.0 * f64::from(H);
    let distance = pass.pooled_everywhere(3.0 * H, |c| (centre(c)[1] - surface) as f32);
    let gathered = pass.pooled_everywhere(FaceSample::default(), |c| {
        let weight = if c[0].rem_euclid(2) == 0 { 1.0 } else { 0.0 };
        FaceSample {
            velocity: [1.0 + c[1] as f32; 4],
            weight: [weight, weight, weight, 0.0],
        }
    });
    let grid = pass.pooled_everywhere(FaceSample::default(), |_| FaceSample {
        velocity: [-2.0; 4],
        weight: [0.5; 4],
    });
    let corners = pass.pooled_everywhere(1.0e30f32, |_| 1.0e30);
    let len = distance.len();
    pass.bind(67, &distance, Layout::Dense)
        .bind(4, &gathered, Layout::Dense)
        .bind(66, &grid, Layout::Dense)
        .bind(9, &corners, Layout::Dense)
        .bind(47, &identity(), Layout::Dense)
        .bind(69, &[0u32; 4], Layout::Dense);
    let untouched: Vec<FaceSample> = pass.run_list("nb_combine", &params, 4, len);
    assert_eq!(
        bytemuck::cast_slice::<FaceSample, u32>(&untouched),
        bytemuck::cast_slice::<FaceSample, u32>(&gathered),
        "an interior from another epoch is never read"
    );
    pass.bind(69, &[EPOCH + 1, 0, 0, 0], Layout::Dense);
    let combined: Vec<FaceSample> = pass.run_list("nb_combine", &params, 4, len);
    let r = f64::from(BAND - 1.0) * f64::from(H);
    let mut deep = 0;
    for &(c, idx) in &inner_cells(&pass, 1) {
        for a in 0..3 {
            let mut y = centre(c)[1];
            if a == 1 {
                y -= 0.5 * f64::from(H);
            }
            let take_grid = y - surface < -r || gathered[idx].weight[a] == 0.0;
            deep += usize::from(y - surface < -r);
            let want = if take_grid { grid[idx] } else { gathered[idx] };
            let got = combined[idx];
            assert_eq!(
                (got.velocity[a], got.weight[a]),
                (want.velocity[a], want.weight[a]),
                "face {a} at {c:?}"
            );
        }
    }
    assert!(deep > 0, "the box reaches below r");
}

/// A cell entering the band (deep by the substep's distance, in the band
/// by the advected interior) reseeds its free octants up to eight particles;
/// a cell that was not deep reseeds none until the restore, which refills
/// every water cell a cell deep.
#[test]
fn gpu_flip_band_reseeds_cells_entering_the_band() {
    let mut pass = Pass::new();
    let h = f64::from(H);
    let origin = box_origin(MIN, H);
    let world = |c: [usize; 3]| -> [i32; 3] { std::array::from_fn(|a| origin[a] + c[a] as i32) };
    let crowded = [1usize, 1, 1];
    let shallow = [3usize, 2, 1];
    let interior = pass.pooled_everywhere(3.0 * H, |_| (-2.0 * h) as f32);
    let distance = pass.pooled_everywhere(3.0 * H, |c| {
        if c == world(shallow) {
            (-5.0 * h) as f32
        } else {
            (-20.0 * h) as f32
        }
    });
    let corners = pass.pooled_everywhere(1.0e30f32, |_| 1.0e30);
    // Three particles of the crowded cell in octants 0, 3 and 5.
    let p = world(crowded);
    let particles: Vec<FluidParticle> = [0u32, 3, 5]
        .iter()
        .map(|&k| {
            let q = [k & 1, (k >> 1) & 1, (k >> 2) & 1].map(|b| 0.25 + 0.5 * b as f64);
            let x: [f32; 3] =
                std::array::from_fn(|a| ((f64::from(p[a]) + q[a]) * h - 0.5 * h) as f32);
            FluidParticle {
                position_radius: [x[0], x[1], x[2], 0.08],
                ..FluidParticle::default()
            }
        })
        .collect();
    let mut ranges = vec![CellRange::default(); cell_len()];
    ranges[cell_index(crowded)] = CellRange { start: 0, count: 3 };
    let len = interior.len();
    pass.bind(65, &interior, Layout::Dense)
        .bind(67, &distance, Layout::Dense)
        .bind(9, &corners, Layout::Dense)
        .bind(38, &particles, Layout::Dense)
        .bind_ranges(&ranges)
        .bind(70, &vec![0u32; len], Layout::Dense);
    for (mode, shallow_sites) in [(1, 0), (2, 8)] {
        let counts: Vec<u32> = pass.run_list("nb_reseed_flags", &band_params(mode), 70, len);
        for c in 0..cell_len() {
            let at = cell_coords(c);
            let want = if at == crowded {
                5
            } else if at == shallow {
                shallow_sites
            } else {
                8
            };
            let idx = pass.table.pool_index(at.map(|v| v as u32));
            assert_eq!(counts[idx], want, "mode {mode}: cell {at:?}");
        }
    }
}

/// After the move, a particle the interior holds deeper than R is removed;
/// one in the band or beside a solid stays.
#[test]
fn gpu_flip_band_removes_particles_below_the_band() {
    // World cells -3 to 20 on x: room for a particle more than R from the
    // wall at x = -2 cells.
    let mut pass = Pass::on([24, 28, 4]);
    let h = f64::from(H);
    let surface = 22.0 * h;
    let interior = pass.pooled_everywhere(3.0 * H, |c| (centre(c)[1] - surface) as f32);
    let wall = -2.0 * h;
    let corners =
        pass.pooled_everywhere(1.0e30f32, |c| (f64::from(c[0]) * h - 0.5 * h - wall) as f32);
    // A particle at world cell x `cell`, `depth` cells under the surface.
    let at = |cell: i32, depth: f64| -> FluidParticle {
        let x = [f64::from(cell) * h, surface - depth * h, h];
        FluidParticle {
            position_radius: [x[0] as f32, x[1] as f32, x[2] as f32, 0.08],
            ..FluidParticle::default()
        }
    };
    // 18 cells from the wall: in the band, just past it, deep. Two cells from
    // the wall, deep.
    let particles = vec![
        at(16, 4.0),
        at(16, f64::from(BAND) + 1.5),
        at(16, 18.0),
        at(0, 18.0),
    ];
    pass.bind(65, &interior, Layout::Dense)
        .bind(9, &corners, Layout::Dense)
        .bind(38, &particles, Layout::Dense);
    let n = particles.len();
    let out: Vec<FluidParticle> = pass.run("nb_delete", &band_params(1), 38, n, n);
    let kept: Vec<bool> = out.iter().map(|p| p.position_radius[3] > 0.0).collect();
    let solid: Vec<f64> = particles
        .iter()
        .map(|p| (f64::from(p.position_radius[0]) - wall) / h)
        .collect();
    for (i, alive) in kept.iter().enumerate() {
        let depth = surface - f64::from(particles[i].position_radius[1]);
        let removed = depth > f64::from(BAND) * h && solid[i] > f64::from(BAND);
        assert_eq!(
            *alive, !removed,
            "particle {i}: depth {depth}, solid {} cells",
            solid[i]
        );
    }
    assert_eq!(
        kept,
        [true, false, false, true],
        "the band, past it, deep, beside the wall"
    );
}
