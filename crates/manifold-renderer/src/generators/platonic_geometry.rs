//! Shared CPU geometry for the five Platonic solids.
//!
//! The point tables intentionally preserve the ordering used by
//! `polytope_vertices_body.wgsl` and `mesh_common::platonic_edges`.  The
//! triangle source is built from those same points, so CPU consumers and the
//! mesh upload primitive cannot drift apart.

use std::sync::OnceLock;

use bytemuck::Zeroable;

use crate::generators::mesh_common::MeshVertex;

/// Number of triangle-list vertices in the largest Platonic solid mesh.
/// The dodecahedron has twelve pentagonal faces, each triangulated as a fan.
pub const PLATONIC_MESH_CAPACITY: usize = 108;

const S: f32 = 0.577_350_26;
const ICOSA_A: f32 = 0.525_731_1;
const ICOSA_B: f32 = 0.850_650_8;
const DODECA_A: f32 = 0.934_172_33;
const DODECA_B: f32 = 0.356_822_1;

// Keep these tables in the exact order of polytope_vertices_body.wgsl.
const TETRA_POINTS: [[f32; 3]; 4] = [[S, S, S], [S, -S, -S], [-S, S, -S], [-S, -S, S]];

const CUBE_POINTS: [[f32; 3]; 8] = [
    [-S, -S, -S],
    [S, -S, -S],
    [S, S, -S],
    [-S, S, -S],
    [-S, -S, S],
    [S, -S, S],
    [S, S, S],
    [-S, S, S],
];

const OCTA_POINTS: [[f32; 3]; 6] = [
    [1.0, 0.0, 0.0],
    [-1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, -1.0, 0.0],
    [0.0, 0.0, 1.0],
    [0.0, 0.0, -1.0],
];

const ICOSA_POINTS: [[f32; 3]; 12] = [
    [-ICOSA_A, ICOSA_B, 0.0],
    [ICOSA_A, ICOSA_B, 0.0],
    [-ICOSA_A, -ICOSA_B, 0.0],
    [ICOSA_A, -ICOSA_B, 0.0],
    [0.0, -ICOSA_A, ICOSA_B],
    [0.0, ICOSA_A, ICOSA_B],
    [0.0, -ICOSA_A, -ICOSA_B],
    [0.0, ICOSA_A, -ICOSA_B],
    [ICOSA_B, 0.0, -ICOSA_A],
    [ICOSA_B, 0.0, ICOSA_A],
    [-ICOSA_B, 0.0, -ICOSA_A],
    [-ICOSA_B, 0.0, ICOSA_A],
];

const DODECA_POINTS: [[f32; 3]; 20] = [
    [S, S, S],
    [S, S, -S],
    [S, -S, S],
    [S, -S, -S],
    [-S, S, S],
    [-S, S, -S],
    [-S, -S, S],
    [-S, -S, -S],
    [0.0, DODECA_A, DODECA_B],
    [0.0, DODECA_A, -DODECA_B],
    [0.0, -DODECA_A, DODECA_B],
    [0.0, -DODECA_A, -DODECA_B],
    [DODECA_B, 0.0, DODECA_A],
    [DODECA_B, 0.0, -DODECA_A],
    [-DODECA_B, 0.0, DODECA_A],
    [-DODECA_B, 0.0, -DODECA_A],
    [DODECA_A, DODECA_B, 0.0],
    [DODECA_A, -DODECA_B, 0.0],
    [-DODECA_A, DODECA_B, 0.0],
    [-DODECA_A, -DODECA_B, 0.0],
];

/// Return the circumradius-one corner points for `shape`.
///
/// Shape indices are the public `PLATONIC_SHAPES` order. Out-of-range values
/// use the final entry, matching the existing edge source's safe fallback.
pub fn platonic_points(shape: u32) -> &'static [[f32; 3]] {
    match shape {
        0 => &TETRA_POINTS,
        1 => &CUBE_POINTS,
        2 => &OCTA_POINTS,
        3 => &ICOSA_POINTS,
        _ => &DODECA_POINTS,
    }
}

const TETRA_FACES: [[u8; 3]; 4] = [[2, 0, 1], [3, 1, 0], [3, 0, 2], [3, 2, 1]];
const CUBE_FACES: [[u8; 4]; 6] = [
    [3, 2, 1, 0],
    [4, 0, 1, 5],
    [4, 7, 3, 0],
    [5, 1, 2, 6],
    [6, 2, 3, 7],
    [7, 4, 5, 6],
];
const OCTA_FACES: [[u8; 3]; 8] = [
    [4, 0, 2],
    [5, 2, 0],
    [4, 3, 0],
    [5, 0, 3],
    [4, 2, 1],
    [5, 1, 2],
    [4, 1, 3],
    [5, 3, 1],
];
const ICOSA_FACES: [[u8; 3]; 20] = [
    [5, 1, 0],
    [7, 0, 1],
    [11, 5, 0],
    [10, 0, 7],
    [11, 0, 10],
    [9, 1, 5],
    [8, 7, 1],
    [9, 8, 1],
    [4, 2, 3],
    [6, 3, 2],
    [11, 2, 4],
    [10, 6, 2],
    [11, 10, 2],
    [9, 4, 3],
    [8, 3, 6],
    [9, 3, 8],
    [9, 5, 4],
    [11, 4, 5],
    [8, 6, 7],
    [10, 7, 6],
];
const DODECA_FACES: [[u8; 5]; 12] = [
    [9, 8, 0, 16, 1],
    [2, 17, 16, 0, 12],
    [4, 14, 12, 0, 8],
    [3, 13, 1, 16, 17],
    [5, 9, 1, 13, 15],
    [11, 3, 17, 2, 10],
    [6, 10, 2, 12, 14],
    [7, 15, 13, 3, 11],
    [9, 5, 18, 4, 8],
    [6, 14, 4, 18, 19],
    [7, 19, 18, 5, 15],
    [11, 10, 6, 19, 7],
];

#[inline]
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn unit(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / len, v[1] / len, v[2] / len]
}

#[inline]
fn triangle(points: &[[f32; 3]], a: u8, b: u8, c: u8, out: &mut Vec<MeshVertex>) {
    let p0 = points[a as usize];
    let p1 = points[b as usize];
    let p2 = points[c as usize];
    let normal = unit(cross(
        [p1[0] - p0[0], p1[1] - p0[1], p1[2] - p0[2]],
        [p2[0] - p0[0], p2[1] - p0[1], p2[2] - p0[2]],
    ));
    for position in [p0, p1, p2] {
        out.push(MeshVertex {
            position,
            _pad0: 0.0,
            normal,
            _pad1: 0.0,
            uv: [0.0, 0.0],
            _pad2: [0.0, 0.0],
            tangent: [0.0; 4],
                color: [1.0; 4],
});
    }
}

fn build_mesh(shape: u32) -> Box<[MeshVertex]> {
    let points = platonic_points(shape);
    let mut mesh = Vec::with_capacity(PLATONIC_MESH_CAPACITY.min(match shape {
        0 => 12,
        1 => 36,
        2 => 24,
        3 => 60,
        _ => PLATONIC_MESH_CAPACITY,
    }));
    match shape {
        0 => {
            for [a, b, c] in TETRA_FACES {
                triangle(points, a, b, c, &mut mesh);
            }
        }
        1 => {
            for [a, b, c, d] in CUBE_FACES {
                triangle(points, a, b, c, &mut mesh);
                triangle(points, a, c, d, &mut mesh);
            }
        }
        2 => {
            for [a, b, c] in OCTA_FACES {
                triangle(points, a, b, c, &mut mesh);
            }
        }
        3 => {
            for [a, b, c] in ICOSA_FACES {
                triangle(points, a, b, c, &mut mesh);
            }
        }
        _ => {
            for [a, b, c, d, e] in DODECA_FACES {
                triangle(points, a, b, c, &mut mesh);
                triangle(points, a, c, d, &mut mesh);
                triangle(points, a, d, e, &mut mesh);
            }
        }
    }
    mesh.into_boxed_slice()
}

/// Return the closed, outward-wound flat-normal triangle list at radius one.
pub fn platonic_mesh(shape: u32) -> &'static [MeshVertex] {
    static TETRA: OnceLock<Box<[MeshVertex]>> = OnceLock::new();
    static CUBE: OnceLock<Box<[MeshVertex]>> = OnceLock::new();
    static OCTA: OnceLock<Box<[MeshVertex]>> = OnceLock::new();
    static ICOSA: OnceLock<Box<[MeshVertex]>> = OnceLock::new();
    static DODECA: OnceLock<Box<[MeshVertex]>> = OnceLock::new();
    match shape {
        0 => TETRA.get_or_init(|| build_mesh(0)),
        1 => CUBE.get_or_init(|| build_mesh(1)),
        2 => OCTA.get_or_init(|| build_mesh(2)),
        3 => ICOSA.get_or_init(|| build_mesh(3)),
        _ => DODECA.get_or_init(|| build_mesh(4)),
    }
}

/// Compact CPU-origin upload payload. A full [`MeshVertex`] is 80 bytes, but
/// only position and normal are authored here. Keeping the source payload at
/// 108 × 32 bytes fits Metal's inline `setBytes` limit; the shader restores
/// the zero UV/tangent fields in the output buffer.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct UploadVertex {
    position: [f32; 3],
    _pad0: f32,
    normal: [f32; 3],
    _pad1: f32,
}

fn build_upload_bytes(shape: u32) -> Box<[u8]> {
    let mut source = [UploadVertex::zeroed(); PLATONIC_MESH_CAPACITY];
    for (dst, src) in source.iter_mut().zip(platonic_mesh(shape).iter()) {
        dst.position = src.position;
        dst.normal = src.normal;
    }
    bytemuck::cast_slice(&source).to_vec().into_boxed_slice()
}

/// Return the cached, padded compact payload used by the GPU upload bridge.
pub(crate) fn platonic_mesh_upload_bytes(shape: u32) -> &'static [u8] {
    static TETRA: OnceLock<Box<[u8]>> = OnceLock::new();
    static CUBE: OnceLock<Box<[u8]>> = OnceLock::new();
    static OCTA: OnceLock<Box<[u8]>> = OnceLock::new();
    static ICOSA: OnceLock<Box<[u8]>> = OnceLock::new();
    static DODECA: OnceLock<Box<[u8]>> = OnceLock::new();
    match shape {
        0 => TETRA.get_or_init(|| build_upload_bytes(0)),
        1 => CUBE.get_or_init(|| build_upload_bytes(1)),
        2 => OCTA.get_or_init(|| build_upload_bytes(2)),
        3 => ICOSA.get_or_init(|| build_upload_bytes(3)),
        _ => DODECA.get_or_init(|| build_upload_bytes(4)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const COUNTS: [(u32, usize, usize); 5] = [
        (0, 4, 12),
        (1, 8, 36),
        (2, 6, 24),
        (3, 12, 60),
        (4, 20, 108),
    ];

    #[test]
    fn points_are_circumradius_one_and_preserve_order() {
        for (shape, points, _) in COUNTS {
            let verts = platonic_points(shape);
            assert_eq!(verts.len(), points);
            for (index, p) in verts.iter().enumerate() {
                let radius = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
                assert!(
                    (radius - 1.0).abs() < 2.0e-6,
                    "shape={shape} point={index} radius={radius}"
                );
            }
        }
        assert_eq!(platonic_points(99), platonic_points(4));
    }

    #[test]
    fn meshes_have_expected_triangles_and_outward_flat_normals() {
        for (shape, _, vertex_count) in COUNTS {
            let mesh = platonic_mesh(shape);
            assert_eq!(mesh.len(), vertex_count);
            for triangle in mesh.chunks_exact(3) {
                let p = triangle[0].position;
                let q = triangle[1].position;
                let r = triangle[2].position;
                let n = cross(
                    [q[0] - p[0], q[1] - p[1], q[2] - p[2]],
                    [r[0] - p[0], r[1] - p[1], r[2] - p[2]],
                );
                let outward = n[0] * (p[0] + q[0] + r[0])
                    + n[1] * (p[1] + q[1] + r[1])
                    + n[2] * (p[2] + q[2] + r[2]);
                assert!(outward > 1.0e-5, "shape={shape} inward triangle");
                for vertex in triangle {
                    assert_eq!(vertex.normal, triangle[0].normal);
                }
                let nl = (triangle[0].normal[0].powi(2)
                    + triangle[0].normal[1].powi(2)
                    + triangle[0].normal[2].powi(2))
                .sqrt();
                assert!((nl - 1.0).abs() < 2.0e-6);
            }
        }
    }

    #[test]
    fn triangle_edges_form_closed_manifolds_without_duplicate_faces() {
        for (shape, _, _) in COUNTS {
            let mut edges: HashMap<([u32; 3], [u32; 3]), usize> = HashMap::new();
            let mut faces = std::collections::HashSet::new();
            for triangle in platonic_mesh(shape).chunks_exact(3) {
                let keys: [[u32; 3]; 3] =
                    std::array::from_fn(|index| triangle[index].position.map(f32::to_bits));
                let mut face = [keys[0], keys[1], keys[2]];
                face.sort();
                assert!(faces.insert(face), "shape={shape} duplicate triangle");
                for (a, b) in [(keys[0], keys[1]), (keys[1], keys[2]), (keys[2], keys[0])] {
                    let edge = if a <= b { (a, b) } else { (b, a) };
                    *edges.entry(edge).or_default() += 1;
                }
            }
            assert!(
                edges.values().all(|count| *count == 2),
                "shape={shape} open edge"
            );
        }
    }

    #[test]
    fn upload_payload_is_compact_and_padded() {
        for shape in 0..5 {
            assert_eq!(
                platonic_mesh_upload_bytes(shape).len(),
                PLATONIC_MESH_CAPACITY * 32
            );
        }
    }
}
