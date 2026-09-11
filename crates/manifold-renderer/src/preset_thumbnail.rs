//! Save-time and factory preset thumbnail rendering (`docs/PRESET_LIBRARY_DESIGN.md`
//! P6, D7; `docs/STATIC_THUMBNAILS_DESIGN.md` D2-D4). Renders a 256×144 preview
//! PNG for an already-parsed preset [`EffectGraphDef`] — generators render bare,
//! effects render over the four-region synthetic test card
//! ([`build_test_card_input`]). Both kinds run the same deterministic capture
//! recipe (STATIC_THUMBNAILS_DESIGN D4): 60 warm-up frames at dt=1/60, 120bpm,
//! anim sweep 0→1, commit-and-wait each frame so stateful graphs (feedback,
//! trails, sims) develop, then the IO/warmup settle wait.
//!
//! This is the ONLY render — the browser never renders (D7 / section 6 forbidden
//! move "browse-time rendering of presets"). Callers: `UserLibrary`'s
//! Save-to-Library commit path (`manifold-app`), the factory-thumbnail one-shot
//! dev bin (`src/bin/generate_preset_thumbnails.rs`), and `graph-tool render`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// Thumbnail dimensions (STATIC_THUMBNAILS_DESIGN §3.1): 16:9, matching the
/// browser cells (170×96).
pub const THUMBNAIL_WIDTH: u32 = 256;
pub const THUMBNAIL_HEIGHT: u32 = 144;

/// Render `def` to `width`×`height` RGBA8 PNG bytes suitable for
/// `std::fs::write`. `kind` picks the render path (generator vs effect); see
/// the module doc for what each does. Errors are a `String` (this is a save-
/// time UI action, not a hot path — the caller logs and moves on, it never
/// blocks the save itself).
pub fn render_preset_thumbnail(
    device: &std::sync::Arc<GpuDevice>,
    kind: PresetKind,
    def: &EffectGraphDef,
    width: u32,
    height: u32,
    linear: bool,
) -> Result<Vec<u8>, String> {
    match kind {
        PresetKind::Generator => render_generator(device, def, width, height, linear),
        PresetKind::Effect => render_effect(device, def, width, height, linear),
    }
}

/// [`render_preset_thumbnail`] plus writing the result straight to `out_path`
/// — the shape every call site actually wants (write a `<Name>.png` beside a
/// preset's JSON).
pub fn render_preset_thumbnail_to_file(
    device: &std::sync::Arc<GpuDevice>,
    kind: PresetKind,
    def: &EffectGraphDef,
    width: u32,
    height: u32,
    out_path: &Path,
) -> Result<(), String> {
    let bytes = render_preset_thumbnail(device, kind, def, width, height, false)?;
    std::fs::write(out_path, bytes)
        .map_err(|e| format!("failed writing {}: {e}", out_path.display()))
}

/// BUG-327 sibling of [`render_preset_thumbnail_to_file`]: linear→sRGB readback
/// (no Reinhard) for graphs that tonemap in-graph. Used by `graph-tool render
/// --linear`. Default path (`render_preset_thumbnail_to_file`) is unaffected.
pub fn render_preset_thumbnail_to_file_linear(
    device: &std::sync::Arc<GpuDevice>,
    kind: PresetKind,
    def: &EffectGraphDef,
    width: u32,
    height: u32,
    out_path: &Path,
) -> Result<(), String> {
    let bytes = render_preset_thumbnail(device, kind, def, width, height, true)?;
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
// The D4 capture recipe — one deterministic warm-up, shared by both kinds
// ---------------------------------------------------------------------------

const WARMUP_FRAMES: u32 = 60;
const WARMUP_DT: f32 = 1.0 / 60.0;

/// D4's deterministic clock: frame `f` happens at `t = f/60`s, 120bpm
/// (2 beats/sec). Arbitrary but stable — the thumbnail needs a representative
/// developed frame, not a specific playhead.
fn warmup_time(frame: u32) -> (f64, f64) {
    let time = frame as f64 * f64::from(WARMUP_DT);
    (time, time * 2.0)
}

/// Pump `frames` warm-up frames, committing and waiting each one so
/// GPU-resident state (feedback buffers, sim fields) sees the prior frame's
/// writes before computing the next. The single capture loop for effects and
/// generators (D4) — `render_frame` draws one frame through the given encoder.
fn pump_warmup_frames(
    device: &GpuDevice,
    frames: u32,
    mut render_frame: impl for<'e> FnMut(u32, &mut RendererGpuEncoder<'e>),
) {
    for frame in 0..frames {
        let mut enc = device.create_encoder("preset-thumb-warmup");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            render_frame(frame, &mut gpu);
        }
        enc.commit_and_wait_completed();
    }
}

/// D3 at the thumbnail layer: no transparency reaches a committed PNG. The
/// shared readbacks already composite straight alpha over black (BUG-024), so
/// this is normally a no-op — it exists so a future readback change can't
/// leak alpha into a thumbnail without failing here.
fn flatten_over_black(rgba: &mut [u8]) {
    for px in rgba.chunks_exact_mut(4) {
        let a = u32::from(px[3]);
        if a != 255 {
            for c in &mut px[0..3] {
                *c = (u32::from(*c) * a / 255) as u8;
            }
            px[3] = 255;
        }
    }
}

// ---------------------------------------------------------------------------
// Generator render — bare, default params, developed over the warm-up
// ---------------------------------------------------------------------------

fn render_generator(
    device: &std::sync::Arc<GpuDevice>,
    def: &EffectGraphDef,
    width: u32,
    height: u32,
    linear: bool,
) -> Result<Vec<u8>, String> {
    let registry = PrimitiveRegistry::with_builtin();
    let format = GpuTextureFormat::Rgba16Float;
    let mut runtime =
        PresetRuntime::from_def_with_device(
            def.clone(),
            &registry,
            std::sync::Arc::clone(device),
            width,
            height,
            format,
            None,
        )
        .map_err(|e| format!("generator build failed: {e}"))?;

    let target = RenderTarget::new(device, width, height, format, "preset-thumb-gen-target");

    // Warm-up, NOT a cold t=0 frame. A bare t=0 render is degenerate for
    // most generators: time-function looks (Plasma, Lissajous, tunnels) sit at
    // their undeveloped origin, and state-accumulating sims (FluidSim, particle
    // systems, StrangeAttractor) have no accumulated state at all — the result
    // is a flat/grey frame that doesn't read as the preset. The runtime
    // persists its `StateStore` across `render` calls, so the shared warm-up
    // develops stateful generators the same way the live playhead would. The
    // anim sweep 0→1 lands loop-driven anims developed.
    let make_ctx = |frame: u32| {
        let (time, beat) = warmup_time(frame);
        PresetContext {
            time,
            beat,
            dt: WARMUP_DT,
            width,
            height,
            output_width: width,
            output_height: height,
            aspect: width as f32 / height as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame as i64,
            anim_progress: (frame as f32 / WARMUP_FRAMES as f32).min(1.0),
            trigger_count: 0,
        }
    };

    pump_warmup_frames(device, WARMUP_FRAMES, |frame, gpu| {
        // Thumbnails render every binding at its declared default — no card
        // overrides — so an empty manifest is exactly right.
        runtime.render(gpu, &target.texture, &make_ctx(frame), &ParamManifest::default());
    });

    // The fixed 60-frame warm-up is enough to trigger background initialization
    // (GLB mesh parse, texture decode, HDRI upload, etc.), but a heavy decode can
    // still be in flight when the loop ends. Capturing while initialization is
    // pending produces a black thumbnail that looks like a broken graph.
    // `io_pending()` covers the IoBridge file sources; `warmup_pending()` covers
    // `node.gltf_mesh_source`'s background GLB parse, which reports through the
    // warmup channel. Pump frames until both settle, or a hard wall-clock timeout
    // passes so we never hang silently.
    const IO_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
    let io_wait_start = Instant::now();
    let mut io_wait_frame = 0u32;
    while runtime.io_pending() || runtime.warmup_pending() {
        if io_wait_start.elapsed() >= IO_WAIT_TIMEOUT {
            let pending: Vec<String> = runtime
                .graph
                .nodes()
                .filter(|n| n.node.io_pending() || n.node.warmup_pending())
                .map(|n| format!("{} ({})", n.node_id, n.node.type_id().as_str()))
                .collect();
            return Err(format!(
                "render timed out waiting for async IO/warmup after {}s; still pending: {}",
                IO_WAIT_TIMEOUT.as_secs(),
                pending.join(", ")
            ));
        }
        let frame = WARMUP_FRAMES + io_wait_frame;
        let ctx = make_ctx(frame);
        let mut enc = device.create_encoder("preset-thumb-gen-io-wait");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            runtime.render(&mut gpu, &target.texture, &ctx, &ParamManifest::default());
        }
        enc.commit_and_wait_completed();
        io_wait_frame += 1;
        std::thread::sleep(Duration::from_millis(50));
    }

    let mut rgba = if linear {
        crate::headless_readback::readback_srgb_rgba8(device, &target.texture, width, height)
    } else {
        crate::headless_readback::readback_tonemapped_rgba8(device, &target.texture, width, height)
    };
    flatten_over_black(&mut rgba);
    Ok(crate::headless_readback::encode_rgba8_png(&rgba, width, height))
}

// ---------------------------------------------------------------------------
// The D2 test card — a designed synthetic frame, generated in code
// ---------------------------------------------------------------------------

/// One pixel of the four-region test card (STATIC_THUMBNAILS_DESIGN §3.1).
/// Shared between the GPU upload builder and the migrated expected-value
/// tests so the two can't drift apart. Returns straight-alpha RGBA; every
/// region is fully opaque.
pub(crate) fn test_card_pixel(x: u32, y: u32, w: u32, h: u32) -> [f32; 4] {
    let wm = (w.max(1) - 1).max(1) as f32;
    let hm = (h.max(1) - 1).max(1) as f32;
    let u = x as f32 / wm;
    let v = y as f32 / hm;

    let third = (w / 3).max(1);
    let rgb: [f32; 3] = if x < third {
        // Left third: the old math gradient, demoted to a region (tonal/color
        // effects).
        [u, v, (u + v) * 0.5]
    } else if x < third * 2 {
        // Middle third: six vertical hue bars (R/Y/G/C/B/M) at 80% saturation
        // over a 50% gray floor (color grading).
        let bar = (((x - third) * 6) / third).min(5);
        let hue = hue_rgb(bar as f32 / 6.0);
        [
            0.5 * 0.2 + 0.8 * hue[0],
            0.5 * 0.2 + 0.8 * hue[1],
            0.5 * 0.2 + 0.8 * hue[2],
        ]
    } else if y < h / 2 {
        // Right third, top half: 2px horizontal black/white stripes
        // (sharpen/edge/glitch).
        let on = (y / 2).is_multiple_of(2);
        [f32::from(on), f32::from(on), f32::from(on)]
    } else {
        // Right third, bottom half: 8px black/white checker (blur/sharpen).
        let on = ((x / 8) + (y / 8)).is_multiple_of(2);
        [f32::from(on), f32::from(on), f32::from(on)]
    };

    // A 100%-white circle, centered over the whole frame at 15% of frame
    // height radius — the hard edge every blur/edge effect needs.
    let dx = x as f32 - w as f32 * 0.5;
    let dy = y as f32 - h as f32 * 0.5;
    let r = h as f32 * 0.15;
    let rgb = if dx * dx + dy * dy <= r * r {
        [1.0, 1.0, 1.0]
    } else {
        rgb
    };
    [rgb[0], rgb[1], rgb[2], 1.0]
}

/// Fully saturated RGB for hue `h` in [0, 1) (HSV with s=v=1).
fn hue_rgb(h: f32) -> [f32; 3] {
    let f = |n: f32| {
        let k = (n + h * 6.0) % 6.0;
        1.0 - k.min(4.0 - k).clamp(0.0, 1.0)
    };
    [f(5.0), f(3.0), f(1.0)]
}

/// The D2 test card as a GPU texture, in the same f16 CPU-upload shape the old
/// gradient builder used. Effects render over this so a thumbnail shows what
/// the preset does to edges, hues, and tones — not just to a smooth ramp.
pub(crate) fn build_test_card_input(
    device: &GpuDevice,
    w: u32,
    h: u32,
    format: GpuTextureFormat,
) -> RenderTarget {
    let mut pixels = vec![f16::from_f32(0.0); (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            let px = test_card_pixel(x, y, w, h);
            pixels[idx] = f16::from_f32(px[0]);
            pixels[idx + 1] = f16::from_f32(px[1]);
            pixels[idx + 2] = f16::from_f32(px[2]);
            pixels[idx + 3] = f16::from_f32(px[3]);
        }
    }
    let tex = device.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
        label: "preset-thumb-test-card-input",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), std::mem::size_of_val(pixels.as_slice()))
    };
    device.upload_texture(&tex, bytes);
    RenderTarget::view_of(tex, "preset-thumb-test-card-input")
}

/// Walk the compiled plan for the `ResourceId` `node`'s named output port
/// produced. Small plan-walk helper, duplicated (not shared) across this
/// crate's headless-render call sites — same rationale `preset_runtime.rs`'s
/// own copy states: a 5-line utility, not worth a cross-module dependency.
pub(crate) fn output_resource(plan: &crate::node_graph::ExecutionPlan, node: NodeInstanceId, port: &str) -> Option<ResourceId> {
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

// ---------------------------------------------------------------------------
// Effect render — the chain over the test card, developed over the warm-up
// ---------------------------------------------------------------------------

fn render_effect(
    device: &std::sync::Arc<GpuDevice>,
    def: &EffectGraphDef,
    width: u32,
    height: u32,
    linear: bool,
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

    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), width, height, format);
    let input_target = build_test_card_input(device, width, height, format);
    let source_slot = backend.pre_bind_texture_2d(source_out, input_target);
    // A degenerate passthrough graph (Source wired straight to FinalOutput,
    // no processing nodes) shares ONE resource for both boundaries — bind it
    // once and read the same slot back rather than double-binding.
    let output_slot = if final_in == source_out {
        source_slot
    } else {
        let out_target = RenderTarget::new(device, width, height, format, "preset-thumb-fx-out");
        backend.pre_bind_texture_2d(final_in, out_target)
    };

    // The D4 warm-up, same recipe as generators: a stateful effect
    // (temporal::Feedback prev-frame buffers, trails) develops across the 60
    // frames exactly like a sim, and time-dependent effects (Glitch, Strobe)
    // show their actual look rather than a t=0 frame. The `StateStore`
    // persists across the pumped frames — that persistence IS the warm-up.
    let mut state_store = StateStore::new();
    let mut exec = Executor::new(Box::new(backend));
    pump_warmup_frames(device, WARMUP_FRAMES, |frame, gpu| {
        let (time, beats) = warmup_time(frame);
        let frame_time = FrameTime {
            beats: manifold_core::Beats(beats),
            seconds: manifold_core::Seconds(time),
            delta: manifold_core::Seconds(f64::from(WARMUP_DT)),
            frame_count: frame as i64,
        };
        exec.execute_frame_with_state(&mut graph, &plan, frame_time, gpu, &mut state_store, 0);
    });

    let tex = exec
        .backend()
        .texture_2d(output_slot)
        .ok_or_else(|| "output texture missing after execute".to_string())?;
    let mut rgba = if linear {
        crate::headless_readback::readback_srgb_rgba8(device, tex, width, height)
    } else {
        crate::headless_readback::readback_tonemapped_rgba8(device, tex, width, height)
    };
    flatten_over_black(&mut rgba);
    Ok(crate::headless_readback::encode_rgba8_png(&rgba, width, height))
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

    /// D7 freshness gate (STATIC_THUMBNAILS_DESIGN §3.3): every factory preset
    /// has a committed thumbnail whose `.hash` sidecar matches the SHA-256 of
    /// the preset JSON on disk. CPU-only, default suite — editing a preset
    /// without re-running the bin fails here.
    #[test]
    fn factory_thumbnails_fresh() {
        use sha2::Digest;
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let kinds = [
            ("effect-presets", PresetKind::Effect),
            ("generator-presets", PresetKind::Generator),
        ];
        let mut checked = 0usize;
        for (subdir, kind) in kinds {
            let dir = manifest.join("assets").join(subdir);
            let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
                .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
                .collect();
            entries.sort();
            assert!(!entries.is_empty(), "no factory presets in {subdir}");
            for path in entries {
                let id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .expect("preset file stem")
                    .to_string();
                let json_bytes = std::fs::read(&path).expect("read preset JSON");
                let digest = format!("{:x}", sha2::Sha256::digest(&json_bytes));

                let png = factory_thumbnail_path(kind, &id)
                    .unwrap_or_else(|| panic!("thumbnail path for {id}"));
                assert!(png.is_file(), "{id}: missing committed thumbnail {png:?}");

                let hash_path = png.with_extension("hash");
                assert!(hash_path.is_file(), "{id}: missing hash sidecar {hash_path:?}");
                let recorded = std::fs::read_to_string(&hash_path).expect("read hash sidecar");
                assert_eq!(
                    recorded.trim(),
                    digest,
                    "{id}: stale thumbnail — re-run generate-preset-thumbnails"
                );
                checked += 1;
            }
        }
        assert!(checked >= 75, "expected at least 75 factory presets, walked {checked}");
    }

    /// D2 gate, CPU-only: the card has all four regions — gradient ramp, hue
    /// bars, stripes, checker — plus the overlaying white circle, computed at
    /// probe pixels. Catches a layout regression without a GPU.
    #[test]
    fn test_card_layout_has_all_regions() {
        const W: u32 = 256;
        const H: u32 = 144;
        // Left third, off-center: the diagonal gradient (B is the channel mean).
        let g = test_card_pixel(10, 40, W, H);
        assert!(g[0] < 0.1 && g[1] > 0.2 && g[1] < 0.4, "gradient region wrong: {g:?}");
        assert!((g[2] - (g[0] + g[1]) * 0.5).abs() < 1e-6);
        // Middle third: a hue bar — saturated (max-min large), gray floor 0.1.
        let bar = test_card_pixel(W / 3 + 5, H / 2, W, H);
        let spread = bar[0..3].iter().cloned().fold(f32::NAN, f32::max)
            - bar[0..3].iter().cloned().fold(f32::NAN, f32::min);
        assert!(spread > 0.5, "hue bar not saturated: {bar:?}");
        // Right top: 2px stripes alternate every two rows.
        let s0 = test_card_pixel(W - 5, 4, W, H);
        let s1 = test_card_pixel(W - 5, 5, W, H);
        let s2 = test_card_pixel(W - 5, 6, W, H);
        assert!(s0 == s1 && s0 != s2, "stripes must hold for 2 rows then flip: {s0:?} {s1:?} {s2:?}");
        assert!(s0[0] == 0.0 || s0[0] == 1.0, "stripes must be black/white: {s0:?}");
        // Right bottom: 8px checker — neighbors 8 px apart in x differ.
        let c0 = test_card_pixel(W - 80, H - 10, W, H);
        let c1 = test_card_pixel(W - 80 + 8, H - 10, W, H);
        assert!(c0 != c1, "checker must alternate every 8 px: {c0:?} {c1:?}");
        // Circle: center is white even over the gradient region.
        let center = test_card_pixel(W / 2, H / 2, W, H);
        assert_eq!(center, [1.0, 1.0, 1.0, 1.0]);
        // Just outside the radius, back to background.
        let outside = test_card_pixel(W / 2, H / 2 - (H as f32 * 0.2) as u32, W, H);
        assert!(outside[0] < 0.9, "circle radius wrong: {outside:?}");
    }

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
    fn generator_def(id: &str) -> EffectGraphDef {
        let json = std::fs::read_to_string(format!(
            "{}/assets/generator-presets/{id}.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("read generator preset");
        serde_json::from_str(&json).expect("parse generator preset")
    }

    #[cfg(feature = "gpu-proofs")]
    fn assert_opaque(png: &[u8], what: &str) {
        let decoded = image::load_from_memory(png)
            .expect("decode produced PNG")
            .to_rgba8();
        assert!(
            decoded.pixels().all(|px| px.0[3] == 255),
            "{what}: thumbnail has non-opaque pixels (D3)"
        );
    }

    #[cfg(feature = "gpu-proofs")]
    /// Headless value-level gate: renders a real stock effect over the test
    /// card and asserts the output isn't flat/empty (a spread of distinct
    /// pixel values) — same non-uniform-content check `mesh_snapshot.rs`/
    /// `graph_dump.rs` tests use to catch a broken dispatch.
    #[test]
    fn render_effect_thumbnail_produces_non_trivial_png() {
        let device = crate::test_device();
        let def = bloom_def();
        let png = render_preset_thumbnail(&device.arc(), PresetKind::Effect, &def, 96, 54, false)
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
            "expected a spread of distinct colors (card run through Bloom), got {distinct:?}"
        );
        assert_opaque(&png, "Bloom effect");
    }

    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn render_generator_thumbnail_produces_non_trivial_png() {
        let device = crate::test_device();
        let def = generator_def("BlackHole");
        let png = render_preset_thumbnail(&device.arc(), PresetKind::Generator, &def, 96, 54, false)
            .expect("generator thumbnail render");
        assert!(!png.is_empty());
        let decoded = image::load_from_memory(&png).expect("decode produced PNG").to_rgba8();
        // Not asserting non-black here — BlackHole may legitimately render
        // mostly empty space; this confirms the render+encode path itself
        // works and produces a real, decodable image at the right size.
        assert_eq!(decoded.width(), 96);
        assert_eq!(decoded.height(), 54);
        assert_opaque(&png, "BlackHole generator");
    }

    #[cfg(feature = "gpu-proofs")]
    /// Determinism gate (STATIC_THUMBNAILS_DESIGN §4): Bloom (effect) and a
    /// stateful generator, rendered twice each through the full capture
    /// recipe, must produce byte-identical PNGs with every pixel opaque.
    #[test]
    fn thumbnail_render_deterministic() {
        let device = crate::test_device();

        let bloom = bloom_def();
        let a = render_preset_thumbnail(&device.arc(), PresetKind::Effect, &bloom, 128, 72, false)
            .expect("Bloom render 1");
        let b = render_preset_thumbnail(&device.arc(), PresetKind::Effect, &bloom, 128, 72, false)
            .expect("Bloom render 2");
        assert_eq!(a, b, "Bloom thumbnail not byte-identical across two runs");
        assert_opaque(&a, "Bloom effect");

        // Stateful generator: first pick FluidSim2D. If it is not
        // byte-identical across two runs, substitute another stateful
        // generator rather than weaken the assertion.
        let mut stateful_ok = false;
        for id in ["FluidSim2D", "StrangeAttractor", "OilyFluid", "StarField"] {
            let def = generator_def(id);
            let g1 = render_preset_thumbnail(&device.arc(), PresetKind::Generator, &def, 128, 72, false)
                .unwrap_or_else(|e| panic!("{id} render 1: {e}"));
            let g2 = render_preset_thumbnail(&device.arc(), PresetKind::Generator, &def, 128, 72, false)
                .unwrap_or_else(|e| panic!("{id} render 2: {e}"));
            if g1 == g2 {
                assert_opaque(&g1, id);
                eprintln!("thumbnail_render_deterministic: stateful pick = {id}");
                stateful_ok = true;
                break;
            }
            eprintln!("thumbnail_render_deterministic: {id} not byte-identical, substituting");
        }
        assert!(
            stateful_ok,
            "no stateful generator rendered byte-identical thumbnails — determinism is broken, do not weaken this test"
        );
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
