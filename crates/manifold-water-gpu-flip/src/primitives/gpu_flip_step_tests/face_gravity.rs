//! node.gpu_flip_step's face_gravity entry: gravity, the scene's force
//! lattice with its air, and impulses on a face, against a CPU f64 reference.

use super::*;
use manifold_physics::force_lattice::ForceLattice;

#[test]
fn gpu_flip_face_gravity_adds_gravity_and_holds_the_walls() {
    let faces = random_faces(0x96a7, false);
    let (g, dt) = ([0.5f32, -9.81, 1.25], 1.0f32 / 120.0);
    let step = StepParams {
        gravity: g,
        step_dt: dt,
        ..lattice()
    };
    let got: Vec<FaceSample> = Pass::new()
        .bind(3, &faces, Layout::Records)
        .bind(4, &faces, Layout::Records)
        .bind(12, &[0.0f32; 4], Layout::Dense)
        .bind(13, &[0.0f32; 4], Layout::Dense)
        .run_list("face_gravity", &step, 4, face_len());
    for (i, (face, before)) in got.iter().zip(&faces).enumerate() {
        let p = pad_coords(i);
        for a in 0..3 {
            let pushed = f64::from(before.velocity[a]) + f64::from(g[a]) * f64::from(dt);
            let (velocity, weight) = if !face_exists(p, a) {
                (0.0, 0.0)
            } else if p[a] == 0 || p[a] == N[a] {
                (0.0, f64::from(before.weight[a]))
            } else {
                (pushed, f64::from(before.weight[a]))
            };
            close(
                face.velocity[a],
                velocity,
                1.0,
                &format!("velocity {p:?}/{a}"),
            );
            close(face.weight[a], weight, 1.0, &format!("weight {p:?}/{a}"));
        }
    }
}

/// The scene's forces read the tick's lattice at each face's centre; the
/// impulses land on step 0 of the impulse tick only. Where the lattice
/// carries air the face relaxes toward the wind at its drag rate, exactly as
/// `ForceSample::step_velocity`; with none the push path is unchanged.
#[test]
fn gpu_flip_face_gravity_adds_the_scene_forces_and_impulses() {
    for air in [false, true] {
        face_gravity_case(air);
    }
}

fn face_gravity_case(air: bool) {
    let faces = random_faces(0x5ce7, false);
    let field = ForceLattice::covering(MIN, H, padded().map(|n| n as u32));
    let nodes = field.nodes();
    let mut rng = Stream::new(0xf0c3);
    let mut force_values = |count: usize| -> Vec<[f32; 4]> {
        (0..count)
            .flat_map(|_| {
                let push = [rng.signed(4.0), rng.signed(4.0), rng.signed(4.0)];
                let drag = if air { 0.25 + 3.0 * rng.unit() } else { 0.0 };
                let wind = [rng.signed(6.0), rng.signed(6.0), rng.signed(6.0)].map(|w| drag * w);
                [
                    [push[0], push[1], push[2], drag],
                    [wind[0], wind[1], wind[2], 0.0],
                ]
            })
            .collect()
    };
    // Two force lattices from tick 10; the step runs tick 11, so it reads the second.
    let forces = force_values(2 * field.node_count());
    let impulses: Vec<[f32; 4]> = (0..field.node_count())
        .map(|_| [rng.signed(4.0), rng.signed(4.0), rng.signed(4.0), 0.0])
        .collect();
    let flat = |values: &[[f32; 4]]| values.iter().flatten().copied().collect::<Vec<f32>>();
    let (g, dt) = ([0.5f32, -9.81, 1.25], 1.0f32 / 120.0);
    for step in [0u32, 1] {
        let params = StepParams {
            gravity: g,
            step_dt: dt,
            tick_index: 11,
            step_in_tick: step as i32,
            field_nodes: nodes,
            field_spacing: field.spacing(),
            force_lattices: 2,
            first_tick: 10,
            impulse_tick: 11,
            ..lattice()
        };
        let got: Vec<FaceSample> = Pass::new()
            .bind(3, &faces, Layout::Records)
            .bind(4, &faces, Layout::Records)
            .bind(12, &flat(&forces), Layout::Dense)
            .bind(13, &flat(&impulses), Layout::Dense)
            .run_list("face_gravity", &params, 4, face_len());
        for (i, (face, before)) in got.iter().zip(&faces).enumerate() {
            let p = pad_coords(i);
            for a in 0..3 {
                if !face_exists(p, a) {
                    close(face.velocity[a], 0.0, 1.0, &format!("velocity {p:?}/{a}"));
                    continue;
                }
                let x: [f32; 3] = std::array::from_fn(|d| {
                    MIN[d]
                        + H * if d == a {
                            p[d] as f32
                        } else {
                            p[d] as f32 + 0.5
                        }
                });
                let force = field.sample_forces(&forces[2 * field.node_count()..], x);
                let impulse = if step == 0 {
                    field.sample(&impulses, x)[a]
                } else {
                    0.0
                };
                let (v0, accel, k) = (
                    f64::from(before.velocity[a]),
                    f64::from(g[a]) + f64::from(force.push[a]),
                    f64::from(force.drag),
                );
                let stepped = if k > 0.0 {
                    let decay = (-k * f64::from(dt)).exp();
                    v0 * decay + (accel + f64::from(force.drag_wind[a])) * (1.0 - decay) / k
                } else {
                    v0 + accel * f64::from(dt)
                };
                let pushed = stepped + f64::from(impulse);
                let velocity = if p[a] == 0 || p[a] == N[a] {
                    0.0
                } else {
                    pushed
                };
                close(
                    face.velocity[a],
                    velocity,
                    1.0,
                    &format!("air {air} step {step} velocity {p:?}/{a}"),
                );
                close(
                    face.weight[a],
                    f64::from(before.weight[a]),
                    1.0,
                    &format!("weight {p:?}/{a}"),
                );
            }
        }
    }
}
