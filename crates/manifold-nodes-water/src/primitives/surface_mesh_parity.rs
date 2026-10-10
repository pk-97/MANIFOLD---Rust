//! CPU references from FLIP Fluids trianglemesh.cpp and manifold-fluids decode_surface.
//! The reference uses a global incidence list; the f32 replica uses the GPU's
//! four cells around each lattice edge, independently of that list.
use glam::DVec3;
#[cfg(test)]
use glam::Vec3;

pub(super) const CORNERS: [[usize; 3]; 8] = [
    [0, 0, 0],
    [1, 0, 0],
    [1, 0, 1],
    [0, 0, 1],
    [0, 1, 0],
    [1, 1, 0],
    [1, 1, 1],
    [0, 1, 1],
];
pub(super) const EDGES: [(usize, usize); 12] = [
    (0, 1),
    (1, 2),
    (2, 3),
    (3, 0),
    (4, 5),
    (5, 6),
    (6, 7),
    (7, 4),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];

pub struct Fixture {
    pub nodes: usize,
    pub field: Vec<f32>,
    pub points: Vec<DVec3>,
    pub triangles: Vec<[usize; 3]>,
    pub gradients: Vec<DVec3>,
    #[cfg(any(test, feature = "gpu-proofs"))]
    edges: Vec<([usize; 3], usize)>,
    #[cfg(any(test, feature = "gpu-proofs"))]
    cell_triangles: Vec<Vec<[usize; 3]>>,
}

pub(super) fn triangle_table() -> Vec<[i32; 16]> {
    let source = include_str!("../../../manifold-fluids/native/flip_engine/polygonizer3d.cpp");
    let start = source.find("_triTable[256][16] = {").unwrap();
    source[start..]
        .lines()
        .skip(1)
        .filter(|line| line.trim_start().starts_with('{'))
        .take(256)
        .map(|line| {
            line.trim()
                .trim_start_matches('{')
                .trim_end_matches([',', '}', ';', ' '])
                .split(',')
                .map(|v| v.trim().parse().unwrap())
                .collect::<Vec<_>>()
                .try_into()
                .unwrap()
        })
        .collect()
}

pub fn fixture(kind: usize) -> Fixture {
    let n = 16;
    let spheres: Vec<(DVec3, f64)> = match kind {
        0 => vec![
            (DVec3::new(-0.24, 0.0, 0.0), 0.55),
            (DVec3::new(0.24, 0.0, 0.0), 0.55),
        ],
        1 => (-8..=8)
            .flat_map(|z| {
                (-8..=8).map(move |x| {
                    let jitter = f64::from((x * 17 + z * 31 + 997_i32).rem_euclid(23)) / 23.0 - 0.5;
                    (
                        DVec3::new(
                            f64::from(x) * 0.1 + jitter * 0.02,
                            jitter * 0.004,
                            f64::from(z) * 0.1 - jitter * 0.02,
                        ),
                        0.3,
                    )
                })
            })
            .collect(),
        2 => vec![(DVec3::ZERO, 0.6)],
        _ => unreachable!(),
    };
    let pos = |p: [usize; 3]| DVec3::from_array(p.map(|v| -1.2 + v as f64 * 2.4 / (n - 1) as f64));
    let flat = |p: [usize; 3]| p[0] + n * (p[1] + n * p[2]);
    let field: Vec<f32> = (0..n * n * n)
        .map(|i| {
            let p = pos([i % n, (i / n) % n, i / (n * n)]);
            spheres
                .iter()
                .map(|(c, r)| p.distance(*c) - r)
                .fold(3.0 * spheres[0].1, f64::min) as f32
        })
        .collect();
    let mut points = Vec::new();
    let mut gradients = Vec::new();
    let mut edges = Vec::new();
    let mut ids = std::collections::HashMap::new();
    // Compact order is the positive-axis prefix scan, not insertion order.
    for i in 0..n * n * n {
        let p = [i % n, (i / n) % n, i / (n * n)];
        for axis in 0..3 {
            let mut q = p;
            q[axis] += 1;
            if q[axis] >= n || (field[i] < 0.0) == (field[flat(q)] < 0.0) {
                continue;
            }
            let a = f64::from(field[i]);
            let b = f64::from(field[flat(q)]);
            let v = pos(p).lerp(pos(q), a / (a - b));
            // Previous GPU shading: central differences of the sampled raw
            // field, interpolated along the edge (one-sided at the border).
            let gradient = |at: [usize; 3]| {
                DVec3::from_array(std::array::from_fn(|d| {
                    let mut lo = at;
                    let mut hi = at;
                    lo[d] = lo[d].saturating_sub(1);
                    hi[d] = (hi[d] + 1).min(n - 1);
                    f64::from(field[flat(hi)] - field[flat(lo)])
                        / ((hi[d] - lo[d]) as f64 * 2.4 / (n - 1) as f64)
                }))
            };
            ids.insert((p, axis), points.len());
            edges.push((p, axis));
            points.push(v);
            gradients.push(gradient(p).lerp(gradient(q), a / (a - b)).normalize());
        }
    }
    let table = triangle_table();
    let c = n - 1;
    let mut triangles = Vec::new();
    let mut cell_triangles = vec![Vec::new(); c * c * c];
    for (i, cell_tris) in cell_triangles.iter_mut().enumerate() {
        let cell = [i % c, (i / c) % c, i / (c * c)];
        let corners = CORNERS.map(|o| std::array::from_fn(|a| cell[a] + o[a]));
        let case = corners.iter().enumerate().fold(0, |bits, (j, p)| {
            bits | (usize::from(field[flat(*p)] < 0.0) << j)
        });
        for tri in table[case]
            .split(|&e| e < 0)
            .next()
            .unwrap()
            .chunks_exact(3)
        {
            let tri = std::array::from_fn(|k| {
                let (a, b) = EDGES[tri[k] as usize];
                let (a, b) = (corners[a], corners[b]);
                let low = std::array::from_fn(|d| a[d].min(b[d]));
                let axis = (0..3).find(|&d| a[d] != b[d]).unwrap();
                ids[&(low, axis)]
            });
            triangles.push(tri);
            cell_tris.push(tri);
        }
    }
    Fixture {
        nodes: n,
        field,
        points,
        triangles,
        gradients,
    #[cfg(any(test, feature = "gpu-proofs"))]
        edges,
    #[cfg(any(test, feature = "gpu-proofs"))]
        cell_triangles,
    }
}

/// Direct port of _vertexTriangles and _smoothTriangleMesh; repeated meetings
/// are deliberately not deduplicated (including open and non-manifold fans).
pub fn flip_smooth(
    points: &[DVec3],
    triangles: &[[usize; 3]],
    value: f64,
    iterations: usize,
) -> Vec<DVec3> {
    let mut incident = vec![Vec::new(); points.len()];
    for (i, t) in triangles.iter().enumerate() {
        for &v in t {
            incident[v].push(i);
        }
    }
    let mut points = points.to_vec();
    for _ in 0..iterations {
        points = points
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let mut sum = DVec3::ZERO;
                let mut count = 0;
                for &t in &incident[i] {
                    for &v in &triangles[t] {
                        if v != i {
                            sum += points[v];
                            count += 1;
                        }
                    }
                }
                if count == 0 {
                    *p
                } else {
                    *p + value * (sum / count as f64 - *p)
                }
            })
            .collect();
    }
    points
}

pub fn flip_normals(points: &[DVec3], triangles: &[[usize; 3]]) -> Vec<DVec3> {
    let mut normals = vec![DVec3::ZERO; points.len()];
    for &[a, b, c] in triangles {
        let normal = (points[b] - points[a]).cross(points[c] - points[a]);
        for i in [a, b, c] {
            normals[i] += normal;
        }
    }
    normals
        .into_iter()
        .map(|n| {
            if n.length() > f64::from(f32::EPSILON) {
                n.normalize()
            } else {
                DVec3::ZERO
            }
        })
        .collect()
}

impl Fixture {
    // Shader order: lower edge endpoint, transverse axes, around 0..4,
    // triangle table order, then the two other corners in cyclic order.
    #[cfg(test)]
    fn gather(&self, vertex: usize, points: &[Vec3]) -> (Vec3, usize, Vec3) {
        let (a, axis) = self.edges[vertex];
        let c = self.nodes - 1;
        let u = if axis == 0 { 1 } else { 0 };
        let v = if axis == 2 { 1 } else { 2 };
        let mut sum = Vec3::ZERO;
        let mut count = 0;
        let mut normal = Vec3::ZERO;
        for around in 0..4 {
            let mut cell = a;
            let du = around & 1;
            let dv = around >> 1;
            if cell[u] < du || cell[v] < dv {
                continue;
            }
            cell[u] -= du;
            cell[v] -= dv;
            if cell.iter().any(|&p| p >= c) {
                continue;
            }
            for tri in &self.cell_triangles[cell[0] + c * (cell[1] + c * cell[2])] {
                for corner in 0..3 {
                    if tri[corner] != vertex {
                        continue;
                    }
                    let next = points[tri[(corner + 1) % 3]];
                    let previous = points[tri[(corner + 2) % 3]];
                    sum += next;
                    sum += previous;
                    count += 2;
                    normal += (next - points[vertex]).cross(previous - points[vertex]);
                }
            }
        }
        (sum, count, normal)
    }
    #[cfg(test)]
    fn replica(&self, value: f32, iterations: usize) -> (Vec<DVec3>, Vec<DVec3>) {
        let mut points: Vec<Vec3> = self.points.iter().map(|p| p.as_vec3()).collect();
        for _ in 0..iterations {
            points = points
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let (sum, count, _) = self.gather(i, &points);
                    if count == 0 {
                        *p
                    } else {
                        *p + value * (sum / count as f32 - *p)
                    }
                })
                .collect();
        }
        let normals = (0..points.len())
            .map(|i| {
                let (_, _, n) = self.gather(i, &points);
                if n.length() > f32::EPSILON {
                    n.normalize().as_dvec3()
                } else {
                    DVec3::ZERO
                }
            })
            .collect();
        (points.into_iter().map(|p| p.as_dvec3()).collect(), normals)
    }
}

#[test]
fn flip_mesh_smoothing_and_normals_match_three_fixtures() {
    for (kind, name) in [
        "overlapping spheres",
        "jittered particle sheet",
        "lone sphere",
    ]
    .into_iter()
    .enumerate()
    {
        let f = fixture(kind);
        assert_eq!(f.field.len(), f.nodes.pow(3));
        // Keep the proof bounded while covering values outside the card's
        // usual range.  FLIP's native smooth(value, iterations) does not
        // clamp either argument, so an injected value must still reach the
        // same f64/f32 computation (the UI range is not the algorithm).
        for (value, iterations) in [(0.5, 0), (0.5, 1), (0.5, 2), (0.5, 5), (0.5, 11), (11.0, 1)] {
            let expected = flip_smooth(&f.points, &f.triangles, value, iterations);
            let (replica, replica_normals) = f.replica(value as f32, iterations);
            // Extreme extrapolation folds the fan through itself. Its area
            // sum can nearly cancel, amplifying tiny position roundoff in
            // the normalized direction. Check that extra range probe one
            // stage at a time; the requested 0.5 fixtures compare end-to-end.
            let normals = flip_normals(if value > 1.0 { &replica } else { &expected }, &f.triangles);
            for i in 0..expected.len() {
                let tolerance = if value > 1.0 {
                    4e-6 * expected[i].length().max(1.0)
                } else {
                    2e-6
                };
                assert!(
                    expected[i].distance(replica[i]) < tolerance,
                    "{name}: value {value}, iteration {iterations}, vertex {i}"
                );
                assert!(
                    normals[i].distance(replica_normals[i]) < 2e-5,
                    "{name}: value {value}, iteration {iterations}, normal {i}"
                );
            }
            if value != 0.5 || iterations != 2 {
                continue;
            }
            if kind == 1 {
                let selected: Vec<_> = f
                    .points
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.x.abs() < 0.5 && p.z.abs() < 0.5 && p.y > 0.1)
                    .map(|(i, _)| i)
                    .collect();
                assert!(!selected.is_empty());
                let spread = |ns: &[DVec3]| {
                    (selected
                        .iter()
                        .map(|&i| ns[i].y.clamp(-1.0, 1.0).acos().to_degrees().powi(2))
                        .sum::<f64>()
                        / selected.len() as f64)
                        .sqrt()
                };
                let (before, after) = (spread(&f.gradients), spread(&replica_normals));
                eprintln!(
                    "{name}: normal RMS angle {before:.4} -> {after:.4} degrees ({} interior vertices)",
                    selected.len()
                );
                assert!(
                    after < 3.0 && after < before,
                    "sheet must approach flat: {before} -> {after}"
                );
            }
            if kind == 2 {
                let radius =
                    |ps: &[DVec3]| ps.iter().map(|p| p.length()).sum::<f64>() / ps.len() as f64;
                let (before, reference, got) =
                    (radius(&f.points), radius(&expected), radius(&replica));
                eprintln!("{name}: mean radius {before:.7} -> {got:.7}; FLIP {reference:.7}");
                assert!(
                    got <= before && (got - reference).abs() < 2e-6,
                    "shrink must equal FLIP"
                );
            }
        }
    }
}

#[test]
fn normals_are_area_weighted_and_degenerate_fans_are_zero() {
    let points = [DVec3::ZERO, DVec3::X, DVec3::Y, DVec3::Z * 2.0];
    let ns = flip_normals(&points, &[[0, 1, 2], [0, 3, 1]]);
    assert!(ns[0].distance(DVec3::new(0.0, 2.0, 1.0).normalize()) < 1e-14);
    assert_eq!(flip_normals(&points, &[[0, 0, 0]])[0], DVec3::ZERO);
}

#[cfg(feature = "gpu-proofs")]
impl Fixture {
    pub fn triangle_scan(&self) -> Vec<u32> {
        let mut total = 0;
        self.cell_triangles
            .iter()
            .map(|t| {
                total += t.len() as u32;
                total
            })
            .collect()
    }
    pub fn edge_scan(&self) -> Vec<u32> {
        let mut counts = vec![0; self.nodes.pow(3)];
        for (p, _) in &self.edges {
            counts[p[0] + self.nodes * (p[1] + self.nodes * p[2])] += 1;
        }
        let mut total = 0;
        counts
            .into_iter()
            .map(|n| {
                total += n;
                total
            })
            .collect()
    }
}

/// ParticleMesher::_computeScalarField negates the distance before
/// ScalarField::getScalarFieldValue clamps positive-inside solid samples.
/// Polygonizer3d sets case bits with > 0; our negative-inside field uses < 0.
#[test]
fn flip_field_solid_sign_and_zero_cases_match_negative_inside_convention() {
    for distance in [-3.0_f64, -0.25, -0.0, 0.0, 0.25, 3.0] {
        for solid in [false, true] {
            let mut native = -distance;
            if solid && native > 0.0 {
                native = 0.0;
            }
            let gpu = if solid { distance.max(0.0) } else { distance };
            assert_eq!(gpu, -native);
            assert_eq!(gpu < 0.0, native > 0.0);
        }
    }
    // Mixed solid/fluid corner cases retain exactly the same MC case,
    // including zeros: a sign flip of the solid clamp would fail these.
    for solid_mask in 0..256 {
        let distances = [-0.75_f64, 0.5, 0.0, -0.0, -0.125, 0.25, -1.0, 3.0];
        let mut native_case = 0;
        let mut gpu_case = 0;
        for (corner, distance) in distances.into_iter().enumerate() {
            let solid = solid_mask & (1 << corner) != 0;
            let native = if solid { (-distance).min(0.0) } else { -distance };
            let gpu = if solid { distance.max(0.0) } else { distance };
            native_case |= usize::from(native > 0.0) << corner;
            gpu_case |= usize::from(gpu < 0.0) << corner;
        }
        assert_eq!(native_case, gpu_case);
    }
}
