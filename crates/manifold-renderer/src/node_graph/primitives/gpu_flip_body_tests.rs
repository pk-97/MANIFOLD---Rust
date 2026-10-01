//! GPU value proofs for the passes that put dynamic bodies inside the GPU
//! FLIP pressure solve (docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (solids in
//! the water)) against CPU f64 references: each body's impulse, the bodies'
//! share of the operator, the velocity change on the solid faces and the
//! reaction. The lattice holds enough face records for two partial groups
//! per body, so the group order of the sum is exercised.

use manifold_gpu::{GpuBuffer, GpuDevice};

use super::gpu_flip_atom_tests::{FACE_FLOATS, assert_close, face_grid_len, random_values, random_water};
use super::gpu_flip_bodies::{BodyPasses, Bodies};
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

/// The constraint's drag through every owned cut face beside water:
/// ρh³·w·f·(u − v_s).
fn cpu_friction(faces: &[f32], water: &[f32], open: &[f32], solid: &[f32]) -> Vec<f64> {
    let mut out = vec![0.0; face_grid_len(N)];
    let mass = f64::from(DENSITY) * f64::from(H).powi(3);
    for i in 0..m().iter().product::<usize>() {
        let p = coords(i, m());
        for a in 0..3 {
            let w = f64::from(open[i * FACE_FLOATS + 4 + a]);
            if !inner(p, a) || owner(solid[i * FACE_FLOATS + 3], a).is_none() || !(w > 0.0 && w < 1.0) {
                continue;
            }
            let mut lo = p;
            lo[a] -= 1;
            if !(water[at(lo, N)] > 0.5 || water[at(p, N)] > 0.5) {
                continue;
            }
            let f = f64::from(solid[i * FACE_FLOATS + 4 + a]);
            let slip = f64::from(faces[i * FACE_FLOATS + a]) - f64::from(solid[i * FACE_FLOATS + a]);
            out[i * FACE_FLOATS + a] = mass * w * f * slip;
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
}

impl Scene {
    fn new(seed: u64) -> Self {
        let device = crate::test_device();
        let (open, solid) = fixture(seed);
        let water = random_water(N.iter().product(), seed + 1);
        let rows = bodies();
        let buffers = [shared(&device, &water), shared(&device, &open), shared(&device, &solid), shared(&device, &rows)];
        let mut passes = BodyPasses::default();
        passes.prepare(&device).expect("body passes");
        Self { device, open, solid, water, rows, buffers, passes }
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
    scene.passes.apply(&mut enc, &scene.bodies(), &direction_gpu, &s).expect("apply");
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
/// velocity change into the solid faces, then the friction against the
/// changed solid velocity into the reaction too.
#[test]
fn gpu_flip_body_reaction_matches_cpu() {
    let scene = Scene::new(0x7ea1);
    let cells: usize = N.iter().product();
    let pressure = random_values(cells, 0x7ea2);
    let faces = random_values(face_grid_len(N), 0x7ea3);
    let base = random_values(BODIES * 8, 0x7ea4);
    let pressure_gpu = shared(&scene.device, &pressure);
    let faces_gpu = shared(&scene.device, &faces);
    let reaction = shared(&scene.device, &base);
    let scratch = shared(&scene.device, &vec![0.0_f32; cells]);

    // The same impulse react starts with, on its own, to read its sums.
    let mut enc = scene.device.create_encoder("pressure sums");
    scene.passes.apply(&mut enc, &scene.bodies(), &pressure_gpu, &scratch).expect("apply");
    enc.commit_and_wait_completed();
    let pushed = scene.sums();
    let (want, scale) =
        cpu_body_sums(&cpu_pressure_impulse(&pressure, &scene.water, &scene.open, &scene.solid), &scene.solid, &scene.rows);
    assert_sums(&pushed, &want, &scale, "pressure sums");

    let mut enc = scene.device.create_encoder("react");
    scene.passes.react(&mut enc, &scene.bodies(), &pressure_gpu, &faces_gpu, &reaction).expect("react");
    enc.commit_and_wait_completed();

    let changed: Vec<f32> = read(&scene.buffers[2], face_grid_len(N));
    let want_changed = cpu_velocity_change(&scene.solid, &pushed, &scene.rows);
    let touched = want_changed.iter().zip(&scene.solid).filter(|(w, s)| (**w - f64::from(**s)).abs() > 1e-4).count();
    assert!(touched > 50, "the dynamic body's faces gain its velocity change: {touched}");
    assert_close(&changed, &want_changed, "velocity change");

    let drag = cpu_friction(&faces, &scene.water, &scene.open, &changed);
    let (want, scale) = cpu_body_sums(&drag, &scene.solid, &scene.rows);
    assert!(want[..3].iter().any(|&v| v.abs() > 1.0), "water drags the dynamic body");
    let dragged = scene.sums();
    assert_sums(&dragged, &want, &scale, "friction sums");

    let got: Vec<f32> = read(&reaction, BODIES * 8);
    let want: Vec<f64> = (0..BODIES * 8)
        .map(|k| {
            let (b, j) = (k / 8, k % 8);
            f64::from(base[k]) + f64::from(pushed[SUM_FLOATS * b + j]) + f64::from(dragged[SUM_FLOATS * b + j])
        })
        .collect();
    assert_close(&got, &want, "reaction");
}
