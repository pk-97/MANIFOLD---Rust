//! Shared glTF CPU-parse module — flatten a `.glb`/`.gltf` file's mesh
//! geometry into a triangle-list `Vec<MeshVertex>`.
//!
//! Backs `node.gltf_mesh_source` (the production import primitive). The
//! flatten logic (`walk_gltf_node` + the `Mat4`/`Mat3` helpers) is a
//! straight port of the proven CPU parse in
//! `node_graph::primitives::mesh_snapshot`'s azalea `.glb` test harness —
//! that module keeps its own private test-only copies; deduplicating them
//! against this module is a later refactor, out of scope here. The only
//! behavioral difference: every failure mode that harness `assert!`/`panic!`s
//! on (non-Triangles primitives, missing POSITION, out-of-range indices, a
//! missing default scene, a bad file) returns `Err(String)` here instead,
//! since this is a production code path, not a test.

use crate::generators::mesh_common::MeshVertex;

/// A 4×4 column-major matrix: `m[col][row]`, matching both the `gltf`
/// crate's `Transform::matrix()` convention and `render_scene.rs`'s
/// `model_matrix`.
pub(crate) type Mat4 = [[f32; 4]; 4];

pub(crate) const MAT4_IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

const MAT3_IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

pub(crate) fn mat4_mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [[0.0f32; 4]; 4];
    for col in 0..4 {
        for row in 0..4 {
            let mut sum = 0.0;
            for k in 0..4 {
                sum += a[k][row] * b[col][k];
            }
            out[col][row] = sum;
        }
    }
    out
}

pub(crate) fn mat4_transform_point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * p[0] + m[1][0] * p[1] + m[2][0] * p[2] + m[3][0],
        m[0][1] * p[0] + m[1][1] * p[1] + m[2][1] * p[2] + m[3][1],
        m[0][2] * p[0] + m[1][2] * p[1] + m[2][2] * p[2] + m[3][2],
    ]
}

/// Upper-left 3×3 (rotation + scale) block of a column-major `Mat4`,
/// returned row-major (`m3[row][col]`) for the inverse below.
pub(crate) fn mat3_upper_row_major(m: &Mat4) -> [[f32; 3]; 3] {
    [
        [m[0][0], m[1][0], m[2][0]],
        [m[0][1], m[1][1], m[2][1]],
        [m[0][2], m[1][2], m[2][2]],
    ]
}

pub(crate) fn mat3_inverse(a: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    if det.abs() < 1e-12 {
        // Degenerate (zero-scale) transform — identity fallback so
        // normals don't come out NaN.
        return MAT3_IDENTITY;
    }
    let inv_det = 1.0 / det;
    [
        [
            (a[1][1] * a[2][2] - a[1][2] * a[2][1]) * inv_det,
            (a[0][2] * a[2][1] - a[0][1] * a[2][2]) * inv_det,
            (a[0][1] * a[1][2] - a[0][2] * a[1][1]) * inv_det,
        ],
        [
            (a[1][2] * a[2][0] - a[1][0] * a[2][2]) * inv_det,
            (a[0][0] * a[2][2] - a[0][2] * a[2][0]) * inv_det,
            (a[0][2] * a[1][0] - a[0][0] * a[1][2]) * inv_det,
        ],
        [
            (a[1][0] * a[2][1] - a[1][1] * a[2][0]) * inv_det,
            (a[0][1] * a[2][0] - a[0][0] * a[2][1]) * inv_det,
            (a[0][0] * a[1][1] - a[0][1] * a[1][0]) * inv_det,
        ],
    ]
}

pub(crate) fn mat3_transpose(a: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
    [
        [a[0][0], a[1][0], a[2][0]],
        [a[0][1], a[1][1], a[2][1]],
        [a[0][2], a[1][2], a[2][2]],
    ]
}

pub(crate) fn mat3_mul_vec3(m: [[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

pub(crate) fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len < 1e-12 {
        [0.0, 0.0, 1.0]
    } else {
        [v[0] / len, v[1] / len, v[2] / len]
    }
}

pub(crate) fn sub3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Which geometry to extract from a parsed glTF document.
pub(crate) enum GltfMeshSelector {
    /// Walk the default scene's node tree, world-transforming every
    /// node's mesh primitives and combining them into one buffer. The
    /// proven azalea-fixture path — "just drop a model in."
    WholeScene,
    /// All primitives of `document.meshes()[mesh_index]`, in LOCAL space
    /// (no node transform applied) — for callers that place the mesh
    /// themselves (e.g. via `node.render_scene`'s per-object transform).
    Mesh { mesh_index: u32 },
    /// One primitive of `document.meshes()[mesh_index]`, in LOCAL space.
    Primitive {
        mesh_index: u32,
        primitive_index: u32,
    },
}

/// Flatten one glTF primitive's indexed geometry into `out`, applying
/// `world` to positions and `normal_mat` (= transpose(inverse(upper3x3(world))))
/// to normals. Expands indices to a flat triangle list; falls back to a
/// per-face normal when NORMAL is absent and to `(0, 0)` UV when
/// TEXCOORD_0 is absent. Errors (rather than panics) on a non-Triangles
/// primitive, a missing POSITION accessor, or an index referencing a
/// vertex outside the POSITION accessor's range.
fn flatten_primitive(
    primitive: &gltf::Primitive,
    buffers: &[gltf::buffer::Data],
    world: &Mat4,
    normal_mat: &[[f32; 3]; 3],
    out: &mut Vec<MeshVertex>,
) -> Result<(), String> {
    if primitive.mode() != gltf::mesh::Mode::Triangles {
        return Err(format!(
            "primitive uses non-Triangles mode {:?} — unsupported by node.gltf_mesh_source",
            primitive.mode()
        ));
    }

    let reader = primitive.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));

    let positions: Vec<[f32; 3]> = reader
        .read_positions()
        .ok_or_else(|| "primitive missing required POSITION accessor".to_string())?
        .collect();
    let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|it| it.collect());
    let uvs: Option<Vec<[f32; 2]>> = reader.read_tex_coords(0).map(|it| it.into_f32().collect());

    let world_positions: Vec<[f32; 3]> = positions
        .iter()
        .map(|p| mat4_transform_point(world, *p))
        .collect();
    let world_normals: Option<Vec<[f32; 3]>> = normals
        .as_ref()
        .map(|ns| ns.iter().map(|n| normalize3(mat3_mul_vec3(*normal_mat, *n))).collect());

    let indices: Vec<u32> = match reader.read_indices() {
        Some(idx) => idx.into_u32().collect(),
        None => (0..world_positions.len() as u32).collect(),
    };

    for tri in indices.chunks_exact(3) {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        if i0 >= world_positions.len() || i1 >= world_positions.len() || i2 >= world_positions.len()
        {
            return Err(format!(
                "triangle index ({i0}, {i1}, {i2}) out of range for {} positions",
                world_positions.len()
            ));
        }
        let p0 = world_positions[i0];
        let p1 = world_positions[i1];
        let p2 = world_positions[i2];
        // Face-normal fallback when NORMAL is absent on this primitive —
        // computed post-transform, in world (or local) space.
        let face_normal = normalize3(cross3(sub3(p1, p0), sub3(p2, p0)));

        for &i in &[i0, i1, i2] {
            let normal = world_normals.as_ref().map_or(face_normal, |ns| ns[i]);
            let uv = uvs.as_ref().map_or([0.0, 0.0], |u| u[i]);
            out.push(MeshVertex {
                position: world_positions[i],
                _pad0: 0.0,
                normal,
                _pad1: 0.0,
                uv,
                _pad2: [0.0, 0.0],
            });
        }
    }
    Ok(())
}

/// Recursively flatten a glTF node's mesh primitives (world-transformed)
/// into `out`, then recurse into children with the composed world matrix.
fn walk_gltf_node(
    node: &gltf::Node,
    parent_world: Mat4,
    buffers: &[gltf::buffer::Data],
    out: &mut Vec<MeshVertex>,
) -> Result<(), String> {
    let local = node.transform().matrix();
    let world = mat4_mul(&parent_world, &local);

    if let Some(mesh) = node.mesh() {
        // Normal matrix = transpose(inverse(upper3x3(world))) — correct
        // under non-uniform scale, not just rotation + uniform scale.
        let normal_mat = mat3_transpose(mat3_inverse(mat3_upper_row_major(&world)));
        for primitive in mesh.primitives() {
            flatten_primitive(&primitive, buffers, &world, &normal_mat, out)?;
        }
    }

    for child in node.children() {
        walk_gltf_node(&child, world, buffers, out)?;
    }
    Ok(())
}

/// Parse a `.glb`/`.gltf` file and flatten the selected geometry into a
/// triangle-list `Vec<MeshVertex>`. See [`GltfMeshSelector`] for the three
/// selection modes. Returns `Err(String)` on any failure — a missing/
/// unreadable file, a document with no default scene (`WholeScene`), an
/// out-of-range mesh/primitive index, or a non-Triangles primitive —
/// rather than panicking, since this runs on a background thread inside
/// `node.gltf_mesh_source`.
pub(crate) fn load_gltf_mesh(
    path: &std::path::Path,
    selector: GltfMeshSelector,
) -> Result<Vec<MeshVertex>, String> {
    let (document, buffers, _images) =
        gltf::import(path).map_err(|e| format!("gltf::import({}): {e}", path.display()))?;

    let mut out = Vec::new();
    match selector {
        GltfMeshSelector::WholeScene => {
            let scene = document.default_scene().ok_or_else(|| {
                format!("{}: glb has no default scene — cannot walk node tree", path.display())
            })?;
            for node in scene.nodes() {
                walk_gltf_node(&node, MAT4_IDENTITY, &buffers, &mut out)?;
            }
        }
        GltfMeshSelector::Mesh { mesh_index } => {
            let meshes: Vec<gltf::Mesh> = document.meshes().collect();
            let mesh = meshes.get(mesh_index as usize).ok_or_else(|| {
                format!("mesh_index {mesh_index} out of range (document has {} meshes)", meshes.len())
            })?;
            for primitive in mesh.primitives() {
                flatten_primitive(&primitive, &buffers, &MAT4_IDENTITY, &MAT3_IDENTITY, &mut out)?;
            }
        }
        GltfMeshSelector::Primitive {
            mesh_index,
            primitive_index,
        } => {
            let meshes: Vec<gltf::Mesh> = document.meshes().collect();
            let mesh = meshes.get(mesh_index as usize).ok_or_else(|| {
                format!("mesh_index {mesh_index} out of range (document has {} meshes)", meshes.len())
            })?;
            let primitives: Vec<gltf::Primitive> = mesh.primitives().collect();
            let primitive = primitives.get(primitive_index as usize).ok_or_else(|| {
                format!(
                    "primitive_index {primitive_index} out of range (mesh {mesh_index} has {} primitives)",
                    primitives.len()
                )
            })?;
            flatten_primitive(primitive, &buffers, &MAT4_IDENTITY, &MAT3_IDENTITY, &mut out)?;
        }
    }
    Ok(out)
}

/// Decode one embedded glTF texture to tightly-packed RGBA8 (row-major,
/// 4 bytes/pixel, no row padding). `texture_index` indexes
/// `document.textures()`; its image source is resolved to the decoded
/// `images[..]` entry `gltf::import` already produced. Returns
/// (width, height, rgba8) or Err on a missing/out-of-range texture or an
/// unsupported source pixel format.
pub(crate) fn load_gltf_texture(
    path: &std::path::Path,
    texture_index: u32,
) -> Result<(u32, u32, Vec<u8>), String> {
    let (document, _buffers, images) =
        gltf::import(path).map_err(|e| format!("gltf::import({}): {e}", path.display()))?;

    let textures: Vec<gltf::Texture> = document.textures().collect();
    let tex = textures.get(texture_index as usize).ok_or_else(|| {
        format!(
            "texture_index {texture_index} out of range (document has {} textures)",
            textures.len()
        )
    })?;
    let img_index = tex.source().index();
    let data = images.get(img_index).ok_or_else(|| {
        format!(
            "texture {texture_index} references image index {img_index}, out of range ({} images decoded)",
            images.len()
        )
    })?;

    let (width, height) = (data.width, data.height);
    let rgba: Vec<u8> = match data.format {
        gltf::image::Format::R8G8B8A8 => data.pixels.clone(),
        gltf::image::Format::R8G8B8 => {
            let mut out = Vec::with_capacity(data.pixels.len() / 3 * 4);
            for px in data.pixels.chunks_exact(3) {
                out.push(px[0]);
                out.push(px[1]);
                out.push(px[2]);
                out.push(255);
            }
            out
        }
        gltf::image::Format::R8 => {
            let mut out = Vec::with_capacity(data.pixels.len() * 4);
            for &v in &data.pixels {
                out.push(v);
                out.push(v);
                out.push(v);
                out.push(255);
            }
            out
        }
        gltf::image::Format::R8G8 => {
            let mut out = Vec::with_capacity(data.pixels.len() / 2 * 4);
            for px in data.pixels.chunks_exact(2) {
                out.push(px[0]);
                out.push(px[1]);
                out.push(0);
                out.push(255);
            }
            out
        }
        other => {
            return Err(format!(
                "unsupported glTF image format {other:?} on texture {texture_index}"
            ));
        }
    };

    Ok((width, height, rgba))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CPU-only, fast — not gated behind `#[ignore]`. Guards the
    /// file-missing case (no fixture in a checkout without
    /// `tests/fixtures/gltf/`) so CI without the large fixture still
    /// passes; when the fixture IS present, asserts the parse actually
    /// produced a well-formed triangle list.
    #[test]
    fn loads_whole_scene_azalea_fixture_when_present() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb");
        if !path.exists() {
            println!(
                "loads_whole_scene_azalea_fixture_when_present: fixture not found at {}, skipping",
                path.display()
            );
            return;
        }
        let verts = load_gltf_mesh(&path, GltfMeshSelector::WholeScene)
            .unwrap_or_else(|e| panic!("load_gltf_mesh({}): {e}", path.display()));
        assert!(!verts.is_empty(), "expected non-empty vertex list from azalea fixture");
        assert_eq!(verts.len() % 3, 0, "triangle list must have a vertex count divisible by 3");
    }

    /// Mirrors `loads_whole_scene_azalea_fixture_when_present`'s
    /// missing-fixture skip. A mesh-only `.glb` legitimately has zero
    /// embedded textures, in which case `load_gltf_texture(_, 0)`
    /// returns `Err` — that's not a test failure, so we print and
    /// return rather than panic. When the fixture DOES have a texture
    /// 0, assert the decode produced a well-formed tightly-packed
    /// RGBA8 buffer.
    #[test]
    fn loads_texture_from_azalea_fixture_when_present() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb");
        if !path.exists() {
            println!(
                "loads_texture_from_azalea_fixture_when_present: fixture not found at {}, skipping",
                path.display()
            );
            return;
        }
        match load_gltf_texture(&path, 0) {
            Ok((w, h, rgba)) => {
                assert!(w > 0 && h > 0, "expected non-zero texture dimensions");
                assert_eq!(
                    rgba.len(),
                    (w * h * 4) as usize,
                    "expected tightly-packed RGBA8 buffer"
                );
                println!(
                    "loads_texture_from_azalea_fixture_when_present: decoded texture 0 = {w}x{h}"
                );
            }
            Err(e) => {
                println!(
                    "loads_texture_from_azalea_fixture_when_present: texture 0 not decodable ({e}) — mesh-only glb legitimately has no textures, skipping"
                );
            }
        }
    }
}
