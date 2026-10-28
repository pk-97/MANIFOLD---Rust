//! A bounding volume hierarchy over mesh triangles for nearest-surface
//! distance and fast winding number queries (Barill et al. 2018, first order).
//!
//! The CPU queries are the reference for a WGSL port: traversal is a plain
//! stack loop over a flat node array, children are always an adjacent pair, and
//! the build bounds the depth so the GPU stack has a fixed size.

use crate::sdf::{distance_squared_to_triangle, signed_solid_angle};
use crate::{PhysicsError, TriangleMesh};

/// At most this many triangles per leaf.
pub const BVH_LEAF_TRIANGLES: usize = 4;
/// GPU traversal stack size; the build guarantees depth <= this.
pub const BVH_MAX_DEPTH: usize = 64;
/// A node is far when |q - centroid| > beta * radius.
pub const WINDING_BETA: f32 = 2.0;

/// Nodes at or below this depth (root = 1) always split at the median, which
/// halves the count each level, so the tree ends within `BVH_MAX_DEPTH`.
const MEDIAN_ONLY_DEPTH: usize = BVH_MAX_DEPTH - 24;
/// Median splits from `MEDIAN_ONLY_DEPTH` shrink at most this many triangles
/// to a leaf within 24 levels.
const MAX_TRIANGLES: usize = BVH_LEAF_TRIANGLES << 24;
const BINS: usize = 16;

type Tri = [[f32; 3]; 3];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BvhNode {
    pub min: [f32; 3],
    pub max: [f32; 3],
    /// Leaf: index of its first triangle in `MeshBvh::triangles`. Inner: index
    /// of its left child; the right child is `first + 1`.
    pub first: u32,
    /// Leaf: its triangle count (1..=BVH_LEAF_TRIANGLES). Inner: 0.
    pub count: u32,
    /// Sum over the triangles below of area * unit normal
    /// (= 0.5 * cross(b - a, c - a)).
    pub dipole: [f32; 3],
    /// Area-weighted mean of the triangle centroids below.
    pub centroid: [f32; 3],
    /// Largest distance from `centroid` to any vertex of the triangles below.
    pub radius: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshBvh {
    /// `nodes[0]` is the root.
    pub nodes: Vec<BvhNode>,
    /// Reordered so each leaf's triangles are contiguous.
    pub triangles: Vec<Tri>,
    depth: u32,
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn to_f64(p: [f32; 3]) -> [f64; 3] {
    p.map(f64::from)
}

fn tri_f64(t: &Tri) -> [[f64; 3]; 3] {
    t.map(to_f64)
}

fn tri_centroid(t: &Tri) -> [f32; 3] {
    std::array::from_fn(|k| (t[0][k] + t[1][k] + t[2][k]) / 3.0)
}

/// 0.5 * cross(b - a, c - a), in f64.
fn tri_area_vector(t: &Tri) -> [f64; 3] {
    let [a, b, c] = tri_f64(t);
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    [
        0.5 * (ab[1] * ac[2] - ab[2] * ac[1]),
        0.5 * (ab[2] * ac[0] - ab[0] * ac[2]),
        0.5 * (ab[0] * ac[1] - ab[1] * ac[0]),
    ]
}

fn tri_box(t: &Tri) -> ([f32; 3], [f32; 3]) {
    let min = std::array::from_fn(|k| t[0][k].min(t[1][k]).min(t[2][k]));
    let max = std::array::from_fn(|k| t[0][k].max(t[1][k]).max(t[2][k]));
    (min, max)
}

fn grow(bounds: &mut ([f32; 3], [f32; 3]), other: ([f32; 3], [f32; 3])) {
    for k in 0..3 {
        bounds.0[k] = bounds.0[k].min(other.0[k]);
        bounds.1[k] = bounds.1[k].max(other.1[k]);
    }
}

const EMPTY_BOX: ([f32; 3], [f32; 3]) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);

fn box_area(b: &([f32; 3], [f32; 3])) -> f32 {
    let d = sub(b.1, b.0);
    2.0 * (d[0] * d[1] + d[1] * d[2] + d[2] * d[0])
}

fn range_box(tris: &[Tri]) -> ([f32; 3], [f32; 3]) {
    let mut b = EMPTY_BOX;
    for t in tris {
        grow(&mut b, tri_box(t));
    }
    b
}

fn longest_axis(min: [f32; 3], max: [f32; 3]) -> usize {
    let d = sub(max, min);
    if d[0] >= d[1] && d[0] >= d[2] {
        0
    } else if d[1] >= d[2] {
        1
    } else {
        2
    }
}

/// Squared distance from `p` to the box (0 inside).
fn box_distance_squared(p: [f32; 3], min: [f32; 3], max: [f32; 3]) -> f32 {
    let mut sum = 0.0;
    for k in 0..3 {
        let d = (min[k] - p[k]).max(p[k] - max[k]).max(0.0);
        sum += d * d;
    }
    sum
}

/// Splits `tris` in place at the index median along `axis` of the centroids and
/// returns the left count.
fn median_split(tris: &mut [Tri], axis: usize) -> usize {
    tris.sort_unstable_by(|a, b| tri_centroid(a)[axis].total_cmp(&tri_centroid(b)[axis]));
    tris.len() / 2
}

/// Binned surface-area-heuristic split on the longest centroid axis. Returns
/// the left count after partitioning `tris` in place, or `None` when no split
/// beats a leaf.
fn sah_split(tris: &mut [Tri], node_box: &([f32; 3], [f32; 3])) -> Option<usize> {
    let mut cmin = [f32::INFINITY; 3];
    let mut cmax = [f32::NEG_INFINITY; 3];
    for t in tris.iter() {
        let c = tri_centroid(t);
        for k in 0..3 {
            cmin[k] = cmin[k].min(c[k]);
            cmax[k] = cmax[k].max(c[k]);
        }
    }
    let axis = longest_axis(cmin, cmax);
    let extent = cmax[axis] - cmin[axis];
    if extent <= 0.0 {
        return None;
    }
    let scale = BINS as f32 / extent;
    let bin_of = |t: &Tri| (((tri_centroid(t)[axis] - cmin[axis]) * scale) as usize).min(BINS - 1);

    let mut counts = [0usize; BINS];
    let mut boxes = [EMPTY_BOX; BINS];
    for t in tris.iter() {
        let bin = bin_of(t);
        counts[bin] += 1;
        grow(&mut boxes[bin], tri_box(t));
    }
    // right_cost[s] is the cost of bins s.. as one child.
    let mut right_cost = [0.0f32; BINS];
    let mut acc = EMPTY_BOX;
    let mut acc_count = 0usize;
    for s in (1..BINS).rev() {
        if counts[s] > 0 {
            grow(&mut acc, boxes[s]);
        }
        acc_count += counts[s];
        right_cost[s] = if acc_count > 0 {
            box_area(&acc) * acc_count as f32
        } else {
            f32::INFINITY
        };
    }
    let mut best = (f32::INFINITY, 0usize);
    let mut acc = EMPTY_BOX;
    let mut left_count = 0usize;
    for s in 1..BINS {
        if counts[s - 1] > 0 {
            grow(&mut acc, boxes[s - 1]);
        }
        left_count += counts[s - 1];
        if left_count == 0 || left_count == tris.len() {
            continue;
        }
        let cost = box_area(&acc) * left_count as f32 + right_cost[s];
        if cost < best.0 {
            best = (cost, s);
        }
    }
    let leaf_cost = box_area(node_box) * tris.len() as f32;
    if best.0 >= leaf_cost {
        return None;
    }
    let split = best.1;
    let mut left = 0;
    for i in 0..tris.len() {
        if bin_of(&tris[i]) < split {
            tris.swap(i, left);
            left += 1;
        }
    }
    Some(left)
}

impl MeshBvh {
    /// Every mesh's triangles go in one tree.
    pub fn build(meshes: &[TriangleMesh]) -> Result<Self, PhysicsError> {
        let mut triangles: Vec<Tri> = Vec::new();
        for mesh in meshes {
            if mesh.vertices.iter().flatten().any(|c| !c.is_finite()) {
                return Err(PhysicsError::InvalidInput("mesh vertex is not finite"));
            }
            for tri in &mesh.triangles {
                let mut corners = [[0.0f32; 3]; 3];
                for (corner, &index) in corners.iter_mut().zip(tri) {
                    *corner = *mesh
                        .vertices
                        .get(index as usize)
                        .ok_or(PhysicsError::InvalidInput("mesh index out of range"))?;
                }
                triangles.push(corners);
            }
        }
        if triangles.is_empty() {
            return Err(PhysicsError::InvalidInput("mesh has no triangles"));
        }
        if triangles.len() > MAX_TRIANGLES {
            return Err(PhysicsError::InvalidInput(
                "mesh has too many triangles for the BVH depth limit",
            ));
        }

        let placeholder = BvhNode {
            min: [0.0; 3],
            max: [0.0; 3],
            first: 0,
            count: 0,
            dipole: [0.0; 3],
            centroid: [0.0; 3],
            radius: 0.0,
        };
        let mut nodes = vec![placeholder];
        // Triangle range [start, end) below each node.
        let mut ranges: Vec<(usize, usize)> = vec![(0, triangles.len())];
        // (node, depth with the root = 1)
        let mut work: Vec<(usize, usize)> = vec![(0, 1)];
        let mut depth = 1;
        while let Some((index, level)) = work.pop() {
            depth = depth.max(level);
            let (start, end) = ranges[index];
            let slice = &mut triangles[start..end];
            let node_box = range_box(slice);
            nodes[index].min = node_box.0;
            nodes[index].max = node_box.1;
            let n = slice.len();
            let left = if n <= BVH_LEAF_TRIANGLES {
                None
            } else if level >= MEDIAN_ONLY_DEPTH {
                Some(median_split(slice, longest_axis(node_box.0, node_box.1)))
            } else {
                Some(sah_split(slice, &node_box).unwrap_or_else(|| {
                    median_split(slice, longest_axis(node_box.0, node_box.1))
                }))
            };
            match left {
                None => {
                    nodes[index].first = start as u32;
                    nodes[index].count = n as u32;
                }
                Some(left) => {
                    let child = nodes.len();
                    nodes.push(placeholder);
                    nodes.push(placeholder);
                    ranges.push((start, start + left));
                    ranges.push((start + left, end));
                    nodes[index].first = child as u32;
                    nodes[index].count = 0;
                    work.push((child + 1, level + 1));
                    work.push((child, level + 1));
                }
            }
        }

        for (node, &(start, end)) in nodes.iter_mut().zip(&ranges) {
            let tris = &triangles[start..end];
            let mut dipole = [0.0f64; 3];
            let mut weighted = [0.0f64; 3];
            let mut plain = [0.0f64; 3];
            let mut area_sum = 0.0f64;
            for t in tris {
                let v = tri_area_vector(t);
                let area = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                let c = to_f64(tri_centroid(t));
                for k in 0..3 {
                    dipole[k] += v[k];
                    weighted[k] += area * c[k];
                    plain[k] += c[k];
                }
                area_sum += area;
            }
            // Zero-area triangles carry no weight; a node of only those uses
            // the plain mean so the centroid still sits on them.
            let centroid: [f64; 3] = if area_sum > 0.0 {
                weighted.map(|w| w / area_sum)
            } else {
                plain.map(|s| s / tris.len() as f64)
            };
            let mut radius = 0.0f64;
            for t in tris {
                for v in t {
                    let d: f64 = (0..3)
                        .map(|k| (f64::from(v[k]) - centroid[k]).powi(2))
                        .sum();
                    radius = radius.max(d.sqrt());
                }
            }
            node.dipole = dipole.map(|d| d as f32);
            node.centroid = centroid.map(|c| c as f32);
            // The f32 centroid differs from the f64 one by rounding; widen so
            // the sphere still covers every vertex.
            let c32 = node.centroid;
            let mut covered = 0.0f32;
            for t in tris {
                for v in t {
                    let d = sub(*v, c32);
                    covered = covered.max(dot(d, d).sqrt());
                }
            }
            node.radius = covered.max(radius as f32);
        }

        Ok(Self {
            nodes,
            triangles,
            depth: depth as u32,
        })
    }

    /// Root alone = 1.
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// The root's box, min then max.
    pub fn bounds(&self) -> [[f32; 3]; 2] {
        [self.nodes[0].min, self.nodes[0].max]
    }

    /// Unsigned distance from `p` to the nearest triangle, or `cap` when none
    /// is nearer. Any node whose box is at least the best distance so far away
    /// is skipped.
    pub fn nearest(&self, p: [f32; 3], cap: f32) -> f32 {
        let mut best2 = cap * cap;
        let mut hit = false;
        let mut stack = [0u32; BVH_MAX_DEPTH + 1];
        let mut top = 1;
        while top > 0 {
            top -= 1;
            let node = &self.nodes[stack[top] as usize];
            if box_distance_squared(p, node.min, node.max) >= best2 {
                continue;
            }
            if node.count > 0 {
                let first = node.first as usize;
                for t in &self.triangles[first..first + node.count as usize] {
                    let d2 = distance_squared_to_triangle(to_f64(p), &tri_f64(t)) as f32;
                    if d2 < best2 {
                        best2 = d2;
                        hit = true;
                    }
                }
            } else {
                let (l, r) = (node.first as usize, node.first as usize + 1);
                let dl = box_distance_squared(p, self.nodes[l].min, self.nodes[l].max);
                let dr = box_distance_squared(p, self.nodes[r].min, self.nodes[r].max);
                // The nearer child is popped first.
                let (far, near) = if dl <= dr { (r, l) } else { (l, r) };
                stack[top] = far as u32;
                stack[top + 1] = near as u32;
                top += 2;
            }
        }
        if hit { best2.sqrt() } else { cap }
    }

    /// Generalized winding number at `p`: about 1 inside an outward-wound
    /// closed surface, -1 inside an inside-out one, 0 outside, fractional near
    /// holes and open sheets. A far node adds its dipole term
    /// `dot(centroid - p, dipole) / (4 pi |centroid - p|^3)`, which matches the
    /// sign of `signed_solid_angle` (positive for a point inside an outward
    /// wound surface); otherwise its children are visited, and a near leaf adds
    /// each triangle's exact signed solid angle / 4 pi.
    pub fn winding(&self, p: [f32; 3]) -> f32 {
        let four_pi = 4.0 * std::f32::consts::PI;
        let mut sum = 0.0f32;
        let mut stack = [0u32; BVH_MAX_DEPTH + 1];
        stack[0] = 0;
        let mut top = 1;
        while top > 0 {
            top -= 1;
            let node = &self.nodes[stack[top] as usize];
            let d = sub(node.centroid, p);
            let r2 = dot(d, d);
            let reach = WINDING_BETA * node.radius;
            if r2 > reach * reach {
                sum += dot(d, node.dipole) / (four_pi * r2 * r2.sqrt());
            } else if node.count > 0 {
                let first = node.first as usize;
                for t in &self.triangles[first..first + node.count as usize] {
                    sum += signed_solid_angle(to_f64(p), &tri_f64(t)) as f32 / four_pi;
                }
            } else {
                stack[top] = node.first + 1;
                stack[top + 1] = node.first;
                top += 2;
            }
        }
        sum
    }

    /// Appends 4 vec4 per node: `[min.xyz, bitcast(first)]`,
    /// `[max.xyz, bitcast(count)]`, `[dipole.xyz, radius]`,
    /// `[centroid.xyz, 0]`.
    pub fn gpu_nodes(&self, out: &mut Vec<[f32; 4]>) {
        out.reserve(self.nodes.len() * 4);
        for n in &self.nodes {
            out.push([n.min[0], n.min[1], n.min[2], f32::from_bits(n.first)]);
            out.push([n.max[0], n.max[1], n.max[2], f32::from_bits(n.count)]);
            out.push([n.dipole[0], n.dipole[1], n.dipole[2], n.radius]);
            out.push([n.centroid[0], n.centroid[1], n.centroid[2], 0.0]);
        }
    }

    /// Appends 3 vec4 per triangle: `[a.xyz, 0]`, `[b.xyz, 0]`, `[c.xyz, 0]`.
    pub fn gpu_triangles(&self, out: &mut Vec<[f32; 4]>) {
        out.reserve(self.triangles.len() * 3);
        for t in &self.triangles {
            for v in t {
                out.push([v[0], v[1], v[2], 0.0]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit cube [0,1]^3 with each face split into an n x n grid of quads
    /// (2n^2 triangles per face), wound outward, or inside-out when `inward`.
    /// `skip_face` removes one of the six faces.
    fn cube(n: usize, inward: bool, skip_face: Option<usize>) -> TriangleMesh {
        let mut mesh = TriangleMesh {
            vertices: Vec::new(),
            triangles: Vec::new(),
        };
        for face in 0..6 {
            if Some(face) == skip_face {
                continue;
            }
            let axis = face / 2;
            let positive = face % 2 == 1;
            let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
            let point = |i: usize, j: usize| {
                let mut q = [0.0f32; 3];
                q[axis] = if positive { 1.0 } else { 0.0 };
                q[u] = i as f32 / n as f32;
                q[v] = j as f32 / n as f32;
                q
            };
            // u x v points along +axis, so the negative face flips.
            let flip = positive == inward;
            for i in 0..n {
                for j in 0..n {
                    let (p00, p10, p11, p01) =
                        (point(i, j), point(i + 1, j), point(i + 1, j + 1), point(i, j + 1));
                    let quads = [[p00, p10, p11], [p00, p11, p01]];
                    for mut t in quads {
                        if flip {
                            t.swap(1, 2);
                        }
                        let base = mesh.vertices.len() as u32;
                        mesh.vertices.extend_from_slice(&t);
                        mesh.triangles.push([base, base + 1, base + 2]);
                    }
                }
            }
        }
        mesh
    }

    fn grid(shift: f32) -> Vec<[f32; 3]> {
        let mut points = Vec::new();
        for i in 0..9 {
            for j in 0..9 {
                for k in 0..9 {
                    points.push([i, j, k].map(|c| -0.5 + 2.0 * c as f32 / 8.0 + shift));
                }
            }
        }
        points
    }

    fn brute_nearest(bvh: &MeshBvh, p: [f32; 3]) -> f32 {
        bvh.triangles
            .iter()
            .map(|t| distance_squared_to_triangle(to_f64(p), &tri_f64(t)))
            .fold(f64::INFINITY, f64::min)
            .sqrt() as f32
    }

    fn brute_winding(bvh: &MeshBvh, p: [f32; 3]) -> f32 {
        let sum: f64 = bvh
            .triangles
            .iter()
            .map(|t| signed_solid_angle(to_f64(p), &tri_f64(t)))
            .sum();
        (sum / (4.0 * std::f64::consts::PI)) as f32
    }

    #[test]
    fn nearest_matches_brute_force() {
        for mesh in [cube(1, false, None), cube(3, false, Some(4))] {
            let bvh = MeshBvh::build(&[mesh]).unwrap();
            for p in grid(0.0) {
                let expected = brute_nearest(&bvh, p);
                let got = bvh.nearest(p, 100.0);
                assert!((got - expected).abs() < 1e-5, "{p:?}: {got} vs {expected}");
                if expected > 0.05 {
                    assert_eq!(bvh.nearest(p, expected * 0.5), expected * 0.5);
                }
            }
        }
    }

    #[test]
    fn winding_matches_brute_force_and_signs() {
        let bvh = MeshBvh::build(&[cube(16, false, None)]).unwrap();
        // Shifted off the grid so no sample lies exactly on a face, where the
        // winding number is undefined.
        for p in grid(0.0137) {
            let expected = brute_winding(&bvh, p);
            let got = bvh.winding(p);
            // First order at WINDING_BETA is a few hundredths off beside a
            // face; the inside and shell tests only need it within ¼.
            assert!((got - expected).abs() < 0.05, "{p:?}: {got} vs {expected}");
        }
        let centre = [0.5; 3];
        assert!((bvh.winding(centre) - 1.0).abs() < 0.02);
        let inward = MeshBvh::build(&[cube(2, true, None)]).unwrap();
        assert!((inward.winding(centre) + 1.0).abs() < 0.02);
        let open = MeshBvh::build(&[cube(2, false, Some(5))]).unwrap();
        let w = open.winding(centre);
        assert!(w > 0.5 && w < 1.0, "{w}");
    }

    #[test]
    fn soup_builds_a_valid_bounded_tree() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 40) as f32 / (1u64 << 24) as f32
        };
        let mut mesh = TriangleMesh {
            vertices: Vec::new(),
            triangles: Vec::new(),
        };
        for i in 0..20_000u32 {
            let base = [next(), next(), next()];
            for _ in 0..3 {
                mesh.vertices
                    .push(std::array::from_fn(|k| base[k] + 0.02 * next()));
            }
            mesh.triangles.push([3 * i, 3 * i + 1, 3 * i + 2]);
        }
        let bvh = MeshBvh::build(&[mesh]).unwrap();
        assert!(bvh.depth() as usize <= BVH_MAX_DEPTH);
        let mut seen = vec![0u8; bvh.triangles.len()];
        for node in &bvh.nodes {
            if node.count == 0 {
                for child in [node.first as usize, node.first as usize + 1] {
                    let c = &bvh.nodes[child];
                    for k in 0..3 {
                        assert!(c.min[k] >= node.min[k] && c.max[k] <= node.max[k]);
                    }
                }
                continue;
            }
            assert!((1..=BVH_LEAF_TRIANGLES as u32).contains(&node.count));
            for i in node.first..node.first + node.count {
                seen[i as usize] += 1;
                let (lo, hi) = tri_box(&bvh.triangles[i as usize]);
                for k in 0..3 {
                    assert!(lo[k] >= node.min[k] && hi[k] <= node.max[k]);
                }
            }
        }
        assert!(seen.iter().all(|&s| s == 1));
    }

    #[test]
    fn build_refuses_bad_meshes() {
        let empty = TriangleMesh {
            vertices: Vec::new(),
            triangles: Vec::new(),
        };
        assert!(MeshBvh::build(&[empty]).is_err());
        let nan = TriangleMesh {
            vertices: vec![[0.0; 3], [1.0, 0.0, 0.0], [f32::NAN, 1.0, 0.0]],
            triangles: vec![[0, 1, 2]],
        };
        assert!(MeshBvh::build(&[nan]).is_err());
        let bad_index = TriangleMesh {
            vertices: vec![[0.0; 3]; 3],
            triangles: vec![[0, 1, 3]],
        };
        assert!(MeshBvh::build(&[bad_index]).is_err());
    }
}
