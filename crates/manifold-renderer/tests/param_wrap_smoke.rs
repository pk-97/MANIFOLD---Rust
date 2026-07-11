//! BUG-039 smoke gate — a periodic (`wraps: true`) card param must render
//! identically at its `min` and `max` rails, since those are the same
//! logical angle (0deg == 360deg, -180deg == 180deg): this is the exact
//! precondition that makes wrapping (instead of clamping) invisible on
//! screen — a saw LFO or an automation ramp crossing the rail jumps between
//! two values that render the same, so the wrap reads as continuous motion,
//! never a snap. Covers one effect param and one generator param from the
//! BUG-039 tag sweep (see `docs/BUG_BACKLOG.md`).
//!
//! Run:
//!   cargo test -p manifold-renderer --test param_wrap_smoke --features gpu-proofs
#![cfg(feature = "gpu-proofs")]

use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::ParamSpecDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

const W: u32 = 256;
const H: u32 = 256;

fn slot(spec: &ParamSpecDef, value: f32) -> Param {
    let mut p = Param::bundled(spec.clone());
    p.value = value;
    p.base = value;
    p
}

/// Render the named bundled generator preset once at `frame 0`/`time 0`
/// with every card param at its declared default EXCEPT `param_id`, which
/// is set to `value`. Returns the tonemapped RGBA8 readback.
fn render_generator_at(preset_id: &'static str, param_id: &str, value: f32) -> Vec<u8> {
    let device = GpuDevice::new();
    let registry = PrimitiveRegistry::with_builtin();
    let json = manifold_renderer::node_graph::bundled_preset_json(&PresetTypeId::new(preset_id))
        .unwrap_or_else(|| panic!("{preset_id} bundled preset json"));
    let def: manifold_core::effect_graph_def::EffectGraphDef =
        serde_json::from_str(&json).expect("parse preset JSON");
    let specs = def
        .preset_metadata
        .as_ref()
        .expect("preset metadata")
        .params
        .clone();
    let params: Vec<Param> = specs
        .iter()
        .map(|s| {
            let v = if s.id == param_id { value } else { s.default_value };
            slot(s, v)
        })
        .collect();
    let manifest = ParamManifest::from_params(params);

    let mut runtime = PresetRuntime::from_json_str_with_device(
        &json,
        &registry,
        &device,
        W,
        H,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .expect("preset runtime build");

    let target = RenderTarget::new(&device, W, H, GpuTextureFormat::Rgba16Float, "wrap-smoke");
    let ctx = PresetContext {
        time: 0.0,
        beat: 0.0,
        dt: 1.0 / 60.0,
        width: W,
        height: H,
        output_width: W,
        output_height: H,
        aspect: W as f32 / H as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let mut enc = device.create_encoder("wrap-smoke-frame");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
        runtime.render(&mut gpu, &target.texture, &ctx, &manifest);
    }
    enc.commit_and_wait_completed();

    let bytes_per_row = W * 8;
    let total = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut readback = device.create_encoder("wrap-smoke-readback");
    readback.copy_texture_to_buffer(&target.texture, &buf, W, H, bytes_per_row);
    readback.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer");
    let halves: &[u16] =
        unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (W * H * 4) as usize) };
    halves
        .chunks_exact(4)
        .flat_map(|px| {
            let tonemap = |bits: u16| -> u8 {
                let v = half::f16::from_bits(bits).to_f32();
                let ldr = (v / (1.0 + v)).clamp(0.0, 1.0);
                (ldr * 255.0).round() as u8
            };
            [tonemap(px[0]), tonemap(px[1]), tonemap(px[2]), tonemap(px[3])]
        })
        .collect()
}

/// Mean absolute per-channel difference between two equal-length RGBA8
/// buffers, 0..255 scale.
fn mean_abs_diff(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let sum: i64 = a
        .iter()
        .zip(b.iter())
        .map(|(&x, &y)| (x as i64 - y as i64).abs())
        .sum();
    sum as f64 / a.len() as f64
}

/// DigitalPlants `cam_orbit` (-180..180, wraps: true) — a camera orbit
/// angle. -180deg and +180deg are the same orbit position, so the two
/// renders must be visually indistinguishable: the exact seam a wrapping
/// LFO/automation ramp crosses.
#[test]
fn digital_plants_cam_orbit_renders_identically_at_min_and_max() {
    let a = render_generator_at("DigitalPlants", "cam_orbit", -180.0);
    let b = render_generator_at("DigitalPlants", "cam_orbit", 180.0);
    let diff = mean_abs_diff(&a, &b);
    assert!(
        diff < 1.0,
        "cam_orbit=-180 and cam_orbit=180 must render the same camera position \
         (BUG-039 wrap precondition), mean abs channel diff = {diff:.3}"
    );
}

/// MetallicGlass `cam_orbit` (-180..180, wraps: true) — same precondition
/// on a second, visually distinct generator (raymarched fractal, not a
/// particle field) so the smoke gate isn't tied to one shader family.
#[test]
fn metallic_glass_cam_orbit_renders_identically_at_min_and_max() {
    let a = render_generator_at("MetallicGlass", "cam_orbit", -180.0);
    let b = render_generator_at("MetallicGlass", "cam_orbit", 180.0);
    let diff = mean_abs_diff(&a, &b);
    assert!(
        diff < 1.0,
        "cam_orbit=-180 and cam_orbit=180 must render the same camera position \
         (BUG-039 wrap precondition), mean abs channel diff = {diff:.3}"
    );
}
