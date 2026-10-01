//! GPU value proofs for the atoms that put dynamic bodies inside the GPU
//! FLIP pressure solve (docs/GPU_FLIP_PRESSURE_SOLVE.md section 8 (solids in
//! the water)) against CPU f64 references, and the fused-vs-unfused proofs
//! of the ones that fuse.

use serde_json::json;

use super::body_pressure_product::BodyPressureProduct;
use super::face_impulse_to_bodies::FaceImpulseToBodies;
use super::friction_face_impulse::FrictionFaceImpulse;
use super::gpu_flip_atom_tests::{
    Chain, FACE_FLOATS, assert_close, face_grid_len, lattice_json, lattice_params, random_values, random_water, run_atom,
    step_ports,
};
use super::liquid_surface_tests::{Harness, read};
use super::pressure_face_impulse::PressureFaceImpulse;
use crate::gpu_encoder::GpuEncoder;
use crate::node_graph::backend::Backend;
use crate::node_graph::liquid::bodies::LiquidBody;

const N: [usize; 3] = [5, 4, 3];
const H: f32 = 0.25;
const MIN: [f32; 3] = [-0.6, 0.0, -0.4];
const TICK: f32 = 0.05;
const DENSITY: f32 = 1000.0;
/// Rows 1 and 2 are the bodies (rows 3, body_count 2).
const ROWS: usize = 3;
const BODIES: usize = 2;

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
    let mut solid = vec![0.0; records * FACE_FLOATS];
    let mut velocity = vec![0.0; records * FACE_FLOATS];
    for i in 0..records {
        let p = coords(i, m());
        let r = |k: usize| draw[i * 16 + k] + 0.5;
        solid[i * FACE_FLOATS + 7] = 0.3 + 0.7 * r(0);
        let mut code = 0u32;
        for a in 0..3 {
            if !inner(p, a) {
                continue;
            }
            let w = r(1 + a);
            let open = if w < 0.2 { 0.0 } else if w < 0.45 { 1.0 } else { 0.05 + 0.9 * (w - 0.45) / 0.55 };
            solid[i * FACE_FLOATS + 4 + a] = open;
            velocity[i * FACE_FLOATS + a] = 2.0 * (r(4 + a) - 0.5);
            velocity[i * FACE_FLOATS + 4 + a] = r(7 + a);
            let pick = r(10 + a);
            if open < 1.0 && pick > 0.33 {
                code += (u32::from(pick > 0.66) + 1) << (8 * a);
            }
        }
        velocity[i * FACE_FLOATS + 3] = code as f32;
    }
    (solid, velocity)
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

fn floats(rows: &[LiquidBody]) -> &[f32] {
    bytemuck::cast_slice(rows)
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

fn body_params() -> Vec<(&'static str, f32)> {
    vec![
        ("lattice_min_x", MIN[0]),
        ("lattice_min_y", MIN[1]),
        ("lattice_min_z", MIN[2]),
        ("cell_size", H),
        ("body_count", BODIES as f32),
        ("rows", ROWS as f32),
        ("tick_seconds", TICK),
    ]
}

// ── CPU references ─────────────────────────────────────────────────────────

fn cpu_pressure_impulse(pressure: &[f32], water: &[f32], solid: &[f32], velocity: &[f32]) -> Vec<f64> {
    let mut out = vec![0.0; face_grid_len(N)];
    let rho_h2 = f64::from(DENSITY) * f64::from(H) * f64::from(H);
    let p_of = |q: [usize; 3]| {
        let c = at(q, N);
        if water[c] > 0.5 { f64::from(pressure[c]) } else { 0.0 }
    };
    for i in 0..m().iter().product::<usize>() {
        let p = coords(i, m());
        let code = velocity[i * FACE_FLOATS + 3];
        out[i * FACE_FLOATS + 3] = f64::from(code);
        for a in 0..3 {
            if !inner(p, a) || owner(code, a).is_none() {
                continue;
            }
            let mut lo = p;
            lo[a] -= 1;
            let w = f64::from(solid[i * FACE_FLOATS + 4 + a]);
            let c_hi = f64::from(solid[i * FACE_FLOATS + 7]);
            let c_lo = f64::from(solid[at(lo, m()) * FACE_FLOATS + 7]);
            out[i * FACE_FLOATS + a] = rho_h2 * ((c_lo - w) * p_of(lo) - (c_hi - w) * p_of(p));
        }
    }
    out
}

fn cpu_friction(faces: &[f32], water: &[f32], solid: &[f32], velocity: &[f32]) -> Vec<f64> {
    let mut out = vec![0.0; face_grid_len(N)];
    let mass = f64::from(DENSITY) * f64::from(H).powi(3);
    for i in 0..m().iter().product::<usize>() {
        let p = coords(i, m());
        let code = velocity[i * FACE_FLOATS + 3];
        out[i * FACE_FLOATS + 3] = f64::from(code);
        for a in 0..3 {
            let w = f64::from(solid[i * FACE_FLOATS + 4 + a]);
            if !inner(p, a) || owner(code, a).is_none() || !(w > 0.0 && w < 1.0) {
                continue;
            }
            let mut lo = p;
            lo[a] -= 1;
            if !(water[at(lo, N)] > 0.5 || water[at(p, N)] > 0.5) {
                continue;
            }
            let f = f64::from(velocity[i * FACE_FLOATS + 4 + a]);
            let slip = f64::from(faces[i * FACE_FLOATS + a]) - f64::from(velocity[i * FACE_FLOATS + a]);
            out[i * FACE_FLOATS + a] = mass * w * f * slip;
        }
    }
    out
}

fn cpu_body_sums(impulses: &[f64], rows: &[LiquidBody], base: Option<&[f32]>) -> Vec<f64> {
    let mut out = vec![0.0; 64 * 16];
    let first = ROWS - BODIES;
    for b in 0..BODIES {
        let row = &rows[first + b];
        let c = posed(row);
        let (mut linear, mut angular) = ([0.0; 3], [0.0; 3]);
        for i in 0..m().iter().product::<usize>() {
            let p = coords(i, m());
            for a in 0..3 {
                if owner(impulses[i * FACE_FLOATS + 3] as f32, a) != Some(b) {
                    continue;
                }
                let s = impulses[i * FACE_FLOATS + a];
                let x = face_centre(p, a);
                let mut axis = [0.0; 3];
                axis[a] = 1.0;
                linear[a] += s;
                let turn = cross(std::array::from_fn(|k| x[k] - c[k]), axis);
                for k in 0..3 {
                    angular[k] += s * turn[k];
                }
            }
        }
        if let Some(base) = base {
            for k in 0..3 {
                linear[k] += f64::from(base[16 * b + k]);
                angular[k] += f64::from(base[16 * b + 4 + k]);
            }
        }
        let s = &mut out[16 * b..16 * b + 16];
        s[..3].copy_from_slice(&linear);
        s[4..7].copy_from_slice(&angular);
        if row.position_inv_mass[3] > 0.0 && row.accel_shape[3] >= 0.0 {
            let inertia = [row.inv_inertia_x, row.inv_inertia_y, row.inv_inertia_z];
            for k in 0..3 {
                s[8 + k] = f64::from(row.position_inv_mass[3]) * linear[k];
                s[12 + k] = (0..3).map(|j| f64::from(inertia[k][j]) * angular[j]).sum();
            }
        }
    }
    out
}

fn cpu_body_product(base: &[f32], water: &[f32], solid: &[f32], velocity: &[f32], sums: &[f32], rows: &[LiquidBody]) -> Vec<f64> {
    let first = ROWS - BODIES;
    (0..N.iter().product::<usize>())
        .map(|c| {
            if water[c] <= 0.5 {
                return f64::from(base[c]);
            }
            let p = coords(c, N);
            let open = f64::from(solid[at(p, m()) * FACE_FLOATS + 7]);
            let mut total = 0.0;
            for a in 0..3 {
                for side in 0..2 {
                    let mut f = p;
                    f[a] += side;
                    if f[a] == 0 || f[a] == N[a] {
                        continue;
                    }
                    let i = at(f, m());
                    let Some(b) = owner(velocity[i * FACE_FLOATS + 3], a) else { continue };
                    let centre = posed(&rows[first + b]);
                    let x = face_centre(f, a);
                    let r: [f64; 3] = std::array::from_fn(|k| x[k] - centre[k]);
                    let dv: [f64; 3] = std::array::from_fn(|k| f64::from(sums[16 * b + 8 + k]));
                    let dw: [f64; 3] = std::array::from_fn(|k| f64::from(sums[16 * b + 12 + k]));
                    let along = dv[a] + cross(dw, r)[a];
                    let sign = if side == 1 { 1.0 } else { -1.0 };
                    total += sign * (open - f64::from(solid[i * FACE_FLOATS + 4 + a])) * along;
                }
            }
            f64::from(base[c]) + total / f64::from(H)
        })
        .collect()
}

// ── Value proofs ───────────────────────────────────────────────────────────

#[test]
fn gpu_flip_pressure_face_impulse_matches_cpu() {
    let cells = N.iter().product();
    let (solid, velocity) = fixture(0xb0d1);
    let (pressure, water) = (random_values(cells, 0xb0d2), random_water(cells, 0xb0d3));
    let got = run_atom(
        &mut PressureFaceImpulse::new(),
        &[("pressure", &pressure), ("water", &water), ("solid_faces", &solid), ("solid_velocity", &velocity)],
        face_grid_len(N),
        &lattice_params(N, &[("cell_size", H), ("density", DENSITY)]),
    );
    let want = cpu_pressure_impulse(&pressure, &water, &solid, &velocity);
    let pushed = want.chunks(FACE_FLOATS).filter(|r| r[..3].iter().any(|&s| s != 0.0)).count();
    assert!(pushed > 10, "owned faces carry impulse: {pushed}");
    assert_close(&got, &want, "pressure face impulse");
}

#[test]
fn gpu_flip_friction_face_impulse_matches_cpu() {
    let cells = N.iter().product();
    let (solid, velocity) = fixture(0xf1c1);
    let faces = random_values(face_grid_len(N), 0xf1c2);
    let water = random_water(cells, 0xf1c3);
    let got = run_atom(
        &mut FrictionFaceImpulse::new(),
        &[("faces", &faces), ("water", &water), ("solid_faces", &solid), ("solid_velocity", &velocity)],
        face_grid_len(N),
        &lattice_params(N, &[("cell_size", H), ("density", DENSITY)]),
    );
    let want = cpu_friction(&faces, &water, &solid, &velocity);
    let dragged = want.chunks(FACE_FLOATS).filter(|r| r[..3].iter().any(|&s| s != 0.0)).count();
    assert!(dragged > 5, "cut owned faces by water drag: {dragged}");
    assert_close(&got, &want, "friction face impulse");
}

/// The sums over every owned face, with and without a base, and the base
/// sums landing in a bound reaction in place.
#[test]
fn gpu_flip_face_impulse_to_bodies_matches_cpu() {
    let (_, velocity) = fixture(0x5a11);
    let mut impulses = random_values(face_grid_len(N), 0x5a12);
    for (record, owners) in impulses.chunks_mut(FACE_FLOATS).zip(velocity.chunks(FACE_FLOATS)) {
        record[3] = owners[3];
    }
    let rows = bodies();
    let base = random_values(BODIES * 16, 0x5a13);
    let impulses64: Vec<f64> = impulses.iter().map(|&v| f64::from(v)).collect();
    let step = lattice_params(N, &body_params());
    let plain = run_atom(&mut FaceImpulseToBodies::new(), &[("impulses", &impulses), ("bodies", floats(&rows))], 64 * 16, &step);
    let want = cpu_body_sums(&impulses64, &rows, None);
    assert!(want[..3].iter().any(|&v| v.abs() > 0.1) && want[16..19].iter().any(|&v| v.abs() > 0.1), "both bodies own faces");
    assert!(want[24..27].iter().all(|&v| v == 0.0), "the prescribed body changes no velocity");
    assert_close(&plain, &want, "body sums");

    // Base and reaction through the real output ports.
    let mut harness = Harness::new();
    let (i_slot, _i) = harness.array(&impulses, impulses.len());
    let (b_slot, _b) = harness.array(floats(&rows), rows.len() * 32);
    let (base_slot, _base) = harness.array(&base, base.len());
    let (reaction_slot, reaction) = harness.array(&[7.0_f32; BODIES * 16], BODIES * 16);
    let (out_slot, out) = harness.array::<f32>(&[], 64 * 16);
    let mut errors = Vec::new();
    let mut native = harness.device.create_encoder("body sums");
    {
        let mut gpu = GpuEncoder::new(&mut native, &harness.device);
        let backend: &dyn Backend = &harness.backend;
        step_ports(
            &mut FaceImpulseToBodies::new(),
            &mut gpu,
            backend,
            &mut errors,
            &[("impulses", i_slot), ("bodies", b_slot), ("base", base_slot), ("reaction", reaction_slot)],
            &[("out", out_slot), ("reaction_out", reaction_slot)],
            &step,
        );
    }
    native.commit_and_wait_completed();
    assert!(errors.is_empty(), "{errors:?}");
    let want = cpu_body_sums(&impulses64, &rows, Some(&base));
    assert_close(&read(&out, 64 * 16), &want, "body sums on a base");
    assert_close(&read(&reaction, BODIES * 16), &want[..BODIES * 16], "reaction in place");
}

#[test]
fn gpu_flip_body_pressure_product_matches_cpu() {
    let cells = N.iter().product();
    let (solid, velocity) = fixture(0x9a0d);
    let (base, water) = (random_values(cells, 0x9a0e), random_water(cells, 0x9a0f));
    let sums = random_values(BODIES * 16, 0x9a10);
    let rows = bodies();
    let got = run_atom(
        &mut BodyPressureProduct::new(),
        &[
            ("base", &base),
            ("water", &water),
            ("solid_faces", &solid),
            ("solid_velocity", &velocity),
            ("sums", &sums),
            ("bodies", floats(&rows)),
        ],
        cells,
        &lattice_params(N, &body_params()),
    );
    let want = cpu_body_product(&base, &water, &solid, &velocity, &sums, &rows);
    let moved = want.iter().zip(&base).filter(|(w, b)| (**w - f64::from(**b)).abs() > 1e-3).count();
    assert!(moved > 5, "water cells beside owned faces change: {moved}");
    assert_close(&got, &want, "body pressure product");
}

// ── Fused vs unfused ───────────────────────────────────────────────────────

fn body_source(chain: &mut Chain, rows: &[LiquidBody]) -> usize {
    let id = chain.node("bodies", "test.body_source", json!({"max_capacity": {"type": "Int", "value": rows.len()}}));
    chain.sources.push(("bodies", floats(rows).to_vec()));
    id
}

fn float_extra(extra: &[(&'static str, f32)]) -> Vec<(&'static str, f64)> {
    extra.iter().map(|&(k, v)| (k, f64::from(v))).collect()
}

/// The pressure's face impulse and the friction impulse, fused side by side
/// on the same solids (the step's shape: both read the solids' face velocity
/// coincident), summed into one face grid by friction's faces input.
#[test]
fn gpu_flip_face_impulses_fuse() {
    let cells = N.iter().product();
    let (solid, velocity) = fixture(0xfe01);
    let (pressure, water) = (random_values(cells, 0xfe02), random_water(cells, 0xfe03));
    let mut chain = Chain::new();
    let s = chain.face_source("solid", solid.clone());
    let v = chain.face_source("velocity", velocity.clone());
    let p = chain.source("pressure", pressure.clone());
    let w = chain.source("water", water.clone());
    let extra = float_extra(&[("cell_size", H), ("density", DENSITY)]);
    let push = chain.node("push", "node.pressure_face_impulse", lattice_json(N, &extra));
    chain.wire(p, "out", push, "pressure");
    chain.wire(w, "out", push, "water");
    chain.wire(s, "out", push, "solid_faces");
    chain.wire(v, "out", push, "solid_velocity");
    let drag = chain.node("drag", "node.friction_face_impulse", lattice_json(N, &extra));
    chain.wire(push, "out", drag, "faces");
    chain.wire(w, "out", drag, "water");
    chain.wire(s, "out", drag, "solid_faces");
    chain.wire(v, "out", drag, "solid_velocity");
    chain.sink = "test.face_sink";
    let got = chain.fused_matches_unfused(drag, face_grid_len(N));
    let pushed: Vec<f32> = cpu_pressure_impulse(&pressure, &water, &solid, &velocity).iter().map(|&v| v as f32).collect();
    assert_close(&got, &cpu_friction(&pushed, &water, &solid, &velocity), "fused impulses");
}

/// A per-cell producer fused into the bodies' share: the product reads its
/// base coincident.
#[test]
fn gpu_flip_divide_into_body_product_fuses() {
    let cells: usize = N.iter().product();
    let (solid, velocity) = fixture(0xfe11);
    let (value, water) = (random_values(cells, 0xfe12), random_water(cells, 0xfe13));
    let sums = random_values(BODIES * 16, 0xfe14);
    let rows = bodies();
    let mut chain = Chain::new();
    let s = chain.face_source("solid", solid.clone());
    let v = chain.face_source("velocity", velocity.clone());
    let x = chain.source("value", value.clone());
    let d = chain.source("divisor", vec![0.25]);
    let w = chain.source("water", water.clone());
    let u = chain.source("sums", sums.clone());
    let b = body_source(&mut chain, &rows);
    let divide = chain.node("divide", "node.divide_by_value", json!({}));
    chain.wire(x, "out", divide, "values");
    chain.wire(d, "out", divide, "divisor");
    let product = chain.node("product", "node.body_pressure_product", lattice_json(N, &float_extra(&body_params())));
    chain.wire(divide, "out", product, "base");
    chain.wire(w, "out", product, "water");
    chain.wire(s, "out", product, "solid_faces");
    chain.wire(v, "out", product, "solid_velocity");
    chain.wire(u, "out", product, "sums");
    chain.wire(b, "out", product, "bodies");
    let got = chain.fused_matches_unfused(product, cells);
    let base: Vec<f32> = value.iter().map(|&v| v / 0.25).collect();
    assert_close(&got, &cpu_body_product(&base, &water, &solid, &velocity, &sums, &rows), "fused divide into product");
}
