//! The pressure solver module against the f64 reference
//! (`scripts/mgpcg_reference.py`, the default rule: halve rounding up to 4
//! or less, then the exact inverse) on the saved Dam Break and deep pool
//! problems at 64³ and refined to 128³, and resampled to the odd and uneven
//! sides 25, 37 and 40. The residual is the true relative residual of the
//! masked Poisson equation.

use manifold_gpu::{GpuBuffer, GpuReplayCache};

use super::gpu_flip_pressure::{MAX_ITERATIONS, PROGRESS_FLOATS, PressureSolver, Stop, Water, level_lattices, passes};

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
    /// The free surface's distance, when the rig solves the ghost rows.
    phi: Option<GpuBuffer>,
}

impl Rig {
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
        self.solver.solve(&mut enc, &lattice, &self.rhs, &self.pressure, Stop::Fixed(iterations), None).expect("solves");
        enc.commit_and_wait_profiled(&self.device)
    }

    fn residual_of(&self, p: &Problem) -> f64 {
        residual(self.pressure(), &p.water, &p.f, self.n, self.cell_size())
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
    rig.solver.solve(&mut enc, &lattice, &rig.rhs, &rig.pressure, Stop::Fixed(iterations), None).expect("solves");
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
    rig.solver.solve(&mut enc, &lattice, &rig.rhs, &rig.pressure, Stop::Converged(cap), None).expect("solves");
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
    rig.solver.solve(&mut enc, &lattice, &rig.rhs, &rig.pressure, stop, None).expect("solves");
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
