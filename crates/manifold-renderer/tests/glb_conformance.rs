//! GLB conformance harness (GLB_CONFORMANCE_DESIGN.md G-P1) — renders every
//! asset in `tests/fixtures/gltf/khronos/manifest.json` through the
//! PRODUCTION import path (`assemble_import_graph`) and gates each
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
//! manifold-renderer --features gpu-proofs --test glb_conformance --
//! --test-threads=1`
//!
//! Regenerate goldens: `UPDATE_CONFORMANCE_GOLDENS=1 cargo test -p
//! manifold-renderer --features gpu-proofs --test glb_conformance --
//! --test-threads=1` — review the diff before committing (D3).

#![cfg(feature = "gpu-proofs")]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use manifold_core::params::{Param, ParamManifest};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::headless_readback::{
    encode_rgba8_png, non_black_fraction, readback_raw_halves, readback_tonemapped_rgba8,
};
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::node_graph::gltf_import::assemble_import_graph;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;
use manifold_renderer::render_target::RenderTarget;

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 720;

fn khronos_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf/khronos")
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/gltf")
}

// ---------------------------------------------------------------------------
// Manifest schema (D1/D3, GLB_CONFORMANCE_DESIGN.md §3)
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
    /// GLB_CONFORMANCE_DESIGN.md G-P4: "feature on vs feature off changes
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
    /// GLB_CONFORMANCE_DESIGN.md G-P4: TextureTransformTest's "Offset U"
    /// badge tile flips hue (green ✓ when the transform is applied vs.
    /// olive ⊘ when it's ignored) at near-IDENTICAL luminance — verified
    /// empirically (falsification render: `region_mean_luminance_below`
    /// alone doesn't separate the two states here, a hue-based check
    /// does). `mean(G) - mean(R)` over `region` must exceed `value`.
    #[serde(rename = "region_green_minus_red_above")]
    RegionGreenMinusRedAbove { region: [f64; 4], value: f64 },
    /// GLB_CONFORMANCE_DESIGN.md G-P5: "clearcoat=1 sphere has brighter
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
    RegionMaxLuminanceAbove { region: [f64; 4], reference_region: [f64; 4], value: f64 },
    /// GLTF_MATERIAL_EXTENSIONS_DESIGN.md E2b: "the transmissive region is
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
    RegionStdDevAbove { region: [f64; 4], reference_region: [f64; 4], value: f64 },
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
/// GLB_CONFORMANCE_DESIGN.md G-P5: isolates a specular PEAK, which a mean
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
/// GLTF_MATERIAL_EXTENSIONS_DESIGN.md E2b: measures "how much real detail is
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
// Render (production import path + the ONE shared readback transform, D2)
// ---------------------------------------------------------------------------

/// Import `path` through `assemble_import_graph` and render it to converged,
/// tonemapped RGBA8 pixels — same convergence-poll shape `render_import.rs`
/// uses (BUG-100/BUG-117: byte-stability alone can't distinguish "done" from
/// "every background texture decode is still mid-flight"), reimplemented
/// here rather than shelling out to the bin so a test failure carries a Rust
/// backtrace, not a subprocess exit code.
fn render_asset(path: &Path, overrides: &[(&str, f32)], non_black_floor: f64) -> Result<Vec<u8>, String> {
    let (def, _report) = assemble_import_graph(path)?;

    let mut params: Vec<Param> = def
        .preset_metadata
        .as_ref()
        .map(|m| m.params.iter().map(|s| Param::bundled(s.clone())).collect())
        .unwrap_or_default();
    for (id, v) in overrides {
        if let Some(p) = params.iter_mut().find(|p| p.id() == *id) {
            p.value = *v;
        }
        // An override naming a param this asset's card doesn't expose (e.g.
        // `sun_int` on an asset with no synthesized sun — shouldn't happen,
        // every import graph carries the same outer-card shape, but this
        // isn't the place to panic over it) is silently a no-op, same as
        // `render_import`'s own tolerance for graphs that vary.
    }
    let manifest = ParamManifest::from_params(params);

    let device = Arc::new(GpuDevice::new());
    let registry = PrimitiveRegistry::with_builtin();
    let format = GpuTextureFormat::Rgba16Float;
    let mut runtime = PresetRuntime::from_def_with_device(
        def,
        &registry,
        Arc::clone(&device),
        WIDTH,
        HEIGHT,
        format,
        Some(&manifest),
    )
    .map_err(|e| format!("build failed: {e:?}"))?;

    let target = RenderTarget::new(&device, WIDTH, HEIGHT, format, "conformance-target");

    const DT: f32 = 1.0 / 60.0;
    const STABLE_STREAK: u32 = 3;
    const MAX_FRAMES: u32 = 300;
    let mut prev_raw: Option<Vec<u8>> = None;
    let mut stable_count = 0u32;
    let mut last_fraction = 0.0f64;

    for frame in 0..MAX_FRAMES {
        // BUG-210: `time`/`beat`/`anim_progress` are FROZEN across the
        // convergence loop. The old advancing clock predates auto-playing
        // imports (GLTF_ANIMATION A1–A4): an animated asset re-poses every
        // frame, so byte-stability never occurs, `last_fraction` keeps its
        // 0.0 initializer, and the error reports a phantom black frame —
        // every animated Khronos asset (CesiumMan, Fox, BrainStem,
        // RiggedFigure, RiggedSimple, AnimatedMorphCube) "never converged"
        // while rendering perfectly. Freezing the clock keeps the loop's
        // real job intact (byte-stability still detects late texture-decode
        // swaps — a swap breaks the streak once, then re-stabilizes) while
        // making animated assets exactly as deterministic as static ones.
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
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            runtime.render(&mut gpu, &target.texture, &ctx, &manifest);
        }
        enc.commit_and_wait_completed();

        let raw = readback_raw_halves(&device, &target.texture, WIDTH, HEIGHT);
        let byte_stable = prev_raw.as_deref() == Some(raw.as_slice());
        prev_raw = Some(raw);
        stable_count = if byte_stable { stable_count + 1 } else { 0 };
        // NOT cosmetic — see render_import.rs's identical loop for why:
        // without pacing, `STABLE_STREAK` frames can render faster than a
        // background texture decode thread can swap even one map in, so a
        // genuine partial-load state reads as fully converged. Found
        // empirically on DamagedHelmet.glb this session (a wrong,
        // reproducible "zebra-striped" partial render, byte-stable and
        // above the non-black floor, at a different STABLE_STREAK frame
        // than the correct one). A golden-diffed conformance harness is
        // exactly the place this bug is most dangerous — a flaky render
        // would make `check_golden` flip between pass/fail across runs.
        std::thread::sleep(std::time::Duration::from_millis(50));

        if stable_count >= STABLE_STREAK {
            let rgba = readback_tonemapped_rgba8(&device, &target.texture, WIDTH, HEIGHT);
            last_fraction = non_black_fraction(&rgba);
            if last_fraction > non_black_floor {
                return Ok(rgba);
            }
        }
    }

    Err(format!(
        "never converged after {MAX_FRAMES} frames (last non-black fraction {last_fraction:.4})"
    ))
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
        println!("  UPDATE_CONFORMANCE_GOLDENS=1 — wrote {}", golden_path.display());
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
fn run_check(asset: &str, path: &Path, check: &CheckSpec, assert: bool) -> Result<(), String> {
    match check {
        CheckSpec::NonBlackFractionMin { value } => {
            let rgba = render_asset(path, &[], 0.02)?;
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
                path,
                &[("sun_int", 0.0), ("env_intensity", 0.0), ("scene_ambient", 0.0)],
                value / 2.0,
            )?;
            let frac = non_black_fraction(&rgba);
            println!("  {asset}: lights-off non_black_fraction = {frac:.4} (floor {value})");
            if !assert || frac > *value {
                Ok(())
            } else {
                Err(format!("lights-off non_black_fraction {frac:.4} <= floor {value}"))
            }
        }
        CheckSpec::Golden { file, mean_abs_tol } => {
            let rgba = render_asset(path, &[], 0.02)?;
            let result = check_golden(&rgba, file, *mean_abs_tol);
            if assert { result } else { Ok(()) }
        }
        CheckSpec::RegionMeanLuminanceBelow { region, reference_region, value } => {
            let rgba = render_asset(path, &[], 0.02)?;
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
            let rgba = render_asset(path, &[], 0.02)?;
            let (r, g, _b) = region_mean_rgb(&rgba, *region);
            let diff = g - r;
            println!("  {asset}: region mean G-R = {diff:.2} (floor {value})");
            if !assert || diff > *value {
                Ok(())
            } else {
                Err(format!("region mean G-R {diff:.2} <= floor {value}"))
            }
        }
        CheckSpec::RegionMaxLuminanceAbove { region, reference_region, value } => {
            let rgba = render_asset(path, &[], 0.02)?;
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
        CheckSpec::RegionStdDevAbove { region, reference_region, value } => {
            let rgba = render_asset(path, &[], 0.02)?;
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
    assert!(!entries.is_empty(), "manifest.json must name at least one asset");

    let mut failures: Vec<String> = Vec::new();
    let mut expect_pass_checked = 0usize;
    let mut xfail_count = 0usize;
    let mut skipped = 0usize;

    for entry in &entries {
        let asset_path = khronos_dir().join(&entry.asset);
        let have_fixture = asset_path.exists();

        if let Some(reason) = entry.status.strip_prefix("xfail:") {
            xfail_count += 1;
            if have_fixture {
                println!("XFAIL {} ({reason}) — running checks informationally, not asserted:", entry.asset);
                for check in &entry.checks {
                    if let Err(e) = run_check(&entry.asset, &asset_path, check, false) {
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
            if let Err(e) = run_check(&entry.asset, &asset_path, check, true) {
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
