//! BUG-066 investigation harness — FluidSim3D corner-bias bisection.
//!
//! Renders the bundled FluidSim3D preset headless for N frames under a
//! configurable scenario matrix (edit `scenarios` in the test), measures
//! per-quadrant luminance shares of the output image, and dumps checkpoint
//! PNGs to /tmp/fluid3d_bias/ for visual inspection. Findings + refuted
//! hypotheses + next steps: docs/BUG_BACKLOG.md BUG-066.
//!
//! `#[ignore]` because it is an investigation tool, not a regression gate —
//! it always passes; the output is the printed quadrant table and the PNGs.
//!
//! Run:
//!   cargo test -p manifold-renderer --test fluid3d_bias --features gpu-proofs -- --ignored --nocapture
#![cfg(feature = "gpu-proofs")]

use std::sync::Arc;

use half::f16;
use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::ParamSpecDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

const W: u32 = 512;
const H: u32 = 512;
const FRAMES: u32 = 900; // 15 s of sim at 60 fps
const CHECKPOINT: u32 = 300;
const OUT_DIR: &str = "/tmp/fluid3d_bias";

fn slot(id: &str, value: f32) -> Param {
    let mut p = Param::bundled(ParamSpecDef {
        id: id.into(),
        name: id.into(),
        min: -1_000_000.0,
        max: 1_000_000.0,
        default_value: value,
        whole_numbers: false,
        is_toggle: false,
        is_trigger: false,
        value_labels: vec![],
        format_string: None,
        osc_suffix: String::new(),
        curve: Default::default(),
        invert: false,
        is_angle: false,
        is_trigger_gate: false,
    });
    p.value = value;
    p.base = value;
    p.exposed = true;
    p
}

/// FluidSim3D card defaults (from presetMetadata), cube container on.
fn base_params() -> Vec<(&'static str, f32)> {
    vec![
        ("flow", -0.01),
        ("feather", 20.0),
        ("curl", 85.0),
        ("turbulence", 0.001),
        ("turb_detail", 8.0),
        ("speed", 1.0),
        ("contrast", 3.5),
        ("scale", 1.0),
        ("count_m", 1.0),
        ("clip_trigger", 0.0),
        ("clip_trigger_mode", 0.0),
        ("size", 3.0),
        ("anti_clump", 20.0),
        ("force", 0.005),
        ("container", 1.0), // Cube
        ("ctr_scale", 0.8),
        ("cam_dist", 3.0),
        ("rotate_x", 0.0),
        ("rotate_y", 0.0),
        ("rotate_z", 0.0),
        ("flatten", 0.0),
    ]
}

fn manifest(overrides: &[(&str, f32)]) -> ParamManifest {
    let mut pairs = base_params();
    for (id, v) in overrides {
        if let Some(p) = pairs.iter_mut().find(|(pid, _)| pid == id) {
            p.1 = *v;
        }
    }
    ParamManifest::from_params(pairs.iter().map(|(id, v)| slot(id, *v)).collect())
}

fn ctx(time: f64, frame: i64) -> PresetContext {
    PresetContext {
        time,
        beat: time * 2.0, // 120 bpm
        dt: 1.0 / 60.0,
        width: W,
        height: H,
        output_width: W,
        output_height: H,
        aspect: W as f32 / H as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: frame,
        anim_progress: 0.0,
        trigger_count: 0,
    }
}

/// Readback → f32 luminance grid.
fn luminance(device: &Arc<GpuDevice>, tex: &manifold_gpu::GpuTexture) -> Vec<f32> {
    let bytes_per_row = W * 8;
    let total = u64::from(H * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("bias-readback");
    enc.copy_texture_to_buffer(tex, &buf, W, H, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared readback buffer");
    let raw: &[u16] =
        unsafe { std::slice::from_raw_parts(ptr as *const u16, (W * H * 4) as usize) };
    raw.chunks_exact(4)
        .map(|px| {
            let r = f16::from_bits(px[0]).to_f32();
            let g = f16::from_bits(px[1]).to_f32();
            let b = f16::from_bits(px[2]).to_f32();
            0.2126 * r + 0.7152 * g + 0.0722 * b
        })
        .collect()
}

/// Quadrant shares (TL, TR, BL, BR) of total luminance, in percent.
fn quadrant_shares(lum: &[f32]) -> [f32; 4] {
    let (hw, hh) = (W / 2, H / 2);
    let mut q = [0f64; 4];
    for y in 0..H {
        for x in 0..W {
            let v = lum[(y * W + x) as usize] as f64;
            let qi = (usize::from(y >= hh)) * 2 + usize::from(x >= hw);
            q[qi] += v;
        }
    }
    let total: f64 = q.iter().sum::<f64>().max(1e-9);
    [
        (q[0] / total * 100.0) as f32,
        (q[1] / total * 100.0) as f32,
        (q[2] / total * 100.0) as f32,
        (q[3] / total * 100.0) as f32,
    ]
}

fn save_png(lum: &[f32], path: &str) {
    let mut img = image::GrayImage::new(W, H);
    for y in 0..H {
        for x in 0..W {
            let v = lum[(y * W + x) as usize].max(0.0);
            let t = v / (1.0 + v); // reinhard-ish so hot spots stay visible
            img.put_pixel(x, y, image::Luma([(t * 255.0) as u8]));
        }
    }
    img.save(path).expect("write png");
}

/// BUG-066 next-step (1b): the momentum meter. Read back every dumped
/// per-particle `Array` output and print per-component means over the live
/// particles — a nonzero mean on a force array IS the conservation break,
/// visible in one frame instead of a 900-frame drift, and comparing arrays
/// down the force chain attributes it to a stage. Run the whole harness a
/// second time with `MANIFOLD_FREEZE=0` to compare fused vs unfused executor
/// schedules (BUG-066 suspect 2). Requires `set_dump_all(true)` before the
/// metered frame's render.
fn force_meter(device: &Arc<GpuDevice>, runtime: &PresetRuntime, tag: &str) {
    let arrays = runtime.dump_arrays_all();

    let read_back = |a: &manifold_renderer::compositor::ArrayDump<'_>| -> Vec<u8> {
        let size = a.buffer.size();
        let staging = device.create_buffer_shared(size);
        let mut enc = device.create_encoder("meter-readback");
        enc.copy_buffer_to_buffer(a.buffer, &staging, size);
        enc.commit_and_wait_completed();
        let ptr = staging.mapped_ptr().expect("shared staging buffer");
        unsafe { std::slice::from_raw_parts(ptr, size as usize) }.to_vec()
    };

    // The particle-state array (the one with a `life` field) provides the
    // live-index mask; the coincident per-particle arrays share its index
    // space, and entries past the live set are uninitialised garbage.
    let mut mask: Option<Vec<bool>> = None;
    for a in &arrays {
        if let Some((_, _, off)) = a.fields.iter().find(|(n, _, _)| n == "life") {
            let bytes = read_back(a);
            let stride = a.item_size as usize;
            let n = if stride == 0 { 0 } else { bytes.len() / stride };
            mask = Some(
                (0..n)
                    .map(|i| {
                        let o = i * stride + *off as usize;
                        f32::from_le_bytes(bytes[o..o + 4].try_into().unwrap()) > 0.0
                    })
                    .collect(),
            );
            break;
        }
    }

    for a in &arrays {
        let stride = a.item_size as usize;
        if stride == 0 {
            continue;
        }
        let bytes = read_back(a);
        let n = bytes.len() / stride;
        // Per-field per-component stats. Array<[f32;3]> decomposes to three
        // scalar f32 fields (x, y, z), NOT one vec3f — cover every numeric
        // kind or the force buffers are silently skipped.
        for (fname, kind, off) in &a.fields {
            let comps = match *kind {
                "vec2f" => 2,
                "vec3f" => 3,
                "vec4f" => 4,
                "f32" => 1,
                _ => continue, // i32/u32 ids carry no momentum information
            };
            let mut sum = vec![0f64; comps];
            let mut max_abs = vec![0f64; comps];
            let mut live = 0u64;
            for i in 0..n {
                if let Some(m) = &mask
                    && !m.get(i).copied().unwrap_or(false)
                {
                    continue;
                }
                live += 1;
                let base = i * stride + *off as usize;
                for (c, s) in sum.iter_mut().enumerate() {
                    let o = base + c * 4;
                    let v = f64::from(f32::from_le_bytes(bytes[o..o + 4].try_into().unwrap()));
                    *s += v;
                    max_abs[c] = max_abs[c].max(v.abs());
                }
            }
            let d = live.max(1) as f64;
            let means: Vec<String> = sum.iter().map(|s| format!("{:+.3e}", s / d)).collect();
            let maxes: Vec<String> = max_abs.iter().map(|m| format!("{m:.2e}")).collect();
            eprintln!(
                "METER {tag} {}[{}].{} {fname}: mean [{}] max|.| [{}] live {live}",
                a.type_id,
                a.name,
                a.port,
                means.join(" "),
                maxes.join(" "),
            );
        }
    }
}

#[test]
#[ignore = "BUG-066 investigation tool, not a regression gate — run with --ignored"]
fn fluid3d_force_bias_bisection() {
    std::fs::create_dir_all(OUT_DIR).expect("scratch dir");
    let device = Arc::new(GpuDevice::new());
    let registry = PrimitiveRegistry::with_builtin();
    let json = manifold_renderer::node_graph::bundled_preset_json(&PresetTypeId::new("FluidSim3D"))
        .expect("FluidSim3D bundled");

    // Edit this matrix per investigation run. Key past results (2026-07-07, at
    // 512², cube container, 900 frames — see BUG-066 for the full story):
    //   default            → TR-heavy, wanders (turbulence tide + slope drift)
    //   all_field_off (turb 0, curl 0, flow 0) → symmetric ≈25% each (baseline OK)
    //   slope_only (turb 0, curl 0)            → TR 33–37%, voxel shelves
    //   slope + flow +0.01 (sign flip)         → mirrors to BL (sign-following)
    //   slope + rotate_y 1.0 (camera 180°)     → mirrors on screen (sim-space)
    //   slope + feather 4                      → bias GONE; feather 40 → TR 50%
    // Turb-detail sweep (2026-07-10): FULL default look (turbulence + curl +
    // slope all on) at noise lattice frequencies 2 (the legacy baked constant,
    // the quadrant-anatomy "before") through 12. Judge the PNGs by eye; the
    // meter rows quantify the wandering-tide shrink as detail rises.
    let scenarios: &[(&str, &[(&str, f32)])] = &[
        ("detail2_legacy", &[("turb_detail", 2.0)]),
        ("detail4", &[("turb_detail", 4.0)]),
        ("detail6", &[("turb_detail", 6.0)]),
        ("detail8", &[("turb_detail", 8.0)]),
        ("detail12", &[("turb_detail", 12.0)]),
    ];

    for (name, overrides) in scenarios {
        let mut runtime = PresetRuntime::from_json_str_with_device(
            &json,
            &registry,
            &device,
            W,
            H,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .unwrap_or_else(|e| panic!("FluidSim3D failed to build: {e}"));

        let params = manifest(overrides);
        let target = RenderTarget::new(&device, W, H, GpuTextureFormat::Rgba16Float, "bias-target");

        for frame in 0..FRAMES {
            let f = frame + 1;
            // Meter early (f30, before pooling develops) and at checkpoints.
            let meter = f == 30 || f % CHECKPOINT == 0;
            if meter {
                runtime.set_dump_all(true);
            }
            let c = ctx(frame as f64 / 60.0, frame as i64);
            let mut enc = device.create_encoder("bias-frame");
            {
                let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                runtime.render(&mut gpu, &target.texture, &c, &params);
            }
            enc.commit_and_wait_completed();
            if meter {
                force_meter(&device, &runtime, &format!("{name} f{f}"));
                runtime.set_dump_all(false);
            }

            if f % CHECKPOINT == 0 {
                let lum = luminance(&device, &target.texture);
                let q = quadrant_shares(&lum);
                eprintln!(
                    "{name:>15} f{f:>4}: TL {:5.1}%  TR {:5.1}%  BL {:5.1}%  BR {:5.1}%",
                    q[0], q[1], q[2], q[3]
                );
                save_png(&lum, &format!("{OUT_DIR}/{name}_f{f}.png"));
            }
        }
    }
}
