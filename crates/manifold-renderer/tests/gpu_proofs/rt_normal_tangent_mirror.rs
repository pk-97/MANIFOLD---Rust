//! `NormalTangentMirrorTest.glb` — the normal-map / mirrored-tangent
//! held-out asset, on the RT path.
//!
//! The fixture is ONE flat plate (z extent 0.09 world units) carrying two
//! 4×5 grids of tiles: a dielectric set (metallic 0.00, roughness 0.30,
//! bluish base colour) and a polished-gold set (metallic 0.99, roughness
//! 0.004, gold base colour). Every dome except the "Geometry" column is a
//! normal map, not geometry, and 80 of its 2770 vertices carry
//! `TANGENT.w = -1` (mirrored handedness) — that is the whole point of the
//! asset. Reading a render of it as "the same GLB twice, one copy broken"
//! is a misread: the gold grid is the second grid.
//!
//! Two gates here.
//!
//! 1. `..._dispatches_rt_on_the_import_path` — liveness. An imported scene
//!    with RT on must actually dispatch the RT kernel. This is the check
//!    whose absence let `rt_r3_heldout_gltf` run green for its whole life
//!    while rendering pure raster (see `harness::import_rt_manifest`).
//!
//! 2. `..._normal_map_reaches_the_traced_reflection` — the real conformance
//!    ask, and it FAILS today: BUG-wytp (rt-reflections-are-normal-map-blind).
//!    The RT kernel's shading normal is `fetch_interpolated_normal`'s
//!    barycentric vertex normal and nothing perturbs it, so a mirror-smooth
//!    normal-mapped surface traces as a flat plate. Because
//!    `render_scene.wgsl` SUBSTITUTES the traced reflection for the
//!    prefiltered env fetch rather than adding to it, turning RT Reflections
//!    on DESTROYS shape the raster IBL had. `#[ignore]`d against the bead
//!    rather than deleted or weakened: the fix is the glTF TANGENT plumbing
//!    (BUG-wfxe, gltf-tangent-attribute-dropped-at-import) plus in-kernel
//!    normal-map sampling, and this is the gate that closes both.

use manifold_core::flatten::flatten_groups;
use manifold_core::params::ParamManifest;
use manifold_gpu::{GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

const W: u32 = 512;
const H: u32 = 512;

/// Peter's reported lighting: a near-black void, so the gold grid's whole
/// appearance comes from the traced/prefiltered reflection rather than a
/// bright environment washing the difference out.
const ENV_INTENSITY: f32 = 0.15;
const FILL_LIGHT: f32 = 0.0;
const SUN_INTENSITY: f32 = 10.0;

fn ctx(frame_count: i64) -> PresetContext {
    PresetContext {
        time: frame_count as f64 / 60.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: W,
        height: H,
        output_width: W,
        output_height: H,
        aspect: W as f32 / H as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

fn frame(
    runtime: &mut PresetRuntime,
    h: &harness::ParityHarness,
    target: &manifold_gpu::GpuTexture,
    f: i64,
    params: &ParamManifest,
) {
    let c = ctx(f);
    let mut enc = h.device.create_encoder("ntm-frame");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
        runtime.render(&mut gpu, target, &c, params);
    }
    enc.commit_and_wait_completed();
}

fn make_target(device: &GpuDevice, label: &str) -> manifold_gpu::GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: W,
        height: H,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::RENDER_TARGET_FULL,
        label,
        mip_levels: 1,
    })
}

fn readback_rgba_f32(device: &GpuDevice, texture: &manifold_gpu::GpuTexture) -> Vec<f32> {
    let bytes_per_row = W * 8;
    let total = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("ntm-readback");
    enc.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer must expose mapped pointer");
    let halves: &[u16] = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (W * H * 4) as usize) };
    halves.iter().map(|&v| half::f16::from_bits(v).to_f32()).collect()
}

fn non_black_fraction(px: &[f32]) -> f64 {
    let n = px.len() / 4;
    if n == 0 {
        return 0.0;
    }
    let lit = (0..n)
        .filter(|&i| px[i * 4] > 0.03 || px[i * 4 + 1] > 0.03 || px[i * 4 + 2] > 0.03)
        .count();
    lit as f64 / n as f64
}

fn fixture() -> std::path::PathBuf {
    let glb = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/gltf/khronos/NormalTangentMirrorTest.glb");
    assert!(glb.exists(), "held-out fixture missing: {glb:?}");
    glb
}

/// Import the fixture with RT on, optionally deleting every wire feeding a
/// `node.scene_object`'s `normal_map` port — the same surgical-unbind shape
/// `rt_r3_heldout_gltf` uses for `mr_map`. Also stamps the void-lighting
/// card values so the reflection term dominates.
fn build_variant(
    h: &harness::ParityHarness,
    strip_normal_map: bool,
) -> (PresetRuntime, manifold_gpu::GpuTexture, ParamManifest, usize) {
    let (def, report) = assemble_import_graph(&fixture()).expect("import must succeed");
    eprintln!("[ntm] import report: {report:?}");
    let mut flat = flatten_groups(&def).expect("flatten_groups must succeed (no-op if already flat)");

    let scene_object_ids: Vec<_> = flat
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.scene_object")
        .map(|n| n.id)
        .collect();
    assert!(!scene_object_ids.is_empty(), "imported def has no node.scene_object bind nodes");
    let normal_wire_count = flat
        .wires
        .iter()
        .filter(|w| scene_object_ids.contains(&w.to_node) && w.to_port == "normal_map")
        .count();
    assert!(
        normal_wire_count > 0,
        "NormalTangentMirrorTest.glb's import wired no `normal_map` port — held-out asset \
         assumption violated (this asset exists to exercise normal mapping)"
    );
    if strip_normal_map {
        flat.wires
            .retain(|w| !(scene_object_ids.contains(&w.to_node) && w.to_port == "normal_map"));
    }

    let mut params = harness::import_rt_manifest(&flat, true, true);
    for (suffix, value) in [
        ("1_intensity", ENV_INTENSITY),
        ("1_fill", FILL_LIGHT),
        ("7_intensity", SUN_INTENSITY),
    ] {
        let id = params
            .iter()
            .find(|p| p.id() == suffix)
            .map(|p| p.id().to_string())
            .unwrap_or_else(|| panic!("imported def exposes no card param `{suffix}`"));
        params.get_mut(&id).expect("id came from this manifest").value = value;
    }

    let registry = PrimitiveRegistry::with_builtin();
    let runtime = PresetRuntime::from_def_with_device(
        flat,
        &registry,
        std::sync::Arc::clone(&h.device),
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("imported def must build a runtime");
    let label = if strip_normal_map { "ntm-stripped" } else { "ntm-bound" };
    (runtime, make_target(&h.device, label), params, normal_wire_count)
}

/// Frames enough for the async accel build + the one-frame rebuild defer and
/// the temporal accumulator to settle.
const SETTLE_FRAMES: i64 = 400;

#[test]
fn normal_tangent_mirror_dispatches_rt_on_the_import_path() {
    let h = harness::shared();
    let (mut runtime, target, params, _) = build_variant(h, false);
    for f in 0..SETTLE_FRAMES {
        frame(&mut runtime, h, &target, f, &params);
    }
    let frac = non_black_fraction(&readback_rgba_f32(&h.device, &target));
    eprintln!("[ntm] RT-on non-black fraction={frac:.4}");
    assert!(
        frac > 0.01,
        "the fixture rendered ~black (frac={frac:.4}) — an import/harness issue, not an RT result"
    );
    harness::assert_rt_dispatched(
        || frame(&mut runtime, h, &target, SETTLE_FRAMES, &params),
        "NormalTangentMirrorTest with RT enabled through the card manifest",
    );
}

/// Settle a variant and return its raw traced-reflection channel.
///
/// `refl_raw`, not the composite: the composite folds in the RASTER shading,
/// which DOES consume the normal map, so a composite comparison passes on
/// raster's contribution alone and proves nothing about the kernel. That
/// false-positive is the reason this reads the internal channel.
fn settle_and_capture_refl_raw(
    h: &harness::ParityHarness,
    strip_normal_map: bool,
) -> Vec<f32> {
    let (mut runtime, target, params, wire_count) = build_variant(h, strip_normal_map);
    eprintln!("[ntm] strip_normal_map={strip_normal_map} normal_map wires={wire_count}");
    for f in 0..SETTLE_FRAMES {
        frame(&mut runtime, h, &target, f, &params);
    }
    let slots = harness::capture_rt_channels(|| {
        frame(&mut runtime, h, &target, SETTLE_FRAMES, &params)
    });
    let refl = slots
        .iter()
        .find(|c| c.label == "refl_raw")
        .unwrap_or_else(|| {
            panic!(
                "no `refl_raw` RT capture (RT never dispatched, or reflections were off) — \
                 captured: {:?}",
                slots.iter().map(|c| c.label.clone()).collect::<Vec<_>>()
            )
        });
    let px = harness::read_rt_channel(&h.device, refl);
    eprintln!("[ntm] refl_raw {}x{} nonblack={:.4}", refl.w, refl.h, non_black_fraction(&px));
    px
}

/// The normal map must change the traced reflection. It does not: the RT
/// kernel shades from `fetch_interpolated_normal`'s barycentric vertex normal
/// and never samples the map, so a mirror-smooth normal-mapped surface traces
/// as the flat plate it geometrically is.
///
/// Measured when filed: `refl_raw` bound vs. normal-map-stripped was
/// BIT-IDENTICAL — differing fraction 0.00000, max abs diff 0.0000, both
/// variants lit over the same 0.0897 of the frame. Un-ignore together with
/// BUG-wytp (rt-reflections-are-normal-map-blind); the prerequisite is
/// BUG-wfxe (gltf-tangent-attribute-dropped-at-import).
#[test]
#[ignore = "BUG-wytp (rt-reflections-are-normal-map-blind): RT traces the vertex normal only"]
fn normal_tangent_mirror_normal_map_reaches_the_traced_reflection() {
    let h = harness::shared();
    // Warm-up variant, discarded: the FIRST PresetRuntime built in this
    // process reads `refl_raw` back all-black no matter how many frames it
    // settles for (the same cumulative-load transient `rt_r3_heldout_gltf`
    // absorbs with a retry). Measuring without this makes the result depend
    // on variant ORDER, not on the normal map.
    let _warmup = settle_and_capture_refl_raw(h, false);
    let bound_px = settle_and_capture_refl_raw(h, false);
    let stripped_px = settle_and_capture_refl_raw(h, true);
    assert_eq!(bound_px.len(), stripped_px.len(), "capture geometry changed between variants");

    // Sanity: the channel must hold real traced radiance, or "no difference"
    // would be vacuously true.
    let bound_lit = non_black_fraction(&bound_px);
    assert!(
        bound_lit > 0.01,
        "traced reflection channel is ~empty (frac={bound_lit:.4}) — a harness/dispatch issue, \
         not a normal-map result"
    );

    let n = bound_px.len() / 4;
    let mut differing = 0usize;
    let mut max_abs_diff = 0.0f32;
    for i in 0..n {
        let mut px_diff = 0.0f32;
        for c in 0..3 {
            px_diff = px_diff.max((bound_px[i * 4 + c] - stripped_px[i * 4 + c]).abs());
        }
        max_abs_diff = max_abs_diff.max(px_diff);
        if px_diff > 0.02 {
            differing += 1;
        }
    }
    let differing_fraction = differing as f64 / n as f64;
    eprintln!(
        "[ntm] refl_raw differing_fraction={differing_fraction:.5} max_abs_diff={max_abs_diff:.4} \
         (normal-map-bound vs. stripped, RT + reflections on)"
    );

    // The plate covers ~15% of the frame and the normal map is what gives
    // every non-"Geometry" tile its dome, so a correct implementation moves
    // most of those texels. A third of the lit area is a floor, not a tuned
    // threshold.
    let min_differing_fraction = bound_lit / 3.0;
    assert!(
        differing_fraction >= min_differing_fraction,
        "binding NormalTangentMirrorTest's normal map made no measurable difference to the TRACED \
         reflection ({differing_fraction:.5} < {min_differing_fraction:.5}) — the normal map is \
         not reaching the RT kernel"
    );
}
