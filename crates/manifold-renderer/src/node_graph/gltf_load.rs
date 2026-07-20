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
/// feature list, plus extensions this codebase maps downstream that the
/// crate has no typed accessor for at this version: `KHR_materials_unlit`
/// (`MATERIAL_SYSTEM_DESIGN.md`'s unlit shading mode),
/// `KHR_materials_pbrSpecularGlossiness` (converted to metal-rough at
/// import, BUG-167), `KHR_materials_clearcoat` (raw-JSON sniff,
/// `GLB_CONFORMANCE_DESIGN.md` G-P5), and — as of
/// `GLTF_MATERIAL_EXTENSIONS_DESIGN.md` E1 — `KHR_materials_sheen`,
/// `KHR_materials_iridescence`, `KHR_materials_anisotropy`, and
/// `KHR_materials_dispersion` (all four raw-JSON sniffed, same doctrine as
/// clearcoat: no typed accessor exists for any of them in `gltf`/
/// `gltf-json` 1.4.1). `KHR_materials_volume` is typed (Cargo feature
/// enabled) so it's covered by the feature-list rows below instead.
/// `EXT_texture_webp` (IMPORT_ANYTHING_WAVE_DESIGN.md W1) has no typed
/// crate feature at all — the extension's image source is read via the
/// raw-JSON `extension_value` sniff and decoded ourselves in
/// [`import_images_with_webp`], since the crate's own bundled `image`
/// dependency has no webp support. An asset whose `extensionsRequired`
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
    "KHR_materials_volume",
    // MANIFOLD-mapped, no typed crate accessor at 1.4.1:
    "KHR_materials_unlit",
    "KHR_materials_pbrSpecularGlossiness",
    "KHR_materials_clearcoat",
    // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E1 — raw-JSON sniffed, no typed
    // accessor at 1.4.1:
    "KHR_materials_sheen",
    "KHR_materials_iridescence",
    "KHR_materials_anisotropy",
    "KHR_materials_dispersion",
    // IMPORT_ANYTHING_WAVE_DESIGN.md W1 — raw-JSON sniffed + manual decode,
    // no crate feature exists for this extension at 1.4.1:
    "EXT_texture_webp",
    // GLB_XFAIL_BURNDOWN_DESIGN.md D6 (BUG-168) — raw-JSON sniffed per-node
    // TRANSLATION/ROTATION/SCALE, composed in [`node_instance_transforms`]:
    "EXT_mesh_gpu_instancing",
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
/// The document+buffer half of [`import_glb`] — parse, re-validate, gate
/// `extensionsRequired`, and resolve buffers, but stop short of image
/// decode. GLTF_ANIM_RUNTIME_V2_DESIGN.md P1: `gltf_anim_cache.rs`'s loader
/// needs the document and buffers (to read animation/skin accessors) but
/// never touches a texture, so routing it through the full `import_glb`
/// would decode every image on a thread whose only job is keyframe data —
/// wasted work, and a second reason to keep exactly one parse entry point.
pub(crate) fn parse_document_and_buffers(
    path: &std::path::Path,
) -> Result<(gltf::Document, Vec<gltf::buffer::Data>), String> {
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

    Ok((document, buffers))
}

pub(crate) fn import_glb(
    path: &std::path::Path,
) -> Result<(gltf::Document, Vec<gltf::buffer::Data>, Vec<gltf::image::Data>), String> {
    let (document, buffers) = parse_document_and_buffers(path)?;
    let base = path.parent().unwrap_or_else(|| std::path::Path::new("./"));
    let images = import_images_with_webp(&document, base, &buffers)
        .map_err(|e| format!("{}: image import failed: {e}", path.display()))?;

    Ok((document, buffers, images))
}

/// `gltf::import_images` hard-fails the ENTIRE document the moment any one
/// image has an unrecognized mime type — its bundled `image` dependency
/// only compiles in `png`/`jpeg` decoders (see the crate's own `Cargo.toml`),
/// so a WebP-textured asset never reaches MANIFOLD's own extension gate at
/// all. IMPORT_ANYTHING_WAVE_DESIGN.md W1: decode webp images ourselves
/// (our `image` dependency has the `webp` feature on) and fall back to the
/// crate's own per-image decode for everything else.
fn import_images_with_webp(
    document: &gltf::Document,
    base: &std::path::Path,
    buffers: &[gltf::buffer::Data],
) -> Result<Vec<gltf::image::Data>, String> {
    let mut out = Vec::with_capacity(document.images().count());
    for image in document.images() {
        let mime_type = match image.source() {
            gltf::image::Source::View { mime_type, .. } => Some(mime_type),
            gltf::image::Source::Uri { mime_type, .. } => mime_type,
        };
        if mime_type == Some("image/webp") {
            out.push(decode_webp_image(&image, buffers)?);
        } else {
            out.push(
                gltf::image::Data::from_source(image.source(), Some(base), buffers)
                    .map_err(|e| format!("image {}: decode failed: {e}", image.index()))?,
            );
        }
    }
    Ok(out)
}

/// Decode one `image/webp` glTF image (bufferView-embedded only — external
/// URI webp is out of scope for this wave) via the top-level `image` crate,
/// which has `webp` enabled unlike the `gltf` crate's own bundled decoder.
fn decode_webp_image(
    image: &gltf::image::Image,
    buffers: &[gltf::buffer::Data],
) -> Result<gltf::image::Data, String> {
    let view = match image.source() {
        gltf::image::Source::View { view, .. } => view,
        gltf::image::Source::Uri { .. } => {
            return Err(format!(
                "image {}: external-URI webp images are not supported",
                image.index()
            ));
        }
    };
    let buf = &buffers[view.buffer().index()].0;
    let begin = view.offset();
    let end = begin + view.length();
    let encoded = &buf[begin..end];

    let decoded = image::load_from_memory_with_format(encoded, image::ImageFormat::WebP)
        .map_err(|e| format!("image {}: webp decode failed: {e}", image.index()))?
        .to_rgba8();
    let (width, height) = (decoded.width(), decoded.height());
    Ok(gltf::image::Data {
        pixels: decoded.into_raw(),
        format: gltf::image::Format::R8G8B8A8,
        width,
        height,
    })
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
    // EXT_texture_webp textures carry their image only behind the
    // extension (base `source` is empty — `allow_empty_texture` feature
    // above) since no fallback format is offered in these assets.
    let webp_source_index = tex
        .extensions()
        .and_then(|ext| ext.get("EXT_texture_webp"))
        .and_then(|ext| ext.get("source"))
        .and_then(|v| v.as_u64());
    let img_index = match webp_source_index {
        Some(idx) => idx as usize,
        None => tex
            .source()
            .ok_or_else(|| format!("texture {texture_index} has no image source"))?
            .index(),
    };
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
    /// `KHR_materials_transmission`'s `transmissionTexture` index, if any
    /// (R channel scales `transmissionFactor`).
    /// GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6 (D1 revised — full spec
    /// surface): typed accessor, `gltf` 1.4.1 has the transmission
    /// feature (see `transmission_factor` above).
    pub transmission_texture: Option<u32>,
    /// `KHR_materials_clearcoat`'s `clearcoatFactor` (default `0.0` — glTF's
    /// own implicit default, and the value that makes G-P5's coat lobe
    /// exactly inert). GLB_CONFORMANCE_DESIGN.md G-P5/D5: parsed by raw
    /// `extensions` JSON, not a typed accessor — gltf 1.4.1 has no
    /// `KHR_materials_clearcoat` feature (VERIFIED against the pinned
    /// crate's own `Cargo.toml`/source this session: no such feature name
    /// exists in either `gltf` or `gltf-json` 1.4.1, unlike `specular`/
    /// `ior` in G-P4 — see the G-P5 execution report). Same raw-JSON-sniff
    /// doctrine `specular_texture` already uses for map presence.
    pub clearcoat_factor: f32,
    /// `KHR_materials_clearcoat`'s `clearcoatRoughnessFactor` (default
    /// `0.0`).
    pub clearcoat_roughness_factor: f32,
    /// `clearcoatTexture` index, if any (R channel scales
    /// `clearcoatFactor`). GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6 (D1
    /// revised — full spec surface): E1/G-P5 mapped factors only; the
    /// texture-completion sweep closes this gap the same way E3/E4/E5
    /// closed sheen/iridescence/anisotropy.
    pub clearcoat_texture: Option<u32>,
    /// `clearcoatRoughnessTexture` index, if any (G channel scales
    /// `clearcoatRoughnessFactor`).
    pub clearcoat_roughness_texture: Option<u32>,
    /// `clearcoatNormalTexture` index, if any — a standard tangent-space
    /// normal map (same RGB convention as the base `normalTexture`)
    /// perturbing ONLY the clearcoat lobe's normal; absent falls back to
    /// the base shading normal (the Khronos-documented default).
    pub clearcoat_normal_texture: Option<u32>,
    /// `KHR_materials_sheen`'s `sheenColorFactor` (default `[0,0,0]` —
    /// glTF's own implicit default, and the value that makes E1's sheen
    /// lobe exactly inert). GLTF_MATERIAL_EXTENSIONS_DESIGN.md E1:
    /// raw-JSON sniff (`extension_value`), same doctrine as clearcoat — no
    /// typed accessor exists for `KHR_materials_sheen` in `gltf`/
    /// `gltf-json` 1.4.1 (VERIFIED this session: no such Cargo feature in
    /// either crate).
    pub sheen_color_factor: [f32; 3],
    /// `KHR_materials_sheen`'s `sheenRoughnessFactor` (default `0.0`).
    pub sheen_roughness_factor: f32,
    /// `true` when `KHR_materials_sheen` carries a `sheenColorTexture`
    /// and/or `sheenRoughnessTexture`.
    /// GLTF_MATERIAL_EXTENSIONS_DESIGN.md E3 (D1 revised — full spec
    /// surface per family): index into `document.textures()` for
    /// `sheenColorTexture`, if any.
    pub sheen_color_texture: Option<u32>,
    /// `sheenRoughnessTexture`'s texture index, if any.
    pub sheen_roughness_texture: Option<u32>,
    /// `KHR_materials_iridescence`'s `iridescenceFactor` (default `0.0`,
    /// inert). Raw-JSON sniff — no typed accessor at 1.4.1.
    pub iridescence_factor: f32,
    /// `KHR_materials_iridescence`'s `iridescenceIor` (default `1.3` —
    /// glTF's own implicit default for the thin-film layer's IOR).
    pub iridescence_ior: f32,
    /// `KHR_materials_iridescence`'s `iridescenceThicknessMinimum`
    /// (default `100.0`, nanometres).
    pub iridescence_thickness_minimum: f32,
    /// `KHR_materials_iridescence`'s `iridescenceThicknessMaximum`
    /// (default `400.0`, nanometres).
    pub iridescence_thickness_maximum: f32,
    /// `true` when `KHR_materials_iridescence` carries an
    /// `iridescenceTexture` and/or `iridescenceThicknessTexture`.
    /// GLTF_MATERIAL_EXTENSIONS_DESIGN.md E4 (D1 revised — full spec
    /// surface per family): index into `document.textures()` for
    /// `iridescenceTexture`, if any (R channel = iridescenceFactor scale).
    pub iridescence_texture: Option<u32>,
    /// `iridescenceThicknessTexture`'s texture index, if any (G channel =
    /// lerp between thickness min/max).
    pub iridescence_thickness_texture: Option<u32>,
    /// `KHR_materials_anisotropy`'s `anisotropyStrength` (default `0.0`,
    /// inert). Raw-JSON sniff — no typed accessor at 1.4.1.
    pub anisotropy_strength: f32,
    /// `KHR_materials_anisotropy`'s `anisotropyRotation` (default `0.0`,
    /// radians).
    pub anisotropy_rotation: f32,
    /// `true` when `KHR_materials_anisotropy` carries an
    /// `anisotropyTexture`.
    /// GLTF_MATERIAL_EXTENSIONS_DESIGN.md E5 (D1 revised — full spec
    /// surface per family): index into `document.textures()` for
    /// `anisotropyTexture`, if any (RG = rotation cos/sin, B = strength,
    /// per the spec's tangent-space encoding).
    pub anisotropy_texture: Option<u32>,
    /// `KHR_materials_dispersion`'s single `dispersion` factor (default
    /// `0.0`, inert). Raw-JSON sniff — no typed accessor at 1.4.1. The
    /// extension defines no texture, so there is no `*_has_texture`
    /// companion field for it.
    pub dispersion: f32,
    /// `KHR_materials_volume`'s `thicknessFactor` (default `0.0` — a
    /// thin-walled surface, glTF's own implicit default). Typed accessor
    /// (`Material::volume()`) — `KHR_materials_volume` HAS crate support
    /// at 1.4.1 (VERIFIED this session: real Cargo feature on both
    /// `gltf`/`gltf-json`), unlike sheen/iridescence/anisotropy/dispersion
    /// above.
    pub volume_thickness_factor: f32,
    /// `KHR_materials_volume`'s `attenuationDistance` (default
    /// `f32::INFINITY` — glTF's own implicit default, meaning "no
    /// attenuation"; the `gltf` 1.4.1 crate's own
    /// `AttenuationDistance::default()` uses the identical sentinel, so
    /// this is not a MANIFOLD-invented substitute).
    pub volume_attenuation_distance: f32,
    /// `KHR_materials_volume`'s `attenuationColor` (default `[1,1,1]` —
    /// neutral, no tint).
    pub volume_attenuation_color: [f32; 3],
    /// `thicknessTexture` index, if any (G channel scales
    /// `thicknessFactor`). GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6 (D1
    /// revised — full spec surface): E1/E2 mapped the factor only; the
    /// texture-completion sweep closes this gap.
    pub volume_thickness_texture: Option<u32>,
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
    /// `[1,1,1]`).
    pub specular_color_factor: [f32; 3],
    /// `specularTexture` index, if any (ALPHA channel scales
    /// `specularFactor` per spec). GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6
    /// (D1 revised — full spec surface): G-P4 mapped factors only; the
    /// texture-completion sweep closes this gap.
    pub specular_texture: Option<u32>,
    /// `specularColorTexture` index, if any (RGB, sRGB — tints
    /// `specularColorFactor`).
    pub specular_color_texture: Option<u32>,
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
    /// GLTF_ANIMATION_DESIGN.md A1/A4: this object's resolved TRS
    /// animation, ONE ENTRY PER PARSED CLIP (aligned to the document's
    /// `animations()` index — A4's D4 clip selection), when its geometry is
    /// contributed by exactly one mesh-owning node (see
    /// [`resolve_object_animation`]) whose ancestor chain carries a
    /// translation/rotation/scale keyframe track in that clip. A clip entry
    /// is `None` when this object isn't animated in that specific clip;
    /// the whole vector is empty for a static object, or when more than one
    /// node contributes this material's geometry (the documented
    /// multi-node-per-material scope boundary — never partially or
    /// incorrectly animated). BUG-207: resolved for the synthetic
    /// default-material entry too, from `nodes_by_material`'s `None`
    /// bucket — the sentinel is a first-class key, not a special case.
    pub animations: Vec<Option<GltfObjectAnimation>>,
    /// GLTF_ANIMATION_DESIGN.md A2 (D2): this object's resolved skin
    /// topology, when its geometry is contributed by exactly one
    /// mesh-owning node (the SAME single-node scope boundary `animation`
    /// above uses) whose `node.skin()` is present. `None` for a static or
    /// rigid-animated object, or when more than one node contributes this
    /// material's geometry. BUG-207: resolved for the synthetic
    /// default-material entry too (see `animations` above).
    pub skin: Option<GltfObjectSkin>,
    /// GLTF_ANIMATION_DESIGN.md A3: this object's resolved morph-target
    /// topology, when its geometry is contributed by exactly one
    /// mesh-owning node (the SAME single-node scope boundary `animation`/
    /// `skin` above use) whose mesh carries `targets` on the primitive(s)
    /// contributing this material. `None` for an unmorphed object or the
    /// multi-node case. BUG-207: resolved for the synthetic
    /// default-material entry too (see `animations` above).
    pub morph: Option<GltfObjectMorph>,
    /// GLTF_ANIM_RUNTIME_V2_DESIGN.md D4 (P3): this object's resolved
    /// node-slot rigid-animation topology — see
    /// [`GltfObjectRigidMultiNode`]'s doc for the two cases that set it.
    /// `None` for a static, single-ancestor-animated single-node, or
    /// skinned object. Mutually exclusive with `skin` (a skinned object
    /// never also gets a node-slot palette) but independent of
    /// `animations` (always empty when this is `Some`).
    pub rigid_multi_node: Option<GltfObjectRigidMultiNode>,
    /// BUG-221: this object's OWN world-space bounding-box center — distinct
    /// from `GltfImportSummary::bbox_min/max`'s SCENE-WIDE center, which is
    /// the ONE value every object was previously recentered by (correct for
    /// whole-scene framing/layout, but it leaves each object's own local
    /// origin wherever the source glTF authored it, so rotating the
    /// object's `node.transform_3d` — which always rotates about local
    /// `(0,0,0)`, `render_scene.rs`'s `model_matrix` — swings it about a
    /// point that can be far from its visual center). Computed by the same
    /// per-material world-space bbox walk as `vertex_count`
    /// (`summarize_node`'s `per_material_bbox` accumulator), so it is
    /// world-combined/instance-aware exactly like the scene-wide bbox.
    /// `gltf_import.rs` uses it to recenter this object's OWN mesh source
    /// about its own center (so local `(0,0,0)` becomes the visual center)
    /// and to reposition the outward-facing `node.transform_3d`'s pos so
    /// net world placement is unchanged — see `build_object_group`.
    pub own_center: [f32; 3],
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

/// Compare a primitive's `material().index()` against a `material_index`
/// that may be [`DEFAULT_MATERIAL_SENTINEL`] — the single comparison every
/// skin/morph resolution path in this file must use so a materialless
/// primitive (glTF `None`) and the synthetic default-material entry mean
/// the same thing on both sides.
fn primitive_material_matches(primitive_material_index: Option<usize>, material_index: u32) -> bool {
    if material_index == DEFAULT_MATERIAL_SENTINEL {
        primitive_material_index.is_none()
    } else {
        primitive_material_index == Some(material_index as usize)
    }
}

/// Human-readable label for a `material_index` in an error/report message —
/// the sentinel reads as "the default material" instead of printing
/// `u32::MAX`.
fn material_label(material_index: u32) -> String {
    if material_index == DEFAULT_MATERIAL_SENTINEL {
        "the default material".to_string()
    } else {
        format!("material {material_index}")
    }
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

/// GLTF_MATERIAL_EXTENSIONS_DESIGN.md E1: finite stand-in for
/// `KHR_materials_volume`'s `attenuationDistance` spec default of
/// `+infinity` ("no attenuation beneath the surface"). Chosen large
/// enough that Beer-Lambert transmittance
/// (`exp(-distance_travelled / attenuation_distance)`, E2's shading math)
/// is indistinguishable from `1.0` — no attenuation — at any distance a
/// real MANIFOLD scene can produce (world units are typically single/low
/// double digits; this is six orders of magnitude beyond that), so it's a
/// byte-identical-in-effect substitute for the spec's true infinity, not
/// an approximation that changes behavior. A true `f32::INFINITY` is not
/// usable here: `serde_json` errors serializing a non-finite float, and
/// this is the default for every glTF import that doesn't carry an
/// explicit `attenuationDistance` — i.e. almost every asset.
pub(crate) const VOLUME_ATTENUATION_DISTANCE_NO_ATTENUATION: f32 = 1.0e6;

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
    /// GLTF_ANIMATION_DESIGN.md A1: every `document.animations()` entry,
    /// parsed as a per-node list of TRS keyframe tracks — raw, unresolved
    /// against any particular material/object. `gltf_import.rs` doesn't
    /// consume this directly; it consumes [`GltfMaterialInfo::animations`]
    /// (the per-object RESOLVED animation, one entry per clip in this
    /// list). Kept on the summary so a parse-only smoke test can assert the
    /// parser sees the right channels on an asset it was never wired
    /// against, independent of the per-material resolution step.
    /// GLTF_ANIMATION_DESIGN.md A2: also read directly by `gltf_import.rs`
    /// to build `node.gltf_skeleton_pose`'s per-joint Table params (a
    /// joint's animated TRS track, looked up by node index per clip). A4:
    /// `gltf_import.rs` now loops every entry here (not just `[0]`) to
    /// build the compound-keyed `(clip_index, joint_index)` Tables D4's
    /// clip selector reads at runtime.
    pub animations: Vec<GltfAnimationInfo>,
    /// Non-fatal animation parse findings — a non-LINEAR interpolation
    /// channel (STEP/CUBICSPLINE, Deferred past A1), a morph-weight
    /// channel (A3 scope), an unreadable channel (e.g. a target that
    /// couldn't be resolved), or a same-object TRS-channel conflict
    /// (§"resolve_object_animation") — one line per occurrence, never a
    /// silent drop. `gltf_import.rs` folds these into `ImportReport::report_lines`.
    pub animation_report_lines: Vec<String>,
    /// BUG-213: `extensionsUsed` entries that are NOT in
    /// [`MANIFOLD_SUPPORTED_EXTENSIONS`] and NOT in `extensionsRequired`
    /// (those hard-fail in [`parse_document_and_buffers`]) — optional
    /// extensions the asset lists that we don't implement, so the import
    /// proceeds without them. One line per extension, never a silent skip.
    /// `gltf_import.rs` folds these into `ImportReport::report_lines`.
    pub extension_report_lines: Vec<String>,
}

/// glTF sampler interpolation mode, as encoded in the runtime track
/// tables' trailing `mode` column (0/1/2 — `node_graph::primitives::
/// gltf_anim_shared`'s `Interp`) — IMPORT_ANYTHING_WAVE_DESIGN.md W2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GltfInterp {
    #[default]
    Linear,
    Step,
    CubicSpline,
}

impl GltfInterp {
    fn from_gltf(interp: gltf::animation::Interpolation) -> Self {
        match interp {
            gltf::animation::Interpolation::Linear => GltfInterp::Linear,
            gltf::animation::Interpolation::Step => GltfInterp::Step,
            gltf::animation::Interpolation::CubicSpline => GltfInterp::CubicSpline,
        }
    }

}

/// One keyframe track (glTF `translation`/`scale` channel), any of the
/// three glTF sampler interpolation modes — GLTF_ANIMATION_DESIGN.md A1,
/// extended by IMPORT_ANYTHING_WAVE_DESIGN.md W2. `times` are seconds,
/// non-decreasing per spec (`input` accessor); `values[i]` is the pose at
/// `times[i]`. `in_tangents`/`out_tangents` are populated (same length as
/// `values`) only when `mode == CubicSpline`; empty otherwise — glTF's own
/// per-keyframe in-tangent/out-tangent triple, needed for the Hermite
/// sampler in `gltf_anim_shared.rs`.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Vec3Track {
    pub times: Vec<f32>,
    pub values: Vec<[f32; 3]>,
    pub mode: GltfInterp,
    pub in_tangents: Vec<[f32; 3]>,
    pub out_tangents: Vec<[f32; 3]>,
}

/// One quaternion (`[x, y, z, w]`) keyframe track (glTF `rotation`
/// channel), any interpolation mode. LINEAR/STEP sampling slerps/holds; see
/// [`Vec3Track`] for the CUBICSPLINE tangent convention.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct QuatTrack {
    pub times: Vec<f32>,
    pub values: Vec<[f32; 4]>,
    pub mode: GltfInterp,
    pub in_tangents: Vec<[f32; 4]>,
    pub out_tangents: Vec<[f32; 4]>,
}


/// One LINEAR-sampled morph-target-weights keyframe track (glTF `weights`
/// channel target path) — GLTF_ANIMATION_DESIGN.md A3. glTF interleaves
/// every target's weight into one flat output accessor per keyframe
/// (`count = input.count * target_count`); de-interleaved here so
/// `values[i]` is keyframe `i`'s full per-target weight vector (length
/// `target_count`), the same "one row per keyframe" shape [`Vec3Track`]
/// uses for translation/scale, just N-wide instead of 3-wide.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WeightsTrack {
    pub times: Vec<f32>,
    pub values: Vec<Vec<f32>>,
}

/// One glTF node's animated TRS channels within a single animation clip.
/// Any of the three may be absent (that channel isn't animated on this
/// node in this clip) — never fabricated.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct GltfNodeAnimation {
    pub node_index: usize,
    pub translation: Option<Vec3Track>,
    pub rotation: Option<QuatTrack>,
    pub scale: Option<Vec3Track>,
    /// GLTF_ANIMATION_DESIGN.md A3: this node's morph-target `weights`
    /// channel, when present. Populated the same single-node-owns-this-
    /// channel way TRS channels are — a `weights` channel targets the
    /// mesh-owning node itself (glTF 2.0 §3.7.2.1: it animates that node's
    /// morph weights, overriding `mesh.weights`), so unlike TRS there is
    /// no ancestor-chain composition to resolve — `gltf_import.rs` looks
    /// this up directly by the contributing node's index.
    pub weights: Option<WeightsTrack>,
}

/// One `document.animations()` entry (a glTF "clip"), parsed into its
/// per-node TRS tracks. glTF's `animations[]` is a list (`Fox` ships
/// three) — A1 always resolves against entry `[0]` (multi-clip selection
/// is D4, deferred to A4); later entries are still parsed (so the smoke
/// test can see them) but not wired.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct GltfAnimationInfo {
    pub name: Option<String>,
    pub nodes: Vec<GltfNodeAnimation>,
    /// One line per channel this parse saw but didn't turn into a track:
    /// non-LINEAR interpolation, morph-weight targets, or an unreadable
    /// target (e.g. `KHR_animation_pointer`, which the pinned `gltf-json`
    /// 1.4.1 can't even deserialize when the extension omits
    /// `target.node` — see BUG_BACKLOG.md). Never silent.
    pub skipped_channels: Vec<String>,
}

/// One object's (= one material's) RESOLVED animation — the merge of
/// whichever ancestor-chain node carries each TRS channel. See
/// [`resolve_object_animation`] for why a chain walk is necessary:
/// `BoxAnimated.glb` itself splits its single animated object's
/// translation onto an ANCESTOR node and rotation onto the mesh's own
/// node, so "the node that owns this material's mesh" is not enough.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GltfObjectAnimation {
    /// Clip duration in SECONDS (glTF's own unit) — the last keyframe
    /// time across whichever channels are present. Never converted to
    /// beats here (D3: seconds→beats happens at RUNTIME, live against the
    /// current tempo, never baked at import).
    pub duration_s: f32,
    pub translation: Option<Vec3Track>,
    pub rotation: Option<QuatTrack>,
    pub scale: Option<Vec3Track>,
    /// GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: the scene-node index EACH
    /// channel's track came from — one per channel, NOT a single
    /// object-wide node. Confirmed load-bearing by `BoxAnimated.glb`
    /// itself (the design's own canonical A1 fixture): its translation
    /// channel targets node 0 (an ancestor), its rotation channel targets
    /// node 2 (the mesh node) — two DIFFERENT physical nodes for the SAME
    /// object, not a rare edge case. `None` when that channel isn't
    /// animated at all (mirrors the sibling `Option<Vec3Track/QuatTrack>`
    /// field). Stamped onto `node.gltf_animation_source`'s `translation_
    /// node`/`rotation_node`/`scale_node` params so each channel samples
    /// from its OWN correct entry in the shared, file-keyed `GltfAnimSet`
    /// (whose `Channel`s are keyed by node index) instead of a per-object
    /// baked Table.
    pub translation_node: Option<usize>,
    pub rotation_node: Option<usize>,
    pub scale_node: Option<usize>,
}

/// Parse every `document.animations()` entry into its per-node TRS
/// tracks. LINEAR, STEP, and CUBICSPLINE interpolation are all supported
/// for translation/rotation/scale channels (GLTF_ANIMATION_DESIGN.md A1 +
/// IMPORT_ANYTHING_WAVE_DESIGN.md W2). Morph-weight channels stay
/// LINEAR-only (A3 scope, unchanged by W2) — a non-LINEAR weights channel
/// still lands in `skipped_channels`, same as an unreadable weights
/// accessor (a length that doesn't divide evenly by the keyframe count).
pub(crate) fn parse_animations(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
) -> Vec<GltfAnimationInfo> {
    document
        .animations()
        .map(|anim| {
            let mut nodes: std::collections::BTreeMap<usize, GltfNodeAnimation> =
                std::collections::BTreeMap::new();
            let mut skipped_channels = Vec::new();
            for channel in anim.channels() {
                let sampler = channel.sampler();
                let target = channel.target();
                let node_index = target.node().index();
                let interpolation = sampler.interpolation();
                let is_weights = target.property() == gltf::animation::Property::MorphTargetWeights;
                if is_weights && interpolation != gltf::animation::Interpolation::Linear {
                    skipped_channels.push(format!(
                        "node {node_index}: {interpolation:?} interpolation not supported on \
                         morph-weight channels (GLTF_ANIMATION_DESIGN.md A3 is LINEAR-only)"
                    ));
                    continue;
                }
                let mode = GltfInterp::from_gltf(interpolation);
                let reader = channel.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));
                let Some(times) = reader.read_inputs().map(|it| it.collect::<Vec<f32>>()) else {
                    skipped_channels.push(format!(
                        "node {node_index}: channel sampler input accessor unreadable"
                    ));
                    continue;
                };
                let Some(outputs) = reader.read_outputs() else {
                    skipped_channels
                        .push(format!("node {node_index}: channel sampler output unreadable"));
                    continue;
                };
                let entry = nodes.entry(node_index).or_insert_with(|| GltfNodeAnimation {
                    node_index,
                    ..Default::default()
                });
                match outputs {
                    gltf::animation::util::ReadOutputs::Translations(it) => {
                        let raw: Vec<[f32; 3]> = it.collect();
                        entry.translation = Some(vec3_track_from_raw(times, raw, mode));
                    }
                    gltf::animation::util::ReadOutputs::Scales(it) => {
                        let raw: Vec<[f32; 3]> = it.collect();
                        entry.scale = Some(vec3_track_from_raw(times, raw, mode));
                    }
                    gltf::animation::util::ReadOutputs::Rotations(it) => {
                        let raw: Vec<[f32; 4]> = it.into_f32().collect();
                        entry.rotation = Some(quat_track_from_raw(times, raw, mode));
                    }
                    gltf::animation::util::ReadOutputs::MorphTargetWeights(it) => {
                        // glTF interleaves every target's weight per
                        // keyframe into one flat accessor
                        // (GLTF_ANIMATION_DESIGN.md A3) — de-interleave
                        // into one `target_count`-wide row per keyframe.
                        // `it` yields `f32`/normalized-int weights
                        // regardless of the accessor's storage type.
                        let flat: Vec<f32> = it.into_f32().collect();
                        let target_count = if times.is_empty() { 0 } else { flat.len() / times.len() };
                        if target_count == 0 || flat.len() != times.len() * target_count {
                            skipped_channels.push(format!(
                                "node {node_index}: weights channel output length {} doesn't \
                                 divide evenly by {} keyframes — unreadable",
                                flat.len(),
                                times.len()
                            ));
                        } else {
                            let values: Vec<Vec<f32>> =
                                flat.chunks_exact(target_count).map(|c| c.to_vec()).collect();
                            entry.weights = Some(WeightsTrack { times, values });
                        }
                    }
                }
            }
            GltfAnimationInfo {
                name: anim.name().map(str::to_string),
                nodes: nodes.into_values().collect(),
                skipped_channels,
            }
        })
        .collect()
}

/// Build a [`Vec3Track`] from a channel's raw decoded output. For
/// CUBICSPLINE, glTF's output accessor packs `3 * times.len()` elements —
/// per keyframe, an in-tangent, the value, and an out-tangent, in that
/// order (glTF 2.0 spec, `Interpolation::CubicSpline` doc) — de-interleaved
/// here into three same-length arrays. LINEAR/STEP outputs are `times.len()`
/// values with no tangents.
fn vec3_track_from_raw(times: Vec<f32>, raw: Vec<[f32; 3]>, mode: GltfInterp) -> Vec3Track {
    if mode == GltfInterp::CubicSpline {
        let n = times.len();
        let mut in_tangents = Vec::with_capacity(n);
        let mut values = Vec::with_capacity(n);
        let mut out_tangents = Vec::with_capacity(n);
        for triple in raw.chunks_exact(3) {
            in_tangents.push(triple[0]);
            values.push(triple[1]);
            out_tangents.push(triple[2]);
        }
        Vec3Track { times, values, mode, in_tangents, out_tangents }
    } else {
        Vec3Track { times, values: raw, mode, in_tangents: Vec::new(), out_tangents: Vec::new() }
    }
}

/// Same as [`vec3_track_from_raw`] for a quaternion (`[x, y, z, w]`) track.
fn quat_track_from_raw(times: Vec<f32>, raw: Vec<[f32; 4]>, mode: GltfInterp) -> QuatTrack {
    if mode == GltfInterp::CubicSpline {
        let n = times.len();
        let mut in_tangents = Vec::with_capacity(n);
        let mut values = Vec::with_capacity(n);
        let mut out_tangents = Vec::with_capacity(n);
        for triple in raw.chunks_exact(3) {
            in_tangents.push(triple[0]);
            values.push(triple[1]);
            out_tangents.push(triple[2]);
        }
        QuatTrack { times, values, mode, in_tangents, out_tangents }
    } else {
        QuatTrack { times, values: raw, mode, in_tangents: Vec::new(), out_tangents: Vec::new() }
    }
}

/// For every mesh-owning node in the scene, its ancestor chain (root
/// node first, the mesh-owning node itself last) — GLTF_ANIMATION_DESIGN.md
/// A1: a material's animated object can have its TRS channels split
/// across MULTIPLE nodes on the path from the scene root down to the
/// node that actually carries the mesh (verified against
/// `BoxAnimated.glb`: its single animated object's translation lives on
/// an ancestor node, rotation on the mesh's own node — neither alone).
/// Resolving a material's animation therefore requires the whole chain,
/// not just the leaf.
fn collect_mesh_node_chains(document: &gltf::Document) -> Vec<(usize, Vec<usize>)> {
    fn walk(node: &gltf::Node, chain: &mut Vec<usize>, out: &mut Vec<(usize, Vec<usize>)>) {
        chain.push(node.index());
        if node.mesh().is_some() {
            out.push((node.index(), chain.clone()));
        }
        for child in node.children() {
            walk(&child, chain, out);
        }
        chain.pop();
    }
    let mut out = Vec::new();
    for node in resolve_import_nodes(document) {
        let mut chain = Vec::new();
        walk(&node, &mut chain, &mut out);
    }
    out
}

/// Merge whichever node(s) along `chain` carry each TRS channel (within
/// ONE animation clip's per-node map, `node_anims`) into a single
/// resolved [`GltfObjectAnimation`] for the material this chain's leaf
/// mesh belongs to. `None` when no chain node carries any TRS channel
/// (the object is static). GLTF_ANIM_RUNTIME_V2_DESIGN.md D4 (P3): a
/// channel duplicated across more than one chain node — composing two
/// independently-animated ancestors into one TRS track isn't generally
/// valid — sets `*ambiguous = true` instead of reporting-and-dropping;
/// the caller ([`resolve_animations_for_key`]) discards this function's
/// whole return value in that case and instead resolves the object as a
/// 1-slot [`GltfObjectRigidMultiNode`], whose whole-hierarchy matrix
/// composition handles any number of animated ancestors correctly.
fn resolve_object_animation(
    chain: &[usize],
    node_anims: &std::collections::BTreeMap<usize, GltfNodeAnimation>,
    ambiguous: &mut bool,
) -> Option<GltfObjectAnimation> {
    let mut translation: Option<Vec3Track> = None;
    let mut rotation: Option<QuatTrack> = None;
    let mut scale: Option<Vec3Track> = None;
    let mut translation_node: Option<usize> = None;
    let mut rotation_node: Option<usize> = None;
    let mut scale_node: Option<usize> = None;
    for &node_index in chain {
        let Some(na) = node_anims.get(&node_index) else { continue };
        if let Some(t) = &na.translation {
            if translation.is_some() {
                *ambiguous = true;
            } else {
                translation = Some(t.clone());
                translation_node = Some(node_index);
            }
        }
        if let Some(r) = &na.rotation {
            if rotation.is_some() {
                *ambiguous = true;
            } else {
                rotation = Some(r.clone());
                rotation_node = Some(node_index);
            }
        }
        if let Some(s) = &na.scale {
            if scale.is_some() {
                *ambiguous = true;
            } else {
                scale = Some(s.clone());
                scale_node = Some(node_index);
            }
        }
    }
    if translation.is_none() && rotation.is_none() && scale.is_none() {
        return None;
    }
    let last_time = |t: &[f32]| t.last().copied().unwrap_or(0.0);
    let duration_s = [
        translation.as_ref().map(|t| last_time(&t.times)),
        rotation.as_ref().map(|t| last_time(&t.times)),
        scale.as_ref().map(|t| last_time(&t.times)),
    ]
    .into_iter()
    .flatten()
    .fold(0.0f32, f32::max);
    Some(GltfObjectAnimation {
        duration_s,
        translation,
        rotation,
        scale,
        translation_node,
        rotation_node,
        scale_node,
    })
}

// ─── GLTF_ANIMATION_DESIGN.md A2 — skinning (D2) ───────────────────────

/// One glTF `skins[]` entry's PARSE-TIME-STATIC topology — which node is
/// which joint (palette index = position in `joint_node_indices`, the
/// same order JOINTS_0 accessor values index into), each joint's parent
/// WITHIN the joint list (or -1), and the inverse-bind matrices. Per-frame
/// joint LOCAL poses (bind or animated) are resolved separately by
/// `node.gltf_skeleton_pose` from `GltfObjectSkin`'s bind-pose + Table
/// tracks — this struct carries only what parsing can fix once.
#[derive(Debug, Clone)]
pub(crate) struct GltfSkinInfo {
    pub joint_node_indices: Vec<usize>,
    /// Index (into THIS skin's own joint list) of each joint's parent, or
    /// `-1` when the joint's real scene-graph parent is not itself a
    /// joint in this skin (the common case for joint 0, the skeleton
    /// root).
    pub joint_parent: Vec<i32>,
    /// Static world transform of the node chain ABOVE the joint tree, for
    /// joints whose `joint_parent == -1` — composed once at parse time
    /// from that ancestor chain's own node transforms (glTF's `matrix()`/
    /// TRS, whichever the node specifies). A real asset whose
    /// ancestor-of-root is ITSELF animated is a scope boundary A2 does
    /// not resolve (not exercised by CesiumMan/Fox/BrainStem — re-derive
    /// if a future asset needs it): that animation is silently not
    /// applied here, never a crash.
    pub joint_root_world: Vec<Mat4>,
    pub inverse_bind_matrices: Vec<Mat4>,
    // GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: the per-joint bind-TRS fields that
    // used to live here (`joint_bind_translation`/`_rotation`/`_scale`)
    // are DELETED — deduped into `GltfAnimSet::node_bind_trs` (P1, indexed
    // by whole-scene node index rather than duplicated per-skin), which
    // `gltf_skeleton_pose::sample_joint_local` now reads exclusively.
}

/// Every node's parent, indexed by node index (`None` = root). Built once
/// per document parse — O(N) over the node tree.
pub(crate) fn build_parent_map(document: &gltf::Document) -> Vec<Option<usize>> {
    let mut parent_of: Vec<Option<usize>> = vec![None; document.nodes().len()];
    for node in document.nodes() {
        for child in node.children() {
            parent_of[child.index()] = Some(node.index());
        }
    }
    parent_of
}

/// Static (non-animated) world matrix of `node_index`, composed by
/// walking UP via `parent_of` to the scene root and multiplying
/// `node.transform().matrix()` root-first. Used only for the (rare)
/// ancestor chain ABOVE a skin's joint tree — see
/// `GltfSkinInfo::joint_root_world`.
fn static_world_matrix(document: &gltf::Document, node_index: usize, parent_of: &[Option<usize>]) -> Mat4 {
    let mut chain = vec![node_index];
    let mut cur = node_index;
    while let Some(p) = parent_of.get(cur).copied().flatten() {
        chain.push(p);
        cur = p;
    }
    chain.reverse(); // root-first
    let mut world = MAT4_IDENTITY;
    for idx in chain {
        if let Some(node) = document.nodes().nth(idx) {
            world = mat4_mul(&world, &node.transform().matrix());
        }
    }
    world
}

/// Parse every `document.skins()` entry, indexed by glTF skin index. A
/// skin with no `inverseBindMatrices` accessor (spec-legal — implies
/// identity per joint) falls back to `MAT4_IDENTITY` for every joint.
pub(crate) fn parse_skins(document: &gltf::Document, buffers: &[gltf::buffer::Data]) -> Vec<GltfSkinInfo> {
    let parent_of = build_parent_map(document);
    document
        .skins()
        .map(|skin| {
            let joint_node_indices: Vec<usize> = skin.joints().map(|n| n.index()).collect();
            let joint_position: std::collections::HashMap<usize, usize> = joint_node_indices
                .iter()
                .enumerate()
                .map(|(pos, &node_idx)| (node_idx, pos))
                .collect();
            let mut joint_parent = Vec::with_capacity(joint_node_indices.len());
            let mut joint_root_world = Vec::with_capacity(joint_node_indices.len());
            for &node_idx in &joint_node_indices {
                let parent_node = parent_of.get(node_idx).copied().flatten();
                match parent_node.and_then(|p| joint_position.get(&p)) {
                    Some(&pos) => {
                        joint_parent.push(pos as i32);
                        joint_root_world.push(MAT4_IDENTITY);
                    }
                    None => {
                        joint_parent.push(-1);
                        let root_world = match parent_node {
                            Some(p) => static_world_matrix(document, p, &parent_of),
                            None => MAT4_IDENTITY,
                        };
                        joint_root_world.push(root_world);
                    }
                }
            }
            let reader = skin.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));
            let inverse_bind_matrices: Vec<Mat4> = match reader.read_inverse_bind_matrices() {
                Some(it) => it.collect(),
                None => vec![MAT4_IDENTITY; joint_node_indices.len()],
            };
            GltfSkinInfo { joint_node_indices, joint_parent, joint_root_world, inverse_bind_matrices }
        })
        .collect()
}

/// This render object's (= one material's) resolved skin topology —
/// `GltfSkinInfo` plus which skin index it came from (for report
/// messages). `gltf_import.rs` reads this to build `node.gltf_skeleton_pose`'s
/// Table params; per-vertex JOINTS_0/WEIGHTS_0 live separately, read at
/// RUNTIME by `node.gltf_skinned_mesh_source` from the same file.
#[derive(Debug, Clone)]
pub(crate) struct GltfObjectSkin {
    pub skin_index: u32,
    pub info: GltfSkinInfo,
}

// ─── GLTF_ANIM_RUNTIME_V2_DESIGN.md D4 (P3) — rigid multi-node via the
// node-slot palette ──────────────────────────────────────────────────

/// This render object's (= one material's) resolved node-slot topology —
/// the sorted scene-node indices contributing its geometry, slot order =
/// list order (slot `i` = `slot_nodes[i]`). Set when EITHER (a) more than
/// one mesh-owning node contributes this material's geometry, at least one
/// animated in some clip, none skinned (the `:2492` case this design
/// deletes), OR (b) exactly one contributing node whose ancestor chain
/// animates the SAME TRS channel on more than one ancestor (the `:1700`
/// case — composing multiple animated ancestors into one TRS track isn't
/// generally valid, but the whole-hierarchy node-slot palette composes
/// them correctly via matrix multiplication regardless of how many
/// ancestors are animated, so a 1-slot palette subsumes it). `None` for a
/// static or single-ancestor-animated single-node object (those keep the
/// existing `GltfObjectAnimation` TRS-track path) or a skinned object.
/// `gltf_import.rs` wires this the same way as `GltfObjectSkin` — through
/// `node.gltf_skinned_mesh_source` (its `material_index` selector already
/// finds these SAME nodes at runtime, sorted the same way — see
/// `find_material_contributing_nodes`) + `node.skin_mesh`, with
/// `node.gltf_skeleton_pose` in node-slot mode (`skin_index == -2`) instead
/// of skin mode.
#[derive(Debug, Clone)]
pub(crate) struct GltfObjectRigidMultiNode {
    pub slot_nodes: Vec<u32>,
}

/// Every scene-node index (ascending) whose mesh has at least one
/// primitive matching `material_index` — the SAME grouping
/// `gltf_import_summary`'s `nodes_by_material` computes, re-derived here
/// so `node.gltf_skinned_mesh_source`'s runtime loader ([`load_gltf_skinned_mesh`]'s
/// D4 fallback) assigns the IDENTICAL slot order the importer stamped into
/// `node.gltf_skeleton_pose`'s `node_slots` Table param, without needing
/// import-time state plumbed into a per-frame primitive.
pub(crate) fn find_material_contributing_nodes(
    document: &gltf::Document,
    material_index: u32,
) -> Vec<usize> {
    fn walk(node: &gltf::Node, material_index: u32, out: &mut std::collections::BTreeSet<usize>) {
        if let Some(mesh) = node.mesh()
            && mesh.primitives().any(|p| primitive_material_matches(p.material().index(), material_index))
        {
            out.insert(node.index());
        }
        for child in node.children() {
            walk(&child, material_index, out);
        }
    }
    let mut set = std::collections::BTreeSet::new();
    for node in resolve_import_nodes(document) {
        walk(&node, material_index, &mut set);
    }
    set.into_iter().collect()
}

// ─── GLTF_ANIMATION_DESIGN.md A3 — morph targets ───────────────────────

/// This render object's (= one material's) resolved morph-target topology
/// — how many targets, which node's `weights` animation channel (if any)
/// drives them, and the static per-target fallback weight for a target
/// that channel doesn't animate. `gltf_import.rs` reads this (plus the
/// SAME `node_anims_clip0` lookup `build_skeleton_pose_tables` already
/// uses for skinning) to build `node.gltf_morph_weights`'s `weight_tracks`
/// Table; the actual per-vertex delta geometry is loaded separately, at
/// RUNTIME, by `node.gltf_morph_deltas_source` (mirrors how per-vertex
/// JOINTS_0/WEIGHTS_0 live outside `GltfObjectSkin` too — vertex-scale
/// data is never baked into import-time Tables).
#[derive(Debug, Clone)]
pub(crate) struct GltfObjectMorph {
    /// The sole contributing node's index — `node_anims_clip0.get(&this)`
    /// is where an animated `weights` channel for this object would live.
    pub mesh_node_index: usize,
    pub target_count: u32,
    /// `mesh.weights()[i]` for `i in 0..target_count`, defaulted to `0.0`
    /// for any target past a short (or absent) `weights` array — glTF 2.0
    /// §3.7.2.1's spec default for an unauthored target weight. Used as
    /// the fallback for a target this object's `weights` channel (if any)
    /// doesn't animate — mirrors `GltfSkinInfo::joint_bind_translation`'s
    /// "unanimated joint gets its bind-pose static row" convention.
    pub static_weights: Vec<f32>,
}

/// (vertices, per-vertex joint indices as f32, per-vertex weights) — the
/// coincident triple `node.gltf_skinned_mesh_source` uploads as its three
/// array outputs.
pub(crate) type SkinnedMeshData = (Vec<MeshVertex>, Vec<[f32; 4]>, Vec<[f32; 4]>);

/// Flatten ONE skinned mesh-owning node's primitives (filtered to
/// `material_index`) into a triangle-list `MeshVertex` buffer plus
/// coincident per-vertex joint indices (as f32 — exact for the spec's
/// joint-count range) and weights. LOCAL space, NO node-transform applied:
/// per glTF 2.0 §3.7.3.3, a skinned mesh's positioning comes ENTIRELY from
/// the joint hierarchy — the mesh-owning node's own transform is ignored,
/// unlike every static/rigid object this importer otherwise
/// world-transforms via `walk_gltf_node`. A vertex missing JOINTS_0/
/// WEIGHTS_0 on an otherwise-skinned primitive gets the neutral
/// `[0,0,0,0]` / `[1,0,0,0]` pair (100% weight on joint 0) — never
/// silently zero-weighted, which would collapse it to the origin under
/// `node.skin_mesh`'s weighted-sum formula.
pub(crate) fn flatten_skinned_node(
    node: &gltf::Node,
    buffers: &[gltf::buffer::Data],
    material_index: u32,
) -> Result<SkinnedMeshData, String> {
    let mesh = node
        .mesh()
        .ok_or_else(|| format!("node {}: expected a skinned mesh, found none", node.index()))?;
    let mut verts = Vec::new();
    let mut joints = Vec::new();
    let mut weights = Vec::new();
    for primitive in mesh.primitives() {
        if !primitive_material_matches(primitive.material().index(), material_index) {
            continue;
        }
        if primitive.mode() != gltf::mesh::Mode::Triangles {
            return Err(format!(
                "primitive uses non-Triangles mode {:?} — unsupported by node.skin_mesh import",
                primitive.mode()
            ));
        }
        let reader = primitive.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));
        let positions: Vec<[f32; 3]> = reader
            .read_positions()
            .ok_or_else(|| "skinned primitive missing required POSITION accessor".to_string())?
            .collect();
        let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|it| it.collect());
        let uvs: Option<Vec<[f32; 2]>> = reader.read_tex_coords(0).map(|it| it.into_f32().collect());
        let vjoints: Option<Vec<[u16; 4]>> = reader.read_joints(0).map(|it| it.into_u16().collect());
        let vweights: Option<Vec<[f32; 4]>> = reader.read_weights(0).map(|it| it.into_f32().collect());
        let indices: Vec<u32> = match reader.read_indices() {
            Some(idx) => idx.into_u32().collect(),
            None => (0..positions.len() as u32).collect(),
        };
        for tri in indices.chunks_exact(3) {
            let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            if i0 >= positions.len() || i1 >= positions.len() || i2 >= positions.len() {
                return Err(format!(
                    "triangle index ({i0}, {i1}, {i2}) out of range for {} positions",
                    positions.len()
                ));
            }
            let p0 = positions[i0];
            let p1 = positions[i1];
            let p2 = positions[i2];
            let face_normal = normalize3(cross3(sub3(p1, p0), sub3(p2, p0)));
            for &i in &[i0, i1, i2] {
                let normal = normals.as_ref().map_or(face_normal, |ns| ns[i]);
                let uv = uvs.as_ref().map_or([0.0, 0.0], |u| u[i]);
                verts.push(MeshVertex {
                    position: positions[i],
                    _pad0: 0.0,
                    normal,
                    _pad1: 0.0,
                    uv,
                    _pad2: [0.0, 0.0],
                });
                joints.push(match &vjoints {
                    Some(j) => [j[i][0] as f32, j[i][1] as f32, j[i][2] as f32, j[i][3] as f32],
                    None => [0.0, 0.0, 0.0, 0.0],
                });
                weights.push(match &vweights {
                    Some(w) => w[i],
                    None => [1.0, 0.0, 0.0, 0.0],
                });
            }
        }
    }
    Ok((verts, joints, weights))
}

fn find_skinned_node_for_material<'a>(
    node: &gltf::Node<'a>,
    material_index: u32,
) -> Option<gltf::Node<'a>> {
    if node.skin().is_some()
        && let Some(mesh) = node.mesh()
        && mesh.primitives().any(|p| primitive_material_matches(p.material().index(), material_index))
    {
        return Some(node.clone());
    }
    for child in node.children() {
        if let Some(found) = find_skinned_node_for_material(&child, material_index) {
            return Some(found);
        }
    }
    None
}

/// Flatten `slot_nodes`' primitives (filtered to `material_index`) into a
/// triangle-list `MeshVertex` buffer plus coincident per-vertex
/// joints/weights, GLTF_ANIM_RUNTIME_V2_DESIGN.md D4 (P3): LOCAL space per
/// contributing node (no node-transform applied — exactly [`flatten_skinned_node`]'s
/// convention, since D4's rigid palette positions every vertex entirely via
/// its slot's `node.skin_mesh` matrix, same as real skinning). Every vertex
/// gets `joints = [slot, 0, 0, 0]`, `weights = [1, 0, 0, 0]` — 100% rigid
/// weight on its OWN contributing node's slot, never blended across slots
/// (unlike real skinning, D4 objects have no authored JOINTS_0/WEIGHTS_0 to
/// read — every vertex belongs to exactly the one node that emitted it).
/// `slot_nodes` order fixes slot assignment; callers must derive it from
/// [`find_material_contributing_nodes`] so the vertex data and the pose
/// node's `node_slots` topology agree on which slot is which scene node.
fn flatten_rigid_multi_node(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    slot_nodes: &[usize],
    material_index: u32,
) -> Result<SkinnedMeshData, String> {
    let mut verts = Vec::new();
    let mut joints = Vec::new();
    let mut weights = Vec::new();
    for (slot, &node_index) in slot_nodes.iter().enumerate() {
        let node = document
            .nodes()
            .nth(node_index)
            .ok_or_else(|| format!("node {node_index} out of range"))?;
        let Some(mesh) = node.mesh() else { continue };
        for primitive in mesh.primitives() {
            if !primitive_material_matches(primitive.material().index(), material_index) {
                continue;
            }
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                return Err(format!(
                    "primitive uses non-Triangles mode {:?} — unsupported by node.skin_mesh import",
                    primitive.mode()
                ));
            }
            let reader = primitive.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));
            let positions: Vec<[f32; 3]> = reader
                .read_positions()
                .ok_or_else(|| "rigid multi-node primitive missing required POSITION accessor".to_string())?
                .collect();
            let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|it| it.collect());
            let uvs: Option<Vec<[f32; 2]>> = reader.read_tex_coords(0).map(|it| it.into_f32().collect());
            let indices: Vec<u32> = match reader.read_indices() {
                Some(idx) => idx.into_u32().collect(),
                None => (0..positions.len() as u32).collect(),
            };
            for tri in indices.chunks_exact(3) {
                let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
                if i0 >= positions.len() || i1 >= positions.len() || i2 >= positions.len() {
                    return Err(format!(
                        "triangle index ({i0}, {i1}, {i2}) out of range for {} positions",
                        positions.len()
                    ));
                }
                let p0 = positions[i0];
                let p1 = positions[i1];
                let p2 = positions[i2];
                let face_normal = normalize3(cross3(sub3(p1, p0), sub3(p2, p0)));
                for &i in &[i0, i1, i2] {
                    let normal = normals.as_ref().map_or(face_normal, |ns| ns[i]);
                    let uv = uvs.as_ref().map_or([0.0, 0.0], |u| u[i]);
                    verts.push(MeshVertex {
                        position: positions[i],
                        _pad0: 0.0,
                        normal,
                        _pad1: 0.0,
                        uv,
                        _pad2: [0.0, 0.0],
                    });
                    joints.push([slot as f32, 0.0, 0.0, 0.0]);
                    weights.push([1.0, 0.0, 0.0, 0.0]);
                }
            }
        }
    }
    if verts.is_empty() {
        return Err(format!(
            "rigid multi-node object (material {}) contributed no geometry across {} node(s)",
            material_label(material_index),
            slot_nodes.len()
        ));
    }
    Ok((verts, joints, weights))
}

/// Load a `.glb`'s skinned OR (GLTF_ANIM_RUNTIME_V2_DESIGN.md D4, P3)
/// rigid-multi-node-palette geometry for `material_index`. Tries the
/// single skinned contributing node first (A2's original scope,
/// unchanged); when none is found, falls back to D4's node-slot palette
/// across every contributing node ([`find_material_contributing_nodes`]) —
/// `gltf_import.rs` only reaches this fallback when
/// `GltfMaterialInfo::rigid_multi_node` resolved (either the multi-node
/// case or the single-node-ambiguous-ancestor case, both of which this
/// same runtime node-enumeration reproduces identically to the import-time
/// resolution, so slot order always agrees).
pub(crate) fn load_gltf_skinned_mesh(
    path: &std::path::Path,
    material_index: u32,
) -> Result<SkinnedMeshData, String> {
    let (document, buffers, _images) = import_glb(path)?;
    for node in resolve_import_nodes(&document) {
        if let Some(found) = find_skinned_node_for_material(&node, material_index) {
            return flatten_skinned_node(&found, &buffers, material_index);
        }
    }
    let slot_nodes = find_material_contributing_nodes(&document, material_index);
    if !slot_nodes.is_empty() {
        return flatten_rigid_multi_node(&document, &buffers, &slot_nodes, material_index);
    }
    Err(format!(
        "{}: no skinned mesh-owning node found contributing {}",
        path.display(),
        material_label(material_index)
    ))
}

// ─── GLTF_ANIMATION_DESIGN.md A3 — morph target deltas (runtime load) ──

/// Find the first mesh-owning node (DFS, default-scene rooted) whose mesh
/// carries a primitive matching `material_index` — no `skin()` requirement,
/// unlike [`find_skinned_node_for_material`] (a morphed object need not
/// also be skinned; these are independent glTF features).
fn find_mesh_node_for_material<'a>(
    node: &gltf::Node<'a>,
    material_index: u32,
) -> Option<gltf::Node<'a>> {
    if let Some(mesh) = node.mesh()
        && mesh.primitives().any(|p| primitive_material_matches(p.material().index(), material_index))
    {
        return Some(node.clone());
    }
    for child in node.children() {
        if let Some(found) = find_mesh_node_for_material(&child, material_index) {
            return Some(found);
        }
    }
    None
}

/// Flatten one primitive's per-target POSITION/NORMAL morph deltas into
/// `per_target_out[target_index]`, using the SAME triangle-index expansion
/// [`flatten_primitive`] uses for the base mesh — deltas[t][k] must line up
/// 1:1 with `node.gltf_mesh_source`'s base_verts[k] for this same
/// node/material, so `node.morph_targets_blend` can add them directly.
/// `world_linear` (the node's world matrix's upper-3x3, NO translation —
/// a delta is a displacement vector, never a point) transforms position
/// deltas; `normal_mat` (transpose-inverse, matching [`flatten_primitive`]'s
/// normal handling) transforms normal deltas. A target missing its
/// POSITION or NORMAL displacement accessor (both are spec-optional per
/// target) contributes an all-zero delta for that channel, never an error.
/// `per_target_out` is resized to the primitive's target count on first
/// call; a LATER primitive with a different target count is an `Err` (not
/// exercised by the A3 gate fixtures — each ships one primitive per
/// morphed material — re-derive if a future asset needs it).
fn flatten_primitive_morph_deltas(
    primitive: &gltf::Primitive,
    buffers: &[gltf::buffer::Data],
    world_linear: &[[f32; 3]; 3],
    normal_mat: &[[f32; 3]; 3],
    per_target_out: &mut Vec<Vec<MeshVertex>>,
) -> Result<(), String> {
    if primitive.mode() != gltf::mesh::Mode::Triangles {
        return Err(format!(
            "primitive uses non-Triangles mode {:?} — unsupported by node.gltf_morph_deltas_source",
            primitive.mode()
        ));
    }
    let reader = primitive.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));
    let base_vertex_count = reader
        .read_positions()
        .ok_or_else(|| "morphed primitive missing required POSITION accessor".to_string())?
        .count();
    let indices: Vec<u32> = match reader.read_indices() {
        Some(idx) => idx.into_u32().collect(),
        None => (0..base_vertex_count as u32).collect(),
    };

    /// One target's optional POSITION/NORMAL displacement accessors,
    /// fully read into memory (both spec-optional per target).
    type TargetDisplacements = (Option<Vec<[f32; 3]>>, Option<Vec<[f32; 3]>>);
    let targets: Vec<TargetDisplacements> = reader
        .read_morph_targets()
        .map(|(pos, norm, _tangent)| {
            (pos.map(|it| it.collect()), norm.map(|it| it.collect()))
        })
        .collect();

    if per_target_out.is_empty() {
        per_target_out.resize(targets.len(), Vec::new());
    } else if per_target_out.len() != targets.len() {
        return Err(format!(
            "inconsistent morph target count across primitives: {} vs {}",
            per_target_out.len(),
            targets.len()
        ));
    }

    for (t_idx, (pos_disp, norm_disp)) in targets.iter().enumerate() {
        for tri in indices.chunks_exact(3) {
            for &i in tri {
                let i = i as usize;
                let dp = pos_disp.as_ref().and_then(|d| d.get(i)).copied().unwrap_or([0.0, 0.0, 0.0]);
                let dn = norm_disp.as_ref().and_then(|d| d.get(i)).copied().unwrap_or([0.0, 0.0, 0.0]);
                let wp = mat3_mul_vec3(*world_linear, dp);
                let wn = mat3_mul_vec3(*normal_mat, dn);
                per_target_out[t_idx].push(MeshVertex {
                    position: wp,
                    _pad0: 0.0,
                    normal: wn,
                    _pad1: 0.0,
                    uv: [0.0, 0.0],
                    _pad2: [0.0, 0.0],
                });
            }
        }
    }
    Ok(())
}

/// Load a `.glb`'s morph-target deltas for `material_index`'s sole
/// contributing node, flattened target-major
/// (`deltas[target_index * vertex_count + vertex_index]`) — the layout
/// `node.morph_targets_blend`'s `deltas` input expects. `gltf_import.rs`
/// only wires `node.gltf_morph_deltas_source` (which calls this) when
/// `GltfMaterialInfo::morph` resolved. Returns `(deltas, target_count,
/// vertex_count)`.
///
/// `skinned`: BUG-208 — a skin+morph combination on the same object chains
/// `node.morph_targets_blend` between `node.gltf_skinned_mesh_source` and
/// `node.skin_mesh` (glTF applies morph THEN skin, §3.7.2). Per A2/A3,
/// `flatten_skinned_node`'s base vertices are emitted in UNTRANSFORMED
/// bind-pose/local space — `node.gltf_skinned_mesh_source` never applies
/// the mesh-owning node's world matrix, since a skinned object's
/// positioning comes entirely from its joint palette. Morph deltas for
/// that combination must land in the SAME untransformed space so
/// `node.morph_targets_blend`'s additive sum stays index-1:1 with
/// `node.skin_mesh`'s `in` — `skinned == true` skips the world-transform
/// step entirely (identity matrices) instead of applying the node's world
/// matrix. The non-skinned (rigid) path is unchanged: it still applies the
/// node's world transform, matching `node.gltf_mesh_source`'s own
/// world-transform of the base mesh.
pub(crate) fn load_gltf_morph_deltas(
    path: &std::path::Path,
    material_index: u32,
    skinned: bool,
) -> Result<(Vec<MeshVertex>, u32, u32), String> {
    let (document, buffers, _images) = import_glb(path)?;
    let parent_of = build_parent_map(&document);
    for node in resolve_import_nodes(&document) {
        let Some(found) = find_mesh_node_for_material(&node, material_index) else {
            continue;
        };
        // The node's own world transform — SAME matrix `walk_gltf_node`
        // would compose for this node when node.gltf_mesh_source's
        // Material selector flattens the base mesh (assuming no
        // EXT_mesh_gpu_instancing on this node; not exercised by the A3
        // gate fixtures, which each ship a single non-instanced morphed
        // object — re-derive if a future asset needs it). Skipped entirely
        // (identity) for a skinned object — see the `skinned` doc comment
        // above (BUG-208).
        let (world_linear, normal_mat) = if skinned {
            (MAT3_IDENTITY, MAT3_IDENTITY)
        } else {
            let world = static_world_matrix(&document, found.index(), &parent_of);
            let world_linear = mat3_upper_row_major(&world);
            let normal_mat = mat3_transpose(mat3_inverse(world_linear));
            (world_linear, normal_mat)
        };
        let mesh = found.mesh().ok_or_else(|| "matched node has no mesh".to_string())?;
        let mut per_target: Vec<Vec<MeshVertex>> = Vec::new();
        for primitive in mesh.primitives() {
            if !primitive_material_matches(primitive.material().index(), material_index) {
                continue;
            }
            flatten_primitive_morph_deltas(&primitive, &buffers, &world_linear, &normal_mat, &mut per_target)?;
        }
        if per_target.is_empty() || per_target[0].is_empty() {
            return Err(format!(
                "{}: {} has no morph targets",
                path.display(),
                material_label(material_index)
            ));
        }
        let target_count = per_target.len() as u32;
        let vertex_count = per_target[0].len() as u32;
        let deltas: Vec<MeshVertex> = per_target.into_iter().flatten().collect();
        return Ok((deltas, target_count, vertex_count));
    }
    Err(format!(
        "{}: no mesh-owning node found contributing {}",
        path.display(),
        material_label(material_index)
    ))
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
    // BUG-221: same key convention as `per_material` — accumulates each
    // material's OWN world-space bbox (min, max) alongside the scene-wide
    // one above, so `gltf_import_summary` can compute a per-object center
    // distinct from the whole-scene center.
    per_material_bbox: &mut std::collections::BTreeMap<Option<usize>, ([f32; 3], [f32; 3])>,
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
        // BUG-205: a SKINNED mesh's rendered position ignores the mesh
        // node's own world transform (glTF 2.0 §3.7.3.3) — its static
        // (bind-pose) world positions are `skin_matrix[j] * v` blended by
        // the vertex weights, exactly what `node.skin_mesh` computes at
        // runtime. Summarizing it through `world` instead put the bbox in
        // a DIFFERENT space than the render (skeleton_animated.glb: bind
        // bbox y 0.36..2.22 vs skinned y -0.57..1.20), so the synthesized
        // camera framed and recentered a box the mesh never occupies.
        let bind_skin_matrices: Option<Vec<Mat4>> =
            node.skin().map(|skin| bind_pose_skin_matrices(&skin, document, buffers));
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
            let own_bbox = per_material_bbox
                .entry(prim.material().index())
                .or_insert(([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]));
            let joints: Option<Vec<[u16; 4]>> =
                reader.read_joints(0).map(|it| it.into_u16().collect());
            let weights: Option<Vec<[f32; 4]>> =
                reader.read_weights(0).map(|it| it.into_f32().collect());
            if let (Some(sm), Some(joints), Some(weights)) =
                (bind_skin_matrices.as_ref(), joints.as_ref(), weights.as_ref())
            {
                for (i, p) in positions.iter().enumerate() {
                    let mut wp = [0.0f32; 3];
                    let wsum: f32 = weights[i].iter().sum::<f32>().max(1e-8);
                    for k in 0..4 {
                        let j = joints[i][k] as usize;
                        let Some(m) = sm.get(j) else { continue };
                        let tp = mat4_transform_point(m, *p);
                        let w = weights[i][k] / wsum;
                        for c in 0..3 {
                            wp[c] += w * tp[c];
                        }
                    }
                    for c in 0..3 {
                        bbox_min[c] = bbox_min[c].min(wp[c]);
                        bbox_max[c] = bbox_max[c].max(wp[c]);
                        own_bbox.0[c] = own_bbox.0[c].min(wp[c]);
                        own_bbox.1[c] = own_bbox.1[c].max(wp[c]);
                    }
                }
                continue;
            }
            for instance_world in &instance_worlds {
                for p in &positions {
                    let wp = mat4_transform_point(instance_world, *p);
                    for i in 0..3 {
                        bbox_min[i] = bbox_min[i].min(wp[i]);
                        bbox_max[i] = bbox_max[i].max(wp[i]);
                        own_bbox.0[i] = own_bbox.0[i].min(wp[i]);
                        own_bbox.1[i] = own_bbox.1[i].max(wp[i]);
                    }
                }
            }
        }
    }

    for child in node.children() {
        summarize_node(&child, world, document, buffers, per_material, bbox_min, bbox_max, per_material_bbox)?;
    }
    Ok(())
}

/// BUG-221: center of a `per_material_bbox` entry, `[0,0,0]` when absent
/// (defensive only — every call site here first checks `vertex_count > 0`,
/// which is set by the exact same accumulation pass that fills this map, so
/// a lookup miss should never actually happen).
fn own_bbox_center(bbox: Option<&([f32; 3], [f32; 3])>) -> [f32; 3] {
    match bbox {
        Some((min, max)) => [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ],
        None => [0.0, 0.0, 0.0],
    }
}

/// BUG-205: per-joint bind-pose skin matrices (`static_world(joint) *
/// inverse_bind_matrix`) — maps a skinned primitive's mesh-local vertices
/// to their STATIC world positions, the space `node.skin_mesh` renders in.
/// Mirrors `parse_skins`' fallback: a skin with no `inverseBindMatrices`
/// accessor implies identity per joint.
fn bind_pose_skin_matrices(
    skin: &gltf::Skin,
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
) -> Vec<Mat4> {
    let parent_of = build_parent_map(document);
    let reader = skin.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));
    let ibms: Vec<Mat4> = match reader.read_inverse_bind_matrices() {
        Some(it) => it.collect(),
        None => vec![MAT4_IDENTITY; skin.joints().count()],
    };
    skin.joints()
        .enumerate()
        .map(|(k, joint)| {
            let world = static_world_matrix(document, joint.index(), &parent_of);
            mat4_mul(&world, ibms.get(k).unwrap_or(&MAT4_IDENTITY))
        })
        .collect()
}

/// Resolve `key`'s skin — `Some(material_index)` for a real material,
/// `None` for the synthetic default-material bucket (BUG-207) — the SAME
/// way for both: exactly one contributing node, and that node carries a
/// `skin()`.
fn resolve_skin_for_key(
    nodes_by_material: &std::collections::BTreeMap<Option<usize>, std::collections::BTreeSet<usize>>,
    document: &gltf::Document,
    skins: &[GltfSkinInfo],
    key: Option<usize>,
    label: &str,
    animation_report_lines: &mut Vec<String>,
) -> Option<GltfObjectSkin> {
    match nodes_by_material.get(&key) {
        Some(nodes) if nodes.len() == 1 => {
            let node_index = *nodes.iter().next().unwrap();
            document.nodes().nth(node_index).and_then(|node| {
                node.skin().map(|s| GltfObjectSkin {
                    skin_index: s.index() as u32,
                    info: skins[s.index()].clone(),
                })
            })
        }
        Some(nodes) if nodes.len() > 1 => {
            if nodes
                .iter()
                .any(|n| document.nodes().nth(*n).map(|node| node.skin().is_some()).unwrap_or(false))
            {
                animation_report_lines.push(format!(
                    "{label}: geometry contributed by {} nodes, at \
                     least one of which is skinned — multi-node-per-material skinning is \
                     out of A2 scope, this object is left unskinned",
                    nodes.len()
                ));
            }
            None
        }
        _ => None,
    }
}

/// Resolve `key`'s morph targets — same `Some`/`None` key convention as
/// [`resolve_skin_for_key`] (BUG-207). `target_count` comes from the FIRST
/// matching primitive with targets; see the call site's original comment
/// for the multi-primitive-inconsistent-target-count caveat (unchanged).
fn resolve_morph_for_key(
    nodes_by_material: &std::collections::BTreeMap<Option<usize>, std::collections::BTreeSet<usize>>,
    document: &gltf::Document,
    key: Option<usize>,
    label: &str,
    animation_report_lines: &mut Vec<String>,
) -> Option<GltfObjectMorph> {
    match nodes_by_material.get(&key) {
        Some(nodes) if nodes.len() == 1 => {
            let node_index = *nodes.iter().next().unwrap();
            document.nodes().nth(node_index).and_then(|node| {
                let mesh = node.mesh()?;
                let target_count = mesh
                    .primitives()
                    .find(|p| p.material().index() == key && p.morph_targets().len() > 0)
                    .map(|p| p.morph_targets().len())?;
                let mesh_weights = mesh.weights().unwrap_or(&[]);
                let static_weights: Vec<f32> = (0..target_count)
                    .map(|i| mesh_weights.get(i).copied().unwrap_or(0.0))
                    .collect();
                Some(GltfObjectMorph {
                    mesh_node_index: node_index,
                    target_count: target_count as u32,
                    static_weights,
                })
            })
        }
        Some(nodes) if nodes.len() > 1 => {
            if nodes.iter().any(|n| {
                document
                    .nodes()
                    .nth(*n)
                    .and_then(|node| node.mesh())
                    .map(|mesh| mesh.primitives().any(|p| p.morph_targets().len() > 0))
                    .unwrap_or(false)
            }) {
                animation_report_lines.push(format!(
                    "{label}: geometry contributed by {} nodes, at \
                     least one of which carries morph targets — multi-node-per-material \
                     morph targets are out of A3 scope, this object is left unmorphed",
                    nodes.len()
                ));
            }
            None
        }
        _ => None,
    }
}

/// Resolve `key`'s animation clips — same `Some`/`None` key convention as
/// [`resolve_skin_for_key`] (BUG-207): every clip resolved against the
/// contributing node's full ancestor chain, once per clip (A4/D4).
///
/// GLTF_ANIM_RUNTIME_V2_DESIGN.md D4 (P3): returns `(per_clip_animations,
/// rigid_multi_node)` — the second slot is `Some` (and the first always
/// empty) whenever this object needs the node-slot palette instead of the
/// single-node TRS-track path: geometry contributed by MORE THAN ONE
/// mesh-owning node with at least one animated in some clip and NONE
/// skinned (deletes the old `:2492` left-static bail), or exactly one
/// contributing node whose ancestor chain animates the same TRS channel on
/// more than one ancestor in any clip (deletes the old `:1700` per-channel
/// drop — see [`resolve_object_animation`]'s `ambiguous` doc).
#[allow(clippy::too_many_arguments)]
fn resolve_animations_for_key(
    nodes_by_material: &std::collections::BTreeMap<Option<usize>, std::collections::BTreeSet<usize>>,
    chain_by_node: &std::collections::BTreeMap<usize, Vec<usize>>,
    node_anims_by_clip: &[std::collections::BTreeMap<usize, GltfNodeAnimation>],
    any_clip_animated_nodes: &std::collections::BTreeSet<usize>,
    document: &gltf::Document,
    key: Option<usize>,
) -> (Vec<Option<GltfObjectAnimation>>, Option<GltfObjectRigidMultiNode>) {
    match nodes_by_material.get(&key) {
        Some(nodes) if nodes.len() == 1 => {
            let node_index = *nodes.iter().next().unwrap();
            let chain = chain_by_node.get(&node_index).cloned().unwrap_or_default();
            let mut ambiguous = false;
            let per_clip: Vec<Option<GltfObjectAnimation>> = node_anims_by_clip
                .iter()
                .map(|node_anims| resolve_object_animation(&chain, node_anims, &mut ambiguous))
                .collect();
            if ambiguous {
                (Vec::new(), Some(GltfObjectRigidMultiNode { slot_nodes: vec![node_index as u32] }))
            } else {
                (per_clip, None)
            }
        }
        Some(nodes) if nodes.len() > 1 && any_clip_animated_nodes.iter().any(|n| nodes.contains(n)) => {
            let any_skinned = nodes
                .iter()
                .any(|n| document.nodes().nth(*n).map(|node| node.skin().is_some()).unwrap_or(false));
            if any_skinned {
                // resolve_skin_for_key already reports this combination
                // ("multi-node-per-material skinning is out of A2 scope")
                // — a skinned node mixed into a multi-node material object
                // stays that bail, not D4's node-slot palette.
                (Vec::new(), None)
            } else {
                (Vec::new(), Some(GltfObjectRigidMultiNode { slot_nodes: nodes.iter().map(|&n| n as u32).collect() }))
            }
        }
        _ => (Vec::new(), None),
    }
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
    // BUG-221: per-material own world-space bbox, alongside the scene-wide
    // one above.
    let mut per_material_bbox: std::collections::BTreeMap<Option<usize>, ([f32; 3], [f32; 3])> =
        std::collections::BTreeMap::new();
    for node in &import_nodes {
        summarize_node(
            node,
            MAT4_IDENTITY,
            &document,
            &buffers,
            &mut per_material,
            &mut bbox_min,
            &mut bbox_max,
            &mut per_material_bbox,
        )?;
    }

    if !bbox_min[0].is_finite() {
        return Err(format!("{}: parsed no geometry", path.display()));
    }

    // GLTF_ANIMATION_DESIGN.md A1/A4: parse every animation clip, then
    // resolve EVERY clip (D4 multi-clip selection) against each material's
    // mesh-owning node's full ancestor chain — see `resolve_object_animation`
    // for why the chain (not just the leaf node) is required.
    let animations = parse_animations(&document, &buffers);
    // GLTF_ANIMATION_DESIGN.md A2: parsed once per skin index — resolved
    // against a material below only when that material's geometry comes
    // from exactly one skinned node (same boundary as `animation`).
    let skins = parse_skins(&document, &buffers);
    let mut animation_report_lines: Vec<String> = Vec::new();
    for anim in &animations {
        for line in &anim.skipped_channels {
            animation_report_lines.push(format!(
                "animation {:?}: {line}",
                anim.name.as_deref().unwrap_or("<unnamed>")
            ));
        }
    }
    let node_anims_by_clip: Vec<std::collections::BTreeMap<usize, GltfNodeAnimation>> = animations
        .iter()
        .map(|a| a.nodes.iter().map(|n| (n.node_index, n.clone())).collect())
        .collect();
    // Union of every clip's animated node set — used only for the
    // multi-node-per-material ambiguity check below (unchanged by A4: that
    // check doesn't need to know WHICH clip animates the ambiguous node).
    let any_clip_animated_nodes: std::collections::BTreeSet<usize> =
        node_anims_by_clip.iter().flat_map(|m| m.keys().copied()).collect();
    let mesh_node_chains = collect_mesh_node_chains(&document);
    let mut chain_by_node: std::collections::BTreeMap<usize, Vec<usize>> =
        std::collections::BTreeMap::new();
    let mut nodes_by_material: std::collections::BTreeMap<Option<usize>, std::collections::BTreeSet<usize>> =
        std::collections::BTreeMap::new();
    for (node_index, chain) in &mesh_node_chains {
        chain_by_node.insert(*node_index, chain.clone());
        if let Some(node) = document.nodes().nth(*node_index)
            && let Some(mesh) = node.mesh()
        {
            for primitive in mesh.primitives() {
                nodes_by_material
                    .entry(primitive.material().index())
                    .or_default()
                    .insert(*node_index);
            }
        }
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
            let own_center = own_bbox_center(per_material_bbox.get(&Some(material_index as usize)));
            // A1/A4: resolve this material's animation only when exactly
            // one mesh-owning node contributes its geometry — the
            // documented multi-node-per-material scope boundary (never
            // partially or ambiguously animated) — once PER CLIP (A4/D4).
            // Same key convention `resolve_animations_for_key` shares with
            // the default-material entry built below (BUG-207).
            let material_key = Some(material_index as usize);
            let label = material_label(material_index);
            let (animations, rigid_multi_node) = resolve_animations_for_key(
                &nodes_by_material,
                &chain_by_node,
                &node_anims_by_clip,
                &any_clip_animated_nodes,
                &document,
                material_key,
            );
            if let Some(rmn) = &rigid_multi_node {
                animation_report_lines.push(format!(
                    "{label}: rigid animation composed across {} nodes via the node-slot \
                     palette (GLTF_ANIM_RUNTIME_V2_DESIGN.md D4)",
                    rmn.slot_nodes.len()
                ));
            }
            // A2 (D2): resolve this material's skin the same way — exactly
            // one contributing node, and that node carries a `skin()`.
            let skin = resolve_skin_for_key(
                &nodes_by_material,
                &document,
                &skins,
                material_key,
                &label,
                &mut animation_report_lines,
            );
            // A3: resolve this material's morph targets the same way —
            // exactly one contributing node, and that node's mesh carries
            // `targets` on the primitive(s) contributing this material.
            // `target_count` comes from the FIRST matching primitive with
            // targets; a mesh with more than one primitive per material
            // and an inconsistent target count across them is not
            // exercised by AnimatedMorphCube/MorphStressTest/
            // MorphPrimitivesTest (re-derive if a future asset needs it).
            let morph = resolve_morph_for_key(
                &nodes_by_material,
                &document,
                material_key,
                &label,
                &mut animation_report_lines,
            );
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
            // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6: raw-JSON texture-index
            // helper, hoisted above its original sheen/iridescence/
            // anisotropy use so clearcoat/specular (parsed below, before
            // that block) can share it too.
            let tex_idx = |v: &serde_json::Value, key: &str| -> Option<u32> {
                v.get(key)?.get("index")?.as_u64().map(|i| i as u32)
            };
            let specular_ext = m.specular();
            let specular_factor = specular_factor_override
                .unwrap_or_else(|| specular_ext.as_ref().map(|s| s.specular_factor()).unwrap_or(1.0));
            let specular_color_factor = specular_ext
                .as_ref()
                .map(|s| s.specular_color_factor())
                .unwrap_or([1.0, 1.0, 1.0]);
            // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6 (D1 revised — full spec
            // surface): specularTexture/specularColorTexture, typed
            // accessors on `Specular<'_>` (gltf 1.4.1 has the feature).
            let specular_texture =
                specular_ext.as_ref().and_then(|s| s.specular_texture()).map(|t| t.texture().index() as u32);
            let specular_color_texture = specular_ext
                .as_ref()
                .and_then(|s| s.specular_color_texture())
                .map(|t| t.texture().index() as u32);

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
            // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6 (D1 revised — full spec
            // surface): clearcoatTexture/clearcoatRoughnessTexture/
            // clearcoatNormalTexture, raw-JSON sniff (same `tex_idx`
            // doctrine sheen/iridescence/anisotropy already use — no typed
            // accessor exists for this extension at 1.4.1).
            let clearcoat_texture = clearcoat_ext.and_then(|v| tex_idx(v, "clearcoatTexture"));
            let clearcoat_roughness_texture =
                clearcoat_ext.and_then(|v| tex_idx(v, "clearcoatRoughnessTexture"));
            let clearcoat_normal_texture =
                clearcoat_ext.and_then(|v| tex_idx(v, "clearcoatNormalTexture"));

            // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E1: KHR_materials_sheen.
            // Same raw-JSON-sniff doctrine as clearcoat above — no typed
            // accessor at 1.4.1. glTF's own implicit defaults ([0,0,0] /
            // 0.0) make an absent extension byte-identical to pre-E1.
            let sheen_ext = m.extension_value("KHR_materials_sheen");
            let f3 = |v: &serde_json::Value, key: &str, default: [f32; 3]| -> [f32; 3] {
                v.get(key)
                    .and_then(|a| a.as_array())
                    .and_then(|a| {
                        Some([
                            a.first()?.as_f64()? as f32,
                            a.get(1)?.as_f64()? as f32,
                            a.get(2)?.as_f64()? as f32,
                        ])
                    })
                    .unwrap_or(default)
            };
            let f1 = |v: &serde_json::Value, key: &str, default: f32| -> f32 {
                v.get(key).and_then(|x| x.as_f64()).map(|x| x as f32).unwrap_or(default)
            };
            let sheen_color_factor = sheen_ext
                .map(|v| f3(v, "sheenColorFactor", [0.0, 0.0, 0.0]))
                .unwrap_or([0.0, 0.0, 0.0]);
            let sheen_roughness_factor =
                sheen_ext.map(|v| f1(v, "sheenRoughnessFactor", 0.0)).unwrap_or(0.0);
            let sheen_color_texture = sheen_ext.and_then(|v| tex_idx(v, "sheenColorTexture"));
            let sheen_roughness_texture =
                sheen_ext.and_then(|v| tex_idx(v, "sheenRoughnessTexture"));

            // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E1: KHR_materials_iridescence.
            // Raw-JSON sniff — no typed accessor at 1.4.1.
            let iridescence_ext = m.extension_value("KHR_materials_iridescence");
            let iridescence_factor =
                iridescence_ext.map(|v| f1(v, "iridescenceFactor", 0.0)).unwrap_or(0.0);
            let iridescence_ior =
                iridescence_ext.map(|v| f1(v, "iridescenceIor", 1.3)).unwrap_or(1.3);
            let iridescence_thickness_minimum = iridescence_ext
                .map(|v| f1(v, "iridescenceThicknessMinimum", 100.0))
                .unwrap_or(100.0);
            let iridescence_thickness_maximum = iridescence_ext
                .map(|v| f1(v, "iridescenceThicknessMaximum", 400.0))
                .unwrap_or(400.0);
            let iridescence_texture =
                iridescence_ext.and_then(|v| tex_idx(v, "iridescenceTexture"));
            let iridescence_thickness_texture =
                iridescence_ext.and_then(|v| tex_idx(v, "iridescenceThicknessTexture"));

            // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E1: KHR_materials_anisotropy.
            // Raw-JSON sniff — no typed accessor at 1.4.1.
            let anisotropy_ext = m.extension_value("KHR_materials_anisotropy");
            let anisotropy_strength =
                anisotropy_ext.map(|v| f1(v, "anisotropyStrength", 0.0)).unwrap_or(0.0);
            let anisotropy_rotation =
                anisotropy_ext.map(|v| f1(v, "anisotropyRotation", 0.0)).unwrap_or(0.0);
            let anisotropy_texture = anisotropy_ext.and_then(|v| tex_idx(v, "anisotropyTexture"));

            // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E1: KHR_materials_dispersion.
            // Raw-JSON sniff — no typed accessor at 1.4.1. Single-field
            // extension, no texture defined by the spec.
            let dispersion = m
                .extension_value("KHR_materials_dispersion")
                .map(|v| f1(v, "dispersion", 0.0))
                .unwrap_or(0.0);

            // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E1: KHR_materials_volume.
            // Typed accessor (`Material::volume()`) — the Cargo feature IS
            // enabled at 1.4.1, unlike the four raw-JSON families above.
            // `attenuation_distance()`'s crate-level default is
            // `f32::INFINITY` (glTF spec's own implicit default meaning "no
            // attenuation") — NOT read straight through: `f32::INFINITY`
            // is not representable in MANIFOLD's JSON project format
            // (`serde_json` errors serializing a non-finite float, and
            // EVERY glTF import without an explicit attenuationDistance —
            // i.e. almost all of them — would hit this on save) nor is it
            // a value later Beer-Lambert shading math (E2) can safely
            // divide distance by. `VOLUME_ATTENUATION_DISTANCE_NO_ATTENUATION`
            // is the finite stand-in — see its doc comment.
            let volume_ext = m.volume();
            let volume_thickness_factor =
                volume_ext.as_ref().map(|v| v.thickness_factor()).unwrap_or(0.0);
            let volume_attenuation_distance = volume_ext
                .as_ref()
                .map(|v| v.attenuation_distance())
                .filter(|d| d.is_finite())
                .unwrap_or(VOLUME_ATTENUATION_DISTANCE_NO_ATTENUATION);
            let volume_attenuation_color = volume_ext
                .as_ref()
                .map(|v| v.attenuation_color())
                .unwrap_or([1.0, 1.0, 1.0]);
            // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6 (D1 revised — full spec
            // surface): thicknessTexture, typed accessor (Volume has crate
            // support at 1.4.1, same as thickness_factor above).
            let volume_thickness_texture = volume_ext
                .as_ref()
                .and_then(|v| v.thickness_texture())
                .map(|t| t.texture().index() as u32);

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
                specular_texture,
                specular_color_texture,
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
                transmission_texture: m
                    .transmission()
                    .and_then(|t| t.transmission_texture())
                    .map(|t| t.texture().index() as u32),
                clearcoat_factor,
                clearcoat_roughness_factor,
                clearcoat_texture,
                clearcoat_roughness_texture,
                clearcoat_normal_texture,
                sheen_color_factor,
                sheen_roughness_factor,
                sheen_color_texture,
                sheen_roughness_texture,
                iridescence_factor,
                iridescence_ior,
                iridescence_thickness_minimum,
                iridescence_thickness_maximum,
                iridescence_texture,
                iridescence_thickness_texture,
                anisotropy_strength,
                anisotropy_rotation,
                anisotropy_texture,
                dispersion,
                volume_thickness_factor,
                volume_attenuation_distance,
                volume_attenuation_color,
                volume_thickness_texture,
                was_blend,
                mr_texture_is_gloss_alpha,
                vertex_count,
                base_color_sampler,
                normal_sampler,
                mr_sampler,
                occlusion_sampler,
                emissive_sampler,
                animations,
                skin,
                morph,
                rigid_multi_node,
                own_center,
            })
        })
        .collect();

    let default_material_vertex_count = per_material.get(&None).copied().unwrap_or(0);
    let default_own_center = own_bbox_center(per_material_bbox.get(&None));
    let mut materials = materials;
    if default_material_vertex_count > 0 {
        // GLB_XFAIL_BURNDOWN_DESIGN.md D4 (BUG-171): geometry with no
        // material assigned gets the glTF spec's implicit default material
        // (base color [1,1,1,1], metallic 1.0, roughness 1.0 — glTF spec
        // §3.9.2) instead of being silently dropped. Sentinel
        // `material_index = DEFAULT_MATERIAL_SENTINEL` (u32::MAX) marks
        // this entry as synthetic — `gltf_import.rs` must never re-query
        // material index u32::MAX in the document for it.
        //
        // BUG-207: the default-material bucket (`nodes_by_material`'s
        // `None` key) is a first-class key here too — a materialless
        // skinned/morphed/animated node resolves exactly like a real
        // material's `Some(idx)` bucket, via the SAME shared functions.
        let default_label = material_label(DEFAULT_MATERIAL_SENTINEL);
        let default_skin = resolve_skin_for_key(
            &nodes_by_material,
            &document,
            &skins,
            None,
            &default_label,
            &mut animation_report_lines,
        );
        let default_morph = resolve_morph_for_key(
            &nodes_by_material,
            &document,
            None,
            &default_label,
            &mut animation_report_lines,
        );
        let (default_animations, default_rigid_multi_node) = resolve_animations_for_key(
            &nodes_by_material,
            &chain_by_node,
            &node_anims_by_clip,
            &any_clip_animated_nodes,
            &document,
            None,
        );
        if let Some(rmn) = &default_rigid_multi_node {
            animation_report_lines.push(format!(
                "{default_label}: rigid animation composed across {} nodes via the \
                 node-slot palette (GLTF_ANIM_RUNTIME_V2_DESIGN.md D4)",
                rmn.slot_nodes.len()
            ));
        }
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
            transmission_texture: None,
            clearcoat_factor: 0.0,
            clearcoat_roughness_factor: 0.0,
            clearcoat_texture: None,
            clearcoat_roughness_texture: None,
            clearcoat_normal_texture: None,
            sheen_color_factor: [0.0, 0.0, 0.0],
            sheen_roughness_factor: 0.0,
            sheen_color_texture: None,
            sheen_roughness_texture: None,
            iridescence_factor: 0.0,
            iridescence_ior: 1.3,
            iridescence_thickness_minimum: 100.0,
            iridescence_thickness_maximum: 400.0,
            iridescence_texture: None,
            iridescence_thickness_texture: None,
            anisotropy_strength: 0.0,
            anisotropy_rotation: 0.0,
            anisotropy_texture: None,
            dispersion: 0.0,
            volume_thickness_factor: 0.0,
            volume_attenuation_distance: VOLUME_ATTENUATION_DISTANCE_NO_ATTENUATION,
            volume_attenuation_color: [1.0, 1.0, 1.0],
            volume_thickness_texture: None,
            was_blend: false,
            ior: 1.5,
            specular_factor: 1.0,
            specular_color_factor: [1.0, 1.0, 1.0],
            specular_texture: None,
            specular_color_texture: None,
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
            animations: default_animations,
            skin: default_skin,
            morph: default_morph,
            rigid_multi_node: default_rigid_multi_node,
            own_center: default_own_center,
        });
    }

    // BUG-213: surface optional extensions we don't implement — required
    // ones already hard-failed in `parse_document_and_buffers`.
    let extension_report_lines = unsupported_optional_extension_lines(&document);

    Ok(GltfImportSummary {
        materials,
        bbox_min,
        bbox_max,
        camera_count: document.cameras().count(),
        default_material_vertex_count,
        animations,
        animation_report_lines,
        extension_report_lines,
    })
}

/// BUG-213: report lines for `extensionsUsed` entries MANIFOLD doesn't
/// implement (`extensionsRequired` unsupported entries never reach here —
/// they hard-fail in [`parse_document_and_buffers`]). One line per
/// extension, import proceeds without it.
fn unsupported_optional_extension_lines(document: &gltf::Document) -> Vec<String> {
    document
        .extensions_used()
        .filter(|ext| !MANIFOLD_SUPPORTED_EXTENSIONS.contains(ext))
        .map(|ext| {
            format!(
                "extensionsUsed[..] = \"{ext}\": optional extension not implemented by MANIFOLD — imported without it (report-only)"
            )
        })
        .collect()
}

#[cfg(test)]
mod animation_tests {
    use super::*;

    fn khronos_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos")
    }

    fn hostile_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf/hostile")
    }

    /// BUG-214: `EXT_mesh_gpu_instancing` is fully implemented (raw-JSON
    /// sniff in [`node_instance_transforms`]) but was missing from the
    /// supported allowlist, so an asset listing it under
    /// `extensionsRequired` would hard-fail at the gate despite our
    /// support. Pin the membership so the allowlist can't drift from the
    /// implementation again.
    #[test]
    fn ext_mesh_gpu_instancing_is_in_supported_allowlist() {
        assert!(
            MANIFOLD_SUPPORTED_EXTENSIONS.contains(&"EXT_mesh_gpu_instancing"),
            "EXT_mesh_gpu_instancing is implemented — it must be in MANIFOLD_SUPPORTED_EXTENSIONS"
        );
    }

    /// BUG-213: an `extensionsUsed` entry MANIFOLD doesn't implement (and
    /// that isn't `extensionsRequired`) must surface as a report line, and
    /// supported extensions must NOT. No shipped fixture carries an
    /// optional-unsupported extension, so the document is built in memory.
    #[test]
    fn unsupported_optional_extension_produces_report_line() {
        let json = br#"{
            "asset": { "version": "2.0" },
            "extensionsUsed": ["KHR_materials_unlit", "KHR_fake_extension_xyz"],
            "scenes": [ { "nodes": [] } ],
            "scene": 0
        }"#;
        let gltf = gltf::Gltf::from_slice_without_validation(json).expect("parse minimal gltf");
        let lines = unsupported_optional_extension_lines(&gltf.document);
        assert_eq!(lines.len(), 1, "only the unsupported extension reports: {lines:?}");
        assert!(lines[0].contains("KHR_fake_extension_xyz"), "line names the extension: {}", lines[0]);
    }

    /// IMPORT_ANYTHING_WAVE_DESIGN.md W2 (BUG-187's STEP half): before this
    /// lane, `step_interp.glb`'s translation channel was dropped with a
    /// report line (`gltf_load.rs`'s old LINEAR-only gate). Asserts it now
    /// parses into a real track carrying `GltfInterp::Step`, never
    /// approximated as LINEAR.
    #[test]
    fn step_interp_fixture_parses_as_step_not_dropped() {        let path = hostile_dir().join("step_interp.glb");
        assert!(path.exists(), "step_interp.glb fixture missing at {}", path.display());
        let (document, buffers, _images) = import_glb(&path).expect("parse step_interp.glb");
        let animations = parse_animations(&document, &buffers);
        assert_eq!(animations.len(), 1);
        assert!(animations[0].skipped_channels.is_empty(), "STEP must not be skipped");
        let node = &animations[0].nodes[0];
        let t = node.translation.as_ref().expect("translation track");
        assert_eq!(t.mode, GltfInterp::Step);
        assert_eq!(t.times, vec![0.0, 0.5, 1.0, 1.5]);
    }

    /// Same as [`step_interp_fixture_parses_as_step_not_dropped`] for
    /// `cubicspline_interp.glb` — also asserts the in/out tangents
    /// de-interleaved correctly (all-zero in this fixture) and the
    /// keyframe VALUES survived the de-interleave untouched.
    #[test]
    fn cubicspline_interp_fixture_parses_with_tangents_deinterleaved() {
        let path = hostile_dir().join("cubicspline_interp.glb");
        assert!(path.exists(), "cubicspline_interp.glb fixture missing at {}", path.display());
        let (document, buffers, _images) =
            import_glb(&path).expect("parse cubicspline_interp.glb");
        let animations = parse_animations(&document, &buffers);
        assert_eq!(animations.len(), 1);
        assert!(animations[0].skipped_channels.is_empty(), "CUBICSPLINE must not be skipped");
        let node = &animations[0].nodes[0];
        let t = node.translation.as_ref().expect("translation track");
        assert_eq!(t.mode, GltfInterp::CubicSpline);
        assert_eq!(t.times, vec![0.0, 0.5, 1.0, 1.5]);
        assert_eq!(
            t.values,
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 0.0, 0.0]],
            "de-interleaving must not disturb the value triple's own numbers"
        );
        assert_eq!(t.in_tangents.len(), 4);
        assert_eq!(t.out_tangents.len(), 4);
        for tangent in t.in_tangents.iter().chain(t.out_tangents.iter()) {
            assert_eq!(*tangent, [0.0, 0.0, 0.0], "this fixture's tangents are authored all-zero");
        }
    }

    /// GLTF_ANIMATION_DESIGN.md A1 gate fixture. Re-derived inventory
    /// finding (this session): `BoxAnimated.glb` is NOT the "one node,
    /// one mesh, one material" asset the phase brief assumed — it has
    /// TWO materials ("inner"/"outer") and its single animated object
    /// splits translation onto an ANCESTOR node (node zero) from rotation
    /// on the mesh's own node (node two), via an intermediate no-op node.
    /// `resolve_object_animation`'s ancestor-chain walk exists
    /// specifically because of this asset.
    #[test]
    fn box_animated_resolves_split_translation_and_rotation_onto_one_object() {
        let path = khronos_dir().join("BoxAnimated.glb");
        if !path.exists() {
            println!(
                "box_animated_resolves_split_translation_and_rotation_onto_one_object: \
                 fixture not found at {}, skipping",
                path.display()
            );
            return;
        }
        let summary = gltf_import_summary(&path).expect("parse BoxAnimated.glb");
        assert_eq!(summary.materials.len(), 2, "inner + outer");
        assert_eq!(summary.animations.len(), 1, "one animation clip");
        assert!(
            summary.animations[0].skipped_channels.is_empty(),
            "BoxAnimated's two channels are both LINEAR — nothing should be skipped: {:?}",
            summary.animations[0].skipped_channels
        );

        let inner = summary
            .materials
            .iter()
            .find(|m| m.name.as_deref() == Some("inner"))
            .expect("inner material present");
        let anim = inner
            .animations
            .first()
            .and_then(|a| a.as_ref())
            .expect("inner material resolves an animation in clip 0");
        assert!(anim.translation.is_some(), "translation lives on inner's ancestor node");
        assert!(anim.rotation.is_some(), "rotation lives on inner's own mesh node");
        assert!(anim.scale.is_none(), "BoxAnimated has no scale channel");
        assert!(
            (anim.duration_s - 3.708_33).abs() < 1e-3,
            "duration should be the translation track's last keyframe time, got {}",
            anim.duration_s
        );

        let outer = summary
            .materials
            .iter()
            .find(|m| m.name.as_deref() == Some("outer"))
            .expect("outer material present");
        assert!(
            outer.animations.iter().all(|a| a.is_none()),
            "outer_box is static in this fixture"
        );
    }

    /// Held-out parse-only smoke test (A1 deliverable 1): proves the
    /// parser isn't shaped around `BoxAnimated` alone. `InterpolationTest.glb`
    /// substitutes for the brief's originally-named
    /// `AnimatedColorsCube.glb`: that asset uses `KHR_animation_pointer`
    /// channels which OMIT `target.node` per that extension's own spec,
    /// and the pinned `gltf-json` 1.4.1's `Target` struct has no
    /// `#[serde(default)]` on its `node` field — so the file fails to
    /// even DESERIALIZE (`missing field \`node\``), before any
    /// validation or animation parsing runs. Logged as BUG-170 (see
    /// docs/BUG_BACKLOG.md) — a pre-existing crate-parsing gap, not a
    /// regression from this phase. `InterpolationTest.glb` instead
    /// exercises translation/rotation/scale channels across MULTIPLE
    /// nodes and BOTH the supported (LINEAR) and unsupported (STEP/
    /// CUBICSPLINE) interpolation modes in one asset — a strictly
    /// broader generality proof than the brief's original ask.
    #[test]
    fn interpolation_test_parses_linear_channels_and_reports_unsupported_ones() {
        let path = khronos_dir().join("InterpolationTest.glb");
        if !path.exists() {
            println!(
                "interpolation_test_parses_linear_channels_and_reports_unsupported_ones: \
                 fixture not found at {}, skipping",
                path.display()
            );
            return;
        }
        let (document, buffers, _images) = import_glb(&path).expect("parse InterpolationTest.glb");
        let animations = parse_animations(&document, &buffers);
        assert_eq!(animations.len(), 9, "one animation clip per channel in this fixture");

        // IMPORT_ANYTHING_WAVE_DESIGN.md W2: all 9 clips now parse (3
        // LINEAR + 3 STEP + 3 CUBICSPLINE, per the fixture's own naming —
        // "Step Scale"/"Linear Scale"/"CubicSpline Scale" etc.) — nothing
        // skipped, no TRS channel silently dropped or approximated.
        let tracks: usize = animations.iter().map(|a| a.nodes.len()).sum();
        let skipped: usize = animations.iter().map(|a| a.skipped_channels.len()).sum();
        assert_eq!(tracks, 9, "every clip's Scale/Rotation/Translation channel must parse");
        assert_eq!(skipped, 0, "STEP and CUBICSPLINE are supported now — nothing to report");

        let mode_of = |name: &str| -> GltfInterp {
            let anim = animations.iter().find(|a| a.name.as_deref() == Some(name)).unwrap();
            let node = &anim.nodes[0];
            node.translation
                .as_ref()
                .map(|t| t.mode)
                .or_else(|| node.rotation.as_ref().map(|r| r.mode))
                .or_else(|| node.scale.as_ref().map(|s| s.mode))
                .unwrap()
        };
        assert_eq!(mode_of("Step Scale"), GltfInterp::Step);
        assert_eq!(mode_of("Linear Scale"), GltfInterp::Linear);
        assert_eq!(mode_of("CubicSpline Scale"), GltfInterp::CubicSpline);
        assert_eq!(mode_of("CubicSpline Rotation"), GltfInterp::CubicSpline);
    }
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

    /// IMPORT_ANYTHING_WAVE_DESIGN.md W1 (BUG-186): a cube whose only
    /// texture is `EXT_texture_webp` (no fallback source, `extensionsRequired`).
    /// Asserts the decode succeeds and the pixels are a real gradient, not
    /// a flat fallback color.
    #[test]
    fn loads_webp_texture_fixture() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/hostile/webp_texture.glb");
        assert!(path.exists(), "webp_texture.glb fixture missing at {}", path.display());
        let (w, h, rgba) =
            load_gltf_texture(&path, 0).unwrap_or_else(|e| panic!("webp texture decode: {e}"));
        assert!(w > 0 && h > 0, "expected non-zero texture dimensions");
        assert_eq!(rgba.len(), (w * h * 4) as usize, "expected tightly-packed RGBA8 buffer");

        let mut min = [255u8; 3];
        let mut max = [0u8; 3];
        for px in rgba.chunks_exact(4) {
            for c in 0..3 {
                min[c] = min[c].min(px[c]);
                max[c] = max[c].max(px[c]);
            }
        }
        assert!(
            max[0] > min[0] + 32 || max[1] > min[1] + 32,
            "expected red-green gradient variance in decoded webp texture, got min={min:?} max={max:?}"
        );
    }

    /// BUG-186: `SheenWoodLeatherSofa.glb` also carries `EXT_texture_webp`
    /// (Khronos conformance suite's real-world webp case). Once W1's decode
    /// path lands, check whether it now imports — this test's own result is
    /// the source of truth for whether the manifest promotes it to
    /// `expect_pass`, not a guess.
    #[test]
    fn sheenwoodleathersofa_webp_import_status() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/SheenWoodLeatherSofa.glb");
        if !path.exists() {
            println!("sheenwoodleathersofa_webp_import_status: fixture not fetched, skipping");
            return;
        }
        match gltf_import_summary(&path) {
            Ok(summary) => println!(
                "sheenwoodleathersofa_webp_import_status: imports OK, {} materials, bbox {:?}..{:?}",
                summary.materials.len(),
                summary.bbox_min,
                summary.bbox_max,
            ),
            Err(e) => println!("sheenwoodleathersofa_webp_import_status: import failed: {e}"),
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



