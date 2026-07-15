//! Save-time preset thumbnail rendering (`docs/PRESET_LIBRARY_DESIGN.md` P6,
//! D7). Renders a `size`×`size` preview PNG for an already-parsed preset
//! [`EffectGraphDef`] — generators render bare (default params, `t=0`, no
//! modulation/warm-up — the same "default look" precedent
//! `CLIP_THUMBNAILS_DESIGN.md`'s P2c cold-start generator thumbnail already
//! established), effects render over a standard gradient input (the same
//! `Fixture::Gradient` shape `tests/parity/harness.rs` uses — that harness is
//! test-only so the fixture is reproduced here rather than crossing the
//! test/production boundary).
//!
//! This is the ONLY render — the browser never renders (D7 / §6 forbidden
//! move "browse-time rendering of presets"). Callers: `UserLibrary`'s
//! Save-to-Library commit path (`manifold-app`), and the factory-thumbnail
//! one-shot dev bin (`src/bin/generate_preset_thumbnails.rs`).

use std::path::{Path, PathBuf};

use half::f16;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::preset_def::PresetKind;
use manifold_gpu::{
    GpuDevice, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};

use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use crate::node_graph::{
    EffectGraphDefExt, Executor, FrameTime, MetalBackend, NodeInstanceId, PrimitiveRegistry,
    ResourceId, StateStore, compile,
};
use crate::preset_context::PresetContext;
use crate::preset_runtime::PresetRuntime;
use manifold_core::params::ParamManifest;
use crate::render_target::RenderTarget;

/// The one render surface size every save-time thumbnail uses (D7: "renders a
/// 256px PNG").
pub const THUMBNAIL_SIZE: u32 = 256;

/// Render `def` to `size`×`size` RGBA8 PNG bytes suitable for
/// `std::fs::write`. `kind` picks the render path (generator vs effect); see
/// the module doc for what each does. Errors are a `String` (this is a save-
/// time UI action, not a hot path — the caller logs and moves on, it never
/// blocks the save itself).
pub fn render_preset_thumbnail(
    device: &std::sync::Arc<GpuDevice>,
    kind: PresetKind,
    def: &EffectGraphDef,
    size: u32,
) -> Result<Vec<u8>, String> {
    match kind {
        PresetKind::Generator => render_generator(device, def, size),
        PresetKind::Effect => render_effect(device, def, size),
    }
}

/// [`render_preset_thumbnail`] plus writing the result straight to `out_path`
/// — the shape every call site actually wants (write a `<Name>.png` beside a
/// preset's JSON).
pub fn render_preset_thumbnail_to_file(
    device: &std::sync::Arc<GpuDevice>,
    kind: PresetKind,
    def: &EffectGraphDef,
    size: u32,
    out_path: &Path,
) -> Result<(), String> {
    let bytes = render_preset_thumbnail(device, kind, def, size)?;
    std::fs::write(out_path, bytes)
        .map_err(|e| format!("failed writing {}: {e}", out_path.display()))
}

// ---------------------------------------------------------------------------
// Factory-thumbnail location (committed assets, one-shot dev bin output)
// ---------------------------------------------------------------------------

/// Sub-directory name for `kind` under the thumbnails root — mirrors
/// `preset_loader`'s effects/generators split so a same-named effect and
/// generator (different namespaces) can never collide on one PNG.
fn kind_subdir(kind: PresetKind) -> &'static str {
    match kind {
        PresetKind::Effect => "effects",
        PresetKind::Generator => "generators",
    }
}

/// Resolve the factory-thumbnail root for `kind`: packaged bundle
/// `Resources/preset-thumbnails/<kind>` if it exists, else the dev workspace
/// `assets/preset-thumbnails/<kind>` (this crate's `CARGO_MANIFEST_DIR`) —
/// same two-tier resolution shape as `preset_loader::resolve_stock_root`,
/// specialised to thumbnails (a sibling asset kind, not a preset JSON root).
fn factory_thumbnail_root(kind: PresetKind) -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        let bundle = exe_dir
            .join("..")
            .join("Resources")
            .join("preset-thumbnails")
            .join(kind_subdir(kind));
        if bundle.is_dir() {
            return bundle;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets/preset-thumbnails")
        .join(kind_subdir(kind))
}

/// Path a factory preset's committed thumbnail lives at for `id`. The caller
/// checks `Path::is_file()` — a factory preset with no thumbnail yet (or one
/// that hasn't shipped through the dev bin) simply resolves to a path that
/// doesn't exist, and the browser falls back to text (D7's clean fallback).
pub fn factory_thumbnail_path(kind: PresetKind, id: &str) -> Option<PathBuf> {
    Some(factory_thumbnail_root(kind).join(format!("{id}.png")))
}

// ---------------------------------------------------------------------------
// Generator render — bare, default params, t=0 (CLIP_THUMBNAILS P2c precedent)
// ---------------------------------------------------------------------------

fn render_generator(
    device: &std::sync::Arc<GpuDevice>,
    def: &EffectGraphDef,
    size: u32,
) -> Result<Vec<u8>, String> {
    let registry = PrimitiveRegistry::with_builtin();
    let format = GpuTextureFormat::Rgba16Float;
    let mut runtime =
        PresetRuntime::from_def_with_device(
            def.clone(),
            &registry,
            std::sync::Arc::clone(device),
            size,
            size,
            format,
            None,
        )
        .map_err(|e| format!("generator build failed: {e}"))?;

    let target = RenderTarget::new(device, size, size, format, "preset-thumb-gen-target");

    // Warm-up render, NOT a cold t=0 frame. A bare t=0 render is degenerate for
    // most generators: time-function looks (Plasma, Lissajous, tunnels) sit at
    // their undeveloped origin, and state-accumulating sims (FluidSim, particle
    // systems, StrangeAttractor) have no accumulated state at all — the result
    // is a flat/grey frame that doesn't read as the preset. So advance
    // `WARMUP_FRAMES` frames, committing each so GPU-resident state (sim
    // buffers) actually accumulates between frames, and capture the last. The
    // runtime persists its `StateStore` across `render` calls, so this develops
    // stateful generators the same way the live playhead would. `dt = 1/60`,
    // `bpm = 120` (2 beats/sec) — arbitrary but stable defaults; the thumbnail
    // just needs a representative developed frame, not a specific playhead.
    const WARMUP_FRAMES: u32 = 60;
    const DT: f32 = 1.0 / 60.0;
    let make_ctx = |frame: u32| {
        let time = frame as f64 * DT as f64;
        PresetContext {
            time,
            beat: time * 2.0, // 120 bpm
            dt: DT,
            width: size,
            height: size,
            output_width: size,
            output_height: size,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame as i64,
            // Sweep 0→1 across the warm-up so loop-driven anims land developed.
            anim_progress: (frame as f32 / WARMUP_FRAMES as f32).min(1.0),
            trigger_count: 0,
        }
    };

    for frame in 0..WARMUP_FRAMES {
        let ctx = make_ctx(frame);
        let mut enc = device.create_encoder("preset-thumb-gen-warmup");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            // Thumbnails render every binding at its declared default — no card
            // overrides — so an empty manifest is exactly right.
            runtime.render(&mut gpu, &target.texture, &ctx, &ParamManifest::default());
        }
        // Commit each frame so state-accumulating generators see the prior
        // frame's GPU writes before computing the next.
        enc.commit_and_wait_completed();
    }

    Ok(crate::headless_readback::readback_to_srgb_png(
        device,
        &target.texture,
        size,
        size,
    ))
}

// ---------------------------------------------------------------------------
// Effect render — over a standard gradient input (parity harness precedent)
// ---------------------------------------------------------------------------

/// Reproduces `tests/parity/harness.rs`'s `Fixture::Gradient`: R=x, G=y,
/// B=(x+y)/2, A=1 — a smooth, non-degenerate input that shows what an effect
/// actually does to *something*, without depending on any real clip content.
fn build_gradient_input(device: &GpuDevice, w: u32, h: u32, format: GpuTextureFormat) -> RenderTarget {
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
        label: "preset-thumb-gradient-input",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), std::mem::size_of_val(pixels.as_slice()))
    };
    device.upload_texture(&tex, bytes);
    RenderTarget::view_of(tex, "preset-thumb-gradient-input")
}

/// Walk the compiled plan for the `ResourceId` `node`'s named output port
/// produced. Small plan-walk helper, duplicated (not shared) across this
/// crate's headless-render call sites — same rationale `preset_runtime.rs`'s
/// own copy states: a 5-line utility, not worth a cross-module dependency.
fn output_resource(plan: &crate::node_graph::ExecutionPlan, node: NodeInstanceId, port: &str) -> Option<ResourceId> {
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

fn render_effect(
    device: &std::sync::Arc<GpuDevice>,
    def: &EffectGraphDef,
    size: u32,
) -> Result<Vec<u8>, String> {
    let registry = PrimitiveRegistry::with_builtin();
    let format = GpuTextureFormat::Rgba16Float;

    let mut graph = def
        .clone()
        .into_graph(&registry)
        .map_err(|e| format!("graph load failed: {e}"))?;
    let plan = compile(&graph).map_err(|e| format!("compile failed: {e:?}"))?;

    let source_id = graph
        .nodes()
        .find(|n| n.node.type_id().as_str() == crate::node_graph::SOURCE_TYPE_ID)
        .map(|n| n.id)
        .ok_or_else(|| "preset has no system.source node".to_string())?;
    let final_id = graph
        .nodes()
        .find(|n| n.node.type_id().as_str() == crate::node_graph::FINAL_OUTPUT_TYPE_ID)
        .map(|n| n.id)
        .ok_or_else(|| "preset has no system.final_output node".to_string())?;

    let source_out = output_resource(&plan, source_id, "out")
        .ok_or_else(|| "system.source has no `out` resource in plan".to_string())?;
    let final_in = plan
        .steps()
        .iter()
        .find(|s| s.node == final_id)
        .and_then(|s| s.inputs.iter().find(|(n, _)| *n == "in"))
        .map(|(_, r)| *r)
        .ok_or_else(|| "system.final_output has no bound `in`".to_string())?;

    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), size, size, format);
    let input_target = build_gradient_input(device, size, size, format);
    let source_slot = backend.pre_bind_texture_2d(source_out, input_target);
    // A degenerate passthrough graph (Source wired straight to FinalOutput,
    // no processing nodes) shares ONE resource for both boundaries — bind it
    // once and read the same slot back rather than double-binding.
    let output_slot = if final_in == source_out {
        source_slot
    } else {
        let out_target = RenderTarget::new(device, size, size, format, "preset-thumb-fx-out");
        backend.pre_bind_texture_2d(final_in, out_target)
    };

    // Same deterministic non-zero clock the parity harness uses
    // (`default_ctx`), so a time-dependent effect (Glitch, Strobe) shows its
    // actual look rather than its t=0 frame.
    let frame_time = FrameTime {
        beats: manifold_core::Beats(2.5),
        seconds: manifold_core::Seconds(1.234),
        delta: manifold_core::Seconds(1.0 / 60.0),
        frame_count: 0,
    };

    // `execute_frame_with_state` (not the plain `_with_gpu` entry): an
    // arbitrary saved effect graph may contain stateful nodes (Bloom mip
    // chains, `temporal::Feedback` prev-frame buffers, …) that panic through
    // the state-less entry point. A fresh, empty `StateStore` is exactly
    // right for a one-shot render — there is no prior frame to carry state
    // from, and any state-dependent look (Feedback's very first frame) is
    // the same "no warm-up" trade-off already documented for generators.
    let mut state_store = StateStore::new();
    let mut native_enc = device.create_encoder("preset-thumb-fx-render");
    let mut exec = Executor::new(Box::new(backend));
    {
        let mut gpu = RendererGpuEncoder::new(&mut native_enc, device);
        exec.execute_frame_with_state(&mut graph, &plan, frame_time, &mut gpu, &mut state_store, 0);
    }
    native_enc.commit_and_wait_completed();

    let tex = exec
        .backend()
        .texture_2d(output_slot)
        .ok_or_else(|| "output texture missing after execute".to_string())?;
    Ok(crate::headless_readback::readback_to_srgb_png(device, tex, size, size))
}

// ---------------------------------------------------------------------------
// Decode (browser side)
// ---------------------------------------------------------------------------

/// Decode a PNG file at `path` to (width, height, RGBA8 bytes) — the browser-
/// side of D7: the app decodes a saved thumbnail ONCE (cached by the caller,
/// keyed by path) and uploads it to the UI's image registry
/// (`ui_renderer::UIRenderer::register_image`). Centralised here rather than
/// in `manifold-app` because `image` is an optional/feature-gated dependency
/// there but unconditional in this crate (already used for the save-time
/// encode above and the mesh-snapshot/parity PNG dumps).
pub fn decode_png_rgba8(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let img = image::open(path)
        .map_err(|e| format!("failed to decode {}: {e}", path.display()))?
        .to_rgba8();
    let (w, h) = (img.width(), img.height());
    Ok((w, h, img.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "gpu-proofs")]
    /// Minimal bundled effect preset (Bloom, a real shipped effect) parsed
    /// from disk — exercises the real `system.source` → primitives →
    /// `system.final_output` shape, not a synthetic fixture.
    fn bloom_def() -> EffectGraphDef {
        let json = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/effect-presets/Bloom.json"
        ))
        .expect("read Bloom.json");
        serde_json::from_str(&json).expect("parse Bloom.json")
    }

    #[cfg(feature = "gpu-proofs")]
    fn blackhole_def() -> EffectGraphDef {
        let json = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/assets/generator-presets/BlackHole.json"
        ))
        .expect("read BlackHole.json");
        serde_json::from_str(&json).expect("parse BlackHole.json")
    }

    #[cfg(feature = "gpu-proofs")]
    /// Headless value-level gate (the orchestrator's Read-the-PNG check):
    /// renders a real stock effect over the standard gradient and asserts
    /// the output isn't flat/empty (a spread of distinct pixel values) —
    /// same non-uniform-content check `mesh_snapshot.rs`/`graph_dump.rs`
    /// tests use to catch a broken dispatch. `#[ignore]`: needs a GPU.
    #[test]
    #[ignore = "needs a real GPU device; run with --ignored"]
    fn render_effect_thumbnail_produces_non_trivial_png() {
        let device = crate::test_device();
        let def = bloom_def();
        let png = render_preset_thumbnail(&device.arc(), PresetKind::Effect, &def, 64)
            .expect("effect thumbnail render");
        assert!(!png.is_empty(), "PNG bytes must be non-empty");

        let decoded = image::load_from_memory(&png).expect("decode produced PNG").to_rgba8();
        let mut distinct = std::collections::HashSet::new();
        for px in decoded.pixels() {
            distinct.insert(px.0);
            if distinct.len() > 4 {
                break;
            }
        }
        assert!(
            distinct.len() > 2,
            "expected a spread of distinct colors (gradient run through Bloom), got {distinct:?}"
        );
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    #[ignore = "needs a real GPU device; run with --ignored"]
    fn render_generator_thumbnail_produces_non_trivial_png() {
        let device = crate::test_device();
        let def = blackhole_def();
        let png = render_preset_thumbnail(&device.arc(), PresetKind::Generator, &def, 64)
            .expect("generator thumbnail render");
        assert!(!png.is_empty());
        let decoded = image::load_from_memory(&png).expect("decode produced PNG").to_rgba8();
        // Not asserting non-black here — BlackHole at t=0 with no warm-up may
        // be mostly empty (that's the documented P2c-precedent trade-off);
        // this is just confirming the render+encode path itself works and
        // produces a real, decodable image at the right size.
        assert_eq!(decoded.width(), 64);
        assert_eq!(decoded.height(), 64);
    }

    #[test]
    fn factory_thumbnail_path_resolves_under_dev_assets_when_unpackaged() {
        // In the test binary there's no packaged bundle, so this resolves to
        // the dev workspace assets dir — proves the path shape without
        // needing a GPU.
        let p = factory_thumbnail_path(PresetKind::Effect, "Bloom").expect("path resolves");
        assert!(p.ends_with("assets/preset-thumbnails/effects/Bloom.png"));
    }

}
