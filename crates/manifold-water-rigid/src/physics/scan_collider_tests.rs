//! A small Body dropped on the cliff scan rests on the surface the scan
//! draws. The
//! Collider is the exact mesh, which needs every triangle under the body (the
//! Box3D patch in `native/box3d/README.md`), and at the import fit scale it
//! leaves out the slivers Box3D drops instead of being refused (BUG-za99y
//! (fit-scale scan Collider refused)).

use manifold_physics::collision_shape::{CollisionShape, Solid};
use manifold_physics::{BodyConfig, BodyKind, PhysicsWorld, Seconds, TriangleMesh};

const CLIFF: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/gltf/coastal_cliff_02/coastal_cliff_02_2k.gltf"
);

/// Half the side of the dropped cube, metres: small beside the scan's 30 m,
/// big enough to cover about a thousand of its triangles.
const HALF: f32 = 0.25;

/// Box3D's linear slop (`B3_LINEAR_SLOP`): the overlap a resting contact is
/// allowed, and the gap a body keeps above a mesh (`B3_MESH_REST_OFFSET`).
const SLOP: f32 = 0.005;

const TICK: f64 = 1.0 / 60.0;

/// The scan in its file's metres (about 41 × 10 × 9 m), as the import hands
/// it to the collision shape: an unindexed triangle list, welded there.
fn cliff() -> CollisionShape {
    let path = std::path::Path::new(CLIFF);
    let file = gltf::Gltf::open(path).expect("the cliff scan fixture opens");
    let buffers = gltf::import_buffers(&file.document, path.parent(), file.blob.clone())
        .expect("the cliff scan's buffers load");
    let mut points = Vec::new();
    for mesh in file.document.meshes() {
        for primitive in mesh.primitives() {
            let reader =
                primitive.reader(|buffer| buffers.get(buffer.index()).map(|data| &data.0[..]));
            let positions: Vec<[f32; 3]> = reader.read_positions().expect("positions").collect();
            let indices: Vec<u32> = reader.read_indices().expect("indices").into_u32().collect();
            points.extend(indices.iter().map(|&index| positions[index as usize]));
        }
    }
    CollisionShape::from_triangle_lists([points.as_slice()]).expect("the cliff scan is a mesh")
}

fn corners(mesh: &TriangleMesh, triangle: &[u32; 3]) -> [[f32; 3]; 3] {
    triangle.map(|index| mesh.vertices[index as usize])
}

/// The highest point of `mesh` straight below `(x, z)` and its upward
/// normal component there.
fn surface_at(mesh: &TriangleMesh, x: f32, z: f32) -> Option<(f32, f32)> {
    let mut best: Option<(f32, f32)> = None;
    for triangle in &mesh.triangles {
        let [a, b, c] = corners(mesh, triangle);
        let det = (b[0] - a[0]) * (c[2] - a[2]) - (c[0] - a[0]) * (b[2] - a[2]);
        if det.abs() < 1e-12 {
            continue;
        }
        let u = ((x - a[0]) * (c[2] - a[2]) - (c[0] - a[0]) * (z - a[2])) / det;
        let v = ((b[0] - a[0]) * (z - a[2]) - (x - a[0]) * (b[2] - a[2])) / det;
        if u < 0.0 || v < 0.0 || u + v > 1.0 {
            continue;
        }
        let y = a[1] + u * (b[1] - a[1]) + v * (c[1] - a[1]);
        let e1 = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let e2 = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let n = [
            e1[1] * e2[2] - e1[2] * e2[1],
            e1[2] * e2[0] - e1[0] * e2[2],
            e1[0] * e2[1] - e1[1] * e2[0],
        ];
        let up = (n[1] / (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()).abs();
        if best.is_none_or(|(top, _)| y > top) {
            best = Some((y, up));
        }
    }
    best
}

/// Up to `count` drop points on gentle slopes of the scan, spread over it.
fn drop_sites(mesh: &TriangleMesh, count: usize) -> Vec<[f32; 3]> {
    let [min, max] = mesh.vertices.iter().fold(
        [[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]],
        |[low, high], v| {
            [
                std::array::from_fn(|axis| low[axis].min(v[axis])),
                std::array::from_fn(|axis| high[axis].max(v[axis])),
            ]
        },
    );
    let mut sites = Vec::new();
    let steps = 24;
    for i in 1..steps {
        for j in 1..steps {
            let x = min[0] + (max[0] - min[0]) * i as f32 / steps as f32;
            let z = min[2] + (max[2] - min[2]) * j as f32 / steps as f32;
            let Some((y, up)) = surface_at(mesh, x, z) else {
                continue;
            };
            // The ground under twice the cube's footprint is the same ledge,
            // so the cube settles where it lands instead of sliding off.
            let covered = [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)]
                .iter()
                .all(|(dx, dz)| {
                    surface_at(mesh, x + dx * 2.0 * HALF, z + dz * 2.0 * HALF)
                        .is_some_and(|(height, _)| (height - y).abs() < HALF)
                });
            if up > 0.95 && covered {
                sites.push([x, y, z]);
            }
        }
    }
    let stride = (sites.len() / count).max(1);
    sites.into_iter().step_by(stride).take(count).collect()
}

/// `v` turned by the inverse of unit quaternion `q` (`[x, y, z, w]`).
fn unrotate(q: [f32; 4], v: [f32; 3]) -> [f32; 3] {
    let [x, y, z, w] = q;
    let u = [-x, -y, -z];
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let t = cross(u, v).map(|c| 2.0 * c);
    let ut = cross(u, t);
    std::array::from_fn(|axis| v[axis] + w * t[axis] + ut[axis])
}

/// How a resting cube sits on the drawn surface.
#[derive(Debug)]
struct Rest {
    /// How deep the drawn surface reaches into the cube, metres.
    sink: f32,
    /// How far the cube floats above the drawn surface, metres.
    gap: f32,
    speed: f32,
}

/// How a cube of half side `half` at `position` rests on `drawn`.
fn rest(
    drawn: &TriangleMesh,
    half: f32,
    position: [f32; 3],
    rotation: [f32; 4],
    speed: f32,
) -> Rest {
    let local = |v: [f32; 3]| {
        unrotate(
            rotation,
            std::array::from_fn(|axis| v[axis] - position[axis]),
        )
    };
    let reach = 2.0 * half;
    let near: Vec<[[f32; 3]; 3]> = drawn
        .triangles
        .iter()
        .map(|triangle| corners(drawn, triangle).map(local))
        .filter(|triangle| triangle.iter().any(|v| v.iter().all(|c| c.abs() < reach)))
        .collect();
    let mut sink = 0.0f32;
    for v in near.iter().flatten() {
        if v.iter().all(|c| c.abs() < half) {
            sink = sink.max(half - v.iter().fold(0.0f32, |m, c| m.max(c.abs())));
        }
    }
    // The cube's surface, sampled, against the nearby drawn triangles.
    let mut gap = f64::INFINITY;
    let n = 12;
    for face in 0..6 {
        let (axis, sign) = (face / 2, if face % 2 == 0 { -1.0 } else { 1.0 });
        for i in 0..=n {
            for j in 0..=n {
                let s = -half + 2.0 * half * i as f32 / n as f32;
                let t = -half + 2.0 * half * j as f32 / n as f32;
                let mut p = [0.0f32; 3];
                p[axis] = sign * half;
                p[(axis + 1) % 3] = s;
                p[(axis + 2) % 3] = t;
                for triangle in &near {
                    let corners = triangle.map(|v| v.map(f64::from));
                    gap = gap.min(
                        manifold_physics::sdf::distance_squared_to_triangle(
                            p.map(f64::from),
                            &corners,
                        )
                        .sqrt(),
                    );
                }
            }
        }
    }
    Rest {
        sink,
        gap: if sink > 0.0 { 0.0 } else { gap as f32 },
        speed,
    }
}

/// Install `shape` as a Fixed Collider at `scale`, drop a cube scaled with it
/// on each site (unscaled), simulate three seconds, and report how each rests
/// against the drawn surface at that scale.
fn drop_cubes(shape: &CollisionShape, scale: f32, sites: &[[f32; 3]]) -> Vec<Rest> {
    let exact = shape.exact_mesh();
    let drawn = TriangleMesh {
        vertices: exact
            .vertices
            .iter()
            .map(|v| v.map(|c| c * scale))
            .collect(),
        triangles: exact.triangles.clone(),
    };
    let mut world = PhysicsWorld::new([0.0, -9.81, 0.0]).unwrap();
    Solid::Mesh(exact)
        .install(
            &mut world,
            [scale; 3],
            BodyConfig {
                kind: BodyKind::Fixed,
                friction: 1.0,
                ..BodyConfig::default()
            },
        )
        .expect("the scan installs as a Collider at this scale");
    let half = HALF * scale;
    let cube = Solid::Hulls(vec![
        (0..8)
            .map(|i| {
                [
                    if i & 1 == 0 { -half } else { half },
                    if i & 2 == 0 { -half } else { half },
                    if i & 4 == 0 { -half } else { half },
                ]
            })
            .collect(),
    ])
    .cook([1.0; 3])
    .unwrap();
    let handles: Vec<_> = sites
        .iter()
        .map(|site| {
            let config = BodyConfig {
                position: [
                    site[0] * scale,
                    (site[1] + HALF + 0.2) * scale,
                    site[2] * scale,
                ],
                mass: 600.0 * scale.powi(3),
                friction: 1.0,
                ..BodyConfig::default()
            };
            world.add_cooked(&cube, config).unwrap()
        })
        .collect();
    for _ in 0..180 {
        world.step(Seconds(TICK), 4).unwrap();
    }
    handles
        .iter()
        .map(|&handle| {
            let pose = world.pose(handle).unwrap();
            let v = world.linear_velocity(handle).unwrap();
            let speed = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            rest(&drawn, half, pose.position, pose.rotation, speed)
        })
        .collect()
}

/// The cliff's scale when the import fits it to the stage (about 2 m wide),
/// where many of its triangles are slivers under Box3D's 0.25 mm² minimum.
const FIT_SCALE: f32 = 0.049;

/// The cube rests on the scan's exact surface at its file size and at its
/// import fit size: settled, sunk no deeper than Box3D's slop, floating no
/// higher than its rest offset plus 1 mm.
#[test]
fn a_small_body_rests_on_the_cliff_scan_it_sees() {
    let shape = cliff();
    let sites = drop_sites(&shape.exact_mesh(), 6);
    assert!(
        sites.len() >= 4,
        "the scan has gentle slopes to drop on: {sites:?}"
    );
    let mut failures = Vec::new();
    for scale in [1.0, FIT_SCALE] {
        let rests = drop_cubes(&shape, scale, &sites);
        for (site, rest) in sites.iter().zip(&rests) {
            println!("scale {scale} at {site:?}: {rest:?}");
            let settled = rest.speed < 0.05 * scale;
            let sunk = rest.sink > SLOP;
            let floats = rest.gap > SLOP + 0.001;
            if !settled || sunk || floats {
                failures.push(format!("scale {scale} at {site:?}: {rest:?}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "cubes not resting on the drawn surface:\n{}",
        failures.join("\n")
    );
}
