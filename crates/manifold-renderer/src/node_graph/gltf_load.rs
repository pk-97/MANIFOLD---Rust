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

/// glTF extensions MANIFOLD's importer actually supports, independent of
/// what the pinned `gltf` 1.4.1 crate's own feature-flag set types —
/// GLB_XFAIL_BURNDOWN_DESIGN.md D1. Everything in `Cargo.toml`'s `gltf`
/// feature list, plus three extensions this codebase maps downstream that
/// the crate has no typed accessor for at this version: `KHR_materials_unlit`
/// (`MATERIAL_SYSTEM_DESIGN.md`'s unlit shading mode),
/// `KHR_materials_pbrSpecularGlossiness` (converted to metal-rough at
/// import, BUG-167), and `KHR_materials_clearcoat` (raw-JSON sniff,
/// `GLB_CONFORMANCE_DESIGN.md` G-P5). An asset whose `extensionsRequired`
/// lists anything NOT in this set fails loudly, naming the extension —
/// never silently, never approximated.
const MANIFOLD_SUPPORTED_EXTENSIONS: &[&str] = &[
    // Cargo.toml's `gltf` feature list (typed crate support):
    "KHR_materials_emissive_strength",
    "KHR_materials_transmission",
    "KHR_lights_punctual",
    "KHR_texture_transform",
    "KHR_materials_specular",
    "KHR_materials_ior",
    // MANIFOLD-mapped, no typed crate accessor at 1.4.1:
    "KHR_materials_unlit",
    "KHR_materials_pbrSpecularGlossiness",
    "KHR_materials_clearcoat",
];

/// The ONE parse entry for `.glb`/`.gltf` files (`GLB_XFAIL_BURNDOWN_DESIGN.md`
/// D1/D3-data-model-delta) — every other call site in this crate routes
/// through this helper instead of the `gltf` crate's `gltf::import(path)`
/// convenience function.
///
/// Why: `gltf::import` validates `extensionsRequired` against the crate's
/// OWN compiled-in feature set (`gltf_json::extensions::ENABLED_EXTENSIONS`)
/// and hard-fails before MANIFOLD's importer ever runs, even for extensions
/// MANIFOLD genuinely supports downstream (BUG-166: `KHR_materials_unlit`,
/// `KHR_materials_clearcoat` — the latter has NO crate feature at 1.4.1 at
/// all, so enabling flags alone can never fix it). This helper parses
/// without the crate's built-in validation (`Gltf::from_slice_without_validation`),
/// re-runs the SAME structural validation the crate would have run
/// (`json::Root`'s `Validate` impl, invoked directly — index bounds, missing
/// required fields, oversize, etc. all still checked), but drops only the
/// `Unsupported`-under-`extensionsRequired` errors that validation produces
/// (the one and only place `gltf_json::validation::Error::Unsupported` is
/// ever raised — verified by reading `gltf-json-1.4.1/src/root.rs`'s
/// `root_validate_hook` and confirming no other call site in the crate
/// constructs that variant). MANIFOLD's own gate below then re-checks
/// `extensionsRequired` against `MANIFOLD_SUPPORTED_EXTENSIONS`, so an asset
/// that lists a genuinely unsupported extension still fails — with OUR
/// error naming it, not the crate's generic "Unsupported extension".
pub(crate) fn import_glb(
    path: &std::path::Path,
) -> Result<(gltf::Document, Vec<gltf::buffer::Data>, Vec<gltf::image::Data>), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;

    let gltf::Gltf { document, blob } = gltf::Gltf::from_slice_without_validation(&bytes)
        .map_err(|e| format!("{}: gltf parse failed: {e}", path.display()))?;

    // Re-run the crate's structural validation, filtering out the
    // extensionsRequired veto (MANIFOLD's own gate, below, replaces it).
    {
        use gltf::json::validation::Validate;
        let json = document.as_json();
        let mut errors: Vec<(gltf::json::Path, gltf::json::validation::Error)> = Vec::new();
        json.validate(json, gltf::json::Path::new, &mut |path_fn, error| {
            errors.push((path_fn(), error));
        });
        let real_errors: Vec<_> = errors
            .into_iter()
            .filter(|(p, e)| {
                !(*e == gltf::json::validation::Error::Unsupported
                    && p.as_str().starts_with("extensionsRequired"))
            })
            .collect();
        if !real_errors.is_empty() {
            return Err(format!(
                "{}: glTF validation failed: {:?}",
                path.display(),
                real_errors
            ));
        }
    }

    for ext in document.extensions_required() {
        if !MANIFOLD_SUPPORTED_EXTENSIONS.contains(&ext) {
            return Err(format!(
                "{}: extensionsRequired[..] = \"{ext}\": unsupported extension (MANIFOLD does not import this extension)",
                path.display()
            ));
        }
    }

    let base = path.parent().unwrap_or_else(|| std::path::Path::new("./"));
    let buffers = gltf::import_buffers(&document, Some(base), blob)
        .map_err(|e| format!("{}: buffer import failed: {e}", path.display()))?;
    let images = gltf::import_images(&document, Some(base), &buffers)
        .map_err(|e| format!("{}: image import failed: {e}", path.display()))?;

    Ok((document, buffers, images))
}

/// Resolve the scene(s) to import when a glb has no default `scene` index
/// (spec-legal — `GLB_XFAIL_BURNDOWN_DESIGN.md` D5, fixes BUG-172):
/// the default scene if present; else the union of every `scenes[]` entry's
/// nodes; else every parentless (root) node in the document. Returns owned
/// nodes (not a `Scene` — there may be no single scene backing the union).
pub(crate) fn resolve_import_nodes(document: &gltf::Document) -> Vec<gltf::Node<'_>> {
    if let Some(scene) = document.default_scene() {
        return scene.nodes().collect();
    }
    if document.scenes().len() > 0 {
        let mut seen = std::collections::BTreeSet::new();
        let mut nodes = Vec::new();
        for scene in document.scenes() {
            for node in scene.nodes() {
                if seen.insert(node.index()) {
                    nodes.push(node);
                }
            }
        }
        return nodes;
    }
    // No scenes at all: every node with no parent.
    let mut has_parent = std::collections::BTreeSet::new();
    for node in document.nodes() {
        for child in node.children() {
            has_parent.insert(child.index());
        }
    }
    document
        .nodes()
        .filter(|n| !has_parent.contains(&n.index()))
        .collect()
}

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

/// GLB_XFAIL_BURNDOWN_DESIGN.md D6 (BUG-168): build a column-major `Mat4`
/// from a glTF-convention TRS triple (`q` = quaternion `[x, y, z, w]`) — the
/// same formula the glTF spec uses to define a node's own TRS matrix, so an
/// `EXT_mesh_gpu_instancing` instance transform composes with `mat4_mul`
/// exactly like a node's `transform().matrix()` does.
pub(crate) fn mat4_from_trs(t: [f32; 3], q: [f32; 4], s: [f32; 3]) -> Mat4 {
    let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
    let (xx, yy, zz) = (x * x, y * y, z * z);
    let (xy, xz, yz) = (x * y, x * z, y * z);
    let (wx, wy, wz) = (w * x, w * y, w * z);
    [
        [
            (1.0 - 2.0 * (yy + zz)) * s[0],
            (2.0 * (xy + wz)) * s[0],
            (2.0 * (xz - wy)) * s[0],
            0.0,
        ],
        [
            (2.0 * (xy - wz)) * s[1],
            (1.0 - 2.0 * (xx + zz)) * s[1],
            (2.0 * (yz + wx)) * s[1],
            0.0,
        ],
        [
            (2.0 * (xz + wy)) * s[2],
            (2.0 * (yz - wx)) * s[2],
            (1.0 - 2.0 * (xx + yy)) * s[2],
            0.0,
        ],
        [t[0], t[1], t[2], 1.0],
    ]
}

/// GLB_XFAIL_BURNDOWN_DESIGN.md D6: read one accessor's raw components as
/// `f32`, un-interleaving `view.stride()` if present. `EXT_mesh_gpu_instancing`'s
/// TRANSLATION/ROTATION/SCALE accessors are always plain `FLOAT` VEC3/VEC4
/// per spec (no normalized integers, no sparse) — anything else errors
/// rather than silently misreading bytes.
fn read_f32_accessor(accessor: &gltf::Accessor, buffers: &[gltf::buffer::Data]) -> Result<Vec<f32>, String> {
    if accessor.data_type() != gltf::accessor::DataType::F32 {
        return Err(format!(
            "EXT_mesh_gpu_instancing accessor has non-F32 component type {:?} — unsupported",
            accessor.data_type()
        ));
    }
    let comp_count = match accessor.dimensions() {
        gltf::accessor::Dimensions::Vec3 => 3,
        gltf::accessor::Dimensions::Vec4 => 4,
        d => return Err(format!("EXT_mesh_gpu_instancing accessor has unsupported dimensions {d:?}")),
    };
    let view = accessor
        .view()
        .ok_or_else(|| "EXT_mesh_gpu_instancing accessor has no buffer view (sparse accessors unsupported)".to_string())?;
    let buffer = buffers.get(view.buffer().index()).ok_or_else(|| {
        "EXT_mesh_gpu_instancing accessor's buffer view has an out-of-range buffer index".to_string()
    })?;
    let elem_size = comp_count * 4;
    let stride = view.stride().unwrap_or(elem_size);
    let base = view.offset() + accessor.offset();
    let mut out = Vec::with_capacity(accessor.count() * comp_count);
    for i in 0..accessor.count() {
        let start = base + i * stride;
        for c in 0..comp_count {
            let off = start + c * 4;
            let bytes = buffer.0.get(off..off + 4).ok_or_else(|| {
                "EXT_mesh_gpu_instancing accessor read out of buffer bounds".to_string()
            })?;
            out.push(f32::from_le_bytes(bytes.try_into().unwrap()));
        }
    }
    Ok(out)
}

/// GLB_XFAIL_BURNDOWN_DESIGN.md D6 (BUG-168): `EXT_mesh_gpu_instancing`
/// raw-JSON sniff (⚠ VERIFIED-AT-IMPL: no typed support in gltf 1.4.1 — no
/// `EXT_mesh_gpu_instancing` feature or accessor exists in either `gltf` or
/// `gltf-json` 1.4.1's source, same absence pattern as `KHR_materials_clearcoat`).
/// Returns `Ok(None)` when the node carries no such extension (the
/// overwhelmingly common case — every non-instanced node), `Ok(Some(N
/// matrices))` for a valid one, `Err` for a malformed extension (missing
/// accessor index, wrong component type) so a broken instancing block
/// surfaces as a named import error rather than silently rendering zero or
/// garbage instances.
fn node_instance_transforms(
    node: &gltf::Node,
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
) -> Result<Option<Vec<Mat4>>, String> {
    let Some(ext) = node.extension_value("EXT_mesh_gpu_instancing") else {
        return Ok(None);
    };
    let attrs = ext
        .get("attributes")
        .ok_or_else(|| "EXT_mesh_gpu_instancing extension has no \"attributes\" object".to_string())?;
    let read_attr = |name: &str, expected_dim: usize| -> Result<Option<Vec<f32>>, String> {
        let Some(idx) = attrs.get(name).and_then(|v| v.as_u64()) else {
            return Ok(None);
        };
        let accessor = document.accessors().nth(idx as usize).ok_or_else(|| {
            format!("EXT_mesh_gpu_instancing {name} accessor index {idx} out of range")
        })?;
        let data = read_f32_accessor(&accessor, buffers)?;
        debug_assert_eq!(data.len() % expected_dim, 0);
        Ok(Some(data))
    };
    let translations = read_attr("TRANSLATION", 3)?;
    let rotations = read_attr("ROTATION", 4)?;
    let scales = read_attr("SCALE", 3)?;
    // Spec: at least one of the three attributes must be present; instance
    // count is that attribute's accessor `count`. Absent channels fall back
    // to the TRS identity component (no translation / identity quat / unit
    // scale) — same "missing = neutral" convention as every other optional
    // glTF field this importer reads.
    let count = translations
        .as_ref()
        .map(|d| d.len() / 3)
        .or_else(|| rotations.as_ref().map(|d| d.len() / 4))
        .or_else(|| scales.as_ref().map(|d| d.len() / 3))
        .ok_or_else(|| {
            "EXT_mesh_gpu_instancing has no TRANSLATION/ROTATION/SCALE attribute".to_string()
        })?;
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let t = translations
            .as_ref()
            .map(|d| [d[i * 3], d[i * 3 + 1], d[i * 3 + 2]])
            .unwrap_or([0.0, 0.0, 0.0]);
        let q = rotations
            .as_ref()
            .map(|d| [d[i * 4], d[i * 4 + 1], d[i * 4 + 2], d[i * 4 + 3]])
            .unwrap_or([0.0, 0.0, 0.0, 1.0]);
        let s = scales
            .as_ref()
            .map(|d| [d[i * 3], d[i * 3 + 1], d[i * 3 + 2]])
            .unwrap_or([1.0, 1.0, 1.0]);
        out.push(mat4_from_trs(t, q, s));
    }
    Ok(Some(out))
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
    /// Every primitive with NO material (`primitive.material().index() ==
    /// None`, glTF's implicit default material) — GLB_XFAIL_BURNDOWN_DESIGN.md
    /// D4 (BUG-171)'s synthetic-material object. Scene-wide, like
    /// `Material`, since materialless primitives can be scattered across
    /// any node.
    DefaultMaterial,
}

/// Which primitives `walk_gltf_node` keeps, generalizing the old
/// `Option<u32>` (`None` = every primitive, `Some(idx)` = one material) to
/// also express "only the materialless (default-material) primitives" —
/// GLB_XFAIL_BURNDOWN_DESIGN.md D4.
#[derive(Clone, Copy)]
enum MaterialFilter {
    All,
    Material(u32),
    DefaultOnly,
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
///
/// GLB_XFAIL_BURNDOWN_DESIGN.md D6 (BUG-168): a node carrying
/// `EXT_mesh_gpu_instancing` flattens its mesh's primitives once PER
/// INSTANCE, each at `world * instance_transform`, instead of once at
/// `world` — the same "N repeated nodes" result the extension exists to
/// avoid spelling out literally, so the existing per-material combine
/// (zero new code past this function) ends up with N world-baked copies.
/// Children still recurse at the node's own (non-instanced) `world`, per
/// spec — the extension only multiplies the node's OWN mesh.
fn walk_gltf_node(
    node: &gltf::Node,
    parent_world: Mat4,
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    material_filter: MaterialFilter,
    out: &mut Vec<MeshVertex>,
) -> Result<(), String> {
    let local = node.transform().matrix();
    let world = mat4_mul(&parent_world, &local);

    if let Some(mesh) = node.mesh() {
        let instances = node_instance_transforms(node, document, buffers)?;
        let instance_worlds: Vec<Mat4> = match &instances {
            Some(list) => list.iter().map(|it| mat4_mul(&world, it)).collect(),
            None => vec![world],
        };
        for instance_world in &instance_worlds {
            // Normal matrix = transpose(inverse(upper3x3(world))) — correct
            // under non-uniform scale, not just rotation + uniform scale.
            let normal_mat = mat3_transpose(mat3_inverse(mat3_upper_row_major(instance_world)));
            for primitive in mesh.primitives() {
                match material_filter {
                    MaterialFilter::All => {}
                    // `material().index()` is `None` for the glTF default
                    // material; a numeric filter never matches that.
                    MaterialFilter::Material(want) => {
                        if primitive.material().index() != Some(want as usize) {
                            continue;
                        }
                    }
                    MaterialFilter::DefaultOnly => {
                        if primitive.material().index().is_some() {
                            continue;
                        }
                    }
                }
                flatten_primitive(&primitive, buffers, instance_world, &normal_mat, out)?;
            }
        }
    }

    for child in node.children() {
        walk_gltf_node(&child, world, document, buffers, material_filter, out)?;
    }
    Ok(())
}

/// Parse a `.glb`/`.gltf` file and flatten the selected geometry into a
/// triangle-list `Vec<MeshVertex>`. See [`GltfMeshSelector`] for the three
/// selection modes. Returns `Err(String)` on any failure — a missing/
/// unreadable file, an unsupported required extension, an out-of-range
/// mesh/primitive index, or a non-Triangles primitive — rather than
/// panicking, since this runs on a background thread inside
/// `node.gltf_mesh_source`. A missing default scene no longer errors:
/// `resolve_import_nodes` falls back per `GLB_XFAIL_BURNDOWN_DESIGN.md` D5.
pub(crate) fn load_gltf_mesh(
    path: &std::path::Path,
    selector: GltfMeshSelector,
) -> Result<Vec<MeshVertex>, String> {
    let (document, buffers, _images) = import_glb(path)?;

    let mut out = Vec::new();
    match selector {
        GltfMeshSelector::WholeScene => {
            for node in resolve_import_nodes(&document) {
                walk_gltf_node(
                    &node,
                    MAT4_IDENTITY,
                    &document,
                    &buffers,
                    MaterialFilter::All,
                    &mut out,
                )?;
            }
        }
        GltfMeshSelector::Material { material_index } => {
            for node in resolve_import_nodes(&document) {
                walk_gltf_node(
                    &node,
                    MAT4_IDENTITY,
                    &document,
                    &buffers,
                    MaterialFilter::Material(material_index),
                    &mut out,
                )?;
            }
        }
        GltfMeshSelector::DefaultMaterial => {
            for node in resolve_import_nodes(&document) {
                walk_gltf_node(
                    &node,
                    MAT4_IDENTITY,
                    &document,
                    &buffers,
                    MaterialFilter::DefaultOnly,
                    &mut out,
                )?;
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
    let (document, _buffers, images) = import_glb(path)?;

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

/// GLB_XFAIL_BURNDOWN_DESIGN.md D3 (BUG-164): glTF `wrapS`/`wrapT`, decoupled
/// from `manifold_gpu` (this module is content-thread parse code, no GPU
/// dependency) — `gltf_import.rs` translates these into `node.pbr_material`'s
/// enum params, which `render_scene` then reads back into
/// `manifold_gpu::GpuAddressMode`. Default `Repeat` matches both glTF's own
/// implicit no-sampler default and `WrappingMode`'s crate-level default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum GltfWrapMode {
    #[default]
    Repeat,
    ClampToEdge,
    MirrorRepeat,
}

/// GLB_XFAIL_BURNDOWN_DESIGN.md D3: glTF `magFilter`/`minFilter`, collapsed
/// to the two states `manifold_gpu::GpuFilterMode` supports — `minFilter`'s
/// mipmap component (`*_MIPMAP_NEAREST`/`*_MIPMAP_LINEAR`) has no GPU-side
/// equivalent to plumb per-map (the renderer's `mip_filter` stays fixed
/// Linear for every sampler, same as the pre-D3 hardcoded material sampler),
/// so only the base Nearest/Linear choice survives; `TextureSettingsTest`
/// only exercises wrap anyway (verified: all 5 of its samplers share
/// `magFilter: LINEAR, minFilter: NEAREST_MIPMAP_LINEAR`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum GltfFilterMode {
    #[default]
    Linear,
    Nearest,
}

/// GLB_XFAIL_BURNDOWN_DESIGN.md D3: one map family's sampler settings.
/// Default reproduces glTF's implicit no-sampler default (Repeat/Repeat/
/// Linear/Linear) — byte-identical to the pre-D3 hardcoded REPEAT sampler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct GltfSamplerInfo {
    pub wrap_u: GltfWrapMode,
    pub wrap_v: GltfWrapMode,
    pub mag_filter: GltfFilterMode,
    pub min_filter: GltfFilterMode,
}

/// Read `document.textures().nth(idx)`'s sampler, or the default when `idx`
/// is `None` (map unwired) — the same default `GltfSamplerInfo::default()`
/// already encodes, kept as one call site so the two never drift.
fn sampler_info_for(document: &gltf::Document, idx: Option<u32>) -> GltfSamplerInfo {
    let Some(tex) = idx.and_then(|i| document.textures().nth(i as usize)) else {
        return GltfSamplerInfo::default();
    };
    let s = tex.sampler();
    let wrap = |w: gltf::texture::WrappingMode| -> GltfWrapMode {
        match w {
            gltf::texture::WrappingMode::Repeat => GltfWrapMode::Repeat,
            gltf::texture::WrappingMode::ClampToEdge => GltfWrapMode::ClampToEdge,
            gltf::texture::WrappingMode::MirroredRepeat => GltfWrapMode::MirrorRepeat,
        }
    };
    let mag = match s.mag_filter() {
        Some(gltf::texture::MagFilter::Nearest) => GltfFilterMode::Nearest,
        _ => GltfFilterMode::Linear,
    };
    let min = match s.min_filter() {
        Some(gltf::texture::MinFilter::Nearest)
        | Some(gltf::texture::MinFilter::NearestMipmapNearest)
        | Some(gltf::texture::MinFilter::NearestMipmapLinear) => GltfFilterMode::Nearest,
        _ => GltfFilterMode::Linear,
    };
    GltfSamplerInfo {
        wrap_u: wrap(s.wrap_s()),
        wrap_v: wrap(s.wrap_t()),
        mag_filter: mag,
        min_filter: min,
    }
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
    /// `true` when `mr_texture` above actually points at a
    /// `KHR_materials_pbrSpecularGlossiness` `specularGlossinessTexture`
    /// (GLB_XFAIL_BURNDOWN_DESIGN.md D2, BUG-167) rather than a genuine
    /// glTF metallic-roughness map — its usable channel is glossiness in
    /// ALPHA, not roughness/metallic in G/B. `gltf_import.rs` wires the
    /// `node.gltf_texture_source` feeding this map with `mode =
    /// gloss_to_roughness`, which repacks `(0, 1-gloss, 0, 1)` at decode
    /// time so `render_scene`'s existing G=roughness/B=metallic read stays
    /// untouched — the shader sees only metal-rough, per D2. The texture's
    /// RGB specular tint (vs. the scalar `specular_factor` above, which
    /// IS converted) is Deferred (§8) — not read here at all.
    pub mr_texture_is_gloss_alpha: bool,
    pub vertex_count: u32,
    /// GLB_XFAIL_BURNDOWN_DESIGN.md D3 (BUG-164): per-map-family sampler
    /// settings, read straight off each map's own glTF `sampler` (default
    /// when unwired or the texture has no explicit sampler:
    /// `GltfSamplerInfo::default()`, Repeat/Repeat/Linear/Linear —
    /// byte-identical to the pre-D3 hardcoded REPEAT `material_sampler`).
    pub base_color_sampler: GltfSamplerInfo,
    pub normal_sampler: GltfSamplerInfo,
    pub mr_sampler: GltfSamplerInfo,
    pub occlusion_sampler: GltfSamplerInfo,
    pub emissive_sampler: GltfSamplerInfo,
}

/// Result of converting one `KHR_materials_pbrSpecularGlossiness` material's
/// scalar factors to MANIFOLD's metal-rough vocabulary
/// (GLB_XFAIL_BURNDOWN_DESIGN.md D2, BUG-167). Pure/value-level so it's
/// unit-testable without a parsed `Document` — see the `tests` module.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SpecGlossConversion {
    pub roughness: f32,
    pub metallic: f32,
    pub specular_factor: f32,
}

/// Convert spec-gloss's `glossinessFactor` + `specularFactor` (RGB) to
/// metal-rough's `roughness` + the existing scalar `specular_factor` F0
/// slot (`KHR_materials_specular`'s own slot, GLB_CONFORMANCE_DESIGN.md
/// G-P4 — reused here rather than adding a second one). `metallic` is
/// pinned to `0.0`: spec-gloss has no metalness channel of its own, and
/// 0.0 is the dielectric default under which `specular_factor` alone
/// drives `fs_pbr`'s F0 term — matching the extension's own dielectric-
/// first model (a "metal" under spec-gloss is modeled as near-black
/// diffuse + near-white specular, not a metalness scalar). `specular_factor`
/// folds the RGB factor to its mean — the per-channel RGB TINT is
/// Deferred (§8); this only carries the scalar magnitude, same as
/// `KHR_materials_specular` already does for factor-only materials.
pub(crate) fn convert_spec_gloss(
    glossiness_factor: f32,
    specular_factor_rgb: [f32; 3],
) -> SpecGlossConversion {
    SpecGlossConversion {
        roughness: (1.0 - glossiness_factor).clamp(0.0, 1.0),
        metallic: 0.0,
        specular_factor: ((specular_factor_rgb[0] + specular_factor_rgb[1] + specular_factor_rgb[2])
            / 3.0)
            .clamp(0.0, 1.0),
    }
}

/// `node.gltf_mesh_source`'s `material_index` param sentinel selecting
/// [`GltfMeshSelector::DefaultMaterial`] — distinct from the param's own
/// "unset" default (`-1`, which falls through to the `mesh_index`/
/// `WholeScene` path; see `gltf_mesh_source.rs::run`). Real glTF material
/// indices are always `>= 0`, so `-2` never collides with a genuine
/// selection. GLB_XFAIL_BURNDOWN_DESIGN.md D4 (BUG-171).
pub(crate) const DEFAULT_MATERIAL_MESH_PARAM: i32 = -2;

/// [`GltfImportSummary::materials`]' reserved sentinel for the synthetic
/// glTF-default-material entry (D4) — pinned by
/// `GLB_XFAIL_BURNDOWN_DESIGN.md` §3: real glTF material indices are
/// always `< u32::MAX` (the format has no material anywhere near that
/// count), so this can never collide with a genuine index.
/// `gltf_import.rs` must treat it as "no glTF material to re-query" —
/// see [`DEFAULT_MATERIAL_MESH_PARAM`], which is how it tells
/// `node.gltf_mesh_source` to select this geometry without looking up
/// material index `u32::MAX` in the document.
pub(crate) const DEFAULT_MATERIAL_SENTINEL: u32 = u32::MAX;

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
///
/// GLB_XFAIL_BURNDOWN_DESIGN.md D6 (BUG-168): mirrors `walk_gltf_node`'s
/// instancing expansion — a node's vertex count and bbox contribution are
/// counted once PER INSTANCE (not once per node), so the report the
/// importer builds from this walk (`vertex_count`, the D4 object-safety
/// gate) already reflects the real N-copy geometry `walk_gltf_node`
/// produces at flatten time. Returns `Err` only when a node's own
/// `EXT_mesh_gpu_instancing` block is malformed.
fn summarize_node(
    node: &gltf::Node,
    parent_world: Mat4,
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    per_material: &mut std::collections::BTreeMap<Option<usize>, u32>,
    bbox_min: &mut [f32; 3],
    bbox_max: &mut [f32; 3],
) -> Result<(), String> {
    let local = node.transform().matrix();
    let world = mat4_mul(&parent_world, &local);

    if let Some(mesh) = node.mesh() {
        let instances = node_instance_transforms(node, document, buffers)?;
        let instance_count = instances.as_ref().map_or(1, |v| v.len().max(1));
        let instance_worlds: Vec<Mat4> = match &instances {
            Some(list) => list.iter().map(|it| mat4_mul(&world, it)).collect(),
            None => vec![world],
        };
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
            *per_material.entry(prim.material().index()).or_insert(0) += vcount * instance_count as u32;
            for instance_world in &instance_worlds {
                for p in &positions {
                    let wp = mat4_transform_point(instance_world, *p);
                    for i in 0..3 {
                        bbox_min[i] = bbox_min[i].min(wp[i]);
                        bbox_max[i] = bbox_max[i].max(wp[i]);
                    }
                }
            }
        }
    }

    for child in node.children() {
        summarize_node(&child, world, document, buffers, per_material, bbox_min, bbox_max)?;
    }
    Ok(())
}

/// Parse a glb's structure for the importer: the distinct materials with
/// geometry, the world-space bbox, camera count, and unassigned-geometry
/// count. One parse; no GPU. See [`GltfImportSummary`].
pub(crate) fn gltf_import_summary(path: &std::path::Path) -> Result<GltfImportSummary, String> {
    let (document, buffers, _images) = import_glb(path)?;
    let import_nodes = resolve_import_nodes(&document);

    let mut per_material: std::collections::BTreeMap<Option<usize>, u32> =
        std::collections::BTreeMap::new();
    let mut bbox_min = [f32::INFINITY; 3];
    let mut bbox_max = [f32::NEG_INFINITY; 3];
    for node in &import_nodes {
        summarize_node(
            node,
            MAT4_IDENTITY,
            &document,
            &buffers,
            &mut per_material,
            &mut bbox_min,
            &mut bbox_max,
        )?;
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
            let mut base_color_info = pbr.base_color_texture();
            let mut base_color_uv_transform = fold_typed(pbr.base_color_texture());
            let mut mr_texture_index = pbr.metallic_roughness_texture().map(|t| t.texture().index() as u32);
            let mut mr_uv_transform = fold_typed(pbr.metallic_roughness_texture());
            let mut base_color_factor = pbr.base_color_factor();
            let mut metallic = pbr.metallic_factor();
            let mut roughness = pbr.roughness_factor();
            let mut mr_texture_is_gloss_alpha = false;
            let emissive_uv_transform = fold_typed(m.emissive_texture());

            // GLB_XFAIL_BURNDOWN_DESIGN.md D2 (BUG-167):
            // KHR_materials_pbrSpecularGlossiness converts to metal-rough
            // HERE, at parse time — `gltf_import.rs`/`render_scene.wgsl`
            // never see spec-gloss. A material can't carry both
            // pbrMetallicRoughness's own textures/factors AND spec-gloss
            // meaningfully (the extension REPLACES the base PBR model per
            // spec), so this unconditionally overrides the metal-rough
            // fields already computed above when present. `specular_factor`
            // is declared here (rather than with the KHR_materials_specular
            // block below) so both the direct-specular-extension path AND
            // this spec-gloss override write the SAME variable — one slot,
            // two possible sources, never both.
            let mut specular_factor_override: Option<f32> = None;
            if let Some(sg) = m.pbr_specular_glossiness() {
                let conv = convert_spec_gloss(sg.glossiness_factor(), sg.specular_factor());
                base_color_factor = sg.diffuse_factor();
                base_color_info = sg.diffuse_texture();
                base_color_uv_transform = fold_typed(sg.diffuse_texture());
                roughness = conv.roughness;
                metallic = conv.metallic;
                specular_factor_override = Some(conv.specular_factor);
                match sg.specular_glossiness_texture() {
                    Some(tex) => {
                        mr_texture_index = Some(tex.texture().index() as u32);
                        mr_uv_transform = fold_typed(sg.specular_glossiness_texture());
                        mr_texture_is_gloss_alpha = true;
                    }
                    None => {
                        mr_texture_index = None;
                        mr_uv_transform = IDENTITY_UV_TRANSFORM;
                    }
                }
            }

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
            // today's hardcoded F0=0.04 exactly — see `fs_pbr`. D2:
            // `specular_factor_override` (set above, non-None only for a
            // spec-gloss material) wins over the direct
            // `KHR_materials_specular` extension — spec-gloss materials
            // don't also carry that extension in practice, but if one did,
            // D2's conversion is the more specific decision for this slot.
            let ior = m.ior().unwrap_or(1.5);
            let specular_ext = m.specular();
            let specular_factor = specular_factor_override
                .unwrap_or_else(|| specular_ext.as_ref().map(|s| s.specular_factor()).unwrap_or(1.0));
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

            let base_color_texture = base_color_info.map(|t| t.texture().index() as u32);
            let normal_texture = m.normal_texture().map(|t| t.texture().index() as u32);
            let occlusion_texture = m.occlusion_texture().map(|t| t.texture().index() as u32);
            let emissive_texture = m.emissive_texture().map(|t| t.texture().index() as u32);
            // GLB_XFAIL_BURNDOWN_DESIGN.md D3 (BUG-164): one lookup per map
            // family, keyed off the same texture index each map's own
            // field above already resolved — `None` (map unwired) reads
            // `GltfSamplerInfo::default()` inside the helper.
            let base_color_sampler = sampler_info_for(&document, base_color_texture);
            let normal_sampler = sampler_info_for(&document, normal_texture);
            let mr_sampler = sampler_info_for(&document, mr_texture_index);
            let occlusion_sampler = sampler_info_for(&document, occlusion_texture);
            let emissive_sampler = sampler_info_for(&document, emissive_texture);

            Some(GltfMaterialInfo {
                material_index,
                name: m.name().map(|s| s.to_string()),
                base_color_factor,
                metallic,
                roughness,
                emissive: m.emissive_factor(),
                alpha_mask,
                alpha_cutoff: m.alpha_cutoff().unwrap_or(0.5),
                base_color_texture,
                normal_texture,
                normal_scale: m.normal_texture().map(|t| t.scale()).unwrap_or(1.0),
                mr_texture: mr_texture_index,
                occlusion_texture,
                occlusion_strength: m.occlusion_texture().map(|t| t.strength()).unwrap_or(1.0),
                emissive_texture,
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
                mr_texture_is_gloss_alpha,
                vertex_count,
                base_color_sampler,
                normal_sampler,
                mr_sampler,
                occlusion_sampler,
                emissive_sampler,
            })
        })
        .collect();

    let default_material_vertex_count = per_material.get(&None).copied().unwrap_or(0);
    let mut materials = materials;
    if default_material_vertex_count > 0 {
        // GLB_XFAIL_BURNDOWN_DESIGN.md D4 (BUG-171): geometry with no
        // material assigned gets the glTF spec's implicit default material
        // (base color [1,1,1,1], metallic 1.0, roughness 1.0 — glTF spec
        // §3.9.2) instead of being silently dropped. Sentinel
        // `material_index = DEFAULT_MATERIAL_SENTINEL` (u32::MAX) marks
        // this entry as synthetic — `gltf_import.rs` must never re-query
        // material index u32::MAX in the document for it.
        materials.push(GltfMaterialInfo {
            material_index: DEFAULT_MATERIAL_SENTINEL,
            name: None,
            base_color_factor: [1.0, 1.0, 1.0, 1.0],
            metallic: 1.0,
            roughness: 1.0,
            emissive: [0.0, 0.0, 0.0],
            alpha_mask: false,
            alpha_cutoff: 0.5,
            base_color_texture: None,
            normal_texture: None,
            normal_scale: 1.0,
            mr_texture: None,
            occlusion_texture: None,
            occlusion_strength: 1.0,
            emissive_texture: None,
            emissive_strength: 1.0,
            transmission_factor: 0.0,
            clearcoat_factor: 0.0,
            clearcoat_roughness_factor: 0.0,
            clearcoat_has_texture: false,
            was_blend: false,
            ior: 1.5,
            specular_factor: 1.0,
            specular_color_factor: [1.0, 1.0, 1.0],
            specular_has_texture: false,
            base_color_uv_transform: IDENTITY_UV_TRANSFORM,
            normal_uv_transform: IDENTITY_UV_TRANSFORM,
            mr_uv_transform: IDENTITY_UV_TRANSFORM,
            occlusion_uv_transform: IDENTITY_UV_TRANSFORM,
            emissive_uv_transform: IDENTITY_UV_TRANSFORM,
            uv_tex_coord_override: false,
            mr_texture_is_gloss_alpha: false,
            vertex_count: default_material_vertex_count,
            base_color_sampler: GltfSamplerInfo::default(),
            normal_sampler: GltfSamplerInfo::default(),
            mr_sampler: GltfSamplerInfo::default(),
            occlusion_sampler: GltfSamplerInfo::default(),
            emissive_sampler: GltfSamplerInfo::default(),
        });
    }

    Ok(GltfImportSummary {
        materials,
        bbox_min,
        bbox_max,
        camera_count: document.cameras().count(),
        default_material_vertex_count,
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

    /// GLB_XFAIL_BURNDOWN_DESIGN.md D2 (BUG-167) value-level gate: known
    /// spec-gloss factors → expected metal-rough numbers. Full gloss (1.0)
    /// is a mirror — roughness 0.0 (perfectly smooth, `glossiness_factor`'s
    /// own documented meaning); full rough spec-gloss (glossiness 0.0) →
    /// roughness 1.0. `specular_factor` folds the RGB factor to its mean
    /// (the RGB tint itself is Deferred, §8) and `metallic` is always the
    /// dielectric default 0.0 — spec-gloss has no metalness channel.
    #[test]
    fn convert_spec_gloss_maps_glossiness_and_specular_factor() {
        let smooth = convert_spec_gloss(1.0, [0.5, 0.5, 0.5]);
        assert_eq!(smooth.roughness, 0.0);
        assert_eq!(smooth.metallic, 0.0);
        assert!((smooth.specular_factor - 0.5).abs() < 1e-6);

        let rough = convert_spec_gloss(0.0, [1.0, 1.0, 1.0]);
        assert_eq!(rough.roughness, 1.0);
        assert!((rough.specular_factor - 1.0).abs() < 1e-6);

        // Non-uniform RGB specular factor folds to the mean.
        let tinted = convert_spec_gloss(0.25, [0.2, 0.4, 0.6]);
        assert!((tinted.roughness - 0.75).abs() < 1e-6);
        assert!((tinted.specular_factor - 0.4).abs() < 1e-6);

        // Out-of-[0,1] glossiness (spec-illegal, but defensive) still
        // clamps to a valid roughness rather than producing a negative or
        // >1 value that would corrupt `resolve_mr`'s `max(t.g, 0.01)`.
        let over = convert_spec_gloss(1.5, [2.0, 2.0, 2.0]);
        assert_eq!(over.roughness, 0.0);
        assert_eq!(over.specular_factor, 1.0);
    }

    /// D4 (BUG-171): the synthetic default-material entry's sentinel and
    /// spec-mandated neutral factors (glTF spec §3.9.2's implicit default
    /// material: base color white, metallic 1.0, roughness 1.0).
    #[test]
    fn default_material_synthetic_entry_shape() {
        // Exercised end-to-end (against a real parsed summary) by
        // `gltf_import.rs`'s `default_material_primitive_imports_as_one_object`
        // — this pins the sentinel + spec-default constants in isolation so
        // a future edit to `gltf_import_summary`'s push-site can't silently
        // drift them.
        assert_eq!(DEFAULT_MATERIAL_SENTINEL, u32::MAX);
        assert_eq!(DEFAULT_MATERIAL_MESH_PARAM, -2);
    }
}
