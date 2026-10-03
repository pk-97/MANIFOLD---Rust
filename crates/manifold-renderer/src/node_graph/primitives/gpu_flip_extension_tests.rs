//! CPU reference shared with the GPU extension proofs. Boundary seeding follows
//! FLIP Fluids GridUtils (MIT, Copyright (C) 2026 Ryan L. Guy & Dennis Fassbaender;
//! see THIRD_PARTY_NOTICES.md), adapted to our box-face walls.

use crate::node_graph::fluid_particles::FaceSample;

fn index(p: [usize; 3], n: [usize; 3]) -> usize {
    p[0] + (n[0] + 1) * (p[1] + (n[1] + 1) * p[2])
}

fn coords(i: usize, n: [usize; 3]) -> [usize; 3] {
    let m = n.map(|v| v + 1);
    [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])]
}

pub(super) fn cpu_extend(faces: &[FaceSample], n: [usize; 3]) -> Vec<FaceSample> {
    (0..faces.len())
        .map(|i| {
            let p = coords(i, n);
            let mut out = FaceSample::default();
            for a in 0..3 {
                if (0..3).any(|b| b != a && p[b] >= n[b]) {
                    continue;
                }
                out.velocity[a] = faces[i].velocity[a];
                out.weight[a] = faces[i].weight[a];
                if faces[i].weight[a] > 0.0 {
                    continue;
                }
                let (mut sum, mut hits) = (0.0f64, 0.0f64);
                let mut seeded = false;
                for b in 0..3 {
                    for d in [-1i64, 1] {
                        let q = p[b] as i64 + d;
                        let top = if b == a { n[b] as i64 } else { n[b] as i64 - 1 };
                        if q < 0 || q > top {
                            continue;
                        }
                        let mut r = p;
                        r[b] = q as usize;
                        let neighbour = faces[index(r, n)];
                        if neighbour.weight[a] > 0.0 {
                            sum += f64::from(neighbour.velocity[a]);
                            hits += 1.0;
                            seeded |= r[a] > 0 && r[a] < n[a];
                        }
                    }
                }
                if seeded {
                    out.velocity[a] = (sum / hits) as f32;
                    out.weight[a] = 1.0;
                }
            }
            out
        })
        .collect()
}

/// Two invalid layers between one moving fluid plane and a held-zero wall.
/// Transverse edge rows are included: these are fluid, not engine ghost cells.
pub(super) fn wall_gap(n: [usize; 3], axis: usize, high: bool) -> (Vec<FaceSample>, usize) {
    let mut faces = vec![FaceSample::default(); n.map(|v| v + 1).iter().product()];
    for (i, face) in faces.iter_mut().enumerate() {
        let p = coords(i, n);
        if (0..3).any(|b| b != axis && p[b] >= n[b]) {
            continue;
        }
        let distance = if high { n[axis] - p[axis] } else { p[axis] };
        if p[axis] == 0 || p[axis] == n[axis] || distance == 3 {
            face.weight[axis] = 1.0;
            face.velocity[axis] = if distance == 3 { 2.0 } else { 0.0 };
        }
    }
    let mut target = [0; 3];
    target[axis] = if high { n[axis] - 1 } else { 1 };
    (faces, index(target, n))
}

#[test]
fn wall_zero_waits_for_fluid_front_on_every_axis() {
    let n = [8; 3];
    for axis in 0..3 {
        for high in [false, true] {
            let (faces, target) = wall_gap(n, axis, high);
            let first = cpu_extend(&faces, n);
            assert_eq!(
                first[target].weight[axis], 0.0,
                "a wall alone cannot mark the gap valid"
            );
            let second = cpu_extend(&first, n);
            for (i, face) in second.iter().enumerate() {
                let p = coords(i, n);
                if (0..3).any(|b| b != axis && p[b] >= n[b]) {
                    continue;
                }
                let distance = if high { n[axis] - p[axis] } else { p[axis] };
                if distance == 1 {
                    assert_eq!(face.weight[axis], 1.0);
                    assert_eq!(
                        face.velocity[axis], 1.0,
                        "fluid 2 and wall 0 average at {p:?}/{axis}"
                    );
                } else if distance == 0 {
                    assert_eq!(face.velocity[axis], 0.0, "wall must stay held");
                }
            }
        }
    }
}

#[test]
fn walls_alone_never_create_valid_air() {
    let n = [8; 3];
    for axis in 0..3 {
        let (mut faces, _) = wall_gap(n, axis, false);
        for face in &mut faces {
            if face.velocity[axis] != 0.0 {
                *face = FaceSample::default();
            }
        }
        let initial_weights: Vec<_> = faces.iter().map(|f| f.weight).collect();
        for _ in 0..8 {
            faces = cpu_extend(&faces, n);
        }
        assert_eq!(
            faces.iter().map(|f| f.weight).collect::<Vec<_>>(),
            initial_weights
        );
    }
}

#[test]
fn stationary_fluid_is_still_a_valid_seed() {
    let n = [8; 3];
    let (mut faces, target) = wall_gap(n, 1, false);
    for face in &mut faces {
        face.velocity = [0.0; 4];
    }
    let extended = cpu_extend(&cpu_extend(&faces, n), n);
    assert_eq!(extended[target].velocity[1], 0.0);
    assert_eq!(
        extended[target].weight[1], 1.0,
        "validity comes from fluid, not nonzero speed"
    );
}
