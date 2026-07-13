//! BUG-098 regression proof — FilmGrain must re-roll every frame instead of
//! panning, and must not read as blocky pixels at 4K.
//!
//! Renders the bundled `FilmGrain.json` preset over a static gradient input
//! (same fixture `preset_thumbnail.rs`'s effect-thumbnail path uses) at two
//! DIFFERENT canvas sizes (1080p and 4K) for two consecutive frames each
//! (`frame_count` N and N+1, everything else held fixed — same time/beat so
//! the ONLY thing that can change the output is the grain layer). Asserts
//! the normalized cross-correlation between the two frames' luma is low
//! (decorrelated grain), where the pre-fix graph (continuous time-based pan)
//! would have measured near-1.0 correlation between adjacent frames.
//!
//! Also dumps both 4K frames as PNGs (target/gpu_proofs_out/) for a human
//! look — the softness/blockiness call is Peter's on the real rig, this test
//! only proves the re-roll is real and gives him something to glance at.

use std::path::PathBuf;

use half::f16;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_gpu::{
    GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::{
    EffectGraphDefExt, Executor, FrameTime, MetalBackend, NodeInstanceId, PrimitiveRegistry,
    ResourceId, StateStore, compile,
};
use manifold_renderer::render_target::RenderTarget;

use crate::harness;

/// Same fixture `preset_thumbnail::build_gradient_input` uses — reproduced
/// here (test-only) rather than exporting a production helper just for this.
fn build_gradient_input(
    device: &manifold_gpu::GpuDevice,
    w: u32,
    h: u32,
    format: GpuTextureFormat,
) -> RenderTarget {
    let mut pixels = vec![f16::from_f32(0.0); (w * h * 4) as usize];
    let wm = (w.max(1) - 1).max(1) as f32;
    let hm = (h.max(1) - 1).max(1) as f32;
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            let u = x as f32 / wm;
            let v = y as f32 / hm;
            pixels[idx] = f16::from_f32(u);
            pixels[idx + 1] = f16::from_f32(v);
            pixels[idx + 2] = f16::from_f32((u + v) * 0.5);
            pixels[idx + 3] = f16::from_f32(1.0);
        }
    }
    let tex = device.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
        label: "film-grain-decorrelation-gradient-input",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), std::mem::size_of_val(pixels.as_slice()))
    };
    device.upload_texture(&tex, bytes);
    RenderTarget::view_of(tex, "film-grain-decorrelation-gradient-input")
}

fn output_resource(plan: &manifold_renderer::node_graph::ExecutionPlan, node: NodeInstanceId, port: &str) -> Option<ResourceId> {
    for step in plan.steps() {
        if step.node == node {
            for &(name, id) in &step.outputs {
                if name == port {
                    return Some(id);
                }
            }
        }
    }
    None
}

/// Raw linear-light (Rgba16Float, non-tonemapped) readbacks from one
/// `FilmGrain` render: the isolated grain layer (`grain_mono`'s output,
/// BEFORE it's blended over the source) for correlation math, and the final
/// composited output (what actually ships to screen) for the visual PNG
/// dump. The composited output is dominated by the static gradient fixture
/// (identical every frame) at Amount=0.35, so measuring decorrelation on
/// the full composite would mostly measure the unchanging source — the
/// grain layer is the right signal per BUG-098's own fix-shape ("cross-
/// correlation of the grain layer").
struct FilmGrainFrame {
    grain: Vec<[f32; 4]>,
    composited: Vec<[f32; 4]>,
}

fn render_film_grain_frame(w: u32, h: u32, frame_count: i64) -> FilmGrainFrame {
    let h_ctx = harness::shared();
    let device = &h_ctx.device;
    let format = GpuTextureFormat::Rgba16Float;

    let json_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets/effect-presets/FilmGrain.json");
    let json = std::fs::read_to_string(&json_path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", json_path.display()));
    let def: EffectGraphDef =
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("FilmGrain.json parse failed: {e}"));

    let registry = PrimitiveRegistry::with_builtin();
    let mut graph = def
        .into_graph(&registry)
        .unwrap_or_else(|e| panic!("FilmGrain graph load failed: {e}"));
    let plan = compile(&graph).unwrap_or_else(|e| panic!("FilmGrain compile failed: {e:?}"));

    let source_id = graph
        .nodes()
        .find(|n| n.node.type_id().as_str() == manifold_renderer::node_graph::SOURCE_TYPE_ID)
        .map(|n| n.id)
        .expect("FilmGrain has a system.source node");
    let final_id = graph
        .nodes()
        .find(|n| n.node.type_id().as_str() == manifold_renderer::node_graph::FINAL_OUTPUT_TYPE_ID)
        .map(|n| n.id)
        .expect("FilmGrain has a system.final_output node");

    // JSON stores the legacy alias "node.channel_mix"; `type_id_migration`
    // rewrites it to the canonical "node.channel_mixer" at load time, so the
    // loaded node reports that string.
    let grain_mono_id = graph
        .nodes()
        .find(|n| n.node.type_id().as_str() == "node.channel_mixer")
        .map(|n| n.id)
        .expect("FilmGrain has a node.channel_mixer (grain_mono) node");

    let source_out = output_resource(&plan, source_id, "out").expect("source has an `out` resource");
    let final_in = plan
        .steps()
        .iter()
        .find(|s| s.node == final_id)
        .and_then(|s| s.inputs.iter().find(|(n, _)| *n == "in"))
        .map(|(_, r)| *r)
        .expect("final_output has a bound `in`");
    let grain_out = output_resource(&plan, grain_mono_id, "out").expect("grain_mono has an `out` resource");

    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), w, h, format);
    let input_target = build_gradient_input(device, w, h, format);
    let source_slot = backend.pre_bind_texture_2d(source_out, input_target);
    let output_slot = if final_in == source_out {
        source_slot
    } else {
        let out_target = RenderTarget::new(device, w, h, format, "film-grain-decorrelation-out");
        backend.pre_bind_texture_2d(final_in, out_target)
    };
    // Pre-bind (pin) grain_mono's output too, otherwise it's an ordinary
    // pooled intermediate resource that the backend recycles once grain_mix
    // consumes it — unreadable after the frame commits.
    let grain_slot = if grain_out == source_out {
        source_slot
    } else if grain_out == final_in {
        output_slot
    } else {
        let grain_target = RenderTarget::new(device, w, h, format, "film-grain-decorrelation-grain");
        backend.pre_bind_texture_2d(grain_out, grain_target)
    };

    // Same time/beat every call — frame_count is the ONLY thing that
    // changes between the two renders under test, isolating the grain
    // re-roll mechanism from any other time-driven behavior.
    let frame_time = FrameTime {
        beats: manifold_core::Beats(2.5),
        seconds: manifold_core::Seconds(1.234),
        delta: manifold_core::Seconds(1.0 / 60.0),
        frame_count,
    };

    let mut state_store = StateStore::new();
    let mut native_enc = device.create_encoder("film-grain-decorrelation-render");
    let mut exec = Executor::new(Box::new(backend));
    {
        let mut gpu = RendererGpuEncoder::new(&mut native_enc, device);
        exec.execute_frame_with_state(&mut graph, &plan, frame_time, &mut gpu, &mut state_store, 0);
    }
    native_enc.commit_and_wait_completed();

    let composited_tex = exec
        .backend()
        .texture_2d(output_slot)
        .expect("output texture bound after execute");
    let composited = readback_rgba_f32(device, composited_tex, w, h);

    let grain_tex = exec
        .backend()
        .texture_2d(grain_slot)
        .expect("grain_mono output texture materialized after execute");
    let grain = readback_rgba_f32(device, grain_tex, w, h);

    FilmGrainFrame { grain, composited }
}

fn readback_rgba_f32(device: &manifold_gpu::GpuDevice, tex: &GpuTexture, w: u32, h: u32) -> Vec<[f32; 4]> {
    let bytes_per_row = w * 8; // Rgba16Float = 8 bytes/pixel
    let total = u64::from(h * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("film-grain-decorrelation-readback");
    enc.copy_texture_to_buffer(tex, &buf, w, h, bytes_per_row);
    enc.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer must expose mapped pointer");
    let halves: &[u16] = unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

    halves
        .chunks_exact(4)
        .map(|px| {
            [
                f16::from_bits(px[0]).to_f32(),
                f16::from_bits(px[1]).to_f32(),
                f16::from_bits(px[2]).to_f32(),
                f16::from_bits(px[3]).to_f32(),
            ]
        })
        .collect()
}

fn luma(px: &[f32; 4]) -> f32 {
    0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2]
}

/// Pearson correlation coefficient between two equal-length luma buffers.
fn pearson_correlation(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len() as f64;
    let mean_a = a.iter().map(|&v| v as f64).sum::<f64>() / n;
    let mean_b = b.iter().map(|&v| v as f64).sum::<f64>() / n;
    let mut cov = 0.0f64;
    let mut var_a = 0.0f64;
    let mut var_b = 0.0f64;
    for (&x, &y) in a.iter().zip(b.iter()) {
        let dx = x as f64 - mean_a;
        let dy = y as f64 - mean_b;
        cov += dx * dy;
        var_a += dx * dx;
        var_b += dy * dy;
    }
    if var_a <= 0.0 || var_b <= 0.0 {
        return 0.0;
    }
    (cov / (var_a.sqrt() * var_b.sqrt())) as f32
}

fn tonemap_and_save_png(pixels: &[[f32; 4]], w: u32, h: u32, path: &std::path::Path) {
    let tonemap = |v: f32| -> u8 { ((v / (1.0 + v)).clamp(0.0, 1.0) * 255.0).round() as u8 };
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for px in pixels {
        let a = px[3].clamp(0.0, 1.0);
        out.push(tonemap(px[0] * a));
        out.push(tonemap(px[1] * a));
        out.push(tonemap(px[2] * a));
        out.push(255);
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    image::save_buffer(path, &out, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("failed to write {}: {e}", path.display()));
}

/// BUG-098: consecutive frames of FilmGrain must be decorrelated (a fresh
/// re-roll), not a slow pan of the same hash lattice. Checked at both 1080p
/// and 4K — the pre-fix bug's continuous `time * const -> offset` pan gave
/// near-1.0 correlation between adjacent frames at any resolution; a proper
/// per-frame re-roll should land close to 0.
#[test]
fn consecutive_frames_are_decorrelated_at_1080p_and_4k() {
    for &(w, h, label) in &[(1920u32, 1080u32, "1080p"), (3840u32, 2160u32, "4k")] {
        let frame_a = render_film_grain_frame(w, h, 500);
        let frame_b = render_film_grain_frame(w, h, 501);
        assert_eq!(frame_a.grain.len(), frame_b.grain.len());

        let luma_a: Vec<f32> = frame_a.grain.iter().map(luma).collect();
        let luma_b: Vec<f32> = frame_b.grain.iter().map(luma).collect();
        let corr = pearson_correlation(&luma_a, &luma_b);

        println!("[film_grain_decorrelation] {label}: frame 500 vs 501 grain-layer correlation = {corr:.4}");

        assert!(
            corr.abs() < 0.15,
            "FilmGrain frames 500/501 at {label} are still correlated ({corr:.4}) — \
             looks like the offset is panning instead of re-rolling per frame (BUG-098)"
        );

        if label == "4k" {
            let out_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/gpu_proofs_out");
            tonemap_and_save_png(&frame_a.composited, w, h, &out_dir.join("film_grain_4k_frame500.png"));
            tonemap_and_save_png(&frame_b.composited, w, h, &out_dir.join("film_grain_4k_frame501.png"));
            tonemap_and_save_png(&frame_a.grain, w, h, &out_dir.join("film_grain_4k_frame500_grain_only.png"));
            println!(
                "[film_grain_decorrelation] wrote {}",
                out_dir.join("film_grain_4k_frame500.png").display()
            );
        }
    }
}
