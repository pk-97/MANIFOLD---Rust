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
    /// Every primitive in the default scene whose material index equals
    /// `material_index`, world-transformed and combined into one buffer —
    /// the importer's per-material object (one `render_scene` object per
    /// distinct material). Walks the node tree exactly like `WholeScene`
    /// but keeps only matching primitives.
    Material { material_index: u32 },
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
/// When `material_filter` is `Some(idx)`, only primitives whose material
/// index equals `idx` are flattened (the importer's per-material object);
/// `None` takes every primitive (the whole-scene combine).
fn walk_gltf_node(
    node: &gltf::Node,
    parent_world: Mat4,
    buffers: &[gltf::buffer::Data],
    material_filter: Option<u32>,
    out: &mut Vec<MeshVertex>,
) -> Result<(), String> {
    let local = node.transform().matrix();
    let world = mat4_mul(&parent_world, &local);

    if let Some(mesh) = node.mesh() {
        // Normal matrix = transpose(inverse(upper3x3(world))) — correct
        // under non-uniform scale, not just rotation + uniform scale.
        let normal_mat = mat3_transpose(mat3_inverse(mat3_upper_row_major(&world)));
        for primitive in mesh.primitives() {
            if let Some(want) = material_filter {
                // `material().index()` is `None` for the glTF default
                // material; a numeric filter never matches that.
                if primitive.material().index() != Some(want as usize) {
                    continue;
                }
            }
            flatten_primitive(&primitive, buffers, &world, &normal_mat, out)?;
        }
    }

    for child in node.children() {
        walk_gltf_node(&child, world, buffers, material_filter, out)?;
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
                walk_gltf_node(&node, MAT4_IDENTITY, &buffers, None, &mut out)?;
            }
        }
        GltfMeshSelector::Material { material_index } => {
            let scene = document.default_scene().ok_or_else(|| {
                format!("{}: glb has no default scene — cannot walk node tree", path.display())
            })?;
            for node in scene.nodes() {
                walk_gltf_node(&node, MAT4_IDENTITY, &buffers, Some(material_index), &mut out)?;
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

/// One distinct glTF material that has geometry, plus everything the
/// importer needs to build its `render_scene` object: the PBR factors,
/// the base-color texture index (into `document.textures()`), the alpha
/// mode, and the exact world-combined triangle-list vertex count (which
/// sizes that object's `gltf_mesh_source.max_capacity`).
// Stage 1 output — consumed by `gltf_import::assemble_import_graph`, which
// the `manifold-app` file-drop handler calls in production.
#[derive(Debug, Clone)]
pub(crate) struct GltfMaterialInfo {
    pub material_index: u32,
    pub name: Option<String>,
    pub base_color_factor: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: [f32; 3],
    /// glTF `alphaMode == MASK` — drives `render_scene`'s cutout discard.
    pub alpha_mask: bool,
    pub alpha_cutoff: f32,
    /// Index into `document.textures()` for the base-color map, if any.
    pub base_color_texture: Option<u32>,
    /// Index into `document.textures()` for the tangent-space normal map,
    /// if any (glTF `normalTexture`) — IMPORT_FIDELITY_DESIGN.md D3/D5/D6.
    pub normal_texture: Option<u32>,
    /// glTF `normalTexture.scale` (default 1.0). `render_scene` wires no
    /// port for it yet (no per-object normal-intensity multiplier exists;
    /// adding one is shader-ABI scope, out of bounds for this phase) — the
    /// importer reads this field to emit a D9 report line whenever it
    /// deviates from neutral, so it is never a silent drop even though it
    /// isn't applied.
    pub normal_scale: f32,
    /// Index into `document.textures()` for the glTF metallic-roughness map
    /// (G = roughness, B = metallic), if any.
    pub mr_texture: Option<u32>,
    /// Index into `document.textures()` for the occlusion map (R channel),
    /// if any. May be the SAME texture index as `mr_texture` (ORM packing)
    /// — the importer wires one source node into both ports in that case.
    pub occlusion_texture: Option<u32>,
    /// glTF `occlusionTexture.strength` (default 1.0) — same "no wired port
    /// yet, reported instead" note as `normal_scale`.
    pub occlusion_strength: f32,
    /// Index into `document.textures()` for the emissive map, if any.
    pub emissive_texture: Option<u32>,
    /// `KHR_materials_emissive_strength` multiplier (1.0 when the
    /// extension is absent) — the importer folds this into the imported
    /// `emission_intensity`, an existing wired param.
    pub emissive_strength: f32,
    /// `KHR_materials_transmission` factor (0.0 when the extension is
    /// absent). IMPORT_FIDELITY_DESIGN.md D8/F-P5: a nonzero factor makes
    /// the importer map this material to `Blend`, with
    /// `alpha = base_color.a * (1 - transmission_factor)`.
    pub transmission_factor: f32,
    /// `KHR_materials_clearcoat`'s `clearcoatFactor` (default `0.0` — glTF's
    /// own implicit default, and the value that makes G-P5's coat lobe
    /// exactly inert). GLB_CONFORMANCE_DESIGN.md G-P5/D5: parsed by raw
    /// `extensions` JSON, not a typed accessor — gltf 1.4.1 has no
    /// `KHR_materials_clearcoat` feature (VERIFIED against the pinned
    /// crate's own `Cargo.toml`/source this session: no such feature name
    /// exists in either `gltf` or `gltf-json` 1.4.1, unlike `specular`/
    /// `ior` in G-P4 — see the G-P5 execution report). Same raw-JSON-sniff
    /// doctrine `specular_has_texture` already uses for map presence.
    pub clearcoat_factor: f32,
    /// `KHR_materials_clearcoat`'s `clearcoatRoughnessFactor` (default
    /// `0.0`).
    pub clearcoat_roughness_factor: f32,
    /// `true` when `KHR_materials_clearcoat` carries a `clearcoatTexture`,
    /// `clearcoatRoughnessTexture`, and/or `clearcoatNormalTexture` — v1
    /// maps the FACTORS only (D5); a textured coat is unmapped and
    /// reported rather than silently dropped (Deferred #2 in
    /// GLB_CONFORMANCE_DESIGN.md owns map-driven coat).
    pub clearcoat_has_texture: bool,
    /// glTF `alphaMode == BLEND` on the source material. F-P4 downgrades
    /// these to `alpha_mask` cutout (the F-P5 stopgap — `alpha_mask` above
    /// is already `true` when this is) and the importer emits a report
    /// line noting the downgrade.
    pub was_blend: bool,
    /// `KHR_materials_ior`'s `ior` (default 1.5 — glTF's implicit default,
    /// and the value that makes `dielectric_f0` below collapse to today's
    /// hardcoded 0.04). GLB_CONFORMANCE_DESIGN.md G-P4/D5.
    pub ior: f32,
    /// `KHR_materials_specular`'s `specularFactor` (default 1.0).
    pub specular_factor: f32,
    /// `KHR_materials_specular`'s `specularColorFactor` (default
    /// `[1,1,1]`). `specular_texture`/`specular_color_texture` (map
    /// variants of the same extension) are report-only in v1 — factor
    /// only, same doctrine as clearcoat — see `specular_has_texture`.
    pub specular_color_factor: [f32; 3],
    /// `true` when `KHR_materials_specular` carries a `specularTexture`
    /// and/or `specularColorTexture` — v1 maps the FACTOR only (see
    /// `specular_factor`/`specular_color_factor`), so a textured specular
    /// map is unmapped and reported rather than silently dropped.
    pub specular_has_texture: bool,
    /// `KHR_texture_transform` on the base-color texture reference, folded
    /// ONCE here (not per frame — CLAUDE.md hot-path discipline) into a
    /// 2×3 affine `[m00, m01, m10, m11, tx, ty]` s.t.
    /// `uv' = (m00*u + m01*v + tx, m10*u + m11*v + ty)`. Identity
    /// (`[1,0,0,1,0,0]`) when the extension is absent on this texture
    /// reference — byte-identical to no transform. GLB_CONFORMANCE_DESIGN.md
    /// G-P4: EVERY map family carries its own transform (the four fields
    /// below) — the AMG puts transforms on 9 normalTexture infos and only
    /// 1 baseColorTexture, so base-color-only would leave its normal maps
    /// sampling untransformed UVs. A `texCoord` index override inside any
    /// map's transform is a report line (nothing silently dropped) — see
    /// `uv_tex_coord_override`.
    pub base_color_uv_transform: [f32; 6],
    /// Same folded affine for the normal map's `KHR_texture_transform`
    /// (gltf 1.4.1's `NormalTexture` has no typed accessor — parsed from
    /// the raw `extensions` JSON via [`parse_uv_transform_json`]).
    pub normal_uv_transform: [f32; 6],
    /// Same for the metallic-roughness map (typed accessor).
    pub mr_uv_transform: [f32; 6],
    /// Same for the occlusion map (raw JSON, like normal).
    pub occlusion_uv_transform: [f32; 6],
    /// Same for the emissive map (typed accessor).
    pub emissive_uv_transform: [f32; 6],
    /// `true` when ANY map's `KHR_texture_transform` specifies a
    /// `texCoord` override (i.e. samples `TEXCOORD_n, n>0`) — v1 has one
    /// UV channel end to end (`MeshVertex` carries a single `uv`), so a
    /// texCoord override can't be honoured. Report-only.
    pub uv_tex_coord_override: bool,
    pub vertex_count: u32,
}

/// Fold glTF `KHR_texture_transform`'s `(offset, rotation, scale)` into the
/// affine `[m00, m01, m10, m11, tx, ty]` `resolve_albedo` applies as
/// `uv' = M*uv + t`. Matches the spec's documented composition order,
/// `translation * rotation * scale` applied to the UV as a column vector
/// (scale innermost, then rotate, then translate) — verified against
/// three.js/Babylon's glTF loaders and the extension README's worked
/// matrices. Identity in, identity out (`sin(0)==0.0`, `cos(0)==1.0`
/// exactly in f32), so an absent extension is byte-identical.
pub(crate) fn fold_uv_transform(offset: [f32; 2], rotation: f32, scale: [f32; 2]) -> [f32; 6] {
    let (s, c) = rotation.sin_cos();
    [
        scale[0] * c,
        -scale[1] * s,
        scale[0] * s,
        scale[1] * c,
        offset[0],
        offset[1],
    ]
}

pub(crate) const IDENTITY_UV_TRANSFORM: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// Parse a raw `KHR_texture_transform` extension JSON object (the shape the
/// spec defines: optional `offset: [f32; 2]`, `rotation: f32`,
/// `scale: [f32; 2]`, `texCoord: u32`) and fold it via
/// [`fold_uv_transform`]. Returns `(folded_affine, has_tex_coord_override)`.
/// Needed for the normal and occlusion maps, whose gltf-1.4.1 wrappers
/// (`NormalTexture`/`OcclusionTexture`) expose no typed
/// `texture_transform()` — only `extension_value()` raw JSON. Spec defaults
/// on absent fields (offset `[0,0]`, rotation `0`, scale `[1,1]`) match the
/// typed accessor's defaults exactly.
pub(crate) fn parse_uv_transform_json(v: &serde_json::Value) -> ([f32; 6], bool) {
    let f2 = |key: &str, default: [f32; 2]| -> [f32; 2] {
        v.get(key)
            .and_then(|a| a.as_array())
            .and_then(|a| {
                Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32])
            })
            .unwrap_or(default)
    };
    let offset = f2("offset", [0.0, 0.0]);
    let scale = f2("scale", [1.0, 1.0]);
    let rotation = v.get("rotation").and_then(|r| r.as_f64()).unwrap_or(0.0) as f32;
    let tex_coord_override = v.get("texCoord").is_some();
    (fold_uv_transform(offset, rotation, scale), tex_coord_override)
}

/// What the importer needs to know about a glb up front: the distinct
/// materials that actually carry geometry (each becomes one
/// `render_scene` object), the world-space bounding box (for the default
/// framing camera + recentre), and counts of glTF cameras and
/// default-material (unassigned) geometry so the importer can report what
/// it did and didn't handle.
#[derive(Debug, Clone)]
pub(crate) struct GltfImportSummary {
    pub materials: Vec<GltfMaterialInfo>,
    pub bbox_min: [f32; 3],
    pub bbox_max: [f32; 3],
    pub camera_count: usize,
    /// Triangle-list vertices belonging to primitives with NO material
    /// (glTF default material). v1 does not import these — reported so the
    /// caller can warn rather than silently drop them.
    pub default_material_vertex_count: u32,
}

/// Recursively accumulate per-material world-combined vertex counts and a
/// world-space bounding box over a node subtree. Keyed by
/// `material().index()` (`None` = glTF default material).
fn summarize_node(
    node: &gltf::Node,
    parent_world: Mat4,
    buffers: &[gltf::buffer::Data],
    per_material: &mut std::collections::BTreeMap<Option<usize>, u32>,
    bbox_min: &mut [f32; 3],
    bbox_max: &mut [f32; 3],
) {
    let local = node.transform().matrix();
    let world = mat4_mul(&parent_world, &local);

    if let Some(mesh) = node.mesh() {
        for prim in mesh.primitives() {
            let reader = prim.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let positions: Vec<[f32; 3]> = positions.collect();
            // Triangle-list vertex count = index count (or position count
            // when non-indexed), matching what `flatten_primitive` emits.
            let vcount = match reader.read_indices() {
                Some(idx) => idx.into_u32().count() as u32,
                None => positions.len() as u32,
            };
            *per_material.entry(prim.material().index()).or_insert(0) += vcount;
            for p in &positions {
                let wp = mat4_transform_point(&world, *p);
                for i in 0..3 {
                    bbox_min[i] = bbox_min[i].min(wp[i]);
                    bbox_max[i] = bbox_max[i].max(wp[i]);
                }
            }
        }
    }

    for child in node.children() {
        summarize_node(&child, world, buffers, per_material, bbox_min, bbox_max);
    }
}

/// Parse a glb's structure for the importer: the distinct materials with
/// geometry, the world-space bbox, camera count, and unassigned-geometry
/// count. One parse; no GPU. See [`GltfImportSummary`].
pub(crate) fn gltf_import_summary(path: &std::path::Path) -> Result<GltfImportSummary, String> {
    let (document, buffers, _images) =
        gltf::import(path).map_err(|e| format!("gltf::import({}): {e}", path.display()))?;
    let scene = document.default_scene().ok_or_else(|| {
        format!("{}: glb has no default scene", path.display())
    })?;

    let mut per_material: std::collections::BTreeMap<Option<usize>, u32> =
        std::collections::BTreeMap::new();
    let mut bbox_min = [f32::INFINITY; 3];
    let mut bbox_max = [f32::NEG_INFINITY; 3];
    for node in scene.nodes() {
        summarize_node(
            &node,
            MAT4_IDENTITY,
            &buffers,
            &mut per_material,
            &mut bbox_min,
            &mut bbox_max,
        );
    }

    if !bbox_min[0].is_finite() {
        return Err(format!("{}: parsed no geometry", path.display()));
    }

    let materials: Vec<GltfMaterialInfo> = document
        .materials()
        .filter_map(|m| {
            let material_index = m.index()? as u32;
            let vertex_count = per_material
                .get(&Some(material_index as usize))
                .copied()
                .unwrap_or(0);
            if vertex_count == 0 {
                // Declared but unused by any geometry — nothing to draw.
                return None;
            }
            let pbr = m.pbr_metallic_roughness();
            // IMPORT_FIDELITY_DESIGN.md D8/F-P5: BLEND maps to a real
            // `Blend` material (see `gltf_import.rs`), never Mask cutout —
            // `alpha_mask` reflects ONLY the genuine glTF MASK mode.
            let was_blend = matches!(m.alpha_mode(), gltf::material::AlphaMode::Blend);
            let alpha_mask = matches!(m.alpha_mode(), gltf::material::AlphaMode::Mask);

            // GLB_CONFORMANCE_DESIGN.md G-P4/D5: KHR_texture_transform,
            // per-map (all five families — see the field doc comments).
            // base-color/mr/emissive route through `texture::Info`, which
            // has the typed `texture_transform()` accessor; normal and
            // occlusion (`NormalTexture`/`OcclusionTexture`) don't, so
            // those parse the raw `extensions` JSON via
            // `parse_uv_transform_json` (identical spec defaults).
            let mut uv_tex_coord_override = false;
            let mut fold_typed = |info: Option<gltf::texture::Info>| -> [f32; 6] {
                match info.as_ref().and_then(|t| t.texture_transform()) {
                    Some(t) => {
                        uv_tex_coord_override |= t.tex_coord().is_some();
                        fold_uv_transform(t.offset(), t.rotation(), t.scale())
                    }
                    None => IDENTITY_UV_TRANSFORM,
                }
            };
            let base_color_info = pbr.base_color_texture();
            let base_color_uv_transform = fold_typed(pbr.base_color_texture());
            let mr_uv_transform = fold_typed(pbr.metallic_roughness_texture());
            let emissive_uv_transform = fold_typed(m.emissive_texture());
            let mut fold_raw = |ext: Option<&serde_json::Value>| -> [f32; 6] {
                match ext {
                    Some(v) => {
                        let (folded, has_override) = parse_uv_transform_json(v);
                        uv_tex_coord_override |= has_override;
                        folded
                    }
                    None => IDENTITY_UV_TRANSFORM,
                }
            };
            let normal_uv_transform = fold_raw(
                m.normal_texture()
                    .as_ref()
                    .and_then(|t| t.extension_value("KHR_texture_transform")),
            );
            let occlusion_uv_transform = fold_raw(
                m.occlusion_texture()
                    .as_ref()
                    .and_then(|t| t.extension_value("KHR_texture_transform")),
            );

            // GLB_CONFORMANCE_DESIGN.md G-P4/D5: KHR_materials_specular +
            // KHR_materials_ior, mapped to F0 scale downstream
            // (gltf_import.rs). Defaults (1.5 / 1.0 / [1,1,1]) reproduce
            // today's hardcoded F0=0.04 exactly — see `fs_pbr`.
            let ior = m.ior().unwrap_or(1.5);
            let specular_ext = m.specular();
            let specular_factor = specular_ext.as_ref().map(|s| s.specular_factor()).unwrap_or(1.0);
            let specular_color_factor = specular_ext
                .as_ref()
                .map(|s| s.specular_color_factor())
                .unwrap_or([1.0, 1.0, 1.0]);
            let specular_has_texture = specular_ext.as_ref().is_some_and(|s| {
                s.specular_texture().is_some() || s.specular_color_texture().is_some()
            });

            // GLB_CONFORMANCE_DESIGN.md G-P5/D5: KHR_materials_clearcoat.
            // No typed accessor in gltf 1.4.1 (verified: no
            // `KHR_materials_clearcoat` feature in either `gltf` or
            // `gltf-json` 1.4.1's own `Cargo.toml`) — raw JSON sniff via
            // the same `extension_value` this file already used for the
            // presence-only boolean, extended to pull the two factors.
            // glTF's own implicit defaults (0.0 for both) make an absent
            // extension byte-identical to pre-G-P5.
            let clearcoat_ext = m.extension_value("KHR_materials_clearcoat");
            let clearcoat_factor = clearcoat_ext
                .and_then(|v| v.get("clearcoatFactor"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            let clearcoat_roughness_factor = clearcoat_ext
                .and_then(|v| v.get("clearcoatRoughnessFactor"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0) as f32;
            let clearcoat_has_texture = clearcoat_ext.is_some_and(|v| {
                v.get("clearcoatTexture").is_some()
                    || v.get("clearcoatRoughnessTexture").is_some()
                    || v.get("clearcoatNormalTexture").is_some()
            });

            Some(GltfMaterialInfo {
                material_index,
                name: m.name().map(|s| s.to_string()),
                base_color_factor: pbr.base_color_factor(),
                metallic: pbr.metallic_factor(),
                roughness: pbr.roughness_factor(),
                emissive: m.emissive_factor(),
                alpha_mask,
                alpha_cutoff: m.alpha_cutoff().unwrap_or(0.5),
                base_color_texture: base_color_info.map(|t| t.texture().index() as u32),
                normal_texture: m.normal_texture().map(|t| t.texture().index() as u32),
                normal_scale: m.normal_texture().map(|t| t.scale()).unwrap_or(1.0),
                mr_texture: pbr.metallic_roughness_texture().map(|t| t.texture().index() as u32),
                occlusion_texture: m.occlusion_texture().map(|t| t.texture().index() as u32),
                occlusion_strength: m.occlusion_texture().map(|t| t.strength()).unwrap_or(1.0),
                emissive_texture: m.emissive_texture().map(|t| t.texture().index() as u32),
                emissive_strength: m.emissive_strength().unwrap_or(1.0),
                ior,
                specular_factor,
                specular_color_factor,
                specular_has_texture,
                base_color_uv_transform,
                normal_uv_transform,
                mr_uv_transform,
                occlusion_uv_transform,
                emissive_uv_transform,
                uv_tex_coord_override,
                transmission_factor: m
                    .transmission()
                    .map(|t| t.transmission_factor())
                    .unwrap_or(0.0),
                clearcoat_factor,
                clearcoat_roughness_factor,
                clearcoat_has_texture,
                was_blend,
                vertex_count,
            })
        })
        .collect();

    Ok(GltfImportSummary {
        materials,
        bbox_min,
        bbox_max,
        camera_count: document.cameras().count(),
        default_material_vertex_count: per_material.get(&None).copied().unwrap_or(0),
    })
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

    /// The per-material import summary the P1c importer builds its
    /// `render_scene` objects from. Skips when the fixture is absent;
    /// otherwise asserts the azalea's known shape (2 textured materials,
    /// each with a base-color texture, no glTF cameras) and that the
    /// `Material` selector actually extracts each material's geometry at
    /// the summarized vertex count.
    #[test]
    fn import_summary_and_material_selector_on_azalea() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb");
        if !path.exists() {
            println!("import_summary_and_material_selector_on_azalea: fixture not found, skipping");
            return;
        }
        let summary = gltf_import_summary(&path).expect("summary");
        println!(
            "azalea summary: {} materials, bbox {:?}..{:?}, cameras={}, default-mat verts={}",
            summary.materials.len(),
            summary.bbox_min,
            summary.bbox_max,
            summary.camera_count,
            summary.default_material_vertex_count,
        );

        // Known azalea shape: 2 distinct textured materials, no cameras,
        // no default-material geometry.
        assert_eq!(summary.materials.len(), 2, "azalea has 2 materials with geometry");
        assert_eq!(summary.camera_count, 0);
        assert_eq!(summary.default_material_vertex_count, 0);
        for m in &summary.materials {
            assert!(
                m.base_color_texture.is_some(),
                "material {} should carry a base-color texture",
                m.material_index
            );
            assert!(m.vertex_count > 0);
            // The Material selector must extract exactly that many verts.
            let verts = load_gltf_mesh(
                &path,
                GltfMeshSelector::Material { material_index: m.material_index },
            )
            .expect("material selector");
            assert_eq!(
                verts.len() as u32,
                m.vertex_count,
                "Material selector vertex count must match the summary for material {}",
                m.material_index
            );
            assert_eq!(verts.len() % 3, 0, "triangle list");
        }
    }
}
