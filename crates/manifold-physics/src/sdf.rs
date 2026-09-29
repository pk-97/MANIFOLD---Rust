//! Body-local signed-distance lattices for liquid colliders, sources and
//! coupled hulls (`docs/GPU_MPM_SOLVER_DESIGN.md` D11). Built on the CPU where
//! role geometry is prepared, never on the content thread: a lattice costs
//! nodes × triangles.

use super::{PhysicsError, TriangleMesh, validate_closed_mesh};

/// Largest lattice the builder accepts, in nodes.
pub const MAX_LATTICE_NODES: u64 = 1 << 24;

/// Signed distance sampled on a regular body-local lattice: negative inside
/// the mesh, positive outside, in metres.
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

/// Builds the signed-distance lattice of a closed mesh (validated by
/// [`validate_closed_mesh`]): nodes `spacing` apart over the mesh bounds
/// grown by `padding` on every side. The magnitude is the exact distance to
/// the nearest triangle; the sign comes from the generalized winding number,
/// so it stays right for concave shapes and several closed parts.
pub fn signed_distance_lattice(
    mesh: &TriangleMesh,
    spacing: f32,
    padding: f32,
) -> Result<DistanceLattice, PhysicsError> {
    if !(spacing.is_finite() && spacing > 0.0 && padding.is_finite() && padding >= 0.0) {
        return Err(PhysicsError::InvalidInput(
            "distance lattice spacing must be positive and padding non-negative",
        ));
    }
    validate_closed_mesh(mesh)?;
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in &mesh.vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex[axis]);
            max[axis] = max[axis].max(vertex[axis]);
        }
    }
    let origin: [f32; 3] = std::array::from_fn(|axis| min[axis] - padding);
    let dims: [u32; 3] = std::array::from_fn(|axis| {
        let extent = f64::from(max[axis] - min[axis]) + 2.0 * f64::from(padding);
        (extent / f64::from(spacing)).ceil() as u32 + 1
    });
    let total = dims.iter().map(|&n| u64::from(n)).product::<u64>();
    if total > MAX_LATTICE_NODES {
        return Err(PhysicsError::InvalidInput(
            "distance lattice is too large; raise the spacing",
        ));
    }
    let triangles: Vec<[[f64; 3]; 3]> = mesh
        .triangles
        .iter()
        .map(|t| t.map(|i| mesh.vertices[i as usize].map(f64::from)))
        .collect();
    let mut lattice = DistanceLattice {
        origin,
        spacing,
        dims,
        values: Vec::with_capacity(total as usize),
    };
    for k in 0..dims[2] {
        for j in 0..dims[1] {
            for i in 0..dims[0] {
                let p = lattice.node_position([i, j, k]).map(f64::from);
                let mut nearest = f64::INFINITY;
                let mut solid_angle = 0.0;
                for triangle in &triangles {
                    nearest = nearest.min(distance_squared_to_triangle(p, triangle));
                    solid_angle += signed_solid_angle(p, triangle);
                }
                // Winding number ΣΩ / 4π is 1 inside an outward-wound volume.
                let inside = solid_angle > 2.0 * std::f64::consts::PI;
                let distance = nearest.sqrt();
                lattice.values.push(if inside { -distance } else { distance } as f32);
            }
        }
    }
    Ok(lattice)
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
fn distance_squared_to_triangle(p: [f64; 3], [a, b, c]: &[[f64; 3]; 3]) -> f64 {
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
        let lattice = signed_distance_lattice(&cuboid([-0.5; 3], [0.5; 3]), 0.1, 0.3).unwrap();
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
    }

    #[test]
    fn sdf_concave_bowl_sign() {
        let mesh = bowl();
        validate_closed_mesh(&mesh).expect("the bowl is closed and outward wound");
        let lattice = signed_distance_lattice(&mesh, 0.25, 0.5).unwrap();
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

    #[test]
    fn sdf_rejects_open_mesh() {
        let mut open = cuboid([-0.5; 3], [0.5; 3]);
        open.triangles.pop();
        assert!(matches!(
            signed_distance_lattice(&open, 0.1, 0.1),
            Err(PhysicsError::InvalidInput(message)) if message.contains("open")
        ));
        assert!(signed_distance_lattice(&cuboid([-0.5; 3], [0.5; 3]), 0.0, 0.1).is_err());
    }
}
