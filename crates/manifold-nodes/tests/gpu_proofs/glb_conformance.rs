//! GLB conformance harness — renders every
//! asset in `tests/fixtures/gltf/khronos/manifest.json` through the
//! PRODUCTION import path (`compile_model_scene`) and gates each
//! `expect_pass` asset with the numeric asserts named in the manifest, never
//! by pixel-matching Khronos's own published renders (D3). `xfail` assets
//! still run (when their fixture is present) so a future phase's flip to
//! `expect_pass` starts from a known value, but their checks are never
//! asserted — only reported (gate: "every xfail reported as xfail, not
//! silently skipped").
//!
//! Skip-if-absent (D1): `tests/fixtures/gltf/khronos/` is gitignored and
//! populated by `scripts/fetch-gltf-conformance.sh`. A missing asset prints a
//! loud `SKIPPED` line and the sweep stays green — CI and a fresh worktree
//! never need network (same established pattern as the AMG GT3 smoke test in
//! `gltf_import.rs`).
//!
//! Run: `bash scripts/fetch-gltf-conformance.sh && cargo test -p
//! manifold-nodes --features gpu-proofs --test gpu_proofs glb_conformance:: --
//! --test-threads=1`
//!
//! Regenerate goldens: `UPDATE_CONFORMANCE_GOLDENS=1 cargo test -p
//! manifold-nodes --features gpu-proofs --test gpu_proofs glb_conformance:: --
//! --test-threads=1` — review the diff before committing (D3).

#![cfg(feature = "gpu-proofs")]

use manifold_nodes as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use manifold_core::effect_graph_def::{EffectGraphDef, SerializedParamValue};
use manifold_core::params::{Param, ParamManifest};
use manifold_core::scene_doc::{ImportedModel, ImportedTransform};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_node_engine::gpu::headless_readback::{
    encode_rgba8_png, non_black_fraction, readback_tonemapped_rgba8,
};
use manifold_node_engine::gpu::render_target::RenderTarget;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;
use manifold_nodes_scene::node_graph::gltf_import::compile_model_scene;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

fn khronos_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf/khronos")
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf")
}

// ---------------------------------------------------------------------------
// Manifest schema
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct ManifestEntry {
    asset: String,
    #[serde(default)]
    checks: Vec<CheckSpec>,
    status: String,
}

#[derive(serde::Deserialize)]
#[serde(tag = "kind")]
enum CheckSpec {
    #[serde(rename = "non_black_fraction_min")]
    NonBlackFractionMin { value: f64 },
    /// D8: render with `sun_int`/`env_intensity`/`scene_ambient` all zeroed —
    /// only emissive can light the frame. Pins that the emissive term is
    /// NOT broken independent of any other light.
    #[serde(rename = "lights_off_nonblack_min")]
    LightsOffNonblackMin { value: f64 },
    /// Regression pin against our OWN prior render (D3) — never Khronos's.
    #[serde(rename = "golden")]
    Golden { file: String, mean_abs_tol: f64 },
    /// "feature on vs feature off changes
    /// the named quantity in the predicted direction" (D3), applied within
    /// ONE render instead of two — `region` (a dim tile: identity-transform
    /// UV sampling a dark part of the source texture, or specularFactor≈0)
    /// must average darker than `reference_region` (a bright/neutral tile)
    /// by at least the given fraction: `mean(region) < value *
    /// mean(reference_region)`. Both regions are normalized `[x0,y0,x1,y1]`
    /// in `[0,1]` screen space (top-left origin, matching the tonemapped
    /// RGBA8 readback). Luminance = the standard-illuminant approximation
    /// `0.2126*r + 0.7152*g + 0.0722*b` over straight (non-premultiplied)
    /// RGBA8 — no premultiplied-alpha correction needed since these assets
    /// have no transparent regions.
    #[serde(rename = "region_mean_luminance_below")]
    RegionMeanLuminanceBelow {
        region: [f64; 4],
        reference_region: [f64; 4],
        value: f64,
    },
    /// TextureTransformTest's "Offset U"
    /// badge tile flips hue (green ✓ when the transform is applied vs.
    /// olive ⊘ when it's ignored) at near-IDENTICAL luminance — verified
    /// empirically (falsification render: `region_mean_luminance_below`
    /// alone doesn't separate the two states here, a hue-based check
    /// does). `mean(G) - mean(R)` over `region` must exceed `value`.
    #[serde(rename = "region_green_minus_red_above")]
    RegionGreenMinusRedAbove { region: [f64; 4], value: f64 },
    /// "clearcoat=1 sphere has brighter
    /// specular peak than clearcoat=0" — a PEAK comparison, not a mean one.
    /// None of the existing mean-based kinds fit: ClearCoatTest's
    /// "Base layer"/"Coated" panels are near-identical diffuse red over
    /// most of their area (the coat's Fresnel-weighted contribution is
    /// concentrated in the specular highlight itself, a small fraction of
    /// each region — a whole-panel MEAN comparison is diluted to noise,
    /// verified empirically this session: mean barely moves while max
    /// moves ~74%). `region`'s max luminance must exceed `value *
    /// reference_region`'s max luminance.
    #[serde(rename = "region_max_luminance_above")]
    RegionMaxLuminanceAbove {
        region: [f64; 4],
        reference_region: [f64; 4],
        value: f64,
    },
    /// "the transmissive region is
    /// NOT simply base_color.a-blended-flat" (D3's rejected-approximation
    /// criterion), expressed as luminance variance rather than mean/max —
    /// neither existing kind fits: a mean/max comparison can't tell a
    /// uniformly-tinted flat wash from a region carrying real image detail
    /// (rim + decal + a genuinely refracted background). Chosen because the
    /// OLD alpha-blend approximation (D8/F-P5) darkened a fully-transmissive
    /// object's alpha toward 0 — i.e. toward "blends smoothly into the
    /// background," which has exactly the *reference_region*'s own std dev,
    /// no more. `region`'s luminance std dev must exceed `value *
    /// reference_region`'s std dev — verified empirically this session
    /// (CompareTransmission.glb's glass-bowl region measures ~59 vs a
    /// plain-background patch's ~27, a >2x ratio) before picking the
    /// threshold, same discipline as every other kind here.
    #[serde(rename = "region_stddev_above")]
    RegionStdDevAbove {
        region: [f64; 4],
        reference_region: [f64; 4],
        value: f64,
    },
}

/// Mean luminance over a normalized `[x0,y0,x1,y1]` rect of a tonemapped
/// RGBA8 `WIDTH`×`HEIGHT` buffer (row-major, 4 bytes/pixel).
fn region_mean_luminance(rgba: &[u8], region: [f64; 4]) -> f64 {
    let x0 = ((region[0] * WIDTH as f64) as u32).min(WIDTH - 1);
    let y0 = ((region[1] * HEIGHT as f64) as u32).min(HEIGHT - 1);
    let x1 = ((region[2] * WIDTH as f64) as u32).clamp(x0 + 1, WIDTH);
    let y1 = ((region[3] * HEIGHT as f64) as u32).clamp(y0 + 1, HEIGHT);
    let mut sum = 0.0f64;
    let mut n = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * WIDTH + x) * 4) as usize;
            let r = rgba[i] as f64;
            let g = rgba[i + 1] as f64;
            let b = rgba[i + 2] as f64;
            sum += 0.2126 * r + 0.7152 * g + 0.0722 * b;
            n += 1;
        }
    }
    sum / n.max(1) as f64
}

/// Max luminance over a normalized `[x0,y0,x1,y1]` rect — same rect
/// convention as [`region_mean_luminance`], same luminance weights.
/// Isolates a specular PEAK, which a mean
/// over the same region dilutes away (see `RegionMaxLuminanceAbove`'s doc
/// comment).
fn region_max_luminance(rgba: &[u8], region: [f64; 4]) -> f64 {
    let x0 = ((region[0] * WIDTH as f64) as u32).min(WIDTH - 1);
    let y0 = ((region[1] * HEIGHT as f64) as u32).min(HEIGHT - 1);
    let x1 = ((region[2] * WIDTH as f64) as u32).clamp(x0 + 1, WIDTH);
    let y1 = ((region[3] * HEIGHT as f64) as u32).clamp(y0 + 1, HEIGHT);
    let mut max_lum = 0.0f64;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * WIDTH + x) * 4) as usize;
            let r = rgba[i] as f64;
            let g = rgba[i + 1] as f64;
            let b = rgba[i + 2] as f64;
            let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            max_lum = max_lum.max(lum);
        }
    }
    max_lum
}

/// Population std dev of luminance over a normalized `[x0,y0,x1,y1]` rect —
/// same rect convention/weights as [`region_mean_luminance`].
/// Measures "how much real detail is
/// in this region" — see `CheckSpec::RegionStdDevAbove`'s doc comment.
fn region_stddev_luminance(rgba: &[u8], region: [f64; 4]) -> f64 {
    let x0 = ((region[0] * WIDTH as f64) as u32).min(WIDTH - 1);
    let y0 = ((region[1] * HEIGHT as f64) as u32).min(HEIGHT - 1);
    let x1 = ((region[2] * WIDTH as f64) as u32).clamp(x0 + 1, WIDTH);
    let y1 = ((region[3] * HEIGHT as f64) as u32).clamp(y0 + 1, HEIGHT);
    let mut sum = 0.0f64;
    let mut sum_sq = 0.0f64;
    let mut n = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * WIDTH + x) * 4) as usize;
            let r = rgba[i] as f64;
            let g = rgba[i + 1] as f64;
            let b = rgba[i + 2] as f64;
            let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            sum += lum;
            sum_sq += lum * lum;
            n += 1;
        }
    }
    let n = n.max(1) as f64;
    let mean = sum / n;
    (sum_sq / n - mean * mean).max(0.0).sqrt()
}

/// Mean `(r, g, b)` over a normalized `[x0,y0,x1,y1]` rect — same rect
/// convention as [`region_mean_luminance`].
fn region_mean_rgb(rgba: &[u8], region: [f64; 4]) -> (f64, f64, f64) {
    let x0 = ((region[0] * WIDTH as f64) as u32).min(WIDTH - 1);
    let y0 = ((region[1] * HEIGHT as f64) as u32).min(HEIGHT - 1);
    let x1 = ((region[2] * WIDTH as f64) as u32).clamp(x0 + 1, WIDTH);
    let y1 = ((region[3] * HEIGHT as f64) as u32).clamp(y0 + 1, HEIGHT);
    let (mut rs, mut gs, mut bs, mut n) = (0.0f64, 0.0f64, 0.0f64, 0u64);
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * WIDTH + x) * 4) as usize;
            rs += rgba[i] as f64;
            gs += rgba[i + 1] as f64;
            bs += rgba[i + 2] as f64;
            n += 1;
        }
    }
    let n = n.max(1) as f64;
    (rs / n, gs / n, bs / n)
}

// ---------------------------------------------------------------------------
// The old importer's stage (camera framing, sun, environment)
// ---------------------------------------------------------------------------
//
// The goldens, coverage floors and regions were captured under the old
// importer's own stage: the model recentred on its vertex box at unit scale,
// an orbit camera fitted to that box, one hard-shadowed sun when the file
// has no lights, and a softbox environment with a stronger fill. Importing
// never moves the scene's camera, so the sweep writes that stage onto the
// compiled card instead. The math is the old `scene_scale::Framing` and
// `gltf_import_summary` box (before the scene document redesign).

type Mat4 = [[f32; 4]; 4];

const MAT4_IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// Column-major `a * b` (`m[col][row]`, glTF's convention).
fn mat4_mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [[0.0f32; 4]; 4];
    for (c, column) in out.iter_mut().enumerate() {
        for (r, cell) in column.iter_mut().enumerate() {
            *cell = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    out
}

fn mat4_point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|r| m[0][r] * p[0] + m[1][r] * p[1] + m[2][r] * p[2] + m[3][r])
}

fn mat4_from_trs(t: [f32; 3], q: [f32; 4], s: [f32; 3]) -> Mat4 {
    let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
    [
        [
            (1.0 - 2.0 * (y * y + z * z)) * s[0],
            2.0 * (x * y + w * z) * s[0],
            2.0 * (x * z - w * y) * s[0],
            0.0,
        ],
        [
            2.0 * (x * y - w * z) * s[1],
            (1.0 - 2.0 * (x * x + z * z)) * s[1],
            2.0 * (y * z + w * x) * s[1],
            0.0,
        ],
        [
            2.0 * (x * z + w * y) * s[2],
            2.0 * (y * z - w * x) * s[2],
            (1.0 - 2.0 * (x * x + y * y)) * s[2],
            0.0,
        ],
        [t[0], t[1], t[2], 1.0],
    ]
}

/// A plain-float VEC3/VEC4 accessor's components, stride honoured.
fn read_f32s(accessor: &gltf::Accessor, buffers: &[gltf::buffer::Data]) -> Option<Vec<f32>> {
    let width = match accessor.dimensions() {
        gltf::accessor::Dimensions::Vec3 => 3,
        gltf::accessor::Dimensions::Vec4 => 4,
        _ => return None,
    };
    let view = accessor.view()?;
    let buffer = buffers.get(view.buffer().index())?;
    let stride = view.stride().unwrap_or(width * 4);
    let base = view.offset() + accessor.offset();
    let mut out = Vec::with_capacity(accessor.count() * width);
    for i in 0..accessor.count() {
        for c in 0..width {
            let at = base + i * stride + c * 4;
            out.push(f32::from_le_bytes(
                buffer.0.get(at..at + 4)?.try_into().ok()?,
            ));
        }
    }
    Some(out)
}

/// `EXT_mesh_gpu_instancing`'s per-instance transforms, none without it.
fn instance_transforms(
    node: &gltf::Node,
    document: &gltf::Document,
    buffers: &[gltf::buffer::Data],
) -> Option<Vec<Mat4>> {
    let attributes = node
        .extension_value("EXT_mesh_gpu_instancing")?
        .get("attributes")?
        .clone();
    let read = |name: &str| {
        let index = attributes.get(name)?.as_u64()? as usize;
        read_f32s(&document.accessors().nth(index)?, buffers)
    };
    let (t, r, s) = (read("TRANSLATION"), read("ROTATION"), read("SCALE"));
    let count = t
        .as_ref()
        .map(|d| d.len() / 3)
        .or(r.as_ref().map(|d| d.len() / 4))
        .or(s.as_ref().map(|d| d.len() / 3))?;
    Some(
        (0..count)
            .map(|i| {
                mat4_from_trs(
                    t.as_ref()
                        .map_or([0.0; 3], |d| [d[i * 3], d[i * 3 + 1], d[i * 3 + 2]]),
                    r.as_ref().map_or([0.0, 0.0, 0.0, 1.0], |d| {
                        [d[i * 4], d[i * 4 + 1], d[i * 4 + 2], d[i * 4 + 3]]
                    }),
                    s.as_ref()
                        .map_or([1.0; 3], |d| [d[i * 3], d[i * 3 + 1], d[i * 3 + 2]]),
                )
            })
            .collect(),
    )
}

/// The old importer's scene box: every vertex in world space, a skinned
/// mesh's through its bind-pose skin matrices (its node transform ignored,
/// as glTF draws it), an instanced mesh's through every instance.
fn old_import_bounds(path: &Path) -> Result<([f32; 3], [f32; 3]), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let gltf = gltf::Gltf::from_slice_without_validation(&bytes)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    let buffers = gltf::import_buffers(&gltf.document, path.parent(), gltf.blob.clone())
        .map_err(|e| format!("buffers {}: {e}", path.display()))?;
    let document = &gltf.document;
    let mut parent_of = vec![None; document.nodes().count()];
    for node in document.nodes() {
        for child in node.children() {
            parent_of[child.index()] = Some(node.index());
        }
    }
    let static_world = |index: usize| {
        let mut chain = vec![index];
        while let Some(parent) = parent_of[*chain.last().unwrap()] {
            chain.push(parent);
        }
        chain.iter().rev().fold(MAT4_IDENTITY, |world, &i| {
            mat4_mul(
                &world,
                &document.nodes().nth(i).unwrap().transform().matrix(),
            )
        })
    };
    // The roots the importer walks: the default scene, else every scene's
    // nodes, else every parentless node.
    let roots: Vec<gltf::Node> = match document.default_scene() {
        Some(scene) => scene.nodes().collect(),
        None if document.scenes().len() > 0 => {
            let mut seen = std::collections::BTreeSet::new();
            document
                .scenes()
                .flat_map(|s| s.nodes())
                .filter(|n| seen.insert(n.index()))
                .collect()
        }
        None => document
            .nodes()
            .filter(|n| parent_of[n.index()].is_none())
            .collect(),
    };
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    let mut grow = |p: [f32; 3]| {
        for axis in 0..3 {
            lo[axis] = lo[axis].min(p[axis]);
            hi[axis] = hi[axis].max(p[axis]);
        }
    };
    let mut stack: Vec<(gltf::Node, Mat4)> =
        roots.into_iter().map(|n| (n, MAT4_IDENTITY)).collect();
    while let Some((node, parent)) = stack.pop() {
        let world = mat4_mul(&parent, &node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            let instance_worlds: Vec<Mat4> = match instance_transforms(&node, document, &buffers) {
                Some(list) => list.iter().map(|m| mat4_mul(&world, m)).collect(),
                None => vec![world],
            };
            let skin_matrices: Option<Vec<Mat4>> = node.skin().map(|skin| {
                let reader = skin.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));
                let inverse_binds: Vec<Mat4> = reader.read_inverse_bind_matrices().map_or_else(
                    || vec![MAT4_IDENTITY; skin.joints().count()],
                    |it| it.collect(),
                );
                skin.joints()
                    .enumerate()
                    .map(|(k, joint)| {
                        mat4_mul(
                            &static_world(joint.index()),
                            inverse_binds.get(k).unwrap_or(&MAT4_IDENTITY),
                        )
                    })
                    .collect()
            });
            for primitive in mesh.primitives() {
                let reader = primitive.reader(|b| buffers.get(b.index()).map(|d| d.0.as_slice()));
                let Some(positions) = reader.read_positions() else {
                    continue;
                };
                let positions: Vec<[f32; 3]> = positions.collect();
                let joints: Option<Vec<[u16; 4]>> =
                    reader.read_joints(0).map(|it| it.into_u16().collect());
                let weights: Option<Vec<[f32; 4]>> =
                    reader.read_weights(0).map(|it| it.into_f32().collect());
                if let (Some(skin), Some(joints), Some(weights)) =
                    (&skin_matrices, &joints, &weights)
                {
                    for (i, p) in positions.iter().enumerate() {
                        let total: f32 = weights[i].iter().sum::<f32>().max(1e-8);
                        let mut posed = [0.0f32; 3];
                        for k in 0..4 {
                            let Some(m) = skin.get(joints[i][k] as usize) else {
                                continue;
                            };
                            let q = mat4_point(m, *p);
                            for axis in 0..3 {
                                posed[axis] += weights[i][k] / total * q[axis];
                            }
                        }
                        grow(posed);
                    }
                    continue;
                }
                for instance in &instance_worlds {
                    positions
                        .iter()
                        .for_each(|p| grow(mat4_point(instance, *p)));
                }
            }
        }
        stack.extend(node.children().map(|child| (child, world)));
    }
    if !lo[0].is_finite() {
        return Err(format!(
            "{}: no geometry for the old framing box",
            path.display()
        ));
    }
    Ok((lo, hi))
}

/// Card values that rebuild the old importer's stage around `model`.
fn old_import_setup(path: &Path, model: &ImportedModel) -> Result<Vec<(String, f32)>, String> {
    let (lo, hi) = old_import_bounds(path)?;
    let center = [0, 1, 2].map(|axis| (lo[axis] + hi[axis]) * 0.5);
    let dims = [0, 1, 2].map(|axis| hi[axis] - lo[axis]);
    let radius =
        ((dims[0] * dims[0] + dims[1] * dims[1] + dims[2] * dims[2]).sqrt() * 0.5).max(1e-3);
    let fov_y = 0.9_f32;
    let half_fov_tan = (fov_y * 0.5).tan();
    let per_axis_fit = dims
        .iter()
        .map(|&extent| (extent * 0.5) / half_fov_tan * 1.15)
        .fold(0.0f32, f32::max);
    let distance = (2.2 * radius).max(per_axis_fit);
    let near = (distance - radius).max(1e-4) * 0.5;
    // 200 is `node.orbit_camera`'s default far plane, 10000 its range max.
    let far = 200.0f32.max(distance + 1.5 * radius).min(10_000.0);

    // The model at its true size with the old box centre at the origin. A
    // single Object keeps its file node's transform (it is drawn about the
    // node's origin); a Group starts at rest.
    let own = model
        .only_part()
        .filter(|_| model.is_single_mesh())
        .map_or(ImportedTransform::IDENTITY, |part| part.transform);
    let model_position = [0, 1, 2].map(|a| own.position[a] - center[a]);

    let mut setup: Vec<(String, f32)> = vec![
        ("model.scale_x".into(), own.scale[0]),
        ("model.scale_y".into(), own.scale[1]),
        ("model.scale_z".into(), own.scale[2]),
        ("model.pos_x".into(), model_position[0]),
        ("model.pos_y".into(), model_position[1]),
        ("model.pos_z".into(), model_position[2]),
        ("camera.orbit".into(), 0.7),
        ("camera.tilt".into(), 0.3),
        ("camera.distance".into(), distance),
        ("camera.fov_y".into(), fov_y),
        ("camera.look_y".into(), 0.0),
        ("camera.near".into(), near),
        ("camera.far".into(), far),
        ("camera.focus_distance".into(), distance),
        ("camera.f_stop".into(), 32.0),
        ("camera.shutter_angle".into(), 180.0),
        // The old contact-shadow reach: half the model's radius.
        (
            "world.ao_distance".into(),
            (radius * 0.5).clamp((0.001 * radius).max(0.01), 2.0 * radius),
        ),
        // Softbox at full strength with the old import's fill and strips.
        // The old environment had no sun wired in, so no sun disc.
        ("environment.env_intensity".into(), 1.0),
        ("environment.fill".into(), 0.6),
        ("environment.emitter_intensity".into(), 3.0),
        ("environment.sun_disc_intensity".into(), 0.0),
    ];
    if model.lights.is_empty() {
        // The old synthesized key: a hard-shadowed sun from (5, 2, 3) onto
        // the origin, its shadow box wrapped to the model.
        setup.extend([
            ("sun.pos_x".into(), 5.0),
            ("sun.pos_y".into(), 2.0),
            ("sun.pos_z".into(), 3.0),
            ("sun.aim_x".into(), 0.0),
            ("sun.aim_y".into(), 0.0),
            ("sun.aim_z".into(), 0.0),
            ("sun.intensity".into(), 3.5),
            ("sun.cast_shadows".into(), 1.0),
            ("sun.shadow_softness".into(), 0.0),
            ("sun.light_size".into(), 1.0),
            ("sun.range".into(), (radius * 1.5).max(0.01)),
            ("sun.shadow_resolution".into(), 4096.0),
        ]);
    } else {
        // The old import lit a file with its own lights only.
        setup.extend([
            ("sun.intensity".into(), 0.0),
            ("sun.cast_shadows".into(), 0.0),
        ]);
    }
    Ok(setup)
}

// ---------------------------------------------------------------------------
// Render (production import path + the ONE shared readback transform, D2)
// ---------------------------------------------------------------------------

/// Import `path` through `compile_model_scene` and render it to converged,
/// tonemapped RGBA8 pixels — same convergence-poll shape `render_import.rs`
/// uses (BUG-100/BUG-117: byte-stability alone can't distinguish "done" from
/// "every background texture decode is still mid-flight"), reimplemented
/// here rather than shelling out to the bin so a test failure carries a Rust
/// backtrace, not a subprocess exit code.
fn render_asset(
    device: &Arc<GpuDevice>,
    path: &Path,
    overrides: &[(&str, f32)],
    non_black_floor: f64,
) -> Result<Vec<u8>, String> {
    let (def, model) = compile_model_scene(path)?;

    let mut params: Vec<Param> = def
        .preset_metadata
        .as_ref()
        .map(|m| m.params.iter().map(|s| Param::bundled(s.clone())).collect())
        .unwrap_or_default();
    for (id, v) in old_import_setup(path, &model)? {
        let param = params
            .iter_mut()
            .find(|p| p.id() == id)
            .ok_or_else(|| format!("setup param {id} is not on the compiled card"))?;
        param.value = v;
    }
    // Each override sets every card param whose id ends with its suffix:
    // `.intensity` reaches every light, `.env_intensity` the environment.
    // Imported materials start with no ambient term, so lights off is dark.
    for (suffix, v) in overrides {
        for p in params.iter_mut().filter(|p| p.id().ends_with(suffix)) {
            p.value = *v;
        }
    }
    let manifest = ParamManifest::from_params(params);

    let registry = PrimitiveRegistry::with_builtin();
    let settle_frames = accumulation_frames(&def, &registry);
    let format = GpuTextureFormat::Rgba16Float;
    let mut runtime = PresetRuntime::from_def_with_device(
        def,
        &registry,
        Arc::clone(device),
        WIDTH,
        HEIGHT,
        format,
        Some(&manifest),
    )
    .map_err(|e| format!("build failed: {e:?}"))?;

    let target = RenderTarget::new(device, WIDTH, HEIGHT, format, "conformance-target");

    const DT: f32 = 1.0 / 60.0;
    const MAX_FRAMES: u32 = 300;
    let mut prev: Option<Vec<u8>> = None;
    // The first frame rendered with every load done.
    let mut io_done_at: Option<u32> = None;
    let mut last_fraction = 0.0f64;

    for frame in 0..MAX_FRAMES {
        // BUG-210 (animated imports never converged): the clock is frozen so
        // an animated asset holds one pose and settles like a static one.
        // `frame_count` still advances: decode/io paths key off it.
        let time = 0.0f64;
        let ctx = PresetContext {
            time,
            beat: time * 2.0,
            dt: DT,
            width: WIDTH,
            height: HEIGHT,
            output_width: WIDTH,
            output_height: HEIGHT,
            aspect: WIDTH as f32 / HEIGHT as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame as i64,
            anim_progress: 1.0,
            trigger_count: 0,
        };
        let mut enc = device.create_encoder("conformance-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifest);
        }
        enc.commit_and_wait().expect("GPU work completes");

        let rgba = readback_tonemapped_rgba8(device, &target.texture, WIDTH, HEIGHT)
            .expect("GPU readback completes");
        // Live ambient occlusion keeps refining with the clock frozen, so a
        // settled frame is one no viewer can tell from the last: a mean
        // change under one 8-bit step, the measure `check_golden` judges by.
        // A per-pixel max never settles while the accumulated noise jitters.
        let change = prev
            .as_deref()
            .map_or(f64::INFINITY, |p| mean_abs_diff(p, &rgba));
        prev = Some(rgba);
        // Loading is file IO plus the load-time work the app's own pre-roll
        // waits for: meshes and animations load on background threads and
        // report only `warmup_pending`.
        if runtime.io_pending() || runtime.warmup_pending() {
            io_done_at = None;
            // Pace the loop so a background load can land before the frame
            // is judged; a partial load can otherwise read as settled
            // (DamagedHelmet rendered zebra-striped that way).
            std::thread::sleep(std::time::Duration::from_millis(50));
            continue;
        }
        let done_at = *io_done_at.get_or_insert(frame);
        if frame == done_at {
            continue;
        }
        if change < 1.0 {
            last_fraction = non_black_fraction(prev.as_deref().expect("this frame was read back"));
            if last_fraction > non_black_floor {
                let rgba = prev.take().expect("this frame was read back");
                if let Ok(directory) = std::env::var("GLTF_CONFORMANCE_CAPTURE_DIR") {
                    let directory = Path::new(&directory);
                    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
                    let name = path.file_stem().ok_or("asset has no file stem")?;
                    let output = directory.join(name).with_extension("png");
                    std::fs::write(output, encode_rgba8_png(&rgba, WIDTH, HEIGHT))
                        .map_err(|e| e.to_string())?;
                }
                return Ok(rgba);
            }
        }
        if frame - done_at >= settle_frames {
            return Err(format!(
                "not settled {settle_frames} frames after loading finished \
                 (last mean change {change:.3} of 255, last non-black fraction {last_fraction:.4})"
            ));
        }
    }

    Err(format!(
        "never converged after {MAX_FRAMES} frames (last non-black fraction {last_fraction:.4})"
    ))
}

/// Frames a render needs after loading to settle: `node.temporal_accumulate`
/// blends with weight 1/n, n capped at its `frames`, so after `frames` frames
/// its history is a full average and each later frame moves it by 1/frames of
/// the noise. With no accumulator the next frame is already final.
fn accumulation_frames(def: &EffectGraphDef, registry: &PrimitiveRegistry) -> u32 {
    const TYPE_ID: &str = "node.temporal_accumulate";
    let default = registry.construct(TYPE_ID).and_then(|node| {
        node.parameters()
            .iter()
            .find(|p| p.name == "frames")
            .and_then(|p| match p.default {
                ParamValue::Float(f) => Some(f),
                _ => None,
            })
    });
    def.nodes
        .iter()
        .filter(|node| node.type_id == TYPE_ID)
        .filter_map(|node| match node.params.get("frames") {
            Some(SerializedParamValue::Float { value }) => Some(*value),
            Some(SerializedParamValue::Int { value }) => Some(*value as f32),
            _ => default,
        })
        .map(|frames| frames.max(1.0) as u32)
        .max()
        .unwrap_or(1)
}

/// Mean absolute difference of two RGBA8 frames, in 8-bit steps.
fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    let sum: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (f64::from(*x) - f64::from(*y)).abs())
        .sum();
    sum / a.len() as f64
}

// ---------------------------------------------------------------------------
// Golden compare/update (D3: 2/255 mean-abs tolerance, UPDATE_CONFORMANCE_GOLDENS=1)
// ---------------------------------------------------------------------------

fn check_golden(rgba: &[u8], rel_file: &str, mean_abs_tol: f64) -> Result<(), String> {
    let golden_path = fixtures_dir().join(rel_file);

    if std::env::var("UPDATE_CONFORMANCE_GOLDENS").is_ok() {
        if let Some(parent) = golden_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        let png = encode_rgba8_png(rgba, WIDTH, HEIGHT);
        std::fs::write(&golden_path, &png)
            .map_err(|e| format!("write golden {}: {e}", golden_path.display()))?;
        println!(
            "  UPDATE_CONFORMANCE_GOLDENS=1 — wrote {}",
            golden_path.display()
        );
        return Ok(());
    }

    if !golden_path.exists() {
        return Err(format!(
            "golden missing at {} — run with UPDATE_CONFORMANCE_GOLDENS=1 to create it \
             (review the diff before committing, D3)",
            golden_path.display()
        ));
    }

    let golden_img = image::open(&golden_path)
        .map_err(|e| format!("decode golden {}: {e}", golden_path.display()))?
        .to_rgba8();
    if golden_img.width() != WIDTH || golden_img.height() != HEIGHT {
        return Err(format!(
            "golden {} is {}x{}, expected {WIDTH}x{HEIGHT} — regenerate with UPDATE_CONFORMANCE_GOLDENS=1",
            golden_path.display(),
            golden_img.width(),
            golden_img.height()
        ));
    }
    let golden_bytes = golden_img.into_raw();

    let mut sum_abs = 0.0f64;
    for (a, b) in rgba.iter().zip(golden_bytes.iter()) {
        sum_abs += (*a as f64 - *b as f64).abs();
    }
    let mean_abs = sum_abs / rgba.len() as f64;
    println!("  golden mean_abs_diff = {mean_abs:.4} (tol {mean_abs_tol})");
    if mean_abs <= mean_abs_tol {
        Ok(())
    } else {
        Err(format!(
            "golden mismatch: mean_abs_diff {mean_abs:.4} > tol {mean_abs_tol} \
             ({} vs {})",
            golden_path.display(),
            rel_file
        ))
    }
}

// ---------------------------------------------------------------------------
// Check dispatch
// ---------------------------------------------------------------------------

/// Non-black-floor used for a check's own render — 0.02 for anything not
/// deliberately dim, `value / 2.0` for a lights-off render so the
/// convergence heuristic never mistakes "genuinely dim" for "still loading"
/// (see `render_import.rs`'s `--non-black-floor` doc comment for why this
/// must never be a single global constant).
fn run_check(
    device: &Arc<GpuDevice>,
    asset: &str,
    path: &Path,
    check: &CheckSpec,
    assert: bool,
) -> Result<(), String> {
    match check {
        CheckSpec::NonBlackFractionMin { value } => {
            let rgba = render_asset(device, path, &[], 0.02)?;
            let frac = non_black_fraction(&rgba);
            println!("  {asset}: non_black_fraction = {frac:.4} (floor {value})");
            if !assert || frac > *value {
                Ok(())
            } else {
                Err(format!("non_black_fraction {frac:.4} <= floor {value}"))
            }
        }
        CheckSpec::LightsOffNonblackMin { value } => {
            let rgba = render_asset(
                device,
                path,
                &[(".intensity", 0.0), (".env_intensity", 0.0)],
                value / 2.0,
            )?;
            let frac = non_black_fraction(&rgba);
            println!("  {asset}: lights-off non_black_fraction = {frac:.4} (floor {value})");
            if !assert || frac > *value {
                Ok(())
            } else {
                Err(format!(
                    "lights-off non_black_fraction {frac:.4} <= floor {value}"
                ))
            }
        }
        CheckSpec::Golden { file, mean_abs_tol } => {
            let rgba = render_asset(device, path, &[], 0.02)?;
            let result = check_golden(&rgba, file, *mean_abs_tol);
            if assert { result } else { Ok(()) }
        }
        CheckSpec::RegionMeanLuminanceBelow {
            region,
            reference_region,
            value,
        } => {
            let rgba = render_asset(device, path, &[], 0.02)?;
            let region_mean = region_mean_luminance(&rgba, *region);
            let reference_mean = region_mean_luminance(&rgba, *reference_region);
            println!(
                "  {asset}: region_mean_luminance = {region_mean:.2}, reference_mean = {reference_mean:.2} \
                 (region must be < {value} * reference = {:.2})",
                value * reference_mean
            );
            if !assert || region_mean < value * reference_mean {
                Ok(())
            } else {
                Err(format!(
                    "region_mean_luminance {region_mean:.2} >= {value} * reference_mean {reference_mean:.2} \
                     ({:.2}) — the feature-dependent region isn't dimmer than the reference",
                    value * reference_mean
                ))
            }
        }
        CheckSpec::RegionGreenMinusRedAbove { region, value } => {
            let rgba = render_asset(device, path, &[], 0.02)?;
            let (r, g, _b) = region_mean_rgb(&rgba, *region);
            let diff = g - r;
            println!("  {asset}: region mean G-R = {diff:.2} (floor {value})");
            if !assert || diff > *value {
                Ok(())
            } else {
                Err(format!("region mean G-R {diff:.2} <= floor {value}"))
            }
        }
        CheckSpec::RegionMaxLuminanceAbove {
            region,
            reference_region,
            value,
        } => {
            let rgba = render_asset(device, path, &[], 0.02)?;
            let region_max = region_max_luminance(&rgba, *region);
            let reference_max = region_max_luminance(&rgba, *reference_region);
            println!(
                "  {asset}: region_max_luminance = {region_max:.2}, reference_max = {reference_max:.2} \
                 (region must be > {value} * reference = {:.2})",
                value * reference_max
            );
            if !assert || region_max > value * reference_max {
                Ok(())
            } else {
                Err(format!(
                    "region_max_luminance {region_max:.2} <= {value} * reference_max {reference_max:.2} \
                     ({:.2}) — the coated region's specular peak isn't brighter than the base",
                    value * reference_max
                ))
            }
        }
        CheckSpec::RegionStdDevAbove {
            region,
            reference_region,
            value,
        } => {
            let rgba = render_asset(device, path, &[], 0.02)?;
            let region_std = region_stddev_luminance(&rgba, *region);
            let reference_std = region_stddev_luminance(&rgba, *reference_region);
            println!(
                "  {asset}: region_stddev = {region_std:.2}, reference_stddev = {reference_std:.2} \
                 (region must be > {value} * reference = {:.2})",
                value * reference_std
            );
            if !assert || region_std > value * reference_std {
                Ok(())
            } else {
                Err(format!(
                    "region_stddev {region_std:.2} <= {value} * reference_stddev {reference_std:.2} \
                     ({:.2}) — the region doesn't carry more image detail than a flat/plain reference",
                    value * reference_std
                ))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------

#[test]
fn glb_conformance_sweep() {
    let manifest_path = khronos_dir().join("manifest.json");
    let manifest_json = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest_path.display()));
    let entries: Vec<ManifestEntry> =
        serde_json::from_str(&manifest_json).expect("parse manifest.json");
    assert!(
        !entries.is_empty(),
        "manifest.json must name at least one asset"
    );
    // Exact asset names let a failed visual contract be reproduced without
    // rendering the whole corpus. An unset filter still runs every entry.
    let selected = std::env::var("GLTF_CONFORMANCE_ASSETS").ok();
    let selected: Vec<&str> = selected
        .as_deref()
        .map(|s| s.split(',').collect())
        .unwrap_or_default();
    for asset in &selected {
        assert!(
            entries.iter().any(|entry| entry.asset == *asset),
            "unknown conformance asset: {asset}"
        );
    }

    // Every graph and render remains fresh; only device pipelines are shared.
    let serial = manifold_gpu::testkit::test_device();
    let device = serial.arc();
    manifold_gpu::load_disk_shader_caches(&device);

    let mut failures: Vec<String> = Vec::new();
    let mut expect_pass_checked = 0usize;
    let mut xfail_count = 0usize;
    let mut skipped = 0usize;

    for entry in &entries {
        if !selected.is_empty() && !selected.contains(&entry.asset.as_str()) {
            continue;
        }
        let asset_path = khronos_dir().join(&entry.asset);
        let have_fixture = asset_path.exists();

        if let Some(reason) = entry.status.strip_prefix("xfail:") {
            xfail_count += 1;
            if have_fixture {
                println!(
                    "XFAIL {} ({reason}) — running checks informationally, not asserted:",
                    entry.asset
                );
                for check in &entry.checks {
                    if let Err(e) = run_check(&device, &entry.asset, &asset_path, check, false) {
                        println!("  (xfail, no assertion) {e}");
                    }
                }
            } else {
                println!(
                    "XFAIL {} ({reason}) — fixture not fetched \
                     (run scripts/fetch-gltf-conformance.sh, or this asset has no fetchable \
                     variant in v1 — see manifest.json's comment set)",
                    entry.asset
                );
            }
            continue;
        }

        assert_eq!(
            entry.status, "expect_pass",
            "manifest status for {} must be `expect_pass` or `xfail:<reason>`, got `{}`",
            entry.asset, entry.status
        );

        if !have_fixture {
            println!(
                "SKIPPED {} (run scripts/fetch-gltf-conformance.sh) — fixture not found at {}",
                entry.asset,
                asset_path.display()
            );
            skipped += 1;
            continue;
        }

        println!("RUNNING {} (expect_pass):", entry.asset);
        expect_pass_checked += 1;
        for check in &entry.checks {
            if let Err(e) = run_check(&device, &entry.asset, &asset_path, check, true) {
                failures.push(format!("{}: {e}", entry.asset));
            }
        }
    }

    println!(
        "\nglb conformance summary: {expect_pass_checked} expect_pass checked, {xfail_count} xfail, \
         {skipped} skipped (not fetched), {} failures",
        failures.len()
    );

    assert!(
        failures.is_empty(),
        "glb conformance failures (an expect_pass case genuinely regressed — this is an \
         escalation, never downgrade it to xfail to get green):\n{}",
        failures.join("\n")
    );
}
