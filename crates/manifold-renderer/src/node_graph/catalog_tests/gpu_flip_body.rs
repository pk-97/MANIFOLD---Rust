//! Checked against FLIP Fluids rigidfluidcoupling.cpp (MIT); see THIRD_PARTY_NOTICES.md.
//! GPU value proofs for the passes that put dynamic bodies inside the GPU
//! FLIP pressure solve (docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (solids in
//! the water)) against CPU f64 references: each body's impulse, the bodies'
//! share of the operator, the velocity change on the solid faces and the
//! reaction. The lattice spans twelve tiles, partial edge tiles among them,
//! so the slot order of the sum is exercised. Then the passes over the
//! solver's active tiles against every tile, bitwise and under NaN poison,
//! on their own and through a step with a Box3D body
//! (docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md D-9).

use manifold_gpu::{GpuBuffer, GpuDevice, GpuReplayCache};
use manifold_physics::coupled_motion::{Held, Mobility, SupportPoint, constrained_mobility, mobility_index};

use manifold_node_engine::testkit::atom::{FACE_FLOATS, assert_close, face_grid_len, random_values, random_water};
use manifold_node_engine::water::primitives::gpu_flip_bodies::{BodyPasses, Bodies};
use manifold_node_engine::water::primitives::gpu_flip_pressure::{MAX_ITERATIONS, PROGRESS_FLOATS, PressureSolver, Solve, Stop, Water};
use manifold_node_engine::water::primitives::gpu_flip_step::{TILE, set_all_tiles, set_gate_off, set_poison};
use manifold_node_engine::testkit::liquid_surface::read;
use manifold_node_engine::water::liquid::bodies::LiquidBody;
use manifold_node_engine::water::liquid::coupling::coupled_start;
use manifold_node_engine::water::primitives::liquid_stats::SOLVER_WORDS;

/// Tail index of stats word 16 (active-tile share): the tail starts at stats word 10 (liquid_stats.rs `SOLVER_WORDS`).
const TILE_SHARE: usize = 16 - 10;

const N: [usize; 3] = [17, 16, 15];
const H: f32 = 0.25;
const MIN: [f32; 3] = [-2.1, -1.9, -1.8];
const TICK: f32 = 0.05;
const DENSITY: f32 = 1000.0;
/// Rows 1 and 2 are the bodies (rows 3, body_count 2).
const ROWS: usize = 3;
const BODIES: usize = 2;
const FIRST: usize = ROWS - BODIES;
const SUM_FLOATS: usize = 16;

fn m() -> [usize; 3] {
    N.map(|v| v + 1)
}

fn coords(i: usize, n: [usize; 3]) -> [usize; 3] {
    [i % n[0], (i / n[0]) % n[1], i / (n[0] * n[1])]
}

fn at(p: [usize; 3], n: [usize; 3]) -> usize {
    p[0] + n[0] * (p[1] + n[1] * p[2])
}

fn inner(p: [usize; 3], a: usize) -> bool {
    (0..3).all(|b| b == a || p[b] < N[b]) && p[a] > 0 && p[a] < N[a]
}

fn owner(code: f32, a: usize) -> Option<usize> {
    let b = ((code as u32) >> (8 * a)) & 255;
    (b > 0).then(|| b as usize - 1)
}

/// Open fractions on inner faces (closed, open or cut) and each record's
/// cell open volume; velocities, frictions and owner codes for the solids:
/// owners only on faces a solid closes or cuts, about one in three empty.
fn fixture(seed: u64) -> (Vec<f32>, Vec<f32>) {
    let records = m().iter().product::<usize>();
    let draw = random_values(records * 16, seed);
    let mut open = vec![0.0; records * FACE_FLOATS];
    let mut solid = vec![0.0; records * FACE_FLOATS];
    for i in 0..records {
        let p = coords(i, m());
        let r = |k: usize| draw[i * 16 + k] + 0.5;
        open[i * FACE_FLOATS + 7] = 0.3 + 0.7 * r(0);
        let mut code = 0u32;
        for a in 0..3 {
            if !inner(p, a) {
                continue;
            }
            let w = r(1 + a);
            let fraction = if w < 0.2 { 0.0 } else if w < 0.45 { 1.0 } else { 0.05 + 0.9 * (w - 0.45) / 0.55 };
            open[i * FACE_FLOATS + 4 + a] = fraction;
            solid[i * FACE_FLOATS + a] = 2.0 * (r(4 + a) - 0.5);
            solid[i * FACE_FLOATS + 4 + a] = r(7 + a);
            let pick = r(10 + a);
            if fraction < 1.0 && pick > 0.33 {
                code += (u32::from(pick > 0.66) + 1) << (8 * a);
            }
        }
        solid[i * FACE_FLOATS + 3] = code as f32;
    }
    (open, solid)
}

/// A junk row, then a dynamic turning body and a prescribed one.
fn bodies() -> Vec<LiquidBody> {
    let mut rows = vec![LiquidBody { position_inv_mass: [9.0, 9.0, 9.0, 3.0], ..LiquidBody::default() }; ROWS];
    rows[1] = LiquidBody {
        position_inv_mass: [-0.05, 0.42, -0.02, 0.4],
        linear_velocity: [0.3, -0.6, 0.1, 0.5],
        inv_inertia_x: [2.0, 0.1, 0.0, 0.0],
        inv_inertia_y: [0.1, 1.5, -0.2, 0.0],
        inv_inertia_z: [0.0, -0.2, 2.5, 0.0],
        accel_shape: [0.0, -9.8, 0.0, 0.0],
        ..LiquidBody::default()
    };
    rows[2] = LiquidBody {
        position_inv_mass: [0.31, 0.6, 0.05, 0.0],
        linear_velocity: [-0.4, 0.2, 0.0, 0.8],
        accel_shape: [0.0, 0.0, 0.0, 1.0],
        ..LiquidBody::default()
    };
    rows
}

fn dynamic(row: &LiquidBody) -> bool {
    row.position_inv_mass[3] > 0.0 && row.accel_shape[3] >= 0.0
}

/// The face centre of axis a's face on record p.
fn face_centre(p: [usize; 3], a: usize) -> [f64; 3] {
    std::array::from_fn(|b| f64::from(MIN[b]) + (p[b] as f64 + if b == a { 0.0 } else { 0.5 }) * f64::from(H))
}

/// A row's centre of mass: the passes read rows the step has already posed
/// (gpu_flip_step.wgsl pose_bodies).
fn posed(row: &LiquidBody) -> [f64; 3] {
    std::array::from_fn(|b| f64::from(row.position_inv_mass[b]))
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

// ── CPU references ─────────────────────────────────────────────────────────

/// The pressure's impulse through every owned inner face:
/// ρh²·((c_lo − w)·x_lo − (c_hi − w)·x_hi), x counted in water cells only.
fn cpu_pressure_impulse(x: &[f32], water: &[f32], open: &[f32], solid: &[f32]) -> Vec<f64> {
    let mut out = vec![0.0; face_grid_len(N)];
    let rho_h2 = f64::from(DENSITY) * f64::from(H) * f64::from(H);
    let x_of = |q: [usize; 3]| {
        let c = at(q, N);
        if water[c] > 0.5 { f64::from(x[c]) } else { 0.0 }
    };
    for i in 0..m().iter().product::<usize>() {
        let p = coords(i, m());
        for a in 0..3 {
            if !inner(p, a) || owner(solid[i * FACE_FLOATS + 3], a).is_none() {
                continue;
            }
            let mut lo = p;
            lo[a] -= 1;
            let w = f64::from(open[i * FACE_FLOATS + 4 + a]);
            let c_hi = f64::from(open[i * FACE_FLOATS + 7]);
            let c_lo = f64::from(open[at(lo, m()) * FACE_FLOATS + 7]);
            out[i * FACE_FLOATS + a] = rho_h2 * ((c_lo - w) * x_of(lo) - (c_hi - w) * x_of(p));
        }
    }
    out
}

/// Each body's sums record from per-face impulses: linear and angular
/// impulse, and its mobility times them for a dynamic body; 0 for any other.
/// Also the sum of |term| per float, the scale an f32 reduction's rounding
/// grows with.
fn cpu_body_sums(impulses: &[f64], solid: &[f32], rows: &[LiquidBody], mobility: &[Mobility]) -> (Vec<f64>, Vec<f64>) {
    let mut out = vec![0.0; BODIES * SUM_FLOATS];
    let mut scale = vec![0.0; BODIES * SUM_FLOATS];
    for b in 0..BODIES {
        let row = &rows[FIRST + b];
        if !dynamic(row) {
            continue;
        }
        let c = posed(row);
        let (mut linear, mut angular) = ([0.0; 3], [0.0; 3]);
        let s = &mut scale[SUM_FLOATS * b..SUM_FLOATS * (b + 1)];
        for i in 0..m().iter().product::<usize>() {
            let p = coords(i, m());
            for a in 0..3 {
                if owner(solid[i * FACE_FLOATS + 3], a) != Some(b) || !inner(p, a) {
                    continue;
                }
                let push = impulses[i * FACE_FLOATS + a];
                let x = face_centre(p, a);
                let mut axis = [0.0; 3];
                axis[a] = 1.0;
                linear[a] += push;
                s[a] += push.abs();
                let turn = cross(std::array::from_fn(|k| x[k] - c[k]), axis);
                for k in 0..3 {
                    angular[k] += push * turn[k];
                    s[4 + k] += (push * turn[k]).abs();
                }
            }
        }
        let record = &mut out[SUM_FLOATS * b..SUM_FLOATS * (b + 1)];
        record[..3].copy_from_slice(&linear);
        record[4..7].copy_from_slice(&angular);
        let m = &mobility[FIRST + b];
        let pushed = [linear[0], linear[1], linear[2], angular[0], angular[1], angular[2]];
        let size = [s[0], s[1], s[2], s[4], s[5], s[6]];
        for i in 0..6 {
            let at = 8 + i + i / 3;
            record[at] = (0..6).map(|j| f64::from(m[mobility_index(i, j)]) * pushed[j]).sum();
            s[at] = (0..6).map(|j| f64::from(m[mobility_index(i, j)]).abs() * size[j]).sum();
        }
    }
    (out, scale)
}

/// The face velocity along a that body b's velocity change gives at face a
/// of record f.
fn change_along(sums: &[f32], rows: &[LiquidBody], b: usize, f: [usize; 3], a: usize) -> f64 {
    let centre = posed(&rows[FIRST + b]);
    let x = face_centre(f, a);
    let r: [f64; 3] = std::array::from_fn(|k| x[k] - centre[k]);
    let dv: [f64; 3] = std::array::from_fn(|k| f64::from(sums[SUM_FLOATS * b + 8 + k]));
    let dw: [f64; 3] = std::array::from_fn(|k| f64::from(sums[SUM_FLOATS * b + 12 + k]));
    dv[a] + cross(dw, r)[a]
}

/// `base` plus (1/h)·Σ sign·(c − w)·(dv + dω × r)[a] over each water cell's
/// owned inner faces.
fn cpu_body_product(base: &[f32], water: &[f32], open: &[f32], solid: &[f32], sums: &[f32], rows: &[LiquidBody]) -> Vec<f64> {
    (0..N.iter().product::<usize>())
        .map(|c| {
            if water[c] <= 0.5 {
                return f64::from(base[c]);
            }
            let p = coords(c, N);
            let volume = f64::from(open[at(p, m()) * FACE_FLOATS + 7]);
            let mut total = 0.0;
            for a in 0..3 {
                for side in 0..2 {
                    let mut f = p;
                    f[a] += side;
                    if f[a] == 0 || f[a] == N[a] {
                        continue;
                    }
                    let i = at(f, m());
                    let Some(b) = owner(solid[i * FACE_FLOATS + 3], a) else { continue };
                    let sign = if side == 1 { 1.0 } else { -1.0 };
                    let w = f64::from(open[i * FACE_FLOATS + 4 + a]);
                    total += sign * (volume - w) * change_along(sums, rows, b, f, a);
                }
            }
            f64::from(base[c]) + total / f64::from(H)
        })
        .collect()
}

/// The solid face velocity after every owned inner face gains its owner's
/// velocity change.
fn cpu_velocity_change(solid: &[f32], sums: &[f32], rows: &[LiquidBody]) -> Vec<f64> {
    let mut out: Vec<f64> = solid.iter().map(|&v| f64::from(v)).collect();
    for i in 0..m().iter().product::<usize>() {
        let p = coords(i, m());
        for a in 0..3 {
            if let (true, Some(b)) = (inner(p, a), owner(solid[i * FACE_FLOATS + 3], a)) {
                out[i * FACE_FLOATS + a] += change_along(sums, rows, b, p, a);
            }
        }
    }
    out
}

// ── Value proofs ───────────────────────────────────────────────────────────

/// Packed mobilities as the GPU holds them, six vec4 each.
fn gpu_mobility(mobility: &[Mobility]) -> Vec<[f32; 24]> {
    mobility
        .iter()
        .map(|m| std::array::from_fn(|k| m.get(k).copied().unwrap_or(0.0)))
        .collect()
}

fn shared<T: bytemuck::Pod>(device: &GpuDevice, values: &[T]) -> GpuBuffer {
    let buffer = device.create_buffer_shared((size_of_val(values) as u64).max(16));
    buffer.zero_fill();
    // SAFETY: shared buffer sized for `values`; no GPU work in flight.
    unsafe { buffer.write(0, bytemuck::cast_slice(values)) };
    buffer
}

fn assert_sums(got: &[f32], want: &[f64], scale: &[f64], what: &str) {
    for (k, ((g, w), s)) in got.iter().zip(want).zip(scale).enumerate() {
        assert!((f64::from(*g) - w).abs() <= 1e-5 * (s + 1.0), "{what}[{k}]: {g} vs {w} (scale {s})");
    }
}

struct Scene {
    device: manifold_gpu::testkit::TestDevice,
    open: Vec<f32>,
    solid: Vec<f32>,
    water: Vec<f32>,
    rows: Vec<LiquidBody>,
    /// Each row's packed mobility, as pose_bodies writes it: free unless
    /// [`Self::hold`] held the dynamic body.
    mobility: Vec<Mobility>,
    buffers: [GpuBuffer; 5],
    passes: BodyPasses,
    /// Prepared on the scene's water for its fine tile set; `all` means
    /// every tile was classified active.
    solver: PressureSolver,
}

impl Scene {
    /// Random water everywhere, every tile active: the CPU references sum
    /// every face.
    fn new(seed: u64) -> Self {
        Self::with_water(seed, random_water(N.iter().product(), seed + 1), true)
    }

    fn with_water(seed: u64, water: Vec<f32>, all: bool) -> Self {
        let device = manifold_gpu::testkit::test_device();
        let (open, solid) = fixture(seed);
        let rows = bodies();
        let mobility: Vec<Mobility> = rows.iter().map(|row| constrained_mobility(&coupled_start(row), &[], Held::default())).collect();
        let buffers = [
            shared(&device, &water),
            shared(&device, &open),
            shared(&device, &solid),
            shared(&device, &rows),
            shared(&device, &gpu_mobility(&mobility)),
        ];
        let mut passes = BodyPasses::default();
        passes.prepare_pipelines(&device);
        passes.prepare(&device, N.map(|v| v as u32), BODIES as u32).expect("body passes");
        let mut solver = PressureSolver::default();
        solver.prepare_pipelines(&device);
        let mut enc = device.create_encoder("body scene tiles");
        set_all_tiles(all);
        let lattice = Water { lattice: N.map(|v| v as u32), cell_size: H, water: &buffers[0], faces: &buffers[1], phi: None };
        solver.prepare(&device, &mut enc, &lattice).expect("the solver prepares its tile lists");
        set_all_tiles(false);
        enc.commit_and_wait_completed();
        Self { device, open, solid, water, rows, mobility, buffers, passes, solver }
    }

    /// The dynamic body held on a floor under it: its bottom corners closed
    /// and their patch unturned: it can only slide along the floor.
    fn hold(&mut self) {
        let corners = [[0.2, -0.2, 0.2], [-0.2, -0.2, 0.2], [-0.2, -0.2, -0.2], [0.2, -0.2, -0.2]];
        let supports: Vec<SupportPoint> = corners
            .iter()
            .map(|&lever| SupportPoint { lever, normal: [0.0, 1.0, 0.0], friction: 0.5, patch_lever: [0.0, -0.2, 0.0], ..SupportPoint::default() })
            .collect();
        let held = Held { closed: 0b1111, stuck: 0, unturned: 0b1111 };
        self.mobility[FIRST] = constrained_mobility(&coupled_start(&self.rows[FIRST]), &supports, held);
        self.buffers[4] = shared(&self.device, &gpu_mobility(&self.mobility));
    }

    fn tiles(&self) -> [&GpuBuffer; 3] {
        self.solver.tiles().expect("prepared")
    }

    fn bodies(&self) -> Bodies<'_> {
        Bodies {
            lattice: N.map(|v| v as u32),
            lattice_min: MIN,
            cell_size: H,
            density: DENSITY,
            tick_seconds: TICK,
            first: FIRST as u32,
            count: BODIES as u32,
            water: &self.buffers[0],
            open: &self.buffers[1],
            solid: &self.buffers[2],
            bodies: &self.buffers[3],
            mobility: &self.buffers[4],
        }
    }

    fn sums(&self) -> Vec<f32> {
        read(self.passes.sums().expect("sums"), BODIES * SUM_FLOATS)
    }
}

/// Inside a solver iteration: the bodies' pressure impulse of the search
/// direction, then their share of the operator added to s.
#[test]
fn gpu_flip_body_operator_matches_cpu() {
    let scene = Scene::new(0xb0d1);
    let cells: usize = N.iter().product();
    let direction = random_values(cells, 0xb0d2);
    let base = random_values(cells, 0xb0d3);
    let (direction_gpu, s) = (shared(&scene.device, &direction), shared(&scene.device, &base));
    let mut enc = scene.device.create_encoder("body operator");
    scene.passes.apply(&mut enc, &scene.bodies(), scene.tiles(), &direction_gpu, &s).expect("apply");
    enc.commit_and_wait_completed();

    let impulses = cpu_pressure_impulse(&direction, &scene.water, &scene.open, &scene.solid);
    let (want, scale) = cpu_body_sums(&impulses, &scene.solid, &scene.rows, &scene.mobility);
    assert!(want[..3].iter().any(|&v| v.abs() > 1.0), "the dynamic body owns pushed faces");
    assert!(want[SUM_FLOATS..].iter().all(|&v| v == 0.0), "the prescribed body takes no impulse");
    let sums = scene.sums();
    assert_sums(&sums, &want, &scale, "pressure sums");

    let product = cpu_body_product(&base, &scene.water, &scene.open, &scene.solid, &sums, &scene.rows);
    let moved = product.iter().zip(&base).filter(|(w, b)| (**w - f64::from(**b)).abs() > 1e-3).count();
    assert!(moved > 50, "water cells beside the dynamic body's faces change: {moved}");
    assert_close(&read(&s, cells), &product, "body operator");
}

/// After the projection: the pressure's impulse into the reaction and its
/// velocity change into the solid faces.
#[test]
fn gpu_flip_body_reaction_matches_cpu() {
    let scene = Scene::new(0x7ea1);
    let cells: usize = N.iter().product();
    let pressure = random_values(cells, 0x7ea2);
    let base = random_values(BODIES * 8, 0x7ea4);
    let pressure_gpu = shared(&scene.device, &pressure);
    let reaction = shared(&scene.device, &base);
    let scratch = shared(&scene.device, &vec![0.0_f32; cells]);

    // The same impulse react starts with, on its own, to read its sums.
    let mut enc = scene.device.create_encoder("pressure sums");
    scene.passes.apply(&mut enc, &scene.bodies(), scene.tiles(), &pressure_gpu, &scratch).expect("apply");
    enc.commit_and_wait_completed();
    let pushed = scene.sums();
    let (want, scale) =
        cpu_body_sums(&cpu_pressure_impulse(&pressure, &scene.water, &scene.open, &scene.solid), &scene.solid, &scene.rows, &scene.mobility);
    assert_sums(&pushed, &want, &scale, "pressure sums");

    let mut enc = scene.device.create_encoder("react");
    scene.passes.react(&mut enc, &scene.bodies(), scene.tiles(), &pressure_gpu, &reaction).expect("react");
    enc.commit_and_wait_completed();

    let changed: Vec<f32> = read(&scene.buffers[2], face_grid_len(N));
    let want_changed = cpu_velocity_change(&scene.solid, &pushed, &scene.rows);
    let touched = want_changed.iter().zip(&scene.solid).filter(|(w, s)| (**w - f64::from(**s)).abs() > 1e-4).count();
    assert!(touched > 50, "the dynamic body's faces gain its velocity change: {touched}");
    assert_close(&changed, &want_changed, "velocity change");

    // The pressure is the only reaction; the water's drag on the body never
    // reaches it (the engine's rigidfluidcoupling.cpp keeps none).
    let got: Vec<f32> = read(&reaction, BODIES * 8);
    let want: Vec<f64> = (0..BODIES * 8)
        .map(|k| {
            let (b, j) = (k / 8, k % 8);
            f64::from(base[k]) + f64::from(pushed[SUM_FLOATS * b + j])
        })
        .collect();
    assert_close(&got, &want, "reaction");
}

/// D16: a body held on its supports answers the pressure through its held
/// mobility: it neither sinks nor tips. The bodies' share of the operator, B,
/// stays symmetric and never negative over the water cells:
/// yᵀ·B·x = xᵀ·B·y and xᵀ·B·x ≥ 0, so the conjugate gradient solve keeps
/// its footing.
#[test]
fn gpu_flip_body_operator_holds_a_supported_body_symmetric() {
    let mut scene = Scene::new(0x5e1d);
    scene.hold();
    let cells: usize = N.iter().product();
    let zeros = vec![0.0_f32; cells];
    let product = |x: &[f32]| -> Vec<f32> {
        let (x_gpu, s) = (shared(&scene.device, x), shared(&scene.device, &zeros));
        let mut enc = scene.device.create_encoder("held body operator");
        scene.passes.apply(&mut enc, &scene.bodies(), scene.tiles(), &x_gpu, &s).expect("apply");
        enc.commit_and_wait_completed();
        read(&s, cells)
    };
    let (x, y) = (random_values(cells, 0x5e1e), random_values(cells, 0x5e1f));
    let bx = product(&x);
    let sums = scene.sums();
    let (want, scale) =
        cpu_body_sums(&cpu_pressure_impulse(&x, &scene.water, &scene.open, &scene.solid), &scene.solid, &scene.rows, &scene.mobility);
    assert_sums(&sums, &want, &scale, "held sums");
    assert!(want[..3].iter().any(|&v| v.abs() > 1.0), "the held body owns pushed faces");
    for (k, what) in [(9, "sinks"), (12, "tips about x"), (14, "tips about z")] {
        assert!(want[k].abs() <= 1e-5 * (scale[k] + 1.0), "the held body {what}: {}", want[k]);
    }
    assert_close(&bx, &cpu_body_product(&zeros, &scene.water, &scene.open, &scene.solid, &sums, &scene.rows), "held body operator");

    let by = product(&y);
    let in_water: Vec<usize> = (0..cells).filter(|&k| scene.water[k] > 0.5).collect();
    let dot = |a: &[f32], b: &[f32]| in_water.iter().map(|&k| f64::from(a[k]) * f64::from(b[k])).sum::<f64>();
    let size = in_water.iter().map(|&k| (f64::from(y[k]) * f64::from(bx[k])).abs()).sum::<f64>();
    let (ybx, xby, xbx) = (dot(&y, &bx), dot(&x, &by), dot(&x, &bx));
    assert!(size > 1.0, "the operator acts: {size}");
    assert!((ybx - xby).abs() <= 1e-4 * size, "yᵀBx {ybx} against xᵀBy {xby} (scale {size})");
    assert!(xbx >= -1e-4 * size, "xᵀBx {xbx} is negative (scale {size})");
}

/// The coupled fine round is one replay segment, including the body product.
/// Stopped and inactive rounds preserve the direct dispatch results exactly.
#[test]
fn gpu_flip_coupled_round_replay_matches_direct() {
    const ROUNDS: u32 = 3;

    struct Run {
        scene: Scene,
        solver: PressureSolver,
        rhs: GpuBuffer,
        pressure: GpuBuffer,
        progress: GpuBuffer,
        plan: GpuBuffer,
        cache: Option<GpuReplayCache>,
    }

    impl Run {
        fn new(replay: bool) -> Self {
            let mut scene = Scene::with_water(0xc09e, random_water(N.iter().product(), 0xc09f), false);
            let cells = N.iter().product::<usize>();
            let rhs = shared(&scene.device, &vec![0.0_f32; cells]);
            let pressure = shared(&scene.device, &vec![0.0_f32; cells]);
            let progress = shared(&scene.device, &vec![0_u32; PROGRESS_FLOATS as usize]);
            let plan = shared(&scene.device, &[0_u32; 12]);
            scene.passes.set_clock_plan(&plan);
            let mut solver = PressureSolver::default();
            solver.prepare_pipelines(&scene.device);
            solver.set_clock_plan(&plan);
            Self { scene, solver, rhs, pressure, progress, plan, cache: replay.then(GpuReplayCache::default) }
        }

        fn solve(&mut self, rhs: &[f32], stop: Stop, active: bool) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
            let mut plan = [0_u32; 12];
            plan[0] = if active { TICK.to_bits() } else { 0 };
            plan[11] = 1;
            // SAFETY: shared buffers sized for these values; the previous solve completed.
            unsafe {
                self.rhs.write(0, bytemuck::cast_slice(rhs));
                self.plan.write(0, bytemuck::cast_slice(&plan));
            }
            let mut enc = self.scene.device.create_encoder("coupled pressure replay");
            let replay = self.cache.take().map(|cache| enc.begin_replay(&self.scene.device, cache)).is_some();
            let water = Water {
                lattice: N.map(|v| v as u32), cell_size: H,
                water: &self.scene.buffers[0], faces: &self.scene.buffers[1], phi: None,
            };
            self.solver.prepare(&self.scene.device, &mut enc, &water).expect("prepare coupled solve");
            self.solver.solve(&mut enc, &water, Solve {
                rhs: &self.rhs, pressure: &self.pressure, stop,
                bodies: Some((&self.scene.passes, &self.scene.bodies())), level: 0, coarse_rhs: None,
            }).expect("coupled solve");
            if replay {
                self.cache = Some(enc.end_replay());
            }
            enc.copy_buffer_to_buffer(self.solver.progress().expect("prepared"), &self.progress, self.progress.size);
            enc.commit_and_wait_completed();
            (
                read(&self.pressure, N.iter().product()),
                bits(&self.scene.sums()),
                read(&self.progress, PROGRESS_FLOATS as usize),
            )
        }
    }

    let rhs = random_values(N.iter().product(), 0xc0a0);
    let zero = vec![0.0_f32; rhs.len()];
    let mut direct = Run::new(false);
    let mut replay = Run::new(true);
    let mut last = GpuReplayCache::default().stats();
    let mut stopped = None;
    let mut last_template = replay.solver.template_stats();
    for visit in 0..7 {
        let (input, stop, active) = match visit {
            4 => (&zero, Stop::Converged(ROUNDS), true),
            5 => (&rhs, Stop::Fixed(ROUNDS), false),
            _ => (&rhs, Stop::Fixed(ROUNDS), true),
        };
        let want = direct.solve(input, stop, active);
        let got = replay.solve(input, stop, active);
        assert_eq!(got, want, "visit {visit}: pressure, body sums and progress match direct bits");
        assert!(got.0.iter().chain(&got.1).all(|&v| f32::from_bits(v).is_finite()), "visit {visit}: finite outputs");
        if visit == 4 {
            assert!(got.0.iter().all(|&v| v == 0), "zero RHS leaves zero pressure");
            assert_eq!(got.2[0], 0);
            assert_eq!(got.2[1], 0, "zero RHS stops before the first iteration");
            assert_eq!(got.2[2], 1.0_f32.to_bits());
            stopped = Some(got.clone());
        } else if visit == 5 {
            assert_eq!(Some(&got), stopped.as_ref(), "inactive slot preserves pressure, body sums and progress");
        } else {
            assert_eq!(got.2[1], (ROUNDS as f32).to_bits(), "visit {visit}: all fixed rounds ran");
            assert_eq!(got.2[2], 0, "fixed solve has no convergence stop");
            assert!(got.0.iter().any(|&v| f32::from_bits(v).abs() > 1e-6), "visit {visit}: nonzero pressure");
            assert!(got.1[..3].iter().any(|&v| f32::from_bits(v).abs() > 1e-6), "visit {visit}: the dynamic body participates");
        }
        let stats = replay.cache.as_ref().expect("replay cache returned").stats();
        let template = replay.solver.template_stats();
        // The first visit records its entry; the second grows the recording.
        if visit >= 2 {
            assert_eq!(stats.recorded, last.recorded, "visit {visit}: warm recordings unchanged");
            assert_eq!(stats.store_allocations, last.store_allocations, "visit {visit}: warm storage unchanged");
            assert_eq!(stats.segments_direct, last.segments_direct, "visit {visit}: no direct round dispatches");
            assert_eq!(template.executes - last_template.executes, manifold_gpu::template_chunks(ROUNDS, 32).count() as u64, "visit {visit}: one execute per chunk of rounds, including stopped/inactive rounds");
            assert_eq!(template.walks - last_template.walks, 1, "visit {visit}: one walked round");
        }
        last = stats;
        last_template = template;
    }
}

// ── Sparse against every tile ──────────────────────────────────────────────

fn tile_dims() -> [usize; 3] {
    N.map(|v| v.div_ceil(TILE as usize))
}

fn tile_of(cell: usize) -> usize {
    let t = coords(cell, N).map(|v| v / TILE as usize);
    at(t, tile_dims())
}

/// The fine tiles the solver classifies active on `water`: a water cell
/// lies in the tile's box grown by one cell (gpu_flip_pressure.wgsl
/// classify_main).
fn active_tiles(water: &[f32]) -> Vec<bool> {
    let dims = tile_dims();
    (0..dims.iter().product::<usize>())
        .map(|t| {
            let origin = coords(t, dims).map(|v| v * TILE as usize);
            let first = origin.map(|v| v.saturating_sub(1));
            let last: [usize; 3] = std::array::from_fn(|a| (origin[a] + TILE as usize).min(N[a] - 1));
            (first[2]..=last[2]).any(|z| {
                (first[1]..=last[1]).any(|y| (first[0]..=last[0]).any(|x| water[at([x, y, z], N)] > 0.5))
            })
        })
        .collect()
}

fn bits(values: &[f32]) -> Vec<u32> {
    values.iter().map(|v| v.to_bits()).collect()
}

/// What one run of the passes leaves: the pressure sums, s after the
/// operator, the solid velocity after the reaction, the friction sums and
/// the reaction, as bits.
struct Left {
    pushed: Vec<u32>,
    s: Vec<u32>,
    solid: Vec<u32>,
    reacted: Vec<u32>,
    reaction: Vec<u32>,
}

fn first_differing(a: &[u32], b: &[u32]) -> Option<usize> {
    assert_eq!(a.len(), b.len());
    (0..a.len()).find(|&i| a[i] != b[i])
}

/// The passes over the solver's active tiles equal the passes over every
/// tile bit for bit: the sums, s, the solid velocity and the reaction; and
/// the same with NaN in x and s outside the active tiles and in every
/// partial slot before the run, so nothing the passes read lies outside the
/// lists. The water fills the first tile column of three, so most tiles are
/// inactive while the bodies own faces throughout the lattice.
#[test]
fn gpu_flip_body_passes_sparse_match_all_tiles() {
    let seed = 0x5a5e;
    let cells: usize = N.iter().product();
    let water: Vec<f32> =
        random_water(cells, seed + 1).iter().enumerate().map(|(i, &w)| if coords(i, N)[0] < 6 { w } else { 0.0 }).collect();
    let active = active_tiles(&water);
    let inactive = active.iter().filter(|a| !**a).count();
    assert!(inactive >= 6, "{inactive} of {} tiles inactive", active.len());
    let x = random_values(cells, seed + 2);
    let base = random_values(cells, seed + 3);
    let reaction_base = random_values(BODIES * 8, seed + 5);
    let run = |all: bool, poison: bool| -> Left {
        let scene = Scene::with_water(seed, water.clone(), all);
        let outside = |v: &[f32]| -> Vec<f32> {
            v.iter().enumerate().map(|(i, &v)| if poison && !active[tile_of(i)] { f32::NAN } else { v }).collect()
        };
        let x_gpu = shared(&scene.device, &outside(&x));
        let s = shared(&scene.device, &outside(&base));
        let reaction = shared(&scene.device, &reaction_base);
        let mut enc = scene.device.create_encoder("sparse apply");
        if poison {
            scene.passes.poison(&mut enc, &scene.bodies());
        }
        scene.passes.apply(&mut enc, &scene.bodies(), scene.tiles(), &x_gpu, &s).expect("apply");
        enc.commit_and_wait_completed();
        let pushed = bits(&scene.sums());
        let mut enc = scene.device.create_encoder("sparse react");
        if poison {
            scene.passes.poison(&mut enc, &scene.bodies());
        }
        scene.passes.react(&mut enc, &scene.bodies(), scene.tiles(), &x_gpu, &reaction).expect("react");
        enc.commit_and_wait_completed();
        let s: Vec<f32> = read(&s, cells);
        // Only the active tiles' s is compared under poison: the rest holds
        // the NaN written in, which nothing touched.
        let s = s.iter().enumerate().map(|(i, v)| if active[tile_of(i)] { v.to_bits() } else { base[i].to_bits() }).collect();
        Left {
            pushed,
            s,
            solid: bits(&read(&scene.buffers[2], face_grid_len(N))),
            reacted: bits(&scene.sums()),
            reaction: bits(&read(&reaction, BODIES * 8)),
        }
    };
    let dense = run(true, false);
    assert!(dense.pushed[..3].iter().any(|&b| f32::from_bits(b).abs() > 1.0), "the dynamic body is pushed");
    for (name, left) in [("sparse", run(false, false)), ("poisoned", run(false, true))] {
        for (what, a, b) in [
            ("pressure sums", &dense.pushed, &left.pushed),
            ("s", &dense.s, &left.s),
            ("solid velocity", &dense.solid, &left.solid),
            ("react sums", &dense.reacted, &left.reacted),
            ("reaction", &dense.reaction, &left.reaction),
        ] {
            if let Some(i) = first_differing(a, b) {
                panic!("{name} {what} differs from all tiles first at {i}: {} vs {}", f32::from_bits(a[i]), f32::from_bits(b[i]));
            }
        }
    }
}

/// A GPU FLIP conformance box scene (a Box3D body in the liquid) run frame
/// by frame through the preset runtime, offline, every frame under the
/// levers it was built with.
struct BoxRun {
    device: manifold_gpu::testkit::TestDevice,
    runtime: manifold_node_engine::runtime::PresetRuntime,
    target: manifold_node_engine::gpu::render_target::RenderTarget,
    manifest: manifold_core::params::ParamManifest,
    frame: i64,
    all: bool,
    poison: bool,
    /// Every pass of an inactive clock slot runs (`set_gate_off`).
    ungated: bool,
    /// Ticks each `step` frame covers; above 1 the coupled pair host-syncs
    /// between them.
    ticks_per_frame: u32,
    _scope: manifold_node_engine::water::physics::PhysicsStepScope,
}

const BOX_SIZE: u32 = 64;

fn box_def(fixture: manifold_node_engine::water::liquid::conformance::Fixture) -> manifold_core::effect_graph_def::EffectGraphDef {
    use crate::testkit::liquid_conformance_fixtures::LIQUID_SOLVERS;
    let row = LIQUID_SOLVERS
        .iter()
        .find(|row| row.type_id == manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID)
        .expect("the GPU FLIP row");
    (row.fixture)(fixture).unwrap_or_else(|| panic!("GPU FLIP has no {fixture:?} scene"))
}

impl BoxRun {
    fn new(fixture: manifold_node_engine::water::liquid::conformance::Fixture, all: bool, poison: bool, level: i32) -> Self {
        Self::of(Self::at_level(fixture, level), all, poison)
    }

    /// The scene with every pass of an inactive clock slot run, as before
    /// the slots were gated.
    fn ungated(fixture: manifold_node_engine::water::liquid::conformance::Fixture, level: i32) -> Self {
        Self::with_levers(Self::at_level(fixture, level), false, false, true)
    }

    fn at_level(fixture: manifold_node_engine::water::liquid::conformance::Fixture, level: i32) -> manifold_core::effect_graph_def::EffectGraphDef {
        let mut def = box_def(fixture);
        manifold_node_engine::water::liquid::conformance::set_node_param(
            &mut def,
            "domain",
            "solve_level",
            manifold_core::effect_graph_def::SerializedParamValue::Int { value: level },
        );
        def
    }

    fn of(def: manifold_core::effect_graph_def::EffectGraphDef, all: bool, poison: bool) -> Self {
        Self::with_levers(def, all, poison, false)
    }

    fn with_levers(def: manifold_core::effect_graph_def::EffectGraphDef, all: bool, poison: bool, ungated: bool) -> Self {
        let fixture = "box scene";
        let device = manifold_gpu::testkit::test_device();
        let registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
        let scope = manifold_node_engine::water::physics::PhysicsStepScope::for_render(true);
        let manifest = manifold_core::params::ParamManifest::from_params(
            def.preset_metadata
                .iter()
                .flat_map(|metadata| metadata.params.iter().cloned().map(manifold_core::params::Param::bundled))
                .collect(),
        );
        let mut runtime = manifold_node_engine::runtime::PresetRuntime::from_def_with_device(
            def,
            &registry,
            device.arc(),
            BOX_SIZE,
            BOX_SIZE,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            None,
        )
        .unwrap_or_else(|error| panic!("{fixture:?} builds: {error}"));
        runtime.set_dump_all(true);
        let target =
            manifold_node_engine::gpu::render_target::RenderTarget::new(&device, BOX_SIZE, BOX_SIZE, manifold_gpu::GpuTextureFormat::Rgba16Float, "body sparse");
        let mut run = Self { device, runtime, target, manifest, frame: 0, all, poison, ungated, ticks_per_frame: 1, _scope: scope };
        let started = std::time::Instant::now();
        loop {
            run.render(true);
            if !run.runtime.warmup_pending() {
                break;
            }
            assert!(started.elapsed().as_secs() < 60, "{fixture:?}: asset warm-up did not finish");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        run
    }

    fn render(&mut self, warming: bool) {
        use manifold_node_engine::water::fluid::TICK;
        let frame_seconds = f64::from(self.ticks_per_frame) * TICK;
        let time = self.frame as f64 * frame_seconds;
        let ctx = manifold_node_engine::runtime::preset_context::PresetContext {
            time,
            beat: time * 2.0,
            dt: if warming { 0.0 } else { frame_seconds as f32 },
            width: BOX_SIZE,
            height: BOX_SIZE,
            output_width: BOX_SIZE,
            output_height: BOX_SIZE,
            aspect: 1.0,
            owner_key: 0x1C0,
            is_clip_level: false,
            frame_count: self.frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut encoder = self.device.create_encoder("body sparse frame");
        set_all_tiles(self.all);
        set_poison(self.poison);
        set_gate_off(self.ungated);
        let status = {
            let mut gpu = manifold_node_engine::gpu::gpu_encoder::GpuEncoder::new(&mut encoder, &self.device);
            self.runtime.render(&mut gpu, &self.target.texture, &ctx, &self.manifest);
            gpu.frame_status()
        };
        encoder.commit_and_wait_completed();
        set_all_tiles(false);
        set_poison(false);
        set_gate_off(false);
        use manifold_node_engine::runtime::frame_status::FrameRenderStatus;
        assert!(
            status == FrameRenderStatus::Complete || (warming && status == FrameRenderStatus::PendingGeometry),
            "frame {} rendered with status {status:?}",
            self.frame
        );
    }

    /// One frame on: `ticks_per_frame` ticks.
    fn step(&mut self) {
        self.frame += 1;
        self.render(false);
    }

    /// The whole of the last-dumped `type_id.port` array, as words.
    fn words(&self, type_id: &str, port: &str) -> Vec<u32> {
        let dumps = self.runtime.dump_arrays_all();
        let dump = dumps
            .iter()
            .rev()
            .find(|dump| dump.type_id == type_id && dump.port == port)
            .unwrap_or_else(|| panic!("no {type_id}.{port} in the dump"));
        let bytes = dump.buffer.size();
        let staging = self.device.create_buffer_shared(bytes);
        let mut encoder = self.device.create_encoder("body sparse readback");
        encoder.copy_buffer_to_buffer(dump.buffer, &staging, bytes);
        encoder.commit_and_wait_completed();
        read(&staging, bytes as usize / 4)
    }

    /// The share of 8^3 tiles the cell passes ran over, from the last tick.
    fn tile_share(&self) -> f32 {
        let capped = self.words("node.gpu_flip_step", "capped");
        f32::from_bits(capped[capped.len() - SOLVER_WORDS as usize + TILE_SHARE])
    }

    /// What a tick leaves that the bodies touch: the one body's row, its
    /// reaction, the step's live particles and its solver words. Each is cut
    /// to what the tick wrote, so a buffer's slack never counts.
    fn left(&self) -> Vec<(&'static str, Vec<u32>)> {
        let domain = manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID;
        let cut = |mut words: Vec<u32>, len: usize| {
            words.truncate(len);
            words
        };
        let capped = self.words("node.gpu_flip_step", "capped");
        let particles = (capped.len() - SOLVER_WORDS as usize) / 2;
        let particle_words = std::mem::size_of::<manifold_node_engine::water::fluid_particles::FluidParticle>() / 4;
        // The active-tile share differs between sparse and all-tiles by design
        // (see `tile_share`); every other solver word must match bit for bit.
        let mut solver = capped[2 * particles..].to_vec();
        solver.remove(TILE_SHARE);
        vec![
            ("body row", cut(self.words(domain, "bodies"), std::mem::size_of::<LiquidBody>() / 4)),
            ("reaction", cut(self.words(domain, "reaction"), 8)),
            ("particles", cut(self.words("node.gpu_flip_step", "out"), particles * particle_words)),
            ("solver words", solver),
        ]
    }
}

/// A step with a Box3D body over its active tiles equals the step over
/// every tile, bit for bit, in the body rows, the reaction, the particles
/// and the solver words, tick after tick over the submerged and the floating
/// box, at Solve Level 0 and 1 (the coarse body term Pᵀ B P, section 11
/// (Solve Level)); and the same with the step's poison on, which also writes
/// NaN into every body partial slot before the solve.
#[test]
fn gpu_flip_body_step_sparse_matches_all_tiles() {
    use manifold_node_engine::water::liquid::conformance::Fixture;
    for (fixture, level) in [(Fixture::SubmergedBox, 0), (Fixture::FloatingBox, 0), (Fixture::SubmergedBox, 1), (Fixture::FloatingBox, 1)] {
        let mut dense = BoxRun::new(fixture, true, false, level);
        let mut sparse = BoxRun::new(fixture, false, false, level);
        let mut poisoned = BoxRun::new(fixture, false, true, level);
        for tick in 1..=90 {
            dense.step();
            sparse.step();
            poisoned.step();
            let want = dense.left();
            for (name, run) in [("sparse", &sparse), ("poisoned", &poisoned)] {
                assert_eq!(dense.tile_share(), 1.0, "{fixture:?} level {level} tick {tick}: all-tiles share");
                assert!(run.tile_share() <= dense.tile_share(), "{fixture:?} level {level} tick {tick}: {name} share {} exceeds all-tiles", run.tile_share());
                for ((what, a), (_, b)) in want.iter().zip(run.left()) {
                    assert_eq!(a.len(), b.len(), "{fixture:?} level {level} tick {tick}: {name} {what} is sized differently");
                    if let Some(i) = first_differing(a, &b) {
                        panic!(
                            "{fixture:?} level {level} tick {tick}: {name} {what} differs from all tiles first at word {i}: {} ({}) vs {} ({})",
                            a[i],
                            f32::from_bits(a[i]),
                            b[i],
                            f32::from_bits(b[i])
                        );
                    }
                }
            }
        }
        let words = dense.words("node.gpu_flip_step", "capped");
        let tail = &words[words.len() - SOLVER_WORDS as usize..];
        println!("{fixture:?} level {level}: 90 ticks bitwise; last solve {} iterations, capped {}", tail[0], tail[2]);
    }
}

/// Inactive clock slots (BUG-e6z6s (inactive FLIP slots)): a slot the
/// clock leaves no time to step arms its solves off, zeroes its tile and
/// pocket triples and skips its sorts and scans, and the step comes out as
/// it did when every pass of such a slot ran: the body row, the reaction,
/// the particles, the solver words, the faces, the substep schedule and
/// every substep's faces bit for bit, tick after tick, over the submerged
/// and the floating box at Solve Level 0 (the body product on the solve's
/// gate) and the floating box at 1 (the plain one, the coarse pockets).
/// Every frame dumps every array, so the ticks are few and level 1 runs one
/// box. The distance may differ only where the gated step holds the
/// canonical 3h: a tile that left C during a frame's inactive slots used to
/// be retired there and then have its φ put back by the slot's commit mask,
/// so it kept a stale φ outside C; now the next active step retires it.
#[test]
fn gpu_flip_inactive_slots_match_the_ungated_step() {
    use manifold_node_engine::water::liquid::conformance::Fixture;
    const STEP: &str = "node.gpu_flip_step";
    const TICKS: u32 = 30;
    for (fixture, level) in [(Fixture::SubmergedBox, 0), (Fixture::FloatingBox, 0), (Fixture::FloatingBox, 1)] {
        let mut gated = BoxRun::new(fixture, false, false, level);
        let mut ungated = BoxRun::ungated(fixture, level);
        let (mut inactive, mut slots, mut canonical_cells) = (0usize, 0usize, 0usize);
        for tick in 1..=TICKS {
            gated.step();
            ungated.step();
            let at = format!("{fixture:?} level {level} tick {tick}");
            let read = |run: &BoxRun| {
                let mut arrays = run.left();
                for port in ["faces", "substep_schedule", "substep_u", "substep_v", "substep_w"] {
                    arrays.push((port, run.words(STEP, port)));
                }
                arrays
            };
            let (arrays, got) = (read(&ungated), read(&gated));
            for ((what, a), (_, b)) in arrays.iter().zip(&got) {
                assert_eq!(a.len(), b.len(), "{at}: {what} is sized differently");
                if let Some(i) = first_differing(a, b) {
                    panic!("{at}: gated {what} differs first at word {i}: {} ({}) vs ungated {} ({})", b[i], f32::from_bits(b[i]), a[i], f32::from_bits(a[i]));
                }
            }
            assert_eq!(gated.tile_share(), ungated.tile_share(), "{at}: tile share");
            let schedule = &got.iter().find(|(what, _)| *what == "substep_schedule").expect("the schedule").1;
            slots += schedule.len() / 4;
            inactive += schedule.chunks_exact(4).filter(|row| f32::from_bits(row[0]) == 0.0).count();
            let (a, b) = (ungated.words(STEP, "distance"), gated.words(STEP, "distance"));
            assert_eq!(a.len(), b.len(), "{at}: distance is sized differently");
            let three_h = b.iter().map(|&w| f32::from_bits(w)).fold(f32::MIN, f32::max);
            assert!(three_h > 0.0, "{at}: no cell holds the canonical distance");
            for (i, (&old, &new)) in a.iter().zip(&b).enumerate() {
                if old != new {
                    assert_eq!(new, three_h.to_bits(), "{at}: distance cell {i} differs and the gated one is not canonical: {} vs ungated {}", f32::from_bits(new), f32::from_bits(old));
                    canonical_cells += 1;
                }
            }
        }
        assert!(inactive > 0, "{fixture:?} level {level}: no inactive slot in {slots}, so nothing was gated");
        println!("{fixture:?} level {level}: {TICKS} ticks bitwise; {inactive} of {slots} slots inactive; {canonical_cells} distance cells canonical where ungated kept a stale φ");
    }
}

/// Fresh one-step recording equals the complete GPU scheduler with two-way
/// coupling. An authored negative speed keeps the baseline on its six-slot
/// loop without changing Steps, CFL, numerical caps or body controls.
#[test]
fn gpu_flip_fresh_speed_preserves_coupled_bodies() {
    fresh_speed_preserves_coupled_bodies(1);
}

/// The second tick of a frame takes its speed sample and obstacle bound from
/// the host sync before it. The clock status read after each frame is that
/// second tick's, so a one-step shortcut there proves the sync sample was used.
#[test]
fn gpu_flip_fresh_speed_preserves_coupled_bodies_two_ticks_a_frame() {
    fresh_speed_preserves_coupled_bodies(2);
}

fn fresh_speed_preserves_coupled_bodies(ticks_per_frame: u32) {
    use manifold_node_engine::water::liquid::conformance::Fixture;
    const STEP: &str = "node.gpu_flip_step";
    const TICKS: u32 = 8;
    fn baseline(def: manifold_core::effect_graph_def::EffectGraphDef) -> manifold_core::effect_graph_def::EffectGraphDef {
        use manifold_core::effect_graph_def::EffectGraphWire;
        // The runtime builds this same flattened graph, so the edit lands on
        // the step it runs, wherever the scene groups it.
        let mut def = manifold_core::flatten::flatten_groups(&def).expect("box scene flattens");
        let steps: Vec<_> = def.nodes.iter().filter(|node| node.type_id == STEP).map(|node| node.id).collect();
        let [step] = steps[..] else { panic!("coupled proof requires exactly one GPU FLIP step") };
        let id = def.nodes.iter().map(|node| node.id).max().expect("box scene nodes")
            .checked_add(1).expect("room for baseline scalar id");
        def.nodes.push(serde_json::from_value(serde_json::json!({
            "id": id,
            "nodeId": "full_gpu_clock",
            "typeId": "node.value",
            "params": {"value": {"type": "Float", "value": -1.0}}
        })).expect("baseline negative speed scalar"));
        def.wires.retain(|wire| !(wire.to_node == step && wire.to_port == "retired_max_speed"));
        def.wires.push(EffectGraphWire {
            from_node: id, from_port: "out".into(),
            to_node: step, to_port: "retired_max_speed".into(),
        });
        def
    }
    for fixture in [Fixture::SubmergedBox, Fixture::FloatingBox] {
        let def = BoxRun::at_level(fixture, 0);
        let mut automatic = BoxRun::of(def.clone(), false, false);
        let mut full = BoxRun::of(baseline(def), false, false);
        automatic.ticks_per_frame = ticks_per_frame;
        full.ticks_per_frame = ticks_per_frame;
        let (mut shortcuts, mut reacting_ticks) = (0u32, 0u32);
        for tick in 1..=TICKS {
            automatic.step();
            full.step();
            let at = format!("{fixture:?} level 0, {ticks_per_frame} ticks a frame, frame {tick}");
            let (mut got, mut want) = (automatic.left(), full.left());
            let reaction = &got.iter().find(|(what, _)| *what == "reaction").expect("body reaction").1;
            reacting_ticks += u32::from(reaction.iter().any(|&word| {
                let value = f32::from_bits(word);
                value.is_finite() && value != 0.0
            }));
            for (what, type_id, port) in [
                ("full capped", STEP, "capped"),
                ("faces", STEP, "faces"),
                ("full stats", "node.liquid_stats", "stats_out"),
            ] {
                got.push((what, automatic.words(type_id, port)));
                want.push((what, full.words(type_id, port)));
            }
            for ((what, a), (expected_what, b)) in got.iter().zip(&want) {
                assert_eq!(what, expected_what, "{at}: matching array labels");
                assert_eq!(a.len(), b.len(), "{at}: {what} is sized differently");
                if let Some(i) = first_differing(a, b) {
                    panic!("{at}: automatic {what} differs first at word {i}: {} ({}) vs full scheduler {} ({})",
                        a[i], f32::from_bits(a[i]), b[i], f32::from_bits(b[i]));
                }
            }
            let (a, b) = (automatic.words(STEP, "clock_status"), full.words(STEP, "clock_status"));
            assert!(a.len() >= 8 && b.len() >= 8, "{at}: complete clock status");
            assert_eq!(&a[1..8], &b[1..8], "{at}: identical completed interval and clock diagnostics");
            for (label, status) in [("automatic", &a), ("full scheduler", &b)] {
                assert_eq!(status[2], 0.0f32.to_bits(), "{at}: {label} completed the interval");
                assert_eq!(status[5], 0, "{at}: {label} has no nonfinite clock input");
            }
            assert_eq!(b[0], 0.0f32.to_bits(), "{at}: baseline must retain its inactive tail slots");
            if f32::from_bits(a[0]) > 0.0 && a[6] == 1 {
                shortcuts += 1;
            }
        }
        assert!(shortcuts > 0, "{fixture:?}, {ticks_per_frame} ticks a frame: a frame's last tick never took the fresh one-step path");
        assert!(reacting_ticks > 0, "{fixture:?}: no nonzero finite body reaction, so coupling was not exercised");
        println!("{fixture:?} level 0, {ticks_per_frame} ticks a frame: {TICKS} coupled frames bitwise; {shortcuts} fresh one-step final ticks; {reacting_ticks} frames with body reaction");
    }
}

impl BoxRun {
    fn body_height(&self) -> f64 {
        let words = self.words(manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID, "bodies");
        let row: &LiquidBody = bytemuck::from_bytes(bytemuck::cast_slice(&words[..std::mem::size_of::<LiquidBody>() / 4]));
        f64::from(row.position_inv_mass[1])
    }
}

/// A box a quarter as dense as water, released under 0.2 m of it:
/// the coupled pressure solve must lift it out of the pool.
#[test]
fn gpu_flip_rising_box_clears_the_surface() {
    use manifold_node_engine::water::liquid::conformance::{BoxScene, Fixture, set_node_param};
    let scene = BoxScene::of(Fixture::SubmergedBox).expect("the submerged box");
    let mut def = box_def(Fixture::SubmergedBox);
    set_node_param(&mut def, "box_body", "density", manifold_core::effect_graph_def::SerializedParamValue::Float { value: 250.0 });
    let mut run = BoxRun::of(def, false, false);
    for _ in 1..=60 {
        run.step();
    }
    let height = run.body_height();
    println!("rising box: box centre at {height:.3} m");
    assert!(height > f64::from(scene.fill), "the box did not clear the surface ({height} m)");
}

// ── The body golden (docs/GPU_FLIP_PRESSURE_CAP_DESIGN.md section 9 (Phasing), C0) ──

const BODY_GOLDEN: &str = "gpu_flip_body_golden.txt";
/// Bits the scalars and the stop record hold before a solve.
const BODY_SENTINEL: u32 = 0x7fc0_dead;

fn fnv(words: &[u32]) -> u64 {
    words.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &w| (h ^ u64::from(w)).wrapping_mul(0x0000_0100_0000_01b3))
}

thread_local! {
    /// Body golden solves on a profiled encoder at this granularity, when set.
    static BODY_PROFILE: std::cell::Cell<Option<manifold_gpu::ProfileGranularity>> = const { std::cell::Cell::new(None) };
}

/// Every body golden case, direct and replayed, as the fixture's lines: the
/// coupled solve with its rounds gated on the fine level, plain (the gate
/// off), and at Solve Level 1, each Fixed and Converged; fingerprints of the
/// pressure, the body sums, the scalars and the stop record.
fn body_golden_lines() -> Vec<String> {
    let cells = N.iter().product::<usize>();
    let rhs_values = random_values(cells, 0xc0a0);
    let mut lines = Vec::new();
    for (name, level, gate_off) in [("gated", 0usize, false), ("plain", 0, true), ("level1", 1, false)] {
        for stop in [Stop::Fixed(1), Stop::Fixed(3), Stop::Fixed(16), Stop::Converged(64)] {
            for replay in [false, true] {
                let mut scene = Scene::with_water(0xc09e, random_water(cells, 0xc09f), false);
                let rhs = shared(&scene.device, &rhs_values);
                let pressure = shared(&scene.device, &vec![0.0_f32; cells]);
                let mut plan = [0_u32; 12];
                plan[0] = TICK.to_bits();
                plan[11] = 1;
                let plan = shared(&scene.device, &plan);
                scene.passes.set_clock_plan(&plan);
                let mut solver = PressureSolver::default();
                solver.prepare_pipelines(&scene.device);
                solver.set_clock_plan(&plan);
                let sentinel = shared(&scene.device, &vec![BODY_SENTINEL; 2 * MAX_ITERATIONS as usize + PROGRESS_FLOATS as usize]);
                let scalars = shared(&scene.device, &vec![0_u32; 2 * MAX_ITERATIONS as usize]);
                let record = shared(&scene.device, &vec![0_u32; PROGRESS_FLOATS as usize]);
                let mut cache = replay.then(GpuReplayCache::default);
                let mut fingerprints = [0u64; 4];
                // Twice: the replayed run records, then replays.
                for _ in 0..2 {
                    set_gate_off(gate_off);
                    let mut enc = scene.device.create_encoder("body golden");
                    let profiled = BODY_PROFILE.get();
                    if let Some(granularity) = profiled {
                        thread_local! {
                            static SAMPLER: std::cell::OnceCell<manifold_gpu::GpuTimestampSampler> = const { std::cell::OnceCell::new() };
                        }
                        let sampler = SAMPLER.with(|s| s.get_or_init(|| scene.device.create_timestamp_sampler(4096).expect("timestamp sampling")).clone());
                        enc.enable_profiling_at(sampler, &scene.device, granularity);
                    }
                    let water = Water { lattice: N.map(|v| v as u32), cell_size: H, water: &scene.buffers[0], faces: &scene.buffers[1], phi: None };
                    // SAFETY: a shared buffer sized for the lattice; the last solve completed.
                    unsafe { pressure.write(0, bytemuck::cast_slice(&vec![0u32; cells])) };
                    solver.prepare(&scene.device, &mut enc, &water).expect("prepares");
                    solver.seed_records(&mut enc, &sentinel);
                    let spanned = cache.take().map(|c| enc.begin_replay(&scene.device, c)).is_some();
                    let solve = Solve { rhs: &rhs, pressure: &pressure, stop, bodies: Some((&scene.passes, &scene.bodies())), level, coarse_rhs: None };
                    solver.solve(&mut enc, &water, solve).expect("coupled solve");
                    if spanned {
                        cache = Some(enc.end_replay());
                    }
                    solver.copy_scalars(&mut enc, &scalars);
                    enc.copy_buffer_to_buffer(solver.progress().expect("prepared"), &record, record.size);
                    if profiled.is_some() {
                        let profile = enc.commit_and_wait_profiled(&scene.device);
                        assert_eq!(profile.failed_command_buffers, 0, "{name} {stop:?}: the profiled solve ran");
                    } else {
                        enc.commit_and_wait_completed();
                    }
                    set_gate_off(false);
                    fingerprints = [
                        fnv(&read::<u32>(&pressure, cells)),
                        fnv(&bits(&scene.sums())),
                        fnv(&read::<u32>(&scalars, 2 * 64)),
                        fnv(&read::<u32>(&record, PROGRESS_FLOATS.min(68) as usize)),
                    ];
                }
                let [p, s, c, r] = fingerprints;
                let mode = if replay { "replay" } else { "direct" };
                lines.push(format!("{name} level {level} {stop:?} {mode} pressure {p:016x} sums {s:016x} scalars {c:016x} record {r:016x}"));
            }
        }
    }
    lines
}

/// The coupled solve against the golden recorded from main's solver before
/// the pressure cap work (BUG-fwp2n — unused solver rounds still cost encode
/// time): fine-level gated bodies, plain bodies and Solve Level 1 bodies,
/// direct and replayed, bit for bit. `MANIFOLD_RECORD_GOLDEN=1` rewrites the
/// fixture; only ever from main's solver code.
#[test]
fn gpu_flip_body_solve_matches_main_golden() {
    let path = format!("{}/tests/fixtures/{BODY_GOLDEN}", env!("CARGO_MANIFEST_DIR"));
    let lines = body_golden_lines();
    if std::env::var("MANIFOLD_RECORD_GOLDEN").is_ok_and(|v| v == "1") {
        let solver = std::env::var("MANIFOLD_GOLDEN_SOLVER").expect("MANIFOLD_GOLDEN_SOLVER names the main commit the solver code is from");
        let header = format!(
            "# GPU FLIP coupled-body golden (gpu_flip_body_solve_matches_main_golden)\n# solver code of {solver}\n# inputs: body scene water 0xc09f, rhs 0xc0a0, pressure zeroed, scalars and record seeded {BODY_SENTINEL:#010x}\n# fingerprints: FNV-1a over the pressure, the body sums, scalars[..128], progress[..68]\n"
        );
        std::fs::write(&path, header + &lines.join("\n") + "\n").expect("golden writes");
        println!("recorded {} cases from {solver}", lines.len());
        return;
    }
    let golden = std::fs::read_to_string(&path).expect("golden fixture reads");
    let expected: Vec<&str> = golden.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(expected.len(), lines.len(), "golden case count");
    let moved: Vec<String> =
        expected.iter().zip(&lines).filter(|(e, l)| **e != l.as_str()).map(|(e, l)| format!("want {e}\n got {l}")).collect();
    assert!(moved.is_empty(), "{} of {} golden cases moved:\n{}", moved.len(), lines.len(), moved.join("\n"));
}

/// The coupled-body golden on a profiled encoder at both granularities:
/// frame replay is off there, the rounds run as the template's executes,
/// and every case matches main's unrolled solve bit for bit.
#[test]
fn gpu_flip_body_golden_holds_profiled() {
    let path = format!("{}/tests/fixtures/{BODY_GOLDEN}", env!("CARGO_MANIFEST_DIR"));
    let golden = std::fs::read_to_string(&path).expect("golden fixture reads");
    let expected: Vec<&str> = golden.lines().filter(|l| !l.starts_with('#')).collect();
    for granularity in [manifold_gpu::ProfileGranularity::Tag, manifold_gpu::ProfileGranularity::Dispatch] {
        BODY_PROFILE.set(Some(granularity));
        let lines = std::panic::catch_unwind(body_golden_lines);
        BODY_PROFILE.set(None);
        let lines = lines.unwrap_or_else(|e| std::panic::resume_unwind(e));
        let moved: Vec<String> = expected.iter().zip(&lines).filter(|(e, l)| **e != l.as_str()).map(|(e, l)| format!("want {e}\n got {l}")).collect();
        assert!(moved.is_empty(), "{granularity:?}: {} golden cases moved:\n{}", moved.len(), moved.join("\n"));
    }
}

/// Coupled rounds past the stop write nothing, body passes included: with
/// the stop leaving later rounds executing, every body golden case still
/// matches main bit for bit.
#[test]
fn gpu_flip_body_rounds_past_the_stop_write_nothing() {
    manifold_node_engine::water::primitives::gpu_flip_pressure::set_keep_ranges(true);
    let lines = body_golden_lines();
    manifold_node_engine::water::primitives::gpu_flip_pressure::set_keep_ranges(false);
    let golden = std::fs::read_to_string(format!("{}/tests/fixtures/{BODY_GOLDEN}", env!("CARGO_MANIFEST_DIR"))).expect("golden fixture reads");
    let expected: Vec<&str> = golden.lines().filter(|l| !l.starts_with('#')).collect();
    let moved: Vec<String> = expected.iter().zip(&lines).filter(|(e, l)| **e != l.as_str()).map(|(e, l)| format!("want {e}\n got {l}")).collect();
    assert!(moved.is_empty(), "coupled rounds past the stop wrote something:\n{}", moved.join("\n"));
}

/// Chunked executes change no bit in the coupled solve either: at one round
/// an execute, at 3 and at the default, every body golden case matches main.
#[test]
fn gpu_flip_body_chunk_sizes_match_main_golden() {
    let golden = std::fs::read_to_string(format!("{}/tests/fixtures/{BODY_GOLDEN}", env!("CARGO_MANIFEST_DIR"))).expect("golden fixture reads");
    let expected: Vec<&str> = golden.lines().filter(|l| !l.starts_with('#')).collect();
    for chunk in [1, 3, 32] {
        let _chunk = manifold_node_engine::water::primitives::gpu_flip_pressure::set_round_chunk(chunk);
        let lines = body_golden_lines();
        let moved: Vec<String> = expected.iter().zip(&lines).filter(|(e, l)| **e != l.as_str()).map(|(e, l)| format!("want {e}\n got {l}")).collect();
        assert!(moved.is_empty(), "chunk {chunk}: {} body golden cases moved:\n{}", moved.len(), moved.join("\n"));
    }
}
