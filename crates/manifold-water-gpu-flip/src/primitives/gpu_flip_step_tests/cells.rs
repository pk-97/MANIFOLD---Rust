//! Cell and face passes against CPU references: transfer, divergence, distance, extension.

use super::*;

#[test]
pub(super) fn gpu_flip_water_is_negative_phi() {
    let mut rng = Stream::new(0xce11);
    // Draws straddle zero, with the eps snap's ±0.005h values among them.
    let phi: Vec<f32> = (0..cell_len())
        .map(|c| match c % 4 {
            0 => 0.005,
            1 => -0.005,
            _ => rng.unit() * 2.0 - 1.0,
        })
        .collect();
    let mut pass = Pass::new();
    let threads = pass.every_tile();
    let got: Vec<f32> =
        pass.bind(7, &phi, Layout::Cells)
            .run("water_from_phi", &lattice(), 5, cell_len(), threads);
    for (c, (g, f)) in got.iter().zip(&phi).enumerate() {
        assert_eq!(*g, f32::from(u8::from(*f < 0.0)), "cell {c}");
    }
}

#[test]
pub(super) fn gpu_flip_particles_to_faces_matches_the_wyvill_sum() {
    // Dense covers most faces; sparse leaves faces no particle reaches.
    for (name, count) in [("dense", 400), ("sparse", 12)] {
        let particles = random_particles(0x9261, count);
        let (sorted, ranges) = cpu_sort(&particles);
        let step = StepParams {
            capacity: sorted.len() as u32,
            ..lattice()
        };
        // The pass owns every record of the held tiles, the box's far edge
        // records among them.
        let mut pass = Pass::new();
        let threads = pass.every_tile();
        let filled = vec![FaceSample::default(); face_len()];
        let got: Vec<FaceSample> = pass
            .bind_ranges(&ranges)
            .bind(2, &sorted, Layout::Dense)
            .bind(4, &filled, Layout::Records)
            .run("particles_to_faces", &step, 4, face_len(), threads);
        let (checked, invalid) = check_wyvill_faces(&particles, &got);
        if name == "dense" {
            assert!(
                checked > 200,
                "the fixture reaches most faces, got {checked}"
            );
        } else {
            assert!(
                checked > 5 && invalid > 50,
                "{name}: {checked} faces reached, {invalid} not"
            );
        }
    }
}

/// Checks the particles to faces pass against the engine's kernel
/// (velocityadvector.cpp, in cells: r = √3/2). Every record has its three
/// faces and a face at the box's edge is gathered like any other. Returns
/// the faces whose velocity was checked and the faces no particle reaches.
pub(super) fn check_wyvill_faces(
    particles: &[FluidParticle],
    got: &[FaceSample],
) -> (usize, usize) {
    let rsq = 0.75f64;
    let wyvill = |d2: f64| {
        if d2 < rsq {
            1.0 - 4.0 / 9.0 * d2.powi(3) / rsq.powi(3) + 17.0 / 9.0 * d2 * d2 / (rsq * rsq)
                - 22.0 / 9.0 * d2 / rsq
        } else {
            0.0
        }
    };
    let (mut checked, mut invalid) = (0, 0);
    for (i, face) in got.iter().enumerate() {
        let p = pad_coords(i);
        for a in 0..3 {
            let centre: [f64; 3] =
                std::array::from_fn(|b| p[b] as f64 + if b == a { 0.0 } else { 0.5 });
            let (mut weight, mut momentum) = (0.0f64, 0.0f64);
            for particle in particles.iter().filter(|q| q.position_radius[3] > 0.0) {
                let d2: f64 = (0..3)
                    .map(|b| {
                        let q = (f64::from(particle.position_radius[b]) - f64::from(MIN[b]))
                            / f64::from(H);
                        (q - centre[b]).powi(2)
                    })
                    .sum();
                let w = wyvill(d2);
                weight += w;
                momentum += w * f64::from(particle.velocity[a]);
            }
            // A sum near the 1e-6 validity line can land on either side in f32.
            if (1e-7..1e-5).contains(&weight) {
                continue;
            }
            let valid = weight > 1e-6;
            let velocity = if valid { momentum / weight } else { 0.0 };
            if !valid {
                // Too little weight to trust: extension fills the face.
                assert_eq!(
                    (face.velocity[a], face.weight[a]),
                    (0.0, 0.0),
                    "invalid face {p:?}/{a}"
                );
                invalid += 1;
                continue;
            }
            close(face.weight[a], weight, weight, &format!("weight {p:?}/{a}"));
            // Near the rim the kernel is 1 minus terms summing to about 1, so
            // each particle's f32 weight is off by ~1e-7 absolute; a ratio is
            // held to 1e-5 only where the weight dwarfs that.
            if weight > 0.05 {
                close(
                    face.velocity[a],
                    velocity,
                    1.0,
                    &format!("velocity {p:?}/{a}"),
                );
                checked += 1;
            }
        }
    }
    (checked, invalid)
}

#[test]
pub(super) fn gpu_flip_face_divergence_is_the_outflow_of_water_cells() {
    let faces = random_faces(0xd1f, false);
    let water = random_water(0x3a7e);
    for solid in [false, true] {
        let open = solid_faces(0xd2f, solid);
        let moving = solid_velocity(0xd3f, &open);
        let mut pass = Pass::new();
        let threads = pass.every_tile();
        let got: Vec<f32> = pass
            .bind(3, &faces, Layout::Records)
            .bind(6, &water, Layout::Cells)
            .bind(10, &open, Layout::Records)
            .bind(11, &moving, Layout::Records)
            .run("divergence", &lattice(), 5, cell_len(), threads);
        let mut pushed = 0;
        for (c, g) in got.iter().enumerate() {
            let p = cell_coords(c);
            // A face counts by its open fraction, plus the solid's (c − w)·v_s,
            // c the cell's open volume. A wall's face is closed by its open
            // fraction and carries no solid velocity, like any solid's.
            let centre = f64::from(open[pad_index(p)].weight[3]);
            let flux = |q: [usize; 3], a: usize| {
                let w = f64::from(open[pad_index(q)].weight[a]);
                w * f64::from(faces[pad_index(q)].velocity[a])
                    + (centre - w) * f64::from(moving[pad_index(q)].velocity[a])
            };
            if water[c] > 0.5 && (0..3).any(|a| moving[pad_index(p)].velocity[a] != 0.0) {
                pushed += 1;
            }
            let want = if water[c] > 0.5 {
                (0..3)
                    .map(|a| {
                        let mut q = p;
                        q[a] += 1;
                        flux(q, a) - flux(p, a)
                    })
                    .sum::<f64>()
                    / f64::from(H)
            } else {
                0.0
            };
            close(*g, want, 10.0, &format!("solid {solid} cell {p:?}"));
        }
        assert_eq!(
            pushed > 10,
            solid,
            "the solid fixture moves faces of water cells: {pushed}"
        );
    }
}

/// Distances as the ghost rows can see them: water cells from −0.6h to 0.2h
/// (some above the −0.005h the solve takes at most), air from −0.3h to 2.9h
/// (some below the 0 it takes at least).
pub(super) fn random_phi(water: &[f32], seed: u64) -> Vec<f32> {
    let mut rng = Stream::new(seed);
    water
        .iter()
        .map(|&w| {
            H * if w > 0.5 {
                -0.6 + 0.8 * rng.unit()
            } else {
                -0.3 + 3.2 * rng.unit()
            }
        })
        .collect()
}

/// Zero distances leave air pressure at zero, the plain Dirichlet projection;
/// real distances give the air side the ghost pressure
/// clamp(max(φ_air, 0) / (min(φ_water, −0.005h) + 1e-6), ±25) · p_water.
#[test]
pub(super) fn gpu_flip_subtract_pressure_projects_faces_touching_water() {
    let faces = random_faces(0x5b7, false);
    let water = random_water(0xa7e2);
    let mut rng = Stream::new(0x9e55);
    let pressure: Vec<f32> = water
        .iter()
        .map(|&w| if w > 0.5 { rng.signed(3.0) } else { 0.0 })
        .collect();
    let ghost = |air: usize, wet: usize, phi: &[f32]| {
        let surface = f64::from(phi[wet]).min(-0.005 * f64::from(H));
        (f64::from(phi[air]).max(0.0) / (surface + 1e-6)).clamp(-25.0, 25.0)
            * f64::from(pressure[wet])
    };
    // Faces whose air side took a ghost pressure that is not zero.
    let mut ghosts = 0;
    let draws = [
        (false, vec![0.0; cell_len()]),
        (false, random_phi(&water, 0x9e56)),
        (true, random_phi(&water, 0x9e57)),
    ];
    for (solid, phi) in draws {
        let open = solid_faces(0x5c7, solid);
        let got: Vec<FaceSample> = Pass::new()
            .bind(20, &faces, Layout::Records)
            .bind(10, &open, Layout::Records)
            .bind(6, &water, Layout::Cells)
            .bind(8, &pressure, Layout::Cells)
            .bind(7, &phi, Layout::Cells)
            .run_list(
                "subtract_pressure",
                &StepParams {
                    ghost: 1,
                    ..lattice()
                },
                20,
                face_len(),
            );
        let mut closed = 0;
        for (i, face) in got.iter().enumerate() {
            let p = pad_coords(i);
            for a in 0..3 {
                let (velocity, weight) = if open[i].weight[a] <= 0.0 {
                    // Pressure subtraction clears closed faces, a wall's among
                    // them; the later solid constraint writes their velocity
                    // independently.
                    closed += usize::from(face_exists(p, a) && p[a] > 0 && p[a] < N[a]);
                    (0.0, 0.0)
                } else {
                    // Only inner faces are open: both cells are in the box.
                    let mut below = p;
                    below[a] -= 1;
                    let (up, down) = (cell_index(p), cell_index(below));
                    let (wet_up, wet_down) = (water[up] > 0.5, water[down] > 0.5);
                    let u = f64::from(faces[i].velocity[a]);
                    let p_up = if wet_up {
                        f64::from(pressure[up])
                    } else {
                        ghost(up, down, &phi)
                    };
                    let p_down = if wet_down {
                        f64::from(pressure[down])
                    } else {
                        ghost(down, up, &phi)
                    };
                    if wet_up != wet_down && (if wet_up { p_down } else { p_up }) != 0.0 {
                        ghosts += 1;
                    }
                    if wet_up || wet_down {
                        (u - (p_up - p_down) / f64::from(H), 1.0)
                    } else {
                        (0.0, 0.0)
                    }
                };
                // Ghost pressures reach 25 × 3, a step of 300 over h.
                close(
                    face.velocity[a],
                    velocity,
                    400.0,
                    &format!("velocity {p:?}/{a}"),
                );
                close(face.weight[a], weight, 1.0, &format!("weight {p:?}/{a}"));
            }
        }
        // Native velocity projection uses 1e-6, unlike the matrix's 1e-9.
        // Check that projection operator with the same error bound.
        let h = f64::from(H);
        let div = |f: &[FaceSample], c: [usize; 3]| -> f64 {
            (0..3)
                .map(|a| {
                    let mut up = c;
                    up[a] += 1;
                    f64::from(f[pad_index(up)].velocity[a]) - f64::from(f[pad_index(c)].velocity[a])
                })
                .sum::<f64>()
                / h
        };
        for c in (0..cell_len()).filter(|&c| water[c] > 0.5) {
            let q = cell_coords(c);
            let centre = f64::from(phi[c]).min(-0.005 * h);
            let mut lp = 0.0;
            for a in 0..3 {
                for side in [-1i64, 1] {
                    let at = q[a] as i64 + side;
                    if at < 0 || at >= N[a] as i64 {
                        continue;
                    }
                    let mut r = q;
                    r[a] = at as usize;
                    // A closed face contributes no pressure flux after the
                    // producer-side validity clear.
                    let mut face = q;
                    face[a] = q[a].max(r[a]);
                    if open[pad_index(face)].weight[a] <= 0.0 {
                        continue;
                    }
                    let j = cell_index(r);
                    let theta = (f64::from(phi[j]).max(0.0) / (centre + 1e-6)).clamp(-25.0, 25.0);
                    let p_j = if water[j] > 0.5 {
                        f64::from(pressure[j])
                    } else {
                        theta * f64::from(pressure[c])
                    };
                    lp += (p_j - f64::from(pressure[c])) / (h * h);
                }
            }
            let left = div(&got, q);
            // Walls and inner closed faces carry no flux after subtraction;
            // this explicitly includes the producer-side closed-face clear.
            let mut walled = faces.clone();
            for (i, face) in walled.iter_mut().enumerate() {
                let p = pad_coords(i);
                for a in 0..3 {
                    let both_air = if face_exists(p, a) && p[a] > 0 && p[a] < N[a] {
                        let mut below = p;
                        below[a] -= 1;
                        water[cell_index(p)] <= 0.5 && water[cell_index(below)] <= 0.5
                    } else {
                        false
                    };
                    if p[a] == 0 || p[a] == N[a] || open[i].weight[a] <= 0.0 || both_air {
                        face.velocity[a] = 0.0;
                    }
                }
            }
            let want = div(&walled, q) - lp;
            // f32 faces up to ~300 round by ~3e-5 each; six over h stays under 1e-3.
            assert!(
                (left - want).abs() <= 5e-3,
                "divergence left in cell {q:?}: {left} vs {want}"
            );
        }
        assert_eq!(
            closed > 20,
            solid,
            "the solid fixture closes faces, the open one none: {closed}"
        );
    }
    assert!(
        ghosts > 20,
        "the draw puts ghost pressures on many faces: {ghosts}"
    );
}

/// The engine's particle radius at its default scale: half a cell's diagonal.
pub(super) fn sdf_radius() -> f64 {
    0.5 * 3f64.sqrt() * f64::from(H)
}

/// The cells a particle at `q` reaches along axis `a`: the engine's box,
/// floor((q ± 2r − min) / h), before the lattice cuts it.
pub(super) fn scatter_box(q: f64, a: usize) -> [i64; 2] {
    let (h, search) = (f64::from(H), 2.0 * sdf_radius());
    let from = q - f64::from(MIN[a]);
    [
        ((from - search) / h).floor() as i64,
        ((from + search) / h).floor() as i64,
    ]
}

/// The engine's level set: fill with 3h, scatter |centre − p| − r from each
/// live particle into every cell of its box, snap |φ| < 0.005h.
pub(super) fn cpu_scatter_distance(particles: &[FluidParticle]) -> Vec<f64> {
    let h = f64::from(H);
    let mut phi = vec![3.0 * h; cell_len()];
    for particle in particles.iter().filter(|q| q.position_radius[3] > 0.0) {
        let q: [f64; 3] = std::array::from_fn(|a| f64::from(particle.position_radius[a]));
        let reach: [[i64; 2]; 3] = std::array::from_fn(|a| {
            let [lo, hi] = scatter_box(q[a], a);
            [lo.max(0), hi.min(N[a] as i64 - 1)]
        });
        for z in reach[2][0]..=reach[2][1] {
            for y in reach[1][0]..=reach[1][1] {
                for x in reach[0][0]..=reach[0][1] {
                    let c = [x as usize, y as usize, z as usize];
                    let centre: [f64; 3] =
                        std::array::from_fn(|a| f64::from(MIN[a]) + (c[a] as f64 + 0.5) * h);
                    let d = (0..3)
                        .map(|a| (centre[a] - q[a]).powi(2))
                        .sum::<f64>()
                        .sqrt()
                        - sdf_radius();
                    let slot = &mut phi[cell_index(c)];
                    *slot = slot.min(d);
                }
            }
        }
    }
    snap(phi)
}

pub(super) fn snap(phi: Vec<f64>) -> Vec<f64> {
    let eps = 0.005 * f64::from(H);
    phi.into_iter()
        .map(|v| {
            if v.abs() < eps {
                if v > 0.0 { eps } else { -eps }
            } else {
                v
            }
        })
        .collect()
}

/// The particle distance pass's own reading: the min over the 125 bins around
/// each cell, from the sort's ranges, keeping a particle only when the cell
/// is inside its box; 3h when no particle contributes.
pub(super) fn cpu_gather_distance(sorted: &[FluidParticle], ranges: &[CellRange]) -> Vec<f64> {
    let h = f64::from(H);
    let phi = (0..cell_len())
        .map(|c| {
            let p = cell_coords(c);
            let centre: [f64; 3] =
                std::array::from_fn(|a| f64::from(MIN[a]) + (p[a] as f64 + 0.5) * h);
            let mut phi = 3.0 * h;
            for z in p[2].saturating_sub(2)..=(p[2] + 2).min(N[2] - 1) {
                for y in p[1].saturating_sub(2)..=(p[1] + 2).min(N[1] - 1) {
                    for x in p[0].saturating_sub(2)..=(p[0] + 2).min(N[0] - 1) {
                        let r = ranges[cell_index([x, y, z])];
                        for particle in &sorted[r.start as usize..(r.start + r.count) as usize] {
                            let q: [f64; 3] =
                                std::array::from_fn(|a| f64::from(particle.position_radius[a]));
                            let inside = (0..3).all(|a| {
                                let [lo, hi] = scatter_box(q[a], a);
                                (lo..=hi).contains(&(p[a] as i64))
                            });
                            if particle.position_radius[3] > 0.0 && inside {
                                let d = (0..3)
                                    .map(|a| (centre[a] - q[a]).powi(2))
                                    .sum::<f64>()
                                    .sqrt();
                                phi = phi.min(d - sdf_radius());
                            }
                        }
                    }
                }
            }
            phi
        })
        .collect();
    snap(phi)
}

pub(super) fn distance_fixtures() -> Vec<(&'static str, Vec<FluidParticle>)> {
    let particle = |q: [f32; 3]| FluidParticle {
        position_radius: [q[0], q[1], q[2], 0.08],
        velocity: [0.0; 3],
        id: 1,
    };
    let centre =
        |p: [usize; 3]| -> [f32; 3] { std::array::from_fn(|a| MIN[a] + (p[a] as f32 + 0.5) * H) };
    let at = |cells: [f32; 3]| -> [f32; 3] { std::array::from_fn(|a| MIN[a] + cells[a] * H) };
    let corner = centre([2, 1, 1]).map(|v| v + 0.499 * H);
    vec![
        ("random", random_particles(0xd157, 70)),
        ("corner", vec![particle(corner)]),
        (
            "boxed",
            vec![
                particle(at([2.001, 0.001, 0.001])),
                particle(at([1.1, 1.5, 1.5])),
            ],
        ),
        ("ring_two_only", vec![particle(at([2.05, 2.5, 1.5]))]),
    ]
}

/// CPU gather must reproduce the native scatter in every cell, including
/// air cells with only ring-two contributors beside a wet cell. Those air
/// distances set the ghost-fluid pressure ratio.
#[test]
pub(super) fn particle_distance_gather_matches_native_scatter() {
    for (name, particles) in distance_fixtures() {
        let (sorted, ranges) = cpu_sort(&particles);
        let gather = cpu_gather_distance(&sorted, &ranges);
        let engine = cpu_scatter_distance(&particles);
        for (c, (a, b)) in gather.iter().zip(&engine).enumerate() {
            assert!(
                (a - b).abs() < 1e-12,
                "{name} cell {:?}: gather {a} vs scatter {b}",
                cell_coords(c)
            );
        }
        if name == "ring_two_only" {
            let h = f64::from(H);
            assert!((engine[cell_index([0, 2, 1])] - (1.55 * h - sdf_radius())).abs() < 1e-6);
            assert!(
                engine[cell_index([1, 2, 1])] < 0.0,
                "adjacent cell must be wet"
            );
        }
    }
}

/// GPU distances match the native scatter oracle in every cell. Fixtures
/// exercise zero snapping, exact scatter-box support, and sparse ring two.
#[test]
pub(super) fn gpu_flip_particle_distance_is_the_engines_level_set() {
    let h = f64::from(H);
    for (name, particles) in distance_fixtures() {
        let (sorted, ranges) = cpu_sort(&particles);
        let want = cpu_scatter_distance(&particles);
        let step = StepParams {
            capacity: sorted.len() as u32,
            ..lattice()
        };
        let mut pass = Pass::new();
        let threads = pass.every_tile();
        let got: Vec<f32> = pass
            .bind_ranges(&ranges)
            .bind(2, &sorted, Layout::Dense)
            .run("particle_distance", &step, 5, cell_len(), threads);
        let empty = ranges.iter().filter(|r| r.count == 0).count();
        assert!(
            empty > 10 && empty < cell_len(),
            "{name}: the draw has empty and full cells ({empty} empty)"
        );
        for (c, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!(
                (f64::from(*g) - w).abs() <= 1e-5,
                "{name} cell {:?}: {g} vs {w}",
                cell_coords(c)
            );
            let r = ranges[c];
            if sorted[r.start as usize..(r.start + r.count) as usize]
                .iter()
                .any(|q| q.position_radius[3] > 0.0)
            {
                assert!(
                    *w <= -0.005 * h,
                    "{name}: occupied cell {:?} reads {w}",
                    cell_coords(c)
                );
            }
        }
        let read = |c: [usize; 3], want: f64| (f64::from(got[cell_index(c)]) - want).abs() <= 1e-6;
        if name == "corner" {
            assert!(
                read([2, 1, 1], -0.005 * h),
                "the particle's cell snaps down"
            );
            assert!(
                read([3, 2, 2], 0.005 * h),
                "the empty cell across the corner snaps up"
            );
        }
        if name == "boxed" {
            let corner_reach = (3.0 * (1.499 * h).powi(2)).sqrt() - sdf_radius();
            assert!(
                read([3, 1, 1], corner_reach),
                "cell 3 takes the near particle, not the boxed-out one"
            );
            assert!(
                read([2, 1, 1], 1.4 * h - sdf_radius()),
                "cell 2 is inside the far particle's box"
            );
        }
    }
}

/// One layer of the fluid velocity's extrapolation on the padded array: a
/// valid face is kept, an invalid one with valid neighbours takes the mean of
/// their velocities and is valid from then on. A wall's face is no different
/// from any other, and nothing lies past the array.
pub(super) fn cpu_extend_layer(faces: &[FaceSample]) -> Vec<FaceSample> {
    let m = padded();
    (0..face_len())
        .map(|i| {
            let p = pad_coords(i);
            let mut out = faces[i];
            for a in 0..3 {
                if faces[i].weight[a] > 0.0 {
                    continue;
                }
                let (mut sum, mut hits) = (0.0f64, 0.0f64);
                for b in 0..3 {
                    for d in [-1i64, 1] {
                        let at = p[b] as i64 + d;
                        if at < 0 || at >= m[b] as i64 {
                            continue;
                        }
                        let mut q = p;
                        q[b] = at as usize;
                        let neighbour = faces[pad_index(q)];
                        if neighbour.weight[a] > 0.0 {
                            sum += f64::from(neighbour.velocity[a]);
                            hits += 1.0;
                        }
                    }
                }
                if hits > 0.0 {
                    out.velocity[a] = (sum / hits) as f32;
                    out.weight[a] = 1.0;
                }
            }
            out
        })
        .collect()
}

#[test]
pub(super) fn gpu_flip_extend_faces_fills_one_layer() {
    let faces = random_faces(0xe7e, true);
    let mut pass = Pass::new();
    pass.bind(3, &faces, Layout::Records)
        .bind(4, &faces, Layout::Records);
    let threads = pass.allocated_tiles(&lattice());
    let got: Vec<FaceSample> = pass.run("extend_faces", &lattice(), 4, face_len(), threads);
    let want = cpu_extend_layer(&faces);
    let mut filled = 0;
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        for a in 0..3 {
            close(
                g.velocity[a],
                f64::from(w.velocity[a]),
                1.0,
                &format!("velocity {:?}/{a}", pad_coords(i)),
            );
            assert_eq!(g.weight[a], w.weight[a], "weight {:?}/{a}", pad_coords(i));
            filled += usize::from(faces[i].weight[a] == 0.0 && w.weight[a] > 0.0);
        }
    }
    assert!(filled > 20, "the fixture fills faces, got {filled}");
}

#[test]
pub(super) fn gpu_flip_pressure_clear_precedes_face_extension() {
    let target = [2, 2, 2];
    let target_index = pad_index(target);
    let mut faces = vec![FaceSample::default(); face_len()];
    faces[target_index].velocity[0] = 9.0;
    faces[pad_index([4, 2, 2])].velocity[0] = 13.0;
    let open = vec![
        FaceSample {
            weight: [1.0; 4],
            ..FaceSample::default()
        };
        face_len()
    ];
    // An inner closed high-velocity face and open faces between air cells must
    // both be cleared by subtraction, so extension has no invalid seed.
    let mut open = open;
    open[target_index].weight[0] = 0.0;
    let water = vec![0.0; cell_len()];
    let pressure = vec![0.0; cell_len()];
    let phi = vec![0.0; cell_len()];
    let projected: Vec<FaceSample> = Pass::new()
        .bind(20, &faces, Layout::Records)
        .bind(10, &open, Layout::Records)
        .bind(6, &water, Layout::Cells)
        .bind(8, &pressure, Layout::Cells)
        .bind(7, &phi, Layout::Cells)
        .run_list("subtract_pressure", &lattice(), 20, face_len());
    assert_eq!(
        projected[target_index].velocity[0], 0.0,
        "closed inner velocity is cleared before extension"
    );
    assert_eq!(
        projected[target_index].weight[0], 0.0,
        "closed inner validity is cleared before extension"
    );
    assert_eq!(
        projected[pad_index([4, 2, 2])].velocity[0],
        0.0,
        "open air-only velocity is cleared before extension"
    );
    let mut pass = Pass::new();
    pass.bind(3, &projected, Layout::Records);
    let threads = pass.allocated_tiles(&lattice());
    let extended: Vec<FaceSample> = pass.run("extend_faces", &lattice(), 4, face_len(), threads);
    for x in [target[0] - 1, target[0], target[0] + 1] {
        let index = pad_index([x, target[1], target[2]]);
        assert_eq!(
            extended[index].velocity[0], 0.0,
            "closed air face cannot seed x-neighbour velocity at x={x}"
        );
        assert_eq!(
            extended[index].weight[0], 0.0,
            "closed air face cannot seed x-neighbour validity at x={x}"
        );
    }
}
