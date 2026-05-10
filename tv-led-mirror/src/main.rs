//! tv-led-mirror — capture a macOS display and stream its edges to ArtNet LEDs.
//!
//! Reuses Manifold's `manifold-led` pipeline end-to-end: the captured frame is
//! handed to `LedOutputController::process_frame` as a `GpuTexture` backed by
//! the IOSurface that `CGDisplayStream` delivers, so the edge-extend compute
//! shader / DMX packing / ArtNet send path are byte-identical to the main app.

use std::ffi::{CString, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use clap::Parser;
use manifold_gpu::{GpuDevice, GpuTextureFormat, GpuTextureUsage};
use manifold_led::{LedOutputController, LedSettings, StripAddressing};
use parking_lot::Mutex;

mod ffi;
mod menu_bar;
mod slicer;

use slicer::{Crop, Slicer};

/// Mirror a macOS display to ArtNet LEDs using Manifold's edge-extend pipeline.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// List available displays and exit.
    #[arg(long)]
    list_displays: bool,

    /// Display ID to capture (use --list-displays to enumerate).
    /// Defaults to the main display.
    #[arg(long)]
    display: Option<u32>,

    /// ArtNet target IP.
    #[arg(long, default_value = "192.168.2.18")]
    ip: String,

    /// ArtNet target port.
    #[arg(long, default_value_t = 6454)]
    port: u16,

    /// Number of LED strips (default: 8 — 4 per side).
    #[arg(long, default_value_t = 8)]
    strips: u32,

    /// LEDs per strip.
    #[arg(long, default_value_t = 120)]
    leds: u32,

    /// First DMX universe (each strip occupies one universe in per-universe mode).
    #[arg(long, default_value_t = 0)]
    start_universe: u16,

    /// Use BGR channel order (default for WS2812-style strips).
    /// Pass --rgb to flip.
    #[arg(long)]
    rgb: bool,

    /// Strip addressing scheme: "per-universe" (default) or "packed".
    #[arg(long, default_value = "per-universe")]
    addressing: String,

    /// Crop margin (fraction of source) to skip on the LEFT before sampling.
    /// Use this to exclude HUD chrome (e.g. a vertical ability column) so it
    /// doesn't get pulled into the strips. The strip×LED grid is stretched
    /// across the cropped content rectangle.
    #[arg(long, default_value_t = 0.0)]
    crop_left: f32,

    /// Crop margin (fraction of source) to skip on the RIGHT before sampling.
    #[arg(long, default_value_t = 0.0)]
    crop_right: f32,

    /// Crop margin (fraction of source) to skip on the TOP before sampling.
    #[arg(long, default_value_t = 0.0)]
    crop_top: f32,

    /// Crop margin (fraction of source) to skip on the BOTTOM before sampling.
    /// Useful for cropping out health/mana bars that sit at the bottom edge.
    #[arg(long, default_value_t = 0.0)]
    crop_bottom: f32,

    /// Spatial blur radius in source texels (smooths single-pixel flicker).
    #[arg(long, default_value_t = 12.0)]
    blur_radius: f32,

    /// Master brightness 0..1.
    #[arg(long, default_value_t = 1.0)]
    brightness: f32,

    /// Capture frame rate cap (Hz). 0 = display's native refresh.
    #[arg(long, default_value_t = 0)]
    fps: u32,

    /// Capture output width (defaults to display native width).
    #[arg(long)]
    cap_width: Option<u32>,

    /// Capture output height (defaults to display native height).
    #[arg(long)]
    cap_height: Option<u32>,

    /// Soft-gate dim regions: pixels with linear luminance below this fade
    /// to black, so dark scenes don't bleed grey ambient onto the wall while
    /// highlights stay vivid. 0 disables the gate.
    #[arg(long, default_value_t = 0.0)]
    luminance_floor: f32,

    /// Width of the smoothstep transition above `--luminance-floor`. Larger =
    /// gentler fade; smaller = closer to a hard threshold.
    #[arg(long, default_value_t = 0.05)]
    luminance_knee: f32,

    /// Soft-gate achromatic content: pixels with HSV saturation below this
    /// fade to black. Defeats the "all-white desktop blasts the LEDs" case
    /// while letting any tint of color through. 0 disables the gate.
    /// Try 0.15 for a moderate dampening of grey/white content.
    #[arg(long, default_value_t = 0.0)]
    saturation_floor: f32,

    /// Width of the smoothstep transition above `--saturation-floor`.
    #[arg(long, default_value_t = 0.05)]
    saturation_knee: f32,
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();

    if cli.list_displays {
        list_displays();
        return;
    }

    let display_id = cli.display.unwrap_or_else(|| unsafe { ffi::CGMainDisplayID() });
    let native_w = unsafe { ffi::CGDisplayPixelsWide(display_id) } as u32;
    let native_h = unsafe { ffi::CGDisplayPixelsHigh(display_id) } as u32;
    if native_w == 0 || native_h == 0 {
        eprintln!("Display {display_id} not found. Try --list-displays.");
        std::process::exit(1);
    }
    let cap_w = cli.cap_width.unwrap_or(native_w);
    let cap_h = cli.cap_height.unwrap_or(native_h);
    log::info!(
        "Capturing display {display_id} (native {native_w}×{native_h}) at {cap_w}×{cap_h}"
    );

    let settings = LedSettings {
        enabled: true,
        artnet_ip: cli.ip.clone(),
        artnet_port: cli.port,
        strip_count: cli.strips,
        leds_per_strip: cli.leds,
        start_universe: cli.start_universe,
        is_bgr: !cli.rgb,
        strip_addressing: parse_addressing(&cli.addressing),
        blur_radius: cli.blur_radius,
        ..LedSettings::default()
    };
    log::info!(
        "ArtNet → {}:{} | {} strips × {} leds | crop L={:.2} R={:.2} T={:.2} B={:.2} blur={:.1}",
        settings.artnet_ip,
        settings.artnet_port,
        settings.strip_count,
        settings.leds_per_strip,
        cli.crop_left,
        cli.crop_right,
        cli.crop_top,
        cli.crop_bottom,
        settings.blur_radius,
    );

    let device = GpuDevice::new();
    let mut controller = LedOutputController::new();
    if !controller.initialize(&device, &settings) {
        eprintln!("LED controller failed to initialize. Bad ArtNet IP/port?");
        std::process::exit(1);
    }
    let slicer = Slicer::new(&device, settings.strip_count, settings.leds_per_strip);

    // Shared state lives on the heap so the CGDisplayStream block + the polling
    // loop both see it. `controller` mutates from both the capture queue (frame
    // ingest → submit GPU work) and the main thread (poll readback).
    let state = Arc::new(SharedState {
        device,
        controller: Mutex::new(controller),
        slicer,
        retained: Mutex::new(Vec::with_capacity(4)),
        brightness: cli.brightness.clamp(0.0, 1.0),
        cap_w,
        cap_h,
        blur_radius: cli.blur_radius,
        luminance_floor: cli.luminance_floor.clamp(0.0, 1.0),
        luminance_knee: cli.luminance_knee.max(0.0001),
        saturation_floor: cli.saturation_floor.clamp(0.0, 1.0),
        saturation_knee: cli.saturation_knee.max(0.0001),
        crop: Crop {
            left: cli.crop_left.clamp(0.0, 0.49),
            right: cli.crop_right.clamp(0.0, 0.49),
            top: cli.crop_top.clamp(0.0, 0.49),
            bottom: cli.crop_bottom.clamp(0.0, 0.49),
        },
        frames_seen: AtomicU64::new(0),
    });

    // SIGINT (Ctrl+C from the launching terminal) and SIGTERM (e.g.
    // `killall tv-led-mirror`) both run our atexit blackout via std::exit.
    // Cmd+Q / "Quit" from the menu also routes through exit(), so the
    // blackout fires on every shutdown path. NSApp.run() blocks the main
    // thread, so we can't unblock it from a signal handler — exit is the
    // simplest correct behavior.
    ctrlc::set_handler(|| std::process::exit(0)).expect("install signal handler");

    // Build the CGDisplayStream. `properties` stays NULL — we cap fps via a
    // throttle in the callback rather than the dictionary, which keeps the FFI
    // surface tiny.
    let queue = unsafe {
        let label = CString::new("tv-led-mirror.capture").unwrap();
        ffi::dispatch_queue_create(label.as_ptr(), std::ptr::null_mut())
    };
    assert!(!queue.is_null(), "dispatch_queue_create failed");

    let min_frame_interval = if cli.fps > 0 {
        Some(Duration::from_secs_f64(1.0 / cli.fps as f64))
    } else {
        None
    };

    let stream = {
        let state = state.clone();
        let last_processed = Mutex::new(Instant::now() - Duration::from_secs(1));
        let handler = block2::RcBlock::new(
            move |status: i32, _display_time: u64, surface: *const c_void, _update: *const c_void| {
                // CGDisplayStreamFrameStatus: 0 = FrameComplete, 1 = FrameIdle,
                // 2 = FrameBlank, 3 = Stopped. Only 0 carries a usable surface.
                if status != 0 || surface.is_null() {
                    return;
                }
                if let Some(min) = min_frame_interval {
                    let mut last = last_processed.lock();
                    if last.elapsed() < min {
                        return;
                    }
                    *last = Instant::now();
                }
                handle_frame(&state, surface);
            },
        );
        unsafe {
            ffi::CGDisplayStreamCreateWithDispatchQueue(
                display_id,
                cap_w as usize,
                cap_h as usize,
                ffi::K_PIXEL_FORMAT_BGRA,
                std::ptr::null(),
                queue,
                block2::RcBlock::as_ptr(&handler) as *const c_void,
            )
        }
    };
    if stream.is_null() {
        eprintln!();
        eprintln!("CGDisplayStreamCreateWithDispatchQueue returned NULL.");
        eprintln!("This almost always means Screen Recording permission was denied.");
        eprintln!();
        eprintln!("Fix:");
        eprintln!("  1. Launch via the .app bundle so the prompt is attributed to");
        eprintln!("     TVLEDMirror (not to your terminal):");
        eprintln!("       ./tv-led-mirror/run.sh --display <id> [other flags]");
        eprintln!("  2. Approve the Screen Recording prompt for TVLEDMirror.");
        eprintln!("  3. If no prompt appears: open System Settings → Privacy &");
        eprintln!("     Security → Screen Recording, look for TVLEDMirror, enable it.");
        eprintln!();
        std::process::exit(1);
    }

    let start_err = unsafe { ffi::CGDisplayStreamStart(stream) };
    if start_err != 0 {
        eprintln!("CGDisplayStreamStart failed (kCGError {start_err}).");
        eprintln!("Re-launch via ./tv-led-mirror/run.sh so TCC sees TVLEDMirror.app.");
        std::process::exit(1);
    }
    log::info!("Capture started. Quit via the menu-bar item, Cmd+Q, or Ctrl+C.");

    // Polling lives on a background thread so the main thread is free for the
    // AppKit run loop (NSApplication.run requires the main thread).
    {
        let state = state.clone();
        std::thread::spawn(move || run_polling_loop(state));
    }

    // Block here until the user quits. exit() is called from the menu's
    // terminate: action / Cmd+Q / our SIGINT handler — at which point the
    // atexit hook in menu_bar runs the controller's shutdown (final blackout).
    menu_bar::run(state);
}

fn run_polling_loop(state: Arc<SharedState>) -> ! {
    let mut last_log = Instant::now();
    let mut last_count = 0u64;
    loop {
        state.controller.lock().poll_readback();
        prune_retained(&state);
        if last_log.elapsed() >= Duration::from_secs(1) {
            let now = state.frames_seen.load(Ordering::Relaxed);
            log::info!("captured {} fps", now - last_count);
            last_count = now;
            last_log = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

// ─── Shared state + frame plumbing ───────────────────────────────────────────

struct SharedState {
    device: GpuDevice,
    controller: Mutex<LedOutputController>,
    slicer: Slicer,
    /// IOSurfaces we've handed to the GPU but not yet released. Each entry is a
    /// retained `IOSurfaceRef` plus the wall-clock time we submitted its work,
    /// used to defer release until the GPU has plausibly finished reading it.
    retained: Mutex<Vec<RetainedSurface>>,
    brightness: f32,
    cap_w: u32,
    cap_h: u32,
    blur_radius: f32,
    luminance_floor: f32,
    luminance_knee: f32,
    saturation_floor: f32,
    saturation_knee: f32,
    crop: Crop,
    frames_seen: AtomicU64,
}

struct RetainedSurface {
    surface: *const c_void,
    submitted_at: Instant,
}

// IOSurfaceRef is a raw pointer to an opaque CF type; manual Send is fine
// because we only touch it under `retained`'s mutex.
unsafe impl Send for RetainedSurface {}

fn handle_frame(state: &Arc<SharedState>, surface: *const c_void) {
    // Bump use count + CFRetain so the framework's pool can't recycle the
    // backing memory while the GPU is still reading from it.
    unsafe {
        ffi::IOSurfaceIncrementUseCount(surface);
        ffi::CFRetain(surface);
    }

    let texture = unsafe {
        state.device.create_texture_from_io_surface(
            surface,
            state.cap_w,
            state.cap_h,
            // sRGB-aware view of the IOSurface: hardware bilinear filter mixes
            // pixels in linear space (per-tap sRGB→linear before filtering),
            // which preserves saturation. Bgra8Unorm here would average bytes
            // in sRGB space and desaturate vibrant colors.
            GpuTextureFormat::Bgra8UnormSrgb,
            GpuTextureUsage::SHADER_READ,
        )
    };

    // 1. Slice the user's left/right edges out of the screen capture, blur,
    //    decode sRGB→linear, write to a strip×LED texture. This is the work
    //    that LedOutputController would normally do on Manifold's pre-sliced
    //    per-layer source — except the controller's widths are hardcoded to
    //    0.5/0.5 so we have to do it ourselves on a full-screen source.
    {
        let mut enc = state.device.create_encoder("tv-led-mirror.slice");
        state.slicer.dispatch(
            &mut enc,
            &texture,
            state.blur_radius,
            state.luminance_floor,
            state.luminance_knee,
            state.saturation_floor,
            state.saturation_knee,
            state.crop,
        );
        enc.commit();
    }

    // 2. Feed our strip×LED slice to the controller. Its hardcoded 0.5/0.5
    //    widths now act as identity at strip-aligned input resolution, so
    //    DMX bytes match what we wrote.
    // active_clip_count: any nonzero value tells the controller "real content
    // is on screen" and skips the blackout fast path. We always have content.
    state.controller.lock().process_frame(
        &state.device,
        state.slicer.output(),
        1,
        state.brightness,
    );
    state.frames_seen.fetch_add(1, Ordering::Relaxed);

    state.retained.lock().push(RetainedSurface {
        surface,
        submitted_at: Instant::now(),
    });
    // The GpuTexture's MTLTexture retains the IOSurface internally for as long
    // as Metal's command buffer holds the texture, but we keep our own retain
    // so the kernel's IOSurface pool doesn't reuse the buffer mid-flight.
    drop(texture);
}

/// Release any IOSurface we've held longer than the GPU could plausibly need.
/// CGDisplayStream's IOSurface pool is small (~8 surfaces) — holding too long
/// stalls the framework's frame delivery while it waits for slots to free up.
/// GPU compute completes in <1ms, so 30ms is a comfortable safety margin
/// (~2 frame intervals at 60fps) without hogging the pool.
fn prune_retained(state: &Arc<SharedState>) {
    const HOLD: Duration = Duration::from_millis(30);
    let mut q = state.retained.lock();
    let now = Instant::now();
    let mut i = 0;
    while i < q.len() {
        if now.duration_since(q[i].submitted_at) >= HOLD {
            let s = q.remove(i);
            unsafe {
                ffi::IOSurfaceDecrementUseCount(s.surface);
                ffi::CFRelease(s.surface);
            }
        } else {
            i += 1;
        }
    }
}

// ─── Display enumeration ─────────────────────────────────────────────────────

fn list_displays() {
    unsafe {
        let main = ffi::CGMainDisplayID();
        let mut count: u32 = 0;
        ffi::CGGetActiveDisplayList(0, std::ptr::null_mut(), &mut count);
        let mut ids = vec![0u32; count as usize];
        ffi::CGGetActiveDisplayList(count, ids.as_mut_ptr(), &mut count);
        ids.truncate(count as usize);
        println!("Active displays ({count}):");
        for (i, &id) in ids.iter().enumerate() {
            let w = ffi::CGDisplayPixelsWide(id);
            let h = ffi::CGDisplayPixelsHigh(id);
            let builtin = ffi::CGDisplayIsBuiltin(id) != 0;
            let is_main = id == main;
            let tags = match (is_main, builtin) {
                (true, true) => "main, builtin",
                (true, false) => "main, external",
                (false, true) => "builtin",
                (false, false) => "external",
            };
            println!("  [{i}] id={id:<10} {w}×{h}  ({tags})");
        }
        println!("\nUse with: tv-led-mirror --display <id>");
    }
}

fn parse_addressing(s: &str) -> StripAddressing {
    match s {
        "packed" => StripAddressing::Packed,
        _ => StripAddressing::PerUniverse,
    }
}

