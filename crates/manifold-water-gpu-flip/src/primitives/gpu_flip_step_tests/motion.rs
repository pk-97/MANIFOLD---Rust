//! Particle motion: sampling, APIC's velocity and affine, RK3 and collisions.

use super::*;

/// Native MACVelocityField's ordinary per-component trilinear sample, in f64.
/// q is in cells from the lattice minimum. Every record of the padded array
/// holds all three faces; a corner past it contributes zero, and face
/// validity does not renormalise the interpolation.
pub(super) fn cpu_sample(q: [f64; 3], field: &[FaceSample]) -> [f64; 3] {
    cpu_sample_on(q, field, N)
}

pub(crate) fn cpu_sample_on(q: [f64; 3], field: &[FaceSample], n: [usize; 3]) -> [f64; 3] {
    if q.iter().any(|value| !value.is_finite()) {
        return [0.0; 3];
    }
    std::array::from_fn(|a| {
        let s: [f64; 3] = std::array::from_fn(|b| q[b] - if b == a { 0.0 } else { 0.5 });
        let base: [i64; 3] = std::array::from_fn(|b| s[b].floor() as i64);
        let t: [f64; 3] = std::array::from_fn(|b| s[b] - base[b] as f64);
        let mut sum = 0.0;
        for corner in 0..8 {
            let bit = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
            let c: [i64; 3] = std::array::from_fn(|b| base[b] + bit[b] as i64);
            if (0..3).all(|b| c[b] >= 0 && c[b] <= n[b] as i64) {
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

/// [`cpu_sample`]'s gradient per cell: row a is the gradient of the a
/// component, the trilinear weights' gradients over the same corners
/// (fluidsimulation.cpp _getIndicesAndGradientWeights), a corner past the
/// padded array contributing zero.
pub(super) fn cpu_gradient(q: [f64; 3], field: &[FaceSample]) -> [[f64; 3]; 3] {
    if q.iter().any(|value| !value.is_finite()) {
        return [[0.0; 3]; 3];
    }
    std::array::from_fn(|a| {
        let s: [f64; 3] = std::array::from_fn(|b| q[b] - if b == a { 0.0 } else { 0.5 });
        let base: [i64; 3] = std::array::from_fn(|b| s[b].floor() as i64);
        let t: [f64; 3] = std::array::from_fn(|b| s[b] - base[b] as f64);
        let mut gradient = [0.0; 3];
        for corner in 0..8 {
            let bit = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1];
            let c: [i64; 3] = std::array::from_fn(|b| base[b] + bit[b] as i64);
            if (0..3).all(|b| c[b] >= 0 && c[b] <= N[b] as i64) {
                let c: [usize; 3] = c.map(|value| value as usize);
                let v = f64::from(field[c[0] + (N[0] + 1) * (c[1] + (N[1] + 1) * c[2])].velocity[a]);
                let w: [f64; 3] = std::array::from_fn(|b| if bit[b] == 1 { t[b] } else { 1.0 - t[b] });
                let d: [f64; 3] = std::array::from_fn(|b| if bit[b] == 1 { 1.0 } else { -1.0 });
                for (g, slope) in gradient.iter_mut().enumerate() {
                    let product: f64 = (0..3).map(|b| if b == g { d[b] } else { w[b] }).product();
                    *slope += product * v;
                }
            }
        }
        gradient
    })
}

/// Zeroed affines for a move over `n` particles to write.
pub(super) fn no_affines(n: usize) -> Vec<ParticleAffine> {
    vec![ParticleAffine::default(); n]
}

/// Checks a particle's affine against the field's gradient at q0 (cells
/// from MIN), per metre, and its id.
fn check_affine(got: &ParticleAffine, q0: [f64; 3], field: &[FaceSample], id: u32, what: &str) {
    let want = cpu_gradient(q0, field);
    for (a, row) in [got.x, got.y, got.z].into_iter().enumerate() {
        for b in 0..3 {
            close(row[b], want[a][b] / f64::from(H), 10.0 / f64::from(H), &format!("{what} affine {a}/{b}"));
        }
    }
    assert_eq!(got.id, id, "{what}: the affine is tagged with its particle's id");
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
    // Past the high edge of the padded array the corners fall away.
    check([6.25, 5.25, 4.25], [1.5, 2.25, 3.75], "upper corner fade");
    check([0.0, 0.0, 0.0], [0.5, 0.75, 1.25], "lower inclusive corner");
    check([f64::NAN, 1.0, 1.0], [0.0; 3], "non-finite position");

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
    // Nonzero data in every packed lane, whatever its weight: every record
    // holds all three faces and the sampler reads them as stored.
    let stored = vec![
        FaceSample {
            velocity: [2.0, 3.0, 5.0, 0.0],
            weight: [0.0; 4]
        };
        face_len()
    ];
    // A linear field: velocity a is 10(a + 1) + c0 + 2 c1 + 3 c2 at record c,
    // so away from the box's edge every gradient row is (1, 2, 3) per cell.
    let mut linear = stored.clone();
    for (i, face) in linear.iter_mut().enumerate() {
        let c = pad_coords(i);
        for a in 0..3 {
            face.velocity[a] = (10 * (a + 1) + c[0] + 2 * c[1] + 3 * c[2]) as f32;
        }
    }
    let run = |faces: &[FaceSample]| {
        let mut pass = Pass::new();
        let solid = pass.wall_corners(MIN, box_hi(), 63);
        let moved = pass
            .bind(2, &particles, Layout::Dense)
            .bind(3, faces, Layout::Records)
            .bind(9, &solid, Layout::Dense)
            .bind(15, &[LiquidShape::default()], Layout::Dense)
            .bind(16, &[0_u32; 4], Layout::Dense)
            .bind(18, faces, Layout::Records)
            .bind(22, &vec![0_u32; 2 * particles.len()], Layout::Dense)
            .bind(36, &[LiquidBody::default()], Layout::Dense)
            .bind(75, &no_affines(particles.len()), Layout::Dense)
            .run::<FluidParticle>(
                "faces_to_particles",
                &StepParams {
                    step_dt: 0.0,
                    particles: particles.len() as u32,
                    ..lattice()
                },
                19,
                particles.len(),
                particles.len(),
            );
        (moved, pass.bound::<ParticleAffine>(75, particles.len()))
    };
    for field in [&stored, &linear] {
        let (moved, affines) = run(field);
        for (i, q) in points.into_iter().enumerate() {
            let want = cpu_sample(q, field);
            for (a, want) in want.into_iter().enumerate() {
                close(moved[i].velocity[a], want, 10.0, "stored sample");
            }
            assert_eq!(moved[i].id, particles[i].id);
            check_affine(&affines[i], q, field, particles[i].id, "stored");
        }
        assert_eq!(
            moved[0].position_radius, particles[0].position_radius,
            "dt=0 keeps the interior position"
        );
    }
    // The linear field's slope, in closed form, at the interior points.
    let (_, affines) = run(&linear);
    for i in [0, 2, 8] {
        for (a, row) in [affines[i].x, affines[i].y, affines[i].z].into_iter().enumerate() {
            let want = [1.0, 2.0, 3.0].map(|slope: f64| slope / f64::from(H));
            for b in 0..3 {
                close(row[b], want[b], 10.0, &format!("point {i} slope {a}/{b}"));
            }
        }
    }
}

#[test]
pub(super) fn gpu_flip_faces_to_particles_takes_apic_velocity_and_moves_by_rk3() {
    let faces = random_faces(0xf1a5, true);
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
    // A broken position: the move must leave it non-finite, never clamp or
    // zero it, so the tick's stats see it. A broken velocity is the grid's
    // problem: the gather spreads it to the faces, and the move takes the
    // faces' velocity, never the particle's own.
    particles[2].position_radius[0] = f32::NAN;
    particles[3].velocity[1] = f32::INFINITY;
    let dt = 0.07f32;
    let step = StepParams {
        step_dt: dt,
        particles: particles.len() as u32,
        ..lattice()
    };
    let mut pass = Pass::new();
    let solid = pass.wall_corners(MIN, box_hi(), 63);
    let got: Vec<FluidParticle> = pass
        .bind(2, &particles, Layout::Dense)
        .bind(3, &faces, Layout::Records)
        // Metal requires the conditional solid/region resources even with zero counts.
        .bind(9, &solid, Layout::Dense)
        .bind(15, &[LiquidShape::default()], Layout::Dense)
        .bind(16, &[0_u32; 4], Layout::Dense)
        .bind(36, &[LiquidBody::default()], Layout::Dense)
        // Spread equal to the new faces: no density move.
        .bind(18, &faces, Layout::Records)
        .bind(22, &vec![7u32; 2 * particles.len()], Layout::Dense)
        .bind(75, &no_affines(particles.len()), Layout::Dense)
        .run(
            "faces_to_particles",
            &step,
            19,
            particles.len(),
            particles.len(),
        );
    // Step 0 of the tick starts the counts over the stale 7s.
    let capped: Vec<u32> = pass.bound(22, 2 * particles.len());
    let affines: Vec<ParticleAffine> = pass.bound(75, particles.len());
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
        check_affine(&affines[i], q0, &faces, p.id, &format!("particle {i}"));
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
            let velocity = after[a];
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
            let solid =
                pass.wall_corners(MIN, std::array::from_fn(|a| MIN[a] + n[a] as f32 * H), 63);
            let got: Vec<FluidParticle> = pass
                .bind(2, &[particle], Layout::Dense)
                .bind(3, &faces, Layout::Records)
                .bind(9, &solid, Layout::Dense)
                .bind(15, &[LiquidShape::default()], Layout::Dense)
                .bind(16, &[0_u32; 4], Layout::Dense)
                .bind(18, &faces, Layout::Records)
                .bind(22, &[91u32; 2], Layout::Dense)
                .bind(75, &no_affines(1), Layout::Dense)
                .bind(36, &[LiquidBody::default()], Layout::Dense)
                .bind(46, &clock, Layout::Dense)
                .run(
                    "faces_to_particles",
                    &StepParams {
                        step_dt: dt,
                        particles: 1,
                        ..lattice()
                    },
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
        ("stationary", [0.15, y, y], [0.0, 0.0, 0.0], 0.01_f32),
        ("approach", [0.15, y, y], [-1.0, 0.0, 0.0], 0.01),
        ("crossing", [0.15, y, y], [-10.0, 0.0, 0.0], 0.04),
        ("tangent", [0.15, y, y], [0.0, 1.0, 0.0], 0.10),
        ("upper", [N[0] as f64 - 0.15, y, y], [1.0, 0.0, 0.0], 0.01),
        ("world epsilon", [0.05, y, y], [0.00005, 0.0, 0.0], 0.01),
        ("nonfinite", [f64::NAN, y, y], [0.0; 3], 0.01),
    ];
    let entry = "faces_to_particles";
    for (name, q0, velocity, dt) in cases {
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
        let mut pass = Pass::new();
        let solid = pass.wall_corners(MIN, box_hi(), 63);
        let got: Vec<FluidParticle> = pass
            .bind(2, &particles, Layout::Dense)
            .bind(3, &faces, Layout::Records)
            .bind(9, &solid, Layout::Dense)
            .bind(15, &[LiquidShape::default()], Layout::Dense)
            .bind(16, &[0_u32; 4], Layout::Dense)
            .bind(18, &faces, Layout::Records)
            .bind(22, &[0_u32; 2], Layout::Dense)
            .bind(75, &no_affines(1), Layout::Dense)
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
        // Sampling changes near the lattice's edges, so even a constant
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
        let want = native_wall_move(q0, reached, N, f64::from(H));
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

/// The particles `faces_to_particles` writes after a move over a field at
/// rest, inside the Bounds box MIN to `box_hi()` with the Wall faces
/// `step.wall_faces` as the solid, and the words of the refused-push counter.
fn move_at_rest(
    step: &StepParams,
    particles: &[FluidParticle],
    block: &[[u32; 3]],
) -> (Vec<FluidParticle>, Vec<u32>) {
    let mut pass = Pass::new();
    let mut solid = pass.wall_corners(MIN, box_hi(), step.wall_faces);
    for &corner in block {
        solid[pass.table.pool_index(corner)] = -H;
    }
    let faces = vec![FaceSample::default(); face_len()];
    let step = StepParams {
        particles: particles.len() as u32,
        ..*step
    };
    let got: Vec<FluidParticle> = pass
        .bind(2, particles, Layout::Dense)
        .bind(3, &faces, Layout::Records)
        .bind(9, &solid, Layout::Dense)
        .bind(15, &[LiquidShape::default()], Layout::Dense)
        .bind(16, &[0_u32; 4], Layout::Dense)
        .bind(18, &faces, Layout::Records)
        .bind(22, &vec![0_u32; 2 * particles.len()], Layout::Dense)
        .bind(75, &no_affines(particles.len()), Layout::Dense)
        .bind(36, &[LiquidBody::default()], Layout::Dense)
        .run(
            "faces_to_particles",
            &step,
            19,
            particles.len(),
            particles.len(),
        );
    let refused = pass.bound::<u32>(22, 2 * particles.len());
    (got, refused)
}

/// A live particle at `at` metres from MIN.
fn resting_particle(at: [f32; 3], id: u32) -> FluidParticle {
    FluidParticle {
        position_radius: [MIN[0] + at[0], MIN[1] + at[1], MIN[2] + at[2], 0.08],
        id,
        ..FluidParticle::default()
    }
}

/// D19: a particle under a body is never removed. It sits at the centre of a
/// solid block, where the distance has no gradient to climb, so it stays,
/// live, and counts as a refused push-out.
#[test]
pub(super) fn gpu_flip_collision_keeps_negative_body_phi() {
    let particle = resting_particle([2.5 * H; 3], 1);
    let corners: Vec<[u32; 3]> = (0..8)
        .map(|c| [2 + (c & 1), 2 + ((c >> 1) & 1), 2 + (c >> 2)])
        .collect();
    let (got, refused) = move_at_rest(&lattice(), &[particle], &corners);
    assert_eq!(
        got[0].position_radius, particle.position_radius,
        "the particle under the body stays, live"
    );
    assert_eq!(refused[1], 1, "its push-out is counted as refused");
}

/// A Wall is a solid like a body: a particle a move leaves beyond one is
/// pushed back to SOLID_BUFFER cells inside its plane, on a low face and on a
/// high one, and is never removed.
#[test]
pub(super) fn gpu_flip_particle_in_a_wall_ends_inside_its_plane() {
    let hi = box_hi();
    let in_box = [2.5 * H, 2.5 * H, 2.5 * H];
    let particles = [
        resting_particle([-0.1 * H, in_box[1], in_box[2]], 1),
        resting_particle([hi[0] - MIN[0] + 0.3 * H, in_box[1], in_box[2]], 2),
        resting_particle([in_box[0], in_box[1], -0.4 * H], 3),
    ];
    let (got, refused) = move_at_rest(&lattice(), &particles, &[]);
    let margin = WALL_MARGIN_CELLS as f32 * H;
    let want = [
        [
            MIN[0] + margin,
            particles[0].position_radius[1],
            particles[0].position_radius[2],
        ],
        [
            hi[0] - margin,
            particles[1].position_radius[1],
            particles[1].position_radius[2],
        ],
        [
            particles[2].position_radius[0],
            particles[2].position_radius[1],
            MIN[2] + margin,
        ],
    ];
    for (i, want) in want.iter().enumerate() {
        for a in 0..3 {
            assert!(
                (got[i].position_radius[a] - want[a]).abs() < 1e-5,
                "particle {i} axis {a}: {:?} vs {want:?}",
                got[i].position_radius
            );
        }
        assert_eq!(
            got[i].position_radius[3], 0.08,
            "particle {i} is pushed, never removed"
        );
    }
    assert_eq!(refused, [0; 6], "every push-out succeeds");
}

/// Water that ends a move beyond the plane of an Open face is removed; on a
/// Wall it is pushed back first, so a corner of an Open and a Wall face
/// removes the water only if it is still beyond the Open plane.
#[test]
pub(super) fn gpu_flip_removes_particles_beyond_an_open_face() {
    let hi = box_hi();
    let span = [hi[0] - MIN[0], hi[1] - MIN[1], hi[2] - MIN[2]];
    // +X is Open (bit 1 of -X +X -Y +Y -Z +Z); the other five faces are Walls.
    let open = 1u32 << 1;
    let step = lattice().with_bounds_for_test(MIN, hi, 63 & !open, open);
    let particles = [
        resting_particle([span[0] + 0.3 * H, 2.5 * H, 2.5 * H], 1),
        resting_particle([span[0] - 0.3 * H, 2.5 * H, 2.5 * H], 2),
        resting_particle([span[0] - 0.5 * H, span[1] + 0.1 * H, 2.5 * H], 3),
        resting_particle([span[0] + 0.5 * H, span[1] + 0.1 * H, 2.5 * H], 4),
        resting_particle([-0.3 * H, 2.5 * H, 2.5 * H], 5),
    ];
    let (got, _) = move_at_rest(&step, &particles, &[]);
    let radius: Vec<f32> = got.iter().map(|p| p.position_radius[3]).collect();
    assert_eq!(
        radius,
        [0.0, 0.08, 0.08, 0.0, 0.08],
        "beyond the Open plane removed; inside it, in the Wall corner pushed in, and beyond the -X Wall kept"
    );
    assert!(
        (got[2].position_radius[1] - (hi[1] - WALL_MARGIN_CELLS as f32 * H)).abs() < 1e-5,
        "the particle beyond the +Y Wall ends inside its plane: {:?}",
        got[2].position_radius
    );
    assert!(
        (got[4].position_radius[0] - (MIN[0] + WALL_MARGIN_CELLS as f32 * H)).abs() < 1e-5,
        "the particle beyond the -X Wall ends inside its plane: {:?}",
        got[4].position_radius
    );
    assert_eq!(got[1].id, 2, "ids stay with their slots");
}

/// With Bounds off the move removes water that ends below `kill_height`; the
/// default height removes none.
#[test]
pub(super) fn gpu_flip_removes_particles_below_the_kill_height() {
    let kill = MIN[1] + 1.5 * H;
    let particles = [
        resting_particle([2.5 * H, 0.5 * H, 2.5 * H], 1),
        resting_particle([2.5 * H, 1.4 * H, 2.5 * H], 2),
        resting_particle([2.5 * H, 1.6 * H, 2.5 * H], 3),
        resting_particle([2.5 * H, 4.5 * H, 2.5 * H], 4),
    ];
    let step = StepParams {
        kill_height: kill,
        ..lattice()
    };
    let (got, _) = move_at_rest(&step, &particles, &[]);
    let radius: Vec<f32> = got.iter().map(|p| p.position_radius[3]).collect();
    assert_eq!(
        radius,
        [0.0, 0.0, 0.08, 0.08],
        "removed below the height only"
    );
    let (kept, _) = move_at_rest(&lattice(), &particles, &[]);
    assert!(
        kept.iter().all(|p| p.position_radius[3] == 0.08),
        "the default height removes none"
    );
}

#[test]
pub(super) fn gpu_flip_collision_refuses_excessive_push_and_keeps_the_particle() {
    // A plane solid whose exit is beyond the native five-cell push limit.
    // D19: the particle is kept where it was, live, and counted as refused.
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
        .bind(18, &faces, Layout::Records)
        .bind(22, &[0_u32; 2], Layout::Dense)
        .bind(75, &no_affines(1), Layout::Dense)
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
