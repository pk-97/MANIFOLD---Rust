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

use manifold_gpu::{GpuBuffer, GpuDevice};

use super::gpu_flip_atom_tests::{FACE_FLOATS, assert_close, face_grid_len, random_values, random_water};
use super::gpu_flip_bodies::{BodyPasses, Bodies};
use super::gpu_flip_pressure::{PressureSolver, Water};
use super::gpu_flip_step::{TILE, set_all_tiles, set_poison};
use super::liquid_surface_tests::read;
use crate::node_graph::liquid::bodies::LiquidBody;

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

fn posed(row: &LiquidBody) -> [f64; 3] {
    std::array::from_fn(|b| f64::from(row.position_inv_mass[b]) + f64::from(row.linear_velocity[b]) * f64::from(TICK))
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
/// impulse, and M⁻¹ times them for a dynamic body; 0 for any other. Also the
/// sum of |term| per float, the scale an f32 reduction's rounding grows with.
fn cpu_body_sums(impulses: &[f64], solid: &[f32], rows: &[LiquidBody]) -> (Vec<f64>, Vec<f64>) {
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
        let inertia = [row.inv_inertia_x, row.inv_inertia_y, row.inv_inertia_z];
        for k in 0..3 {
            record[8 + k] = f64::from(row.position_inv_mass[3]) * linear[k];
            record[12 + k] = (0..3).map(|j| f64::from(inertia[k][j]) * angular[j]).sum();
            s[8 + k] = f64::from(row.position_inv_mass[3]) * s[k];
            s[12 + k] = (0..3).map(|j| f64::from(inertia[k][j]).abs() * s[4 + j]).sum();
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
    device: crate::TestDevice,
    open: Vec<f32>,
    solid: Vec<f32>,
    water: Vec<f32>,
    rows: Vec<LiquidBody>,
    buffers: [GpuBuffer; 4],
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
        let device = crate::test_device();
        let (open, solid) = fixture(seed);
        let rows = bodies();
        let buffers = [shared(&device, &water), shared(&device, &open), shared(&device, &solid), shared(&device, &rows)];
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
        Self { device, open, solid, water, rows, buffers, passes, solver }
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
    let (want, scale) = cpu_body_sums(&impulses, &scene.solid, &scene.rows);
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
        cpu_body_sums(&cpu_pressure_impulse(&pressure, &scene.water, &scene.open, &scene.solid), &scene.solid, &scene.rows);
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
    device: crate::TestDevice,
    runtime: crate::preset_runtime::PresetRuntime,
    target: crate::render_target::RenderTarget,
    manifest: manifold_core::params::ParamManifest,
    frame: i64,
    all: bool,
    poison: bool,
    _scope: crate::node_graph::physics::PhysicsStepScope,
}

const BOX_SIZE: u32 = 64;

impl BoxRun {
    fn new(fixture: crate::node_graph::liquid::conformance::Fixture, all: bool, poison: bool, level: i32) -> Self {
        use crate::node_graph::liquid::conformance::LIQUID_SOLVERS;
        let device = crate::test_device();
        let row = LIQUID_SOLVERS
            .iter()
            .find(|row| row.type_id == manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID)
            .expect("the GPU FLIP row");
        let mut def = (row.fixture)(fixture).unwrap_or_else(|| panic!("GPU FLIP has no {fixture:?} scene"));
        let domain = def.nodes.iter_mut().find(|node| node.node_id.as_str() == "domain").expect("domain");
        domain.params.insert("solve_level".into(), manifold_core::effect_graph_def::SerializedParamValue::Int { value: level });
        let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
        let scope = crate::node_graph::physics::PhysicsStepScope::for_render(true);
        let manifest = manifold_core::params::ParamManifest::from_params(
            def.preset_metadata
                .iter()
                .flat_map(|metadata| metadata.params.iter().cloned().map(manifold_core::params::Param::bundled))
                .collect(),
        );
        let mut runtime = crate::preset_runtime::PresetRuntime::from_def_with_device(
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
            crate::render_target::RenderTarget::new(&device, BOX_SIZE, BOX_SIZE, manifold_gpu::GpuTextureFormat::Rgba16Float, "body sparse");
        let mut run = Self { device, runtime, target, manifest, frame: 0, all, poison, _scope: scope };
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
        use crate::node_graph::fluid::TICK;
        let time = self.frame as f64 * TICK;
        let ctx = crate::preset_context::PresetContext {
            time,
            beat: time * 2.0,
            dt: if warming { 0.0 } else { TICK as f32 },
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
        let status = {
            let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut encoder, &self.device);
            self.runtime.render(&mut gpu, &self.target.texture, &ctx, &self.manifest);
            gpu.frame_status()
        };
        encoder.commit_and_wait_completed();
        set_all_tiles(false);
        set_poison(false);
        use crate::frame_status::FrameRenderStatus;
        assert!(
            status == FrameRenderStatus::Complete || (warming && status == FrameRenderStatus::PendingGeometry),
            "frame {} rendered with status {status:?}",
            self.frame
        );
    }

    /// One tick on.
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
        let particles = (capped.len() - 7) / 2;
        let particle_words = std::mem::size_of::<crate::node_graph::fluid_particles::FluidParticle>() / 4;
        // The last solver word is the active-tile share, 1 under the forced
        // lever by construction; the six before it are the solve's own.
        let solver = capped[2 * particles..capped.len() - 1].to_vec();
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
    use crate::node_graph::liquid::conformance::Fixture;
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
        let tail = &words[words.len() - 7..];
        println!("{fixture:?} level {level}: 90 ticks bitwise; last solve {} iterations, capped {}", tail[0], tail[2]);
    }
}
