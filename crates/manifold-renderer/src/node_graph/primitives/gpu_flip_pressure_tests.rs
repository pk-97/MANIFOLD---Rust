//! The pressure solver module against the f64 reference
//! (`scripts/mgpcg_reference.py`, the default rule: halve rounding up to 4
//! or less, then the exact inverse) on the saved Dam Break and deep pool
//! problems at 64³ and refined to 128³, and resampled to the odd and uneven
//! sides 25, 37 and 40. The residual is the true relative residual of the
//! masked Poisson equation.

use manifold_gpu::GpuBuffer;

use super::gpu_flip_pressure::{PressureSolver, Water, level_lattices, passes};

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
    let (mut miss, mut size) = (0.0, 0.0);
    for c in (0..n * n * n).filter(|&c| water[c]) {
        let at = [c % n, (c / n) % n, c / (n * n)];
        let stride = [1, n, n * n];
        let centre = f64::from(p[c]);
        let mut sum = 0.0;
        for a in 0..3 {
            for next in [at[a].checked_sub(1), Some(at[a] + 1).filter(|&q| q < n)].into_iter().flatten() {
                let neighbour = c + next * stride[a] - at[a] * stride[a];
                let value = if water[neighbour] { f64::from(p[neighbour]) } else { 0.0 };
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

/// The box side in metres the fixtures were saved at.
const BOX_METRES: f64 = 4.0;

/// The residual f32 arithmetic holds on these problems: past it the f64
/// reference keeps falling and the GPU cannot follow.
const F32_FLOOR: f64 = 3e-5;

/// The solver on one cubic lattice, its inputs in shared buffers.
struct Rig {
    n: usize,
    device: crate::TestDevice,
    solver: PressureSolver,
    water: GpuBuffer,
    faces: GpuBuffer,
    rhs: GpuBuffer,
    pressure: GpuBuffer,
}

impl Rig {
    fn new(n: usize) -> Self {
        let device = crate::test_device();
        let cells = (n * n * n * 4) as u64;
        let records = open_face_records(n);
        let faces = device.create_buffer_shared(records.len() as u64 * 32);
        // SAFETY: a shared buffer sized for the records; no GPU work is queued.
        unsafe { faces.write(0, bytemuck::cast_slice(&records)) };
        Self {
            n,
            water: device.create_buffer_shared(cells),
            rhs: device.create_buffer_shared(cells),
            pressure: device.create_buffer_shared(cells),
            faces,
            device,
            solver: PressureSolver::default(),
        }
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
        let lattice = Water { lattice: [n; 3], cell_size: self.cell_size() as f32, water: &self.water, faces: &self.faces };
        self.solver.prepare(&self.device, &mut enc, &lattice).expect("prepares");
        self.solver.solve(&mut enc, &lattice, &self.rhs, &self.pressure, iterations).expect("solves");
        enc.commit_and_wait_profiled(&self.device)
    }

    fn residual_of(&self, p: &Problem) -> f64 {
        let cells = self.n * self.n * self.n;
        let ptr = self.pressure.mapped_ptr().expect("shared pressure");
        // SAFETY: the solve completed; the buffer holds `cells` floats.
        let pressure = unsafe { std::slice::from_raw_parts(ptr.cast::<f32>().cast_const(), cells) };
        residual(pressure, &p.water, &p.f, self.n, self.cell_size())
    }

    fn solve(&mut self, p: &Problem, iterations: u32) -> f64 {
        self.run(p, iterations, false);
        self.residual_of(p)
    }
}

/// Solve every problem at `m` cells a side. At 3 iterations the GPU runs the
/// reference's algorithm step for step, so its residual is within 10% of the
/// f64 one. At 8 it is within 2× the reference, or of what f32 reaches at
/// all: the larger of F32_FLOOR and the residual after 16 iterations. Pins
/// are (frame, residual after 3, after 8) from
/// `scripts/mgpcg_reference.py FIXTURE [--refine 2 | --side M] --iterations 3,8,16`.
fn check(fixture: &str, m: usize, pinned: &[(u32, f64, f64)]) {
    let (n, problems) = load_fixture(fixture);
    assert_eq!(problems.len(), pinned.len(), "{fixture}: one pin per problem");
    let mut rig = Rig::new(m);
    let mut failures = Vec::new();
    for (problem, &(frame, at3, at8)) in problems.iter().zip(pinned) {
        assert_eq!(problem.frame, frame);
        let problem = resample(problem, n, m);
        let got3 = rig.solve(&problem, 3);
        let floor = rig.solve(&problem, 16).max(F32_FLOOR);
        let got8 = rig.solve(&problem, 8);
        let again = rig.solve(&problem, 8);
        println!(
            "pressure module {fixture} {m}³ frame {frame:3}: 3 iterations {got3:.3e} (f64 {at3:.3e}, {:.3}×); 8 iterations {got8:.3e} (f64 {at8:.3e}, f32 floor {floor:.3e})",
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
        let (prepare, solve) = passes([m as u32; 3], 8);
        assert_eq!(profile.spans.len(), prepare + solve, "{m}³ ({} levels)", level_lattices([m as u32; 3]).len());
        assert!(profile.spans.iter().all(|s| s.label.starts_with("gpu_flip.pressure.")), "{m}³: an unlabelled pass");
        let mut times: Vec<f64> = (0..5).map(|_| rig.run(&problem, 8, false).total_ms).collect();
        times.sort_by(f64::total_cmp);
        println!("pressure module {m}³: {prepare} + {solve} passes, {:.2} ms GPU per prepare and 8-iteration solve (median of 5)", times[2]);
    }
}
