//! Body-local signed-distance lattices for liquid colliders, sources and
//! coupled hulls (`docs/GPU_MPM_SOLVER_DESIGN.md` D11). Built on the CPU where
//! role geometry is prepared, never on the content thread: a lattice costs
//! nodes × triangles.

use super::{PhysicsError, TriangleMesh, closed_triangles};

/// Largest lattice the builder accepts, in nodes.
pub const MAX_LATTICE_NODES: u64 = 1 << 24;

/// Signed distance sampled on a regular body-local lattice: negative inside
/// the solid, positive outside, in metres.
#[derive(Clone, Debug, PartialEq)]
pub struct DistanceLattice {
    /// Local position of node (0, 0, 0).
    pub origin: [f32; 3],
    pub spacing: f32,
    pub dims: [u32; 3],
    /// One value per node, x fastest.
    pub values: Vec<f32>,
}

impl DistanceLattice {
    pub fn index(&self, node: [u32; 3]) -> usize {
        (node[2] as usize * self.dims[1] as usize + node[1] as usize) * self.dims[0] as usize
            + node[0] as usize
    }

    pub fn node_position(&self, node: [u32; 3]) -> [f32; 3] {
        std::array::from_fn(|axis| self.origin[axis] + node[axis] as f32 * self.spacing)
    }

    pub fn value(&self, node: [u32; 3]) -> f32 {
        self.values[self.index(node)]
    }
}

/// The distance lattice of one mesh; see [`signed_distance_union`].
pub fn signed_distance_lattice(
    mesh: &TriangleMesh,
    spacing: f32,
    padding: f32,
    shell_thickness: f32,
) -> Result<DistanceLattice, PhysicsError> {
    signed_distance_union(std::slice::from_ref(mesh), spacing, padding, shell_thickness)
}

/// The solid of several meshes on one lattice, nodes `spacing` apart over
/// their joint bounds grown by `padding` on every side
/// (SCENE_SETUP_PROGRAMME.md D8 (open meshes are solids)). Each part of a
/// mesh ([`closed_triangles`]) is solid in one of two ways:
///
/// - A closed part is the volume it bounds: the exact distance to its nearest
///   triangle, signed by the magnitude of the generalized winding number, so
///   concave shapes, several closed parts and inside-out parts stay right.
/// - An open part is a shell: the unsigned distance to its nearest triangle
///   minus half of `shell_thickness`. This is how Houdini's VDB from Polygons
///   treats a surface with no interior and how Blender gives non-manifold
///   colliders a thickness.
///
/// Each node takes the smallest of these distances over every part and
/// mesh. That is exact outside the union and has its sign everywhere;
/// inside, it is one part's own depth, a lower bound that reaches 0 where
/// parts touch.
///
/// A shell is never thinner than two spacings: half its thickness is at
/// least `spacing`, so every lattice cell the surface crosses has a node
/// inside the shell and trilinear sampling cannot step over it. The floor
/// moves the shell's faces with the spacing, so a caller picks the spacing
/// from what samples the solid, never from the mesh's size alone. When any
/// part is open, the bounds also grow by that half thickness.
///
/// An open part is built as a narrow band: its distance is exact within
/// `padding` (at least one spacing) of the shell's faces, and every node
/// farther away reads `padding`, a lower bound. So an open part costs the
/// nodes near each of its triangles, not nodes × triangles; a closed part
/// still costs nodes × triangles, for its winding number.
pub fn signed_distance_union(
    meshes: &[TriangleMesh],
    spacing: f32,
    padding: f32,
    shell_thickness: f32,
) -> Result<DistanceLattice, PhysicsError> {
    signed_distance_union_within(meshes, spacing, padding, shell_thickness, None)
}

/// [`signed_distance_union`] over only the part of its bounds inside `crop`
/// (local min then max), so a solid larger than what samples it costs only
/// the nodes that are sampled.
pub fn signed_distance_union_within(
    meshes: &[TriangleMesh],
    spacing: f32,
    padding: f32,
    shell_thickness: f32,
    crop: Option<[[f32; 3]; 2]>,
) -> Result<DistanceLattice, PhysicsError> {
    if !(spacing.is_finite() && spacing > 0.0 && padding.is_finite() && padding >= 0.0) {
        return Err(PhysicsError::InvalidInput(
            "distance lattice spacing must be positive and padding non-negative",
        ));
    }
    if !(shell_thickness.is_finite() && shell_thickness >= 0.0) {
        return Err(PhysicsError::InvalidInput(
            "shell thickness must be finite and non-negative",
        ));
    }
    if meshes.is_empty() {
        return Err(PhysicsError::InvalidInput("distance lattice needs a mesh"));
    }
    let half_shell = (0.5 * shell_thickness).max(spacing);
    let mut parts = Vec::with_capacity(meshes.len());
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for mesh in meshes {
        let closed = closed_triangles(mesh)?;
        let mut closed_part = Vec::new();
        let mut open_part = Vec::new();
        for (triangle, closed) in mesh.triangles.iter().zip(closed) {
            let corners = triangle.map(|i| mesh.vertices[i as usize].map(f64::from));
            if closed { closed_part.push(corners) } else { open_part.push(corners) }
        }
        for vertex in &mesh.vertices {
            for axis in 0..3 {
                min[axis] = min[axis].min(vertex[axis]);
                max[axis] = max[axis].max(vertex[axis]);
            }
        }
        parts.push((closed_part, open_part));
    }
    let band = f64::from(padding.max(spacing));
    let padding = if parts.iter().any(|(_, open)| !open.is_empty()) {
        padding + half_shell
    } else {
        padding
    };
    let mut origin: [f32; 3] = std::array::from_fn(|axis| min[axis] - padding);
    let mut end: [f32; 3] = std::array::from_fn(|axis| max[axis] + padding);
    if let Some([low, high]) = crop {
        if low.iter().chain(&high).any(|value| !value.is_finite()) {
            return Err(PhysicsError::InvalidInput("a distance lattice crop must be finite"));
        }
        for axis in 0..3 {
            origin[axis] = origin[axis].max(low[axis]);
            end[axis] = end[axis].min(high[axis]);
        }
        if (0..3).any(|axis| end[axis] < origin[axis]) {
            return Err(PhysicsError::InvalidInput("the mesh lies outside the distance lattice crop"));
        }
    }
    let dims = lattice_dims(std::array::from_fn(|axis| end[axis] - origin[axis]), spacing, 0.0);
    let total = dims.iter().map(|&n| u64::from(n)).product::<u64>();
    if total > MAX_LATTICE_NODES {
        return Err(PhysicsError::InvalidInput(
            "distance lattice is too large; raise the spacing",
        ));
    }
    let mut lattice = DistanceLattice {
        origin,
        spacing,
        dims,
        values: vec![f32::INFINITY; total as usize],
    };
    let half_shell = f64::from(half_shell);
    // An open part is exact within the padding (at least a spacing) of its
    // shell's faces, as far as the lattice reaches past a closed part; beyond
    // that every node reads this lower bound, so the build visits only the
    // nodes near each open triangle.
    let reach = half_shell + band;
    let far = band as f32;
    for (closed, open) in &parts {
        if !closed.is_empty() {
            for k in 0..dims[2] {
                for j in 0..dims[1] {
                    for i in 0..dims[0] {
                        let p = lattice.node_position([i, j, k]).map(f64::from);
                        let mut nearest = f64::INFINITY;
                        let mut solid_angle = 0.0;
                        for triangle in closed {
                            nearest = nearest.min(distance_squared_to_triangle(p, triangle));
                            solid_angle += signed_solid_angle(p, triangle);
                        }
                        // Winding number ΣΩ / 4π is 1 inside an outward-wound
                        // volume and −1 inside an inside-out one; both are solid.
                        let inside = solid_angle.abs() > 2.0 * std::f64::consts::PI;
                        let volume = if inside { -nearest.sqrt() } else { nearest.sqrt() };
                        let index = lattice.index([i, j, k]);
                        lattice.values[index] = lattice.values[index].min(volume as f32);
                    }
                }
            }
        }
        if open.is_empty() {
            continue;
        }
        for value in &mut lattice.values {
            *value = value.min(far);
        }
        for triangle in open {
            // A node farther than `reach` from the triangle's box is farther
            // than that from the triangle.
            let range = |axis: usize| {
                let low = triangle.iter().map(|v| v[axis]).fold(f64::INFINITY, f64::min) - reach;
                let high = triangle.iter().map(|v| v[axis]).fold(f64::NEG_INFINITY, f64::max) + reach;
                let node = |x: f64| (x - f64::from(origin[axis])) / f64::from(spacing);
                let first = node(low).floor().max(0.0) as u32;
                let last = (node(high).ceil().max(0.0) as u32).min(dims[axis] - 1);
                first..=last
            };
            let (xs, ys, zs) = (range(0), range(1), range(2));
            for k in zs {
                for j in ys.clone() {
                    for i in xs.clone() {
                        let p = lattice.node_position([i, j, k]).map(f64::from);
                        let shell = (distance_squared_to_triangle(p, triangle).sqrt() - half_shell) as f32;
                        let index = lattice.index([i, j, k]);
                        lattice.values[index] = lattice.values[index].min(shell);
                    }
                }
            }
        }
    }
    Ok(lattice)
}

/// Nodes per axis over `extent` grown by `padding` on every side.
fn lattice_dims(extent: [f32; 3], spacing: f32, padding: f32) -> [u32; 3] {
    std::array::from_fn(|axis| {
        let extent = f64::from(extent[axis]) + 2.0 * f64::from(padding);
        (extent / f64::from(spacing)).ceil() as u32 + 1
    })
}

/// How many nodes [`signed_distance_union`] builds for meshes whose joint
/// bounds span `extent`, so a caller can pick a spacing under
/// [`MAX_LATTICE_NODES`] before paying for the build.
pub fn union_lattice_nodes(extent: [f32; 3], spacing: f32, padding: f32, shell_thickness: f32, any_open: bool) -> u64 {
    let padding = if any_open { padding + (0.5 * shell_thickness).max(spacing) } else { padding };
    lattice_dims(extent, spacing, padding).iter().map(|&n| u64::from(n)).product()
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn length(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

/// Solid angle the triangle subtends at `p`, signed by its winding (Van
/// Oosterom and Strackee 1983).
fn signed_solid_angle(p: [f64; 3], [a, b, c]: &[[f64; 3]; 3]) -> f64 {
    let (a, b, c) = (sub(*a, p), sub(*b, p), sub(*c, p));
    let (la, lb, lc) = (length(a), length(b), length(c));
    let numerator = dot(a, cross(b, c));
    let denominator = la * lb * lc + dot(a, b) * lc + dot(b, c) * la + dot(c, a) * lb;
    2.0 * numerator.atan2(denominator)
}

/// Squared distance from `p` to the closest point of a triangle (Ericson,
/// Real-Time Collision Detection, section 5.1.5).
pub fn distance_squared_to_triangle(p: [f64; 3], [a, b, c]: &[[f64; 3]; 3]) -> f64 {
    let closest = |q: [f64; 3]| {
        let d = sub(p, q);
        dot(d, d)
    };
    let along = |from: [f64; 3], edge: [f64; 3], t: f64| -> [f64; 3] {
        std::array::from_fn(|axis| from[axis] + t * edge[axis])
    };
    let ab = sub(*b, *a);
    let ac = sub(*c, *a);
    let ap = sub(p, *a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return closest(*a);
    }
    let bp = sub(p, *b);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return closest(*b);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return closest(along(*a, ab, d1 / (d1 - d3)));
    }
    let cp = sub(p, *c);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return closest(*c);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return closest(along(*a, ac, d2 / (d2 - d6)));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return closest(along(*b, sub(*c, *b), (d4 - d3) / ((d4 - d3) + (d5 - d6))));
    }
    let denominator = 1.0 / (va + vb + vc);
    let v = vb * denominator;
    let w = vc * denominator;
    closest(std::array::from_fn(|axis| a[axis] + ab[axis] * v + ac[axis] * w))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An axis-aligned box, outward wound.
    fn cuboid(min: [f32; 3], max: [f32; 3]) -> TriangleMesh {
        let v = |x: usize, y: usize, z: usize| {
            [[min[0], max[0]][x], [min[1], max[1]][y], [min[2], max[2]][z]]
        };
        TriangleMesh {
            vertices: vec![
                v(0, 0, 0), v(1, 0, 0), v(1, 1, 0), v(0, 1, 0),
                v(0, 0, 1), v(1, 0, 1), v(1, 1, 1), v(0, 1, 1),
            ],
            triangles: vec![
                [0, 2, 1], [0, 3, 2], [4, 5, 6], [4, 6, 7],
                [0, 1, 5], [0, 5, 4], [2, 3, 7], [2, 7, 6],
                [1, 2, 6], [1, 6, 5], [0, 4, 7], [0, 7, 3],
            ],
        }
    }

    /// A U-shaped prism: the counter-clockwise outline (0,0) (3,0) (3,3) (2,3)
    /// (2,1) (1,1) (1,3) (0,3) extruded over z ∈ [0, 1]. The notch between the
    /// posts, x ∈ (1, 2), y ∈ (1, 3), is outside.
    fn bowl() -> TriangleMesh {
        let outline = [[0.0, 0.0], [3.0, 0.0], [3.0, 3.0], [2.0, 3.0], [2.0, 1.0], [1.0, 1.0], [1.0, 3.0], [0.0, 3.0]];
        let caps = [[0, 1, 4], [1, 2, 4], [2, 3, 4], [0, 4, 5], [0, 5, 6], [0, 6, 7]];
        let n = outline.len() as u32;
        let mut vertices: Vec<[f32; 3]> = outline.iter().map(|&[x, y]| [x, y, 0.0]).collect();
        vertices.extend(outline.iter().map(|&[x, y]| [x, y, 1.0]));
        let mut triangles = Vec::new();
        for [a, b, c] in caps {
            triangles.push([a, c, b]);
            triangles.push([a + n, b + n, c + n]);
        }
        for i in 0..n {
            let j = (i + 1) % n;
            triangles.push([i, j, j + n]);
            triangles.push([i, j + n, i + n]);
        }
        TriangleMesh { vertices, triangles }
    }

    fn box_distance(p: [f32; 3], half: f32) -> f32 {
        let q = p.map(|v| v.abs() - half);
        let outside = q.map(|v| v.max(0.0));
        (outside[0] * outside[0] + outside[1] * outside[1] + outside[2] * outside[2]).sqrt()
            + q[0].max(q[1]).max(q[2]).min(0.0)
    }

    #[test]
    fn sdf_box_matches_analytic() {
        let lattice = signed_distance_lattice(&cuboid([-0.5; 3], [0.5; 3]), 0.1, 0.3, 0.0).unwrap();
        assert_eq!(lattice.dims, [17; 3]);
        let mut worst = 0.0f32;
        for k in 0..17 {
            for j in 0..17 {
                for i in 0..17 {
                    let expected = box_distance(lattice.node_position([i, j, k]), 0.5);
                    worst = worst.max((lattice.value([i, j, k]) - expected).abs());
                }
            }
        }
        assert!(worst < 1e-5, "worst error {worst}");
        assert!((lattice.value([8, 8, 8]) + 0.5).abs() < 1e-5);
        // Shell thickness only shapes open parts: a closed box is unchanged.
        let thick = signed_distance_lattice(&cuboid([-0.5; 3], [0.5; 3]), 0.1, 0.3, 0.5).unwrap();
        assert_eq!(thick, lattice);
        // An inside-out box is the same solid, not a thin skin.
        let mut inward = cuboid([-0.5; 3], [0.5; 3]);
        for triangle in &mut inward.triangles {
            triangle.swap(1, 2);
        }
        assert_eq!(signed_distance_lattice(&inward, 0.1, 0.3, 0.5).unwrap(), lattice);
        assert_eq!(
            union_lattice_nodes([1.0; 3], 0.1, 0.3, 0.5, false),
            lattice.values.len() as u64
        );
    }

    #[test]
    fn sdf_concave_bowl_sign() {
        let mesh = bowl();
        crate::validate_closed_mesh(&mesh).expect("the bowl is closed and outward wound");
        let lattice = signed_distance_lattice(&mesh, 0.25, 0.5, 0.0).unwrap();
        let at = |p: [f32; 3]| {
            let node = std::array::from_fn(|axis| ((p[axis] - lattice.origin[axis]) / lattice.spacing).round() as u32);
            assert_eq!(lattice.node_position(node), p, "{p:?} is a node");
            lattice.value(node)
        };
        // In the notch: outside, half a unit from both posts.
        assert!((at([1.5, 2.0, 0.5]) - 0.5).abs() < 1e-5);
        // Inside a post and the base.
        assert!((at([0.5, 2.0, 0.5]) + 0.5).abs() < 1e-5);
        assert!((at([1.5, 0.5, 0.5]) + 0.5).abs() < 1e-5);
        // Above the notch's mouth: outside, nearest the posts' inner top edges
        // at (1, 3) and (2, 3).
        assert!((at([1.5, 3.5, 0.5]) - 0.5f32.sqrt()).abs() < 1e-5);
    }

    /// The unit square in the z = 0 plane, one-sided: an open surface.
    fn open_quad() -> TriangleMesh {
        TriangleMesh {
            vertices: vec![[-0.5, -0.5, 0.0], [0.5, -0.5, 0.0], [0.5, 0.5, 0.0], [-0.5, 0.5, 0.0]],
            triangles: vec![[0, 1, 2], [0, 2, 3]],
        }
    }

    fn quad_distance(p: [f32; 3]) -> f32 {
        let dx = (p[0].abs() - 0.5).max(0.0);
        let dy = (p[1].abs() - 0.5).max(0.0);
        (dx * dx + dy * dy + p[2] * p[2]).sqrt()
    }

    /// Unsigned distance minus half the thickness at every node within the
    /// band, and the band's width beyond it: negative within half the
    /// thickness of the surface on either side, positive beyond it. Doubling
    /// the thickness moves the shell's faces out by the same amount
    /// everywhere.
    #[test]
    fn sdf_open_quad_is_a_shell_scaled_by_thickness() {
        for thickness in [0.2f32, 0.4] {
            let half = 0.5 * thickness;
            let lattice = signed_distance_lattice(&open_quad(), 0.05, 0.1, thickness).unwrap();
            // The lattice grows by half the shell beyond the padding.
            assert!((lattice.origin[2] + 0.1 + half).abs() < 1e-6, "{:?}", lattice.origin);
            assert_eq!(
                union_lattice_nodes([1.0, 1.0, 0.0], 0.05, 0.1, thickness, true),
                lattice.values.len() as u64
            );
            let mut worst = 0.0f32;
            for k in 0..lattice.dims[2] {
                for j in 0..lattice.dims[1] {
                    for i in 0..lattice.dims[0] {
                        let p = lattice.node_position([i, j, k]);
                        let distance = quad_distance(p);
                        let expected = if distance <= half + 0.1 { distance - half } else { 0.1 };
                        worst = worst.max((lattice.value([i, j, k]) - expected).abs());
                    }
                }
            }
            assert!(worst < 1e-5, "thickness {thickness}: worst error {worst}");
            let centre: [u32; 3] = std::array::from_fn(|axis| (-lattice.origin[axis] / 0.05).round() as u32);
            assert!(lattice.node_position(centre).iter().all(|v| v.abs() < 1e-6));
            assert!((lattice.value(centre) + half).abs() < 1e-5);
            for side in [-1i32, 1] {
                let inside = [centre[0], centre[1], (centre[2] as i32 + side) as u32];
                assert!(lattice.value(inside) < 0.0, "0.05 m from the surface is in the shell");
                let beyond = [centre[0], centre[1], (centre[2] as i32 + side * (1 + (half / 0.05).round() as i32)) as u32];
                assert!(lattice.value(beyond) > 0.0, "past half the thickness is outside");
            }
        }
    }

    /// A dense open mesh, a 200k-triangle rolling terrain like an open
    /// photoscan, builds in about a second: the band visits only nodes near
    /// each triangle. Every node against all triangles would be about 10¹¹
    /// distance tests, minutes even optimised, so the budget cannot pass by
    /// luck and a loaded machine cannot fail it. Sampled nodes match the
    /// exact shell distance inside the band and its width outside.
    #[test]
    fn sdf_dense_open_mesh_builds_as_a_narrow_band() {
        let side = 317u32;
        let height = |x: f32, z: f32| 0.4 * (0.7 * x).sin() * (0.5 * z).cos();
        let mut vertices = Vec::new();
        for j in 0..=side {
            for i in 0..=side {
                let (x, z) = (10.0 * i as f32 / side as f32 - 5.0, 10.0 * j as f32 / side as f32 - 5.0);
                vertices.push([x, height(x, z), z]);
            }
        }
        let mut triangles = Vec::new();
        for j in 0..side {
            for i in 0..side {
                let at = |i: u32, j: u32| j * (side + 1) + i;
                triangles.push([at(i, j), at(i, j + 1), at(i + 1, j)]);
                triangles.push([at(i + 1, j), at(i, j + 1), at(i + 1, j + 1)]);
            }
        }
        assert!(triangles.len() > 200_000);
        let terrain = TriangleMesh { vertices, triangles };
        let started = std::time::Instant::now();
        let lattice = signed_distance_lattice(&terrain, 0.05, 0.1, 0.1).unwrap();
        let took = started.elapsed();
        assert!(took < std::time::Duration::from_secs(20), "the band took {took:?}");
        let corners: Vec<[[f64; 3]; 3]> = terrain
            .triangles
            .iter()
            .map(|t| t.map(|i| terrain.vertices[i as usize].map(f64::from)))
            .collect();
        for (x, z) in [(0.3f32, -1.7f32), (-4.2, 2.9), (3.33, 0.01)] {
            for dy in [-0.3f32, -0.12, -0.04, 0.0, 0.06, 0.13, 0.4] {
                let node: [u32; 3] = std::array::from_fn(|axis| {
                    (([x, height(x, z) + dy, z][axis] - lattice.origin[axis]) / lattice.spacing).round() as u32
                });
                let p = lattice.node_position(node).map(f64::from);
                let nearest = corners.iter().map(|t| distance_squared_to_triangle(p, t)).fold(f64::INFINITY, f64::min).sqrt();
                let expected = if nearest <= 0.15 { nearest - 0.05 } else { 0.1 };
                assert!((f64::from(lattice.value(node)) - expected).abs() < 1e-5, "{p:?}: {} against {expected}", lattice.value(node));
            }
        }
    }

    /// A shell thinner than the lattice holds is two spacings thick.
    #[test]
    fn sdf_shell_is_at_least_two_spacings_thick() {
        let thin = signed_distance_lattice(&open_quad(), 0.05, 0.1, 0.01).unwrap();
        let floor = signed_distance_lattice(&open_quad(), 0.05, 0.1, 0.1).unwrap();
        assert_eq!(thin, floor);
        assert!(signed_distance_lattice(&open_quad(), 0.05, 0.1, -0.1).is_err());
        assert!(signed_distance_lattice(&open_quad(), 0.0, 0.1, 0.1).is_err());
    }

    /// One mesh holding a closed cube and an open quad beside it: the cube
    /// keeps its signed distance, the quad is a shell.
    #[test]
    fn sdf_mixed_mesh_keeps_closed_parts_solid() {
        let mut mesh = cuboid([-0.5; 3], [0.5; 3]);
        let quad = open_quad();
        let base = mesh.vertices.len() as u32;
        mesh.vertices.extend(quad.vertices.iter().map(|v| [v[0] + 3.0, v[1], v[2]]));
        mesh.triangles.extend(quad.triangles.iter().map(|t| t.map(|i| i + base)));
        let lattice = signed_distance_lattice(&mesh, 0.1, 0.3, 0.4).unwrap();
        let node = |p: [f32; 3]| -> [u32; 3] {
            std::array::from_fn(|axis| ((p[axis] - lattice.origin[axis]) / lattice.spacing).round() as u32)
        };
        let cube_centre = node([0.0; 3]);
        assert!((lattice.value(cube_centre) + 0.5).abs() < 1e-5);
        let quad_centre = node([3.0, 0.0, 0.0]);
        assert!((lattice.value(quad_centre) + 0.2).abs() < 1e-5);
    }
}
