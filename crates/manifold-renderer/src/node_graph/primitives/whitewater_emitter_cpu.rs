//! CPU reference for BUG-imy3.1, checked against the unchanged vendored engine.
//! FLIP Fluids turbulencefield.cpp (MIT); see THIRD_PARTY_NOTICES.md.

use super::whitewater_particle_cpu::{Box3, face_index};

/// FLIP's influence-scaled rate over one duration, rounded before tick replication.
pub(super) fn emission_count(
    energy: f32,
    potentials: [f32; 2],
    rates: [f32; 2],
    influence: f32,
    points_per_cell: f32,
    ticks: f32,
    dt: f32,
) -> u32 {
    let per_tick = influence * energy
        * (rates[0] * potentials[0] + rates[1] * potentials[1])
        * dt * 8.0 / points_per_cell;
    if per_tick > 0.0 {
        (per_tick + 0.5).floor() as u32 * ticks.round().max(0.0) as u32
    } else {
        0
    }
}

#[test]
fn turbulence_emission_count_uses_duration_and_rounds_each_tick() {
    // Engine _getNumberOfEmissionParticles: round(175 * dt) at full potential.
    assert_eq!(emission_count(1.0, [0.0, 1.0], [175.0; 2], 1.0, 8.0, 1.0, 1.0 / 60.0), 3);
    assert_eq!(emission_count(1.0, [0.0, 1.0], [175.0; 2], 1.0, 8.0, 1.0, 1.0 / 30.0), 6);
    // Rounding after multiplying the duration by three would give 17.
    assert_eq!(emission_count(1.0, [0.0, 1.0], [175.0; 2], 1.0, 8.0, 3.0, 1.0 / 30.0), 18);
}

/// A second of full-potential emission keeps the authored per-second rate at
/// every Sim Rate: each step rounds its own count, so `hz` steps land within
/// half a particle a step of it.
#[test]
fn emission_per_second_holds_at_every_sim_rate() {
    use super::emission_count::WAVECREST_RATE;
    for rate in manifold_physics::SimRate::ALL {
        let dt = rate.interval() as f32;
        let emitted: u32 = (0..rate.hz())
            .map(|_| emission_count(1.0, [1.0, 1.0], [WAVECREST_RATE; 2], 1.0, 8.0, 1.0, dt))
            .sum();
        let authored = 2.0 * WAVECREST_RATE;
        let rounding = rate.hz() as f32 / 2.0;
        assert!(
            (emitted as f32 - authored).abs() <= rounding,
            "{} Hz: {emitted} particles in a second against {authored} authored",
            rate.hz()
        );
    }
}

#[test]
fn whitewater_new_emitter_atoms_generate_valid_wgsl() {
    fn check<P: crate::node_graph::primitive::Primitive>() {
        let wgsl = crate::node_graph::freeze::codegen::standalone_for_spec::<P>().unwrap();
        let module = naga::front::wgsl::parse_str(&wgsl)
            .unwrap_or_else(|e| panic!("{}: {}", P::TYPE_ID, e.emit_to_string(&wgsl)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}: {}", P::TYPE_ID, e.emit_to_string(&wgsl)));
    }
    check::<super::turbulence_field::TurbulenceField>();
    check::<super::inside_turbulence_potential::InsideTurbulencePotential>();
    check::<super::turbulence_emission_count::TurbulenceEmissionCount>();
    check::<super::whitewater_emitter_velocity::WhitewaterEmitterVelocity>();
    check::<super::whitewater_obstacle_source::WhitewaterObstacleSource>();
    check::<super::whitewater_influence::WhitewaterInfluence>();
    check::<super::dust_potential::DustPotential>();
    check::<super::whitewater_type::WhitewaterType>();
}

pub(super) fn turbulence(
    faces: [&[f32]; 3],
    face_cells: [u32; 3],
    distance: &[f32],
    grid: Box3,
) -> Vec<f32> {
    let cells = grid.cells;
    let pad = (cells[0] - face_cells[0]) as i32 / 2;
    let velocity = |c: [i32; 3]| -> [f32; 3] {
        std::array::from_fn(|a| {
            let mut next = c;
            next[a] += 1;
            let get = |at| face_index(at, a, pad, face_cells).map_or(0.0, |i| faces[a][i]);
            0.5 * (get(c) + get(next))
        })
    };
    let mut field = vec![0.0; distance.len()];
    for k in 0..cells[2] as i32 {
        for j in 0..cells[1] as i32 {
            for i in 0..cells[0] as i32 {
                let c = [i, j, k];
                let index = grid.index(c);
                if distance[index] >= 0.0 {
                    continue;
                }
                let vi = velocity(c);
                let mut sum = 0.0f64;
                for z in (k - 2).max(0)..(k + 2).min(cells[2] as i32 - 1) {
                    for y in (j - 2).max(0)..(j + 2).min(cells[1] as i32 - 1) {
                        for x in (i - 2).max(0)..(i + 2).min(cells[0] as i32 - 1) {
                            let vj = velocity([x, y, z]);
                            let dv: [f32; 3] = std::array::from_fn(|a| vi[a] - vj[a]);
                            let len = dv.iter().map(|v| v * v).sum::<f32>().sqrt();
                            if len < 1e-5 {
                                continue;
                            }
                            let delta = [(i - x) as f32, (j - y) as f32, (k - z) as f32]
                                .map(|v| v * grid.cell_size());
                            let r = delta.iter().map(|v| v * v).sum::<f32>().sqrt();
                            let dot = (0..3).map(|a| (dv[a] / len) * (delta[a] / r)).sum::<f32>();
                            sum += f64::from(len)
                                * (1.0 - f64::from(dot))
                                * (1.0
                                    - f64::from(r)
                                        / (12.0f64).sqrt()
                                        / f64::from(grid.cell_size()));
                        }
                    }
                }
                field[index] = sum as f32;
            }
        }
    }
    field
}

pub(super) fn sample(field: &[f32], grid: Box3, p: [f32; 3]) -> f32 {
    let q = grid.position(p).map(|v| v - 0.5);
    let base = q.map(|v| v.floor() as i32);
    let f: [f32; 3] = std::array::from_fn(|a| q[a] - base[a] as f32);
    (0..8)
        .map(|corner| {
            let at: [i32; 3] = std::array::from_fn(|a| base[a] + ((corner >> a) & 1));
            let weight = (0..3)
                .map(|a| {
                    if (corner >> a) & 1 == 1 {
                        f[a]
                    } else {
                        1.0 - f[a]
                    }
                })
                .product::<f32>();
            if grid.in_grid(at) {
                weight * field[grid.index(at)]
            } else {
                0.0
            }
        })
        .sum()
}

#[test]
fn whitewater_turbulence_uniform_field_is_zero_and_air_is_zero() {
    let grid = Box3 {
        cells: [8; 3],
        center: [4.0; 3],
        size: [8.0; 3],
    };
    let face = vec![2.0; 9 * 8 * 8];
    let field = turbulence([&face; 3], [8; 3], &vec![-1.0; 512], grid);
    assert!(field.iter().all(|v| *v == 0.0));
    assert_eq!(sample(&field, grid, [0.01, 2.7, 3.9]), 0.0);
}

#[cfg(feature = "whitewater-oracle")]
#[test]
fn whitewater_turbulence_values_match_vendored_engine() {
    use manifold_fluids::{
        WhitewaterFields, WhitewaterGrid, WhitewaterLifecycle, whitewater_oracle,
    };
    let n = 8usize;
    let h = 0.25;
    let grid = Box3 {
        cells: [n as u32; 3],
        center: [1.0; 3],
        size: [2.0; 3],
    };
    let fields: [Vec<f32>; 3] = std::array::from_fn(|axis| {
        (0..(n + 1) * n * n)
            .map(|i| {
                let dims = std::array::from_fn::<_, 3, _>(|a| n + usize::from(a == axis));
                let x = i % dims[0];
                let y = i / dims[0] % dims[1];
                let z = i / (dims[0] * dims[1]);
                ((x * 7 + y * 11 + z * 3 + axis * 5) % 19) as f32 * 2.0 - 12.0
            })
            .collect()
    });
    let faces = fields.each_ref().map(Vec::as_slice);
    let phi: Vec<f32> = (0..n * n * n)
        .map(|i| if i % 7 == 0 { 0.5 } else { -1.0 })
        .collect();
    let solid = vec![10.0; (n + 1).pow(3)];
    let mut engine = WhitewaterLifecycle::new(
        WhitewaterGrid {
            cells: [n as u32; 3],
            cell_size: h,
            origin: [0.0; 3],
        },
        1000,
        1,
    )
    .unwrap();
    engine
        .set_fields(&WhitewaterFields {
            face_u: faces[0],
            face_v: faces[1],
            face_w: faces[2],
            face_cells: [n as u32; 3],
            face_offset: [0; 3],
            level: &phi,
            solid: &solid,
            gravity: [0.0; 3],
        })
        .unwrap();
    let positions = [
        [0.01, 0.02, 0.03],
        [1.99, 1.98, 1.97],
        [0.731, 1.213, 0.887],
        [1.375; 3],
    ];
    let (actual, samples) = whitewater_oracle::turbulence(&mut engine, &positions).unwrap();
    let expected = turbulence(faces, [n as u32; 3], &phi, grid);
    for (i, (&a, &b)) in actual.iter().zip(&expected).enumerate() {
        assert!(
            (a - b).abs() <= 2e-5 * b.abs().max(1.0),
            "cell {i}: engine {a}, reference {b}"
        );
    }
    for (p, a) in positions.into_iter().zip(samples) {
        let b = sample(&expected, grid, p);
        assert!(
            (a - b).abs() <= 2e-5 * b.abs().max(1.0),
            "sample {p:?}: engine {a}, reference {b}"
        );
    }
}

#[cfg(feature = "whitewater-oracle")]
#[test]
fn whitewater_inside_emission_counts_match_vendored_engine() {
    use manifold_fluids::{
        WhitewaterFields, WhitewaterGrid, WhitewaterLifecycle,
        whitewater_oracle::{self, EmissionOptions},
    };
    let n = 16usize;
    let u = vec![12.0; (n + 1) * n * n];
    let v: Vec<f32> = (0..n * (n + 1) * n)
        .map(|i| if i % n % 2 == 0 { 24.0 } else { -24.0 })
        .collect();
    let w = vec![0.0; n * n * (n + 1)];
    let phi = vec![-5.0; n * n * n];
    let solid = vec![10.0; (n + 1).pow(3)];
    let markers = vec![[8.0; 3]; 32];
    for (rate, influence, generation, dt) in [
        (175.0, 1.0, 1.0, 1.0 / 60.0),
        (175.0, 1.0, 1.0, 1.0 / 30.0),
        (60.0, 1.0, 1.0, 1.0 / 60.0),
        (175.0, 0.5, 1.0, 1.0 / 60.0),
        (175.0, 1.0, 0.0, 1.0 / 60.0),
    ] {
        let mut engine = WhitewaterLifecycle::new(
            WhitewaterGrid {
                cells: [n as u32; 3],
                cell_size: 1.0,
                origin: [0.0; 3],
            },
            10000,
            19,
        )
        .unwrap();
        engine
            .set_fields(&WhitewaterFields {
                face_u: &u,
                face_v: &v,
                face_w: &w,
                face_cells: [n as u32; 3],
                face_offset: [0; 3],
                level: &phi,
                solid: &solid,
                gravity: [0.0; 3],
            })
            .unwrap();
        let (_, samples) = whitewater_oracle::turbulence(&mut engine, &markers).unwrap();
        assert!(
            samples.iter().all(|t| *t > 200.0),
            "fixture must saturate turbulence"
        );
        whitewater_oracle::emit_configured(
            &mut engine,
            &vec![0.0; phi.len()],
            &markers,
            dt,
            EmissionOptions {
                turbulence: rate,
                influence,
                generation,
                ..EmissionOptions::default()
            },
        )
        .unwrap();
        let mut particles = Vec::new();
        engine.particles(&mut particles).unwrap();
        let expected = if generation == 0.0 {
            0
        } else {
            emission_count(1.0, [0.0, 1.0], [0.0, rate as f32], influence as f32, 8.0, 1.0, dt as f32) as usize * markers.len()
        };
        assert_eq!(
            particles.len(),
            expected,
            "rate {rate}, influence {influence}, coin {generation}, duration {dt}"
        );
        assert!(
            particles
                .iter()
                .all(|p| p.kind == manifold_fluids::WhitewaterKind::Bubble)
        );
    }
}
