use std::collections::HashMap;

use super::{
    BodyHandle, COOKED_HULL_MAX_VERTICES, CookedForm, PhysicsError, PhysicsWorld, ffi,
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
        let CookedForm::Hulls(hulls) = record.geometry.form() else {
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

/// Checks that a mesh bounds only closed, outward-wound volumes. The native
/// FLIP solver needs it: it signs its mesh level set by ray-crossing parity.
pub fn validate_closed_mesh(mesh: &TriangleMesh) -> Result<(), PhysicsError> {
    if part_windings(mesh)?.iter().any(|&winding| winding != Winding::Outward) {
        return Err(PhysicsError::InvalidInput(
            "mesh must be closed, manifold and outward oriented",
        ));
    }
    Ok(())
}

/// Which triangles bound a closed volume, one flag per triangle. Triangles
/// sharing an edge form a part. A part is closed when every edge it touches
/// is shared by exactly two of its triangles with opposite winding and it
/// encloses nonzero volume of either sign: an inside-out part (a mirrored
/// node, a flipped export) is still a sealed volume, and a distance signs it
/// by the magnitude of its winding number. Any other part is an open
/// surface. Errors only on geometry no distance can use: no triangles,
/// non-finite vertices, out-of-range indices, degenerate or zero-area
/// triangles.
pub fn closed_triangles(mesh: &TriangleMesh) -> Result<Vec<bool>, PhysicsError> {
    Ok(part_windings(mesh)?.into_iter().map(|winding| winding != Winding::Open).collect())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Winding {
    Open,
    Outward,
    Inward,
}

/// Each triangle's part: open, or sealed with positive (outward) or negative
/// (inward) volume.
fn part_windings(mesh: &TriangleMesh) -> Result<Vec<Winding>, PhysicsError> {
    if mesh.triangles.is_empty() {
        return Err(PhysicsError::InvalidInput("mesh has no triangles"));
    }
    if mesh
        .vertices
        .iter()
        .any(|vertex| vertex.iter().any(|value| !value.is_finite()))
    {
        return Err(PhysicsError::InvalidInput("mesh vertices must be finite"));
    }
    let mut edges: HashMap<(u32, u32), Vec<(usize, bool)>> = HashMap::new();
    let mut minimum = [f64::INFINITY; 3];
    let mut maximum = [f64::NEG_INFINITY; 3];
    for vertex in &mesh.vertices {
        for axis in 0..3 {
            let value = f64::from(vertex[axis]);
            minimum[axis] = minimum[axis].min(value);
            maximum[axis] = maximum[axis].max(value);
        }
    }
    let scale = (0..3)
        .map(|axis| maximum[axis] - minimum[axis])
        .fold(1.0_f64, f64::max);
    for (triangle_index, triangle) in mesh.triangles.iter().enumerate() {
        let [a, b, c] = *triangle;
        if [a, b, c]
            .iter()
            .any(|&index| index as usize >= mesh.vertices.len())
        {
            return Err(PhysicsError::InvalidInput("mesh triangle index is out of range"));
        }
        if a == b || b == c || c == a {
            return Err(PhysicsError::InvalidInput("mesh contains a degenerate triangle"));
        }
        let va = mesh.vertices[a as usize];
        let vb = mesh.vertices[b as usize];
        let vc = mesh.vertices[c as usize];
        let ab = [
            f64::from(vb[0]) - f64::from(va[0]),
            f64::from(vb[1]) - f64::from(va[1]),
            f64::from(vb[2]) - f64::from(va[2]),
        ];
        let ac = [
            f64::from(vc[0]) - f64::from(va[0]),
            f64::from(vc[1]) - f64::from(va[1]),
            f64::from(vc[2]) - f64::from(va[2]),
        ];
        let cross = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        let area2 = cross.iter().map(|value| value * value).sum::<f64>();
        if !area2.is_finite() || area2 <= 1.0e-24 * scale.max(1.0).powi(4) {
            return Err(PhysicsError::InvalidInput("mesh contains a zero-area triangle"));
        }
        for (from, to) in [(a, b), (b, c), (c, a)] {
            edges
                .entry((from.min(to), from.max(to)))
                .or_default()
                .push((triangle_index, from < to));
        }
    }
    let mut neighbours = vec![Vec::new(); mesh.triangles.len()];
    let mut sealed = vec![true; mesh.triangles.len()];
    for uses in edges.values() {
        let paired = uses.len() == 2 && uses[0].1 != uses[1].1;
        for &(triangle, _) in uses {
            sealed[triangle] &= paired;
        }
        let first = uses[0].0;
        for &(other, _) in &uses[1..] {
            neighbours[first].push(other);
            neighbours[other].push(first);
        }
    }
    let volume_epsilon = 1.0e-12 * scale.max(1.0).powi(3);
    let mut closed = vec![Winding::Open; mesh.triangles.len()];
    let mut visited = vec![false; mesh.triangles.len()];
    let mut part = Vec::new();
    for start in 0..mesh.triangles.len() {
        if visited[start] {
            continue;
        }
        part.clear();
        let mut stack = vec![start];
        visited[start] = true;
        let mut volume = 0.0_f64;
        let [origin_a, _, _] = mesh.triangles[start];
        let origin = mesh.vertices[origin_a as usize].map(f64::from);
        while let Some(index) = stack.pop() {
            part.push(index);
            let [a, b, c] = mesh.triangles[index];
            let va = mesh.vertices[a as usize].map(f64::from);
            let vb = mesh.vertices[b as usize].map(f64::from);
            let vc = mesh.vertices[c as usize].map(f64::from);
            let a = [va[0] - origin[0], va[1] - origin[1], va[2] - origin[2]];
            let b = [vb[0] - origin[0], vb[1] - origin[1], vb[2] - origin[2]];
            let c = [vc[0] - origin[0], vc[1] - origin[1], vc[2] - origin[2]];
            volume += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0;
            for &neighbour in &neighbours[index] {
                if !visited[neighbour] {
                    visited[neighbour] = true;
                    stack.push(neighbour);
                }
            }
        }
        let winding = if !volume.is_finite()
            || volume.abs() <= volume_epsilon
            || !part.iter().all(|&triangle| sealed[triangle])
        {
            Winding::Open
        } else if volume > 0.0 {
            Winding::Outward
        } else {
            Winding::Inward
        };
        for &triangle in &part {
            closed[triangle] = winding;
        }
    }
    Ok(closed)
}

/// Centre of mass and inertia tensor (column-major) of a triangle surface
/// carrying `mass` spread evenly over its area.
pub(crate) fn triangle_mesh_mass_properties(
    vertices: &[[f32; 3]],
    triangles: &[[u32; 3]],
    mass: f32,
) -> Result<([f32; 3], [f32; 9]), PhysicsError> {
    if vertices.len() < 3 {
        return Err(PhysicsError::InvalidInput("mesh needs at least 3 vertices"));
    }
    if triangles.is_empty() {
        return Err(PhysicsError::InvalidInput("mesh needs at least 1 triangle"));
    }
    if vertices
        .iter()
        .any(|vertex| !vertex.iter().all(|value| value.is_finite()))
    {
        return Err(PhysicsError::InvalidInput("mesh vertices must be finite"));
    }

    let mut area_sum = 0.0_f64;
    let mut first_moment = [0.0_f64; 3];
    let mut second_moment = [[0.0_f64; 3]; 3];
    for triangle in triangles {
        let [a_index, b_index, c_index] = *triangle;
        let a = *vertices
            .get(a_index as usize)
            .ok_or(PhysicsError::InvalidInput("mesh index is out of bounds"))?;
        let b = *vertices
            .get(b_index as usize)
            .ok_or(PhysicsError::InvalidInput("mesh index is out of bounds"))?;
        let c = *vertices
            .get(c_index as usize)
            .ok_or(PhysicsError::InvalidInput("mesh index is out of bounds"))?;
        let ab = [
            f64::from(b[0]) - f64::from(a[0]),
            f64::from(b[1]) - f64::from(a[1]),
            f64::from(b[2]) - f64::from(a[2]),
        ];
        let ac = [
            f64::from(c[0]) - f64::from(a[0]),
            f64::from(c[1]) - f64::from(a[1]),
            f64::from(c[2]) - f64::from(a[2]),
        ];
        let cross = [
            ab[1] * ac[2] - ab[2] * ac[1],
            ab[2] * ac[0] - ab[0] * ac[2],
            ab[0] * ac[1] - ab[1] * ac[0],
        ];
        let area = 0.5 * cross.iter().map(|value| value * value).sum::<f64>().sqrt();
        if !area.is_finite() || area <= 0.0 {
            return Err(PhysicsError::InvalidInput(
                "mesh triangles must have positive area",
            ));
        }
        area_sum += area;
        let points = [a, b, c];
        for axis in 0..3 {
            first_moment[axis] += area
                * points
                    .iter()
                    .map(|point| f64::from(point[axis]))
                    .sum::<f64>()
                / 3.0;
        }
        let mut sum = [0.0_f64; 3];
        for axis in 0..3 {
            sum[axis] = points.iter().map(|point| f64::from(point[axis])).sum();
        }
        for row in 0..3 {
            for column in 0..3 {
                second_moment[row][column] += area
                    * (sum[row] * sum[column]
                        + points
                            .iter()
                            .map(|point| f64::from(point[row]) * f64::from(point[column]))
                            .sum::<f64>())
                    / 12.0;
            }
        }
    }

    if !area_sum.is_finite() || area_sum <= 0.0 {
        return Err(PhysicsError::InvalidInput(
            "mesh surface area must be positive",
        ));
    }
    let center = first_moment.map(|value| value / area_sum);
    let mut inertia = [[0.0_f64; 3]; 3];
    let center_norm = center.iter().map(|value| value * value).sum::<f64>();
    for row in 0..3 {
        for column in 0..3 {
            let identity = if row == column { 1.0 } else { 0.0 };
            inertia[row][column] = (identity
                * second_moment
                    .iter()
                    .enumerate()
                    .map(|(axis, values)| values[axis])
                    .sum::<f64>()
                - second_moment[row][column])
                - area_sum * (identity * center_norm - center[row] * center[column]);
        }
    }
    let density = f64::from(mass) / area_sum;
    let center = center.map(|value| value as f32);
    let inertia = [
        (inertia[0][0] * density) as f32,
        (inertia[1][0] * density) as f32,
        (inertia[2][0] * density) as f32,
        (inertia[0][1] * density) as f32,
        (inertia[1][1] * density) as f32,
        (inertia[2][1] * density) as f32,
        (inertia[0][2] * density) as f32,
        (inertia[1][2] * density) as f32,
        (inertia[2][2] * density) as f32,
    ];
    if !center.iter().all(|value| value.is_finite())
        || !inertia.iter().all(|value| value.is_finite())
    {
        return Err(PhysicsError::InvalidInput(
            "mesh mass properties are non-finite",
        ));
    }
    Ok((center, inertia))
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

    fn cube_mesh() -> TriangleMesh {
        TriangleMesh {
            vertices: cube(),
            triangles: vec![
                [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
                [0, 1, 5], [0, 5, 4], [2, 3, 7], [2, 7, 6],
                [1, 2, 6], [1, 6, 5], [0, 4, 7], [0, 7, 3],
            ],
        }
    }

    /// Each part is classified on its own; open and non-manifold parts are
    /// surfaces, not errors, and a sealed part of either winding is closed.
    #[test]
    fn scene_physics_closed_triangles_classifies_each_part() {
        let cube = cube_mesh();
        assert_eq!(closed_triangles(&cube).unwrap(), vec![true; 12]);
        validate_closed_mesh(&cube).unwrap();

        let mut open = cube.clone();
        open.triangles.pop();
        assert_eq!(closed_triangles(&open).unwrap(), vec![false; 11]);
        assert!(validate_closed_mesh(&open).is_err());

        let mut inward = cube.clone();
        for triangle in &mut inward.triangles {
            triangle.swap(1, 2);
        }
        assert_eq!(closed_triangles(&inward).unwrap(), vec![true; 12], "an inside-out cube is still sealed");
        assert!(validate_closed_mesh(&inward).is_err(), "the native solver still needs outward winding");

        let mut mixed = cube.clone();
        mixed.vertices.extend([[2.0, 0.0, 0.0], [3.0, 0.0, 0.0], [2.0, 1.0, 0.0]]);
        mixed.triangles.push([8, 9, 10]);
        let flags = closed_triangles(&mixed).unwrap();
        assert!(flags[..12].iter().all(|&closed| closed));
        assert!(!flags[12]);

        let mut fin = cube;
        fin.vertices.push([0.0, 0.0, 2.0]);
        fin.triangles.push([6, 7, 8]);
        assert_eq!(closed_triangles(&fin).unwrap(), vec![false; 13], "a fin on an edge opens its part");

        let unusable = TriangleMesh { vertices: vec![[0.0; 3], [1.0, 0.0, 0.0], [f32::NAN, 0.0, 0.0]], triangles: vec![[0, 1, 2]] };
        assert!(closed_triangles(&unusable).is_err());
        assert!(closed_triangles(&TriangleMesh { vertices: self::cube(), triangles: Vec::new() }).is_err());
    }
}
