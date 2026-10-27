//! Checked against FLIP Fluids rigidfluidcoupling.cpp (MIT); see THIRD_PARTY_NOTICES.md.
//! GPU value proofs for the passes that put dynamic bodies inside the GPU
//! FLIP pressure solve (solids in the water) against CPU f64 references: each body's impulse, the bodies'
//! share of the operator, the velocity change on the solid faces and the
//! reaction. The lattice spans twelve tiles, partial edge tiles among them,
//! so the slot order of the sum is exercised. Then the passes over the
//! solver's active tiles against every tile, bitwise and under NaN poison,
//! on their own and through a step with a Box3D body
//! (docs/GPU_FLIP_SPARSE_BLOCKS_DESIGN.md D-9).

use manifold_gpu::{GpuBuffer, GpuDevice, GpuReplayCache};
use manifold_physics::coupled_motion::{
    Held, Mobility, SupportPoint, constrained_mobility, mobility_index,
};

use manifold_node_engine::testkit::array_harness::read;
use manifold_node_engine::testkit::atom::{
    FACE_FLOATS, assert_close, face_grid_len, random_values, random_water,
};
use manifold_water_gpu_flip::primitives::gpu_flip_bodies::{Bodies, BodyPasses};
use manifold_water_gpu_flip::primitives::gpu_flip_pressure::{
    PROGRESS_FLOATS, PressureSolver, Solve, Stop, Water,
};
use manifold_water_gpu_flip::primitives::gpu_flip_step::{set_all_tiles, set_poison};
use manifold_water_liquid::bodies::LiquidBody;
use manifold_water_liquid::coupling::coupled_start;
use manifold_water_liquid::primitives::liquid_stats::SOLVER_WORDS;
use manifold_water_liquid::tile_storage::testkit::{
    from_pool, identity_table, records_from_pool, records_to_pool, to_pool,
};
use manifold_water_liquid::tile_storage::{TILE, pool_len};

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
            let fraction = if w < 0.2 {
                0.0
            } else if w < 0.45 {
                1.0
            } else {
                0.05 + 0.9 * (w - 0.45) / 0.55
            };
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
    let mut rows = vec![
        LiquidBody {
            position_inv_mass: [9.0, 9.0, 9.0, 3.0],
            ..LiquidBody::default()
        };
        ROWS
    ];
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
    std::array::from_fn(|b| {
        f64::from(MIN[b]) + (p[b] as f64 + if b == a { 0.0 } else { 0.5 }) * f64::from(H)
    })
}

/// A row's centre of mass: the passes read rows the step has already posed
/// (gpu_flip_step/solids.wgsl pose_bodies).
fn posed(row: &LiquidBody) -> [f64; 3] {
    std::array::from_fn(|b| f64::from(row.position_inv_mass[b]))
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
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
fn cpu_body_sums(
    impulses: &[f64],
    solid: &[f32],
    rows: &[LiquidBody],
    mobility: &[Mobility],
) -> (Vec<f64>, Vec<f64>) {
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
        let pushed = [
            linear[0], linear[1], linear[2], angular[0], angular[1], angular[2],
        ];
        let size = [s[0], s[1], s[2], s[4], s[5], s[6]];
        for i in 0..6 {
            let at = 8 + i + i / 3;
            record[at] = (0..6)
                .map(|j| f64::from(m[mobility_index(i, j)]) * pushed[j])
                .sum();
            s[at] = (0..6)
                .map(|j| f64::from(m[mobility_index(i, j)]).abs() * size[j])
                .sum();
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
fn cpu_body_product(
    base: &[f32],
    water: &[f32],
    open: &[f32],
    solid: &[f32],
    sums: &[f32],
    rows: &[LiquidBody],
) -> Vec<f64> {
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
                    let Some(b) = owner(solid[i * FACE_FLOATS + 3], a) else {
                        continue;
                    };
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
        assert!(
            (f64::from(*g) - w).abs() <= 1e-5 * (s + 1.0),
            "{what}[{k}]: {g} vs {w} (scale {s})"
        );
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
    /// The identity slot table (every box tile allocated) and its pool size.
    table: GpuBuffer,
    pool_tiles: u32,
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
        let mobility: Vec<Mobility> = rows
            .iter()
            .map(|row| constrained_mobility(&coupled_start(row), &[], Held::default()))
            .collect();
        let cells = N.map(|v| v as u32);
        let (table_words, pool_tiles) = identity_table(cells);
        let table = shared(&device, &table_words);
        // Records past the lattice are answered by the canonical block, so the
        // CPU references read the round trip the GPU sees.
        let pool_records = |dense: &[f32]| -> Vec<[f32; 8]> {
            records_to_pool(
                bytemuck::cast_slice::<f32, [f32; 8]>(dense),
                cells,
                &[[0.0f32; 8]; 8],
            )
        };
        let (open_pool, solid_pool) = (pool_records(&open), pool_records(&solid));
        let dense_records = |pool: &[[f32; 8]]| -> Vec<f32> {
            records_from_pool(pool, cells)
                .into_iter()
                .flatten()
                .collect()
        };
        let (open, solid) = (dense_records(&open_pool), dense_records(&solid_pool));
        let buffers = [
            shared(&device, &to_pool(&water, cells, &[0.0f32])),
            shared(&device, &open_pool),
            shared(&device, &solid_pool),
            shared(&device, &rows),
            shared(&device, &gpu_mobility(&mobility)),
        ];
        let mut passes = BodyPasses::default();
        passes.prepare_pipelines(&device);
        passes
            .prepare(&device, N.map(|v| v as u32), BODIES as u32)
            .expect("body passes");
        let mut solver = PressureSolver::default();
        solver.prepare_pipelines(&device);
        let mut enc = device.create_encoder("body scene tiles");
        set_all_tiles(all);
        let lattice = Water {
            lattice: N.map(|v| v as u32),
            cell_size: H,
            water: &buffers[0],
            faces: &buffers[1],
            phi: None,
        };
        solver
            .prepare(&device, &mut enc, &lattice, &table, pool_tiles)
            .expect("the solver prepares its tile lists");
        set_all_tiles(false);
        enc.commit_and_wait_completed();
        Self {
            device,
            open,
            solid,
            water,
            rows,
            mobility,
            buffers,
            table,
            pool_tiles,
            passes,
            solver,
        }
    }

    fn cells(&self) -> [u32; 3] {
        N.map(|v| v as u32)
    }

    /// Floats in a pooled cell field.
    fn pool_floats(&self) -> usize {
        pool_len(self.pool_tiles) as usize
    }

    /// A dense cell field uploaded pooled (the canonical block holds 0).
    fn cells_to_gpu(&self, dense: &[f32]) -> GpuBuffer {
        shared(&self.device, &to_pool(dense, self.cells(), &[0.0f32]))
    }

    /// A pooled cell field read back dense.
    fn cells_from_gpu(&self, buffer: &GpuBuffer) -> Vec<f32> {
        from_pool(&read::<f32>(buffer, self.pool_floats()), self.cells())
    }

    /// A pooled record array read back dense, flattened to floats.
    fn records_from_gpu(&self, buffer: &GpuBuffer) -> Vec<f32> {
        let pool: Vec<[f32; 8]> = read(buffer, self.pool_floats());
        records_from_pool(&pool, self.cells())
            .into_iter()
            .flatten()
            .collect()
    }

    /// The dynamic body held on a floor under it: its bottom corners closed
    /// and their patch unturned: it can only slide along the floor.
    fn hold(&mut self) {
        let corners = [
            [0.2, -0.2, 0.2],
            [-0.2, -0.2, 0.2],
            [-0.2, -0.2, -0.2],
            [0.2, -0.2, -0.2],
        ];
        let supports: Vec<SupportPoint> = corners
            .iter()
            .map(|&lever| SupportPoint {
                lever,
                normal: [0.0, 1.0, 0.0],
                friction: 0.5,
                patch_lever: [0.0, -0.2, 0.0],
                ..SupportPoint::default()
            })
            .collect();
        let held = Held {
            closed: 0b1111,
            stuck: 0,
            unturned: 0b1111,
        };
        self.mobility[FIRST] =
            constrained_mobility(&coupled_start(&self.rows[FIRST]), &supports, held);
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
    let (direction_gpu, s) = (scene.cells_to_gpu(&direction), scene.cells_to_gpu(&base));
    let mut enc = scene.device.create_encoder("body operator");
    scene
        .passes
        .apply(
            &mut enc,
            &scene.bodies(),
            scene.tiles(),
            &direction_gpu,
            &s,
            &scene.table,
            scene.pool_tiles,
        )
        .expect("apply");
    enc.commit_and_wait_completed();

    let impulses = cpu_pressure_impulse(&direction, &scene.water, &scene.open, &scene.solid);
    let (want, scale) = cpu_body_sums(&impulses, &scene.solid, &scene.rows, &scene.mobility);
    assert!(
        want[..3].iter().any(|&v| v.abs() > 1.0),
        "the dynamic body owns pushed faces"
    );
    assert!(
        want[SUM_FLOATS..].iter().all(|&v| v == 0.0),
        "the prescribed body takes no impulse"
    );
    let sums = scene.sums();
    assert_sums(&sums, &want, &scale, "pressure sums");

    let product = cpu_body_product(
        &base,
        &scene.water,
        &scene.open,
        &scene.solid,
        &sums,
        &scene.rows,
    );
    let moved = product
        .iter()
        .zip(&base)
        .filter(|(w, b)| (**w - f64::from(**b)).abs() > 1e-3)
        .count();
    assert!(
        moved > 50,
        "water cells beside the dynamic body's faces change: {moved}"
    );
    assert_close(&scene.cells_from_gpu(&s), &product, "body operator");
}

/// After the projection: the pressure's impulse into the reaction and its
/// velocity change into the solid faces.
#[test]
fn gpu_flip_body_reaction_matches_cpu() {
    let scene = Scene::new(0x7ea1);
    let cells: usize = N.iter().product();
    let pressure = random_values(cells, 0x7ea2);
    let base = random_values(BODIES * 8, 0x7ea4);
    let pressure_gpu = scene.cells_to_gpu(&pressure);
    let reaction = shared(&scene.device, &base);
    let scratch = scene.cells_to_gpu(&vec![0.0_f32; cells]);

    // The same impulse react starts with, on its own, to read its sums.
    let mut enc = scene.device.create_encoder("pressure sums");
    scene
        .passes
        .apply(
            &mut enc,
            &scene.bodies(),
            scene.tiles(),
            &pressure_gpu,
            &scratch,
            &scene.table,
            scene.pool_tiles,
        )
        .expect("apply");
    enc.commit_and_wait_completed();
    let pushed = scene.sums();
    let (want, scale) = cpu_body_sums(
        &cpu_pressure_impulse(&pressure, &scene.water, &scene.open, &scene.solid),
        &scene.solid,
        &scene.rows,
        &scene.mobility,
    );
    assert_sums(&pushed, &want, &scale, "pressure sums");

    let mut enc = scene.device.create_encoder("react");
    scene
        .passes
        .react(
            &mut enc,
            &scene.bodies(),
            scene.tiles(),
            &pressure_gpu,
            &reaction,
            &scene.table,
            scene.pool_tiles,
        )
        .expect("react");
    enc.commit_and_wait_completed();

    let changed = scene.records_from_gpu(&scene.buffers[2]);
    let want_changed = cpu_velocity_change(&scene.solid, &pushed, &scene.rows);
    let touched = want_changed
        .iter()
        .zip(&scene.solid)
        .filter(|(w, s)| (**w - f64::from(**s)).abs() > 1e-4)
        .count();
    assert!(
        touched > 50,
        "the dynamic body's faces gain its velocity change: {touched}"
    );
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
        let (x_gpu, s) = (scene.cells_to_gpu(x), scene.cells_to_gpu(&zeros));
        let mut enc = scene.device.create_encoder("held body operator");
        scene
            .passes
            .apply(
                &mut enc,
                &scene.bodies(),
                scene.tiles(),
                &x_gpu,
                &s,
                &scene.table,
                scene.pool_tiles,
            )
            .expect("apply");
        enc.commit_and_wait_completed();
        scene.cells_from_gpu(&s)
    };
    let (x, y) = (random_values(cells, 0x5e1e), random_values(cells, 0x5e1f));
    let bx = product(&x);
    let sums = scene.sums();
    let (want, scale) = cpu_body_sums(
        &cpu_pressure_impulse(&x, &scene.water, &scene.open, &scene.solid),
        &scene.solid,
        &scene.rows,
        &scene.mobility,
    );
    assert_sums(&sums, &want, &scale, "held sums");
    assert!(
        want[..3].iter().any(|&v| v.abs() > 1.0),
        "the held body owns pushed faces"
    );
    for (k, what) in [(9, "sinks"), (12, "tips about x"), (14, "tips about z")] {
        assert!(
            want[k].abs() <= 1e-5 * (scale[k] + 1.0),
            "the held body {what}: {}",
            want[k]
        );
    }
    assert_close(
        &bx,
        &cpu_body_product(
            &zeros,
            &scene.water,
            &scene.open,
            &scene.solid,
            &sums,
            &scene.rows,
        ),
        "held body operator",
    );

    let by = product(&y);
    let in_water: Vec<usize> = (0..cells).filter(|&k| scene.water[k] > 0.5).collect();
    let dot = |a: &[f32], b: &[f32]| {
        in_water
            .iter()
            .map(|&k| f64::from(a[k]) * f64::from(b[k]))
            .sum::<f64>()
    };
    let size = in_water
        .iter()
        .map(|&k| (f64::from(y[k]) * f64::from(bx[k])).abs())
        .sum::<f64>();
    let (ybx, xby, xbx) = (dot(&y, &bx), dot(&x, &by), dot(&x, &bx));
    assert!(size > 1.0, "the operator acts: {size}");
    assert!(
        (ybx - xby).abs() <= 1e-4 * size,
        "yᵀBx {ybx} against xᵀBy {xby} (scale {size})"
    );
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
            let mut scene =
                Scene::with_water(0xc09e, random_water(N.iter().product(), 0xc09f), false);
            let cells = N.iter().product::<usize>();
            let rhs = scene.cells_to_gpu(&vec![0.0_f32; cells]);
            let pressure = scene.cells_to_gpu(&vec![0.0_f32; cells]);
            let progress = shared(&scene.device, &vec![0_u32; PROGRESS_FLOATS as usize]);
            let plan = shared(&scene.device, &[0_u32; 12]);
            scene.passes.set_clock_plan(&plan);
            let mut solver = PressureSolver::default();
            solver.prepare_pipelines(&scene.device);
            solver.set_clock_plan(&plan);
            Self {
                scene,
                solver,
                rhs,
                pressure,
                progress,
                plan,
                cache: replay.then(GpuReplayCache::default),
            }
        }

        fn solve(
            &mut self,
            rhs: &[f32],
            stop: Stop,
            active: bool,
        ) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
            let pooled_rhs = to_pool(rhs, self.scene.cells(), &[0.0f32]);
            let mut plan = [0_u32; 12];
            plan[0] = if active { TICK.to_bits() } else { 0 };
            plan[11] = 1;
            // SAFETY: shared buffers sized for these values; the previous solve completed.
            unsafe {
                self.rhs.write(0, bytemuck::cast_slice(&pooled_rhs));
                self.plan.write(0, bytemuck::cast_slice(&plan));
            }
            let mut enc = self.scene.device.create_encoder("coupled pressure replay");
            let replay = self
                .cache
                .take()
                .map(|cache| enc.begin_replay(&self.scene.device, cache))
                .is_some();
            let water = Water {
                lattice: N.map(|v| v as u32),
                cell_size: H,
                water: &self.scene.buffers[0],
                faces: &self.scene.buffers[1],
                phi: None,
            };
            self.solver
                .prepare(
                    &self.scene.device,
                    &mut enc,
                    &water,
                    &self.scene.table,
                    self.scene.pool_tiles,
                )
                .expect("prepare coupled solve");
            self.solver
                .solve(
                    &mut enc,
                    &water,
                    Solve {
                        rhs: &self.rhs,
                        pressure: &self.pressure,
                        stop,
                        bodies: Some((&self.scene.passes, &self.scene.bodies())),
                        level: 0,
                        coarse_rhs: None,
                    },
                    &self.scene.table,
                    self.scene.pool_tiles,
                )
                .expect("coupled solve");
            if replay {
                self.cache = Some(enc.end_replay());
            }
            enc.copy_buffer_to_buffer(
                self.solver.progress().expect("prepared"),
                &self.progress,
                self.progress.size,
            );
            enc.commit_and_wait_completed();
            (
                bits(&self.scene.cells_from_gpu(&self.pressure)),
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
        assert_eq!(
            got, want,
            "visit {visit}: pressure, body sums and progress match direct bits"
        );
        assert!(
            got.0
                .iter()
                .chain(&got.1)
                .all(|&v| f32::from_bits(v).is_finite()),
            "visit {visit}: finite outputs"
        );
        if visit == 4 {
            assert!(
                got.0.iter().all(|&v| v == 0),
                "zero RHS leaves zero pressure"
            );
            assert_eq!(got.2[0], 0);
            assert_eq!(got.2[1], 0, "zero RHS stops before the first iteration");
            assert_eq!(got.2[2], 1.0_f32.to_bits());
            stopped = Some(got.clone());
        } else if visit == 5 {
            assert_eq!(
                Some(&got),
                stopped.as_ref(),
                "inactive slot preserves pressure, body sums and progress"
            );
        } else {
            assert_eq!(
                got.2[1],
                (ROUNDS as f32).to_bits(),
                "visit {visit}: all fixed rounds ran"
            );
            assert_eq!(got.2[2], 0, "fixed solve has no convergence stop");
            assert!(
                got.0.iter().any(|&v| f32::from_bits(v).abs() > 1e-6),
                "visit {visit}: nonzero pressure"
            );
            assert!(
                got.1[..3].iter().any(|&v| f32::from_bits(v).abs() > 1e-6),
                "visit {visit}: the dynamic body participates"
            );
        }
        let stats = replay
            .cache
            .as_ref()
            .expect("replay cache returned")
            .stats();
        let template = replay.solver.template_stats();
        // The first visit records its entry; the second grows the recording.
        if visit >= 2 {
            assert_eq!(
                stats.recorded, last.recorded,
                "visit {visit}: warm recordings unchanged"
            );
            assert_eq!(
                stats.store_allocations, last.store_allocations,
                "visit {visit}: warm storage unchanged"
            );
            assert_eq!(
                stats.segments_direct, last.segments_direct,
                "visit {visit}: no direct round dispatches"
            );
            assert_eq!(
                template.executes - last_template.executes,
                manifold_gpu::template_chunks(ROUNDS, 32).count() as u64,
                "visit {visit}: one execute per chunk of rounds, including stopped/inactive rounds"
            );
            assert_eq!(
                template.walks - last_template.walks,
                1,
                "visit {visit}: one walked round"
            );
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
            let last: [usize; 3] =
                std::array::from_fn(|a| (origin[a] + TILE as usize).min(N[a] - 1));
            (first[2]..=last[2]).any(|z| {
                (first[1]..=last[1])
                    .any(|y| (first[0]..=last[0]).any(|x| water[at([x, y, z], N)] > 0.5))
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
    let water: Vec<f32> = random_water(cells, seed + 1)
        .iter()
        .enumerate()
        .map(|(i, &w)| if coords(i, N)[0] < 6 { w } else { 0.0 })
        .collect();
    let active = active_tiles(&water);
    let inactive = active.iter().filter(|a| !**a).count();
    assert!(
        inactive >= 6,
        "{inactive} of {} tiles inactive",
        active.len()
    );
    let x = random_values(cells, seed + 2);
    let base = random_values(cells, seed + 3);
    let reaction_base = random_values(BODIES * 8, seed + 5);
    let run = |all: bool, poison: bool| -> Left {
        let scene = Scene::with_water(seed, water.clone(), all);
        let outside = |v: &[f32]| -> Vec<f32> {
            v.iter()
                .enumerate()
                .map(|(i, &v)| {
                    if poison && !active[tile_of(i)] {
                        f32::NAN
                    } else {
                        v
                    }
                })
                .collect()
        };
        let x_gpu = scene.cells_to_gpu(&outside(&x));
        let s = scene.cells_to_gpu(&outside(&base));
        let reaction = shared(&scene.device, &reaction_base);
        let mut enc = scene.device.create_encoder("sparse apply");
        if poison {
            scene.passes.poison(&mut enc, &scene.bodies());
        }
        scene
            .passes
            .apply(
                &mut enc,
                &scene.bodies(),
                scene.tiles(),
                &x_gpu,
                &s,
                &scene.table,
                scene.pool_tiles,
            )
            .expect("apply");
        enc.commit_and_wait_completed();
        let pushed = bits(&scene.sums());
        let mut enc = scene.device.create_encoder("sparse react");
        if poison {
            scene.passes.poison(&mut enc, &scene.bodies());
        }
        scene
            .passes
            .react(
                &mut enc,
                &scene.bodies(),
                scene.tiles(),
                &x_gpu,
                &reaction,
                &scene.table,
                scene.pool_tiles,
            )
            .expect("react");
        enc.commit_and_wait_completed();
        let s = scene.cells_from_gpu(&s);
        // Only the active tiles' s is compared under poison: the rest holds
        // the NaN written in, which nothing touched.
        let s = s
            .iter()
            .enumerate()
            .map(|(i, v)| {
                if active[tile_of(i)] {
                    v.to_bits()
                } else {
                    base[i].to_bits()
                }
            })
            .collect();
        Left {
            pushed,
            s,
            solid: bits(&scene.records_from_gpu(&scene.buffers[2])),
            reacted: bits(&scene.sums()),
            reaction: bits(&read(&reaction, BODIES * 8)),
        }
    };
    let dense = run(true, false);
    assert!(
        dense.pushed[..3]
            .iter()
            .any(|&b| f32::from_bits(b).abs() > 1.0),
        "the dynamic body is pushed"
    );
    for (name, left) in [
        ("sparse", run(false, false)),
        ("poisoned", run(false, true)),
    ] {
        for (what, a, b) in [
            ("pressure sums", &dense.pushed, &left.pushed),
            ("s", &dense.s, &left.s),
            ("solid velocity", &dense.solid, &left.solid),
            ("react sums", &dense.reacted, &left.reacted),
            ("reaction", &dense.reaction, &left.reaction),
        ] {
            if let Some(i) = first_differing(a, b) {
                panic!(
                    "{name} {what} differs from all tiles first at {i}: {} vs {}",
                    f32::from_bits(a[i]),
                    f32::from_bits(b[i])
                );
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
}

const BOX_SIZE: u32 = 64;

fn box_def(
    fixture: manifold_nodes_water::testkit::conformance::Fixture,
) -> manifold_core::effect_graph_def::EffectGraphDef {
    use manifold_nodes::testkit::liquid_conformance_fixtures::LIQUID_SOLVERS;
    let row = LIQUID_SOLVERS
        .iter()
        .find(|row| row.type_id == manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID)
        .expect("the GPU FLIP row");
    (row.fixture)(fixture).unwrap_or_else(|| panic!("GPU FLIP has no {fixture:?} scene"))
}

impl BoxRun {
    fn new(
        fixture: manifold_nodes_water::testkit::conformance::Fixture,
        all: bool,
        poison: bool,
        level: i32,
    ) -> Self {
        Self::of(Self::at_level(fixture, level), all, poison)
    }

    fn at_level(
        fixture: manifold_nodes_water::testkit::conformance::Fixture,
        level: i32,
    ) -> manifold_core::effect_graph_def::EffectGraphDef {
        let mut def = box_def(fixture);
        manifold_nodes_water::testkit::conformance::set_node_param(
            &mut def,
            "domain",
            "solve_level",
            manifold_core::effect_graph_def::SerializedParamValue::Int { value: level },
        );
        def
    }

    fn of(def: manifold_core::effect_graph_def::EffectGraphDef, all: bool, poison: bool) -> Self {
        let fixture = "box scene";
        let device = manifold_gpu::testkit::test_device();
        let registry = manifold_node_engine::persistence::PrimitiveRegistry::with_builtin();
        let manifest = manifold_core::params::ParamManifest::from_params(
            def.preset_metadata
                .iter()
                .flat_map(|metadata| {
                    metadata
                        .params
                        .iter()
                        .cloned()
                        .map(manifold_core::params::Param::bundled)
                })
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
        let target = manifold_node_engine::gpu::render_target::RenderTarget::new(
            &device,
            BOX_SIZE,
            BOX_SIZE,
            manifold_gpu::GpuTextureFormat::Rgba16Float,
            "body sparse",
        );
        let mut run = Self {
            device,
            runtime,
            target,
            manifest,
            frame: 0,
            all,
            poison,
        };
        // A poll count, not a wall-clock budget, so a loaded machine cannot fail it.
        let mut polls = 0u32;
        loop {
            run.render(true);
            if !run.runtime.warmup_pending() {
                break;
            }
            polls += 1;
            assert!(polls < 6000, "{fixture:?}: asset warm-up did not finish");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        run
    }

    fn render(&mut self, warming: bool) {
        use manifold_physics::clock::TICK;
        let time = self.frame as f64 * TICK;
        let ctx = manifold_node_engine::runtime::preset_context::PresetContext {
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
            let mut gpu =
                manifold_node_engine::gpu::gpu_encoder::GpuEncoder::new(&mut encoder, &self.device);
            self.runtime
                .render(&mut gpu, &self.target.texture, &ctx, &self.manifest);
            gpu.frame_status()
        };
        encoder.commit_and_wait_completed();
        set_all_tiles(false);
        set_poison(false);
        use manifold_node_engine::runtime::frame_status::FrameRenderStatus;
        assert!(
            status == FrameRenderStatus::Complete
                || (warming && status == FrameRenderStatus::PendingGeometry),
            "frame {} rendered with status {status:?}",
            self.frame
        );
    }

    /// One frame on: one tick.
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
        let particle_words =
            std::mem::size_of::<manifold_node_engine::particles::FluidParticle>() / 4;
        // The active-tile share differs between sparse and all-tiles by design
        // (see `tile_share`); every other solver word must match bit for bit.
        let mut solver = capped[2 * particles..].to_vec();
        solver.remove(TILE_SHARE);
        vec![
            (
                "body row",
                cut(
                    self.words(domain, "bodies"),
                    std::mem::size_of::<LiquidBody>() / 4,
                ),
            ),
            ("reaction", cut(self.words(domain, "reaction"), 8)),
            (
                "particles",
                cut(
                    self.words("node.gpu_flip_step", "out"),
                    particles * particle_words,
                ),
            ),
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
    use manifold_nodes_water::testkit::conformance::Fixture;
    for (fixture, level) in [
        (Fixture::SubmergedBox, 0),
        (Fixture::FloatingBox, 0),
        (Fixture::SubmergedBox, 1),
        (Fixture::FloatingBox, 1),
    ] {
        let mut dense = BoxRun::new(fixture, true, false, level);
        let mut sparse = BoxRun::new(fixture, false, false, level);
        let mut poisoned = BoxRun::new(fixture, false, true, level);
        for tick in 1..=10 {
            dense.step();
            sparse.step();
            poisoned.step();
            let want = dense.left();
            for (name, run) in [("sparse", &sparse), ("poisoned", &poisoned)] {
                assert_eq!(
                    dense.tile_share(),
                    1.0,
                    "{fixture:?} level {level} tick {tick}: all-tiles share"
                );
                assert!(
                    run.tile_share() <= dense.tile_share(),
                    "{fixture:?} level {level} tick {tick}: {name} share {} exceeds all-tiles",
                    run.tile_share()
                );
                for ((what, a), (_, b)) in want.iter().zip(run.left()) {
                    assert_eq!(
                        a.len(),
                        b.len(),
                        "{fixture:?} level {level} tick {tick}: {name} {what} is sized differently"
                    );
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
        println!(
            "{fixture:?} level {level}: 10 ticks bitwise; last solve {} iterations, capped {}",
            tail[0], tail[2]
        );
    }
}
