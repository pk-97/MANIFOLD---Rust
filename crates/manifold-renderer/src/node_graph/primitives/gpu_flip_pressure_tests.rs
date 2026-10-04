//! Checked against FLIP Fluids pressuresolver.cpp and pcgsolver.h (MIT); see THIRD_PARTY_NOTICES.md.
//! The pressure solver module against the f64 reference
//! (`scripts/mgpcg_reference.py`, the default rule: halve rounding up to 4
//! or less, then the exact inverse) on the saved Dam Break and deep pool
//! problems at 64³ and refined to 128³, and resampled to the odd and uneven
//! sides 25, 37 and 40. The residual is the true relative residual of the
//! masked Poisson equation.

use manifold_gpu::{GpuBinding, GpuBuffer, GpuReplayCache};

use super::gpu_flip_pressure::{MAX_ITERATIONS, PROGRESS_FLOATS, PressureSolver, ROW_FLOATS, Solve, Stop, Water, level_lattices, max_solve_level, passes};
use super::liquid_surface_tests::read;

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
    let path = format!("{}/tests/fixtures/{name}.bin.zst", env!("CARGO_MANIFEST_DIR"));
    let raw = zstd::decode_all(std::fs::File::open(&path).expect("fixture opens")).expect("fixture decodes");
    let word = |at: usize| u32::from_le_bytes(raw[at..at + 4].try_into().expect("four bytes"));
    assert_eq!(&raw[..4], b"SWFX");
    assert_eq!(word(4), 1, "fixture version");
    let (n, count) = (word(8) as usize, word(20) as usize);
    assert!(word(12) as usize == n && word(16) as usize == n, "cubic fixture");
    let cells = n * n * n;
    let mut at = 24;
    let problems = (0..count)
        .map(|_| {
            let (frame, wet) = (word(at), word(at + 4) as usize);
            at += 8;
            let water: Vec<bool> = (0..cells).map(|c| raw[at + c / 8] >> (c % 8) & 1 == 1).collect();
            at += cells / 8;
            let mut values = raw[at..at + 4 * wet].chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap()));
            at += 4 * wet;
            let f = water.iter().map(|&w| if w { values.next().expect("one f per water cell") } else { 0.0 }).collect();
            assert!(values.next().is_none(), "frame {frame}: more f values than water cells");
            Problem { frame, water, f }
        })
        .collect();
    (n, problems)
}

/// The problem at `m` cells a side, nearest cell, as the reference's
/// `resample` (and, at a multiple of n, its `refine`) builds it.
pub(crate) fn resample(p: &Problem, n: usize, m: usize) -> Problem {
    let pick: Vec<usize> = (0..m).map(|i| ((i as f64 + 0.5) * n as f64 / m as f64) as usize).collect();
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
/// side holds θ times the water's pressure (docs/GPU_FLIP_PRESSURE_SOLVE.md
/// section 2 (the equation)).
fn ghost_ratio(phi: &[f32], own: usize, air: usize, h: f64) -> f64 {
    let centre = f64::from(phi[own]).min(-0.005 * h);
    (f64::from(phi[air]).max(0.0) / (centre + 1e-9)).clamp(-25.0, 25.0)
}

/// [`residual`] with the free surface's ghost rows when `phi` is given: an
/// air neighbour holds θ·p of the water cell instead of 0.
fn ghost_residual(p: &[f32], water: &[bool], f: &[f32], phi: Option<&[f32]>, n: usize, h: f64) -> f64 {
    let (mut miss, mut size) = (0.0, 0.0);
    for c in (0..n * n * n).filter(|&c| water[c]) {
        let at = [c % n, (c / n) % n, c / (n * n)];
        let stride = [1, n, n * n];
        let centre = f64::from(p[c]);
        let mut sum = 0.0;
        for a in 0..3 {
            for next in [at[a].checked_sub(1), Some(at[a] + 1).filter(|&q| q < n)].into_iter().flatten() {
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

/// Every inner face of an `n`³ box whole, the walls closed, as FaceSample
/// records (velocity, then weight per axis).
fn open_face_records(n: usize) -> Vec<[f32; 8]> {
    let m = n + 1;
    (0..m * m * m)
        .map(|i| {
            let p = [i % m, (i / m) % m, i / (m * m)];
            let open = |a: usize| f32::from(u8::from((0..3).all(|b| b == a || p[b] < n) && p[a] > 0 && p[a] < n));
            [0.0, 0.0, 0.0, 0.0, open(0), open(1), open(2), 0.0]
        })
        .collect()
}

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct LentineFluxParams {
    nx: u32,
    ny: u32,
    nz: u32,
    color: u32,
    cx: u32,
    cy: u32,
    cz: u32,
    mode: u32,
    cell_size: f32,
    slot: u32,
    ghost: u32,
    tolerance: f32,
    list_base: u32,
    level: u32,
    all_tiles: u32,
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
            record[4 + axis] = if boundary { 0.0 } else { 0.18 + 0.037 * code as f32 };
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

fn lentine_fine_divergence(q: [usize; 3], n: [usize; 3], faces: &[[f32; 8]], fluid: &[f32], solid: &[f32], h: f64) -> f64 {
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
            source += sign * (open * f64::from(fluid[8 * face + axis]) + (volume - open) * f64::from(solid[8 * face + axis]));
        }
    }
    source / h
}

fn lentine_face_gather(p: [usize; 3], axis: usize, n: [usize; 3], faces: &[[f32; 8]], fluid: &[f32]) -> (f64, f64) {
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

fn lentine_solid_source(p: [usize; 3], n: [usize; 3], faces: &[[f32; 8]], solid: &[f32]) -> (f64, f64) {
    let faces_n = n.map(|side| side + 1);
    let mut source = 0.0;
    let mut volume = 0.0;
    for child in 0..8 {
        let q = [2 * p[0] + (child & 1), 2 * p[1] + ((child >> 1) & 1), 2 * p[2] + (child >> 2)];
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
                source += sign * (open_volume - f64::from(faces[face][4 + axis])) * f64::from(solid[8 * face + axis]);
            }
        }
    }
    (source, volume)
}

fn lentine_internal_solid_remainder(p: [usize; 3], n: [usize; 3], faces: &[[f32; 8]], solid: &[f32]) -> f64 {
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
    assert!((actual - expected).abs() <= tolerance, "{what}: GPU {actual:.8e}, CPU {expected:.8e}, tolerance {tolerance:.3e}");
}

/// The conservative Lentine gather preserves the average fine divergence,
/// including rigid motion in cut cells. The CPU oracle uses f64 accumulation;
/// the tolerance allows only the shader's f32 operation order. Odd sides also
/// prove that a virtual child contributes no fluid or solid source.
#[test]
fn pressure_module_lentine_flux_conserves_blocks() {
    let shader = include_str!("shaders/gpu_flip_pressure.wgsl");
    for n in lentine_cases() {
        let c = n.map(|side| side.div_ceil(2));
        let output_n = c.map(|side| side + 1);
        let h = 0.37_f32;
        let (water_values, face_values, fluid_values, solid_values) = lentine_fixture(n);
        let device = crate::test_device();
        let pipeline = device.create_compute_pipeline(shader, "lentine_flux_main", "gpu-flip-pressure-lentine-flux");
        let water = device.create_buffer_shared((water_values.len() * 4) as u64);
        let faces = device.create_buffer_shared((face_values.len() * 32) as u64);
        let fluid = device.create_buffer_shared((fluid_values.len() * 4) as u64);
        let solid = device.create_buffer_shared((solid_values.len() * 4) as u64);
        let output = device.create_buffer_shared((output_n[0] * output_n[1] * output_n[2] * 32) as u64);
        // SAFETY: each shared buffer is sized for the typed fixture and no GPU
        // work is in flight before these writes.
        unsafe {
            water.write(0, bytemuck::cast_slice(&water_values));
            faces.write(0, bytemuck::cast_slice(&face_values));
            fluid.write(0, bytemuck::cast_slice(&fluid_values));
            solid.write(0, bytemuck::cast_slice(&solid_values));
            output.write(0, bytemuck::cast_slice(&vec![f32::from_bits(0x7fc00000); output_n[0] * output_n[1] * output_n[2] * 8]));
        }
        let params = LentineFluxParams {
            nx: n[0] as u32,
            ny: n[1] as u32,
            nz: n[2] as u32,
            cx: c[0] as u32,
            cy: c[1] as u32,
            cz: c[2] as u32,
            cell_size: h,
            ..LentineFluxParams::default()
        };
        let mut encoder = device.create_encoder("gpu-flip-pressure-lentine-flux");
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                GpuBinding::Buffer { binding: 1, buffer: &water, offset: 0 },
                GpuBinding::Buffer { binding: 2, buffer: &faces, offset: 0 },
                GpuBinding::Buffer { binding: 3, buffer: &fluid, offset: 0 },
                GpuBinding::Buffer { binding: 4, buffer: &solid, offset: 0 },
                GpuBinding::Buffer { binding: 9, buffer: &output, offset: 0 },
            ],
            [((output_n[0] * output_n[1] * output_n[2]) as u32).div_ceil(256), 1, 1],
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
                    (0.0, if p[axis] + 1 < c[axis] { f64::from(actual[lattice_index(high, output_n)][axis]) } else { 0.0 })
                } else if p[axis] + 1 == c[axis] {
                    (f64::from(actual[lattice_index(low, output_n)][axis]), 0.0)
                } else {
                    (f64::from(actual[lattice_index(low, output_n)][axis]), f64::from(actual[lattice_index(high, output_n)][axis]))
                };
                fluid_difference += (high_flux - low_flux) / (2.0 * f64::from(h));
                let (area, flux) = if p[axis] == 0 {
                    (0.0, 0.0)
                } else {
                    lentine_face_gather(p, axis, n, &face_values, &fluid_values)
                };
                assert_lentine_close(actual[out_index][axis + 4], area, &format!("{n:?} block {p:?} axis {axis} area"));
                assert_lentine_close(actual[out_index][axis], flux, &format!("{n:?} block {p:?} axis {axis} flux"));
                if p[axis] == 0 {
                    assert_lentine_close(actual[out_index][axis], 0.0, &format!("{n:?} block {p:?} axis {axis} wall"));
                    assert_lentine_close(actual[out_index][axis + 4], 0.0, &format!("{n:?} block {p:?} axis {axis} wall area"));
                }
            }
            let (solid_source, volume) = lentine_solid_source(p, n, &face_values, &solid_values);
            internal_solid_activity += lentine_internal_solid_remainder(p, n, &face_values, &solid_values).abs();
            assert_lentine_close(actual[out_index][3], solid_source / (8.0 * f64::from(h)), &format!("{n:?} block {p:?} solid source"));
            assert_lentine_close(actual[out_index][7], volume / 8.0, &format!("{n:?} block {p:?} volume"));
            let fine_average = (0..8)
                .map(|child| {
                    let q = [2 * p[0] + (child & 1), 2 * p[1] + ((child >> 1) & 1), 2 * p[2] + (child >> 2)];
                    if q.iter().zip(n).any(|(&v, side)| v >= side) {
                        0.0
                    } else {
                        lentine_fine_divergence(q, n, &face_values, &fluid_values, &solid_values, f64::from(h))
                    }
                })
                .sum::<f64>()
                / 8.0;
            assert_lentine_close(f64::from(actual[out_index][3]) + fluid_difference, fine_average, &format!("{n:?} block {p:?} divergence"));
        }
        for (index, record) in actual.iter().enumerate() {
            let p = lattice_coords(index, output_n);
            let padded = p.iter().zip(c).any(|(&v, side)| v >= side);
            for axis in 0..3 {
                if padded || p[axis] == 0 || p[axis] == c[axis] {
                    assert_lentine_close(record[axis], 0.0, &format!("{n:?} output padding {p:?} velocity {axis}"));
                    assert_lentine_close(record[axis + 4], 0.0, &format!("{n:?} output padding {p:?} area {axis}"));
                }
            }
            if padded {
                assert_lentine_close(record[3], 0.0, &format!("{n:?} output padding {p:?} solid source"));
                assert_lentine_close(record[7], 0.0, &format!("{n:?} output padding {p:?} volume"));
            }
        }
        assert!(internal_solid_activity > 1.0e-3, "{n:?}: fixture did not exercise (c_low-c_high)*v_s");
    }
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
        Solve { rhs: &$rig.rhs, pressure: &$rig.pressure, stop: $stop, bodies: None, level: $rig.level, coarse_rhs: None }
    };
}

/// The solver on one cubic lattice, its inputs in shared buffers.
struct Rig {
    n: usize,
    device: crate::TestDevice,
    solver: PressureSolver,
    water: GpuBuffer,
    faces: GpuBuffer,
    rhs: GpuBuffer,
    pressure: GpuBuffer,
    /// The free surface's distance, when the rig solves the ghost rows.
    phi: Option<GpuBuffer>,
    /// The V-cycle level the gradient runs on; 0 is the fine lattice.
    level: usize,
}

impl Rig {
    fn at_level(mut self, level: usize) -> Self {
        self.level = level;
        self
    }

    fn new(n: usize) -> Self {
        let device = crate::test_device();
        let cells = (n * n * n * 4) as u64;
        let records = open_face_records(n);
        let faces = device.create_buffer_shared(records.len() as u64 * 32);
        // SAFETY: a shared buffer sized for the records; no GPU work is queued.
        unsafe { faces.write(0, bytemuck::cast_slice(&records)) };
        let mut solver = PressureSolver::default();
        solver.prepare_pipelines(&device);
        Self {
            n,
            water: device.create_buffer_shared(cells),
            rhs: device.create_buffer_shared(cells),
            pressure: device.create_buffer_shared(cells),
            faces,
            device,
            solver,
            phi: None,
            level: 0,
        }
    }

    fn with_phi(mut self, phi: &[f32]) -> Self {
        let buffer = self.device.create_buffer_shared(phi.len() as u64 * 4);
        // SAFETY: a shared buffer sized for φ; no GPU work is queued.
        unsafe { buffer.write(0, bytemuck::cast_slice(phi)) };
        self.phi = Some(buffer);
        self
    }

    fn pressure(&self) -> &[f32] {
        let ptr = self.pressure.mapped_ptr().expect("shared pressure");
        // SAFETY: the solve completed; the buffer holds `cells` floats.
        unsafe { std::slice::from_raw_parts(ptr.cast::<f32>().cast_const(), self.n * self.n * self.n) }
    }

    fn cell_size(&self) -> f64 {
        BOX_METRES / self.n as f64
    }

    /// One prepare and solve in its own command buffer: the profile.
    fn run(&mut self, p: &Problem, iterations: u32, profile: bool) -> manifold_gpu::GpuFrameProfile {
        let water: Vec<f32> = p.water.iter().map(|&w| f32::from(u8::from(w))).collect();
        // SAFETY: shared buffers sized for the lattice; the last solve completed.
        unsafe {
            self.water.write(0, bytemuck::cast_slice(&water));
            self.rhs.write(0, bytemuck::cast_slice(&p.f));
        }
        let mut enc = self.device.create_encoder("gpu-flip-pressure-module");
        if profile {
            let sampler = self.device.create_timestamp_sampler(8192).expect("timestamp sampling");
            enc.enable_dispatch_profiling(sampler, &self.device);
        }
        let n = self.n as u32;
        let lattice = Water { lattice: [n; 3], cell_size: self.cell_size() as f32, water: &self.water, faces: &self.faces, phi: self.phi.as_ref() };
        self.solver.prepare(&self.device, &mut enc, &lattice).expect("prepares");
        self.solver.solve(&mut enc, &lattice, run_at!(self, Stop::Fixed(iterations))).expect("solves");
        enc.commit_and_wait_profiled(&self.device)
    }

    /// The last solve's residual on the fine lattice, as the problem is posed.
    fn residual_of(&self, p: &Problem) -> f64 {
        residual(self.pressure(), &p.water, &p.f, self.n, self.cell_size())
    }

    /// The last solve's residual on its own level: the fine one at level 0,
    /// else level k's rows against its restricted right-hand side.
    fn level_residual_of(&self, p: &Problem) -> f64 {
        if self.level == 0 {
            return self.residual_of(p);
        }
        let lattice = level_lattices([self.n as u32; 3])[self.level];
        let cells = lattice.iter().map(|&v| v as usize).product::<usize>();
        let rows = self.device.create_buffer_shared((cells * ROW_FLOATS * 4) as u64);
        let rhs = self.device.create_buffer_shared((cells * 4) as u64);
        let e = self.device.create_buffer_shared((cells * 4) as u64);
        let mut enc = self.device.create_encoder("gpu-flip-pressure-level");
        let (n, h) = self.solver.copy_level(&mut enc, self.level, &rows, &rhs, &e);
        enc.commit_and_wait_completed();
        assert_eq!(n, lattice);
        let rows: Vec<f32> = read(&rows, cells * ROW_FLOATS);
        row_residual(&rows, &read(&rhs, cells), &read(&e, cells), lattice, f64::from(h))
    }

    fn solve(&mut self, p: &Problem, iterations: u32) -> f64 {
        self.run(p, iterations, false);
        self.level_residual_of(p)
    }
}

/// |L_k p − f_k| / |f_k| on one level's own rows (gpu_flip_pressure.wgsl
/// Row: six weights to water neighbours, low then high per axis, the ghost
/// and the plain diagonal), its right-hand side and its solution; a cell
/// with a zero plain diagonal is off the level's water.
fn row_residual(rows: &[f32], rhs: &[f32], p: &[f32], n: [u32; 3], h: f64) -> f64 {
    let n = n.map(|v| v as usize);
    let stride = [1, n[0], n[0] * n[1]];
    let (mut miss, mut size) = (0.0, 0.0);
    for c in 0..n[0] * n[1] * n[2] {
        let row = &rows[c * ROW_FLOATS..(c + 1) * ROW_FLOATS];
        if row[7] <= 0.0 {
            continue;
        }
        let mut sum = -f64::from(row[7]) * f64::from(p[c]);
        for (k, &w) in row.iter().enumerate().take(6) {
            if w == 0.0 {
                continue;
            }
            let q = if k % 2 == 0 { c - stride[k / 2] } else { c + stride[k / 2] };
            sum += f64::from(w) * f64::from(p[q]);
        }
        let f = f64::from(rhs[c]);
        miss += (sum / (h * h) - f).powi(2);
        size += f * f;
    }
    (miss / size).sqrt()
}

/// Solve every problem at `m` cells a side. At 3 iterations the GPU runs the
/// reference's algorithm step for step, so its residual is within 10% of the
/// f64 one. At 8 it is within 2× the reference, or of what f32 reaches at
/// all: the larger of F32_FLOOR and the residual after 16 iterations. Pins
/// are (frame, residual after 3, after 8) from
/// `scripts/mgpcg_reference.py FIXTURE [--refine 2 | --side M] --iterations 3,8,16`.
fn check(fixture: &str, m: usize, pinned: &[(u32, f64, f64)]) {
    check_at(fixture, m, 0, pinned);
}

/// [`check`] with the gradient on V-cycle level `level`: the residual is the
/// level's own, on its rows against its restricted right-hand side, as
/// `scripts/mgpcg_reference.py --solve-level LEVEL` measures it.
fn check_at(fixture: &str, m: usize, level: usize, pinned: &[(u32, f64, f64)]) {
    let (n, problems) = load_fixture(fixture);
    assert_eq!(problems.len(), pinned.len(), "{fixture}: one pin per problem");
    let mut rig = Rig::new(m).at_level(level);
    let mut failures = Vec::new();
    for (problem, &(frame, at3, at8)) in problems.iter().zip(pinned) {
        assert_eq!(problem.frame, frame);
        let problem = resample(problem, n, m);
        let got3 = rig.solve(&problem, 3);
        let floor = rig.solve(&problem, 16).max(F32_FLOOR);
        let got8 = rig.solve(&problem, 8);
        let again = rig.solve(&problem, 8);
        println!(
            "pressure module {fixture} {m}³ level {level} frame {frame:3}: 3 iterations {got3:.3e} (f64 {at3:.3e}, {:.3}×); 8 iterations {got8:.3e} (f64 {at8:.3e}, f32 floor {floor:.3e})",
            got3 / at3
        );
        assert_eq!(got8, again, "frame {frame}: repeat solves differ");
        if !(got3 / at3 - 1.0).abs().lt(&0.1) {
            failures.push(format!("frame {frame}: 3 iterations {got3:.3e} against {at3:.3e}"));
        }
        if got8.is_nan() || got8 > 2.0 * at8.max(floor) {
            failures.push(format!("frame {frame}: 8 iterations {got8:.3e} against {at8:.3e} (floor {floor:.3e})"));
        }
    }
    assert!(failures.is_empty(), "{fixture} {m}³: {failures:?}");
}

const DAM_BREAK: &str = "dambreak_pressure_problems";

#[test]
fn pressure_module_matches_reference_64() {
    check(
        DAM_BREAK,
        64,
        &[
            (0, 1.339e-02, 5.914e-07),
            (15, 2.168e-02, 3.360e-06),
            (30, 5.072e-02, 7.789e-06),
            (45, 2.588e-02, 7.831e-06),
            (60, 2.061e-02, 7.406e-06),
            (90, 1.206e-02, 3.585e-06),
            (120, 1.246e-02, 5.902e-06),
        ],
    );
}

#[test]
fn pressure_module_matches_reference_128() {
    check(
        DAM_BREAK,
        128,
        &[
            (0, 8.780e-03, 2.374e-07),
            (15, 2.614e-02, 3.853e-06),
            (30, 3.620e-02, 5.937e-06),
            (45, 2.719e-02, 3.435e-06),
            (60, 2.351e-02, 3.072e-06),
            (90, 1.338e-02, 2.408e-06),
            (120, 1.475e-02, 3.619e-06),
        ],
    );
}

/// Odd sides, whose coarse levels carry the virtual solid half-cell: 25
/// (25, 13, 7, 4) and 37 (37, 19, 10, 5, 3); and 40, even down to 5.
#[test]
fn pressure_module_matches_reference_odd_sides() {
    check(
        DAM_BREAK,
        25,
        &[
            (0, 1.264e-03, 2.351e-08),
            (15, 2.315e-03, 2.610e-08),
            (30, 3.554e-03, 1.053e-07),
            (45, 4.025e-03, 5.635e-08),
            (60, 2.851e-03, 9.921e-08),
            (90, 2.227e-03, 9.913e-08),
            (120, 1.328e-03, 7.313e-08),
        ],
    );
    check(
        DAM_BREAK,
        37,
        &[
            (0, 5.880e-03, 1.099e-07),
            (15, 1.392e-02, 6.265e-07),
            (30, 1.108e-02, 5.916e-07),
            (45, 5.470e-03, 2.777e-07),
            (60, 6.904e-03, 6.417e-07),
            (90, 5.333e-03, 4.049e-07),
            (120, 5.361e-03, 4.309e-07),
        ],
    );
    check(
        DAM_BREAK,
        40,
        &[
            (0, 2.568e-03, 1.988e-08),
            (15, 1.067e-02, 2.733e-07),
            (30, 2.242e-02, 3.175e-06),
            (45, 7.727e-03, 2.149e-06),
            (60, 6.061e-03, 5.958e-07),
            (90, 1.205e-02, 1.916e-06),
            (120, 7.300e-03, 1.429e-06),
        ],
    );
}

/// The deep still pool's main solve and the deep drop's density solves: the
/// coarsest level is water but for its top row.
#[test]
fn pressure_module_matches_reference_deep_pool() {
    check("deep_pool_pressure_problems", 64, &[(60, 1.188e-02, 5.352e-08)]);
    check("deep_pool_pressure_problems", 128, &[(60, 2.614e-02, 3.440e-07)]);
    check("deep_pool_density_problems", 64, &[(30, 6.912e-03, 5.795e-07), (60, 5.103e-03, 2.577e-07)]);
    check("deep_pool_density_problems", 128, &[(30, 6.632e-03, 3.220e-07), (60, 6.508e-03, 2.830e-07)]);
}

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
            let ghost = |rig: &Rig| ghost_residual(rig.pressure(), &problem.water, &problem.f, Some(&phi), m, f64::from(h));
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
                failures.push(format!("{m}³ frame {}: 8 iterations {at8:.3e}, 3 {at3:.3e}, floor {floor:.3e}", problem.frame));
            }
            if plain < 10.0 * at8 {
                failures.push(format!("{m}³ frame {}: the plain residual {plain:.3e} is as small as the ghost one", problem.frame));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The original classifier's boolean rule, independently scanning each
/// clipped tile box grown by one cell. No lane partition or reduction here.
fn pressure_tile_flags(n: [usize; 3], water: &[f32], all_tiles: bool) -> Vec<u32> {
    let tiles = n.map(|side| side.div_ceil(8));
    (0..tiles.iter().product()).map(|tile| {
        if all_tiles { return 1; }
        let origin = lattice_coords(tile, tiles).map(|v| 8 * v);
        let first = origin.map(|v| v.saturating_sub(1));
        let last: [usize; 3] = std::array::from_fn(|a| (origin[a] + 8).min(n[a] - 1));
        for z in first[2]..=last[2] {
            for y in first[1]..=last[1] {
                for x in first[0]..=last[0] {
                    if water[lattice_index([x, y, z], n)] > 0.5 { return 1; }
                }
            }
        }
        0
    }).collect()
}

#[test]
fn pressure_module_parallel_classification_matches_serial_boolean_oracle() {
    fn buffer(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
        GpuBinding::Buffer { binding, buffer, offset: 0 }
    }
    let device = crate::test_device();
    let shader = include_str!("shaders/gpu_flip_pressure.wgsl");
    let classify = device.create_compute_pipeline(shader, "classify_main", "pressure.classify.proof");
    let lists = device.create_compute_pipeline(shader, "lists_main", "pressure.lists.proof");
    const BASE: usize = 3;
    const SENTINEL: u32 = 0xa17e_5afe;
    for n in [[1, 1, 1], [7, 9, 5], [17, 10, 25], [64, 64, 64], [128, 128, 128]] {
        let cells: usize = n.iter().product();
        let total: usize = n.map(|v| v.div_ceil(8)).iter().product();
        let water_buffer = device.create_buffer_shared((cells * 4) as u64);
        let flags = device.create_buffer_shared(((total + 2 * BASE) * 4) as u64);
        let active_list = device.create_buffer_shared(flags.size);
        let armed = device.create_buffer_shared(9 * 4);
        let plan = device.create_buffer_shared(16 * 4);
        let mut expected_flags = vec![SENTINEL; total + 2 * BASE];
        let mut expected_list = expected_flags.clone();
        let mut expected_armed = vec![SENTINEL; 9];
        // SAFETY: shared buffers hold these words and no GPU work is queued.
        unsafe {
            flags.write(0, bytemuck::cast_slice(&expected_flags));
            active_list.write(0, bytemuck::cast_slice(&expected_list));
            armed.write(0, bytemuck::cast_slice(&expected_armed));
        }
        // Empty -> full -> sparse -> halo-only -> empty reuses the storage, then
        // all_tiles and an inactive clock prove the override and preservation.
        for (mask, all_tiles, inactive) in [
            (0, false, false), (1, false, false), (2, false, false),
            (3, false, false), (0, false, false), (0, true, false),
            (0, false, true), (3, false, false),
        ] {
            let mut water = vec![if mask == 1 { 1.0_f32 } else { 0.0 }; cells];
            if mask == 2 {
                for p in [[0; 3], n.map(|v| v - 1), n.map(|v| 7.min(v - 1)), n.map(|v| 9.min(v - 1))] {
                    water[lattice_index(p, n)] = 1.0;
                }
                // Exactly 0.5 is air; its next representable neighbour is wet.
                for (index, value) in [0.5_f32, 0.5_f32.next_down(), 0.5_f32.next_up(), -1.0, f32::NAN].into_iter().enumerate().take(cells) {
                    water[index] = value;
                }
            } else if mask == 3 {
                // At x=8, this cell touches both tile 0's halo and tile 1.
                water[lattice_index([8.min(n[0] - 1), 0, 0], n)] = 1.0;
            }
            let mut clock = [0u32; 16];
            clock[11] = 1;
            clock[0] = if inactive { 0.0_f32 } else { 1.0_f32 / 60.0 }.to_bits();
            // SAFETY: previous command buffer completed; storage covers all values.
            unsafe {
                water_buffer.write(0, bytemuck::cast_slice(&water));
                plan.write(0, bytemuck::cast_slice(&clock));
            }
            let params = LentineFluxParams {
                nx: n[0] as u32, ny: n[1] as u32, nz: n[2] as u32,
                list_base: BASE as u32, level: 1, all_tiles: u32::from(all_tiles),
                ..LentineFluxParams::default()
            };
            let mut enc = device.create_encoder("pressure classifier boolean proof");
            enc.dispatch_compute(&classify, &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                buffer(1, &water_buffer), buffer(19, &flags), buffer(21, &plan),
            ], [((total * 32).div_ceil(256)) as u32, 1, 1], "pressure.classify.proof");
            enc.dispatch_compute(&lists, &[
                GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&params) },
                buffer(14, &armed), buffer(19, &flags), buffer(20, &active_list), buffer(21, &plan),
            ], [1, 1, 1], "pressure.lists.proof");
            enc.commit_and_wait_completed();
            if !inactive {
                let oracle = pressure_tile_flags(n, &water, all_tiles);
                expected_flags[BASE..BASE + total].copy_from_slice(&oracle);
                let mut count = 0;
                for (tile, &lit) in oracle.iter().enumerate() {
                    if lit != 0 {
                        expected_list[BASE + count] = tile as u32;
                        count += 1;
                    }
                }
                expected_armed[3..6].copy_from_slice(&[2 * count as u32, 1, 1]);
            }
            let context = format!("{n:?} mask {mask}, all_tiles {all_tiles}, inactive {inactive}");
            assert_eq!(read::<u32>(&flags, expected_flags.len()), expected_flags, "flags: {context}");
            assert_eq!(read::<u32>(&active_list, expected_list.len()), expected_list, "lists and retained tail: {context}");
            assert_eq!(read::<u32>(&armed, expected_armed.len()), expected_armed, "armed counts: {context}");
        }
    }
}

/// The solve encodes exactly the passes `passes` counts, every one labelled
/// as the solver's; and its GPU time per solve, printed.
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
        let (prepare, solve) = passes([m as u32; 3], 8);
        assert_eq!(profile.spans.len(), prepare + solve, "{m}³ ({} levels)", level_lattices([m as u32; 3]).len());
        assert!(profile.spans.iter().all(|s| s.label.starts_with("gpu_flip.pressure.")), "{m}³: an unlabelled pass");
        let mut times: Vec<f64> = (0..5).map(|_| rig.run(&problem, 8, false).total_ms).collect();
        times.sort_by(f64::total_cmp);
        println!("pressure module {m}³: {prepare} + {solve} passes, {:.2} ms GPU per prepare and 8-iteration solve (median of 5)", times[2]);
    }
}

/// (r·z, p·s) of every iteration of one solve of `p`, `iterations` long.
fn rz(rig: &mut Rig, p: &Problem, iterations: u32) -> Vec<(f32, f32)> {
    let water: Vec<f32> = p.water.iter().map(|&w| f32::from(u8::from(w))).collect();
    // SAFETY: shared buffers sized for the lattice; the last solve completed.
    unsafe {
        rig.water.write(0, bytemuck::cast_slice(&water));
        rig.rhs.write(0, bytemuck::cast_slice(&p.f));
    }
    let scalars = rig.device.create_buffer_shared(u64::from(2 * MAX_ITERATIONS) * 4);
    let mut enc = rig.device.create_encoder("gpu-flip-pressure-rz");
    let n = rig.n as u32;
    let lattice = Water { lattice: [n; 3], cell_size: rig.cell_size() as f32, water: &rig.water, faces: &rig.faces, phi: rig.phi.as_ref() };
    rig.solver.prepare(&rig.device, &mut enc, &lattice).expect("prepares");
    rig.solver.solve(&mut enc, &lattice, run_at!(rig, Stop::Fixed(iterations))).expect("solves");
    rig.solver.copy_scalars(&mut enc, &scalars);
    enc.commit_and_wait_completed();
    let ptr = scalars.mapped_ptr().expect("shared scalars");
    // SAFETY: the copy completed; the buffer holds 2 · MAX_ITERATIONS floats.
    let all = unsafe { std::slice::from_raw_parts(ptr.cast::<f32>().cast_const(), 2 * MAX_ITERATIONS as usize) };
    (0..iterations as usize).map(|k| (all[2 * k], all[2 * k + 1])).collect()
}

/// A still pool: the lower half of the box is water, its divergence a fixed
/// pseudo-random spread.
fn still_pool_problem(m: usize) -> Problem {
    let water: Vec<bool> = (0..m * m * m).map(|c| (c / m) % m < m / 2).collect();
    let f = (0..m * m * m)
        .map(|c| if water[c] { ((c as u64).wrapping_mul(2_654_435_761) >> 8) as f32 % 1000.0 / 500.0 - 1.0 } else { 0.0 })
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
    for fixture in [DAM_BREAK, "deep_pool_pressure_problems", "deep_pool_density_problems"] {
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
            if let Some((k, (rz, ps))) = values.iter().enumerate().find(|(_, (rz, ps))| rz.is_nan() || ps.is_nan() || *rz >= 0.0 || *ps <= 0.0) {
                failures.push(format!("{name} {m}³ frame {} ghost {ghost}: iteration {k} r·z {rz:e}, p·s {ps:e} ({values:?})", problem.frame));
            }
        }
    }
    println!("preconditioner: {checked} iterations over {} solves, r·z < 0 and p·s > 0 on every one: {}", problems.len() * 2, failures.is_empty());
    assert!(failures.is_empty(), "{failures:#?}");
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
    // SAFETY: shared buffers sized for the lattice; the last solve completed.
    unsafe {
        rig.water.write(0, bytemuck::cast_slice(&water));
        rig.rhs.write(0, bytemuck::cast_slice(&p.f));
    }
    let floats = PROGRESS_FLOATS as usize;
    let record = rig.device.create_buffer_shared(floats as u64 * 4);
    let mut enc = rig.device.create_encoder("gpu-flip-pressure-converge");
    let n = rig.n as u32;
    let lattice = Water { lattice: [n; 3], cell_size: rig.cell_size() as f32, water: &rig.water, faces: &rig.faces, phi: rig.phi.as_ref() };
    rig.solver.prepare(&rig.device, &mut enc, &lattice).expect("prepares");
    rig.solver.solve(&mut enc, &lattice, run_at!(rig, Stop::Converged(cap))).expect("solves");
    let progress = rig.solver.progress().expect("prepared");
    enc.copy_buffer_to_buffer(progress, &record, record.size);
    enc.commit_and_wait_completed();
    let ptr = record.mapped_ptr().expect("shared record");
    // SAFETY: the copy completed; the buffer holds PROGRESS_FLOATS floats.
    let all = unsafe { std::slice::from_raw_parts(ptr.cast::<f32>().cast_const(), floats) };
    let iterations = all[1] as u32;
    Converged { f_norm: all[0], iterations, stopped: all[2] > 0.5, norms: all[4..4 + iterations as usize].to_vec() }
}

/// One prepare and solve of `p` on `stop`, inside a replay span when `cache`
/// holds one (the step's region on stage): the pressure and the stop's
/// record, as bits.
fn solve_bits(rig: &mut Rig, p: &Problem, stop: Stop, cache: &mut Option<GpuReplayCache>) -> (Vec<u32>, Vec<u32>) {
    solve_bits_under(rig, p, stop, cache, false)
}

/// [`solve_bits`], poisoning the solver's vectors outside its active tiles
/// between the prepare and the solve when `poison`.
fn solve_bits_under(rig: &mut Rig, p: &Problem, stop: Stop, cache: &mut Option<GpuReplayCache>, poison: bool) -> (Vec<u32>, Vec<u32>) {
    let water: Vec<f32> = p.water.iter().map(|&w| f32::from(u8::from(w))).collect();
    // SAFETY: shared buffers sized for the lattice; the last solve completed.
    unsafe {
        rig.water.write(0, bytemuck::cast_slice(&water));
        rig.rhs.write(0, bytemuck::cast_slice(&p.f));
    }
    let floats = PROGRESS_FLOATS as usize;
    let record = rig.device.create_buffer_shared(floats as u64 * 4);
    let mut enc = rig.device.create_encoder("gpu-flip-pressure-replay");
    let spanned = cache.take().map(|cache| enc.begin_replay(&rig.device, cache)).is_some();
    let n = rig.n as u32;
    let lattice = Water { lattice: [n; 3], cell_size: rig.cell_size() as f32, water: &rig.water, faces: &rig.faces, phi: rig.phi.as_ref() };
    rig.solver.prepare(&rig.device, &mut enc, &lattice).expect("prepares");
    if poison {
        rig.solver.poison(&mut enc, rig.level);
    }
    rig.solver.solve(&mut enc, &lattice, run_at!(rig, stop)).expect("solves");
    if spanned {
        *cache = Some(enc.end_replay());
    }
    let progress = rig.solver.progress().expect("prepared");
    enc.copy_buffer_to_buffer(progress, &record, record.size);
    enc.commit_and_wait_completed();
    let ptr = record.mapped_ptr().expect("shared record");
    // SAFETY: the copy completed; the buffer holds PROGRESS_FLOATS floats.
    let all = unsafe { std::slice::from_raw_parts(ptr.cast::<u32>().cast_const(), floats) };
    (bytemuck::cast_slice(rig.pressure()).to_vec(), all.to_vec())
}

/// Exact six-face arithmetic against the original dynamic-loop shader,
/// with odd edges, ghost rows, coarse levels, changing masks and replay.
#[test]
fn pressure_module_unrolled_stencil_matches_original_loop() {
    const ORIGINAL: &str = r#"fn row_weight(row: Row, k: u32) -> f32 {
    if k < 4u {
        return row.lo[k];
    }
    return row.hi[k - 4u];
}

// Cell idx's neighbour across face k (axis k / 2, low side for even k):
// the weight is positive only inside the box, so this never leaves it.
fn across(idx: u32, k: u32) -> u32 {
    let stride = select(select(u.nx * u.ny, u.nx, k < 4u), 1u, k < 2u);
    return select(idx + stride, idx - stride, (k & 1u) == 0u);
}

fn stencil(idx: u32, source: u32) -> Stencil {
    let row = rows[idx];
    var s = Stencil(select(row.hi.w, row.hi.z, u.ghost == 1u), 0.0);
    if source == 2u {
        return s;
    }
    for (var k = 0u; k < 6u; k = k + 1u) {
        let w = row_weight(row, k);
        if w > 0.0 {
            let at = across(idx, k);
            if source == 0u {
                s.sum = s.sum + w * out[at];
            } else {
                s.sum = s.sum + w * aux[at];
            }
        }
    }
    return s;
}

"#;
    let shader = include_str!("shaders/gpu_flip_pressure.wgsl");
    let start = shader.find("fn stencil_add(").expect("unrolled stencil helper");
    let end = shader.find("// One red-black Gauss-Seidel").expect("stencil end");
    let mut original = shader.to_owned();
    original.replace_range(start..end, ORIGINAL);
    let (n, saved) = load_fixture(DAM_BREAK);
    for (m, level, ghost) in [(16, 0, false), (25, 0, true), (32, 1, true), (64, 0, false), (64, 0, true), (128, 0, true)] {
        let mut current = Rig::new(m).at_level(level);
        let mut reference = Rig::new(m).at_level(level);
        reference.solver.set_stencil_shader_for_proof(&reference.device, &original);
        let mut current_cache = Some(GpuReplayCache::default());
        let mut reference_cache = Some(GpuReplayCache::default());
        for frame in 0..4 {
            let problem = resample(&saved[if frame % 2 == 0 { 0 } else { 4 }], n, m);
            if ghost {
                let phi = surface_phi(&problem.water, m, current.cell_size() as f32);
                current = current.with_phi(&phi);
                reference = reference.with_phi(&phi);
            }
            let stop = if frame < 2 { Stop::Fixed(8) } else { Stop::Converged(MAX_ITERATIONS) };
            let got = solve_bits_under(&mut current, &problem, stop, &mut current_cache, true);
            let before = solve_bits_under(&mut reference, &problem, stop, &mut reference_cache, true);
            assert_eq!(got.0, before.0, "{m}³ level{level} ghost{ghost} frame{frame}: pressure");
            assert_eq!(got.1, before.1, "{m}³ level{level} ghost{ghost} frame{frame}: stop record");
        }
        if level == 0 && (m == 64 || m == 128) {
            let problem = resample(&saved[4], n, m);
            let mut current_ms = Vec::new();
            let mut reference_ms = Vec::new();
            for sample in 0..12 {
                let (now, old) = if sample % 2 == 0 {
                    (current.run(&problem, 8, false).total_ms, reference.run(&problem, 8, false).total_ms)
                } else {
                    let old = reference.run(&problem, 8, false).total_ms;
                    (current.run(&problem, 8, false).total_ms, old)
                };
                assert_eq!(bytemuck::cast_slice::<f32, u32>(current.pressure()), bytemuck::cast_slice::<f32, u32>(reference.pressure()), "timed solve stays exact");
                if sample >= 4 {
                    current_ms.push(now);
                    reference_ms.push(old);
                }
            }
            current_ms.sort_by(f64::total_cmp);
            reference_ms.sort_by(f64::total_cmp);
            let median = |v: &[f64]| (v[3] + v[4]) / 2.0;
            println!("pressure stencil {m}³ ghost{ghost}:8-iteration prepare+solve,4warm+8measured, unrolled {:.4} ms original {:.4} ms", median(&current_ms), median(&reference_ms));
        }
    }
}

/// Paired x cells retain the original smoother's arithmetic and reduction
/// positions, including sparse partial tiles, ghost rows and warmed replay.
#[test]
fn gpu_flip_paired_smoother_matches_original() {
    // Exact smooth_main declaration/body from 5bc9e128c; every other shader
    // function remains current, so this oracle isolates cell pairing.
    const ORIGINAL: &str = r#"@compute @workgroup_size(256, 1, 1)
fn smooth_main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if !listed(gid.x) {
        return;
    }
    let n = lattice();
    let idx = listed_cell(gid.x, n);
    var product = 0.0;
    if idx != NO_CELL {
        let p = coords(idx, n);
        let swept = is_water(idx) && u32(p.x + p.y + p.z) % 2u == u.color;
        var e = out[idx];
        if swept {
            let h2 = u.cell_size * u.cell_size;
            let s = stencil(idx, select(0u, 2u, from_zero()));
            e = select(0.0, (s.sum - h2 * src[idx]) / s.diagonal, s.diagonal > 0.0);
            out[idx] = e;
        } else if from_zero() {
            e = 0.0;
            out[idx] = e;
        }
        product = src[idx] * e;
    }
    if reduces() {
        fold_sum(li, listed_partial(gid.x), product);
    }
}
"#;
    const N: [usize; 3] = [17, 15, 13];
    const H: f32 = 0.25;
    const ROUNDS: u32 = 32;

    struct Run {
        device: crate::TestDevice,
        solver: PressureSolver,
        // Water, faces, rhs, phi, pressure, scalar copy, progress copy, fine flags.
        buffers: [GpuBuffer; 8],
        ghost: bool,
        cache: Option<GpuReplayCache>,
    }
    impl Run {
        fn new(water: &[f32], faces: &[[f32; 8]], phi: &[f32], ghost: bool, original: Option<&str>, replay: bool) -> Self {
            let device = crate::test_device();
            let counts = [water.len(), faces.len() * 8, water.len(), phi.len(), water.len(), 2 * MAX_ITERATIONS as usize, PROGRESS_FLOATS as usize, 12];
            let buffers = counts.map(|count| device.create_buffer_shared((count * 4) as u64));
            // SAFETY: shared buffers sized above; no GPU work is queued.
            unsafe {
                buffers[0].write(0, bytemuck::cast_slice(water));
                buffers[1].write(0, bytemuck::cast_slice(faces));
                buffers[3].write(0, bytemuck::cast_slice(phi));
            }
            let mut solver = PressureSolver::default();
            solver.prepare_pipelines(&device);
            if let Some(shader) = original {
                solver.set_stencil_shader_for_proof(&device, shader);
            }
            Self { device, solver, buffers, ghost, cache: replay.then(GpuReplayCache::default) }
        }

        fn solve(&mut self, rhs: &[f32], stop: Stop) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
            // SAFETY: shared rhs sized for the lattice; the last solve completed.
            unsafe { self.buffers[2].write(0, bytemuck::cast_slice(rhs)) };
            let mut enc = self.device.create_encoder("paired pressure smoother proof");
            let replay = self.cache.take().map(|cache| enc.begin_replay(&self.device, cache)).is_some();
            let water = Water {
                lattice: N.map(|v| v as u32), cell_size: H, water: &self.buffers[0], faces: &self.buffers[1],
                phi: self.ghost.then_some(&self.buffers[3]),
            };
            self.solver.prepare(&self.device, &mut enc, &water).expect("prepares odd sparse lattice");
            self.solver.solve(&mut enc, &water, Solve {
                rhs: &self.buffers[2], pressure: &self.buffers[4], stop, bodies: None, level: 0, coarse_rhs: None,
            }).expect("solves odd sparse lattice");
            if replay {
                self.cache = Some(enc.end_replay());
            }
            self.solver.copy_scalars(&mut enc, &self.buffers[5]);
            enc.copy_buffer_to_buffer(self.solver.progress().expect("prepared"), &self.buffers[6], self.buffers[6].size);
            enc.copy_buffer_to_buffer(self.solver.tiles().expect("prepared")[1], &self.buffers[7], self.buffers[7].size);
            enc.commit_and_wait_completed();
            let flags: Vec<u32> = read(&self.buffers[7], 12);
            assert!(flags.contains(&0) && flags.contains(&1), "fine tiles include active and inactive tiles");
            assert_eq!(flags[11], 1, "the partial high corner tile is active");
            (read(&self.buffers[4], N.iter().product()), read(&self.buffers[5], 2 * MAX_ITERATIONS as usize), read(&self.buffers[6], PROGRESS_FLOATS as usize))
        }
    }

    let shader = include_str!("shaders/gpu_flip_pressure.wgsl");
    let declaration = shader.find("fn smooth_main(").expect("current smoother");
    let start = shader[..declaration].rfind("@compute").expect("smoother workgroup declaration");
    let end = declaration + shader[declaration..].find("\n}\n").expect("smoother body end") + 3;
    let mut original = shader.to_owned();
    original.replace_range(start..end, ORIGINAL);

    let cells = N.iter().product::<usize>();
    let water: Vec<f32> = (0..cells).map(|i| {
        let [x, y, z] = lattice_coords(i, N);
        f32::from(u8::from(x >= 10 && y >= 8 && z >= 5))
    }).collect();
    let faces_n = N.map(|v| v + 1);
    let faces: Vec<[f32; 8]> = (0..faces_n.iter().product()).map(|i| {
        let p = lattice_coords(i, faces_n);
        let open = |a: usize| f32::from(u8::from((0..3).all(|b| b == a || p[b] < N[b]) && p[a] > 0 && p[a] < N[a]));
        [0.0, 0.0, 0.0, 0.0, open(0), open(1), open(2), 0.0]
    }).collect();
    let phi: Vec<f32> = water.iter().map(|&w| if w > 0.5 { -0.11 } else { 0.14 }).collect();
    for ghost in [false, true] {
        let mut direct = Run::new(&water, &faces, &phi, ghost, None, false);
        let mut reference = Run::new(&water, &faces, &phi, ghost, Some(&original), false);
        let mut replay = Run::new(&water, &faces, &phi, ghost, None, true);
        let mut last = GpuReplayCache::default().stats();
        for visit in 0..4 {
            let rhs: Vec<f32> = (0..cells).map(|i| {
                if visit == 3 || water[i] < 0.5 { 0.0 } else { ((i * 17 + visit * 13) % 41) as f32 * 0.037 - 0.6 }
            }).collect();
            let stop = if visit < 2 { Stop::Fixed(ROUNDS) } else { Stop::Converged(ROUNDS) };
            let want = reference.solve(&rhs, stop);
            let got = direct.solve(&rhs, stop);
            let cached = replay.solve(&rhs, stop);
            assert_eq!(got, want, "ghost{ghost} visit{visit}: full pressure/scalars/progress equal original smoother");
            assert_eq!(cached, want, "ghost{ghost} visit{visit}: replay equals original direct bits");
            assert!(got.0.iter().all(|&v| f32::from_bits(v).is_finite()), "finite pressure");
            if visit < 2 {
                assert_eq!(got.2[1], (ROUNDS as f32).to_bits(), "fixed iteration count");
                assert!(got.0.iter().any(|&v| f32::from_bits(v).abs() > 1e-6), "nontrivial pressure");
            } else if visit == 2 {
                assert_eq!(got.2[2], 1.0_f32.to_bits(), "nonzero RHS converges");
                let iterations = f32::from_bits(got.2[1]);
                assert!(iterations > 0.0 && iterations < ROUNDS as f32, "nonzero RHS stops inside the cap");
            } else if visit == 3 {
                assert_eq!(got.2[1], 0, "zero RHS stops before smoothing");
                assert_eq!(got.2[2], 1.0_f32.to_bits(), "converged early stop");
            }
            let stats = replay.cache.as_ref().expect("replay cache returned").stats();
            if visit >= 2 {
                assert_eq!(stats.recorded, last.recorded, "warm solve records nothing");
                assert_eq!(stats.store_allocations, last.store_allocations, "warm solve allocates no replay storage");
                assert_eq!(stats.segments_direct, last.segments_direct, "warm rounds never dispatch directly");
                assert_eq!(stats.segments_replayed - last.segments_replayed, u64::from(ROUNDS), "every scheduled round is replayed");
            }
            last = stats;
        }
    }
}

/// A solve replayed from a recording (every round one gated segment the
/// GPU switches off once the stop fires) gives the same pressure and the
/// same stop record, bit for bit, as encoding it directly, on the engine's
/// stop and at a fixed count, over frames whose problem changes. Once warm,
/// a frame records nothing and runs no round directly. Two direct solves
/// of one problem agree bit for bit too: the folded reductions sum in a
/// fixed order.
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
        for frame in 0..6 {
            let p = &problems[frame % problems.len()];
            let (dp, dr) = solve_bits(&mut direct, p, stop, &mut none);
            let (ap, ar) = solve_bits(&mut again, p, stop, &mut none);
            assert!(dp == ap && dr == ar, "{stop:?} frame {frame}: two direct solves differ");
            let (rp, rr) = solve_bits(&mut replay, p, stop, &mut cache);
            assert!(dp == rp, "{stop:?} frame {frame}: the replayed pressure differs from direct");
            assert_eq!(dr, rr, "{stop:?} frame {frame}: the replayed stop record differs from direct");
            let stats = cache.as_ref().expect("the span handed its cache back").stats();
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
                assert_eq!(stats.recorded, last.recorded, "{stop:?} frame {frame}: a warm solve records nothing");
                assert_eq!(stats.segments_direct, last.segments_direct, "{stop:?} frame {frame}: no round runs directly");
                let rounds = match stop {
                    Stop::Converged(cap) | Stop::Fixed(cap) => u64::from(cap),
                };
                assert_eq!(stats.segments_replayed - last.segments_replayed, rounds, "{stop:?} frame {frame}: every round is one segment execute");
            }
            last = stats;
        }
    }
}

/// The solve over each level's active tiles equals the solve over every
/// tile, bit for bit, in the pressure and the stop record, on the engine's
/// stop and at a fixed count, direct and replayed, over Dam Break frames at
/// 64³ and 25³ (odd sides: partial edge tiles); and the same with NaN
/// written into every vector cell and fine partial outside the active tiles
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
                for (name, (xp, xr)) in [("sparse", (&sp, &sr)), ("poisoned", (&pp, &pr)), ("poisoned replay", (&rp, &rr))] {
                    assert_eq!(&dr, xr, "{m}³ level {level} {stop:?} frame {frame}: the {name} stop record differs from all tiles");
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
    let f = (0..m * m * m).map(|c| if water[c] && (c / m).is_multiple_of(m) { -inflow } else { 0.0 }).collect();
    Problem { frame: 0, water, f }
}

/// The engine's stop on every shipped problem: each solve stops on the
/// tolerance, never the cap. The still and resting pools stop within 12
/// iterations (measured 11), the Dam Break splash frames within 16 (measured
/// 12 to 14). The stop reads the recursive residual, as the engine's PCG
/// does, and f32 carries it below 1e-9 · |f|∞ on all of them.
#[test]
fn pressure_module_converges_on_the_engine_tolerance() {
    let mut failures = Vec::new();
    let mut problems: Vec<(String, Problem)> = Vec::new();
    for fixture in [DAM_BREAK, "deep_pool_pressure_problems", "deep_pool_density_problems"] {
        let (n, saved) = load_fixture(fixture);
        for p in &saved {
            problems.push((fixture.to_string(), resample(p, n, 64)));
        }
    }
    problems.push(("still_pool".into(), still_pool_problem(64)));
    problems.push(("resting_pool".into(), resting_pool_problem(64)));
    for (name, p) in &problems {
        let mut rig = Rig::new(64);
        let c = converge(&mut rig, p, MAX_ITERATIONS);
        let rel: Vec<String> = c.norms.iter().map(|r| format!("{:.1e}", r / c.f_norm)).collect();
        println!("curve {name} frame {}: |f| {:.3e} it {} stopped {} rel {}", p.frame, c.f_norm, c.iterations, c.stopped, rel.join(" "));
        let limit = if name.ends_with("pool") { 12 } else { 16 };
        if !c.stopped || c.iterations > limit {
            failures.push(format!("{name} frame {}: {} iterations, stopped {}", p.frame, c.iterations, c.stopped));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
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
    let mut f: Vec<f32> = (0..cells).map(|c| ((c as u64).wrapping_mul(2_654_435_761) >> 8) as f32 % 1000.0 / 500.0 - 1.0).collect();
    let mean = f.iter().map(|&v| f64::from(v)).sum::<f64>() / cells as f64;
    for v in &mut f {
        *v -= mean as f32;
    }
    let mut water = vec![true; cells];
    let unpinned = converge(&mut Rig::new(m), &Problem { frame: 0, water: water.clone(), f: f.clone() }, MAX_ITERATIONS);
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
    assert!(c.stopped, "pinned sealed box ran {} iterations to the cap", c.iterations);
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
        let q = if k % 2 == 0 { c - stride[a] } else { c + stride[a] };
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

/// The rows prepare assembles are what the face stencil computed per sweep:
/// the fused (assembled once) operator against the unfused (recomputed)
/// one, with the ghost rows and plain, on a Dam Break frame at 64 and the
/// odd side 37. The weights and the plain diagonal match the CPU bit for
/// bit; the ghost diagonal's θ is a division the GPU rounds its own way, so
/// that slot is within `GHOST_ULPS`. (GPU against GPU, the row solve was
/// bit-equal to the stencil solve it replaced, with and without φ.)
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
            let cells = m * m * m;
            let copy = rig.device.create_buffer_shared((cells * ROW_FLOATS * 4) as u64);
            let mut enc = rig.device.create_encoder("gpu-flip-pressure-rows");
            rig.solver.copy_rows(&mut enc, &copy);
            enc.commit_and_wait_completed();
            let ptr = copy.mapped_ptr().expect("shared rows");
            // SAFETY: the copy completed; the buffer holds cells × ROW_FLOATS floats.
            let rows = unsafe { std::slice::from_raw_parts(ptr.cast::<f32>().cast_const(), cells * ROW_FLOATS) };
            let mut wrong = 0;
            let mut ghosted = 0;
            let mut ulps = 0;
            for c in 0..cells {
                let expected = cpu_row(&problem.water, with_phi.then_some(phi.as_slice()), c, m, h);
                let got = &rows[c * ROW_FLOATS..(c + 1) * ROW_FLOATS];
                let exact = (0..ROW_FLOATS).filter(|&k| k != 6).all(|k| got[k].to_bits() == expected[k].to_bits());
                let apart = got[6].to_bits().abs_diff(expected[6].to_bits());
                ulps = ulps.max(apart);
                if !exact || apart > GHOST_ULPS || got[6].is_sign_negative() != expected[6].is_sign_negative() {
                    wrong += 1;
                    if wrong <= 3 {
                        println!("rows {m}³ phi {with_phi} cell {c}: got {got:?}, expected {expected:?}");
                    }
                }
                ghosted += usize::from(expected[6].to_bits() != expected[7].to_bits());
            }
            println!("rows {m}³ phi {with_phi}: ghost diagonal at most {ulps} ulp from the CPU's");
            assert_eq!(wrong, 0, "{m}³ with phi {with_phi}: rows differ from the stencil");
            assert_eq!(ghosted > 0, with_phi, "{m}³: the ghost diagonal differs from the plain one exactly with φ");
        }
    }
}

/// Separating solids (GPU_FLIP_PRESSURE_SOLVE.md section 8 (Separating
/// solids)) against `scripts/mgpcg_reference.py --separating n,0.25,0.016667`:
/// a pool a quarter deep after one step of ±20 m/s², the active set run over
/// the GPU solve (each round a converged solve on the water less the let-go
/// cells, then the join and leave rule) to the reference's rounds, let-go
/// count and pressure. Down: 1 round, nothing let go, minimum pressure
/// 8.333e-2 (16³) and 4.167e-2 (32³). Up: 2 rounds, every wall-touching
/// cell let go (436 and 1892), the pressure 0 on every water cell.
#[test]
fn pressure_module_separating_matches_reference() {
    for (m, g, rounds, let_go, p_min) in
        [(16, -20.0, 1, 0, 8.333e-2), (16, 20.0, 2, 436, 0.0), (32, -20.0, 1, 0, 4.167e-2), (32, 20.0, 2, 1892, 0.0)]
    {
        let cells = m * m * m;
        let h = BOX_METRES / m as f64;
        let rows = ((0.25 * m as f64).round() as usize).max(1);
        let at = |x: usize, y: usize, z: usize| x + m * (y + m * z);
        let water: Vec<bool> = (0..cells).map(|c| (c / m) % m < rows).collect();
        let f: Vec<f32> = (0..cells).map(|c| if water[c] && (c / m) % m == 0 { (g / 60.0 / h) as f32 } else { 0.0 }).collect();
        // Every inner face is whole, so a cell touches a solid on the box walls.
        let touching: Vec<bool> = (0..cells)
            .map(|c| {
                let p = [c % m, (c / m) % m, c / (m * m)];
                water[c] && p.iter().any(|&v| v == 0 || v == m - 1)
            })
            .collect();
        let mut rig = Rig::new(m);
        let mut out = vec![false; cells];
        let mut pressure = Vec::new();
        let mut taken = 0;
        for round in 1..=64 {
            taken = round;
            let mask: Vec<bool> = (0..cells).map(|c| water[c] && !out[c]).collect();
            let c = converge(&mut rig, &Problem { frame: 0, water: mask.clone(), f: f.clone() }, MAX_ITERATIONS);
            assert!(c.stopped || c.f_norm == 0.0, "{m}³ g {g} round {round}: the solve did not stop on its tolerance");
            pressure = rig.pressure().to_vec();
            let p_at = |c: usize| if mask[c] { f64::from(pressure[c]) } else { 0.0 };
            let mut changed = false;
            for c in 0..cells {
                if mask[c] && touching[c] && pressure[c] < 0.0 {
                    out[c] = true;
                    changed = true;
                } else if out[c] {
                    let [x, y, z] = [c % m, (c / m) % m, c / (m * m)];
                    let mut pushed = 0.0;
                    for (q, inside) in [
                        (x.wrapping_sub(1), x > 0),
                        (x + 1, x + 1 < m),
                    ] {
                        if inside {
                            pushed += p_at(at(q, y, z));
                        }
                    }
                    for (q, inside) in [(y.wrapping_sub(1), y > 0), (y + 1, y + 1 < m)] {
                        if inside {
                            pushed += p_at(at(x, q, z));
                        }
                    }
                    for (q, inside) in [(z.wrapping_sub(1), z > 0), (z + 1, z + 1 < m)] {
                        if inside {
                            pushed += p_at(at(x, y, q));
                        }
                    }
                    if f64::from(f[c]) - pushed / (h * h) < 0.0 {
                        out[c] = false;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        let count = out.iter().filter(|&&o| o).count();
        let low = (0..cells).filter(|&c| water[c]).map(|c| if out[c] { 0.0 } else { f64::from(pressure[c]) }).fold(f64::INFINITY, f64::min);
        println!("separating {m}³ g {g:+}: {taken} rounds, {count} let go, pressure min {low:.4e}");
        assert_eq!((taken, count), (rounds, let_go), "{m}³ g {g}: rounds and let-go count against the reference");
        let tolerance = if p_min == 0.0 { 1e-6 } else { 1e-3 * p_min };
        assert!((low - p_min).abs() <= tolerance, "{m}³ g {g}: pressure min {low} against the reference {p_min}");
    }
}

// ── Solve Level (docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md section 11) ──────────

/// FNV-1a over words: a run's bits as one number to pin.
fn fingerprint(words: &[u32]) -> u64 {
    words.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &w| (h ^ u64::from(w)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// Solve Level 0 is the fine solve: on the first three Dam Break problems at
/// 64³ the engine's stop lands within one iteration of the count before the
/// level existed (12, 12, 13), never on the cap, and a run repeats bit for
/// bit. The value-level pin is [`pressure_module_matches_reference_64`]
/// (3 and 8 iterations against the f64 reference). The bitwise pin against
/// main went when the paper's cell-type coarsening came in (section 11
/// (Solve Level)): a coarse cell over water and solid children is water
/// now, which moves every level's rows under a body or a floor.
#[test]
fn gpu_flip_solve_level_zero_is_the_fine_step() {
    const BEFORE: [(u32, u32); 3] = [(0, 12), (15, 12), (30, 13)];
    let (n, saved) = load_fixture(DAM_BREAK);
    let mut rig = Rig::new(64);
    assert_eq!(rig.level, 0);
    let mut none = None;
    let mut failures = Vec::new();
    for (p, &(frame, before)) in saved.iter().zip(&BEFORE) {
        assert_eq!(p.frame, frame);
        let problem = resample(p, n, 64);
        let c = converge(&mut rig, &problem, MAX_ITERATIONS);
        let (pressure, record) = solve_bits(&mut rig, &problem, Stop::Converged(MAX_ITERATIONS), &mut none);
        let again = solve_bits(&mut rig, &problem, Stop::Converged(MAX_ITERATIONS), &mut none);
        println!(
            "solve level 0 frame {frame}: {} iterations (before the paper's coarsening {before}), stopped {}, pressure {:#018x} record {:#018x}",
            c.iterations,
            c.stopped,
            fingerprint(&pressure),
            fingerprint(&record)
        );
        if !c.stopped || c.iterations.abs_diff(before) > 1 {
            failures.push(format!("frame {frame}: {} iterations, stopped {}, against {before} before", c.iterations, c.stopped));
        }
        if (pressure, record) != again {
            failures.push(format!("frame {frame}: a repeated solve differs"));
        }
    }
    assert!(failures.is_empty(), "level 0 moved: {failures:#?}");
}

/// The solve on V-cycle level 1 against the reference run there
/// (`scripts/mgpcg_reference.py FIXTURE --solve-level 1 --iterations 3,8,16`):
/// the level's own residual, at the fine pins' bars.
#[test]
fn pressure_module_solve_level_matches_reference_64() {
    check_at(
        DAM_BREAK,
        64,
        1,
        &[
            (0, 4.898e-03, 7.396e-08),
            (15, 1.253e-02, 5.298e-07),
            (30, 2.716e-02, 6.876e-07),
            (45, 9.892e-03, 1.073e-06),
            (60, 2.282e-02, 1.595e-06),
            (90, 1.042e-02, 3.412e-07),
            (120, 9.030e-03, 1.300e-06),
        ],
    );
}

#[test]
fn pressure_module_solve_level_matches_reference_128() {
    check_at(
        DAM_BREAK,
        128,
        1,
        &[
            (0, 1.500e-02, 5.702e-07),
            (15, 2.760e-02, 4.391e-06),
            (30, 7.460e-02, 1.074e-05),
            (45, 3.245e-02, 9.446e-06),
            (60, 3.688e-02, 1.074e-05),
            (90, 1.972e-02, 5.322e-06),
            (120, 1.942e-02, 8.970e-06),
        ],
    );
}

/// Odd and uneven sides at level 1: the virtual solid cell past an odd side
/// restricts as the script pads it.
#[test]
fn pressure_module_solve_level_matches_reference_odd_sides() {
    check_at(
        DAM_BREAK,
        25,
        1,
        &[
            (0, 3.575e-04, 1.371e-10),
            (15, 8.813e-04, 8.661e-10),
            (30, 1.163e-03, 1.613e-09),
            (45, 1.510e-03, 3.253e-09),
            (60, 1.838e-03, 7.043e-09),
            (90, 1.755e-03, 1.049e-08),
            (120, 4.252e-04, 1.184e-09),
        ],
    );
    check_at(
        DAM_BREAK,
        37,
        1,
        &[
            (0, 2.826e-03, 2.159e-08),
            (15, 2.816e-03, 3.288e-08),
            (30, 6.902e-03, 1.761e-07),
            (45, 4.523e-03, 3.903e-08),
            (60, 2.012e-03, 1.056e-08),
            (90, 4.742e-03, 2.070e-08),
            (120, 4.631e-03, 7.286e-08),
        ],
    );
    check_at(
        DAM_BREAK,
        40,
        1,
        &[
            (0, 4.102e-03, 6.875e-08),
            (15, 4.303e-03, 3.565e-08),
            (30, 6.129e-03, 7.271e-08),
            (45, 5.492e-03, 2.154e-07),
            (60, 4.579e-03, 7.953e-08),
            (90, 7.210e-03, 1.326e-07),
            (120, 4.939e-03, 2.259e-07),
        ],
    );
}

/// The divergence left on the fine faces after a project from the fine
/// pressure P·p₁: per cell f − L₀(P p₁) on the box (the step's project is
/// that subtraction, the walls closed), as |·|∞ over |f|∞, its 2-norm over
/// |f|₂, and the largest box mean over a coarse cell's water children over
/// |f|∞, beside the same box mean of f itself. Level 0 is the side column.
/// The coarse solve guarantees the Rᵀ-weighted means, not the box means or
/// the fine remainder, and a particle divergence is mostly finer than a
/// coarse cell: level 1 leaves the cell-scale divergence in place (the
/// measured ratios sit near 1), which is the trade the level makes. The
/// ceilings are pinned from the run that introduced the level (every Dam
/// Break problem at 64³ on the engine's stop); a change that raises one
/// fails.
#[test]
fn gpu_flip_solve_level_divergence_bound() {
    // (frame, |r|∞/|f|∞ at level 1, largest box mean / |f|∞ at level 1) plus 5%.
    const CEILINGS: [(u32, f64, f64); 7] = [
        (0, 1.542, 1.379),
        (15, 1.220, 0.7301),
        (30, 1.050, 0.5687),
        (45, 1.172, 0.5434),
        (60, 1.055, 0.8567),
        (90, 1.017, 0.8210),
        (120, 1.043, 0.9151),
    ];
    let (n, saved) = load_fixture(DAM_BREAK);
    let m = 64;
    let mut rigs = [Rig::new(m), Rig::new(m).at_level(1)];
    let mut failures = Vec::new();
    for (p, &(frame, inf_ceiling, mean_ceiling)) in saved.iter().zip(&CEILINGS) {
        assert_eq!(p.frame, frame);
        let problem = resample(p, n, m);
        let f_inf = problem.f.iter().map(|v| f64::from(v.abs())).fold(0.0, f64::max);
        let f64s: Vec<f64> = problem.f.iter().map(|&v| f64::from(v)).collect();
        let f_mean = box_means(&f64s, &problem.water, m).into_iter().map(f64::abs).fold(0.0, f64::max) / f_inf;
        println!("divergence bound frame {frame:3}: largest box mean of f/|f|∞ {f_mean:.4e}");
        let mut got = [(0.0, 0.0, 0.0); 2];
        for (level, rig) in rigs.iter_mut().enumerate() {
            let c = converge(rig, &problem, MAX_ITERATIONS);
            assert!(c.stopped, "frame {frame} level {level}: {} iterations to the cap", c.iterations);
            let r = fine_remainder(rig.pressure(), &problem.water, &problem.f, m, rig.cell_size());
            let inf = r.iter().map(|v| v.abs()).fold(0.0, f64::max) / f_inf;
            let two = residual(rig.pressure(), &problem.water, &problem.f, m, rig.cell_size());
            let mean = box_means(&r, &problem.water, m).into_iter().map(f64::abs).fold(0.0, f64::max) / f_inf;
            got[level] = (inf, two, mean);
            println!(
                "divergence bound frame {frame:3} level {level}: {} iterations; |r|∞/|f|∞ {inf:.4e}, |r|₂/|f|₂ {two:.4e}, largest box mean/|f|∞ {mean:.4e}",
                c.iterations
            );
        }
        let (inf, _, mean) = got[1];
        if inf > inf_ceiling || mean > mean_ceiling {
            failures.push(format!("frame {frame}: |r|∞/|f|∞ {inf:.4e} (ceiling {inf_ceiling:.4e}), box mean {mean:.4e} (ceiling {mean_ceiling:.4e})"));
        }
    }
    assert!(failures.is_empty(), "the level 1 remainder rose: {failures:#?}");
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
                for next in [at[a].checked_sub(1), Some(at[a] + 1).filter(|&q| q < n)].into_iter().flatten() {
                    let q = c + next * stride[a] - at[a] * stride[a];
                    sum += if water[q] { f64::from(p[q]) } else { 0.0 } - centre;
                }
            }
            f64::from(f[c]) - sum / (h * h)
        })
        .collect()
}

/// The mean of `r` over each level-1 cell's water children (zero with none).
fn box_means(r: &[f64], water: &[bool], n: usize) -> Vec<f64> {
    let c = n.div_ceil(2);
    (0..c * c * c)
        .map(|i| {
            let at = [i % c, (i / c) % c, i / (c * c)];
            let (mut sum, mut count) = (0.0, 0);
            for child in 0..8 {
                let q = [2 * at[0] + (child & 1), 2 * at[1] + (child >> 1 & 1), 2 * at[2] + (child >> 2)];
                if q.iter().any(|&v| v >= n) {
                    continue;
                }
                let cell = q[0] + n * (q[1] + n * q[2]);
                if water[cell] {
                    sum += r[cell];
                    count += 1;
                }
            }
            if count == 0 { 0.0 } else { sum / f64::from(count) }
        })
        .collect()
}

/// The engine's stop at Solve Level 1 on every shipped problem: each solve
/// stops on the tolerance, never the cap, within the fine level's bar; the
/// counts beside the fine ones. The deepest level each lattice offers is
/// levels − 2, and that level stops too. Level 0 lands within one iteration
/// of its count before the paper's cell-type coarsening came in (BEFORE,
/// measured on the same problems): the rule may move the rows under a body
/// or a floor, never the convergence on these.
#[test]
fn pressure_module_solve_level_converges_on_the_engine_tolerance() {
    const BEFORE: [(&str, u32, u32); 12] = [
        (DAM_BREAK, 0, 12),
        (DAM_BREAK, 15, 12),
        (DAM_BREAK, 30, 13),
        (DAM_BREAK, 45, 14),
        (DAM_BREAK, 60, 13),
        (DAM_BREAK, 90, 12),
        (DAM_BREAK, 120, 14),
        ("deep_pool_pressure_problems", 60, 11),
        ("deep_pool_density_problems", 30, 12),
        ("deep_pool_density_problems", 60, 12),
        ("still_pool", 0, 11),
        ("resting_pool", 0, 11),
    ];
    let mut failures = Vec::new();
    let mut problems: Vec<(String, Problem)> = Vec::new();
    for fixture in [DAM_BREAK, "deep_pool_pressure_problems", "deep_pool_density_problems"] {
        let (n, saved) = load_fixture(fixture);
        for p in &saved {
            problems.push((fixture.to_string(), resample(p, n, 64)));
        }
    }
    problems.push(("still_pool".into(), still_pool_problem(64)));
    problems.push(("resting_pool".into(), resting_pool_problem(64)));
    let deepest = max_solve_level([64; 3]);
    assert_eq!(deepest, 3, "64³ halves to 32, 16, 8, 4: the gradient runs on 0 to 3");
    for (name, p) in &problems {
        let mut counts = Vec::new();
        for level in [0, 1, deepest] {
            let mut rig = Rig::new(64).at_level(level);
            let c = converge(&mut rig, p, MAX_ITERATIONS);
            let limit = if name.ends_with("pool") { 12 } else { 16 };
            if !c.stopped || (level <= 1 && c.iterations > limit) {
                failures.push(format!("{name} frame {} level {level}: {} iterations, stopped {}", p.frame, c.iterations, c.stopped));
            }
            if level == 0 {
                let before = BEFORE.iter().find(|row| row.0 == name && row.1 == p.frame).map(|row| row.2).expect("a count before the rule per problem");
                if c.iterations.abs_diff(before) > 1 {
                    failures.push(format!("{name} frame {} level 0: {} iterations against {before} before the paper's coarsening", p.frame, c.iterations));
                }
            }
            counts.push(format!("level {level}: {} iterations{}", c.iterations, if c.stopped { "" } else { " (capped)" }));
        }
        println!("solve level counts {name} frame {}: {}", p.frame, counts.join(", "));
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A level the lattice lacks is refused by name before any GPU work, at the
/// solver and at the step's reading of the param; the deepest level is the
/// one above the exactly-solved coarsest.
#[test]
fn pressure_module_refuses_a_level_past_the_coarsest() {
    use super::gpu_flip_step::read_solve_level;
    let (n, saved) = load_fixture(DAM_BREAK);
    let problem = resample(&saved[0], n, 64);
    let mut rig = Rig::new(64).at_level(4);
    let water: Vec<f32> = problem.water.iter().map(|&w| f32::from(u8::from(w))).collect();
    // SAFETY: shared buffers sized for the lattice; nothing is in flight.
    unsafe {
        rig.water.write(0, bytemuck::cast_slice(&water));
        rig.rhs.write(0, bytemuck::cast_slice(&problem.f));
    }
    let mut enc = rig.device.create_encoder("gpu-flip-pressure-refusal");
    let lattice = Water { lattice: [64; 3], cell_size: rig.cell_size() as f32, water: &rig.water, faces: &rig.faces, phi: None };
    rig.solver.prepare(&rig.device, &mut enc, &lattice).expect("prepares");
    let refused = rig.solver.solve(&mut enc, &lattice, run_at!(rig, Stop::Fixed(1))).expect_err("level 4 on 64³");
    assert_eq!(refused, "Solve Level must be 0 to 3 on a [64, 64, 64] lattice, not 4");
    assert_eq!(read_solve_level(4.0, [64; 3]).expect_err("the step's reading"), refused);
    assert_eq!(read_solve_level(3.0, [64; 3]), Ok(3));
    assert_eq!(read_solve_level(1.0, [25; 3]), Ok(1));
    assert!(read_solve_level(3.0, [25; 3]).is_err(), "25³ halves to 13, 7, 4: level 3 is the coarsest");
}
