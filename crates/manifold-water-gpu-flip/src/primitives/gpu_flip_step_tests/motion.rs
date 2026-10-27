//! Particle motion: sampling, the FLIP blend, RK3 and collisions.

use super::*;

#[test]
pub(super) fn native_wall_oracle_covers_margin_crossing_tangent_and_grid_escape() {
    let n = N;
    let h = f64::from(H);
    let e = boundary_edge();
    let y = 2.5;
    let stationary = native_wall_move([0.15, y, y], [0.15, y, y], n, h);
    assert_eq!(
        stationary,
        [0.15, y, y],
        "stationary point between .1h and .2h is unchanged"
    );
    let approach = native_wall_move([0.15, y, y], [0.05, y, y], n, h);
    assert!(
        (approach[0] - 0.2).abs() < 1e-12,
        "wall approach pushes to .2h: {approach:?}"
    );
    let tangent = native_wall_move([0.15, y, y], [0.15, y + 0.4, y], n, h);
    assert_eq!(tangent[0], 0.15, "tangent move keeps its wall distance");
    let upper = native_wall_move([n[0] as f64 - 0.15, y, y], [n[0] as f64 - 0.05, y, y], n, h);
    assert!(
        (upper[0] - (n[0] as f64 - 0.2)).abs() < 1e-12,
        "upper wall push: {upper:?}"
    );
    let escaped = native_wall_move([2.0, y, y], [-2.0, y, y], n, h);
    assert!(
        (escaped[0] - e).abs() < 1e-12,
        "grid escape clamps to the inclusive lower safety edge: {escaped:?}"
    );
    let upper_escape = native_wall_move([n[0] as f64 - 2.0, y, y], [n[0] as f64 + 2.0, y, y], n, h);
    let upper_bound = n[0] as f64 - e - MOVE_EPS_METRES / h;
    assert!(
        (upper_escape[0] - upper_bound).abs() < 1e-12,
        "upper grid escape uses the exclusive AABB epsilon: {upper_escape:?}"
    );
    let tiny = native_wall_move([0.05, y, y], [0.050002, y, y], n, h);
    assert_eq!(
        tiny,
        [0.050002, y, y],
        "native movement epsilon is in metres"
    );
    let fallback = native_collision_move(
        [0.05, y, y],
        [0.05, y + 0.1, y],
        n,
        h,
        |_| 1.0,
        |_| [0.0; 3],
    );
    assert_eq!(
        fallback,
        [e, y, y],
        "zero-gradient fallback still receives the boundary clamp"
    );
}

/// Native MACVelocityField's ordinary per-component trilinear sample, in f64.
/// q is in cells from the lattice minimum. Missing corners contribute zero;
/// face validity does not renormalise the interpolation.
pub(super) fn cpu_sample(q: [f64; 3], field: &[FaceSample]) -> [f64; 3] {
    cpu_sample_on(q, field, N)
}

pub(crate) fn cpu_sample_on(q: [f64; 3], field: &[FaceSample], n: [usize; 3]) -> [f64; 3] {
    if q.iter().any(|value| !value.is_finite() || *value < 0.0)
        || q.iter()
            .zip(n.iter())
            .any(|(value, &extent)| *value >= extent as f64)
    {
        return [0.0; 3];
    }
    std::array::from_fn(|a| {
        let top: [i64; 3] =
            std::array::from_fn(|b| if b == a { n[b] as i64 } else { n[b] as i64 - 1 });
        let s: [f64; 3] = std::array::from_fn(|b| q[b] - if b == a { 0.0 } else { 0.5 });
        let base: [i64; 3] = std::array::from_fn(|b| s[b].floor() as i64);
        let t: [f64; 3] = std::array::from_fn(|b| s[b] - base[b] as f64);
        let mut sum = 0.0;
        for corner in 0..8 {
            let bit = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
            let c: [i64; 3] = std::array::from_fn(|b| base[b] + bit[b] as i64);
            if (0..3).all(|b| c[b] >= 0 && c[b] <= top[b]) {
                let c: [usize; 3] = c.map(|value| value as usize);
                let face = field[c[0] + (n[0] + 1) * (c[1] + (n[1] + 1) * c[2])];
                let w: f64 = (0..3)
                    .map(|b| if bit[b] == 1 { t[b] } else { 1.0 - t[b] })
                    .product();
                sum += w * f64::from(face.velocity[a]);
            }
        }
        sum
    })
}

#[test]
pub(super) fn cpu_sample_matches_native_mac_interpolation_and_bounds() {
    // Constant stored values include invalid faces deliberately: native
    // interpolation reads stored values and only fades missing lattice
    // corners, independent of the validity weights.
    let constant = vec![
        FaceSample {
            velocity: [2.0, 3.0, 5.0, 0.0],
            weight: [0.0; 4]
        };
        face_len()
    ];
    let check = |q: [f64; 3], want: [f64; 3], label: &str| {
        let got = cpu_sample(q, &constant);
        for a in 0..3 {
            assert!(
                (got[a] - want[a]).abs() < 1e-12,
                "{label} component {a}: {} vs {}",
                got[a],
                want[a]
            );
        }
    };
    check([2.0, 2.0, 2.0], [2.0, 3.0, 5.0], "constant interior");
    check(
        [0.25, 0.25, 0.25],
        [1.125, 1.6875, 2.8125],
        "lower corner fade",
    );
    check(
        [5.75, 4.75, 3.75],
        [1.125, 1.6875, 2.8125],
        "upper corner fade",
    );
    check([0.0, 0.0, 0.0], [0.5, 0.75, 1.25], "lower inclusive corner");
    check([-f64::EPSILON, 1.0, 1.0], [0.0; 3], "below grid");
    check([N[0] as f64, 1.0, 1.0], [0.0; 3], "exclusive high edge");

    // An affine field is reproduced exactly at an interior point by each
    // component's face lattice, including its half-cell transverse offsets.
    let mut affine = vec![FaceSample::default(); face_len()];
    for (i, face) in affine.iter_mut().enumerate() {
        let c = pad_coords(i).map(|value| value as f32);
        for a in 0..3 {
            face.velocity[a] = 10.0 * (a as f32 + 1.0) + c[0] + 2.0 * c[1] + 3.0 * c[2];
        }
    }
    let q = [2.25, 2.75, 1.5];
    let got = cpu_sample(q, &affine);
    for (a, got) in got.into_iter().enumerate() {
        let expected = 10.0 * (a as f64 + 1.0)
            + (q[0] - f64::from((a != 0) as u8) * 0.5)
            + 2.0 * (q[1] - f64::from((a != 1) as u8) * 0.5)
            + 3.0 * (q[2] - f64::from((a != 2) as u8) * 0.5);
        assert!(
            (got - expected).abs() < 1e-5,
            "affine component {a}: {got} vs {expected}"
        );
    }
}

#[test]
pub(super) fn gpu_flip_faces_to_particles_reads_native_stored_face_values() {
    // Exact binary positions from MIN (whole eighths) exercise all three
    // exclusive high edges without a world-coordinate rounding ambiguity.
    let points = [
        [2.0, 2.0, 2.0],
        [0.25, 0.25, 0.25],
        [5.75, 4.75, 3.75],
        [0.0, 0.0, 0.0],
        [-0.125, 1.0, 1.0],
        [6.0, 1.0, 1.0],
        [1.0, 5.0, 1.0],
        [1.0, 1.0, 4.0],
        [2.25, 2.75, 1.5],
    ];
    let particles: Vec<_> = points
        .iter()
        .enumerate()
        .map(|(i, q)| FluidParticle {
            position_radius: [
                MIN[0] + q[0] as f32 * H,
                MIN[1] + q[1] as f32 * H,
                MIN[2] + q[2] as f32 * H,
                0.08,
            ],
            velocity: [0.0; 3],
            id: i as u32 + 1,
        })
        .collect();
    // Include nonzero data in non-existent packed component lanes. Correct
    // component bounds must discard those lanes, regardless of their weight.
    let stored = vec![
        FaceSample {
            velocity: [2.0, 3.0, 5.0, 0.0],
            weight: [0.0; 4]
        };
        face_len()
    ];
    let mut affine = stored.clone();
    for (i, face) in affine.iter_mut().enumerate() {
        let c = pad_coords(i);
        for a in 0..3 {
            face.velocity[a] = (10 * (a + 1) + c[0] + 2 * c[1] + 3 * c[2]) as f32;
        }
    }
    let zero = vec![FaceSample::default(); face_len()];
    let solid = wall_solid(N, H);
    let run = |flip: f32, faces: &[FaceSample], old: &[FaceSample]| {
        Pass::new()
            .bind(2, &particles, Layout::Dense)
            .bind(3, faces, Layout::Records)
            .bind(9, &solid, Layout::Records)
            .bind(15, &[LiquidShape::default()], Layout::Dense)
            .bind(16, &[0_u32; 4], Layout::Dense)
            .bind(17, old, Layout::Records)
            .bind(18, faces, Layout::Records)
            .bind(22, &vec![0_u32; 2 * particles.len()], Layout::Dense)
            .bind(36, &[LiquidBody::default()], Layout::Dense)
            .run::<FluidParticle>(
                "faces_to_particles",
                &StepParams {
                    step_dt: 0.0,
                    flip,
                    particles: particles.len() as u32,
                    ..lattice()
                },
                19,
                particles.len(),
                particles.len(),
            )
    };
    for field in [&stored, &affine] {
        let fresh = run(0.0, field, &zero);
        let old_grid = run(1.0, &zero, field);
        for (i, q) in points.into_iter().enumerate() {
            let want = cpu_sample(q, field);
            for (a, want) in want.into_iter().enumerate() {
                close(fresh[i].velocity[a], want, 10.0, "new-grid stored sample");
                close(
                    old_grid[i].velocity[a],
                    -want,
                    10.0,
                    "old-grid stored sample",
                );
            }
            assert_eq!(fresh[i].id, particles[i].id);
        }
        assert_eq!(
            fresh[0].position_radius, particles[0].position_radius,
            "dt=0 keeps the interior position"
        );
        assert_eq!(
            old_grid[0].position_radius, particles[0].position_radius,
            "dt=0 keeps the old-grid interior position"
        );
    }
}

#[test]
pub(super) fn gpu_flip_faces_to_particles_blends_flip_and_moves_by_rk3() {
    let faces = random_faces(0xf1a5, true);
    let old = random_faces(0x01d5, false);
    let mut particles = random_particles(0x2b3, 300);
    // Keep the random draw away from wall corners so the f64 wall oracle
    // compares the same one-axis lattice gradient as the shader.
    for particle in &mut particles {
        for a in 0..3 {
            let q = ((particle.position_radius[a] - MIN[a]) / H).clamp(1.0, N[a] as f32 - 1.0);
            particle.position_radius[a] = MIN[a] + q * H;
        }
    }
    // Two particles against the walls, so the native march is exercised.
    particles[0].position_radius[..3].copy_from_slice(&[
        MIN[0] + 0.15 * H,
        MIN[1] + 2.5 * H,
        MIN[2] + 2.5 * H,
    ]);
    particles[1].position_radius[..3].copy_from_slice(&[
        MIN[0] + 2.5 * H,
        MIN[1] + (N[1] as f32 - 0.15) * H,
        MIN[2] + 2.5 * H,
    ]);
    // Two broken particles: the move must leave them non-finite, never clamp
    // or zero them, so the tick's stats see them.
    particles[2].position_radius[0] = f32::NAN;
    particles[3].velocity[1] = f32::INFINITY;
    let (dt, flip) = (0.07f32, 0.9f32);
    let step = StepParams {
        step_dt: dt,
        flip,
        particles: particles.len() as u32,
        ..lattice()
    };
    let solid = wall_solid(N, H);
    let mut pass = Pass::new();
    let got: Vec<FluidParticle> = pass
        .bind(2, &particles, Layout::Dense)
        .bind(3, &faces, Layout::Records)
        // Metal requires the conditional solid/region resources even with zero counts.
        .bind(9, &solid, Layout::Records)
        .bind(15, &[LiquidShape::default()], Layout::Dense)
        .bind(16, &[0_u32; 4], Layout::Dense)
        .bind(36, &[LiquidBody::default()], Layout::Dense)
        .bind(17, &old, Layout::Records)
        // Spread equal to the new faces: no density move.
        .bind(18, &faces, Layout::Records)
        .bind(22, &vec![7u32; 2 * particles.len()], Layout::Dense)
        .run(
            "faces_to_particles",
            &step,
            19,
            particles.len(),
            particles.len(),
        );
    // Step 0 of the tick starts the counts over the stale 7s.
    let capped: Vec<u32> = pass.bound(22, 2 * particles.len());
    let per_cell = f64::from(dt) / f64::from(H);
    for (i, (g, p)) in got.iter().zip(&particles).enumerate() {
        if p.position_radius[3] <= 0.0 {
            assert_eq!(g, p, "unused slot {i} passes through");
            continue;
        }
        if i == 2 {
            assert!(
                !g.position_radius[0].is_finite(),
                "a non-finite position stays non-finite: {:?}",
                g.position_radius
            );
            continue;
        }
        if i == 3 {
            assert!(
                !g.velocity[1].is_finite(),
                "a non-finite velocity stays non-finite: {:?}",
                g.velocity
            );
            continue;
        }
        let q0: [f64; 3] = std::array::from_fn(|a| {
            (f64::from(p.position_radius[a]) - f64::from(MIN[a])) / f64::from(H)
        });
        let after = cpu_sample(q0, &faces);
        let k1 = after;
        let k2 = cpu_sample(
            std::array::from_fn(|a| q0[a] + 0.5 * per_cell * k1[a]),
            &faces,
        );
        let k3 = cpu_sample(
            std::array::from_fn(|a| q0[a] + 0.75 * per_cell * k2[a]),
            &faces,
        );
        let before = cpu_sample(q0, &old);
        assert_eq!(
            capped[2 * i],
            0,
            "particle {i}: native RK3 does not cap stage velocities"
        );
        assert_eq!(
            capped[2 * i + 1],
            0,
            "particle {i}: no bodies, no refused push"
        );
        let reached: [f64; 3] = std::array::from_fn(|a| {
            q0[a] + per_cell * (2.0 * k1[a] + 3.0 * k2[a] + 4.0 * k3[a]) / 9.0
        });
        let q1 = native_wall_move(q0, reached, N, f64::from(H));
        for a in 0..3 {
            let position = f64::from(MIN[a]) + q1[a] * f64::from(H);
            let velocity = f64::from(flip) * (f64::from(p.velocity[a]) + after[a] - before[a])
                + (1.0 - f64::from(flip)) * after[a];
            assert!(
                (f64::from(g.position_radius[a]) - position).abs() < 2e-5,
                "particle {i} position {a}: {} vs {position}",
                g.position_radius[a]
            );
            assert!(
                (f64::from(g.velocity[a]) - velocity).abs() < 1e-4,
                "particle {i} velocity {a}: {} vs {velocity}",
                g.velocity[a]
            );
        }
        assert_eq!(
            (g.position_radius[3], g.id),
            (p.position_radius[3], p.id),
            "particle {i} keeps radius and id"
        );
    }
}

/// A low pre-step marker speed does not bound the post-force grid. Exercise
/// both a uniformly high field and a field whose later RK3 stages accelerate.
#[test]
pub(super) fn gpu_flip_rk3_stage_velocity_is_not_truncated() {
    let n = [96usize, 8, 8];
    let m = n.map(|v| v + 1);
    let len = m.iter().product();
    let q0 = [32.0, 4.0, 4.0];
    let dt = 0.01f32;
    let per_cell = f64::from(dt) / f64::from(H);
    // The former default Top Speed of 20 m/s rounded this dt/h to one cell.
    let old_limit = 1.0 / per_cell;
    for (name, initial, slope) in [("uniform", 600.0, 0.0), ("later stages", 20.0, 20.0)] {
        let faces: Vec<_> = (0..len)
            .map(|i| FaceSample {
                velocity: [
                    initial + slope * (i % m[0]) as f32 - slope * q0[0] as f32,
                    0.0,
                    0.0,
                    0.0,
                ],
                weight: [1.0; 4],
            })
            .collect();
        let particle = FluidParticle {
            position_radius: [
                MIN[0] + q0[0] as f32 * H,
                MIN[1] + q0[1] as f32 * H,
                MIN[2] + q0[2] as f32 * H,
                0.08,
            ],
            velocity: [0.0; 3],
            id: 17,
        };
        let k1 = cpu_sample_on(q0, &faces, n);
        let k2 = cpu_sample_on(
            std::array::from_fn(|a| q0[a] + 0.5 * per_cell * k1[a]),
            &faces,
            n,
        );
        let k3 = cpu_sample_on(
            std::array::from_fn(|a| q0[a] + 0.75 * per_cell * k2[a]),
            &faces,
            n,
        );
        assert!(k3[0] > old_limit, "{name} exceeds the former stage limiter");
        if slope != 0.0 {
            assert!(k1[0] < old_limit && k2[0] > old_limit);
        }
        let want: [f64; 3] = std::array::from_fn(|a| {
            q0[a] + per_cell * (2.0 * k1[a] + 3.0 * k2[a] + 4.0 * k3[a]) / 9.0
        });
        assert!(
            want[0] - q0[0] > 1.0,
            "{name} distinguishes the former one-cell stage cap"
        );
        assert!(
            want[0] < n[0] as f64 - 2.0,
            "the proof remains clear of collisions"
        );
        for live in [false, true] {
            let mut clock = [0u32; 12];
            clock[0] = dt.to_bits();
            clock[3] = 0.0f32.to_bits(); // Pre-step marker speed before the high grid field.
            clock[11] = u32::from(live);
            let mut pass = Pass::on(n);
            let got: Vec<FluidParticle> = pass
                .bind(2, &[particle], Layout::Dense)
                .bind(3, &faces, Layout::Records)
                .bind(9, &wall_solid(n, H), Layout::Records)
                .bind(15, &[LiquidShape::default()], Layout::Dense)
                .bind(16, &[0_u32; 4], Layout::Dense)
                .bind(17, &faces, Layout::Records)
                .bind(18, &faces, Layout::Records)
                .bind(22, &[91u32; 2], Layout::Dense)
                .bind(36, &[LiquidBody::default()], Layout::Dense)
                .bind(46, &clock, Layout::Dense)
                .run(
                    "faces_to_particles",
                    &StepParams {
                        step_dt: dt,
                        particles: 1,
                        ..lattice()
                    }
                    .with_grid_for_test(n.map(|v| v as u32), MIN, H),
                    19,
                    1,
                    1,
                );
            for a in 0..3 {
                let actual =
                    (f64::from(got[0].position_radius[a]) - f64::from(MIN[a])) / f64::from(H);
                assert!(
                    (actual - want[a]).abs() < 2e-5,
                    "{name}, live={live}, axis {a}: {actual} != {want:?}"
                );
            }
            assert_eq!(
                pass.bound::<u32>(22, 2),
                [0, 0],
                "{name}: no stage cap or refused push"
            );
            assert_eq!(
                (got[0].id, got[0].position_radius[3]),
                (particle.id, particle.position_radius[3])
            );
        }
    }
}

#[test]
pub(super) fn gpu_flip_marker_motion_matches_native_wall_sequence_without_bodies() {
    let y = 2.5_f64;
    let cases = [
        ("stationary", [0.15, y, y], [0.0, 0.0, 0.0], 0.01_f32, false),
        ("approach", [0.15, y, y], [-1.0, 0.0, 0.0], 0.01, false),
        ("crossing", [0.15, y, y], [-10.0, 0.0, 0.0], 0.04, false),
        ("tangent", [0.15, y, y], [0.0, 1.0, 0.0], 0.10, false),
        (
            "upper",
            [N[0] as f64 - 0.15, y, y],
            [1.0, 0.0, 0.0],
            0.01,
            false,
        ),
        ("grid escape", [2.0, y, y], [-20.0, 0.0, 0.0], 0.10, false),
        (
            "upper grid escape",
            [N[0] as f64 - 2.0, y, y],
            [20.0, 0.0, 0.0],
            0.10,
            false,
        ),
        (
            "world epsilon",
            [0.05, y, y],
            [0.00005, 0.0, 0.0],
            0.01,
            false,
        ),
        ("fallback clamp", [0.05, y, y], [0.0, 1.0, 0.0], 0.025, true),
        ("nonfinite", [f64::NAN, y, y], [0.0; 3], 0.01, false),
    ];
    let entry = "faces_to_particles";
    for (name, q0, velocity, dt, flat) in cases {
        let solid = if flat {
            vec![H; face_len()]
        } else {
            wall_solid(N, H)
        };
        let particles = [FluidParticle {
            position_radius: [
                MIN[0] + q0[0] as f32 * H,
                MIN[1] + q0[1] as f32 * H,
                MIN[2] + q0[2] as f32 * H,
                0.08,
            ],
            velocity: [0.0; 3],
            id: 1,
        }];
        // The RK3 marker mover must read stored solid velocities even when
        // their validity flag is zero.
        let faces = vec![
            FaceSample {
                velocity: [
                    velocity[0] as f32,
                    velocity[1] as f32,
                    velocity[2] as f32,
                    0.0
                ],
                weight: [0.0; 4]
            };
            face_len()
        ];
        let step = StepParams {
            step_dt: dt,
            particles: 1,
            ..lattice()
        };
        let got: Vec<FluidParticle> = Pass::new()
            .bind(2, &particles, Layout::Dense)
            .bind(3, &faces, Layout::Records)
            .bind(9, &solid, Layout::Records)
            .bind(15, &[LiquidShape::default()], Layout::Dense)
            .bind(16, &[0_u32; 4], Layout::Dense)
            .bind(17, &faces, Layout::Records)
            .bind(18, &faces, Layout::Records)
            .bind(22, &[0_u32; 2], Layout::Dense)
            .bind(36, &[LiquidBody::default()], Layout::Dense)
            .run(entry, &step, 19, 1, 1);
        if name == "nonfinite" {
            assert!(
                !got[0].position_radius[0].is_finite(),
                "{entry}: invalid position remains visible"
            );
            assert_eq!(
                (got[0].position_radius[3], got[0].id),
                (particles[0].position_radius[3], particles[0].id)
            );
            continue;
        }
        // Sampling changes near the current grid edges, so even a constant
        // stored field is not a constant particle velocity there. Compute the
        // actual integrator stages before applying the independent collision
        // oracle; these fixtures keep every stage inside the travel guard.
        let per_cell = f64::from(dt) / f64::from(H);
        let at = |fraction: f64, v: [f64; 3]| {
            cpu_sample(
                std::array::from_fn(|a| q0[a] + fraction * per_cell * v[a]),
                &faces,
            )
        };
        let k1 = cpu_sample(q0, &faces);
        let k2 = at(0.5, k1);
        let k3 = at(0.75, k2);
        let reached: [f64; 3] = std::array::from_fn(|a| {
            q0[a] + per_cell * (2.0 * k1[a] + 3.0 * k2[a] + 4.0 * k3[a]) / 9.0
        });
        if name.contains("grid escape") {
            assert!(
                reached[0] < 0.0 || reached[0] >= N[0] as f64,
                "{entry}/{name}: fixture reaches outside the grid: {reached:?}"
            );
        }
        let want = if flat {
            native_collision_move(q0, reached, N, f64::from(H), |_| 1.0, |_| [0.0; 3])
        } else {
            native_wall_move(q0, reached, N, f64::from(H))
        };
        let actual: [f64; 3] = std::array::from_fn(|a| {
            (f64::from(got[0].position_radius[a]) - f64::from(MIN[a])) / f64::from(H)
        });
        for a in 0..3 {
            assert!(
                (actual[a] - want[a]).abs() < 2e-5,
                "{entry}/{name} axis {a}: {actual:?} vs {want:?}"
            );
        }
        assert!(got[0].position_radius[3] > 0.0, "{name} remains live");
    }
}

/// D19: a particle under a body is never removed; only an open face's band
/// removes water. The first sits at the centre of a solid block, where the
/// distance has no gradient to climb, so it stays and counts as refused.
#[test]
pub(super) fn gpu_flip_collision_keeps_negative_body_phi_and_removes_open_band_particles() {
    let mut solid = wall_solid(N, H);
    let m = N.map(|n| n + 1);
    for z in 2..=3 {
        for y in 2..=3 {
            for x in 2..=3 {
                solid[x + m[0] * (y + m[1] * z)] = -H;
            }
        }
    }
    let particles = [
        FluidParticle {
            position_radius: [MIN[0] + 2.5 * H, MIN[1] + 2.5 * H, MIN[2] + 2.5 * H, 0.08],
            id: 1,
            ..FluidParticle::default()
        },
        FluidParticle {
            position_radius: [MIN[0] + 0.5 * H, MIN[1] + 2.5 * H, MIN[2] + 2.5 * H, 0.08],
            id: 2,
            ..FluidParticle::default()
        },
    ];
    let faces = vec![FaceSample::default(); face_len()];
    let entry = "faces_to_particles";
    for closed_faces in [62, 63] {
        let step = StepParams {
            body_count: 1,
            rows: 1,
            region_count: 0,
            closed_faces,
            particles: 2,
            ..lattice()
        };
        let mut pass = Pass::new();
        let got: Vec<FluidParticle> = pass
            .bind(2, &particles, Layout::Dense)
            .bind(3, &faces, Layout::Records)
            .bind(9, &solid, Layout::Records)
            .bind(15, &[LiquidShape::default()], Layout::Dense)
            .bind(16, &[0_u32; 4], Layout::Dense)
            .bind(17, &faces, Layout::Records)
            .bind(18, &faces, Layout::Records)
            .bind(22, &[0_u32; 4], Layout::Dense)
            .bind(36, &[LiquidBody::default()], Layout::Dense)
            .run(entry, &step, 19, particles.len(), particles.len());
        assert_eq!(
            got[0].position_radius, particles[0].position_radius,
            "{entry}: the particle under the body stays, live"
        );
        assert_eq!(
            pass.bound::<u32>(22, 2)[1],
            1,
            "{entry}: its push-out is counted as refused"
        );
        assert_eq!(
            got[1].position_radius[3],
            if closed_faces == 62 { 0.0 } else { 0.08 },
            "only the open low-X band removes the second particle"
        );
    }
}

#[test]
pub(super) fn gpu_flip_collision_refuses_excessive_push_and_keeps_the_particle() {
    // A plane solid whose exit is beyond the native five-cell push limit.
    // The last point also lies outside the safety AABB; its attempted clamp
    // stays inside this solid, so the final native fallback is the old point.
    // D19: the particle is kept there, live, and counted as refused.
    let solid: Vec<f32> = (0..face_len())
        .map(|i| (pad_coords(i)[0] as f32 - 10.0) * H)
        .collect();
    let particle = FluidParticle {
        position_radius: [MIN[0] + 0.05 * H, MIN[1] + 2.5 * H, MIN[2] + 2.5 * H, 0.08],
        id: 1,
        ..FluidParticle::default()
    };
    let faces = vec![
        FaceSample {
            velocity: [0.0, 1.0, 0.0, 0.0],
            weight: [1.0; 4]
        };
        face_len()
    ];
    let step = StepParams {
        body_count: 1,
        rows: 1,
        step_dt: 0.025,
        particles: 1,
        ..lattice()
    };
    let entry = "faces_to_particles";
    let mut pass = Pass::new();
    let got: Vec<FluidParticle> = pass
        .bind(2, &[particle], Layout::Dense)
        .bind(3, &faces, Layout::Records)
        .bind(9, &solid, Layout::Records)
        .bind(15, &[LiquidShape::default()], Layout::Dense)
        .bind(16, &[0_u32; 4], Layout::Dense)
        .bind(17, &faces, Layout::Records)
        .bind(18, &faces, Layout::Records)
        .bind(22, &[0_u32; 2], Layout::Dense)
        .bind(36, &[LiquidBody::default()], Layout::Dense)
        .run(entry, &step, 19, 1, 1);
    assert_eq!(
        got[0].position_radius[..3],
        particle.position_radius[..3],
        "{entry}: last-position fallback"
    );
    assert_eq!(
        got[0].position_radius[3], particle.position_radius[3],
        "{entry}: water is never deleted at a solid"
    );
    assert_eq!(
        pass.bound::<u32>(22, 2)[1],
        1,
        "{entry}: refused push is counted"
    );
}
