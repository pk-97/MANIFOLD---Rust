//! `manifold rt-capture <project> [--frames N]` — headless RT channel
//! verification harness.
//!
//! Loads a `.manifold` project through the real ContentCommand::LoadProject
//! path, drives continuous play via headless ContentThread, and captures
//! internal RT textures (raw reflection trace, accumulated history,
//! irradiance, moments) plus composited output at fixed frame intervals.
//!
//! Output: per-capture stats (hit-fraction, mean luma, luma stddev) to
//! stderr and tonemapped PNGs to /tmp/rt_capture/ for visual inspection.
//! Verdicts come from the numbers — PNGs are debug visualization.
//!
//! What this proves: RT channel health (hit-fraction, contrast) across
//! motion→still transitions without a GUI session. Use to validate that
//! the load path, accel rebuild, accumulation, denoise, and composite
//! substitution all function correctly after a project load.
//!
//! Usage:
//!   cargo run --features perf-soak --bin manifold -- rt-capture <project.manifold>
//!   cargo run ... -- rt-capture --paused <project>   # Play 60 → Pause 300
//!   --frame-clock: engine time = 1/fps per rendered frame, not wall clock
//!                  (BUG-jbxt — driver-based motion repros under slow renders)
//!   --set-at N param=value: one-shot param snap at frame N (repeatable)
//!   --width W / --height H: override render resolution (cost measurement;
//!                  applied to the in-memory project, file untouched)
//!   --sync-gpu: block on the GPU fence each frame + print `[GPU_FRAME_MS]`
//!                  (per-frame GPU cost; without it the content thread pipelines
//!                  and frame times read as CPU encode only)
//!
//! MANIFOLD_RT_PROBE is NOT required — the subcommand arms the capture
//! flags directly.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use manifold_renderer::headless_readback::{encode_rgba8_png, linear_to_srgb8};
use manifold_renderer::node_graph::primitives::{
    RtCaptureSlot, RT_CAPTURE_ARM, RT_CAPTURE_ARM_COMPOSITE, RT_CAPTURE_QUEUE,
};
use crate::content_command::ContentCommand;
use crate::headless_harness::headless_content_thread;

/// BUG-fh95: format-aware raw readback + f16 decode. The mask channels
/// (`rt_mask_half`/`rt_mask_full`) are `Rg16Float` (4 B/px: R=vis, G=ao),
/// not `Rgba16Float` (8 B/px) — the old fixed-8-byte decode read the mask
/// garbled (adjacent-pixel bytes landed in b/a), which is what made the
/// 2026-07-28 open-plane recheck inconclusive. Decodes every capture into
/// `[r, g, b, a]` f32 pixels; Rg16Float fills b=0, a=0. Unsupported
/// formats return an empty vec and the caller reports the skip loudly.
pub(crate) fn decode_capture_pixels(
    cap: &RtCaptureSlot,
    device: &manifold_gpu::GpuDevice,
) -> Vec<[f32; 4]> {
    use manifold_gpu::GpuTextureFormat;
    // (bytes/px, component count, f32-not-f16)
    let (bpp, comps, is_f32) = match cap.tex.format {
        GpuTextureFormat::Rgba16Float => (8u32, 4usize, false),
        GpuTextureFormat::Rg16Float => (4u32, 2usize, false),
        GpuTextureFormat::Rg32Float => (8u32, 2usize, true),
        // Rgba32Float — the RT luminance-moments history (ED-A moved it from
        // Rg16Float to Rgba32Float for the variance-precision argument in
        // render_scene.rs; without this arm the noise gate's `moments`
        // channel silently vanished from every capture). 16 B/px, 4 comps,
        // native f32 — the shared f32 decode path below already handles it.
        GpuTextureFormat::Rgba32Float => (16u32, 4usize, true),
        other => {
            eprintln!(
                "[rt-capture] SKIP {}: unsupported capture format {other:?}",
                cap.label
            );
            return Vec::new();
        }
    };
    let bytes_per_row = cap.w * bpp;
    let total = u64::from(cap.h * bytes_per_row);
    let buf = device.create_buffer_shared(total);
    let mut enc = device.create_encoder("rt-capture-readback");
    enc.copy_texture_to_buffer(&cap.tex, &buf, cap.w, cap.h, bytes_per_row);
    enc.commit_and_wait_completed();
    let ptr = buf
        .mapped_ptr()
        .expect("shared readback buffer must expose mapped pointer");
    let raw: &[u8] = unsafe { std::slice::from_raw_parts(ptr, total as usize) };

    let pixel_count = (cap.w * cap.h) as usize;
    let mut out = Vec::with_capacity(pixel_count);
    for i in 0..pixel_count {
        let base = i * bpp as usize;
        let mut px = [0.0f32; 4];
        for (c, slot) in px.iter_mut().enumerate().take(comps) {
            *slot = if is_f32 {
                let o = base + c * 4;
                f32::from_le_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]])
            } else {
                let o = base + c * 2;
                half::f16::from_bits(u16::from_le_bytes([raw[o], raw[o + 1]])).to_f32()
            };
        }
        out.push(px);
    }
    out
}

/// Stats over already-decoded pixels. Returns (hit_frac, mean_luma, stddev).
pub(crate) fn channel_stats(label: &str, pixels: &[[f32; 4]]) -> (f64, f64, f64) {
    let mut n_hits = 0usize;
    let mut sum_luma = 0.0f64; let mut sum_luma_sq = 0.0f64;
    let is_composite = label == "composite";
    for [r, g, b, a] in pixels.iter().copied() {
        if is_composite {
            if r > 0.03 || g > 0.03 || b > 0.03 { n_hits += 1; }
        } else if a > 0.0 && a < 1e6 && !a.is_nan() {
            n_hits += 1;
        }
        let luma = 0.2126 * r.max(0.0) + 0.7152 * g.max(0.0) + 0.0722 * b.max(0.0);
        sum_luma += luma as f64; sum_luma_sq += (luma*luma) as f64;
    }
    if pixels.is_empty() { return (0.0, 0.0, 0.0); }
    let n = pixels.len() as f64;
    let hit_frac = n_hits as f64 / n;
    let mn = sum_luma / n;
    let vr = (sum_luma_sq / n) - mn*mn;
    (hit_frac, mn, vr.sqrt())
}

/// Shared stats computation — reused by headless harness and live capture.
/// Decodes the texture; callers that already hold pixels use `channel_stats`.
pub(crate) fn compute_rt_channel_stats(cap: &RtCaptureSlot, device: &manifold_gpu::GpuDevice) -> (f64, f64, f64) {
    channel_stats(&cap.label, &decode_capture_pixels(cap, device))
}

/// PNG + stats line for one capture, from pixels decoded ONCE by the caller.
/// A 4K channel decode is a full-texture readback plus an 8M-pixel f32 vec —
/// re-decoding per consumer was a measurable share of the capture cost
/// (BUG-olp9).
fn write_capture_png(cap: &RtCaptureSlot, pixels: &[[f32; 4]], stats: (f64, f64, f64), out_dir: &std::path::Path) {
    if pixels.is_empty() {
        return;
    }
    let (hit_frac, mn, sd) = stats;

    // BUG-fh95: per-channel means + center-region means (middle 20% box) —
    // the open-plane probe reads vis (r) / ao (g) at frame center from the
    // RAW `mask_half` trace texture, where the original 0/0 was observed.
    let mut sum = [0.0f64; 4];
    let mut csum = [0.0f64; 4];
    let mut cn = 0usize;
    let (cw0, cw1) = (cap.w * 2 / 5, cap.w * 3 / 5);
    let (ch0, ch1) = (cap.h * 2 / 5, cap.h * 3 / 5);
    for (i, px) in pixels.iter().enumerate() {
        for c in 0..4 { sum[c] += f64::from(px[c]); }
        let (x, y) = (i as u32 % cap.w, i as u32 / cap.w);
        if x >= cw0 && x < cw1 && y >= ch0 && y < ch1 {
            for c in 0..4 { csum[c] += f64::from(px[c]); }
            cn += 1;
        }
    }
    let n = pixels.len() as f64;
    let cd = cn.max(1) as f64;

    // Tonemapped PNG (alpha encodes hit distance for rgba RT channels;
    // rg mask channels render as r=vis, g=ao, opaque).
    let mut rgba8 = Vec::with_capacity(pixels.len() * 4);
    let is_rg = matches!(cap.tex.format, manifold_gpu::GpuTextureFormat::Rg16Float);
    for [r, g, b, a] in pixels.iter().copied() {
        rgba8.push(linear_to_srgb8(r.max(0.0)));
        rgba8.push(linear_to_srgb8(g.max(0.0)));
        rgba8.push(linear_to_srgb8(b.max(0.0)));
        rgba8.push(if is_rg { 255 } else { (a.clamp(0.0, 1.0) * 255.0) as u8 });
    }
    let png_path = out_dir.join(format!("{}_{:04}.png", cap.label, cap.frame));
    std::fs::write(&png_path, encode_rgba8_png(&rgba8, cap.w, cap.h))
        .unwrap_or_else(|e| eprintln!("[rt-capture] write {}: {e}", png_path.display()));
    eprintln!(
        "[rt-capture] {} f={:04} dim={}x{} hit={:.6} luma={:.6} sd={:.6} mean=[{:.4},{:.4},{:.4},{:.4}] center=[{:.4},{:.4},{:.4},{:.4}] {}",
        cap.label, cap.frame, cap.w, cap.h, hit_frac, mn, sd,
        sum[0]/n, sum[1]/n, sum[2]/n, sum[3]/n,
        csum[0]/cd, csum[1]/cd, csum[2]/cd, csum[3]/cd,
        png_path.display(),
    );
}

/// Drain capture queue, compute per-channel stats, return label→(hit,luma,sd) map.
/// Used by both headless harness and live capture (content_thread).
pub(crate) fn drain_capture_stats(
    device: &manifold_gpu::GpuDevice,
) -> std::collections::BTreeMap<String, (f64, f64, f64)> {
    let caps = {
        let mut q = RT_CAPTURE_QUEUE.lock().unwrap();
        std::mem::take(&mut *q)
    };
    if caps.is_empty() { return Default::default(); }
    let mut map = std::collections::BTreeMap::new();
    for c in &caps {
        let stats = compute_rt_channel_stats(c, device);
        map.insert(c.label.clone(), stats);
    }
    map
}

/// Resolve a param ID across all layers by testing mutation: exact match
/// first, then suffix match on `_<param_id>` (prefixed IDs like
/// `8_rt_enabled`). Returns `(layer_index, resolved_param_id)` — only IDs
/// where `set_param` actually changes the value — or exits with the full
/// per-layer param listing.
fn resolve_param_id(project: &manifold_core::project::Project, param_id: &str) -> (usize, String) {
    // Create a test project copy to mutate without side effects.
    let mut test = project.clone();

    // First pass: exact match on all layers.
    for (layer_idx, layer) in test.timeline.layers.iter_mut().enumerate() {
        if let Some(gen_params) = layer.gen_params_mut() {
            let before = gen_params.get_param(param_id);
            let test_val = if before == 0.0 { 1.0 } else { 0.0 };
            gen_params.set_param(param_id, test_val);
            let after = gen_params.get_param(param_id);
            if (before - after).abs() > 0.01 {
                eprintln!("[rt-capture] Found param '{param_id}' on layer[{layer_idx}] (exact match, verified {before:.2}→{after:.2})");
                return (layer_idx, param_id.to_string());
            }
        }
    }

    // Second pass: suffix match (try common node ID prefixes + _<param_id>).
    for (layer_idx, layer) in test.timeline.layers.iter_mut().enumerate() {
        if let Some(gen_params) = layer.gen_params_mut() {
            for prefix in 0..50usize {
                let candidate = format!("{prefix}_{param_id}");
                let before = gen_params.get_param(&candidate);
                let test_val = if before == 0.0 { 1.0 } else { 0.0 };
                gen_params.set_param(&candidate, test_val);
                let after = gen_params.get_param(&candidate);
                if (before - after).abs() > 0.01 {
                    eprintln!("[rt-capture] Found param '{candidate}' on layer[{layer_idx}] (suffix match, prefix={prefix}, verified {before:.2}→{after:.2})");
                    return (layer_idx, candidate);
                }
            }
        }
    }

    // Not found: print diagnostic and exit.
    eprintln!("[rt-capture] Param '{param_id}' not found (exact or prefixed form) — no layer param mutation took effect.");
    eprintln!("[rt-capture] Layer inventory:");
    for (layer_idx, layer) in project.timeline.layers.iter().enumerate() {
        if let Some(_gen_params) = layer.gen_params() {
            eprintln!("  layer[{layer_idx}]: has gen_params");
        } else {
            eprintln!("  layer[{layer_idx}]: no gen_params");
        }
    }
    std::process::exit(1);
}

fn drain_captures(
    device: &manifold_gpu::GpuDevice,
    frame: u32,
    // Running last-seen (hit, luma, sd) per channel label — the live-flip
    // verdict compares snapshots of this map. The per-frame drain empties
    // RT_CAPTURE_QUEUE, so a drain AT verdict time always sees an empty
    // queue; the running map is the only place the history survives.
    last_stats: &mut std::collections::BTreeMap<String, (f64, f64, f64)>,
) {
    let caps = {
        let mut q = RT_CAPTURE_QUEUE.lock().unwrap();
        for c in &mut *q { c.frame = frame; }
        std::mem::take(&mut *q)
    };
    if caps.is_empty() { return; }
    // Overridable because the default is a FIXED shared path: two captures
    // running at once interleave their PNGs into one pile, and a run that
    // clears the directory first silently destroys the other's frames. That
    // produced a phantom "RT channels are all zero" bug report on 2026-07-30
    // (BUG-mw0x) when three sessions captured in parallel. Callers that may
    // overlap — the noise gate, any parallel agent — set this to a unique dir.
    let dir = std::env::var_os("MANIFOLD_RT_CAPTURE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/rt_capture"));
    let _ = std::fs::create_dir_all(&dir);
    for c in &caps {
        let pixels = decode_capture_pixels(c, device);
        let stats = channel_stats(&c.label, &pixels);
        last_stats.insert(c.label.clone(), stats);
        write_capture_png(c, &pixels, stats, &dir);
    }
}

pub(crate) fn arm_capture() {
    RT_CAPTURE_ARM.store(true, Ordering::Relaxed);
    RT_CAPTURE_ARM_COMPOSITE.store(true, Ordering::Relaxed);
}

pub fn run(args: &[String]) -> ! {
    // main.rs's logger init runs after subcommand dispatch — without this the
    // harness drops every log::info from the RT path (rebuild/fallback).
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();

    let paused_mode = args.iter().any(|a| a == "--paused");

    // `--sync-gpu` (cost measurement): block on the GPU fence after every
    // frame and print the per-frame GPU work time as `[GPU_FRAME_MS]`. The
    // content thread otherwise pipelines (CPU encodes frame N+1 while the GPU
    // runs N), so `_t_frame`/`[RENDER_TRACE]` measure CPU encode only — useless
    // for a frame-budget question. Serializing makes each measured frame's
    // wall-clock ≈ its GPU cost (CPU encode ~0.3ms is negligible overlap).
    let sync_gpu = args.iter().any(|a| a == "--sync-gpu");

    // `--set param=value` (repeatable): one MutateProject write after load —
    // the RT-off baseline runs `--set 8_rt_enabled=0`.
    let sets: Vec<(String, f32)> = args
        .windows(2)
        .filter(|w| w[0] == "--set")
        .filter_map(|w| w[1].split_once('=').map(|(k, v)| (k.to_string(), v.parse::<f32>().ok())))
        .filter_map(|(k, v)| v.map(|v| (k, v)))
        .collect();
    // `--set-at N param=value` (repeatable): one-shot MutateProject write at
    // frame N — deterministic param snaps (sawtooth wrap, fast user drags)
    // that --animate's per-frame ramp and the boolean-only --live-flip
    // can't express (BUG-jbxt).
    let set_ats: Vec<(u32, String, f32)> = args
        .windows(3)
        .filter(|w| w[0] == "--set-at")
        .filter_map(|w| {
            let frame = w[1].parse::<u32>().ok()?;
            let (k, v) = w[2].split_once('=')?;
            Some((frame, k.to_string(), v.parse::<f32>().ok()?))
        })
        .collect();
    // `--frame-clock` (BUG-jbxt): engine time advances exactly 1/fps per
    // rendered frame instead of wall clock — without it a harness that
    // renders slower than realtime compresses beat-driven drivers (a
    // 32-beat sawtooth into ~13 frames at debug res).
    let frame_clock = args.iter().any(|a| a == "--frame-clock");
    // `--animate <param> <delta>`: per-frame MutateProject write of
    // base + frame*delta — the same storage-level write a modulator makes.
    let animate: Option<(String, f32)> = args
        .windows(3)
        .find(|w| w[0] == "--animate")
        .and_then(|w| w[2].parse::<f32>().ok().map(|d| (w[1].clone(), d)));

    // `--capture-every N`: arm on every Nth frame (plus the fixed points) —
    // consecutive-frame runs during --animate motion, where ghosting and
    // flicker live. `--capture-from M` skips the early warmup frames.
    let capture_every: Option<u32> = args
        .windows(2)
        .find(|w| w[0] == "--capture-every")
        .and_then(|w| w[1].parse().ok())
        .filter(|n: &u32| *n > 0);
    let capture_from: u32 = args
        .windows(2)
        .find(|w| w[0] == "--capture-from")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(0);

    // `--disable-driver-at N`: at frame N, MutateProject disables every
    // param driver on layer 0's generator — the "motion stops mid-play"
    // case, with the transport still running.
    let disable_driver_at: Option<u32> = args
        .windows(2)
        .find(|w| w[0] == "--disable-driver-at")
        .and_then(|w| w[1].parse().ok());

    // `--live-flip <param_id>`: after initial play phase, flip a scene param
    // live (e.g., "rt_enabled" or "temporal_upscale") and capture stats before/after
    // to verify the toggle takes effect. Used to repro BUG-18l (inert live toggles).
    let live_flip_param: Option<String> = args
        .windows(2)
        .find(|w| w[0] == "--live-flip")
        .map(|w| w[1].to_string());

    // Resolve project path: skip the subcommand name (args[0]), then first
    // non-flag arg that isn't a flag VALUE (--set-at and --animate take two).
    let mut skip_next = 0u8;
    let project_path = args.iter().skip(1)
        .find(|a| {
            if skip_next > 0 {
                skip_next -= 1;
                return false;
            }
            match a.as_str() {
                "--set-at" | "--animate" => { skip_next = 2; false }
                "--frames" | "--width" | "--height" | "--capture-every"
                | "--capture-from" | "--live-flip" | "--disable-driver-at"
                | "--fps" => { skip_next = 1; false }
                f if f.starts_with("--") => false,
                _ => true
            }
        })
        .map(PathBuf::from)
        .unwrap_or_else(|| { eprintln!("usage: manifold rt-capture [--paused] <project.manifold> [--frames N]"); std::process::exit(2); });
    if !project_path.exists() { eprintln!("not found: {}", project_path.display()); std::process::exit(1); }

    // Parse optional --frames flag; default 360.
    let total_frames: u32 = args.windows(2)
        .find(|w| w[0] == "--frames")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(360);

    // Optional resolution override (cost measurement): render at this size
    // regardless of the project's output dims. Applied to the in-memory
    // project before LoadProject so the content pipeline's own resize lands
    // on the override (LoadProject re-resizes to settings — see
    // content_commands.rs); the project file on disk is untouched.
    // rt_a2_term_cost pins every fixture to 3840x2160 this way.
    let override_w: Option<u32> = args.windows(2)
        .find(|w| w[0] == "--width")
        .and_then(|w| w[1].parse().ok());
    let override_h: Option<u32> = args.windows(2)
        .find(|w| w[0] == "--height")
        .and_then(|w| w[1].parse().ok());

    println!("=== RT CAPTURE {}", if paused_mode { "(PAUSED MODE)" } else { "" });
    println!("path: {} frames={}", project_path.display(), total_frames);

    let mut real_project = manifold_io::loader::load_project_with(&project_path, crate::project_io::install_embedded_presets)
        .unwrap_or_else(|e| { eprintln!("FAILED: {e}"); std::process::exit(1); });
    if let Some(w) = override_w {
        real_project.settings.output_width = w as i32;
    }
    if let Some(h) = override_h {
        real_project.settings.output_height = h as i32;
    }
    // `--fps F`: override the project frame rate. With --frame-clock this
    // stretches engine time per rendered frame (1/F s each), so a long
    // timeline can be scanned in a fraction of the frames.
    let fr: f64 = args
        .windows(2)
        .find(|w| w[0] == "--fps")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(real_project.settings.frame_rate as f64);
    let w = real_project.settings.output_width.max(1) as u32;
    let h = real_project.settings.output_height.max(1) as u32;
    println!("output={w}x{h} fps={fr}");

    // Resolve param IDs from the loaded project BEFORE sending to content thread.
    // --animate goes through the same resolver: a bare name like "orbit" must
    // land on the prefixed "5_orbit", otherwise set_param is a silent no-op.
    let animate: Option<(usize, String, f32)> = animate.map(|(param, delta)| {
        let (layer_idx, resolved) = resolve_param_id(&real_project, &param);
        (layer_idx, resolved, delta)
    });
    let mut resolved_sets: Vec<(usize, String, f32)> = Vec::new();
    for (param_id, value) in &sets {
        let (layer_idx, resolved_id) = resolve_param_id(&real_project, param_id);
        resolved_sets.push((layer_idx, resolved_id, *value));
    }
    let mut resolved_set_ats: Vec<(u32, usize, String, f32)> = Vec::new();
    for (frame, param_id, value) in &set_ats {
        let (layer_idx, resolved_id) = resolve_param_id(&real_project, param_id);
        resolved_set_ats.push((*frame, layer_idx, resolved_id, *value));
    }

    let (resolved_flip_layer, resolved_flip_param) = if let Some(ref flip_id) = live_flip_param {
        resolve_param_id(&real_project, flip_id)
    } else {
        (0, String::new())
    };

    let empty = manifold_core::project::Project::default();
    let mut ct = headless_content_thread(empty, w, h);
    ct.timer.set_target_fps(fr);
    ct.timer.set_frame_clocked(frame_clock);
    crate::content_thread::apply_realtime_thread_policy(fr);

    let (state_tx, state_rx) = crossbeam_channel::unbounded::<crate::content_state::ContentState>();
    let drain = std::thread::Builder::new()
        .name("rt-capture-drain".into())
        .spawn(move || while state_rx.recv().is_ok() {})
        .expect("spawn drain");

    ct.handle_command(ContentCommand::LoadProject(Box::new(real_project)));
    // Mirror the production drain loop (content_thread.rs LoadProject arm):
    // the load-time warmup pass runs right after the load. Without this call
    // the harness exercises a load path production never takes.
    {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<ContentCommand>();
        let warm_start = std::time::Instant::now();
        ct.run_warmup(&cmd_rx, &cmd_tx, &state_tx);
        eprintln!("[rt-capture] warmup pass took {:.1?}", warm_start.elapsed());
    }

    // Phase 1: Play N frames (rotation, beat advancing).
    ct.handle_command(ContentCommand::Play);
    let rotation_frames = if paused_mode { 60 } else { total_frames };

    // One-shot --set writes, using resolved layer indices and param IDs.
    for (layer_idx, param, value) in resolved_sets {
        ct.handle_command(ContentCommand::MutateProject(Box::new(move |project| {
            if let Some(g) = project.timeline.layers.get_mut(layer_idx).and_then(|l| l.gen_params_mut()) {
                let old = g.get_param(&param);
                g.set_param(&param, value);
                let new = g.get_param(&param);
                eprintln!("[rt-capture] --set: layer[{layer_idx}] param '{param}' {old:.2} → {new:.2}");
            }
        })));
    }
    // Base value for --animate, read from the loaded project before frame 0.
    let mut animate_base: Option<f32> = None;

    // Capture early AND late: the static-death window is seconds out. When
    // --disable-driver-at is used, bracket the expected death (~15 frames
    // after motion stops) densely.
    let capture_at = |frame: u32, total: u32| {
        frame == 30
            || frame == 120
            || frame == 300
            || frame == total.saturating_sub(1)
            || capture_every.is_some_and(|n| frame >= capture_from && frame.is_multiple_of(n))
            || disable_driver_at.is_some_and(|n| {
                frame == n + 5 || frame == n + 15 || frame == n + 30 || frame == n + 90
            })
    };

    // Track stats before/after live flip for verdict reporting.
    let mut last_stats: std::collections::BTreeMap<String, (f64, f64, f64)> =
        Default::default();
    let mut stats_before_flip: Option<std::collections::BTreeMap<String, (f64, f64, f64)>> = None;
    let mut stats_after_flip: Option<std::collections::BTreeMap<String, (f64, f64, f64)>> = None;
    let mut live_flip_sent = false;

    for frame in 0..rotation_frames {
        for (at, layer_idx, param, value) in &resolved_set_ats {
            if *at == frame {
                let (layer_idx, param, value) = (*layer_idx, param.clone(), *value);
                ct.handle_command(ContentCommand::MutateProject(Box::new(move |project| {
                    if let Some(g) = project.timeline.layers.get_mut(layer_idx).and_then(|l| l.gen_params_mut()) {
                        let old = g.get_param(&param);
                        g.set_param(&param, value);
                        let new = g.get_param(&param);
                        eprintln!("[rt-capture] --set-at f{frame}: layer[{layer_idx}] param '{param}' {old:.2} → {new:.2}");
                    }
                })));
            }
        }
        if disable_driver_at == Some(frame) {
            ct.handle_command(ContentCommand::MutateProject(Box::new(move |project| {
                if let Some(g) = project.timeline.layers[0].gen_params_mut()
                    && let Some(drivers) = g.drivers.as_mut()
                {
                    for d in drivers.iter_mut() {
                        d.enabled = false;
                    }
                }
            })));
            println!("=== DRIVERS DISABLED at frame {frame} (transport keeps playing) ===");
        }
        if let Some((layer_idx, param, delta)) = &animate {
            let layer_idx = *layer_idx;
            let base = *animate_base.get_or_insert_with(|| {
                ct.engine
                    .project()
                    .and_then(|p| p.timeline.layers[layer_idx].gen_params())
                    .map(|g| g.get_param(param))
                    .unwrap_or(0.0)
            });
            let (param, value) = (param.clone(), base + frame as f32 * delta);
            ct.handle_command(ContentCommand::MutateProject(Box::new(move |project| {
                if let Some(g) = project.timeline.layers[layer_idx].gen_params_mut() {
                    g.set_param(&param, value);
                }
            })));
        }
        if capture_at(frame, total_frames) {
            arm_capture();
        }
        ct.timer.wait_for_deadline();
        let gpu_t0 = std::time::Instant::now();
        ct.tick_frame(&state_tx);
        if sync_gpu {
            // Block until this frame's command buffer completes. Wall-clock
            // from tick start to fence done is the per-frame GPU cost (CPU
            // encode ~0.3ms is negligible overlap) — the metric a frame budget
            // is measured against. Without this the content thread pipelines
            // and per-frame times read as CPU encode only.
            ct.content_pipeline.wait_for_render_complete();
            eprintln!(
                "[GPU_FRAME_MS] frame={frame} ms={:.2}",
                gpu_t0.elapsed().as_secs_f64() * 1000.0
            );
        }
        if let Some(dev) = ct.content_pipeline.native_device() { drain_captures(dev, frame, &mut last_stats); }
    }

    // Capture stats before live flip (at end of phase 1).
    if live_flip_param.is_some() {
        stats_before_flip = Some(last_stats.clone());
    }

    // Send live flip command (toggle the param value).
    if live_flip_param.is_some() {
        let param = resolved_flip_param.clone();
        let layer_idx = resolved_flip_layer;
        ct.handle_command(ContentCommand::MutateProject(Box::new(move |project| {
            if let Some(g) = project.timeline.layers.get_mut(layer_idx).and_then(|l| l.gen_params_mut()) {
                // Read current value and flip it (for bool params).
                let current = g.get_param(&param);
                let flipped = 1.0 - current;
                g.set_param(&param, flipped);
                let verified = g.get_param(&param);
                eprintln!("[rt-capture] --live-flip: layer[{layer_idx}] param '{param}' {current:.2} → {verified:.2}");
            }
        })));
        live_flip_sent = true;
        println!("=== LIVE FLIP: toggled param '{resolved_flip_param}' on layer[{resolved_flip_layer}] ===");
    }

    // Phase 2: play additional frames after flip.
    if live_flip_param.is_some() {
        let flip_frames = 120u32;
        for frame in rotation_frames..(rotation_frames + flip_frames) {
            if frame == rotation_frames + 30 || frame == rotation_frames + 90 {
                arm_capture();
            }
            ct.timer.wait_for_deadline();
            ct.tick_frame(&state_tx);
            if let Some(dev) = ct.content_pipeline.native_device() { drain_captures(dev, frame, &mut last_stats); }
        }
        stats_after_flip = Some(last_stats.clone());
    }

    // Phase 2 (paused mode only): Pause, keep calling tick_frame.
    if paused_mode {
        println!("=== PAUSED phase ===");
        ct.handle_command(ContentCommand::Pause);
        for f in 0..(total_frames - rotation_frames) {
            let host = rotation_frames + f;
            // A run of CONSECUTIVE captures deep into the paused phase: the
            // static-boil question is "what differs between frame N and
            // frame N+1 when nothing moves", which sparse captures cannot
            // answer. Late enough that every accumulator has converged.
            let consecutive_run = f >= (total_frames - rotation_frames).saturating_sub(6);
            if f == 10 || f == 30 || f == 90 || consecutive_run {
                arm_capture();
            }
            ct.timer.wait_for_deadline();
            ct.tick_frame(&state_tx);
            if let Some(dev) = ct.content_pipeline.native_device() { drain_captures(dev, host, &mut last_stats); }
        }
    }

    // Final flush.
    if let Some(dev) = ct.content_pipeline.native_device() { drain_captures(dev, total_frames, &mut last_stats); }
    drop(state_tx); drain.join().expect("drain join");

    // Report live-flip verdict.
    if live_flip_sent
        && let (Some(before), Some(after)) = (stats_before_flip, stats_after_flip)
    {
            let mut changed = false;
            // Union of labels: an RT channel that only APPEARS after the
            // flip (rt off→on) or vanishes (on→off) is the strongest
            // possible evidence of an effective toggle — a before-keys-only
            // walk silently missed exactly that case.
            let labels: std::collections::BTreeSet<&String> =
                before.keys().chain(after.keys()).collect();
            for label in labels {
                let b = before.get(label);
                let a = after.get(label);
                match (b, a) {
                    (Some((hb, lb, _)), Some((ha, la, _))) => {
                        if (hb - ha).abs() > 0.01 || (lb - la).abs() > 0.01 {
                            changed = true;
                            eprintln!(
                                "[rt-capture] {label} stats changed: hit {hb:.6} → {ha:.6}, luma {lb:.6} → {la:.6}"
                            );
                        }
                    }
                    (None, Some((ha, la, _))) => {
                        changed = true;
                        eprintln!(
                            "[rt-capture] {label} APPEARED after flip: hit {ha:.6}, luma {la:.6}"
                        );
                    }
                    (Some((hb, lb, _)), None) => {
                        changed = true;
                        eprintln!(
                            "[rt-capture] {label} VANISHED after flip (was hit {hb:.6}, luma {lb:.6})"
                        );
                    }
                    (None, None) => unreachable!(),
                }
            }
            let verdict = if changed { "LIVE-FLIP EFFECTIVE" } else { "LIVE-FLIP INERT" };
            println!("{verdict}");
            eprintln!("{verdict}");
    }

    // Warmup verification surface: any cold touch recorded after the warmup
    // pass's reset means a lazy path fired late (WARMUP_DESIGN.md INV1).
    {
        use manifold_core::cold_touch::{
            ColdTouchKind, cold_touch_count, total_cold_touches,
        };
        eprintln!(
            "[rt-capture] cold touches during playback: {}",
            total_cold_touches()
        );
        for kind in [
            ColdTouchKind::PipelineCompile,
            ColdTouchKind::GlbParse,
            ColdTouchKind::HdriDecode,
            ColdTouchKind::ModelLoad,
            ColdTouchKind::ChainConstruction,
        ] {
            let n = cold_touch_count(kind);
            if n > 0 {
                eprintln!("[rt-capture]   {kind:?}: {n}");
            }
        }
    }

    println!("=== DONE ===");
    std::process::exit(0);
}
