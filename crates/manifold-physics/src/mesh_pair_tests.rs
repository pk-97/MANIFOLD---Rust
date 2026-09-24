use super::*;

const DT: Seconds = Seconds(1.0 / 960.0);
const SUBSTEPS: u32 = 4;
const SURFACE_OFFSET: f32 = 0.005;

fn cube_mesh(half_extent: f32) -> (Vec<[f32; 3]>, Vec<[u32; 3]>) {
    let vertices = vec![
        [-half_extent, -half_extent, -half_extent],
        [half_extent, -half_extent, -half_extent],
        [half_extent, half_extent, -half_extent],
        [-half_extent, half_extent, -half_extent],
        [-half_extent, -half_extent, half_extent],
        [half_extent, -half_extent, half_extent],
        [half_extent, half_extent, half_extent],
        [-half_extent, half_extent, half_extent],
    ];
    let triangles = vec![
        [0, 2, 1],
        [0, 3, 2],
        [4, 5, 6],
        [4, 6, 7],
        [0, 1, 5],
        [0, 5, 4],
        [1, 2, 6],
        [1, 6, 5],
        [2, 3, 7],
        [2, 7, 6],
        [3, 0, 4],
        [3, 4, 7],
    ];
    (vertices, triangles)
}

fn hull_box(half_extents: [f32; 3]) -> Vec<[f32; 3]> {
    let [x, y, z] = half_extents;
    vec![
        [-x, -y, -z],
        [x, -y, -z],
        [x, y, -z],
        [-x, y, -z],
        [-x, -y, z],
        [x, -y, z],
        [x, y, z],
        [-x, y, z],
    ]
}

fn step_for_seconds(world: &mut PhysicsWorld, seconds: usize) {
    for _ in 0..seconds * 960 {
        world.step(DT, SUBSTEPS).unwrap();
    }
}

fn dynamic_mesh_config(position: [f32; 3]) -> BodyConfig {
    BodyConfig {
        position,
        friction: 0.8,
        restitution: 0.0,
        ..BodyConfig::default()
    }
}

fn fixed_mesh_config() -> BodyConfig {
    BodyConfig {
        kind: BodyKind::Fixed,
        mass: 0.0,
        friction: 0.8,
        ..BodyConfig::default()
    }
}

#[test]
fn dynamic_mesh_cubes_stack_on_hull_floor_without_crossing() {
    let mut world = PhysicsWorld::new([0.0, -9.8, 0.0]).unwrap();
    world
        .add_hull(
            &hull_box([2.0, 0.05, 2.0]),
            BodyConfig {
                kind: BodyKind::Fixed,
                mass: 0.0,
                position: [0.0, -0.05, 0.0],
                friction: 0.8,
                ..BodyConfig::default()
            },
        )
        .unwrap();

    let (vertices, triangles) = cube_mesh(0.5);
    assert_eq!(triangles.len(), 12);
    let lower = world
        .add_triangle_mesh(&vertices, &triangles, dynamic_mesh_config([0.0, 1.2, 0.0]))
        .unwrap();
    let upper = world
        .add_triangle_mesh(&vertices, &triangles, dynamic_mesh_config([0.0, 2.35, 0.0]))
        .unwrap();

    step_for_seconds(&mut world, 3);

    let lower_pose = world.pose(lower).unwrap();
    let upper_pose = world.pose(upper).unwrap();
    let lower_velocity = world.linear_velocity(lower).unwrap();
    let upper_velocity = world.linear_velocity(upper).unwrap();
    assert!(
        lower_pose.position[1] >= 0.5 - 0.001,
        "lower mesh crossed the floor: {lower_pose:?}"
    );
    assert!(
        upper_pose.position[1] >= lower_pose.position[1] + 1.0 - 0.001,
        "stacked meshes crossed or overlapped: lower={lower_pose:?}, upper={upper_pose:?}"
    );
    assert!(
        (lower_pose.position[1] - (0.5 + SURFACE_OFFSET)).abs() < 0.03,
        "lower mesh did not settle at the floor: {lower_pose:?}"
    );
    assert!(
        (upper_pose.position[1] - (1.5 + SURFACE_OFFSET)).abs() < 0.03,
        "upper mesh did not settle on the lower mesh: {upper_pose:?}"
    );
    assert!(
        lower_velocity.iter().all(|component| component.abs() < 0.1),
        "lower mesh did not settle: {lower_velocity:?}"
    );
    assert!(
        upper_velocity.iter().all(|component| component.abs() < 0.1),
        "upper mesh did not settle: {upper_velocity:?}"
    );
}

#[test]
fn dynamic_mesh_contacts_fixed_mesh_platform_with_both_windings() {
    let (platform_vertices, platform_triangles) = (
        vec![
            [-2.0, 0.0, -2.0],
            [2.0, 0.0, -2.0],
            [2.0, 0.0, 2.0],
            [-2.0, 0.0, 2.0],
        ],
        vec![[0, 1, 2], [0, 2, 3]],
    );
    let (vertices, cube_triangles) = cube_mesh(0.5);

    for winding in [1, -1] {
        let mut world = PhysicsWorld::new([0.0, -9.8, 0.0]).unwrap();
        let triangles = if winding == 1 {
            platform_triangles.clone()
        } else {
            platform_triangles
                .iter()
                .map(|triangle| [triangle[0], triangle[2], triangle[1]])
                .collect()
        };
        world
            .add_triangle_mesh(&platform_vertices, &triangles, fixed_mesh_config())
            .unwrap();
        let body = world
            .add_triangle_mesh(
                &vertices,
                &cube_triangles,
                dynamic_mesh_config([0.0, 1.2, 0.0]),
            )
            .unwrap();

        step_for_seconds(&mut world, 3);

        let pose = world.pose(body).unwrap();
        let velocity = world.linear_velocity(body).unwrap();
        assert!(
            (pose.position[1] - (0.5 + SURFACE_OFFSET)).abs() < 0.03,
            "winding {winding} mesh did not settle on platform: {pose:?}"
        );
        assert!(
            velocity.iter().all(|component| component.abs() < 0.1),
            "winding {winding} mesh did not settle: {velocity:?}"
        );
    }
}

#[test]
fn disconnected_mesh_opening_allows_small_mesh_to_fall_through() {
    let mut platform_vertices = Vec::new();
    let mut platform_triangles = Vec::new();
    let mut add_ledge = |x_min: f32, x_max: f32, z_min: f32, z_max: f32| {
        let base = platform_vertices.len() as u32;
        platform_vertices.extend([
            [x_min, 0.0, z_min],
            [x_max, 0.0, z_min],
            [x_max, 0.0, z_max],
            [x_min, 0.0, z_max],
        ]);
        platform_triangles.extend([[base, base + 1, base + 2], [base, base + 2, base + 3]]);
    };
    add_ledge(-2.0, -0.4, -2.0, 2.0);
    add_ledge(0.4, 2.0, -2.0, 2.0);
    add_ledge(-0.4, 0.4, -2.0, -0.4);
    add_ledge(-0.4, 0.4, 0.4, 2.0);

    let (vertices, triangles) = cube_mesh(0.1);
    let mut world = PhysicsWorld::new([0.0, -9.8, 0.0]).unwrap();
    world
        .add_triangle_mesh(&platform_vertices, &platform_triangles, fixed_mesh_config())
        .unwrap();
    let body = world
        .add_triangle_mesh(&vertices, &triangles, dynamic_mesh_config([0.0, 1.0, 0.0]))
        .unwrap();

    step_for_seconds(&mut world, 2);

    let pose = world.pose(body).unwrap();
    let velocity = world.linear_velocity(body).unwrap();
    assert!(
        pose.position[1] < -0.5,
        "mesh was falsely blocked by a disconnected opening: {pose:?}"
    );
    assert!(
        velocity[1] < -0.5,
        "mesh did not continue falling through the opening: {velocity:?}"
    );
}

#[test]
fn fast_rotated_mesh_keeps_every_vertex_above_floor() {
    let (vertices, triangles) = cube_mesh(0.35);
    let mut world = PhysicsWorld::new([0.0, -9.81, 0.0]).unwrap();
    world
        .add_hull(
            &hull_box([10.0, 0.1, 10.0]),
            BodyConfig {
                kind: BodyKind::Fixed,
                position: [0.0, -0.1, 0.0],
                ..BodyConfig::default()
            },
        )
        .unwrap();
    let angle = 0.3_f32;
    let body = world
        .add_triangle_mesh(
            &vertices,
            &triangles,
            BodyConfig {
                position: [0.0, 2.0, 0.0],
                rotation: [0.0, 0.0, angle.sin(), angle.cos()],
                ..BodyConfig::default()
            },
        )
        .unwrap();
    world
        .set_velocity(body, [0.0, -8.0, 0.0], [0.0, 0.0, 1.0])
        .unwrap();
    let mut lowest = f32::INFINITY;
    for _ in 0..1920 {
        world.step(DT, SUBSTEPS).unwrap();
        let pose = world.pose(body).unwrap();
        let [x, y, z, w] = pose.rotation;
        for p in &vertices {
            let height = 2.0 * (x * y + w * z) * p[0]
                + (1.0 - 2.0 * (x * x + z * z)) * p[1]
                + 2.0 * (y * z - w * x) * p[2]
                + pose.position[1];
            lowest = lowest.min(height);
        }
    }
    assert!(
        lowest >= -0.001,
        "fast rotated mesh penetrated floor by {} m",
        -lowest
    );
}
