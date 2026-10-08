//! Scene import and ray-tracing helpers for GPU proofs.

use std::slice;
use half::f16;
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_gpu::raytrace::{EmissiveAliasEntry, EmissiveTriangleGpu};

/// Allocate a valid empty emissive-table argument for RT fixtures. Metal
/// validates the declared argument length even when the table has no entries.
pub fn dummy_emissive_buffer(device: &GpuDevice) -> manifold_gpu::GpuBuffer {
    let size = std::mem::size_of::<EmissiveTriangleGpu>()
        .max(std::mem::size_of::<EmissiveAliasEntry>()) as u64;
    let buffer = device.create_buffer_shared(size);
    buffer.zero_fill();
    buffer
}

/// Build the outer-card [`ParamManifest`] for an imported glTF def and set
/// the RT toggles on it.
///
/// The ONLY way to turn RT on for an imported scene. Writing `rt_enabled`
/// into the def's `render_scene` node params does NOT work and fails
/// SILENTLY: `assemble_import_graph` promotes every scene-atom param to an
/// outer card param, and `PresetRuntime::render` calls `bound.apply()`
/// every frame, which re-asserts the card default (`rt_enabled = false`)
/// over whatever the node param said. A test that sets the node param
/// renders pure raster forever while reading as if it were testing RT —
/// that is how `rt_r3_heldout_gltf`'s held-out gate ran green for its whole
/// life without the RT kernel ever dispatching. Pair every use with
/// [`assert_rt_dispatched`].
pub fn import_rt_manifest(
    def: &manifold_core::effect_graph_def::EffectGraphDef,
    rt_enabled: bool,
    rt_reflections: bool,
) -> manifold_core::params::ParamManifest {
    use manifold_core::params::{Param, ParamManifest};
    let metadata = def
        .preset_metadata
        .as_ref()
        .expect("an imported def always carries card metadata");
    let mut manifest =
        ParamManifest::from_params(metadata.params.iter().cloned().map(Param::bundled).collect());
    for (suffix, on) in [("_rt_enabled", rt_enabled), ("_rt_reflections", rt_reflections)] {
        let id = manifest
            .iter()
            .find(|p| p.id().ends_with(suffix))
            .map(|p| p.id().to_string())
            .unwrap_or_else(|| panic!("imported def exposes no card param ending `{suffix}`"));
        manifest
            .get_mut(&id)
            .expect("id came from this manifest")
            .value = if on { 1.0 } else { 0.0 };
    }
    manifest
}

/// Render one frame with the RT channel capture armed and return whatever
/// internal RT textures `render_scene` handed over.
///
/// `render_scene` only pushes capture slots from inside its `rt_enabled &&
/// rt_ready` branch, so a non-empty result is direct evidence the RT kernel
/// dispatched this frame. Call after the caller's own convergence loop has
/// given the async accel build time to land.
pub fn capture_rt_channels(
    render_one_armed_frame: impl FnOnce(),
) -> Vec<manifold_renderer::node_graph::primitives::RtCaptureSlot> {
    use manifold_renderer::node_graph::primitives::{arm_rt_capture, disarm_rt_capture, take_rt_captures};
    take_rt_captures();
    arm_rt_capture(false);
    render_one_armed_frame();
    disarm_rt_capture();
    take_rt_captures()
}

/// Anti-vacuity guard: fail unless the RT block actually ran.
///
/// The one thing a pixel comparison cannot tell you apart from "the raster
/// path happened to differ".
pub fn assert_rt_dispatched(render_one_armed_frame: impl FnOnce(), context: &str) {
    assert!(
        !capture_rt_channels(render_one_armed_frame).is_empty(),
        "{context}: the RT kernel never dispatched — every number this test reports is a \
         pure-raster measurement. Drive RT through `import_rt_manifest`, not the def's node params."
    );
}

/// Read back one captured RT channel as `[r, g, b, a]` f32 pixels.
/// `R16Float` and `Rg16Float` channels (the masks) fill missing channels with
/// zero.
pub fn read_rt_channel(
    device: &GpuDevice,
    cap: &manifold_renderer::node_graph::primitives::RtCaptureSlot,
) -> Vec<f32> {
    let (bpp, comps) = match cap.tex.format {
        GpuTextureFormat::Rgba16Float => (8u32, 4usize),
        GpuTextureFormat::R16Float => (2, 1),
        GpuTextureFormat::Rg16Float => (4, 2),
        other => panic!("RT capture `{}` has unreadable format {other:?}", cap.label),
    };
    let bytes_per_row = cap.w * bpp;
    let total = u64::from(cap.h * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("rt-channel-readback");
    enc.copy_texture_to_buffer(&cap.tex, &buf, cap.w, cap.h, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer must expose mapped pointer");
    let raw: &[u8] = unsafe { slice::from_raw_parts(ptr, total as usize) };
    let mut out = vec![0.0f32; (cap.w * cap.h * 4) as usize];
    for i in 0..(cap.w * cap.h) as usize {
        for c in 0..comps {
            let o = i * bpp as usize + c * 2;
            out[i * 4 + c] = f16::from_bits(u16::from_le_bytes([raw[o], raw[o + 1]])).to_f32();
        }
    }
    out
}
