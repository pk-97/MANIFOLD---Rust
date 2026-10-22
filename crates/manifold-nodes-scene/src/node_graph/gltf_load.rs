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

use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::scene::material::{MapSamplerDesc, MaterialMapInfo};

/// glTF extensions MANIFOLD's importer actually supports, independent of
/// what the pinned `gltf` 1.4.1 crate's own feature-flag set types —
/// GLB_XFAIL_BURNDOWN_DESIGN.md D1. Everything in `Cargo.toml`'s `gltf`
/// feature list (which now includes `KHR_materials_unlit`'s typed
/// `Material::unlit()` accessor — BUG-w5wv: this extension was declared
/// supported here for a long time but never actually read anywhere, so
/// unlit-authored assets got full PBR lighting; this module now reads the
/// flag onto [`GltfMaterialInfo::unlit`] genuinely, unremapped, and
/// `gltf_import/object_group.rs` routes an unlit material to
/// `node.unlit_material` instead of `node.pbr_material`), plus extensions
/// this codebase maps downstream that the crate has no typed accessor for
/// at this version:
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
    // BUG-w5wv: now genuinely consumed — see `Material::unlit()` in the
    // per-material parse below, routed downstream to `node.unlit_material`.
    "KHR_materials_unlit",
    "KHR_materials_pbrSpecularGlossiness",
    "KHR_materials_clearcoat",
    // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E1 — raw-JSON sniffed, no typed
    // accessor at 1.4.1:
    "KHR_materials_sheen",
    "KHR_materials_iridescence",
    "KHR_materials_anisotropy",
    "KHR_materials_dispersion",
    // RAYTRACING_DESIGN.md section 16 TL3 — raw-JSON sniffed, no typed
    // accessor at 1.4.1:
    "KHR_materials_diffuse_transmission",
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
    parse_document_and_buffers_from_slice(path, &bytes)
}

/// The exact document and resolved buffers used by a mesh decode. The
/// identity includes contents, not the location of the owning model file.
pub(crate) struct GltfBufferSnapshot {
    pub document: gltf::Document,
    pub buffers: Vec<gltf::buffer::Data>,
    pub identity: [u8; 32],
}

/// Resolve dependencies through the canonical importer before consulting the
/// decoded mesh cache. A missing or changed external buffer must not be hidden
/// by a cache hit. On a miss the caller flattens these same bytes, without
/// reopening the model or its buffers under the captured identity.
pub(crate) fn parse_buffer_snapshot(
    path: &std::path::Path,
) -> Result<GltfBufferSnapshot, String> {
    use sha2::{Digest, Sha256};

    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (document, buffers) = parse_document_and_buffers_from_slice(path, &bytes)?;
    let mut hash = Sha256::new();
    hash.update(b"manifold.gltf-buffer-snapshot.v1");
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(&bytes);
    hash.update((buffers.len() as u64).to_le_bytes());
    for buffer in &buffers {
        hash.update((buffer.0.len() as u64).to_le_bytes());
        hash.update(&buffer.0);
    }
    Ok(GltfBufferSnapshot {
        document,
        buffers,
        identity: hash.finalize().into(),
    })
}

/// The pinned glTF importer unwraps UTF-8 after percent-decoding relative
/// URIs. Validate that one fallible step before calling it, including release
/// builds with panic=abort. Scheme-bearing URIs are not percent-decoded by
/// glTF; invalid/incomplete percent escapes and literal `+` remain unchanged.
fn validate_import_uri(uri: &str) -> Result<(), String> {
    if uri.contains(':') || !uri.contains('%') {
        return Ok(());
    }
    urlencoding::decode(uri)
        .map(|_| ())
        .map_err(|error| format!("invalid resource URI {uri:?}: {error}"))
}

fn parse_document_and_buffers_from_slice(
    path: &std::path::Path,
    bytes: &[u8],
) -> Result<(gltf::Document, Vec<gltf::buffer::Data>), String> {
    let gltf::Gltf { document, blob } = gltf::Gltf::from_slice_without_validation(bytes)
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
    for buffer in document.buffers() {
        if let gltf::buffer::Source::Uri(uri) = buffer.source() {
            validate_import_uri(uri)
                .map_err(|error| format!("{}: buffer {}: {error}", path.display(), buffer.index()))?;
        }
    }
    let buffers = gltf::import_buffers(&document, Some(base), blob)
        .map_err(|e| format!("{}: buffer import failed: {e}", path.display()))?;

    Ok((document, buffers))
}

/// `(document, buffers, images, image_report_lines)` — `image_report_lines`
/// is BUG-ssgz's per-image decode-failure report (KTX2/BasisU and any other
/// unsupported source), one line per substituted dummy texture.
pub(crate) type GlbImport =
    (gltf::Document, Vec<gltf::buffer::Data>, Vec<gltf::image::Data>, Vec<String>);

pub(crate) fn import_glb(path: &std::path::Path) -> Result<GlbImport, String> {
    let (document, buffers) = parse_document_and_buffers(path)?;
    let base = path.parent().unwrap_or_else(|| std::path::Path::new("./"));
    let (images, image_report_lines) = import_images_with_webp(&document, base, &buffers);

    Ok((document, buffers, images, image_report_lines))
}

/// `gltf::import_images` hard-fails the ENTIRE document the moment any one
/// image has an unrecognized mime type — its bundled `image` dependency
/// only compiles in `png`/`jpeg` decoders (see the crate's own `Cargo.toml`),
/// so a WebP-textured asset never reaches MANIFOLD's own extension gate at
/// all. IMPORT_ANYTHING_WAVE_DESIGN.md W1: decode webp images ourselves
/// (our `image` dependency has the `webp` feature on) and fall back to the
/// crate's own per-image decode for everything else.
///
/// BUG-ssgz: a per-image decode failure (KTX2/BasisU and any other
/// unsupported source) no longer aborts the whole import — it substitutes a
/// dummy texture and returns a report line instead of erroring. Never
/// returns `Err`; the return type stays a plain tuple rather than `Result`
/// to make that a compile-time fact at the call site.
fn import_images_with_webp(
    document: &gltf::Document,
    base: &std::path::Path,
    buffers: &[gltf::buffer::Data],
) -> (Vec<gltf::image::Data>, Vec<String>) {
    let mut out = Vec::with_capacity(document.images().count());
    let mut report_lines = Vec::new();
    for image in document.images() {
        let mime_type = match image.source() {
            gltf::image::Source::View { mime_type, .. } => Some(mime_type),
            gltf::image::Source::Uri { mime_type, .. } => mime_type,
        };
        #[cfg(test)]
        TEXTURE_DECODE_COUNT.with(|count| count.set(count.get() + 1));
        let valid_uri = match image.source() {
            gltf::image::Source::Uri { uri, .. } => validate_import_uri(uri),
            gltf::image::Source::View { .. } => Ok(()),
        };
        let decoded = valid_uri.and_then(|()| {
            if mime_type == Some("image/webp") {
                decode_webp_image(&image, buffers)
            } else {
                gltf::image::Data::from_source(image.source(), Some(base), buffers)
                    .map_err(|e| e.to_string())
            }
        });
        match decoded {
            Ok(data) => out.push(data),
            Err(e) => {
                if mime_type == Some("image/ktx2") {
                    report_lines.push(format!(
                        "image {}: KTX2/BasisU compressed texture is not supported — \
                         dummy texture substituted (no transcoder)",
                        image.index()
                    ));
                } else {
                    report_lines.push(format!(
                        "image {} (\"{}\"): decode failed ({e}) — dummy texture substituted",
                        image.index(),
                        image_label(&image)
                    ));
                }
                out.push(dummy_image_data());
            }
        }
    }
    (out, report_lines)
}

/// Best-effort human label for an image-decode-failure report line: the
/// glTF `name`, else the external URI, else `<unnamed>` for a
/// bufferView-embedded image with neither.
fn image_label(image: &gltf::image::Image) -> String {
    if let Some(name) = image.name() {
        return name.to_string();
    }
    match image.source() {
        gltf::image::Source::Uri { uri, .. } => uri.to_string(),
        gltf::image::Source::View { .. } => "<unnamed>".to_string(),
    }
}

/// Placeholder substituted for any image MANIFOLD can't decode (BUG-ssgz):
/// 2x2 magenta RGBA8 — loud enough to spot on a mesh, cheap enough to not
/// matter. No existing 1x1/checker CPU-side `gltf::image::Data` helper to
/// reuse (the `1×1 dummy` precedent in `render_instanced_3d_mesh.rs` and
/// `standalone.rs` is a GPU-side `manifold_gpu::GpuTexture`, a different
/// layer entirely).
pub(crate) fn dummy_image_data() -> gltf::image::Data {
    const MAGENTA: [u8; 4] = [255, 0, 255, 255];
    gltf::image::Data {
        pixels: MAGENTA.repeat(4),
        format: gltf::image::Format::R8G8B8A8,
        width: 2,
        height: 2,
    }
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
            return Err("external-URI webp images are not supported".to_string());
        }
    };
    let buf = &buffers[view.buffer().index()].0;
    let begin = view.offset();
    let end = begin + view.length();
    let encoded = &buf[begin..end];

    let decoded = image::load_from_memory_with_format(encoded, image::ImageFormat::WebP)
        .map_err(|e| format!("webp decode failed: {e}"))?
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

// Data shared by catalog proofs is public only in testkit builds. Keeping
// one declaration per type prevents the production and proof layouts drifting.

manifold_core::testkit_visible! {
/// BUG-5uqg: `KHR_lights_punctual` light subcategory, mirroring
/// `gltf::khr_lights_punctual::Kind` but without its borrow (so it can be
/// carried on [`GltfPunctualLight`] past the parsed `Document`'s lifetime).
/// `gltf_import::scene` maps `Directional` to `node.light`'s Sun mode and
/// both `Point` and `Spot` to its Point mode — `node.light` has no cone
/// attenuation, so a Spot import is a lossy (but visually-closer-than-omitting)
/// approximation; see that module's conversion doc for the reasoning.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum GltfLightKind {
    Directional,
    Point,
    Spot {
        /// Radians from the cone centre where falloff begins.
        inner_cone_angle: f32,
        /// Radians from the cone centre where falloff ends.
        outer_cone_angle: f32,
    },
}
}

manifold_core::testkit_visible! {
/// One `KHR_lights_punctual` light, resolved to its owning node's world
/// transform. `color`/`intensity`/`range` are the RAW spec values (linear
/// RGB; lux for `Directional`, candela for `Point`/`Spot`; range in metres,
/// `None` = spec's unbounded default) — `gltf_import::scene` converts
/// `intensity` into `node.light`'s unitless scale at import time, not here,
/// so a parse-only test can assert the raw spec numbers unchanged.
#[derive(Debug, Clone)]
pub(crate) struct GltfPunctualLight {
    pub name: Option<String>,
    /// The owning node's index in the document.
    pub node_index: usize,
    pub kind: GltfLightKind,
    pub color: [f32; 3],
    pub intensity: f32,
    pub range: Option<f32>,
    /// World-space translation of the light's node.
    pub world_pos: [f32; 3],
    /// Normalised world-space direction the light illuminates — the node's
    /// local -Z axis (glTF's fixed light-forward convention) carried through
    /// the node's world rotation. Position and scale don't affect it (per
    /// spec, only orientation does), but deriving it as
    /// `normalize(transform(-Z) - transform(0))` gets the same answer
    /// without a separate rotation-only matrix path.
    pub world_forward: [f32; 3],
}
}

/// Recursively collect every node in `document` carrying a
/// `KHR_lights_punctual` light, alongside its world transform. Mirrors
/// `walk_gltf_node`'s recursion shape but only needs `node.transform()` — no
/// buffers, no geometry.
fn walk_gltf_node_for_lights(node: &gltf::Node, parent_world: Mat4, out: &mut Vec<GltfPunctualLight>) {
    let world = mat4_mul(&parent_world, &node.transform().matrix());
    if let Some(light) = node.light() {
        let world_pos = mat4_transform_point(&world, [0.0, 0.0, 0.0]);
        let fwd_pt = mat4_transform_point(&world, [0.0, 0.0, -1.0]);
        let world_forward = normalize3(sub3(fwd_pt, world_pos));
        let kind = match light.kind() {
            gltf::khr_lights_punctual::Kind::Directional => GltfLightKind::Directional,
            gltf::khr_lights_punctual::Kind::Point => GltfLightKind::Point,
            gltf::khr_lights_punctual::Kind::Spot { inner_cone_angle, outer_cone_angle } => {
                GltfLightKind::Spot { inner_cone_angle, outer_cone_angle }
            }
        };
        out.push(GltfPunctualLight {
            name: light.name().map(str::to_string),
            node_index: node.index(),
            kind,
            color: light.color(),
            intensity: light.intensity(),
            range: light.range(),
            world_pos,
            world_forward,
        });
    }
    for child in node.children() {
        walk_gltf_node_for_lights(&child, world, out);
    }
}

/// BUG-5uqg: every `KHR_lights_punctual` light in `document`'s imported node
/// set (`resolve_import_nodes` — same scene-resolution fallback the mesh
/// walk uses), resolved to world space. Called once from
/// `gltf_import_summary`, stored on [`GltfImportSummary::lights`].
pub(crate) fn collect_punctual_lights(document: &gltf::Document) -> Vec<GltfPunctualLight> {
    let mut out = Vec::new();
    for node in resolve_import_nodes(document) {
        walk_gltf_node_for_lights(&node, MAT4_IDENTITY, &mut out);
    }
    out
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
#[derive(Clone, Debug)]
pub(crate) enum GltfMeshSelector {
    /// The mesh of the one file node at `path` (see [`file_node_tree`]),
    /// world-transformed through its ancestors, children excluded. An
    /// imported Object's mesh (SCENE_DOCUMENT_DESIGN.md section 7.7).
    /// `material` keeps the primitives of one material: `-1` every
    /// primitive, `-2` ([`DEFAULT_MATERIAL_MESH_PARAM`]) the materialless
    /// ones, else that glTF material index. A node whose mesh carries
    /// several materials is one Object per material.
    Node { path: std::sync::Arc<str>, material: i32 },
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
    let uv1s: Option<Vec<[f32; 2]>> = reader.read_tex_coords(1).map(|it| it.into_f32().collect());

    let positions: Vec<[f32; 3]> = reader
        .read_positions()
        .ok_or_else(|| "primitive missing required POSITION accessor".to_string())?
        .collect();
    let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(|it| it.collect());
    let uvs: Option<Vec<[f32; 2]>> = reader.read_tex_coords(0).map(|it| it.into_f32().collect());
    let colors: Option<Vec<[f32; 4]>> = reader.read_colors(0).map(|it| it.into_rgba_f32().collect());
    // TANGENT (vec4: xyz direction + w bitangent sign) feeds authored
    // tangent-space normal mapping. Tangent directions are vectors, so use
    // the model's linear transform; the normal matrix is only for normals.
    // A reflected model flips the handedness sign in w. Absent → [0,0,0,0],
    // the shader's derived-frame sentinel.
    let tangents: Option<Vec<[f32; 4]>> = reader.read_tangents().map(|it| it.collect());

    let world_linear = mat3_upper_row_major(world);
    let world_det = world_linear[0][0]
        * (world_linear[1][1] * world_linear[2][2]
            - world_linear[1][2] * world_linear[2][1])
        - world_linear[0][1]
            * (world_linear[1][0] * world_linear[2][2]
                - world_linear[1][2] * world_linear[2][0])
        + world_linear[0][2]
            * (world_linear[1][0] * world_linear[2][1]
                - world_linear[1][1] * world_linear[2][0]);
    let tangent_sign = if world_det < 0.0 { -1.0 } else { 1.0 };

    let world_positions: Vec<[f32; 3]> = positions
        .iter()
        .map(|p| mat4_transform_point(world, *p))
        .collect();
    let world_normals: Option<Vec<[f32; 3]>> = normals
        .as_ref()
        .map(|ns| ns.iter().map(|n| normalize3(mat3_mul_vec3(*normal_mat, *n))).collect());
    let world_tangents: Option<Vec<[f32; 4]>> = tangents.as_ref().map(|ts| {
        ts.iter()
            .map(|t| {
                let d = normalize3(mat3_mul_vec3(world_linear, [t[0], t[1], t[2]]));
                [d[0], d[1], d[2], t[3] * tangent_sign]
            })
            .collect()
    });

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
            let uv1 = uv1s.as_ref().map_or([0.0, 0.0], |u| u[i]);
            let color = colors.as_ref().map_or([1.0, 1.0, 1.0, 1.0], |c| c[i]);
            let tangent = world_tangents.as_ref().map_or([0.0; 4], |ts| ts[i]);
            out.push(MeshVertex {
                position: world_positions[i],
                _pad0: 0.0,
                normal,
                _pad1: 0.0,
                uv,
                _pad2: uv1,
                tangent,
                color,
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
    flatten_node_mesh(node, &world, document, buffers, material_filter, out)?;
    for child in node.children() {
        walk_gltf_node(&child, world, document, buffers, material_filter, out)?;
    }
    Ok(())
}

/// One node's own mesh primitives (every instance of it), world-transformed
/// by `world`; its children are not visited.
fn flatten_node_mesh(
    node: &gltf::Node,
    world: &Mat4,
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    material_filter: MaterialFilter,
    out: &mut Vec<MeshVertex>,
) -> Result<(), String> {
    let Some(mesh) = node.mesh() else {
        return Ok(());
    };
    let instances = node_instance_transforms(node, document, buffers)?;
    let instance_worlds: Vec<Mat4> = match &instances {
        Some(list) => list.iter().map(|it| mat4_mul(world, it)).collect(),
        None => vec![*world],
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
    Ok(())
}

/// Where each copy of a node's mesh sits in the file's world: `world` once,
/// or `world` times each `EXT_mesh_gpu_instancing` instance (an unreadable
/// instancing block counts as one plain copy). The mesh loader places
/// vertices the same way, so bounds measured through this match what is
/// drawn.
fn instance_worlds(node: &gltf::Node, world: &Mat4, document: &gltf::Document, buffers: &[gltf::buffer::Data]) -> Vec<Mat4> {
    match node_instance_transforms(node, document, buffers).ok().flatten() {
        Some(list) if !list.is_empty() => list.iter().map(|it| mat4_mul(world, it)).collect(),
        _ => vec![*world],
    }
}

/// One file node as the scene import sees it: its path from the scene
/// root, its own mesh, and its children.
pub(crate) struct FileNode {
    /// Segments joined by `/`: each is the node's name (`/` swapped for a
    /// space; "node" when unnamed), with `#2`, `#3` for later siblings that
    /// share a name. Stable across a file swap while the names are.
    pub path: String,
    pub name: String,
    /// The node's index in the document.
    pub index: usize,
    pub has_mesh: bool,
    /// The node's mesh split by material, in first-use order: one part per
    /// glTF material (`None` is the glTF default material).
    pub parts: Vec<FilePart>,
    pub children: Vec<FileNode>,
}

/// The primitives of one node's mesh that share a material.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct FilePart {
    pub material: Option<u32>,
    /// Their vertex count once triangles are expanded and instances counted.
    pub vertex_count: u32,
    /// The centre of their bounding box in the file's world space.
    pub center: [f32; 3],
    /// Half that box's size on each axis: with `center`, the part's rest-pose
    /// bounds, which a modifier measures it by.
    pub half_extent: [f32; 3],
}

/// The file's node tree under the same roots the mesh loader walks
/// ([`resolve_import_nodes`]).
pub(crate) fn file_node_tree(document: &gltf::Document, buffers: &[gltf::buffer::Data]) -> Vec<FileNode> {
    fn segments(nodes: &[gltf::Node]) -> Vec<(String, String)> {
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        nodes
            .iter()
            .map(|node| {
                let name = node.name().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("node").replace('/', " ");
                let count = seen.entry(name.clone()).or_insert(0);
                *count += 1;
                let segment = if *count == 1 { name.clone() } else { format!("{name}#{count}") };
                (segment, name)
            })
            .collect()
    }
    fn parts(
        node: &gltf::Node,
        world: &Mat4,
        document: &gltf::Document,
        buffers: &[gltf::buffer::Data],
        parent_of: &[Option<usize>],
    ) -> Vec<FilePart> {
        let Some(mesh) = node.mesh() else {
            return Vec::new();
        };
        let placements = instance_worlds(node, world, document, buffers);
        let instances = placements.len();
        // A skinned mesh is where its skin poses it (glTF ignores its node's
        // transform), the same box `file_bounds` measures.
        let joints = node.skin().map(|skin| skin_matrices(&skin, document, buffers, parent_of));
        let mut parts: Vec<(FilePart, Bounds)> = Vec::new();
        for p in mesh.primitives() {
            let n = p.indices().map(|a| a.count()).or_else(|| p.get(&gltf::Semantic::Positions).map(|a| a.count()));
            let vertices = (n.unwrap_or(0) / 3 * 3) as u64 * instances as u64;
            let material = p.material().index().map(|index| index as u32);
            let at = match parts.iter().position(|(part, _)| part.material == material) {
                Some(at) => at,
                None => {
                    let empty = FilePart { material, vertex_count: 0, center: [0.0; 3], half_extent: [0.0; 3] };
                    parts.push((empty, None));
                    parts.len() - 1
                }
            };
            let (part, bounds) = &mut parts[at];
            part.vertex_count = (u64::from(part.vertex_count) + vertices).min(u64::from(u32::MAX)) as u32;
            grow_primitive(&p, joints.as_deref(), &placements, buffers, bounds);
        }
        parts
            .into_iter()
            .map(|(mut part, bounds)| {
                if let Some((lo, hi)) = bounds.filter(|(lo, hi)| lo.iter().chain(hi.iter()).all(|v| v.is_finite())) {
                    part.center = [0, 1, 2].map(|axis| (lo[axis] + hi[axis]) * 0.5);
                    part.half_extent = [0, 1, 2].map(|axis| (hi[axis] - lo[axis]) * 0.5);
                }
                part
            })
            .collect()
    }
    fn build(
        nodes: &[gltf::Node],
        prefix: &str,
        parent: Mat4,
        document: &gltf::Document,
        buffers: &[gltf::buffer::Data],
        parent_of: &[Option<usize>],
    ) -> Vec<FileNode> {
        segments(nodes)
            .into_iter()
            .zip(nodes)
            .map(|((segment, name), node)| {
                let path = if prefix.is_empty() { segment } else { format!("{prefix}/{segment}") };
                let world = mat4_mul(&parent, &node.transform().matrix());
                let children: Vec<gltf::Node> = node.children().collect();
                let parts = parts(node, &world, document, buffers, parent_of);
                FileNode {
                    children: build(&children, &path, world, document, buffers, parent_of),
                    parts,
                    path,
                    name,
                    index: node.index(),
                    has_mesh: node.mesh().is_some(),
                }
            })
            .collect()
    }
    let parent_of = build_parent_map(document);
    build(&resolve_import_nodes(document), "", MAT4_IDENTITY, document, buffers, &parent_of)
}

/// The node at `path` and its world matrix, found the way
/// [`file_node_tree`] names them.
fn find_file_node<'a>(document: &'a gltf::Document, path: &str) -> Option<(gltf::Node<'a>, Mat4)> {
    fn segment_of(nodes: &[gltf::Node<'_>], segment: &str) -> Option<usize> {
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        nodes.iter().position(|node| {
            let name = node.name().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("node").replace('/', " ");
            let count = seen.entry(name.clone()).or_insert(0);
            *count += 1;
            let this = if *count == 1 { name } else { format!("{name}#{count}") };
            this == segment
        })
    }
    let mut level = resolve_import_nodes(document);
    let mut world = MAT4_IDENTITY;
    let mut found = None;
    for segment in path.split('/') {
        let index = segment_of(&level, segment)?;
        let node = level.swap_remove(index);
        world = mat4_mul(&world, &node.transform().matrix());
        level = node.children().collect();
        found = Some(node);
    }
    found.map(|node| (node, world))
}

type Bounds = Option<([f32; 3], [f32; 3])>;
fn grow(bounds: &mut Bounds, p: [f32; 3]) {
    let (lo, hi) = bounds.get_or_insert((p, p));
    for axis in 0..3 {
        lo[axis] = lo[axis].min(p[axis]);
        hi[axis] = hi[axis].max(p[axis]);
    }
}
/// Each of the skin's joints' bind-pose matrix (joint world times its
/// inverse bind), or `None` when the skin cannot be read.
fn skin_matrices(
    skin: &gltf::Skin,
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    parent_of: &[Option<usize>],
) -> Vec<Mat4> {
    let reader = skin.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));
    let inverse_binds: Vec<Mat4> = reader.read_inverse_bind_matrices().map(|it| it.collect()).unwrap_or_default();
    skin.joints()
        .enumerate()
        .map(|(k, joint)| {
            let world = static_world_matrix(document, joint.index(), parent_of);
            mat4_mul(&world, inverse_binds.get(k).unwrap_or(&MAT4_IDENTITY))
        })
        .collect()
}
/// Grow by a skinned primitive's bind-pose vertices; false when it has
/// no skin attributes to pose by.
fn grow_skinned(primitive: &gltf::Primitive, joints: &[Mat4], buffers: &[gltf::buffer::Data], bounds: &mut Bounds) -> bool {
    let reader = primitive.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));
    let (Some(positions), Some(vertex_joints), Some(weights)) =
        (reader.read_positions(), reader.read_joints(0), reader.read_weights(0))
    else {
        return false;
    };
    for ((p, vertex_joints), weights) in positions.zip(vertex_joints.into_u16()).zip(weights.into_f32()) {
        let total: f32 = weights.iter().sum();
        if total <= 0.0 {
            continue;
        }
        let mut posed = [0.0f32; 3];
        for k in 0..4 {
            let Some(m) = joints.get(vertex_joints[k] as usize) else { continue };
            let q = mat4_transform_point(m, p);
            for axis in 0..3 {
                posed[axis] += weights[k] / total * q[axis];
            }
        }
        grow(bounds, posed);
    }
    true
}

/// Grow `bounds` by one primitive: its bind-pose vertices when the node is
/// skinned and the primitive carries skin attributes, else its box at each
/// placement of the node.
fn grow_primitive(
    primitive: &gltf::Primitive,
    joints: Option<&[Mat4]>,
    placements: &[Mat4],
    buffers: &[gltf::buffer::Data],
    bounds: &mut Bounds,
) {
    if let Some(joints) = joints
        && grow_skinned(primitive, joints, buffers, bounds)
    {
        return;
    }
    let b = primitive.bounding_box();
    for placed in placements {
        for corner in 0..8 {
            let p = [
                if corner & 1 == 0 { b.min[0] } else { b.max[0] },
                if corner & 2 == 0 { b.min[1] } else { b.max[1] },
                if corner & 4 == 0 { b.min[2] } else { b.max[2] },
            ];
            grow(bounds, mat4_transform_point(placed, p));
        }
    }
}

/// The file's world-space bounding box over every node mesh, and none when
/// it has no geometry. A skinned mesh is measured where its skin draws it in
/// the bind pose: glTF ignores a skinned mesh node's own transform (glTF 2.0
/// section 3.7.3.3), so its node box can sit far from what is drawn.
pub(crate) fn file_bounds(document: &gltf::Document, buffers: &[gltf::buffer::Data]) -> Option<([f32; 3], [f32; 3])> {
    fn visit(
        node: &gltf::Node,
        parent: Mat4,
        document: &gltf::Document,
        buffers: &[gltf::buffer::Data],
        parent_of: &[Option<usize>],
        bounds: &mut Bounds,
    ) {
        let world = mat4_mul(&parent, &node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            let joints = node.skin().map(|skin| skin_matrices(&skin, document, buffers, parent_of));
            let placements = instance_worlds(node, &world, document, buffers);
            for primitive in mesh.primitives() {
                grow_primitive(&primitive, joints.as_deref(), &placements, buffers, bounds);
            }
        }
        for child in node.children() {
            visit(&child, world, document, buffers, parent_of, bounds);
        }
    }
    let parent_of = build_parent_map(document);
    let mut bounds = None;
    for node in resolve_import_nodes(document) {
        visit(&node, MAT4_IDENTITY, document, buffers, &parent_of, &mut bounds);
    }
    bounds
}

/// Uncached fixture entry point. Production mesh loads flatten the snapshot
/// already read and authenticated by the shared decoded-mesh cache.
#[cfg(test)]
pub(crate) fn load_gltf_mesh(
    path: &std::path::Path,
    selector: GltfMeshSelector,
) -> Result<Vec<MeshVertex>, String> {
    let (document, buffers) = parse_document_and_buffers(path)?;
    load_gltf_mesh_from_buffers(&document, &buffers, selector)
}

/// Flatten the already resolved source used to key a decoded-mesh cache entry.
/// Mesh vertices depend on document data and buffers, not decoded images.
/// Invalid selectors or unsupported geometry return an error. A missing
/// default scene retains the existing `resolve_import_nodes` policy.
pub(crate) fn load_gltf_mesh_from_buffers(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    selector: GltfMeshSelector,
) -> Result<Vec<MeshVertex>, String> {
    let mut out = Vec::new();
    match selector {
        GltfMeshSelector::Node { path, material } => {
            let (node, world) = find_file_node(document, &path)
                .ok_or_else(|| format!("the file has no node at \"{path}\""))?;
            let filter = match material {
                DEFAULT_MATERIAL_MESH_PARAM => MaterialFilter::DefaultOnly,
                index if index >= 0 => MaterialFilter::Material(index as u32),
                _ => MaterialFilter::All,
            };
            flatten_node_mesh(&node, &world, document, buffers, filter, &mut out)?;
        }
        GltfMeshSelector::WholeScene => {
            for node in resolve_import_nodes(document) {
                walk_gltf_node(
                    &node,
                    MAT4_IDENTITY,
                    document,
                    buffers,
                    MaterialFilter::All,
                    &mut out,
                )?;
            }
        }
        GltfMeshSelector::Material { material_index } => {
            for node in resolve_import_nodes(document) {
                walk_gltf_node(
                    &node,
                    MAT4_IDENTITY,
                    document,
                    buffers,
                    MaterialFilter::Material(material_index),
                    &mut out,
                )?;
            }
        }
        GltfMeshSelector::DefaultMaterial => {
            for node in resolve_import_nodes(document) {
                walk_gltf_node(
                    &node,
                    MAT4_IDENTITY,
                    document,
                    buffers,
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
                flatten_primitive(&primitive, buffers, &MAT4_IDENTITY, &MAT3_IDENTITY, &mut out)?;
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
            flatten_primitive(primitive, buffers, &MAT4_IDENTITY, &MAT3_IDENTITY, &mut out)?;
        }
    }
    Ok(out)
}

/// One source resolver for both import dimensions and runtime pixels. WebP
/// overrides a base PNG/JPEG fallback, including when the base source is absent.
fn texture_image_index(texture: &gltf::Texture<'_>) -> Result<usize, String> {
    if let Some(extension) = texture.extensions().and_then(|e| e.get("EXT_texture_webp")) {
        return extension
            .get("source")
            .and_then(|v| v.as_u64())
            .and_then(|v| usize::try_from(v).ok())
            .ok_or_else(|| {
                format!(
                    "texture {} has no valid EXT_texture_webp image source",
                    texture.index()
                )
            });
    }
    texture
        .source()
        .map(|image| image.index())
        .ok_or_else(|| format!("texture {} has no image source", texture.index()))
}

/// Encoded bytes captured before consulting the shared decode jobs. Resolving
/// every requested dependency first prevents edited or deleted external images
/// from being hidden by a live cache entry. Decode uses these exact bytes.
pub(crate) struct GltfTextureSnapshot {
    encoded: Vec<u8>,
    format: Result<image::ImageFormat, String>,
    pub identity: [u8; 32],
}

/// The encoded image `texture_index` of a glTF document names, or the whole
/// file when it is a picture (PNG, JPG and the rest): a picture is its own
/// one image, so a Material's File texture reads it as it reads a model's
/// map. The file's own bytes decide which it is; the extension only names a
/// picture format that has no signature (TGA).
pub(crate) fn parse_texture_snapshot(
    path: &std::path::Path,
    texture_index: u32,
) -> Result<GltfTextureSnapshot, String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let picture = image::guess_format(&bytes).or_else(|_| image::ImageFormat::from_path(path));
    let (encoded, format) = match picture {
        Ok(format) => (bytes, Ok(format)),
        Err(_) => {
            let (document, buffers) = parse_document_and_buffers_from_slice(path, &bytes)?;
            let base = path.parent().unwrap_or_else(|| std::path::Path::new("./"));
            texture_encoded(&document, &buffers, base, texture_index)?
        }
    };
    let mut hash = Sha256::new();
    hash.update(b"manifold.gltf-texture-snapshot.v1");
    match &format {
        Ok(format) => hash.update(format.extensions_str()[0].as_bytes()),
        Err(error) => hash.update(error.as_bytes()),
    }
    hash.update(&encoded);
    Ok(GltfTextureSnapshot {
        encoded,
        format,
        identity: hash.finalize().into(),
    })
}

/// Every texture's pixel size, by texture index, read from the image
/// headers alone, with the same source selection the runtime decode uses.
/// A texture that cannot be read is 2x2, the size of the stand-in the
/// runtime draws for it.
pub(crate) fn texture_dims(
    path: &std::path::Path,
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
) -> Vec<(u32, u32)> {
    let base = path.parent().unwrap_or_else(|| std::path::Path::new("./"));
    (0..document.textures().len() as u32)
        .map(|index| {
            let (encoded, format) = texture_encoded(document, buffers, base, index).ok()?;
            image::ImageReader::with_format(std::io::Cursor::new(encoded), format.ok()?)
                .into_dimensions()
                .ok()
        })
        .map(|dims| dims.unwrap_or((2, 2)))
        .collect()
}

/// The pixel size of a picture file, read from its header alone (the pixels
/// are never decoded), and `None` when the file is not a picture it can
/// read.
pub(crate) fn picture_file_dims(path: &std::path::Path) -> Option<(u32, u32)> {
    let file = std::fs::File::open(path).ok()?;
    let reader = image::ImageReader::new(std::io::BufReader::new(file)).with_guessed_format().ok()?;
    let reader = if reader.format().is_some() {
        reader
    } else {
        let format = image::ImageFormat::from_path(path).ok()?;
        image::ImageReader::with_format(std::io::BufReader::new(std::fs::File::open(path).ok()?), format)
    };
    reader.into_dimensions().ok()
}

/// The encoded bytes of texture `texture_index` and their image format.
fn texture_encoded(
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
    base: &std::path::Path,
    texture_index: u32,
) -> Result<(Vec<u8>, Result<image::ImageFormat, String>), String> {
    let texture = document
        .textures()
        .nth(texture_index as usize)
        .ok_or_else(|| {
            format!(
                "texture_index {texture_index} out of range (document has {} textures)",
                document.textures().len()
            )
        })?;
    let index = texture_image_index(&texture)?;
    let image = document.images().nth(index).ok_or_else(|| {
        format!(
            "texture {texture_index} references image index {index}, out of range ({} images)",
            document.images().len()
        )
    })?;
    let (encoded, mime, uri) = match image.source() {
        gltf::image::Source::View { view, mime_type } => {
            let buffer = &buffers[view.buffer().index()].0;
            let end = view
                .offset()
                .checked_add(view.length())
                .ok_or("image buffer view range overflow")?;
            let bytes = buffer
                .get(view.offset()..end)
                .ok_or("image buffer view outside buffer")?;
            (bytes.to_vec(), Some(mime_type), None)
        }
        gltf::image::Source::Uri { uri, mime_type } => {
            // Reuse glTF's URI/data-URI decoding and percent-escaped path rules.
            validate_import_uri(uri)?;
            let data = gltf::buffer::Data::from_source(gltf::buffer::Source::Uri(uri), Some(base))
                .map_err(|e| format!("image {index} (\"{}\"): {e}", image_label(&image)))?;
            let mime = uri
                .strip_prefix("data:")
                .and_then(|v| v.split_once(";base64,"))
                .map(|v| v.0)
                .or(mime_type);
            (data.0, mime, Some(uri))
        }
    };
    let format = match mime {
        Some("image/png") => Ok(image::ImageFormat::Png),
        Some("image/jpeg") => Ok(image::ImageFormat::Jpeg),
        Some("image/webp") if uri.is_none() => Ok(image::ImageFormat::WebP),
        Some("image/webp") => Err("external-URI webp images are not supported".to_string()),
        None => match uri.and_then(|u| u.rsplit('.').next()) {
            Some("png") => Ok(image::ImageFormat::Png),
            Some("jpg" | "jpeg") => Ok(image::ImageFormat::Jpeg),
            _ => Err(format!(
                "image {index}: unsupported image encoding {mime:?}"
            )),
        },
        _ => Err(format!(
            "image {index}: unsupported image encoding {mime:?}"
        )),
    };
    Ok((encoded, format))
}

pub(crate) fn decode_texture_snapshot(
    snapshot: GltfTextureSnapshot,
) -> Result<(u32, u32, Vec<u8>), String> {
    #[cfg(test)]
    TEXTURE_DECODE_COUNT.with(|count| count.set(count.get() + 1));
    let decoded = image::load_from_memory_with_format(&snapshot.encoded, snapshot.format?)
        .map_err(|e| format!("glTF image decode failed: {e}"))?;
    let format = match &decoded {
        image::DynamicImage::ImageLuma8(_) => gltf::image::Format::R8,
        image::DynamicImage::ImageLumaA8(_) => gltf::image::Format::R8G8,
        image::DynamicImage::ImageRgb8(_) => gltf::image::Format::R8G8B8,
        image::DynamicImage::ImageRgba8(_) => gltf::image::Format::R8G8B8A8,
        // 16-bit and float pictures (EXR, HDR, 16-bit PNG) arrive as 8-bit
        // colour, clamped to 0..1.
        _ => {
            let rgba = decoded.to_rgba8();
            return Ok((rgba.width(), rgba.height(), rgba.into_raw()));
        }
    };
    image_data_to_rgba8(gltf::image::Data {
        width: decoded.width(),
        height: decoded.height(),
        format,
        pixels: decoded.into_bytes(),
    })
}

/// Decode only the selected texture, without decoding unrelated document images.
#[cfg(test)]
pub(crate) fn load_gltf_texture(
    path: &std::path::Path,
    texture_index: u32,
) -> Result<(u32, u32, Vec<u8>), String> {
    decode_texture_snapshot(parse_texture_snapshot(path, texture_index)?)
}

fn image_data_to_rgba8(data: gltf::image::Data) -> Result<(u32, u32, Vec<u8>), String> {
    let (width, height) = (data.width, data.height);
    let rgba: Vec<u8> = match data.format {
        gltf::image::Format::R8G8B8A8 => data.pixels,
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
                out.push(px[0]);
                out.push(px[0]);
                out.push(px[1]);
            }
            out
        }
        other => {
            return Err(format!("unsupported glTF image format {other:?}"));
        }
    };

    Ok((width, height, rgba))
}

manifold_core::testkit_visible! {
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
}

manifold_core::testkit_visible! {
/// glTF `magFilter`/`minFilter` components, retaining both the base filter
/// and the optional mip filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum GltfFilterMode {
    #[default]
    Linear,
    Nearest,
}
}

manifold_core::testkit_visible! {
/// GLB_XFAIL_BURNDOWN_DESIGN.md D3: one map family's sampler settings.
/// Default reproduces glTF's implicit no-sampler default (Repeat/Repeat/
/// Linear/Linear) — byte-identical to the pre-D3 hardcoded REPEAT sampler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GltfSamplerInfo {
    pub wrap_u: GltfWrapMode,
    pub wrap_v: GltfWrapMode,
    pub mag_filter: GltfFilterMode,
    pub min_filter: GltfFilterMode,
    pub mip_filter: Option<GltfFilterMode>,
}
}

impl Default for GltfSamplerInfo {
    fn default() -> Self {
        Self {
            wrap_u: GltfWrapMode::Repeat,
            wrap_v: GltfWrapMode::Repeat,
            mag_filter: GltfFilterMode::Linear,
            min_filter: GltfFilterMode::Linear,
            mip_filter: Some(GltfFilterMode::Linear),
        }
    }
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
    let (min, mip) = match s.min_filter() {
        Some(gltf::texture::MinFilter::Nearest) => (GltfFilterMode::Nearest, None),
        Some(gltf::texture::MinFilter::Linear) => (GltfFilterMode::Linear, None),
        Some(gltf::texture::MinFilter::NearestMipmapNearest) =>
            (GltfFilterMode::Nearest, Some(GltfFilterMode::Nearest)),
        Some(gltf::texture::MinFilter::NearestMipmapLinear) =>
            (GltfFilterMode::Nearest, Some(GltfFilterMode::Linear)),
        Some(gltf::texture::MinFilter::LinearMipmapNearest) =>
            (GltfFilterMode::Linear, Some(GltfFilterMode::Nearest)),
        Some(gltf::texture::MinFilter::LinearMipmapLinear) | None =>
            (GltfFilterMode::Linear, Some(GltfFilterMode::Linear)),
    };
    GltfSamplerInfo {
        wrap_u: wrap(s.wrap_s()),
        wrap_v: wrap(s.wrap_t()),
        mag_filter: mag,
        min_filter: min,
        mip_filter: mip,
    }
}

fn material_sampler_info(info: GltfSamplerInfo) -> MapSamplerDesc {
    let convert = |filter| match filter {
        GltfFilterMode::Linear => manifold_gpu::GpuFilterMode::Linear,
        GltfFilterMode::Nearest => manifold_gpu::GpuFilterMode::Nearest,
    };
    MapSamplerDesc {
        wrap_u: match info.wrap_u {
            GltfWrapMode::Repeat => manifold_gpu::GpuAddressMode::Repeat,
            GltfWrapMode::ClampToEdge => manifold_gpu::GpuAddressMode::ClampToEdge,
            GltfWrapMode::MirrorRepeat => manifold_gpu::GpuAddressMode::MirrorRepeat,
        },
        wrap_v: match info.wrap_v {
            GltfWrapMode::Repeat => manifold_gpu::GpuAddressMode::Repeat,
            GltfWrapMode::ClampToEdge => manifold_gpu::GpuAddressMode::ClampToEdge,
            GltfWrapMode::MirrorRepeat => manifold_gpu::GpuAddressMode::MirrorRepeat,
        },
        mag_filter: convert(info.mag_filter),
        min_filter: convert(info.min_filter),
        mip_filter: info.mip_filter.map(convert),
    }
}

fn extension_map_info(
    document: &gltf::Document,
    extension: Option<&serde_json::Value>,
    key: &str,
) -> MaterialMapInfo {
    let Some(info) = extension.and_then(|v| v.get(key)) else {
        return MaterialMapInfo::default();
    };
    let texture_index = info
        .get("index")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
    let transform = info
        .get("extensions")
        .and_then(|v| v.get("KHR_texture_transform"));
    let uv_transform = transform
        .map(parse_uv_transform_json)
        .map(|(uv, _)| uv)
        .unwrap_or(IDENTITY_UV_TRANSFORM);
    let tex_coord = transform
        .and_then(|v| v.get("texCoord"))
        .and_then(|v| v.as_u64())
        .or_else(|| info.get("texCoord").and_then(|v| v.as_u64()))
        .unwrap_or(0) as u32;
    MaterialMapInfo {
        uv_transform,
        tex_coord,
        sampler: material_sampler_info(sampler_info_for(document, texture_index)),
    }
}

manifold_core::testkit_visible! {
/// One distinct glTF material that has geometry, plus everything the
/// importer needs to build its `render_scene` object: the PBR factors,
/// the base-color texture index (into `document.textures()`), the alpha
/// mode, and the exact world-combined triangle-list vertex count (which
/// sizes that object's `gltf_mesh_source.max_capacity`).
// Stage 1 output — consumed by `gltf_import::assemble_import_graph`, which
// the `manifold-app` file-drop handler calls in production.
#[derive(Debug, Clone)]
pub(crate) struct GltfMaterialInfo {
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
    /// glTF `normalTexture.scale` (default 1.0).
    pub normal_scale: f32,
    /// Index into `document.textures()` for the glTF metallic-roughness map
    /// (G = roughness, B = metallic), if any.
    pub mr_texture: Option<u32>,
    /// Index into `document.textures()` for the occlusion map (R channel),
    /// if any. May be the SAME texture index as `mr_texture` (ORM packing)
    /// — the importer wires one source node into both ports in that case.
    pub occlusion_texture: Option<u32>,
    /// glTF `occlusionTexture.strength` (default 1.0).
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
    /// RAYTRACING_DESIGN.md section 16 TL3:
    /// `KHR_materials_diffuse_transmission`'s `diffuseTransmissionFactor`
    /// (default `0.0` when the extension is absent). Raw-JSON sniff — no
    /// typed accessor for this extension exists in `gltf`/`gltf-json` 1.4.1.
    /// Maps to `Material::translucency` via the importer's param table.
    pub diffuse_transmission_factor: f32,
    /// `KHR_materials_diffuse_transmission`'s `diffuseTransmissionColorFactor`
    /// (default `[1,1,1]`).
    pub diffuse_transmission_color: [f32; 3],
    /// `KHR_materials_diffuse_transmission`'s `diffuseTransmissionTexture`
    /// index, if any (A channel scales `diffuseTransmissionFactor`).
    pub diffuse_transmission_texture: Option<u32>,
    /// `KHR_materials_diffuse_transmission`'s `diffuseTransmissionColorTexture`
    /// index, if any (RGB tints the diffuse-transmission colour factor).
    pub diffuse_transmission_color_texture: Option<u32>,
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
    /// `KHR_materials_clearcoat`'s `clearcoatNormalTexture.scale` (default
    /// `1.0`).
    pub clearcoat_normal_scale: f32,
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
    /// `[1,1,1]`), or the dielectric F0 tint recovered from a
    /// `KHR_materials_pbrSpecularGlossiness` RGB factor.
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
    /// sampling untransformed UVs. A `texCoord` index override is preserved in
    /// the per-map metadata below.
    pub base_color_uv_transform: [f32; 6],
    /// Same affine transform for the normal map's `KHR_texture_transform`
    /// (gltf 1.4.1's `NormalTexture` has no typed accessor — parsed from
    /// the raw `extensions` JSON via [`parse_uv_transform_json`]).
    pub normal_uv_transform: [f32; 6],
    /// Same for the metallic-roughness map (typed accessor).
    pub mr_uv_transform: [f32; 6],
    /// Same for the occlusion map (raw JSON, like normal).
    pub occlusion_uv_transform: [f32; 6],
    /// Same for the emissive map (typed accessor).
    pub emissive_uv_transform: [f32; 6],
    /// Core map UV sets in base, normal, metallic-roughness, occlusion,
    /// emissive order. A transform's `texCoord` override wins over the
    /// texture-info `texCoord` value.
    pub core_tex_coords: [u32; 5],
    /// `true` when `mr_texture` above actually points at a
    /// `KHR_materials_pbrSpecularGlossiness` `specularGlossinessTexture`
    /// (GLB_XFAIL_BURNDOWN_DESIGN.md D2, BUG-167) rather than a genuine
    /// glTF metallic-roughness map — its usable channel is glossiness in
    /// ALPHA, not roughness/metallic in G/B. `gltf_import.rs` wires the
    /// `node.gltf_texture_source` feeding this map with `mode =
    /// gloss_to_roughness`, which repacks `(0, 1-gloss, 0, 1)` at decode
    /// time so `render_scene`'s existing G=roughness/B=metallic read stays
    /// untouched — the shader sees only metal-rough, per D2. The same
    /// texture's RGB is carried as the specular-color map (sRGB) while its
    /// alpha is repacked as glossiness, so both channels remain available to
    /// the downstream material.
    pub mr_texture_is_gloss_alpha: bool,
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
    /// UV, UV-set, and sampler metadata for extension maps in the
    /// fixed [`MaterialMapInfo`] order documented on `Material`.
    pub extension_maps: [MaterialMapInfo; 14],
}
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
    pub specular_tint: [f32; 3],
}

/// Convert spec-gloss's `glossinessFactor` + `specularFactor` (RGB) to
/// metal-rough's `roughness` plus the existing dielectric F0 controls
/// (`KHR_materials_specular`'s slots, GLB_CONFORMANCE_DESIGN.md G-P4).
/// The authored RGB factor is already an F0 colour, while MANIFOLD's shader
/// multiplies the tint by the default dielectric reflectance (0.04), so the
/// tint is normalized by 0.04 and the scalar weight stays at 1.0. `metallic`
/// is pinned to `0.0`: spec-gloss has no metalness channel of its own.
pub(crate) fn convert_spec_gloss(
    glossiness_factor: f32,
    specular_factor_rgb: [f32; 3],
) -> SpecGlossConversion {
    let specular_tint = specular_factor_rgb.map(|value| value.clamp(0.0, 1.0) / 0.04);
    SpecGlossConversion {
        roughness: (1.0 - glossiness_factor).clamp(0.0, 1.0),
        metallic: 0.0,
        specular_factor: 1.0,
        specular_tint,
    }
}

manifold_core::testkit_visible! {
/// `node.gltf_mesh_source`'s `material_index` param sentinel selecting
/// [`GltfMeshSelector::DefaultMaterial`] — distinct from the param's own
/// "unset" default (`-1`, which falls through to the `mesh_index`/
/// `WholeScene` path; see `gltf_mesh_source.rs::run`). Real glTF material
/// indices are always `>= 0`, so `-2` never collides with a genuine
/// selection. GLB_XFAIL_BURNDOWN_DESIGN.md D4 (BUG-171).
pub(crate) const DEFAULT_MATERIAL_MESH_PARAM: i32 = -2;
}

/// [`GltfImportSummary::materials`]' reserved sentinel for the synthetic
/// glTF-default-material entry (D4) — pinned by
/// `GLB_XFAIL_BURNDOWN_DESIGN.md` section 3: real glTF material indices are
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

manifold_core::testkit_visible! {
pub(crate) const IDENTITY_UV_TRANSFORM: [f32; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
}

use manifold_node_engine::scene::material::VOLUME_ATTENUATION_DISTANCE_NO_ATTENUATION;

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

manifold_core::testkit_visible! {
/// One embedded glTF camera resolved to world-space pose, ready for
/// `node.free_camera`'s pos/yaw/pitch/roll param surface. Perspective
/// projection only (BUG-d2qz scope) — `parse_cameras` reports and skips
/// orthographic cameras rather than guessing an equivalent.
#[derive(Debug, Clone)]
pub(crate) struct GltfCameraInfo {
    /// The glTF camera's own `name`, falling back to its owning node's
    /// `name`, falling back to `None` (caller numbers it).
    pub name: Option<String>,
    /// The owning node's index in the document.
    pub node_index: usize,
    pub pos: [f32; 3],
    /// Radians, `node.free_camera`'s convention: yaw about world +Y, pitch
    /// about the camera's own right axis, roll about `fwd` — see
    /// [`euler_from_fwd_up`], which inverts `Camera::from_pos_euler`.
    pub yaw: f32,
    pub pitch: f32,
    pub roll: f32,
    pub fov_y: f32,
    pub near: f32,
    pub far: f32,
}
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

manifold_core::testkit_visible! {
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
}

manifold_core::testkit_visible! {
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
}

manifold_core::testkit_visible! {
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
}

manifold_core::testkit_visible! {
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
    /// mesh-owning node itself (glTF 2.0 section 3.7.2.1: it animates that node's
    /// morph weights, overriding `mesh.weights`), so unlike TRS there is
    /// no ancestor-chain composition to resolve — `gltf_import.rs` looks
    /// this up directly by the contributing node's index.
    pub weights: Option<WeightsTrack>,
}
}

manifold_core::testkit_visible! {
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

// ─── GLTF_ANIMATION_DESIGN.md A2 — skinning (D2) ───────────────────────

manifold_core::testkit_visible! {
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
    /// TRS, whichever the node specifies). Since BUG-209's fix the pose
    /// sampler reads this only as a parse-time record — root joints compose
    /// through the whole-scene ANIMATED walk at runtime, which equals this
    /// value when no ancestor animates.
    pub joint_root_world: Vec<Mat4>,
    pub inverse_bind_matrices: Vec<Mat4>,
    // GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: the per-joint bind-TRS fields that
    // used to live here (`joint_bind_translation`/`_rotation`/`_scale`)
    // are DELETED — deduped into `GltfAnimSet::node_bind_trs` (P1, indexed
    // by whole-scene node index rather than duplicated per-skin), which
    // `gltf_skeleton_pose::sample_joint_local` now reads exclusively.
}
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
/// `node.transform().matrix()` root-first. Used for the (rare) ancestor
/// chain ABOVE a skin's joint tree (see `GltfSkinInfo::joint_root_world`)
/// and, since BUG-d2qz, for a camera node's own world pose — cameras are
/// never animated by A1/A2's scope, so the static chain is exact for them.
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

/// Invert `Camera::from_pos_euler`'s basis construction
/// (`crate::node_graph::camera`): given a world-space forward and up
/// (both expected normalized; `up` need only be non-parallel to `fwd` —
/// it's re-orthonormalized here the same way `from_pos_euler` builds its
/// own `up0`), recover the `(yaw, pitch, roll)` triple that reproduces
/// them. `pitch = asin(fwd.y)`, `yaw = atan2(-fwd.x, -fwd.z)` fall straight
/// out of `from_pos_euler`'s `fwd` formula; `roll` is recovered by
/// projecting `up` onto the pre-roll `(right0, up0)` basis `from_pos_euler`
/// itself derives from `yaw`/`pitch` before rotating by `roll` — see the
/// round-trip proof in `tests::euler_from_fwd_up_round_trips_from_pos_euler`.
/// Degenerate only where `from_pos_euler` itself is (`pitch` at exactly
/// ±90°, where `yaw` isn't observable from `fwd` alone) — not exercised by
/// any camera-bearing fixture in the Khronos sample set.
fn euler_from_fwd_up(fwd: [f32; 3], up: [f32; 3]) -> (f32, f32, f32) {
    let pitch = fwd[1].clamp(-1.0, 1.0).asin();
    let yaw = (-fwd[0]).atan2(-fwd[2]);
    let right0 = [yaw.cos(), 0.0, -yaw.sin()];
    let up0 = [yaw.sin() * pitch.sin(), pitch.cos(), yaw.cos() * pitch.sin()];
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let s = -dot(up, right0);
    let c = dot(up, up0);
    let roll = s.atan2(c);
    (yaw, pitch, roll)
}

/// Every `document.nodes()` entry carrying a `camera()`, resolved to
/// world-space pose via `static_world_matrix` — BUG-d2qz. Perspective
/// cameras become a [`GltfCameraInfo`]; orthographic cameras (no
/// `node.free_camera` ortho mode) are skipped with a report line each,
/// never silently.
pub(crate) fn parse_cameras(document: &gltf::Document, parent_of: &[Option<usize>]) -> (Vec<GltfCameraInfo>, Vec<String>) {
    let mut cameras = Vec::new();
    let mut report_lines = Vec::new();
    for node in document.nodes() {
        let Some(cam) = node.camera() else { continue };
        let label = cam
            .name()
            .or(node.name())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("camera {}", cam.index()));
        match cam.projection() {
            gltf::camera::Projection::Perspective(persp) => {
                let world = static_world_matrix(document, node.index(), parent_of);
                let pos = [world[3][0], world[3][1], world[3][2]];
                // Local -Z is forward, local +Y is up (glTF camera
                // convention, identical to `node.free_camera`'s own
                // yaw=pitch=roll=0 convention) — transform both by the
                // world rotation (columns 2 and 1) and normalize away any
                // uniform node scale.
                let fwd = normalize3([-world[2][0], -world[2][1], -world[2][2]]);
                let up = normalize3([world[1][0], world[1][1], world[1][2]]);
                let (yaw, pitch, roll) = euler_from_fwd_up(fwd, up);
                cameras.push(GltfCameraInfo {
                    name: Some(label),
                    node_index: node.index(),
                    pos,
                    yaw,
                    pitch,
                    roll,
                    fov_y: persp.yfov(),
                    near: persp.znear(),
                    far: persp.zfar().unwrap_or(10_000.0),
                });
            }
            gltf::camera::Projection::Orthographic(_) => {
                report_lines.push(format!(
                    "camera {label:?}: orthographic projection not imported (node.free_camera \
                     has no orthographic mode) — skipped"
                ));
            }
        }
    }
    (cameras, report_lines)
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

// ─── GLTF_ANIM_RUNTIME_V2_DESIGN.md D4 (P3) — rigid multi-node via the
// node-slot palette ──────────────────────────────────────────────────

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

/// (vertices, per-vertex joint indices as f32, per-vertex weights) — the
/// coincident triple `node.gltf_skinned_mesh_source` uploads as its three
/// array outputs.
pub(crate) type SkinnedMeshData = (Vec<MeshVertex>, Vec<[f32; 4]>, Vec<[f32; 4]>);

/// Flatten ONE skinned mesh-owning node's primitives (filtered to
/// `material_index`) into a triangle-list `MeshVertex` buffer plus
/// coincident per-vertex joint indices (as f32 — exact for the spec's
/// joint-count range) and weights. LOCAL space, NO node-transform applied:
/// per glTF 2.0 section 3.7.3.3, a skinned mesh's positioning comes ENTIRELY from
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
        let uv1s: Option<Vec<[f32; 2]>> = reader.read_tex_coords(1).map(|it| it.into_f32().collect());
        let colors: Option<Vec<[f32; 4]>> = reader.read_colors(0).map(|it| it.into_rgba_f32().collect());
        // BUG-wfxe: authored tangents stay ZERO here deliberately — the GPU
        // skinning path transforms positions/normals with the joints but has
        // no tangent transform, so passing the authored frame through would
        // shade skinned animation with a stale basis. Zero = the shader's
        // derived-frame sentinel = today's behavior, until the skin shader
        // learns to skin tangents (follow-up).
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
                let uv1 = uv1s.as_ref().map_or([0.0, 0.0], |u| u[i]);
                let color = colors.as_ref().map_or([1.0, 1.0, 1.0, 1.0], |c| c[i]);
                verts.push(MeshVertex {
                    position: positions[i],
                    _pad0: 0.0,
                    normal,
                    _pad1: 0.0,
                    uv,
                    _pad2: uv1,
                    tangent: [0.0; 4],
                    color,
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

/// Collect EVERY skinned node whose mesh carries a primitive matching
/// `material_index`, each paired with its glTF skin index — the multi-node
/// shared-skin merge (a Sketchfab FBX convert splits one armature's meshes
/// across several nodes under one material) needs all of them, not the
/// first DFS hit.
fn collect_skinned_nodes_for_material<'a>(
    node: &gltf::Node<'a>,
    material_index: u32,
    out: &mut Vec<(gltf::Node<'a>, usize)>,
) {
    if let Some(skin) = node.skin()
        && let Some(mesh) = node.mesh()
        && mesh.primitives().any(|p| primitive_material_matches(p.material().index(), material_index))
    {
        out.push((node.clone(), skin.index()));
    }
    for child in node.children() {
        collect_skinned_nodes_for_material(&child, material_index, out);
    }
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
            let uv1s: Option<Vec<[f32; 2]>> = reader.read_tex_coords(1).map(|it| it.into_f32().collect());
            let colors: Option<Vec<[f32; 4]>> = reader.read_colors(0).map(|it| it.into_rgba_f32().collect());
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
                    let uv1 = uv1s.as_ref().map_or([0.0, 0.0], |u| u[i]);
                    let color = colors.as_ref().map_or([1.0, 1.0, 1.0, 1.0], |c| c[i]);
                    verts.push(MeshVertex {
                        position: positions[i],
                        _pad0: 0.0,
                        normal,
                        _pad1: 0.0,
                        uv,
                        _pad2: uv1,
                        // Zero like the skinned path — GPU node-slot
                        // transforms have no tangent transform (BUG-wfxe).
                        tangent: [0.0; 4],
                        color,
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
/// rigid-multi-node-palette geometry for `material_index`. Collects EVERY
/// skinned node contributing the material (A2's original single-node scope
/// is the common one-node case of the same loop); when more than one is
/// found they must all share ONE glTF skin index — the same condition
/// `resolve_skin_for_key` checks at import time — and their vertices
/// concatenate directly, since their JOINTS_0 all index that one shared
/// palette. Only when NO skinned contributor exists does this fall back to
/// D4's node-slot palette across every contributing node
/// ([`find_material_contributing_nodes`]) — `gltf_import.rs` only reaches
/// that fallback when `GltfMaterialInfo::rigid_multi_node` resolved
/// (either the multi-node case or the single-node-ambiguous-ancestor case,
/// both of which this same runtime node-enumeration reproduces identically
/// to the import-time resolution, so slot order always agrees).
pub(crate) fn load_gltf_skinned_mesh(
    path: &std::path::Path,
    material_index: u32,
    node_path: &str,
) -> Result<SkinnedMeshData, String> {
    let (document, buffers, _images, _image_report_lines) = import_glb(path)?;
    // An imported part (SCENE_DOCUMENT_DESIGN.md section 7.7): one file
    // node's primitives of one material. Skinned, it reads its own skin;
    // otherwise it is a one-slot node palette driven by its animated chain.
    if !node_path.is_empty() {
        let (node, _) = find_file_node(&document, node_path)
            .ok_or_else(|| format!("{}: the file has no node at \"{node_path}\"", path.display()))?;
        return if node.skin().is_some() {
            flatten_skinned_node(&node, &buffers, material_index)
        } else {
            flatten_rigid_multi_node(&document, &buffers, &[node.index()], material_index)
        };
    }
    let mut skinned: Vec<(gltf::Node, usize)> = Vec::new();
    for root in resolve_import_nodes(&document) {
        collect_skinned_nodes_for_material(&root, material_index, &mut skinned);
    }
    match skinned.len() {
        0 => {}
        1 => {
            let (node, _) = skinned.into_iter().next().unwrap();
            return flatten_skinned_node(&node, &buffers, material_index);
        }
        _ => {
            let distinct: std::collections::BTreeSet<usize> =
                skinned.iter().map(|(_, s)| *s).collect();
            if distinct.len() != 1 {
                return Err(format!(
                    "{}: {} skinned nodes contribute {} but use {} DIFFERENT skins — \
                     a merged palette is unsupported",
                    path.display(),
                    skinned.len(),
                    material_label(material_index),
                    distinct.len()
                ));
            }
            let (mut verts, mut joints, mut weights) = (Vec::new(), Vec::new(), Vec::new());
            for (node, _) in &skinned {
                let (v, j, w) = flatten_skinned_node(node, &buffers, material_index)?;
                verts.extend(v);
                joints.extend(j);
                weights.extend(w);
            }
            if verts.is_empty() {
                return Err(format!(
                    "{}: {} skinned nodes contributed no geometry for {}",
                    path.display(),
                    skinned.len(),
                    material_label(material_index)
                ));
            }
            return Ok((verts, joints, weights));
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
                    // UVs are attributes of the base vertex, not morph
                    // displacements.  Keeping this lane zero ensures the
                    // base mesh's UV1 survives the additive morph stage.
                    _pad2: [0.0, 0.0],
                    // Morph TANGENT deltas exist in the spec (the reader's
                    // third tuple element above) but morph-blended tangent
                    // frames are unbuilt — zero = derived-frame sentinel.
                    tangent: [0.0; 4],
                    // COLOR_0 is a base attribute, never a morph delta.
                    color: [0.0; 4],
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
/// `node.skin_mesh` (glTF applies morph THEN skin, section 3.7.2). Per A2/A3,
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
    node_path: &str,
) -> Result<(Vec<MeshVertex>, u32, u32), String> {
    let (document, buffers, _images, _image_report_lines) = import_glb(path)?;
    let parent_of = build_parent_map(&document);
    // An imported part names its file node; otherwise the first node
    // carrying the material.
    let by_path = if node_path.is_empty() {
        None
    } else {
        let (node, _) = find_file_node(&document, node_path)
            .ok_or_else(|| format!("{}: the file has no node at \"{node_path}\"", path.display()))?;
        Some(node)
    };
    let roots = if by_path.is_some() { Vec::new() } else { resolve_import_nodes(&document) };
    if let Some(found) = by_path.or_else(|| roots.iter().find_map(|node| find_mesh_node_for_material(node, material_index))) {
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

/// glTF's implicit default material (spec section 3.9.2: white, metallic 1, roughness 1) for geometry with no material, with empty geometry facts.
pub(crate) fn default_material_info() -> GltfMaterialInfo {
    GltfMaterialInfo {
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
        diffuse_transmission_factor: 0.0,
        diffuse_transmission_color: [1.0, 1.0, 1.0],
        diffuse_transmission_texture: None,
        diffuse_transmission_color_texture: None,
        clearcoat_factor: 0.0,
        clearcoat_roughness_factor: 0.0,
        clearcoat_normal_scale: 1.0,
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
        core_tex_coords: [0; 5],
        mr_texture_is_gloss_alpha: false,
        base_color_sampler: GltfSamplerInfo::default(),
        normal_sampler: GltfSamplerInfo::default(),
        mr_sampler: GltfSamplerInfo::default(),
        occlusion_sampler: GltfSamplerInfo::default(),
        emissive_sampler: GltfSamplerInfo::default(),
        extension_maps: [MaterialMapInfo::default(); 14],
    }
}

/// One glTF material's surface: factors, maps, samplers, texture transforms
/// and extensions, read from the document alone. The scene compiler reads a model file's materials through this
/// (SCENE_DOCUMENT_DESIGN.md section 7.6 (Materials and layer planes)).
pub(crate) fn material_info(
    document: &gltf::Document,
    m: &gltf::Material,
    label: &str,
    geometry_report_lines: &mut Vec<String>,
) -> GltfMaterialInfo {
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
    let fold_typed = |info: Option<gltf::texture::Info>| -> [f32; 6] {
        match info.as_ref().and_then(|t| t.texture_transform()) {
            Some(t) => fold_uv_transform(t.offset(), t.rotation(), t.scale()),
            None => IDENTITY_UV_TRANSFORM,
        }
    };
    let mut base_color_info = pbr.base_color_texture();
    let mut base_color_uv_transform = fold_typed(pbr.base_color_texture());
    let mut mr_texture_index = pbr.metallic_roughness_texture().map(|t| t.texture().index() as u32);
    // BUG-pm9m: retained alongside `mr_texture_index` (updated in
    // lockstep, including the spec-gloss override below) purely to
    // read its `tex_coord()` for the TEXCOORD_n warning further
    // down — `mr_texture_index` alone only carries the texture,
    // not which UV set it samples.
    let mut mr_info = pbr.metallic_roughness_texture();
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
    let mut specular_color_factor_override: Option<[f32; 3]> = None;
    let mut spec_gloss_color_texture: Option<u32> = None;
    let mut spec_gloss_color_map: Option<MaterialMapInfo> = None;
    let mut spec_gloss_present = false;
    if let Some(sg) = m.pbr_specular_glossiness() {
        spec_gloss_present = true;
        let conv = convert_spec_gloss(sg.glossiness_factor(), sg.specular_factor());
        base_color_factor = sg.diffuse_factor();
        base_color_info = sg.diffuse_texture();
        base_color_uv_transform = fold_typed(sg.diffuse_texture());
        roughness = conv.roughness;
        metallic = conv.metallic;
        specular_factor_override = Some(conv.specular_factor);
        specular_color_factor_override = Some(conv.specular_tint);
        match sg.specular_glossiness_texture() {
            Some(tex) => {
                let texture_index = tex.texture().index() as u32;
                let tex_coord = tex
                    .texture_transform()
                    .and_then(|transform| transform.tex_coord())
                    .unwrap_or_else(|| tex.tex_coord());
                let uv_transform = fold_typed(Some(tex));
                mr_texture_index = Some(texture_index);
                mr_info = sg.specular_glossiness_texture();
                mr_uv_transform = fold_typed(sg.specular_glossiness_texture());
                mr_texture_is_gloss_alpha = true;
                spec_gloss_color_texture = Some(texture_index);
                spec_gloss_color_map = Some(MaterialMapInfo {
                    uv_transform,
                    tex_coord,
                    sampler: material_sampler_info(sampler_info_for(
                        document,
                        Some(texture_index),
                    )),
                });
            }
            None => {
                mr_texture_index = None;
                mr_info = None;
                mr_uv_transform = IDENTITY_UV_TRANSFORM;
            }
        }
    }

    let fold_raw = |ext: Option<&serde_json::Value>| -> [f32; 6] {
        match ext {
            Some(v) => {
                parse_uv_transform_json(v).0
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
    let typed_tex_coord = |info: Option<gltf::texture::Info>| -> u32 {
        info.map(|t| {
            t.texture_transform()
                .and_then(|transform| transform.tex_coord())
                .unwrap_or_else(|| t.tex_coord())
        })
        .unwrap_or(0)
    };
    let core_tex_coords = [
        typed_tex_coord(base_color_info.clone()),
        m.normal_texture()
            .map(|t| t.extension_value("KHR_texture_transform").and_then(|v| v.get("texCoord")).and_then(|v| v.as_u64()).map(|v| v as u32).unwrap_or_else(|| t.tex_coord()))
            .unwrap_or(0),
        typed_tex_coord(mr_info.clone()),
        m.occlusion_texture()
            .map(|t| t.extension_value("KHR_texture_transform").and_then(|v| v.get("texCoord")).and_then(|v| v.as_u64()).map(|v| v as u32).unwrap_or_else(|| t.tex_coord()))
            .unwrap_or(0),
        typed_tex_coord(m.emissive_texture()),
    ];

    // GLB_CONFORMANCE_DESIGN.md G-P4/D5: KHR_materials_specular +
    // KHR_materials_ior, mapped to F0 scale downstream
    // (gltf_import.rs). Defaults (1.5 / 1.0 / [1,1,1]) reproduce
    // today's hardcoded F0=0.04 exactly — see `fs_pbr`. D2:
    // `specular_factor_override` (set above, non-None only for a
    // spec-gloss material) wins over the direct
    // `KHR_materials_specular` extension — spec-gloss materials
    // don't also carry that extension in practice, but if one did,
    // D2's conversion is the more specific decision for this slot.
    let ior = if spec_gloss_present {
        1.5
    } else {
        m.ior().unwrap_or(1.5)
    };
    // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6: raw-JSON texture-index
    // helper, hoisted above its original sheen/iridescence/
    // anisotropy use so clearcoat/specular (parsed below, before
    // that block) can share it too.
    let tex_idx = |v: &serde_json::Value, key: &str| -> Option<u32> {
        v.get(key)?.get("index")?.as_u64().map(|i| i as u32)
    };
    // BUG-pm9m: sibling of `tex_idx` — the raw-JSON texture-info
    // object's own `texCoord` (glTF spec default 0), read the same
    // way for the extension texture families below that have no
    // typed accessor at 1.4.1 (clearcoat/sheen/iridescence/
    // anisotropy). `None` means the slot itself is absent, not
    // texCoord 0 — filtered out below same as every typed slot.
    let tex_coord_idx = |v: &serde_json::Value, key: &str| -> Option<u32> {
        let obj = v.get(key)?;
        Some(obj
            .get("extensions")
            .and_then(|e| e.get("KHR_texture_transform"))
            .and_then(|t| t.get("texCoord"))
            .or_else(|| obj.get("texCoord"))
            .and_then(|t| t.as_u64())
            .unwrap_or(0) as u32)
    };
    let specular_ext = m.specular();
    let specular_factor = specular_factor_override
        .unwrap_or_else(|| specular_ext.as_ref().map(|s| s.specular_factor()).unwrap_or(1.0));
    let specular_color_factor = specular_color_factor_override.unwrap_or_else(|| {
        specular_ext
            .as_ref()
            .map(|s| s.specular_color_factor())
            .unwrap_or([1.0, 1.0, 1.0])
    });
    // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6 (D1 revised — full spec
    // surface): specularTexture/specularColorTexture, typed
    // accessors on `Specular<'_>` (gltf 1.4.1 has the feature).
    let specular_texture = if spec_gloss_present {
        None
    } else {
        specular_ext
            .as_ref()
            .and_then(|s| s.specular_texture())
            .map(|t| t.texture().index() as u32)
    };
    let specular_color_texture = if spec_gloss_present {
        spec_gloss_color_texture
    } else {
        specular_ext
            .as_ref()
            .and_then(|s| s.specular_color_texture())
            .map(|t| t.texture().index() as u32)
    };

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
    let clearcoat_normal_scale = clearcoat_ext
        .and_then(|v| v.get("clearcoatNormalTexture"))
        .and_then(|v| v.get("scale"))
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0) as f32;
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

    // RAYTRACING_DESIGN.md section 16 TL3:
    // KHR_materials_diffuse_transmission. Raw-JSON sniff — no typed
    // accessor at 1.4.1. Factor and colour values feed the material;
    // both texture families are retained for SceneObject map wiring.
    let diffuse_transmission_ext =
        m.extension_value("KHR_materials_diffuse_transmission");
    let diffuse_transmission_factor = diffuse_transmission_ext
        .map(|v| f1(v, "diffuseTransmissionFactor", 0.0))
        .unwrap_or(0.0);
    let diffuse_transmission_color = diffuse_transmission_ext
        .and_then(|v| v.get("diffuseTransmissionColorFactor"))
        .and_then(|a| a.as_array())
        .and_then(|a| {
            Some([
                a.first()?.as_f64()? as f32,
                a.get(1)?.as_f64()? as f32,
                a.get(2)?.as_f64()? as f32,
            ])
        })
        .unwrap_or([1.0, 1.0, 1.0]);
    let diffuse_transmission_texture = diffuse_transmission_ext
        .and_then(|v| tex_idx(v, "diffuseTransmissionTexture"));
    let diffuse_transmission_color_texture = diffuse_transmission_ext
        .and_then(|v| tex_idx(v, "diffuseTransmissionColorTexture"));

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

    let base_color_texture = base_color_info.as_ref().map(|t| t.texture().index() as u32);
    let normal_texture = m.normal_texture().map(|t| t.texture().index() as u32);
    let occlusion_texture = m.occlusion_texture().map(|t| t.texture().index() as u32);
    let emissive_texture = m.emissive_texture().map(|t| t.texture().index() as u32);

    // BUG-pm9m: UV sets 0 and 1 are carried through MeshVertex;
    // report only higher authored sets, which still fall back to UV0.
    for (slot, tex_coord) in [
        ("baseColor", base_color_info.as_ref().map(|_| core_tex_coords[0])),
        ("metallicRoughness", mr_info.as_ref().map(|_| core_tex_coords[2])),
        ("normal", m.normal_texture().map(|_| core_tex_coords[1])),
        ("occlusion", m.occlusion_texture().map(|_| core_tex_coords[3])),
        ("emissive", m.emissive_texture().map(|_| core_tex_coords[4])),
        (
            "specular",
            if spec_gloss_present {
                None
            } else {
                specular_ext
                    .as_ref()
                    .and_then(|s| s.specular_texture())
                    .map(|t| t.tex_coord())
            },
        ),
        (
            "specularColor",
            if spec_gloss_present {
                spec_gloss_color_map.map(|info| info.tex_coord)
            } else {
                specular_ext
                    .as_ref()
                    .and_then(|s| s.specular_color_texture())
                    .map(|t| t.tex_coord())
            },
        ),
        (
            "volumeThickness",
            volume_ext.as_ref().and_then(|v| v.thickness_texture()).map(|t| t.tex_coord()),
        ),
        ("clearcoat", clearcoat_ext.and_then(|v| tex_coord_idx(v, "clearcoatTexture"))),
        (
            "clearcoatRoughness",
            clearcoat_ext.and_then(|v| tex_coord_idx(v, "clearcoatRoughnessTexture")),
        ),
        (
            "clearcoatNormal",
            clearcoat_ext.and_then(|v| tex_coord_idx(v, "clearcoatNormalTexture")),
        ),
        ("sheenColor", sheen_ext.and_then(|v| tex_coord_idx(v, "sheenColorTexture"))),
        (
            "sheenRoughness",
            sheen_ext.and_then(|v| tex_coord_idx(v, "sheenRoughnessTexture")),
        ),
        (
            "iridescence",
            iridescence_ext.and_then(|v| tex_coord_idx(v, "iridescenceTexture")),
        ),
        (
            "iridescenceThickness",
            iridescence_ext.and_then(|v| tex_coord_idx(v, "iridescenceThicknessTexture")),
        ),
        (
            "anisotropy",
            anisotropy_ext.and_then(|v| tex_coord_idx(v, "anisotropyTexture")),
        ),
        (
            "transmission",
            m.transmission().and_then(|t| t.transmission_texture()).map(|t| t.tex_coord()),
        ),
        (
            "diffuseTransmission",
            diffuse_transmission_ext.and_then(|v| tex_coord_idx(v, "diffuseTransmissionTexture")),
        ),
        (
            "diffuseTransmissionColor",
            diffuse_transmission_ext
                .and_then(|v| tex_coord_idx(v, "diffuseTransmissionColorTexture")),
        ),
    ] {
        if let Some(n) = tex_coord
            && n > 1
        {
            geometry_report_lines.push(format!(
                "{label}: {slot} texture uses TEXCOORD_{n} — only UV sets 0 and 1 are supported; it falls back to UV0"
            ));
        }
    }

    let emissive = m.emissive_factor();
    let emissive_strength = m.emissive_strength().unwrap_or(1.0);
    // GLB_XFAIL_BURNDOWN_DESIGN.md D3 (BUG-164): one lookup per map
    // family, keyed off the same texture index each map's own
    // field above already resolved — `None` (map unwired) reads
    // `GltfSamplerInfo::default()` inside the helper.
    let base_color_sampler = sampler_info_for(document, base_color_texture);
    let normal_sampler = sampler_info_for(document, normal_texture);
    let mr_sampler = sampler_info_for(document, mr_texture_index);
    let occlusion_sampler = sampler_info_for(document, occlusion_texture);
    let emissive_sampler = sampler_info_for(document, emissive_texture);
    let extension_maps = [
        extension_map_info(document, sheen_ext, "sheenColorTexture"),
        extension_map_info(document, sheen_ext, "sheenRoughnessTexture"),
        extension_map_info(document, iridescence_ext, "iridescenceTexture"),
        extension_map_info(document, iridescence_ext, "iridescenceThicknessTexture"),
        extension_map_info(document, anisotropy_ext, "anisotropyTexture"),
        extension_map_info(document, clearcoat_ext, "clearcoatTexture"),
        extension_map_info(document, clearcoat_ext, "clearcoatRoughnessTexture"),
        extension_map_info(document, clearcoat_ext, "clearcoatNormalTexture"),
        if spec_gloss_present {
            MaterialMapInfo::default()
        } else {
            extension_map_info(
                document,
                m.extension_value("KHR_materials_specular"),
                "specularTexture",
            )
        },
        if spec_gloss_present {
            spec_gloss_color_map.unwrap_or_default()
        } else {
            extension_map_info(
                document,
                m.extension_value("KHR_materials_specular"),
                "specularColorTexture",
            )
        },
        extension_map_info(
            document,
            m.extension_value("KHR_materials_transmission"),
            "transmissionTexture",
        ),
        extension_map_info(
            document,
            m.extension_value("KHR_materials_volume"),
            "thicknessTexture",
        ),
        extension_map_info(
            document,
            diffuse_transmission_ext,
            "diffuseTransmissionTexture",
        ),
        extension_map_info(
            document,
            diffuse_transmission_ext,
            "diffuseTransmissionColorTexture",
        ),
    ];

    GltfMaterialInfo {
        base_color_factor,
        metallic,
        roughness,
        emissive,
        alpha_mask,
        alpha_cutoff: m.alpha_cutoff().unwrap_or(0.5),
        base_color_texture,
        normal_texture,
        normal_scale: m.normal_texture().map(|t| t.scale()).unwrap_or(1.0),
        mr_texture: mr_texture_index,
        occlusion_texture,
        occlusion_strength: m.occlusion_texture().map(|t| t.strength()).unwrap_or(1.0),
        emissive_texture,
        emissive_strength,
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
        core_tex_coords,
            transmission_factor: m
            .transmission()
            .map(|t| t.transmission_factor())
            .unwrap_or(0.0),
        transmission_texture: m
            .transmission()
            .and_then(|t| t.transmission_texture())
            .map(|t| t.texture().index() as u32),
        diffuse_transmission_factor,
        diffuse_transmission_color,
        diffuse_transmission_texture,
        diffuse_transmission_color_texture,
        clearcoat_factor,
        clearcoat_roughness_factor,
        clearcoat_normal_scale,
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
        base_color_sampler,
        normal_sampler,
        mr_sampler,
        occlusion_sampler,
        emissive_sampler,
        extension_maps,
    }
}

/// The file at `path` as the scene compiler reads it.
#[cfg(test)]
fn test_model(path: &std::path::Path) -> std::rc::Rc<crate::node_graph::gltf_import::model_file::ModelFile> {
    crate::node_graph::gltf_import::model_file::model_file(&path.to_string_lossy())
        .unwrap_or_else(|| panic!("{} reads as a model", path.display()))
}

/// The glTF material named `name` in the file at `path`, as the compiler
/// reads it, and its index.
#[cfg(test)]
fn test_material(path: &std::path::Path, name: &str) -> (u32, GltfMaterialInfo) {
    let (document, _) = parse_document_and_buffers(path).expect("parse model");
    let index = document
        .materials()
        .position(|m| m.name() == Some(name))
        .unwrap_or_else(|| panic!("material {name} in {}", path.display()));
    (index as u32, test_model(path).materials[index].clone())
}

#[cfg(test)]
mod animation_tests {
    use super::*;
    use crate::node_graph::gltf_import::model_file::Rig;

    // Parallel tests in this module can create and remove synthetic GLBs at
    // the same clock tick. The process-local counter keeps each path unique.
    static SYNTHETIC_COLOR0_COUNTER: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(0);

    fn khronos_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos")
    }

    fn hostile_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf/hostile")
    }

    /// IMPORT_ANYTHING_WAVE_DESIGN.md W2 (BUG-187's STEP half): before this
    /// lane, `step_interp.glb`'s translation channel was dropped with a
    /// report line (`gltf_load.rs`'s old LINEAR-only gate). Asserts it now
    /// parses into a real track carrying `GltfInterp::Step`, never
    /// approximated as LINEAR.
    #[test]
    fn step_interp_fixture_parses_as_step_not_dropped() {        let path = hostile_dir().join("step_interp.glb");
        assert!(path.exists(), "step_interp.glb fixture missing at {}", path.display());
        let (document, buffers, _images, _image_report_lines) = import_glb(&path).expect("parse step_interp.glb");
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
        let (document, buffers, _images, _image_report_lines) =
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
    /// `resolve_object_animation`'s ancestor-chain walk identifies the
    /// split channels, while the node-slot palette now carries the final
    /// hierarchy transform because one channel lives on an ancestor.
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
        let (document, buffers, _, _) = import_glb(&path).unwrap();
        let animations = parse_animations(&document, &buffers);
        assert_eq!(animations.len(), 1, "one animation clip");
        assert!(
            animations[0].skipped_channels.is_empty(),
            "BoxAnimated's two channels are both LINEAR — nothing should be skipped: {:?}",
            animations[0].skipped_channels
        );

        let model = test_model(&path);
        let (inner, _) = test_material(&path, "inner");
        let (outer, _) = test_material(&path, "outer");
        let rig_of = |material: u32| {
            model
                .nodes
                .values()
                .find(|facts| facts.parts.iter().any(|part| part.material == Some(material)))
                .and_then(|facts| facts.rig.clone())
        };
        // The ancestor's animation drives the inner box through its node
        // chain as a one-slot palette.
        let Some(Rig::Chain { node_index, .. }) = rig_of(inner) else {
            panic!("inner part resolves the hierarchy palette");
        };
        // The palette needs original local vertices. Baking the ancestor bind
        // transform here would apply it twice when the animation is sampled.
        let node = document.nodes().nth(node_index as usize).unwrap();
        let mesh = node.mesh().unwrap();
        let primitive = mesh.primitives().find(|p| p.material().index() == Some(inner as usize)).unwrap();
        let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()]));
        let source: Vec<_> = reader.read_positions().unwrap().collect();
        let first_index = reader.read_indices().unwrap().into_u32().next().unwrap() as usize;
        let node_path = model.nodes.iter().find(|(_, facts)| facts.parts.iter().any(|p| p.material == Some(inner))).unwrap().0;
        let (vertices, joints, weights) = load_gltf_skinned_mesh(&path, inner, node_path).unwrap();
        assert_eq!(vertices[0].position, source[first_index]);
        assert_eq!(joints[0], [0.0; 4]);
        assert_eq!(weights[0], [1.0, 0.0, 0.0, 0.0]);

        assert!(rig_of(outer).is_none(), "outer_box is static in this fixture");
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
        let (document, buffers, _images, _image_report_lines) = import_glb(&path).expect("parse InterpolationTest.glb");
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

    /// BUG-5mma: minimal single-triangle `.glb` with an explicit COLOR_0
    /// accessor (VEC4 FLOAT, one value per vertex), so the fold/varies
    /// logic can be proven directly rather than depending on a specific
    /// Khronos fixture's exact authored colors. Same hand-rolled
    /// binary-container shape as
    /// `gltf_import::tests::write_synthetic_multimaterial_glb`.
    fn write_synthetic_color0_glb(
        base_color_factor: [f32; 4],
        vertex_colors: [[f32; 4]; 3],
    ) -> std::path::PathBuf {
        let tri: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let mut bin: Vec<u8> = Vec::new();
        for v in &tri {
            for c in v {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        let color_byte_offset = bin.len();
        for c in &vertex_colors {
            for ch in c {
                bin.extend_from_slice(&ch.to_le_bytes());
            }
        }

        let doc = serde_json::json!({
            "asset": { "version": "2.0" },
            "scene": 0,
            "scenes": [{ "nodes": [0] }],
            "nodes": [{ "mesh": 0 }],
            "meshes": [{ "primitives": [{
                "attributes": { "POSITION": 0, "COLOR_0": 1 },
                "material": 0,
            }] }],
            "accessors": [
                {
                    "bufferView": 0,
                    "componentType": 5126, // FLOAT
                    "count": 3,
                    "type": "VEC3",
                    "min": [0.0, 0.0, 0.0],
                    "max": [1.0, 1.0, 0.0],
                },
                {
                    "bufferView": 1,
                    "componentType": 5126, // FLOAT
                    "count": 3,
                    "type": "VEC4",
                },
            ],
            "bufferViews": [
                { "buffer": 0, "byteOffset": 0, "byteLength": 36 },
                { "buffer": 0, "byteOffset": color_byte_offset, "byteLength": 48 },
            ],
            "materials": [
                {
                    "name": "ColoredTri",
                    "pbrMetallicRoughness": { "baseColorFactor": base_color_factor },
                },
            ],
            "buffers": [{ "byteLength": bin.len() }],
        });
        let json_bytes = serde_json::to_vec(&doc).expect("serialize synthetic glTF JSON");

        // GLB container: header + JSON chunk (space-padded to 4 bytes) + BIN
        // chunk (zero-padded to 4 bytes). Chunk type magics per the Binary
        // glTF spec: 0x4E4F534A = "JSON", 0x004E4942 = "BIN\0".
        let mut json_padded = json_bytes;
        while !json_padded.len().is_multiple_of(4) {
            json_padded.push(b' ');
        }
        let mut bin_padded = bin;
        while !bin_padded.len().is_multiple_of(4) {
            bin_padded.push(0);
        }
        let total_len = 12 + 8 + json_padded.len() + 8 + bin_padded.len();

        let mut glb = Vec::with_capacity(total_len);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total_len as u32).to_le_bytes());
        glb.extend_from_slice(&(json_padded.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json_padded);
        glb.extend_from_slice(&(bin_padded.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&bin_padded);

        let path = std::env::temp_dir().join(format!(
            "manifold_synthetic_color0_{}_{}.glb",
            std::process::id(),
            SYNTHETIC_COLOR0_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&path, &glb).expect("write synthetic glb to temp dir");
        path
    }

    /// Two local-space triangles share one material and sit below a common
    /// parent. The parent translates and rotates at t=1, while each child has
    /// its own bind translation. This exercises the same source/palette shape
    /// as a multi-node rigid object without depending on a checked-in asset.
    fn write_synthetic_common_parent_animation_glb() -> std::path::PathBuf {
        let mut bin = Vec::new();
        let push_f32 = |bin: &mut Vec<u8>, value: f32| bin.extend_from_slice(&value.to_le_bytes());
        for position in [[0.0, 0.0, 0.0], [0.5, 0.0, 0.0], [0.0, 0.5, 0.0]] {
            for component in position {
                push_f32(&mut bin, component);
            }
        }
        let child_two_offset = bin.len();
        for position in [[0.0, 0.0, 0.0], [-0.5, 0.0, 0.0], [0.0, -0.5, 0.0]] {
            for component in position {
                push_f32(&mut bin, component);
            }
        }
        let time_offset = bin.len();
        for time in [0.0, 1.0] {
            push_f32(&mut bin, time);
        }
        let translation_offset = bin.len();
        for translation in [[0.0, 0.0, 0.0], [3.0, 2.0, 0.0]] {
            for component in translation {
                push_f32(&mut bin, component);
            }
        }
        let rotation_offset = bin.len();
        for rotation in [[0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.70710677, 0.70710677]] {
            for component in rotation {
                push_f32(&mut bin, component);
            }
        }
        let doc = serde_json::json!({
            "asset": { "version": "2.0" },
            "scene": 0,
            "scenes": [{ "nodes": [0] }],
            "nodes": [
                { "children": [1, 2] },
                { "mesh": 0, "translation": [1.0, 0.0, 0.0] },
                { "mesh": 1, "translation": [-1.0, 0.0, 0.0] }
            ],
            "meshes": [
                { "primitives": [{ "attributes": { "POSITION": 0 }, "material": 0 }] },
                { "primitives": [{ "attributes": { "POSITION": 1 }, "material": 0 }] }
            ],
            "materials": [{ "name": "Shared" }],
            "accessors": [
                {
                    "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
                    "min": [0.0, 0.0, 0.0], "max": [0.5, 0.5, 0.0]
                },
                {
                    "bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3",
                    "min": [-0.5, -0.5, 0.0], "max": [0.0, 0.0, 0.0]
                },
                { "bufferView": 2, "componentType": 5126, "count": 2, "type": "SCALAR" },
                { "bufferView": 3, "componentType": 5126, "count": 2, "type": "VEC3" },
                { "bufferView": 4, "componentType": 5126, "count": 2, "type": "VEC4" }
            ],
            "bufferViews": [
                { "buffer": 0, "byteOffset": 0, "byteLength": 36 },
                { "buffer": 0, "byteOffset": child_two_offset, "byteLength": 36 },
                { "buffer": 0, "byteOffset": time_offset, "byteLength": 8 },
                { "buffer": 0, "byteOffset": translation_offset, "byteLength": 24 },
                { "buffer": 0, "byteOffset": rotation_offset, "byteLength": 32 }
            ],
            "animations": [{
                "samplers": [
                    { "input": 2, "output": 3 },
                    { "input": 2, "output": 4 }
                ],
                "channels": [
                    { "sampler": 0, "target": { "node": 0, "path": "translation" } },
                    { "sampler": 1, "target": { "node": 0, "path": "rotation" } }
                ]
            }],
            "buffers": [{ "byteLength": bin.len() }]
        });
        let json_bytes = serde_json::to_vec(&doc).expect("serialize synthetic hierarchy glTF JSON");
        let mut json_padded = json_bytes;
        while !json_padded.len().is_multiple_of(4) {
            json_padded.push(b' ');
        }
        let mut bin_padded = bin;
        while !bin_padded.len().is_multiple_of(4) {
            bin_padded.push(0);
        }
        let total_len = 12 + 8 + json_padded.len() + 8 + bin_padded.len();
        let mut glb = Vec::with_capacity(total_len);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total_len as u32).to_le_bytes());
        glb.extend_from_slice(&(json_padded.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json_padded);
        glb.extend_from_slice(&(bin_padded.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&bin_padded);
        let path = std::env::temp_dir().join(format!(
            "manifold_synthetic_common_parent_{}_{}.glb",
            std::process::id(),
            SYNTHETIC_COLOR0_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&path, &glb).expect("write synthetic hierarchy glb to temp dir");
        path
    }

    #[test]
    fn synthetic_common_parent_material_uses_palette_and_moves_child_vertices() {
        let path = write_synthetic_common_parent_animation_glb();
        let model = test_model(&path);
        let mut chains: Vec<u32> = model
            .nodes
            .values()
            .filter_map(|facts| match facts.rig {
                Some(Rig::Chain { node_index, .. }) => Some(node_index),
                _ => None,
            })
            .collect();
        chains.sort_unstable();
        assert_eq!(chains, [1, 2], "both child mesh nodes move by their node chain");

        let (document, buffers, _, _) = import_glb(&path).expect("import synthetic hierarchy glb");
        let animations = parse_animations(&document, &buffers);
        let parent = animations[0]
            .nodes
            .iter()
            .find(|node| node.node_index == 0)
            .expect("common parent animation");
        let parent_translation = parent.translation.as_ref().unwrap().values[1];
        let parent_rotation = parent.rotation.as_ref().unwrap().values[1];
        let parent_world = mat4_from_trs(parent_translation, parent_rotation, [1.0; 3]);
        let (vertices, joints, weights) = load_gltf_skinned_mesh(&path, 0, "").expect("load palette vertices");
        assert_eq!(vertices.len(), 6, "two triangle contributors stay in local space");
        assert_eq!(joints[0], [0.0; 4]);
        assert_eq!(joints[3], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(weights[0], [1.0, 0.0, 0.0, 0.0]);
        assert_eq!(weights[3], [1.0, 0.0, 0.0, 0.0]);

        let child_world = |node_index: usize| {
            let node = document.nodes().nth(node_index).unwrap();
            let (translation, rotation, scale) = node.transform().decomposed();
            mat4_mul(&parent_world, &mat4_from_trs(translation, rotation, scale))
        };
        let child_one_position = mat4_transform_point(&child_world(1), vertices[0].position);
        let child_two_position = mat4_transform_point(&child_world(2), vertices[3].position);
        assert_eq!(child_one_position, [3.0, 3.0, 0.0]);
        assert_eq!(child_two_position, [3.0, 1.0, 0.0]);
        std::fs::remove_file(&path).ok();
    }

    /// BUG-5mma core case: every vertex on the (sole) primitive sharing this
    /// material agrees on one constant COLOR_0. The material factor remains
    /// untouched while the authored color arrives on every loaded vertex.
    #[test]
    fn constant_vertex_color_preserves_factor_and_loads_color() {
        let base = [0.8, 0.6, 0.4, 1.0];
        let tint = [0.5, 1.0, 0.25, 0.5];
        let path = write_synthetic_color0_glb(base, [tint, tint, tint]);
        let m = &test_model(&path).materials[0];
        assert_eq!(m.base_color_factor, base, "COLOR_0 must not be folded into the material factor");
        let verts = load_gltf_mesh(&path, GltfMeshSelector::WholeScene)
            .expect("load synthetic constant-color vertices");
        assert_eq!(verts.len(), 3);
        assert!(verts.iter().all(|vertex| vertex.color == tint));
        std::fs::remove_file(&path).expect("remove synthetic constant-color glb");
    }

    /// BUG-5mma varying case: three distinct authored colors remain distinct
    /// in the loaded vertices, while the material factor stays untouched.
    #[test]
    fn varying_vertex_color_preserves_factor_and_loads_each_vertex() {
        let base = [0.8, 0.6, 0.4, 1.0];
        let path = write_synthetic_color0_glb(
            base,
            [[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]],
        );
        let m = &test_model(&path).materials[0];
        assert_eq!(m.base_color_factor, base, "varying color must leave base_color_factor untouched");
        let verts = load_gltf_mesh(&path, GltfMeshSelector::WholeScene)
            .expect("load synthetic varying-color vertices");
        assert_eq!(verts.len(), 3);
        let colors: Vec<[f32; 4]> = verts.iter().map(|vertex| vertex.color).collect();
        assert_eq!(colors, vec![[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]]);
        std::fs::remove_file(&path).expect("remove synthetic varying-color glb");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::gltf_import::model_file::Rig;

    /// One self-contained triangle exercises every loader variant under test:
    /// a reflected nonuniform node transform, authored tangent frame, UV1,
    /// one skin joint, and one POSITION morph target.
    fn write_synthetic_loader_proof_glb() -> std::path::PathBuf {
        fn append_aligned(bin: &mut Vec<u8>, bytes: &[u8]) -> (usize, usize) {
            while !bin.len().is_multiple_of(4) {
                bin.push(0);
            }
            let offset = bin.len();
            bin.extend_from_slice(bytes);
            (offset, bytes.len())
        }
        fn f32_bytes<const N: usize>(rows: &[[f32; N]]) -> Vec<u8> {
            rows.iter()
                .flat_map(|row| row.iter().flat_map(|value| value.to_le_bytes()))
                .collect()
        }

        let positions = [[0.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 1.0, 1.0]];
        let normal = [-0.57735026, -0.57735026, 0.57735026];
        let tangent = [0.70710677, -0.70710677, 0.0, 1.0];
        let normals = [normal; 3];
        let tangents = [tangent; 3];
        let uv0 = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
        let uv1 = [[0.2, 0.3], [0.4, 0.7], [0.8, 0.9]];
        let joints = [0u8; 12];
        let weights = [[1.0, 0.0, 0.0, 0.0]; 3];
        let indices = [0u16, 1, 2];
        let morph_positions = [[0.1, 0.2, 0.3], [0.0, 0.1, 0.0], [-0.1, 0.0, 0.2]];
        let inverse_bind = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];

        let mut bin = Vec::new();
        let (positions_offset, positions_len) = append_aligned(&mut bin, &f32_bytes(&positions));
        let (normals_offset, normals_len) = append_aligned(&mut bin, &f32_bytes(&normals));
        let (tangents_offset, tangents_len) = append_aligned(&mut bin, &f32_bytes(&tangents));
        let (uv0_offset, uv0_len) = append_aligned(&mut bin, &f32_bytes(&uv0));
        let (uv1_offset, uv1_len) = append_aligned(&mut bin, &f32_bytes(&uv1));
        let (joints_offset, joints_len) = append_aligned(&mut bin, &joints);
        let (weights_offset, weights_len) = append_aligned(&mut bin, &f32_bytes(&weights));
        let index_bytes: Vec<u8> = indices.iter().flat_map(|index| index.to_le_bytes()).collect();
        let (indices_offset, indices_len) = append_aligned(&mut bin, &index_bytes);
        let (morph_offset, morph_len) = append_aligned(&mut bin, &f32_bytes(&morph_positions));
        let (inverse_bind_offset, inverse_bind_len) =
            append_aligned(&mut bin, &f32_bytes(&inverse_bind));

        let doc = serde_json::json!({
            "asset": { "version": "2.0" },
            "scene": 0,
            "scenes": [{ "nodes": [0, 1] }],
            "nodes": [
                { "mesh": 0, "skin": 0, "scale": [-2.0, 3.0, 4.0] },
                { "translation": [0.0, 0.0, 0.0] }
            ],
            "meshes": [{
                "weights": [0.0],
                "primitives": [{
                    "attributes": {
                        "POSITION": 0, "NORMAL": 1, "TANGENT": 2,
                        "TEXCOORD_0": 3, "TEXCOORD_1": 4,
                        "JOINTS_0": 5, "WEIGHTS_0": 6
                    },
                    "indices": 7,
                    "material": 0,
                    "targets": [{ "POSITION": 8 }]
                }]
            }],
            "accessors": [
                { "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 1.0] },
                { "bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3" },
                { "bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC4" },
                { "bufferView": 3, "componentType": 5126, "count": 3, "type": "VEC2" },
                { "bufferView": 4, "componentType": 5126, "count": 3, "type": "VEC2" },
                { "bufferView": 5, "componentType": 5121, "count": 3, "type": "VEC4" },
                { "bufferView": 6, "componentType": 5126, "count": 3, "type": "VEC4" },
                { "bufferView": 7, "componentType": 5123, "count": 3, "type": "SCALAR" },
                { "bufferView": 8, "componentType": 5126, "count": 3, "type": "VEC3", "min": [-0.1, 0.0, 0.0], "max": [0.1, 0.2, 0.3] },
                { "bufferView": 9, "componentType": 5126, "count": 1, "type": "MAT4" }
            ],
            "bufferViews": [
                { "buffer": 0, "byteOffset": positions_offset, "byteLength": positions_len },
                { "buffer": 0, "byteOffset": normals_offset, "byteLength": normals_len },
                { "buffer": 0, "byteOffset": tangents_offset, "byteLength": tangents_len },
                { "buffer": 0, "byteOffset": uv0_offset, "byteLength": uv0_len },
                { "buffer": 0, "byteOffset": uv1_offset, "byteLength": uv1_len },
                { "buffer": 0, "byteOffset": joints_offset, "byteLength": joints_len },
                { "buffer": 0, "byteOffset": weights_offset, "byteLength": weights_len },
                { "buffer": 0, "byteOffset": indices_offset, "byteLength": indices_len },
                { "buffer": 0, "byteOffset": morph_offset, "byteLength": morph_len },
                { "buffer": 0, "byteOffset": inverse_bind_offset, "byteLength": inverse_bind_len }
            ],
            "materials": [{ "name": "LoaderProofMaterial", "pbrMetallicRoughness": {} }],
            "skins": [{ "joints": [1], "inverseBindMatrices": 9 }],
            "buffers": [{ "byteLength": bin.len() }]
        });
        let mut json_bytes = serde_json::to_vec(&doc).expect("serialize loader proof JSON");
        while !json_bytes.len().is_multiple_of(4) {
            json_bytes.push(b' ');
        }
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let total_len = 12 + 8 + json_bytes.len() + 8 + bin.len();
        let mut glb = Vec::with_capacity(total_len);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total_len as u32).to_le_bytes());
        glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json_bytes);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&bin);

        let path = std::env::temp_dir().join(format!(
            "manifold_loader_proof_{}_{}.glb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos()
        ));
        std::fs::write(&path, glb).expect("write loader proof GLB");
        path
    }

    /// The multi-node shared-skin merge, against the real fixture that
    /// motivated it: surveillance_cam.glb's material 0 is fed by THREE
    /// skinned mesh nodes (arm/cam/joint), all bound to the same 4-joint
    /// skin — the Sketchfab FBX-convert shape. Pre-fix this object
    /// imported unskinned (the whole model frozen; only the 4-vertex glass
    /// pane animated). The resolve must now produce skin 0, and the runtime
    /// load must concatenate all three nodes' geometry with every JOINTS_0
    /// index still inside the one shared 4-slot palette.
    #[test]
    fn multi_node_shared_skin_material_resolves_and_concatenates() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/surveillance_cam.glb");
        // Skip-if-missing (azalea convention): the fixture is a local
        // look-test drop, deliberately untracked per .gitignore's
        // licensing-vet note.
        if !path.exists() {
            println!("multi_node_shared_skin_material_resolves_and_concatenates: fixture not found, skipping");
            return;
        }

        let model = test_model(&path);
        let skins_of = |material: u32| -> Vec<(u32, u32)> {
            model
                .nodes
                .values()
                .filter(|facts| facts.parts.iter().any(|part| part.material == Some(material)))
                .filter_map(|facts| match facts.rig {
                    Some(Rig::Skin { skin_index, joint_count, .. }) => Some((skin_index, joint_count)),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(skins_of(0), [(0, 4); 3], "material 0's three contributors share 4-joint skin 0");

        let (verts, joints, weights) = load_gltf_skinned_mesh(&path, 0, "")
            .unwrap_or_else(|e| panic!("load_gltf_skinned_mesh({}): {e}", path.display()));
        // arm (1260) + cam (2748) + joint (96) triangle indices, one
        // emitted vertex per index entry.
        assert_eq!(verts.len(), 4104, "all three contributing nodes' geometry, concatenated");
        assert_eq!(joints.len(), verts.len(), "coincident joints");
        assert_eq!(weights.len(), verts.len(), "coincident weights");
        assert!(
            joints.iter().all(|j| j.iter().all(|&x| (0.0..4.0).contains(&x))),
            "every JOINTS_0 entry indexes the shared 4-slot palette"
        );

        // The single-node glass material (4 verts, one contributing node)
        // must keep resolving exactly as before the merge.
        assert_eq!(skins_of(1).len(), 1, "single-node skinned material still resolves");
    }

    /// The single-node path through the same collect-all loop the merge
    /// introduced: kuma_heavy_robot's two materials are each fed by exactly
    /// one skinned node (12-joint skin) — vertex counts must match the
    /// file's index counts exactly (no double-collection, no loss).
    #[test]
    fn single_node_skinned_materials_unchanged_by_collect_all() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/kuma_heavy_robot_r-9000s.glb");
        if !path.exists() {
            println!("single_node_skinned_materials_unchanged_by_collect_all: fixture not found, skipping");
            return;
        }
        let model = test_model(&path);
        for (material_index, expected_verts) in [(0u32, 34014usize), (1u32, 34017usize)] {
            let (node_path, facts) = model
                .nodes
                .iter()
                .find(|(_, facts)| facts.parts.iter().any(|part| part.material == Some(material_index)))
                .expect("robot material has geometry");
            let Some(Rig::Skin { joint_count, .. }) = facts.rig else {
                panic!("robot material resolves skinned");
            };
            assert_eq!(joint_count, 12);
            let (verts, _, _) = load_gltf_skinned_mesh(&path, material_index, node_path)
                .unwrap_or_else(|e| panic!("load_gltf_skinned_mesh({material_index}): {e}"));
            assert_eq!(verts.len(), expected_verts, "material {material_index} vertex count");
        }
    }
    /// Decisive seam-split for the robot's black-body report: the decoded
    /// baseColor image (texture 0 → image 0, a 4096² JPEG) must arrive
    /// bright CPU-side. If this fails, the vendored decoder is the bug;
    /// if it passes, the black enters GPU-side (upload/resample/sample).
    #[test]
    fn robot_basecolor_jpeg_decodes_bright() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/kuma_heavy_robot_r-9000s.glb");
        if !path.exists() {
            println!("robot_basecolor_jpeg_decodes_bright: fixture not found, skipping");
            return;
        }
        let (_doc, _buffers, images, report) =
            import_glb(&path).unwrap_or_else(|e| panic!("import_glb: {e}"));
        assert!(report.is_empty(), "no decode-failure substitutions expected: {report:?}");
        let img = &images[0];
        assert!(img.width >= 1024 && img.height >= 1024, "full-res decode, got {}x{}", img.width, img.height);
        let bytes_per_px = img.pixels.len() / (img.width as usize * img.height as usize);
        let mean: f64 = img
            .pixels
            .chunks_exact(bytes_per_px)
            .map(|px| px.iter().take(3).map(|&c| c as f64).sum::<f64>() / 3.0)
            .sum::<f64>()
            / (img.width as usize * img.height as usize) as f64;
        assert!(mean > 40.0, "the KUMA atlas is mostly bright orange/white — mean {mean:.1} says decode is dark");
    }

    /// BUG-wfxe red proof: `NormalTangentMirrorTest.glb` (Khronos'
    /// conformance asset for exactly this path — authored tangents with
    /// BOTH handednesses, w = +1 and w = -1 halves) must import its
    /// TANGENT accessor end to end — pre-fix every vertex carried the
    /// `[0,0,0,0]` sentinel. Asserts real tangents (unit-ish xyz, w = ±1)
    /// and that BOTH signs survive (the mirrored half is the case the
    /// derived cotangent frame gets wrong).
    #[test]
    fn normal_tangent_mirror_fixture_imports_authored_tangents() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/NormalTangentMirrorTest.glb");
        assert!(path.exists(), "NormalTangentMirrorTest.glb missing at {}", path.display());
        let verts = load_gltf_mesh(&path, GltfMeshSelector::WholeScene)
            .unwrap_or_else(|e| panic!("load_gltf_mesh({}): {e}", path.display()));
        assert!(!verts.is_empty());
        let authored = verts.iter().filter(|v| v.tangent[3] != 0.0).count();
        assert!(
            authored as f32 > verts.len() as f32 * 0.9,
            "NormalTangentMirrorTest ships TANGENT — expected >90% authored tangents, \
             got {authored}/{} (the import is dropping the accessor)",
            verts.len()
        );
        let pos = verts.iter().filter(|v| (v.tangent[3] - 1.0).abs() < 1e-6).count();
        let neg = verts.iter().filter(|v| (v.tangent[3] + 1.0).abs() < 1e-6).count();
        assert!(
            pos > 0 && neg > 0,
            "both bitangent signs must survive (mirror test asset): +1={pos}, -1={neg}"
        );
        let sample = verts.iter().find(|v| v.tangent[3] != 0.0).unwrap();
        let len = (sample.tangent[0].powi(2) + sample.tangent[1].powi(2) + sample.tangent[2].powi(2))
            .sqrt();
        assert!(
            (len - 1.0).abs() < 0.05,
            "authored tangent direction must be ~unit (world-transformed), got |t|={len}"
        );
    }

    #[test]
    fn texture_transform_multi_preserves_extension_map_metadata() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/TextureTransformMultiTest.glb");
        assert!(path.exists(), "TextureTransformMultiTest.glb missing at {}", path.display());
        let materials = &test_model(&path).materials;

        // Materials 20 and 23 are the fixture's clearcoat transform cases;
        // their authored maps exercise two extension families independently.
        let clearcoat = &materials[20].extension_maps[5];
        let clearcoat_roughness = &materials[23].extension_maps[6];
        assert_ne!(clearcoat.uv_transform, IDENTITY_UV_TRANSFORM);
        assert_ne!(clearcoat_roughness.uv_transform, IDENTITY_UV_TRANSFORM);
        assert_eq!(clearcoat.tex_coord, 0);
        assert_eq!(clearcoat_roughness.tex_coord, 0);
        assert_eq!(clearcoat.sampler.wrap_u, manifold_gpu::GpuAddressMode::Repeat);
        assert_eq!(clearcoat_roughness.sampler.min_filter, manifold_gpu::GpuFilterMode::Linear);

        let normal = &materials[9];
        let occlusion = &materials[15];
        assert_ne!(normal.normal_uv_transform, IDENTITY_UV_TRANSFORM);
        assert_eq!(normal.core_tex_coords[1], 1);
        assert_ne!(occlusion.occlusion_uv_transform, IDENTITY_UV_TRANSFORM);
        assert_eq!(occlusion.core_tex_coords[3], 1);
    }

    #[test]
    fn texture_settings_preserves_sampler_wrap_and_mip_choice() {
        let settings = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/TextureSettingsTest.glb");
        let settings_materials = &test_model(&settings).materials;
        assert_eq!(settings_materials[0].base_color_sampler.mip_filter, Some(GltfFilterMode::Linear));
        assert_eq!(settings_materials[0].base_color_sampler.wrap_v, GltfWrapMode::MirrorRepeat);
        assert_eq!(settings_materials[1].base_color_sampler.wrap_u, GltfWrapMode::MirrorRepeat);

        let nearest_mip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/MaterialsVariantsShoe.glb");
        let nearest_materials = &test_model(&nearest_mip).materials;
        assert_eq!(nearest_materials[0].mr_sampler.mip_filter, Some(GltfFilterMode::Nearest));

        let no_mip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/SpecularSilkPouf.glb");
        let no_mip_materials = &test_model(&no_mip).materials;
        assert_eq!(no_mip_materials[0].occlusion_sampler.mip_filter, None);
    }

    #[test]
    fn reflected_nonuniform_node_transform_preserves_tangent_direction_and_handedness() {
        let path = write_synthetic_loader_proof_glb();
        let verts = load_gltf_mesh(&path, GltfMeshSelector::WholeScene)
            .expect("load synthetic reflected tangent fixture");
        assert_eq!(verts.len(), 3, "synthetic triangle must produce exactly three vertices");

        let local_normal = [-0.57735026, -0.57735026, 0.57735026];
        let local_tangent = [0.70710677, -0.70710677, 0.0];
        let world_linear = [[-2.0, 0.0, 0.0], [0.0, 3.0, 0.0], [0.0, 0.0, 4.0]];
        let normal_matrix = [[-0.5, 0.0, 0.0], [0.0, 1.0 / 3.0, 0.0], [0.0, 0.0, 0.25]];
        let expected_tangent = normalize3(mat3_mul_vec3(world_linear, local_tangent));
        let expected_normal = normalize3(mat3_mul_vec3(normal_matrix, local_normal));
        for vertex in &verts {
            let tangent = [vertex.tangent[0], vertex.tangent[1], vertex.tangent[2]];
            let normal = vertex.normal;
            assert!(
                tangent
                    .iter()
                    .zip(expected_tangent)
                    .map(|(actual, expected)| *actual * expected)
                    .sum::<f32>()
                    > 0.999,
                "tangent direction must follow the transformed authored tangent: {tangent:?}"
            );
            assert!(
                normal
                    .iter()
                    .zip(expected_normal)
                    .map(|(actual, expected)| *actual * expected)
                    .sum::<f32>()
                    > 0.999,
                "normal must follow the inverse-transpose transform: {normal:?}"
            );
            let orthogonality = normal.iter().zip(tangent).map(|(n, t)| *n * t).sum::<f32>();
            assert!(orthogonality.abs() < 1e-5, "normal/tangent lost orthogonality: {orthogonality}");
            assert_eq!(vertex.tangent[3], -1.0, "reflection must flip authored handedness");
        }
        std::fs::remove_file(&path).expect("remove synthetic tangent fixture");
    }

    #[test]
    fn skinned_and_morph_loaders_preserve_uv1() {
        let path = write_synthetic_loader_proof_glb();
        let expected_uv1 = [[0.2, 0.3], [0.4, 0.7], [0.8, 0.9]];

        let model = test_model(&path);
        assert_eq!(model.nodes.len(), 1);
        let facts = model.nodes.values().next().unwrap();
        assert!(matches!(facts.rig, Some(Rig::Skin { .. })), "synthetic fixture must resolve a skin");
        assert!(!facts.morphs.is_empty(), "synthetic fixture must resolve morph targets");

        let base = load_gltf_mesh(&path, GltfMeshSelector::WholeScene)
            .expect("load synthetic morph base mesh");
        assert_eq!(base.len(), expected_uv1.len());
        for (vertex, expected) in base.iter().zip(expected_uv1) {
            assert_eq!(vertex._pad2, expected, "base mesh UV1 must survive static loading");
        }

        let (skinned, joints, weights) =
            load_gltf_skinned_mesh(&path, 0, "").expect("load synthetic skinned mesh");
        assert_eq!(skinned.len(), expected_uv1.len());
        assert_eq!(joints.len(), skinned.len());
        assert_eq!(weights.len(), skinned.len());
        for ((vertex, joint), (weight, expected)) in skinned
            .iter()
            .zip(joints)
            .zip(weights.into_iter().zip(expected_uv1))
        {
            assert_eq!(vertex._pad2, expected, "skinned UV1 must survive loading");
            assert_eq!(joint, [0.0, 0.0, 0.0, 0.0]);
            assert_eq!(weight, [1.0, 0.0, 0.0, 0.0]);
        }

        let (deltas, target_count, vertex_count) =
            load_gltf_morph_deltas(&path, 0, false, "").expect("load synthetic morph deltas");
        assert_eq!(target_count, 1);
        assert_eq!(vertex_count, 3);
        assert_eq!(deltas.len(), 3);
        assert!(
            deltas.iter().any(|vertex| vertex.position != [0.0, 0.0, 0.0]),
            "synthetic morph target must produce nonzero position deltas"
        );
        assert!(
            deltas.iter().all(|vertex| vertex._pad2 == [0.0, 0.0]),
            "morph deltas must leave UV1 zero for base-mesh preservation"
        );
        std::fs::remove_file(&path).expect("remove synthetic loader proof fixture");
    }

    /// BUG-wfxe fallback half: a mesh WITHOUT a TANGENT accessor keeps the
    /// zero sentinel, so the shader takes the derived cotangent frame
    /// (pre-BUG-wfxe behavior, byte-identical for procedural meshes).
    #[test]
    fn mesh_without_tangent_keeps_zero_sentinel() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb");
        if !path.exists() {
            println!("mesh_without_tangent_keeps_zero_sentinel: fixture not found, skipping");
            return;
        }
        let verts = load_gltf_mesh(&path, GltfMeshSelector::WholeScene)
            .unwrap_or_else(|e| panic!("load_gltf_mesh({}): {e}", path.display()));
        let authored = verts.iter().filter(|v| v.tangent[3] != 0.0).count();
        assert_eq!(authored, 0, "azalea ships no TANGENT — every vertex must keep the sentinel");
    }

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
        // Known azalea shape: 2 distinct textured materials, no cameras,
        // no default-material geometry. Both materials declare
        // `KHR_materials_unlit`; the parse reads every field genuinely, so
        // `base_color_texture` stays wired as for a lit material.
        let model = test_model(&path);
        let (document, _) = parse_document_and_buffers(&path).expect("parse azalea");
        assert_eq!(parse_cameras(&document, &build_parent_map(&document)).0.len(), 0);
        let mut counts: std::collections::BTreeMap<Option<u32>, u32> = std::collections::BTreeMap::new();
        for part in model.nodes.values().flat_map(|facts| &facts.parts) {
            *counts.entry(part.material).or_default() += part.vertex_count;
        }
        assert_eq!(counts.keys().copied().collect::<Vec<_>>(), [Some(0), Some(1)], "2 materials with geometry");
        for (material, vertex_count) in counts {
            let material_index = material.unwrap();
            let m = &model.materials[material_index as usize];
            assert!(
                m.base_color_texture.is_some(),
                "material {material_index} should carry a base-color texture (parsed genuinely, unremapped)"
            );
            assert!(vertex_count > 0);
            // The Material selector must extract exactly that many verts.
            let verts = load_gltf_mesh(&path, GltfMeshSelector::Material { material_index }).expect("material selector");
            assert_eq!(
                verts.len() as u32,
                vertex_count,
                "Material selector vertex count must match the import's parts for material {material_index}"
            );
            assert_eq!(verts.len() % 3, 0, "triangle list");
        }
    }

    /// BUG-d2qz: `euler_from_fwd_up` inverts `Camera::from_pos_euler`'s own
    /// basis construction — round-trip every non-degenerate angle triple
    /// (pitch away from ±90°, where yaw isn't observable from `fwd` alone,
    /// matches `from_pos_euler`'s own documented pole limits) through
    /// `from_pos_euler` to get `(fwd, up)`, decompose back, and recover the
    /// SAME angles.
    #[test]
    fn euler_from_fwd_up_round_trips_from_pos_euler() {
        use manifold_node_engine::scene::camera::Camera;

        for yaw in [-2.4_f32, -0.7, 0.0, 0.3, 1.1, 2.9] {
            for pitch in [-1.3_f32, -0.5, 0.0, 0.4, 1.3] {
                for roll in [0.0_f32, 0.5, -1.8, 3.0] {
                    let cam = Camera::from_pos_euler([0.0, 0.0, 0.0], yaw, pitch, roll, 0.9, 0.05, 200.0);
                    let (yaw2, pitch2, roll2) = euler_from_fwd_up(cam.fwd, cam.up);
                    let cam2 = Camera::from_pos_euler([0.0, 0.0, 0.0], yaw2, pitch2, roll2, 0.9, 0.05, 200.0);
                    for axis in 0..3 {
                        assert!(
                            (cam.fwd[axis] - cam2.fwd[axis]).abs() < 1e-4,
                            "fwd mismatch at yaw={yaw} pitch={pitch} roll={roll}: {:?} vs {:?}",
                            cam.fwd,
                            cam2.fwd
                        );
                        assert!(
                            (cam.up[axis] - cam2.up[axis]).abs() < 1e-4,
                            "up mismatch at yaw={yaw} pitch={pitch} roll={roll}: {:?} vs {:?}",
                            cam.up,
                            cam2.up
                        );
                    }
                }
            }
        }
    }

    /// BUG-d2qz: Khronos `Duck.glb` (CC-BY 4.0 — attribution in
    /// `tests/fixtures/gltf/README.md`) carries one embedded perspective
    /// camera on a child node under a uniformly-scaled root — the real
    /// shape `parse_cameras` has to resolve (world transform through an
    /// ancestor chain, not just the camera node's own local matrix). Proof
    /// that the resolved pose actually reproduces the glb's authored
    /// direction: rebuild a `Camera` from the parsed yaw/pitch/roll and
    /// check its `fwd`/`up` against the SAME world matrix decomposed
    /// directly — an independent check, not a re-assertion of
    /// `parse_cameras`' own math. (`AntiqueCamera.glb`, despite its name,
    /// carries no embedded glTF camera — it's a model of a camera object,
    /// not a scene camera — so it can't serve as this fixture.)
    #[test]
    fn duck_embedded_camera_resolves_to_authored_world_pose() {
        use manifold_node_engine::scene::camera::Camera;

        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/Duck.glb");
        if !path.exists() {
            println!("duck_embedded_camera_resolves_to_authored_world_pose: fixture not found, skipping");
            return;
        }
        let (document, _buffers, _images, _image_report_lines) = import_glb(&path).expect("parse Duck.glb");
        assert_eq!(document.cameras().count(), 1, "Duck.glb carries exactly one embedded camera");
        let (cameras, camera_report_lines) = parse_cameras(&document, &build_parent_map(&document));
        assert_eq!(cameras.len(), 1, "Duck's one camera is perspective — must resolve");
        assert!(camera_report_lines.is_empty(), "no orthographic camera to skip");

        let parsed = &cameras[0];

        // Independent oracle: re-derive world position/fwd/up straight off
        // the document's node tree (root's uniform scale composed with the
        // camera node's local matrix), bypassing `parse_cameras` entirely.
        let cam_node = document
            .nodes()
            .find(|n| n.camera().is_some())
            .expect("Duck.glb has exactly one camera node");
        let parent_of = build_parent_map(&document);
        let world = static_world_matrix(&document, cam_node.index(), &parent_of);
        let expected_pos = [world[3][0], world[3][1], world[3][2]];
        let expected_fwd = normalize3([-world[2][0], -world[2][1], -world[2][2]]);
        let expected_up = normalize3([world[1][0], world[1][1], world[1][2]]);

        for axis in 0..3 {
            assert!(
                (parsed.pos[axis] - expected_pos[axis]).abs() < 1e-3,
                "pos mismatch: {:?} vs {:?}",
                parsed.pos,
                expected_pos
            );
        }

        let rebuilt =
            Camera::from_pos_euler(parsed.pos, parsed.yaw, parsed.pitch, parsed.roll, parsed.fov_y, parsed.near, parsed.far);
        for axis in 0..3 {
            assert!(
                (rebuilt.fwd[axis] - expected_fwd[axis]).abs() < 1e-3,
                "fwd mismatch: {:?} vs {:?}",
                rebuilt.fwd,
                expected_fwd
            );
            assert!(
                (rebuilt.up[axis] - expected_up[axis]).abs() < 1e-3,
                "up mismatch: {:?} vs {:?}",
                rebuilt.up,
                expected_up
            );
        }

        // FOV/clip planes pass through untouched from the glTF accessor.
        assert!((parsed.fov_y - 0.660_592_56_f32).abs() < 1e-5);
        assert!((parsed.near - 1.0).abs() < 1e-5);
        assert!((parsed.far - 10000.0).abs() < 1e-5);
    }

    /// An unlit-authored material is lit by the scene like any other: every
    /// field (`base_color_factor`, `base_color_texture`, `metallic`,
    /// `roughness`) stays the real parsed glTF value, never remapped.
    /// Khronos's `UnlitTest.glb` carries two unlit, factor-only materials.
    #[test]
    fn unlit_materials_read_genuinely_without_remapping() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/UnlitTest.glb");
        assert!(path.exists(), "UnlitTest.glb fixture missing at {}", path.display());
        let expected_base_color: &[(&str, [f32; 3])] = &[
            ("Orange", [1.0, 0.217_637_64, 0.0]),
            ("Blue", [0.0, 0.217_637_64, 1.0]),
        ];
        for (name, expected) in expected_base_color {
            let (_, m) = test_material(&path, name);
            for (c, expected_c) in expected.iter().enumerate() {
                assert!(
                    (m.base_color_factor[c] - expected_c).abs() < 1e-6,
                    "{name}: base_color_factor[{c}] = {}, expected {} (genuine glTF value, unremapped)",
                    m.base_color_factor[c],
                    expected_c
                );
            }
            assert_eq!(m.base_color_factor[3], 1.0, "{name}: alpha untouched");
            assert_eq!(m.base_color_texture, None, "{name}: UnlitTest.glb has no textures");
            assert_eq!(m.emissive, [0.0, 0.0, 0.0], "{name}: no emissiveFactor declared");
            assert_eq!(m.metallic, 1.0, "{name}: metallic stays the glTF-spec default, unremapped");
            assert_eq!(m.roughness, 1.0, "{name}: roughness stays the glTF-spec default, unremapped");
        }
    }

    /// GLB_XFAIL_BURNDOWN_DESIGN.md D2 (BUG-167) value-level gate: known
    /// spec-gloss factors → expected metal-rough numbers. Full gloss (1.0)
    /// is a mirror — roughness 0.0 (perfectly smooth, `glossiness_factor`'s
    /// own documented meaning); full rough spec-gloss (glossiness 0.0) →
    /// roughness 1.0. The authored RGB factor is recovered as an exact
    /// dielectric F0 tint (`specular_tint = clamp(rgb, 0, 1) / 0.04`), with
    /// unit scalar weight; `metallic` is always the dielectric default 0.0 —
    /// spec-gloss has no metalness channel.
    #[test]
    fn convert_spec_gloss_maps_glossiness_and_specular_factor() {
        let smooth = convert_spec_gloss(1.0, [0.5, 0.5, 0.5]);
        assert_eq!(smooth.roughness, 0.0);
        assert_eq!(smooth.metallic, 0.0);
        assert_eq!(smooth.specular_factor, 1.0);
        for tint in smooth.specular_tint {
            assert!((0.04 * tint - 0.5).abs() < 1e-6);
        }

        let rough = convert_spec_gloss(0.0, [1.0, 1.0, 1.0]);
        assert_eq!(rough.roughness, 1.0);
        assert!((rough.specular_factor - 1.0).abs() < 1e-6);
        for tint in rough.specular_tint {
            assert!((0.04 * tint - 1.0).abs() < 1e-6);
        }

        // Non-uniform RGB specular factors recover each authored F0 channel.
        let tinted = convert_spec_gloss(0.25, [0.2, 0.4, 0.6]);
        assert!((tinted.roughness - 0.75).abs() < 1e-6);
        assert_eq!(tinted.specular_factor, 1.0);
        for (tint, authored) in tinted.specular_tint.into_iter().zip([0.2, 0.4, 0.6]) {
            assert!((0.04 * tint - authored).abs() < 1e-6);
        }

        // Out-of-[0,1] glossiness (spec-illegal, but defensive) still
        // clamps to a valid roughness rather than producing a negative or
        // >1 value that would corrupt `resolve_mr`'s `max(t.g, 0.01)`.
        let over = convert_spec_gloss(1.5, [2.0, 2.0, 2.0]);
        assert_eq!(over.roughness, 0.0);
        assert_eq!(over.specular_factor, 1.0);
        for tint in over.specular_tint {
            assert!((0.04 * tint - 1.0).abs() < 1e-6);
        }

        let clamped = convert_spec_gloss(0.5, [-1.0, 0.25, 2.0]);
        for (tint, authored) in clamped.specular_tint.into_iter().zip([0.0, 0.25, 1.0]) {
            assert!((0.04 * tint - authored).abs() < 1e-6);
        }
    }

    #[test]
    fn spec_gloss_texture_routes_rgb_to_typed_specular_color_metadata() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/SpecGlossVsMetalRough.glb");
        assert!(path.exists(), "SpecGlossVsMetalRough.glb missing at {}", path.display());
        let (_, material) = test_material(&path, "BottleMat_SpecGloss");

        assert_eq!(material.mr_texture, Some(6));
        assert!(material.mr_texture_is_gloss_alpha);
        assert_eq!(material.specular_factor, 1.0);
        assert_eq!(material.specular_color_factor, [25.0, 25.0, 25.0]);
        assert_eq!(material.ior, 1.5);
        assert_eq!(material.specular_texture, None);
        assert_eq!(material.specular_color_texture, Some(6));
        assert_eq!(material.extension_maps[9].uv_transform, IDENTITY_UV_TRANSFORM);
        assert_eq!(material.extension_maps[9].tex_coord, 0);
    }

    #[test]
    fn spec_gloss_texture_preserves_authored_typed_sampler_and_transform() {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/SpecGlossVsMetalRough.glb");
        assert!(source.exists(), "SpecGlossVsMetalRough.glb missing at {}", source.display());
        let bytes = std::fs::read(&source).expect("read spec-gloss fixture");
        let mut offset = 12usize;
        let mut chunks = Vec::new();
        while offset < bytes.len() {
            let length = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            let chunk_type = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
            let start = offset + 8;
            let end = start + length;
            chunks.push((chunk_type, bytes[start..end].to_vec()));
            offset = end;
        }
        let json_chunk = chunks
            .iter_mut()
            .find(|(chunk_type, _)| *chunk_type == 0x4e4f534a)
            .expect("fixture JSON chunk");
        let mut document: serde_json::Value = serde_json::from_slice(&json_chunk.1)
            .expect("parse fixture JSON chunk");
        let texture_info = &mut document["materials"][0]["extensions"]
            ["KHR_materials_pbrSpecularGlossiness"]["specularGlossinessTexture"];
        texture_info["texCoord"] = serde_json::json!(1);
        texture_info["extensions"]["KHR_texture_transform"] = serde_json::json!({
            "offset": [0.125, -0.25],
            "rotation": 0.0,
            "scale": [0.5, 0.75],
            "texCoord": 1,
        });
        let samplers = document
            .as_object_mut()
            .expect("fixture JSON object")
            .entry("samplers")
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .expect("fixture sampler array");
        let sampler_index = samplers.len();
        samplers.push(serde_json::json!({
            "magFilter": 9728,
            "minFilter": 9984,
            "wrapS": 33648,
            "wrapT": 33648,
        }));
        document["textures"][6]["sampler"] = serde_json::json!(sampler_index);
        json_chunk.1 = serde_json::to_vec(&document).expect("serialize mutated fixture JSON");
        while json_chunk.1.len() % 4 != 0 {
            json_chunk.1.push(b' ');
        }

        let total_length = 12 + chunks.iter().map(|(_, payload)| 8 + payload.len()).sum::<usize>();
        let mut mutated = Vec::with_capacity(total_length);
        mutated.extend_from_slice(b"glTF");
        mutated.extend_from_slice(&2u32.to_le_bytes());
        mutated.extend_from_slice(&(total_length as u32).to_le_bytes());
        for (chunk_type, payload) in chunks {
            mutated.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            mutated.extend_from_slice(&chunk_type.to_le_bytes());
            mutated.extend_from_slice(&payload);
        }
        let path = std::env::temp_dir().join(format!(
            "manifold-spec-gloss-typed-metadata-{}.glb",
            std::process::id()
        ));
        std::fs::write(&path, mutated).expect("write mutated spec-gloss fixture");
        let (_, material) = test_material(&path, "BottleMat_SpecGloss");
        let map = &material.extension_maps[9];
        assert_eq!(map.tex_coord, 1);
        assert!((map.uv_transform[0] - 0.5).abs() < 1e-6);
        assert!((map.uv_transform[1] - 0.0).abs() < 1e-6);
        assert!((map.uv_transform[2] - 0.0).abs() < 1e-6);
        assert!((map.uv_transform[3] - 0.75).abs() < 1e-6);
        assert!((map.uv_transform[4] - 0.125).abs() < 1e-6);
        assert!((map.uv_transform[5] + 0.25).abs() < 1e-6);
        assert_eq!(map.sampler.wrap_u, manifold_gpu::GpuAddressMode::MirrorRepeat);
        assert_eq!(map.sampler.wrap_v, manifold_gpu::GpuAddressMode::MirrorRepeat);
        assert_eq!(map.sampler.mag_filter, manifold_gpu::GpuFilterMode::Nearest);
        assert_eq!(map.sampler.min_filter, manifold_gpu::GpuFilterMode::Nearest);
        assert_eq!(map.sampler.mip_filter, Some(manifold_gpu::GpuFilterMode::Nearest));
        std::fs::remove_file(path).expect("remove mutated spec-gloss fixture");
    }

    /// BUG-5uqg: `KHR_lights_punctual` was listed in
    /// `MANIFOLD_SUPPORTED_EXTENSIONS` but never actually parsed — this
    /// pins `collect_punctual_lights` against the
    /// Khronos conformance fixture's single directional light: identity
    /// node transform at the origin, so world_pos is the origin and
    /// world_forward is the untouched local -Z axis.
    #[test]
    fn directional_light_fixture_imports_one_light_with_expected_shape() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/DirectionalLight.glb");
        if !path.exists() {
            println!(
                "directional_light_fixture_imports_one_light_with_expected_shape: fixture not found at {}, skipping",
                path.display()
            );
            return;
        }
        let (document, _) = parse_document_and_buffers(&path).expect("parse light fixture");
        let lights = collect_punctual_lights(&document);
        assert_eq!(lights.len(), 1, "expected exactly one KHR_lights_punctual light");
        let light = &lights[0];
        assert_eq!(light.kind, GltfLightKind::Directional);
        assert!((light.color[0] - 0.9).abs() < 1e-4);
        assert!((light.color[1] - 0.8).abs() < 1e-4);
        assert!((light.color[2] - 0.1).abs() < 1e-4);
        assert!((light.intensity - 1.0).abs() < 1e-4);
        assert_eq!(light.range, None);
        assert!(
            (light.world_pos[0]).abs() < 1e-4
                && (light.world_pos[1]).abs() < 1e-4
                && (light.world_pos[2]).abs() < 1e-4,
            "identity node transform should leave the light at the origin, got {:?}",
            light.world_pos
        );
        assert!(
            (light.world_forward[0]).abs() < 1e-4
                && (light.world_forward[1]).abs() < 1e-4
                && (light.world_forward[2] - (-1.0)).abs() < 1e-4,
            "identity node transform should leave the light aiming down -Z, got {:?}",
            light.world_forward
        );
    }

    /// BUG-5uqg: the multi-light Khronos fixture — one directional and one
    /// point light on non-identity node transforms — pins that both
    /// spec kinds parse with the RAW (unconverted) intensity units;
    /// `gltf_import::scene`'s conversion is exercised separately by the
    /// import-graph assembly tests.
    #[test]
    fn playset_light_test_fixture_imports_directional_and_point_lights() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/khronos/PlaysetLightTest.glb");
        if !path.exists() {
            println!(
                "playset_light_test_fixture_imports_directional_and_point_lights: fixture not found at {}, skipping",
                path.display()
            );
            return;
        }
        let (document, _) = parse_document_and_buffers(&path).expect("parse light fixture");
        let lights = collect_punctual_lights(&document);
        assert_eq!(lights.len(), 2, "expected the fixture's two KHR_lights_punctual lights");

        let directional = lights
            .iter()
            .find(|l| l.kind == GltfLightKind::Directional)
            .expect("expected one Directional light");
        assert!((directional.intensity - 512.25).abs() < 1e-2);

        let point = lights
            .iter()
            .find(|l| l.kind == GltfLightKind::Point)
            .expect("expected one Point light");
        assert!((point.intensity - 1500.0).abs() < 1e-1);
        assert_eq!(point.name.as_deref(), Some("LEDlight"));
        // Non-identity node transform (translation ~[0.117, 0.124, 0.001]):
        // world_pos must actually carry it, not fall back to the origin.
        assert!(point.world_pos[0].abs() > 1e-3 || point.world_pos[1].abs() > 1e-3);
    }
}

#[cfg(test)]
thread_local! {
    static TEXTURE_DECODE_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) mod texture_tests;

#[cfg(test)]
mod bounds_tests;
