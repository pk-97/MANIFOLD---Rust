//! `MeshSource` — authored source geometry description carried on graph wires.
//!
//! This wire contains only source selectors and authored dimensions. It does
//! not carry GPU resources, solver state, transforms, or prepared proxies.

use std::sync::Arc;

use crate::mesh::{MeshVertex, PLATONIC_SHAPES};
use crate::platonic::platonic_mesh;
use crate::scene::curve_sweep::SweptCurve;
use crate::scene::mesh_bounds::MeshBounds;
use crate::scene::mesh_selection::MeshSelection;

/// Authored source geometry description for mesh-producing nodes.
#[derive(Clone, Debug, PartialEq)]
pub enum MeshSource {
    Cube {
        size: f32,
    },
    Platonic {
        shape: u32,
        radius: f32,
    },
    /// `node.plane_mesh`: the XY plane facing +Z, `width` by `height`.
    Plane {
        width: f32,
        height: f32,
    },
    /// `node.sphere_mesh`: a smooth UV sphere about the origin.
    Sphere {
        radius: f32,
        segments: u32,
        rings: u32,
    },
    /// A curve swept into a solid (`node.curve_solid_source`): the CPU
    /// mirror of the sweep and `node.make_triangles` the curve shape draws.
    Swept(Arc<SweptCurve>),
    Gltf {
        path: Arc<str>,
        /// The one file node whose mesh this is (an imported part), or `None`
        /// for what `selection` picks from the whole file.
        node_path: Option<Arc<str>>,
        selection: MeshSelection,
    },
    /// Piece `index` of `of` broken into `count` (Shatter): `of`'s
    /// triangles, each split in four `detail` times so a shape with few
    /// triangles still breaks into `count`, partitioned as an imported
    /// mesh's pieces are ([`crate::scene::mesh_selection::select_fragment`]).
    Fragment {
        of: Arc<MeshSource>,
        count: u32,
        index: u32,
        detail: u32,
    },
    /// A Terrain's ground (`node.terrain`): `points` by `points` heights, row
    /// by row in z, on a grid `size` across centred on the origin. It
    /// collides as its heights, never as triangles.
    HeightField {
        points: u32,
        size: f32,
        heights: Arc<[f32]>,
    },
}

/// The fewest triangles a piece is cut from, so a piece of a few-triangle
/// shape is still a chunk rather than a sliver.
const PIECE_TRIANGLES: u64 = 4;

/// How many times a shape of `triangles` triangles is split in four before
/// it breaks into `count` pieces: none when it already has enough.
pub fn fragment_detail(triangles: u64, count: u32) -> u32 {
    let wanted = u64::from(count) * PIECE_TRIANGLES;
    let mut detail = 0;
    let mut have = triangles.max(1);
    while have < wanted {
        have *= 4;
        detail += 1;
    }
    detail
}

/// Every triangle of `vertices` split in four at its edge midpoints,
/// `levels` times; each new corner is the mean of its edge's two corners,
/// normal renormalised (`node.subdivide_mesh`'s rule).
pub fn subdivide(mut vertices: Vec<MeshVertex>, levels: u32) -> Vec<MeshVertex> {
    for _ in 0..levels {
        let mid = |a: &MeshVertex, b: &MeshVertex| {
            let (a, b): (&[f32; 20], &[f32; 20]) = (bytemuck::cast_ref(a), bytemuck::cast_ref(b));
            let mut m: MeshVertex =
                bytemuck::cast(std::array::from_fn::<f32, 20, _>(|k| (a[k] + b[k]) * 0.5));
            let length = m.normal.iter().map(|v| v * v).sum::<f32>().sqrt();
            if length > 0.0 {
                m.normal = m.normal.map(|v| v / length);
            }
            m
        };
        let mut out = Vec::with_capacity(vertices.len() * 4);
        for tri in vertices.chunks_exact(3) {
            let [a, b, c] = [&tri[0], &tri[1], &tri[2]];
            let (ab, bc, ca) = (mid(a, b), mid(b, c), mid(c, a));
            out.extend_from_slice(&[*a, ab, ca, ab, *b, bc, ca, bc, *c, ab, bc, ca]);
        }
        vertices = out;
    }
    vertices
}

/// `vertices` without the zero-area triangles (a sphere's poles, a sweep's
/// doubled rim), which no piece can be cut from.
fn with_area(vertices: Vec<MeshVertex>) -> Vec<MeshVertex> {
    let area = |tri: &[MeshVertex]| {
        let [a, b, c] = [tri[0].position, tri[1].position, tri[2].position];
        let (u, v) = (
            [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
            [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
        );
        let n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        n[0] * n[0] + n[1] * n[1] + n[2] * n[2]
    };
    vertices
        .chunks_exact(3)
        .filter(|tri| area(tri) > 0.0)
        .flatten()
        .copied()
        .collect()
}

impl MeshSource {
    /// The box that contains every vertex the cube, platonic, sphere and plane nodes
    /// write, from their parameters alone. `None` for every other source (its
    /// extent needs the vertices) and for non-finite parameters.
    pub fn local_bounds(&self) -> Option<MeshBounds> {
        match self {
            Self::Cube { size } => {
                let half = size.abs() * 0.5;
                MeshBounds::of_positions([[-half; 3], [half; 3]])
            }
            Self::Sphere { radius, .. } => {
                let radius = radius.abs();
                MeshBounds::of_positions([[-radius; 3], [radius; 3]])
            }
            Self::Plane { width, height } => {
                let (x, y) = (width.abs() * 0.5, height.abs() * 0.5);
                MeshBounds::of_positions([[-x, -y, 0.0], [x, y, 0.0]])
            }
            // Circumradius one scaled by `radius`, so every vertex lies in
            // the cube of that half-extent.
            Self::Platonic { radius, .. } => {
                let radius = radius.abs();
                MeshBounds::of_positions([[-radius; 3], [radius; 3]])
            }
            _ => None,
        }
    }

    /// Load the immutable, local-space triangle-list vertices described by this
    /// source.  Selectors are validated here so every geometry consumer uses
    /// the same source contract before applying its own fixed transform.
    pub fn load_vertices(&self) -> Result<Vec<MeshVertex>, String> {
        let (shape, radius) = match self {
            Self::Plane { width, height } => return plane_vertices(*width, *height),
            Self::Sphere {
                radius,
                segments,
                rings,
            } => return sphere_vertices(*radius, *segments, *rings),
            Self::Swept(curve) => return curve.load_vertices(),
            Self::HeightField { .. } => {
                return Err("a Terrain collides as its heights, not as triangles".into());
            }
            Self::Fragment {
                of,
                count,
                index,
                detail,
            } => {
                let whole = subdivide(with_area(of.load_vertices()?), *detail);
                return crate::scene::mesh_selection::select_fragment(whole, *count, *index);
            }
            Self::Cube { size } => {
                if !size.is_finite() || *size <= 0.0 {
                    return Err("mesh source size must be finite and positive".into());
                }
                (1, *size * 3.0_f32.sqrt() / 2.0)
            }
            Self::Platonic { shape, radius } => {
                if !radius.is_finite() || *radius <= 0.0 {
                    return Err("mesh source size must be finite and positive".into());
                }
                (*shape, *radius)
            }
            Self::Gltf {
                path,
                node_path,
                selection,
            } => {
                if path.is_empty() {
                    return Err("mesh source has no file".into());
                }
                if selection.translate.iter().any(|value| !value.is_finite()) {
                    return Err("mesh source offsets must be finite".into());
                }
                if !(1..=manifold_foundation::mesh_partition::MAX_PIECES)
                    .contains(&selection.fragment_count)
                    || selection.fragment_index >= selection.fragment_count
                    || selection.mesh < -1
                    || selection.primitive < -1
                    || selection.material < -2
                {
                    return Err("mesh source has invalid selectors".into());
                }
                return selection
                    .load_node(std::path::Path::new(path.as_ref()), node_path.as_deref());
            }
        };
        if shape >= PLATONIC_SHAPES.len() as u32 {
            return Err("mesh source has an invalid builtin shape".into());
        }
        if !radius.is_finite() || radius <= 0.0 {
            return Err("mesh source size must be finite and positive".into());
        }
        let mut vertices = platonic_mesh(shape).to_vec();
        for vertex in &mut vertices {
            for axis in 0..3 {
                vertex.position[axis] *= radius;
            }
        }
        Ok(vertices)
    }
}

/// The two triangles of `node.plane_mesh`: the XY plane facing +Z.
fn plane_vertices(width: f32, height: f32) -> Result<Vec<MeshVertex>, String> {
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) {
        return Err("mesh source size must be finite and positive".into());
    }
    let (x, y) = (width * 0.5, height * 0.5);
    let corner = |px: f32, py: f32| {
        let mut vertex: MeshVertex = bytemuck::Zeroable::zeroed();
        vertex.position = [px, py, 0.0];
        vertex.normal = [0.0, 0.0, 1.0];
        vertex.uv = [px / width + 0.5, 0.5 - py / height];
        vertex.tangent = [1.0, 0.0, 0.0, 1.0];
        vertex.color = [1.0; 4];
        vertex
    };
    Ok(vec![
        corner(-x, -y),
        corner(x, -y),
        corner(x, y),
        corner(-x, -y),
        corner(x, y),
        corner(-x, y),
    ])
}

/// The triangle-list vertex count of `node.sphere_mesh`: a cap fan at each
/// pole and two triangles per quad between, `segments` around and `rings`
/// from pole to pole.
pub fn sphere_vertex_count(segments: u32, rings: u32) -> u32 {
    6 * segments * rings.saturating_sub(1)
}

/// Vertex `idx` of `node.sphere_mesh`, the CPU mirror of its WGSL body: a
/// smooth UV sphere of `radius` about the origin, outward wound, with
/// longitude-latitude UVs and the poles exact.
pub fn sphere_vertex(idx: u32, segments: u32, rings: u32, radius: f32) -> MeshVertex {
    let tri = idx / 3;
    let corner = idx % 3;
    let caps = segments;
    let total = 2 * segments * (rings - 1);
    // (ring, segment) of each corner; a pole corner takes the middle of its
    // segment so its UV sits under the fan.
    let (ring, seg, pole) = if tri < caps {
        let s = tri;
        [(0, s, true), (1, s + 1, false), (1, s, false)][corner as usize]
    } else if tri >= total - caps {
        let s = tri - (total - caps);
        [
            (rings - 1, s, false),
            (rings - 1, s + 1, false),
            (rings, s, true),
        ][corner as usize]
    } else {
        let band = tri - caps;
        let quad = band / 2;
        let (r, s) = (1 + quad / segments, quad % segments);
        let corners = if band.is_multiple_of(2) {
            [(r, s), (r, s + 1), (r + 1, s + 1)]
        } else {
            [(r, s), (r + 1, s + 1), (r + 1, s)]
        };
        let (r, s) = corners[corner as usize];
        (r, s, false)
    };
    let u = if pole {
        (seg as f32 + 0.5) / segments as f32
    } else {
        seg as f32 / segments as f32
    };
    let v = ring as f32 / rings as f32;
    let phi = std::f32::consts::TAU * u;
    let theta = std::f32::consts::PI * v;
    let (sin_phi, cos_phi) = phi.sin_cos();
    let normal = if ring == 0 {
        [0.0, 1.0, 0.0]
    } else if ring == rings {
        [0.0, -1.0, 0.0]
    } else {
        let (sin_theta, cos_theta) = theta.sin_cos();
        [sin_theta * cos_phi, cos_theta, sin_theta * sin_phi]
    };
    let mut vertex: MeshVertex = bytemuck::Zeroable::zeroed();
    vertex.position = normal.map(|axis| axis * radius);
    vertex.normal = normal;
    vertex.uv = [u, v];
    vertex.tangent = [-sin_phi, 0.0, cos_phi, 1.0];
    vertex.color = [1.0; 4];
    vertex
}

fn sphere_vertices(radius: f32, segments: u32, rings: u32) -> Result<Vec<MeshVertex>, String> {
    if !radius.is_finite() || radius <= 0.0 {
        return Err("mesh source size must be finite and positive".into());
    }
    if segments < 3 || rings < 2 {
        return Err("a sphere needs at least 3 segments and 2 rings".into());
    }
    Ok((0..sphere_vertex_count(segments, rings))
        .map(|idx| sphere_vertex(idx, segments, rings, radius))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bindings::NodeInputs, bindings::NodeOutputs, exec::backend::Backend,
        exec::backend::MockBackend, exec::execution_plan::ResourceId, ports::PortType,
    };

    fn source() -> MeshSource {
        MeshSource::Gltf {
            path: Arc::from("assets/mesh.glb"),
            node_path: None,
            selection: MeshSelection {
                mesh: 2,
                primitive: 3,
                material: 4,
                fit: true,
                recenter: false,
                translate: [1.0, 2.0, 3.0],
                fragment_count: 2,
                fragment_index: 1,
                own_space: false,
            },
        }
    }

    #[test]
    fn mesh_source_cpu_wire_round_trip_preserves_payload() {
        let mut backend = MockBackend::new();
        let slot = backend.acquire(ResourceId(0), PortType::MeshSource, None, (0, 0));
        let bindings: &[(&'static str, crate::bindings::Slot)] = &[("source", slot)];
        let mut scalar = Vec::new();
        let mut camera = Vec::new();
        let mut light = Vec::new();
        let mut material = Vec::new();
        let mut transform = Vec::new();
        let mut atmosphere = Vec::new();
        let mut render_mode = Vec::new();
        let mut object = Vec::new();
        let mut writes = Vec::new();
        let value = source();
        {
            let mut outputs = NodeOutputs::new(
                bindings,
                &backend,
                &mut scalar,
                &mut camera,
                &mut light,
                &mut material,
                &mut transform,
                &mut atmosphere,
                &mut render_mode,
                &mut object,
            )
            .with_mesh_source_writes(&mut writes);
            outputs.set_mesh_source("source", value.clone());
        }
        for (slot, value) in writes.drain(..) {
            backend.set_mesh_source(slot, value);
        }

        let inputs = NodeInputs::new(bindings, &backend, &[]);
        assert_eq!(inputs.mesh_source("source"), Some(value));
    }

    #[test]
    fn mesh_source_clone_shares_gltf_path_arc() {
        let value = source();
        let clone = value.clone();
        let (MeshSource::Gltf { path, .. }, MeshSource::Gltf { path: cloned, .. }) =
            (&value, &clone)
        else {
            unreachable!()
        };
        assert!(Arc::ptr_eq(path, cloned));
    }

    #[test]
    fn mesh_source_cube_size_matches_visible_bounds() {
        let vertices = MeshSource::Cube { size: 2.0 }.load_vertices().unwrap();
        let extent = vertices
            .iter()
            .map(|vertex| vertex.position[0].abs())
            .fold(0.0_f32, f32::max);
        assert!((extent - 1.0).abs() < 1.0e-6);
    }

    #[test]
    fn mesh_source_platonic_shape_and_radius_are_applied() {
        let vertices = MeshSource::Platonic {
            shape: 0,
            radius: 2.5,
        }
        .load_vertices()
        .unwrap();
        assert_eq!(vertices.len(), 12);
        let radius = vertices
            .iter()
            .map(|vertex| {
                vertex
                    .position
                    .iter()
                    .map(|value| value * value)
                    .sum::<f32>()
                    .sqrt()
            })
            .fold(0.0_f32, f32::max);
        assert!((radius - 2.5).abs() < 1.0e-6);
    }

    /// The sphere the collision shape reads: every vertex on the sphere, every
    /// triangle facing out, and every edge shared by exactly one other
    /// triangle running the other way (closed and consistently wound).
    #[test]
    fn mesh_source_sphere_is_closed_round_and_faces_out() {
        let (segments, rings, radius) = (12, 7, 0.5);
        let vertices = MeshSource::Sphere {
            radius,
            segments,
            rings,
        }
        .load_vertices()
        .unwrap();
        assert_eq!(vertices.len() as u32, sphere_vertex_count(segments, rings));
        let key = |p: [f32; 3]| p.map(|v| (v * 1.0e4).round() as i64);
        let mut edges = std::collections::HashMap::new();
        for tri in vertices.chunks(3) {
            let [a, b, c] = [tri[0].position, tri[1].position, tri[2].position];
            let (e1, e2) = (
                [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
                [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
            );
            let n = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            let centre = [0, 1, 2].map(|k| (a[k] + b[k] + c[k]) / 3.0);
            assert!(
                n.iter().zip(centre).map(|(n, c)| n * c).sum::<f32>() > 0.0,
                "a triangle faces in"
            );
            for (from, to) in [(a, b), (b, c), (c, a)] {
                *edges.entry((key(from), key(to))).or_insert(0) += 1;
            }
        }
        for vertex in &vertices {
            let length = vertex.position.iter().map(|v| v * v).sum::<f32>().sqrt();
            assert!((length - radius).abs() < 1.0e-5);
        }
        for ((from, to), count) in &edges {
            assert_eq!(*count, 1, "an edge is wound twice the same way");
            assert_eq!(edges.get(&(*to, *from)), Some(&1), "an open edge");
        }
    }

    #[test]
    fn mesh_source_rejects_invalid_dimensions_and_selectors() {
        assert!(
            MeshSource::Sphere {
                radius: 1.0,
                segments: 2,
                rings: 4
            }
            .load_vertices()
            .is_err()
        );
        assert!(MeshSource::Cube { size: 0.0 }.load_vertices().is_err());
        assert!(
            MeshSource::Platonic {
                shape: PLATONIC_SHAPES.len() as u32,
                radius: 1.0,
            }
            .load_vertices()
            .is_err()
        );
        assert!(
            MeshSource::Gltf {
                path: Arc::from("assets/mesh.glb"),
                node_path: None,
                selection: MeshSelection {
                    mesh: -1,
                    primitive: -1,
                    material: -1,
                    fit: false,
                    recenter: true,
                    translate: [0.0; 3],
                    fragment_count: 1,
                    fragment_index: 1,
                    own_space: false,
                },
            }
            .load_vertices()
            .is_err()
        );
    }

    /// The published box holds every vertex the CPU mirror of each analytic
    /// source lists, and the sources that need their vertices publish none.
    #[test]
    fn local_bounds_hold_every_analytic_vertex() {
        let sources = [
            MeshSource::Cube { size: 2.5 },
            MeshSource::Sphere {
                radius: 1.25,
                segments: 9,
                rings: 5,
            },
            MeshSource::Plane {
                width: 3.0,
                height: 0.4,
            },
        ]
        .into_iter()
        .chain(
            (0..PLATONIC_SHAPES.len() as u32)
                .map(|shape| MeshSource::Platonic { shape, radius: 1.7 }),
        );
        for source in sources {
            let bounds = source.local_bounds().expect("analytic source");
            for vertex in source.load_vertices().unwrap() {
                for axis in 0..3 {
                    let p = vertex.position[axis];
                    assert!(
                        p >= bounds.min[axis] - 1.0e-6 && p <= bounds.max[axis] + 1.0e-6,
                        "{source:?} axis {axis}: {p} outside {bounds:?}"
                    );
                }
            }
        }
        let plane = MeshSource::Plane {
            width: 3.0,
            height: 0.4,
        }
        .local_bounds()
        .unwrap();
        assert_eq!((plane.min, plane.max), ([-1.5, -0.2, 0.0], [1.5, 0.2, 0.0]));
        let flipped = MeshSource::Cube { size: -0.75 }.local_bounds().unwrap();
        assert_eq!((flipped.min, flipped.max), ([-0.375; 3], [0.375; 3]));
        assert_eq!(MeshSource::Cube { size: f32::NAN }.local_bounds(), None);
        let solid = MeshSource::Platonic {
            shape: 0,
            radius: -1.7,
        }
        .local_bounds()
        .unwrap();
        assert_eq!((solid.min, solid.max), ([-1.7; 3], [1.7; 3]));
        assert_eq!(
            MeshSource::Platonic {
                shape: 0,
                radius: f32::NAN
            }
            .local_bounds(),
            None
        );
    }

    #[test]
    fn mesh_source_release_and_clear_drop_values() {
        let mut backends: [Box<dyn Backend>; 2] = [
            Box::new(MockBackend::new()),
            Box::new(crate::exec::metal_backend::MetalBackend::without_device(
                1,
                1,
                manifold_gpu::GpuTextureFormat::Rgba16Float,
            )),
        ];
        for backend in &mut backends {
            for clear in [false, true] {
                let slot = backend.acquire(ResourceId(0), PortType::MeshSource, None, (0, 0));
                backend.set_mesh_source(slot, source());
                assert!(backend.mesh_source(slot).is_some());
                if clear {
                    backend.clear();
                } else {
                    backend.release(ResourceId(0), PortType::MeshSource, None, (0, 0));
                }
                assert!(backend.mesh_source(slot).is_none());
            }
        }
    }
}

#[cfg(test)]
mod fragment_tests {
    use super::*;

    fn area(vertices: &[MeshVertex]) -> f32 {
        vertices
            .chunks_exact(3)
            .map(|tri| {
                let [a, b, c] = [tri[0].position, tri[1].position, tri[2].position];
                let (u, v) = (
                    [b[0] - a[0], b[1] - a[1], b[2] - a[2]],
                    [c[0] - a[0], c[1] - a[1], c[2] - a[2]],
                );
                let n = [
                    u[1] * v[2] - u[2] * v[1],
                    u[2] * v[0] - u[0] * v[2],
                    u[0] * v[1] - u[1] * v[0],
                ];
                0.5 * (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt()
            })
            .sum()
    }

    /// A tetrahedron (four triangles) still breaks into 16 pieces: each
    /// triangle splits twice, every piece has triangles, and together the
    /// pieces are the whole surface. A sphere's zero-area pole triangles
    /// do not stop it breaking.
    #[test]
    fn a_few_triangle_shape_breaks_into_every_piece() {
        for whole in [
            MeshSource::Platonic {
                shape: 0,
                radius: 1.0,
            },
            MeshSource::Sphere {
                radius: 1.0,
                segments: 8,
                rings: 4,
            },
        ] {
            let triangles = (whole.load_vertices().unwrap().len() / 3) as u64;
            let count = 16;
            let detail = fragment_detail(triangles, count);
            let of = Arc::new(whole.clone());
            let pieces: Vec<Vec<MeshVertex>> = (0..count)
                .map(|index| {
                    MeshSource::Fragment {
                        of: Arc::clone(&of),
                        count,
                        index,
                        detail,
                    }
                    .load_vertices()
                    .unwrap()
                })
                .collect();
            assert!(
                pieces.iter().all(|piece| !piece.is_empty()),
                "{whole:?}: an empty piece"
            );
            let total: f32 = pieces.iter().map(|piece| area(piece)).sum();
            let surface = area(&whole.load_vertices().unwrap());
            assert!(
                (total - surface).abs() < 1e-3 * surface,
                "{whole:?}: pieces cover {total}, surface {surface}"
            );
        }
        assert_eq!(fragment_detail(4, 16), 2);
        assert_eq!(fragment_detail(10_000, 16), 0, "a dense mesh is not split");
    }
}
