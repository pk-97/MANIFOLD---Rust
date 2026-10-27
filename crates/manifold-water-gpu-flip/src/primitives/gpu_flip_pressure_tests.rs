//! Checked against FLIP Fluids pressuresolver.cpp and pcgsolver.h (MIT); see THIRD_PARTY_NOTICES.md.
//! The pressure solver module on world tiles, on the saved Dam Break and
//! deep pool problems at 64³ and 128³ and resampled to the odd sides 25 and
//! 37. Each problem is a box of cells placed across the world origin on tile
//! boundaries, in tiles the water's own allocation builds, so every level
//! holds negative tiles and its slots are the build's order. The residual is
//! the true relative residual of the masked Poisson equation.

use std::collections::{BTreeSet, HashMap, HashSet};

use manifold_gpu::testkit::{TestDevice, test_device};
use manifold_gpu::{GpuBinding, GpuBuffer, GpuDevice, GpuEncoder, GpuReplayCache};
use manifold_node_engine::particles::FluidParticle;
use manifold_node_engine::scene::world_tiles::{
    EMPTY, NONE, TILE, TILE_CELLS, find, insert, lattice_origin, local_index, neighbour_row_word,
    pool_len, slot_row_word, table_entries, table_words, tile_key, tile_of_cell,
};
use manifold_node_engine::testkit::array_harness::read;
use manifold_water_liquid::testkit::world_box::table_words_of;
use manifold_water_liquid::world_tiles::{
    Allocation, LIQUID_POINTS, POOL_SHORT, PointCount, Reach, WorldTiles, count_words,
    exhausted_word, rank_count_word,
};

use super::gpu_flip_pressure::{
    MAX_ITERATIONS, PROGRESS_FLOATS, PressureSolver, ROW_FLOATS, Solve, Stop, Water, level_pools,
    max_solve_level, passes, shader,
};

/// One saved problem: water cells and the divergence f (zero in air).
pub(crate) struct Problem {
    pub frame: u32,
    pub water: Vec<bool>,
    pub f: Vec<f32>,
}

/// A fixture under tests/fixtures: "SWFX", version 1, nx, ny, nz, count;
/// then per problem the frame, the water count, n³/8 bytes of water bits (LSB
/// first, cell x + nx·(y + ny·z)) and f32 f per water cell in cell order.
pub(crate) fn load_fixture(name: &str) -> (usize, Vec<Problem>) {
    let path = format!(
        "{}/tests/fixtures/{name}.bin.zst",
        env!("CARGO_MANIFEST_DIR")
    );
    let raw = zstd::decode_all(std::fs::File::open(&path).expect("fixture opens"))
        .expect("fixture decodes");
    let word = |at: usize| u32::from_le_bytes(raw[at..at + 4].try_into().expect("four bytes"));
    assert_eq!(&raw[..4], b"SWFX");
    assert_eq!(word(4), 1, "fixture version");
    let (n, count) = (word(8) as usize, word(20) as usize);
    assert!(
        word(12) as usize == n && word(16) as usize == n,
        "cubic fixture"
    );
    let cells = n * n * n;
    let mut at = 24;
    let problems = (0..count)
        .map(|_| {
            let (frame, wet) = (word(at), word(at + 4) as usize);
            at += 8;
            let water: Vec<bool> = (0..cells)
                .map(|c| raw[at + c / 8] >> (c % 8) & 1 == 1)
                .collect();
            at += cells / 8;
            let mut values = raw[at..at + 4 * wet]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()));
            at += 4 * wet;
            let f = water
                .iter()
                .map(|&w| {
                    if w {
                        values.next().expect("one f per water cell")
                    } else {
                        0.0
                    }
                })
                .collect();
            assert!(
                values.next().is_none(),
                "frame {frame}: more f values than water cells"
            );
            Problem { frame, water, f }
        })
        .collect();
    (n, problems)
}

/// The problem at `m` cells a side, nearest cell, as the reference's
/// `resample` (and, at a multiple of n, its `refine`) builds it.
pub(crate) fn resample(p: &Problem, n: usize, m: usize) -> Problem {
    let pick: Vec<usize> = (0..m)
        .map(|i| ((i as f64 + 0.5) * n as f64 / m as f64) as usize)
        .collect();
    let source = |c: usize| pick[c % m] + n * (pick[(c / m) % m] + n * pick[c / (m * m)]);
    Problem {
        frame: p.frame,
        water: (0..m * m * m).map(|c| p.water[source(c)]).collect(),
        f: (0..m * m * m).map(|c| p.f[source(c)]).collect(),
    }
}

/// |masked Laplacian(p) − f| / |f|: air neighbours hold zero pressure,
/// neighbours past the box walls are missing.
pub(crate) fn residual(p: &[f32], water: &[bool], f: &[f32], n: usize, h: f64) -> f64 {
    ghost_residual(p, water, f, None, n, h)
}

/// The ghost fluid's θ for air neighbour `air` of water cell `own`: the air
/// side holds θ times the water's pressure.
fn ghost_ratio(phi: &[f32], own: usize, air: usize, h: f64) -> f64 {
    let centre = f64::from(phi[own]).min(-0.005 * h);
    (f64::from(phi[air]).max(0.0) / (centre + 1e-9)).clamp(-25.0, 25.0)
}

/// [`residual`] with the free surface's ghost rows when `phi` is given: an
/// air neighbour holds θ·p of the water cell instead of 0.
fn ghost_residual(
    p: &[f32],
    water: &[bool],
    f: &[f32],
    phi: Option<&[f32]>,
    n: usize,
    h: f64,
) -> f64 {
    let (mut miss, mut size) = (0.0, 0.0);
    for c in (0..n * n * n).filter(|&c| water[c]) {
        let at = [c % n, (c / n) % n, c / (n * n)];
        let stride = [1, n, n * n];
        let centre = f64::from(p[c]);
        let mut sum = 0.0;
        for a in 0..3 {
            for next in [at[a].checked_sub(1), Some(at[a] + 1).filter(|&q| q < n)]
                .into_iter()
                .flatten()
            {
                let neighbour = c + next * stride[a] - at[a] * stride[a];
                let value = match (water[neighbour], phi) {
                    (true, _) => f64::from(p[neighbour]),
                    (false, Some(phi)) => ghost_ratio(phi, c, neighbour, h) * centre,
                    (false, None) => 0.0,
                };
                sum += value - centre;
            }
        }
        let f = f64::from(f[c]);
        miss += (sum / (h * h) - f).powi(2);
        size += f * f;
    }
    (miss / size).sqrt()
}

/// A shared buffer holding `data`, written before any GPU work sees it.
fn shared<T: bytemuck::Pod>(device: &GpuDevice, data: &[T]) -> GpuBuffer {
    let bytes: &[u8] = bytemuck::cast_slice(data);
    let buffer = device.create_buffer_shared(bytes.len().max(4) as u64);
    // SAFETY: a new shared buffer at least `bytes` long; no GPU work in flight.
    unsafe { buffer.write(0, bytes) };
    buffer
}

fn bind(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}

/// `LentineParams` of `shaders/gpu_flip_pressure.wgsl`: the Lentine flux
/// entry's dense boxes.
#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct LentineFluxParams {
    nx: u32,
    ny: u32,
    nz: u32,
    cell_size: f32,
    cx: u32,
    cy: u32,
    cz: u32,
    pad: u32,
}

fn lattice_index(p: [usize; 3], n: [usize; 3]) -> usize {
    p[0] + n[0] * (p[1] + n[1] * p[2])
}

fn lattice_coords(mut index: usize, n: [usize; 3]) -> [usize; 3] {
    let x = index % n[0];
    index /= n[0];
    let y = index % n[1];
    [x, y, index / n[1]]
}

fn lentine_cases() -> [[usize; 3]; 2] {
    [[8, 8, 8], [7, 9, 5]]
}

/// Inputs with distinct cut fractions, cell volumes, fluid velocities and
/// rigid normal velocities. The volume is attached to a cell record at its
/// low corner, as lentine_flux_main expects; the normal velocities are face
/// records and are deliberately not copied from the fluid velocity.
fn lentine_fixture(n: [usize; 3]) -> (Vec<f32>, Vec<[f32; 8]>, Vec<f32>, Vec<f32>) {
    let faces_n = n.map(|side| side + 1);
    let cells = n[0] * n[1] * n[2];
    let face_count = faces_n[0] * faces_n[1] * faces_n[2];
    let water = vec![1.0; cells];
    let mut faces = vec![[0.0; 8]; face_count];
    let mut fluid = vec![0.0; face_count * 8];
    let mut solid = vec![0.0; face_count * 8];
    let translation = [0.31, -0.22, 0.17];
    let angular = [0.07, -0.05, 0.09];
    for index in 0..face_count {
        let p = lattice_coords(index, faces_n);
        let record = &mut faces[index];
        for axis in 0..3 {
            let boundary = p[axis] == 0 || p[axis] == n[axis];
            let code = (p[0] * 5 + p[1] * 7 + p[2] * 11 + axis * 13) % 17;
            record[4 + axis] = if boundary {
                0.0
            } else {
                0.18 + 0.037 * code as f32
            };
            fluid[8 * index + axis] = -0.7 + 0.11 * (p[0] + 2 * p[1] + 3 * p[2] + axis) as f32;
            let mut center = [p[0] as f32 + 0.5, p[1] as f32 + 0.5, p[2] as f32 + 0.5];
            center[axis] = p[axis] as f32;
            let rigid = [
                translation[0] + angular[1] * center[2] - angular[2] * center[1],
                translation[1] + angular[2] * center[0] - angular[0] * center[2],
                translation[2] + angular[0] * center[1] - angular[1] * center[0],
            ];
            solid[8 * index + axis] = rigid[axis];
        }
        record[3] = 0.0;
        record[7] = if p[0] < n[0] && p[1] < n[1] && p[2] < n[2] {
            0.41 + 0.031 * ((p[0] * 3 + p[1] * 5 + p[2] * 7) % 13) as f32
        } else {
            0.0
        };
    }
    (water, faces, fluid, solid)
}

fn lentine_fine_divergence(
    q: [usize; 3],
    n: [usize; 3],
    faces: &[[f32; 8]],
    fluid: &[f32],
    solid: &[f32],
    h: f64,
) -> f64 {
    let faces_n = n.map(|side| side + 1);
    let cell = lattice_index(q, faces_n);
    let volume = f64::from(faces[cell][7]);
    let mut source = 0.0;
    for axis in 0..3 {
        for side in 0..2 {
            let mut f = q;
            f[axis] += side;
            if f[axis] == 0 || f[axis] == n[axis] {
                continue;
            }
            let face = lattice_index(f, faces_n);
            let sign = if side == 0 { -1.0 } else { 1.0 };
            let open = f64::from(faces[face][4 + axis]);
            source += sign
                * (open * f64::from(fluid[8 * face + axis])
                    + (volume - open) * f64::from(solid[8 * face + axis]));
        }
    }
    source / h
}

fn lentine_face_gather(
    p: [usize; 3],
    axis: usize,
    n: [usize; 3],
    faces: &[[f32; 8]],
    fluid: &[f32],
) -> (f64, f64) {
    let faces_n = n.map(|side| side + 1);
    let mut area = 0.0;
    let mut flux = 0.0;
    for k in 0..4 {
        let mut q = [2 * p[0], 2 * p[1], 2 * p[2]];
        let mut bit = 0;
        for (b, coordinate) in q.iter_mut().enumerate() {
            if b != axis {
                *coordinate += (k >> bit) & 1;
                bit += 1;
            }
        }
        let mut inside = q;
        inside[axis] = 0;
        if inside.iter().zip(n).all(|(&v, side)| v < side) {
            let face = lattice_index(q, faces_n);
            let weight = f64::from(faces[face][4 + axis]);
            area += weight;
            flux += weight * f64::from(fluid[8 * face + axis]);
        }
    }
    (0.25 * area, 0.25 * flux)
}

fn lentine_solid_source(
    p: [usize; 3],
    n: [usize; 3],
    faces: &[[f32; 8]],
    solid: &[f32],
) -> (f64, f64) {
    let faces_n = n.map(|side| side + 1);
    let mut source = 0.0;
    let mut volume = 0.0;
    for child in 0..8 {
        let q = [
            2 * p[0] + (child & 1),
            2 * p[1] + ((child >> 1) & 1),
            2 * p[2] + (child >> 2),
        ];
        if q.iter().zip(n).any(|(&v, side)| v >= side) {
            continue;
        }
        let cell = lattice_index(q, faces_n);
        let open_volume = f64::from(faces[cell][7]);
        volume += open_volume;
        for axis in 0..3 {
            for side in 0..2 {
                let mut f = q;
                f[axis] += side;
                if f[axis] == 0 || f[axis] == n[axis] {
                    continue;
                }
                let face = lattice_index(f, faces_n);
                let sign = if side == 0 { -1.0 } else { 1.0 };
                source += sign
                    * (open_volume - f64::from(faces[face][4 + axis]))
                    * f64::from(solid[8 * face + axis]);
            }
        }
    }
    (source, volume)
}

fn lentine_internal_solid_remainder(
    p: [usize; 3],
    n: [usize; 3],
    faces: &[[f32; 8]],
    solid: &[f32],
) -> f64 {
    let faces_n = n.map(|side| side + 1);
    let mut remainder = 0.0;
    for axis in 0..3 {
        for k in 0..4 {
            let mut low = [2 * p[0], 2 * p[1], 2 * p[2]];
            let mut bit = 0;
            for (b, coordinate) in low.iter_mut().enumerate() {
                if b != axis {
                    *coordinate += (k >> bit) & 1;
                    bit += 1;
                }
            }
            let mut high = low;
            high[axis] += 1;
            if high.iter().zip(n).any(|(&v, side)| v >= side) {
                continue;
            }
            let low_cell = lattice_index(low, faces_n);
            let high_cell = lattice_index(high, faces_n);
            let face = lattice_index(high, faces_n);
            let cut_low = f64::from(faces[low_cell][7]) - f64::from(faces[face][4 + axis]);
            let cut_high = f64::from(faces[high_cell][7]) - f64::from(faces[face][4 + axis]);
            remainder += (cut_low - cut_high) * f64::from(solid[8 * face + axis]);
        }
    }
    remainder
}

fn assert_lentine_close(actual: impl Into<f64>, expected: f64, what: &str) {
    let actual = actual.into();
    let tolerance = 2.0e-5 * (1.0 + expected.abs());
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: GPU {actual:.8e}, CPU {expected:.8e}, tolerance {tolerance:.3e}"
    );
}

/// The conservative Lentine gather preserves the average fine divergence,
/// including rigid motion in cut cells. The CPU oracle uses f64 accumulation;
/// the tolerance allows only the shader's f32 operation order. Odd sides also
/// prove that a virtual child contributes no fluid or solid source.
#[test]
fn pressure_module_lentine_flux_conserves_blocks() {
    let shader = shader();
    for n in lentine_cases() {
        let c = n.map(|side| side.div_ceil(2));
        let output_n = c.map(|side| side + 1);
        let h = 0.37_f32;
        let (water_values, face_values, fluid_values, solid_values) = lentine_fixture(n);
        let device = test_device();
        let pipeline = device.create_compute_pipeline(
            &shader,
            "lentine_flux_main",
            "gpu-flip-pressure-lentine-flux",
        );
        // The gather's lattices are dense boxes.
        let water = shared(&device, &water_values);
        let faces = shared(&device, &face_values);
        let fluid = shared(&device, &fluid_values);
        let solid = shared(&device, &solid_values);
        let output = shared(
            &device,
            &vec![f32::from_bits(0x7fc00000); output_n[0] * output_n[1] * output_n[2] * 8],
        );
        let params = LentineFluxParams {
            nx: n[0] as u32,
            ny: n[1] as u32,
            nz: n[2] as u32,
            cell_size: h,
            cx: c[0] as u32,
            cy: c[1] as u32,
            cz: c[2] as u32,
            pad: 0,
        };
        let mut encoder = device.create_encoder("gpu-flip-pressure-lentine-flux");
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 27,
                    data: bytemuck::bytes_of(&params),
                },
                bind(1, &water),
                bind(2, &faces),
                bind(3, &fluid),
                bind(4, &solid),
                bind(9, &output),
            ],
            [
                ((output_n[0] * output_n[1] * output_n[2]) as u32).div_ceil(256),
                1,
                1,
            ],
            "gpu-flip-pressure-lentine-flux",
        );
        encoder.commit_and_wait_completed();
        let actual: Vec<[f32; 8]> = read(&output, output_n[0] * output_n[1] * output_n[2]);
        let blocks = c[0] * c[1] * c[2];
        let mut internal_solid_activity = 0.0;
        for block in 0..blocks {
            let p = lattice_coords(block, c);
            let out_index = lattice_index(p, output_n);
            let mut fluid_difference = 0.0;
            for axis in 0..3 {
                let low = p;
                let mut high = p;
                high[axis] += 1;
                let (low_flux, high_flux) = if p[axis] == 0 {
                    (
                        0.0,
                        if p[axis] + 1 < c[axis] {
                            f64::from(actual[lattice_index(high, output_n)][axis])
                        } else {
                            0.0
                        },
                    )
                } else if p[axis] + 1 == c[axis] {
                    (f64::from(actual[lattice_index(low, output_n)][axis]), 0.0)
                } else {
                    (
                        f64::from(actual[lattice_index(low, output_n)][axis]),
                        f64::from(actual[lattice_index(high, output_n)][axis]),
                    )
                };
                fluid_difference += (high_flux - low_flux) / (2.0 * f64::from(h));
                let (area, flux) = if p[axis] == 0 {
                    (0.0, 0.0)
                } else {
                    lentine_face_gather(p, axis, n, &face_values, &fluid_values)
                };
                assert_lentine_close(
                    actual[out_index][axis + 4],
                    area,
                    &format!("{n:?} block {p:?} axis {axis} area"),
                );
                assert_lentine_close(
                    actual[out_index][axis],
                    flux,
                    &format!("{n:?} block {p:?} axis {axis} flux"),
                );
                if p[axis] == 0 {
                    assert_lentine_close(
                        actual[out_index][axis],
                        0.0,
                        &format!("{n:?} block {p:?} axis {axis} wall"),
                    );
                    assert_lentine_close(
                        actual[out_index][axis + 4],
                        0.0,
                        &format!("{n:?} block {p:?} axis {axis} wall area"),
                    );
                }
            }
            let (solid_source, volume) = lentine_solid_source(p, n, &face_values, &solid_values);
            internal_solid_activity +=
                lentine_internal_solid_remainder(p, n, &face_values, &solid_values).abs();
            assert_lentine_close(
                actual[out_index][3],
                solid_source / (8.0 * f64::from(h)),
                &format!("{n:?} block {p:?} solid source"),
            );
            assert_lentine_close(
                actual[out_index][7],
                volume / 8.0,
                &format!("{n:?} block {p:?} volume"),
            );
            let fine_average = (0..8)
                .map(|child| {
                    let q = [
                        2 * p[0] + (child & 1),
                        2 * p[1] + ((child >> 1) & 1),
                        2 * p[2] + (child >> 2),
                    ];
                    if q.iter().zip(n).any(|(&v, side)| v >= side) {
                        0.0
                    } else {
                        lentine_fine_divergence(
                            q,
                            n,
                            &face_values,
                            &fluid_values,
                            &solid_values,
                            f64::from(h),
                        )
                    }
                })
                .sum::<f64>()
                / 8.0;
            assert_lentine_close(
                f64::from(actual[out_index][3]) + fluid_difference,
                fine_average,
                &format!("{n:?} block {p:?} divergence"),
            );
        }
        for (index, record) in actual.iter().enumerate() {
            let p = lattice_coords(index, output_n);
            let padded = p.iter().zip(c).any(|(&v, side)| v >= side);
            for axis in 0..3 {
                if padded || p[axis] == 0 || p[axis] == c[axis] {
                    assert_lentine_close(
                        record[axis],
                        0.0,
                        &format!("{n:?} output padding {p:?} velocity {axis}"),
                    );
                    assert_lentine_close(
                        record[axis + 4],
                        0.0,
                        &format!("{n:?} output padding {p:?} area {axis}"),
                    );
                }
            }
            if padded {
                assert_lentine_close(
                    record[3],
                    0.0,
                    &format!("{n:?} output padding {p:?} solid source"),
                );
                assert_lentine_close(
                    record[7],
                    0.0,
                    &format!("{n:?} output padding {p:?} volume"),
                );
            }
        }
        assert!(
            internal_solid_activity > 1.0e-3,
            "{n:?}: fixture did not exercise (c_low-c_high)*v_s"
        );
    }
}

const DAM_BREAK: &str = "dambreak_pressure_problems";

/// A spread of surface distances: water cells by an air neighbour sit 0.01h
/// to 0.9h under the surface, air cells 0.05h to 0.95h above it, so θ runs
/// the whole clamp from 0 to −25. Deeper cells sit 1.5h from it.
fn surface_phi(water: &[bool], n: usize, h: f32) -> Vec<f32> {
    let spread = |c: usize| ((c as u64).wrapping_mul(2_654_435_761) >> 8) as f32 % 1000.0 / 1000.0;
    (0..n * n * n)
        .map(|c| {
            let at = [c % n, (c / n) % n, c / (n * n)];
            let stride = [1, n, n * n];
            let by_other = (0..3).any(|a| {
                [at[a].checked_sub(1), Some(at[a] + 1).filter(|&q| q < n)]
                    .into_iter()
                    .flatten()
                    .any(|q| water[c + q * stride[a] - at[a] * stride[a]] != water[c])
            });
            match (water[c], by_other) {
                (true, true) => -h * (0.01 + 0.89 * spread(c)),
                (false, true) => h * (0.05 + 0.9 * spread(c)),
                (true, false) => -1.5 * h,
                (false, false) => 1.5 * h,
            }
        })
        .collect()
}

/// The free surface's ghost rows on the finest level, coarse levels plain,
/// against the CPU's ghost residual: 8 iterations within 2× of what 16
/// reach or the f32 floor, every Dam Break frame at 64 and the odd side 37.
/// The ghost answer misses the plain equation, so the rows took.
#[test]
fn pressure_module_solves_the_ghost_rows() {
    let (n, problems) = load_fixture(DAM_BREAK);
    let mut failures = Vec::new();
    for m in [64, 37] {
        let h = (BOX_METRES / m as f64) as f32;
        for problem in &problems {
            let problem = resample(problem, n, m);
            let phi = surface_phi(&problem.water, m, h);
            let mut rig = Rig::new(m).with_phi(&phi);
            let ghost = |rig: &Rig| {
                ghost_residual(
                    &rig.pressure(),
                    &problem.water,
                    &problem.f,
                    Some(&phi),
                    m,
                    f64::from(h),
                )
            };
            rig.run(&problem, 16, false);
            let floor = ghost(&rig).max(F32_FLOOR);
            rig.run(&problem, 3, false);
            let at3 = ghost(&rig);
            rig.run(&problem, 8, false);
            let at8 = ghost(&rig);
            let plain = rig.residual_of(&problem);
            println!(
                "ghost rows {m}³ frame {:3}: 3 iterations {at3:.3e}, 8 iterations {at8:.3e} (floor {floor:.3e}); plain residual of the ghost answer {plain:.3e}",
                problem.frame
            );
            if at8.is_nan() || at8 > 2.0 * floor || at8 > at3 {
                failures.push(format!(
                    "{m}³ frame {}: 8 iterations {at8:.3e}, 3 {at3:.3e}, floor {floor:.3e}",
                    problem.frame
                ));
            }
            if plain < 10.0 * at8 {
                failures.push(format!(
                    "{m}³ frame {}: the plain residual {plain:.3e} is as small as the ghost one",
                    problem.frame
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The box side in metres the fixtures were saved at.
const BOX_METRES: f64 = 4.0;

/// The residual f32 arithmetic holds on these problems: past it the f64
/// reference keeps falling and the GPU cannot follow.
const F32_FLOOR: f64 = 3e-5;

/// One solve of a rig's problem at its level, no bodies; a macro so the
/// solver stays borrowable beside the rig's buffers.
macro_rules! run_at {
    ($rig:expr, $stop:expr) => {
        Solve {
            rhs: &$rig.rhs,
            pressure: &$rig.pressure,
            stop: $stop,
            bodies: None,
            level: $rig.level,
            coarse_rhs: None,
        }
    };
}

/// A rig's water as the solver takes it; a macro for the same reason.
macro_rules! water_of {
    ($rig:expr) => {
        Water {
            cell_size: $rig.h as f32,
            water: &$rig.water,
            faces: &$rig.faces,
            phi: $rig.phi.as_ref(),
        }
    };
}

/// World tiles built by the water's own allocation from one particle in
/// each of a set of tiles, so the slots are the build's order, not the
/// tiles'.
struct FineTiles {
    tiles: WorldTiles,
    /// Each slot's tile; every slot is held.
    tile_of: Vec<[i32; 3]>,
    slot_of: HashMap<[i32; 3], u32>,
}

impl FineTiles {
    fn build(device: &GpuDevice, h: f32, tiles: &[[i32; 3]]) -> Self {
        let points: Vec<FluidParticle> = tiles
            .iter()
            .enumerate()
            .map(|(i, t)| {
                // The tile's first cell, at its centre.
                let at = t.map(|c| (c * TILE as i32) as f32 * h);
                FluidParticle {
                    position_radius: [at[0], at[1], at[2], 0.05],
                    velocity: [0.0; 3],
                    id: i as u32 + 1,
                }
            })
            .collect();
        let count = points.len() as u32;
        let reach = Reach {
            ring_max: 0,
            alloc_ring: 0,
            cell_reach: 0,
        };
        let mut built = WorldTiles::new(device, count, reach, LIQUID_POINTS).expect("world tiles");
        built.prepare(device);
        let records = shared(device, &points);
        let record = shared(device, &[count.div_ceil(256), 1, 1, count]);
        let gate = shared(device, &[0u32; 12]);
        let mut enc = device.create_encoder("pressure rig tiles");
        built.encode_allocation(
            &mut enc,
            &Allocation {
                points: &records,
                count: PointCount::Record(&record),
                origin: lattice_origin(h),
                inv_cell: 1.0 / h,
                replay_count: 0,
                gate: &gate,
                clip: None,
                kept: None,
            },
        );
        enc.commit_and_wait_completed();
        let words: Vec<u32> = read(built.table(), table_words(count) as usize);
        let entries = table_entries(count);
        let tile_of: Vec<[i32; 3]> = (0..count)
            .map(|slot| {
                let row = slot_row_word(entries, slot) as usize;
                assert_ne!(words[row + 3], NONE, "slot {slot} holds a tile");
                [words[row], words[row + 1], words[row + 2]].map(|w| w as i32)
            })
            .collect();
        let slot_of: HashMap<[i32; 3], u32> = tile_of
            .iter()
            .enumerate()
            .map(|(slot, &t)| (t, slot as u32))
            .collect();
        assert!(
            tiles.iter().all(|t| slot_of.contains_key(t)),
            "every tile has its own slot"
        );
        Self {
            tiles: built,
            tile_of,
            slot_of,
        }
    }
}

/// The low corner of a box of `n` cells a side placed across the origin on
/// tile boundaries, a different number of tiles below 0 on each axis: the
/// box holds ⌈n/8⌉³ tiles, so the solver's depth is the dense box's.
fn across_origin(n: usize) -> [i32; 3] {
    let tiles = n.div_ceil(TILE as usize) as i32;
    assert!(tiles >= 2, "a box across the origin spans two tiles");
    [0, -1, 1].map(|d| -(TILE as i32) * (tiles / 2 + d).clamp(1, tiles - 1))
}

/// Every cell of a box of `side` cells from `min`, x fastest.
fn box_cells(min: [i32; 3], side: i32) -> impl Iterator<Item = [i32; 3]> {
    (0..side)
        .flat_map(move |z| (0..side).flat_map(move |y| (0..side).map(move |x| [x, y, z])))
        .map(move |c| std::array::from_fn(|a| min[a] + c[a]))
}

/// Every cell of `tile`, x fastest.
fn tile_cells(tile: [i32; 3]) -> impl Iterator<Item = [i32; 3]> {
    box_cells(tile.map(|t| t * TILE as i32), TILE as i32)
}

/// The solver on a set of cells in world tiles, its inputs in shared
/// buffers. A rig's dense arrays run over its cells (a box's x fastest);
/// it converts them to the pooled layout and its outputs back.
struct Rig {
    /// The box side the problems are posed on; 0 for any other cell set.
    n: usize,
    h: f64,
    /// The world cell of each dense entry.
    cells: Vec<[i32; 3]>,
    /// The pool index of each dense entry.
    index: Vec<usize>,
    fine: FineTiles,
    device: TestDevice,
    solver: PressureSolver,
    water: GpuBuffer,
    faces: GpuBuffer,
    rhs: GpuBuffer,
    pressure: GpuBuffer,
    /// The free surface's distance, when the rig solves the ghost rows.
    phi: Option<GpuBuffer>,
    /// The V-cycle level the gradient runs on; 0 is the fine level.
    level: usize,
    /// Solves start from the pressure the last one left (the warm start);
    /// off, every upload zeroes it and the solve starts cold.
    keep_pressure: bool,
}

impl Rig {
    fn at_level(mut self, level: usize) -> Self {
        self.level = level;
        self
    }

    /// A box of `n`³ cells across the origin ([`across_origin`]), cell size
    /// BOX_METRES / n, its walls closed.
    fn new(n: usize) -> Self {
        let min = across_origin(n);
        let side = n as i32;
        let cells: Vec<[i32; 3]> = box_cells(min, side).collect();
        let (first, last) = (tile_of_cell(min), tile_of_cell(min.map(|c| c + side - 1)));
        let mut tiles = Vec::new();
        for z in first[2]..=last[2] {
            for y in first[1]..=last[1] {
                for x in first[0]..=last[0] {
                    tiles.push([x, y, z]);
                }
            }
        }
        let inside = |p: [i32; 3]| (0..3).all(|a| p[a] >= min[a] && p[a] < min[a] + side);
        let mut rig = Self::on(cells, &tiles, BOX_METRES / n as f64, inside);
        rig.n = n;
        rig
    }

    /// The solver over `cells` in `tiles` at cell size `h`: a face is open
    /// between two cells `inside` holds, every other face closed.
    fn on(
        cells: Vec<[i32; 3]>,
        tiles: &[[i32; 3]],
        h: f64,
        inside: impl Fn([i32; 3]) -> bool,
    ) -> Self {
        let device = test_device();
        let fine = FineTiles::build(&device, h as f32, tiles);
        let pool = fine.tiles.pool_tiles();
        let at = |slot: usize, p: [i32; 3]| slot * TILE_CELLS as usize + local_index(p) as usize;
        // Cells run x fastest, so a tile's slot is looked up once a run.
        let mut last = ([i32::MIN; 3], 0);
        let index: Vec<usize> = cells
            .iter()
            .map(|&p| {
                let tile = tile_of_cell(p);
                if tile != last.0 {
                    last = (tile, fine.slot_of[&tile] as usize);
                }
                at(last.1, p)
            })
            .collect();
        // A record holds its cell's low faces; the canonical one is closed.
        let mut records = vec![[0.0f32; 8]; pool_len(pool) as usize];
        for (slot, &tile) in fine.tile_of.iter().enumerate() {
            for p in tile_cells(tile).filter(|&p| inside(p)) {
                for a in 0..3 {
                    let mut below = p;
                    below[a] -= 1;
                    if inside(below) {
                        records[at(slot, p)][4 + a] = 1.0;
                    }
                }
            }
        }
        let floats = pool_len(pool) * 4;
        let mut solver = PressureSolver::default();
        solver.prepare_pipelines(&device);
        Self {
            n: 0,
            h,
            cells,
            index,
            faces: shared(&device, &records),
            water: device.create_buffer_shared(floats),
            rhs: device.create_buffer_shared(floats),
            pressure: device.create_buffer_shared(floats),
            fine,
            device,
            solver,
            phi: None,
            level: 0,
            keep_pressure: false,
        }
    }

    fn pool_floats(&self) -> usize {
        pool_len(self.fine.tiles.pool_tiles()) as usize
    }

    /// `dense` as a pooled field: each cell in place, everything else `fill`.
    fn to_pool<T: Copy>(&self, dense: &[T], fill: T) -> Vec<T> {
        assert_eq!(dense.len(), self.cells.len());
        let mut pool = vec![fill; self.pool_floats()];
        for (&at, &value) in self.index.iter().zip(dense) {
            pool[at] = value;
        }
        pool
    }

    /// The rig's cells out of a pooled field.
    fn dense_of<T: Copy>(&self, pool: &[T]) -> Vec<T> {
        self.index.iter().map(|&at| pool[at]).collect()
    }

    /// φ per cell; air (3h) everywhere else.
    fn with_phi(mut self, phi: &[f32]) -> Self {
        let pooled = self.to_pool(phi, 3.0 * self.h as f32);
        self.phi = Some(shared(&self.device, &pooled));
        self
    }

    /// Dense water (1 water, 0 air) and right-hand side into the pooled
    /// buffers, 0 off the cells. The pressure is zeroed unless the rig keeps
    /// it.
    fn upload(&self, water: &[f32], rhs: &[f32]) {
        let (water, rhs) = (self.to_pool(water, 0.0f32), self.to_pool(rhs, 0.0f32));
        // SAFETY: shared buffers sized for the pool; the last solve completed.
        unsafe {
            self.water.write(0, bytemuck::cast_slice(&water));
            self.rhs.write(0, bytemuck::cast_slice(&rhs));
            if !self.keep_pressure {
                let zeros = vec![0u32; self.pool_floats()];
                self.pressure.write(0, bytemuck::cast_slice(&zeros));
            }
        }
    }

    /// The fine pressure over the rig's cells.
    fn pressure(&self) -> Vec<f32> {
        self.dense_of(&read::<f32>(&self.pressure, self.pool_floats()))
    }

    fn cell_size(&self) -> f64 {
        self.h
    }

    fn prepare(&mut self, enc: &mut GpuEncoder) {
        let water = water_of!(self);
        self.solver
            .prepare(&self.device, enc, &water, &self.fine.tiles)
            .expect("prepares");
    }

    fn solve_on(&mut self, enc: &mut GpuEncoder, stop: Stop) {
        let water = water_of!(self);
        self.solver
            .solve(enc, &water, run_at!(self, stop), &self.fine.tiles)
            .expect("solves");
    }

    /// One prepare and solve in its own command buffer: the profile.
    fn run(
        &mut self,
        p: &Problem,
        iterations: u32,
        profile: bool,
    ) -> manifold_gpu::GpuFrameProfile {
        let water: Vec<f32> = p.water.iter().map(|&w| f32::from(u8::from(w))).collect();
        self.upload(&water, &p.f);
        let mut enc = self.device.create_encoder("gpu-flip-pressure-module");
        if profile {
            let sampler = self
                .device
                .create_timestamp_sampler(8192)
                .expect("timestamp sampling");
            enc.enable_dispatch_profiling(sampler, &self.device);
        }
        self.prepare(&mut enc);
        self.solve_on(&mut enc, Stop::Fixed(iterations));
        enc.commit_and_wait_profiled(&self.device)
    }

    /// The last solve's residual on the fine level, as the problem is posed.
    fn residual_of(&self, p: &Problem) -> f64 {
        residual(&self.pressure(), &p.water, &p.f, self.n, self.cell_size())
    }

    fn solve(&mut self, p: &Problem, iterations: u32) -> f64 {
        self.run(p, iterations, false);
        self.residual_of(p)
    }

    /// The fine level's slot flags (1 active) and active list, P words each,
    /// and its gate triple, after the last prepare.
    fn slot_words(&self) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
        let pool = u64::from(self.fine.tiles.pool_tiles());
        let [armed, flags, lists] = self.solver.tiles().expect("prepared");
        let mut enc = self.device.create_encoder("gpu-flip-pressure-slot-words");
        let copies = [flags, lists].map(|from| {
            let into = self.device.create_buffer_shared(pool * 4);
            enc.copy_buffer_to_buffer(from, &into, pool * 4);
            into
        });
        enc.commit_and_wait_completed();
        (
            read(&copies[0], pool as usize),
            read(&copies[1], pool as usize),
            read(armed, 3),
        )
    }
}

/// The classifier's boolean rule per slot of `rig`: active when a water
/// cell (> 0.5) lies in the slot's tile grown by one cell, or with
/// `all_tiles` every slot.
fn slot_flags(rig: &Rig, water: &[f32], all_tiles: bool) -> Vec<u32> {
    let mut lit = vec![u32::from(all_tiles); rig.fine.tile_of.len()];
    for (c, _) in water.iter().enumerate().filter(|&(_, &w)| w > 0.5) {
        let p = rig.cells[c];
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let t = tile_of_cell([p[0] + dx, p[1] + dy, p[2] + dz]);
                    if let Some(&slot) = rig.fine.slot_of.get(&t) {
                        lit[slot as usize] = 1;
                    }
                }
            }
        }
    }
    lit
}

/// Classification through prepare against the boolean rule, on boxes with
/// a partial edge tile and without: a held slot is active when a water cell
/// lies in its tile grown by one cell, the list holds the active slots in
/// slot order and the level's triple two workgroups each. Exactly 0.5 is
/// air, its next representable neighbour water. All tiles marks every held
/// slot; an inactive clock slot keeps the last classification.
#[test]
fn pressure_module_parallel_classification_matches_serial_boolean_oracle() {
    use super::gpu_flip_step::set_all_tiles;
    for m in [12, 25, 64] {
        let n = [m; 3];
        let cells = m * m * m;
        let mut rig = Rig::new(m);
        let plan = rig.device.create_buffer_shared(48);
        rig.solver.set_clock_plan(&plan);
        let mut expected: Option<(Vec<u32>, Vec<u32>)> = None;
        // Empty -> full -> sparse -> halo-only -> empty reuses the storage, then
        // all_tiles and an inactive clock prove the override and preservation.
        for (mask, all_tiles, inactive) in [
            (0, false, false),
            (1, false, false),
            (2, false, false),
            (3, false, false),
            (0, false, false),
            (0, true, false),
            (0, false, true),
            (3, false, false),
        ] {
            let mut water = vec![if mask == 1 { 1.0_f32 } else { 0.0 }; cells];
            if mask == 2 {
                for p in [
                    [0; 3],
                    n.map(|v| v - 1),
                    n.map(|v| 7.min(v - 1)),
                    n.map(|v| 9.min(v - 1)),
                ] {
                    water[lattice_index(p, n)] = 1.0;
                }
                for (index, value) in [
                    0.5_f32,
                    0.5_f32.next_down(),
                    0.5_f32.next_up(),
                    -1.0,
                    f32::NAN,
                ]
                .into_iter()
                .enumerate()
                {
                    water[index] = value;
                }
            } else if mask == 3 {
                // At x = 8 the cell touches the first tile's halo and the second tile.
                water[lattice_index([8, 0, 0], n)] = 1.0;
            }
            let mut clock = [0u32; 12];
            clock[11] = 1;
            clock[0] = if inactive { 0.0_f32 } else { 1.0_f32 / 60.0 }.to_bits();
            // SAFETY: a shared buffer sized for the plan; the last prepare completed.
            unsafe { plan.write(0, bytemuck::cast_slice(&clock)) };
            rig.upload(&water, &vec![0.0; cells]);
            set_all_tiles(all_tiles);
            let mut enc = rig.device.create_encoder("pressure classifier proof");
            rig.prepare(&mut enc);
            enc.commit_and_wait_completed();
            set_all_tiles(false);
            if !inactive {
                let flags = slot_flags(&rig, &water, all_tiles);
                let list = (0..flags.len() as u32)
                    .filter(|&s| flags[s as usize] != 0)
                    .collect();
                expected = Some((flags, list));
            }
            let (want_flags, want_list) = expected.as_ref().expect("an active prepare first");
            let (flags, lists, armed) = rig.slot_words();
            let context = format!("{m}³ mask {mask}, all_tiles {all_tiles}, inactive {inactive}");
            assert_eq!(&flags, want_flags, "flags: {context}");
            assert_eq!(&lists[..want_list.len()], &want_list[..], "list: {context}");
            assert_eq!(
                armed,
                [2 * want_list.len() as u32, 1, 1],
                "armed counts: {context}"
            );
        }
    }
}

/// The solve encodes exactly the passes `passes` counts, every one labelled
/// as the solver's (the coarse levels' world tile builds aside); and its GPU
/// time per solve, printed.
#[test]
fn pressure_module_passes_match_the_count() {
    let (n, problems) = load_fixture(DAM_BREAK);
    for m in [37, 64, 128] {
        let mut rig = Rig::new(m);
        let problem = resample(&problems[4], n, m);
        rig.run(&problem, 8, false);
        let profile = rig.run(&problem, 8, true);
        assert_eq!(profile.overflow, 0);
        assert_eq!(profile.invalid, 0);
        assert_eq!(profile.failed_command_buffers, 0);
        let mut costs = std::collections::BTreeMap::<&str, (usize, f64)>::new();
        for span in &profile.spans {
            let row = costs.entry(&span.label).or_default();
            row.0 += 1;
            row.1 += span.millis;
        }
        for (label, (count, ms)) in costs {
            println!("pressure active-pass {m}: {label} count {count} sampled_ms {ms:.5}");
        }
        let pool = rig.fine.tiles.pool_tiles();
        let (prepare, solve) = passes(pool, Stop::Fixed(8));
        // The rounds run as the template's executes, one span each at
        // Dispatch granularity; the prelude (arm, init, the start's check,
        // the warm start's residual) is what a solve encodes besides them.
        let round = passes(pool, Stop::Fixed(2)).1 - passes(pool, Stop::Fixed(1)).1;
        let prelude = solve - 8 * round;
        let executes = manifold_gpu::template_chunks(
            8,
            super::gpu_flip_pressure::ROUND_CHUNK,
            super::gpu_flip_pressure::SINGLE_ROUNDS,
        )
        .count();
        let own = profile
            .spans
            .iter()
            .filter(|s| s.label.starts_with("gpu_flip.pressure."))
            .count();
        assert_eq!(
            own,
            prepare + prelude,
            "{m}³ ({} levels)",
            level_pools(pool).len()
        );
        assert_eq!(
            profile
                .spans
                .iter()
                .filter(|s| s.label == "pressure rounds")
                .count(),
            executes,
            "{m}³: one span per execute"
        );
        assert!(
            profile
                .spans
                .iter()
                .all(|s| s.label.starts_with("gpu_flip.pressure.")
                    || s.label.starts_with("world_tiles.")
                    || s.label == "pressure rounds"),
            "{m}³: an unlabelled pass"
        );
        let mut times: Vec<f64> = (0..5)
            .map(|_| rig.run(&problem, 8, false).total_ms)
            .collect();
        times.sort_by(f64::total_cmp);
        println!(
            "pressure module {m}³: {prepare} + {solve} passes, {:.2} ms GPU per prepare and 8-iteration solve (median of 5)",
            times[2]
        );
    }
}

/// (r·z, p·s) of every iteration of one solve of `p`, `iterations` long.
fn rz(rig: &mut Rig, p: &Problem, iterations: u32) -> Vec<(f32, f32)> {
    let water: Vec<f32> = p.water.iter().map(|&w| f32::from(u8::from(w))).collect();
    rig.upload(&water, &p.f);
    let scalars = rig
        .device
        .create_buffer_shared(u64::from(2 * MAX_ITERATIONS) * 4);
    let mut enc = rig.device.create_encoder("gpu-flip-pressure-rz");
    rig.prepare(&mut enc);
    rig.solve_on(&mut enc, Stop::Fixed(iterations));
    rig.solver.copy_scalars(&mut enc, &scalars);
    enc.commit_and_wait_completed();
    let all: Vec<f32> = read(&scalars, 2 * MAX_ITERATIONS as usize);
    (0..iterations as usize)
        .map(|k| (all[2 * k], all[2 * k + 1]))
        .collect()
}

/// A still pool: the lower half of the box is water, its divergence a fixed
/// pseudo-random spread.
fn still_pool_problem(m: usize) -> Problem {
    let water: Vec<bool> = (0..m * m * m).map(|c| (c / m) % m < m / 2).collect();
    let f = (0..m * m * m)
        .map(|c| {
            if water[c] {
                ((c as u64).wrapping_mul(2_654_435_761) >> 8) as f32 % 1000.0 / 500.0 - 1.0
            } else {
                0.0
            }
        })
        .collect();
    Problem { frame: 0, water, f }
}

/// The V-cycle preconditioner re-derives L at each coarse level rather than
/// taking R·L·P, so its definiteness is not given by construction. The solve
/// runs CG on A = −L (s = A p, p·s > 0) with z = V(r) ≈ L⁻¹ r = −A⁻¹ r, so the
/// preconditioner −V is positive definite exactly when r·z < 0. Every
/// iteration of every solve keeps both signs on the shipped problems (the Dam
/// Break, the deep pool, a still pool), plain and with the ghost rows. A sign
/// flip is a finding to report, not to patch.
#[test]
fn pressure_module_preconditioner_keeps_rz_negative() {
    let mut problems: Vec<(String, usize, Problem)> = Vec::new();
    for fixture in [
        DAM_BREAK,
        "deep_pool_pressure_problems",
        "deep_pool_density_problems",
    ] {
        let (n, saved) = load_fixture(fixture);
        for m in [64, 37] {
            for p in &saved {
                problems.push((fixture.to_string(), m, resample(p, n, m)));
            }
        }
    }
    for m in [64, 37] {
        problems.push(("still_pool".into(), m, still_pool_problem(m)));
    }
    let mut failures = Vec::new();
    let mut checked = 0;
    for (name, m, problem) in &problems {
        let h = (BOX_METRES / *m as f64) as f32;
        for ghost in [false, true] {
            let mut rig = Rig::new(*m);
            if ghost {
                rig = rig.with_phi(&surface_phi(&problem.water, *m, h));
            }
            let values = rz(&mut rig, problem, 16);
            checked += values.len();
            if let Some((k, (rz, ps))) = values
                .iter()
                .enumerate()
                .find(|(_, (rz, ps))| rz.is_nan() || ps.is_nan() || *rz >= 0.0 || *ps <= 0.0)
            {
                failures.push(format!("{name} {m}³ frame {} ghost {ghost}: iteration {k} r·z {rz:e}, p·s {ps:e} ({values:?})", problem.frame));
            }
        }
    }
    println!(
        "preconditioner: {checked} iterations over {} solves, r·z < 0 and p·s > 0 on every one: {}",
        problems.len() * 2,
        failures.is_empty()
    );
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The full cap runs and stays sane: a small still pool solved for all 900
/// rounds (no tolerance stop) keeps every r·z and p·s finite, its pressure
/// finite, and its residual at or below where 16 rounds left it, which is
/// already well below 1 rounds'. Long past convergence f32 CG only wanders at
/// rounding level; a blow-up or NaN would fail here.
#[test]
fn pressure_module_runs_the_full_cap() {
    let m = 12;
    let problem = still_pool_problem(m);
    let mut rig = Rig::new(m);
    let after = |rig: &mut Rig, k: u32| rig.solve(&problem, k);
    let (one, sixteen) = (after(&mut rig, 1), after(&mut rig, 16));
    let full = after(&mut rig, MAX_ITERATIONS);
    assert!(
        rig.pressure().iter().all(|v| v.is_finite()),
        "pressure finite after {MAX_ITERATIONS} rounds"
    );
    let values = rz(&mut rig, &problem, MAX_ITERATIONS);
    assert_eq!(values.len(), MAX_ITERATIONS as usize);
    assert!(
        values
            .iter()
            .all(|(rz, ps)| rz.is_finite() && ps.is_finite()),
        "every round's scalars finite"
    );
    println!("full cap {m}³: residual after 1 {one:e}, 16 {sixteen:e}, {MAX_ITERATIONS} {full:e}");
    assert!(
        sixteen < one * 1e-2,
        "16 rounds converge ({one:e} → {sixteen:e})"
    );
    assert!(
        full <= sixteen.max(1e-5),
        "{MAX_ITERATIONS} rounds stay converged ({sixteen:e} → {full:e})"
    );
}

/// One solve of `p` on the engine's stop, at most `cap` iterations: |f|∞,
/// the iterations run, whether the tolerance stopped it, and |r|∞ after each.
struct Converged {
    f_norm: f32,
    iterations: u32,
    stopped: bool,
    norms: Vec<f32>,
}

fn converge(rig: &mut Rig, p: &Problem, cap: u32) -> Converged {
    let water: Vec<f32> = p.water.iter().map(|&w| f32::from(u8::from(w))).collect();
    rig.upload(&water, &p.f);
    let floats = PROGRESS_FLOATS as usize;
    let record = rig.device.create_buffer_shared(floats as u64 * 4);
    let mut enc = rig.device.create_encoder("gpu-flip-pressure-converge");
    rig.prepare(&mut enc);
    rig.solve_on(&mut enc, Stop::Converged(cap));
    let progress = rig.solver.progress().expect("prepared");
    enc.copy_buffer_to_buffer(progress, &record, record.size);
    enc.commit_and_wait_completed();
    let all: Vec<f32> = read(&record, floats);
    let iterations = all[1] as u32;
    Converged {
        f_norm: all[0],
        iterations,
        stopped: all[2] > 0.5,
        norms: all[4..4 + iterations as usize].to_vec(),
    }
}

/// One prepare and solve of `p` on `stop`, inside a replay span when `cache`
/// holds one (the step's region on stage): the pressure and the stop's
/// record, as bits.
fn solve_bits(
    rig: &mut Rig,
    p: &Problem,
    stop: Stop,
    cache: &mut Option<GpuReplayCache>,
) -> (Vec<u32>, Vec<u32>) {
    solve_bits_under(rig, p, stop, cache, false)
}

/// [`solve_bits`], poisoning the solver's vectors outside its active slots
/// between the prepare and the solve when `poison`.
fn solve_bits_under(
    rig: &mut Rig,
    p: &Problem,
    stop: Stop,
    cache: &mut Option<GpuReplayCache>,
    poison: bool,
) -> (Vec<u32>, Vec<u32>) {
    let water: Vec<f32> = p.water.iter().map(|&w| f32::from(u8::from(w))).collect();
    rig.upload(&water, &p.f);
    let floats = PROGRESS_FLOATS as usize;
    let record = rig.device.create_buffer_shared(floats as u64 * 4);
    let mut enc = rig.device.create_encoder("gpu-flip-pressure-replay");
    let spanned = cache
        .take()
        .map(|cache| enc.begin_replay(&rig.device, cache))
        .is_some();
    rig.prepare(&mut enc);
    if poison {
        rig.solver
            .poison(&mut enc, &water_of!(rig), rig.level, &rig.fine.tiles);
    }
    rig.solve_on(&mut enc, stop);
    if spanned {
        *cache = Some(enc.end_replay());
    }
    let progress = rig.solver.progress().expect("prepared");
    enc.copy_buffer_to_buffer(progress, &record, record.size);
    enc.commit_and_wait_completed();
    (
        bytemuck::cast_slice(&rig.pressure()).to_vec(),
        read(&record, floats),
    )
}

/// A solve replayed from a recording (every round one gated segment the
/// GPU switches off once the stop fires) gives the same pressure and the
/// same stop record, bit for bit, as encoding it directly, on the engine's
/// stop and at a fixed count, over frames whose problem changes. Once warm,
/// a frame records nothing, runs no round directly and validates the same
/// prelude every frame. Two direct solves of one problem agree bit for bit
/// too: the folded reductions sum in a fixed order.
#[test]
fn pressure_module_replay_matches_direct() {
    let (n, saved) = load_fixture(DAM_BREAK);
    let problems: Vec<Problem> = saved.iter().take(3).map(|p| resample(p, n, 64)).collect();
    for stop in [Stop::Converged(MAX_ITERATIONS), Stop::Fixed(16)] {
        let mut direct = Rig::new(64);
        let mut again = Rig::new(64);
        let mut replay = Rig::new(64);
        let mut cache = Some(GpuReplayCache::default());
        let mut none = None;
        let mut last = GpuReplayCache::default().stats();
        let mut last_template = replay.solver.template_stats();
        let mut prelude = None;
        for frame in 0..6 {
            let p = &problems[frame % problems.len()];
            let (dp, dr) = solve_bits(&mut direct, p, stop, &mut none);
            let (ap, ar) = solve_bits(&mut again, p, stop, &mut none);
            assert!(
                dp == ap && dr == ar,
                "{stop:?} frame {frame}: two direct solves differ"
            );
            let (rp, rr) = solve_bits(&mut replay, p, stop, &mut cache);
            assert!(
                dp == rp,
                "{stop:?} frame {frame}: the replayed pressure differs from direct"
            );
            assert_eq!(
                dr, rr,
                "{stop:?} frame {frame}: the replayed stop record differs from direct"
            );
            let stats = cache
                .as_ref()
                .expect("the span handed its cache back")
                .stats();
            let template = replay.solver.template_stats();
            let iterations = f32::from_bits(rr[1]) as u64;
            println!(
                "{stop:?} frame {frame}: {iterations} iterations, recorded {} replayed {} direct {} segments replayed {} direct {}",
                stats.recorded - last.recorded,
                stats.replayed - last.replayed,
                stats.direct - last.direct,
                stats.segments_replayed - last.segments_replayed,
                stats.segments_direct - last.segments_direct
            );
            // The first visit records what its new entry holds; the second
            // grows it and records the rest.
            if frame >= 2 {
                assert_eq!(
                    stats.recorded, last.recorded,
                    "{stop:?} frame {frame}: a warm solve records nothing"
                );
                assert_eq!(
                    stats.segments_direct, last.segments_direct,
                    "{stop:?} frame {frame}: no round runs directly"
                );
                let rounds = match stop {
                    Stop::Converged(cap) | Stop::Fixed(cap) => cap,
                };
                // The rounds are the template's executes, outside the frame
                // recording: one walk, no build, one execute per chunk.
                assert_eq!(
                    stats.segments_replayed, last.segments_replayed,
                    "{stop:?} frame {frame}: no round is in the frame recording"
                );
                assert_eq!(
                    template.executes - last_template.executes,
                    manifold_gpu::template_chunks(
                        rounds,
                        super::gpu_flip_pressure::ROUND_CHUNK,
                        super::gpu_flip_pressure::SINGLE_ROUNDS,
                    )
                    .count() as u64,
                    "{stop:?} frame {frame}: one execute per chunk of rounds"
                );
                assert_eq!(
                    template.walks - last_template.walks,
                    1,
                    "{stop:?} frame {frame}: one walked round per solve"
                );
                assert_eq!(
                    (template.builds, template.command_writes),
                    (last_template.builds, last_template.command_writes),
                    "{stop:?} frame {frame}: a warm solve writes no command"
                );
                // The prepare (the solver's passes and the coarse levels'
                // world tile builds) and the prelude, the same every frame.
                let validated = stats.replayed - last.replayed;
                assert!(validated > 0, "{stop:?} frame {frame}: the prelude replays");
                assert_eq!(
                    *prelude.get_or_insert(validated),
                    validated,
                    "{stop:?} frame {frame}: the warm solve validates its prelude"
                );
            }
            last = stats;
            last_template = template;
        }
    }
}

/// Bits every scalar and record float holds before a solve: a quiet NaN no
/// solve writes, so storage it leaves alone is known.
const SENTINEL: u32 = 0x7fc0_dead;

/// A frame of the step's two solves on one solver, as the density solve
/// follows the pressure one: each solve's pressure and stop record copied
/// out after it, so the second solve's rearm is seen on its own record.
/// `live` per solve: false gives it a zero right-hand side, which stops it
/// before its first round. `active` is the clock slot's plan.
fn rearm_frame(
    rig: &mut Rig,
    bufs: &[GpuBuffer],
    p: &Problem,
    live: [bool; 2],
    active: bool,
    plan: &GpuBuffer,
    cache: &mut Option<GpuReplayCache>,
) -> Vec<(Vec<u32>, Vec<u32>)> {
    let cells = rig.cells.len();
    let pooled = rig.pool_floats();
    let water: Vec<f32> = p.water.iter().map(|&w| f32::from(u8::from(w))).collect();
    let density: Vec<f32> = p.f.iter().map(|f| 0.5 * f).collect();
    let rhs: [Vec<f32>; 2] = std::array::from_fn(|s| {
        if !live[s] {
            vec![0.0; cells]
        } else if s == 0 {
            p.f.clone()
        } else {
            density.clone()
        }
    });
    // The same buffers every frame, as the step keeps its own: a new
    // buffer is a new recording.
    let (rhs_buffers, pressures, records, sentinel) =
        (&bufs[0..2], &bufs[2..4], &bufs[4..6], &bufs[6]);
    let mut words = [0u32; 12];
    words[0] = if active { 1.0f32.to_bits() } else { 0 };
    words[11] = 1;
    // SAFETY: shared buffers sized for what is written; the last frame completed.
    unsafe {
        rig.water
            .write(0, bytemuck::cast_slice(&rig.to_pool(&water, 0.0f32)));
        plan.write(0, bytemuck::cast_slice(&words));
        sentinel.write(
            0,
            bytemuck::cast_slice(&vec![SENTINEL; (sentinel.size / 4) as usize]),
        );
        for s in 0..2 {
            rhs_buffers[s].write(0, bytemuck::cast_slice(&rig.to_pool(&rhs[s], 0.0f32)));
            pressures[s].write(0, bytemuck::cast_slice(&vec![0u32; pooled]));
        }
    }
    rig.solver.set_clock_plan(plan);
    let mut enc = rig.device.create_encoder("gpu-flip-pressure-rearm");
    rig.prepare(&mut enc);
    rig.solver.seed_records(&mut enc, sentinel);
    let spanned = cache
        .take()
        .map(|cache| enc.begin_replay(&rig.device, cache))
        .is_some();
    let water = water_of!(rig);
    for s in 0..2 {
        let solve = Solve {
            rhs: &rhs_buffers[s],
            pressure: &pressures[s],
            stop: Stop::Converged(MAX_ITERATIONS),
            bodies: None,
            level: rig.level,
            coarse_rhs: None,
        };
        rig.solver
            .solve(&mut enc, &water, solve, &rig.fine.tiles)
            .expect("solves");
        enc.copy_buffer_to_buffer(
            rig.solver.progress().expect("prepared"),
            &records[s],
            records[s].size,
        );
    }
    if spanned {
        *cache = Some(enc.end_replay());
    }
    enc.commit_and_wait_completed();
    (0..2)
        .map(|s| {
            (
                rig.dense_of(&read::<u32>(&pressures[s], pooled)),
                read::<u32>(&records[s], PROGRESS_FLOATS as usize),
            )
        })
        .collect()
}

/// The density solve rearms what the pressure solve's stop switched off,
/// and the other way round: a pressure solve stopped before its first round
/// leaves the density solve live, a live pressure solve leaves a stopped
/// density solve stopped, each with its own record; an inactive clock slot
/// runs neither, and the slot turning active runs both again. Replayed
/// equals direct on every solve, through the round template.
#[test]
fn pressure_module_rearms_between_solves() {
    let (n, saved) = load_fixture(DAM_BREAK);
    let p = resample(&saved[0], n, 64);
    let frames: [([bool; 2], bool); 7] = [
        ([true, true], true),
        ([false, true], true),
        ([true, false], true),
        ([true, true], false),
        ([true, true], true),
        ([false, true], true),
        ([true, true], true),
    ];
    let mut direct = Rig::new(64);
    let mut replay = Rig::new(64);
    let direct_plan = direct.device.create_buffer_shared(48);
    let replay_plan = replay.device.create_buffer_shared(48);
    let bufs = |rig: &Rig| -> Vec<GpuBuffer> {
        let cells = rig.pool_floats() as u64 * 4;
        let record = u64::from(PROGRESS_FLOATS) * 4;
        let sentinel = (u64::from(2 * MAX_ITERATIONS) * 4).max(record);
        [cells, cells, cells, cells, record, record, sentinel]
            .iter()
            .map(|&size| rig.device.create_buffer_shared(size))
            .collect()
    };
    let (direct_bufs, replay_bufs) = (bufs(&direct), bufs(&replay));
    let mut none = None;
    let mut cache = Some(GpuReplayCache::default());
    for (frame, &(live, active)) in frames.iter().enumerate() {
        let d = rearm_frame(
            &mut direct,
            &direct_bufs,
            &p,
            live,
            active,
            &direct_plan,
            &mut none,
        );
        let before = replay.solver.template_stats();
        let r = rearm_frame(
            &mut replay,
            &replay_bufs,
            &p,
            live,
            active,
            &replay_plan,
            &mut cache,
        );
        let stats = replay.solver.template_stats();
        for s in 0..2 {
            let solve = ["pressure", "density"][s];
            assert!(
                d[s].0 == r[s].0,
                "frame {frame} {solve}: the replayed pressure differs from direct"
            );
            assert_eq!(
                d[s].1, r[s].1,
                "frame {frame} {solve}: the replayed record differs from direct"
            );
            let (iterations, stopped) = (f32::from_bits(r[s].1[1]), f32::from_bits(r[s].1[2]));
            println!(
                "frame {frame} {solve} live {} active {active}: {iterations} iterations, stopped {stopped}",
                live[s]
            );
            if !active {
                assert!(
                    r[s].0.iter().all(|&w| w == 0),
                    "frame {frame} {solve}: an inactive slot's solve writes no pressure"
                );
                assert!(
                    r[s].1.iter().all(|&w| w == SENTINEL),
                    "frame {frame} {solve}: an inactive slot's solve leaves its record unwritten"
                );
            } else if live[s] {
                assert!(
                    (1.0..=16.0).contains(&iterations) && stopped == 1.0,
                    "frame {frame} {solve}: a live solve converges on its own"
                );
            } else {
                assert!(
                    iterations == 0.0 && stopped == 1.0,
                    "frame {frame} {solve}: a zero right-hand side stops before round 0"
                );
                assert!(
                    r[s].0.iter().all(|&w| w == 0),
                    "frame {frame} {solve}: a solve stopped at the start writes no pressure"
                );
            }
        }
        if frame >= 2 {
            assert_eq!(
                stats.hits - before.hits,
                2,
                "frame {frame}: both solves run a built template"
            );
            assert_eq!(
                stats.builds, before.builds,
                "frame {frame}: nothing rebuilt once warm"
            );
        }
    }
}

/// The solve over each level's active slots equals the solve over every
/// held slot, bit for bit, in the pressure and the stop record, on the
/// engine's stop and at a fixed count, direct and replayed, over Dam Break
/// frames at 64³ and 25³ (a partial edge tile); and the same with NaN
/// written into every vector cell and partial outside the active slots
/// before the solve, so nothing the solve reads lies outside its lists
/// (docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 7 (Phase 2)); at Solve
/// Level 0 and 1 (section 11 (Solve Level)).
#[test]
fn pressure_module_sparse_matches_all_tiles() {
    use super::gpu_flip_step::set_all_tiles;
    let (n, saved) = load_fixture(DAM_BREAK);
    for (m, level) in [(64, 0), (25, 0), (64, 1), (25, 1)] {
        let problems: Vec<Problem> = saved.iter().take(3).map(|p| resample(p, n, m)).collect();
        for stop in [Stop::Converged(MAX_ITERATIONS), Stop::Fixed(16)] {
            let mut dense = Rig::new(m).at_level(level);
            let mut sparse = Rig::new(m).at_level(level);
            let mut poisoned = Rig::new(m).at_level(level);
            let mut replay = Rig::new(m).at_level(level);
            let mut cache = Some(GpuReplayCache::default());
            let mut none = None;
            for frame in 0..4 {
                let p = &problems[frame % problems.len()];
                set_all_tiles(true);
                let (dp, dr) = solve_bits(&mut dense, p, stop, &mut none);
                set_all_tiles(false);
                let (sp, sr) = solve_bits(&mut sparse, p, stop, &mut none);
                let (pp, pr) = solve_bits_under(&mut poisoned, p, stop, &mut none, true);
                let (rp, rr) = solve_bits_under(&mut replay, p, stop, &mut cache, true);
                let iterations = f32::from_bits(sr[1]);
                println!("{m}³ level {level} {stop:?} frame {frame}: {iterations} iterations");
                for (name, (xp, xr)) in [
                    ("sparse", (&sp, &sr)),
                    ("poisoned", (&pp, &pr)),
                    ("poisoned replay", (&rp, &rr)),
                ] {
                    assert_eq!(
                        &dr, xr,
                        "{m}³ level {level} {stop:?} frame {frame}: the {name} stop record differs from all tiles"
                    );
                    if let Some(i) = (0..dp.len()).find(|&i| dp[i] != xp[i]) {
                        panic!(
                            "{m}³ level {level} {stop:?} frame {frame}: the {name} pressure differs from all tiles first at cell {i} [{}, {}, {}]: {} vs {}",
                            i % m,
                            (i / m) % m,
                            i / (m * m),
                            f32::from_bits(dp[i]),
                            f32::from_bits(xp[i])
                        );
                    }
                }
            }
        }
    }
}

/// A pool at rest one step after gravity: every water face moved down by
/// g·dt except the floor's, so the only divergence is the bottom layer's
/// inflow, g·dt/h.
fn resting_pool_problem(m: usize) -> Problem {
    let water: Vec<bool> = (0..m * m * m).map(|c| (c / m) % m < m / 2).collect();
    let inflow = (9.81 / 60.0 / (BOX_METRES / m as f64)) as f32;
    let f = (0..m * m * m)
        .map(|c| {
            if water[c] && (c / m).is_multiple_of(m) {
                -inflow
            } else {
                0.0
            }
        })
        .collect();
    Problem { frame: 0, water, f }
}

/// The engine's stop on every shipped problem, the Dam Break at 64³ and
/// 128³: each solve stops on the tolerance, never the cap. The dense box
/// solver with the exact coarsest inverse took 11 iterations on the still
/// and resting pools and 12 to 14 on the Dam Break splash frames at 64³; the
/// sparse levels take more (up to 25 on the pools), and more coarsest sweeps
/// do not change that, so the bar of 26 catches a broken V-cycle, not a slow
/// one. The counts print beside the dense ones. The stop reads the recursive
/// residual, as the engine's PCG does, and f32 carries it below
/// 1e-9 · |f|∞ on all of them.
#[test]
fn pressure_module_converges_on_the_engine_tolerance() {
    let mut failures = Vec::new();
    let mut problems: Vec<(String, usize, Problem)> = Vec::new();
    for fixture in [
        DAM_BREAK,
        "deep_pool_pressure_problems",
        "deep_pool_density_problems",
    ] {
        let (n, saved) = load_fixture(fixture);
        for p in &saved {
            problems.push((fixture.to_string(), 64, resample(p, n, 64)));
        }
    }
    let (n, saved) = load_fixture(DAM_BREAK);
    for p in &saved {
        problems.push((DAM_BREAK.to_string(), 128, resample(p, n, 128)));
    }
    problems.push(("still_pool".into(), 64, still_pool_problem(64)));
    problems.push(("resting_pool".into(), 64, resting_pool_problem(64)));
    for (name, m, p) in &problems {
        let mut rig = Rig::new(*m);
        let c = converge(&mut rig, p, MAX_ITERATIONS);
        let rel: Vec<String> = c
            .norms
            .iter()
            .map(|r| format!("{:.1e}", r / c.f_norm))
            .collect();
        let (limit, before) = if name.ends_with("pool") {
            (26, "11")
        } else {
            (26, "12 to 14 at 64³")
        };
        println!(
            "curve {name} {m}³ frame {}: |f| {:.3e} it {} (dense solver {before}) stopped {} rel {}",
            p.frame,
            c.f_norm,
            c.iterations,
            c.stopped,
            rel.join(" ")
        );
        if !c.stopped || c.iterations > limit {
            failures.push(format!(
                "{name} {m}³ frame {}: {} iterations, stopped {}",
                p.frame, c.iterations, c.stopped
            ));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The warm start: a solve starts from the pooled pressure its buffer
/// holds. A Dam Break problem solved again from its own converged pressure
/// stops on the tolerance in fewer iterations than cold. It need not stop at
/// the start: the stop reads the recursive residual down to 1e-9 of |f|∞,
/// and the converged f32 pressure's own residual (printed) sits far above
/// that. Each later saved frame from the frame before's converged pressure,
/// beside cold, printed; over the run warm takes at most cold's iterations.
#[test]
fn pressure_module_warm_start_stops_sooner() {
    let (n, saved) = load_fixture(DAM_BREAK);
    let problems: Vec<Problem> = saved.iter().map(|p| resample(p, n, 64)).collect();
    let mut warm = Rig::new(64);
    let mut cold = Rig::new(64);
    let first = converge(&mut warm, &problems[0], MAX_ITERATIONS);
    let remainder = fine_remainder(
        &warm.pressure(),
        &problems[0].water,
        &problems[0].f,
        64,
        warm.cell_size(),
    );
    let guess = remainder.iter().map(|r| r.abs()).fold(0.0, f64::max) / f64::from(first.f_norm);
    warm.keep_pressure = true;
    let again = converge(&mut warm, &problems[0], MAX_ITERATIONS);
    println!(
        "warm start frame {}: cold {} iterations; again from its pressure (|f − L p|∞/|f|∞ {guess:.2e}) {}",
        problems[0].frame, first.iterations, again.iterations
    );
    assert!(first.stopped && again.stopped, "both stop on the tolerance");
    assert!(
        again.iterations < first.iterations,
        "from its own pressure {} iterations, cold {}",
        again.iterations,
        first.iterations
    );
    let (mut warm_total, mut cold_total) = (0, 0);
    for p in &problems[1..] {
        let w = converge(&mut warm, p, MAX_ITERATIONS);
        let c = converge(&mut cold, p, MAX_ITERATIONS);
        println!(
            "warm start frame {}: warm {} iterations, cold {}",
            p.frame, w.iterations, c.iterations
        );
        assert!(
            w.stopped && c.stopped,
            "frame {}: both stop on the tolerance",
            p.frame
        );
        warm_total += w.iterations;
        cold_total += c.iterations;
    }
    println!("warm start over the saved frames: warm {warm_total} iterations, cold {cold_total}");
    assert!(
        warm_total <= cold_total,
        "warm {warm_total} iterations over the run, cold {cold_total}"
    );
}

/// A box full of water with every wall closed is one sealed pocket: pure
/// Neumann, L singular. The step pins the pocket's leader (its lowest cell)
/// to p = 0 by leaving it out of the solve's water mask, so L is nonsingular.
/// With a zero-sum source the pinned solve stops on the tolerance, the pinned
/// cell reads 0, and the masked residual meets the f32 floor.
#[test]
fn pressure_module_converges_on_a_pinned_sealed_box() {
    let m = 32;
    let cells = m * m * m;
    let mut f: Vec<f32> = (0..cells)
        .map(|c| ((c as u64).wrapping_mul(2_654_435_761) >> 8) as f32 % 1000.0 / 500.0 - 1.0)
        .collect();
    let mean = f.iter().map(|&v| f64::from(v)).sum::<f64>() / cells as f64;
    for v in &mut f {
        *v -= mean as f32;
    }
    let mut water = vec![true; cells];
    let unpinned = converge(
        &mut Rig::new(m),
        &Problem {
            frame: 0,
            water: water.clone(),
            f: f.clone(),
        },
        MAX_ITERATIONS,
    );
    water[0] = false;
    f[0] = 0.0;
    let pinned = Problem { frame: 0, water, f };
    let mut rig = Rig::new(m);
    let c = converge(&mut rig, &pinned, MAX_ITERATIONS);
    let residual = rig.residual_of(&pinned);
    println!(
        "sealed box {m}³: unpinned {} iterations stopped {}; pinned {} iterations stopped {}, p[0] {}, residual {residual:.3e}",
        unpinned.iterations,
        unpinned.stopped,
        c.iterations,
        c.stopped,
        rig.pressure()[0]
    );
    assert!(
        c.stopped,
        "pinned sealed box ran {} iterations to the cap",
        c.iterations
    );
    assert_eq!(rig.pressure()[0], 0.0, "the pinned cell");
    assert!(residual < F32_FLOOR, "pinned residual {residual:.3e}");
}

/// A cell's row of L the way the face stencil summed it before the rows
/// were assembled, in f32 and in the kernel's face order: the six weights
/// masked to water neighbours, then the ghost and the plain diagonal.
fn cpu_row(water: &[bool], phi: Option<&[f32]>, c: usize, n: usize, h: f32) -> [f32; ROW_FLOATS] {
    let mut row = [0.0f32; ROW_FLOATS];
    if !water[c] {
        return row;
    }
    let at = [c % n, (c / n) % n, c / (n * n)];
    let stride = [1, n, n * n];
    let (mut ghost, mut plain) = (0.0f32, 0.0f32);
    for (k, slot) in row.iter_mut().enumerate().take(6) {
        let a = k / 2;
        let open = if k % 2 == 0 { at[a] > 0 } else { at[a] + 1 < n };
        if !open {
            continue;
        }
        // Every inner face of the rig's box is whole.
        let w = 1.0f32;
        ghost += w;
        plain += w;
        let q = if k % 2 == 0 {
            c - stride[a]
        } else {
            c + stride[a]
        };
        if water[q] {
            *slot = w;
        } else if let Some(phi) = phi {
            let centre = phi[c].min(-0.005 * h);
            let theta = (phi[q].max(0.0) / (centre + 1e-9)).clamp(-25.0, 25.0);
            ghost -= w * theta;
        }
    }
    row[6] = ghost;
    row[7] = plain;
    row
}

/// The rows prepare assembles are what the face stencil computed per sweep,
/// with the ghost rows and plain, on a Dam Break frame at 64 and the odd
/// side 37: every cell of an active slot (the only rows a solve reads). The
/// weights and the plain diagonal match the CPU bit for bit; the ghost
/// diagonal's θ is a division the GPU rounds its own way, so that slot is
/// within `GHOST_ULPS`.
#[test]
fn pressure_module_rows_match_the_stencil() {
    const GHOST_ULPS: u32 = 2;
    let (n, problems) = load_fixture(DAM_BREAK);
    for m in [64, 37] {
        let h = (BOX_METRES / m as f64) as f32;
        let problem = resample(&problems[2], n, m);
        let phi = surface_phi(&problem.water, m, h);
        for with_phi in [false, true] {
            let mut rig = Rig::new(m);
            if with_phi {
                rig = rig.with_phi(&phi);
            }
            rig.run(&problem, 1, false);
            let pooled = rig.pool_floats();
            let copy = rig
                .device
                .create_buffer_shared((pooled * ROW_FLOATS * 4) as u64);
            let mut enc = rig.device.create_encoder("gpu-flip-pressure-rows");
            rig.solver.copy_rows(&mut enc, &copy);
            enc.commit_and_wait_completed();
            let rows = rig.dense_of(&read::<[f32; ROW_FLOATS]>(&copy, pooled));
            let (flags, _, _) = rig.slot_words();
            let mut wrong = 0;
            let mut ghosted = 0;
            let mut ulps = 0;
            let mut compared = 0;
            for (c, got) in rows.iter().enumerate() {
                if flags[rig.index[c] / TILE_CELLS as usize] == 0 {
                    continue;
                }
                compared += 1;
                let expected = cpu_row(&problem.water, with_phi.then_some(phi.as_slice()), c, m, h);
                let exact = (0..ROW_FLOATS)
                    .filter(|&k| k != 6)
                    .all(|k| got[k].to_bits() == expected[k].to_bits());
                let apart = got[6].to_bits().abs_diff(expected[6].to_bits());
                ulps = ulps.max(apart);
                if !exact
                    || apart > GHOST_ULPS
                    || got[6].is_sign_negative() != expected[6].is_sign_negative()
                {
                    wrong += 1;
                    if wrong <= 3 {
                        println!(
                            "rows {m}³ phi {with_phi} cell {c}: got {got:?}, expected {expected:?}"
                        );
                    }
                }
                ghosted += usize::from(expected[6].to_bits() != expected[7].to_bits());
            }
            println!(
                "rows {m}³ phi {with_phi}: {compared} cells, ghost diagonal at most {ulps} ulp from the CPU's"
            );
            assert!(
                problem
                    .water
                    .iter()
                    .zip(&rig.index)
                    .all(|(&w, &at)| !w || flags[at / TILE_CELLS as usize] != 0),
                "{m}³: every water cell's slot is active"
            );
            assert_eq!(
                wrong, 0,
                "{m}³ with phi {with_phi}: rows differ from the stencil"
            );
            assert_eq!(
                ghosted > 0,
                with_phi,
                "{m}³: the ghost diagonal differs from the plain one exactly with φ"
            );
        }
    }
}

// ── Solve Level (docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 11) ──────────

/// Solve Level 0 is the fine solve: on the first three Dam Break problems at
/// 64³ the engine's stop lands within the Dam Break bar of 16 iterations,
/// never on the cap, and a run repeats bit for bit.
#[test]
fn gpu_flip_solve_level_zero_is_the_fine_step() {
    let (n, saved) = load_fixture(DAM_BREAK);
    let mut rig = Rig::new(64);
    assert_eq!(rig.level, 0);
    let mut none = None;
    let mut failures = Vec::new();
    for p in saved.iter().take(3) {
        let problem = resample(p, n, 64);
        let c = converge(&mut rig, &problem, MAX_ITERATIONS);
        let first = solve_bits(
            &mut rig,
            &problem,
            Stop::Converged(MAX_ITERATIONS),
            &mut none,
        );
        let again = solve_bits(
            &mut rig,
            &problem,
            Stop::Converged(MAX_ITERATIONS),
            &mut none,
        );
        println!(
            "solve level 0 frame {}: {} iterations, stopped {}",
            p.frame, c.iterations, c.stopped
        );
        if !c.stopped || c.iterations > 16 {
            failures.push(format!(
                "frame {}: {} iterations, stopped {}",
                p.frame, c.iterations, c.stopped
            ));
        }
        if first != again {
            failures.push(format!("frame {}: a repeated solve differs", p.frame));
        }
    }
    assert!(failures.is_empty(), "level 0 moved: {failures:#?}");
}

/// Per cell f − L p on the water (zero off it): the divergence a project
/// from `p` leaves, with air at zero pressure and the box walls closed.
fn fine_remainder(p: &[f32], water: &[bool], f: &[f32], n: usize, h: f64) -> Vec<f64> {
    let stride = [1, n, n * n];
    (0..n * n * n)
        .map(|c| {
            if !water[c] {
                return 0.0;
            }
            let at = [c % n, (c / n) % n, c / (n * n)];
            let centre = f64::from(p[c]);
            let mut sum = 0.0;
            for a in 0..3 {
                for next in [at[a].checked_sub(1), Some(at[a] + 1).filter(|&q| q < n)]
                    .into_iter()
                    .flatten()
                {
                    let q = c + next * stride[a] - at[a] * stride[a];
                    sum += if water[q] { f64::from(p[q]) } else { 0.0 } - centre;
                }
            }
            f64::from(f[c]) - sum / (h * h)
        })
        .collect()
}

/// The engine's stop at Solve Levels 0 and 1 on every shipped problem: each
/// solve stops on the tolerance, never the cap, within the fine level's bar
/// (pools 12 iterations, the rest 16); the counts side by side. The deepest
/// level a pool offers is levels − 2, and that level stops too.
#[test]
fn pressure_module_solve_level_converges_on_the_engine_tolerance() {
    let mut failures = Vec::new();
    let mut problems: Vec<(String, Problem)> = Vec::new();
    for fixture in [
        DAM_BREAK,
        "deep_pool_pressure_problems",
        "deep_pool_density_problems",
    ] {
        let (n, saved) = load_fixture(fixture);
        for p in &saved {
            problems.push((fixture.to_string(), resample(p, n, 64)));
        }
    }
    problems.push(("still_pool".into(), still_pool_problem(64)));
    problems.push(("resting_pool".into(), resting_pool_problem(64)));
    let deepest = max_solve_level(512);
    assert_eq!(
        deepest, 3,
        "a 64³ box's 512 tiles halve to 256, 128, 64, 32: the gradient runs on 0 to 3"
    );
    for (name, p) in &problems {
        let mut counts = Vec::new();
        for level in [0, 1, deepest] {
            let mut rig = Rig::new(64).at_level(level);
            let c = converge(&mut rig, p, MAX_ITERATIONS);
            // The same bar as the level-0 convergence test.
            let limit = 26;
            if !c.stopped || (level <= 1 && c.iterations > limit) {
                failures.push(format!(
                    "{name} frame {} level {level}: {} iterations, stopped {}",
                    p.frame, c.iterations, c.stopped
                ));
            }
            counts.push(format!(
                "level {level}: {} iterations{}",
                c.iterations,
                if c.stopped { "" } else { " (capped)" }
            ));
        }
        println!(
            "solve level counts {name} frame {}: {}",
            p.frame,
            counts.join(", ")
        );
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A level the pool lacks is refused by name before any GPU work, at the
/// solver and at the step's reading of the param; the deepest level is the
/// one above the coarsest.
#[test]
fn pressure_module_refuses_a_level_past_the_coarsest() {
    use super::gpu_flip_step::read_solve_level;
    let (n, saved) = load_fixture(DAM_BREAK);
    let problem = resample(&saved[0], n, 64);
    let mut rig = Rig::new(64).at_level(4);
    let water: Vec<f32> = problem
        .water
        .iter()
        .map(|&w| f32::from(u8::from(w)))
        .collect();
    rig.upload(&water, &problem.f);
    let mut enc = rig.device.create_encoder("gpu-flip-pressure-refusal");
    rig.prepare(&mut enc);
    let refused = rig
        .solver
        .solve(
            &mut enc,
            &water_of!(rig),
            run_at!(rig, Stop::Fixed(1)),
            &rig.fine.tiles,
        )
        .expect_err("level 4 with 512 tiles");
    assert_eq!(
        refused,
        "Coarse Solve must be 0 to 3 with a pool of 512 tiles, not 4"
    );
    assert_eq!(
        read_solve_level(4.0, 512).expect_err("the step's reading"),
        refused
    );
    assert_eq!(read_solve_level(3.0, 512), Ok(3));
    assert_eq!(read_solve_level(1.0, 64), Ok(1));
    assert!(
        read_solve_level(3.0, 64).is_err(),
        "64 tiles halve to 32, 16, 8: level 3 is the coarsest"
    );
}

/// The GPU's own arm and stop at chunk boundaries, read back, on a direct
/// encoder and in a replaying span: every chunked execute that starts at or
/// past the completed rounds has length 0 (the arm's cut for Fixed, the
/// stop's cancel for Converged), every earlier one runs its rounds of one
/// round's commands, and the scalars and record past the completed rounds
/// still hold the sentinel. The pressure, scalars, record, ranges and every
/// work vector (r, z, p, scratch, body product) agree bit for bit between
/// the two paths, so a round past the stop wrote nowhere on either. The
/// ranges are the arm's and the stop's, whatever the lattice: the problems
/// run at 16³.
#[test]
fn pressure_module_chunk_ranges_arm_and_stop_on_the_gpu() {
    use super::gpu_flip_pressure::{CHUNK_ENTRIES, ROUND_CHUNK, SINGLE_ROUNDS};
    const M: usize = 16;
    let (n, dam) = load_fixture(DAM_BREAK);
    let (_, density) = load_fixture("deep_pool_density_problems");
    let fixed = [
        1, 2, 3, 4, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 95, 96, 97, 900,
    ];
    let mut checked = 0;
    for chunk in [ROUND_CHUNK, 8, 3] {
        let _chunk = super::gpu_flip_pressure::set_round_chunk(chunk);
        for saved in [&dam[0], &dam[4], &density[0]] {
            let p = resample(saved, n, M);
            let mut direct = Rig::new(M);
            let mut spanned = Rig::new(M);
            let mut none = None;
            let mut cache = Some(GpuReplayCache::default());
            let mut per_round = 0;
            let stops = fixed
                .iter()
                .filter(|&&k| k <= MAX_ITERATIONS)
                .map(|&k| Stop::Fixed(k))
                .chain([Stop::Converged(64), Stop::Converged(MAX_ITERATIONS)]);
            for stop in stops {
                let cap = match stop {
                    Stop::Fixed(k) | Stop::Converged(k) => k,
                };
                let d = chunk_solve(&mut direct, &p, stop, &mut none);
                // The span's first visit records its prelude; the second replays.
                chunk_solve(&mut spanned, &p, stop, &mut cache);
                let s = chunk_solve(&mut spanned, &p, stop, &mut cache);
                assert!(
                    d == s,
                    "chunk {chunk} {stop:?}: the direct and replayed solves differ"
                );
                let ChunkSolve {
                    ranges,
                    scalars,
                    record,
                    ..
                } = d;
                let completed = f32::from_bits(record[1]) as u32;
                assert!(
                    completed <= cap,
                    "chunk {chunk} {stop:?}: {completed} rounds completed"
                );
                if let Stop::Fixed(k) = stop {
                    assert_eq!(
                        completed, k,
                        "chunk {chunk} {stop:?}: a fixed count runs every round"
                    );
                }
                let length = |j: u32| ranges[2 * (CHUNK_ENTRIES + j) as usize + 1];
                if per_round == 0 {
                    per_round = length(0);
                    assert!(
                        per_round > 0,
                        "chunk {chunk} {stop:?}: the first execute runs"
                    );
                }
                for (j, (start, rounds)) in
                    manifold_gpu::template_chunks(cap, chunk, SINGLE_ROUNDS).enumerate()
                {
                    let want = if start < completed {
                        rounds * per_round
                    } else {
                        0
                    };
                    assert_eq!(
                        length(j as u32),
                        want,
                        "chunk {chunk} {stop:?} completed {completed}: execute {j} from round {start}"
                    );
                }
                let tail = |words: &[u32], from: usize| {
                    words[from..]
                        .iter()
                        .position(|&w| w != SENTINEL)
                        .map(|i| i + from)
                };
                assert_eq!(
                    tail(&scalars, 2 * completed as usize),
                    None,
                    "chunk {chunk} {stop:?}: a scalar past round {completed} was written"
                );
                assert_eq!(
                    tail(&record, 4 + completed as usize),
                    None,
                    "chunk {chunk} {stop:?}: a record past round {completed} was written"
                );
                checked += 1;
            }
        }
    }
    let per_case = fixed.iter().filter(|&&k| k <= MAX_ITERATIONS).count() + 2;
    assert_eq!(checked, 3 * 3 * per_case);
}

/// Everything a chunked solve leaves, read back.
#[derive(PartialEq)]
struct ChunkSolve {
    ranges: Vec<u32>,
    scalars: Vec<u32>,
    record: Vec<u32>,
    pressure: Vec<u32>,
    vectors: Vec<Vec<u32>>,
}

/// One cold solve, its scalars and record seeded with [`SENTINEL`], inside
/// a replay span when `cache` holds one, directly otherwise.
fn chunk_solve(
    rig: &mut Rig,
    p: &Problem,
    stop: Stop,
    cache: &mut Option<GpuReplayCache>,
) -> ChunkSolve {
    let water: Vec<f32> = p.water.iter().map(|&w| f32::from(u8::from(w))).collect();
    let pooled = rig.pool_floats();
    let scalar_bytes = u64::from(2 * MAX_ITERATIONS) * 4;
    let record_bytes = u64::from(PROGRESS_FLOATS) * 4;
    let sentinel = rig
        .device
        .create_buffer_shared(scalar_bytes.max(record_bytes));
    rig.upload(&water, &p.f);
    // SAFETY: a shared buffer sized for what is written; the last solve completed.
    unsafe {
        sentinel.write(
            0,
            bytemuck::cast_slice(&vec![SENTINEL; (sentinel.size / 4) as usize]),
        );
    }
    let scalars = rig.device.create_buffer_shared(scalar_bytes);
    let record = rig.device.create_buffer_shared(record_bytes);
    let mut enc = rig.device.create_encoder("gpu-flip-pressure-chunk-ranges");
    rig.prepare(&mut enc);
    rig.solver.seed_records(&mut enc, &sentinel);
    let spanned = cache
        .take()
        .map(|c| enc.begin_replay(&rig.device, c))
        .is_some();
    rig.solve_on(&mut enc, stop);
    if spanned {
        *cache = Some(enc.end_replay());
    }
    let copy = |enc: &mut GpuEncoder, src: &GpuBuffer| {
        let dst = rig.device.create_buffer_shared(src.size);
        enc.copy_buffer_to_buffer(src, &dst, src.size);
        dst
    };
    let ranges = copy(&mut enc, rig.solver.ranges().expect("prepared"));
    let vectors: Vec<GpuBuffer> = rig
        .solver
        .work_vectors()
        .expect("prepared")
        .iter()
        .map(|v| copy(&mut enc, v))
        .collect();
    rig.solver.copy_scalars(&mut enc, &scalars);
    enc.copy_buffer_to_buffer(
        rig.solver.progress().expect("prepared"),
        &record,
        record_bytes,
    );
    enc.commit_and_wait_completed();
    let words = |b: &GpuBuffer| read::<u32>(b, (b.size / 4) as usize);
    ChunkSolve {
        ranges: words(&ranges),
        scalars: words(&scalars),
        record: words(&record),
        pressure: read::<u32>(&rig.pressure, pooled),
        vectors: vectors.iter().map(words).collect(),
    }
}

// ── World-tile levels (docs/WATER_INFINITE_DOMAIN_DESIGN.md section 7) ──────

/// Every coarse level prepare builds equals a CPU mirror of the parent
/// build, on a sheet of tiles and on a line, both across the origin: the
/// parents (tile >> 1) of the level below's held tiles keyed into an empty
/// table (keys and entry slots), slot j for the j-th key in table order with
/// its tile and its 27 neighbours, each slot's 8 children (the level below's
/// slots of tiles 2t + (c & 1, (c >> 1) & 1, c >> 2), NONE where absent),
/// the solver's parent of each slot below, and the held count with no
/// shortfall.
#[test]
fn pressure_levels_mirror_the_parent_build() {
    let device = test_device();
    let sheet: Vec<[i32; 3]> = (-2..=2)
        .flat_map(|z| (-3..=3).map(move |x| [x, -1, z]))
        .collect();
    let line: Vec<[i32; 3]> = (-4..=2).map(|x| [x, 2, -1]).collect();
    for (name, tiles) in [("sheet", sheet), ("line", line)] {
        let h = 0.25;
        let fine = FineTiles::build(&device, h, &tiles);
        let pool = fine.tiles.pool_tiles();
        let pools = level_pools(pool);
        // Dry water: the levels' tables need only the tiles.
        let dry = device.create_buffer_shared(pool_len(pool) * 32);
        dry.zero_fill();
        let mut solver = PressureSolver::default();
        solver.prepare_pipelines(&device);
        let mut enc = device.create_encoder("pressure levels mirror");
        let water = Water {
            cell_size: h,
            water: &dry,
            faces: &dry,
            phi: None,
        };
        solver
            .prepare(&device, &mut enc, &water, &fine.tiles)
            .expect("prepares");
        let parents: Vec<GpuBuffer> = (1..pools.len())
            .map(|level| {
                let from = solver.level_parents(level).expect("a coarse level");
                let into = device.create_buffer_shared(from.size);
                enc.copy_buffer_to_buffer(from, &into, from.size);
                into
            })
            .collect();
        enc.commit_and_wait_completed();
        let mut below = fine.tile_of.clone();
        let mut below_slot = fine.slot_of.clone();
        for level in 1..pools.len() {
            let at = solver.level_tiles(level).expect("a coarse level");
            let p = pools[level];
            assert_eq!(at.pool_tiles(), p, "{name} level {level}: the pool");
            let wanted: BTreeSet<[i32; 3]> = below.iter().map(|t| t.map(|c| c >> 1)).collect();
            assert!(
                wanted.len() <= p as usize,
                "{name} level {level}: the pool holds every parent"
            );
            let entries = table_entries(p);
            let mut keys = vec![EMPTY; entries as usize];
            for &t in &wanted {
                assert!(insert(&mut keys, tile_key(t)), "{name} level {level}: room");
            }
            let mut order: Vec<[i32; 3]> = wanted.into_iter().collect();
            order.sort_by_key(|&t| find(&keys, tile_key(t)).expect("an inserted key"));
            let held: Vec<([i32; 3], u32)> = order
                .iter()
                .enumerate()
                .map(|(slot, &t)| (t, slot as u32))
                .collect();
            let want = table_words_of(p, &held);
            let got: Vec<u32> = read(at.table(), table_words(p) as usize);
            let e = entries as usize;
            assert_eq!(
                got[..2 * e],
                want[..2 * e],
                "{name} level {level}: keys and entry slots"
            );
            for &(_, slot) in &held {
                let row = slot_row_word(entries, slot) as usize;
                assert_eq!(
                    got[row..row + 4],
                    want[row..row + 4],
                    "{name} level {level}: slot {slot}'s row"
                );
                let row = neighbour_row_word(entries, p, slot) as usize;
                assert_eq!(
                    got[row..row + 27],
                    want[row..row + 27],
                    "{name} level {level}: slot {slot}'s neighbours"
                );
            }
            let rows: Vec<u32> = read(at.child_rows().expect("children rows"), 8 * p as usize);
            for &(t, slot) in &held {
                for c in 0..8 {
                    let child: [i32; 3] = std::array::from_fn(|a| 2 * t[a] + ((c >> a) & 1));
                    let want = below_slot.get(&child).copied().unwrap_or(NONE);
                    assert_eq!(
                        rows[8 * slot as usize + c as usize],
                        want,
                        "{name} level {level}: slot {slot}'s child {c}"
                    );
                }
            }
            let here: HashMap<[i32; 3], u32> = held.iter().copied().collect();
            let up: Vec<u32> = read(&parents[level - 1], below.len());
            for (slot, t) in below.iter().enumerate() {
                assert_eq!(
                    up[slot],
                    here[&t.map(|c| c >> 1)],
                    "{name} level {level}: the parent of slot {slot} below"
                );
            }
            let counts: Vec<u32> = read(at.counts(), count_words(0) as usize);
            assert_eq!(
                counts[rank_count_word(0)],
                held.len() as u32,
                "{name} level {level}: held"
            );
            assert_eq!(
                counts[exhausted_word(0)],
                0,
                "{name} level {level}: no shortfall"
            );
            println!("{name} level {level}: {} of {p} slots held", held.len());
            below = order;
            below_slot = here;
        }
    }
}

/// The V-cycle is a symmetric operator M: ⟨M x, y⟩ = ⟨x, M y⟩ to f32
/// rounding for two random right-hand sides, plain and with the ghost rows,
/// on a small sparse set across the origin: the 8 tiles around it (a 16³
/// cube, whose 8 parents stay apart at every level) and a pair of tiles off
/// to one side, water in each block but its outer layer. Level 1 holds 5 of
/// the 10 parents, so restriction and prolongation both skip the orphaned
/// tiles. M x is z after a cold first iteration on right-hand side x.
#[test]
fn pressure_vcycle_is_symmetric() {
    let mut tiles: Vec<[i32; 3]> = Vec::new();
    for z in -1..=0 {
        for y in -1..=0 {
            for x in -1..=0 {
                tiles.push([x, y, z]);
            }
        }
    }
    tiles.extend([[3, -2, 1], [4, -2, 1]]);
    let cells: Vec<[i32; 3]> = tiles.iter().flat_map(|&t| tile_cells(t)).collect();
    let set: HashSet<[i32; 3]> = cells.iter().copied().collect();
    let within =
        |p: [i32; 3], lo: [i32; 3], hi: [i32; 3]| (0..3).all(|a| p[a] >= lo[a] && p[a] <= hi[a]);
    let water: Vec<f32> = cells
        .iter()
        .map(|&p| {
            let wet = within(p, [-7; 3], [6; 3]) || within(p, [25, -15, 9], [38, -10, 14]);
            f32::from(u8::from(wet))
        })
        .collect();
    let spread = |c: usize, seed: u64| {
        ((c as u64 ^ seed).wrapping_mul(2_654_435_761) >> 8) as f32 % 1000.0 / 500.0 - 1.0
    };
    let random = |seed: u64| -> Vec<f32> {
        (0..cells.len())
            .map(|c| if water[c] > 0.5 { spread(c, seed) } else { 0.0 })
            .collect()
    };
    let (x, y) = (random(0x51), random(0xa7));
    let h = 0.125;
    let phi: Vec<f32> = (0..cells.len())
        .map(|c| {
            let s = 0.5 * (spread(c, 0x3c) + 1.0);
            if water[c] > 0.5 {
                -(h as f32) * (0.01 + 0.89 * s)
            } else {
                h as f32 * (0.05 + 0.9 * s)
            }
        })
        .collect();
    for ghost in [false, true] {
        let mut rig = Rig::on(cells.clone(), &tiles, h, |p| set.contains(&p));
        if ghost {
            rig = rig.with_phi(&phi);
        }
        let mut precondition = |v: &[f32]| -> Vec<f32> {
            rig.upload(&water, v);
            let mut enc = rig.device.create_encoder("gpu-flip-pressure-symmetry");
            rig.prepare(&mut enc);
            rig.solve_on(&mut enc, Stop::Fixed(1));
            let z = rig.solver.work_vectors().expect("prepared")[1];
            let into = rig.device.create_buffer_shared(z.size);
            enc.copy_buffer_to_buffer(z, &into, z.size);
            enc.commit_and_wait_completed();
            rig.dense_of(&read::<f32>(&into, rig.pool_floats()))
        };
        let (mx, my) = (precondition(&x), precondition(&y));
        let short = read::<u32>(
            rig.solver.level_tiles(1).expect("level 1").counts(),
            count_words(0) as usize,
        )[exhausted_word(0)];
        assert_ne!(short & POOL_SHORT, 0, "level 1 runs out of slots");
        let dot = |a: &[f32], b: &[f32]| {
            a.iter()
                .zip(b)
                .map(|(&a, &b)| f64::from(a) * f64::from(b))
                .sum::<f64>()
        };
        let norm = |a: &[f32]| dot(a, a).sqrt();
        let (mxy, xmy) = (dot(&mx, &y), dot(&x, &my));
        let scale = norm(&mx) * norm(&y);
        println!(
            "V-cycle symmetry ghost {ghost}: ⟨Mx, y⟩ {mxy:.6e}, ⟨x, My⟩ {xmy:.6e}, scale {scale:.3e}"
        );
        assert!(scale > 0.0, "ghost {ghost}: the V-cycle is not zero");
        assert!(
            (mxy - xmy).abs() <= 1e-4 * scale,
            "ghost {ghost}: ⟨Mx, y⟩ {mxy:e} against ⟨x, My⟩ {xmy:e} (scale {scale:e})"
        );
    }
}
