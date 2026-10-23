//! A small Body dropped on the cliff scan rests on the surface the scan
//! draws (SCENE_REFINEMENT_DESIGN.md section 3, P2d (Scan colliders)). The
//! Collider is the exact mesh, which needs every triangle under the body (the
//! Box3D patch in `native/box3d/README.md`), and at the import fit scale it
//! leaves out the slivers Box3D drops instead of being refused (BUG-za99y
//! (fit-scale scan Collider refused)).

use manifold_physics::collision_shape::{CollisionShape, Solid};
use manifold_physics::{BodyConfig, BodyImpulse, BodyKind, PhysicsWorld, Seconds, TriangleMesh};

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
fn drop_sites(mesh: &TriangleMesh, count: usize, half: f32) -> Vec<[f32; 3]> {
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
                surface_at(mesh, x + dx * 2.0 * half, z + dz * 2.0 * half).is_some_and(|(height, _)| (height - y).abs() < half)
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

/// How a cube of half side `half` at `position` rests on `drawn`.
fn rest(drawn: &TriangleMesh, half: f32, position: [f32; 3], rotation: [f32; 4], speed: f32) -> Rest {
    let local = |v: [f32; 3]| unrotate(rotation, std::array::from_fn(|axis| v[axis] - position[axis]));
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
                    gap = gap.min(manifold_physics::sdf::distance_squared_to_triangle(p.map(f64::from), &corners).sqrt());
                }
            }
        }
    }
    Rest { sink, gap: if sink > 0.0 { 0.0 } else { gap as f32 }, speed }
}

/// Install `shape` as a Fixed Collider at `scale`, drop a cube scaled with it
/// on each site (unscaled), simulate three seconds, and report how each rests
/// against the drawn surface at that scale.
fn drop_cubes(shape: &CollisionShape, scale: f32, sites: &[[f32; 3]]) -> Vec<Rest> {
    let exact = shape.exact_mesh();
    let drawn = TriangleMesh { vertices: exact.vertices.iter().map(|v| v.map(|c| c * scale)).collect(), triangles: exact.triangles.clone() };
    let mut world = PhysicsWorld::new([0.0, -9.81, 0.0]).unwrap();
    Solid::Mesh(exact)
        .install(&mut world, [scale; 3], BodyConfig { kind: BodyKind::Fixed, friction: 1.0, ..BodyConfig::default() })
        .expect("the scan installs as a Collider at this scale");
    let half = HALF * scale;
    let cube = Solid::Hulls(vec![
        (0..8).map(|i| [if i & 1 == 0 { -half } else { half }, if i & 2 == 0 { -half } else { half }, if i & 4 == 0 { -half } else { half }]).collect(),
    ])
    .cook([1.0; 3])
    .unwrap();
    let handles: Vec<_> = sites
        .iter()
        .map(|site| {
            let config = BodyConfig {
                position: [site[0] * scale, (site[1] + HALF + 0.2) * scale, site[2] * scale],
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
    let sites = drop_sites(&shape.exact_mesh(), 6, HALF);
    assert!(sites.len() >= 4, "the scan has gentle slopes to drop on: {sites:?}");
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
    assert!(failures.is_empty(), "cubes not resting on the drawn surface:\n{}", failures.join("\n"));
}

/// What one resting step costs with a body of half side `half` on the scan
/// at `scale`.
struct StepCost {
    ms_per_step: f64,
    /// Drawn triangles whose bounds reach within Box3D's speculative distance
    /// (four slops) of the body's bounds: the ones its contact collects.
    triangles_under: usize,
    /// Touching contact patches against the scan (a normal off the floor's
    /// straight up).
    scan_contacts: usize,
    speed: f32,
}

/// Drop a cube of half side `half` (metres, at the scene's scale) on the scan
/// at `scale` from above `site` (the scan's file metres), let it settle for
/// ten seconds, then time Box3D steps while it rests, kept awake as a body
/// in water is (`PhysicsWorld::queue_reaction_over_step`). A floor sits at
/// `floor` of the way up the scan, as a scene has one, so a body wider than
/// the scan rests across both instead of tipping off it.
fn resting_step_cost(shape: &CollisionShape, scale: f32, half: f32, site: [f32; 3], floor: f32) -> StepCost {
    let exact = shape.exact_mesh();
    let drawn = TriangleMesh { vertices: exact.vertices.iter().map(|v| v.map(|c| c * scale)).collect(), triangles: exact.triangles.clone() };
    let mut world = PhysicsWorld::new([0.0, -9.81, 0.0]).unwrap();
    Solid::Mesh(exact)
        .install(&mut world, [scale; 3], BodyConfig { kind: BodyKind::Fixed, friction: 1.0, ..BodyConfig::default() })
        .expect("the scan installs as a Collider at this scale");
    let (bottom, top) = drawn.vertices.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(low, high), v| (low.min(v[1]), high.max(v[1])));
    let floor_top = bottom + floor * (top - bottom);
    let ground = Solid::Hulls(vec![
        (0..8).map(|i| [if i & 1 == 0 { -100.0 } else { 100.0 }, if i & 2 == 0 { floor_top - 1.0 } else { floor_top }, if i & 4 == 0 { -100.0 } else { 100.0 }]).collect(),
    ]);
    ground.install(&mut world, [1.0; 3], BodyConfig { kind: BodyKind::Fixed, friction: 1.0, ..BodyConfig::default() }).unwrap();
    let cube = Solid::Hulls(vec![
        (0..8).map(|i| [if i & 1 == 0 { -half } else { half }, if i & 2 == 0 { -half } else { half }, if i & 4 == 0 { -half } else { half }]).collect(),
    ])
    .cook([1.0; 3])
    .unwrap();
    let config = BodyConfig {
        position: [site[0] * scale, site[1] * scale + half + 0.05, site[2] * scale],
        mass: 1000.0 * (2.0 * half).powi(3),
        friction: 1.0,
        ..BodyConfig::default()
    };
    let handle = world.add_cooked(&cube, config).unwrap();
    for _ in 0..600 {
        world.step(Seconds(TICK), 4).unwrap();
    }
    let steps = 60;
    let start = std::time::Instant::now();
    // A zero wrench leaves a sleeping body asleep; a micronewton wakes it.
    let still = [BodyImpulse { body: handle, linear: [0.0, 1e-6 * TICK as f32, 0.0], angular: [0.0; 3] }];
    for _ in 0..steps {
        world.queue_reaction_over_step(&still, Seconds(TICK)).unwrap();
        world.step(Seconds(TICK), 4).unwrap();
    }
    let ms_per_step = start.elapsed().as_secs_f64() * 1000.0 / f64::from(steps);
    let pose = world.pose(handle).unwrap();
    let v = world.linear_velocity(handle).unwrap();
    // The cube's world bounds: its corners turned by the pose.
    let conjugate = [-pose.rotation[0], -pose.rotation[1], -pose.rotation[2], pose.rotation[3]];
    let reach = 4.0 * SLOP;
    let [low, high] = (0..8).fold([[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]], |[low, high], i| {
        let corner = unrotate(conjugate, [if i & 1 == 0 { -half } else { half }, if i & 2 == 0 { -half } else { half }, if i & 4 == 0 { -half } else { half }]);
        let p: [f32; 3] = std::array::from_fn(|axis| corner[axis] + pose.position[axis]);
        [std::array::from_fn(|axis| low[axis].min(p[axis] - reach)), std::array::from_fn(|axis| high[axis].max(p[axis] + reach))]
    });
    let triangles_under = drawn
        .triangles
        .iter()
        .filter(|triangle| {
            let c = corners(&drawn, triangle);
            (0..3).all(|axis| c.iter().any(|v| v[axis] >= low[axis]) && c.iter().any(|v| v[axis] <= high[axis]))
        })
        .count();
    let mut supports = [manifold_physics::coupled_motion::SupportPoint::default(); manifold_physics::coupled_motion::MAX_SUPPORT_POINTS];
    let count = world.support_points(handle, &mut supports).unwrap();
    let scan_contacts = supports[..count.kept].iter().filter(|support| support.normal[1] < 0.9999).count();
    StepCost { ms_per_step, triangles_under, scan_contacts, speed: (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt() }
}

/// What a body costs per step resting on the dense scan, kept as a number:
/// the base Cube on the scan scaled down by hand, a 10 m box and the base
/// Cube on the scan at its true size. Run with `--no-capture` to read it.
/// The timing is not asserted (it depends on the machine); that each body
/// rests on the scan is. Each body tries the scan's middle, then gentle
/// ledges, and is timed at the first place it comes to rest on the scan.
#[test]
fn a_body_resting_on_the_cliff_scan_costs_this_per_step() {
    let shape = cliff();
    let mesh = shape.exact_mesh();
    let [min, max] = mesh.vertices.iter().fold([[f32::INFINITY; 3], [f32::NEG_INFINITY; 3]], |[low, high], v| {
        [std::array::from_fn(|axis| low[axis].min(v[axis])), std::array::from_fn(|axis| high[axis].max(v[axis]))]
    });
    let (x, z) = (0.5 * (min[0] + max[0]), 0.5 * (min[2] + max[2]));
    let (y, _) = surface_at(&mesh, x, z).expect("the scan covers its own middle");
    let sites: Vec<[f32; 3]> = std::iter::once([x, y, z]).chain(drop_sites(&mesh, 24, HALF)).collect();
    let base_cube = 0.575;
    for (name, scale, half, floor) in [
        ("1.15 m cube on the cliff at 0.049", FIT_SCALE, base_cube, 0.5),
        ("10 m box on the cliff at 1", 1.0, 5.0, 0.5),
        ("1.15 m cube on the cliff at 1", 1.0, base_cube, 0.0),
    ] {
        let cost = sites
            .iter()
            .map(|&site| resting_step_cost(&shape, scale, half, site, floor))
            .find(|cost| cost.scan_contacts > 0 && cost.speed < 0.05)
            .unwrap_or_else(|| panic!("{name}: the body comes to rest on the scan somewhere"));
        println!(
            "{name}: {:.3} ms per step, {} triangles under it, {} contacts on the scan, {:.1e} m/s",
            cost.ms_per_step, cost.triangles_under, cost.scan_contacts, cost.speed
        );
    }
}
