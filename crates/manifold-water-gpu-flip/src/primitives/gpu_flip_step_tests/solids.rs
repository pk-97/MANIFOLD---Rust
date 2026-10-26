//! Solids: open fractions, poses, solid face velocity, extrapolation, the constraint.

use super::*;

/// The solids' lattice: 5×4×3 cells a quarter metre apart.
pub(super) const SOLID_N: [usize; 3] = [5, 4, 3];

pub(super) const SOLID_H: f32 = 0.25;

pub(super) const SOLID_MIN: [f32; 3] = [-0.6, 0.0, -0.4];

pub(super) const SOLID_TICK: f32 = 0.05;

pub(super) fn solid_records() -> usize {
    SOLID_N.iter().map(|v| v + 1).product()
}

pub(super) fn solid_lattice() -> StepParams {
    StepParams {
        n: SOLID_N.map(|n| n as u32),
        box_min: SOLID_MIN,
        cell_size: SOLID_H,
        impulse_tick: -1,
        ..StepParams::default()
    }
}

/// In-box neighbours of cell `c`, each with the axis and side of the face to it.
pub(super) fn solid_neighbours(c: usize, n: [usize; 3]) -> Vec<(usize, isize)> {
    let p = [c % n[0], (c / n[0]) % n[1], c / (n[0] * n[1])];
    let mut out = Vec::with_capacity(6);
    for a in 0..3 {
        if p[a] > 0 {
            out.push((a, -1));
        }
        if p[a] + 1 < n[a] {
            out.push((a, 1));
        }
    }
    out
}

/// Inner faces one in four closed, one in four whole, the rest a fraction;
/// every face of cell `isolated` closed. Box walls closed.
pub(super) fn random_open_faces(n: [usize; 3], seed: u64, isolated: usize) -> Vec<f32> {
    let m = n.map(|v| v + 1);
    let mut faces = vec![0.0; face_grid_len(n)];
    for i in 0..m.iter().product::<usize>() {
        let p = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
        for a in 0..3 {
            if (0..3).all(|b| b == a || p[b] < n[b]) && p[a] > 0 && p[a] < n[a] {
                faces[i * FACE_FLOATS + 4 + a] = 1.0;
            }
        }
    }
    let draw = random_values(faces.len(), seed);
    for (i, v) in faces.iter_mut().enumerate() {
        if *v > 0.0 {
            let r = draw[i] + 0.5;
            *v = if r < 0.25 {
                0.0
            } else if r < 0.5 {
                1.0
            } else {
                0.05 + 0.95 * (r - 0.5) * 2.0
            };
        }
    }
    for (a, d) in solid_neighbours(isolated, n) {
        let mut p = [
            isolated % n[0],
            (isolated / n[0]) % n[1],
            isolated / (n[0] * n[1]),
        ];
        if d > 0 {
            p[a] += 1;
        }
        faces[(p[0] + m[0] * (p[1] + m[1] * p[2])) * FACE_FLOATS + 4 + a] = 0.0;
    }
    faces
}

/// FLIP Fluids' LevelsetUtils::fractionInside for a segment, in f64.
pub(super) fn engine_segment(left: f64, right: f64) -> f64 {
    if left < 0.0 && right < 0.0 {
        1.0
    } else if left < 0.0 {
        left / (left - right)
    } else if right < 0.0 {
        right / (right - left)
    } else {
        0.0
    }
}

/// FLIP Fluids' LevelsetUtils::fractionInside for a square, in f64, with the
/// branch it took (inside corners, and for two diagonal corners the middle's
/// sign) so a test can show it reached every case.
pub(super) fn engine_square(bl: f64, br: f64, tl: f64, tr: f64) -> (f64, (usize, bool)) {
    let inside = [bl, tl, br, tr].iter().filter(|&&v| v < 0.0).count();
    let mut list = [bl, br, tr, tl];
    let mut middle_inside = false;
    let fraction = match inside {
        4 => 1.0,
        3 => {
            while list[0] < 0.0 {
                list.rotate_left(1);
            }
            let side0 = 1.0 - engine_segment(list[0], list[3]);
            let side1 = 1.0 - engine_segment(list[0], list[1]);
            1.0 - 0.5 * side0 * side1
        }
        2 => {
            while list[0] >= 0.0 || !(list[1] < 0.0 || list[2] < 0.0) {
                list.rotate_left(1);
            }
            if list[1] < 0.0 {
                0.5 * (engine_segment(list[0], list[3]) + engine_segment(list[1], list[2]))
            } else if 0.25 * (list[0] + list[1] + list[2] + list[3]) < 0.0 {
                middle_inside = true;
                let side1 = 1.0 - engine_segment(list[0], list[3]);
                let side3 = 1.0 - engine_segment(list[2], list[3]);
                let side2 = 1.0 - engine_segment(list[2], list[1]);
                let side0 = 1.0 - engine_segment(list[0], list[1]);
                1.0 - (0.5 * side1 * side3 + 0.5 * side0 * side2)
            } else {
                let side0 = engine_segment(list[0], list[1]);
                let side1 = engine_segment(list[0], list[3]);
                let side2 = engine_segment(list[2], list[1]);
                let side3 = engine_segment(list[2], list[3]);
                0.5 * side0 * side1 + 0.5 * side2 * side3
            }
        }
        1 => {
            while list[0] >= 0.0 {
                list.rotate_left(1);
            }
            0.5 * engine_segment(list[0], list[3]) * engine_segment(list[0], list[1])
        }
        _ => 0.0,
    };
    let diagonal = inside == 2 && !((bl < 0.0) == (br < 0.0) || (bl < 0.0) == (tl < 0.0));
    (fraction, (inside, diagonal && middle_inside))
}

/// FLIP Fluids' LevelsetUtils::volumeFraction for a tetrahedron, in f64,
/// sorted by the engine's five-swap network.
pub(super) fn engine_tet(p: [f64; 4]) -> f64 {
    let [mut a, mut b, mut c, mut d] = p;
    for (x, y) in [(0, 1), (2, 3), (0, 2), (1, 3), (1, 2)] {
        let mut v = [a, b, c, d];
        if v[x] > v[y] {
            v.swap(x, y);
        }
        [a, b, c, d] = v;
    }
    let tet = |a: f64, b: f64, c: f64, d: f64| a * a * a / ((a - b) * (a - c) * (a - d));
    if d <= 0.0 {
        1.0
    } else if c <= 0.0 {
        1.0 - tet(d, c, b, a)
    } else if b <= 0.0 {
        let (p, q, r, s) = (a / (a - c), a / (a - d), b / (b - d), b / (b - c));
        p * q * (1.0 - s) + q * (1.0 - r) * s + r * s
    } else if a <= 0.0 {
        tet(a, b, c, d)
    } else {
        0.0
    }
}

/// FLIP Fluids' MeshLevelSet::_getCellWeight: the fraction of a cell inside
/// the solid, c[i + 2j + 4k] the distance at corner (i, j, k).
pub(super) fn engine_cube(c: [f64; 8]) -> f64 {
    if c.iter().all(|&v| v < 0.0) {
        return 1.0;
    }
    if c.iter().all(|&v| v >= 0.0) {
        return 0.0;
    }
    let [p000, p100, p010, p110, p001, p101, p011, p111] = c;
    (engine_tet([p000, p001, p101, p011])
        + engine_tet([p000, p101, p100, p110])
        + engine_tet([p000, p010, p011, p110])
        + engine_tet([p101, p011, p111, p110])
        + 2.0 * engine_tet([p000, p011, p101, p110])
        + engine_tet([p100, p101, p001, p111])
        + engine_tet([p100, p001, p000, p010])
        + engine_tet([p100, p110, p111, p010])
        + engine_tet([p001, p111, p011, p010])
        + 2.0 * engine_tet([p100, p111, p001, p010]))
        / 12.0
}

/// The open fraction of every face from a corner lattice, as
/// FluidSimulation::_updateWeightGridThread takes MeshLevelSet's face
/// weights: U from (i,j,k), (i,j+1,k), (i,j,k+1), (i,j+1,k+1); V from
/// (i,j,k), (i,j,k+1), (i+1,j,k), (i+1,j,k+1); W from (i,j,k), (i,j+1,k),
/// (i+1,j,k), (i+1,j+1,k). Box walls closed.
pub(super) fn cpu_open_fractions(
    phi: &[f32],
    n: [usize; 3],
    tolerance: f64,
) -> (Vec<f64>, Vec<(usize, bool)>) {
    let m = n.map(|v| v + 1);
    let at = |i: usize, j: usize, k: usize| f64::from(phi[i + m[0] * (j + m[1] * k)]);
    let mut out = vec![0.0; m.iter().product::<usize>() * FACE_FLOATS];
    let mut branches = Vec::new();
    for k in 0..m[2] {
        for j in 0..m[1] {
            for i in 0..m[0] {
                let p = [i, j, k];
                if (0..3).all(|b| p[b] < n[b]) {
                    let corners: [f64; 8] =
                        std::array::from_fn(|c| at(i + (c & 1), j + ((c >> 1) & 1), k + (c >> 2)));
                    out[(i + m[0] * (j + m[1] * k)) * FACE_FLOATS + 7] =
                        (1.0 - engine_cube(corners)).clamp(0.0, 1.0);
                }
                for a in 0..3 {
                    if !(0..3).all(|b| b == a || p[b] < n[b]) || p[a] == 0 || p[a] == n[a] {
                        continue;
                    }
                    let corners = match a {
                        0 => [
                            at(i, j, k),
                            at(i, j + 1, k),
                            at(i, j, k + 1),
                            at(i, j + 1, k + 1),
                        ],
                        1 => [
                            at(i, j, k),
                            at(i, j, k + 1),
                            at(i + 1, j, k),
                            at(i + 1, j, k + 1),
                        ],
                        _ => [
                            at(i, j, k),
                            at(i, j + 1, k),
                            at(i + 1, j, k),
                            at(i + 1, j + 1, k),
                        ],
                    };
                    let (mut inside, branch) =
                        engine_square(corners[0], corners[1], corners[2], corners[3]);
                    if corners.iter().all(|c| c.abs() <= tolerance) {
                        inside = 0.5;
                    }
                    branches.push(branch);
                    out[(i + m[0] * (j + m[1] * k)) * FACE_FLOATS + 4 + a] =
                        (1.0 - inside).clamp(0.0, 1.0);
                }
            }
        }
    }
    (out, branches)
}

/// The step's open fractions against FLIP Fluids' face weights ported to f64
/// here: random corner distances reach every fractionInside case, and one
/// face's corners all within the interface tolerance is half open.
#[test]
pub(super) fn gpu_flip_open_fractions_match_the_engine() {
    let n = SOLID_N;
    let m = n.map(|v| v + 1);
    let offset = 1.5_f32;
    let mut phi: Vec<f32> = random_values(m.iter().product(), 0x50f)
        .iter()
        .map(|v| v * 0.6)
        .collect();
    // The U face at padded (2, 1, 1): corners (2,1,1), (2,2,1), (2,1,2), (2,2,2).
    for (i, j, k) in [(2, 1, 1), (2, 2, 1), (2, 1, 2), (2, 2, 2)] {
        phi[i + m[0] * (j + m[1] * k)] = 1.0e-7;
    }
    let tolerance = 8.0 * f64::from(f32::EPSILON) * (f64::from(SOLID_H) * 5.0 + f64::from(offset));
    let step = StepParams {
        box_offset: offset,
        ..solid_lattice()
    };
    let got: Vec<f32> = Pass::new().bind(9, &phi).run(
        "open_fractions",
        &step,
        4,
        face_grid_len(n),
        solid_records(),
    );
    let (want, branches) = cpu_open_fractions(&phi, n, tolerance);
    for case in [
        (0, false),
        (1, false),
        (2, false),
        (2, true),
        (3, false),
        (4, false),
    ] {
        assert!(
            branches.contains(&case),
            "the fixture reaches fractionInside case {case:?}"
        );
    }
    assert!(
        branches
            .iter()
            .any(|&(inside, middle)| inside == 2 && !middle),
        "an adjacent or outside-middle pair"
    );
    assert_eq!(
        want[(2 + m[0] * (1 + m[1])) * FACE_FLOATS + 4],
        0.5,
        "the planted face is on the interface"
    );
    let open_volume: Vec<f64> = want.chunks(FACE_FLOATS).map(|r| r[7]).collect();
    assert!(
        open_volume.iter().any(|&v| v > 0.0 && v < 1.0),
        "a cell cut by the solid"
    );
    assert_close(&got, &want, "open fractions");
}

/// Two moving, turning bodies whose lattices overlap, one scaled and one
/// dynamic, and a disabled row; their distance lattices as the atlas stores
/// them (half precision) and as f32.
pub(super) struct Solids {
    pub(super) bodies: Vec<LiquidBody>,
    pub(super) shapes: Vec<LiquidShape>,
    pub(super) atlas: Vec<u32>,
    pub(super) distances: Vec<f32>,
}

pub(super) fn solids() -> Solids {
    let shapes = vec![
        LiquidShape {
            origin_spacing: [-0.47, -0.43, -0.41, 0.3],
            dims_x: 4,
            dims_y: 4,
            dims_z: 4,
            atlas_offset: 0,
            scale_min: [1.0, 1.0, 1.0, 1.0],
        },
        LiquidShape {
            origin_spacing: [-0.39, -0.42, -0.44, 0.4],
            dims_x: 3,
            dims_y: 3,
            dims_z: 3,
            atlas_offset: 64,
            scale_min: [1.2, 1.0, 1.1, 1.0],
        },
    ];
    let distances: Vec<f32> = random_values(64 + 27, 0x50d)
        .iter()
        .map(|v| half::f16::from_f32(v * 0.9).to_f32())
        .collect();
    let mut atlas = Vec::new();
    pack_distance_atlas(&distances, &mut atlas);
    let body = |position: [f32; 3], velocity: [f32; 4], angular: [f32; 3], shape: f32| LiquidBody {
        position_inv_mass: [position[0], position[1], position[2], 0.0],
        rotation: [0.0, 0.0, 0.0, 1.0],
        linear_velocity: velocity,
        angular_velocity: [angular[0], angular[1], angular[2], 0.0],
        accel_shape: [0.0, 0.0, 0.0, shape],
        ..LiquidBody::default()
    };
    let mut bodies = vec![
        body(
            [-0.21, 0.38, -0.02],
            [0.7, -0.3, 0.2, 0.4],
            [0.0, 0.3, 1.5],
            0.0,
        ),
        body(
            [0.33, 0.61, 0.03],
            [-0.5, 0.1, 0.1, 0.9],
            [0.8, 0.0, -0.2],
            1.0,
        ),
        body([0.0, 0.5, 0.0], [3.0, 3.0, 3.0, 0.7], [0.0; 3], -1.0),
    ];
    // The second body is dynamic: it moves at its predicted velocity.
    bodies[1].position_inv_mass[3] = 0.5;
    bodies[1].accel_shape = [0.4, -9.8, 0.2, 1.0];
    bodies[1].inv_inertia_x = [0.3, 0.0, 0.0, 1.1];
    bodies[1].inv_inertia_y = [0.0, 0.3, 0.0, -0.6];
    bodies[1].inv_inertia_z = [0.0, 0.0, 0.3, 0.4];
    Solids {
        bodies,
        shapes,
        atlas,
        distances,
    }
}

pub(super) fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let cross = |a: [f64; 3], b: [f64; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let u = [q[0], q[1], q[2]];
    let t = cross(u, v).map(|c| 2.0 * c);
    let c = cross(u, t);
    std::array::from_fn(|i| v[i] + q[3] * t[i] + c[i])
}

/// A body at `SOLID_TICK` as pose_bodies poses it: position, rotation,
/// linear and angular velocity, and its packed mobility on the supports it
/// stays on.
pub(super) struct Posed {
    pub(super) position: [f32; 3],
    pub(super) rotation: [f32; 4],
    pub(super) linear: [f32; 3],
    pub(super) angular: [f32; 3],
    pub(super) mobility: Mobility,
}

/// Box3D's substep over a `SOLID_TICK` tick: the law's h.
pub(super) fn solid_h() -> f32 {
    manifold_physics::coupled_motion::coupled_substep(manifold_physics::Seconds(f64::from(
        SOLID_TICK,
    )))
}

/// Each body of `solids` at `SOLID_TICK`: a dynamic one by the coupled motion
/// law (`manifold_physics::coupled_motion`, the CPU twin) under its reaction
/// so far (8 floats a body) and its support points (one set a body); any
/// other at its own velocity (`body_pose_at`).
pub(super) fn posed(solids: &Solids, reaction: &[f32], contacts: &[BodySupports]) -> Vec<Posed> {
    let xyz = |v: [f32; 4]| [v[0], v[1], v[2]];
    solids
        .bodies
        .iter()
        .enumerate()
        .map(|(row, body)| {
            let start = coupled_start(body);
            let mut points = [SupportPoint::default(); MAX_SUPPORT_POINTS];
            let kept = unpack_supports(&contacts[row], &mut points);
            let points = &points[..kept];
            if body.position_inv_mass[3] > 0.0 {
                let push = &reaction[8 * row..8 * row + 8];
                let s = coupled_state_at(
                    &start,
                    points,
                    [push[0], push[1], push[2]],
                    [push[4], push[5], push[6]],
                    SOLID_TICK,
                    solid_h(),
                );
                let mobility = constrained_mobility(&start, points, s.held);
                Posed {
                    position: s.position,
                    rotation: s.rotation,
                    linear: s.linear_velocity,
                    angular: s.angular_velocity,
                    mobility,
                }
            } else {
                let (position, rotation) = body_pose_at(body, SOLID_TICK);
                let mobility = constrained_mobility(&start, points, Held::default());
                Posed {
                    position,
                    rotation,
                    linear: xyz(body.linear_velocity),
                    angular: xyz(body.angular_velocity),
                    mobility,
                }
            }
        })
        .collect()
}

/// The closest enabled body at x posed as `poses` holds them (its row and
/// signed distance), as liquid_shape_distance samples a lattice, past it adds
/// the gap and scales along the trilinear slope; none when the walls (inset
/// 0) are nearer. The margin is the gap
/// to the runner-up, body or walls, so the fixture can show no f32 rounding
/// decides either.
pub(super) fn closest(solids: &Solids, poses: &[Posed], x: [f64; 3]) -> (Option<usize>, f64) {
    let mut best: Option<(usize, f64)> = None;
    let mut gaps = Vec::new();
    for (row, body) in solids.bodies.iter().enumerate() {
        let shape_index = body.accel_shape[3];
        if shape_index < 0.0 {
            continue;
        }
        let shape = solids.shapes[shape_index as usize];
        let (position, q) = (poses[row].position, poses[row].rotation);
        let inverse = [
            -f64::from(q[0]),
            -f64::from(q[1]),
            -f64::from(q[2]),
            f64::from(q[3]),
        ];
        let local = rotate(
            inverse,
            std::array::from_fn(|i| x[i] - f64::from(position[i])),
        );
        let dims = [shape.dims_x, shape.dims_y, shape.dims_z].map(|d| d as usize);
        let g: [f64; 3] = std::array::from_fn(|i| {
            (local[i] / f64::from(shape.scale_min[i]) - f64::from(shape.origin_spacing[i]))
                / f64::from(shape.origin_spacing[3])
        });
        let c: [f64; 3] = std::array::from_fn(|i| g[i].clamp(0.0, (dims[i] - 1) as f64));
        let beyond = (0..3).map(|i| (g[i] - c[i]).powi(2)).sum::<f64>().sqrt()
            * f64::from(shape.origin_spacing[3]);
        let base: [usize; 3] = std::array::from_fn(|i| (c[i].floor() as usize).min(dims[i] - 2));
        let f: [f64; 3] = std::array::from_fn(|i| c[i] - base[i] as f64);
        let (mut d, mut slope) = (0.0, [0.0f64; 3]);
        for corner in 0..8 {
            let o = [corner & 1, (corner >> 1) & 1, corner >> 2];
            let w: [f64; 3] = std::array::from_fn(|i| if o[i] == 1 { f[i] } else { 1.0 - f[i] });
            let at = shape.atlas_offset as usize
                + (base[0] + o[0])
                + dims[0] * ((base[1] + o[1]) + dims[1] * (base[2] + o[2]));
            let v = f64::from(solids.distances[at]);
            d += w[0] * w[1] * w[2] * v;
            for i in 0..3 {
                let sign = if o[i] == 1 { 1.0 } else { -1.0 };
                slope[i] += sign * w[(i + 1) % 3] * w[(i + 2) % 3] * v;
            }
        }
        let steep = slope.iter().map(|v| v * v).sum::<f64>().sqrt();
        let across = (0..3)
            .map(|i| (slope[i] / f64::from(shape.scale_min[i])).powi(2))
            .sum::<f64>()
            .sqrt();
        let stretch = if steep > 1e-3 * f64::from(shape.origin_spacing[3]) {
            steep / across
        } else {
            f64::from(shape.scale_min[3])
        };
        d = (d + beyond) * stretch;
        gaps.push(d);
        if best.is_none_or(|(_, nearest)| d < nearest) {
            best = Some((row, d));
        }
    }
    let walls = (0..3)
        .map(|i| {
            let low = f64::from(SOLID_MIN[i]);
            (x[i] - low).min(low + SOLID_N[i] as f64 * f64::from(SOLID_H) - x[i])
        })
        .fold(f64::INFINITY, f64::min);
    gaps.push(walls);
    gaps.sort_by(f64::total_cmp);
    let gap = if gaps.len() > 1 {
        gaps[1] - gaps[0]
    } else {
        f64::INFINITY
    };
    (best.filter(|&(_, d)| d <= walls).map(|(row, _)| row), gap)
}

/// The step's solid face velocity in f64: the closest body's rigid velocity
/// at each cut inner face's centre, the mean friction at its four corners,
/// and the owner code Σ (body + 1)·256^axis in velocity w. Also the smallest
/// margin any query had (see `closest`) and how many faces a body moved.
pub(super) fn cpu_solid_face_velocity(
    open: &[f64],
    solids: &Solids,
    poses: &[Posed],
) -> (Vec<f64>, f64, usize) {
    let n = SOLID_N;
    let m = n.map(|v| v + 1);
    let h = f64::from(SOLID_H);
    let min = SOLID_MIN.map(f64::from);
    let mut out = vec![0.0; face_grid_len(n)];
    let mut margin = f64::INFINITY;
    let mut moved = 0;
    for i in 0..m.iter().product::<usize>() {
        let p = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
        for a in 0..3 {
            if !(0..3).all(|b| b == a || p[b] < n[b]) || p[a] == 0 || p[a] == n[a] {
                continue;
            }
            // Only a face a solid covers is sampled; the extrapolation
            // carries it out.
            if open[i * FACE_FLOATS + 4 + a] >= 1.0 {
                continue;
            }
            out[i * FACE_FLOATS + 7] += f64::from(1u32 << a);
            let mut centre: [f64; 3] = std::array::from_fn(|b| min[b] + (p[b] as f64 + 0.5) * h);
            centre[a] = min[a] + p[a] as f64 * h;
            let (row, clear) = closest(solids, poses, centre);
            margin = margin.min(clear);
            if let Some(row) = row {
                let pose = &poses[row];
                let r: [f64; 3] = std::array::from_fn(|b| centre[b] - f64::from(pose.position[b]));
                let (v, w) = (pose.linear.map(f64::from), pose.angular.map(f64::from));
                let spin = [
                    w[1] * r[2] - w[2] * r[1],
                    w[2] * r[0] - w[0] * r[2],
                    w[0] * r[1] - w[1] * r[0],
                ];
                out[i * FACE_FLOATS + a] = v[a] + spin[a];
                out[i * FACE_FLOATS + 3] += (row + 1) as f64 * 256_f64.powi(a as i32);
                moved += 1;
            }
            let (b, c) = match a {
                0 => (1, 2),
                1 => (2, 0),
                _ => (1, 0),
            };
            let mut friction = 0.0;
            for k in 0..4 {
                let mut q = p;
                q[b] += k & 1;
                q[c] += (k >> 1) & 1;
                let (row, clear) = closest(
                    solids,
                    poses,
                    std::array::from_fn(|d| min[d] + q[d] as f64 * h),
                );
                margin = margin.min(clear);
                if let Some(row) = row {
                    friction += f64::from(solids.bodies[row].linear_velocity[3]);
                }
            }
            out[i * FACE_FLOATS + 4 + a] = 0.25 * friction;
        }
    }
    (out, margin, moved)
}

/// The bodies of `solids` as a second tick's rows behind a first tick's that
/// would pose every body differently, a reaction so far, and support points
/// for both ticks' rows (the second tick's dynamic body on a floor's four
/// corners and against a slanted wall; the first tick's under a ceiling):
/// rows, reaction, supports, and the step's params.
pub(super) fn two_tick_bodies(solids: &Solids) -> (Vec<LiquidBody>, Vec<f32>, Vec<BodySupports>, StepParams) {
    let count = solids.bodies.len();
    let reaction: Vec<f32> = random_values(8 * count, 0x5fb)
        .iter()
        .map(|v| v * 0.4)
        .collect();
    let mut rows = solids.bodies.clone();
    for body in &mut rows {
        body.linear_velocity[0] += 5.0;
        body.position_inv_mass[1] -= 0.2;
    }
    rows.extend(solids.bodies.iter().copied());
    let point = |lever, normal, patch, patch_lever| SupportPoint {
        lever,
        normal,
        friction: 0.6,
        patch,
        patch_lever,
        ..SupportPoint::default()
    };
    let mut contacts = vec![[[0.0f32; 4]; SUPPORT_VEC4S]; rows.len()];
    let ceiling = [0.0, 0.15, 0.0];
    contacts[1] = pack_supports(&[point(ceiling, [0.0, -1.0, 0.0], 0, ceiling)]);
    let floor = [
        [0.1, -0.15, 0.1],
        [-0.1, -0.15, 0.1],
        [-0.1, -0.15, -0.1],
        [0.1, -0.15, -0.1],
    ];
    let mut held: Vec<SupportPoint> = floor
        .iter()
        .map(|&lever| point(lever, [0.0, 1.0, 0.0], 0, [0.0, -0.15, 0.0]))
        .collect();
    let wall = [0.09, 0.0, -0.12];
    held.push(point(wall, [-0.6, 0.0, 0.8], 1, wall));
    contacts[count + 1] = pack_supports(&held);
    let step = StepParams {
        body_count: count as i32,
        rows: rows.len() as i32,
        tick_seconds: SOLID_TICK,
        shapes_len: solids.shapes.len() as u32,
        coupled_h: solid_h(),
        ..solid_lattice()
    };
    (rows, reaction, contacts, step)
}

/// I18: pose_bodies poses each body of the
/// tick as the CPU law does: the dynamic body by `coupled_state_at` with a
/// floor and a slanted wall cutting its predicted fall, the prescribed ones
/// at their own velocity, every other field kept; and each body's mobility
/// as `constrained_mobility` gives it on the supports the law leaves it on.
#[test]
pub(super) fn gpu_flip_pose_bodies_matches_coupled_motion() {
    let solids = solids();
    let (rows, reaction, contacts, step) = two_tick_bodies(&solids);
    let count = solids.bodies.len();
    let mut pass = Pass::new();
    pass.bind(14, &rows)
        .bind(21, &reaction)
        .bind(49, &contacts)
        .bind(50, &vec![[0.0f32; 4]; 6 * count]);
    let got: Vec<LiquidBody> = pass.run("pose_bodies", &step, 48, count, count);
    let mobility: Vec<[f32; 4]> = pass.bound(50, 6 * count);
    let want = posed(&solids, &reaction, &contacts[count..]);
    let unprojected = posed(&solids, &reaction, &vec![[[0.0; 4]; SUPPORT_VEC4S]; count]);
    assert!(
        (want[1].linear[1] - unprojected[1].linear[1]).abs() > 0.1,
        "the floor cuts the dynamic body's predicted fall: {} against {}",
        want[1].linear[1],
        unprojected[1].linear[1]
    );
    for (b, (g, w)) in got.iter().zip(&want).enumerate() {
        let near = |a: &[f32], e: &[f32], what: &str| {
            for (x, y) in a.iter().zip(e) {
                assert!(
                    (x - y).abs() <= 2e-6 * (1.0 + y.abs()),
                    "body {b} {what}: {a:?} against {e:?}"
                );
            }
        };
        near(&g.position_inv_mass[..3], &w.position, "position");
        near(&g.rotation, &w.rotation, "rotation");
        near(&g.linear_velocity[..3], &w.linear, "linear velocity");
        near(&g.angular_velocity[..3], &w.angular, "angular velocity");
        let scale = w.mobility.iter().fold(1.0f32, |a, b| a.max(b.abs()));
        let packed: Vec<f32> = mobility[6 * b..6 * b + 6]
            .iter()
            .flatten()
            .copied()
            .collect();
        for (k, (x, y)) in packed.iter().zip(&w.mobility).enumerate() {
            assert!(
                (x - y).abs() <= 1e-5 * scale,
                "body {b} mobility {k}: {x} against {y}"
            );
        }
        let body = &solids.bodies[b];
        assert_eq!(
            (
                g.position_inv_mass[3],
                g.linear_velocity[3],
                g.angular_velocity[3],
                g.inv_inertia_x,
                g.inv_inertia_y,
                g.inv_inertia_z,
                g.accel_shape
            ),
            (
                body.position_inv_mass[3],
                body.linear_velocity[3],
                body.angular_velocity[3],
                body.inv_inertia_x,
                body.inv_inertia_y,
                body.inv_inertia_z,
                body.accel_shape
            ),
            "body {b} keeps its other fields"
        );
    }
}

/// The step's solid face velocity against the rigid velocity, corner
/// friction and owner codes computed here, on random cut faces under two
/// overlapping turning bodies, one dynamic with a reaction so far and two
/// contact normals, posed by pose_bodies. The bodies are a second tick's
/// rows, behind a first tick's that would move every face differently.
#[test]
pub(super) fn gpu_flip_solid_face_velocity_matches_cpu() {
    let solids = solids();
    let open = random_open_faces(SOLID_N, 0x5fa, 0);
    let count = solids.bodies.len();
    let (rows, reaction, contacts, step) = two_tick_bodies(&solids);
    let mut pass = Pass::new();
    pass.bind(10, &open)
        .bind(14, &rows)
        .bind(15, &solids.shapes)
        .bind(16, &solids.atlas)
        .bind(21, &reaction)
        .bind(49, &contacts);
    pass.bind(50, &vec![[0.0f32; 4]; 6 * count]);
    let _: Vec<LiquidBody> = pass.run("pose_bodies", &step, 48, count, count);
    let got: Vec<f32> = pass.run(
        "solid_face_velocity",
        &step,
        4,
        face_grid_len(SOLID_N),
        solid_records(),
    );
    let open64: Vec<f64> = open.iter().map(|&v| f64::from(v)).collect();
    let poses = posed(&solids, &reaction, &contacts[count..]);
    let (want, margin, moved) = cpu_solid_face_velocity(&open64, &solids, &poses);
    assert!(
        margin > 1.0e-4,
        "no query sits on a lattice edge or a tie between bodies: {margin}"
    );
    assert!(moved > 10, "the bodies reach cut faces: {moved}");
    let frictions: Vec<f64> = want
        .chunks(FACE_FLOATS)
        .flat_map(|r| r[4..7].to_vec())
        .collect();
    assert!(
        frictions.iter().any(|&f| f > 0.0 && f < 0.4),
        "a face with corners outside every body"
    );
    assert!(
        frictions.iter().any(|&f| f > 0.4),
        "corners reaching the second body"
    );
    let codes: Vec<f64> = want.chunks(FACE_FLOATS).map(|r| r[3]).collect();
    assert!(
        codes.iter().any(|&c| c > 256.0),
        "a record owned on two axes"
    );
    for (record, (g, w)) in got
        .chunks(FACE_FLOATS)
        .zip(want.chunks(FACE_FLOATS))
        .enumerate()
    {
        assert_eq!(f64::from(g[3]), w[3], "owner code of record {record}");
    }
    assert_close(&got, &want, "solid face velocity");
}

/// One layer of the solid velocity's extrapolation in f64, as
/// GridUtils::extrapolateGridWithObserver runs it on each axis's face
/// lattice: border faces are done from the start but never seed; an unknown
/// face beside a known one takes the mean of its done neighbours.
pub(super) fn cpu_solid_extrapolate(records: &[f32], n: [usize; 3]) -> Vec<f64> {
    let m = n.map(|v| v + 1);
    let mut out: Vec<f64> = records.iter().map(|&v| f64::from(v)).collect();
    for i in 0..m.iter().product::<usize>() {
        let p = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
        let mut known = records[i * FACE_FLOATS + 7] as u32;
        let mut code = records[i * FACE_FLOATS + 3] as u32;
        for a in 0..3 {
            let mut top = n.map(|v| v - 1);
            top[a] = n[a];
            if (0..3).any(|b| p[b] > top[b]) {
                continue;
            }
            let border = |q: [usize; 3]| (0..3).any(|b| q[b] == 0 || q[b] == top[b]);
            let is_known = |r: usize| (records[r * FACE_FLOATS + 7] as u32 >> a) & 1 == 1;
            if border(p) || is_known(i) {
                continue;
            }
            let (mut sum, mut count, mut owner, mut seeded) = (0.0, 0.0, 0u32, false);
            for b in 0..3 {
                for d in [-1i64, 1] {
                    let mut q = p;
                    let x = p[b] as i64 + d;
                    if x < 0 || x > top[b] as i64 {
                        continue;
                    }
                    q[b] = x as usize;
                    let r = q[0] + m[0] * (q[1] + m[1] * q[2]);
                    if border(q) || is_known(r) {
                        sum += f64::from(records[r * FACE_FLOATS + a]);
                        count += 1.0;
                    }
                    if !border(q) && is_known(r) {
                        seeded = true;
                        if owner == 0 {
                            owner = (records[r * FACE_FLOATS + 3] as u32 >> (8 * a)) & 255;
                        }
                    }
                }
            }
            if seeded {
                out[i * FACE_FLOATS + a] = sum / count;
                known |= 1 << a;
                code |= owner << (8 * a);
            }
        }
        out[i * FACE_FLOATS + 3] = f64::from(code);
        out[i * FACE_FLOATS + 7] = f64::from(known);
    }
    out
}

/// One layer of the solid extrapolation against the engine's rule in f64,
/// on random velocities, a sparse random known set and random owners.
#[test]
pub(super) fn gpu_flip_solid_extrapolate_matches_cpu() {
    let n = SOLID_N;
    let mut seed = 0x5e7u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    let records: Vec<f32> = random_values(face_grid_len(n), 0x5e6)
        .chunks(FACE_FLOATS)
        .flat_map(|r| {
            let known = (0..3)
                .filter(|_| next() % 5 == 0)
                .fold(0u32, |k, a| k | (1 << a));
            let code = (0..3)
                .filter(|a| (known >> a) & 1 == 1)
                .fold(0u32, |c, a| c | ((1 + next() % 3) << (8 * a)));
            [r[0], r[1], r[2], code as f32, 0.0, 0.0, 0.0, known as f32]
        })
        .collect();
    let got: Vec<f32> = Pass::new().bind(3, &records).run(
        "solid_extrapolate",
        &solid_lattice(),
        4,
        face_grid_len(n),
        solid_records(),
    );
    let want = cpu_solid_extrapolate(&records, n);
    let grown = got
        .chunks(FACE_FLOATS)
        .zip(records.chunks(FACE_FLOATS))
        .filter(|(g, r)| g[7] as u32 != r[7] as u32)
        .count();
    assert!(grown > 5, "the layer reaches unknown faces: {grown}");
    for (record, (g, w)) in got
        .chunks(FACE_FLOATS)
        .zip(want.chunks(FACE_FLOATS))
        .enumerate()
    {
        assert_eq!(f64::from(g[3]), w[3], "owner code of record {record}");
        assert_eq!(f64::from(g[7]), w[7], "known mask of record {record}");
    }
    assert_close(&got, &want, "solid extrapolation");
}

/// The step's solid constraint in f64.
pub(super) fn cpu_constrain(faces: &[f32], open: &[f64], moving: &[f64], n: [usize; 3]) -> Vec<f64> {
    let m = n.map(|v| v + 1);
    let mut out: Vec<f64> = faces.iter().map(|&v| f64::from(v)).collect();
    for i in 0..m.iter().product::<usize>() {
        let p = [i % m[0], (i / m[0]) % m[1], i / (m[0] * m[1])];
        for a in 0..3 {
            if !(0..3).all(|b| b == a || p[b] < n[b]) {
                continue;
            }
            let (v, w) = (i * FACE_FLOATS + a, i * FACE_FLOATS + 4 + a);
            // A box wall is the static domain's closed face.
            if p[a] == 0 || p[a] == n[a] {
                out[v] = 0.0;
                continue;
            }
            let solid = moving[v];
            if open[w] <= 0.0 {
                out[v] = solid;
            } else if open[w] < 1.0 {
                out[v] = moving[w] * solid + (1.0 - moving[w]) * f64::from(faces[v]);
            }
        }
    }
    out
}

#[test]
pub(super) fn gpu_flip_constrain_solid_faces_matches_cpu() {
    let n = SOLID_N;
    let faces = random_values(face_grid_len(n), 0xc51);
    let open = random_open_faces(n, 0xc52, 0);
    let moving: Vec<f32> = random_values(face_grid_len(n), 0xc53)
        .chunks(FACE_FLOATS)
        .flat_map(|r| {
            [
                r[0],
                r[1],
                r[2],
                r[3],
                r[4] + 0.5,
                r[5] + 0.5,
                r[6] + 0.5,
                r[7],
            ]
        })
        .collect();
    let got: Vec<f32> = Pass::new()
        .bind(20, &faces)
        .bind(10, &open)
        .bind(11, &moving)
        .run(
            "constrain_solid_faces",
            &solid_lattice(),
            20,
            face_grid_len(n),
            solid_records(),
        );
    let open64: Vec<f64> = open.iter().map(|&v| f64::from(v)).collect();
    let moving64: Vec<f64> = moving.iter().map(|&v| f64::from(v)).collect();
    let want = cpu_constrain(&faces, &open64, &moving64, n);
    assert!(
        open.contains(&0.0) && open.iter().any(|&w| w > 0.0 && w < 1.0),
        "closed and cut faces"
    );
    assert_close(&got, &want, "constrain solid faces");
}
