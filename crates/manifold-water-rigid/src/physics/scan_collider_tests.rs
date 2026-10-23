//! A small Body dropped on the cliff scan rests on the surface the scan
//! draws (SCENE_REFINEMENT_DESIGN.md section 3, P2d (Scan colliders)): on the
//! exact mesh, which needs every triangle under the body (the Box3D patch in
//! `native/box3d/README.md`), and on the contact mesh the Collider installs.

use std::time::Instant;

use manifold_physics::collision_shape::{CONTACT_TOLERANCE, ColliderMesh, CollisionShape, Solid};
use manifold_physics::{BodyConfig, BodyKind, PhysicsWorld, Seconds, TriangleMesh};

const CLIFF: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/gltf/coastal_cliff_02/coastal_cliff_02_2k.gltf");

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
    let buffers = gltf::import_buffers(&file.document, path.parent(), file.blob.clone()).expect("the cliff scan's buffers load");
    let mut points = Vec::new();
    for mesh in file.document.meshes() {
        for primitive in mesh.primitives() {
            let reader = primitive.reader(|buffer| buffers.get(buffer.index()).map(|data| &data.0[..]));
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
        let n = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
        let up = (n[1] / (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()).abs();
        if best.is_none_or(|(top, _)| y > top) {
            best = Some((y, up));
        }
    }
    best
}

/// Up to `count` drop points on gentle slopes of the scan, spread over it.
fn drop_sites(mesh: &TriangleMesh, count: usize) -> Vec<[f32; 3]> {
    let [min, max] = mesh.vertices.iter().fold([[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]], |[low, high], v| {
        [std::array::from_fn(|axis| low[axis].min(v[axis])), std::array::from_fn(|axis| high[axis].max(v[axis]))]
    });
    let mut sites = Vec::new();
    let steps = 24;
    for i in 1..steps {
        for j in 1..steps {
            let x = min[0] + (max[0] - min[0]) * i as f32 / steps as f32;
            let z = min[2] + (max[2] - min[2]) * j as f32 / steps as f32;
            let Some((y, up)) = surface_at(mesh, x, z) else { continue };
            // The ground under twice the cube's footprint is the same ledge,
            // so the cube settles where it lands instead of sliding off.
            let covered = [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)].iter().all(|(dx, dz)| {
                surface_at(mesh, x + dx * 2.0 * HALF, z + dz * 2.0 * HALF).is_some_and(|(height, _)| (height - y).abs() < HALF)
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
    let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
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

fn rest(drawn: &TriangleMesh, position: [f32; 3], rotation: [f32; 4], speed: f32) -> Rest {
    let local = |v: [f32; 3]| unrotate(rotation, std::array::from_fn(|axis| v[axis] - position[axis]));
    let reach = 2.0 * HALF;
    let near: Vec<[[f32; 3]; 3]> = drawn
        .triangles
        .iter()
        .map(|triangle| corners(drawn, triangle).map(local))
        .filter(|triangle| triangle.iter().any(|v| v.iter().all(|c| c.abs() < reach)))
        .collect();
    let mut sink = 0.0f32;
    for v in near.iter().flatten() {
        if v.iter().all(|c| c.abs() < HALF) {
            sink = sink.max(HALF - v.iter().fold(0.0f32, |m, c| m.max(c.abs())));
        }
    }
    // The cube's surface, sampled, against the nearby drawn triangles.
    let mut gap = f64::INFINITY;
    let n = 12;
    for face in 0..6 {
        let (axis, sign) = (face / 2, if face % 2 == 0 { -1.0 } else { 1.0 });
        for i in 0..=n {
            for j in 0..=n {
                let s = -HALF + 2.0 * HALF * i as f32 / n as f32;
                let t = -HALF + 2.0 * HALF * j as f32 / n as f32;
                let mut p = [0.0f32; 3];
                p[axis] = sign * HALF;
                p[(axis + 1) % 3] = s;
                p[(axis + 2) % 3] = t;
                for triangle in &near {
                    let corners = triangle.map(|v| v.map(f64::from));
                    gap = gap.min(manifold_physics::sdf::distance_squared_to_triangle(p.map(f64::from), &corners).sqrt());
                }
            }
        }
    }
    Rest { sink, gap: if sink > 0.0 { 0.0 } else { gap as f32 }, speed }
}

/// Drop a cube on each site onto `collider`, simulate three seconds, and
/// report how each rests against the drawn surface and the mean step time.
fn drop_cubes(collider: &ColliderMesh, sites: &[[f32; 3]]) -> (Vec<Rest>, f64) {
    let mut world = PhysicsWorld::new([0.0, -9.81, 0.0]).unwrap();
    let ground = Solid::Mesh(collider.clone());
    ground.install(&mut world, [1.0; 3], BodyConfig { kind: BodyKind::Fixed, friction: 1.0, ..BodyConfig::default() }).unwrap();
    let cube = Solid::Hulls(vec![
        (0..8).map(|i| [if i & 1 == 0 { -HALF } else { HALF }, if i & 2 == 0 { -HALF } else { HALF }, if i & 4 == 0 { -HALF } else { HALF }]).collect(),
    ])
    .cook([1.0; 3])
    .unwrap();
    let handles: Vec<_> = sites
        .iter()
        .map(|site| {
            let config = BodyConfig { position: [site[0], site[1] + HALF + 0.2, site[2]], mass: 600.0, friction: 1.0, ..BodyConfig::default() };
            world.add_cooked(&cube, config).unwrap()
        })
        .collect();
    let started = Instant::now();
    let ticks = 180;
    for _ in 0..ticks {
        world.step(Seconds(TICK), 4).unwrap();
    }
    let mean_ms = started.elapsed().as_secs_f64() * 1000.0 / ticks as f64;
    let rests = handles
        .iter()
        .map(|&handle| {
            let pose = world.pose(handle).unwrap();
            let v = world.linear_velocity(handle).unwrap();
            let speed = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            rest(&collider.exact, pose.position, pose.rotation, speed)
        })
        .collect();
    (rests, mean_ms)
}

/// The cube rests on the scan's drawn surface: it sinks no deeper than the
/// contact tolerance plus Box3D's slop, and floats no higher than the
/// tolerance plus its rest offset, on the exact mesh and on the contact mesh.
#[test]
fn a_small_body_rests_on_the_cliff_scan_it_sees() {
    let shape = cliff();
    let started = Instant::now();
    let collider = shape.collider_mesh([1.0; 3]).unwrap();
    let simplify_s = started.elapsed().as_secs_f64();
    let exact = ColliderMesh::unsimplified(collider.exact.clone());
    let sites = drop_sites(&collider.exact, 6);
    assert!(sites.len() >= 4, "the scan has gentle slopes to drop on: {sites:?}");
    let (on_exact, exact_ms) = drop_cubes(&exact, &sites);
    let (on_contact, contact_ms) = drop_cubes(&collider, &sites);
    println!(
        "cliff scan: {} triangles exact, {} contact, measured {:.2} mm from the drawn surface (simplified in {simplify_s:.2} s); \
         step {exact_ms:.3} ms exact, {contact_ms:.3} ms contact",
        collider.exact.triangles.len(),
        collider.contact.triangles.len(),
        collider.error * 1000.0,
    );
    assert!(collider.error <= CONTACT_TOLERANCE, "contact mesh is {} m from the drawn surface", collider.error);
    assert!(collider.contact.triangles.len() < collider.exact.triangles.len());
    for (name, rests) in [("exact", &on_exact), ("contact", &on_contact)] {
        for (site, rest) in sites.iter().zip(rests.iter()) {
            println!("{name} at {site:?}: {rest:?}");
        }
    }
    for (name, rests) in [("exact", &on_exact), ("contact", &on_contact)] {
        for (site, rest) in sites.iter().zip(rests.iter()) {
            assert!(rest.speed < 0.05, "{name}: the cube at {site:?} has not settled: {rest:?}");
            assert!(rest.sink <= CONTACT_TOLERANCE + SLOP, "{name}: the cube at {site:?} sank into the drawn surface: {rest:?}");
            assert!(rest.gap <= CONTACT_TOLERANCE + SLOP + 0.001, "{name}: the cube at {site:?} floats above the drawn surface: {rest:?}");
        }
    }
}
