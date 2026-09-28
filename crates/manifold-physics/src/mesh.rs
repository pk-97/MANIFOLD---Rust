use super::{
    BodyHandle, COOKED_HULL_MAX_VERTICES, OwnedGeometry, PhysicsError, PhysicsWorld, ffi,
    native_lock,
};

/// An owned indexed triangle surface shared by physics adapters.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriangleMesh {
    pub vertices: Vec<[f32; 3]>,
    pub triangles: Vec<[u32; 3]>,
}

/// Cook a point cloud and expose the native hull's outward-wound surface.
pub fn cook_hull_mesh(points: &[[f32; 3]]) -> Result<TriangleMesh, PhysicsError> {
    if points.len() < 4 {
        return Err(PhysicsError::InvalidInput("hull needs at least 4 points"));
    }
    if points
        .iter()
        .any(|point| !point.iter().all(|value| value.is_finite()))
    {
        return Err(PhysicsError::InvalidInput("hull points must be finite"));
    }
    let point_count = i32::try_from(points.len())
        .map_err(|_| PhysicsError::InvalidInput("too many hull points"))?;

    let _lock = native_lock();
    let hull = unsafe {
        ffi::manifold_box3d_cook_hull(
            points.as_ptr().cast::<f32>(),
            point_count,
            COOKED_HULL_MAX_VERTICES,
        )
    };
    if hull == 0 {
        return Err(PhysicsError::NativeAllocation);
    }

    let result = copy_hull_mesh(hull);
    unsafe { ffi::manifold_box3d_destroy_hull(hull) };
    result
}

impl PhysicsWorld {
    /// Copy the exact convex collision surfaces installed on this body, in
    /// body-local coordinates and native shape order. No hull is re-cooked.
    ///
    /// This allocates owned meshes for adapter preparation; do not call it on
    /// a simulation hot path. Body pose, mass, enabled state and velocity are
    /// unchanged. Static triangle-mesh terrain is not a convex proxy and is
    /// rejected explicitly.
    pub fn hull_meshes(&self, body: BodyHandle) -> Result<Vec<TriangleMesh>, PhysicsError> {
        let record = self.body_record(body)?;
        let OwnedGeometry::Hulls(hulls) = &record.owned_geometry else {
            return Err(PhysicsError::InvalidInput("body does not use convex hulls"));
        };
        let _lock = native_lock();
        hulls.iter().map(|&hull| copy_hull_mesh(hull)).collect()
    }
}

// The caller holds the native lock and owns the hull for this entire copy.
fn copy_hull_mesh(hull: usize) -> Result<TriangleMesh, PhysicsError> {
    let vertex_count =
        unsafe { ffi::manifold_box3d_hull_copy_points(hull, std::ptr::null_mut(), 0) };
    if vertex_count < 4 {
        return Err(PhysicsError::NativeFailure);
    }
    let vertex_count = usize::try_from(vertex_count).map_err(|_| PhysicsError::NativeFailure)?;
    let mut vertices = vec![[0.0_f32; 3]; vertex_count];
    let copied_vertices = unsafe {
        ffi::manifold_box3d_hull_copy_points(
            hull,
            vertices.as_mut_ptr().cast::<f32>(),
            i32::try_from(vertex_count).map_err(|_| PhysicsError::NativeFailure)?,
        )
    };
    if copied_vertices < 4 || copied_vertices as usize != vertex_count {
        return Err(PhysicsError::NativeFailure);
    }
    if vertices
        .iter()
        .any(|vertex| !vertex.iter().all(|value| value.is_finite()))
    {
        return Err(PhysicsError::NativeFailure);
    }

    let triangle_count =
        unsafe { ffi::manifold_box3d_hull_copy_triangles(hull, std::ptr::null_mut(), 0) };
    if triangle_count < 1 {
        return Err(PhysicsError::NativeFailure);
    }
    let triangle_count =
        usize::try_from(triangle_count).map_err(|_| PhysicsError::NativeFailure)?;
    let mut triangles = vec![[0_u32; 3]; triangle_count];
    let copied_triangles = unsafe {
        ffi::manifold_box3d_hull_copy_triangles(
            hull,
            triangles.as_mut_ptr().cast::<u32>(),
            i32::try_from(triangle_count).map_err(|_| PhysicsError::NativeFailure)?,
        )
    };
    if copied_triangles < 1 || copied_triangles as usize != triangle_count {
        return Err(PhysicsError::NativeFailure);
    }
    if triangles.iter().any(|triangle| {
        triangle
            .iter()
            .any(|&index| index as usize >= vertices.len())
    }) {
        return Err(PhysicsError::NativeFailure);
    }
    Ok(TriangleMesh {
        vertices,
        triangles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cube() -> Vec<[f32; 3]> {
        vec![
            [-0.5, -0.5, -0.5],
            [0.5, -0.5, -0.5],
            [0.5, 0.5, -0.5],
            [-0.5, 0.5, -0.5],
            [-0.5, -0.5, 0.5],
            [0.5, -0.5, 0.5],
            [0.5, 0.5, 0.5],
            [-0.5, 0.5, 0.5],
        ]
    }

    fn subtract(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
        [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
    }

    fn cross(left: [f32; 3], right: [f32; 3]) -> [f32; 3] {
        [
            left[1] * right[2] - left[2] * right[1],
            left[2] * right[0] - left[0] * right[2],
            left[0] * right[1] - left[1] * right[0],
        ]
    }

    fn dot(left: [f32; 3], right: [f32; 3]) -> f32 {
        left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
    }

    #[test]
    fn scene_physics_cooked_mesh_has_positive_volume_and_outward_winding() {
        let mesh = cook_hull_mesh(&cube()).unwrap();
        let mut volume = 0.0;
        for &[a, b, c] in &mesh.triangles {
            let pa = mesh.vertices[a as usize];
            let pb = mesh.vertices[b as usize];
            let pc = mesh.vertices[c as usize];
            volume += dot(pa, cross(pb, pc)) / 6.0;
            let normal = cross(subtract(pb, pa), subtract(pc, pa));
            let centroid = [
                (pa[0] + pb[0] + pc[0]) / 3.0,
                (pa[1] + pb[1] + pc[1]) / 3.0,
                (pa[2] + pb[2] + pc[2]) / 3.0,
            ];
            assert!(dot(normal, centroid) > 0.0);
        }
        assert!((volume - 1.0).abs() < 1e-5, "unit cube volume: {volume}");
    }

    #[test]
    fn scene_physics_cooked_mesh_has_paired_edges_and_bounded_vertices() {
        let mesh = cook_hull_mesh(&cube()).unwrap();
        let mut directed_edges = HashMap::new();
        for triangle in &mesh.triangles {
            for edge in [
                (triangle[0], triangle[1]),
                (triangle[1], triangle[2]),
                (triangle[2], triangle[0]),
            ] {
                *directed_edges.entry(edge).or_insert(0_u32) += 1;
            }
        }
        for (&(start, end), &count) in &directed_edges {
            assert_eq!(count, 1, "each oriented edge belongs to one face");
            assert_eq!(
                count,
                directed_edges.get(&(end, start)).copied().unwrap_or(0)
            );
        }
        for vertex in &mesh.vertices {
            assert!(vertex.iter().all(|value| value.is_finite()));
            assert!(vertex.iter().all(|value| (-0.5..=0.5).contains(value)));
        }
        for triangle in &mesh.triangles {
            assert!(
                triangle
                    .iter()
                    .all(|&index| (index as usize) < mesh.vertices.len())
            );
        }
    }

    #[test]
    fn scene_physics_cooked_mesh_rejects_nonfinite_and_insufficient_input() {
        assert_eq!(
            cook_hull_mesh(&[[0.0; 3]; 3]),
            Err(PhysicsError::InvalidInput("hull needs at least 4 points"))
        );
        let mut points = cube();
        points[0][0] = f32::NAN;
        assert_eq!(
            cook_hull_mesh(&points),
            Err(PhysicsError::InvalidInput("hull points must be finite"))
        );
    }

    #[test]
    fn scene_physics_body_mesh_preserves_installed_hull_detail() {
        // The general point-cloud cooker caps output at 42 vertices. An
        // installed body can retain more, so re-cooking is not an export.
        // A 24-sided prism has 48 vertices and 144 half-edges, within the
        // native 255 half-edge limit (a triangulated sphere need not be).
        let mut points = Vec::new();
        for y in [-0.5, 0.5] {
            for longitude in 0..24 {
                let theta = std::f32::consts::TAU * longitude as f32 / 24.0;
                points.push([theta.cos(), y, theta.sin()]);
            }
        }
        let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
        let body = world
            .add_hull(&points, super::super::BodyConfig::default())
            .unwrap();
        let meshes = world.hull_meshes(body).unwrap();
        assert_eq!(meshes.len(), 1);
        assert!(meshes[0].vertices.len() > COOKED_HULL_MAX_VERTICES as usize);
        assert!(
            cook_hull_mesh(&points).unwrap().vertices.len() <= COOKED_HULL_MAX_VERTICES as usize
        );
        for point in &meshes[0].vertices {
            assert!(
                points
                    .iter()
                    .any(|input| input.iter().zip(point).all(|(a, b)| (a - b).abs() < 1e-6)),
                "installed hull vertex exceeds f32 cooking precision: {point:?}"
            );
        }
        let mesh = &meshes[0];
        for &[a, b, c] in &mesh.triangles {
            let [pa, pb, pc] = [a, b, c].map(|i| mesh.vertices[i as usize]);
            let normal = cross(subtract(pb, pa), subtract(pc, pa));
            assert!(dot(normal, pa) > 0.0, "surface winding must remain outward");
        }
    }

    #[test]
    fn scene_physics_body_mesh_keeps_compound_parts_local_and_owned() {
        let hulls: Vec<_> = [-2.0, 2.0]
            .into_iter()
            .map(|x| cube().into_iter().map(|p| [p[0] + x, p[1], p[2]]).collect())
            .collect();
        let mut world = PhysicsWorld::new([0.0; 3]).unwrap();
        let body = world
            .add_hulls(
                &hulls,
                super::super::BodyConfig {
                    position: [8.0, -3.0, 5.0],
                    rotation: [0.0, 0.5, 0.0, 3.0_f32.sqrt() * 0.5],
                    mass: 30.0,
                    ..super::super::BodyConfig::default()
                },
            )
            .unwrap();
        world
            .set_velocity(body, [1.0, 2.0, 3.0], [0.0, 0.4, 0.0])
            .unwrap();
        let pose = world.pose(body).unwrap();
        let dynamics = world.dynamics(body).unwrap();
        let meshes = world.hull_meshes(body).unwrap();
        assert_eq!(world.pose(body).unwrap(), pose);
        assert_eq!(world.dynamics(body).unwrap(), dynamics);
        assert_eq!(meshes.len(), 2);
        for (mesh, points) in meshes.iter().zip(&hulls) {
            assert_eq!(mesh.vertices.len(), 8);
            assert!(mesh.vertices.iter().all(|p| points.contains(p)));
            let volume: f32 = mesh
                .triangles
                .iter()
                .map(|&[a, b, c]| {
                    dot(
                        mesh.vertices[a as usize],
                        cross(mesh.vertices[b as usize], mesh.vertices[c as usize]),
                    ) / 6.0
                })
                .sum();
            assert!((volume - 1.0).abs() < 1e-5);
        }
        world.step(super::super::Seconds(1.0 / 60.0), 4).unwrap();
        assert_eq!(
            world.hull_meshes(body).unwrap(),
            meshes,
            "world motion must not enter local geometry"
        );
        drop(world);
        assert!(meshes.iter().all(|mesh| mesh.vertices.len() == 8));
    }

    #[test]
    fn scene_physics_body_mesh_rejects_foreign_handles_and_triangle_terrain() {
        let mut local = PhysicsWorld::new([0.0; 3]).unwrap();
        let mut foreign = PhysicsWorld::new([0.0; 3]).unwrap();
        let body = foreign
            .add_hull(&cube(), super::super::BodyConfig::default())
            .unwrap();
        assert_eq!(local.hull_meshes(body), Err(PhysicsError::InvalidHandle));
        let mesh = cook_hull_mesh(&cube()).unwrap();
        let terrain = local
            .add_triangle_mesh(
                &mesh.vertices,
                &mesh.triangles,
                super::super::BodyConfig {
                    kind: super::super::BodyKind::Fixed,
                    ..super::super::BodyConfig::default()
                },
            )
            .unwrap();
        assert_eq!(
            local.hull_meshes(terrain),
            Err(PhysicsError::InvalidInput("body does not use convex hulls"))
        );
    }
}
